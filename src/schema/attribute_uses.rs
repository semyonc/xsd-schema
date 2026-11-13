//! The {attribute uses} of a complex type (Structures §3.4.2.4), as one walk
//! over the resolved [`SchemaSet`].
//!
//! The validation runtime and the validation plan both read a type's effective
//! attribute uses through [`SchemaSet::for_each_attribute_use`], so the rule is
//! implemented once.

use std::collections::HashSet;

use crate::arenas::{ComplexTypeDefData, ResolvedAttributeUse};
use crate::ids::{AttributeGroupKey, AttributeKey, ComplexTypeKey, NameId, TypeKey};
use crate::namespace::table::well_known;
use crate::parser::frames::{AttributeUseKind, AttributeUseResult};
use crate::parser::location::SourceRef;
use crate::schema::SchemaSet;
use crate::validation::context::AttrNameSet;

// ---------------------------------------------------------------------------
// GroupAttribute — flat representation of an attribute from an attribute group
// ---------------------------------------------------------------------------

/// An attribute use collected from a resolved attribute group.
pub(crate) struct GroupAttribute {
    pub(crate) name: NameId,
    pub(crate) namespace: Option<NameId>,
    pub(crate) use_kind: AttributeUseKind,
    pub(crate) type_key: Option<TypeKey>,
    pub(crate) attr_key: Option<AttributeKey>,
    pub(crate) fixed_value: Option<String>,
    /// The declaration the fixed value was written on (its namespace
    /// bindings read a QName value).
    pub(crate) fixed_source: Option<SourceRef>,
    /// The lexical form of the use's ·effective value constraint· (`default`
    /// or `fixed`): the use's own, otherwise its declaration's.
    pub(crate) value: Option<String>,
    /// The element that value was written on (its namespace bindings read a
    /// QName value).
    pub(crate) value_source: Option<SourceRef>,
    #[cfg(feature = "xsd11")]
    pub(crate) inheritable: bool,
}

// ---------------------------------------------------------------------------
// AttributeUseEntry — one member of a complex type's {attribute uses}
// ---------------------------------------------------------------------------

/// One member of a complex type's {attribute uses} (Structures §3.4.2.4), as
/// [`SchemaSet::for_each_attribute_use`] reports it: never a
/// prohibited `<attribute>`, which "does not correspond to any component".
pub(crate) struct AttributeUseEntry<'s> {
    pub(crate) name: NameId,
    pub(crate) namespace: Option<NameId>,
    /// `{required}`: `Required` or `Optional`.
    pub(crate) use_kind: AttributeUseKind,
    /// The global declaration, when the use references one.
    pub(crate) attr_key: Option<AttributeKey>,
    /// The use's resolved type, otherwise its declaration's.
    pub(crate) type_key: Option<TypeKey>,
    /// The lexical form of the use's ·effective value constraint· — its own
    /// `default` / `fixed`, otherwise its declaration's.
    pub(crate) value: Option<&'s str>,
    /// The element `value` was written on: its namespace bindings read a
    /// QName value.
    pub(crate) value_source: Option<&'s SourceRef>,
    /// The use's `inheritable` (XSD 1.1).
    #[cfg(feature = "xsd11")]
    pub(crate) inheritable: bool,
}

/// A set of attribute names for one walk of the attribute uses
/// ([`SchemaSet::for_each_attribute_use`]): held inline while small,
/// so the walk allocates nothing for the common type, and in a hash set past
/// [`AttrNames::INLINE`] names.
pub(crate) struct AttrNames {
    inline: [(Option<NameId>, NameId); AttrNames::INLINE],
    len: usize,
    spilled: AttrNameSet,
}

impl AttrNames {
    pub(crate) const INLINE: usize = 16;

    pub(crate) fn new() -> Self {
        AttrNames {
            inline: [(None, NameId(0)); AttrNames::INLINE],
            len: 0,
            spilled: AttrNameSet::default(),
        }
    }

    pub(crate) fn contains(&self, name: &(Option<NameId>, NameId)) -> bool {
        self.inline[..self.len].contains(name) || self.spilled.contains(name)
    }

    /// Add `name`; `false` when it was already there.
    pub(crate) fn insert(&mut self, name: (Option<NameId>, NameId)) -> bool {
        if self.contains(&name) {
            return false;
        }
        if self.len < AttrNames::INLINE {
            self.inline[self.len] = name;
            self.len += 1;
        } else {
            self.spilled.insert(name);
        }
        true
    }

