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
use crate::ids::{ComplexTypeKey, ElementKey, NameId, SimpleTypeKey, TypeKey};
use crate::parser::frames::{
    ComplexContentResult, Compositor, DerivationMethod, ElementFrameResult, ModelGroupDefResult,
    ParticleResult, ParticleTerm, ProcessContents, SimpleTypeVariety, WildcardNamespace,
    WildcardResult,
};
#[cfg(feature = "xsd11")]
use crate::parser::frames::{OpenContentMode, OpenContentResult};
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
        }
        SimpleTypeVariety::Union => {
            stats.union_types_validated += 1;
            validate_simple_union(schema_set, type_def)?;
        }
    }

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
                let location = type_def.source.as_ref().and_then(|s| schema_set.source_maps.locate(s));
                let type_name = format_type_name(schema_set, type_def.name, type_def.target_namespace);
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
                let location = type_def.source.as_ref().and_then(|s| schema_set.source_maps.locate(s));
                let type_name = format_type_name(schema_set, type_def.name, type_def.target_namespace);
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
            // Atomic types: all facets potentially applicable (depends on base type)
        }
    }

    Ok(())
}

/// List inapplicable facet names for list types
fn list_inapplicable_facets_for_list(facets: &FacetSet) -> String {
    let mut names = Vec::new();
    if facets.min_inclusive.is_some() { names.push("minInclusive"); }
    if facets.max_inclusive.is_some() { names.push("maxInclusive"); }
    if facets.min_exclusive.is_some() { names.push("minExclusive"); }
    if facets.max_exclusive.is_some() { names.push("maxExclusive"); }
    if facets.total_digits.is_some() { names.push("totalDigits"); }
    if facets.fraction_digits.is_some() { names.push("fractionDigits"); }
    if facets.explicit_timezone.is_some() { names.push("explicitTimezone"); }
    names.join(", ")
}

