//! The plan builder (feature `codegen`): a [`PlanData`] from a compiled
//! [`SchemaSet`].
//!
//! It decides nothing the runtime decides elsewhere: content models are
//! determinized with the runtime's own content-model state as the oracle
//! (`ContentValidatorState::try_advance_element` per symbol, `is_complete`
//! for acceptance), children get their declaration and type through the same
//! two steps the runtime uses (`SchemaSet::governing_element_declaration`,
//! `SchemaSet::declared_element_type`), attribute tables come from
//! `SchemaSet::for_each_attribute_use`, and string-family value checks read
//! the effective facets and white space the runtime reads. Whatever is not
//! covered is marked so, and answers `Unknown` at run time.

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::compiler::NfaTerm;
use crate::ids::{ComplexTypeKey, ElementKey, NameId, SimpleTypeKey, TypeKey};
use crate::parser::frames::{AttributeUseKind, IdentityKind, SimpleTypeVariety};
use crate::schema::attribute_uses::AttributeLookup;
use crate::schema::model::{RegexCompat, XsdVersion};
use crate::schema::SchemaSet;
use crate::types::facets::{FacetSet, WhitespaceMode};
use crate::types::XmlTypeCode;
use crate::validation::content::{CompiledContentModel, ContentValidatorState};
use crate::validation::info::{ContentType, ValidationFlags};
use crate::validation::runtime::determine_content_type;
use crate::validation::{validate_simple_type, SchemaValidator};

use super::alphabet::{step_alphabet, ComposedAlphabet};
use super::model::*;
use super::pattern::build_table;

/// A content model whose DFA has more states is not covered.
pub const DFA_STATE_LIMIT: usize = 20_000;

const XS_NAMESPACE: &str = "http://www.w3.org/2001/XMLSchema";

/// What the builder left uncovered, and other figures.
#[derive(Clone, Debug, Default)]
pub struct BuildReport {
    /// One line per uncovered type, declaration or check, with the reason.
    pub uncovered: Vec<String>,
    /// Pattern steps without a table or alphabet, with the reason.
    pub patterns_refused: Vec<String>,
    /// Pattern steps with a table.
    pub patterns_built: usize,
}

/// Build the plan of every global element of `set`.
pub fn build(set: &SchemaSet) -> (PlanData, BuildReport) {
    let validator = SchemaValidator::new(set, ValidationFlags::default());
    let mut b = Builder {
        set,
        validator: &validator,
        version: set.xsd_version,
        xs: set.name_table.get(XS_NAMESPACE),
        plan: PlanData::default(),
        report: BuildReport::default(),
        ns_ids: HashMap::new(),
        name_ids: HashMap::new(),
        type_memo: HashMap::new(),
        child_memo: HashMap::new(),
        check_memo: HashMap::new(),
        step_memo: HashMap::new(),
        alphabet_memo: HashMap::new(),
        type_states: HashMap::new(),
        valued_attrs: BTreeSet::new(),
        literal_valued: BTreeSet::new(),
        building: HashSet::new(),
    };
    b.plan.namespaces.push(Span { start: 0, len: 0 });
    b.ns_ids.insert(None, 0);

    // Global elements in a stable order (namespace URI, local name).
    let mut globals: Vec<(String, String, Option<NameId>, NameId, ElementKey)> = Vec::new();
    for (ns, table) in &set.namespaces {
        for (&local, &key) in &table.elements {
            let uri = ns
                .map(|n| set.name_table.resolve_ref(n).to_string())
                .unwrap_or_default();
            let name = set.name_table.resolve_ref(local).to_string();
            globals.push((uri, name, *ns, local, key));
        }
    }
    globals.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
    for (_, _, ns, local, key) in globals {
        // The root's declaration, by the rule a child's is found by.
        let Some(decl) = set.governing_element_declaration(local, ns, None, false) else {
            continue;
        };
        debug_assert_eq!(decl, key);
        let name = b.intern_name(ns, local);
        let child = b.child_plan(decl);
        b.plan.roots.push(Root { name, child });
    }
    b.plan.roots.sort_by_key(|r| r.name);
    b.plan.hint_check = b.check(TypeKey::Simple(set.builtin_types().any_uri));
    b.check_uniques();
    b.finish_name_table();
    (b.plan, b.report)
}

/// An alphabet as a map key: ASCII masks, past-ASCII flag, exclusions.
type AlphabetKey = (u64, u64, bool, Vec<(u32, u32)>);

struct Builder<'a> {
    set: &'a SchemaSet,
    validator: &'a SchemaValidator<'a>,
    version: XsdVersion,
    xs: Option<NameId>,
    plan: PlanData,
    report: BuildReport,
    ns_ids: HashMap<Option<NameId>, u32>,
    name_ids: HashMap<(Option<NameId>, NameId), u32>,
    type_memo: HashMap<TypeKey, u32>,
    child_memo: HashMap<ElementKey, u32>,
    check_memo: HashMap<TypeKey, u32>,
    step_memo: HashMap<(Vec<String>, XsdVersion), PatternStep>,
    alphabet_memo: HashMap<AlphabetKey, u32>,
    /// Each element-only type plan's states: `states[start..end]`.
    type_states: HashMap<u32, (u32, u32)>,
    /// (type plan, attribute name) of attribute uses with a value constraint.
    valued_attrs: BTreeSet<(u32, u32)>,
    /// Child plans still being built (their entry is a placeholder).
    building: HashSet<u32>,
    /// Checks whose type's value is its white-space-normalized literal
    /// (the string family and `xs:anyURI`): value equality is equality of
    /// normalized literals.
    literal_valued: BTreeSet<u32>,
}

impl<'a> Builder<'a> {
    fn name(&self, id: NameId) -> &str {
        self.set.name_table.resolve_ref(id)
    }

    fn push_bytes(&mut self, b: &[u8]) -> Span {
        let start = self.plan.bytes.len() as u32;
        self.plan.bytes.extend_from_slice(b);
        Span {
            start,
            len: b.len() as u32,
        }
    }

    fn literal(&mut self, s: &str) -> u32 {
        let span = self.push_bytes(s.as_bytes());
        self.plan.literals.push(span);
        self.plan.literals.len() as u32 - 1
    }

