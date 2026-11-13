//! The interpreter: a loop over the crate's well-formedness reader and a
//! [`PlanRef`], answering [`Verdict::Valid`] only when the dynamic validator
//! would accept the document, and [`Verdict::Unknown`] otherwise.
//!
//! Every check of the shared well-formedness layer and the default parse
//! limits hold for an event before it reaches this loop (the reader is the
//! one the dynamic driver uses), so any reader error is `Unknown` and the
//! dynamic path reports it. Character data is joined as the dynamic driver
//! joins it (text, references); a comment, processing instruction or CDATA
//! section inside character content, a document type declaration, and an XML
//! declaration naming another version than 1.0 or another encoding than
//! UTF-8 answer `Unknown`.

use std::collections::HashSet;

use quick_xml::events::{BytesStart, Event};

use crate::xml::reader::{AttrKind, SliceWfReader};
use crate::xml::ParseLimits;
use crate::xml_entity::resolve_general_ref;

use super::model::*;

const XSI_NAMESPACE: &[u8] = b"http://www.w3.org/2001/XMLSchema-instance";

/// The answer of [`check`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The dynamic validator accepts the document (with default flags and
    /// parse limits): no error, and the root is valid.
    Valid,
    /// Ask the dynamic validator. This does not mean the document is
    /// invalid.
    Unknown(UnknownReason),
}

/// Why the fast path stopped (not why a document is invalid).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UnknownReason {
    /// The reader refused the input, or a limit was reached.
    Reader,
    /// A document type declaration, or an XML declaration with another
    /// version or encoding.
    Prolog,
    /// A construct the plan does not cover (a type, a declaration, a name,
    /// an `xsi:` or `xml:` attribute, a comment inside character content…).
    Construct,
    /// A value the plan's checks did not accept.
    Value,
    /// Structure: a child the content model does not take, content that
    /// does not end in an accepting state, a missing required attribute.
    Structure,
}

/// The value tier behind the plan's own checks.
///
/// The shipped tier ([`Shipped`]) has none: an uncovered value check answers
/// `Unknown`. The harness's delegating tier (feature `codegen`) asks the
/// dynamic validator's own function instead, and runs the narrow forms of
/// `xs:ID`, `xs:unique`, element `default` and attribute `fixed`, so the
/// differential tests names, content models, attributes and patterns on real
/// documents. A plan without a `SchemaSet` has only the shipped tier.
pub trait ValueTier {
    /// Whether the delegating tier is on.
    fn delegating(&self) -> bool;
    /// Whether `value` is valid for the check's type (only asked when
    /// `delegating()`).
    fn value(&mut self, delegate: u32, value: &str) -> bool;
}

/// The shipped value tier: nothing behind the plan.
#[derive(Clone, Copy, Debug, Default)]
pub struct Shipped;

impl ValueTier for Shipped {
    fn delegating(&self) -> bool {
        false
    }
    fn value(&mut self, _: u32, _: &str) -> bool {
        false
    }
}

#[derive(Clone, Copy, Debug)]
struct Frame {
    child: u32,
    ty: u32,
    state: u32,
    scope_len: u32,
    scope_bytes: u32,
    unique_inst: u32,
}

/// Reusable buffers of the interpreter: one per thread or task. A warm
/// scratch makes no allocation for a document of a shape it has seen.
#[derive(Debug)]
pub struct Scratch {
    stack: Vec<Frame>,
    text: String,
    /// (prefix start, prefix end, namespace id) in `scope_bytes`.
    scope: Vec<(u32, u32, u32)>,
    scope_bytes: Vec<u8>,
    hasher: ahash::RandomState,
    ids: HashSet<u64>,
    uniques: HashSet<(u32, u64)>,
    unique_counter: u32,
    norm: String,
}

impl Default for Scratch {
    fn default() -> Self {
        Self::new()
    }
}

impl Scratch {
    /// Empty buffers, with a fresh hash key.
    pub fn new() -> Self {
        Scratch {
            stack: Vec::new(),
            text: String::new(),
            scope: Vec::new(),
            scope_bytes: Vec::new(),
            hasher: ahash::RandomState::new(),
            ids: HashSet::new(),
            uniques: HashSet::new(),
            unique_counter: 0,
            norm: String::new(),
        }
    }

    fn reset(&mut self) {
        self.stack.clear();
        self.text.clear();
        self.scope.clear();
        self.scope_bytes.clear();
        self.ids.clear();
        self.uniques.clear();
        self.unique_counter = 0;
    }