    /// Move every name of `other` into this set, leaving `other` empty.
    pub(crate) fn absorb(&mut self, other: &mut AttrNames) {
        for i in 0..other.len {
            self.insert(other.inline[i]);
        }
        for name in other.spilled.drain() {
            self.insert(name);
        }
        other.len = 0;
    }

    pub(crate) fn clear(&mut self) {
        self.len = 0;
        self.spilled.clear();
    }
}

impl SchemaSet {
    /// Collect all attribute uses from resolved attribute groups (recursively).
    pub(crate) fn collect_group_attributes(
        &self,
        ct_data: &ComplexTypeDefData,
    ) -> Vec<GroupAttribute> {
        let mut result = Vec::new();
        let mut visited = HashSet::new();
        for &group_key in &ct_data.resolved_attribute_groups {
            self.collect_group_attributes_recursive(group_key, &mut result, &mut visited);
        }
        result
    }

    fn collect_group_attributes_recursive(
        &self,
        group_key: AttributeGroupKey,
        result: &mut Vec<GroupAttribute>,
        visited: &mut HashSet<AttributeGroupKey>,
    ) {
        if !visited.insert(group_key) {
            return; // prevent infinite recursion on circular refs
        }
        let group_data = match self.arenas.get_attribute_group(group_key) {
            Some(g) => g,
            None => return,
        };
        for (i, attr_use) in group_data.attributes.iter().enumerate() {
            let resolved = group_data.resolved_attributes.get(i);
            let attr_key = resolved.and_then(|r| r.resolved_ref);
            // For `<xs:attribute ref="x:foo"/>` uses, the use's own
            // `resolved_type` is `None` — the type lives on the global
            // declaration. Fall back to the resolved type recorded on the
            // global decl, otherwise simple-type validation gets skipped
            // (the W3C `xsd003b` fixture exercises this through a redefined
            // `simpleType` whose enum-restricted facet must be applied to
            // an attribute reached via an attribute group).
            let attr_type = resolved.and_then(|r| r.resolved_type).or_else(|| {
                attr_key
                    .and_then(|k| self.arenas.attributes.get(k))
                    .and_then(|d| d.resolved_type)
            });
            let (name, namespace) =
                self.resolve_attr_use_name_ns(attr_use, resolved, group_data.target_namespace);
            let (fixed_value, fixed_source) = self.attribute_use_fixed(attr_use, attr_key);
            let (value, value_source) = self.effective_value_constraint(attr_use, attr_key);
            result.push(GroupAttribute {
                name,
                namespace,
                use_kind: attr_use.use_kind,
                type_key: attr_type,
                attr_key,
                fixed_value,
                fixed_source,
                value: value.map(str::to_string),
                value_source: value_source.cloned(),
                #[cfg(feature = "xsd11")]
                inheritable: attr_use
                    .use_inheritable(attr_key.and_then(|k| self.arenas.attributes.get(k))),
            });
        }
        for &nested_key in &group_data.resolved_attribute_groups {
            self.collect_group_attributes_recursive(nested_key, result, visited);
        }
    }

