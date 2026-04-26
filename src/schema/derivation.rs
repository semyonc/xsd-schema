//! Type derivation validation
//!
//! This module validates type derivation rules according to the XSD specification.
//! It is run after reference resolution (Task 3.1) and dependency graph construction
//! (Task 3.2), using the topological order to process types in correct order.
//!
//! # Validation Rules
//!
//! ## Simple Type Derivation
//!
//! - **Restriction**: Derived facets must be more restrictive than base facets
//! - **List**: Item type must be atomic (not list or union of lists)
//! - **Union**: Member types must be simple types
//!
//! ## Complex Type Derivation
//!
//! - **Extension**: Base type content + new content must be valid
//! - **Restriction**: Content model must be valid restriction of base content model
//!
//! # XSD Constraint IDs
//!
//! - `cos-st-restricts` - Derivation Valid (Restriction, Simple)
//! - `cos-list-of-atomic` - List item type must be atomic
//! - `cos-union-memberTypes` - Union member types must be simple
//! - `cos-ct-extends` - Complex Type Derivation OK (Extension)
//! - `derivation-ok-restriction` - Complex Type Derivation OK (Restriction)

use crate::error::{SchemaError, SchemaResult};
use crate::ids::{
    AttributeGroupKey, AttributeKey, ComplexTypeKey, ElementKey, NameId, SimpleTypeKey, TypeKey,
};
use crate::parser::frames::{
    AttributeUseKind, ComplexContentResult, Compositor, DerivationMethod, ElementFrameResult,
    ModelGroupDefResult, ParticleResult, ParticleTerm, ProcessContents, SimpleTypeVariety,
    WildcardNamespace, WildcardResult,
};
#[cfg(feature = "xsd11")]
use crate::parser::frames::{OpenContentMode, OpenContentResult};
use crate::parser::location::{SourceLocation, SourceRef};
use crate::schema::dependencies::DependencyGraph;
use crate::schema::model::DerivationSet;
use crate::schema::SchemaSet;
use crate::types::facets::{FacetKind, FacetSet};

/// Statistics from derivation validation
#[derive(Debug, Default)]
pub struct DerivationStats {
    /// Number of simple types validated
    pub simple_types_validated: usize,
    /// Number of complex types validated
    pub complex_types_validated: usize,
    /// Number of list types validated
    pub list_types_validated: usize,
    /// Number of union types validated
    pub union_types_validated: usize,
    /// Number of restriction derivations validated
    pub restrictions_validated: usize,
    /// Number of extension derivations validated
    pub extensions_validated: usize,
    /// Number of errors encountered
    pub errors: usize,
}

/// Validate all type derivations in a schema set
///
/// Uses the dependency graph to process types in topological order,
/// ensuring base types are validated before derived types.
///
/// # Arguments
///
/// * `schema_set` - The schema set with resolved references
/// * `dep_graph` - The dependency graph with sorted types
///
/// # Errors
///
/// Returns the first error encountered. All errors have source locations.
pub fn validate_all_derivations(
    schema_set: &SchemaSet,
    dep_graph: &DependencyGraph,
) -> SchemaResult<DerivationStats> {
    let mut stats = DerivationStats::default();
    let mut errors: Vec<SchemaError> = Vec::new();

    // Process types in compilation order (dependencies first)
    for &type_key in dep_graph.compilation_order() {
        match type_key {
            TypeKey::Simple(key) => {
                if let Err(e) = validate_simple_type(schema_set, key, &mut stats) {
                    errors.push(e);
                    stats.errors += 1;
                }
            }
            TypeKey::Complex(key) => {
                if let Err(e) = validate_complex_type(schema_set, key, &mut stats) {
                    errors.push(e);
                    stats.errors += 1;
                }
            }
        }
    }

    // §src-redefine 6.2.2 / 7.2.2 deferred restriction checks — must run
    // after reference resolution and type-derivation passes, because
    // `resolved_particle_types` / `resolved_attributes` on the flagged
    // groups are only populated post-resolve.
    validate_all_redefine_group_restrictions(schema_set, &mut errors, &mut stats);
    validate_all_redefine_attribute_group_restrictions(schema_set, &mut errors, &mut stats);

    // src-attribute_group circularity: an attribute group cannot transitively
    // reference itself in XSD 1.0. XSD 1.1 explicitly relaxed this — circular
    // attribute groups are permitted (W3C Bugzilla 15795). Walks `resolved_ref`
    // and `resolved_attribute_groups` for each group via DFS and flags any
    // back-edge in XSD 1.0 mode only.
    if schema_set.is_xsd10() {
        validate_attribute_group_no_circular(schema_set, &mut errors);
    } else {
        // XSD 1.1: circular attribute groups are allowed in general, but a
        // schema-level `defaultAttributes` group cannot itself participate in
        // a cycle — `resolve_all_references` injects the resolved group into
        // every applicable complex type, so a cycle here would imply the
        // schema-for-schemas validity rule §3.6.3 (no cycles via the
        // defaulting closure). Targeted DFS only on each document's selected
        // defaultAttributes group avoids re-enabling the global ban.
        validate_default_attribute_groups_no_circular(schema_set, &mut errors);
    }

    // Return first error if any
    if let Some(first_error) = errors.into_iter().next() {
        return Err(first_error);
    }

    Ok(stats)
}

/// Walk the attribute-group reference DAG and report cycles.
///
/// Each `xs:attributeGroup` definition references zero or more nested
/// attribute groups (including self via `ref` or `<attributeGroup ref=...>`
/// children). The schema-for-schemas does not allow circular references —
/// `src-attribute_group` constraint 3 in XSD 1.0 / §3.6.3 in XSD 1.1 forbid
/// any group from transitively referencing itself.
fn validate_attribute_group_no_circular(schema_set: &SchemaSet, errors: &mut Vec<SchemaError>) {
    use std::collections::HashSet;

    // Iterate every attribute group key once.
    let keys: Vec<_> = schema_set.arenas.attribute_groups.keys().collect();

    for start in keys {
        let mut path: Vec<AttributeGroupKey> = Vec::new();
        let mut visited: HashSet<AttributeGroupKey> = HashSet::new();
        if let Some(cycle_key) =
            find_attribute_group_cycle(schema_set, start, &mut path, &mut visited)
        {
            let location = schema_set
                .arenas
                .attribute_groups
                .get(cycle_key)
                .and_then(|ag| ag.source.as_ref())
                .and_then(|s| schema_set.source_maps.locate(s));
            errors.push(SchemaError::structural(
                "src-attribute_group",
                "Circular attribute group reference detected",
                location,
            ));
        }
    }
}

/// DFS helper for `validate_attribute_group_no_circular`. Returns the key
/// involved in the cycle (the first repeated node on the stack) when one is
/// found.
fn find_attribute_group_cycle(
    schema_set: &SchemaSet,
    key: AttributeGroupKey,
    path: &mut Vec<AttributeGroupKey>,
    visited: &mut std::collections::HashSet<AttributeGroupKey>,
) -> Option<AttributeGroupKey> {
    if path.contains(&key) {
        return Some(key);
    }
    if !visited.insert(key) {
        return None;
    }
    path.push(key);

    let result = if let Some(ag) = schema_set.arenas.attribute_groups.get(key) {
        let mut found: Option<AttributeGroupKey> = None;
        if let Some(ref_key) = ag.resolved_ref {
            if let Some(c) = find_attribute_group_cycle(schema_set, ref_key, path, visited) {
                found = Some(c);
            }
        }
        if found.is_none() {
            for &nested_key in &ag.resolved_attribute_groups {
                if let Some(c) = find_attribute_group_cycle(schema_set, nested_key, path, visited) {
                    found = Some(c);
                    break;
                }
            }
        }
        found
    } else {
        None
    };

    path.pop();
    result
}

/// XSD 1.1: Validate that the schema-level `defaultAttributes` selected groups
/// are not part of a circular reference chain. Targeted to the defaultAttributes
/// closure only — XSD 1.1 (W3C Bugzilla 15795) permits circular AGs in general,
/// but the defaulting injection in `resolver::resolve_all_references` would
/// loop forever if its starting group were itself cyclic.
fn validate_default_attribute_groups_no_circular(
    schema_set: &SchemaSet,
    errors: &mut Vec<SchemaError>,
) {
    use std::collections::HashSet;
    let mut seen_starts: HashSet<AttributeGroupKey> = HashSet::new();
    for doc in &schema_set.documents {
        let Some(ref qname) = doc.default_attributes else {
            continue;
        };
        let Some(start) = schema_set.lookup_attribute_group(qname.namespace_uri, qname.local_name)
        else {
            continue; // unresolvable defaultAttributes already reported elsewhere
        };
        if !seen_starts.insert(start) {
            continue;
        }
        let mut path: Vec<AttributeGroupKey> = Vec::new();
        let mut visited: HashSet<AttributeGroupKey> = HashSet::new();
        if let Some(cycle_key) =
            find_attribute_group_cycle(schema_set, start, &mut path, &mut visited)
        {
            let location = schema_set
                .arenas
                .attribute_groups
                .get(cycle_key)
                .and_then(|ag| ag.source.as_ref())
                .and_then(|s| schema_set.source_maps.locate(s));
            errors.push(SchemaError::structural(
                "src-attribute_group",
                "Circular attribute group reference detected via defaultAttributes",
                location,
            ));
        }
    }
}

/// Validate a simple type definition
fn validate_simple_type(
    schema_set: &SchemaSet,
    key: SimpleTypeKey,
    stats: &mut DerivationStats,
) -> SchemaResult<()> {
    let type_def = schema_set
        .arenas
        .simple_types
        .get(key)
        .ok_or_else(|| SchemaError::internal("Simple type not found in arena"))?;

    stats.simple_types_validated += 1;

    // cos-applicable-facets: Check that facets are applicable to the type variety
    validate_applicable_facets(schema_set, type_def)?;

    match type_def.variety {
        SimpleTypeVariety::Atomic => {
            // Atomic types are derived by restriction
            validate_simple_restriction(schema_set, type_def, stats)?;
        }
        SimpleTypeVariety::List => {
            stats.list_types_validated += 1;
            validate_simple_list(schema_set, type_def)?;
            validate_facets_against_resolved_base(schema_set, type_def)?;
        }
        SimpleTypeVariety::Union => {
            stats.union_types_validated += 1;
            validate_simple_union(schema_set, type_def)?;
            validate_facets_against_resolved_base(schema_set, type_def)?;
        }
    }

    Ok(())
}

/// Run `FacetSet::merge_with_base` against the resolved base of a list or
/// union simple type, then validate that local facet values fall in the
/// base type's value space. Atomic types perform both checks inline in
/// `validate_simple_restriction`; list (length/minLength/maxLength/whiteSpace)
/// and union (pattern/enumeration/assertions) varieties share the same
/// {facets} derivation semantics for the facets they are allowed to carry,
/// so the merge must run for them too.
fn validate_facets_against_resolved_base(
    schema_set: &SchemaSet,
    type_def: &crate::arenas::SimpleTypeDefData,
) -> SchemaResult<()> {
    let Some(base_key) = type_def.resolved_base_type else {
        return Ok(());
    };
    if let Some(base_facets) = get_type_facets(schema_set, base_key)? {
        type_def.facets.merge_with_base(&base_facets).map_err(|e| {
            let (location, type_name) = type_error_context(schema_set, type_def);
            SchemaError::structural(
                "cos-st-restricts",
                format!("Simple type '{}' has invalid restriction: {}", type_name, e),
                location,
            )
        })?;
    }
    validate_facet_values_against_base_type(schema_set, type_def, base_key)?;
    Ok(())
}

/// Validate cos-applicable-facets: only certain facets are applicable to certain type varieties
///
/// - List types: length, minLength, maxLength, pattern, enumeration, whiteSpace
/// - Union types (XSD 1.0): pattern, enumeration
/// - Union types (XSD 1.1): pattern, enumeration, assertions
fn validate_applicable_facets(
    schema_set: &SchemaSet,
    type_def: &crate::arenas::SimpleTypeDefData,
) -> SchemaResult<()> {
    let facets = &type_def.facets;

    match type_def.variety {
        SimpleTypeVariety::List => {
            // List types: only length, minLength, maxLength, pattern, enumeration, whiteSpace
            let has_inapplicable = facets.min_inclusive.is_some()
                || facets.max_inclusive.is_some()
                || facets.min_exclusive.is_some()
                || facets.max_exclusive.is_some()
                || facets.total_digits.is_some()
                || facets.fraction_digits.is_some()
                || facets.explicit_timezone.is_some();

            if has_inapplicable {
                let (location, type_name) = type_error_context(schema_set, type_def);
                let inapplicable = list_inapplicable_facets_for_list(facets);
                return Err(SchemaError::structural(
                    "cos-applicable-facets",
                    format!(
                        "List type '{}' has inapplicable facet(s): {}",
                        type_name, inapplicable
                    ),
                    location,
                ));
            }
        }
        SimpleTypeVariety::Union => {
            // Union types: only pattern, enumeration (and assertions in XSD 1.1)
            let has_inapplicable = facets.length.is_some()
                || facets.min_length.is_some()
                || facets.max_length.is_some()
                || facets.whitespace.is_some()
                || facets.min_inclusive.is_some()
                || facets.max_inclusive.is_some()
                || facets.min_exclusive.is_some()
                || facets.max_exclusive.is_some()
                || facets.total_digits.is_some()
                || facets.fraction_digits.is_some()
                || facets.explicit_timezone.is_some();

            if has_inapplicable {
                let (location, type_name) = type_error_context(schema_set, type_def);
                let inapplicable = list_inapplicable_facets_for_union(facets);
                return Err(SchemaError::structural(
                    "cos-applicable-facets",
                    format!(
                        "Union type '{}' has inapplicable facet(s): {}",
                        type_name, inapplicable
                    ),
                    location,
                ));
            }
        }
        SimpleTypeVariety::Atomic => {
            // Atomic types: applicability depends on the primitive ancestor
            // (§4.1.5 / Table F.1 of Datatypes). Walk up the base chain to the
            // closest built-in primitive and check each facet kind against it.
            if let Some(primitive_code) = primitive_type_code(schema_set, type_def) {
                use crate::types::facets::{
                    facet_applicable_for_type, ExplicitTimezone, FacetApplicability, FacetKind,
                    WhitespaceMode,
                };
                let facets = &type_def.facets;
                let mut bad: Vec<&'static str> = Vec::new();
                let mut check = |present: bool, kind: FacetKind| {
                    if present
                        && matches!(
                            facet_applicable_for_type(kind, primitive_code),
                            FacetApplicability::NotApplicable
                        )
                    {
                        bad.push(kind.name());
                    }
                };
                check(facets.length.is_some(), FacetKind::Length);
                check(facets.min_length.is_some(), FacetKind::MinLength);
                check(facets.max_length.is_some(), FacetKind::MaxLength);
                check(facets.whitespace.is_some(), FacetKind::Whitespace);
                check(facets.min_inclusive.is_some(), FacetKind::MinInclusive);
                check(facets.max_inclusive.is_some(), FacetKind::MaxInclusive);
                check(facets.min_exclusive.is_some(), FacetKind::MinExclusive);
                check(facets.max_exclusive.is_some(), FacetKind::MaxExclusive);
                check(facets.total_digits.is_some(), FacetKind::TotalDigits);
                check(facets.fraction_digits.is_some(), FacetKind::FractionDigits);
                check(
                    facets.explicit_timezone.is_some(),
                    FacetKind::ExplicitTimezone,
                );
                if let Some(ws) = &facets.whitespace {
                    if !matches!(
                        primitive_code,
                        crate::types::XmlTypeCode::String
                            | crate::types::XmlTypeCode::NormalizedString
                            | crate::types::XmlTypeCode::Token
                            | crate::types::XmlTypeCode::Language
                            | crate::types::XmlTypeCode::NmToken
                            | crate::types::XmlTypeCode::Name
                            | crate::types::XmlTypeCode::NCName
                            | crate::types::XmlTypeCode::Id
                            | crate::types::XmlTypeCode::IdRef
                            | crate::types::XmlTypeCode::Entity
                    ) && ws.value != WhitespaceMode::Collapse
                    {
                        bad.push(FacetKind::Whitespace.name());
                    }
                }
                if let Some(tz) = &facets.explicit_timezone {
                    if primitive_code == crate::types::XmlTypeCode::DateTimeStamp
                        && tz.value != ExplicitTimezone::Required
                    {
                        bad.push(FacetKind::ExplicitTimezone.name());
                    }
                }
                if !bad.is_empty() {
                    let (location, type_name) = type_error_context(schema_set, type_def);
                    return Err(SchemaError::structural(
                        "cos-applicable-facets",
                        format!(
                            "Atomic type '{}' has inapplicable facet(s) for primitive '{}': {}",
                            type_name,
                            primitive_code.local_name().unwrap_or("<unnamed>"),
                            bad.join(", ")
                        ),
                        location,
                    ));
                }
            }
        }
    }

    Ok(())
}

/// Walk a simple type's base chain to the closest built-in with an `XmlTypeCode`.
///
/// Depth is capped at 64 — the XSD primitive hierarchy is shallow and the
/// dependency graph already rejects cycles before this runs, so the bound is
/// purely a defence against a malformed arena state.
fn primitive_type_code(
    schema_set: &SchemaSet,
    type_def: &crate::arenas::SimpleTypeDefData,
) -> Option<crate::types::XmlTypeCode> {
    let builtin = schema_set.builtin_types();
    let mut current_base = type_def.resolved_base_type;
    for _ in 0..64 {
        let Some(TypeKey::Simple(k)) = current_base else {
            return None;
        };
        if let Some(code) = builtin.get_type_code(k) {
            return Some(code);
        }
        current_base = schema_set
            .arenas
            .simple_types
            .get(k)
            .and_then(|t| t.resolved_base_type);
    }
    None
}

/// Emit a `cos-st-restricts`-family error when `simple_key` names
/// `xs:anyAtomicType`, which XSD 1.1 bug 11103 declared abstract — it must
/// not appear as a restriction base, list item type, or union member.
fn reject_any_atomic_type(
    schema_set: &SchemaSet,
    type_def: &crate::arenas::SimpleTypeDefData,
    simple_key: SimpleTypeKey,
    constraint: &'static str,
    role: &'static str,
) -> SchemaResult<()> {
    if !schema_set.builtin_types().is_any_atomic_type(simple_key) {
        return Ok(());
    }
    let (location, type_name) = type_error_context(schema_set, type_def);
    Err(SchemaError::structural(
        constraint,
        format!(
            "Simple type '{}' cannot {} xs:anyAtomicType (abstract per XSD 1.1 bug 11103)",
            type_name, role
        ),
        location,
    ))
}

/// List inapplicable facet names for list types
fn list_inapplicable_facets_for_list(facets: &FacetSet) -> String {
    let mut names = Vec::new();
    if facets.min_inclusive.is_some() {
        names.push("minInclusive");
    }
    if facets.max_inclusive.is_some() {
        names.push("maxInclusive");
    }
    if facets.min_exclusive.is_some() {
        names.push("minExclusive");
    }
    if facets.max_exclusive.is_some() {
        names.push("maxExclusive");
    }
    if facets.total_digits.is_some() {
        names.push("totalDigits");
    }
    if facets.fraction_digits.is_some() {
        names.push("fractionDigits");
    }
    if facets.explicit_timezone.is_some() {
        names.push("explicitTimezone");
    }
    names.join(", ")
}

/// List inapplicable facet names for union types
fn list_inapplicable_facets_for_union(facets: &FacetSet) -> String {
    let mut names = Vec::new();
    if facets.length.is_some() {
        names.push("length");
    }
    if facets.min_length.is_some() {
        names.push("minLength");
    }
    if facets.max_length.is_some() {
        names.push("maxLength");
    }
    if facets.whitespace.is_some() {
        names.push("whiteSpace");
    }
    if facets.min_inclusive.is_some() {
        names.push("minInclusive");
    }
    if facets.max_inclusive.is_some() {
        names.push("maxInclusive");
    }
    if facets.min_exclusive.is_some() {
        names.push("minExclusive");
    }
    if facets.max_exclusive.is_some() {
        names.push("maxExclusive");
    }
    if facets.total_digits.is_some() {
        names.push("totalDigits");
    }
    if facets.fraction_digits.is_some() {
        names.push("fractionDigits");
    }
    if facets.explicit_timezone.is_some() {
        names.push("explicitTimezone");
    }
    names.join(", ")
}

/// Validate simple type restriction derivation
///
/// Constraint: cos-st-restricts (Derivation Valid - Restriction, Simple)
fn validate_simple_restriction(
    schema_set: &SchemaSet,
    type_def: &crate::arenas::SimpleTypeDefData,
    stats: &mut DerivationStats,
) -> SchemaResult<()> {
    // If no base type, this is a primitive type or xs:anySimpleType derivation
    let base_key = match type_def.resolved_base_type {
        Some(key) => key,
        None => return Ok(()), // No base type to validate against
    };

    // cos-st-restricts.1.1: base type must be a simple type definition
    if let TypeKey::Complex(_) = base_key {
        let (location, type_name) = type_error_context(schema_set, type_def);
        return Err(SchemaError::structural(
            "cos-st-restricts",
            format!("Simple type '{}': base type must be a simple type definition (cos-st-restricts.1.1)", type_name),
            location,
        ));
    }

    if let TypeKey::Simple(base_simple_key) = base_key {
        reject_any_atomic_type(
            schema_set,
            type_def,
            base_simple_key,
            "cos-st-restricts",
            "restrict",
        )?;
    }

    stats.restrictions_validated += 1;

    // Check that base type is not final for restriction
    if let TypeKey::Simple(base_simple_key) = base_key {
        if let Some(base_type) = schema_set.arenas.simple_types.get(base_simple_key) {
            if base_type.final_derivation.contains_restriction() {
                let (location, type_name) = type_error_context(schema_set, type_def);
                let base_name =
                    format_type_name(schema_set, base_type.name, base_type.target_namespace);
                return Err(SchemaError::structural(
                    "cos-st-restricts",
                    format!(
                        "Simple type '{}' cannot restrict '{}' because base type is final for restriction",
                        type_name, base_name
                    ),
                    location,
                ));
            }
        }
    }

    // Get base type facets
    let base_facets = get_type_facets(schema_set, base_key)?;

    // Validate that derived facets are more restrictive
    if let Some(ref base_facets) = base_facets {
        // FacetSet.merge_with_base validates derivation rules
        type_def.facets.merge_with_base(base_facets).map_err(|e| {
            let (location, type_name) = type_error_context(schema_set, type_def);
            SchemaError::structural(
                "cos-st-restricts",
                format!("Simple type '{}' has invalid restriction: {}", type_name, e),
                location,
            )
        })?;
    }

    // Validate that facet values are in the base type's value space
    // (e.g., enumeration values must be valid for xs:float when base is xs:float)
    validate_facet_values_against_base_type(schema_set, type_def, base_key)?;

    Ok(())
}

/// Validate simple type list derivation
///
/// Constraint: cos-list-of-atomic (List item type must be atomic)
fn validate_simple_list(
    schema_set: &SchemaSet,
    type_def: &crate::arenas::SimpleTypeDefData,
) -> SchemaResult<()> {
    // Check that the item type is not final for list derivation
    if let Some(TypeKey::Simple(item_simple_key)) = type_def.resolved_item_type {
        if let Some(item_type) = schema_set.arenas.simple_types.get(item_simple_key) {
            if item_type.final_derivation.contains_list() {
                let (location, type_name) = type_error_context(schema_set, type_def);
                let item_name =
                    format_type_name(schema_set, item_type.name, item_type.target_namespace);
                return Err(SchemaError::structural(
                    "cos-st-restricts",
                    format!(
                        "List type '{}' cannot use '{}' as item type because it is final for list",
                        type_name, item_name
                    ),
                    location,
                ));
            }
        }
    }

    // Also check the base type's final for restriction (list types restrict xs:anySimpleType)
    if let Some(TypeKey::Simple(base_simple_key)) = type_def.resolved_base_type {
        if let Some(base_type) = schema_set.arenas.simple_types.get(base_simple_key) {
            if base_type.final_derivation.contains_list() {
                let (location, type_name) = type_error_context(schema_set, type_def);
                let base_name =
                    format_type_name(schema_set, base_type.name, base_type.target_namespace);
                return Err(SchemaError::structural(
                    "cos-st-restricts",
                    format!(
                        "List type '{}' cannot derive from '{}' because it is final for list",
                        type_name, base_name
                    ),
                    location,
                ));
            }
        }
    }

    // Get the item type
    let item_key = match type_def.resolved_item_type {
        Some(key) => key,
        None => {
            // No resolved item type - might be inline or error
            return Ok(());
        }
    };

    // Item type must be atomic (not a list, not a union containing lists)
    match item_key {
        TypeKey::Simple(simple_key) => {
            reject_any_atomic_type(
                schema_set,
                type_def,
                simple_key,
                "cos-list-of-atomic",
                "use as list item type",
            )?;
            if let Some(item_type) = schema_set.arenas.simple_types.get(simple_key) {
                match item_type.variety {
                    SimpleTypeVariety::Atomic => {
                        // Valid - atomic types are OK
                    }
                    SimpleTypeVariety::List => {
                        // Invalid - list of list is not allowed
                        let (location, type_name) = type_error_context(schema_set, type_def);
                        return Err(SchemaError::structural(
                            "cos-list-of-atomic",
                            format!(
                                "List type '{}' has list item type, which is not allowed",
                                type_name
                            ),
                            location,
                        ));
                    }
                    SimpleTypeVariety::Union => {
                        // Must check that union doesn't contain list members
                        if union_contains_list(schema_set, item_type) {
                            let (location, type_name) = type_error_context(schema_set, type_def);
                            return Err(SchemaError::structural(
                                "cos-list-of-atomic",
                                format!(
                                    "List type '{}' has union item type containing list member",
                                    type_name
                                ),
                                location,
                            ));
                        }
                    }
                }
            }
        }
        TypeKey::Complex(_) => {
            // Complex types cannot be list item types
            let (location, type_name) = type_error_context(schema_set, type_def);
            return Err(SchemaError::structural(
                "cos-list-of-atomic",
                format!(
                    "List type '{}' has complex item type, which is not allowed",
                    type_name
                ),
                location,
            ));
        }
    }

    Ok(())
}

/// Check if a union type (or nested unions) contains any list members
fn union_contains_list(
    schema_set: &SchemaSet,
    union_type: &crate::arenas::SimpleTypeDefData,
) -> bool {
    for member_key in &union_type.resolved_member_types {
        if let TypeKey::Simple(simple_key) = member_key {
            if let Some(member) = schema_set.arenas.simple_types.get(*simple_key) {
                match member.variety {
                    SimpleTypeVariety::List => return true,
                    SimpleTypeVariety::Union => {
                        if union_contains_list(schema_set, member) {
                            return true;
                        }
                    }
                    SimpleTypeVariety::Atomic => {}
                }
            }
        }
    }
    false
}

/// Validate simple type union derivation
///
/// Constraint: cos-union-memberTypes (Union member types must be simple types)
fn validate_simple_union(
    schema_set: &SchemaSet,
    type_def: &crate::arenas::SimpleTypeDefData,
) -> SchemaResult<()> {
    // Check that member types are not final for union derivation
    for member_key in &type_def.resolved_member_types {
        if let TypeKey::Simple(simple_key) = member_key {
            if let Some(member_type) = schema_set.arenas.simple_types.get(*simple_key) {
                if member_type.final_derivation.contains_union() {
                    let (location, type_name) = type_error_context(schema_set, type_def);
                    let member_name = format_type_name(
                        schema_set,
                        member_type.name,
                        member_type.target_namespace,
                    );
                    return Err(SchemaError::structural(
                        "cos-st-restricts",
                        format!(
                            "Union type '{}' cannot use '{}' as member type because it is final for union",
                            type_name, member_name
                        ),
                        location,
                    ));
                }
            }
        }
    }

    // All member types must be simple types
    for member_key in &type_def.resolved_member_types {
        match member_key {
            TypeKey::Simple(simple_key) => {
                reject_any_atomic_type(
                    schema_set,
                    type_def,
                    *simple_key,
                    "cos-union-memberTypes",
                    "use as union member type",
                )?;
            }
            TypeKey::Complex(_) => {
                // Invalid - complex types cannot be union members
                let (location, type_name) = type_error_context(schema_set, type_def);
                return Err(SchemaError::structural(
                    "cos-union-memberTypes",
                    format!(
                        "Union type '{}' has complex member type, which is not allowed",
                        type_name
                    ),
                    location,
                ));
            }
        }
    }

    Ok(())
}

/// Validate a complex type definition
fn validate_complex_type(
    schema_set: &SchemaSet,
    key: ComplexTypeKey,
    stats: &mut DerivationStats,
) -> SchemaResult<()> {
    let type_def = schema_set
        .arenas
        .complex_types
        .get(key)
        .ok_or_else(|| SchemaError::internal("Complex type not found in arena"))?;

    stats.complex_types_validated += 1;

    // Check derivation method
    match type_def.derivation_method {
        Some(DerivationMethod::Extension) => {
            stats.extensions_validated += 1;
            validate_complex_extension(schema_set, key, type_def)?;
        }
        Some(DerivationMethod::Restriction) => {
            stats.restrictions_validated += 1;
            validate_complex_restriction(schema_set, type_def)?;
        }
        None => {
            // No explicit derivation - this is a new complex type definition
            // Implicitly derived from xs:anyType by restriction
        }
    }

    Ok(())
}

/// Validate complex type extension
///
/// Constraint: cos-ct-extends (Complex Type Derivation OK - Extension)
fn validate_complex_extension(
    schema_set: &SchemaSet,
    #[cfg_attr(not(feature = "xsd11"), allow(unused_variables))] derived_key: ComplexTypeKey,
    type_def: &crate::arenas::ComplexTypeDefData,
) -> SchemaResult<()> {
    // Get base type
    let base_key = match type_def.resolved_base_type {
        Some(key) => key,
        None => return Ok(()), // No base type
    };

    // Check that base type exists and is accessible
    match base_key {
        TypeKey::Simple(base_simple_key) => {
            // Extension from simple type is valid only with simpleContent
            // complexContent extension from simple type is invalid
            if matches!(type_def.content, ComplexContentResult::Complex(_)) {
                let (location, type_name) = type_error_context(schema_set, type_def);
                return Err(SchemaError::structural(
                    "cos-ct-extends",
                    format!(
                        "Complex type '{}' cannot use complexContent extension from a simple type",
                        type_name,
                    ),
                    location,
                ));
            }

            // Check that simple base type is not final for extension
            if let Some(base_type) = schema_set.arenas.simple_types.get(base_simple_key) {
                if base_type.final_derivation.contains_extension() {
                    let (location, type_name) = type_error_context(schema_set, type_def);
                    let base_name =
                        format_type_name(schema_set, base_type.name, base_type.target_namespace);
                    return Err(SchemaError::structural(
                        "cos-ct-extends",
                        format!(
                            "Complex type '{}' cannot extend simple type '{}' because it is final for extension",
                            type_name, base_name,
                        ),
                        location,
                    ));
                }
            }
        }
        TypeKey::Complex(base_complex_key) => {
            if let Some(base_type) = schema_set.arenas.complex_types.get(base_complex_key) {
                // Check that base type is not final for extension
                if base_type.final_derivation.contains_extension() {
                    let (location, type_name) = type_error_context(schema_set, type_def);
                    let base_name =
                        format_type_name(schema_set, base_type.name, base_type.target_namespace);
                    return Err(SchemaError::structural(
                        "cos-ct-extends",
                        format!(
                            "Complex type '{}' cannot extend '{}' because base type is final for extension",
                            type_name, base_name
                        ),
                        location,
                    ));
                }

                // src-ct.2 (§3.4.6.2): when the derived type uses <xs:simpleContent>
                // and the <xs:extension> alternative, the base's {content type}
                // must be either a simple type (clause 2.1.3 requires a simple-type
                // base, handled above via TypeKey::Simple) or a complex type whose
                // {content type} is a simple type definition (clause 2.1.1).
                // A base with element-only or mixed complex content is rejected.
                if matches!(type_def.content, ComplexContentResult::Simple(_))
                    && !matches!(base_type.content, ComplexContentResult::Simple(_))
                {
                    let (location, type_name) = type_error_context(schema_set, type_def);
                    let base_name =
                        format_type_name(schema_set, base_type.name, base_type.target_namespace);
                    return Err(SchemaError::structural(
                        "src-ct",
                        format!(
                            "Complex type '{}' uses xs:simpleContent extension but base '{}' \
                             does not have a simple {{content type}} (src-ct.2.1.1)",
                            type_name, base_name,
                        ),
                        location,
                    ));
                }

                // cos-ct-extends: Cannot use complexContent extension to add particles
                // to a base type with simpleContent.
                // XSD 1.0: only rejected when a particle is actually added.
                // XSD 1.1 cos-ct-extends clause 1.4: content variety must match;
                //   simpleContent base + complexContent derived is always invalid.
                if matches!(base_type.content, ComplexContentResult::Simple(_)) {
                    if let ComplexContentResult::Complex(ref complex) = type_def.content {
                        if complex.particle.is_some() || schema_set.is_xsd11() {
                            let (location, type_name) = type_error_context(schema_set, type_def);
                            let base_name = format_type_name(
                                schema_set,
                                base_type.name,
                                base_type.target_namespace,
                            );
                            return Err(SchemaError::structural(
                                "cos-ct-extends",
                                format!(
                                    "Complex type '{}' cannot use complexContent to extend '{}' which has simpleContent{}",
                                    type_name, base_name,
                                    if complex.particle.is_some() { " with element content" }
                                    else { " (XSD 1.1 cos-ct-extends clause 1.4)" },
                                ),
                                location,
                            ));
                        }
                    }
                }

                validate_extension_mixed_parity(schema_set, type_def, base_type)?;

                // cos-ct-extends / cos-particle-extend: Cannot extend non-empty
                // non-all content with an all compositor.  The effective content
                // type of an extension is sequence(base, extension) per §3.4.2.3.3.
                // cos-particle-extend §3.9.6.2 only allows: (1) same particle,
                // (2) E is a sequence wrapping B, or (3) both are all groups.
                // An all group nested inside a sequence also violates
                // cos-all-limited.1 (placement constraint).
                //
                // Exception: XSD 1.1 allows all-over-all extensions (clause 3 of
                // cos-particle-extend). If both base and extension are all groups,
                // skip this check.
                if let ComplexContentResult::Complex(ref base_complex) = base_type.content {
                    if let Some(ref base_particle) = base_complex.particle {
                        if let ComplexContentResult::Complex(ref derived_complex) = type_def.content
                        {
                            if let Some(ref ext_particle) = derived_complex.particle {
                                let ext_compositor = match &ext_particle.term {
                                    ParticleTerm::Group(mg) => mg.compositor,
                                    _ => None,
                                };
                                let base_is_all = matches!(
                                    base_particle.term,
                                    ParticleTerm::Group(ModelGroupDefResult {
                                        compositor: Some(Compositor::All),
                                        ..
                                    })
                                );

                                // cos-particle-extend §3.9.6.2: over a non-empty base,
                                // the only valid extension shapes are:
                                // (1) No extension particle  (handled by outer if-let)
                                // (2) Extension particle is a sequence
                                // (3) XSD 1.1: all-over-all
                                match ext_compositor {
                                    Some(Compositor::Sequence) => {
                                        // OK: sequence extension is always valid
                                    }
                                    Some(Compositor::All)
                                        if base_is_all
                                            && schema_set.xsd_version
                                                == crate::schema::model::XsdVersion::V1_1 =>
                                    {
                                        // OK: XSD 1.1 all-over-all
                                    }
                                    Some(Compositor::Choice) if !base_is_all => {
                                        // OK: the effective content type is
                                        // sequence(base, extension) per §3.4.2.3.3
                                        // clause 4.2.3.3, so cos-particle-extend
                                        // clause 2 is satisfied regardless of the
                                        // extension particle's compositor — as long
                                        // as the base particle is not xs:all (which
                                        // would get nested inside the sequence and
                                        // violate cos-all-limited.1).
                                    }
                                    Some(compositor @ (Compositor::All | Compositor::Choice)) => {
                                        let location = type_def
                                            .source
                                            .as_ref()
                                            .and_then(|s| schema_set.source_maps.locate(s));
                                        let type_name = format_type_name(
                                            schema_set,
                                            type_def.name,
                                            type_def.target_namespace,
                                        );
                                        let base_name = format_type_name(
                                            schema_set,
                                            base_type.name,
                                            base_type.target_namespace,
                                        );
                                        let (comp_name, reason) = match compositor {
                                            Compositor::All => (
                                                "all",
                                                "the resulting content model would \
                                                 violate cos-all-limited placement \
                                                 constraints",
                                            ),
                                            Compositor::Choice => (
                                                "choice",
                                                "the base type's xs:all particle would \
                                                 be nested inside a sequence, \
                                                 violating cos-all-limited.1",
                                            ),
                                            Compositor::Sequence => unreachable!(),
                                        };
                                        return Err(SchemaError::structural(
                                            "cos-ct-extends",
                                            format!(
                                                "Complex type '{}' cannot extend '{}' with \
                                                 an xs:{} compositor because the base type \
                                                 has non-empty content; {}",
                                                type_name, base_name, comp_name, reason,
                                            ),
                                            location,
                                        ));
                                    }
                                    None => {
                                        // Bare element or wildcard term (no model group
                                        // wrapper).  The effective content type mapping
                                        // wraps it in a sequence with the base, so this
                                        // is equivalent to a sequence extension — OK.
                                    }
                                }
                            }
                        }
                    }
                }

                // XSD 1.1: Validate open-content compatibility
                #[cfg(feature = "xsd11")]
                validate_open_content_extension(
                    schema_set,
                    derived_key,
                    type_def,
                    base_complex_key,
                    base_type,
                )?;

                // ct-props-correct.4 is enforced globally by
                // `validate_complex_type_attribute_uniqueness` (run from
                // `pipeline.rs` after reference resolution); no extension-
                // local check needed here.
            }
        }
    }

    Ok(())
}

/// Validate complex type restriction
///
/// Constraint: derivation-ok-restriction (Complex Type Derivation OK - Restriction)
fn validate_complex_restriction(
    schema_set: &SchemaSet,
    type_def: &crate::arenas::ComplexTypeDefData,
) -> SchemaResult<()> {
    // Get base type
    let base_key = match type_def.resolved_base_type {
        Some(key) => key,
        None => return Ok(()), // No base type (derived from anyType)
    };

    match base_key {
        TypeKey::Simple(base_simple_key) => {
            // ct-props-correct.2: If the base type is a simple type definition,
            // the derivation method must be extension (not restriction).
            let (location, type_name) = type_error_context(schema_set, type_def);
            let base_name =
                if let Some(base_type) = schema_set.arenas.simple_types.get(base_simple_key) {
                    format_type_name(schema_set, base_type.name, base_type.target_namespace)
                } else {
                    "(unknown)".to_string()
                };
            return Err(SchemaError::structural(
                "ct-props-correct",
                format!(
                    "Complex type '{}' cannot restrict simple type '{}'; \
                     derivation from a simple type must use extension",
                    type_name, base_name,
                ),
                location,
            ));
        }
        TypeKey::Complex(base_complex_key) => {
            if let Some(base_type) = schema_set.arenas.complex_types.get(base_complex_key) {
                // Check that base type is not final for restriction
                if base_type.final_derivation.contains_restriction() {
                    let (location, type_name) = type_error_context(schema_set, type_def);
                    let base_name =
                        format_type_name(schema_set, base_type.name, base_type.target_namespace);
                    return Err(SchemaError::structural(
                        "derivation-ok-restriction",
                        format!(
                            "Complex type '{}' cannot restrict '{}' because base type is final for restriction",
                            type_name, base_name
                        ),
                        location,
                    ));
                }

                // (§3.4.6.4 clause 5.4.1 mixed-parity check intentionally
                // omitted: the W3C suite marks several valid-schema tests
                // that restrict mixed to element-only — e.g. particlesL012,
                // mgA015, idK012 — relying on the spec's "pointless mixed
                // restriction" tolerance applied by Saxon and Xerces.)

                // src-ct.2 (§3.4.6.2): when the derived type uses <xs:simpleContent>
                // and the <xs:restriction> alternative, the base must be either a
                // complex type with a simple {content type} (clause 2.1.1) or a
                // complex type whose {content type} is mixed and whose particle is
                // emptiable (clause 2.1.2). Element-only or mixed-but-not-emptiable
                // bases are rejected.
                if matches!(type_def.content, ComplexContentResult::Simple(_))
                    && !is_valid_simple_content_restriction_base(schema_set, base_type)
                {
                    let (location, type_name) = type_error_context(schema_set, type_def);
                    let base_name =
                        format_type_name(schema_set, base_type.name, base_type.target_namespace);
                    return Err(SchemaError::structural(
                        "src-ct",
                        format!(
                            "Complex type '{}' uses xs:simpleContent restriction but base '{}' \
                             does not have a simple {{content type}} nor mixed+emptiable content \
                             (src-ct.2.1.1 / 2.1.2)",
                            type_name, base_name,
                        ),
                        location,
                    ));
                }

                validate_content_particle_restriction(schema_set, type_def, base_type)?;

                // XSD 1.1 §3.4.6.4 / cos-element-consistent (extended for
                // all-group restrictions): when a derived all-group restricts
                // a base all-group and removes a base local element, any
                // wildcard in the derived that admits the removed element's
                // QName must resolve to a governing type that's substitutable
                // for the base local's type. This is the schema-time analog of
                // dynamic EDC, applied where the all-group's unordered
                // matching makes the conflict structurally inevitable
                // (wild069 — the corresponding xs:sequence case wild068 is
                // covered by runtime dynamic EDC instead).
                #[cfg(feature = "xsd11")]
                validate_all_group_restriction_edc(schema_set, type_def, base_type)?;

                // XSD 1.1: Validate open-content compatibility
                #[cfg(feature = "xsd11")]
                validate_open_content_restriction(schema_set, type_def, base_type)?;

                // Validate attribute restriction (derivation-ok-restriction clause 3)
                validate_attribute_restriction(schema_set, type_def, base_type)?;

                // Validate simpleContent inline type restriction
                // (derivation-ok-restriction clause 2.2.2.1)
                validate_simple_content_restriction(schema_set, type_def, base_type)?;
            }
        }
    }

    Ok(())
}

/// §3.4.2.3 mapping: the `mixed` flag carried by a `<complexContent>` wrapper
/// overrides the outer `<complexType mixed="…">` attribute.  For complex
/// types authored in the short form (no `<complexContent>` wrapper), the
/// outer attribute applies unchanged.
fn effective_mixed_of(
    type_def: &crate::arenas::ComplexTypeDefData,
    complex: &crate::parser::frames::ComplexContentDefResult,
) -> bool {
    // `complex.mixed` reflects the `<complexContent mixed="…">` attribute.
    // When the complexType was parsed from a short form, the wrapper is
    // synthesized with mixed=false and the outer attribute is preserved on
    // `type_def.mixed` — so we OR the two.  When the wrapper is present,
    // `type_def.mixed` carries the same bit, so the OR is a no-op.
    complex.mixed || type_def.mixed
}

/// cos-ct-extends clause 1.4.3.2.2.4.1 (§3.4.6.2): when the derived type
/// supplies its own particle, the effective mixed of the derived {content
/// type} must match the base's — both element-only, or both mixed. Skipped
/// when either side lacks a particle, because §3.4.2.3 clause 4.1 then
/// copies the base's {content type} verbatim into the derived, trivially
/// satisfying parity regardless of the outer `mixed="true"` attribute.
fn validate_extension_mixed_parity(
    schema_set: &SchemaSet,
    type_def: &crate::arenas::ComplexTypeDefData,
    base_type: &crate::arenas::ComplexTypeDefData,
) -> SchemaResult<()> {
    let ComplexContentResult::Complex(ref base_complex) = base_type.content else {
        return Ok(());
    };
    let ComplexContentResult::Complex(ref derived_complex) = type_def.content else {
        return Ok(());
    };
    if base_complex.particle.is_none() || derived_complex.particle.is_none() {
        return Ok(());
    }
    let base_mixed = effective_mixed_of(base_type, base_complex);
    let derived_mixed = effective_mixed_of(type_def, derived_complex);
    if derived_mixed == base_mixed {
        return Ok(());
    }
    let (location, type_name) = type_error_context(schema_set, type_def);
    let base_name = format_type_name(schema_set, base_type.name, base_type.target_namespace);
    Err(SchemaError::structural(
        "cos-ct-extends",
        format!(
            "Complex type '{}' cannot extend '{}' — derived is {} but base is {} \
             (cos-ct-extends clause 1.4.3.2.2.4.1)",
            type_name,
            base_name,
            if derived_mixed {
                "mixed"
            } else {
                "element-only"
            },
            if base_mixed { "mixed" } else { "element-only" },
        ),
        location,
    ))
}

/// §3.4.6.2 src-ct.2: a complex type derived via `<xs:simpleContent>` from
/// another complex base is legal only when the base's `{content type}` is
/// a simple type (clause 2.1.1) or when — for the restriction branch — the
/// base is mixed with an emptiable particle (clause 2.1.2).
/// Returns `true` if the base is acceptable for a simpleContent restriction.
fn is_valid_simple_content_restriction_base(
    schema_set: &SchemaSet,
    base: &crate::arenas::ComplexTypeDefData,
) -> bool {
    match &base.content {
        ComplexContentResult::Simple(_) => true,
        ComplexContentResult::Complex(complex) => {
            // Clause 2.1.2: mixed content with an emptiable particle.  The
            // `mixed` flag on a <complexContent> wrapper overrides the outer
            // <complexType mixed="…"> attribute per §3.4.2.2, so consult it
            // here; the outer flag applies only to the short-form path.
            if !complex.mixed {
                return false;
            }
            match &complex.particle {
                // Absent particle ≡ empty sequence ≡ emptiable.
                None => true,
                Some(particle) => match normalize_type_particle(schema_set, base, particle) {
                    Ok(normalized) => particle_is_emptiable(&normalized),
                    Err(_) => false,
                },
            }
        }
        // Short-form complex type without <simpleContent>/<complexContent>:
        // the {content type} is determined by §3.4.2.2 from the outer
        // `mixed` attribute and any top-level particle.  When `mixed` is
        // true and no particle is present, the type has mixed emptiable
        // content and is a valid base for simpleContent restriction
        // (clause 2.1.2).  When `mixed` is false, the content type is
        // element-only-empty, which is not a valid base.
        ComplexContentResult::Empty => base.mixed,
    }
}

#[derive(Debug, Clone)]
struct NormalizedParticle {
    term: NormalizedParticleTerm,
    min_occurs: u32,
    max_occurs: Option<u32>,
    source: Option<SourceRef>,
}

#[derive(Debug, Clone)]
enum NormalizedParticleTerm {
    Element(NormalizedElement),
    Wildcard(Box<NormalizedWildcard>),
    Group(NormalizedGroup),
}

#[derive(Debug, Clone)]
struct NormalizedElement {
    name: NameId,
    namespace: Option<NameId>,
    type_key: TypeKey,
    element_key: Option<ElementKey>,
    block: DerivationSet,
    nillable: bool,
    fixed_value: Option<String>,
}

#[derive(Debug, Clone)]
struct NormalizedWildcard {
    wildcard: WildcardResult,
    target_namespace: Option<NameId>,
}

#[derive(Debug, Clone)]
struct NormalizedGroup {
    compositor: Compositor,
    particles: Vec<NormalizedParticle>,
}

struct ParticleNormalizer<'a> {
    schema_set: &'a SchemaSet,
    target_namespace: Option<NameId>,
    resolved_types: &'a [Option<TypeKey>],
    flat_index: usize,
    depth: usize,
}

const MAX_PARTICLE_RESTRICTION_DEPTH: usize = 100;

impl<'a> ParticleNormalizer<'a> {
    fn new(
        schema_set: &'a SchemaSet,
        target_namespace: Option<NameId>,
        resolved_types: &'a [Option<TypeKey>],
    ) -> Self {
        Self {
            schema_set,
            target_namespace,
            resolved_types,
            flat_index: 0,
            depth: 0,
        }
    }

    fn normalize_particle(
        &mut self,
        particle: &ParticleResult,
    ) -> SchemaResult<NormalizedParticle> {
        if self.depth >= MAX_PARTICLE_RESTRICTION_DEPTH {
            return Err(SchemaError::internal(
                "particle restriction normalization exceeded recursion limit",
            ));
        }

        self.depth += 1;
        let term = match &particle.term {
            ParticleTerm::Element(elem) => {
                let source = particle.source.as_ref().or(elem.source.as_ref());
                NormalizedParticleTerm::Element(self.normalize_element(elem, source)?)
            }
            ParticleTerm::Any(wildcard) => {
                NormalizedParticleTerm::Wildcard(Box::new(NormalizedWildcard {
                    wildcard: wildcard.clone(),
                    target_namespace: self.target_namespace,
                }))
            }
            ParticleTerm::Group(group) => {
                NormalizedParticleTerm::Group(self.normalize_group(group)?)
            }
        };
        self.depth -= 1;

        Ok(collapse_single_child_groups(NormalizedParticle {
            term,
            min_occurs: particle.min_occurs,
            max_occurs: particle.max_occurs,
            source: particle.source.clone(),
        }))
    }

    fn normalize_element(
        &mut self,
        elem: &ElementFrameResult,
        source: Option<&SourceRef>,
    ) -> SchemaResult<NormalizedElement> {
        if let Some(ref_name) = &elem.ref_name {
            let elem_key = self
                .schema_set
                .lookup_element(ref_name.namespace, ref_name.local_name);
            let (type_key, block, nillable, fixed_value) = elem_key
                .and_then(|key| self.schema_set.arenas.elements.get(key))
                .map(|decl| {
                    let (eff_block, _) =
                        crate::compiler::substitution::effective_element_constraints(
                            self.schema_set,
                            decl,
                        );
                    let tk = decl
                        .resolved_type
                        .unwrap_or_else(|| TypeKey::Complex(self.schema_set.any_type_key()));
                    (tk, eff_block, decl.nillable, decl.fixed_value.clone())
                })
                .unwrap_or_else(|| {
                    (
                        TypeKey::Complex(self.schema_set.any_type_key()),
                        DerivationSet::empty(),
                        false,
                        None,
                    )
                });
            return Ok(NormalizedElement {
                name: ref_name.local_name,
                namespace: ref_name.namespace,
                type_key,
                element_key: elem_key,
                block,
                nillable,
                fixed_value,
            });
        }

        let name = elem
            .name
            .ok_or_else(|| SchemaError::internal("element particle missing name and ref"))?;
        let index = self.flat_index;
        self.flat_index += 1;

        let namespace = self.schema_set.effective_local_element_namespace(
            elem.target_namespace,
            elem.form.as_deref(),
            source,
            self.target_namespace,
        );
        let type_key = self
            .resolved_types
            .get(index)
            .copied()
            .flatten()
            .or_else(|| resolve_element_type_ref(self.schema_set, elem))
            .unwrap_or_else(|| TypeKey::Complex(self.schema_set.any_type_key()));

        // Compute effective block for local element
        // block=None means absent → inherit blockDefault; Some(b) means explicit (including "").
        let block = match elem.block {
            Some(b) => b,
            None => source
                .and_then(|s| {
                    let doc_id = s.schema_defaults_doc.unwrap_or(s.doc_id);
                    self.schema_set
                        .documents
                        .get(doc_id as usize)
                        .map(|d| d.block_default)
                })
                .unwrap_or_default(),
        };

        Ok(NormalizedElement {
            name,
            namespace,
            type_key,
            element_key: None,
            block,
            nillable: elem.nillable,
            fixed_value: elem.fixed_value.clone(),
        })
    }

    fn normalize_group(&mut self, group: &ModelGroupDefResult) -> SchemaResult<NormalizedGroup> {
        if let Some(ref_name) = &group.ref_name {
            let group_key = self
                .schema_set
                .lookup_model_group(ref_name.namespace, ref_name.local_name)
                .ok_or_else(|| SchemaError::internal("model group reference was not resolved"))?;
            let group_data = self
                .schema_set
                .arenas
                .get_model_group(group_key)
                .ok_or_else(|| SchemaError::internal("resolved model group not found"))?;
            let compositor = group_data
                .compositor
                .ok_or_else(|| SchemaError::internal("resolved model group missing compositor"))?;
            let mut nested = ParticleNormalizer::new(
                self.schema_set,
                group_data.target_namespace,
                &group_data.resolved_particle_types,
            );
            nested.depth = self.depth;
            let particles = group_data
                .particles
                .iter()
                .map(|particle| nested.normalize_particle(particle))
                .collect::<SchemaResult<Vec<_>>>()?;
            return Ok(NormalizedGroup {
                compositor,
                particles,
            });
        }

        let compositor = group
            .compositor
            .ok_or_else(|| SchemaError::internal("inline model group missing compositor"))?;
        let particles = group
            .particles
            .iter()
            .map(|particle| self.normalize_particle(particle))
            .collect::<SchemaResult<Vec<_>>>()?;
        Ok(NormalizedGroup {
            compositor,
            particles,
        })
    }
}

fn resolve_element_type_ref(schema_set: &SchemaSet, elem: &ElementFrameResult) -> Option<TypeKey> {
    match &elem.type_ref {
        Some(crate::parser::frames::TypeRefResult::QName(qname)) => schema_set
            .lookup_type(qname.namespace, qname.local_name)
            .or_else(|| schema_set.get_built_in_type_by_qname(qname.namespace, qname.local_name)),
        _ => None,
    }
}

fn validate_content_particle_restriction(
    schema_set: &SchemaSet,
    derived: &crate::arenas::ComplexTypeDefData,
    base: &crate::arenas::ComplexTypeDefData,
) -> SchemaResult<()> {
    let derived_particle = complex_content_particle(&derived.content);
    // For the base, resolve effective content by walking up extension chain.
    // An empty extension inherits its base type's content model.
    let (effective_base, base_particle) = effective_base_content_particle(schema_set, base);

    let location = derived
        .source
        .as_ref()
        .and_then(|s| schema_set.source_maps.locate(s));
    let type_name = format_type_name(schema_set, derived.name, derived.target_namespace);
    let base_name = format_type_name(schema_set, base.name, base.target_namespace);

    match (derived_particle, base_particle) {
        (None, None) => Ok(()),
        (Some(derived_particle), None) => {
            let derived_particle = normalize_type_particle(schema_set, derived, derived_particle)?;
            // Empty derived particle can restrict an empty-particle base, but not simpleContent (different violation).
            if !matches!(effective_base.content, ComplexContentResult::Simple(_))
                && is_effectively_empty(&derived_particle)
            {
                Ok(())
            } else {
                Err(SchemaError::structural(
                    "derivation-ok-restriction",
                    format!(
                        "Complex type '{}' adds particle content while restricting '{}' which has empty content",
                        type_name, base_name
                    ),
                    location,
                ))
            }
        }
        (None, Some(base_particle)) => {
            let base_particle = normalize_type_particle(schema_set, effective_base, base_particle)?;
            if particle_is_emptiable(&base_particle) {
                Ok(())
            } else {
                Err(SchemaError::structural(
                    "derivation-ok-restriction",
                    format!(
                        "Complex type '{}' removes required particle content from base type '{}'",
                        type_name, base_name
                    ),
                    location,
                ))
            }
        }
        (Some(derived_particle), Some(base_particle)) => {
            let derived_particle = normalize_type_particle(schema_set, derived, derived_particle)?;
            let base_particle = normalize_type_particle(schema_set, effective_base, base_particle)?;

            if is_effectively_empty(&derived_particle) {
                if particle_is_emptiable(&base_particle) {
                    return Ok(());
                } else {
                    return Err(SchemaError::structural(
                        "derivation-ok-restriction",
                        format!(
                            "Complex type '{}' removes required particle content from base type '{}'",
                            type_name, base_name
                        ),
                        location,
                    ));
                }
            }

            // All compositor combinations are now handled by
            // particle_restricts: same-compositor, sequence→choice,
            // sequence→all, choice expansion, and the catch-all rejection
            // for structurally forbidden pairs like all→choice.

            if particle_restricts(schema_set, &derived_particle, &base_particle) {
                Ok(())
            } else {
                Err(SchemaError::structural(
                    "derivation-ok-restriction",
                    format!(
                        "Content model of '{}' is not a valid restriction of base type '{}'",
                        type_name, base_name
                    ),
                    location,
                ))
            }
        }
    }
}

/// Check if a normalized particle is an empty group (all children removed as pointless).
fn is_empty_group(particle: &NormalizedParticle) -> bool {
    matches!(&particle.term, NormalizedParticleTerm::Group(group) if group.particles.is_empty())
}

/// Top-level particle with maxOccurs=0 or fully pruned group — treated as empty content per §3.8.
fn is_effectively_empty(particle: &NormalizedParticle) -> bool {
    particle.max_occurs == Some(0) || is_empty_group(particle)
}

fn complex_content_particle(content: &ComplexContentResult) -> Option<&ParticleResult> {
    match content {
        ComplexContentResult::Complex(def) => def.particle.as_ref(),
        ComplexContentResult::Empty | ComplexContentResult::Simple(_) => None,
    }
}

/// Walk up the extension chain to find the effective content particle.
/// Empty extensions inherit their base type's content model.
/// Returns (type_def_owning_particle, particle) so the normalizer uses the
/// correct target_namespace and resolved_content_particle_types.
fn effective_base_content_particle<'a>(
    schema_set: &'a SchemaSet,
    base: &'a crate::arenas::ComplexTypeDefData,
) -> (
    &'a crate::arenas::ComplexTypeDefData,
    Option<&'a ParticleResult>,
) {
    let mut current = base;
    let mut depth = 0;
    loop {
        if let Some(particle) = complex_content_particle(&current.content) {
            return (current, Some(particle));
        }
        // If this type has no content and was derived by extension, check its base
        if current.derivation_method != Some(DerivationMethod::Extension) {
            return (current, None);
        }
        let Some(TypeKey::Complex(base_key)) = current.resolved_base_type else {
            return (current, None);
        };
        let Some(base_type) = schema_set.arenas.complex_types.get(base_key) else {
            return (current, None);
        };
        depth += 1;
        if depth > 50 {
            return (current, None); // safety limit
        }
        current = base_type;
    }
}

fn normalize_type_particle(
    schema_set: &SchemaSet,
    type_def: &crate::arenas::ComplexTypeDefData,
    particle: &ParticleResult,
) -> SchemaResult<NormalizedParticle> {
    let mut normalizer = ParticleNormalizer::new(
        schema_set,
        type_def.target_namespace,
        &type_def.resolved_content_particle_types,
    );
    let particle = normalizer.normalize_particle(particle)?;
    let particle = remove_pointless_particles(particle);
    // XSD 1.1: skip flattening to preserve structural grouping needed for
    // intensional restriction (e.g. a single-branch choice whose collapsed
    // sequence must match against a multi-branch choice in the base).
    if !schema_set.is_xsd11() {
        let particle = flatten_same_compositor_groups(particle);
        return Ok(particle);
    }
    Ok(particle)
}

/// Normalize a top-level named model group into a `NormalizedParticle` for
/// §src-redefine 6.2.2 restriction comparisons.
///
/// **Chain-of-redefine caveat**: when called on an original whose own
/// particles include `group-ref`s, those refs are resolved via
/// `schema_set.lookup_model_group` inside [`ParticleNormalizer::normalize_group`]
/// — which returns the *currently bound* version. For a chain
/// `orig → v1 → v2`, v1's inner group-refs resolve to whatever the
/// current namespace binding is, not to what v1 saw at creation time. This
/// is a pre-existing limitation shared with the complex-type restriction
/// path; it is not fixed here.
fn normalize_model_group_as_particle(
    schema_set: &SchemaSet,
    group_data: &crate::arenas::ModelGroupData,
) -> SchemaResult<NormalizedParticle> {
    let compositor = group_data
        .compositor
        .ok_or_else(|| SchemaError::internal("redefined named model group missing compositor"))?;

    let mut normalizer = ParticleNormalizer::new(
        schema_set,
        group_data.target_namespace,
        &group_data.resolved_particle_types,
    );
    let particles = group_data
        .particles
        .iter()
        .map(|particle| normalizer.normalize_particle(particle))
        .collect::<SchemaResult<Vec<_>>>()?;

    let wrapper = NormalizedParticle {
        term: NormalizedParticleTerm::Group(NormalizedGroup {
            compositor,
            particles,
        }),
        min_occurs: group_data.min_occurs,
        max_occurs: group_data.max_occurs,
        source: group_data.source.clone(),
    };

    // `normalize_particle` collapses every child; the outer wrapper we
    // built by hand above is *not* collapsed and must be so explicitly so
    // a single-element named group ends up shaped identically to the same
    // content inside a complex type.
    let particle = collapse_single_child_groups(wrapper);
    let particle = remove_pointless_particles(particle);
    // XSD 1.1: skip flattening (same rationale as `normalize_type_particle`).
    if !schema_set.is_xsd11() {
        let particle = flatten_same_compositor_groups(particle);
        return Ok(particle);
    }
    Ok(particle)
}

/// Remove "pointless" particles per Section 3.8 normalization:
/// - Particles with maxOccurs=0 are effectively absent
/// - Groups with no remaining children after removal are also pointless
fn remove_pointless_particles(mut particle: NormalizedParticle) -> NormalizedParticle {
    if let NormalizedParticleTerm::Group(group) = &mut particle.term {
        group.particles = group
            .particles
            .drain(..)
            .map(remove_pointless_particles)
            .filter(|p| p.max_occurs != Some(0))
            .collect();
    }
    particle
}

/// Flatten nested groups with unit occurs and the same compositor into
/// their parent (Section 3.8 particle normalization).
/// E.g. sequence(sequence{1,1}(a, b), c) → sequence(a, b, c).
fn flatten_same_compositor_groups(mut particle: NormalizedParticle) -> NormalizedParticle {
    if let NormalizedParticleTerm::Group(group) = &mut particle.term {
        // First recurse into children
        group.particles = group
            .particles
            .drain(..)
            .map(flatten_same_compositor_groups)
            .collect();
        // Then flatten children that are same-compositor groups with unit occurs
        let parent_compositor = group.compositor;
        let mut flattened = Vec::with_capacity(group.particles.len());
        for child in group.particles.drain(..) {
            if let NormalizedParticleTerm::Group(ref child_group) = child.term {
                if child_group.compositor == parent_compositor
                    && occurs_is_unit(child.min_occurs, child.max_occurs)
                {
                    flattened.extend(child_group.particles.iter().cloned());
                    continue;
                }
            }
            flattened.push(child);
        }
        group.particles = flattened;
    }
    particle
}

fn collapse_single_child_groups(mut particle: NormalizedParticle) -> NormalizedParticle {
    if let NormalizedParticleTerm::Group(group) = &mut particle.term {
        group.particles = group
            .particles
            .drain(..)
            .map(collapse_single_child_groups)
            .collect();
    }

    loop {
        let child = match &particle.term {
            NormalizedParticleTerm::Group(group)
                if group.particles.len() == 1
                    && can_collapse_single_child_group(
                        group.compositor,
                        particle.min_occurs,
                        particle.max_occurs,
                        &group.particles[0],
                    ) =>
            {
                Some(group.particles[0].clone())
            }
            _ => None,
        };
        let Some(child) = child else {
            return particle;
        };
        let (min_occurs, max_occurs) = multiply_occurs(
            particle.min_occurs,
            particle.max_occurs,
            child.min_occurs,
            child.max_occurs,
        );
        particle = NormalizedParticle {
            term: child.term,
            min_occurs,
            max_occurs,
            source: particle.source.clone().or(child.source),
        };
    }
}

fn can_collapse_single_child_group(
    compositor: Compositor,
    group_min_occurs: u32,
    group_max_occurs: Option<u32>,
    child: &NormalizedParticle,
) -> bool {
    if compositor == Compositor::Choice {
        return true;
    }

    occurs_is_unit(group_min_occurs, group_max_occurs) || child.max_occurs == Some(1)
}

fn occurs_is_unit(min_occurs: u32, max_occurs: Option<u32>) -> bool {
    min_occurs == 1 && max_occurs == Some(1)
}

fn multiply_occurs(
    left_min: u32,
    left_max: Option<u32>,
    right_min: u32,
    right_max: Option<u32>,
) -> (u32, Option<u32>) {
    let min_occurs = left_min.saturating_mul(right_min);
    let max_occurs = match (left_max, right_max) {
        (Some(left), Some(right)) => Some(left.saturating_mul(right)),
        (Some(0), None) | (None, Some(0)) => Some(0),
        _ => None,
    };
    (min_occurs, max_occurs)
}

/// XSD 1.1: fold a single-child sequence/all group by multiplying occurs.
/// sequence{M,N}(e{m,n}) ≡ e{M*m, N*n}
fn fold_single_child_group(particle: &NormalizedParticle) -> Option<NormalizedParticle> {
    if let NormalizedParticleTerm::Group(group) = &particle.term {
        if group.particles.len() == 1
            && matches!(group.compositor, Compositor::Sequence | Compositor::All)
        {
            let child = &group.particles[0];
            let (min_occurs, max_occurs) = multiply_occurs(
                particle.min_occurs,
                particle.max_occurs,
                child.min_occurs,
                child.max_occurs,
            );
            return Some(NormalizedParticle {
                term: child.term.clone(),
                min_occurs,
                max_occurs,
                source: particle.source.clone().or(child.source.clone()),
            });
        }
    }
    None
}

fn particle_restricts(
    schema_set: &SchemaSet,
    derived: &NormalizedParticle,
    base: &NormalizedParticle,
) -> bool {
    // XSD 1.1 intensional restriction: fold single-child sequence/all groups
    // symmetrically on both sides so they are compared in the same normal form.
    if schema_set.is_xsd11() {
        if let Some(folded) = fold_single_child_group(derived) {
            return particle_restricts(schema_set, &folded, base);
        }
        if let Some(folded_base) = fold_single_child_group(base) {
            return particle_restricts(schema_set, derived, &folded_base);
        }
    }

    // XSD 1.0: A non-choice optional particle cannot restrict an optional non-repeated
    // multi-branch choice. The expand_choice_branches approach merges choice occurs into
    // branches, which gives wrong results for RecurseLax when max_occurs=1.
    // For repeated choices (max>1), the spec is ambiguous — provisionally accept.
    if schema_set.is_xsd10()
        && derived.min_occurs == 0
        && !matches!(
            &derived.term,
            NormalizedParticleTerm::Group(group) if group.compositor == Compositor::Choice
        )
        && matches!(
            &base.term,
            NormalizedParticleTerm::Group(group)
                if group.compositor == Compositor::Choice
                    && base.min_occurs == 0
                    && base.max_occurs == Some(1)
                    && group.particles.len() > 1
        )
    {
        return false;
    }

    if let Some(base_branches) = expand_choice_branches(base) {
        if let Some(derived_branches) = expand_choice_branches(derived) {
            // XSD 1.0 RecurseLax: order-preserving mapping required.
            // XSD 1.1: unordered set-based matching.
            if schema_set.is_xsd10() {
                return choice_branches_restrict_ordered(
                    schema_set,
                    &derived_branches,
                    &base_branches,
                );
            }
            // XSD 1.1: each derived branch must restrict some base branch —
            // OR, when the derived branch is emptiable and at least one base
            // branch is emptiable, the derived branch's empty production is
            // covered by that emptiable base branch and the non-empty form
            // (min≥1) must restrict some base branch. This handles cases
            // like addB118 where the derived choice is optional (min=0) but
            // no single base choice branch is emptiable AND accepts the
            // derived's elements — the union of branches covers it.
            let base_has_emptiable_branch = base_branches.iter().any(particle_is_emptiable);
            return derived_branches.iter().all(|branch| {
                if base_branches
                    .iter()
                    .any(|candidate| particle_restricts(schema_set, branch, candidate))
                {
                    return true;
                }
                if branch.min_occurs == 0 && base_has_emptiable_branch {
                    let mut non_empty = branch.clone();
                    non_empty.min_occurs = non_empty.min_occurs.max(1);
                    if non_empty.max_occurs.is_some_and(|m| m == 0) {
                        // original was min=0,max=0 (empty); empty production
                        // alone is covered, no non-empty form to check.
                        return true;
                    }
                    return base_branches
                        .iter()
                        .any(|candidate| particle_restricts(schema_set, &non_empty, candidate));
                }
                false
            });
        }

        // Sequence-vs-choice: dedicated handler instead of "any branch" check.
        if let NormalizedParticleTerm::Group(derived_group) = &derived.term {
            if derived_group.compositor == Compositor::Sequence {
                // XSD 1.1: try "restricts any single branch" first.
                // Sound because if derived restricts one branch, it restricts
                // a subset of the choice's language.
                if schema_set.is_xsd11() {
                    let any_branch = base_branches
                        .iter()
                        .any(|candidate| particle_restricts(schema_set, derived, candidate));
                    if any_branch {
                        return true;
                    }
                }
                let NormalizedParticleTerm::Group(base_group) = &base.term else {
                    unreachable!()
                };
                return sequence_restricts_choice(
                    schema_set,
                    derived,
                    derived_group,
                    base,
                    base_group,
                );
            }
        }

        return base_branches
            .iter()
            .any(|candidate| particle_restricts(schema_set, derived, candidate));
    }

    if let Some(derived_branches) = expand_choice_branches(derived) {
        return derived_branches
            .iter()
            .all(|branch| particle_restricts(schema_set, branch, base));
    }

    match (&derived.term, &base.term) {
        (
            NormalizedParticleTerm::Element(derived_element),
            NormalizedParticleTerm::Element(base_element),
        ) => {
            let names_match = derived_element.name == base_element.name
                && derived_element.namespace == base_element.namespace;
            let subst_match = !names_match
                && match (derived_element.element_key, base_element.element_key) {
                    (Some(d_key), Some(b_key)) => {
                        crate::compiler::substitution::is_element_substitutable_for(
                            schema_set, b_key, d_key,
                        )
                    }
                    _ => false,
                };
            // NameAndTypeOK (§3.9.6):
            // 1. Names match or substitution group
            (names_match || subst_match)
            // 2. Occurrence range subset
            && occurs_range_is_subset(
                derived.min_occurs,
                derived.max_occurs,
                base.min_occurs,
                base.max_occurs,
            )
            // 3. nillable: derived nillable only if base nillable
            && (base_element.nillable || !derived_element.nillable)
            // 4. fixed value: if base is fixed, derived must be fixed with same value (value-space)
            && match (&base_element.fixed_value, &derived_element.fixed_value) {
                (None, _) => true,
                (Some(_), None) => false,
                (Some(base_fixed), Some(derived_fixed)) => {
                    crate::validation::simple::fixed_values_equal(
                        derived_fixed,
                        base_fixed,
                        Some(derived_element.type_key),
                        schema_set,
                    )
                }
            }
            // TODO: 5. identity-constraint definitions subset (not yet implemented)
            // 6. block superset (masked to element-relevant bits)
            && derived_element.block.element_block_mask()
                .contains(base_element.block.element_block_mask())
            // 7. type derivation
            && schema_set.is_type_derived_from(
                derived_element.type_key,
                base_element.type_key,
                DerivationSet::extension(),
            )
        }
        (
            NormalizedParticleTerm::Element(element),
            NormalizedParticleTerm::Wildcard(base_wildcard),
        ) => {
            occurs_range_is_subset(
                derived.min_occurs,
                derived.max_occurs,
                base.min_occurs,
                base.max_occurs,
            ) && wildcard_allows_element(element, base_wildcard)
        }
        (
            NormalizedParticleTerm::Wildcard(derived_wildcard),
            NormalizedParticleTerm::Wildcard(base_wildcard),
        ) => {
            occurs_range_is_subset(
                derived.min_occurs,
                derived.max_occurs,
                base.min_occurs,
                base.max_occurs,
            ) && wildcard_restricts(derived_wildcard, base_wildcard)
        }
        (
            NormalizedParticleTerm::Group(derived_group),
            NormalizedParticleTerm::Wildcard(base_wildcard),
        ) => group_particle_restricts_wildcard(derived, derived_group, base, base_wildcard),
        (
            NormalizedParticleTerm::Group(derived_group),
            NormalizedParticleTerm::Group(base_group),
        ) if derived_group.compositor == base_group.compositor => {
            if !occurs_range_is_subset(
                derived.min_occurs,
                derived.max_occurs,
                base.min_occurs,
                base.max_occurs,
            ) {
                return false;
            }
            match derived_group.compositor {
                Compositor::Sequence => sequence_particles_restrict(
                    schema_set,
                    &derived_group.particles,
                    &base_group.particles,
                ),
                Compositor::All => all_particles_restrict(
                    schema_set,
                    &derived_group.particles,
                    &base_group.particles,
                ),
                Compositor::Choice => unreachable!("choice particles are handled earlier"),
            }
        }
        // Sequence:All — RecurseUnordered (§3.9.6): unordered bipartite
        // matching regardless of XSD version.  The XSD 1.0 ordered fallback
        // in all_particles_restrict is only correct for All:All (Recurse).
        (
            NormalizedParticleTerm::Group(derived_group),
            NormalizedParticleTerm::Group(base_group),
        ) if derived_group.compositor == Compositor::Sequence
            && base_group.compositor == Compositor::All =>
        {
            if !occurs_range_is_subset(
                derived.min_occurs,
                derived.max_occurs,
                base.min_occurs,
                base.max_occurs,
            ) {
                return false;
            }
            recurse_unordered(schema_set, &derived_group.particles, &base_group.particles)
        }
        // recurseAsIfGroup: wrap derived element/wildcard in an implicit group{1,1}
        // and check outer occurs before delegating to sequence/all matching.
        (
            NormalizedParticleTerm::Element(_) | NormalizedParticleTerm::Wildcard(_),
            NormalizedParticleTerm::Group(base_group),
        ) if base_group.compositor == Compositor::Sequence => {
            occurs_range_is_subset(1, Some(1), base.min_occurs, base.max_occurs)
                && sequence_particles_restrict(
                    schema_set,
                    std::slice::from_ref(derived),
                    &base_group.particles,
                )
        }
        (
            NormalizedParticleTerm::Element(_) | NormalizedParticleTerm::Wildcard(_),
            NormalizedParticleTerm::Group(base_group),
        ) if base_group.compositor == Compositor::All => {
            occurs_range_is_subset(1, Some(1), base.min_occurs, base.max_occurs)
                && all_particles_restrict(
                    schema_set,
                    std::slice::from_ref(derived),
                    &base_group.particles,
                )
        }
        _ => false,
    }
}

fn group_particle_restricts_wildcard(
    derived: &NormalizedParticle,
    group: &NormalizedGroup,
    base: &NormalizedParticle,
    wildcard: &NormalizedWildcard,
) -> bool {
    let (derived_min, derived_max) = particle_total_occurrence_range(derived);
    if !occurs_range_is_subset(derived_min, derived_max, base.min_occurs, base.max_occurs) {
        return false;
    }

    group_particles_fit_wildcard(&group.particles, wildcard)
}

fn occurs_range_is_subset(
    derived_min: u32,
    derived_max: Option<u32>,
    base_min: u32,
    base_max: Option<u32>,
) -> bool {
    if derived_min < base_min {
        return false;
    }

    match (derived_max, base_max) {
        (_, None) => true,
        (Some(derived), Some(base)) => derived <= base,
        (None, Some(_)) => false,
    }
}

fn expand_choice_branches(particle: &NormalizedParticle) -> Option<Vec<NormalizedParticle>> {
    let NormalizedParticleTerm::Group(group) = &particle.term else {
        return None;
    };
    if group.compositor != Compositor::Choice {
        return None;
    }

    Some(
        group
            .particles
            .iter()
            .map(|child| {
                let (min_occurs, max_occurs) = multiply_occurs(
                    particle.min_occurs,
                    particle.max_occurs,
                    child.min_occurs,
                    child.max_occurs,
                );
                collapse_single_child_groups(NormalizedParticle {
                    term: child.term.clone(),
                    min_occurs,
                    max_occurs,
                    source: particle.source.clone().or(child.source.clone()),
                })
            })
            .collect(),
    )
}

fn particle_total_occurrence_range(particle: &NormalizedParticle) -> (u32, Option<u32>) {
    let (term_min, term_max) = match &particle.term {
        NormalizedParticleTerm::Element(_) | NormalizedParticleTerm::Wildcard(_) => (1, Some(1)),
        NormalizedParticleTerm::Group(group) => match group.compositor {
            Compositor::Sequence | Compositor::All => {
                group
                    .particles
                    .iter()
                    .fold((0u32, Some(0u32)), |(acc_min, acc_max), child| {
                        let (child_min, child_max) = particle_total_occurrence_range(child);
                        (
                            acc_min.saturating_add(child_min),
                            add_optional_occurs(acc_max, child_max),
                        )
                    })
            }
            Compositor::Choice => {
                let mut min_total: Option<u32> = None;
                let mut max_total: Option<Option<u32>> = None;
                for child in &group.particles {
                    let (child_min, child_max) = particle_total_occurrence_range(child);
                    min_total = Some(match min_total {
                        Some(current) => current.min(child_min),
                        None => child_min,
                    });
                    max_total = Some(match max_total {
                        Some(current) => max_optional_occurs(current, child_max),
                        None => child_max,
                    });
                }
                (min_total.unwrap_or(0), max_total.unwrap_or(Some(0)))
            }
        },
    };

    multiply_occurs(particle.min_occurs, particle.max_occurs, term_min, term_max)
}

fn add_optional_occurs(left: Option<u32>, right: Option<u32>) -> Option<u32> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.saturating_add(right)),
        _ => None,
    }
}

fn max_optional_occurs(left: Option<u32>, right: Option<u32>) -> Option<u32> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        _ => None,
    }
}

fn group_particles_fit_wildcard(
    particles: &[NormalizedParticle],
    wildcard: &NormalizedWildcard,
) -> bool {
    particles
        .iter()
        .all(|particle| particle_fits_wildcard(particle, wildcard))
}

fn particle_fits_wildcard(particle: &NormalizedParticle, wildcard: &NormalizedWildcard) -> bool {
    if let Some(branches) = expand_choice_branches(particle) {
        return branches
            .iter()
            .all(|branch| particle_fits_wildcard(branch, wildcard));
    }

    match &particle.term {
        NormalizedParticleTerm::Element(element) => wildcard_allows_element(element, wildcard),
        NormalizedParticleTerm::Wildcard(derived_wildcard) => {
            wildcard_restricts(derived_wildcard, wildcard)
        }
        NormalizedParticleTerm::Group(group) => {
            group_particles_fit_wildcard(&group.particles, wildcard)
        }
    }
}

/// Check whether a derived sequence restricts a base choice.
///
/// Two conditions are verified:
///
/// 1. **Per-particle match** — every child of the derived sequence must
///    restrict at least one *raw* base choice branch (name, type, and
///    per-iteration occurs).
///
/// 2. **Iteration budget** — each non-empty derived particle consumes at
///    least one choice iteration.  The total iterations across all sequence
///    repetitions must fit within the base choice's occurs range.
fn sequence_restricts_choice(
    schema_set: &SchemaSet,
    derived: &NormalizedParticle,
    derived_group: &NormalizedGroup,
    base: &NormalizedParticle,
    base_group: &NormalizedGroup,
) -> bool {
    let base_branches = &base_group.particles;

    let mut required_per_iter: u32 = 0;
    let mut total_per_iter: u32 = 0;

    for derived_child in &derived_group.particles {
        // Each derived child must restrict at least one raw base branch.
        let found = base_branches
            .iter()
            .any(|branch| particle_restricts(schema_set, derived_child, branch));
        if !found {
            return false;
        }

        // Count choice-iteration demand per sequence iteration.
        if derived_child.min_occurs > 0 {
            required_per_iter += 1;
        }
        if derived_child.max_occurs != Some(0) {
            total_per_iter += 1;
        }
    }

    // The total choice iterations across all sequence repetitions must fit
    // within the base choice's occurs range.
    let min_demand = derived.min_occurs.saturating_mul(required_per_iter);
    let max_demand = match derived.max_occurs {
        Some(m) => Some(m.saturating_mul(total_per_iter)),
        None => {
            if total_per_iter == 0 {
                Some(0)
            } else {
                None
            }
        }
    };

    occurs_range_is_subset(min_demand, max_demand, base.min_occurs, base.max_occurs)
}

/// XSD 1.0 RecurseLax: order-preserving matching of choice branches.
/// Each derived branch must map to a base branch at or after the previous match.
/// Unmatched base branches are implicitly skipped (lax, not strict).
fn choice_branches_restrict_ordered(
    schema_set: &SchemaSet,
    derived_branches: &[NormalizedParticle],
    base_branches: &[NormalizedParticle],
) -> bool {
    let mut base_index = 0;
    for derived in derived_branches {
        let mut found = false;
        while base_index < base_branches.len() {
            if particle_restricts(schema_set, derived, &base_branches[base_index]) {
                base_index += 1;
                found = true;
                break;
            }
            base_index += 1;
        }
        if !found {
            return false;
        }
    }
    true
}

fn sequence_particles_restrict(
    schema_set: &SchemaSet,
    derived_particles: &[NormalizedParticle],
    base_particles: &[NormalizedParticle],
) -> bool {
    let mut base_index = 0;
    let mut derived_index = 0;

    while derived_index < derived_particles.len() {
        let mut matched = false;

        while let Some(base) = base_particles.get(base_index) {
            // 1. Direct particle-vs-particle match (the normal greedy step).
            if particle_restricts(schema_set, &derived_particles[derived_index], base) {
                matched = true;
                base_index += 1;
                derived_index += 1;
                break;
            }

            // 2. Atomic sequence-unit match: when the base particle is a
            //    sequence group (e.g. an expanded repetition unit), try to
            //    match a contiguous slice of derived particles against the
            //    unit's children.  This keeps the unit atomic — either the
            //    full slice matches or we fall through.
            if let NormalizedParticleTerm::Group(base_group) = &base.term {
                if base_group.compositor == Compositor::Sequence && !base_group.particles.is_empty()
                {
                    let unit_len = base_group.particles.len();
                    let remaining = derived_particles.len() - derived_index;
                    if remaining >= unit_len
                        && sequence_particles_restrict(
                            schema_set,
                            &derived_particles[derived_index..derived_index + unit_len],
                            &base_group.particles,
                        )
                    {
                        matched = true;
                        base_index += 1;
                        derived_index += unit_len;
                        break;
                    }
                }
            }

            // 3. XSD 1.1: expand choice in derived sequence.
            //    Each branch must independently work from the current base position
            //    for the entire remaining derived + base suffix.
            if schema_set.is_xsd11() {
                if let Some(branches) = expand_choice_branches(&derived_particles[derived_index]) {
                    let all_ok = branches.iter().all(|branch| {
                        let mut remaining = vec![branch.clone()];
                        remaining.extend_from_slice(&derived_particles[derived_index + 1..]);
                        sequence_particles_restrict(
                            schema_set,
                            &remaining,
                            &base_particles[base_index..],
                        )
                    });
                    if all_ok {
                        return true;
                    }
                }
            }

            // 3a. XSD 1.1: inline unit-occurs derived sequence group.
            //    Compensates for the disabled flatten_same_compositor_groups.
            //    sequence{1,1}(a, b, c, ...) at derived can be inlined into
            //    the parent sequence and matched element-by-element against base.
            if schema_set.is_xsd11() {
                if let NormalizedParticleTerm::Group(dg) = &derived_particles[derived_index].term {
                    if dg.compositor == Compositor::Sequence
                        && occurs_is_unit(
                            derived_particles[derived_index].min_occurs,
                            derived_particles[derived_index].max_occurs,
                        )
                        && !dg.particles.is_empty()
                    {
                        let mut inlined = dg.particles.clone();
                        inlined.extend_from_slice(&derived_particles[derived_index + 1..]);
                        if sequence_particles_restrict(
                            schema_set,
                            &inlined,
                            &base_particles[base_index..],
                        ) {
                            return true;
                        }
                    }
                }
            }

            // 4. Skip emptiable base particles.
            if particle_is_emptiable(base) {
                base_index += 1;
                continue;
            }

            return false;
        }

        if !matched {
            return false;
        }
    }

    base_particles[base_index..]
        .iter()
        .all(particle_is_emptiable)
}

/// Merge element particles in an unordered-matching context that share the
/// same expanded name (local + target namespace). The merged particle sums
/// `min_occurs` and `max_occurs` (unbounded on either side stays unbounded).
///
/// Used by `recurse_unordered` to support Sequence→All derivations where the
/// derived sequence lists the same element more than once (saxonData
/// All/all216 is the canonical case). Order within the derived side
/// doesn't affect the base's all-group language, so treating duplicate
/// names as one combined occurrence range is sound.
///
/// Non-element particles (wildcards, nested groups) are passed through
/// unchanged: collapsing them would change their matching semantics.
fn merge_duplicate_elements(particles: &[NormalizedParticle]) -> Vec<NormalizedParticle> {
    let mut merged: Vec<NormalizedParticle> = Vec::with_capacity(particles.len());
    for particle in particles {
        let NormalizedParticleTerm::Element(elem) = &particle.term else {
            merged.push(particle.clone());
            continue;
        };
        let existing = merged.iter_mut().find(|m| {
            matches!(
                &m.term,
                NormalizedParticleTerm::Element(other)
                    if other.name == elem.name && other.namespace == elem.namespace
            )
        });
        if let Some(existing) = existing {
            existing.min_occurs = existing.min_occurs.saturating_add(particle.min_occurs);
            existing.max_occurs = match (existing.max_occurs, particle.max_occurs) {
                (None, _) | (_, None) => None,
                (Some(a), Some(b)) => Some(a.saturating_add(b)),
            };
        } else {
            merged.push(particle.clone());
        }
    }
    merged
}

/// RecurseUnordered: order-independent matching of derived particles against
/// the base all-group's particles. Combines two strategies:
///
/// 1. **Count-based bucket subsumption** (preferred — handles substitution
///    groups, wildcard partition, and choice expansion). Each derived
///    particle is assigned to a base "bucket" (by name match, substitution
///    group head, or wildcard subset). For each bucket the summed derived
///    occurrence range must fit within the base particle's range; unassigned
///    base particles must be emptiable.
///
/// 2. **Bipartite 1-to-1 matching** (fallback for derived particles that
///    contain nested groups not handled by the bucket approach). Each derived
///    particle must restrict some base particle; unmatched base particles
///    must be emptiable.
fn recurse_unordered(
    schema_set: &SchemaSet,
    derived_particles: &[NormalizedParticle],
    base_particles: &[NormalizedParticle],
) -> bool {
    // Try count-based bucket subsumption first — it handles substitution
    // group merging, wildcard partition, and choice distribution.
    if let Some(result) = try_count_based_subsumption(schema_set, derived_particles, base_particles)
    {
        if result {
            return true;
        }
        // Bucket said "no" — but for the failure path we still try bipartite
        // because it can recover via different particle wirings (e.g. when
        // a derived particle could fit either an element or wildcard bucket
        // but the bucket heuristic picked the wrong one).
    }

    // Fallback: bipartite 1-to-1 matching with same-name merge.
    fn backtrack(
        schema_set: &SchemaSet,
        derived_particles: &[NormalizedParticle],
        base_particles: &[NormalizedParticle],
        used: &mut [bool],
        derived_index: usize,
    ) -> bool {
        if derived_index == derived_particles.len() {
            return base_particles
                .iter()
                .enumerate()
                .all(|(index, particle)| used[index] || particle_is_emptiable(particle));
        }

        for (base_index, base_particle) in base_particles.iter().enumerate() {
            if used[base_index]
                || !particle_restricts(schema_set, &derived_particles[derived_index], base_particle)
            {
                continue;
            }
            used[base_index] = true;
            if backtrack(
                schema_set,
                derived_particles,
                base_particles,
                used,
                derived_index + 1,
            ) {
                return true;
            }
            used[base_index] = false;
        }

        false
    }

    // Merge duplicate-name element particles in the derived side: unordered
    // matching counts total occurrences per element, not particle positions.
    let merged_owned;
    let derived_particles = if derived_particles
        .iter()
        .any(|p| matches!(&p.term, NormalizedParticleTerm::Element(_)))
        && has_duplicate_element_names(derived_particles)
    {
        merged_owned = merge_duplicate_elements(derived_particles);
        &merged_owned[..]
    } else {
        derived_particles
    };

    let mut used = vec![false; base_particles.len()];
    backtrack(schema_set, derived_particles, base_particles, &mut used, 0)
}

/// Count-based subsumption: bucket each derived particle by the base particle
/// it maps to, sum derived occurrence ranges per bucket, and check that the
/// summed range fits the base range. Unassigned base particles must be
/// emptiable.
///
/// Returns:
/// - `Some(true)` — the derived particles validly restrict the base under
///   count-based subsumption (substitution groups, wildcard partition, and
///   top-level derived choices are all handled).
/// - `Some(false)` — every derived particle was bucket-able but the
///   per-bucket sum exceeded the base range or an unassigned base particle
///   is not emptiable.
/// - `None` — at least one derived particle is a nested model group; the
///   caller should fall back to bipartite matching.
fn try_count_based_subsumption(
    schema_set: &SchemaSet,
    derived_particles: &[NormalizedParticle],
    base_particles: &[NormalizedParticle],
) -> Option<bool> {
    // Step 1: Expand top-level choices into optional alternatives.
    let expanded = expand_top_level_choices_for_unordered(derived_particles)?;

    // Step 2: Bucket each expanded derived particle to a base index.
    let mut buckets: Vec<Vec<(u32, Option<u32>)>> = vec![Vec::new(); base_particles.len()];
    for derived in &expanded {
        // Nested groups are not bucket-able here.
        if matches!(&derived.term, NormalizedParticleTerm::Group(_)) {
            return None;
        }

        match find_subsumption_bucket(schema_set, derived, base_particles)? {
            BucketAssignment::Single(idx) => {
                buckets[idx].push((derived.min_occurs, derived.max_occurs))
            }
            BucketAssignment::Partition(idxs) => {
                // Each base bucket the partition spans must be emptiable
                // (b_min = 0) since derived elements may not land in it,
                // and derived's max must fit each spanned base's max
                // (worst case: all elements land in one bucket).
                for &i in &idxs {
                    let base = &base_particles[i];
                    if base.min_occurs > 0 {
                        return Some(false);
                    }
                    if !occurs_max_fits(derived.max_occurs, base.max_occurs) {
                        return Some(false);
                    }
                }
                // Contribute (0, derived.max) to each spanned bucket — the
                // total never exceeds the partition's d_max in any one
                // bucket, but might be 0.
                for &i in &idxs {
                    buckets[i].push((0, derived.max_occurs));
                }
            }
            BucketAssignment::None => return Some(false),
        }
    }

    // Step 3: Check each bucket's summed range fits the base range; unmatched
    // base particles must be emptiable.
    for (i, ranges) in buckets.iter().enumerate() {
        let base = &base_particles[i];
        if ranges.is_empty() {
            if !particle_is_emptiable(base) {
                return Some(false);
            }
        } else {
            let (sum_min, sum_max) =
                ranges
                    .iter()
                    .fold((0u32, Some(0u32)), |(amin, amax), &(min, max)| {
                        (amin.saturating_add(min), add_optional_occurs(amax, max))
                    });
            if !occurs_range_is_subset(sum_min, sum_max, base.min_occurs, base.max_occurs) {
                return Some(false);
            }
        }
    }

    Some(true)
}

/// Expand top-level derived choices into a flat list of element/wildcard
/// particles, treating each branch as an optional alternative. This enables
/// the count-based subsumption to handle cases like all234 where a derived
/// sequence contains a `<xs:choice>` that distributes across base all-group
/// particles.
///
/// Returns `None` if any derived particle contains a nested group whose
/// shape can't be flattened (the caller falls back to bipartite matching).
fn expand_top_level_choices_for_unordered(
    particles: &[NormalizedParticle],
) -> Option<Vec<NormalizedParticle>> {
    let mut result = Vec::with_capacity(particles.len());
    for p in particles {
        match &p.term {
            NormalizedParticleTerm::Group(group) if group.compositor == Compositor::Choice => {
                // Each branch becomes a particle with min=0 (might not be picked)
                // and max = outer_max * branch_max.
                let outer_max = p.max_occurs;
                for branch in &group.particles {
                    if matches!(&branch.term, NormalizedParticleTerm::Group(_)) {
                        // Nested group inside choice — bail to bipartite.
                        return None;
                    }
                    let new_max = match (outer_max, branch.max_occurs) {
                        (Some(om), Some(bm)) => Some(om.saturating_mul(bm)),
                        _ => None,
                    };
                    result.push(NormalizedParticle {
                        term: branch.term.clone(),
                        min_occurs: 0,
                        max_occurs: new_max,
                        source: branch.source.clone(),
                    });
                }
            }
            NormalizedParticleTerm::Group(_) => {
                // Other nested groups (Sequence, All) — bail to bipartite.
                return None;
            }
            _ => result.push(p.clone()),
        }
    }
    Some(result)
}

/// How a derived particle is assigned to base particle bucket(s).
#[derive(Debug, Clone)]
enum BucketAssignment {
    /// derived maps to a single base particle.
    Single(usize),
    /// derived (necessarily a wildcard) partitions across multiple base
    /// wildcards — its admissible (ns, name) set is covered by the union of
    /// the listed base wildcards.
    Partition(Vec<usize>),
    /// derived has no matching base particle (restriction is invalid).
    None,
}

/// Find which base particle bucket(s) the derived particle should be assigned
/// to.
///
/// Returns:
/// - `Some(BucketAssignment::Single(idx))` — derived maps to base particle `idx`.
/// - `Some(BucketAssignment::Partition(idxs))` — derived wildcard partitions
///   across multiple base wildcards (wild049-style restriction).
/// - `Some(BucketAssignment::None)` — derived has no matching base particle.
/// - `None` — derived is a nested group that can't be bucketed (caller
///   should fall back to bipartite matching).
///
/// Search order:
/// 1. Direct element name + namespace match (NameAndTypeOK without occurs).
/// 2. Substitution group head match.
/// 3. Element fitting a base wildcard.
/// 4. Wildcard subset of a base wildcard.
/// 5. Wildcard subset of the union of multiple base wildcards (partition).
fn find_subsumption_bucket(
    schema_set: &SchemaSet,
    derived: &NormalizedParticle,
    base_particles: &[NormalizedParticle],
) -> Option<BucketAssignment> {
    match &derived.term {
        NormalizedParticleTerm::Element(d_elem) => {
            // 1. Direct name match
            for (i, base) in base_particles.iter().enumerate() {
                if let NormalizedParticleTerm::Element(b_elem) = &base.term {
                    if d_elem.name == b_elem.name
                        && d_elem.namespace == b_elem.namespace
                        && name_and_type_ok_no_occurs(schema_set, d_elem, b_elem)
                    {
                        return Some(BucketAssignment::Single(i));
                    }
                }
            }
            // 2. Substitution group head match
            for (i, base) in base_particles.iter().enumerate() {
                if let NormalizedParticleTerm::Element(b_elem) = &base.term {
                    if d_elem.name == b_elem.name && d_elem.namespace == b_elem.namespace {
                        continue; // already tried
                    }
                    if derived_element_substitutes_base(schema_set, d_elem, b_elem)
                        && name_and_type_ok_no_occurs(schema_set, d_elem, b_elem)
                    {
                        return Some(BucketAssignment::Single(i));
                    }
                }
            }
            // 3. Element fits base wildcard
            for (i, base) in base_particles.iter().enumerate() {
                if let NormalizedParticleTerm::Wildcard(b_wc) = &base.term {
                    if wildcard_allows_element(d_elem, b_wc) {
                        return Some(BucketAssignment::Single(i));
                    }
                }
            }
            Some(BucketAssignment::None)
        }
        NormalizedParticleTerm::Wildcard(d_wc) => {
            // 4. Wildcard subset of base wildcard
            for (i, base) in base_particles.iter().enumerate() {
                if let NormalizedParticleTerm::Wildcard(b_wc) = &base.term {
                    if wildcard_restricts(d_wc, b_wc) {
                        return Some(BucketAssignment::Single(i));
                    }
                }
            }
            // 5. Wildcard subset of the union of multiple base wildcards.
            // Collect all base wildcard indices and check coverage. Only
            // consider buckets that share the derived processContents
            // strictness or stronger.
            let candidate_idxs: Vec<usize> = base_particles
                .iter()
                .enumerate()
                .filter_map(|(i, base)| {
                    let NormalizedParticleTerm::Wildcard(b_wc) = &base.term else {
                        return None;
                    };
                    if process_contents_strictness(d_wc.wildcard.process_contents)
                        < process_contents_strictness(b_wc.wildcard.process_contents)
                    {
                        return None;
                    }
                    Some(i)
                })
                .collect();
            if candidate_idxs.len() >= 2 {
                let bases: Vec<&NormalizedWildcard> = candidate_idxs
                    .iter()
                    .map(|&i| match &base_particles[i].term {
                        NormalizedParticleTerm::Wildcard(b_wc) => b_wc.as_ref(),
                        _ => unreachable!(),
                    })
                    .collect();
                if let Some(spanned) = wildcard_subset_of_union(d_wc, &bases) {
                    let idxs: Vec<usize> = spanned
                        .into_iter()
                        .map(|local_idx| candidate_idxs[local_idx])
                        .collect();
                    return Some(BucketAssignment::Partition(idxs));
                }
            }
            Some(BucketAssignment::None)
        }
        NormalizedParticleTerm::Group(_) => None,
    }
}

/// Return the smallest occurs-max that fits both `derived_max` and
/// `base_max`, treating `None` as unbounded. `derived_max` must fit within
/// `base_max`.
fn occurs_max_fits(derived_max: Option<u32>, base_max: Option<u32>) -> bool {
    match (derived_max, base_max) {
        (_, None) => true,
        (None, Some(_)) => false,
        (Some(d), Some(b)) => d <= b,
    }
}

/// NameAndTypeOK clauses 3, 4, 6, 7 (everything except clause 2 — the
/// occurrence range subset check). Used by count-based subsumption where
/// the occurrence check is performed at the bucket aggregate level.
fn name_and_type_ok_no_occurs(
    schema_set: &SchemaSet,
    derived: &NormalizedElement,
    base: &NormalizedElement,
) -> bool {
    // Clause 3: derived nillable only if base nillable.
    if derived.nillable && !base.nillable {
        return false;
    }
    // Clause 4: fixed value.
    match (&base.fixed_value, &derived.fixed_value) {
        (None, _) => {}
        (Some(_), None) => return false,
        (Some(base_fixed), Some(derived_fixed)) => {
            if !crate::validation::simple::fixed_values_equal(
                derived_fixed,
                base_fixed,
                Some(derived.type_key),
                schema_set,
            ) {
                return false;
            }
        }
    }
    // Clause 6: block superset (masked to element bits).
    if !derived
        .block
        .element_block_mask()
        .contains(base.block.element_block_mask())
    {
        return false;
    }
    // Clause 7: type derivation.
    schema_set.is_type_derived_from(derived.type_key, base.type_key, DerivationSet::extension())
}

/// Check whether the derived element is a substitution group member of the
/// base element. Looks up the global element by name when the derived
/// element_key is None (local declaration), per W3C bug 5296 — a local
/// element can match a substitution group via its global namesake.
fn derived_element_substitutes_base(
    schema_set: &SchemaSet,
    derived: &NormalizedElement,
    base: &NormalizedElement,
) -> bool {
    let d_key = derived
        .element_key
        .or_else(|| schema_set.lookup_element(derived.namespace, derived.name));
    let b_key = base
        .element_key
        .or_else(|| schema_set.lookup_element(base.namespace, base.name));
    match (d_key, b_key) {
        (Some(d), Some(b)) => {
            crate::compiler::substitution::is_element_substitutable_for(schema_set, b, d)
        }
        _ => false,
    }
}

/// True when two or more element particles in the slice share the same
/// expanded name — a cheap check to avoid the allocation in
/// `merge_duplicate_elements` for the overwhelming-majority case where
/// every element particle is unique.
fn has_duplicate_element_names(particles: &[NormalizedParticle]) -> bool {
    for (i, a) in particles.iter().enumerate() {
        let NormalizedParticleTerm::Element(a_elem) = &a.term else {
            continue;
        };
        for b in &particles[i + 1..] {
            if let NormalizedParticleTerm::Element(b_elem) = &b.term {
                if a_elem.name == b_elem.name && a_elem.namespace == b_elem.namespace {
                    return true;
                }
            }
        }
    }
    false
}

fn all_particles_restrict(
    schema_set: &SchemaSet,
    derived_particles: &[NormalizedParticle],
    base_particles: &[NormalizedParticle],
) -> bool {
    // XSD 1.0: All:All uses order-preserving Recurse (same as Sequence:Sequence).
    // XSD 1.1: RecurseUnordered allows reordering via backtracking.
    if schema_set.is_xsd10() {
        return sequence_particles_restrict(schema_set, derived_particles, base_particles);
    }
    recurse_unordered(schema_set, derived_particles, base_particles)
}

fn particle_is_emptiable(particle: &NormalizedParticle) -> bool {
    if particle.min_occurs == 0 {
        return true;
    }

    match &particle.term {
        NormalizedParticleTerm::Element(_) | NormalizedParticleTerm::Wildcard(_) => false,
        NormalizedParticleTerm::Group(group) => match group.compositor {
            Compositor::Sequence | Compositor::All => {
                group.particles.iter().all(particle_is_emptiable)
            }
            Compositor::Choice => group.particles.iter().any(particle_is_emptiable),
        },
    }
}

fn wildcard_restricts(derived: &NormalizedWildcard, base: &NormalizedWildcard) -> bool {
    is_wildcard_ns_subset(
        &derived.wildcard,
        derived.target_namespace,
        &base.wildcard,
        base.target_namespace,
    ) && process_contents_strictness(derived.wildcard.process_contents)
        >= process_contents_strictness(base.wildcard.process_contents)
}

/// Check whether the derived wildcard's admissible (namespace, name) set is
/// covered by the union of the given base wildcards. Returns the indices of
/// the bases that participate in the partition (those that admit at least
/// one (ns, name) admitted by derived); returns `None` if the union doesn't
/// cover derived.
///
/// Implements the partition extension of NSSubset (§3.10.6.2): a single
/// derived wildcard restricts a base content model if every (ns, name)
/// admitted by derived is admitted by some base wildcard. Used for cases
/// like wild049 where the base type's xs:all has multiple wildcards
/// partitioning the namespace space, and the derived sequence has a single
/// wildcard whose admissions span both base wildcards.
fn wildcard_subset_of_union(
    derived: &NormalizedWildcard,
    bases: &[&NormalizedWildcard],
) -> Option<Vec<usize>> {
    use crate::parser::frames::NotQNameItem;

    let derived_target = derived.target_namespace;

    // Collect all explicit namespace witnesses from both sides.
    let mut explicit_namespaces: Vec<Option<NameId>> = Vec::new();
    let push_ns = |ns: Option<NameId>, out: &mut Vec<Option<NameId>>| {
        if !out.contains(&ns) {
            out.push(ns);
        }
    };

    let collect_namespaces = |wc: &NormalizedWildcard, out: &mut Vec<Option<NameId>>| {
        let target = wc.target_namespace;
        match &wc.wildcard.namespace {
            WildcardNamespace::TargetNamespace => {
                if !out.contains(&target) {
                    out.push(target);
                }
            }
            WildcardNamespace::Local => {
                if !out.contains(&None) {
                    out.push(None);
                }
            }
            WildcardNamespace::List(tokens) => {
                for t in tokens {
                    let ns = t.resolve(target);
                    if !out.contains(&ns) {
                        out.push(ns);
                    }
                }
            }
            _ => {}
        }
        for t in &wc.wildcard.not_namespace {
            let ns = t.resolve(target);
            if !out.contains(&ns) {
                out.push(ns);
            }
        }
        for item in &wc.wildcard.not_qname {
            if let NotQNameItem::QName { namespace, .. } = item {
                if !out.contains(namespace) {
                    out.push(*namespace);
                }
            }
        }
    };

    collect_namespaces(derived, &mut explicit_namespaces);
    for base in bases {
        collect_namespaces(base, &mut explicit_namespaces);
    }
    push_ns(derived_target, &mut explicit_namespaces);
    for base in bases {
        push_ns(base.target_namespace, &mut explicit_namespaces);
    }
    push_ns(None, &mut explicit_namespaces);

    let mut spanned: Vec<usize> = Vec::new();

    let check_namespace = |ns: Option<NameId>,
                           is_explicit_ns: bool,
                           bases: &[&NormalizedWildcard],
                           spanned: &mut Vec<usize>|
     -> bool {
        if !wildcard_admits_ns(derived, ns) {
            return true;
        }
        // Find admitting bases (their indices in `bases`).
        let admitting: Vec<usize> = (0..bases.len())
            .filter(|&i| wildcard_admits_ns(bases[i], ns))
            .collect();
        if admitting.is_empty() {
            return false;
        }
        // For each name witness in this namespace, check coverage.
        let mut name_witnesses: Vec<NameId> = Vec::new();
        let push_name = |n: NameId, out: &mut Vec<NameId>| {
            if !out.contains(&n) {
                out.push(n);
            }
        };
        for item in &derived.wildcard.not_qname {
            if let NotQNameItem::QName {
                namespace,
                local_name,
            } = item
            {
                if *namespace == ns {
                    push_name(*local_name, &mut name_witnesses);
                }
            }
        }
        for &i in &admitting {
            for item in &bases[i].wildcard.not_qname {
                if let NotQNameItem::QName {
                    namespace,
                    local_name,
                } = item
                {
                    if *namespace == ns {
                        push_name(*local_name, &mut name_witnesses);
                    }
                }
            }
        }
        for name in &name_witnesses {
            if !wildcard_admits_qname(derived, ns, *name) {
                continue;
            }
            let any_admits = admitting
                .iter()
                .any(|&i| wildcard_admits_qname(bases[i], ns, *name));
            if !any_admits {
                return false;
            }
        }
        // "Any other name" witness: derived admits some name not in the
        // explicit witness set if and only if no `##defined`/`##definedSibling`
        // entry catches it. If derived admits this symbolic case, at least
        // one base must too.
        let derived_admits_other = wildcard_admits_qname_symbolic_other(derived, ns);
        if derived_admits_other {
            let any_admits_other = admitting
                .iter()
                .any(|&i| wildcard_admits_qname_symbolic_other(bases[i], ns));
            if !any_admits_other {
                return false;
            }
        }
        // Record participating bases. For explicit-namespace witnesses, we
        // know derived admits at least one name in this namespace, so each
        // admitting base participates; for the symbolic "any-other-namespace"
        // sentinel we'd over-count, so the caller marks those separately.
        if is_explicit_ns {
            for &i in &admitting {
                if !spanned.contains(&i) {
                    spanned.push(i);
                }
            }
        }
        true
    };

    // Check each explicit namespace witness.
    for ns in &explicit_namespaces {
        if !check_namespace(*ns, true, bases, &mut spanned) {
            return None;
        }
    }

    // Symbolic "fresh namespace" witness: a namespace not in any explicit
    // set. Use a sentinel NameId guaranteed not to appear (NameId::MAX).
    let fresh_ns = Some(NameId(u32::MAX));
    let derived_admits_fresh =
        wildcard_admits_ns(derived, fresh_ns) || wildcard_admits_ns(derived, None);
    if derived_admits_fresh {
        // The "fresh ns" sentinel covers any unmentioned namespace; we need
        // at least one base to admit it for partition coverage to work.
        let mut fresh_bases: Vec<usize> = (0..bases.len())
            .filter(|&i| wildcard_admits_ns(bases[i], fresh_ns))
            .collect();
        if wildcard_admits_ns(derived, fresh_ns) && fresh_bases.is_empty() {
            return None;
        }
        for &i in &fresh_bases {
            if !spanned.contains(&i) {
                spanned.push(i);
            }
        }
        fresh_bases.clear();
    }

    if spanned.is_empty() {
        // Derived admits nothing — degenerate, treat as covered by no bases.
        return Some(spanned);
    }
    Some(spanned)
}

/// Whether the wildcard admits `ns` in its `{namespace constraint}` after
/// applying `notNamespace`.
fn wildcard_admits_ns(wc: &NormalizedWildcard, ns: Option<NameId>) -> bool {
    if !wildcard_namespace_matches(&wc.wildcard.namespace, ns, wc.target_namespace) {
        return false;
    }
    !wc.wildcard
        .not_namespace
        .iter()
        .any(|t| t.resolve(wc.target_namespace) == ns)
}

/// Whether the wildcard admits the QName `(ns, name)` after applying
/// `notNamespace` and `notQName`. `##defined` and `##definedSibling` are
/// treated pessimistically — if either appears, the QName is rejected, since
/// at schema-time we cannot resolve which names they catch.
fn wildcard_admits_qname(wc: &NormalizedWildcard, ns: Option<NameId>, name: NameId) -> bool {
    use crate::parser::frames::NotQNameItem;
    if !wildcard_admits_ns(wc, ns) {
        return false;
    }
    !wc.wildcard.not_qname.iter().any(|item| match item {
        NotQNameItem::QName {
            namespace,
            local_name,
        } => *namespace == ns && *local_name == name,
        NotQNameItem::Defined | NotQNameItem::DefinedSibling => true,
    })
}

/// Whether the wildcard admits a "symbolic other name" in `ns` — i.e., some
/// name that doesn't appear in any explicit `notQName` entry. The check
/// fails only if `##defined`/`##definedSibling` would catch any name, since
/// concrete QName entries match only specific names.
fn wildcard_admits_qname_symbolic_other(wc: &NormalizedWildcard, ns: Option<NameId>) -> bool {
    use crate::parser::frames::NotQNameItem;
    if !wildcard_admits_ns(wc, ns) {
        return false;
    }
    !wc.wildcard
        .not_qname
        .iter()
        .any(|item| matches!(item, NotQNameItem::Defined | NotQNameItem::DefinedSibling))
}

fn wildcard_allows_element(element: &NormalizedElement, wildcard: &NormalizedWildcard) -> bool {
    if !wildcard_namespace_matches(
        &wildcard.wildcard.namespace,
        element.namespace,
        wildcard.target_namespace,
    ) {
        return false;
    }

    let excluded_namespace = wildcard
        .wildcard
        .not_namespace
        .iter()
        .map(|token| token.resolve(wildcard.target_namespace))
        .any(|namespace| namespace == element.namespace);
    if excluded_namespace {
        return false;
    }

    !wildcard_not_qname_excludes(
        &wildcard.wildcard.not_qname,
        element.namespace,
        element.name,
    )
}

fn wildcard_namespace_matches(
    namespace: &WildcardNamespace,
    element_namespace: Option<NameId>,
    target_namespace: Option<NameId>,
) -> bool {
    match namespace {
        WildcardNamespace::Any => true,
        WildcardNamespace::Other => {
            !other_exclusion_set(target_namespace).contains(&element_namespace)
        }
        WildcardNamespace::TargetNamespace => element_namespace == target_namespace,
        WildcardNamespace::Local => element_namespace.is_none(),
        WildcardNamespace::List(tokens) => tokens
            .iter()
            .map(|token| token.resolve(target_namespace))
            .any(|resolved| resolved == element_namespace),
    }
}

fn wildcard_not_qname_excludes(
    not_qname: &[crate::parser::frames::NotQNameItem],
    namespace: Option<NameId>,
    local_name: NameId,
) -> bool {
    not_qname.iter().any(|item| match item {
        crate::parser::frames::NotQNameItem::QName {
            namespace: excluded_ns,
            local_name: excluded_name,
        } => *excluded_ns == namespace && *excluded_name == local_name,
        crate::parser::frames::NotQNameItem::Defined => true,
        crate::parser::frames::NotQNameItem::DefinedSibling => false,
    })
}

// ---------------------------------------------------------------------------
// XSD 1.1: Open-content derivation helpers
// ---------------------------------------------------------------------------

/// Return the effective open content, treating `mode=None` as absent.
///
/// `compile.rs::open_content_from_result` collapses `mode=None` to `None`,
/// so derivation validation must agree: a raw `OpenContentResult` with
/// `mode=None` is semantically equivalent to no open content.
#[cfg(feature = "xsd11")]
fn effective_open_content(oc: Option<&OpenContentResult>) -> Option<&OpenContentResult> {
    oc.filter(|o| o.mode != OpenContentMode::None)
}

/// Map processContents to a strictness level (Strict=2, Lax=1, Skip=0).
fn process_contents_strictness(pc: ProcessContents) -> u8 {
    match pc {
        ProcessContents::Strict => 2,
        ProcessContents::Lax => 1,
        ProcessContents::Skip => 0,
    }
}

/// Compute the exclusion set for `##other` per the spec (§3.10.1):
/// `namespace="##other"` maps to `not({target namespace}, absent)`.
/// The result always contains `None` (absent) and, if the target namespace
/// is present, also contains `Some(target_ns)`.
fn other_exclusion_set(target_ns: Option<NameId>) -> Vec<Option<NameId>> {
    match target_ns {
        Some(ns) => vec![Some(ns), None],
        None => vec![None],
    }
}

/// Resolve a `WildcardNamespace` to a set of effective `Option<NameId>` values
/// that the wildcard **allows** (positive set).
///
/// Returns `None` for unbounded / complement constraints (`Any`, `Other`)
/// that cannot be represented as a finite positive set — callers must handle
/// those structurally.
fn resolve_ns_set(
    wns: &WildcardNamespace,
    target_ns: Option<NameId>,
) -> Option<Vec<Option<NameId>>> {
    match wns {
        WildcardNamespace::Any | WildcardNamespace::Other => None,
        WildcardNamespace::TargetNamespace => Some(vec![target_ns]),
        WildcardNamespace::Local => Some(vec![None]),
        WildcardNamespace::List(tokens) => {
            Some(tokens.iter().map(|t| t.resolve(target_ns)).collect())
        }
    }
}

/// Check whether `derived` namespace constraint is a subset of `base`
/// (cos-ns-subset, §3.10.6.2).
///
/// Both constraints are resolved against their respective target namespaces
/// so that `##targetNamespace` and an explicit URI equal to the target
/// namespace are treated as equivalent.
///
/// Key spec detail: `##other` maps to `not({target namespace}, absent)`,
/// i.e. it **always** excludes both the target namespace and the absent
/// namespace (§3.10.1).
///
/// processContents is checked separately by the open-content derivation
/// validators.
fn is_namespace_subset(
    derived: &WildcardNamespace,
    derived_target_ns: Option<NameId>,
    base: &WildcardNamespace,
    base_target_ns: Option<NameId>,
) -> bool {
    match base {
        WildcardNamespace::Any => true,

        WildcardNamespace::Other => {
            // base = not({base_target_ns, absent}).
            // Derived ⊆ base iff every namespace derived allows is also
            // allowed by base, i.e. is not in base's exclusion set.
            let base_excluded = other_exclusion_set(base_target_ns);

            match derived {
                WildcardNamespace::Any => false,

                WildcardNamespace::Other => {
                    // derived = not({derived_target_ns, absent}).
                    // Derived ⊆ base iff base_excluded ⊆ derived_excluded,
                    // i.e. derived excludes at least everything base excludes.
                    let derived_excluded = other_exclusion_set(derived_target_ns);
                    base_excluded.iter().all(|ns| derived_excluded.contains(ns))
                }

                _ => {
                    // Finite positive set — every allowed ns must not be in
                    // base's exclusion set.
                    match resolve_ns_set(derived, derived_target_ns) {
                        Some(resolved) => resolved.iter().all(|ns| !base_excluded.contains(ns)),
                        None => false,
                    }
                }
            }
        }

        WildcardNamespace::TargetNamespace
        | WildcardNamespace::Local
        | WildcardNamespace::List(_) => {
            // Base is a finite positive set — resolve both sides and check
            // set inclusion.
            let Some(base_set) = resolve_ns_set(base, base_target_ns) else {
                return false;
            };
            match derived {
                WildcardNamespace::Any | WildcardNamespace::Other => false,
                _ => {
                    let Some(derived_set) = resolve_ns_set(derived, derived_target_ns) else {
                        return false;
                    };
                    derived_set.iter().all(|ns| base_set.contains(ns))
                }
            }
        }
    }
}

/// Check whether `derived` wildcard's namespace constraint is a subset of
/// `base` wildcard's, also considering notNamespace and notQName exclusions.
///
/// Implements cos-ns-subset (§3.10.6.2) — a pure namespace-constraint
/// relation.  processContents is NOT checked here; callers handle it
/// separately for extension vs restriction semantics.
///
/// `derived_target_ns` / `base_target_ns` are the effective target namespaces
/// of the schema documents that contain the derived / base types.
fn is_wildcard_ns_subset(
    derived: &WildcardResult,
    derived_target_ns: Option<NameId>,
    base: &WildcardResult,
    base_target_ns: Option<NameId>,
) -> bool {
    // Namespace constraint must be a subset
    if !is_namespace_subset(
        &derived.namespace,
        derived_target_ns,
        &base.namespace,
        base_target_ns,
    ) {
        return false;
    }

    // notNamespace: for every namespace that base excludes, derived must
    // not allow it.  The naive "derived.not_namespace ⊇ base.not_namespace"
    // check over-rejects when derived's positive `{namespace constraint}`
    // already excludes the namespace by construction (e.g. derived is a
    // finite List whose members don't overlap base's notNamespace set).
    for base_excl in &base.not_namespace {
        let base_ns = base_excl.resolve(base_target_ns);
        let derived_allows =
            wildcard_namespace_matches(&derived.namespace, base_ns, derived_target_ns)
                && !derived
                    .not_namespace
                    .iter()
                    .any(|d| d.resolve(derived_target_ns) == base_ns);
        if derived_allows {
            return false;
        }
    }

    // notQName: derived must exclude at least everything base excludes that
    // derived's namespace constraint actually admits. If derived's
    // {namespace constraint} ∪ notNamespace already excludes the QName's
    // namespace, base's exclusion is moot for the subset check.
    for item in &base.not_qname {
        match item {
            crate::parser::frames::NotQNameItem::QName { namespace, .. } => {
                let derived_admits_ns =
                    wildcard_namespace_matches(&derived.namespace, *namespace, derived_target_ns)
                        && !derived
                            .not_namespace
                            .iter()
                            .any(|t| t.resolve(derived_target_ns) == *namespace);
                if derived_admits_ns && !derived.not_qname.contains(item) {
                    return false;
                }
            }
            crate::parser::frames::NotQNameItem::Defined
            | crate::parser::frames::NotQNameItem::DefinedSibling => {
                if !derived.not_qname.contains(item) {
                    return false;
                }
            }
        }
    }

    true
}

/// Validate open-content compatibility for complex type extension (cos-ct-extends).
///
/// Implements §3.4.6.2 clauses 1.4.3.2.2 by comparing the **effective**
/// `{open content}` property of each type (BOT, EOT) per §3.4.2.3 clauses
/// 4–6, rather than the raw `<xs:openContent>` child elements.
///
/// EOT inherits from the base when the derivation omits `<openContent>` or
/// specifies `mode="none"` (clause 6.1); otherwise EOT's wildcard is the
/// union (§3.10.6.3 cos-aw-union) of the derivation's wildcard with the
/// base's (clause 6.2). This lets schemas like saxonData/Open/open027 (base
/// has suffix OC, derived declares none) and open047 (derivation widens the
/// wildcard via notNamespace) pass validation.
#[cfg(feature = "xsd11")]
fn validate_open_content_extension(
    schema_set: &SchemaSet,
    derived_key: ComplexTypeKey,
    derived: &crate::arenas::ComplexTypeDefData,
    base_key: ComplexTypeKey,
    base: &crate::arenas::ComplexTypeDefData,
) -> SchemaResult<()> {
    let bot = compute_effective_open_content(schema_set, base_key);
    let eot = compute_effective_open_content(schema_set, derived_key);

    // Clause 1.4.3.2.2.3.1: if BOT is absent, extension is unconstrained wrt OC.
    let Some(bot) = bot else {
        return Ok(());
    };

    let (location, type_name) = type_error_context(schema_set, derived);
    let base_name = format_type_name(schema_set, base.name, base.target_namespace);

    // If BOT is present then EOT must be too (by construction of EOT: if
    // derivation has no OC and no mode=none override, clause 6.1 inherits BOT).
    // Reaching `None` here means the derivation's own `<openContent mode="none"/>`
    // plus an empty explicit content type collapsed EOT to absent in a way the
    // base does not satisfy — or, without that override, that the base chain
    // produced an OC but the derived chain didn't (mismatch).
    let Some(eot) = eot else {
        return Err(SchemaError::structural(
            "cos-ct-extends",
            format!(
                "Complex type '{}' extends '{}' which has open content, \
                 but derived type has no open content",
                type_name, base_name
            ),
            location,
        ));
    };

    // Clause 1.4.3.2.2.3: either EOT.mode = interleave, or both modes = suffix.
    let mode_ok = eot.mode == OpenContentMode::Interleave
        || (bot.mode == OpenContentMode::Suffix && eot.mode == OpenContentMode::Suffix);
    if !mode_ok {
        return Err(SchemaError::structural(
            "cos-ct-extends",
            format!(
                "Complex type '{}' uses suffix open content mode but base type '{}' \
                 uses interleave mode — suffix cannot extend interleave",
                type_name, base_name
            ),
            location,
        ));
    }

    // Clause 1.4.3.2.2.4: BOT.{wildcard}.{namespace constraint} ⊆ EOT.{wildcard}.
    if let (Some(bot_wc), Some(eot_wc)) = (bot.wildcard.as_ref(), eot.wildcard.as_ref()) {
        if !is_wildcard_ns_subset(
            bot_wc,
            base.target_namespace,
            eot_wc,
            derived.target_namespace,
        ) {
            return Err(SchemaError::structural(
                "cos-ct-extends",
                format!(
                    "Open content wildcard of '{}' is not a valid extension \
                     of base type '{}' wildcard",
                    type_name, base_name
                ),
                location,
            ));
        }
    }

    Ok(())
}

/// Effective `{open content}` property per §3.4.2.3 clauses 5–6.
///
/// Represents a non-absent open content: an absent OC is encoded as `None`
/// (returned by `compute_effective_open_content`). `target_namespace` is the
/// context for resolving any unresolved `##targetNamespace` tokens inside
/// `wildcard` — needed because a type's own `<openContent>` child is stored
/// with tokens in parser form.
#[cfg(feature = "xsd11")]
#[derive(Debug, Clone)]
struct EffectiveOpenContent {
    mode: OpenContentMode,
    wildcard: Option<WildcardResult>,
    target_namespace: Option<NameId>,
}

/// Compute the effective `{open content}` of a complex type per §3.4.2.3
/// clauses 5 and 6. Walks the base chain through extension derivations.
///
/// Returns `None` when the type's effective OC is absent.
#[cfg(feature = "xsd11")]
fn compute_effective_open_content(
    schema_set: &SchemaSet,
    key: ComplexTypeKey,
) -> Option<EffectiveOpenContent> {
    compute_effective_open_content_bounded(schema_set, key, 0)
}

#[cfg(feature = "xsd11")]
fn compute_effective_open_content_bounded(
    schema_set: &SchemaSet,
    key: ComplexTypeKey,
    depth: u32,
) -> Option<EffectiveOpenContent> {
    // Guard against pathological cycles — reference resolution should have
    // detected them upstream, but keep a local belt-and-braces cap.
    if depth > 100 {
        return None;
    }
    let type_data = schema_set.arenas.complex_types.get(key)?;
    let target_ns = type_data.target_namespace;

    // Clause 5: select the "wildcard element" (the OC source for this type).
    // Clause 5.1 picks the <xs:openContent> child element regardless of its
    // @mode — a literal `mode="none"` still "corresponds" to the element per
    // the spec.  The clause-6.1 mode=none branch below handles that case,
    // short-circuiting the defaultOpenContent fallback that 5.2 would apply.
    let own_oc: Option<EffectiveOpenContent> =
        type_data
            .open_content
            .as_ref()
            .map(|oc| EffectiveOpenContent {
                mode: oc.mode,
                wildcard: oc.wildcard.clone(),
                target_namespace: target_ns,
            });

    let wildcard_element: Option<EffectiveOpenContent> = if own_oc.is_some() {
        // Clause 5.1
        own_oc
    } else if let Some(default) = type_data
        .source
        .as_ref()
        .and_then(|s| schema_set.documents.get(s.defaults_doc() as usize))
        .and_then(|d| d.default_open_content.as_ref())
    {
        // Clause 5.2: schema-level <xs:defaultOpenContent> applies when
        // appliesToEmpty=true OR the explicit content type is non-empty.
        if default.applies_to_empty || !explicit_content_is_empty(schema_set, type_data, 0) {
            default_open_content_to_effective(default, target_ns)
        } else {
            None
        }
    } else {
        None // Clause 5.3
    };

    // Base's effective OC (clause 4.2 inheritance). Only inherited across
    // extension; for restriction or anyType-derivation the base OC does not
    // flow into the derived explicit content type.
    let base_oc: Option<EffectiveOpenContent> = if matches!(
        type_data.derivation_method,
        Some(DerivationMethod::Extension)
    ) {
        match type_data.resolved_base_type {
            Some(TypeKey::Complex(base_key)) => {
                compute_effective_open_content_bounded(schema_set, base_key, depth + 1)
            }
            _ => None,
        }
    } else {
        None
    };

    // Clause 6.1: absent / mode=None wildcard element → inherit base.
    let Some(we) = wildcard_element else {
        return base_oc;
    };
    if we.mode == OpenContentMode::None {
        return base_oc;
    }

    // Clause 6.2: build a new OC record with the unioned wildcard.
    let wildcard = match (base_oc.as_ref(), we.wildcard.as_ref()) {
        (Some(b), Some(w)) => match &b.wildcard {
            Some(bw) => Some(wildcard_result_union(bw, b.target_namespace, w, target_ns)),
            None => Some(w.clone()),
        },
        (None, Some(w)) => Some(w.clone()),
        (Some(b), None) => b.wildcard.clone(),
        (None, None) => None,
    };

    Some(EffectiveOpenContent {
        mode: we.mode,
        wildcard,
        target_namespace: target_ns,
    })
}

/// Determine whether a complex type's *explicit content type* is empty per
/// §3.4.2.3 clause 3 (needed for clause 5.2.2's `appliesToEmpty` gate).
///
/// For extension with no derivation-level particle, the explicit content is
/// the base's — so recurse. For restriction or non-derivation types the
/// explicit content is the derivation's own content.
#[cfg(feature = "xsd11")]
fn explicit_content_is_empty(
    schema_set: &SchemaSet,
    type_data: &crate::arenas::ComplexTypeDefData,
    depth: u32,
) -> bool {
    if depth > 100 {
        return true;
    }
    // Use the §3.4.2.3 5.2.2 gate (explicit content type variety = empty),
    // which incorporates the effective-mixed promotion of step 3.1.1.
    if !type_data.content.explicit_content_type_is_empty() {
        return false;
    }
    if matches!(
        type_data.derivation_method,
        Some(DerivationMethod::Extension)
    ) {
        if let Some(TypeKey::Complex(base_key)) = type_data.resolved_base_type {
            if let Some(base_data) = schema_set.arenas.complex_types.get(base_key) {
                return explicit_content_is_empty(schema_set, base_data, depth + 1);
            }
        }
    }
    true
}

/// Convert a `DefaultOpenContent` (schema-model form built from the
/// `<xs:defaultOpenContent>` element) into the `EffectiveOpenContent` form
/// used during derivation validation.
#[cfg(feature = "xsd11")]
fn default_open_content_to_effective(
    default: &crate::schema::model::DefaultOpenContent,
    target_ns: Option<NameId>,
) -> Option<EffectiveOpenContent> {
    let mode = match default.mode {
        crate::schema::model::OpenContentMode::None => OpenContentMode::None,
        crate::schema::model::OpenContentMode::Interleave => OpenContentMode::Interleave,
        crate::schema::model::OpenContentMode::Suffix => OpenContentMode::Suffix,
    };
    if mode == OpenContentMode::None {
        return None;
    }
    let wildcard = default.wildcard.as_ref().map(element_wildcard_to_result);
    Some(EffectiveOpenContent {
        mode,
        wildcard,
        target_namespace: target_ns,
    })
}

/// Convert a schema-model `ElementWildcard` to a parser-form `WildcardResult`
/// so it can share the subset / union helpers below.
#[cfg(feature = "xsd11")]
fn element_wildcard_to_result(ew: &crate::schema::wildcard::ElementWildcard) -> WildcardResult {
    use crate::parser::frames::NotQNameItem;
    use crate::schema::wildcard::{NamespaceConstraint, QNameDisallowed};

    let (namespace, not_namespace) = match &ew.namespace_constraint {
        NamespaceConstraint::Any => (WildcardNamespace::Any, Vec::new()),
        NamespaceConstraint::Other => (WildcardNamespace::Other, Vec::new()),
        NamespaceConstraint::Enumeration(nss) => (
            WildcardNamespace::List(nss.iter().copied().map(ns_token).collect()),
            Vec::new(),
        ),
        NamespaceConstraint::Not(nss) => (
            WildcardNamespace::Any,
            nss.iter().copied().map(ns_token).collect(),
        ),
    };

    let process_contents = match ew.process_contents {
        crate::schema::wildcard::ProcessContents::Strict => ProcessContents::Strict,
        crate::schema::wildcard::ProcessContents::Lax => ProcessContents::Lax,
        crate::schema::wildcard::ProcessContents::Skip => ProcessContents::Skip,
    };

    let not_qname = ew
        .not_qnames
        .iter()
        .map(|q| match q {
            QNameDisallowed::QName {
                namespace,
                local_name,
            } => NotQNameItem::QName {
                namespace: *namespace,
                local_name: *local_name,
            },
            QNameDisallowed::Defined => NotQNameItem::Defined,
            QNameDisallowed::DefinedSibling => NotQNameItem::DefinedSibling,
        })
        .collect();

    WildcardResult {
        namespace,
        process_contents,
        not_namespace,
        not_qname,
        id: ew.id.clone(),
        annotation: None,
        source: ew.source.clone(),
    }
}

/// Canonical namespace form: finite allowed set, or finite excluded set
/// (complement in the "namespace universe").
#[cfg(feature = "xsd11")]
#[derive(Debug, Clone)]
enum NsForm {
    Pos(Vec<Option<NameId>>),
    Neg(Vec<Option<NameId>>),
}

/// Normalise a wildcard's `{namespace constraint}` into canonical form,
/// resolving `##targetNamespace`/`##local` tokens and merging `notNamespace`
/// into the excluded set.
#[cfg(feature = "xsd11")]
fn wildcard_to_ns_form(
    ns: &WildcardNamespace,
    not_namespace: &[crate::parser::frames::NamespaceToken],
    target_ns: Option<NameId>,
) -> NsForm {
    let resolved_not: Vec<Option<NameId>> =
        not_namespace.iter().map(|t| t.resolve(target_ns)).collect();
    match ns {
        WildcardNamespace::Any => NsForm::Neg(resolved_not),
        WildcardNamespace::Other => {
            let mut excl = other_exclusion_set(target_ns);
            for r in resolved_not {
                if !excl.contains(&r) {
                    excl.push(r);
                }
            }
            NsForm::Neg(excl)
        }
        WildcardNamespace::TargetNamespace => {
            let base = target_ns;
            if resolved_not.contains(&base) {
                NsForm::Pos(Vec::new())
            } else {
                NsForm::Pos(vec![base])
            }
        }
        WildcardNamespace::Local => {
            if resolved_not.contains(&None) {
                NsForm::Pos(Vec::new())
            } else {
                NsForm::Pos(vec![None])
            }
        }
        WildcardNamespace::List(tokens) => {
            let allowed: Vec<Option<NameId>> = tokens
                .iter()
                .map(|t| t.resolve(target_ns))
                .filter(|r| !resolved_not.contains(r))
                .collect();
            NsForm::Pos(allowed)
        }
    }
}

/// Convert a canonical `NsForm` back into `(WildcardNamespace, not_namespace)`
/// pair suitable for a `WildcardResult`. Any excluded-set result that's empty
/// collapses to `##any`; a non-empty excluded set becomes `##any` with
/// `notNamespace` tokens.
#[cfg(feature = "xsd11")]
fn ns_form_to_wildcard(
    form: NsForm,
) -> (
    WildcardNamespace,
    Vec<crate::parser::frames::NamespaceToken>,
) {
    use crate::parser::frames::NamespaceToken;
    match form {
        NsForm::Pos(list) => {
            let tokens: Vec<NamespaceToken> = list.into_iter().map(ns_token).collect();
            (WildcardNamespace::List(tokens), Vec::new())
        }
        NsForm::Neg(list) if list.is_empty() => (WildcardNamespace::Any, Vec::new()),
        NsForm::Neg(list) => {
            let tokens: Vec<NamespaceToken> = list.into_iter().map(ns_token).collect();
            (WildcardNamespace::Any, tokens)
        }
    }
}

/// Convert a resolved namespace (`Some(id)` = URI, `None` = absent/local) into
/// a parser-form `NamespaceToken`. Used by the open-content derivation helpers
/// to reconstruct parser-form wildcards from canonicalised lists.
#[cfg(feature = "xsd11")]
fn ns_token(ns: Option<NameId>) -> crate::parser::frames::NamespaceToken {
    match ns {
        Some(id) => crate::parser::frames::NamespaceToken::Uri(id),
        None => crate::parser::frames::NamespaceToken::Local,
    }
}

/// Wildcard union per §3.10.6.3 cos-aw-union, restricted to the namespace
/// constraint portion. `notQName` items are intersected (an excluded QName
/// stays excluded only if both wildcards exclude it). `processContents` is
/// inherited from `a` (convention: `a` is the derivation's own `<any>`).
///
/// Tokens in the produced `WildcardResult` are already resolved against the
/// input target namespaces, so the caller does not need to supply one.
#[cfg(feature = "xsd11")]
pub(crate) fn wildcard_result_union(
    a: &WildcardResult,
    a_tns: Option<NameId>,
    b: &WildcardResult,
    b_tns: Option<NameId>,
) -> WildcardResult {
    let form_a = wildcard_to_ns_form(&a.namespace, &a.not_namespace, a_tns);
    let form_b = wildcard_to_ns_form(&b.namespace, &b.not_namespace, b_tns);

    let merged = match (form_a, form_b) {
        (NsForm::Pos(mut pa), NsForm::Pos(pb)) => {
            for item in pb {
                if !pa.contains(&item) {
                    pa.push(item);
                }
            }
            NsForm::Pos(pa)
        }
        (NsForm::Pos(pa), NsForm::Neg(nb)) | (NsForm::Neg(nb), NsForm::Pos(pa)) => {
            NsForm::Neg(nb.into_iter().filter(|ns| !pa.contains(ns)).collect())
        }
        (NsForm::Neg(na), NsForm::Neg(nb)) => {
            NsForm::Neg(na.into_iter().filter(|ns| nb.contains(ns)).collect())
        }
    };

    let (namespace, not_namespace) = ns_form_to_wildcard(merged);

    let not_qname: Vec<crate::parser::frames::NotQNameItem> = a
        .not_qname
        .iter()
        .filter(|item| b.not_qname.contains(item))
        .cloned()
        .collect();

    WildcardResult {
        namespace,
        process_contents: a.process_contents,
        not_namespace,
        not_qname,
        id: None,
        annotation: None,
        source: a.source.clone(),
    }
}

/// True when the derived complex type's explicit particle is absent or
/// normalizes to an empty group (`<xs:sequence/>`, `<xs:all/>`, or a group
/// whose children all prune away). Used to decide when the stricter
/// open-content restriction checks can be safely relaxed: if the derived
/// particle contributes no elements to the content language, the derived
/// type's language comes entirely from its open content wildcard, so the
/// mode-and-subset checks against the base reduce to a pure wildcard-
/// subset check (cos-ns-subset).
#[cfg(feature = "xsd11")]
fn derived_particle_is_empty(
    schema_set: &SchemaSet,
    derived: &crate::arenas::ComplexTypeDefData,
) -> bool {
    let Some(particle) = complex_content_particle(&derived.content) else {
        return true;
    };
    let Ok(normalized) = normalize_type_particle(schema_set, derived, particle) else {
        return false;
    };
    is_effectively_empty(&normalized)
}

/// Extract a wildcard from a base type's effective content particle when it
/// is a single wildcard (optionally wrapped in a pointless sequence/all
/// group). Returns `None` when the base has element particles that would
/// require the derived type's empty particle to not be a valid restriction.
///
/// This supports §3.4.6.4 clause 1 (language containment) for the narrow
/// case where the base has no `<xs:openContent>` but its content model is
/// a single wildcard — which is language-equivalent to having
/// interleave/suffix open content over that same wildcard.
#[cfg(feature = "xsd11")]
fn base_content_single_wildcard<'a>(
    schema_set: &'a SchemaSet,
    base: &'a crate::arenas::ComplexTypeDefData,
) -> Option<NormalizedWildcard> {
    let (particle_owner, particle) = effective_base_content_particle(schema_set, base);
    let particle = particle?;
    let normalized = normalize_type_particle(schema_set, particle_owner, particle).ok()?;
    match &normalized.term {
        NormalizedParticleTerm::Wildcard(wc) => Some((**wc).clone()),
        NormalizedParticleTerm::Group(group) => {
            if group.particles.len() == 1 {
                if let NormalizedParticleTerm::Wildcard(wc) = &group.particles[0].term {
                    return Some((**wc).clone());
                }
            }
            None
        }
        _ => None,
    }
}

/// XSD 1.1 §3.4.6.4 schema-time EDC for all-group restrictions
/// (cvc-complex-type rule 5 / cos-element-consistent extended). When a
/// derived all-group restricts a base all-group and removes a base local
/// element, the derived's wildcard can structurally admit elements with the
/// removed QName. The "tighter EDC rule" of XSD 1.1 (Saxon test category
/// `xsd1_1-Wildcards-TighterMatchingRuleForEDC`, e.g. wild069) demands the
/// schema be invalid if the wildcard's governing type for that QName is not
/// validly substitutable for the base local's declared type.
///
/// The xs:sequence variant of the same construct (wild068) is intentionally
/// not subject to this check: the position constraint of sequence keeps the
/// conflict from arising structurally, leaving runtime dynamic EDC as the
/// catcher.
#[cfg(feature = "xsd11")]
fn validate_all_group_restriction_edc(
    schema_set: &SchemaSet,
    derived: &crate::arenas::ComplexTypeDefData,
    base: &crate::arenas::ComplexTypeDefData,
) -> SchemaResult<()> {
    use crate::parser::frames::ProcessContents;

    let derived_particle = complex_content_particle(&derived.content);
    let (effective_base, base_particle) = effective_base_content_particle(schema_set, base);

    let (Some(derived_p), Some(base_p)) = (derived_particle, base_particle) else {
        return Ok(());
    };

    let derived_norm = match normalize_type_particle(schema_set, derived, derived_p) {
        Ok(n) => n,
        Err(_) => return Ok(()),
    };
    let base_norm = match normalize_type_particle(schema_set, effective_base, base_p) {
        Ok(n) => n,
        Err(_) => return Ok(()),
    };

    // Trigger only when both top-level groups are xs:all.
    if !is_top_all_group(&derived_norm) || !is_top_all_group(&base_norm) {
        return Ok(());
    }

    // Collect derived's local element QNames and wildcards (top-level only —
    // an all-group's particles are flat).
    let mut derived_local_qnames: Vec<(Option<NameId>, NameId)> = Vec::new();
    let mut derived_wildcards: Vec<&NormalizedWildcard> = Vec::new();
    if let NormalizedParticleTerm::Group(group) = &derived_norm.term {
        for p in &group.particles {
            match &p.term {
                NormalizedParticleTerm::Element(elem) => {
                    derived_local_qnames.push((elem.namespace, elem.name));
                }
                NormalizedParticleTerm::Wildcard(wc) => {
                    derived_wildcards.push(wc.as_ref());
                }
                NormalizedParticleTerm::Group(_) => {}
            }
        }
    }

    if derived_wildcards.is_empty() {
        return Ok(());
    }

    // Walk base's local elements (top-level all-group particles).
    let base_locals: Vec<(Option<NameId>, NameId, TypeKey)> =
        if let NormalizedParticleTerm::Group(group) = &base_norm.term {
            group
                .particles
                .iter()
                .filter_map(|p| match &p.term {
                    NormalizedParticleTerm::Element(elem) => {
                        Some((elem.namespace, elem.name, elem.type_key))
                    }
                    _ => None,
                })
                .collect()
        } else {
            Vec::new()
        };

    let (location, type_name) = type_error_context(schema_set, derived);
    let base_name = format_type_name(schema_set, base.name, base.target_namespace);

    for (l_ns, l_name, l_type) in &base_locals {
        // Skip if derived also has this local element.
        if derived_local_qnames.contains(&(*l_ns, *l_name)) {
            continue;
        }

        for wc in &derived_wildcards {
            if !wildcard_admits_qname(wc, *l_ns, *l_name) {
                continue;
            }

            let pc = wc.wildcard.process_contents;
            if matches!(pc, ProcessContents::Skip) {
                continue;
            }

            let global_key = schema_set.lookup_element(*l_ns, *l_name);
            let governing_type = global_key
                .and_then(|k| schema_set.arenas.elements.get(k))
                .and_then(|d| d.resolved_type);

            match (pc, governing_type) {
                (ProcessContents::Strict, None) => {
                    // strict + no global: instance with this QName fails strict
                    // wildcard validation, so derived rejects. No conflict.
                    continue;
                }
                (ProcessContents::Lax, None) => {
                    // lax + no global: wildcard skip-validates, so derived
                    // admits arbitrary content for this QName. Base local
                    // would enforce its type — derived broader → reject.
                }
                (_, Some(gov_type)) => {
                    if schema_set.is_type_derived_from(gov_type, *l_type, DerivationSet::empty()) {
                        continue;
                    }
                }
                _ => continue,
            }

            return Err(SchemaError::structural(
                "cos-element-consistent",
                format!(
                    "Complex type '{}' restricts '{}' (xs:all) by removing local element \
                     while keeping a wildcard that admits the same QName; the wildcard's \
                     governing type is not validly substitutable for the base local element's \
                     type (cvc-complex-type rule 5 / tighter EDC for xs:all restriction)",
                    type_name, base_name,
                ),
                location.clone(),
            ));
        }
    }

    Ok(())
}

/// Whether a normalized particle is a top-level xs:all group (with min/max
/// = 1, since xs:all allows minOccurs ∈ {0,1} and maxOccurs = 1).
#[cfg(feature = "xsd11")]
fn is_top_all_group(particle: &NormalizedParticle) -> bool {
    matches!(
        &particle.term,
        NormalizedParticleTerm::Group(group) if group.compositor == Compositor::All
    )
}

/// Validate open-content compatibility for complex type restriction
/// (derivation-ok-restriction).
///
/// Rules:
/// - If base has no OC, derived must not add OC — **unless** the derived
///   particle is empty and the base's content model is a single wildcard
///   that subsumes the derived OC's wildcard (language-equivalent case).
/// - If base has OC but derived doesn't — OK (restriction removes it).
/// - Interleave cannot restrict suffix — **unless** the derived particle
///   is empty, in which case the mode choice is irrelevant because the
///   derived language is the wildcard closure.
/// - Derived wildcard must be a subset of base wildcard.
#[cfg(feature = "xsd11")]
fn validate_open_content_restriction(
    schema_set: &SchemaSet,
    derived: &crate::arenas::ComplexTypeDefData,
    base: &crate::arenas::ComplexTypeDefData,
) -> SchemaResult<()> {
    let base_oc = effective_open_content(base.open_content.as_ref());
    let derived_oc = effective_open_content(derived.open_content.as_ref());

    // If base has no open content, derived must not add one — except when
    // the derived particle is empty and the base's single-wildcard particle
    // subsumes the derived OC wildcard (language containment, §3.4.6.4).
    if base_oc.is_none() && derived_oc.is_some() {
        if derived_particle_is_empty(schema_set, derived) {
            let derived_oc_wc = derived_oc.as_ref().and_then(|o| o.wildcard.as_ref());
            if let Some(d_wc) = derived_oc_wc {
                if let Some(base_wc) = base_content_single_wildcard(schema_set, base) {
                    if is_wildcard_ns_subset(
                        d_wc,
                        derived.target_namespace,
                        &base_wc.wildcard,
                        base_wc.target_namespace,
                    ) {
                        return Ok(());
                    }
                }
            }
        }
        let (location, type_name) = type_error_context(schema_set, derived);
        let base_name = format_type_name(schema_set, base.name, base.target_namespace);
        return Err(SchemaError::structural(
            "derivation-ok-restriction",
            format!(
                "Complex type '{}' restricts '{}' which has no open content, \
                 but adds open content — not allowed",
                type_name, base_name
            ),
            location,
        ));
    }

    // If base has OC but derived doesn't — OK (restriction removes it)
    let (Some(base_oc), Some(derived_oc)) = (base_oc, derived_oc) else {
        return Ok(());
    };

    let (location, type_name) = type_error_context(schema_set, derived);
    let base_name = format_type_name(schema_set, base.name, base.target_namespace);

    // Mode: if base is suffix, derived cannot use interleave — unless the
    // derived particle is empty, in which case the derived language is just
    // the wildcard closure and the mode choice is irrelevant.
    if base_oc.mode == OpenContentMode::Suffix
        && derived_oc.mode == OpenContentMode::Interleave
        && !derived_particle_is_empty(schema_set, derived)
    {
        return Err(SchemaError::structural(
            "derivation-ok-restriction",
            format!(
                "Complex type '{}' uses interleave open content mode but base type '{}' \
                 uses suffix mode — interleave cannot restrict suffix",
                type_name, base_name
            ),
            location,
        ));
    }

    // Wildcard: derived must be subset of base
    if let (Some(base_wc), Some(derived_wc)) =
        (base_oc.wildcard.as_ref(), derived_oc.wildcard.as_ref())
    {
        if !is_wildcard_ns_subset(
            derived_wc,
            derived.target_namespace,
            base_wc,
            base.target_namespace,
        ) {
            return Err(SchemaError::structural(
                "derivation-ok-restriction",
                format!(
                    "Open content wildcard of '{}' is not a valid restriction \
                     of base type '{}' wildcard",
                    type_name, base_name
                ),
                location,
            ));
        }

        // processContents: restriction must be at least as strict
        if process_contents_strictness(derived_wc.process_contents)
            < process_contents_strictness(base_wc.process_contents)
        {
            return Err(SchemaError::structural(
                "derivation-ok-restriction",
                format!(
                    "Open content wildcard of '{}' has weaker processContents \
                     than base type '{}' wildcard",
                    type_name, base_name
                ),
                location,
            ));
        }
    }

    Ok(())
}

/// Check if a type derives from NOTATION or QName.
fn is_notation_or_qname_base(schema_set: &SchemaSet, key: TypeKey) -> bool {
    let TypeKey::Simple(sk) = key else {
        return false;
    };
    let bt = schema_set.builtin_types();
    schema_set.derives_from(sk, bt.notation) || schema_set.derives_from(sk, bt.qname)
}

/// Walk up the simple type chain past any types that have enumeration facets.
///
/// Returns the first ancestor without enumeration facets.  This lets us
/// validate enumeration values against the "structural" base (bounds, digits,
/// lexical form) without hitting the string-equality enumeration comparison
/// in `validate_simple_type` (which can false-reject when canonical forms
/// differ — e.g. `12:00:00.990` vs `12:00:00.99`).  The enumeration-subset
/// rule is already enforced by `merge_with_base`.
fn base_without_enumeration(schema_set: &SchemaSet, key: TypeKey) -> TypeKey {
    let mut current = key;
    for _ in 0..100 {
        if let TypeKey::Simple(sk) = current {
            if let Some(st_data) = schema_set.arenas.simple_types.get(sk) {
                if st_data.facets.enumeration.is_none() {
                    return current;
                }
                if let Some(base) = st_data.resolved_base_type {
                    current = base;
                    continue;
                }
            }
        }
        break;
    }
    current
}

/// Validate that facet values are in the value space of the base type.
///
/// Reuses the existing `validate_simple_type` runtime infrastructure (type code
/// resolution, facet collection, validator dispatch) to check each locally
/// declared facet value against the base type at schema-compile time.
///
/// Implements XSD Part 2 constraints:
/// - `enumeration-valid-restriction`: enumeration values must be in the base type's value space
/// - `minInclusive-valid-restriction`, `maxInclusive-valid-restriction`,
///   `minExclusive-valid-restriction`, `maxExclusive-valid-restriction`:
///   bound values must be in the base type's value space
fn validate_facet_values_against_base_type(
    schema_set: &SchemaSet,
    type_def: &crate::arenas::SimpleTypeDefData,
    base_key: TypeKey,
) -> SchemaResult<()> {
    let (location, type_name) = type_error_context(schema_set, type_def);

    // QName/NOTATION need a runtime namespace context for full value-space
    // validation, so most lexical checks are deferred. We *can* still reject
    // the always-invalid empty literal in enumeration / bound facets — an
    // empty string is not a valid QName or NOTATION lexically (xsd:QName ≡
    // Prefix? ':'? LocalPart, where LocalPart is an NCName, never empty).
    if is_notation_or_qname_base(schema_set, base_key) {
        if let Some(ref enum_facet) = type_def.facets.enumeration {
            for value in &enum_facet.values {
                if value.trim().is_empty() {
                    return Err(SchemaError::structural(
                        "enumeration-valid-restriction",
                        format!(
                            "Enumeration value '' in type '{}' is not in the value space of the base type",
                            type_name
                        ),
                        location.clone(),
                    ));
                }
            }
        }
        return Ok(());
    }

    // Validate enumeration values.
    // Walk past any base with its own enumeration to avoid string-equality comparison
    // (canonical-form mismatch). merge_with_base already checks the subset rule.
    if let Some(ref enum_facet) = type_def.facets.enumeration {
        let enum_base = base_without_enumeration(schema_set, base_key);
        for value in &enum_facet.values {
            if crate::validation::simple::validate_simple_type(value, enum_base, schema_set)
                .is_err()
            {
                return Err(SchemaError::structural(
                    "enumeration-valid-restriction",
                    format!(
                        "Enumeration value '{}' in type '{}' is not in the value space of the base type",
                        value, type_name
                    ),
                    location.clone(),
                ));
            }
        }
    }

    // Validate bound facet values. XSD Part 2 §4.3.9 permits a derived bound
    // to equal the base's same-kind bound (boundary equality), even though
    // that base value is not in the base's own value space.
    let check_bound = |value: &str,
                       constraint: &'static str,
                       kind: FacetKind|
     -> SchemaResult<()> {
        match crate::validation::simple::validate_simple_type(value, base_key, schema_set) {
            Ok(_) => Ok(()),
            Err(err) if is_bound_self_violation(&err, kind, schema_set, base_key, value) => Ok(()),
            Err(_) => Err(SchemaError::structural(
                constraint,
                format!(
                    "{} value '{}' in type '{}' is not in the value space of the base type",
                    kind.name(),
                    value,
                    type_name
                ),
                location.clone(),
            )),
        }
    };

    if let Some(ref f) = type_def.facets.min_inclusive {
        check_bound(
            &f.value,
            "minInclusive-valid-restriction",
            FacetKind::MinInclusive,
        )?;
    }
    if let Some(ref f) = type_def.facets.max_inclusive {
        check_bound(
            &f.value,
            "maxInclusive-valid-restriction",
            FacetKind::MaxInclusive,
        )?;
    }
    if let Some(ref f) = type_def.facets.min_exclusive {
        check_bound(
            &f.value,
            "minExclusive-valid-restriction",
            FacetKind::MinExclusive,
        )?;
    }
    if let Some(ref f) = type_def.facets.max_exclusive {
        check_bound(
            &f.value,
            "maxExclusive-valid-restriction",
            FacetKind::MaxExclusive,
        )?;
    }

    validate_typed_bound_consistency(
        schema_set,
        &type_def.facets,
        base_key,
        &type_name,
        &location,
    )?;

    Ok(())
}

fn validate_typed_bound_consistency(
    schema_set: &SchemaSet,
    facets: &FacetSet,
    base_key: TypeKey,
    type_name: &str,
    location: &Option<SourceLocation>,
) -> SchemaResult<()> {
    let check_pair = |lower: Option<&str>,
                      upper: Option<&str>,
                      lower_name: &'static str,
                      upper_name: &'static str,
                      allow_equal: bool|
     -> SchemaResult<()> {
        let (Some(lower), Some(upper)) = (lower, upper) else {
            return Ok(());
        };
        let Some(cmp) = compare_bound_literals(schema_set, base_key, lower, upper) else {
            return Ok(());
        };
        let valid = if allow_equal {
            matches!(cmp, std::cmp::Ordering::Less | std::cmp::Ordering::Equal)
        } else {
            cmp == std::cmp::Ordering::Less
        };
        if valid {
            return Ok(());
        }
        Err(SchemaError::structural(
            "cos-st-restricts",
            format!(
                "{} value '{}' is not below {} value '{}' in type '{}'",
                lower_name, lower, upper_name, upper, type_name
            ),
            location.clone(),
        ))
    };

    check_pair(
        facets.min_inclusive.as_ref().map(|f| f.value.as_str()),
        facets.max_inclusive.as_ref().map(|f| f.value.as_str()),
        "minInclusive",
        "maxInclusive",
        true,
    )?;
    check_pair(
        facets.min_exclusive.as_ref().map(|f| f.value.as_str()),
        facets.max_exclusive.as_ref().map(|f| f.value.as_str()),
        "minExclusive",
        "maxExclusive",
        false,
    )?;
    check_pair(
        facets.min_inclusive.as_ref().map(|f| f.value.as_str()),
        facets.max_exclusive.as_ref().map(|f| f.value.as_str()),
        "minInclusive",
        "maxExclusive",
        false,
    )?;
    check_pair(
        facets.min_exclusive.as_ref().map(|f| f.value.as_str()),
        facets.max_inclusive.as_ref().map(|f| f.value.as_str()),
        "minExclusive",
        "maxInclusive",
        false,
    )
}

fn compare_bound_literals(
    schema_set: &SchemaSet,
    base_key: TypeKey,
    lower: &str,
    upper: &str,
) -> Option<std::cmp::Ordering> {
    let parse_base = bound_comparison_base(schema_set, base_key);
    let lower =
        crate::validation::simple::validate_simple_type(lower, parse_base, schema_set).ok()?;
    let upper =
        crate::validation::simple::validate_simple_type(upper, parse_base, schema_set).ok()?;
    compare_xml_values(&lower.typed_value, &upper.typed_value)
}

fn bound_comparison_base(schema_set: &SchemaSet, key: TypeKey) -> TypeKey {
    let mut current = key;
    for _ in 0..100 {
        let TypeKey::Simple(sk) = current else {
            return current;
        };
        let Some(st) = schema_set.arenas.simple_types.get(sk) else {
            return current;
        };
        let has_bounds_or_enum = st.facets.enumeration.is_some()
            || st.facets.min_inclusive.is_some()
            || st.facets.min_exclusive.is_some()
            || st.facets.max_inclusive.is_some()
            || st.facets.max_exclusive.is_some();
        if !has_bounds_or_enum {
            return current;
        }
        let Some(base) = st.resolved_base_type else {
            return current;
        };
        current = base;
    }
    current
}

fn compare_xml_values(
    lower: &crate::types::value::XmlValue,
    upper: &crate::types::value::XmlValue,
) -> Option<std::cmp::Ordering> {
    use crate::types::value::XmlValueKind;
    match (&lower.value, &upper.value) {
        (XmlValueKind::Atomic(a), XmlValueKind::Atomic(b)) => compare_xml_atomic_values(a, b),
        (XmlValueKind::Union(a), _) => compare_xml_values(a, upper),
        (_, XmlValueKind::Union(b)) => compare_xml_values(lower, b),
        _ => None,
    }
}

fn compare_xml_atomic_values(
    lower: &crate::types::value::XmlAtomicValue,
    upper: &crate::types::value::XmlAtomicValue,
) -> Option<std::cmp::Ordering> {
    use crate::types::value::XmlAtomicValue;
    match (lower, upper) {
        (XmlAtomicValue::DateTime(a), XmlAtomicValue::DateTime(b)) => a.partial_cmp(b),
        (XmlAtomicValue::Date(a), XmlAtomicValue::Date(b)) => a.partial_cmp(b),
        (XmlAtomicValue::Time(a), XmlAtomicValue::Time(b)) => a.partial_cmp(b),
        (XmlAtomicValue::Duration(a), XmlAtomicValue::Duration(b)) => a.partial_cmp(b),
        (XmlAtomicValue::YearMonthDuration(a), XmlAtomicValue::YearMonthDuration(b)) => {
            a.partial_cmp(b)
        }
        (XmlAtomicValue::DayTimeDuration(a), XmlAtomicValue::DayTimeDuration(b)) => {
            a.partial_cmp(b)
        }
        (XmlAtomicValue::GYearMonth(a), XmlAtomicValue::GYearMonth(b)) => a.partial_cmp(b),
        (XmlAtomicValue::GYear(a), XmlAtomicValue::GYear(b)) => a.partial_cmp(b),
        (XmlAtomicValue::GMonthDay(a), XmlAtomicValue::GMonthDay(b)) => a.partial_cmp(b),
        (XmlAtomicValue::GDay(a), XmlAtomicValue::GDay(b)) => a.partial_cmp(b),
        (XmlAtomicValue::GMonth(a), XmlAtomicValue::GMonth(b)) => a.partial_cmp(b),
        _ => None,
    }
}

fn get_type_facets(schema_set: &SchemaSet, type_key: TypeKey) -> SchemaResult<Option<FacetSet>> {
    match type_key {
        TypeKey::Simple(key) => {
            if let Some(type_def) = schema_set.arenas.simple_types.get(key) {
                Ok(Some(type_def.facets.clone()))
            } else {
                Ok(None)
            }
        }
        TypeKey::Complex(_) => {
            // Complex types don't have direct facets
            // (simpleContent types have facets in their content definition)
            Ok(None)
        }
    }
}

/// Format a type name for error messages
pub(crate) fn format_type_name(
    schema_set: &SchemaSet,
    name: Option<NameId>,
    namespace: Option<NameId>,
) -> String {
    match name {
        Some(name_id) => {
            let local = schema_set.name_table.resolve(name_id);
            match namespace {
                Some(ns_id) => {
                    let ns = schema_set.name_table.resolve(ns_id);
                    if ns.is_empty() {
                        local.to_string()
                    } else {
                        format!("{{{}}}{}", ns, local)
                    }
                }
                None => local.to_string(),
            }
        }
        None => "(anonymous)".to_string(),
    }
}

/// Minimal "type-like component" view used by [`type_error_context`].
/// Implemented by the type-def structs whose errors carry both a source
/// location and a formatted type name.
pub(crate) trait TypeDefForError {
    fn error_name(&self) -> Option<NameId>;
    fn error_target_namespace(&self) -> Option<NameId>;
    fn error_source(&self) -> Option<&SourceRef>;
}

impl TypeDefForError for crate::arenas::SimpleTypeDefData {
    fn error_name(&self) -> Option<NameId> {
        self.name
    }
    fn error_target_namespace(&self) -> Option<NameId> {
        self.target_namespace
    }
    fn error_source(&self) -> Option<&SourceRef> {
        self.source.as_ref()
    }
}

impl TypeDefForError for crate::arenas::ComplexTypeDefData {
    fn error_name(&self) -> Option<NameId> {
        self.name
    }
    fn error_target_namespace(&self) -> Option<NameId> {
        self.target_namespace
    }
    fn error_source(&self) -> Option<&SourceRef> {
        self.source.as_ref()
    }
}

/// Returns `(location, type_name)` for error construction on a type component.
/// Pairs [`SchemaSet::locate`] with [`format_type_name`] since they always
/// co-occur in `SchemaError::structural` calls built for a type.
pub(crate) fn type_error_context<T: TypeDefForError>(
    schema_set: &SchemaSet,
    type_def: &T,
) -> (Option<SourceLocation>, String) {
    (
        schema_set.locate(type_def.error_source()),
        format_type_name(
            schema_set,
            type_def.error_name(),
            type_def.error_target_namespace(),
        ),
    )
}

/// Resolved effective attribute use for comparison during restriction validation.
/// Attribute identity is (target_namespace, name) per §3.2.6.
struct EffectiveAttributeUse {
    name: NameId,
    target_namespace: Option<NameId>,
    use_kind: AttributeUseKind,
    resolved_type: Option<TypeKey>,
    fixed_value: Option<String>,
    default_value: Option<String>,
    /// XSD 1.1 §3.5.1 / §3.2.2.3: the use-level `{inheritable}` is the local
    /// `inheritable` attribute when present; for `ref`-based uses without an
    /// own `inheritable` it falls back to the resolved declaration's
    /// `{inheritable}`. Used by §3.4.6.3 derivation-ok-restriction to enforce
    /// `G.{inheritable} = S.{inheritable}` (subsumes clause 5.3).
    inheritable: bool,
}

/// Resolve a single attribute use + its parallel resolved data into an
/// `EffectiveAttributeUse`.  Returns `None` when the attribute name
/// cannot be determined (malformed data).
fn resolve_single_attribute_use(
    schema_set: &SchemaSet,
    attr_use: &crate::parser::frames::AttributeUseResult,
    resolved: Option<&crate::arenas::ResolvedAttributeUse>,
) -> Option<EffectiveAttributeUse> {
    let (name, target_namespace) = if let Some(ref_name) = &attr_use.attribute.ref_name {
        if let Some(resolved_attr) = resolved.and_then(|r| r.resolved_ref) {
            let decl = schema_set.arenas.attributes.get(resolved_attr);
            (
                decl.and_then(|d| d.name)?,
                decl.and_then(|d| d.target_namespace),
            )
        } else {
            (ref_name.local_name, ref_name.namespace)
        }
    } else {
        let n = attr_use.attribute.name?;
        // For inline (non-ref) attributes, compute effective namespace
        // using form + attributeFormDefault per §3.2.2.
        let ns = schema_set.effective_local_attribute_namespace(
            attr_use.attribute.target_namespace,
            attr_use.attribute.form.as_deref(),
            attr_use.attribute.source.as_ref(),
            None,
        );
        (n, ns)
    };

    // Prefer the use's resolved_type; fall back to the global declaration's type.
    let resolved_type = resolved.and_then(|r| r.resolved_type).or_else(|| {
        resolved
            .and_then(|r| r.resolved_ref)
            .and_then(|ref_key| schema_set.arenas.attributes.get(ref_key))
            .and_then(|decl| decl.resolved_type)
    });

    // For fixed_value: use the inline fixed, or the resolved global decl's fixed.
    let fixed_value = attr_use.attribute.fixed_value.clone().or_else(|| {
        resolved
            .and_then(|r| r.resolved_ref)
            .and_then(|ref_key| schema_set.arenas.attributes.get(ref_key))
            .and_then(|decl| decl.fixed_value.clone())
    });
    // For default_value: use the inline default, or the resolved global decl's default.
    let default_value = attr_use.attribute.default_value.clone().or_else(|| {
        resolved
            .and_then(|r| r.resolved_ref)
            .and_then(|ref_key| schema_set.arenas.attributes.get(ref_key))
            .and_then(|decl| decl.default_value.clone())
    });

    // {inheritable} per §3.2.2.3: the actual value of the use's
    // `inheritable` attribute (default false). For ref-based uses, the
    // mapping rule says use the {attribute declaration}.{inheritable} —
    // but the parser stores the use's literal value with a `false`
    // default, indistinguishable from "unspecified". Fall back to the
    // referenced declaration's inheritable when the use itself is a
    // ref and is not flagged.
    let inheritable = if attr_use.attribute.inheritable {
        true
    } else if attr_use.attribute.ref_name.is_some() {
        resolved
            .and_then(|r| r.resolved_ref)
            .and_then(|ref_key| schema_set.arenas.attributes.get(ref_key))
            .map(|decl| decl.inheritable)
            .unwrap_or(false)
    } else {
        false
    };

    Some(EffectiveAttributeUse {
        name,
        target_namespace,
        use_kind: attr_use.use_kind,
        resolved_type,
        fixed_value,
        default_value,
        inheritable,
    })
}

/// Collect effective attribute uses from a complex type definition.
///
/// Resolves attribute refs and expands attribute groups into a flat list.
/// Attributes are always on `type_def.attributes` (moved from sc/cc at parse time).
fn collect_effective_attribute_uses(
    schema_set: &SchemaSet,
    type_def: &crate::arenas::ComplexTypeDefData,
) -> Vec<EffectiveAttributeUse> {
    let mut result = Vec::new();

    for (i, attr_use) in type_def.attributes.iter().enumerate() {
        let resolved = type_def.resolved_attributes.get(i);
        if let Some(eau) = resolve_single_attribute_use(schema_set, attr_use, resolved) {
            result.push(eau);
        }
    }

    for &ag_key in &type_def.resolved_attribute_groups {
        collect_attribute_group_uses(schema_set, ag_key, &mut result, 0);
    }

    result
}

// ---------------------------------------------------------------------------
// §3.6.2.2 Effective Attribute Wildcard + §3.10.6.4 Intersection
// ---------------------------------------------------------------------------
//
// These helpers implement the "Common Rules for Attribute Wildcards"
// (§3.6.2.2) used by both the complex-type restriction path
// (`validate_attribute_restriction`) and the redefine attribute-group
// restriction path (`validate_all_redefine_attribute_group_restrictions`).
//
// The output is an `EffectiveAttributeWildcard` with a canonical namespace
// constraint (`CanonicalNs`) in which all `WildcardNamespace` variants have
// been normalized to either `Any`, an explicit positive set, or a
// complement set, with `not_namespace` exclusions already folded in and
// `##other` resolved against XSD version. Intersection then reduces to
// pure set theory on `HashSet<Option<NameId>>`.
//
// Intentionally private to this module — the canonical form never leaks
// into the arena model.

/// Canonical namespace constraint for attribute wildcards (§3.10.6.4).
///
/// All `WildcardNamespace` variants normalize to one of these three cases,
/// with `not_namespace` exclusions already folded in and `##other`
/// resolved via XSD-version-aware rules (XSD 1.0 excludes absent namespace,
/// XSD 1.1 does not).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CanonicalNs {
    /// Every namespace is allowed.
    Any,
    /// Positive set of allowed namespaces. `None` represents the absent
    /// (no-namespace) case.
    Enum(std::collections::HashSet<Option<NameId>>),
    /// Complement set: every namespace except those in the set is allowed.
    /// `Not(empty)` is equivalent to `Any` but is preserved as-is for
    /// symmetry; `canonical_ns_subset` handles this case.
    Not(std::collections::HashSet<Option<NameId>>),
}

/// Effective attribute wildcard, the result of §3.6.2.2.
///
/// Target-namespace-free: `namespace` has already been resolved against
/// each contributor's own target namespace during normalization.
#[derive(Debug, Clone)]
pub(crate) struct EffectiveAttributeWildcard {
    pub(crate) namespace: CanonicalNs,
    pub(crate) not_qname: Vec<crate::parser::frames::NotQNameItem>,
    pub(crate) process_contents: ProcessContents,
}

/// Normalize a single `WildcardResult` into canonical form, resolving
/// `##other`, `##targetNamespace`, `##local`, list tokens, and folding in
/// `not_namespace` exclusions against `target_ns`.
fn normalize_attribute_wildcard(
    schema_set: &SchemaSet,
    wc: &WildcardResult,
    target_ns: Option<NameId>,
) -> EffectiveAttributeWildcard {
    use std::collections::HashSet;

    // Step 1: resolve the primary namespace constraint.
    let base: CanonicalNs = match &wc.namespace {
        WildcardNamespace::Any => CanonicalNs::Any,
        WildcardNamespace::Other => {
            // Version-aware ##other exclusion set (§3.10.1):
            //   XSD 1.0: excludes {target_ns, absent}
            //   XSD 1.1: excludes {target_ns} only
            //
            // When the schema has no target namespace, the "target
            // namespace" IS the absent namespace (None), so
            // `target_ns` is inserted unconditionally to capture that
            // case. For XSD 1.0 with a non-absent target, we additionally
            // insert None (HashSet dedupes if target is already None).
            let mut excl = HashSet::new();
            excl.insert(target_ns);
            if schema_set.is_xsd10() {
                excl.insert(None);
            }
            CanonicalNs::Not(excl)
        }
        WildcardNamespace::TargetNamespace => {
            let mut s = HashSet::new();
            s.insert(target_ns);
            CanonicalNs::Enum(s)
        }
        WildcardNamespace::Local => {
            let mut s = HashSet::new();
            s.insert(None);
            CanonicalNs::Enum(s)
        }
        WildcardNamespace::List(tokens) => {
            let mut s = HashSet::new();
            for tok in tokens {
                s.insert(tok.resolve(target_ns));
            }
            CanonicalNs::Enum(s)
        }
    };

    // Step 2: fold `not_namespace` exclusions into the canonical form.
    let not_ns: HashSet<Option<NameId>> = wc
        .not_namespace
        .iter()
        .map(|t| t.resolve(target_ns))
        .collect();

    let namespace = if not_ns.is_empty() {
        base
    } else {
        match base {
            CanonicalNs::Any => CanonicalNs::Not(not_ns),
            CanonicalNs::Enum(set) => {
                let filtered: HashSet<Option<NameId>> =
                    set.into_iter().filter(|ns| !not_ns.contains(ns)).collect();
                CanonicalNs::Enum(filtered)
            }
            CanonicalNs::Not(set) => {
                let mut combined = set;
                combined.extend(not_ns);
                CanonicalNs::Not(combined)
            }
        }
    };

    EffectiveAttributeWildcard {
        namespace,
        not_qname: wc.not_qname.clone(),
        process_contents: wc.process_contents,
    }
}

/// §3.10.6.4 namespace-constraint intersection. Pure set theory on the
/// canonical lattice.
fn intersect_canonical_ns(a: &CanonicalNs, b: &CanonicalNs) -> CanonicalNs {
    use std::collections::HashSet;
    match (a, b) {
        // Any ∩ X = X
        (CanonicalNs::Any, other) | (other, CanonicalNs::Any) => other.clone(),

        // Enum ∩ Enum = set intersection
        (CanonicalNs::Enum(s1), CanonicalNs::Enum(s2)) => {
            let inter: HashSet<Option<NameId>> = s1.intersection(s2).copied().collect();
            CanonicalNs::Enum(inter)
        }

        // Enum ∩ Not(N) = Enum \ N
        (CanonicalNs::Enum(s), CanonicalNs::Not(n))
        | (CanonicalNs::Not(n), CanonicalNs::Enum(s)) => {
            let filtered: HashSet<Option<NameId>> =
                s.iter().filter(|ns| !n.contains(ns)).copied().collect();
            CanonicalNs::Enum(filtered)
        }

        // Not(N1) ∩ Not(N2) = Not(N1 ∪ N2)
        (CanonicalNs::Not(n1), CanonicalNs::Not(n2)) => {
            let mut union = n1.clone();
            union.extend(n2.iter().copied());
            CanonicalNs::Not(union)
        }
    }
}

/// §3.10.6.3 cos-aw-union on the canonical namespace lattice.
/// Mirror of `intersect_canonical_ns` for the union side.
fn union_canonical_ns(a: &CanonicalNs, b: &CanonicalNs) -> CanonicalNs {
    use std::collections::HashSet;
    match (a, b) {
        // Any ∪ X = Any
        (CanonicalNs::Any, _) | (_, CanonicalNs::Any) => CanonicalNs::Any,

        // Enum(s1) ∪ Enum(s2) = set union
        (CanonicalNs::Enum(s1), CanonicalNs::Enum(s2)) => {
            let mut union = s1.clone();
            union.extend(s2.iter().copied());
            CanonicalNs::Enum(union)
        }

        // Enum(s) ∪ Not(n) = Not(n \ s) — every namespace b allows
        // (= everything except n) plus everything in s. The result is
        // "not (n minus s)": elements of n already in s no longer need
        // to be excluded.
        (CanonicalNs::Enum(s), CanonicalNs::Not(n))
        | (CanonicalNs::Not(n), CanonicalNs::Enum(s)) => {
            let filtered: HashSet<Option<NameId>> =
                n.iter().filter(|ns| !s.contains(ns)).copied().collect();
            if filtered.is_empty() {
                CanonicalNs::Any
            } else {
                CanonicalNs::Not(filtered)
            }
        }

        // Not(n1) ∪ Not(n2) = Not(n1 ∩ n2). A namespace is excluded by
        // the union only if it's excluded by both sides.
        (CanonicalNs::Not(n1), CanonicalNs::Not(n2)) => {
            let inter: HashSet<Option<NameId>> = n1.intersection(n2).copied().collect();
            if inter.is_empty() {
                CanonicalNs::Any
            } else {
                CanonicalNs::Not(inter)
            }
        }
    }
}

/// Canonical namespace subset: `a ⊆ b`. Tests whether every namespace
/// allowed by `a` is also allowed by `b`.
fn canonical_ns_subset(a: &CanonicalNs, b: &CanonicalNs) -> bool {
    match (a, b) {
        // Anything ⊆ Any (Any accepts all namespaces)
        (_, CanonicalNs::Any) => true,

        // Any ⊆ Not(empty) also holds, but only when b is literally `Any`
        // after the above match. Otherwise Any is not a subset of anything
        // finite or complemented.
        (CanonicalNs::Any, _) => false,

        // Enum(s) ⊆ Enum(t) iff s ⊆ t
        (CanonicalNs::Enum(s), CanonicalNs::Enum(t)) => s.iter().all(|ns| t.contains(ns)),

        // Enum(s) ⊆ Not(n) iff s ∩ n = ∅  (no element of s is excluded by n)
        (CanonicalNs::Enum(s), CanonicalNs::Not(n)) => s.iter().all(|ns| !n.contains(ns)),

        // Not(n1) ⊆ Not(n2) iff n2 ⊆ n1  (a's exclusion set must be at
        // least as large as b's; the larger the exclusion, the smaller the
        // allowed set)
        (CanonicalNs::Not(n1), CanonicalNs::Not(n2)) => n2.iter().all(|ns| n1.contains(ns)),

        // Not(n) ⊆ Enum(s): `Not(n)` allows infinitely many namespaces, a
        // finite `Enum(s)` cannot contain them all. False.
        (CanonicalNs::Not(_), CanonicalNs::Enum(_)) => false,
    }
}

/// Intersect two effective attribute wildcards per §3.10.6.4.
///
/// - `namespace`: `intersect_canonical_ns`
/// - `not_qname`: union of both lists (deduplicated). Per §3.10.6.4
///   disallowed_names clause 3, `##defined` is preserved if present on
///   either side.
/// - `process_contents`: takes the LEFT operand's value. Callers must
///   pass the operands in the order required by §3.6.2.2 (clause 3.2.1
///   passes L first, clause 3.2.2 passes W[0] first).
fn intersect_effective_attribute_wildcards(
    a: &EffectiveAttributeWildcard,
    b: &EffectiveAttributeWildcard,
) -> EffectiveAttributeWildcard {
    let namespace = intersect_canonical_ns(&a.namespace, &b.namespace);

    // Union not_qname lists. Items whose namespace is no longer admitted
    // by the intersected constraint are redundant but harmless to keep.
    let mut not_qname = a.not_qname.clone();
    for item in &b.not_qname {
        if !not_qname.contains(item) {
            not_qname.push(item.clone());
        }
    }

    EffectiveAttributeWildcard {
        namespace,
        not_qname,
        process_contents: a.process_contents,
    }
}

/// Structural error for attribute-group reference cycles exceeding the
/// depth guard during §3.6.2.2 walking. These should already be rejected
/// by the resolver — fail loudly rather than silently synthesize `Any`.
fn attribute_group_cycle_error() -> SchemaError {
    SchemaError::structural(
        "derivation-ok-restriction",
        "attribute group reference cycle exceeded max depth while computing \
         effective attribute wildcard (§3.6.2.2)",
        None,
    )
}

/// Combine a local effective wildcard with an ordered sequence of
/// contributed effective wildcards per §3.6.2.2 clauses 3.1/3.2.1/3.2.2:
///
/// * W empty ⇒ `local` (or `None` if neither side is present).
/// * L non-absent ⇒ pc from L, intersect L with every Wi.
/// * L absent, W non-empty ⇒ pc from W[0], intersect every Wi.
fn combine_effective_wildcards(
    local: Option<EffectiveAttributeWildcard>,
    w: Vec<EffectiveAttributeWildcard>,
) -> Option<EffectiveAttributeWildcard> {
    match (local, w.is_empty()) {
        (None, true) => None,
        (Some(l), true) => Some(l),
        (Some(l), false) => Some(w.into_iter().fold(l, |acc, wi| {
            intersect_effective_attribute_wildcards(&acc, &wi)
        })),
        (None, false) => {
            let mut it = w.into_iter();
            let first = it.next().expect("w is non-empty");
            Some(it.fold(first, |acc, wi| {
                intersect_effective_attribute_wildcards(&acc, &wi)
            }))
        }
    }
}

/// §3.6.2.2 Common Rules for Attribute Wildcards.
///
/// Given a local wildcard `local_wc` (optional) and the ordered sequence
/// of resolved referenced attribute groups, compute the effective
/// attribute wildcard. Each referenced group's own effective wildcard is
/// computed recursively (so wildcards inherited through chains of
/// `<xs:attributeGroup ref=...>` references are properly intersected).
///
/// Returns `Err` if the attribute-group reference tree exceeds the depth
/// guard (cycle protection, matching `collect_attribute_group_uses`).
pub(crate) fn effective_attribute_wildcard(
    schema_set: &SchemaSet,
    local_wc: Option<&WildcardResult>,
    local_target_ns: Option<NameId>,
    attribute_groups: &[AttributeGroupKey],
) -> SchemaResult<Option<EffectiveAttributeWildcard>> {
    let local = local_wc.map(|w| normalize_attribute_wildcard(schema_set, w, local_target_ns));

    let mut w: Vec<EffectiveAttributeWildcard> = Vec::new();
    for &ag_key in attribute_groups {
        collect_effective_group_wildcards(schema_set, ag_key, &mut w, 0)?;
    }

    Ok(combine_effective_wildcards(local, w))
}

/// Runtime entry point for attribute wildcard matching.
///
/// Returns the type's full effective `{attribute wildcard}` per §3.6.2.2
/// (intersection of own + attribute groups) chained with §3.4.2.5's
/// extension union over the base chain. Restriction picks the derived's
/// own (§3.6.2.2 result), falling back to the base only when the derived
/// has no own wildcard at all — matching the prior `find_effective_wildcard`
/// runtime convention.
///
/// The return value is target-namespace-free: all `##targetNamespace` /
/// `##other` / list tokens have been resolved against each contributor's
/// origin target namespace, so the runtime can match attributes against
/// `EffectiveAttributeWildcard.namespace` directly.
pub(crate) fn compute_runtime_attribute_wildcard(
    schema_set: &SchemaSet,
    ct_key: ComplexTypeKey,
) -> Option<EffectiveAttributeWildcard> {
    compute_runtime_attribute_wildcard_bounded(schema_set, ct_key, 0)
}

fn compute_runtime_attribute_wildcard_bounded(
    schema_set: &SchemaSet,
    ct_key: ComplexTypeKey,
    depth: u32,
) -> Option<EffectiveAttributeWildcard> {
    if depth > 100 {
        return None;
    }
    let ct = schema_set.arenas.complex_types.get(ct_key)?;

    // Own §3.6.2.2 result: own xs:anyAttribute combined with all referenced
    // attribute groups via the existing canonical helpers. Errors here
    // (cycle overflow) collapse to "no wildcard" — this is the runtime
    // path; cycles are rejected upstream.
    let own_local = own_attribute_wildcard_ref(ct);
    let own = effective_attribute_wildcard(
        schema_set,
        own_local,
        ct.target_namespace,
        &ct.resolved_attribute_groups,
    )
    .ok()
    .flatten();

    let Some(TypeKey::Complex(base_key)) = ct.resolved_base_type else {
        return own;
    };
    if base_key == schema_set.any_type_key() {
        return own;
    }

    match ct.derivation_method {
        Some(DerivationMethod::Extension) => {
            let base = compute_runtime_attribute_wildcard_bounded(schema_set, base_key, depth + 1);
            match (own, base) {
                (Some(a), Some(b)) => Some(union_effective_attribute_wildcards(&a, &b)),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            }
        }
        // Restriction or no derivation: derived's own wildcard is
        // authoritative. If the derived has no wildcard at all, fall
        // back to the base for inheritance-style behaviour matching
        // the prior runtime semantics.
        _ => own.or_else(|| {
            compute_runtime_attribute_wildcard_bounded(schema_set, base_key, depth + 1)
        }),
    }
}

/// Pull the type's "own" attribute wildcard out of either the top-level
/// field or the SimpleContent / ComplexContent derivation arm where the
/// `<xs:anyAttribute>` legitimately lives.
fn own_attribute_wildcard_ref(ct: &crate::arenas::ComplexTypeDefData) -> Option<&WildcardResult> {
    if let Some(wc) = ct.attribute_wildcard.as_ref() {
        return Some(wc);
    }
    match &ct.content {
        ComplexContentResult::Empty => None,
        ComplexContentResult::Simple(sc) => sc.attribute_wildcard.as_ref(),
        ComplexContentResult::Complex(cc) => cc.attribute_wildcard.as_ref(),
    }
}

/// §3.4.2.5 extension union of two effective attribute wildcards.
///
/// - `namespace`: `union_canonical_ns` (set-theoretic union)
/// - `not_qname`: per §3.10.6.3 cos-aw-union, the union must not admit any
///   name that neither input wildcard admits. A literal QName excluded by
///   one wildcard stays excluded in the union iff the other wildcard
///   doesn't admit it either (whether by namespace or by its own
///   disallowed_names). `##defined` / `##definedSibling` keep the simple
///   intersection rule — each is in the result iff both inputs have it.
/// - `process_contents`: less restrictive of the two (Skip > Lax > Strict).
fn union_effective_attribute_wildcards(
    a: &EffectiveAttributeWildcard,
    b: &EffectiveAttributeWildcard,
) -> EffectiveAttributeWildcard {
    use crate::parser::frames::NotQNameItem;

    let namespace = union_canonical_ns(&a.namespace, &b.namespace);

    // Combine disallowed names. For each QName excluded by one side, keep
    // it excluded iff the other side also excludes it (by namespace or by
    // literal QName). For `##defined` / `##definedSibling`, the simple
    // intersection rule applies.
    let mut not_qname: Vec<NotQNameItem> = Vec::new();

    let mut consider = |item: &NotQNameItem, other: &EffectiveAttributeWildcard| match item {
        NotQNameItem::QName {
            namespace,
            local_name,
        } => {
            let admitted_by_other_ns = match &other.namespace {
                CanonicalNs::Any => true,
                CanonicalNs::Enum(set) => set.contains(namespace),
                CanonicalNs::Not(set) => !set.contains(namespace),
            };
            let excluded_by_other_qname = other.not_qname.iter().any(|o| match o {
                NotQNameItem::QName {
                    namespace: ons,
                    local_name: oln,
                } => ons == namespace && oln == local_name,
                NotQNameItem::Defined | NotQNameItem::DefinedSibling => false,
            });
            if (!admitted_by_other_ns || excluded_by_other_qname) && !not_qname.contains(item) {
                not_qname.push(item.clone());
            }
        }
        NotQNameItem::Defined | NotQNameItem::DefinedSibling => {
            if other
                .not_qname
                .iter()
                .any(|o| std::mem::discriminant(o) == std::mem::discriminant(item))
                && !not_qname.contains(item)
            {
                not_qname.push(item.clone());
            }
        }
    };

    for item in &a.not_qname {
        consider(item, b);
    }
    for item in &b.not_qname {
        consider(item, a);
    }

    let process_contents = if process_contents_strictness(a.process_contents)
        <= process_contents_strictness(b.process_contents)
    {
        a.process_contents
    } else {
        b.process_contents
    };

    EffectiveAttributeWildcard {
        namespace,
        not_qname,
        process_contents,
    }
}

/// Recursive walker for `effective_attribute_wildcard`: follows
/// `resolved_ref` delegation, then iterates `resolved_attribute_groups`
/// in document order. Each referenced group's own effective wildcard is
/// computed and appended to `out` if it is non-absent (per §3.6.2.2
/// step 2 — "non-absent `{attribute wildcard}`s").
///
/// Depth guard matches `collect_attribute_group_uses` (> 20).
fn collect_effective_group_wildcards(
    schema_set: &SchemaSet,
    ag_key: AttributeGroupKey,
    out: &mut Vec<EffectiveAttributeWildcard>,
    depth: usize,
) -> SchemaResult<()> {
    if depth > 20 {
        return Err(attribute_group_cycle_error());
    }

    let Some(ag) = schema_set.arenas.attribute_groups.get(ag_key) else {
        return Ok(());
    };

    if let Some(ref_key) = ag.resolved_ref {
        return collect_effective_group_wildcards(schema_set, ref_key, out, depth + 1);
    }

    if let Some(eff) = effective_attribute_wildcard_for_group(schema_set, ag, depth + 1)? {
        out.push(eff);
    }

    Ok(())
}

/// Compute the effective wildcard for a single attribute group, walking
/// `resolved_ref` delegation and `resolved_attribute_groups` recursively.
/// Separate from the top-level `effective_attribute_wildcard` so the depth
/// counter propagates correctly through nested calls.
fn effective_attribute_wildcard_for_group(
    schema_set: &SchemaSet,
    ag: &crate::arenas::AttributeGroupData,
    depth: usize,
) -> SchemaResult<Option<EffectiveAttributeWildcard>> {
    if depth > 20 {
        return Err(attribute_group_cycle_error());
    }

    if let Some(ref_key) = ag.resolved_ref {
        let Some(target) = schema_set.arenas.attribute_groups.get(ref_key) else {
            return Ok(None);
        };
        return effective_attribute_wildcard_for_group(schema_set, target, depth + 1);
    }

    let local = ag
        .attribute_wildcard
        .as_ref()
        .map(|w| normalize_attribute_wildcard(schema_set, w, ag.target_namespace));

    let mut w: Vec<EffectiveAttributeWildcard> = Vec::new();
    for &nested_key in &ag.resolved_attribute_groups {
        collect_effective_group_wildcards(schema_set, nested_key, &mut w, depth + 1)?;
    }

    Ok(combine_effective_wildcards(local, w))
}

/// Check that `derived` is a valid restriction of `base` (derived ⊆ base)
/// per cos-ns-subset (§3.10.6.2) on attribute wildcards.
///
/// Verifies:
/// 1. canonical namespace subset (clauses 1-4 of §3.10.6.2 on the
///    namespace constraint),
/// 2. each QName in `base.not_qname` must not be allowed by `derived`
///    (§3.10.6.2 disallowed_names clause 1). "Not allowed" covers any
///    rejection mechanism on the derived side: namespace constraint,
///    literal notQName entry, or `##defined` (which rejects any
///    globally declared attribute). This is delegated to
///    `effective_wildcard_allows_attribute` so all three mechanisms
///    are checked uniformly.
/// 3. if `base.not_qname` contains `##defined`, derived must also
///    (§3.10.6.2 clause 2); same for `##sibling` (clause 3). These
///    two keywords require literal containment, unlike QName members.
/// 4. derived processContents strictness ≥ base strictness (mirrors
///    `validate_open_content_restriction` at derivation.rs:2488-2501).
///
/// Takes `schema_set` because `##defined` coverage requires a lookup
/// against `schema_set.lookup_attribute` via
/// `effective_wildcard_allows_attribute`.
///
/// On failure returns `Err(reason)` so callers can build informative
/// error messages.
fn effective_attribute_wildcard_restricts(
    schema_set: &SchemaSet,
    derived: &EffectiveAttributeWildcard,
    base: &EffectiveAttributeWildcard,
) -> Result<(), &'static str> {
    use crate::parser::frames::NotQNameItem;

    if !canonical_ns_subset(&derived.namespace, &base.namespace) {
        return Err("namespace constraint is not a subset of the base wildcard");
    }

    // §3.10.6.2 disallowed_names clause 1: each QName member of base's
    // not_qname must not be admitted by derived. `effective_wildcard_allows_attribute`
    // correctly handles namespace-constraint rejection, literal QName
    // exclusion, and `##defined` (with schema lookup) in one pass.
    for item in &base.not_qname {
        match item {
            NotQNameItem::QName {
                namespace,
                local_name,
            } => {
                if effective_wildcard_allows_attribute(schema_set, derived, *namespace, *local_name)
                {
                    return Err(
                        "notQName exclusions do not cover the base wildcard's disallowed names",
                    );
                }
            }
            // Clause 2: `##defined` requires literal containment.
            NotQNameItem::Defined => {
                if !derived
                    .not_qname
                    .iter()
                    .any(|d| matches!(d, NotQNameItem::Defined))
                {
                    return Err("base wildcard excludes ##defined but derived does not");
                }
            }
            // Clause 3: `##definedSibling` requires literal containment.
            NotQNameItem::DefinedSibling => {
                if !derived
                    .not_qname
                    .iter()
                    .any(|d| matches!(d, NotQNameItem::DefinedSibling))
                {
                    return Err("base wildcard excludes ##definedSibling but derived does not");
                }
            }
        }
    }

    if process_contents_strictness(derived.process_contents)
        < process_contents_strictness(base.process_contents)
    {
        return Err("processContents is weaker than the base wildcard");
    }

    Ok(())
}

/// Does this effective wildcard admit a specific `(namespace, name)`
/// attribute?
///
/// Mirror of `wildcard_allows_attribute` (derivation.rs:3091) operating
/// on the canonical form. Preserves the load-bearing `NotQNameItem::Defined`
/// semantics documented at derivation.rs:3078-3090 — `##defined` only
/// excludes attributes that are actually globally declared, not an
/// unconditional block.
pub(crate) fn effective_wildcard_allows_attribute(
    schema_set: &SchemaSet,
    wc: &EffectiveAttributeWildcard,
    attr_namespace: Option<NameId>,
    attr_name: NameId,
) -> bool {
    // Namespace constraint check.
    let ns_ok = match &wc.namespace {
        CanonicalNs::Any => true,
        CanonicalNs::Enum(set) => set.contains(&attr_namespace),
        CanonicalNs::Not(set) => !set.contains(&attr_namespace),
    };
    if !ns_ok {
        return false;
    }

    // not_qname exclusions, including ##defined schema lookup.
    for item in &wc.not_qname {
        match item {
            crate::parser::frames::NotQNameItem::QName {
                namespace: qns,
                local_name,
            } => {
                if *qns == attr_namespace && *local_name == attr_name {
                    return false;
                }
            }
            crate::parser::frames::NotQNameItem::Defined => {
                if schema_set
                    .lookup_attribute(attr_namespace, attr_name)
                    .is_some()
                {
                    return false;
                }
            }
            crate::parser::frames::NotQNameItem::DefinedSibling => {
                // Not meaningful for attribute wildcards; ignore (matches
                // `wildcard_allows_attribute`).
            }
        }
    }

    true
}

/// Recursively expand an attribute group into effective attribute uses.
fn collect_attribute_group_uses(
    schema_set: &SchemaSet,
    ag_key: AttributeGroupKey,
    result: &mut Vec<EffectiveAttributeUse>,
    depth: usize,
) {
    if depth > 20 {
        return;
    }

    let Some(ag) = schema_set.arenas.attribute_groups.get(ag_key) else {
        return;
    };

    if let Some(ref_key) = ag.resolved_ref {
        collect_attribute_group_uses(schema_set, ref_key, result, depth + 1);
        return;
    }

    for (i, attr_use) in ag.attributes.iter().enumerate() {
        let resolved = ag.resolved_attributes.get(i);
        if let Some(eau) = resolve_single_attribute_use(schema_set, attr_use, resolved) {
            result.push(eau);
        }
    }

    for &nested_key in &ag.resolved_attribute_groups {
        collect_attribute_group_uses(schema_set, nested_key, result, depth + 1);
    }
}

/// Delegate to `SchemaSet::is_type_derived_from` with no method exclusions.
fn is_type_derived_from(schema_set: &SchemaSet, derived_key: TypeKey, base_key: TypeKey) -> bool {
    schema_set.is_type_derived_from(derived_key, base_key, DerivationSet::empty())
}

/// Outcome of comparing derived and base effective attribute wildcards.
/// Shared by the complex-type restriction path and the redefine
/// attribute-group restriction path.
enum WildcardRestrictionOutcome {
    /// Derived has no effective wildcard; any base is valid.
    DerivedAbsent,
    /// Derived has a wildcard but base has none — invalid restriction.
    AddedInDerived,
    /// Both have wildcards and the subset check failed with the given
    /// reason string.
    NotSubset(&'static str),
    /// Both have wildcards and derived is a valid restriction of base.
    Valid,
}

/// Compare two precomputed effective attribute wildcards and classify
/// the restriction relationship. The caller is responsible for deciding
/// how to report each outcome.
fn classify_attribute_wildcard_restriction(
    schema_set: &SchemaSet,
    derived_eff: Option<&EffectiveAttributeWildcard>,
    base_eff: Option<&EffectiveAttributeWildcard>,
) -> WildcardRestrictionOutcome {
    match (derived_eff, base_eff) {
        (None, _) => WildcardRestrictionOutcome::DerivedAbsent,
        (Some(_), None) => WildcardRestrictionOutcome::AddedInDerived,
        (Some(d), Some(b)) => match effective_attribute_wildcard_restricts(schema_set, d, b) {
            Ok(()) => WildcardRestrictionOutcome::Valid,
            Err(reason) => WildcardRestrictionOutcome::NotSubset(reason),
        },
    }
}

/// True when the complex type has no local attribute wildcard AND no
/// attribute groups that could contribute one — the §3.6.2.2 walk is
/// guaranteed to return `None`, so callers can skip the full computation.
fn complex_type_has_no_attribute_wildcard_source(
    type_def: &crate::arenas::ComplexTypeDefData,
) -> bool {
    type_def.attribute_wildcard.is_none() && type_def.resolved_attribute_groups.is_empty()
}

/// Validate attribute uses in a complex type restriction.
///
/// derivation-ok-restriction clause 3 (§3.4.6.3): If E's attributes satisfy
/// T's attribute constraints, they must also satisfy B's.  This means:
/// - Required attributes in the base must remain required in the derived type
///
/// derivation-ok-restriction clause 4: Attribute types in T must be validly
/// substitutable for those in B.
fn validate_attribute_restriction(
    schema_set: &SchemaSet,
    derived: &crate::arenas::ComplexTypeDefData,
    base: &crate::arenas::ComplexTypeDefData,
) -> SchemaResult<()> {
    let derived_attrs = collect_effective_attribute_uses(schema_set, derived);
    let base_attrs = collect_effective_attribute_uses(schema_set, base);

    let location = derived
        .source
        .as_ref()
        .and_then(|s| schema_set.source_maps.locate(s));
    let type_name = format_type_name(schema_set, derived.name, derived.target_namespace);
    let base_name = format_type_name(schema_set, base.name, base.target_namespace);

    // Check clause 3: required base attributes must remain required in the
    // derived type's effective {attribute uses}.
    //
    // Per §3.4.2.3 mapping rules, an attribute use in the base that is NOT
    // matched (by name + target namespace) by a directly-declared use in the
    // restriction is inherited into the derived type's {attribute uses}
    // unchanged.  The derived type therefore satisfies clause 3 trivially for
    // inherited attribute uses — we only need to reject cases where the
    // derived type explicitly *declares* the attribute with a weaker use
    // kind (optional or prohibited).
    for base_attr in &base_attrs {
        if base_attr.use_kind != AttributeUseKind::Required {
            continue;
        }

        // Find matching derived attribute by expanded name (namespace + local)
        let derived_attr = derived_attrs
            .iter()
            .find(|a| a.name == base_attr.name && a.target_namespace == base_attr.target_namespace);

        match derived_attr {
            // Explicit re-declaration preserves required-ness: OK.
            Some(da) if da.use_kind == AttributeUseKind::Required => {}
            // Not declared in restriction → inherited from base as required: OK.
            None => {}
            // Explicit re-declaration with weaker use (optional / prohibited):
            // the derived type's effective {attribute uses} no longer guarantees
            // presence — reject as invalid restriction.
            Some(_) => {
                let attr_name_str = schema_set.name_table.resolve(base_attr.name);
                return Err(SchemaError::structural(
                    "derivation-ok-restriction",
                    format!(
                        "Complex type '{}' restricting '{}': base type requires attribute '{}' \
                         but the derived type weakens it to optional or prohibited",
                        type_name, base_name, attr_name_str,
                    ),
                    location,
                ));
            }
        }
    }

    // Check clause 4: attribute type derivation
    for derived_attr in &derived_attrs {
        let Some(derived_type_key) = derived_attr.resolved_type else {
            continue;
        };

        let base_attr = base_attrs.iter().find(|a| {
            a.name == derived_attr.name && a.target_namespace == derived_attr.target_namespace
        });
        let Some(base_attr) = base_attr else { continue };
        let Some(base_type_key) = base_attr.resolved_type else {
            continue;
        };

        if derived_type_key == base_type_key {
            continue;
        }

        if !is_type_derived_from(schema_set, derived_type_key, base_type_key) {
            let attr_name_str = schema_set.name_table.resolve(derived_attr.name);
            return Err(SchemaError::structural(
                "derivation-ok-restriction",
                format!(
                    "Complex type '{}' restricting '{}': attribute '{}' has a type \
                     that is not validly derived from the base attribute type",
                    type_name, base_name, attr_name_str,
                ),
                location,
            ));
        }
    }

    // §3.4.6.3 derivation-ok-restriction value-constraint check: when the
    // base attribute use has a `fixed` value, the derived attribute use
    // (when re-declared) must also have a `fixed` value equal to the base's.
    // A derived `default` or no value constraint over a base `fixed` is
    // invalid because it loosens the constraint.
    for base_attr in &base_attrs {
        let Some(base_fixed) = base_attr.fixed_value.as_deref() else {
            continue;
        };
        // Find re-declared derived attr matching this base attr.
        let Some(derived_attr) = derived_attrs
            .iter()
            .find(|a| a.name == base_attr.name && a.target_namespace == base_attr.target_namespace)
        else {
            // Inherited unchanged: OK.
            continue;
        };
        match derived_attr.fixed_value.as_deref() {
            Some(d_fixed)
                if crate::validation::simple::fixed_values_equal(
                    d_fixed,
                    base_fixed,
                    base_attr.resolved_type,
                    schema_set,
                ) => {}
            Some(d_fixed) => {
                let attr_name_str = schema_set.name_table.resolve(derived_attr.name);
                return Err(SchemaError::structural(
                    "derivation-ok-restriction",
                    format!(
                        "Complex type '{}' restricting '{}': attribute '{}' \
                         changes 'fixed' value from '{}' to '{}'",
                        type_name, base_name, attr_name_str, base_fixed, d_fixed,
                    ),
                    location,
                ));
            }
            None => {
                // Derived has no fixed value (either default or nothing) — too
                // loose under base's fixed constraint.
                let attr_name_str = schema_set.name_table.resolve(derived_attr.name);
                let what = if derived_attr.default_value.is_some() {
                    "uses 'default' (cannot weaken base 'fixed')"
                } else {
                    "drops the 'fixed' value constraint"
                };
                return Err(SchemaError::structural(
                    "derivation-ok-restriction",
                    format!(
                        "Complex type '{}' restricting '{}': attribute '{}' {}",
                        type_name, base_name, attr_name_str, what,
                    ),
                    location,
                ));
            }
        }
    }

    // §3.4.6.3 clause 3 / "subsumes" clause 5.3 (XSD 1.1 only):
    // for each attribute use that exists in both the base and the derived
    // type, the base's {inheritable} must equal the derived's. The default
    // binding for an attribute information item subsumes only when the
    // attribute use's inheritability matches; flipping it changes the
    // descendant attribute-inheritance graph, which is not a valid
    // restriction.
    if schema_set.is_xsd11() {
        for derived_attr in &derived_attrs {
            let Some(base_attr) = base_attrs.iter().find(|a| {
                a.name == derived_attr.name && a.target_namespace == derived_attr.target_namespace
            }) else {
                continue;
            };
            if base_attr.inheritable != derived_attr.inheritable {
                let attr_name_str = schema_set.name_table.resolve(derived_attr.name);
                return Err(SchemaError::structural(
                    "derivation-ok-restriction",
                    format!(
                        "Complex type '{}' restricting '{}': attribute '{}' changes \
                         {{inheritable}} from {} to {}",
                        type_name,
                        base_name,
                        attr_name_str,
                        base_attr.inheritable,
                        derived_attr.inheritable,
                    ),
                    location,
                ));
            }
        }
    }

    // §3.4.6.3 clause 3 (attribute wildcard half): compute the effective
    // attribute wildcard for both sides per §3.6.2.2 and verify
    // derived ⊆ base. When the derived type has no wildcard source at
    // all, the §3.6.2.2 walk is guaranteed to return `None` and the
    // restriction is trivially valid — skip both walks in that common
    // case to avoid O(types × groups) arena lookups per compile.
    if !complex_type_has_no_attribute_wildcard_source(derived) {
        let derived_eff = effective_attribute_wildcard(
            schema_set,
            derived.attribute_wildcard.as_ref(),
            derived.target_namespace,
            &derived.resolved_attribute_groups,
        )?;
        let base_eff = effective_attribute_wildcard(
            schema_set,
            base.attribute_wildcard.as_ref(),
            base.target_namespace,
            &base.resolved_attribute_groups,
        )?;

        match classify_attribute_wildcard_restriction(
            schema_set,
            derived_eff.as_ref(),
            base_eff.as_ref(),
        ) {
            WildcardRestrictionOutcome::DerivedAbsent | WildcardRestrictionOutcome::Valid => {}
            WildcardRestrictionOutcome::AddedInDerived => {
                return Err(SchemaError::structural(
                    "derivation-ok-restriction",
                    format!(
                        "Complex type '{}' restricting '{}': derived type has an attribute \
                         wildcard but the base type does not",
                        type_name, base_name,
                    ),
                    location,
                ));
            }
            WildcardRestrictionOutcome::NotSubset(reason) => {
                return Err(SchemaError::structural(
                    "derivation-ok-restriction",
                    format!(
                        "Complex type '{}' restricting '{}': attribute wildcard is not \
                         a valid restriction of the base wildcard: {}",
                        type_name, base_name, reason,
                    ),
                    location,
                ));
            }
        }
    }

    Ok(())
}

/// Treat a facet-bound-literal validation failure as acceptable only when it
/// is a same-kind bound violation *and* the derived literal equals the base
/// type's matching bound literal. XSD Part 2 §4.3.9 permits equality at the
/// boundary (derived `maxExclusive` = base `maxExclusive`) even though the
/// base's value space excludes values equal to its own bound.
fn is_bound_self_violation(
    err: &crate::validation::errors::ValidationError,
    kind: FacetKind,
    schema_set: &SchemaSet,
    base_key: TypeKey,
    value: &str,
) -> bool {
    let code = match kind {
        FacetKind::MaxExclusive => "cvc-maxExclusive-valid",
        FacetKind::MaxInclusive => "cvc-maxInclusive-valid",
        FacetKind::MinExclusive => "cvc-minExclusive-valid",
        FacetKind::MinInclusive => "cvc-minInclusive-valid",
        _ => return false,
    };
    if err.constraint != code {
        return false;
    }
    let Some(base_bound) = find_base_bound_literal(schema_set, base_key, kind) else {
        return false;
    };
    let Some(v) = parse_past_own_bound(schema_set, base_key, value) else {
        return false;
    };
    let Some(b) = parse_past_own_bound(schema_set, base_key, &base_bound) else {
        return false;
    };
    v.typed_value == b.typed_value
}

/// Parse `value` as an instance of `base_key`, falling back to the nearest
/// ancestor without bound facets when the direct parse fails on a same-kind
/// bound violation (the boundary-equality case this helper exists to serve).
fn parse_past_own_bound(
    schema_set: &SchemaSet,
    base_key: TypeKey,
    value: &str,
) -> Option<crate::validation::simple::SimpleTypeResult> {
    if let Ok(r) = crate::validation::simple::validate_simple_type(value, base_key, schema_set) {
        return Some(r);
    }
    let without_bounds = lexical_base(schema_set, base_key)?;
    crate::validation::simple::validate_simple_type(value, without_bounds, schema_set).ok()
}

/// Walk past bound-restriction types to find a primitive base suitable for
/// lexical-only parsing of a bound literal.
fn lexical_base(schema_set: &SchemaSet, base_key: TypeKey) -> Option<TypeKey> {
    let mut current = base_key;
    for _ in 0..100 {
        match current {
            TypeKey::Simple(sk) => {
                let st = schema_set.arenas.simple_types.get(sk)?;
                let has_bounds = st.facets.min_inclusive.is_some()
                    || st.facets.min_exclusive.is_some()
                    || st.facets.max_inclusive.is_some()
                    || st.facets.max_exclusive.is_some();
                if !has_bounds {
                    return Some(current);
                }
                current = st.resolved_base_type?;
            }
            TypeKey::Complex(_) => return None,
        }
    }
    None
}

/// Find the base type's same-kind bound literal by walking the simple-type
/// chain. Returns the first matching facet literal encountered.
fn find_base_bound_literal(
    schema_set: &SchemaSet,
    base_key: TypeKey,
    kind: FacetKind,
) -> Option<String> {
    let mut current = base_key;
    for _ in 0..100 {
        match current {
            TypeKey::Simple(sk) => {
                let st = schema_set.arenas.simple_types.get(sk)?;
                let literal = match kind {
                    FacetKind::MaxExclusive => {
                        st.facets.max_exclusive.as_ref().map(|f| f.value.clone())
                    }
                    FacetKind::MaxInclusive => {
                        st.facets.max_inclusive.as_ref().map(|f| f.value.clone())
                    }
                    FacetKind::MinExclusive => {
                        st.facets.min_exclusive.as_ref().map(|f| f.value.clone())
                    }
                    FacetKind::MinInclusive => {
                        st.facets.min_inclusive.as_ref().map(|f| f.value.clone())
                    }
                    _ => None,
                };
                if let Some(v) = literal {
                    return Some(v);
                }
                current = st.resolved_base_type?;
            }
            TypeKey::Complex(_) => return None,
        }
    }
    None
}

/// Walk the complex type extension chain to find the effective simple content
/// type key. Returns `None` if there is no simple content type in the chain.
fn effective_simple_content_type_key(
    schema_set: &SchemaSet,
    type_def: &crate::arenas::ComplexTypeDefData,
) -> Option<TypeKey> {
    let mut current_base = type_def.resolved_base_type?;
    for _ in 0..50 {
        match current_base {
            TypeKey::Simple(sk) => return Some(TypeKey::Simple(sk)),
            TypeKey::Complex(ck) => {
                let ct = schema_set.arenas.complex_types.get(ck)?;
                current_base = ct.resolved_base_type?;
            }
        }
    }
    None
}

/// Validate simpleContent restriction inline simpleType.
///
/// derivation-ok-restriction clause 2.2.2.1 (§3.4.6.3): let S_B = B's content
/// type simple type definition and S_T = T's content type simple type definition.
/// S_T must be validly derived from S_B.
fn validate_simple_content_restriction(
    schema_set: &SchemaSet,
    derived: &crate::arenas::ComplexTypeDefData,
    base: &crate::arenas::ComplexTypeDefData,
) -> SchemaResult<()> {
    // Only applies when derived has simpleContent with an inline simpleType
    let ComplexContentResult::Simple(ref sc) = derived.content else {
        return Ok(());
    };

    let Some(ref inline_st) = sc.content_type else {
        return Ok(());
    };

    // Find the base type's effective simple content type
    let Some(base_simple_key) = effective_simple_content_type_key(schema_set, base) else {
        return Ok(());
    };

    // anySimpleType is the ur-type of all simple types — any variety
    // is a valid restriction.  Per §3.14.6 clause 2, inline list/union
    // types automatically derive from anySimpleType.  This function
    // only checks variety compatibility, not facets, so the early
    // return is safe.
    if let TypeKey::Simple(sk) = base_simple_key {
        if sk == schema_set.builtin_types().any_simple_type {
            return Ok(());
        }
    }

    // Get the base simple type's variety
    let base_variety = match base_simple_key {
        TypeKey::Simple(sk) => schema_set.arenas.simple_types.get(sk).map(|st| st.variety),
        TypeKey::Complex(_) => None,
    };

    let Some(base_variety) = base_variety else {
        return Ok(());
    };

    // Check variety compatibility:
    // A list type cannot restrict an atomic type.
    // A union type cannot restrict an atomic type (unless it's a restriction of
    // the base union/atomic via resolved_base_type chain).
    let derived_variety = inline_st.variety;

    if derived_variety != base_variety {
        // Different varieties — check if the inline type's base chain leads to
        // the base simple type (which would mean it's a valid restriction despite
        // variety difference, e.g. restriction of a union member).
        // For the common case (list restricting atomic, union restricting atomic),
        // this chain walk will NOT find the base type.
        if let Some(inline_resolved_base) = resolve_inline_simple_type_base(schema_set, inline_st) {
            if is_type_derived_from(schema_set, inline_resolved_base, base_simple_key) {
                return Ok(());
            }
        }

        let location = derived
            .source
            .as_ref()
            .and_then(|s| schema_set.source_maps.locate(s));
        let type_name = format_type_name(schema_set, derived.name, derived.target_namespace);
        let base_name = format_type_name(schema_set, base.name, base.target_namespace);
        return Err(SchemaError::structural(
            "derivation-ok-restriction",
            format!(
                "Complex type '{}' restricting '{}': simpleContent inline type \
                 has variety {:?} which is not a valid restriction of the base \
                 type's simple content (variety {:?})",
                type_name, base_name, derived_variety, base_variety,
            ),
            location,
        ));
    }

    Ok(())
}

/// Try to resolve the base type key of an inline SimpleTypeResult.
/// The inline type may have a base_type as a QName that has been resolved,
/// or it may reference a known type directly.
fn resolve_inline_simple_type_base(
    schema_set: &SchemaSet,
    inline_st: &crate::parser::frames::SimpleTypeResult,
) -> Option<TypeKey> {
    // For inline types used in simpleContent/restriction, the base_type
    // is the type the restriction derives from. If it was resolved during
    // assembly, it would be in the arena. We can try to find it by matching
    // the QName if present.
    match &inline_st.base_type {
        Some(crate::parser::frames::TypeRefResult::QName(qname)) => {
            schema_set.lookup_type(qname.namespace, qname.local_name)
        }
        _ => None,
    }
}

/// XSD 1.0 §3.2.6 constraint 2 (`cos-attribute-decl`): if an attribute's type
/// is or derives from xs:ID, it must not have a value constraint (default or
/// fixed). XSD 1.1 relaxes this restriction. Called from the pipeline after
/// reference resolution.
pub fn validate_attribute_id_constraints(schema_set: &SchemaSet) -> SchemaResult<()> {
    use crate::types::XmlTypeCode;

    if !schema_set.is_xsd10() {
        return Ok(());
    }

    let id_key = match schema_set.builtin_types().get_by_type_code(XmlTypeCode::Id) {
        Some(k) => k,
        None => return Ok(()),
    };

    for (_key, attr_data) in schema_set.arenas.attributes.iter() {
        if attr_data.default_value.is_none() && attr_data.fixed_value.is_none() {
            continue;
        }
        if let Some(TypeKey::Simple(st_key)) = attr_data.resolved_type {
            if schema_set.derives_from(st_key, id_key) {
                let attr_name = attr_data
                    .name
                    .map(|n| schema_set.name_table.resolve(n).to_string())
                    .unwrap_or_else(|| "(anonymous)".to_string());
                let constraint = if attr_data.default_value.is_some() {
                    "default"
                } else {
                    "fixed"
                };
                return Err(SchemaError::structural(
                    "cos-attribute-decl",
                    format!(
                        "Attribute '{}' has type xs:ID (or derived) and must not have a {} value constraint",
                        attr_name, constraint
                    ),
                    schema_set.locate(attr_data.source.as_ref()),
                ));
            }
        }
    }

    for (_key, ct_data) in schema_set.arenas.complex_types.iter() {
        for (i, attr_use) in ct_data.attributes.iter().enumerate() {
            if attr_use.use_kind == AttributeUseKind::Prohibited {
                continue;
            }
            let resolved = ct_data.resolved_attributes.get(i);
            let ref_decl = resolved
                .and_then(|r| r.resolved_ref)
                .and_then(|k| schema_set.arenas.attributes.get(k));

            let has_constraint = attr_use.attribute.default_value.is_some()
                || attr_use.attribute.fixed_value.is_some()
                || ref_decl.is_some_and(|d| d.default_value.is_some() || d.fixed_value.is_some());
            if !has_constraint {
                continue;
            }

            let attr_type = resolved
                .and_then(|r| r.resolved_type)
                .or_else(|| ref_decl.and_then(|d| d.resolved_type));
            if let Some(TypeKey::Simple(st_key)) = attr_type {
                if schema_set.derives_from(st_key, id_key) {
                    let attr_name = attr_use
                        .attribute
                        .name
                        .map(|n| schema_set.name_table.resolve(n).to_string())
                        .or_else(|| {
                            ref_decl
                                .and_then(|d| d.name)
                                .map(|n| schema_set.name_table.resolve(n).to_string())
                        })
                        .unwrap_or_else(|| "(anonymous)".to_string());
                    let constraint = if attr_use.attribute.default_value.is_some()
                        || ref_decl.and_then(|d| d.default_value.as_ref()).is_some()
                    {
                        "default"
                    } else {
                        "fixed"
                    };
                    let location = attr_use
                        .attribute
                        .source
                        .as_ref()
                        .or(ct_data.source.as_ref())
                        .and_then(|s| schema_set.source_maps.locate(s));
                    return Err(SchemaError::structural(
                        "cos-attribute-decl",
                        format!(
                            "Attribute '{}' has type xs:ID (or derived) and must not have a {} value constraint",
                            attr_name, constraint
                        ),
                        location,
                    ));
                }
            }
        }
    }

    Ok(())
}

/// `a-props-correct.3`: validate that attribute `default`/`fixed` values
/// are type-valid for the declared type.
///
/// Walks every globally-declared attribute and every attribute use inside
/// complex types and rejects when the value constraint cannot be parsed
/// against the attribute's declared simple type.
pub fn validate_attribute_value_constraints(schema_set: &SchemaSet) -> SchemaResult<()> {
    // Top-level attribute declarations
    for (_key, attr) in schema_set.arenas.attributes.iter() {
        if attr.source.is_none() {
            // Built-in xsi:* attributes have `source: None`; skip them.
            continue;
        }
        let (value, is_fixed) = match (&attr.default_value, &attr.fixed_value) {
            (Some(v), _) => (v.as_str(), false),
            (_, Some(v)) => (v.as_str(), true),
            (None, None) => continue,
        };
        let Some(type_key @ TypeKey::Simple(_)) = attr.resolved_type else {
            continue;
        };
        if crate::validation::simple::validate_simple_type(value, type_key, schema_set).is_err() {
            let attr_name = attr
                .name
                .map(|n| schema_set.name_table.resolve(n).to_string())
                .unwrap_or_else(|| "(anonymous)".to_string());
            let constraint = if is_fixed { "fixed" } else { "default" };
            return Err(SchemaError::structural(
                "a-props-correct",
                format!(
                    "Attribute '{}' {} value '{}' is not valid for its declared type",
                    attr_name, constraint, value
                ),
                schema_set.locate(attr.source.as_ref()),
            ));
        }
    }

    // Attribute uses inside complex types
    for (_key, ct) in schema_set.arenas.complex_types.iter() {
        for (i, attr_use) in ct.attributes.iter().enumerate() {
            if attr_use.use_kind == AttributeUseKind::Prohibited {
                continue;
            }
            let resolved = ct.resolved_attributes.get(i);
            let ref_decl = resolved
                .and_then(|r| r.resolved_ref)
                .and_then(|k| schema_set.arenas.attributes.get(k));

            // Use the local (override) value-constraint when present;
            // otherwise the global declaration's. au-props-correct.2 says
            // a use's fixed must equal the declaration's, but here we only
            // need to check that *some* effective value-constraint parses.
            let value_constraint: Option<(&str, bool)> = attr_use
                .attribute
                .fixed_value
                .as_deref()
                .map(|v| (v, true))
                .or_else(|| {
                    attr_use
                        .attribute
                        .default_value
                        .as_deref()
                        .map(|v| (v, false))
                })
                .or_else(|| {
                    ref_decl.and_then(|d| {
                        d.fixed_value
                            .as_deref()
                            .map(|v| (v, true))
                            .or_else(|| d.default_value.as_deref().map(|v| (v, false)))
                    })
                });
            let Some((value, is_fixed)) = value_constraint else {
                continue;
            };

            let attr_type = resolved
                .and_then(|r| r.resolved_type)
                .or_else(|| ref_decl.and_then(|d| d.resolved_type));
            let Some(type_key @ TypeKey::Simple(_)) = attr_type else {
                continue;
            };
            if crate::validation::simple::validate_simple_type(value, type_key, schema_set).is_err()
            {
                let attr_name = attr_use
                    .attribute
                    .name
                    .map(|n| schema_set.name_table.resolve(n).to_string())
                    .or_else(|| {
                        ref_decl
                            .and_then(|d| d.name)
                            .map(|n| schema_set.name_table.resolve(n).to_string())
                    })
                    .unwrap_or_else(|| "(anonymous)".to_string());
                let constraint = if is_fixed { "fixed" } else { "default" };
                let location = attr_use
                    .attribute
                    .source
                    .as_ref()
                    .or(ct.source.as_ref())
                    .and_then(|s| schema_set.source_maps.locate(s));
                return Err(SchemaError::structural(
                    "a-props-correct",
                    format!(
                        "Attribute '{}' {} value '{}' is not valid for its declared type",
                        attr_name, constraint, value
                    ),
                    location,
                ));
            }
        }
    }

    Ok(())
}

/// `e-props-correct.2` and `e-props-correct.4`: validate that element
/// `default`/`fixed` values are type-valid for the declared type.
///
/// - `e-props-correct.2`: the value must be valid for the element's type.
/// - `e-props-correct.4`: if the type is (or derives from) xs:ID, no value
///   constraint is allowed.
pub fn validate_element_value_constraints(schema_set: &SchemaSet) -> SchemaResult<()> {
    use crate::parser::frames::ComplexContentResult;
    use crate::types::XmlTypeCode;

    let id_key = schema_set.builtin_types().get_by_type_code(XmlTypeCode::Id);
    let any_type_key = TypeKey::Complex(schema_set.any_type_key());

    for (_key, elem) in schema_set.arenas.elements.iter() {
        let (value, is_fixed) = match (&elem.default_value, &elem.fixed_value) {
            (Some(v), _) => (v.as_str(), false),
            (_, Some(v)) => (v.as_str(), true),
            (None, None) => continue,
        };

        // Element refs inherit constraints from the referenced element
        if elem.resolved_ref.is_some() {
            continue;
        }

        let type_key = match elem.resolved_type {
            Some(tk) if tk != any_type_key => tk,
            _ => continue,
        };

        let elem_name = || {
            elem.name
                .map(|n| schema_set.name_table.resolve_ref(n))
                .unwrap_or("(anonymous)")
        };
        let location = || schema_set.locate(elem.source.as_ref());
        let constraint = if is_fixed { "fixed" } else { "default" };

        // src-element §3.3.3 clause 3.2: when an element declaration has a
        // `default` or `fixed` value, the type definition must be either a
        // simple type, a complex type with simple content, or a complex
        // type with mixed=true. Anything else (complex non-simple, non-mixed
        // content) is invalid.
        if let TypeKey::Complex(ct_key) = type_key {
            if let Some(ct) = schema_set.arenas.complex_types.get(ct_key) {
                let simple_content = matches!(ct.content, ComplexContentResult::Simple(_));
                if !simple_content && !ct.mixed {
                    return Err(SchemaError::structural(
                        "src-element",
                        format!(
                            "Element '{}' has '{}' value but its type is a complex type \
                             with non-mixed, non-simple content",
                            elem_name(),
                            constraint
                        ),
                        location(),
                    ));
                }
            }
        }

        // e-props-correct.4: xs:ID (or derived) cannot have a value constraint.
        // XSD 1.1 §3.3.6.1 removes this restriction (it has no analogous clause);
        // only apply in XSD 1.0 mode.
        if !schema_set.is_xsd11() {
            if let (Some(id_simple_key), TypeKey::Simple(st_key)) = (id_key, type_key) {
                if schema_set.derives_from(st_key, id_simple_key) {
                    return Err(SchemaError::structural(
                        "e-props-correct.4",
                        format!(
                            "Element '{}' has type xs:ID (or derived) and must not have a {} value constraint",
                            elem_name(), constraint
                        ),
                        location(),
                    ));
                }
            }
        }

        // e-props-correct.2: value must be valid for the declared type
        let effective_type = match type_key {
            TypeKey::Simple(_) => Some(type_key),
            TypeKey::Complex(ck) => schema_set
                .arenas
                .complex_types
                .get(ck)
                .and_then(|ct| effective_simple_content_type_key(schema_set, ct)),
        };

        if let Some(st_key) = effective_type {
            if crate::validation::simple::validate_simple_type(value, st_key, schema_set).is_err() {
                return Err(SchemaError::structural(
                    "e-props-correct.2",
                    format!(
                        "Element '{}' {} value '{}' is not valid for its declared type",
                        elem_name(),
                        constraint,
                        value
                    ),
                    location(),
                ));
            }
        }
    }

    Ok(())
}

/// XSD 1.1 §3.3.2 Schema Representation Constraint: Type Alternative
/// Representation OK (`src-type-alternative`).
///
/// Among an element's sequence of `<xs:alternative>` children, only the last
/// alternative is allowed to omit the `test` attribute (acting as a default
/// fallback). An alternative without `@test` in a non-final position is a
/// schema error.
#[cfg(feature = "xsd11")]
pub fn validate_element_type_alternatives(schema_set: &SchemaSet) -> SchemaResult<()> {
    if !schema_set.is_xsd11() {
        return Ok(());
    }
    for (_key, elem) in schema_set.arenas.elements.iter() {
        let alts = &elem.alternatives;
        if alts.len() < 2 {
            continue;
        }
        for alt in &alts[..alts.len() - 1] {
            if alt.test.is_none() {
                let name = elem
                    .name
                    .map(|n| schema_set.name_table.resolve_ref(n))
                    .unwrap_or("(anonymous)");
                let location = schema_set.locate(elem.source.as_ref());
                return Err(SchemaError::structural(
                    "src-type-alternative",
                    format!(
                        "Element '{}': <xs:alternative> without a 'test' attribute is only \
                         permitted as the last alternative",
                        name
                    ),
                    location,
                ));
            }
        }
    }
    Ok(())
}

/// `ct-props-correct.4` / `ag-props-correct.2`: every complex type's
/// effective `{attribute uses}` must contain at most one entry per
/// `(target_namespace, name)`. Two distinct attribute declarations with the
/// same expanded name are forbidden by both XSD 1.0 and XSD 1.1.
///
/// The check is keyed by *declaration identity*, not by `(name, namespace)`,
/// so reaching the same declaration along multiple paths (including XSD 1.1
/// circular attribute groups) does not produce a false positive — only
/// genuinely distinct declarations that happen to share an expanded name
/// are flagged. The W3C `attQ011` fixture exercises the cross-attribute-
/// group case where attribute "foo" appears once via a global
/// `<attribute ref="x:foo"/>` reference and once via a redefined
/// `<attributeGroup ref="x:red"/>` whose members include a local
/// `<attribute name="foo"/>`.
pub fn validate_complex_type_attribute_uniqueness(schema_set: &SchemaSet) -> SchemaResult<()> {
    use std::collections::{HashMap, HashSet};

    // Stable identity of an attribute declaration. Variants:
    // - `GlobalRef(k)`        — `<xs:attribute ref="...">` resolving to
    //                            global attribute key `k`.
    // - `InlineGroup(g, i)`   — i-th inline `<xs:attribute>` in
    //                            attribute group `g`.
    // - `InlineComplex(c, i)` — i-th inline `<xs:attribute>` in
    //                            complex type `c`.
    #[derive(Hash, PartialEq, Eq, Clone, Copy)]
    enum AttrDeclId {
        GlobalRef(AttributeKey),
        InlineGroup(AttributeGroupKey, usize),
        InlineComplex(ComplexTypeKey, usize),
    }

    fn walk_attribute_group(
        schema_set: &SchemaSet,
        ag_key: AttributeGroupKey,
        visiting_groups: &mut HashSet<AttributeGroupKey>,
        seen: &mut HashSet<AttrDeclId>,
        out: &mut Vec<EffectiveAttributeUse>,
    ) {
        if !visiting_groups.insert(ag_key) {
            return;
        }
        let Some(ag) = schema_set.arenas.attribute_groups.get(ag_key) else {
            visiting_groups.remove(&ag_key);
            return;
        };
        if let Some(ref_key) = ag.resolved_ref {
            walk_attribute_group(schema_set, ref_key, visiting_groups, seen, out);
            visiting_groups.remove(&ag_key);
            return;
        }

        for (i, attr_use) in ag.attributes.iter().enumerate() {
            let resolved = ag.resolved_attributes.get(i);
            let decl_id = if let Some(global_key) = resolved.and_then(|r| r.resolved_ref) {
                AttrDeclId::GlobalRef(global_key)
            } else {
                AttrDeclId::InlineGroup(ag_key, i)
            };
            if seen.insert(decl_id) {
                if let Some(eau) = resolve_single_attribute_use(schema_set, attr_use, resolved) {
                    out.push(eau);
                }
            }
        }
        for &nested in &ag.resolved_attribute_groups {
            walk_attribute_group(schema_set, nested, visiting_groups, seen, out);
        }

        visiting_groups.remove(&ag_key);
    }

    fn collect_with_dedup(
        schema_set: &SchemaSet,
        type_def: &crate::arenas::ComplexTypeDefData,
        ct_key: ComplexTypeKey,
        depth: usize,
        visiting_groups: &mut HashSet<AttributeGroupKey>,
        seen: &mut HashSet<AttrDeclId>,
        out: &mut Vec<EffectiveAttributeUse>,
    ) {
        if depth > 50 {
            return;
        }
        for (i, attr_use) in type_def.attributes.iter().enumerate() {
            let resolved = type_def.resolved_attributes.get(i);
            let decl_id = if let Some(global_key) = resolved.and_then(|r| r.resolved_ref) {
                AttrDeclId::GlobalRef(global_key)
            } else {
                AttrDeclId::InlineComplex(ct_key, i)
            };
            if seen.insert(decl_id) {
                if let Some(eau) = resolve_single_attribute_use(schema_set, attr_use, resolved) {
                    out.push(eau);
                }
            }
        }
        for &ag_key in &type_def.resolved_attribute_groups {
            visiting_groups.clear();
            walk_attribute_group(schema_set, ag_key, visiting_groups, seen, out);
        }
        if type_def.derivation_method == Some(DerivationMethod::Extension) {
            if let Some(TypeKey::Complex(base_key)) = type_def.resolved_base_type {
                if let Some(base) = schema_set.arenas.complex_types.get(base_key) {
                    collect_with_dedup(
                        schema_set,
                        base,
                        base_key,
                        depth + 1,
                        visiting_groups,
                        seen,
                        out,
                    );
                }
            }
        }
    }

    // Reusable scratch buffers, cleared per type to avoid per-iteration
    // allocator traffic on schemas with many complex types.
    let mut seen: HashSet<AttrDeclId> = HashSet::new();
    let mut attrs: Vec<EffectiveAttributeUse> = Vec::new();
    let mut visiting_groups: HashSet<AttributeGroupKey> = HashSet::new();
    let mut by_name: HashMap<(Option<NameId>, NameId), ()> = HashMap::new();

    // ct-props-correct clause 4 is XSD 1.0-only. Hoist the `xs:ID` builtin
    // lookup out of the per-complex-type loop; it's an arena-backed constant
    // for the lifetime of the schema set.
    let id_key_for_xsd10 = if schema_set.is_xsd10() {
        schema_set
            .builtin_types()
            .get_by_type_code(crate::types::XmlTypeCode::Id)
    } else {
        None
    };

    for (key, type_def) in schema_set.arenas.complex_types.iter() {
        seen.clear();
        attrs.clear();
        by_name.clear();
        collect_with_dedup(
            schema_set,
            type_def,
            key,
            0,
            &mut visiting_groups,
            &mut seen,
            &mut attrs,
        );

        // §3.4.6: a prohibited attribute use is NOT an entry in the
        // `{attribute uses}` set, so it cannot collide with a (re-)declared
        // use in a derived type.
        attrs.retain(|eau| eau.use_kind != AttributeUseKind::Prohibited);

        for attr in &attrs {
            if by_name
                .insert((attr.target_namespace, attr.name), ())
                .is_some()
            {
                let attr_name_str = schema_set.name_table.resolve(attr.name);
                let type_name =
                    format_type_name(schema_set, type_def.name, type_def.target_namespace);
                let location = type_def
                    .source
                    .as_ref()
                    .and_then(|s| schema_set.source_maps.locate(s));
                return Err(SchemaError::structural(
                    "ct-props-correct",
                    format!(
                        "Complex type '{}': two distinct attribute declarations \
                         with the same expanded name '{}' (ct-props-correct \
                         clause 4 / ag-props-correct clause 2)",
                        type_name, attr_name_str,
                    ),
                    location,
                ));
            }
        }

        // ct-props-correct clause 4 (XSD 1.0 only): "Two distinct members of
        // the {attribute uses} must not have {type definition}s which are
        // both `xs:ID` or are derived from `xs:ID`." XSD 1.1 dropped this
        // constraint — see saxon's id001 test which explicitly documents
        // that an XSD 1.1 type may declare multiple ID-typed attributes.
        if let Some(id_key) = id_key_for_xsd10 {
            let mut id_attrs = attrs.iter().filter(|attr| match attr.resolved_type {
                Some(TypeKey::Simple(st_key)) => schema_set.derives_from(st_key, id_key),
                _ => false,
            });
            if let (Some(first), Some(second)) = (id_attrs.next(), id_attrs.next()) {
                let first_name = schema_set.name_table.resolve(first.name);
                let second_name = schema_set.name_table.resolve(second.name);
                let type_name =
                    format_type_name(schema_set, type_def.name, type_def.target_namespace);
                let location = type_def
                    .source
                    .as_ref()
                    .and_then(|s| schema_set.source_maps.locate(s));
                return Err(SchemaError::structural(
                    "ct-props-correct",
                    format!(
                        "Complex type '{}': attributes '{}' and '{}' both have \
                         xs:ID-derived types (ct-props-correct clause 4; XSD 1.0 only)",
                        type_name, first_name, second_name,
                    ),
                    location,
                ));
            }
        }
    }
    Ok(())
}

/// XSD 1.1 §3.10.6.1 rule 4 (Wildcard Properties Correct): for every QName
/// member in a wildcard's `{disallowed names}`, that QName's namespace name
/// must be admitted by the wildcard's `{namespace constraint}` (the
/// combination of `namespace` and `notNamespace`).
///
/// In other words: the schema cannot list a notQName entry whose namespace
/// the wildcard already excludes — such an entry would be redundant and
/// the spec rules it out as a structural error. Covers W3C saxonData
/// `wild031`..`wild035` (and is the post-parse step that backs the
/// per-entry checks `parse_not_qname` performs at parse time).
///
/// The check needs the resolved target namespace of the wildcard's owner
/// to interpret `##targetNamespace`/`##other`/`##local`, which is only
/// available after assembly — hence this is a separate pipeline pass
/// rather than something `parse_not_qname` can do on its own.
#[cfg(feature = "xsd11")]
pub fn validate_wildcard_disallowed_names(schema_set: &SchemaSet) -> SchemaResult<()> {
    if !schema_set.is_xsd11() {
        return Ok(());
    }

    fn check_wildcard(
        schema_set: &SchemaSet,
        wc: &WildcardResult,
        target_ns: Option<NameId>,
    ) -> SchemaResult<()> {
        use crate::parser::frames::NotQNameItem;

        for item in &wc.not_qname {
            let NotQNameItem::QName {
                namespace: q_ns,
                local_name,
            } = item
            else {
                continue;
            };
            // The QName must satisfy the wildcard's namespace constraint
            // (cvc-wildcard-namespace §3.10.4.3): it must be admitted by
            // the positive constraint AND not be excluded by notNamespace.
            let admitted_by_constraint =
                wildcard_namespace_matches(&wc.namespace, *q_ns, target_ns);
            let excluded_by_not_namespace = wc
                .not_namespace
                .iter()
                .any(|t| t.resolve(target_ns) == *q_ns);
            if !admitted_by_constraint || excluded_by_not_namespace {
                let location = schema_set.locate(wc.source.as_ref());
                let qname_text = match q_ns {
                    Some(ns) => format!(
                        "{{{}}}:{}",
                        schema_set.name_table.resolve_ref(*ns),
                        schema_set.name_table.resolve_ref(*local_name),
                    ),
                    None => schema_set.name_table.resolve_ref(*local_name).to_string(),
                };
                return Err(SchemaError::structural(
                    "w-props-correct",
                    format!(
                        "notQName entry '{}' is not admitted by the wildcard's \
                         namespace constraint (§3.10.6.1 rule 4)",
                        qname_text
                    ),
                    location,
                ));
            }
        }
        Ok(())
    }

    fn check_particle(
        schema_set: &SchemaSet,
        particle: &ParticleResult,
        target_ns: Option<NameId>,
        depth: usize,
    ) -> SchemaResult<()> {
        if depth > 100 {
            return Ok(());
        }
        match &particle.term {
            ParticleTerm::Any(wc) => check_wildcard(schema_set, wc, target_ns)?,
            ParticleTerm::Group(group) => {
                for child in &group.particles {
                    check_particle(schema_set, child, target_ns, depth + 1)?;
                }
            }
            ParticleTerm::Element(_) => {}
        }
        Ok(())
    }

    // Complex types: own attribute_wildcard + content particles + any
    // attribute_wildcard hiding inside the SimpleContent / ComplexContent
    // derivation defs.
    for (_key, ct) in schema_set.arenas.complex_types.iter() {
        let target_ns = ct.target_namespace;
        if let Some(wc) = ct.attribute_wildcard.as_ref() {
            check_wildcard(schema_set, wc, target_ns)?;
        }
        match &ct.content {
            ComplexContentResult::Empty => {}
            ComplexContentResult::Simple(sc) => {
                if let Some(wc) = sc.attribute_wildcard.as_ref() {
                    check_wildcard(schema_set, wc, target_ns)?;
                }
            }
            ComplexContentResult::Complex(cc) => {
                if let Some(wc) = cc.attribute_wildcard.as_ref() {
                    check_wildcard(schema_set, wc, target_ns)?;
                }
                if let Some(p) = cc.particle.as_ref() {
                    check_particle(schema_set, p, target_ns, 0)?;
                }
                if let Some(oc) = cc.open_content.as_ref() {
                    if let Some(wc) = oc.wildcard.as_ref() {
                        check_wildcard(schema_set, wc, target_ns)?;
                    }
                }
            }
        }
        if let Some(oc) = ct.open_content.as_ref() {
            if let Some(wc) = oc.wildcard.as_ref() {
                check_wildcard(schema_set, wc, target_ns)?;
            }
        }
    }

    // Attribute groups: own attribute_wildcard.
    for (_key, ag) in schema_set.arenas.attribute_groups.iter() {
        if let Some(wc) = ag.attribute_wildcard.as_ref() {
            check_wildcard(schema_set, wc, ag.target_namespace)?;
        }
    }

    // Model-group definitions: walk content particles for element wildcards.
    for (_key, mg) in schema_set.arenas.model_groups.iter() {
        for child in &mg.particles {
            check_particle(schema_set, child, mg.target_namespace, 0)?;
        }
    }

    Ok(())
}

/// XSD 1.1 §3.8.6.3 / cos-element-consistent (second clause): when a complex
/// type's content model contains both a local element declaration with
/// expanded name Q AND a strict/lax wildcard that admits Q, AND a top-level
/// element declaration G with expanded name Q exists, then the type tables
/// of the local element and G must be either both absent or both present
/// and equivalent.
///
/// Closes wild078/079 (local has no type table, global has one) and wild081
/// (local has a type table, global doesn't).
#[cfg(feature = "xsd11")]
pub fn validate_wildcard_element_type_table_consistency(
    schema_set: &SchemaSet,
) -> SchemaResult<()> {
    use crate::parser::frames::AlternativeResult;

    if !schema_set.is_xsd11() {
        return Ok(());
    }

    /// Walk a particle tree using the parallel `local_keys`/`flat_idx` scheme
    /// from `allocate_content_particle_elements`. For each local element
    /// particle, look up its allocated arena key (where post-resolution
    /// alternatives live) instead of relying on the stale parser-frame copy.
    #[allow(clippy::too_many_arguments)]
    fn walk_collect<'a>(
        particle: &'a ParticleResult,
        target_ns: Option<NameId>,
        schema_set: &'a SchemaSet,
        local_keys: &[Option<ElementKey>],
        flat_idx: &mut usize,
        local_elems: &mut Vec<(Option<NameId>, NameId, ElementKey, Option<SourceRef>)>,
        wildcards: &mut Vec<&'a WildcardResult>,
        depth: usize,
    ) {
        if depth > 100 {
            return;
        }
        match &particle.term {
            ParticleTerm::Element(elem) => {
                if let Some(ref_qn) = &elem.ref_name {
                    // ref slot: increment flat_idx but no local key.
                    *flat_idx += 1;
                    let _ = ref_qn;
                } else if let Some(name) = elem.name {
                    let ns = elem.target_namespace.or(target_ns);
                    let idx = *flat_idx;
                    *flat_idx += 1;
                    if let Some(key) = local_keys.get(idx).copied().flatten() {
                        local_elems.push((ns, name, key, elem.source.clone()));
                    }
                }
            }
            ParticleTerm::Any(wc) => {
                wildcards.push(wc);
            }
            ParticleTerm::Group(group) => {
                if let Some(ref_qn) = &group.ref_name {
                    if let Some(group_key) =
                        schema_set.lookup_model_group(ref_qn.namespace, ref_qn.local_name)
                    {
                        let mg = &schema_set.arenas.model_groups[group_key];
                        let mg_ns = mg.target_namespace.or(target_ns);
                        let mut group_flat_idx = 0usize;
                        for child in &mg.particles {
                            walk_collect(
                                child,
                                mg_ns,
                                schema_set,
                                &mg.resolved_particle_elements,
                                &mut group_flat_idx,
                                local_elems,
                                wildcards,
                                depth + 1,
                            );
                        }
                    }
                    // Group refs do not advance the outer flat_idx (mirrors
                    // collect_content_particle_elements_recursive).
                } else {
                    for child in &group.particles {
                        walk_collect(
                            child,
                            target_ns,
                            schema_set,
                            local_keys,
                            flat_idx,
                            local_elems,
                            wildcards,
                            depth + 1,
                        );
                    }
                }
            }
        }
    }

    /// Resolve an alternative's effective type — fall back to looking up the
    /// QName via `schema_set.lookup_type` when the parser-frame copy hasn't
    /// been resolved yet (which is the case for local element alternatives
    /// allocated post-`resolve_all_references`).
    fn alt_effective_type(alt: &AlternativeResult, schema_set: &SchemaSet) -> Option<TypeKey> {
        use crate::parser::frames::TypeRefResult;
        if let Some(t) = alt.resolved_type {
            return Some(t);
        }
        if let Some(TypeRefResult::QName(qname)) = &alt.type_ref {
            return schema_set
                .lookup_type(qname.namespace, qname.local_name)
                .or_else(|| {
                    schema_set.get_built_in_type_by_qname(qname.namespace, qname.local_name)
                });
        }
        None
    }

    fn alternatives_equivalent(
        a: &[AlternativeResult],
        b: &[AlternativeResult],
        schema_set: &SchemaSet,
    ) -> bool {
        if a.len() != b.len() {
            return false;
        }
        for (x, y) in a.iter().zip(b.iter()) {
            if x.test != y.test {
                return false;
            }
            if alt_effective_type(x, schema_set) != alt_effective_type(y, schema_set) {
                return false;
            }
        }
        true
    }

    for (_key, ct) in schema_set.arenas.complex_types.iter() {
        let target_ns = ct.target_namespace;
        let ComplexContentResult::Complex(cc) = &ct.content else {
            continue;
        };
        let Some(particle) = cc.particle.as_ref() else {
            continue;
        };

        let mut local_elems: Vec<(Option<NameId>, NameId, ElementKey, Option<SourceRef>)> =
            Vec::new();
        let mut wildcards: Vec<&WildcardResult> = Vec::new();
        let mut flat_idx = 0usize;
        walk_collect(
            particle,
            target_ns,
            schema_set,
            &ct.resolved_content_particle_elements,
            &mut flat_idx,
            &mut local_elems,
            &mut wildcards,
            0,
        );

        // Open-content wildcards also count.
        if let Some(oc) = cc.open_content.as_ref() {
            if let Some(wc) = oc.wildcard.as_ref() {
                wildcards.push(wc);
            }
        }

        if wildcards.is_empty() {
            continue;
        }

        for (l_ns, l_name, l_key, l_source) in &local_elems {
            let global_key = schema_set.lookup_element(*l_ns, *l_name);
            let Some(g_key) = global_key else {
                continue;
            };
            // If the local declaration is the global itself (e.g., a ref'd
            // element resolving back to the same arena key), skip — there's
            // only one declaration so no inconsistency is possible.
            if *l_key == g_key {
                continue;
            }
            let l_decl = &schema_set.arenas.elements[*l_key];
            let g_decl = &schema_set.arenas.elements[g_key];

            // The wildcard must be lax/strict (skip wildcards bypass the EDC
            // per wild080) AND admit (l_ns, l_name).
            let admitted = wildcards.iter().any(|wc| {
                if matches!(wc.process_contents, ProcessContents::Skip) {
                    return false;
                }
                wildcard_result_admits_qname(wc, target_ns, *l_ns, *l_name)
            });
            if !admitted {
                continue;
            }

            if !alternatives_equivalent(&l_decl.alternatives, &g_decl.alternatives, schema_set) {
                let location = schema_set
                    .locate(l_source.as_ref())
                    .or_else(|| schema_set.locate(ct.source.as_ref()));
                let qname = match l_ns {
                    Some(ns) => format!(
                        "{{{}}}:{}",
                        schema_set.name_table.resolve_ref(*ns),
                        schema_set.name_table.resolve_ref(*l_name),
                    ),
                    None => schema_set.name_table.resolve_ref(*l_name).to_string(),
                };
                return Err(SchemaError::structural(
                    "cos-element-consistent",
                    format!(
                        "Local element '{}' is in the same content model as a strict/lax \
                         wildcard that admits its expanded name; the local element's type \
                         table is not equivalent to that of the top-level declaration of \
                         the same name (§3.8.6.3 / cos-element-consistent)",
                        qname
                    ),
                    location,
                ));
            }
        }
    }

    Ok(())
}

// Shared helpers for §3.8.6.3 / §3.4.6.3 cos-element-consistent.

/// Per-local-element record produced by [`collect_local_elements`].
#[cfg(feature = "xsd11")]
type LocalElementEntry = (Option<NameId>, NameId, ElementKey, Option<SourceRef>);

/// Recursively walk a complex type's content model and emit one
/// `LocalElementEntry` per inline local element declaration.
///
/// `flat_idx` tracks the walker's position in the owning CT's
/// `resolved_content_particle_elements`, which is the post-resolution
/// arena lookup that carries CTA alternatives (the parser-frame
/// `AlternativeResult` slice on the `ElementParticle` is stale for
/// inline alternatives resolved later).
#[cfg(feature = "xsd11")]
fn walk_collect_local_elements(
    particle: &ParticleResult,
    target_ns: Option<NameId>,
    local_keys: &[Option<ElementKey>],
    flat_idx: &mut usize,
    out: &mut Vec<LocalElementEntry>,
) {
    if let ParticleTerm::Group(group) = &particle.term {
        walk_group_local_elements(&group.particles, target_ns, local_keys, flat_idx, out);
    }
}

#[cfg(feature = "xsd11")]
fn walk_group_local_elements(
    particles: &[ParticleResult],
    target_ns: Option<NameId>,
    local_keys: &[Option<ElementKey>],
    flat_idx: &mut usize,
    out: &mut Vec<LocalElementEntry>,
) {
    for p in particles {
        match &p.term {
            ParticleTerm::Element(elem) if elem.ref_name.is_none() => {
                if let Some(Some(elem_key)) = local_keys.get(*flat_idx) {
                    let ns = elem.target_namespace.or(target_ns);
                    if let Some(name) = elem.name {
                        out.push((ns, name, *elem_key, elem.source.clone()));
                    }
                }
                *flat_idx += 1;
            }
            ParticleTerm::Element(_) => {
                *flat_idx += 1;
            }
            ParticleTerm::Group(group) if group.ref_name.is_none() => {
                walk_group_local_elements(&group.particles, target_ns, local_keys, flat_idx, out);
            }
            _ => {}
        }
    }
}

/// Collect every local element declaration in `ct`'s content model.
#[cfg(feature = "xsd11")]
fn collect_local_elements(ct: &crate::arenas::ComplexTypeDefData) -> Vec<LocalElementEntry> {
    let mut out = Vec::new();
    let ComplexContentResult::Complex(cc) = &ct.content else {
        return out;
    };
    let Some(particle) = cc.particle.as_ref() else {
        return out;
    };
    let mut flat_idx = 0usize;
    walk_collect_local_elements(
        particle,
        ct.target_namespace,
        &ct.resolved_content_particle_elements,
        &mut flat_idx,
        &mut out,
    );
    out
}

/// Resolve an alternative's effective `TypeKey`, falling back to the
/// schema-set's name lookup or built-in registry when the parser-frame
/// `resolved_type` is absent.
#[cfg(feature = "xsd11")]
fn alt_effective_type(
    alt: &crate::parser::frames::AlternativeResult,
    schema_set: &SchemaSet,
) -> Option<TypeKey> {
    use crate::parser::frames::TypeRefResult;
    if let Some(t) = alt.resolved_type {
        return Some(t);
    }
    if let Some(TypeRefResult::QName(qname)) = &alt.type_ref {
        return schema_set
            .lookup_type(qname.namespace, qname.local_name)
            .or_else(|| schema_set.get_built_in_type_by_qname(qname.namespace, qname.local_name));
    }
    None
}

/// Two alternative lists are equivalent when they have the same length,
/// pairwise-equal `@test` strings, and pairwise-equal effective types.
#[cfg(feature = "xsd11")]
fn alternatives_equivalent(
    a: &[crate::parser::frames::AlternativeResult],
    b: &[crate::parser::frames::AlternativeResult],
    schema_set: &SchemaSet,
) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b.iter()).all(|(x, y)| {
        x.test == y.test && alt_effective_type(x, schema_set) == alt_effective_type(y, schema_set)
    })
}

/// XSD 1.1 §3.8.6.3 / cos-element-consistent: two local element declarations
/// with the same expanded QName in the same complex-type content model must
/// have equivalent type tables (or both have no type table).
#[cfg(feature = "xsd11")]
pub fn validate_local_element_type_table_consistency(schema_set: &SchemaSet) -> SchemaResult<()> {
    use std::collections::HashMap;

    if !schema_set.is_xsd11() {
        return Ok(());
    }

    for (_key, ct) in schema_set.arenas.complex_types.iter() {
        let local_elems = collect_local_elements(ct);
        if local_elems.len() < 2 {
            continue;
        }

        let mut by_name: HashMap<(Option<NameId>, NameId), Vec<usize>> = HashMap::new();
        for (i, (ns, name, _, _)) in local_elems.iter().enumerate() {
            by_name.entry((*ns, *name)).or_default().push(i);
        }

        for (qname, indices) in &by_name {
            if indices.len() < 2 {
                continue;
            }
            let first_idx = indices[0];
            let first_decl = &schema_set.arenas.elements[local_elems[first_idx].2];
            for &idx in &indices[1..] {
                let other_decl = &schema_set.arenas.elements[local_elems[idx].2];
                if alternatives_equivalent(
                    &first_decl.alternatives,
                    &other_decl.alternatives,
                    schema_set,
                ) {
                    continue;
                }
                let qn_str = match qname.0 {
                    Some(ns) => format!(
                        "{{{}}}{}",
                        schema_set.name_table.resolve_ref(ns),
                        schema_set.name_table.resolve_ref(qname.1),
                    ),
                    None => schema_set.name_table.resolve_ref(qname.1).to_string(),
                };
                let location = schema_set
                    .locate(local_elems[idx].3.as_ref())
                    .or_else(|| schema_set.locate(ct.source.as_ref()));
                return Err(SchemaError::structural(
                    "cos-element-consistent",
                    format!(
                        "Two local element declarations of '{}' appear in the same \
                         content model but their type tables are not equivalent \
                         (§3.8.6.3 / cos-element-consistent)",
                        qn_str
                    ),
                    location,
                ));
            }
        }
    }

    Ok(())
}

/// XSD 1.1 §3.4.6.3 / cos-element-consistent (cross-derivation): when a
/// complex type T restricts a base type B and both contain local element
/// declarations with the same expanded QName, the type tables of those
/// declarations must be equivalent (or both absent).
///
/// Complements `validate_local_element_type_table_consistency`, which
/// catches duplicates *within* one content model.
#[cfg(feature = "xsd11")]
pub fn validate_restriction_local_element_type_table_consistency(
    schema_set: &SchemaSet,
) -> SchemaResult<()> {
    use std::collections::HashMap;

    if !schema_set.is_xsd11() {
        return Ok(());
    }

    for (_key, ct) in schema_set.arenas.complex_types.iter() {
        // §3.4.6.2 (extension) only adds particles and never re-issues
        // them, so type-table consistency is automatic there. Restrict to
        // §3.4.6.3 (restriction).
        if ct.derivation_method != Some(crate::parser::frames::DerivationMethod::Restriction) {
            continue;
        }
        let Some(TypeKey::Complex(base_ck)) = ct.resolved_base_type else {
            continue;
        };
        let Some(base_ct) = schema_set.arenas.complex_types.get(base_ck) else {
            continue;
        };

        let derived_locals = collect_local_elements(ct);
        if derived_locals.is_empty() {
            continue;
        }
        let base_locals = collect_local_elements(base_ct);
        if base_locals.is_empty() {
            continue;
        }

        // Multiple base locals with the same name is itself invalid; the
        // in-CT consistency pass will reject the base independently.
        let mut base_by_name: HashMap<(Option<NameId>, NameId), ElementKey> = HashMap::new();
        for (ns, name, ek, _) in &base_locals {
            base_by_name.entry((*ns, *name)).or_insert(*ek);
        }

        for (ns, name, derived_ek, derived_src) in &derived_locals {
            let Some(&base_ek) = base_by_name.get(&(*ns, *name)) else {
                continue;
            };
            let derived_decl = &schema_set.arenas.elements[*derived_ek];
            let base_decl = &schema_set.arenas.elements[base_ek];
            if alternatives_equivalent(
                &derived_decl.alternatives,
                &base_decl.alternatives,
                schema_set,
            ) {
                continue;
            }
            let qn_str = match ns {
                Some(ns_id) => format!(
                    "{{{}}}{}",
                    schema_set.name_table.resolve_ref(*ns_id),
                    schema_set.name_table.resolve_ref(*name),
                ),
                None => schema_set.name_table.resolve_ref(*name).to_string(),
            };
            let location = schema_set
                .locate(derived_src.as_ref())
                .or_else(|| schema_set.locate(ct.source.as_ref()));
            let derived_name = format_type_name(schema_set, ct.name, ct.target_namespace);
            let base_name = format_type_name(schema_set, base_ct.name, base_ct.target_namespace);
            return Err(SchemaError::structural(
                "cos-element-consistent",
                format!(
                    "Complex type '{}' restricting '{}': local element '{}' has a \
                     type table that is not equivalent to the base type's local \
                     element of the same name (§3.4.6.3 / cos-element-consistent)",
                    derived_name, base_name, qn_str,
                ),
                location,
            ));
        }
    }

    Ok(())
}

/// Whether the wildcard's namespace constraint and notQName admit the QName
/// `(ns, name)`. Treats `##defined` and `##definedSibling` pessimistically
/// (rejects).
#[cfg(feature = "xsd11")]
fn wildcard_result_admits_qname(
    wc: &WildcardResult,
    target_ns: Option<NameId>,
    ns: Option<NameId>,
    name: NameId,
) -> bool {
    use crate::parser::frames::NotQNameItem;
    if !wildcard_namespace_matches(&wc.namespace, ns, target_ns) {
        return false;
    }
    if wc.not_namespace.iter().any(|t| t.resolve(target_ns) == ns) {
        return false;
    }
    !wc.not_qname.iter().any(|item| match item {
        NotQNameItem::QName {
            namespace,
            local_name,
        } => *namespace == ns && *local_name == name,
        NotQNameItem::Defined | NotQNameItem::DefinedSibling => true,
    })
}

/// XSD 1.0 §3.2.17 lexical check for `xs:anyURI` source attributes on
/// `xs:appinfo` and `xs:documentation`. The W3C `anyURI_a001_1336` fixture
/// places `source="9999...anyURI:"` and `source="1111...http://foo/bar"`
/// on annotations of an element declaration; both have a colon whose
/// scheme prefix starts with a digit, which is invalid per RFC 2396.
/// XSD 1.1 explicitly relaxed the rule, so this validator is XSD 1.0-only.
///
/// We deliberately scope the check to annotation `source` attributes:
///   - directives' `schemaLocation` values like `"0"` and `"123"` are
///     valid relative URIs per RFC 2396 and survive any reasonable
///     strict lexer;
///   - the same goes for `xs:notation/@public`/`@system` and
///     `xs:anyAttribute/@namespace` numeric values in the same fixture.
///
/// The annotation source values are the only unambiguously-malformed
/// anyURIs in the fixture, and they alone are sufficient to make the
/// schema fail per the W3C "one or more invalid anyURIs" ruling.
pub fn validate_xsd10_annotation_source_anyuri(schema_set: &SchemaSet) -> SchemaResult<()> {
    use crate::schema::annotation::{Annotation, AnnotationItem};
    use crate::types::validators::is_strict_xsd10_anyuri;

    if !schema_set.is_xsd10() {
        return Ok(());
    }

    fn check_annotation(
        schema_set: &SchemaSet,
        annotation: Option<&Annotation>,
    ) -> SchemaResult<()> {
        let Some(annotation) = annotation else {
            return Ok(());
        };
        for item in &annotation.items {
            match item {
                AnnotationItem::AppInfo(ai) => {
                    if let Some(ref src) = ai.source {
                        if !is_strict_xsd10_anyuri(src) {
                            let location = ai
                                .source_ref
                                .as_ref()
                                .and_then(|s| schema_set.source_maps.locate(s));
                            return Err(SchemaError::structural(
                                "cvc-datatype-valid",
                                format!(
                                    "<xs:appinfo source=\"{}\"> is not a valid xs:anyURI \
                                     (XSD 1.0 strict scheme syntax)",
                                    src
                                ),
                                location,
                            ));
                        }
                    }
                }
                AnnotationItem::Documentation(d) => {
                    if let Some(ref src) = d.source {
                        if !is_strict_xsd10_anyuri(src) {
                            let location = d
                                .source_ref
                                .as_ref()
                                .and_then(|s| schema_set.source_maps.locate(s));
                            return Err(SchemaError::structural(
                                "cvc-datatype-valid",
                                format!(
                                    "<xs:documentation source=\"{}\"> is not a valid \
                                     xs:anyURI (XSD 1.0 strict scheme syntax)",
                                    src
                                ),
                                location,
                            ));
                        }
                    }
                }
            }
        }
        Ok(())
    }

    // Walk every arena that can carry an annotation. NOTE: anyone adding a
    // new annotatable arena type must extend this list.
    for (_k, ct) in schema_set.arenas.complex_types.iter() {
        check_annotation(schema_set, ct.annotation.as_ref())?;
    }
    for (_k, st) in schema_set.arenas.simple_types.iter() {
        check_annotation(schema_set, st.annotation.as_ref())?;
    }
    for (_k, el) in schema_set.arenas.elements.iter() {
        check_annotation(schema_set, el.annotation.as_ref())?;
    }
    for (_k, at) in schema_set.arenas.attributes.iter() {
        check_annotation(schema_set, at.annotation.as_ref())?;
    }
    for (_k, ag) in schema_set.arenas.attribute_groups.iter() {
        check_annotation(schema_set, ag.annotation.as_ref())?;
    }
    for (_k, mg) in schema_set.arenas.model_groups.iter() {
        check_annotation(schema_set, mg.annotation.as_ref())?;
    }
    for (_k, n) in schema_set.arenas.notations.iter() {
        check_annotation(schema_set, n.annotation.as_ref())?;
    }
    for (_k, ic) in schema_set.arenas.identity_constraints.iter() {
        check_annotation(schema_set, ic.annotation.as_ref())?;
    }
    // Schema-level top-level `<xs:annotation>` elements (a schema can hold
    // several, hence `Vec<Annotation>` rather than `Option<Annotation>`).
    for doc in &schema_set.documents {
        for ann in &doc.annotations {
            check_annotation(schema_set, Some(ann))?;
        }
    }
    Ok(())
}

/// Validate `xsi:` Not Allowed (§3.2.6.4 / `no-xsi`): the `{target namespace}`
/// of a *user-declared* attribute must not match the XML Schema instance
/// namespace. The four pre-defined XSI attributes (`type`, `nil`,
/// `schemaLocation`, `noNamespaceSchemaLocation`) are seeded into the
/// attributes arena with `source: None`; that absence is the marker we use
/// to skip them.
pub fn validate_no_xsi_attribute_declarations(schema_set: &SchemaSet) -> SchemaResult<()> {
    use crate::namespace::table::well_known;

    for (_key, attr) in schema_set.arenas.attributes.iter() {
        if attr.source.is_none() {
            // Built-in xsi:type / xsi:nil / xsi:schemaLocation /
            // xsi:noNamespaceSchemaLocation are seeded in
            // `types::builtin::initialize_xsi_attributes` with no source.
            continue;
        }
        let Some(ns) = attr.target_namespace else {
            continue;
        };
        if ns != well_known::XSI_NAMESPACE {
            continue;
        }
        let attr_name = attr
            .name
            .map(|n| schema_set.name_table.resolve_ref(n).to_string())
            .unwrap_or_else(|| "(anonymous)".to_string());
        let location = schema_set.locate(attr.source.as_ref());
        return Err(SchemaError::structural(
            "no-xsi",
            format!(
                "Attribute declaration '{}' has target namespace \
                 'http://www.w3.org/2001/XMLSchema-instance', which is \
                 reserved (no-xsi, §3.2.6.4)",
                attr_name
            ),
            location,
        ));
    }
    Ok(())
}

/// Validate src-element §3.3.3 clause 4.3 / src-attribute §3.2.3 clause 6.3:
/// a local `<element>` or `<attribute>` declaring an explicit
/// `targetNamespace` attribute that differs from the schema's own
/// `targetNamespace` is permitted only when there is a `<restriction>`
/// ancestor (between the local declaration and its nearest `<complexType>`
/// ancestor) whose base does not match `xs:anyType`.
///
/// Implementation: per complex type, treat the declaration's "nearest
/// `<complexType>` ancestor" as this complex type. The clause is satisfied
/// iff the type's `derivation_method` is `Restriction` and its
/// `resolved_base_type` is not `xs:anyType`. In every other case (extension,
/// no derivation, or restriction of `xs:anyType`), any local element /
/// attribute carrying a divergent `targetNamespace` is invalid.
///
/// Closes saxon `target002` (element case) and `target004` (attribute case);
/// `target001`/`target003` (the matching `valid` cases) keep passing because
/// they use `restriction` of a non-`anyType` base.
pub fn validate_local_decl_target_namespace(schema_set: &SchemaSet) -> SchemaResult<()> {
    use crate::parser::frames::{ComplexContentResult, ParticleResult, ParticleTerm};

    fn find_divergent_local_element<'a>(
        schema_set: &'a SchemaSet,
        particle: &'a ParticleResult,
        schema_tns: Option<NameId>,
        depth: usize,
    ) -> Option<(Option<SourceRef>, String)> {
        if depth > 100 {
            return None;
        }
        match &particle.term {
            ParticleTerm::Element(elem) => {
                if elem.ref_name.is_some() {
                    return None;
                }
                if let Some(ns) = elem.target_namespace {
                    if Some(ns) != schema_tns {
                        let name_str = elem
                            .name
                            .map(|n| schema_set.name_table.resolve_ref(n).to_string())
                            .unwrap_or_default();
                        return Some((elem.source.clone(), name_str));
                    }
                }
            }
            ParticleTerm::Group(group) => {
                // Only descend into inline groups (no ref_name); a group ref
                // points at a top-level group whose own decls are validated
                // independently when their containing context is examined.
                if group.ref_name.is_none() {
                    for child in &group.particles {
                        if let Some(found) =
                            find_divergent_local_element(schema_set, child, schema_tns, depth + 1)
                        {
                            return Some(found);
                        }
                    }
                }
            }
            ParticleTerm::Any(_) => {}
        }
        None
    }

    for (_, ct) in schema_set.arenas.complex_types.iter() {
        let schema_tns = ct.target_namespace;
        let restriction_of_non_any = match (ct.derivation_method, ct.resolved_base_type) {
            (Some(crate::parser::frames::DerivationMethod::Restriction), Some(base_key)) => {
                !schema_set.is_any_type(base_key)
            }
            _ => false,
        };
        if restriction_of_non_any {
            continue;
        }

        // Walk content particles for local elements with divergent
        // targetNamespace.
        let particle_opt = match &ct.content {
            ComplexContentResult::Complex(def) => def.particle.as_ref(),
            _ => None,
        };
        if let Some(particle) = particle_opt {
            if let Some((src, name)) =
                find_divergent_local_element(schema_set, particle, schema_tns, 0)
            {
                let location = schema_set
                    .locate(src.as_ref())
                    .or_else(|| schema_set.locate(ct.source.as_ref()));
                return Err(SchemaError::structural(
                    "src-element",
                    format!(
                        "Local element '{}' has an explicit targetNamespace differing from the \
                         schema's, but is not inside a <restriction> of a non-anyType base \
                         (src-element §3.3.3 clause 4.3)",
                        name
                    ),
                    location,
                ));
            }
        }

        // Check direct attribute uses for divergent targetNamespace.
        for au in &ct.attributes {
            let attr = &au.attribute;
            if attr.ref_name.is_some() {
                continue;
            }
            let Some(ns) = attr.target_namespace else {
                continue;
            };
            if Some(ns) == schema_tns {
                continue;
            }
            let name = attr
                .name
                .map(|n| schema_set.name_table.resolve_ref(n).to_string())
                .unwrap_or_default();
            let location = schema_set
                .locate(attr.source.as_ref())
                .or_else(|| schema_set.locate(ct.source.as_ref()));
            return Err(SchemaError::structural(
                "src-attribute",
                format!(
                    "Local attribute '{}' has an explicit targetNamespace differing from the \
                     schema's, but is not inside a <restriction> of a non-anyType base \
                     (src-attribute §3.2.3 clause 6.3)",
                    name
                ),
                location,
            ));
        }
    }
    Ok(())
}

/// Validate cos-element-consistent (§3.8.6.3) for the substitution-group
/// case: a content model that contains both a local element with QName Q
/// AND an element ref whose substitution-group expansion includes another
/// declaration with the same QName Q must agree on `{type definition}`.
///
/// The base XSD 1.1 EDC machinery (`validate_local_element_type_table_*`)
/// only compares type *tables*; this pass also covers the type-definition
/// rule that makes saxon `subsgroup901.bad.xsd` invalid (a CT containing
/// local `n: xs:date` plus `<xs:element ref="appendixContent">`, where the
/// global `n: xs:string` substitutes for `appendixContent`).
///
/// Active for both XSD 1.0 and 1.1.
pub fn validate_substitution_group_element_consistency(schema_set: &SchemaSet) -> SchemaResult<()> {
    use crate::parser::frames::{ComplexContentResult, ParticleResult, ParticleTerm};
    use std::collections::HashMap;

    type Entry = (TypeKey, Option<SourceRef>);

    // head → direct substitution members. Built once so the per-ref expansion
    // is O(direct members) instead of an O(elements) arena scan per ref.
    let mut subst_index: HashMap<ElementKey, Vec<ElementKey>> = HashMap::new();
    for (mk, m) in schema_set.arenas.elements.iter() {
        for &head in &m.resolved_substitution_groups {
            subst_index.entry(head).or_default().push(mk);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn walk_particle(
        schema_set: &SchemaSet,
        particle: &ParticleResult,
        target_ns: Option<NameId>,
        local_keys: &[Option<ElementKey>],
        flat_idx: &mut usize,
        subst_index: &HashMap<ElementKey, Vec<ElementKey>>,
        out: &mut HashMap<(Option<NameId>, NameId), Vec<Entry>>,
        depth: usize,
    ) {
        if depth > 100 {
            return;
        }
        match &particle.term {
            ParticleTerm::Element(elem) => {
                if let Some(ref_qn) = &elem.ref_name {
                    *flat_idx += 1;
                    // The head itself contributes only when non-abstract;
                    // otherwise only its substitution members can appear.
                    let Some(head_key) =
                        schema_set.lookup_element(ref_qn.namespace, ref_qn.local_name)
                    else {
                        return;
                    };
                    let mut visited: std::collections::HashSet<ElementKey> =
                        std::collections::HashSet::new();
                    let mut stack = vec![head_key];
                    while let Some(current) = stack.pop() {
                        if !visited.insert(current) {
                            continue;
                        }
                        let Some(decl) = schema_set.arenas.elements.get(current) else {
                            continue;
                        };
                        // Per XSD 1.1 (W3C Bugzilla 4337), abstract members participate
                        // in the substitution group for cos-element-consistent purposes;
                        // XSD 1.0 excludes them.
                        if !decl.is_abstract || schema_set.is_xsd11() {
                            if let (Some(name), Some(t)) = (decl.name, decl.resolved_type) {
                                out.entry((decl.target_namespace, name))
                                    .or_default()
                                    .push((t, particle.source.clone()));
                            }
                        }
                        if let Some(members) = subst_index.get(&current) {
                            stack.extend(members.iter().copied());
                        }
                    }
                } else {
                    let idx = *flat_idx;
                    *flat_idx += 1;
                    if let Some(Some(elem_key)) = local_keys.get(idx) {
                        if let Some(decl) = schema_set.arenas.elements.get(*elem_key) {
                            if let (Some(name), Some(t)) = (decl.name, decl.resolved_type) {
                                // Arena's `target_namespace` is already the effective
                                // namespace (form + elementFormDefault applied during
                                // allocate_content_particle_elements), so we use it
                                // directly instead of falling back to the outer CT's
                                // target_ns.
                                let ns = decl.target_namespace;
                                out.entry((ns, name))
                                    .or_default()
                                    .push((t, decl.source.clone()));
                            }
                        }
                    }
                }
            }
            ParticleTerm::Group(group) => {
                if let Some(ref_qn) = &group.ref_name {
                    // Group refs don't advance the outer flat_idx; the
                    // model-group arena owns its own resolved_particle_elements.
                    if let Some(group_key) =
                        schema_set.lookup_model_group(ref_qn.namespace, ref_qn.local_name)
                    {
                        let mg = &schema_set.arenas.model_groups[group_key];
                        let inner_ns = mg.target_namespace.or(target_ns);
                        let mut inner_idx = 0usize;
                        for child in &mg.particles {
                            walk_particle(
                                schema_set,
                                child,
                                inner_ns,
                                &mg.resolved_particle_elements,
                                &mut inner_idx,
                                subst_index,
                                out,
                                depth + 1,
                            );
                        }
                    }
                } else {
                    for child in &group.particles {
                        walk_particle(
                            schema_set,
                            child,
                            target_ns,
                            local_keys,
                            flat_idx,
                            subst_index,
                            out,
                            depth + 1,
                        );
                    }
                }
            }
            ParticleTerm::Any(_) => {}
        }
    }

    for (_key, ct) in schema_set.arenas.complex_types.iter() {
        let ComplexContentResult::Complex(cc) = &ct.content else {
            continue;
        };
        let Some(particle) = cc.particle.as_ref() else {
            continue;
        };
        let mut entries: HashMap<(Option<NameId>, NameId), Vec<Entry>> = HashMap::new();
        let mut flat_idx = 0usize;
        walk_particle(
            schema_set,
            particle,
            ct.target_namespace,
            &ct.resolved_content_particle_elements,
            &mut flat_idx,
            &subst_index,
            &mut entries,
            0,
        );

        for ((ns, name), list) in &entries {
            if list.len() < 2 {
                continue;
            }
            let first_type = list[0].0;
            for (other_type, other_src) in &list[1..] {
                if *other_type == first_type {
                    continue;
                }
                let qn_str = format_type_name(schema_set, Some(*name), *ns);
                let location = schema_set
                    .locate(other_src.as_ref())
                    .or_else(|| schema_set.locate(list[0].1.as_ref()))
                    .or_else(|| schema_set.locate(ct.source.as_ref()));
                return Err(SchemaError::structural(
                    "cos-element-consistent",
                    format!(
                        "Element declarations for '{}' in the same content model \
                         (counting substitution-group expansion) have different \
                         {{type definition}}s (§3.8.6.3 / cos-element-consistent)",
                        qn_str
                    ),
                    location,
                ));
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// §src-redefine 6.2.2 / 7.2.2 — deferred restriction validation for redefines
// ---------------------------------------------------------------------------
//
// When an `<xs:redefine>` child group (or attribute group) has zero
// self-references, §src-redefine clauses 6.2.2 / 7.2.2 require that the
// redefined component be a *valid restriction* of the original. Composition
// (`schema/redefine.rs`) validates the self-reference shape (clauses 6.1 /
// 7.1) and flags zero-self-ref redefines via
// `redefine_requires_restriction_check`; this module does the deferred
// restriction check after reference resolution is complete.
//
// Spec anchors (source of truth: `structures.html` — W3C XSD 1.1 §src-redefine
// and §3.4.6.3 Derivation Valid (Restriction, Complex)):
//   - §src-redefine 6.2.2: redefined model group must be a valid restriction
//     of the original per §3.9.6 Particle Valid (Restriction).
//   - §src-redefine 7.2.2: redefined attribute group must satisfy clause 3
//     of §3.4.6.3 (clause-3 only, NOT clause-4 local-type-substitution).
//   - §3.8: pointless particles (`maxOccurs=0`) are eliminated before the
//     restriction check.
//   - §3.2.2: a prohibited `<xs:attribute>` is NOT an attribute use.
//
// Scope limitations:
//   - Chained redefines (`orig → v1 → v2`) resolve nested `group-ref`s via
//     the currently bound namespace version; see
//     `normalize_model_group_as_particle` doc comment.

/// Flatten an attribute group's effective attribute uses, filtering out
/// prohibited uses per §3.2.2 (a prohibited `<xs:attribute>` is not an
/// attribute use on either side of a restriction comparison).
fn collect_flat_attribute_uses_for_group(
    schema_set: &SchemaSet,
    ag_key: AttributeGroupKey,
) -> Vec<EffectiveAttributeUse> {
    let mut result = Vec::new();
    collect_attribute_group_uses(schema_set, ag_key, &mut result, 0);
    result.retain(|eau| eau.use_kind != AttributeUseKind::Prohibited);
    result
}

/// Construct a §src-redefine 6.2.2 structural error for a model group whose
/// restriction of its original cannot be validated.
fn make_redefine_group_restriction_error(
    schema_set: &SchemaSet,
    derived: &crate::arenas::ModelGroupData,
    detail: &str,
) -> SchemaError {
    let name = format_type_name(schema_set, derived.name, derived.target_namespace);
    let location = derived
        .source
        .as_ref()
        .and_then(|s| schema_set.source_maps.locate(s));
    SchemaError::structural(
        "src-redefine.6.2.2",
        format!(
            "Redefined group '{}' must be a valid restriction of the original \
             (§src-redefine 6.2.2): {}",
            name, detail,
        ),
        location,
    )
}

/// Construct a §src-redefine 7.2.2 structural error for an attribute group
/// whose restriction of its original cannot be validated.
fn make_redefine_attr_group_restriction_error(
    schema_set: &SchemaSet,
    derived: &crate::arenas::AttributeGroupData,
    detail: &str,
) -> SchemaError {
    let name = format_type_name(schema_set, derived.name, derived.target_namespace);
    let location = derived
        .source
        .as_ref()
        .and_then(|s| schema_set.source_maps.locate(s));
    SchemaError::structural(
        "src-redefine.7.2.2",
        format!(
            "Redefined attribute group '{}' must be a valid restriction of the original \
             (§src-redefine 7.2.2): {}",
            name, detail,
        ),
        location,
    )
}

/// Driver for §src-redefine 6.2.2: for each model group flagged as a
/// zero-self-reference redefine, verify its normalized particle is a valid
/// restriction of the original's normalized particle per §3.9.6 Particle
/// Valid (Restriction).
fn validate_all_redefine_group_restrictions(
    schema_set: &SchemaSet,
    errors: &mut Vec<SchemaError>,
    stats: &mut DerivationStats,
) {
    for (_key, derived) in schema_set.arenas.model_groups.iter() {
        if !derived.redefine_requires_restriction_check {
            continue;
        }
        let Some(original_key) = derived.redefine_original else {
            continue;
        };
        let Some(original) = schema_set.arenas.model_groups.get(original_key) else {
            continue;
        };

        let derived_particle = match normalize_model_group_as_particle(schema_set, derived) {
            Ok(p) => p,
            Err(e) => {
                errors.push(e);
                stats.errors += 1;
                continue;
            }
        };
        let base_particle = match normalize_model_group_as_particle(schema_set, original) {
            Ok(p) => p,
            Err(e) => {
                errors.push(e);
                stats.errors += 1;
                continue;
            }
        };

        // Empty-group special case (§3.8 + §3.9.6): when the derived group
        // normalizes to empty content after pointless-particle removal
        // (e.g. its only child had `maxOccurs=0`), `particle_restricts` does
        // not model the "empty content" case correctly — it would reject
        // legal restrictions whenever the base does not normalize to the
        // exact same surviving shape. Mirror the existing short-circuit in
        // `validate_content_particle_restriction` (derivation.rs:1148-1161):
        // empty derived is a valid restriction iff the base is emptiable.
        if is_effectively_empty(&derived_particle) {
            if !particle_is_emptiable(&base_particle) {
                errors.push(make_redefine_group_restriction_error(
                    schema_set,
                    derived,
                    "removes required content model of the original group",
                ));
                stats.errors += 1;
            }
            continue;
        }

        if !particle_restricts(schema_set, &derived_particle, &base_particle) {
            errors.push(make_redefine_group_restriction_error(
                schema_set,
                derived,
                "content model is not a valid restriction of the original group",
            ));
            stats.errors += 1;
        }
    }
}

/// Driver for §src-redefine 7.2.2: implementation of §3.4.6.3 clause 3
/// (derivation-ok-restriction, attribute side) applied to redefined
/// attribute groups. Checks:
///  - every derived attribute is present in the base (direct match) or
///    admitted by the base's effective attribute wildcard (§3.6.2.2);
///  - clause 3(b) type tightening on directly-matched pairs;
///  - required-stays-required;
///  - wildcard-vs-wildcard subset: the derived group's effective
///    attribute wildcard must be a valid restriction of the original's.
fn validate_all_redefine_attribute_group_restrictions(
    schema_set: &SchemaSet,
    errors: &mut Vec<SchemaError>,
    stats: &mut DerivationStats,
) {
    for (_key, derived) in schema_set.arenas.attribute_groups.iter() {
        if !derived.redefine_requires_restriction_check {
            continue;
        }
        let Some(original_key) = derived.redefine_original else {
            continue;
        };
        let Some(original) = schema_set.arenas.attribute_groups.get(original_key) else {
            continue;
        };

        // Flatten both sides, filtering out Prohibited uses (§3.2.2).
        // The derived key is the key we're iterating on — look it up to
        // get an `AttributeGroupKey` for `collect_flat_attribute_uses_for_group`.
        let derived_attrs = collect_flat_attribute_uses_for_group(schema_set, _key);
        let base_attrs = collect_flat_attribute_uses_for_group(schema_set, original_key);

        // Compute the base's effective attribute wildcard per §3.6.2.2
        // once, outside the per-attribute loop. This is the full
        // intersection across the original group's local wildcard and
        // the wildcards of every referenced nested attribute group.
        let base_effective_wc = match effective_attribute_wildcard(
            schema_set,
            original.attribute_wildcard.as_ref(),
            original.target_namespace,
            &original.resolved_attribute_groups,
        ) {
            Ok(eff) => eff,
            Err(e) => {
                errors.push(e);
                stats.errors += 1;
                continue;
            }
        };

        // Subset check (clause 3, first half): every derived attribute must
        // be valid in the base, either directly by (namespace, name) match
        // or via the base's effective {attribute wildcard}. Also applies the
        // clause 3(b) type-subsumption check for directly-matched pairs.
        let mut failed = false;
        for da in &derived_attrs {
            // (a) Direct match by (namespace, name).
            if let Some(ba) = base_attrs
                .iter()
                .find(|b| b.name == da.name && b.target_namespace == da.target_namespace)
            {
                // Type tightening (clause 3(b)): derived type must equal or
                // be derived from base type when both are resolved.
                if let (Some(dt), Some(bt)) = (da.resolved_type, ba.resolved_type) {
                    if dt != bt && !is_type_derived_from(schema_set, dt, bt) {
                        let attr_name_str = schema_set.name_table.resolve(da.name).to_string();
                        errors.push(make_redefine_attr_group_restriction_error(
                            schema_set,
                            derived,
                            &format!(
                                "attribute '{}' has a type that is not validly derived from the \
                                 base attribute type",
                                attr_name_str,
                            ),
                        ));
                        stats.errors += 1;
                        failed = true;
                        break;
                    }
                }
                // Fixed-value tightening (clause 3, derivation-ok-restriction
                // §3.4.6.3 attribute side): if the base attribute use has
                // {value constraint} = (fixed, V), the derived attribute use
                // must also have {value constraint} = (fixed, V). It cannot
                // be relaxed to (default, V), nor removed entirely. The W3C
                // `schM10` fixture exercises the fixed→default relaxation.
                if let Some(ref base_fixed) = ba.fixed_value {
                    let derived_matches =
                        da.fixed_value.as_ref().is_some_and(|dv| dv == base_fixed);
                    if !derived_matches {
                        let attr_name_str = schema_set.name_table.resolve(da.name).to_string();
                        errors.push(make_redefine_attr_group_restriction_error(
                            schema_set,
                            derived,
                            &format!(
                                "attribute '{}' relaxes or removes the base 'fixed=\"{}\"' \
                                 value constraint",
                                attr_name_str, base_fixed,
                            ),
                        ));
                        stats.errors += 1;
                        failed = true;
                        break;
                    }
                }
                continue;
            }
            // (b) Admitted by the base's *effective* {attribute wildcard}
            // (§3.6.2.2), not just the original group's local wildcard.
            if let Some(ref bwc) = base_effective_wc {
                if effective_wildcard_allows_attribute(
                    schema_set,
                    bwc,
                    da.target_namespace,
                    da.name,
                ) {
                    continue;
                }
            }
            // Neither (a) nor (b) holds — not a valid restriction.
            let attr_name_str = schema_set.name_table.resolve(da.name).to_string();
            errors.push(make_redefine_attr_group_restriction_error(
                schema_set,
                derived,
                &format!(
                    "attribute '{}' is not present in the original and is not admitted by \
                     the original's attribute wildcard",
                    attr_name_str,
                ),
            ));
            stats.errors += 1;
            failed = true;
            break;
        }
        if failed {
            continue;
        }

        // Required-stays-required (clause 3(a)): every base Required attribute
        // must also be Required in the derived side.
        let mut req_failed = false;
        for ba in &base_attrs {
            if ba.use_kind != AttributeUseKind::Required {
                continue;
            }
            let matching = derived_attrs
                .iter()
                .find(|d| d.name == ba.name && d.target_namespace == ba.target_namespace);
            match matching {
                Some(da) if da.use_kind == AttributeUseKind::Required => {}
                _ => {
                    let attr_name_str = schema_set.name_table.resolve(ba.name).to_string();
                    errors.push(make_redefine_attr_group_restriction_error(
                        schema_set,
                        derived,
                        &format!(
                            "base attribute '{}' is required but the redefined group does not \
                             declare it as required",
                            attr_name_str,
                        ),
                    ));
                    stats.errors += 1;
                    req_failed = true;
                    break;
                }
            }
        }
        if req_failed {
            continue;
        }

        // Wildcard-vs-wildcard subset (clause 3, second half of §3.6.2.2):
        // the derived group's effective attribute wildcard must be a
        // valid restriction of the original's. This catches cases where
        // the redefined group broadens an inherited wildcard even when
        // every directly-named attribute already checks out.
        let derived_effective_wc = match effective_attribute_wildcard(
            schema_set,
            derived.attribute_wildcard.as_ref(),
            derived.target_namespace,
            &derived.resolved_attribute_groups,
        ) {
            Ok(eff) => eff,
            Err(e) => {
                errors.push(e);
                stats.errors += 1;
                continue;
            }
        };
        match classify_attribute_wildcard_restriction(
            schema_set,
            derived_effective_wc.as_ref(),
            base_effective_wc.as_ref(),
        ) {
            WildcardRestrictionOutcome::DerivedAbsent | WildcardRestrictionOutcome::Valid => {}
            WildcardRestrictionOutcome::AddedInDerived => {
                errors.push(make_redefine_attr_group_restriction_error(
                    schema_set,
                    derived,
                    "redefined attribute group declares an attribute wildcard but \
                     the original has none",
                ));
                stats.errors += 1;
            }
            WildcardRestrictionOutcome::NotSubset(reason) => {
                errors.push(make_redefine_attr_group_restriction_error(
                    schema_set,
                    derived,
                    &format!(
                        "attribute wildcard is not a valid restriction of the \
                         original: {}",
                        reason,
                    ),
                ));
                stats.errors += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arenas::{ComplexTypeDefData, SimpleTypeDefData};
    use crate::parser::frames::ComplexContentResult;
    use crate::schema::model::DerivationSet;

    fn create_simple_type_data(
        name: Option<NameId>,
        variety: SimpleTypeVariety,
    ) -> SimpleTypeDefData {
        SimpleTypeDefData {
            name,
            target_namespace: None,
            variety,
            base_type: None,
            item_type: None,
            member_types: Vec::new(),
            facets: FacetSet::new(),
            final_derivation: DerivationSet::empty(),
            id: None,
            derivation_id: None,
            annotation: None,
            source: None,
            resolved_base_type: None,
            resolved_item_type: None,
            resolved_member_types: Vec::new(),
            redefine_original: None,
        }
    }

    fn create_complex_type_data(name: Option<NameId>) -> ComplexTypeDefData {
        ComplexTypeDefData {
            name,
            target_namespace: None,
            base_type: None,
            derivation_method: None,
            content: ComplexContentResult::Empty,
            open_content: None,
            attributes: Vec::new(),
            attribute_groups: Vec::new(),
            attribute_wildcard: None,
            mixed: false,
            is_abstract: false,
            final_derivation: DerivationSet::empty(),
            block: DerivationSet::empty(),
            default_attributes_apply: true,
            id: None,
            #[cfg(feature = "xsd11")]
            assertions: Vec::new(),
            #[cfg(feature = "xsd11")]
            xpath_default_namespace: None,
            annotation: None,
            source: None,
            resolved_base_type: None,
            resolved_attribute_groups: Vec::new(),
            resolved_attributes: Vec::new(),
            resolved_content_particle_types: Vec::new(),
            resolved_content_particle_elements: Vec::new(),
            resolved_simple_content_type: None,
            redefine_original: None,
        }
    }

    #[test]
    fn test_derivation_stats_default() {
        let stats = DerivationStats::default();
        assert_eq!(stats.simple_types_validated, 0);
        assert_eq!(stats.complex_types_validated, 0);
        assert_eq!(stats.errors, 0);
    }

    #[test]
    fn test_validate_empty_schema() {
        let schema_set = SchemaSet::new();
        let dep_graph = DependencyGraph::new();

        let result = validate_all_derivations(&schema_set, &dep_graph);
        assert!(result.is_ok());

        let stats = result.unwrap();
        assert_eq!(stats.simple_types_validated, 0);
        assert_eq!(stats.complex_types_validated, 0);
    }

    #[test]
    fn test_validate_atomic_type_no_base() {
        let mut schema_set = SchemaSet::new();
        let type_data = create_simple_type_data(None, SimpleTypeVariety::Atomic);
        let key = schema_set.arenas.alloc_simple_type(type_data);

        let mut stats = DerivationStats::default();
        let result = validate_simple_type(&schema_set, key, &mut stats);

        assert!(result.is_ok());
        assert_eq!(stats.simple_types_validated, 1);
    }

    #[test]
    fn test_validate_list_type_no_item() {
        let mut schema_set = SchemaSet::new();
        let type_data = create_simple_type_data(None, SimpleTypeVariety::List);
        let key = schema_set.arenas.alloc_simple_type(type_data);

        let mut stats = DerivationStats::default();
        let result = validate_simple_type(&schema_set, key, &mut stats);

        assert!(result.is_ok());
        assert_eq!(stats.list_types_validated, 1);
    }

    #[test]
    fn test_validate_union_type_no_members() {
        let mut schema_set = SchemaSet::new();
        let type_data = create_simple_type_data(None, SimpleTypeVariety::Union);
        let key = schema_set.arenas.alloc_simple_type(type_data);

        let mut stats = DerivationStats::default();
        let result = validate_simple_type(&schema_set, key, &mut stats);

        assert!(result.is_ok());
        assert_eq!(stats.union_types_validated, 1);
    }

    #[test]
    fn test_validate_list_of_atomic() {
        let mut schema_set = SchemaSet::new();

        // Create an atomic item type
        let item_type_data = create_simple_type_data(None, SimpleTypeVariety::Atomic);
        let item_key = schema_set.arenas.alloc_simple_type(item_type_data);

        // Create a list type with atomic item type
        let mut list_type_data = create_simple_type_data(None, SimpleTypeVariety::List);
        list_type_data.resolved_item_type = Some(TypeKey::Simple(item_key));
        let list_key = schema_set.arenas.alloc_simple_type(list_type_data);

        let mut stats = DerivationStats::default();
        let result = validate_simple_type(&schema_set, list_key, &mut stats);

        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_list_of_list_error() {
        let mut schema_set = SchemaSet::new();

        // Create a list item type (invalid)
        let inner_list_data = create_simple_type_data(None, SimpleTypeVariety::List);
        let inner_key = schema_set.arenas.alloc_simple_type(inner_list_data);

        // Create a list type with list item type (should fail)
        let mut outer_list_data = create_simple_type_data(None, SimpleTypeVariety::List);
        outer_list_data.resolved_item_type = Some(TypeKey::Simple(inner_key));
        let outer_key = schema_set.arenas.alloc_simple_type(outer_list_data);

        let mut stats = DerivationStats::default();
        let result = validate_simple_type(&schema_set, outer_key, &mut stats);

        assert!(result.is_err());
        if let Err(SchemaError::StructuralError { constraint, .. }) = result {
            assert_eq!(constraint, "cos-list-of-atomic");
        } else {
            panic!("Expected structural error with cos-list-of-atomic constraint");
        }
    }

    #[test]
    fn test_validate_union_with_complex_member_error() {
        let mut schema_set = SchemaSet::new();

        // Create a complex type (invalid for union member)
        let complex_data = create_complex_type_data(None);
        let complex_key = schema_set.arenas.alloc_complex_type(complex_data);

        // Create a union type with complex member (should fail)
        let mut union_data = create_simple_type_data(None, SimpleTypeVariety::Union);
        union_data.resolved_member_types = vec![TypeKey::Complex(complex_key)];
        let union_key = schema_set.arenas.alloc_simple_type(union_data);

        let mut stats = DerivationStats::default();
        let result = validate_simple_type(&schema_set, union_key, &mut stats);

        assert!(result.is_err());
        if let Err(SchemaError::StructuralError { constraint, .. }) = result {
            assert_eq!(constraint, "cos-union-memberTypes");
        } else {
            panic!("Expected structural error with cos-union-memberTypes constraint");
        }
    }

    #[test]
    fn test_validate_complex_type_no_base() {
        let mut schema_set = SchemaSet::new();
        let type_data = create_complex_type_data(None);
        let key = schema_set.arenas.alloc_complex_type(type_data);

        let mut stats = DerivationStats::default();
        let result = validate_complex_type(&schema_set, key, &mut stats);

        assert!(result.is_ok());
        assert_eq!(stats.complex_types_validated, 1);
    }

    #[test]
    fn test_validate_complex_extension() {
        let mut schema_set = SchemaSet::new();

        // Create base complex type
        let base_data = create_complex_type_data(None);
        let base_key = schema_set.arenas.alloc_complex_type(base_data);

        // Create derived type with extension
        let mut derived_data = create_complex_type_data(None);
        derived_data.derivation_method = Some(DerivationMethod::Extension);
        derived_data.resolved_base_type = Some(TypeKey::Complex(base_key));
        let derived_key = schema_set.arenas.alloc_complex_type(derived_data);

        let mut stats = DerivationStats::default();
        let result = validate_complex_type(&schema_set, derived_key, &mut stats);

        assert!(result.is_ok());
        assert_eq!(stats.extensions_validated, 1);
    }

    #[test]
    fn test_validate_complex_restriction() {
        let mut schema_set = SchemaSet::new();

        // Create base complex type
        let base_data = create_complex_type_data(None);
        let base_key = schema_set.arenas.alloc_complex_type(base_data);

        // Create derived type with restriction
        let mut derived_data = create_complex_type_data(None);
        derived_data.derivation_method = Some(DerivationMethod::Restriction);
        derived_data.resolved_base_type = Some(TypeKey::Complex(base_key));
        let derived_key = schema_set.arenas.alloc_complex_type(derived_data);

        let mut stats = DerivationStats::default();
        let result = validate_complex_type(&schema_set, derived_key, &mut stats);

        assert!(result.is_ok());
        assert_eq!(stats.restrictions_validated, 1);
    }

    #[test]
    fn test_validate_extension_of_final_type_error() {
        let mut schema_set = SchemaSet::new();

        // Create base complex type with final="extension"
        let mut base_data = create_complex_type_data(None);
        base_data.final_derivation = DerivationSet::extension();
        let base_key = schema_set.arenas.alloc_complex_type(base_data);

        // Create derived type with extension (should fail)
        let mut derived_data = create_complex_type_data(None);
        derived_data.derivation_method = Some(DerivationMethod::Extension);
        derived_data.resolved_base_type = Some(TypeKey::Complex(base_key));
        let derived_key = schema_set.arenas.alloc_complex_type(derived_data);

        let mut stats = DerivationStats::default();
        let result = validate_complex_type(&schema_set, derived_key, &mut stats);

        assert!(result.is_err());
        if let Err(SchemaError::StructuralError { constraint, .. }) = result {
            assert_eq!(constraint, "cos-ct-extends");
        } else {
            panic!("Expected structural error with cos-ct-extends constraint");
        }
    }

    #[test]
    fn test_validate_extension_of_final_default_type_error() {
        // Assembly would apply finalDefault to types without an explicit final.
        // This test simulates that: base.final_derivation = extension (inherited from finalDefault).
        let mut schema_set = SchemaSet::new();

        let mut base_data = create_complex_type_data(None);
        base_data.final_derivation = DerivationSet::extension();
        let base_key = schema_set.arenas.alloc_complex_type(base_data);

        // Create derived type with extension (should fail).
        let mut derived_data = create_complex_type_data(None);
        derived_data.derivation_method = Some(DerivationMethod::Extension);
        derived_data.resolved_base_type = Some(TypeKey::Complex(base_key));
        let derived_key = schema_set.arenas.alloc_complex_type(derived_data);

        let mut stats = DerivationStats::default();
        let result = validate_complex_type(&schema_set, derived_key, &mut stats);

        assert!(result.is_err());
        if let Err(SchemaError::StructuralError { constraint, .. }) = result {
            assert_eq!(constraint, "cos-ct-extends");
        } else {
            panic!("Expected structural error with cos-ct-extends constraint");
        }
    }

    #[test]
    fn test_validate_restriction_of_final_type_error() {
        let mut schema_set = SchemaSet::new();

        // Create base complex type with final="restriction"
        let mut base_data = create_complex_type_data(None);
        base_data.final_derivation = DerivationSet::restriction();
        let base_key = schema_set.arenas.alloc_complex_type(base_data);

        // Create derived type with restriction (should fail)
        let mut derived_data = create_complex_type_data(None);
        derived_data.derivation_method = Some(DerivationMethod::Restriction);
        derived_data.resolved_base_type = Some(TypeKey::Complex(base_key));
        let derived_key = schema_set.arenas.alloc_complex_type(derived_data);

        let mut stats = DerivationStats::default();
        let result = validate_complex_type(&schema_set, derived_key, &mut stats);

        assert!(result.is_err());
        if let Err(SchemaError::StructuralError { constraint, .. }) = result {
            assert_eq!(constraint, "derivation-ok-restriction");
        } else {
            panic!("Expected structural error with derivation-ok-restriction constraint");
        }
    }

    #[test]
    fn test_format_type_name_anonymous() {
        let schema_set = SchemaSet::new();
        let name = format_type_name(&schema_set, None, None);
        assert_eq!(name, "(anonymous)");
    }

    #[test]
    fn test_format_type_name_with_namespace() {
        let schema_set = SchemaSet::new();
        let name_id = schema_set.name_table.add("myType");
        let ns_id = schema_set.name_table.add("http://example.com");
        let name = format_type_name(&schema_set, Some(name_id), Some(ns_id));
        assert_eq!(name, "{http://example.com}myType");
    }

    #[test]
    fn test_format_type_name_no_namespace() {
        let schema_set = SchemaSet::new();
        let name_id = schema_set.name_table.add("myType");
        let name = format_type_name(&schema_set, Some(name_id), None);
        assert_eq!(name, "myType");
    }

    // ====================================================================
    // XSD 1.1: Open-content derivation tests
    // ====================================================================

    #[cfg(feature = "xsd11")]
    fn make_open_content(
        mode: crate::parser::frames::OpenContentMode,
        namespace: crate::parser::frames::WildcardNamespace,
        pc: crate::parser::frames::ProcessContents,
    ) -> crate::parser::frames::OpenContentResult {
        crate::parser::frames::OpenContentResult {
            mode,
            wildcard: Some(crate::parser::frames::WildcardResult {
                namespace,
                process_contents: pc,
                not_namespace: Vec::new(),
                not_qname: Vec::new(),
                id: None,
                annotation: None,
                source: None,
            }),
            id: None,
            annotation: None,
            source: None,
        }
    }

    #[cfg(feature = "xsd11")]
    #[test]
    fn test_extension_suffix_cannot_extend_interleave() {
        use crate::parser::frames::{OpenContentMode, ProcessContents, WildcardNamespace};

        let mut schema_set = SchemaSet::new();

        let mut base_data = create_complex_type_data(None);
        base_data.open_content = Some(make_open_content(
            OpenContentMode::Interleave,
            WildcardNamespace::Any,
            ProcessContents::Lax,
        ));
        let base_key = schema_set.arenas.alloc_complex_type(base_data);

        let mut derived_data = create_complex_type_data(None);
        derived_data.derivation_method = Some(DerivationMethod::Extension);
        derived_data.resolved_base_type = Some(TypeKey::Complex(base_key));
        derived_data.open_content = Some(make_open_content(
            OpenContentMode::Suffix,
            WildcardNamespace::Any,
            ProcessContents::Lax,
        ));
        let derived_key = schema_set.arenas.alloc_complex_type(derived_data);

        let mut stats = DerivationStats::default();
        let result = validate_complex_type(&schema_set, derived_key, &mut stats);

        assert!(result.is_err());
        if let Err(SchemaError::StructuralError { constraint, .. }) = result {
            assert_eq!(constraint, "cos-ct-extends");
        } else {
            panic!("Expected cos-ct-extends error");
        }
    }

    #[cfg(feature = "xsd11")]
    #[test]
    fn test_extension_interleave_extends_interleave_valid() {
        use crate::parser::frames::{OpenContentMode, ProcessContents, WildcardNamespace};

        let mut schema_set = SchemaSet::new();

        let mut base_data = create_complex_type_data(None);
        base_data.open_content = Some(make_open_content(
            OpenContentMode::Interleave,
            WildcardNamespace::Any,
            ProcessContents::Lax,
        ));
        let base_key = schema_set.arenas.alloc_complex_type(base_data);

        let mut derived_data = create_complex_type_data(None);
        derived_data.derivation_method = Some(DerivationMethod::Extension);
        derived_data.resolved_base_type = Some(TypeKey::Complex(base_key));
        derived_data.open_content = Some(make_open_content(
            OpenContentMode::Interleave,
            WildcardNamespace::Any,
            ProcessContents::Lax,
        ));
        let derived_key = schema_set.arenas.alloc_complex_type(derived_data);

        let mut stats = DerivationStats::default();
        let result = validate_complex_type(&schema_set, derived_key, &mut stats);
        assert!(result.is_ok());
    }

    #[cfg(feature = "xsd11")]
    #[test]
    fn test_extension_base_has_oc_derived_has_none() {
        use crate::parser::frames::{OpenContentMode, ProcessContents, WildcardNamespace};

        // Per §3.4.2.3 clause 6.1 and §3.4.6.2 clause 1.4.3.2.2:
        // when the derivation declares no <xs:openContent>, the effective
        // {open content} of the derived type (EOT) inherits the base's
        // (BOT).  That trivially satisfies clauses 1.4.3.2.2.3 and
        // 1.4.3.2.2.4, so extension is valid.  (saxonData/Open/open027.)
        let mut schema_set = SchemaSet::new();

        let mut base_data = create_complex_type_data(None);
        base_data.open_content = Some(make_open_content(
            OpenContentMode::Interleave,
            WildcardNamespace::Any,
            ProcessContents::Lax,
        ));
        let base_key = schema_set.arenas.alloc_complex_type(base_data);

        let mut derived_data = create_complex_type_data(None);
        derived_data.derivation_method = Some(DerivationMethod::Extension);
        derived_data.resolved_base_type = Some(TypeKey::Complex(base_key));
        // No open_content on derived — inherits from base per clause 6.1.
        let derived_key = schema_set.arenas.alloc_complex_type(derived_data);

        let mut stats = DerivationStats::default();
        let result = validate_complex_type(&schema_set, derived_key, &mut stats);

        assert!(
            result.is_ok(),
            "derived inherits BOT per clause 6.1: {:?}",
            result
        );
    }

    #[cfg(feature = "xsd11")]
    #[test]
    fn test_extension_base_no_oc_derived_adds_oc_valid() {
        use crate::parser::frames::{OpenContentMode, ProcessContents, WildcardNamespace};

        let mut schema_set = SchemaSet::new();

        // Base has no open content
        let base_data = create_complex_type_data(None);
        let base_key = schema_set.arenas.alloc_complex_type(base_data);

        let mut derived_data = create_complex_type_data(None);
        derived_data.derivation_method = Some(DerivationMethod::Extension);
        derived_data.resolved_base_type = Some(TypeKey::Complex(base_key));
        derived_data.open_content = Some(make_open_content(
            OpenContentMode::Interleave,
            WildcardNamespace::Any,
            ProcessContents::Lax,
        ));
        let derived_key = schema_set.arenas.alloc_complex_type(derived_data);

        let mut stats = DerivationStats::default();
        let result = validate_complex_type(&schema_set, derived_key, &mut stats);
        assert!(result.is_ok());
    }

    #[cfg(feature = "xsd11")]
    #[test]
    fn test_restriction_adds_oc_when_base_has_none() {
        use crate::parser::frames::{OpenContentMode, ProcessContents, WildcardNamespace};

        let mut schema_set = SchemaSet::new();

        // Base has no open content
        let base_data = create_complex_type_data(None);
        let base_key = schema_set.arenas.alloc_complex_type(base_data);

        let mut derived_data = create_complex_type_data(None);
        derived_data.derivation_method = Some(DerivationMethod::Restriction);
        derived_data.resolved_base_type = Some(TypeKey::Complex(base_key));
        derived_data.open_content = Some(make_open_content(
            OpenContentMode::Interleave,
            WildcardNamespace::Any,
            ProcessContents::Lax,
        ));
        let derived_key = schema_set.arenas.alloc_complex_type(derived_data);

        let mut stats = DerivationStats::default();
        let result = validate_complex_type(&schema_set, derived_key, &mut stats);

        assert!(result.is_err());
        if let Err(SchemaError::StructuralError { constraint, .. }) = result {
            assert_eq!(constraint, "derivation-ok-restriction");
        } else {
            panic!("Expected derivation-ok-restriction error");
        }
    }

    #[cfg(feature = "xsd11")]
    #[test]
    fn test_restriction_removes_oc_valid() {
        use crate::parser::frames::{OpenContentMode, ProcessContents, WildcardNamespace};

        let mut schema_set = SchemaSet::new();

        let mut base_data = create_complex_type_data(None);
        base_data.open_content = Some(make_open_content(
            OpenContentMode::Interleave,
            WildcardNamespace::Any,
            ProcessContents::Lax,
        ));
        let base_key = schema_set.arenas.alloc_complex_type(base_data);

        let mut derived_data = create_complex_type_data(None);
        derived_data.derivation_method = Some(DerivationMethod::Restriction);
        derived_data.resolved_base_type = Some(TypeKey::Complex(base_key));
        // No open_content — restriction removes it
        let derived_key = schema_set.arenas.alloc_complex_type(derived_data);

        let mut stats = DerivationStats::default();
        let result = validate_complex_type(&schema_set, derived_key, &mut stats);
        assert!(result.is_ok());
    }

    #[cfg(feature = "xsd11")]
    #[test]
    fn test_restriction_empty_derived_allows_interleave_over_suffix() {
        // Per §3.4.6.4 (language containment), an empty derived particle
        // emits only wildcard content, so the OC mode choice is irrelevant —
        // interleave and suffix accept the same empty-particle language.
        // Mirrors W3C saxonData/Open/open020/open021 which expect VALID.
        use crate::parser::frames::{OpenContentMode, ProcessContents, WildcardNamespace};

        let mut schema_set = SchemaSet::new();

        let mut base_data = create_complex_type_data(None);
        base_data.open_content = Some(make_open_content(
            OpenContentMode::Suffix,
            WildcardNamespace::Any,
            ProcessContents::Lax,
        ));
        let base_key = schema_set.arenas.alloc_complex_type(base_data);

        let mut derived_data = create_complex_type_data(None);
        derived_data.derivation_method = Some(DerivationMethod::Restriction);
        derived_data.resolved_base_type = Some(TypeKey::Complex(base_key));
        derived_data.open_content = Some(make_open_content(
            OpenContentMode::Interleave,
            WildcardNamespace::Any,
            ProcessContents::Lax,
        ));
        let derived_key = schema_set.arenas.alloc_complex_type(derived_data);

        let mut stats = DerivationStats::default();
        let result = validate_complex_type(&schema_set, derived_key, &mut stats);
        assert!(
            result.is_ok(),
            "empty derived content should accept interleave over suffix, got {:?}",
            result.err(),
        );
    }

    #[cfg(feature = "xsd11")]
    #[test]
    fn test_restriction_suffix_restricts_interleave_valid() {
        use crate::parser::frames::{OpenContentMode, ProcessContents, WildcardNamespace};

        let mut schema_set = SchemaSet::new();

        let mut base_data = create_complex_type_data(None);
        base_data.open_content = Some(make_open_content(
            OpenContentMode::Interleave,
            WildcardNamespace::Any,
            ProcessContents::Lax,
        ));
        let base_key = schema_set.arenas.alloc_complex_type(base_data);

        let mut derived_data = create_complex_type_data(None);
        derived_data.derivation_method = Some(DerivationMethod::Restriction);
        derived_data.resolved_base_type = Some(TypeKey::Complex(base_key));
        derived_data.open_content = Some(make_open_content(
            OpenContentMode::Suffix,
            WildcardNamespace::Any,
            ProcessContents::Lax,
        ));
        let derived_key = schema_set.arenas.alloc_complex_type(derived_data);

        let mut stats = DerivationStats::default();
        let result = validate_complex_type(&schema_set, derived_key, &mut stats);
        assert!(result.is_ok());
    }

    // ====================================================================
    // cos-ns-subset: ##other exclusion-set tests (§3.10.1, §3.10.6.2)
    //
    // ##other maps to not({target namespace}, absent), so it always
    // excludes both the target namespace AND absent.
    // ====================================================================

    #[cfg(feature = "xsd11")]
    #[test]
    fn test_ns_subset_local_not_subset_of_other() {
        // Base ##other with target ns urn:a excludes {Some(urn:a), None}.
        // Derived ##local allows {None}.
        // None is in base's exclusion set → NOT a subset.
        use crate::parser::frames::WildcardNamespace;

        let schema_set = SchemaSet::new();
        let urn_a = schema_set.name_table.add("urn:a");

        let result = is_namespace_subset(
            &WildcardNamespace::Local,
            None,
            &WildcardNamespace::Other,
            Some(urn_a),
        );
        assert!(
            !result,
            "##local must NOT be a subset of ##other (absent is excluded)"
        );
    }

    #[cfg(feature = "xsd11")]
    #[test]
    fn test_ns_subset_other_no_tns_not_subset_of_other_with_tns() {
        // Base ##other with tns=urn:a excludes {Some(urn:a), None}.
        // Derived ##other with tns=None excludes {None}.
        // Derived still allows urn:a, which base excludes → NOT a subset.
        use crate::parser::frames::WildcardNamespace;

        let schema_set = SchemaSet::new();
        let urn_a = schema_set.name_table.add("urn:a");

        let result = is_namespace_subset(
            &WildcardNamespace::Other,
            None,
            &WildcardNamespace::Other,
            Some(urn_a),
        );
        assert!(
            !result,
            "##other(tns=None) must NOT be a subset of ##other(tns=urn:a)"
        );
    }

    #[cfg(feature = "xsd11")]
    #[test]
    fn test_ns_subset_other_with_tns_is_subset_of_other_no_tns() {
        // Base ##other with tns=None excludes {None}.
        // Derived ##other with tns=urn:a excludes {Some(urn:a), None}.
        // Derived excludes a superset → IS a subset.
        use crate::parser::frames::WildcardNamespace;

        let schema_set = SchemaSet::new();
        let urn_a = schema_set.name_table.add("urn:a");

        let result = is_namespace_subset(
            &WildcardNamespace::Other,
            Some(urn_a),
            &WildcardNamespace::Other,
            None,
        );
        assert!(
            result,
            "##other(tns=urn:a) MUST be a subset of ##other(tns=None)"
        );
    }

    #[cfg(feature = "xsd11")]
    #[test]
    fn test_ns_subset_list_with_tns_uri_not_subset_of_other() {
        // Base ##other with tns=urn:a excludes {Some(urn:a), None}.
        // Derived list contains explicit urn:a URI.
        // urn:a is in base's exclusion set → NOT a subset.
        use crate::parser::frames::{NamespaceToken, WildcardNamespace};

        let schema_set = SchemaSet::new();
        let urn_a = schema_set.name_table.add("urn:a");
        let urn_b = schema_set.name_table.add("urn:b");

        let result = is_namespace_subset(
            &WildcardNamespace::List(vec![NamespaceToken::Uri(urn_a), NamespaceToken::Uri(urn_b)]),
            None,
            &WildcardNamespace::Other,
            Some(urn_a),
        );
        assert!(
            !result,
            "List containing base's target ns must NOT be a subset of ##other"
        );
    }

    // -----------------------------------------------------------------
    // §src-redefine 6.2.2 / 7.2.2 — focused pin tests
    //
    // Broad end-to-end rejection coverage (schR5/attgC028/mgO013) and
    // positive-coverage guards (annotA019, attgC017, schH1, schU1, …)
    // are already exercised by the W3C conformance suite. The tests
    // below pin the subtle invariants that are NOT directly covered by
    // conformance: (a) the `wildcard_allows_attribute` helper's
    // `##defined` correctness — which is the whole reason the helper
    // exists, and (b) the `all{required_e1}` vs `all{}` particle shape
    // that `mgO013` ultimately relies on.
    // -----------------------------------------------------------------

    fn default_wildcard(ns: WildcardNamespace) -> WildcardResult {
        WildcardResult {
            namespace: ns,
            process_contents: ProcessContents::Strict,
            not_namespace: Vec::new(),
            not_qname: Vec::new(),
            id: None,
            annotation: None,
            source: None,
        }
    }

    /// Normalize `w` against `target_ns` and ask whether the resulting
    /// effective wildcard admits `(attr_ns, attr_name)`. Used by the
    /// spec-invariant pin tests below so they exercise the production
    /// canonical-form path.
    fn admits(
        schema_set: &SchemaSet,
        w: &WildcardResult,
        target_ns: Option<NameId>,
        attr_ns: Option<NameId>,
        attr_name: NameId,
    ) -> bool {
        let eff = normalize_attribute_wildcard(schema_set, w, target_ns);
        effective_wildcard_allows_attribute(schema_set, &eff, attr_ns, attr_name)
    }

    #[test]
    fn test_effective_wildcard_any_admits_anything() {
        let schema_set = SchemaSet::new();
        let name = schema_set.name_table.add("foo");
        let w = default_wildcard(WildcardNamespace::Any);
        assert!(admits(&schema_set, &w, None, None, name));
    }

    #[test]
    fn test_effective_wildcard_other_excludes_target_ns() {
        // ##other must exclude the target namespace itself.
        let schema_set = SchemaSet::new();
        let ns = schema_set.name_table.add("urn:foo");
        let name = schema_set.name_table.add("bar");
        let w = default_wildcard(WildcardNamespace::Other);
        assert!(
            !admits(&schema_set, &w, Some(ns), Some(ns), name),
            "##other must NOT admit the target namespace"
        );
    }

    #[test]
    fn test_effective_wildcard_other_admits_different_ns() {
        let schema_set = SchemaSet::new();
        let tns = schema_set.name_table.add("urn:foo");
        let other_ns = schema_set.name_table.add("urn:bar");
        let name = schema_set.name_table.add("qux");
        let w = default_wildcard(WildcardNamespace::Other);
        assert!(
            admits(&schema_set, &w, Some(tns), Some(other_ns), name),
            "##other must admit a namespace different from the target"
        );
    }

    #[test]
    fn test_effective_wildcard_other_absent_ns_xsd10_vs_xsd11() {
        // §3.10.4.2 `##other` differs by version:
        //   - XSD 1.0: excludes both the target namespace AND the absent namespace.
        //   - XSD 1.1: excludes only the target namespace; the absent namespace is admitted.
        let schema_10 = SchemaSet::new(); // defaults to XSD 1.0
        let tns = schema_10.name_table.add("urn:foo");
        let name = schema_10.name_table.add("local_attr");
        let w = default_wildcard(WildcardNamespace::Other);

        assert!(
            !admits(&schema_10, &w, Some(tns), None, name),
            "XSD 1.0: ##other must NOT admit the absent namespace"
        );

        let schema_11 = SchemaSet::xsd11();
        let tns11 = schema_11.name_table.add("urn:foo");
        let name11 = schema_11.name_table.add("local_attr");
        assert!(
            admits(&schema_11, &w, Some(tns11), None, name11),
            "XSD 1.1: ##other MUST admit the absent namespace"
        );
    }

    #[test]
    fn test_effective_wildcard_defined_excludes_declared_only() {
        // §3.10.4 `##defined` excludes ONLY attributes that are globally
        // declared — not all attributes unconditionally.
        use crate::arenas::AttributeDeclData;
        use crate::parser::frames::NotQNameItem;

        let mut schema_set = SchemaSet::new();
        let declared_name = schema_set.name_table.add("declared_attr");
        let undeclared_name = schema_set.name_table.add("undeclared_attr");

        let attr_data = AttributeDeclData {
            name: Some(declared_name),
            target_namespace: None,
            ref_name: None,
            type_ref: None,
            inline_type: None,
            default_value: None,
            fixed_value: None,
            use_kind: None,
            form: None,
            inheritable: false,
            id: None,
            annotation: None,
            source: None,
            resolved_type: None,
            resolved_ref: None,
        };
        let attr_key = schema_set.arenas.alloc_attribute(attr_data);
        schema_set
            .get_or_create_namespace(None)
            .register_attribute(declared_name, attr_key);

        let mut w = default_wildcard(WildcardNamespace::Any);
        w.not_qname = vec![NotQNameItem::Defined];

        assert!(
            !admits(&schema_set, &w, None, None, declared_name),
            "##defined MUST exclude globally-declared attributes"
        );
        assert!(
            admits(&schema_set, &w, None, None, undeclared_name),
            "##defined MUST NOT exclude attributes that are not globally declared"
        );
    }

    #[test]
    fn test_effective_wildcard_not_qname_literal_excludes() {
        use crate::parser::frames::NotQNameItem;

        let schema_set = SchemaSet::new();
        let blocked = schema_set.name_table.add("blocked");
        let allowed = schema_set.name_table.add("allowed");

        let mut w = default_wildcard(WildcardNamespace::Any);
        w.not_qname = vec![NotQNameItem::QName {
            namespace: None,
            local_name: blocked,
        }];

        assert!(!admits(&schema_set, &w, None, None, blocked));
        assert!(admits(&schema_set, &w, None, None, allowed));
    }

    #[test]
    fn test_particle_restricts_all_required_over_empty_all_rejects() {
        // Pin test for the exact shape mgO013 reaches after
        // `remove_pointless_particles`: base `all{}` (e1{0,0} removed)
        // vs derived `all{e1{1,1}}`. The driver must reject — derived
        // adds a required particle to empty content, which is not a
        // valid restriction under §3.9.6.
        let schema_set = SchemaSet::new();
        let e1_name = schema_set.name_table.add("e1");
        let any_type = TypeKey::Complex(schema_set.any_type_key());

        let make_elem = |min_occurs: u32, max_occurs: Option<u32>| NormalizedParticle {
            term: NormalizedParticleTerm::Element(NormalizedElement {
                name: e1_name,
                namespace: None,
                type_key: any_type,
                element_key: None,
                block: DerivationSet::empty(),
                nillable: false,
                fixed_value: None,
            }),
            min_occurs,
            max_occurs,
            source: None,
        };

        let derived = NormalizedParticle {
            term: NormalizedParticleTerm::Group(NormalizedGroup {
                compositor: Compositor::All,
                particles: vec![make_elem(1, Some(1))],
            }),
            min_occurs: 1,
            max_occurs: Some(1),
            source: None,
        };
        let base_empty_all = NormalizedParticle {
            term: NormalizedParticleTerm::Group(NormalizedGroup {
                compositor: Compositor::All,
                particles: Vec::new(),
            }),
            min_occurs: 1,
            max_occurs: Some(1),
            source: None,
        };

        assert!(
            !particle_restricts(&schema_set, &derived, &base_empty_all),
            "all{{e1{{1,1}}}} must NOT restrict all{{}} — derived adds a required particle"
        );
    }

    #[test]
    fn test_collect_flat_attribute_uses_filters_prohibited() {
        // §3.2.2: prohibited attribute uses do NOT correspond to components
        // and must not appear in either side of a restriction comparison.
        use crate::arenas::AttributeGroupData;
        use crate::parser::frames::{
            AttributeFrameResult, AttributeUseKind as AuK, AttributeUseResult,
        };

        let mut schema_set = SchemaSet::new();
        let grp_name = schema_set.name_table.add("ag");
        let opt_name = schema_set.name_table.add("opt");
        let banned_name = schema_set.name_table.add("banned");

        let make_attr = |name: NameId, kind: AuK| AttributeUseResult {
            attribute: AttributeFrameResult {
                name: Some(name),
                ref_name: None,
                target_namespace: None,
                type_ref: None,
                inline_type: None,
                default_value: None,
                fixed_value: None,
                use_kind: None,
                form: None,
                inheritable: false,
                id: None,
                annotation: None,
                source: None,
            },
            use_kind: kind,
        };

        let ag = AttributeGroupData {
            name: Some(grp_name),
            target_namespace: None,
            ref_name: None,
            attributes: vec![
                make_attr(opt_name, AuK::Optional),
                make_attr(banned_name, AuK::Prohibited),
            ],
            attribute_groups: Vec::new(),
            attribute_wildcard: None,
            id: None,
            annotation: None,
            source: None,
            resolved_ref: None,
            resolved_attribute_groups: Vec::new(),
            resolved_attributes: vec![
                crate::arenas::ResolvedAttributeUse {
                    resolved_type: None,
                    resolved_ref: None,
                },
                crate::arenas::ResolvedAttributeUse {
                    resolved_type: None,
                    resolved_ref: None,
                },
            ],
            redefine_original: None,
            redefine_requires_restriction_check: false,
        };
        let ag_key = schema_set.arenas.alloc_attribute_group(ag);

        let uses = collect_flat_attribute_uses_for_group(&schema_set, ag_key);
        // Prohibited must be dropped; only `opt` survives.
        assert_eq!(
            uses.len(),
            1,
            "prohibited attribute uses must be filtered out"
        );
        assert_eq!(uses[0].name, opt_name);
    }

    // -----------------------------------------------------------------
    // §3.6.2.2 effective attribute wildcard + §3.10.6.4 intersection
    // -----------------------------------------------------------------

    fn wildcard_with_ns(namespace: WildcardNamespace) -> WildcardResult {
        WildcardResult {
            namespace,
            process_contents: ProcessContents::Strict,
            not_namespace: Vec::new(),
            not_qname: Vec::new(),
            id: None,
            annotation: None,
            source: None,
        }
    }

    #[test]
    fn test_normalize_any() {
        let schema_set = SchemaSet::new();
        let wc = wildcard_with_ns(WildcardNamespace::Any);
        let eff = normalize_attribute_wildcard(&schema_set, &wc, None);
        assert!(matches!(eff.namespace, CanonicalNs::Any));
    }

    #[test]
    fn test_normalize_list_resolves_tokens() {
        use crate::parser::frames::NamespaceToken;
        let schema_set = SchemaSet::new();
        let ns_a = schema_set.name_table.add("http://a");
        let target = schema_set.name_table.add("http://t");

        let wc = wildcard_with_ns(WildcardNamespace::List(vec![
            NamespaceToken::Uri(ns_a),
            NamespaceToken::TargetNamespace,
            NamespaceToken::Local,
        ]));
        let eff = normalize_attribute_wildcard(&schema_set, &wc, Some(target));
        match eff.namespace {
            CanonicalNs::Enum(set) => {
                assert!(set.contains(&Some(ns_a)));
                assert!(set.contains(&Some(target)));
                assert!(set.contains(&None));
                assert_eq!(set.len(), 3);
            }
            _ => panic!("expected Enum"),
        }
    }

    #[test]
    fn test_normalize_other_xsd10_vs_xsd11() {
        // Pins the fix documented at types/complex.rs:287-303 — XSD 1.0
        // `##other` excludes {target, absent}; XSD 1.1 excludes {target}
        // only.
        let schema_10 = SchemaSet::new();
        let schema_11 = SchemaSet::xsd11();
        let target_10 = schema_10.name_table.add("http://t");
        let target_11 = schema_11.name_table.add("http://t");

        let wc10 = wildcard_with_ns(WildcardNamespace::Other);
        let wc11 = wildcard_with_ns(WildcardNamespace::Other);

        let eff10 = normalize_attribute_wildcard(&schema_10, &wc10, Some(target_10));
        let eff11 = normalize_attribute_wildcard(&schema_11, &wc11, Some(target_11));

        match eff10.namespace {
            CanonicalNs::Not(set) => {
                assert!(set.contains(&Some(target_10)));
                assert!(set.contains(&None), "XSD 1.0 ##other excludes absent");
            }
            _ => panic!("expected Not"),
        }
        match eff11.namespace {
            CanonicalNs::Not(set) => {
                assert!(set.contains(&Some(target_11)));
                assert!(!set.contains(&None), "XSD 1.1 ##other admits absent");
            }
            _ => panic!("expected Not"),
        }
    }

    #[test]
    fn test_normalize_other_absent_target_namespace() {
        // Regression: when the schema has no target namespace, the
        // "target namespace" IS the absent namespace (None), so
        // ##other must exclude None even in XSD 1.1. An earlier
        // implementation skipped inserting None for XSD 1.1 when
        // target_ns was None, producing Not({}) ≡ Any and incorrectly
        // accepting invalid derivations for no-targetNamespace schemas.
        let schema_10 = SchemaSet::new();
        let schema_11 = SchemaSet::xsd11();

        let wc = wildcard_with_ns(WildcardNamespace::Other);
        let eff10 = normalize_attribute_wildcard(&schema_10, &wc, None);
        let eff11 = normalize_attribute_wildcard(&schema_11, &wc, None);

        for (label, eff) in [("XSD 1.0", eff10), ("XSD 1.1", eff11)] {
            match eff.namespace {
                CanonicalNs::Not(set) => {
                    assert!(
                        set.contains(&None),
                        "{}: ##other with absent target MUST exclude the absent namespace",
                        label,
                    );
                }
                other => panic!("{}: expected Not, got {:?}", label, other),
            }
        }
    }

    #[test]
    fn test_effective_wildcard_restricts_defined_covers_declared_qname() {
        // Regression: per §3.10.6.2 disallowed_names clause 1, a base
        // QName exclusion is satisfied whenever the derived wildcard
        // is "not allowed" for that QName — including via a derived
        // `##defined` when the base QName names a globally declared
        // attribute. An earlier implementation required literal
        // `QName{}` containment and wrongly rejected this pattern.
        use crate::arenas::AttributeDeclData;
        use crate::parser::frames::NotQNameItem;

        let mut schema_set = SchemaSet::new();
        let declared_name = schema_set.name_table.add("declared_attr");

        // Globally declare `declared_attr`.
        let attr_data = AttributeDeclData {
            name: Some(declared_name),
            target_namespace: None,
            ref_name: None,
            type_ref: None,
            inline_type: None,
            default_value: None,
            fixed_value: None,
            use_kind: None,
            form: None,
            inheritable: false,
            id: None,
            annotation: None,
            source: None,
            resolved_type: None,
            resolved_ref: None,
        };
        let attr_key = schema_set.arenas.alloc_attribute(attr_data);
        schema_set
            .get_or_create_namespace(None)
            .register_attribute(declared_name, attr_key);

        // Base excludes the declared attribute by literal QName.
        let base = EffectiveAttributeWildcard {
            namespace: CanonicalNs::Any,
            not_qname: vec![NotQNameItem::QName {
                namespace: None,
                local_name: declared_name,
            }],
            process_contents: ProcessContents::Strict,
        };
        // Derived excludes via ##defined — should cover the base
        // exclusion because declared_attr is globally declared.
        let derived = EffectiveAttributeWildcard {
            namespace: CanonicalNs::Any,
            not_qname: vec![NotQNameItem::Defined],
            process_contents: ProcessContents::Strict,
        };

        assert!(
            effective_attribute_wildcard_restricts(&schema_set, &derived, &base).is_ok(),
            "derived ##defined must cover a base literal QName exclusion \
             when the attribute is globally declared"
        );

        // Undeclared attribute: ##defined does NOT cover it.
        let undeclared = schema_set.name_table.add("undeclared_attr");
        let base_undeclared = EffectiveAttributeWildcard {
            namespace: CanonicalNs::Any,
            not_qname: vec![NotQNameItem::QName {
                namespace: None,
                local_name: undeclared,
            }],
            process_contents: ProcessContents::Strict,
        };
        assert!(
            effective_attribute_wildcard_restricts(&schema_set, &derived, &base_undeclared)
                .is_err(),
            "derived ##defined must NOT cover a base QName exclusion \
             when the attribute is not globally declared"
        );
    }

    #[test]
    fn test_normalize_folds_not_namespace() {
        use crate::parser::frames::NamespaceToken;
        let schema_set = SchemaSet::new();
        let ns_a = schema_set.name_table.add("http://a");

        // Any wildcard with not_namespace=[ns_a] becomes Not({ns_a}).
        let mut wc = wildcard_with_ns(WildcardNamespace::Any);
        wc.not_namespace = vec![NamespaceToken::Uri(ns_a)];
        let eff = normalize_attribute_wildcard(&schema_set, &wc, None);
        match eff.namespace {
            CanonicalNs::Not(set) => {
                assert_eq!(set.len(), 1);
                assert!(set.contains(&Some(ns_a)));
            }
            _ => panic!("expected Not"),
        }
    }

    #[test]
    fn test_intersect_any_is_identity() {
        let mut s = std::collections::HashSet::new();
        let schema_set = SchemaSet::new();
        let ns_a = schema_set.name_table.add("http://a");
        s.insert(Some(ns_a));

        let enum_a = CanonicalNs::Enum(s.clone());
        let result = intersect_canonical_ns(&CanonicalNs::Any, &enum_a);
        assert_eq!(result, enum_a);
        let result2 = intersect_canonical_ns(&enum_a, &CanonicalNs::Any);
        assert_eq!(result2, enum_a);
    }

    #[test]
    fn test_intersect_enum_enum_is_set_intersection() {
        let schema_set = SchemaSet::new();
        let ns_a = schema_set.name_table.add("http://a");
        let ns_b = schema_set.name_table.add("http://b");
        let ns_c = schema_set.name_table.add("http://c");

        let mut s1 = std::collections::HashSet::new();
        s1.insert(Some(ns_a));
        s1.insert(Some(ns_b));
        let mut s2 = std::collections::HashSet::new();
        s2.insert(Some(ns_b));
        s2.insert(Some(ns_c));

        let result = intersect_canonical_ns(&CanonicalNs::Enum(s1), &CanonicalNs::Enum(s2));
        match result {
            CanonicalNs::Enum(set) => {
                assert_eq!(set.len(), 1);
                assert!(set.contains(&Some(ns_b)));
            }
            _ => panic!("expected Enum"),
        }
    }

    #[test]
    fn test_intersect_enum_not_is_set_difference() {
        let schema_set = SchemaSet::new();
        let ns_a = schema_set.name_table.add("http://a");
        let ns_b = schema_set.name_table.add("http://b");

        let mut s = std::collections::HashSet::new();
        s.insert(Some(ns_a));
        s.insert(Some(ns_b));
        let mut n = std::collections::HashSet::new();
        n.insert(Some(ns_b));

        let result = intersect_canonical_ns(&CanonicalNs::Enum(s), &CanonicalNs::Not(n));
        match result {
            CanonicalNs::Enum(set) => {
                assert_eq!(set.len(), 1);
                assert!(set.contains(&Some(ns_a)));
            }
            _ => panic!("expected Enum"),
        }
    }

    #[test]
    fn test_intersect_not_not_is_union_of_exclusions() {
        let schema_set = SchemaSet::new();
        let ns_a = schema_set.name_table.add("http://a");
        let ns_b = schema_set.name_table.add("http://b");

        let mut n1 = std::collections::HashSet::new();
        n1.insert(Some(ns_a));
        let mut n2 = std::collections::HashSet::new();
        n2.insert(Some(ns_b));

        let result = intersect_canonical_ns(&CanonicalNs::Not(n1), &CanonicalNs::Not(n2));
        match result {
            CanonicalNs::Not(set) => {
                assert_eq!(set.len(), 2);
                assert!(set.contains(&Some(ns_a)));
                assert!(set.contains(&Some(ns_b)));
            }
            _ => panic!("expected Not"),
        }
    }

    #[test]
    fn test_canonical_ns_subset_various() {
        let schema_set = SchemaSet::new();
        let ns_a = schema_set.name_table.add("http://a");
        let ns_b = schema_set.name_table.add("http://b");

        let empty_set = std::collections::HashSet::new();
        let mut s_a = std::collections::HashSet::new();
        s_a.insert(Some(ns_a));
        let mut s_ab = std::collections::HashSet::new();
        s_ab.insert(Some(ns_a));
        s_ab.insert(Some(ns_b));

        // Anything ⊆ Any
        assert!(canonical_ns_subset(&CanonicalNs::Any, &CanonicalNs::Any));
        assert!(canonical_ns_subset(
            &CanonicalNs::Enum(s_a.clone()),
            &CanonicalNs::Any
        ));
        assert!(canonical_ns_subset(
            &CanonicalNs::Not(s_a.clone()),
            &CanonicalNs::Any
        ));

        // Any ⊄ non-Any
        assert!(!canonical_ns_subset(
            &CanonicalNs::Any,
            &CanonicalNs::Enum(s_a.clone())
        ));

        // Enum(s) ⊆ Enum(t) iff s ⊆ t
        assert!(canonical_ns_subset(
            &CanonicalNs::Enum(s_a.clone()),
            &CanonicalNs::Enum(s_ab.clone()),
        ));
        assert!(!canonical_ns_subset(
            &CanonicalNs::Enum(s_ab.clone()),
            &CanonicalNs::Enum(s_a.clone()),
        ));

        // Enum(s) ⊆ Not(n) iff s ∩ n = ∅
        assert!(canonical_ns_subset(
            &CanonicalNs::Enum(s_a.clone()),
            &CanonicalNs::Not(empty_set.clone()),
        ));
        assert!(!canonical_ns_subset(
            &CanonicalNs::Enum(s_a.clone()),
            &CanonicalNs::Not(s_a.clone()),
        ));

        // Not(n1) ⊆ Not(n2) iff n2 ⊆ n1 (derived exclusion must be ≥ base)
        assert!(canonical_ns_subset(
            &CanonicalNs::Not(s_ab.clone()),
            &CanonicalNs::Not(s_a.clone()),
        ));
        assert!(!canonical_ns_subset(
            &CanonicalNs::Not(s_a.clone()),
            &CanonicalNs::Not(s_ab.clone()),
        ));

        // Not(n) ⊄ Enum(s) — infinite cannot fit in finite
        assert!(!canonical_ns_subset(
            &CanonicalNs::Not(empty_set),
            &CanonicalNs::Enum(s_a),
        ));
    }

    #[test]
    fn test_effective_attribute_wildcard_absent_no_groups_returns_none() {
        let schema_set = SchemaSet::new();
        let result = effective_attribute_wildcard(&schema_set, None, None, &[]);
        assert!(matches!(result, Ok(None)));
    }

    #[test]
    fn test_effective_attribute_wildcard_local_only() {
        let schema_set = SchemaSet::new();
        let wc = wildcard_with_ns(WildcardNamespace::Any);
        let result = effective_attribute_wildcard(&schema_set, Some(&wc), None, &[]).unwrap();
        let eff = result.expect("expected Some");
        assert!(matches!(eff.namespace, CanonicalNs::Any));
    }

    #[test]
    fn test_effective_attribute_wildcard_intersects_across_group_and_local() {
        use crate::arenas::AttributeGroupData;
        use crate::parser::frames::NamespaceToken;

        let mut schema_set = SchemaSet::new();
        let ns_a = schema_set.name_table.add("http://a");
        let ns_b = schema_set.name_table.add("http://b");

        // Referenced group has wildcard List[a, b]
        let group_wc = WildcardResult {
            namespace: WildcardNamespace::List(vec![
                NamespaceToken::Uri(ns_a),
                NamespaceToken::Uri(ns_b),
            ]),
            process_contents: ProcessContents::Strict,
            not_namespace: Vec::new(),
            not_qname: Vec::new(),
            id: None,
            annotation: None,
            source: None,
        };
        let group = AttributeGroupData {
            name: None,
            target_namespace: None,
            ref_name: None,
            attributes: Vec::new(),
            attribute_groups: Vec::new(),
            attribute_wildcard: Some(group_wc),
            id: None,
            annotation: None,
            source: None,
            resolved_ref: None,
            resolved_attribute_groups: Vec::new(),
            resolved_attributes: Vec::new(),
            redefine_original: None,
            redefine_requires_restriction_check: false,
        };
        let group_key = schema_set.arenas.alloc_attribute_group(group);

        // Local wildcard is List[a]. Intersection should be {a}.
        let local = WildcardResult {
            namespace: WildcardNamespace::List(vec![NamespaceToken::Uri(ns_a)]),
            process_contents: ProcessContents::Strict,
            not_namespace: Vec::new(),
            not_qname: Vec::new(),
            id: None,
            annotation: None,
            source: None,
        };
        let result =
            effective_attribute_wildcard(&schema_set, Some(&local), None, &[group_key]).unwrap();
        let eff = result.expect("expected Some");
        match eff.namespace {
            CanonicalNs::Enum(set) => {
                assert_eq!(set.len(), 1);
                assert!(set.contains(&Some(ns_a)));
            }
            other => panic!("expected Enum({{ns_a}}), got {:?}", other),
        }
    }

    #[test]
    fn test_effective_attribute_wildcard_no_local_uses_first_group_pc() {
        use crate::arenas::AttributeGroupData;

        let mut schema_set = SchemaSet::new();
        let group_wc = WildcardResult {
            namespace: WildcardNamespace::Any,
            process_contents: ProcessContents::Lax,
            not_namespace: Vec::new(),
            not_qname: Vec::new(),
            id: None,
            annotation: None,
            source: None,
        };
        let group = AttributeGroupData {
            name: None,
            target_namespace: None,
            ref_name: None,
            attributes: Vec::new(),
            attribute_groups: Vec::new(),
            attribute_wildcard: Some(group_wc),
            id: None,
            annotation: None,
            source: None,
            resolved_ref: None,
            resolved_attribute_groups: Vec::new(),
            resolved_attributes: Vec::new(),
            redefine_original: None,
            redefine_requires_restriction_check: false,
        };
        let group_key = schema_set.arenas.alloc_attribute_group(group);

        // No local ⇒ pc comes from W[0] (Lax).
        let result = effective_attribute_wildcard(&schema_set, None, None, &[group_key]).unwrap();
        let eff = result.expect("expected Some");
        assert_eq!(eff.process_contents, ProcessContents::Lax);
        assert!(matches!(eff.namespace, CanonicalNs::Any));
    }

    #[test]
    fn test_effective_wildcard_allows_attribute_basic() {
        let schema_set = SchemaSet::new();
        let name = schema_set.name_table.add("foo");
        let ns_a = schema_set.name_table.add("http://a");

        let any_eff = EffectiveAttributeWildcard {
            namespace: CanonicalNs::Any,
            not_qname: Vec::new(),
            process_contents: ProcessContents::Strict,
        };
        assert!(effective_wildcard_allows_attribute(
            &schema_set,
            &any_eff,
            Some(ns_a),
            name,
        ));

        let mut s = std::collections::HashSet::new();
        s.insert(Some(ns_a));
        let enum_eff = EffectiveAttributeWildcard {
            namespace: CanonicalNs::Enum(s),
            not_qname: Vec::new(),
            process_contents: ProcessContents::Strict,
        };
        assert!(effective_wildcard_allows_attribute(
            &schema_set,
            &enum_eff,
            Some(ns_a),
            name,
        ));
        assert!(!effective_wildcard_allows_attribute(
            &schema_set,
            &enum_eff,
            None,
            name,
        ));
    }

    #[test]
    fn test_effective_wildcard_restricts_enforces_subset() {
        let schema_set = SchemaSet::new();
        let ns_a = schema_set.name_table.add("http://a");
        let ns_b = schema_set.name_table.add("http://b");

        let mut s_a = std::collections::HashSet::new();
        s_a.insert(Some(ns_a));
        let mut s_ab = std::collections::HashSet::new();
        s_ab.insert(Some(ns_a));
        s_ab.insert(Some(ns_b));

        let derived_narrow = EffectiveAttributeWildcard {
            namespace: CanonicalNs::Enum(s_a.clone()),
            not_qname: Vec::new(),
            process_contents: ProcessContents::Strict,
        };
        let base_wide = EffectiveAttributeWildcard {
            namespace: CanonicalNs::Enum(s_ab.clone()),
            not_qname: Vec::new(),
            process_contents: ProcessContents::Strict,
        };

        assert!(
            effective_attribute_wildcard_restricts(&schema_set, &derived_narrow, &base_wide)
                .is_ok()
        );
        assert!(
            effective_attribute_wildcard_restricts(&schema_set, &base_wide, &derived_narrow)
                .is_err()
        );
    }

    #[test]
    fn test_effective_wildcard_restricts_enforces_process_contents() {
        let schema_set = SchemaSet::new();
        let skip_eff = EffectiveAttributeWildcard {
            namespace: CanonicalNs::Any,
            not_qname: Vec::new(),
            process_contents: ProcessContents::Skip,
        };
        let strict_eff = EffectiveAttributeWildcard {
            namespace: CanonicalNs::Any,
            not_qname: Vec::new(),
            process_contents: ProcessContents::Strict,
        };
        // Strict restricts Skip (tightening).
        assert!(
            effective_attribute_wildcard_restricts(&schema_set, &strict_eff, &skip_eff).is_ok()
        );
        // Skip cannot restrict Strict (loosening).
        assert!(
            effective_attribute_wildcard_restricts(&schema_set, &skip_eff, &strict_eff).is_err()
        );
    }

    #[test]
    fn test_validate_attribute_restriction_rejects_added_wildcard() {
        // Base has no wildcard, derived adds Any — invalid restriction.
        let mut schema_set = SchemaSet::new();
        let base = create_complex_type_data(None);
        let base_key = schema_set.arenas.alloc_complex_type(base);

        let mut derived = create_complex_type_data(None);
        derived.attribute_wildcard = Some(wildcard_with_ns(WildcardNamespace::Any));
        derived.derivation_method = Some(DerivationMethod::Restriction);
        derived.resolved_base_type = Some(TypeKey::Complex(base_key));
        let derived_key = schema_set.arenas.alloc_complex_type(derived);

        let derived_ref = schema_set.arenas.complex_types.get(derived_key).unwrap();
        let base_ref = schema_set.arenas.complex_types.get(base_key).unwrap();
        let result = validate_attribute_restriction(&schema_set, derived_ref, base_ref);
        assert!(result.is_err());
        if let Err(SchemaError::StructuralError {
            constraint,
            message,
            ..
        }) = result
        {
            assert_eq!(constraint, "derivation-ok-restriction");
            assert!(
                message.contains("wildcard"),
                "message should mention wildcard, got: {}",
                message
            );
        } else {
            panic!("expected StructuralError");
        }
    }

    #[test]
    fn test_validate_attribute_restriction_accepts_narrower_wildcard() {
        use crate::parser::frames::NamespaceToken;
        let mut schema_set = SchemaSet::new();
        let ns_a = schema_set.name_table.add("http://a");

        let mut base = create_complex_type_data(None);
        base.attribute_wildcard = Some(wildcard_with_ns(WildcardNamespace::Any));
        let base_key = schema_set.arenas.alloc_complex_type(base);

        let mut derived = create_complex_type_data(None);
        derived.attribute_wildcard = Some(wildcard_with_ns(WildcardNamespace::List(vec![
            NamespaceToken::Uri(ns_a),
        ])));
        derived.derivation_method = Some(DerivationMethod::Restriction);
        derived.resolved_base_type = Some(TypeKey::Complex(base_key));
        let derived_key = schema_set.arenas.alloc_complex_type(derived);

        let derived_ref = schema_set.arenas.complex_types.get(derived_key).unwrap();
        let base_ref = schema_set.arenas.complex_types.get(base_key).unwrap();
        assert!(validate_attribute_restriction(&schema_set, derived_ref, base_ref).is_ok());
    }

    #[test]
    fn test_validate_attribute_restriction_allows_removing_wildcard() {
        // Base has Any, derived removes the wildcard — always valid
        // (restriction may remove the wildcard).
        let mut schema_set = SchemaSet::new();
        let mut base = create_complex_type_data(None);
        base.attribute_wildcard = Some(wildcard_with_ns(WildcardNamespace::Any));
        let base_key = schema_set.arenas.alloc_complex_type(base);

        let mut derived = create_complex_type_data(None);
        derived.derivation_method = Some(DerivationMethod::Restriction);
        derived.resolved_base_type = Some(TypeKey::Complex(base_key));
        let derived_key = schema_set.arenas.alloc_complex_type(derived);

        let derived_ref = schema_set.arenas.complex_types.get(derived_key).unwrap();
        let base_ref = schema_set.arenas.complex_types.get(base_key).unwrap();
        assert!(validate_attribute_restriction(&schema_set, derived_ref, base_ref).is_ok());
    }

    #[test]
    fn test_redefine_attribute_group_rejects_broader_wildcard() {
        // Original has Any; redefined "restriction" keeps Any + adds a
        // broader-than-original effective wildcard via added scope —
        // emulated here by giving the redefined side a wildcard that
        // excludes fewer namespaces than the original (via not_namespace).
        use crate::arenas::AttributeGroupData;
        use crate::parser::frames::NamespaceToken;

        let mut schema_set = SchemaSet::new();
        let ns_a = schema_set.name_table.add("http://a");

        // Original wildcard: Any with not_namespace=[ns_a]  ⇒  Not({ns_a})
        let mut original_wc = wildcard_with_ns(WildcardNamespace::Any);
        original_wc.not_namespace = vec![NamespaceToken::Uri(ns_a)];
        let original = AttributeGroupData {
            name: None,
            target_namespace: None,
            ref_name: None,
            attributes: Vec::new(),
            attribute_groups: Vec::new(),
            attribute_wildcard: Some(original_wc),
            id: None,
            annotation: None,
            source: None,
            resolved_ref: None,
            resolved_attribute_groups: Vec::new(),
            resolved_attributes: Vec::new(),
            redefine_original: None,
            redefine_requires_restriction_check: false,
        };
        let original_key = schema_set.arenas.alloc_attribute_group(original);

        // Derived wildcard: plain Any (allows ns_a, which original excludes).
        let derived = AttributeGroupData {
            name: None,
            target_namespace: None,
            ref_name: None,
            attributes: Vec::new(),
            attribute_groups: Vec::new(),
            attribute_wildcard: Some(wildcard_with_ns(WildcardNamespace::Any)),
            id: None,
            annotation: None,
            source: None,
            resolved_ref: None,
            resolved_attribute_groups: Vec::new(),
            resolved_attributes: Vec::new(),
            redefine_original: Some(original_key),
            redefine_requires_restriction_check: true,
        };
        schema_set.arenas.alloc_attribute_group(derived);

        let mut errors = Vec::new();
        let mut stats = DerivationStats::default();
        validate_all_redefine_attribute_group_restrictions(&schema_set, &mut errors, &mut stats);

        assert!(
            !errors.is_empty(),
            "expected a src-redefine.7.2.2 error for broader derived wildcard"
        );
        let msg = match &errors[0] {
            SchemaError::StructuralError {
                constraint,
                message,
                ..
            } => {
                assert_eq!(*constraint, "src-redefine.7.2.2");
                message.clone()
            }
            _ => panic!("expected StructuralError"),
        };
        assert!(
            msg.contains("wildcard") || msg.contains("restriction"),
            "error should mention wildcard restriction, got: {}",
            msg
        );
    }

    #[test]
    fn test_redefine_attribute_group_effective_wildcard_admits_inherited_attr() {
        // Original attribute group references a nested group whose local
        // wildcard is List[ns_a]. The redefined group adds an attribute in
        // ns_a. Without the §3.6.2.2 effective-wildcard fix, this would
        // fail: the original's *local* attribute_wildcard is None, so the
        // old code would reject ns_a even though the inherited wildcard
        // admits it.
        use crate::arenas::{AttributeGroupData, ResolvedAttributeUse};
        use crate::parser::frames::{
            AttributeFrameResult, AttributeUseKind as AuK, AttributeUseResult, NamespaceToken,
        };

        let mut schema_set = SchemaSet::new();
        let ns_a = schema_set.name_table.add("http://a");
        let attr_name = schema_set.name_table.add("foo");

        // Nested group with wildcard List[ns_a].
        let nested_wc = WildcardResult {
            namespace: WildcardNamespace::List(vec![NamespaceToken::Uri(ns_a)]),
            process_contents: ProcessContents::Strict,
            not_namespace: Vec::new(),
            not_qname: Vec::new(),
            id: None,
            annotation: None,
            source: None,
        };
        let nested = AttributeGroupData {
            name: None,
            target_namespace: None,
            ref_name: None,
            attributes: Vec::new(),
            attribute_groups: Vec::new(),
            attribute_wildcard: Some(nested_wc),
            id: None,
            annotation: None,
            source: None,
            resolved_ref: None,
            resolved_attribute_groups: Vec::new(),
            resolved_attributes: Vec::new(),
            redefine_original: None,
            redefine_requires_restriction_check: false,
        };
        let nested_key = schema_set.arenas.alloc_attribute_group(nested);

        // Original group: no local wildcard, references nested.
        let original = AttributeGroupData {
            name: None,
            target_namespace: None,
            ref_name: None,
            attributes: Vec::new(),
            attribute_groups: Vec::new(),
            attribute_wildcard: None,
            id: None,
            annotation: None,
            source: None,
            resolved_ref: None,
            resolved_attribute_groups: vec![nested_key],
            resolved_attributes: Vec::new(),
            redefine_original: None,
            redefine_requires_restriction_check: false,
        };
        let original_key = schema_set.arenas.alloc_attribute_group(original);

        // Redefined group: adds an attribute in ns_a, inherits the nested
        // wildcard through the same reference chain.
        let attr_use = AttributeUseResult {
            attribute: AttributeFrameResult {
                name: Some(attr_name),
                ref_name: None,
                target_namespace: Some(ns_a),
                type_ref: None,
                inline_type: None,
                default_value: None,
                fixed_value: None,
                use_kind: None,
                form: None,
                inheritable: false,
                id: None,
                annotation: None,
                source: None,
            },
            use_kind: AuK::Optional,
        };
        let derived = AttributeGroupData {
            name: None,
            target_namespace: None,
            ref_name: None,
            attributes: vec![attr_use],
            attribute_groups: Vec::new(),
            attribute_wildcard: None,
            id: None,
            annotation: None,
            source: None,
            resolved_ref: None,
            resolved_attribute_groups: vec![nested_key],
            resolved_attributes: vec![ResolvedAttributeUse {
                resolved_type: None,
                resolved_ref: None,
            }],
            redefine_original: Some(original_key),
            redefine_requires_restriction_check: true,
        };
        schema_set.arenas.alloc_attribute_group(derived);

        let mut errors = Vec::new();
        let mut stats = DerivationStats::default();
        validate_all_redefine_attribute_group_restrictions(&schema_set, &mut errors, &mut stats);

        assert!(
            errors.is_empty(),
            "attribute admitted by inherited effective wildcard should not error; got: {:?}",
            errors
                .iter()
                .map(|e| format!("{:?}", e))
                .collect::<Vec<_>>()
        );
    }
}