    fn resolve(&self, prefix: &[u8]) -> Option<u32> {
        if prefix == b"xml" {
            return None;
        }
        for &(a, b, ns) in self.scope.iter().rev() {
            if &self.scope_bytes[a as usize..b as usize] == prefix {
                return Some(ns);
            }
        }
        // The reader refuses an unbound prefix; unprefixed means no
        // namespace until a default is declared.
        prefix.is_empty().then_some(0)
    }
}

/// Check `input` with the shipped tier.
pub fn check(plan: PlanRef<'_>, input: &[u8], scratch: &mut Scratch) -> Verdict {
    check_with(plan, input, scratch, &mut Shipped)
}

/// Check `input` with a value tier.
pub fn check_with<T: ValueTier>(
    plan: PlanRef<'_>,
    input: &[u8],
    scratch: &mut Scratch,
    tier: &mut T,
) -> Verdict {
    scratch.reset();
    match run(plan, input, scratch, tier) {
        Ok(()) => Verdict::Valid,
        Err(reason) => Verdict::Unknown(reason),
    }
}

type Step = Result<(), UnknownReason>;

fn run<T: ValueTier>(plan: PlanRef<'_>, input: &[u8], s: &mut Scratch, tier: &mut T) -> Step {
    let limits = ParseLimits::default();
    let mut reader = SliceWfReader::from_slice(input, &limits, false);
    let mut root_seen = false;
    loop {
        // The one place events come from.
        let event = reader.read_event().map_err(|_| UnknownReason::Reader)?;
        match event {
            Event::Decl(_) => {
                let d = reader.declaration().ok_or(UnknownReason::Reader)?;
                let utf8 = d
                    .encoding
                    .as_deref()
                    .is_none_or(|e| e.eq_ignore_ascii_case("UTF-8"));
                if d.version != "1.0" || !utf8 {
                    return Err(UnknownReason::Prolog);
                }
            }
            Event::DocType(_) => return Err(UnknownReason::Prolog),
            Event::Start(ref e) => start(plan, &reader, e, s, tier, &mut root_seen)?,
            Event::Empty(ref e) => {
                start(plan, &reader, e, s, tier, &mut root_seen)?;
                end(plan, s, tier)?;
            }
            Event::End(_) => end(plan, s, tier)?,
            Event::Text(ref t) => {
                if s.stack.is_empty() {
                    continue; // white space outside the root
                }
                match top_content(plan, s) {
                    Content::Simple => s.text.push_str(&t.xml_content(reader.quick_xml_version())),
                    Content::ElementOnly
                        if t.as_bytes()
                            .iter()
                            .all(|&b| matches!(b, b' ' | b'\t' | b'\n' | b'\r')) => {}
                    _ => return Err(UnknownReason::Structure),
                }
            }
            Event::GeneralRef(ref g) => {
                if s.stack.is_empty() {
                    continue;
                }
                if top_content(plan, s) != Content::Simple {
                    return Err(UnknownReason::Construct);
                }
                let r = resolve_general_ref(g).map_err(|_| UnknownReason::Reader)?;
                s.text.push_str(&r);
            }
            Event::CData(_) => {
                if !s.stack.is_empty() {
                    return Err(UnknownReason::Construct);
                }
            }
            Event::Comment(_) | Event::PI(_) => {
                if !s.stack.is_empty() && top_content(plan, s) == Content::Simple {
                    return Err(UnknownReason::Construct);
                }
            }
            Event::Eof => {
                return if root_seen && s.stack.is_empty() {
                    Ok(())
                } else {
                    Err(UnknownReason::Structure)
                };
            }
        }
    }
}

fn top_content(plan: PlanRef<'_>, s: &Scratch) -> Content {
    s.stack
        .last()
        .and_then(|f| plan.types.get(f.ty as usize))
        .map_or(Content::Uncovered, |t| t.content)
}

fn split_name(name: &[u8]) -> (&[u8], &[u8]) {
    match name.iter().position(|&b| b == b':') {
        Some(i) => (&name[..i], &name[i + 1..]),
        None => (&[], name),
    }
}