    fn intern_ns(&mut self, ns: Option<NameId>) -> u32 {
        if let Some(&id) = self.ns_ids.get(&ns) {
            return id;
        }
        let uri = ns.map(|n| self.name(n).to_string()).unwrap_or_default();
        let span = self.push_bytes(uri.as_bytes());
        self.plan.namespaces.push(span);
        let id = self.plan.namespaces.len() as u32 - 1;
        self.ns_ids.insert(ns, id);
        id
    }

    fn intern_name(&mut self, ns: Option<NameId>, local: NameId) -> u32 {
        if let Some(&id) = self.name_ids.get(&(ns, local)) {
            return id;
        }
        let ns_id = self.intern_ns(ns);
        let local_bytes = self.name(local).to_string();
        let span = self.push_bytes(local_bytes.as_bytes());
        self.plan.names.push(NameEntry {
            ns: ns_id,
            start: span.start,
            len: span.len,
        });
        let id = self.plan.names.len() as u32 - 1;
        self.name_ids.insert((ns, local), id);
        id
    }

    fn finish_name_table(&mut self) {
        let n = self.plan.names.len().max(1);
        let cap = (n * 4).next_power_of_two();
        let mut table = vec![NONE; cap];
        let mask = cap - 1;
        for (i, e) in self.plan.names.iter().enumerate() {
            let local = &self.plan.bytes[e.start as usize..(e.start + e.len) as usize];
            let mut p = hash_name(e.ns, local) as usize & mask;
            while table[p] != NONE {
                p = (p + 1) & mask;
            }
            table[p] = i as u32;
        }
        self.plan.name_table = table;
    }

    fn uncovered(&mut self, what: impl Into<String>) {
        self.report.uncovered.push(what.into());
    }

    fn element_label(&self, key: ElementKey) -> String {
        let e = &self.set.arenas.elements[key];
        e.name
            .map(|n| self.name(n).to_string())
            .unwrap_or_else(|| "(unnamed)".into())
    }

    fn type_label(&self, tk: TypeKey) -> String {
        let name = match tk {
            TypeKey::Simple(k) => self.set.arenas.simple_types.get(k).and_then(|t| t.name),
            TypeKey::Complex(k) => self.set.arenas.complex_types.get(k).and_then(|t| t.name),
        };
        name.map(|n| self.name(n).to_string())
            .unwrap_or_else(|| "(anonymous)".into())
    }

    // -----------------------------------------------------------------------
    // Child plans: a declaration's type plan with what the declaration adds.
    // -----------------------------------------------------------------------

    fn child_plan(&mut self, key: ElementKey) -> u32 {
        if let Some(&c) = self.child_memo.get(&key) {
            return c;
        }
        let id = self.plan.children.len() as u32;
        self.plan.children.push(ChildPlan {
            ty: NONE,
            flags: CHILD_UNCOVERED,
            value: NONE,
            unique: NONE,
        });
        self.child_memo.insert(key, id);
        self.building.insert(id);

        let set = self.set;
        let e = &set.arenas.elements[key];
        let mut flags = 0u8;
        let mut value = NONE;
        let label = self.element_label(key);
        if e.is_abstract {
            self.uncovered(format!("element {label}: abstract"));
            flags |= CHILD_UNCOVERED;
        }
        if !e.alternatives.is_empty() {
            self.uncovered(format!("element {label}: type alternatives"));
            flags |= CHILD_UNCOVERED;
        }
        if e.deferred_type_error.is_some() {
            self.uncovered(format!("element {label}: deferred type error"));
            flags |= CHILD_UNCOVERED;
        }
        if e.nillable {
            flags |= CHILD_NILLABLE;
        }
        if let Some(f) = &e.fixed_value {
            flags |= CHILD_FIXED;
            value = self.literal(f);
        } else if let Some(d) = &e.default_value {
            flags |= CHILD_DEFAULT;
            value = self.literal(d);
        }
        let mut unique = NONE;
        for &ic in &e.identity_constraints {
            match self.narrow_unique(ic) {
                Some(attr) if unique == NONE => {
                    self.plan.uniques.push(UniquePlan { attr });
                    unique = self.plan.uniques.len() as u32 - 1;
                }
                _ => {
                    self.uncovered(format!(
                        "element {label}: identity constraint beyond xs:unique ./* @attr"
                    ));
                    flags |= CHILD_UNCOVERED;
                }
            }
        }
        let ty = match set.declared_element_type(key) {
            Some(t) => self.type_plan(t),
            None => {
                self.uncovered(format!("element {label}: no type"));
                flags |= CHILD_UNCOVERED;
                NONE
            }
        };
        if flags & (CHILD_DEFAULT | CHILD_FIXED) != 0 && ty != NONE {
            // A value constraint is the value of an empty element: checked
            // like content, on simple content only. An `xs:ID` value would
            // count at document level for every such element.
            let tp = self.plan.types[ty as usize];
            let checkable = tp.content == Content::Simple
                && self
                    .plan
                    .checks
                    .get(tp.check as usize)
                    .is_some_and(|c| c.flags & CHECK_ID == 0);
            if !checkable {
                self.uncovered(format!(
                    "element {label}: a value constraint on content the plan does not check"
                ));
                flags |= CHILD_UNCOVERED;
            }
        }
        self.plan.children[id as usize] = ChildPlan {
            ty,
            flags,
            value,
            unique,
        };
        self.building.remove(&id);
        id
    }

    /// The attribute name id of an `xs:unique` with selector `./*` and one
    /// field `@attr` (unqualified), declared in place.
    fn narrow_unique(&mut self, ic: crate::ids::IdentityConstraintKey) -> Option<u32> {
        let d = self.set.arenas.identity_constraints.get(ic)?;
        if !matches!(d.kind, IdentityKind::Unique) || d.ref_name.is_some() || d.fields.len() != 1 {
            return None;
        }
        if d.selector.xpath.trim() != "./*" {
            return None;
        }
        let attr = d.fields[0].xpath.trim().strip_prefix('@')?;
        let ncname = attr.bytes().enumerate().all(|(i, b)| {
            b.is_ascii_alphabetic()
                || b == b'_'
                || (i > 0 && (b.is_ascii_digit() || b == b'-' || b == b'.'))
        });
        if attr.is_empty() || !ncname {
            return None;
        }
        let local = self.set.name_table.get(attr)?;
        Some(self.intern_name(None, local))
    }

