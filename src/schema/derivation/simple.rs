//! Simple-type derivation: variety dispatch, applicable-facet and
//! facet-value checks against the base type, and the restriction / list /
//! union rules (`cos-st-restricts`, `cos-list-of-atomic`,
//! `cos-union-memberTypes`).

use super::{format_type_name, type_error_context, DerivationStats};
use crate::error::{SchemaError, SchemaResult};
use crate::ids::{SimpleTypeKey, TypeKey};
use crate::parser::frames::SimpleTypeVariety;
use crate::parser::location::SourceLocation;
use crate::schema::SchemaSet;
use crate::types::facets::{FacetKind, FacetSet};

/// Validate a simple type definition
pub(super) fn validate_simple_type(
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

        // cos-st-restricts.1.1: an atomic restriction's {base type definition}
        // must be an atomic simple type or a built-in primitive.
        // xs:anySimpleType has no {variety}, so a user-defined
        // <xs:restriction base="xs:anySimpleType"/> is invalid (msData
        // addB110, stZ005/stZ006/stZ011). Built-in primitives themselves
        // never reach this check — they carry no resolved base.
        if base_simple_key == schema_set.builtin_types().any_simple_type {
            let (location, type_name) = type_error_context(schema_set, type_def);
            return Err(SchemaError::structural(
                "cos-st-restricts",
                format!(
                    "Simple type '{}' cannot restrict xs:anySimpleType directly \
                     (cos-st-restricts.1.1: base must be atomic or a built-in primitive)",
                    type_name
                ),
                location,
            ));
        }
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

    // XSD 1.0 strict anyURI lexical rules for enumeration facet values
    // (msData anyURI_a003/b004/b006: bad scheme, bare `%`, `^`, `\`).
    // XSD 1.1 dropped the RFC 2396 tie, so this is version-gated; instance
    // values keep the permissive path.
    if schema_set.is_xsd10() {
        if let (TypeKey::Simple(base_sk), Some(enum_facet)) =
            (base_key, type_def.facets.enumeration.as_ref())
        {
            if schema_set.derives_from(base_sk, schema_set.builtin_types().any_uri) {
                for value in &enum_facet.values {
                    if !crate::types::validators::is_strict_xsd10_anyuri_enum_value(value) {
                        return Err(SchemaError::structural(
                            "enumeration-valid-restriction",
                            format!(
                                "Enumeration value '{}' in type '{}' is not a valid xs:anyURI \
                                 (XSD 1.0 / RFC 2396 lexical rules)",
                                value, type_name
                            ),
                            location.clone(),
                        ));
                    }
                }
            }
        }
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