    /// Visit every member of the {attribute uses} of the complex type
    /// `ct_key`, in order: its own `<attribute>` uses, its attribute groups'
    /// uses, then those inherited from each base type down to `xs:anyType`.
    ///
    /// Structures §3.4.2.4 (Mapping Rule for Attribute Uses Property): "1 The
    /// set of attribute uses corresponding to the <attribute> \[children\], if
    /// any. 2 The {attribute uses} of the attribute groups … 3 The attribute
    /// uses "inherited" from the {base type definition} T" — for an extension
    /// (3.1) all of T's; for a restriction (3.2) all except those whose name is
    /// "3.2.1 the expanded name of the {attribute declaration} of an attribute
    /// use which has already been included in the set, following the rules in
    /// clause 1 or clause 2 above; 3.2.2 the expanded name of the {attribute
    /// declaration} of what would have been an attribute use corresponding to
    /// an <attribute> \[child\], if the <attribute> had not had use =
    /// prohibited." So a prohibition only stops a restriction's inheritance:
    /// it never hides a use of the type itself, an extension's is "simply
    /// ignored", and an attribute group's prohibited `<attribute>` is no use
    /// at all (XSD 1.0 §3.2.2: it "corresponds to nothing at all"; W3C
    /// Bugzilla #4043). The set is cumulative down the base chain, as the
    /// clause composes. A name is reported once.
    ///
    /// The one walk behind every runtime consumer of the attribute uses: the
    /// required-attribute check, the defaulted attributes (identity-constraint
    /// fields, ID/IDREF, `get_default_attributes`), the inheritable defaults
    /// and `get_expected_attributes`. The runtime's `find_attribute_in_type`
    /// looks one name up by the same rule, and the validation plan builds its
    /// attribute tables from this walk, so both see the same effective uses.
    pub(crate) fn for_each_attribute_use(
        &self,
        ct_key: ComplexTypeKey,
        mut visit: impl FnMut(&AttributeUseEntry<'_>),
    ) {
        let any_type = self.any_type_key();
        // Names a more derived restriction keeps out of what its base types
        // contribute (clauses 3.2.1 and 3.2.2).
        let mut not_inherited = AttrNames::new();
        let mut own = AttrNames::new();
        let mut prohibited = AttrNames::new();
        let mut current = Some(ct_key);
        while let Some(key) = current.filter(|&key| key != any_type) {
            let ct_data = &self.arenas.complex_types[key];
            let base = match ct_data.resolved_base_type {
                Some(TypeKey::Complex(base)) if base != any_type => Some(base),
                _ => None,
            };
            let groups = self.collect_group_attributes(ct_data);
            // The names are needed only to keep a group use from repeating a
            // use of this type, and to stop inheritance from a base still to
            // come — not for the common type with neither.
            let track = base.is_some() || !groups.is_empty();
            own.clear();
            prohibited.clear();

            for (i, attr_use) in ct_data.attributes.iter().enumerate() {
                let resolved = ct_data.resolved_attributes.get(i);
                let (name, namespace) =
                    self.resolve_attr_use_name_ns(attr_use, resolved, ct_data.target_namespace);
                if attr_use.use_kind == AttributeUseKind::Prohibited {
                    if base.is_some() {
                        prohibited.insert((namespace, name));
                    }
                    continue;
                }
                if not_inherited.contains(&(namespace, name))
                    || (track && !own.insert((namespace, name)))
                {
                    continue;
                }
                let attr_key = resolved.and_then(|r| r.resolved_ref);
                let decl = attr_key.and_then(|k| self.arenas.attributes.get(k));
                let (value, value_source) = self.effective_value_constraint(attr_use, attr_key);
                visit(&AttributeUseEntry {
                    name,
                    namespace,
                    use_kind: attr_use.use_kind,
                    attr_key,
                    type_key: resolved
                        .and_then(|r| r.resolved_type)
                        .or_else(|| decl.and_then(|d| d.resolved_type)),
                    value,
                    value_source,
                    #[cfg(feature = "xsd11")]
                    inheritable: attr_use.use_inheritable(decl),
                });
            }

            for ga in &groups {
                if ga.use_kind == AttributeUseKind::Prohibited
                    || not_inherited.contains(&(ga.namespace, ga.name))
                    || !own.insert((ga.namespace, ga.name))
                {
                    continue;
                }
                visit(&AttributeUseEntry {
                    name: ga.name,
                    namespace: ga.namespace,
                    use_kind: ga.use_kind,
                    attr_key: ga.attr_key,
                    type_key: ga.type_key,
                    value: ga.value.as_deref(),
                    value_source: ga.value_source.as_ref(),
                    #[cfg(feature = "xsd11")]
                    inheritable: ga.inheritable,
                });
            }

            if base.is_some() {
                // Clause 3: an extension inherits every use of its base (3.1:
                // its own names cannot repeat them, and a prohibition there is
                // ignored); a restriction none of the names it includes or
                // prohibits (3.2).
                not_inherited.absorb(&mut own);
                if !matches!(
                    ct_data.derivation_method,
                    Some(crate::parser::frames::DerivationMethod::Extension)
                ) {
                    not_inherited.absorb(&mut prohibited);
                }
            }
            current = base;
        }
    }

    /// Resolve the effective name and namespace for an attribute use.
    ///
    /// For inline attributes, returns the name directly from the use.
    /// For `<xs:attribute ref="..."/>`, resolves through the global declaration.
    ///
    /// `fallback_namespace` is used when the attribute's source document is
    /// unavailable (e.g. synthesized attributes); callers should pass the
    /// containing type's or group's target namespace.
    pub(crate) fn resolve_attr_use_name_ns(
        &self,
        attr_use: &AttributeUseResult,
        resolved: Option<&ResolvedAttributeUse>,
        fallback_namespace: Option<NameId>,
    ) -> (NameId, Option<NameId>) {
        if let Some(name) = attr_use.attribute.name {
            if attr_use.attribute.ref_name.is_none() {
                // Inline local attribute: apply form / attributeFormDefault
                let ns = self.effective_local_attribute_namespace(
                    attr_use.attribute.target_namespace,
                    attr_use.attribute.form.as_deref(),
                    attr_use.attribute.source.as_ref(),
                    fallback_namespace,
                );
                return (name, ns);
            }
            // ref with name — fall through to ref resolution
        }
        if let Some(attr_key) = resolved.and_then(|r| r.resolved_ref) {
            if let Some(decl) = self.arenas.attributes.get(attr_key) {
                if let Some(name) = decl.name {
                    return (name, decl.target_namespace);
                }
            }
        }
        (well_known::EMPTY, None)
    }

    // Effective-attribute-wildcard helpers superseded by
    // `crate::schema::derivation::compute_runtime_attribute_wildcard`,
    // which implements full §3.6.2.2 (own + groups intersection) plus
    // §3.4.2.5 (extension union over the base chain) in canonical form.

    /// The lexical form of an attribute use's ·effective value constraint·
    /// with the element it was written on. Structures §3.4.4.2: "The
    /// effective value constraint of an attribute use U is U.{value
    /// constraint}, if present, otherwise U.{attribute declaration}.{value
    /// constraint}, if present, otherwise the effective value constraint is
    /// ·absent·" — the use's own `default` / `fixed` first, for a reference
    /// in a type or in an attribute group alike.
    pub(crate) fn effective_value_constraint<'s>(
        &'s self,
        attr_use: &'s AttributeUseResult,
        attr_key: Option<AttributeKey>,
    ) -> (Option<&'s str>, Option<&'s SourceRef>) {
        let own = &attr_use.attribute;
        if let Some(value) = own.default_value.as_deref().or(own.fixed_value.as_deref()) {
            return (Some(value), own.source.as_ref());
        }
        match attr_key.and_then(|k| self.arenas.attributes.get(k)) {
            Some(decl) => match decl
                .default_value
                .as_deref()
                .or(decl.fixed_value.as_deref())
            {
                Some(value) => (Some(value), decl.source.as_ref()),
                None => (None, None),
            },
            None => (None, None),
        }
    }