    /// A narrow `xs:unique` compares attribute values as white-space
    /// normalized literals, which is value equality only for the string
    /// family and `xs:anyURI`, and sees defaulted attributes the interpreter
    /// does not: so every child type the element may contain must give the
    /// field's attribute such a type and no value constraint, or the element
    /// is uncovered.
    fn check_uniques(&mut self) {
        for c in 0..self.plan.children.len() {
            let child = self.plan.children[c];
            if child.unique == NONE || child.ty == NONE {
                continue;
            }
            let attr = self.plan.uniques[child.unique as usize].attr;
            let Some(&(s0, s1)) = self.type_states.get(&child.ty) else {
                continue; // no element children: the selector selects nothing
            };
            let mut bad: Option<String> = None;
            for s in s0..s1 {
                let st = self.plan.states[s as usize];
                for t in &self.plan.transitions[st.t_start as usize..st.t_end as usize] {
                    let ty = self.plan.children[t.child as usize].ty;
                    if ty == NONE {
                        continue;
                    }
                    let tp = self.plan.types[ty as usize];
                    for u in &self.plan.attrs[tp.attr_start as usize..tp.attr_end as usize] {
                        if u.name != attr {
                            continue;
                        }
                        let ch = self.plan.checks[u.check as usize];
                        let literal_is_value =
                            ch.kind == CheckKind::Any || self.literal_valued.contains(&u.check);
                        if !literal_is_value || self.valued_attrs.contains(&(ty, attr)) {
                            bad = Some(format!("{:?} {:?}", ch.kind, ch.ws));
                        }
                    }
                }
            }
            if let Some(why) = bad {
                self.plan.children[c].flags |= CHILD_UNCOVERED;
                let n = self.plan.names[attr as usize];
                let local = String::from_utf8_lossy(
                    &self.plan.bytes[n.start as usize..(n.start + n.len) as usize],
                )
                .into_owned();
                self.uncovered(format!(
                    "xs:unique over @{local}: a child type gives it a check that is no preserve \
                     string ({why}), or a value constraint"
                ));
            }
        }
    }

    // -----------------------------------------------------------------------
    // Type plans.
    // -----------------------------------------------------------------------

    fn type_plan(&mut self, tk: TypeKey) -> u32 {
        if let Some(&t) = self.type_memo.get(&tk) {
            return t;
        }
        let id = self.plan.types.len() as u32;
        self.plan.types.push(TypePlan {
            content: Content::Uncovered,
            start: NONE,
            check: NONE,
            attr_start: 0,
            attr_end: 0,
            required: 0,
        });
        self.type_memo.insert(tk, id);
        let ck = match tk {
            TypeKey::Simple(_) => {
                let check = self.check(tk);
                let t = &mut self.plan.types[id as usize];
                t.content = Content::Simple;
                t.check = check;
                return id;
            }
            TypeKey::Complex(ck) => ck,
        };
        let label = self.type_label(tk);
        let set = self.set;
        let ct = &set.arenas.complex_types[ck];
        if ck == set.any_type_key() || ct.target_namespace == self.xs {
            self.uncovered(format!("type {label}: xs:anyType"));
            return id;
        }
        if ct.is_abstract {
            self.uncovered(format!("type {label}: abstract"));
            return id;
        }
        if self.has_assertions(ck) {
            self.uncovered(format!("type {label}: assertions"));
            return id;
        }
        let Some((a0, a1, required)) = self.attribute_uses(ck, id) else {
            self.uncovered(format!("type {label}: attribute uses"));
            return id;
        };
        {
            let t = &mut self.plan.types[id as usize];
            t.attr_start = a0;
            t.attr_end = a1;
            t.required = required;
        }
        match determine_content_type(set, ct) {
            ContentType::Empty => self.plan.types[id as usize].content = Content::Empty,
            ContentType::TextOnly => {
                // Simple content: its value check (delegated in this stage).
                let check = self.check(tk);
                let t = &mut self.plan.types[id as usize];
                t.content = Content::Simple;
                t.check = check;
            }
            ContentType::Mixed => self.uncovered(format!("type {label}: mixed content")),
            ContentType::ElementOnly => match self.content_model(ck) {
                Ok(start) => {
                    let t = &mut self.plan.types[id as usize];
                    t.content = Content::ElementOnly;
                    t.start = start;
                    let n = self.plan.states.len() as u32;
                    self.type_states.insert(id, (start, n));
                }
                Err(why) => self.uncovered(format!("type {label}: {why}")),
            },
        }
        id
    }

    /// Whether the type or a base type has `xs:assert` (XSD 1.1).
    fn has_assertions(&self, ck: ComplexTypeKey) -> bool {
        let set = self.set;
        let mut cur = Some(ck);
        for _ in 0..100 {
            let Some(k) = cur else { return false };
            let ct = &set.arenas.complex_types[k];
            #[cfg(feature = "xsd11")]
            if !ct.assertions.is_empty() {
                return true;
            }
            if let crate::parser::frames::ComplexContentResult::Simple(sc) = &ct.content {
                if !sc.assertions.is_empty() {
                    return true;
                }
            }
            cur = match ct.resolved_base_type {
                Some(TypeKey::Complex(b)) if b != k && b != set.any_type_key() => Some(b),
                _ => None,
            };
        }
        true
    }