/// List inapplicable facet names for union types
fn list_inapplicable_facets_for_union(facets: &FacetSet) -> String {
    let mut names = Vec::new();
    if facets.length.is_some() { names.push("length"); }
    if facets.min_length.is_some() { names.push("minLength"); }
    if facets.max_length.is_some() { names.push("maxLength"); }
    if facets.whitespace.is_some() { names.push("whiteSpace"); }
    if facets.min_inclusive.is_some() { names.push("minInclusive"); }
    if facets.max_inclusive.is_some() { names.push("maxInclusive"); }
    if facets.min_exclusive.is_some() { names.push("minExclusive"); }
    if facets.max_exclusive.is_some() { names.push("maxExclusive"); }
    if facets.total_digits.is_some() { names.push("totalDigits"); }
    if facets.fraction_digits.is_some() { names.push("fractionDigits"); }
    if facets.explicit_timezone.is_some() { names.push("explicitTimezone"); }
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

    stats.restrictions_validated += 1;

    // Check that base type is not final for restriction
    if let TypeKey::Simple(base_simple_key) = base_key {
        if let Some(base_type) = schema_set.arenas.simple_types.get(base_simple_key) {
            let effective_final = effective_type_final(
                schema_set,
                base_type.final_derivation,
                base_type.source.as_ref(),
            );
            if effective_final.contains_restriction() {
                let location = type_def.source.as_ref().and_then(|s| schema_set.source_maps.locate(s));
                let type_name = format_type_name(schema_set, type_def.name, type_def.target_namespace);
                let base_name = format_type_name(schema_set, base_type.name, base_type.target_namespace);
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
    // Check that the item type is not final for list derivation
    if let Some(TypeKey::Simple(item_simple_key)) = type_def.resolved_item_type {
        if let Some(item_type) = schema_set.arenas.simple_types.get(item_simple_key) {
            let effective_final = effective_type_final(
                schema_set,
                item_type.final_derivation,
                item_type.source.as_ref(),
            );
            if effective_final.contains_list() {
                let location = type_def.source.as_ref().and_then(|s| schema_set.source_maps.locate(s));
                let type_name = format_type_name(schema_set, type_def.name, type_def.target_namespace);
                let item_name = format_type_name(schema_set, item_type.name, item_type.target_namespace);
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
            let effective_final = effective_type_final(
                schema_set,
                base_type.final_derivation,
                base_type.source.as_ref(),
            );
            if effective_final.contains_list() {
                let location = type_def.source.as_ref().and_then(|s| schema_set.source_maps.locate(s));
                let type_name = format_type_name(schema_set, type_def.name, type_def.target_namespace);
                let base_name = format_type_name(schema_set, base_type.name, base_type.target_namespace);
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
    // Check that member types are not final for union derivation
    for member_key in &type_def.resolved_member_types {
        if let TypeKey::Simple(simple_key) = member_key {
            if let Some(member_type) = schema_set.arenas.simple_types.get(*simple_key) {
                let effective_final = effective_type_final(
                    schema_set,
                    member_type.final_derivation,
                    member_type.source.as_ref(),
                );
                if effective_final.contains_union() {
                    let location = type_def.source.as_ref().and_then(|s| schema_set.source_maps.locate(s));
                    let type_name = format_type_name(schema_set, type_def.name, type_def.target_namespace);
                    let member_name = format_type_name(schema_set, member_type.name, member_type.target_namespace);
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
        TypeKey::Simple(base_simple_key) => {
            // Extension from simple type is valid only with simpleContent
            // complexContent extension from simple type is invalid
            if matches!(type_def.content, ComplexContentResult::Complex(_)) {
                let location = type_def.source.as_ref().and_then(|s| schema_set.source_maps.locate(s));
                let type_name = format_type_name(schema_set, type_def.name, type_def.target_namespace);
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
                let effective_final = effective_type_final(
                    schema_set,
                    base_type.final_derivation,
                    base_type.source.as_ref(),
                );
                if effective_final.contains_extension() {
                    let location = type_def.source.as_ref().and_then(|s| schema_set.source_maps.locate(s));
                    let type_name = format_type_name(schema_set, type_def.name, type_def.target_namespace);
                    let base_name = format_type_name(schema_set, base_type.name, base_type.target_namespace);
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

                // cos-ct-extends: Cannot use complexContent extension to add particles
                // to a base type with simpleContent
                if matches!(base_type.content, ComplexContentResult::Simple(_)) {
                    if let ComplexContentResult::Complex(ref complex) = type_def.content {
                        // Extension adds a content model particle — invalid
                        if complex.particle.is_some() {
                            let location = type_def.source.as_ref().and_then(|s| schema_set.source_maps.locate(s));
                            let type_name = format_type_name(schema_set, type_def.name, type_def.target_namespace);
                            let base_name = format_type_name(schema_set, base_type.name, base_type.target_namespace);
                            return Err(SchemaError::structural(
                                "cos-ct-extends",
                                format!(
                                    "Complex type '{}' cannot use complexContent to extend '{}' which has simpleContent with element content",
                                    type_name, base_name,
                                ),
                                location,
                            ));
                        }
                    }
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
        TypeKey::Simple(base_simple_key) => {
            // ct-props-correct.2: If the base type is a simple type definition,
            // the derivation method must be extension (not restriction).
            let location = type_def.source.as_ref().and_then(|s| schema_set.source_maps.locate(s));
            let type_name = format_type_name(schema_set, type_def.name, type_def.target_namespace);
            let base_name = if let Some(base_type) = schema_set.arenas.simple_types.get(base_simple_key) {
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

                validate_content_particle_restriction(schema_set, type_def, base_type)?;

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

    fn normalize_particle(&mut self, particle: &ParticleResult) -> SchemaResult<NormalizedParticle> {
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
        let block = if !elem.block.is_empty() {
            elem.block
        } else {
            source
                .and_then(|s| {
                    let doc_id = s.schema_defaults_doc.unwrap_or(s.doc_id);
                    self.schema_set
                        .documents
                        .get(doc_id as usize)
                        .map(|d| d.block_default)
                })
                .unwrap_or_default()
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

fn resolve_element_type_ref(
    schema_set: &SchemaSet,
    elem: &ElementFrameResult,
) -> Option<TypeKey> {
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
        (Some(_), None) => Err(SchemaError::structural(
            "derivation-ok-restriction",
            format!(
                "Complex type '{}' adds particle content while restricting '{}' which has empty content",
                type_name, base_name
            ),
            location,
        )),
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

            // A top-level particle with maxOccurs=0 is pointless (§3.8) — treat as empty
            if derived_particle.max_occurs == Some(0) || is_empty_group(&derived_particle) {
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
) -> (&'a crate::arenas::ComplexTypeDefData, Option<&'a ParticleResult>) {
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
    let particle = flatten_same_compositor_groups(particle);
    Ok(expand_bounded_repeated_sequences(particle))
}

/// Expand bounded repeated sequences into atomic repetition units.
///
/// `sequence(a,b){min,max}` becomes:
/// ```text
/// sequence {
///   sequence(a,b){1,1},  // × min  (required copies)
///   sequence(a,b){0,1},  // × (max - min)  (optional copies)
/// }
/// ```
///
/// Each copy is a nested sequence with occurs {1,1} or {0,1}, keeping
/// the repetition unit atomic (you can't take `a` without `b`).
///
/// Only expands when `max_occurs` is finite, ≤ 10, and total children
/// after expansion ≤ 50.
fn expand_bounded_repeated_sequences(mut particle: NormalizedParticle) -> NormalizedParticle {
    // First, recurse into children
    if let NormalizedParticleTerm::Group(group) = &mut particle.term {
        group.particles = group
            .particles
            .drain(..)
            .map(expand_bounded_repeated_sequences)
            .collect();
    }

    // Then check if this particle itself should be expanded
    let NormalizedParticleTerm::Group(ref group) = particle.term else {
        return particle;
    };
    if group.compositor != Compositor::Sequence {
        return particle;
    }
    let child_count = group.particles.len();
    if child_count <= 1 {
        return particle; // single-child already handled by existing code
    }
    let Some(max) = particle.max_occurs else {
        return particle; // unbounded — can't expand
    };
    if max <= 1 {
        return particle; // not repeated
    }
    if max > 10 || (max as usize) * child_count > 50 {
        return particle; // too large to expand safely
    }

    let min = particle.min_occurs;
    let inner_group = match particle.term {
        NormalizedParticleTerm::Group(g) => g,
        _ => unreachable!(),
    };
    let source = particle.source.clone();

    let mut copies = Vec::with_capacity(max as usize);

    // Required copies (min_occurs=1, max_occurs=1)
    for _ in 0..min {
        copies.push(NormalizedParticle {
            term: NormalizedParticleTerm::Group(inner_group.clone()),
            min_occurs: 1,
            max_occurs: Some(1),
            source: source.clone(),
        });
    }

    // Optional copies (min_occurs=0, max_occurs=1)
    for _ in min..max {
        copies.push(NormalizedParticle {
            term: NormalizedParticleTerm::Group(inner_group.clone()),
            min_occurs: 0,
            max_occurs: Some(1),
            source: source.clone(),
        });
    }

    NormalizedParticle {
        term: NormalizedParticleTerm::Group(NormalizedGroup {
            compositor: Compositor::Sequence,
            particles: copies,
        }),
        min_occurs: 1,
        max_occurs: Some(1),
        source,
    }
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

    occurs_is_unit(group_min_occurs, group_max_occurs)
        || child.max_occurs == Some(1)
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

fn particle_restricts(
    schema_set: &SchemaSet,
    derived: &NormalizedParticle,
    base: &NormalizedParticle,
) -> bool {
    // XSD 1.0: A non-choice optional particle cannot restrict an optional non-repeated
    // multi-branch choice. The expand_choice_branches approach merges choice occurs into
    // branches, which gives wrong results for RecurseLax when max_occurs=1.
    // For repeated choices (max>1), the spec is ambiguous — provisionally accept.
    if schema_set.xsd_version == crate::schema::model::XsdVersion::V1_0
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
            if schema_set.xsd_version == crate::schema::model::XsdVersion::V1_0 {
                return choice_branches_restrict_ordered(
                    schema_set,
                    &derived_branches,
                    &base_branches,
                );
            }
            return derived_branches.iter().all(|branch| {
                base_branches
                    .iter()
                    .any(|candidate| particle_restricts(schema_set, branch, candidate))
            });
        }

        // Sequence-vs-choice: dedicated handler instead of "any branch" check.
        if let NormalizedParticleTerm::Group(derived_group) = &derived.term {
            if derived_group.compositor == Compositor::Sequence {
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
            // 4. fixed value: if base is fixed, derived must be fixed with same value
            && match &base_element.fixed_value {
                None => true,
                Some(base_fixed) => derived_element.fixed_value.as_ref() == Some(base_fixed),
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
        ) => {
            group_particle_restricts_wildcard(derived, derived_group, base, base_wildcard)
        }
        (
            NormalizedParticleTerm::Group(derived_group),
            NormalizedParticleTerm::Group(base_group),
        ) if derived_group.compositor == base_group.compositor =>
        {
            if !occurs_range_is_subset(
                derived.min_occurs,
                derived.max_occurs,
                base.min_occurs,
                base.max_occurs,
            ) {
                return false;
            }
            match derived_group.compositor {
                Compositor::Sequence => {
                    sequence_particles_restrict(
                        schema_set,
                        &derived_group.particles,
                        &base_group.particles,
                    )
                }
                Compositor::All => {
                    all_particles_restrict(schema_set, &derived_group.particles, &base_group.particles)
                }
                Compositor::Choice => unreachable!("choice particles are handled earlier"),
            }
        }
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
            all_particles_restrict(schema_set, &derived_group.particles, &base_group.particles)
        }
        // recurseAsIfGroup: wrap derived element/wildcard in an implicit group{1,1}
        // and check outer occurs before delegating to sequence/all matching.
        (NormalizedParticleTerm::Element(_) | NormalizedParticleTerm::Wildcard(_),
         NormalizedParticleTerm::Group(base_group))
            if base_group.compositor == Compositor::Sequence =>
        {
            occurs_range_is_subset(1, Some(1), base.min_occurs, base.max_occurs)
                && sequence_particles_restrict(
                    schema_set,
                    std::slice::from_ref(derived),
                    &base_group.particles,
                )
        }
        (NormalizedParticleTerm::Element(_) | NormalizedParticleTerm::Wildcard(_),
         NormalizedParticleTerm::Group(base_group))
            if base_group.compositor == Compositor::All =>
        {
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
            Compositor::Sequence | Compositor::All => group.particles.iter().fold(
                (0u32, Some(0u32)),
                |(acc_min, acc_max), child| {
                    let (child_min, child_max) = particle_total_occurrence_range(child);
                    (
                        acc_min.saturating_add(child_min),
                        add_optional_occurs(acc_max, child_max),
                    )
                },
            ),
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

fn particle_fits_wildcard(
    particle: &NormalizedParticle,
    wildcard: &NormalizedWildcard,
) -> bool {
    if let Some(branches) = expand_choice_branches(particle) {
        return branches
            .iter()
            .all(|branch| particle_fits_wildcard(branch, wildcard));
    }

    match &particle.term {
        NormalizedParticleTerm::Element(element) => wildcard_allows_element(element, wildcard),
        NormalizedParticleTerm::Wildcard(derived_wildcard) => wildcard_restricts(derived_wildcard, wildcard),
        NormalizedParticleTerm::Group(group) => group_particles_fit_wildcard(&group.particles, wildcard),
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
                if base_group.compositor == Compositor::Sequence
                    && !base_group.particles.is_empty()
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

            // 3. Skip emptiable base particles.
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

fn all_particles_restrict(
    schema_set: &SchemaSet,
    derived_particles: &[NormalizedParticle],
    base_particles: &[NormalizedParticle],
) -> bool {
    // XSD 1.0: All:All uses order-preserving Recurse (same as Sequence:Sequence).
    // XSD 1.1: RecurseUnordered allows reordering via backtracking.
    if schema_set.xsd_version == crate::schema::model::XsdVersion::V1_0 {
        return sequence_particles_restrict(schema_set, derived_particles, base_particles);
    }

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
            if used[base_index] || !particle_restricts(schema_set, &derived_particles[derived_index], base_particle) {
                continue;
            }
            used[base_index] = true;
            if backtrack(schema_set, derived_particles, base_particles, used, derived_index + 1) {
                return true;
            }
            used[base_index] = false;
        }

        false
    }

    let mut used = vec![false; base_particles.len()];
    backtrack(schema_set, derived_particles, base_particles, &mut used, 0)
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

fn wildcard_allows_element(
    element: &NormalizedElement,
    wildcard: &NormalizedWildcard,
) -> bool {
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

    !wildcard_not_qname_excludes(&wildcard.wildcard.not_qname, element.namespace, element.name)
}

fn wildcard_namespace_matches(
    namespace: &WildcardNamespace,
    element_namespace: Option<NameId>,
    target_namespace: Option<NameId>,
) -> bool {
    match namespace {
        WildcardNamespace::Any => true,
        WildcardNamespace::Other => !other_exclusion_set(target_namespace).contains(&element_namespace),
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
