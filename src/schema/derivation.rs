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
use crate::ids::{ComplexTypeKey, NameId, SimpleTypeKey, TypeKey};
use crate::parser::frames::{DerivationMethod, SimpleTypeVariety};
#[cfg(feature = "xsd11")]
use crate::parser::frames::{OpenContentMode, OpenContentResult, ProcessContents, WildcardNamespace, WildcardResult};
use crate::parser::location::SourceRef;
use crate::schema::dependencies::DependencyGraph;
use crate::schema::SchemaSet;
use crate::schema::model::DerivationSet;
use crate::types::facets::FacetSet;

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

    // Return first error if any
    if let Some(first_error) = errors.into_iter().next() {
        return Err(first_error);
    }

    Ok(stats)
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

    match type_def.variety {
        SimpleTypeVariety::Atomic => {
            // Atomic types are derived by restriction
            validate_simple_restriction(schema_set, type_def, stats)?;
        }
        SimpleTypeVariety::List => {
            stats.list_types_validated += 1;
            validate_simple_list(schema_set, type_def)?;
        }
        SimpleTypeVariety::Union => {
            stats.union_types_validated += 1;
            validate_simple_union(schema_set, type_def)?;
        }
    }

    Ok(())
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

    stats.restrictions_validated += 1;

    // Get base type facets
    let base_facets = get_type_facets(schema_set, base_key)?;

    // Validate that derived facets are more restrictive
    if let Some(ref base_facets) = base_facets {
        // FacetSet.merge_with_base validates derivation rules
        type_def
            .facets
            .merge_with_base(base_facets)
            .map_err(|e| {
                let location = type_def.source.as_ref().and_then(|s| schema_set.source_maps.locate(s));
                let type_name = format_type_name(schema_set, type_def.name, type_def.target_namespace);
                SchemaError::structural(
                    "cos-st-restricts",
                    format!("Simple type '{}' has invalid restriction: {}", type_name, e),
                    location,
                )
            })?;
    }

    Ok(())
}