    /// The type's attribute uses, through the runtime's walk: (start, end,
    /// required bits), or `None` when uncovered.
    fn attribute_uses(&mut self, ck: ComplexTypeKey, type_id: u32) -> Option<(u32, u32, u64)> {
        let set = self.set;
        let mut uses = Vec::new();
        set.for_each_attribute_use(ck, |u| {
            uses.push((
                u.namespace,
                u.name,
                u.use_kind,
                u.type_key,
                u.value.map(str::to_string),
                u.fixed,
            ));
        });
        if uses.len() > 64 {
            return None;
        }
        let mut out = Vec::with_capacity(uses.len());
        let mut required = 0u64;
        let ct = &set.arenas.complex_types[ck];
        for (bit, (ns, local, use_kind, ty, value, fixed)) in uses.into_iter().enumerate() {
            // The walk drives required and defaulted attributes; a present
            // attribute is validated against the per-name lookup, which
            // differs from the walk in one XSD 1.0 corner (a prohibited use
            // with a fixed value). Take the use only where both agree on its
            // type and fixed value.
            let walk_fixed = if fixed { value.as_deref() } else { None };
            match set.find_attribute_in_type(ct, local, ns) {
                AttributeLookup::Found(_, found_ty, found_fixed, _, _)
                    if found_ty == ty && found_fixed.as_deref() == walk_fixed => {}
                _ => return None,
            }
            let name = self.intern_name(ns, local);
            let check = match ty {
                Some(TypeKey::Simple(_)) => self.check(ty?),
                _ => return None,
            };
            if let Some(v) = &value {
                // An absent attribute with a value constraint is still there
                // for the runtime: its value is validated, and an ID, IDREF,
                // ENTITY or QName value counts at document level. So the
                // constraint must be a value the plan could check itself and
                // that its type accepts.
                let c = self.plan.checks[check as usize];
                if c.kind == CheckKind::Uncovered
                    || c.flags & CHECK_ID != 0
                    || validate_simple_type(v, ty?, set).is_err()
                {
                    return None;
                }
                self.valued_attrs.insert((type_id, name));
            }
            if use_kind == AttributeUseKind::Required {
                required |= 1 << bit;
            }
            let fixed = match (fixed, value) {
                (true, Some(v)) => self.literal(&v),
                _ => NONE,
            };
            out.push(AttrUse {
                name,
                check,
                bit: bit as u8,
                fixed,
            });
        }
        // XSD 1.0 allows one ID attribute per element instance, and the
        // runtime counts them (ct-props-correct.5).
        let ids = out
            .iter()
            .filter(|u| self.plan.checks[u.check as usize].flags & CHECK_ID != 0)
            .count();
        if ids > 1 && self.version == XsdVersion::V1_0 {
            return None;
        }
        let start = self.plan.attrs.len() as u32;
        self.plan.attrs.extend(out);
        Some((start, self.plan.attrs.len() as u32, required))
    }

    /// Determinize the type's content model with the runtime's own
    /// content-model state as the oracle. Returns the start state.
    fn content_model(&mut self, ck: ComplexTypeKey) -> Result<u32, String> {
        let set = self.set;
        let ct = &set.arenas.complex_types[ck];
        let model = match self.validator.content_models.models.get(&ck) {
            Some(m) => m,
            None => return Err("no prepared content model".into()),
        };
        let nfa = match model {
            CompiledContentModel::Nfa { nfa, .. } => nfa.clone(),
            CompiledContentModel::NfaWithOpenContent { .. } => return Err("open content".into()),
            CompiledContentModel::AllGroup(_) => return Err("xs:all".into()),
        };
        let subst = self.validator.subst_groups.as_ref();

        // The alphabet: every element term's name. A name several terms carry
        // must lead to one child plan, whichever term the runtime matches.
        let mut symbols: Vec<(Option<NameId>, NameId)> = Vec::new();
        let mut decls: HashMap<(Option<NameId>, NameId), Vec<ElementKey>> = HashMap::new();
        for st in &nfa.states {
            match &st.term {
                Some(NfaTerm::Element {
                    name,
                    namespace,
                    element_key,
                    resolved_type,
                }) => {
                    let term = crate::validation::content::ElementMatchInfo {
                        element_key: *element_key,
                        resolved_type: *resolved_type,
                        process_contents: None,
                    };
                    let Some(decl) =
                        set.governing_element_declaration(*name, *namespace, Some(term), false)
                    else {
                        return Err("an element term with no declaration".into());
                    };
                    if subst.is_some_and(|m| m.contains_key(&decl)) {
                        return Err("substitution group".into());
                    }
                    let entry = decls.entry((*namespace, *name)).or_default();
                    if entry.is_empty() {
                        symbols.push((*namespace, *name));
                    }
                    if !entry.contains(&decl) {
                        entry.push(decl);
                    }
                }
                Some(NfaTerm::Wildcard { .. }) => return Err("wildcard".into()),
                None => {}
            }
        }
        let mut sym_child = Vec::with_capacity(symbols.len());
        let mut sym_name = Vec::with_capacity(symbols.len());
        for &(ns, local) in &symbols {
            let keys = decls[&(ns, local)].clone();
            let plans: Vec<u32> = keys.iter().map(|&k| self.child_plan(k)).collect();
            let first = plans[0];
            // A plan still being built is a placeholder, equal to nothing
            // yet: with several declarations for one name, every plan must
            // be finished to be compared.
            if plans.len() > 1 && plans.iter().any(|p| self.building.contains(p)) {
                return Err("one name, declarations that differ (one still being built)".into());
            }
            if plans[1..].iter().any(|&p| !self.same_child(first, p)) {
                return Err("one name, declarations that differ".into());
            }
            sym_child.push(first);
            sym_name.push(self.intern_name(ns, local));
        }

        let initial = ContentValidatorState::from_compiled(model);
        let mut keys: HashMap<String, u32> = HashMap::new();
        keys.insert(state_key(&initial)?, 0);
        let mut frontiers = vec![initial];
        let mut rows: Vec<Vec<(usize, u32)>> = Vec::new();
        let mut accept = Vec::new();
        let mut i = 0;
        while i < frontiers.len() {
            if frontiers.len() > DFA_STATE_LIMIT {
                return Err(format!("DFA over {DFA_STATE_LIMIT} states"));
            }
            accept.push(frontiers[i].is_complete());
            let mut row = Vec::new();
            for (si, &(ns, local)) in symbols.iter().enumerate() {
                let mut next = frontiers[i].clone();
                match next.try_advance_element(local, ns, ct.target_namespace, self.version, subst)
                {
                    Ok(Some(info)) => {
                        if info.process_contents.is_some() {
                            return Err("a wildcard match".into());
                        }
                        let decl = set.governing_element_declaration(local, ns, Some(info), false);
                        if decl.is_none_or(|d| !decls[&(ns, local)].contains(&d)) {
                            return Err("a match the alphabet does not explain".into());
                        }
                        let key = state_key(&next)?;
                        let id = match keys.get(&key) {
                            Some(&id) => id,
                            None => {
                                let id = frontiers.len() as u32;
                                keys.insert(key, id);
                                frontiers.push(next);
                                id
                            }
                        };
                        row.push((si, id));
                    }
                    Ok(None) => {}
                    Err(e) => return Err(format!("content-model limit: {e}")),
                }
            }
            rows.push(row);
            i += 1;
        }
        let base = self.plan.states.len() as u32;
        for (row, acc) in rows.into_iter().zip(accept) {
            let t_start = self.plan.transitions.len() as u32;
            for (si, next) in row {
                self.plan.transitions.push(Transition {
                    name: sym_name[si],
                    next: base + next,
                    child: sym_child[si],
                });
            }
            self.plan.states.push(StatePlan {
                t_start,
                t_end: self.plan.transitions.len() as u32,
                accept: acc,
            });
        }
        Ok(base)
    }

