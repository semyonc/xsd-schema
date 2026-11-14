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

use std::borrow::Cow;
use std::collections::HashSet;

use quick_xml::events::{BytesStart, BytesText, Event};
use quick_xml::XmlVersion;

use crate::xml::reader::{AttrKind, AttrSpan, SliceWfReader};
use crate::xml::ParseLimits;
use crate::xml_entity::resolve_general_ref;

use super::model::*;
use super::values::{check_value, is_xml_space, normalize};

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
/// The shipped tier ([`Shipped`]) has none: a value check the plan does not
/// cover, or a value its check does not accept, answers `Unknown`. The
/// harness's delegating tier (feature `codegen`) asks the dynamic validator's
/// own function instead, so the differential also tests names, content
/// models and attributes where a value the plan leaves out occurs. A plan
/// without a `SchemaSet` has only the shipped tier.
pub trait ValueTier {
    /// Whether the delegating tier is on.
    fn delegating(&self) -> bool;
    /// Whether `value` is valid for the check's type (only asked when
    /// `delegating()`).
    fn value(&mut self, delegate: u32, value: &str) -> bool;
    /// The value check `check` is about to run (a harness counts them; the
    /// shipped tier does nothing).
    #[inline]
    fn note(&mut self, check: u32) {
        let _ = check;
    }
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

/// One open element: what its children, its character content and its end
/// tag need of its child plan and type plan, read from the tables once, when
/// it starts.
#[derive(Clone, Copy, Debug)]
struct Frame {
    /// What the type allows as content.
    content: Content,
    /// The declaration's `CHILD_*` bits.
    flags: u8,
    /// The DFA state (element-only content).
    state: u32,
    /// The value check (simple content).
    check: u32,
    /// The declaration's `default` or `fixed` literal.
    value: u32,
    /// The declaration's narrow `xs:unique` (`NONE` without one).
    unique: u32,
    /// This element's instance of it.
    unique_inst: u32,
    scope_len: u32,
    scope_bytes: u32,
}

/// The character content of the innermost open element so far.
///
/// Most values arrive as one piece of the input (one text event that end-of-
/// line normalization leaves alone): that piece is kept as it is, borrowed.
/// Only a value of several pieces (text, a reference, text) is joined, in
/// [`Scratch::text`].
#[derive(Clone, Copy, Debug)]
enum Text<'a> {
    /// No character content yet.
    None,
    /// One piece, borrowed from the input, and whether it is known to have
    /// no byte up to #x20 (so that no `whiteSpace` changes it).
    One(&'a str, bool),
    /// Several pieces, joined in `Scratch::text`.
    Joined,
}

/// Whether `b` has a carriage return, and whether it has any byte up to #x20
/// (white space; the reader lets no other such byte through).
#[inline]
fn scan_text(b: &[u8]) -> (bool, bool) {
    let (mut cr, mut low) = (false, false);
    for &x in b {
        cr |= x == b'\r';
        low |= x <= b' ';
    }
    (cr, low)
}

impl<'a> Text<'a> {
    /// Append the text event `t`, as `BytesText::xml_content` gives it for
    /// `version`: with the line ends normalized.
    #[inline]
    fn push_text(&mut self, t: BytesText<'a>, version: XmlVersion, joined: &mut String) {
        let (cr, low) = scan_text(t.as_bytes());
        if cr || version == XmlVersion::Explicit1_1 {
            // A line end to normalize (XML 1.1 has more of them).
            self.push(t.xml_content(version), false, joined);
        } else {
            // XML 1.0 normalizes #xD only: without one, the text as written
            // is the content.
            self.push(t.into_inner(), !low, joined);
        }
    }

    /// Append a piece that lives as long as the input; `clean`: it is known
    /// to have no byte up to #x20.
    #[inline]
    fn push(&mut self, piece: Cow<'a, str>, clean: bool, joined: &mut String) {
        match (*self, piece) {
            (Text::None, Cow::Borrowed(p)) => *self = Text::One(p, clean),
            (_, piece) => self.push_str(&piece, joined),
        }
    }