/// Validate simple type list derivation
///
/// Constraint: cos-list-of-atomic (List item type must be atomic)
fn validate_simple_list(
    schema_set: &SchemaSet,
    type_def: &crate::arenas::SimpleTypeDefData,
) -> SchemaResult<()> {
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
            if let Some(item_type) = schema_set.arenas.simple_types.get(simple_key) {
                match item_type.variety {
                    SimpleTypeVariety::Atomic => {
                        // Valid - atomic types are OK
                    }
                    SimpleTypeVariety::List => {
                        // Invalid - list of list is not allowed
                        let location = type_def.source.as_ref().and_then(|s| schema_set.source_maps.locate(s));
                        let type_name = format_type_name(schema_set, type_def.name, type_def.target_namespace);
                        return Err(SchemaError::structural(
                            "cos-list-of-atomic",
                            format!("List type '{}' has list item type, which is not allowed", type_name),
                            location,
                        ));
                    }
                    SimpleTypeVariety::Union => {
                        // Must check that union doesn't contain list members
                        if union_contains_list(schema_set, item_type) {
                            let location = type_def.source.as_ref().and_then(|s| schema_set.source_maps.locate(s));
                            let type_name = format_type_name(schema_set, type_def.name, type_def.target_namespace);
                            return Err(SchemaError::structural(
                                "cos-list-of-atomic",
                                format!("List type '{}' has union item type containing list member", type_name),
                                location,
                            ));
                        }
                    }
                }
            }
        }
        TypeKey::Complex(_) => {
            // Complex types cannot be list item types
            let location = type_def.source.as_ref().and_then(|s| schema_set.source_maps.locate(s));
            let type_name = format_type_name(schema_set, type_def.name, type_def.target_namespace);
            return Err(SchemaError::structural(
                "cos-list-of-atomic",
                format!("List type '{}' has complex item type, which is not allowed", type_name),
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
    // All member types must be simple types
    for member_key in &type_def.resolved_member_types {
        match member_key {
            TypeKey::Simple(_) => {
                // Valid - simple types are OK
            }
            TypeKey::Complex(_) => {
                // Invalid - complex types cannot be union members
                let location = type_def.source.as_ref().and_then(|s| schema_set.source_maps.locate(s));
                let type_name = format_type_name(schema_set, type_def.name, type_def.target_namespace);
                return Err(SchemaError::structural(
                    "cos-union-memberTypes",
                    format!("Union type '{}' has complex member type, which is not allowed", type_name),
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
            validate_complex_extension(schema_set, type_def)?;
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
    type_def: &crate::arenas::ComplexTypeDefData,
) -> SchemaResult<()> {
    // Get base type
    let base_key = match type_def.resolved_base_type {
        Some(key) => key,
        None => return Ok(()), // No base type
    };

    // Check that base type exists and is accessible
    match base_key {
        TypeKey::Simple(_) => {
            // Extension from simple type is valid (simpleContent)
            // The derived type must have simpleContent
        }
        TypeKey::Complex(base_complex_key) => {
            if let Some(base_type) = schema_set.arenas.complex_types.get(base_complex_key) {
                // Check that base type is not final for extension
                if effective_type_final(schema_set, base_type.final_derivation, base_type.source.as_ref())
                    .contains_extension()
                {
                    let location = type_def.source.as_ref().and_then(|s| schema_set.source_maps.locate(s));
                    let type_name = format_type_name(schema_set, type_def.name, type_def.target_namespace);
                    let base_name = format_type_name(schema_set, base_type.name, base_type.target_namespace);
                    return Err(SchemaError::structural(
                        "cos-ct-extends",
                        format!(
                            "Complex type '{}' cannot extend '{}' because base type is final for extension",
                            type_name, base_name
                        ),
                        location,
                    ));
                }

                // XSD 1.1: Validate open-content compatibility
                #[cfg(feature = "xsd11")]
                validate_open_content_extension(schema_set, type_def, base_type)?;
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
        TypeKey::Simple(_) => {
            // Restriction of simple type is not typically valid for complex types
            // unless using simpleContent
        }
        TypeKey::Complex(base_complex_key) => {
            if let Some(base_type) = schema_set.arenas.complex_types.get(base_complex_key) {
                // Check that base type is not final for restriction
                if effective_type_final(schema_set, base_type.final_derivation, base_type.source.as_ref())
                    .contains_restriction()
                {
                    let location = type_def.source.as_ref().and_then(|s| schema_set.source_maps.locate(s));
                    let type_name = format_type_name(schema_set, type_def.name, type_def.target_namespace);
                    let base_name = format_type_name(schema_set, base_type.name, base_type.target_namespace);
                    return Err(SchemaError::structural(
                        "derivation-ok-restriction",
                        format!(
                            "Complex type '{}' cannot restrict '{}' because base type is final for restriction",
                            type_name, base_name
                        ),
                        location,
                    ));
                }

                // XSD 1.1: Validate open-content compatibility
                #[cfg(feature = "xsd11")]
                validate_open_content_restriction(schema_set, type_def, base_type)?;
            }
        }
    }

    // Note: Full content model restriction validation (particle restriction, attribute
    // subsetting) is complex and requires comparing content models. This will be
    // implemented in Phase 4 (Content Model Compilation).

    Ok(())
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
#[cfg(feature = "xsd11")]
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
#[cfg(feature = "xsd11")]
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
#[cfg(feature = "xsd11")]
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
#[cfg(feature = "xsd11")]
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
                        Some(resolved) => {
                            resolved.iter().all(|ns| !base_excluded.contains(ns))
                        }
                        None => false,
                    }
                }
            }
        }

        WildcardNamespace::TargetNamespace | WildcardNamespace::Local
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
#[cfg(feature = "xsd11")]
fn is_wildcard_ns_subset(
    derived: &WildcardResult,
    derived_target_ns: Option<NameId>,
    base: &WildcardResult,
    base_target_ns: Option<NameId>,
) -> bool {
    // Namespace constraint must be a subset
    if !is_namespace_subset(
        &derived.namespace, derived_target_ns,
        &base.namespace, base_target_ns,
    ) {
        return false;
    }

    // notNamespace: derived must exclude at least everything base excludes.
    // Resolve tokens before comparing so that ##targetNamespace and an
    // explicit URI are treated as equivalent.
    for base_excl in &base.not_namespace {
        let base_ns = base_excl.resolve(base_target_ns);
        let found = derived.not_namespace.iter().any(|d| d.resolve(derived_target_ns) == base_ns);
        if !found {
            return false;
        }
    }

    // notQName: derived must exclude at least everything base excludes
    for item in &base.not_qname {
        if !derived.not_qname.contains(item) {
            return false;
        }
    }

    true
}

/// Validate open-content compatibility for complex type extension (cos-ct-extends).
///
/// Rules:
/// - If base has no OC, derived may freely add OC.
/// - If base has OC, derived must also have OC.
/// - Suffix cannot extend interleave.
/// - Derived wildcard must be a superset of base wildcard.
#[cfg(feature = "xsd11")]
fn validate_open_content_extension(
    schema_set: &SchemaSet,
    derived: &crate::arenas::ComplexTypeDefData,
    base: &crate::arenas::ComplexTypeDefData,
) -> SchemaResult<()> {
    let base_oc = effective_open_content(base.open_content.as_ref());
    let derived_oc = effective_open_content(derived.open_content.as_ref());

    // If base has no open content, derived may freely add — OK
    let Some(base_oc) = base_oc else { return Ok(()); };

    let location = derived.source.as_ref().and_then(|s| schema_set.source_maps.locate(s));
    let type_name = format_type_name(schema_set, derived.name, derived.target_namespace);
    let base_name = format_type_name(schema_set, base.name, base.target_namespace);

    // Derived must also have open content when base has it
    let Some(derived_oc) = derived_oc else {
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

    // Mode: suffix cannot extend interleave
    if base_oc.mode == OpenContentMode::Interleave
        && derived_oc.mode == OpenContentMode::Suffix
    {
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

    // Wildcard: derived must be superset of base (i.e. base ns-constraint ⊆ derived)
    if let (Some(base_wc), Some(derived_wc)) =
        (base_oc.wildcard.as_ref(), derived_oc.wildcard.as_ref())
    {
        if !is_wildcard_ns_subset(
            base_wc, base.target_namespace,
            derived_wc, derived.target_namespace,
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

/// Validate open-content compatibility for complex type restriction (derivation-ok-restriction).
///
/// Rules:
/// - If base has no OC, derived must not add OC.
/// - If base has OC but derived doesn't — OK (restriction removes it).
/// - Interleave cannot restrict suffix.
/// - Derived wildcard must be a subset of base wildcard.
#[cfg(feature = "xsd11")]
fn validate_open_content_restriction(
    schema_set: &SchemaSet,
    derived: &crate::arenas::ComplexTypeDefData,
    base: &crate::arenas::ComplexTypeDefData,
) -> SchemaResult<()> {
    let base_oc = effective_open_content(base.open_content.as_ref());
    let derived_oc = effective_open_content(derived.open_content.as_ref());

    // If base has no open content, derived must not add one
    if base_oc.is_none() && derived_oc.is_some() {
        let location = derived.source.as_ref().and_then(|s| schema_set.source_maps.locate(s));
        let type_name = format_type_name(schema_set, derived.name, derived.target_namespace);
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

    let location = derived.source.as_ref().and_then(|s| schema_set.source_maps.locate(s));
    let type_name = format_type_name(schema_set, derived.name, derived.target_namespace);
    let base_name = format_type_name(schema_set, base.name, base.target_namespace);

    // Mode: if base is suffix, derived cannot use interleave
    if base_oc.mode == OpenContentMode::Suffix
        && derived_oc.mode == OpenContentMode::Interleave
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
            derived_wc, derived.target_namespace,
            base_wc, base.target_namespace,
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

/// Get facets for a type (works for both simple and complex types)
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
fn format_type_name(
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

fn effective_type_final(
    schema_set: &SchemaSet,
    final_derivation: DerivationSet,
    source: Option<&SourceRef>,
) -> DerivationSet {
    if !final_derivation.is_empty() {
        return final_derivation;
    }

    let Some(source) = source else {
        return final_derivation;
    };

    schema_set
        .documents
        .get(source.doc_id as usize)
        .map(|doc| doc.final_default)
        .unwrap_or(final_derivation)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arenas::{ComplexTypeDefData, SimpleTypeDefData};
    use crate::parser::frames::ComplexContentResult;
    use crate::parser::location::{SourceRef, SourceSpan};
    use crate::schema::model::SchemaDocument;
    use crate::schema::model::DerivationSet;

    fn create_simple_type_data(name: Option<NameId>, variety: SimpleTypeVariety) -> SimpleTypeDefData {
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
        let mut schema_set = SchemaSet::new();
        let doc_id = schema_set.documents.len() as crate::ids::DocumentId;
        let mut doc = SchemaDocument::new(doc_id, "test.xsd".to_string());
        doc.final_default = DerivationSet::extension();
        schema_set.documents.push(doc);

        // Create base complex type with final from schema default.
        let mut base_data = create_complex_type_data(None);
        base_data.source = Some(SourceRef::new(doc_id, SourceSpan::new(0, 0)));
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
            OpenContentMode::Interleave, WildcardNamespace::Any, ProcessContents::Lax,
        ));
        let base_key = schema_set.arenas.alloc_complex_type(base_data);

        let mut derived_data = create_complex_type_data(None);
        derived_data.derivation_method = Some(DerivationMethod::Extension);
        derived_data.resolved_base_type = Some(TypeKey::Complex(base_key));
        derived_data.open_content = Some(make_open_content(
            OpenContentMode::Suffix, WildcardNamespace::Any, ProcessContents::Lax,
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
            OpenContentMode::Interleave, WildcardNamespace::Any, ProcessContents::Lax,
        ));
        let base_key = schema_set.arenas.alloc_complex_type(base_data);

        let mut derived_data = create_complex_type_data(None);
        derived_data.derivation_method = Some(DerivationMethod::Extension);
        derived_data.resolved_base_type = Some(TypeKey::Complex(base_key));
        derived_data.open_content = Some(make_open_content(
            OpenContentMode::Interleave, WildcardNamespace::Any, ProcessContents::Lax,
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

        let mut schema_set = SchemaSet::new();

        let mut base_data = create_complex_type_data(None);
        base_data.open_content = Some(make_open_content(
            OpenContentMode::Interleave, WildcardNamespace::Any, ProcessContents::Lax,
        ));
        let base_key = schema_set.arenas.alloc_complex_type(base_data);

        let mut derived_data = create_complex_type_data(None);
        derived_data.derivation_method = Some(DerivationMethod::Extension);
        derived_data.resolved_base_type = Some(TypeKey::Complex(base_key));
        // No open_content on derived
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
            OpenContentMode::Interleave, WildcardNamespace::Any, ProcessContents::Lax,
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
            OpenContentMode::Interleave, WildcardNamespace::Any, ProcessContents::Lax,
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
            OpenContentMode::Interleave, WildcardNamespace::Any, ProcessContents::Lax,
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
    fn test_restriction_interleave_cannot_restrict_suffix() {
        use crate::parser::frames::{OpenContentMode, ProcessContents, WildcardNamespace};

        let mut schema_set = SchemaSet::new();

        let mut base_data = create_complex_type_data(None);
        base_data.open_content = Some(make_open_content(
            OpenContentMode::Suffix, WildcardNamespace::Any, ProcessContents::Lax,
        ));
        let base_key = schema_set.arenas.alloc_complex_type(base_data);

        let mut derived_data = create_complex_type_data(None);
        derived_data.derivation_method = Some(DerivationMethod::Restriction);
        derived_data.resolved_base_type = Some(TypeKey::Complex(base_key));
        derived_data.open_content = Some(make_open_content(
            OpenContentMode::Interleave, WildcardNamespace::Any, ProcessContents::Lax,
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
    fn test_restriction_suffix_restricts_interleave_valid() {
        use crate::parser::frames::{OpenContentMode, ProcessContents, WildcardNamespace};

        let mut schema_set = SchemaSet::new();

        let mut base_data = create_complex_type_data(None);
        base_data.open_content = Some(make_open_content(
            OpenContentMode::Interleave, WildcardNamespace::Any, ProcessContents::Lax,
        ));
        let base_key = schema_set.arenas.alloc_complex_type(base_data);

        let mut derived_data = create_complex_type_data(None);
        derived_data.derivation_method = Some(DerivationMethod::Restriction);
        derived_data.resolved_base_type = Some(TypeKey::Complex(base_key));
        derived_data.open_content = Some(make_open_content(
            OpenContentMode::Suffix, WildcardNamespace::Any, ProcessContents::Lax,
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
            &WildcardNamespace::Local, None,
            &WildcardNamespace::Other, Some(urn_a),
        );
        assert!(!result, "##local must NOT be a subset of ##other (absent is excluded)");
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
            &WildcardNamespace::Other, None,
            &WildcardNamespace::Other, Some(urn_a),
        );
        assert!(!result, "##other(tns=None) must NOT be a subset of ##other(tns=urn:a)");
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
            &WildcardNamespace::Other, Some(urn_a),
            &WildcardNamespace::Other, None,
        );
        assert!(result, "##other(tns=urn:a) MUST be a subset of ##other(tns=None)");
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
        assert!(!result, "List containing base's target ns must NOT be a subset of ##other");
    }
}