    /// Whether two child plans are the same in everything the interpreter
    /// reads.
    fn same_child(&self, a: u32, b: u32) -> bool {
        let (x, y) = (
            self.plan.children[a as usize],
            self.plan.children[b as usize],
        );
        let lit = |v: u32| (v != NONE).then(|| self.plan.literals[v as usize]);
        let bytes = |s: Option<Span>| s.map(|s| self.plan.as_ref().span(s).to_vec());
        let uniq = |u: u32| (u != NONE).then(|| self.plan.uniques[u as usize].attr);
        a == b
            || (x.ty == y.ty
                && x.flags == y.flags
                && bytes(lit(x.value)) == bytes(lit(y.value))
                && uniq(x.unique) == uniq(y.unique))
    }

    // -----------------------------------------------------------------------
    // Value checks.
    // -----------------------------------------------------------------------

    fn push_check(&mut self, c: CheckPlan) -> u32 {
        self.plan.checks.push(c);
        self.plan.checks.len() as u32 - 1
    }

    fn delegate(&mut self, tk: TypeKey) -> u32 {
        self.plan.delegates.push(tk);
        self.plan.delegates.len() as u32 - 1
    }

    fn simple_check(kind: CheckKind, delegate: u32) -> CheckPlan {
        CheckPlan {
            kind,
            lex: Lex::String,
            ws: Ws::Preserve,
            flags: 0,
            min_len: 0,
            max_len: NONE,
            enum_start: NONE,
            enum_end: NONE,
            pat_start: 0,
            pat_end: 0,
            bounds: NONE,
            next: NONE,
            delegate,
        }
    }

    fn check(&mut self, tk: TypeKey) -> u32 {
        if let Some(&c) = self.check_memo.get(&tk) {
            return c;
        }
        let delegate = self.delegate(tk);
        let plan = match tk {
            TypeKey::Complex(ck) => self.simple_content_check(ck, delegate),
            TypeKey::Simple(sk) => self.simple_type_check(sk, None, delegate),
        };
        if plan.kind != CheckKind::Value && plan.kind != CheckKind::Any {
            let label = self.type_label(tk);
            self.uncovered(format!("value type {label}: {:?}", plan.kind));
        }
        let id = self.push_check(plan);
        // A value of the string family or `xs:anyURI` is its normalized
        // literal: a narrow `xs:unique` may compare such values as bytes.
        if plan.kind == CheckKind::Value && plan.lex == Lex::String && plan.next == NONE {
            self.literal_valued.insert(id);
        }
        self.check_memo.insert(tk, id);
        id
    }

    /// The value check of a complex type with simple content, as the
    /// dynamic path validates such content (`validate_simple_type` on the
    /// complex type): along the derivation chain each restriction's own
    /// `whiteSpace` normalizes the value first, the content's simple type
    /// validates it, and then the facets each restriction declares are
    /// checked on that value, read by the simple type's built-in datatype.
    /// The facets of a restriction are not those of a simple type: their
    /// patterns are compiled when they are matched, as XSD 1.1 patterns.
    ///
    /// All of it becomes a chain of checks of the one value: the simple
    /// type's check, then one per restriction with facets, each normalizing
    /// with the strongest white space that applies to it (normalizations
    /// compose to the strongest of them). A content type whose values are
    /// IDs, references, QNames or notations — or a list or union, which may
    /// hold them — is not covered, nor a chain without a simple type.
    fn simple_content_check(&mut self, ck: ComplexTypeKey, delegate: u32) -> CheckPlan {
        use crate::parser::frames::{ComplexContentResult, DerivationMethod};
        let set = self.set;
        let mut cur = TypeKey::Complex(ck);
        let mut outer_ws = Ws::Preserve;
        let mut layers: Vec<ComplexTypeKey> = Vec::new();
        let mut value_type = None;
        for _ in 0..100 {
            match cur {
                TypeKey::Simple(sk) => {
                    value_type = Some(sk);
                    break;
                }
                TypeKey::Complex(k) => {
                    let Some(ct) = set.arenas.complex_types.get(k) else {
                        break;
                    };
                    if k == set.any_type_key() {
                        break;
                    }
                    let ComplexContentResult::Simple(sc) = &ct.content else {
                        break;
                    };
                    if sc.derivation == DerivationMethod::Restriction {
                        if let Some(w) = &sc.facets.whitespace {
                            outer_ws = strongest(outer_ws, ws_of(w.value));
                        }
                        if !sc.facets.is_empty() {
                            layers.push(k);
                        }
                    }
                    match ct.resolved_simple_content_type.or(ct.resolved_base_type) {
                        Some(next) if next != cur => cur = next,
                        _ => break,
                    }
                }
            }
        }
        let Some(sk) = value_type else {
            return Self::simple_check(CheckKind::Uncovered, delegate);
        };
        let mut head = self.simple_type_check(sk, Some(outer_ws), delegate);
        if head.kind != CheckKind::Value && head.kind != CheckKind::Any {
            // Uncovered stays uncovered (IDs, references, QNames, lists,
            // unions); anything else the delegating tier decides on the
            // whole content.
            return head;
        }
        if head.flags & CHECK_ID != 0 {
            return Self::simple_check(CheckKind::Uncovered, delegate);
        }
        let Some(code) = crate::validation::simple::resolve_type_code(sk, set) else {
            return Self::simple_check(CheckKind::Uncovered, delegate);
        };
        // The value the restrictions' facets see: normalized by the simple
        // type's white space after the restrictions' own.
        let inner_ws = strongest(outer_ws, head.ws);
        let mut next = NONE;
        for &k in layers.iter().rev() {
            let ComplexContentResult::Simple(sc) = &set.arenas.complex_types[k].content else {
                return Self::simple_check(CheckKind::Uncovered, delegate);
            };
            let facets = sc.facets.clone();
            let ws = match &facets.whitespace {
                Some(w) => strongest(inner_ws, ws_of(w.value)),
                None => strongest(inner_ws, default_ws(code)),
            };
            let mut layer = self.value_check(code, &facets, ws, XsdVersion::V1_1, false, delegate);
            match layer.kind {
                CheckKind::Value | CheckKind::Any => {}
                _ => return Self::simple_check(CheckKind::Delegate, delegate),
            }
            layer.next = next;
            next = self.push_check(layer);
        }
        if head.kind == CheckKind::Any && next != NONE {
            head.kind = CheckKind::Value;
        }
        head.next = next;
        head
    }