fn start<T: ValueTier>(
    plan: PlanRef<'_>,
    reader: &SliceWfReader<'_>,
    e: &BytesStart<'_>,
    s: &mut Scratch,
    tier: &mut T,
    root_seen: &mut bool,
) -> Step {
    use UnknownReason::*;
    let tag = e.as_bytes();
    let spans = reader.attributes();
    let version = reader.quick_xml_version();
    let scope_len = s.scope.len() as u32;
    let scope_bytes = s.scope_bytes.len() as u32;

    // 1. Namespace declarations.
    for span in spans.iter().filter(|a| a.kind != AttrKind::Attribute) {
        let prefix = span.prefix(tag);
        let uri = span.value(tag, version).map_err(|_| Reader)?;
        let ns = if uri.as_bytes() == XSI_NAMESPACE {
            XSI_NS
        } else {
            plan.namespace_id(uri.as_bytes())
        };
        let a = s.scope_bytes.len() as u32;
        s.scope_bytes.extend_from_slice(prefix);
        s.scope.push((a, s.scope_bytes.len() as u32, ns));
    }

    // 2. The element's name.
    let (prefix, local) = split_name(e.name().0.as_bytes());
    let ns = s.resolve(prefix).ok_or(Construct)?;
    let name = plan.lookup(ns, local);
    if name == NONE {
        return Err(Construct);
    }

    // 3. Its child plan: a global element at the root, else the parent's
    //    transition.
    let mut parent_unique = None;
    let child_id = match s.stack.last_mut() {
        None => {
            if *root_seen {
                return Err(Structure);
            }
            *root_seen = true;
            plan.root(name)
        }
        Some(f) => {
            let pt = plan.types.get(f.ty as usize).ok_or(Construct)?;
            if pt.content != Content::ElementOnly {
                return Err(Structure);
            }
            let pc = plan.children.get(f.child as usize).ok_or(Construct)?;
            if pc.unique != NONE {
                let u = plan.uniques.get(pc.unique as usize).ok_or(Construct)?;
                parent_unique = Some((u.attr, f.unique_inst));
            }
            let st = plan.states.get(f.state as usize).ok_or(Construct)?;
            let ts = plan
                .transitions
                .get(st.t_start as usize..st.t_end as usize)
                .ok_or(Construct)?;
            match ts.iter().find(|t| t.name == name) {
                Some(t) => {
                    f.state = t.next;
                    t.child
                }
                None => return Err(Structure),
            }
        }
    };
    let child = *plan.children.get(child_id as usize).ok_or(Structure)?;
    if child.flags & (CHILD_UNCOVERED | CHILD_FIXED) != 0 {
        return Err(Construct);
    }
    if (child.flags & CHILD_DEFAULT != 0 || child.unique != NONE) && !tier.delegating() {
        return Err(Construct);
    }
    let ty = *plan.types.get(child.ty as usize).ok_or(Construct)?;
    if ty.content == Content::Uncovered {
        return Err(Construct);
    }

    // 4. Attributes.
    let uses = plan
        .attrs
        .get(ty.attr_start as usize..ty.attr_end as usize)
        .ok_or(Construct)?;
    let mut seen = 0u64;
    for span in spans.iter().filter(|a| a.kind == AttrKind::Attribute) {
        let prefix = span.prefix(tag);
        let ans = if prefix.is_empty() {
            0
        } else {
            s.resolve(prefix).ok_or(Construct)?
        };
        if ans == XSI_NS {
            let value = span.value(tag, version).map_err(|_| Reader)?;
            schema_location_hint(plan, span.local(tag), &value, s.stack.is_empty(), tier)?;
            continue;
        }
        let aname = plan.lookup(ans, span.local(tag));
        let u = uses
            .iter()
            .find(|u| aname != NONE && u.name == aname)
            .ok_or(Construct)?;
        let bit = 1u64 << u.bit;
        if seen & bit != 0 {
            return Err(Structure);
        }
        seen |= bit;
        let value = span.value(tag, version).map_err(|_| Reader)?;
        if u.fixed != NONE {
            if !tier.delegating() {
                return Err(Construct);
            }
            let lit = plan.literals.get(u.fixed as usize).ok_or(Construct)?;
            // Equal lexical forms have equal values.
            if plan.span(*lit) != value.as_bytes() {
                return Err(Value);
            }
        }
        run_check(plan, u.check, &value, s, tier)?;
        if let Some((attr, inst)) = parent_unique {
            if attr == aname {
                // The builder admits only fields whose value is the
                // normalized literal.
                let ws = plan.checks.get(u.check as usize).ok_or(Construct)?.ws;
                normalize_into(&value, ws, &mut s.norm);
                let h = s.hasher.hash_one(s.norm.as_bytes());
                if !s.uniques.insert((inst, h)) {
                    return Err(Value);
                }
            }
        }
    }
    if seen & ty.required != ty.required {
        return Err(Structure);
    }
    let unique_inst = if child.unique != NONE {
        s.unique_counter += 1;
        s.unique_counter
    } else {
        0
    };
    s.stack.push(Frame {
        child: child_id,
        ty: child.ty,
        state: ty.start,
        scope_len,
        scope_bytes,
        unique_inst,
    });
    s.text.clear();
    Ok(())
}