    /// Append a piece of any lifetime.
    fn push_str(&mut self, piece: &str, joined: &mut String) {
        match *self {
            Text::None => joined.clear(),
            Text::One(first, _) => {
                joined.clear();
                joined.push_str(first);
            }
            Text::Joined => {}
        }
        joined.push_str(piece);
        *self = Text::Joined;
    }
}

/// Reusable buffers of the interpreter: one per thread or task. A warm
/// scratch makes no allocation for a document of a shape it has seen.
#[derive(Debug)]
pub struct Scratch {
    stack: Vec<Frame>,
    /// The joined character content of the innermost open element, when it
    /// came in several pieces ([`Text::Joined`]).
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

    /// The namespace id of a prefix (the empty one: the default namespace).
    #[inline]
    fn resolve(&self, prefix: &[u8]) -> Option<u32> {
        if prefix.is_empty() && self.scope.is_empty() {
            // No declaration in scope: no namespace.
            return Some(0);
        }
        self.resolve_declared(prefix)
    }

    #[inline(never)]
    fn resolve_declared(&self, prefix: &[u8]) -> Option<u32> {
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
    // The plan is twenty slices: every function below takes it by reference.
    match run(&plan, input, scratch, tier) {
        Ok(()) => Verdict::Valid,
        Err(reason) => Verdict::Unknown(reason),
    }
}

type Step = Result<(), UnknownReason>;

fn run<T: ValueTier>(plan: &PlanRef<'_>, input: &[u8], s: &mut Scratch, tier: &mut T) -> Step {
    let limits = ParseLimits::default();
    let mut reader = SliceWfReader::from_slice(input, &limits, false);
    let mut root_seen = false;
    // The character content of the innermost open element.
    let mut text = Text::None;
    loop {
        // The one place events come from.
        let event = reader.read_event().map_err(|_| UnknownReason::Reader)?;
        match event {
            Event::Decl(_) => {
                let d = reader.declaration().ok_or(UnknownReason::Reader)?;
                let utf8 = d.encoding.is_none_or(|e| e.eq_ignore_ascii_case("UTF-8"));
                if d.version != "1.0" || !utf8 {
                    return Err(UnknownReason::Prolog);
                }
            }
            Event::DocType(_) => return Err(UnknownReason::Prolog),
            Event::Start(ref e) => {
                start(plan, &reader, e, s, tier, &mut root_seen)?;
                text = Text::None;
            }
            Event::Empty(ref e) => {
                start(plan, &reader, e, s, tier, &mut root_seen)?;
                end(plan, s, tier, Text::None)?;
                text = Text::None;
            }
            Event::End(_) => {
                end(plan, s, tier, text)?;
                text = Text::None;
            }
            Event::Text(t) => {
                if s.stack.is_empty() {
                    continue; // white space outside the root
                }
                match top_content(s) {
                    Content::Simple => text.push_text(t, reader.quick_xml_version(), &mut s.text),
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
                if top_content(s) != Content::Simple {
                    return Err(UnknownReason::Construct);
                }
                let r = resolve_general_ref(g).map_err(|_| UnknownReason::Reader)?;
                text.push_str(&r, &mut s.text);
            }
            Event::CData(_) => {
                if !s.stack.is_empty() {
                    return Err(UnknownReason::Construct);
                }
            }
            Event::Comment(_) | Event::PI(_) => {
                if !s.stack.is_empty() && top_content(s) == Content::Simple {
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

/// What the innermost open element allows as content.
#[inline]
fn top_content(s: &Scratch) -> Content {
    s.stack.last().map_or(Content::Uncovered, |f| f.content)
}

fn split_name(name: &[u8]) -> (&[u8], &[u8]) {
    match name.iter().position(|&b| b == b':') {
        Some(i) => (&name[..i], &name[i + 1..]),
        None => (&[], name),
    }
}

/// Open an element. The common tag has no attribute and no namespace
/// declaration: what only the others need is out of line
/// ([`declare_namespaces`], [`attributes`], and [`root_child`] for the
/// document's first tag).
fn start<T: ValueTier>(
    plan: &PlanRef<'_>,
    reader: &SliceWfReader<'_>,
    e: &BytesStart<'_>,
    s: &mut Scratch,
    tier: &mut T,
    root_seen: &mut bool,
) -> Step {
    use UnknownReason::*;
    let tag = e.as_bytes();
    let spans = reader.attributes();
    let scope_len = s.scope.len() as u32;
    let scope_bytes = s.scope_bytes.len() as u32;

    // 1. Namespace declarations.
    if !spans.is_empty() {
        declare_namespaces(plan, reader, tag, spans, s)?;
    }

    // 2. The element's name.
    let (prefix, local) = split_name(e.name().0.as_bytes());
    let ns = s.resolve(prefix).ok_or(Construct)?;

    // 3. Its child plan: a global element at the root, else the parent's
    //    transition, found by comparing the name with the transitions'.
    let mut parent_unique = None;
    let child_id = match s.stack.last_mut() {
        None => root_child(plan, ns, local, root_seen)?,
        Some(f) => {
            if f.content != Content::ElementOnly {
                return Err(Structure);
            }
            if f.unique != NONE {
                let u = plan.uniques.get(f.unique as usize).ok_or(Construct)?;
                parent_unique = Some((u.attr, f.unique_inst));
            }
            let st = plan.states.get(f.state as usize).ok_or(Construct)?;
            let ts = plan
                .transitions
                .get(st.t_start as usize..st.t_end as usize)
                .ok_or(Construct)?;
            // A few transitions: compare names; many: one hashed lookup.
            let mut found = None;
            if ts.len() <= 4 {
                for t in ts {
                    if plan.name_is(t.name, ns, local) {
                        found = Some(t);
                        break;
                    }
                }
            } else {
                let name = plan.lookup(ns, local);
                if name != NONE {
                    found = ts.iter().find(|t| t.name == name);
                }
            }
            match found {
                Some(t) => {
                    f.state = t.next;
                    t.child
                }
                None => return Err(Structure),
            }
        }
    };
    let child = plan.children.get(child_id as usize).ok_or(Structure)?;
    if child.flags & CHILD_UNCOVERED != 0 {
        return Err(Construct);
    }
    let ty = plan.types.get(child.ty as usize).ok_or(Construct)?;
    if ty.content == Content::Uncovered {
        return Err(Construct);
    }
    // A value constraint is checked on simple content only (the builder
    // leaves other declarations with one uncovered).
    if child.flags & (CHILD_DEFAULT | CHILD_FIXED) != 0 && ty.content != Content::Simple {
        return Err(Construct);
    }

    // 4. Attributes.
    let uses = plan
        .attrs
        .get(ty.attr_start as usize..ty.attr_end as usize)
        .ok_or(Construct)?;
    if spans.is_empty() {
        if ty.required != 0 {
            return Err(Structure);
        }
    } else {
        attributes(
            plan,
            reader,
            tag,
            spans,
            uses,
            ty.required,
            parent_unique,
            s,
            tier,
        )?;
    }
    let unique_inst = if child.unique != NONE {
        s.unique_counter += 1;
        s.unique_counter
    } else {
        0
    };
    s.stack.push(Frame {
        content: ty.content,
        flags: child.flags,
        state: ty.start,
        check: ty.check,
        value: child.value,
        unique: child.unique,
        unique_inst,
        scope_len,
        scope_bytes,
    });
    Ok(())
}

/// Bring the namespace declarations of a start tag into scope.
#[inline(never)]
fn declare_namespaces(
    plan: &PlanRef<'_>,
    reader: &SliceWfReader<'_>,
    tag: &[u8],
    spans: &[AttrSpan],
    s: &mut Scratch,
) -> Step {
    let version = reader.quick_xml_version();
    for span in spans.iter().filter(|a| a.kind != AttrKind::Attribute) {
        let prefix = span.prefix(tag);
        let uri = span
            .value(tag, version)
            .map_err(|_| UnknownReason::Reader)?;
        let ns = if uri.as_bytes() == XSI_NAMESPACE {
            XSI_NS
        } else {
            plan.namespace_id(uri.as_bytes())
        };
        let a = s.scope_bytes.len() as u32;
        s.scope_bytes.extend_from_slice(prefix);
        s.scope.push((a, s.scope_bytes.len() as u32, ns));
    }
    Ok(())
}

/// The child plan of the document's first tag: a global element.
#[inline(never)]
fn root_child(
    plan: &PlanRef<'_>,
    ns: u32,
    local: &[u8],
    root_seen: &mut bool,
) -> Result<u32, UnknownReason> {
    if *root_seen {
        return Err(UnknownReason::Structure);
    }
    *root_seen = true;
    let name = plan.lookup(ns, local);
    if name == NONE {
        return Err(UnknownReason::Construct);
    }
    Ok(plan.root(name))
}

/// The attributes of a start tag against the attribute uses `uses` of its
/// type, `required` being the bits of the required ones. `parent_unique` is
/// the field attribute and the instance of the parent's narrow `xs:unique`.
#[inline(never)]
#[allow(clippy::too_many_arguments)]
fn attributes<T: ValueTier>(
    plan: &PlanRef<'_>,
    reader: &SliceWfReader<'_>,
    tag: &[u8],
    spans: &[AttrSpan],
    uses: &[AttrUse],
    required: u64,
    parent_unique: Option<(u32, u32)>,
    s: &mut Scratch,
    tier: &mut T,
) -> Step {
    use UnknownReason::*;
    let version = reader.quick_xml_version();
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
            let root = s.stack.is_empty();
            schema_location_hint(plan, span.local(tag), &value, root, s, tier)?;
            continue;
        }
        let alocal = span.local(tag);
        let u = uses
            .iter()
            .find(|u| plan.name_is(u.name, ans, alocal))
            .ok_or(Construct)?;
        let aname = u.name;
        let bit = 1u64 << u.bit;
        if seen & bit != 0 {
            return Err(Structure);
        }
        seen |= bit;
        let value = span.value(tag, version).map_err(|_| Reader)?;
        if u.fixed != NONE {
            let lit = plan.literals.get(u.fixed as usize).ok_or(Construct)?;
            // Equal lexical forms have equal values.
            if plan.span(*lit) != value.as_bytes() {
                return Err(Value);
            }
        }
        run_check(plan, u.check, &value, false, s, tier)?;
        if let Some((attr, inst)) = parent_unique {
            if attr == aname {
                // The builder admits only fields whose value is the
                // normalized literal.
                let ws = plan.checks.get(u.check as usize).ok_or(Construct)?.ws;
                let v = normalize(&value, ws, &mut s.norm);
                let h = s.hasher.hash_one(v.as_bytes());
                if !s.uniques.insert((inst, h)) {
                    return Err(Value);
                }
            }
        }
    }
    if seen & required != required {
        return Err(Structure);
    }
    Ok(())
}

/// An `xsi:schemaLocation` or `xsi:noNamespaceSchemaLocation` hint, on the
/// root only: there no item of any namespace has appeared yet, so XSD 1.0's
/// rule against late-arriving components cannot apply. The value is checked
/// as the runtime checks it: `xs:anyURI` tokens, an even number of them for
/// `schemaLocation`. Every other `xsi:` attribute is not covered.
fn schema_location_hint<T: ValueTier>(
    plan: &PlanRef<'_>,
    local: &[u8],
    value: &str,
    root: bool,
    s: &mut Scratch,
    tier: &mut T,
) -> Step {
    use UnknownReason::*;
    if !root || plan.hint_check == NONE {
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
    match local {
        b"schemaLocation" => {
            let mut n = 0usize;
            for t in value.split_ascii_whitespace() {
                run_check(plan, plan.hint_check, t, false, s, tier)?;
                n += 1;
            }
            if !n.is_multiple_of(2) {
                return Err(Value);
            }
            Ok(())
        }
        b"noNamespaceSchemaLocation" => run_check(plan, plan.hint_check, value, false, s, tier),
        _ => Err(Construct),
    }
}

/// Close the innermost open element, whose character content is `text`.
fn end<T: ValueTier>(plan: &PlanRef<'_>, s: &mut Scratch, tier: &mut T, text: Text<'_>) -> Step {
    use UnknownReason::*;
    let f = s.stack.pop().ok_or(Structure)?;
    s.scope.truncate(f.scope_len as usize);
    s.scope_bytes.truncate(f.scope_bytes as usize);
    match f.content {
        Content::ElementOnly => match plan.states.get(f.state as usize) {
            Some(st) if st.accept => Ok(()),
            _ => Err(Structure),
        },
        Content::Simple => match text {
            Text::None => simple_value(plan, &f, "", false, s, tier),
            Text::One(value, clean) => simple_value(plan, &f, value, clean, s, tier),
            Text::Joined => {
                let value = std::mem::take(&mut s.text);
                let r = simple_value(plan, &f, &value, false, s, tier);
                s.text = value;
                r
            }
        },
        Content::Empty => Ok(()),
        Content::Uncovered => Err(Construct),
    }
}

/// The value of the element `f`, of simple content: its character content
/// `value` (`clean`: known to have no byte up to #x20), or its value
/// constraint when it has no character content.
fn simple_value<T: ValueTier>(
    plan: &PlanRef<'_>,
    f: &Frame,
    value: &str,
    clean: bool,
    s: &mut Scratch,
    tier: &mut T,
) -> Step {
    use UnknownReason::*;
    let constraint = f.flags & (CHILD_DEFAULT | CHILD_FIXED) != 0;
    let (value, clean) = if constraint && value.is_empty() {
        // An element with no character children takes its value constraint
        // as its value (cvc-elt.5.1).
        let lit = plan.literals.get(f.value as usize).ok_or(Construct)?;
        let lit = std::str::from_utf8(plan.span(*lit)).map_err(|_| Construct)?;
        (lit, false)
    } else if f.flags & CHILD_FIXED != 0 {
        // Equal lexical forms have equal values (cvc-elt.5.2.2).
        let lit = plan.literals.get(f.value as usize).ok_or(Construct)?;
        if plan.span(*lit) != value.as_bytes() {
            return Err(Value);
        }
        (value, clean)
    } else {
        (value, clean)
    };
    run_check(plan, f.check, value, clean, s, tier)
}

/// Run the check `id` (and the checks chained to it) on `value` (`clean`:
/// known to have no byte up to #x20).
fn run_check<T: ValueTier>(
    plan: &PlanRef<'_>,
    id: u32,
    value: &str,
    clean: bool,
    s: &mut Scratch,
    tier: &mut T,
) -> Step {
    use UnknownReason::*;
    tier.note(id);
    let head = plan.checks.get(id as usize).ok_or(Construct)?;
    // What the delegating tier says where the plan does not decide: about
    // the whole value, for the head check's type.
    let delegate = head.delegate;
    let fallback = |tier: &mut T| {
        if tier.delegating() && delegate != NONE && tier.value(delegate, value) {
            Ok(())
        } else {
            Err(Value)
        }
    };
    let mut c = head;
    loop {
        match c.kind {
            CheckKind::Uncovered => return Err(Construct),
            CheckKind::Any => {}
            CheckKind::Delegate => return fallback(tier),
            CheckKind::Value => match check_value(plan, c, value, clean, &mut s.norm) {
                Ok(v) => {
                    if c.flags & CHECK_ID != 0 {
                        // The ID's value is its collapsed literal; a hash
                        // collision reads as a duplicate.
                        let h = s.hasher.hash_one(v.as_bytes());
                        if !s.ids.insert(h) {
                            return Err(Value);
                        }
                    }
                }
                Err(e) if !tier.delegating() => return Err(e),
                Err(_) => {
                    fallback(tier)?;
                    if c.flags & CHECK_ID != 0 {
                        let v =
                            value.trim_matches(|ch: char| ch.is_ascii() && is_xml_space(ch as u8));
                        let h = s.hasher.hash_one(v.as_bytes());
                        if !s.ids.insert(h) {
                            return Err(Value);
                        }
                    }
                    return Ok(());
                }
            },
        }
        if c.next == NONE {
            return Ok(());
        }
        c = plan.checks.get(c.next as usize).ok_or(Construct)?;
    }
}

/// Whether a pattern table accepts the whole of `v`.
pub fn table_accepts(plan: &PlanRef<'_>, t: &PatternTable, v: &[u8]) -> bool {
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
pub fn in_alphabet(plan: &PlanRef<'_>, a: &Alphabet, v: &str) -> bool {
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