    /// The value check of a simple type: its effective facets and white
    /// space (`ws_floor`: a white space at least as strong is applied
    /// first, for simple content), as `validate_atomic_type` reads them.
    fn simple_type_check(
        &mut self,
        sk: SimpleTypeKey,
        ws_floor: Option<Ws>,
        delegate: u32,
    ) -> CheckPlan {
        let set = self.set;
        let Some(builtin) = self.builtin_of(sk) else {
            return Self::simple_check(CheckKind::Uncovered, delegate);
        };
        if matches!(
            builtin.as_str(),
            "IDREF" | "IDREFS" | "ENTITY" | "ENTITIES" | "NOTATION" | "QName"
        ) {
            return Self::simple_check(CheckKind::Uncovered, delegate);
        }
        let Some(code) = crate::validation::simple::resolve_type_code(sk, set) else {
            return Self::simple_check(CheckKind::Delegate, delegate);
        };
        let facets = set.effective_facets(sk);
        if matches!(
            code,
            XmlTypeCode::AnySimpleType | XmlTypeCode::AnyAtomicType
        ) {
            // No datatype: every value, as written.
            return if facets.is_empty() {
                Self::simple_check(CheckKind::Any, delegate)
            } else {
                Self::simple_check(CheckKind::Delegate, delegate)
            };
        }
        let ws = ws_of(crate::validation::simple::atomic_whitespace(sk, set));
        let ws = match ws_floor {
            Some(w) => strongest(w, ws),
            None => ws,
        };
        let id = code == XmlTypeCode::Id;
        self.value_check(code, &facets, ws, self.version, id, delegate)
    }

    /// A `Value` check of a value of the built-in datatype `code` against
    /// `facets`, normalized with `ws`, patterns read as patterns of
    /// `pattern_version`. Any facet the check cannot carry exactly leaves it
    /// to the delegating tier.
    fn value_check(
        &mut self,
        code: XmlTypeCode,
        facets: &FacetSet,
        ws: Ws,
        pattern_version: XsdVersion,
        id: bool,
        delegate: u32,
    ) -> CheckPlan {
        use XmlTypeCode as C;
        // What the plan cannot check itself goes to the delegating tier,
        // except an `xs:ID` value: the runtime counts it at document level
        // (also as an absent attribute's default), which a delegated check
        // does not see, so such a type is uncovered.
        let fallback = Self::simple_check(
            if id {
                CheckKind::Uncovered
            } else {
                CheckKind::Delegate
            },
            delegate,
        );
        let (lex, mut flags) = match code {
            C::String | C::NormalizedString | C::Token | C::AnyUri => (Lex::String, 0),
            C::Name => (Lex::Name, 0),
            C::NCName => (Lex::NCName, 0),
            C::Id => (Lex::NCName, 0),
            C::NmToken => (Lex::NmToken, 0),
            C::Language => (Lex::Language, 0),
            C::Integer
            | C::Long
            | C::Int
            | C::Short
            | C::Byte
            | C::NonNegativeInteger
            | C::PositiveInteger
            | C::NonPositiveInteger
            | C::NegativeInteger
            | C::UnsignedLong
            | C::UnsignedInt
            | C::UnsignedShort
            | C::UnsignedByte => (Lex::Integer, 0),
            C::Decimal => (Lex::Decimal, 0),
            C::Boolean => (Lex::Boolean, CHECK_RAW_PATTERNS),
            C::Float => (Lex::Float, CHECK_RAW_PATTERNS),
            C::Double => (Lex::Double, CHECK_RAW_PATTERNS),
            C::DateTime => (Lex::DateTime, CHECK_RAW_PATTERNS),
            C::DateTimeStamp => (Lex::DateTime, CHECK_RAW_PATTERNS | CHECK_TZ_REQUIRED),
            C::Date => (Lex::Date, CHECK_RAW_PATTERNS),
            C::Time => (Lex::Time, CHECK_RAW_PATTERNS),
            C::GYearMonth => (Lex::GYearMonth, CHECK_RAW_PATTERNS),
            C::GYear => (Lex::GYear, CHECK_RAW_PATTERNS),
            C::GMonthDay => (Lex::GMonthDay, CHECK_RAW_PATTERNS),
            C::GDay => (Lex::GDay, CHECK_RAW_PATTERNS),
            C::GMonth => (Lex::GMonth, CHECK_RAW_PATTERNS),
            C::Duration => (Lex::Duration, CHECK_RAW_PATTERNS),
            C::YearMonthDuration => (Lex::YearMonthDuration, CHECK_RAW_PATTERNS),
            C::DayTimeDuration => (Lex::DayTimeDuration, CHECK_RAW_PATTERNS),
            C::HexBinary => (Lex::HexBinary, 0),
            C::Base64Binary => (Lex::Base64Binary, 0),
            _ => return fallback,
        };
        if id {
            flags |= CHECK_ID;
        }
        if self.version == XsdVersion::V1_0 {
            flags |= CHECK_XSD10;
        }
        // XSD 1.1 assertions bind `$value`; the plan has no XPath.
        if !facets.assertions.is_empty() {
            return fallback;
        }
        let stringlike = matches!(
            lex,
            Lex::String | Lex::Name | Lex::NCName | Lex::NmToken | Lex::Language
        );
        let binary = matches!(lex, Lex::HexBinary | Lex::Base64Binary);
        let numeric = matches!(lex, Lex::Integer | Lex::Decimal);
        let timed = matches!(lex, Lex::DateTime | Lex::Date | Lex::Time);
        let has_length =
            facets.length.is_some() || facets.min_length.is_some() || facets.max_length.is_some();
        let has_range = facets.min_inclusive.is_some()
            || facets.max_inclusive.is_some()
            || facets.min_exclusive.is_some()
            || facets.max_exclusive.is_some();
        let has_digits = facets.total_digits.is_some() || facets.fraction_digits.is_some();
        if (has_length && !(stringlike || binary))
            || (has_range && !(numeric || timed))
            || (has_digits && !numeric)
        {
            return fallback;
        }
        if let Some(tz) = &facets.explicit_timezone {
            use crate::types::facets::ExplicitTimezone as E;
            let dated = timed
                || matches!(
                    lex,
                    Lex::GYearMonth | Lex::GYear | Lex::GMonthDay | Lex::GDay | Lex::GMonth
                );
            if !dated {
                return fallback;
            }
            match tz.value {
                E::Required => flags |= CHECK_TZ_REQUIRED,
                E::Prohibited => flags |= CHECK_TZ_PROHIBITED,
                E::Optional => {}
            }
        }

        // Lengths.
        let mut min = 0u64;
        let mut max = u64::MAX;
        if let Some(l) = &facets.length {
            min = min.max(l.value);
            max = max.min(l.value);
        }
        if let Some(l) = &facets.min_length {
            min = min.max(l.value);
        }
        if let Some(l) = &facets.max_length {
            max = max.min(l.value);
        }
        if min > u64::from(u32::MAX - 1) || (max != u64::MAX && max >= u64::from(NONE)) {
            return fallback;
        }
        let max_len = if max >= u64::from(NONE) {
            NONE
        } else {
            max as u32
        };

        // Range and digit facets.
        let mut bounds = NONE;
        if numeric || has_range {
            let Some(b) = self.bounds(code, lex, facets) else {
                return fallback;
            };
            self.plan.bounds.push(b);
            bounds = self.plan.bounds.len() as u32 - 1;
        }

        // Enumeration: the literals as written, sorted.
        let (enum_start, enum_end) = match &facets.enumeration {
            Some(e) => {
                let mut values: Vec<&String> = e.values.iter().collect();
                values.sort();
                let lits: Vec<u32> = values.into_iter().map(|v| self.literal(v)).collect();
                let start = self.plan.enum_literals.len() as u32;
                self.plan.enum_literals.extend(lits);
                (start, self.plan.enum_literals.len() as u32)
            }
            None => (NONE, NONE),
        };

        // Patterns.
        let steps: Vec<Vec<String>> = facets
            .patterns
            .iter()
            .filter(|s| !s.is_empty())
            .map(|s| s.iter().map(|p| p.value.clone()).collect())
            .collect();
        let built: Vec<PatternStep> = steps
            .into_iter()
            .map(|s| self.pattern_step(s, pattern_version))
            .collect();
        let pat_start = self.plan.pattern_steps.len() as u32;
        self.plan.pattern_steps.extend(built);
        let pat_end = self.plan.pattern_steps.len() as u32;

        let plain = lex == Lex::String
            && ws == Ws::Preserve
            && min == 0
            && max_len == NONE
            && enum_start == NONE
            && pat_start == pat_end
            && !id;
        CheckPlan {
            kind: if plain {
                CheckKind::Any
            } else {
                CheckKind::Value
            },
            lex,
            ws,
            flags,
            min_len: min as u32,
            max_len,
            enum_start,
            enum_end,
            pat_start,
            pat_end,
            bounds,
            next: NONE,
            delegate,
        }
    }