/// An `xsi:schemaLocation` or `xsi:noNamespaceSchemaLocation` hint, in the
/// delegating tier only, and on the root only: there no item of any
/// namespace has appeared yet, so XSD 1.0's rule against late-arriving
/// components cannot apply. The value is checked as the runtime checks it:
/// `xs:anyURI` tokens, an even number of them for `schemaLocation`. Every
/// other `xsi:` attribute is not covered.
fn schema_location_hint<T: ValueTier>(
    plan: PlanRef<'_>,
    local: &[u8],
    value: &str,
    root: bool,
    tier: &mut T,
) -> Step {
    use UnknownReason::*;
    if !tier.delegating() || !root || plan.hint_check == NONE {
        return Err(Construct);
    }
    // The runtime splits on Unicode white space; take only values where
    // that is XML white space.
    if value
        .chars()
        .any(|c| c.is_whitespace() && !(c.is_ascii() && is_xml_space(c as u8)))
    {
        return Err(Value);
    }
    let delegate = plan
        .checks
        .get(plan.hint_check as usize)
        .ok_or(Construct)?
        .delegate;
    match local {
        b"schemaLocation" => {
            let tokens: Vec<&str> = value.split_ascii_whitespace().collect();
            if !tokens.len().is_multiple_of(2) || !tokens.iter().all(|t| tier.value(delegate, t)) {
                return Err(Value);
            }
            Ok(())
        }
        b"noNamespaceSchemaLocation" if tier.value(delegate, value) => Ok(()),
        b"noNamespaceSchemaLocation" => Err(Value),
        _ => Err(Construct),
    }
}

fn end<T: ValueTier>(plan: PlanRef<'_>, s: &mut Scratch, tier: &mut T) -> Step {
    use UnknownReason::*;
    let f = s.stack.pop().ok_or(Structure)?;
    s.scope.truncate(f.scope_len as usize);
    s.scope_bytes.truncate(f.scope_bytes as usize);
    let ty = *plan.types.get(f.ty as usize).ok_or(Construct)?;
    let out = match ty.content {
        Content::ElementOnly => match plan.states.get(f.state as usize) {
            Some(st) if st.accept => Ok(()),
            _ => Err(Structure),
        },
        Content::Simple => {
            let child = plan.children.get(f.child as usize).ok_or(Construct)?;
            if child.flags & CHILD_DEFAULT != 0 && s.text.is_empty() {
                // The default would be the value.
                Err(Value)
            } else {
                let text = std::mem::take(&mut s.text);
                let r = run_check(plan, ty.check, &text, s, tier);
                s.text = text;
                r
            }
        }
        Content::Empty => Ok(()),
        Content::Uncovered => Err(Construct),
    };
    s.text.clear();
    out
}

/// `value` normalized as `ws` says (Datatypes §4.3.6), into `out`.
fn normalize_into(value: &str, ws: Ws, out: &mut String) {
    out.clear();
    match ws {
        Ws::Preserve => out.push_str(value),
        Ws::Replace => out.extend(value.chars().map(|c| {
            if matches!(c, '\t' | '\n' | '\r') {
                ' '
            } else {
                c
            }
        })),
        Ws::Collapse => {
            for (i, t) in value
                .split(|c: char| c.is_ascii() && is_xml_space(c as u8))
                .filter(|t| !t.is_empty())
                .enumerate()
            {
                if i > 0 {
                    out.push(' ');
                }
                out.push_str(t);
            }
        }
    }
}

fn is_xml_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r')
}

/// The value as the check's white space leaves it, when that needs no real
/// normalization.
fn normalized(value: &str, ws: Ws) -> Option<&str> {
    match ws {
        Ws::Preserve => Some(value),
        Ws::Replace => {
            (!value.bytes().any(|b| matches!(b, b'\t' | b'\n' | b'\r'))).then_some(value)
        }
        Ws::Collapse => {
            let t = value.trim_matches(|c: char| c.is_ascii() && is_xml_space(c as u8));
            let b = t.as_bytes();
            let clean = !b.iter().any(|&c| matches!(c, b'\t' | b'\n' | b'\r'))
                && !b.windows(2).any(|w| w == b"  ");
            clean.then_some(t)
        }
    }
}