    /// An attribute use's fixed value — its own, otherwise its declaration's —
    /// with the source of the element it was written on.
    pub(crate) fn attribute_use_fixed(
        &self,
        attr_use: &crate::parser::frames::AttributeUseResult,
        attr_key: Option<AttributeKey>,
    ) -> (Option<String>, Option<SourceRef>) {
        if let Some(fixed) = &attr_use.attribute.fixed_value {
            return (Some(fixed.clone()), attr_use.attribute.source.clone());
        }
        match attr_key.and_then(|k| self.arenas.attributes.get(k)) {
            Some(decl) => (decl.fixed_value.clone(), decl.source.clone()),
            None => (None, None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The name set of the attribute-use walk: inline up to `AttrNames::INLINE`
    /// names, spilled to a hash set past that, the same answers either way.
    #[test]
    fn attribute_name_sets_spill_past_the_inline_capacity() {
        let name = |i: u32| (if i < 25 { None } else { Some(NameId(1000)) }, NameId(i));
        let mut set = AttrNames::new();
        for i in 0..40 {
            assert!(set.insert(name(i)), "{i}");
            assert!(!set.insert(name(i)), "{i} twice");
        }
        assert!((0..40).all(|i| set.contains(&name(i))));
        assert!(!set.contains(&name(40)));
        let mut other = AttrNames::new();
        for i in 30..60 {
            other.insert(name(i));
        }
        set.absorb(&mut other);
        assert!((0..60).all(|i| set.contains(&name(i))));
        assert!(!other.contains(&name(30)) && !other.contains(&name(59)));
        set.clear();
        assert!(!(0..60).any(|i| set.contains(&name(i))));
        assert!(set.insert(name(5)));
    }
}