    /// The range and digit facets of a check, with the built-in range of an
    /// integer type; `None` when one of them is a literal the plan does not
    /// read exactly, or when dates disagree on having a time zone.
    fn bounds(&mut self, code: XmlTypeCode, lex: Lex, facets: &FacetSet) -> Option<Bounds> {
        use crate::types::facets::normalize_whitespace;
        use XmlTypeCode as C;
        let (int_lo, int_hi) = match code {
            C::Long => (i128::from(i64::MIN), i128::from(i64::MAX)),
            C::Int => (i128::from(i32::MIN), i128::from(i32::MAX)),
            C::Short => (i128::from(i16::MIN), i128::from(i16::MAX)),
            C::Byte => (i128::from(i8::MIN), i128::from(i8::MAX)),
            C::NonNegativeInteger => (0, i128::MAX),
            C::PositiveInteger => (1, i128::MAX),
            C::NonPositiveInteger => (i128::MIN, 0),
            C::NegativeInteger => (i128::MIN, -1),
            C::UnsignedLong => (0, i128::from(u64::MAX)),
            C::UnsignedInt => (0, i128::from(u32::MAX)),
            C::UnsignedShort => (0, i128::from(u16::MAX)),
            C::UnsignedByte => (0, i128::from(u8::MAX)),
            _ => (i128::MIN, i128::MAX),
        };
        let mut zoned: Option<bool> = None;
        let mut bound = |literal: Option<&String>, this: &mut Self| -> Option<Bound> {
            let Some(literal) = literal else {
                return Some(Bound::NONE);
            };
            let lit = normalize_whitespace(literal, crate::types::facets::WhitespaceMode::Collapse);
            match lex {
                Lex::Integer | Lex::Decimal => {
                    if !crate::types::validators::is_valid_xsd_decimal_lexical(&lit) {
                        return None;
                    }
                    let (mant, scale, _, _) = super::values::read_decimal(&lit)?;
                    Some(Bound {
                        set: true,
                        mant,
                        scale,
                        frac: Span { start: 0, len: 0 },
                    })
                }
                Lex::DateTime | Lex::Date | Lex::Time => {
                    let (whole, frac, tz) = super::values::read_moment(&lit, lex)?;
                    if *zoned.get_or_insert(tz) != tz {
                        return None;
                    }
                    let frac = this.push_bytes(frac.as_bytes());
                    Some(Bound {
                        set: true,
                        mant: whole,
                        scale: 0,
                        frac,
                    })
                }
                _ => None,
            }
        };
        let min_incl = bound(facets.min_inclusive.as_ref().map(|f| &f.value), self)?;
        let min_excl = bound(facets.min_exclusive.as_ref().map(|f| &f.value), self)?;
        let max_incl = bound(facets.max_inclusive.as_ref().map(|f| &f.value), self)?;
        let max_excl = bound(facets.max_exclusive.as_ref().map(|f| &f.value), self)?;
        Some(Bounds {
            min_incl,
            min_excl,
            max_incl,
            max_excl,
            int_lo,
            int_hi,
            total_digits: facets.total_digits.as_ref().map_or(NONE, |f| f.value),
            fraction_digits: facets.fraction_digits.as_ref().map_or(NONE, |f| f.value),
            zoned: zoned.unwrap_or(false),
        })
    }