fn run_check<T: ValueTier>(
    plan: PlanRef<'_>,
    id: u32,
    value: &str,
    s: &mut Scratch,
    tier: &mut T,
) -> Step {
    use UnknownReason::*;
    let c = *plan.checks.get(id as usize).ok_or(Construct)?;
    // What the delegating tier says where the plan cannot decide.
    let fallback = |tier: &mut T| {
        if tier.delegating() && c.delegate != NONE && tier.value(c.delegate, value) {
            Ok(())
        } else {
            Err(Value)
        }
    };
    match c.kind {
        CheckKind::Uncovered => Err(Construct),
        CheckKind::Any => Ok(()),
        CheckKind::Delegate => fallback(tier),
        CheckKind::Id => {
            if !tier.delegating() {
                return Err(Construct);
            }
            fallback(tier)?;
            let v = value.trim_matches(|c: char| c.is_ascii() && is_xml_space(c as u8));
            let h = s.hasher.hash_one(v.as_bytes());
            if s.ids.insert(h) {
                Ok(())
            } else {
                Err(Value)
            }
        }
        CheckKind::Str => {
            let Some(v) = normalized(value, c.ws) else {
                return fallback(tier);
            };
            if c.min_len != 0 || c.max_len != NONE {
                let n = v.chars().count() as u64;
                if n < u64::from(c.min_len) || (c.max_len != NONE && n > u64::from(c.max_len)) {
                    return Err(Value);
                }
            }
            if c.enum_start != NONE {
                let lits = plan
                    .enum_literals
                    .get(c.enum_start as usize..c.enum_end as usize)
                    .ok_or(Construct)?;
                let found = lits
                    .binary_search_by(|&l| {
                        let span = plan.literals.get(l as usize).copied();
                        span.map_or(&[][..], |sp| plan.span(sp)).cmp(v.as_bytes())
                    })
                    .is_ok();
                if !found {
                    return Err(Value);
                }
            }
            let steps = plan
                .pattern_steps
                .get(c.pat_start as usize..c.pat_end as usize)
                .ok_or(Construct)?;
            let mut undecided = false;
            for step in steps {
                let table = plan.patterns.get(step.table as usize);
                let alphabet = plan.alphabets.get(step.alphabet as usize);
                match (table, alphabet) {
                    (Some(t), Some(a)) if in_alphabet(plan, a, v) => {
                        if !table_accepts(plan, t, v.as_bytes()) {
                            return Err(Value);
                        }
                    }
                    _ => undecided = true,
                }
            }
            if undecided {
                return fallback(tier);
            }
            Ok(())
        }
    }
}

/// Whether a pattern table accepts the whole of `v`.
pub fn table_accepts(plan: PlanRef<'_>, t: &PatternTable, v: &[u8]) -> bool {
    let Some(classes) = plan.bytes.get(t.classes as usize..t.classes as usize + 256) else {
        return false;
    };
    let Some(trans) = plan.pattern_trans.get(t.trans as usize..) else {
        return false;
    };
    let stride = t.stride as usize;
    let mut state = t.start as usize;
    for &b in v {
        let i = state * stride + classes[b as usize] as usize;
        state = match trans.get(i) {
            Some(&n) => n as usize,
            None => return false,
        };
        if state == 0 {
            return false;
        }
    }
    state < t.states as usize && plan.bytes.get(t.accept as usize + state) == Some(&1)
}

/// Whether every character of `v` is in the alphabet.
pub fn in_alphabet(plan: PlanRef<'_>, a: &Alphabet, v: &str) -> bool {
    let excluded = plan
        .excluded
        .get(a.excl_start as usize..a.excl_end as usize)
        .unwrap_or_default();
    v.chars().all(|ch| {
        let c = ch as u32;
        if c < 64 {
            a.ascii_lo & (1 << c) != 0
        } else if c < 128 {
            a.ascii_hi & (1 << (c - 64)) != 0
        } else {
            a.non_ascii
                && excluded
                    .binary_search_by(|&(lo, hi)| {
                        if hi < c {
                            std::cmp::Ordering::Less
                        } else if lo > c {
                            std::cmp::Ordering::Greater
                        } else {
                            std::cmp::Ordering::Equal
                        }
                    })
                    .is_err()
        }
    })
}