    /// The built-in datatype a simple type is derived from (its nearest
    /// ancestor in the XML Schema namespace), with every step atomic.
    fn builtin_of(&self, sk: SimpleTypeKey) -> Option<String> {
        let set = self.set;
        let mut cur = sk;
        for _ in 0..100 {
            let d = set.arenas.simple_types.get(cur)?;
            if !matches!(d.variety, SimpleTypeVariety::Atomic) {
                return None;
            }
            if d.target_namespace == self.xs && d.name.is_some() {
                return d.name.map(|n| self.name(n).to_string());
            }
            match d.resolved_base_type {
                Some(TypeKey::Simple(b)) if b != cur => cur = b,
                _ => return None,
            }
        }
        None
    }

    fn pattern_step(&mut self, patterns: Vec<String>, version: XsdVersion) -> PatternStep {
        let key = (patterns, version);
        if let Some(&s) = self.step_memo.get(&key) {
            return s;
        }
        let patterns = key.0;
        let refs: Vec<&str> = patterns.iter().map(String::as_str).collect();
        let mut refused = None;
        let alphabet = match step_alphabet(&refs, version) {
            Ok(a) => self.intern_alphabet(&a),
            Err(e) => {
                refused = Some(format!("{e:?}"));
                self.intern_alphabet(&ComposedAlphabet {
                    ascii_lo: 0,
                    ascii_hi: 0,
                    non_ascii: false,
                    excluded: Vec::new(),
                })
            }
        };
        let table = if refused.is_some() {
            NONE
        } else if self.set.regex_compatibility() != RegexCompat::Strict {
            refused = Some("lenient regex mode".into());
            NONE
        } else {
            match build_table(&refs, version) {
                Ok(t) => {
                    let classes = self.push_bytes(&t.classes).start;
                    let accept: Vec<u8> = t.accept.iter().map(|&a| u8::from(a)).collect();
                    let accept = self.push_bytes(&accept).start;
                    let trans = self.plan.pattern_trans.len() as u32;
                    self.plan.pattern_trans.extend_from_slice(&t.trans);
                    self.plan.patterns.push(PatternTable {
                        classes,
                        stride: t.stride as u32,
                        trans,
                        states: t.accept.len() as u32,
                        start: t.start,
                        accept,
                    });
                    self.report.patterns_built += 1;
                    self.plan.patterns.len() as u32 - 1
                }
                Err(e) => {
                    refused = Some(e);
                    NONE
                }
            }
        };
        if let Some(why) = refused {
            self.report
                .patterns_refused
                .push(format!("{patterns:?}: {why}"));
        }
        let step = PatternStep { table, alphabet };
        self.step_memo.insert((patterns, version), step);
        step
    }

    fn intern_alphabet(&mut self, a: &ComposedAlphabet) -> u32 {
        let key = (a.ascii_lo, a.ascii_hi, a.non_ascii, a.excluded.clone());
        if let Some(&id) = self.alphabet_memo.get(&key) {
            return id;
        }
        let excl_start = self.plan.excluded.len() as u32;
        self.plan.excluded.extend_from_slice(&a.excluded);
        self.plan.alphabets.push(Alphabet {
            ascii_lo: a.ascii_lo,
            ascii_hi: a.ascii_hi,
            non_ascii: a.non_ascii,
            excl_start,
            excl_end: self.plan.excluded.len() as u32,
        });
        let id = self.plan.alphabets.len() as u32 - 1;
        self.alphabet_memo.insert(key, id);
        id
    }
}

/// The stronger of two white-space normalizations: normalizing with one and
/// then the other is normalizing with the stronger.
fn strongest(a: Ws, b: Ws) -> Ws {
    match (a, b) {
        (Ws::Collapse, _) | (_, Ws::Collapse) => Ws::Collapse,
        (Ws::Replace, _) | (_, Ws::Replace) => Ws::Replace,
        _ => Ws::Preserve,
    }
}

/// The white space a built-in datatype's validator applies when no
/// `whiteSpace` facet says otherwise.
fn default_ws(code: XmlTypeCode) -> Ws {
    match crate::types::validators::VALIDATOR_REGISTRY.get_by_code(code) {
        Some(v) => ws_of(v.whitespace()),
        None => Ws::Preserve,
    }
}

fn ws_of(mode: WhitespaceMode) -> Ws {
    match mode {
        WhitespaceMode::Preserve => Ws::Preserve,
        WhitespaceMode::Replace => Ws::Replace,
        WhitespaceMode::Collapse => Ws::Collapse,
    }
}

/// A canonical key of a content-model state: equal keys, equal states. The
/// configurations of the counted forms are sets, so their entries are sorted
/// (their iteration order carries no meaning); every other field is kept.
fn state_key(state: &ContentValidatorState) -> Result<String, String> {
    use crate::compiler::ActiveStates;
    let ContentValidatorState::Nfa {
        active_states,
        open_content: None,
        ..
    } = state
    else {
        return Err("a content-model state other than a plain NFA".into());
    };
    let sorted = |mut v: Vec<String>| {
        v.sort();
        v
    };
    Ok(match active_states {
        ActiveStates::Simple(s) => format!("S{s:?}"),
        ActiveStates::Counted {
            configs,
            num_counters,
        } => format!(
            "C{num_counters}{:?}",
            sorted(configs.iter().map(|c| format!("{c:?}")).collect())
        ),
        ActiveStates::RangedSingle {
            state_ranges,
            counter_def,
        } => format!(
            "R{counter_def:?}{:?}",
            sorted(
                state_ranges
                    .iter()
                    .map(|(k, r)| format!("{k:?}:{r:?}"))
                    .collect()
            )
        ),
        ActiveStates::Hybrid {
            configs,
            ranged_counter_idx,
            num_counters,
        } => format!(
            "H{ranged_counter_idx},{num_counters}{:?}",
            sorted(
                configs
                    .iter()
                    .map(|(k, r)| format!("{k:?}:{r:?}"))
                    .collect()
            )
        ),
    })
}
