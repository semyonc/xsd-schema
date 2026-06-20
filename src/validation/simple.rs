//! Simple type value validation
//!
//! Validates text content against XSD simple types (atomic, list, union).
//! Reuses `VALIDATOR_REGISTRY` for lexical validation and `FacetSet` for
//! facet constraint checking.

use crate::ids::{SimpleTypeKey, TypeKey};
use crate::parser::frames::{ComplexContentResult, DerivationMethod, SimpleTypeVariety};
use crate::schema::SchemaSet;
use crate::types::facets::{normalize_whitespace, FacetSet, WhitespaceMode};
use crate::types::validators::VALIDATOR_REGISTRY;
use crate::types::value::{XmlValue, XmlValueKind};
use crate::types::XmlTypeCode;

use super::errors::{self, ValidationError};

/// Result of simple type validation
#[derive(Debug)]
pub struct SimpleTypeResult {
    /// The typed value produced by validation
    pub typed_value: XmlValue,
    /// For union types: the member type that matched
    pub member_type: Option<TypeKey>,
    /// The whitespace-normalized value (PSVI `[schema normalized value]`)
    pub normalized_value: Option<String>,
}

/// Validate a string value against a simple type.
///
/// Dispatches to atomic, list, or union validation depending on the type's variety.
/// For complex types with simpleContent, walks the base type chain to find the
/// underlying simple type.
pub fn validate_simple_type(
    value: &str,
    type_key: TypeKey,
    schema_set: &SchemaSet,
) -> Result<SimpleTypeResult, ValidationError> {
    validate_simple_type_inner(value, type_key, schema_set)
}

/// Value-space equality for fixed/default values (§3.3.4.3 clause 5.2.2.2.2, §3.2.4.4).
///
/// Fast-path lexical match; otherwise parses both strings against `type_key`
/// and compares their typed values. Returns `false` when `type_key` is `None`
/// or when either parse fails.
pub fn fixed_values_equal(
    a: &str,
    b: &str,
    type_key: Option<TypeKey>,
    schema_set: &SchemaSet,
) -> bool {
    if a == b {
        return true;
    }
    let Some(tk) = type_key else {
        return false;
    };
    let Ok(a_result) = validate_simple_type(a, tk, schema_set) else {
        return false;
    };
    let Ok(b_result) = validate_simple_type(b, tk, schema_set) else {
        return false;
    };
    a_result.typed_value == b_result.typed_value
}

/// Value-space equality when the instance's typed value is already computed.
///
/// Avoids re-parsing `instance_raw`; only parses `fixed` on lexical mismatch.
/// Returns `false` when `type_key` is `None` or when parsing `fixed` fails.
pub fn fixed_matches_typed(
    instance_raw: &str,
    instance_typed: &XmlValue,
    fixed: &str,
    type_key: Option<TypeKey>,
    schema_set: &SchemaSet,
) -> bool {
    if instance_raw == fixed {
        return true;
    }
    let Some(tk) = type_key else {
        return false;
    };
    let Ok(fixed_result) = validate_simple_type(fixed, tk, schema_set) else {
        return false;
    };
    *instance_typed == fixed_result.typed_value
}

fn validate_simple_type_inner(
    value: &str,
    type_key: TypeKey,
    schema_set: &SchemaSet,
) -> Result<SimpleTypeResult, ValidationError> {
    match type_key {
        TypeKey::Simple(sk) => {
            let st_data = match schema_set.arenas.simple_types.get(sk) {
                Some(d) => d,
                None => {
                    return Err(errors::error(
                        "cvc-simple-type",
                        "Simple type definition not found",
                        None,
                    ));
                }
            };
            match st_data.variety {
                SimpleTypeVariety::Atomic => validate_atomic_type(value, sk, schema_set),
                SimpleTypeVariety::List => validate_list_type(value, sk, schema_set),
                SimpleTypeVariety::Union => validate_union_type(value, sk, schema_set),
            }
        }
        TypeKey::Complex(ck) => {
            // §3.4.2.2 clause 1.1: the inline <xs:simpleType> inside
            // simpleContent/restriction is the content type when present;
            // otherwise inherit by walking resolved_base_type.
            let ct_data = match schema_set.arenas.complex_types.get(ck) {
                Some(d) => d,
                None => {
                    return Err(errors::error(
                        "cvc-simple-type",
                        "Complex type definition not found",
                        None,
                    ));
                }
            };
            let result = if let Some(inline_st) = ct_data.resolved_simple_content_type {
                validate_simple_type_inner(value, inline_st, schema_set)?
            } else if let Some(base_key) = ct_data.resolved_base_type {
                validate_simple_type_inner(value, base_key, schema_set)?
            } else {
                // No base type — treat as anySimpleType (accept any value)
                SimpleTypeResult {
                    typed_value: XmlValue::untyped(value),
                    member_type: None,
                    normalized_value: None,
                }
            };
            // §3.4.2.2 clause 1.2: a simpleContent restriction may declare
            // local string-applicable facets (length / pattern / enumeration)
            // that further constrain the recursively validated value. The
            // base/inline call already applied the base type's facets; here we
            // apply only the restriction's local set. Extension cannot add
            // facets, so it is excluded.
            if let ComplexContentResult::Simple(sc) = &ct_data.content {
                if sc.derivation == DerivationMethod::Restriction && !sc.facets.is_empty() {
                    let lex = result.normalized_value.as_deref().unwrap_or(value);
                    sc.facets.validate_string(lex).map_err(|e| {
                        let code = errors::facet_constraint_code(&e);
                        errors::from_facet_error(code, e, None)
                    })?;
                }
            }
            Ok(result)
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Walk the resolved_base_type chain from `sk` until we find a built-in type code.
/// Returns `None` if no built-in ancestor is found (cycle guard at depth 100).
fn resolve_type_code(sk: SimpleTypeKey, schema_set: &SchemaSet) -> Option<XmlTypeCode> {
    let mut current = sk;
    for _ in 0..100 {
        if let Some(code) = schema_set.get_type_code(current) {
            return Some(code);
        }
        let st_data = schema_set.arenas.simple_types.get(current)?;
        match st_data.resolved_base_type {
            Some(TypeKey::Simple(base)) => current = base,
            _ => return None,
        }
    }
    None
}

/// Walk the simple-type derivation chain and return `true` if any
/// ancestor (including `sk` itself) declares an `enumeration` facet.
/// `FacetSet::inherit_from` does not propagate enumeration through base
/// types, so [`collect_facets`] alone cannot answer this question.
fn has_enumeration_in_chain(sk: SimpleTypeKey, schema_set: &SchemaSet) -> bool {
    let mut current = sk;
    for _ in 0..100 {
        let Some(st_data) = schema_set.arenas.simple_types.get(current) else {
            return false;
        };
        if st_data.facets.enumeration.is_some() {
            return true;
        }
        match st_data.resolved_base_type {
            Some(TypeKey::Simple(base)) => current = base,
            _ => return false,
        }
    }
    false
}

/// Collect the effective facet set for a simple type by walking the derivation chain.
///
/// Starts from the most-derived type's facets, then inherits from each base type.
/// `inherit_from` only fills missing values, so derived facets take priority.
fn collect_facets(sk: SimpleTypeKey, schema_set: &SchemaSet) -> FacetSet {
    let st_data = match schema_set.arenas.simple_types.get(sk) {
        Some(d) => d,
        None => return FacetSet::new(),
    };
    let mut facets = st_data.facets.clone();

    // Walk the base type chain
    let mut current_base = st_data.resolved_base_type;
    for _ in 0..100 {
        match current_base {
            Some(TypeKey::Simple(base_sk)) => {
                if let Some(base_data) = schema_set.arenas.simple_types.get(base_sk) {
                    facets.inherit_from(&base_data.facets);
                    current_base = base_data.resolved_base_type;
                } else {
                    break;
                }
            }
            _ => break,
        }
    }
    facets
}

// ---------------------------------------------------------------------------
// Atomic
// ---------------------------------------------------------------------------

fn validate_atomic_type(
    value: &str,
    sk: SimpleTypeKey,
    schema_set: &SchemaSet,
) -> Result<SimpleTypeResult, ValidationError> {
    let type_code = resolve_type_code(sk, schema_set);

    // Short-circuit for abstract base types — accept as untyped
    match type_code {
        Some(XmlTypeCode::AnySimpleType) | Some(XmlTypeCode::AnyAtomicType) | None => {
            return Ok(SimpleTypeResult {
                typed_value: XmlValue::untyped(value),
                member_type: None,
                normalized_value: None,
            });
        }
        _ => {}
    }
    let type_code = type_code.unwrap();

    let validator = match VALIDATOR_REGISTRY.get_by_code(type_code) {
        Some(v) => v,
        None => {
            return Err(errors::error(
                "cvc-datatype-valid",
                format!("No validator for type code {:?}", type_code),
                None,
            ));
        }
    };

    let facets = collect_facets(sk, schema_set);

    // XSD Part 2 §3.2 / W3C bug 14388: `xs:NOTATION` is only usable via
    // restriction with an `enumeration` facet binding the value to a
    // declared notation. The schema may legally reference `xs:NOTATION`
    // directly, but any instance value must fail because there is no
    // enumeration to match against.
    if type_code == XmlTypeCode::Notation && !has_enumeration_in_chain(sk, schema_set) {
        return Err(errors::error(
            "cvc-datatype-valid",
            "xs:NOTATION cannot be used directly for instance validation; \
             only datatypes derived from NOTATION by restriction with an \
             enumeration facet are permitted (§3.2)",
            None,
        ));
    }

    // Compute normalized value (PSVI [schema normalized value])
    let effective_ws = facets
        .whitespace
        .as_ref()
        .map(|w| w.value)
        .unwrap_or_else(|| validator.whitespace());
    let normalized = normalize_whitespace(value, effective_ws);

    let typed_value = if facets.is_empty() {
        validator.validate(value)
    } else {
        validator.validate_with_facets(value, &facets)
    };

    match typed_value {
        Ok(mut val) => {
            // Propagate schema type so XPath2 sequence type matching works
            if val.schema_type.is_none() {
                val.schema_type = Some(sk);
            }
            // XSD 1.0 §3.2.7.2: year zero ("0000") is not allowed in dateTime,
            // date, gYear, or gYearMonth lexical forms. XSD 1.1 explicitly
            // permits 0000 as the representation of 1 BCE.
            // XSD 1.0 §3.2.4 / §3.2.5 also omit `+INF` from the special
            // float / double lexicals enumerated by XSD 1.1 §3.3.16/17.
            if schema_set.is_xsd10() {
                check_xsd10_year_zero(&val, value)?;
                check_xsd10_plus_inf(&val, value)?;
            }
            // XSD 1.1: evaluate assertion facets
            #[cfg(feature = "xsd11")]
            evaluate_assertion_facets(&val, &facets, schema_set, Some(sk))?;
            Ok(SimpleTypeResult {
                typed_value: val,
                member_type: None,
                normalized_value: Some(normalized),
            })
        }
        Err(type_err) => {
            let code = errors::value_error_constraint_code(&type_err);
            Err(errors::from_value_error(code, type_err, None))
        }
    }
}

/// XSD 1.0 §3.2.4 / §3.2.5: `+INF` is not in the lexical space (added in
/// Datatypes 1.1). The collapsed value is checked against the original
/// lexical form (after whitespace collapse, since `xs:float` / `xs:double`
/// declare `whiteSpace = collapse`).
fn check_xsd10_plus_inf(val: &XmlValue, raw: &str) -> Result<(), ValidationError> {
    use crate::types::value::XmlAtomicValue;
    let is_float_or_double = matches!(
        val.value,
        XmlValueKind::Atomic(XmlAtomicValue::Float(_))
            | XmlValueKind::Atomic(XmlAtomicValue::Double(_))
    );
    if !is_float_or_double {
        return Ok(());
    }
    let collapsed = crate::types::facets::normalize_whitespace(
        raw,
        crate::types::facets::WhitespaceMode::Collapse,
    );
    if collapsed == "+INF" {
        Err(errors::error(
            "cvc-datatype-valid",
            format!(
                "'+INF' is not a valid XSD 1.0 float/double lexical form (value '{}')",
                raw
            ),
            None,
        ))
    } else {
        Ok(())
    }
}

/// XSD 1.0 §3.2.7.2: reject year 0000 in dateTime / date / gYear / gYearMonth.
fn check_xsd10_year_zero(val: &XmlValue, raw: &str) -> Result<(), ValidationError> {
    use crate::types::value::XmlAtomicValue;
    let year_zero = match &val.value {
        XmlValueKind::Atomic(XmlAtomicValue::DateTime(v)) => v.year == 0,
        XmlValueKind::Atomic(XmlAtomicValue::Date(v)) => v.year == 0,
        XmlValueKind::Atomic(XmlAtomicValue::GYear(v)) => v.year == 0,
        XmlValueKind::Atomic(XmlAtomicValue::GYearMonth(v)) => v.year == 0,
        _ => false,
    };
    if year_zero {
        Err(errors::error(
            "cvc-datatype-valid",
            format!(
                "Year 0000 is not a valid XSD 1.0 lexical form (value '{}'); use '-0001' for 1 BCE",
                raw
            ),
            None,
        ))
    } else {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// List
// ---------------------------------------------------------------------------

fn validate_list_type(
    value: &str,
    sk: SimpleTypeKey,
    schema_set: &SchemaSet,
) -> Result<SimpleTypeResult, ValidationError> {
    let st_data = match schema_set.arenas.simple_types.get(sk) {
        Some(d) => d,
        None => {
            return Err(errors::error(
                "cvc-simple-type",
                "List type definition not found",
                None,
            ));
        }
    };

    let item_type_key = match st_data.resolved_item_type {
        Some(tk) => tk,
        None => {
            if let Some(deferred) = &st_data.deferred_item_type_error {
                return Err(errors::error("src-resolve", deferred.message.clone(), None));
            }
            // No item type resolved — cannot validate list items, accept as untyped
            return Ok(SimpleTypeResult {
                typed_value: XmlValue::untyped(value),
                member_type: None,
                normalized_value: None,
            });
        }
    };

    // Normalize whitespace (collapse) and split
    let normalized = normalize_whitespace(value, WhitespaceMode::Collapse);
    let items_str: Vec<&str> = if normalized.is_empty() {
        Vec::new()
    } else {
        normalized.split(' ').collect()
    };

    // Validate each item
    let mut items = Vec::with_capacity(items_str.len());
    let mut item_type_code = XmlTypeCode::UntypedAtomic;
    for item_str in &items_str {
        let result = validate_simple_type_inner(item_str, item_type_key, schema_set)?;
        item_type_code = result.typed_value.type_code;
        match result.typed_value.value {
            XmlValueKind::Atomic(atom) => items.push(atom),
            XmlValueKind::UntypedAtomic(s) => {
                items.push(crate::types::value::XmlAtomicValue::String(s));
            }
            _ => {
                items.push(crate::types::value::XmlAtomicValue::String(
                    result.typed_value.to_string_value(),
                ));
            }
        }
    }

    // Check list-level facets (length, minLength, maxLength, pattern, enumeration)
    let facets = collect_facets(sk, schema_set);
    if !facets.is_empty() {
        // Length constraints on list are item count
        facets
            .validate_list_length(items_str.len() as u64)
            .map_err(|e| {
                let code = errors::facet_constraint_code(&e);
                errors::from_facet_error(code, e, None)
            })?;

        // Pattern/enumeration on the normalized string representation
        facets
            .validate_string_patterns_enums(&normalized)
            .map_err(|e| {
                let code = errors::facet_constraint_code(&e);
                errors::from_facet_error(code, e, None)
            })?;
    }

    // Use the list type's own code (e.g. IdRefs, NmTokens, Entities) when it
    // has a built-in one, so that downstream consumers like collect_id_idref
    // can distinguish list types from their item types.
    let list_type_code = resolve_type_code(sk, schema_set)
        .filter(|code| code.is_list())
        .unwrap_or(item_type_code);

    let typed_value = XmlValue::with_schema_type(
        list_type_code,
        sk,
        XmlValueKind::List {
            item_type: item_type_code,
            items,
        },
    );

    // XSD 1.1: evaluate assertion facets ($value is the sequence of list items)
    #[cfg(feature = "xsd11")]
    {
        let item_sk = item_type_key.as_simple();
        evaluate_assertion_facets(&typed_value, &facets, schema_set, item_sk)?;
    }

    Ok(SimpleTypeResult {
        typed_value,
        member_type: None,
        normalized_value: Some(normalized),
    })
}

// ---------------------------------------------------------------------------
// Union
// ---------------------------------------------------------------------------

fn validate_union_type(
    value: &str,
    sk: SimpleTypeKey,
    schema_set: &SchemaSet,
) -> Result<SimpleTypeResult, ValidationError> {
    let st_data = match schema_set.arenas.simple_types.get(sk) {
        Some(d) => d,
        None => {
            return Err(errors::error(
                "cvc-simple-type",
                "Union type definition not found",
                None,
            ));
        }
    };

    // Try each member type in order
    for &member_key in &st_data.resolved_member_types {
        if let Ok(result) = validate_simple_type_inner(value, member_key, schema_set) {
            // Match found — check union-level facets (pattern, enumeration)
            let facets = collect_facets(sk, schema_set);
            if !facets.is_empty() {
                let check_value = result.typed_value.to_string_value();
                // §cvc-pattern-valid: patterns apply to the LEXICAL form (original value),
                // not the canonical/parsed form (e.g. "-.5" must match "\-\.\d+" not "-0.5")
                let lex_value = normalize_whitespace(value, WhitespaceMode::Collapse);
                facets.validate_patterns_only(&lex_value).map_err(|e| {
                    let code = errors::facet_constraint_code(&e);
                    errors::from_facet_error(code, e, None)
                })?;
                // Enumeration checked in value space: re-parse each enum value as the
                // matched primitive type and compare canonical forms (handles float/
                // double rounding and duration leading-zero differences)
                let type_code = result.typed_value.type_code;
                facets
                    .validate_enum_value_space(
                        |s| {
                            VALIDATOR_REGISTRY
                                .validate(type_code, s)
                                .map(|v| v.to_string_value() == check_value)
                                .unwrap_or(false)
                        },
                        &check_value,
                    )
                    .map_err(|e| {
                        let code = errors::facet_constraint_code(&e);
                        errors::from_facet_error(code, e, None)
                    })?;
            }

            // Propagate schema type on inner value from the matched member type
            let mut inner = result.typed_value;
            if inner.schema_type.is_none() {
                inner.schema_type = member_key.as_simple();
            }

            // XSD 1.1: evaluate assertion facets ($value is the member-validated value)
            #[cfg(feature = "xsd11")]
            {
                let item_sk = member_key.as_simple().and_then(|sk| {
                    crate::types::sequence::resolve_list_item_schema_type(sk, schema_set)
                });
                evaluate_assertion_facets(&inner, &facets, schema_set, item_sk)?;
            }

            return Ok(SimpleTypeResult {
                normalized_value: result.normalized_value,
                typed_value: XmlValue::with_schema_type(
                    inner.type_code,
                    sk,
                    XmlValueKind::Union(Box::new(inner)),
                ),
                member_type: Some(member_key),
            });
        }
    }

    // No member matched
    Err(errors::error(
        "cvc-simple-type",
        format!(
            "Value '{}' does not match any member type of the union",
            value
        ),
        None,
    ))
}

// ---------------------------------------------------------------------------
// XSD 1.1: Assertion facet evaluation
// ---------------------------------------------------------------------------

#[cfg(feature = "xsd11")]
fn resolve_assertion_default_ns(
    raw: Option<&str>,
    source: Option<&crate::parser::location::SourceRef>,
    ns_snapshot: &crate::namespace::context::NamespaceContextSnapshot,
    schema_set: &SchemaSet,
) -> Option<crate::ids::NameId> {
    // Look up the schema document that defines this assertion
    let doc = source.and_then(|s| schema_set.documents.get(s.doc_id as usize));

    // Cascade: facet-level > schema-level (of the defining document)
    let effective = if let Some(raw) = raw {
        Some(raw.to_string())
    } else {
        doc.and_then(|d| d.xpath_default_namespace)
            .map(|id| schema_set.name_table.resolve(id))
    };

    match effective.as_deref() {
        Some("##defaultNamespace") => ns_snapshot.default_ns,
        Some("##targetNamespace") => doc.and_then(|d| d.target_namespace),
        Some("##local") | None => None,
        Some(uri) => Some(schema_set.name_table.add(uri)),
    }
}

/// Evaluate assertion facets against a typed value (XSD 1.1).
///
/// Assertion facets on simpleType restrictions receive only the `$value`
/// variable (the typed value being validated). No context node or DOM
/// access is needed, so this evaluates inline during streaming validation.
#[cfg(feature = "xsd11")]
fn evaluate_assertion_facets(
    typed_value: &XmlValue,
    facets: &FacetSet,
    schema_set: &SchemaSet,
    item_schema_type: Option<SimpleTypeKey>,
) -> Result<(), ValidationError> {
    use crate::xpath::api::XPathExpr;
    use crate::xpath::functions::effective_boolean_value;
    use crate::xpath::{RoXmlNavigator, XPathContext};

    if facets.assertions.is_empty() {
        return Ok(());
    }

    for assertion in &facets.assertions {
        if assertion.test.is_empty() {
            continue;
        }

        // Resolve assertion source location for error reporting
        let location = assertion
            .source
            .as_ref()
            .and_then(|s| schema_set.source_maps.locate(s));

        // Build XPath static context
        let ctx = XPathContext::new(&schema_set.name_table)
            .with_namespaces(assertion.ns_snapshot.clone())
            .with_schema_set(schema_set);

        // Set default element namespace from xpathDefaultNamespace cascade
        let ctx = if let Some(default_ns) = resolve_assertion_default_ns(
            assertion.xpath_default_namespace.as_deref(),
            assertion.source.as_ref(),
            &assertion.ns_snapshot,
            schema_set,
        ) {
            ctx.with_default_element_ns(default_ns)
        } else {
            ctx
        };

        // Compile the XPath expression with $value declared
        let expr =
            XPathExpr::compile_with_vars(&assertion.test, &ctx, &["value"]).map_err(|e| {
                errors::error(
                    "cvc-assertion",
                    format!(
                        "Failed to compile assertion test '{}': {}",
                        assertion.test, e
                    ),
                    location.clone(),
                )
            })?;

        // Convert typed value to XPathValue
        let xpath_value = typed_value.to_xpath_value::<RoXmlNavigator<'static>>(item_schema_type);

        // Evaluate with $value bound
        let result = expr
            .evaluator(&ctx)
            .run_with::<RoXmlNavigator<'static>, _>(|eval| {
                eval.set_variable_by_name("value", xpath_value).unwrap();
            })
            .map_err(|e| {
                errors::error(
                    "cvc-assertion",
                    format!(
                        "Failed to evaluate assertion test '{}': {}",
                        assertion.test, e
                    ),
                    location.clone(),
                )
            })?;

        // Effective boolean value must be true
        let ebv = effective_boolean_value(&result).map_err(|e| {
            errors::error(
                "cvc-assertion",
                format!(
                    "Failed to compute boolean value for assertion '{}': {}",
                    assertion.test, e
                ),
                location.clone(),
            )
        })?;

        if !ebv {
            return Err(errors::error(
                "cvc-assertion",
                format!(
                    "Assertion '{}' failed for value '{}'",
                    assertion.test,
                    typed_value.to_string_value()
                ),
                location,
            ));
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::load_and_process_schema;

    fn load_schema(xsd: &str) -> SchemaSet {
        let mut schema_set = SchemaSet::new();
        load_and_process_schema(xsd.as_bytes(), "test.xsd", &mut schema_set, None)
            .expect("failed to load schema");
        schema_set
    }

    /// Helper: get the TypeKey for a global element's type
    fn element_type(schema_set: &SchemaSet, local_name: &str) -> TypeKey {
        let name_id = schema_set.name_table.add(local_name);
        let elem_key = schema_set
            .lookup_element(None, name_id)
            .expect("element not found");
        schema_set.arenas.elements[elem_key]
            .resolved_type
            .expect("element has no resolved type")
    }

    #[test]
    fn test_builtin_string_accepts_anything() {
        let schema = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="e" type="xs:string"/>
            </xs:schema>"#,
        );
        let tk = element_type(&schema, "e");
        let result = validate_simple_type("hello world", tk, &schema).unwrap();
        assert!(result.member_type.is_none());
        assert_eq!(result.typed_value.type_code, XmlTypeCode::String);
    }

    #[test]
    fn test_builtin_integer_valid() {
        let schema = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="e" type="xs:integer"/>
            </xs:schema>"#,
        );
        let tk = element_type(&schema, "e");
        let result = validate_simple_type("42", tk, &schema).unwrap();
        assert_eq!(result.typed_value.type_code, XmlTypeCode::Integer);
    }

    #[test]
    fn test_builtin_integer_invalid() {
        let schema = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="e" type="xs:integer"/>
            </xs:schema>"#,
        );
        let tk = element_type(&schema, "e");
        let err = validate_simple_type("abc", tk, &schema).unwrap_err();
        assert_eq!(err.constraint, "cvc-datatype-valid");
    }

    #[test]
    fn test_builtin_boolean_valid() {
        let schema = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="e" type="xs:boolean"/>
            </xs:schema>"#,
        );
        let tk = element_type(&schema, "e");
        for v in &["true", "false", "1", "0"] {
            assert!(
                validate_simple_type(v, tk, &schema).is_ok(),
                "failed for '{}'",
                v
            );
        }
    }

    #[test]
    fn test_builtin_boolean_invalid() {
        let schema = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="e" type="xs:boolean"/>
            </xs:schema>"#,
        );
        let tk = element_type(&schema, "e");
        assert!(validate_simple_type("yes", tk, &schema).is_err());
    }

    #[test]
    fn test_user_defined_restriction_min_max() {
        let schema = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:simpleType name="score">
                    <xs:restriction base="xs:integer">
                        <xs:minInclusive value="0"/>
                        <xs:maxInclusive value="100"/>
                    </xs:restriction>
                </xs:simpleType>
                <xs:element name="e" type="score"/>
            </xs:schema>"#,
        );
        let tk = element_type(&schema, "e");

        assert!(validate_simple_type("50", tk, &schema).is_ok());
        assert!(validate_simple_type("0", tk, &schema).is_ok());
        assert!(validate_simple_type("100", tk, &schema).is_ok());

        let err = validate_simple_type("101", tk, &schema).unwrap_err();
        assert_eq!(err.constraint, "cvc-maxInclusive-valid");

        let err = validate_simple_type("-1", tk, &schema).unwrap_err();
        assert_eq!(err.constraint, "cvc-minInclusive-valid");
    }

    #[test]
    fn test_enumeration_facet() {
        let schema = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:simpleType name="color">
                    <xs:restriction base="xs:string">
                        <xs:enumeration value="red"/>
                        <xs:enumeration value="green"/>
                        <xs:enumeration value="blue"/>
                    </xs:restriction>
                </xs:simpleType>
                <xs:element name="e" type="color"/>
            </xs:schema>"#,
        );
        let tk = element_type(&schema, "e");

        assert!(validate_simple_type("red", tk, &schema).is_ok());
        assert!(validate_simple_type("green", tk, &schema).is_ok());

        let err = validate_simple_type("yellow", tk, &schema).unwrap_err();
        assert_eq!(err.constraint, "cvc-enumeration-valid");
    }

    #[test]
    fn test_pattern_facet() {
        let schema = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:simpleType name="zipCode">
                    <xs:restriction base="xs:string">
                        <xs:pattern value="[0-9]{5}"/>
                    </xs:restriction>
                </xs:simpleType>
                <xs:element name="e" type="zipCode"/>
            </xs:schema>"#,
        );
        let tk = element_type(&schema, "e");

        assert!(validate_simple_type("12345", tk, &schema).is_ok());

        let err = validate_simple_type("1234", tk, &schema).unwrap_err();
        assert_eq!(err.constraint, "cvc-pattern-valid");
    }

    #[test]
    fn test_empty_string_against_integer() {
        let schema = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="e" type="xs:integer"/>
            </xs:schema>"#,
        );
        let tk = element_type(&schema, "e");
        assert!(validate_simple_type("", tk, &schema).is_err());
    }

    #[test]
    fn test_empty_string_against_string() {
        let schema = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="e" type="xs:string"/>
            </xs:schema>"#,
        );
        let tk = element_type(&schema, "e");
        assert!(validate_simple_type("", tk, &schema).is_ok());
    }

    #[test]
    fn test_list_type() {
        let schema = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:simpleType name="intList">
                    <xs:list itemType="xs:integer"/>
                </xs:simpleType>
                <xs:element name="e" type="intList"/>
            </xs:schema>"#,
        );
        let tk = element_type(&schema, "e");

        let result = validate_simple_type("1 2 3", tk, &schema).unwrap();
        assert!(result.typed_value.is_list());

        // Non-integer item should fail
        assert!(validate_simple_type("1 abc 3", tk, &schema).is_err());
    }

    #[test]
    fn test_union_type() {
        let schema = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:simpleType name="intOrString">
                    <xs:union memberTypes="xs:integer xs:string"/>
                </xs:simpleType>
                <xs:element name="e" type="intOrString"/>
            </xs:schema>"#,
        );
        let tk = element_type(&schema, "e");

        // Integer matches first
        let result = validate_simple_type("42", tk, &schema).unwrap();
        assert!(result.member_type.is_some());
        assert_eq!(result.typed_value.type_code, XmlTypeCode::Integer);

        // Non-integer matches xs:string
        let result = validate_simple_type("hello", tk, &schema).unwrap();
        assert!(result.member_type.is_some());
        assert_eq!(result.typed_value.type_code, XmlTypeCode::String);
    }
}

// ---------------------------------------------------------------------------
// XSD 1.1 tests: schema_type propagation and to_xpath_value conversion
// ---------------------------------------------------------------------------

#[cfg(all(test, feature = "xsd11"))]
mod xsd11_tests {
    use super::*;
    use crate::navigator::RoXmlNavigator;
    use crate::pipeline::load_and_process_schema;
    use crate::types::sequence::resolve_list_item_schema_type;
    use crate::xpath::XPathValue;

    fn load_schema(xsd: &str) -> SchemaSet {
        let mut schema_set = SchemaSet::xsd11();
        load_and_process_schema(xsd.as_bytes(), "test.xsd", &mut schema_set, None)
            .expect("failed to load schema");
        schema_set
    }

    fn element_type(schema_set: &SchemaSet, local_name: &str) -> TypeKey {
        let name_id = schema_set.name_table.add(local_name);
        let elem_key = schema_set
            .lookup_element(None, name_id)
            .expect("element not found");
        schema_set.arenas.elements[elem_key]
            .resolved_type
            .expect("element has no resolved type")
    }

    #[test]
    fn test_atomic_schema_type_set() {
        let schema = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:simpleType name="score">
                    <xs:restriction base="xs:integer">
                        <xs:minInclusive value="0"/>
                        <xs:maxInclusive value="100"/>
                    </xs:restriction>
                </xs:simpleType>
                <xs:element name="e" type="score"/>
            </xs:schema>"#,
        );
        let tk = element_type(&schema, "e");
        let result = validate_simple_type("50", tk, &schema).unwrap();
        assert!(
            result.typed_value.schema_type.is_some(),
            "atomic value should have schema_type set"
        );
    }

    #[test]
    fn test_list_schema_type_set() {
        let schema = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:simpleType name="intList">
                    <xs:list itemType="xs:integer"/>
                </xs:simpleType>
                <xs:element name="e" type="intList"/>
            </xs:schema>"#,
        );
        let tk = element_type(&schema, "e");
        let result = validate_simple_type("1 2 3", tk, &schema).unwrap();
        assert!(
            result.typed_value.schema_type.is_some(),
            "list value should have schema_type set"
        );
    }

    #[test]
    fn test_union_schema_type_set() {
        let schema = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:simpleType name="intOrString">
                    <xs:union memberTypes="xs:integer xs:string"/>
                </xs:simpleType>
                <xs:element name="e" type="intOrString"/>
            </xs:schema>"#,
        );
        let tk = element_type(&schema, "e");
        let result = validate_simple_type("42", tk, &schema).unwrap();

        // Outer union value has schema_type
        assert!(
            result.typed_value.schema_type.is_some(),
            "union value should have schema_type set"
        );

        // Inner value also has schema_type (from matched member)
        if let XmlValueKind::Union(ref inner) = result.typed_value.value {
            assert!(
                inner.schema_type.is_some(),
                "inner union value should have schema_type set from matched member"
            );
        } else {
            panic!("expected Union variant");
        }
    }

    #[test]
    fn test_list_to_xpath_value() {
        let schema = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:simpleType name="intList">
                    <xs:list itemType="xs:integer"/>
                </xs:simpleType>
                <xs:element name="e" type="intList"/>
            </xs:schema>"#,
        );
        let tk = element_type(&schema, "e");
        let result = validate_simple_type("1 2 3", tk, &schema).unwrap();

        // Resolve list item schema type
        let list_sk = tk.as_simple().expect("should be simple type");
        let item_sk = resolve_list_item_schema_type(list_sk, &schema);

        let xpath_val: XPathValue<RoXmlNavigator<'static>> =
            result.typed_value.to_xpath_value(item_sk);

        // Should produce a sequence of 3 items
        let items = xpath_val.into_vec();
        assert_eq!(items.len(), 3, "list should produce 3 XPath items");

        // Each item should have schema_type set
        for item in &items {
            let val = item.as_atomic().expect("should be atomic");
            assert!(
                val.schema_type.is_some(),
                "each list item should have schema_type"
            );
        }
    }

    #[test]
    fn test_union_to_xpath_value() {
        let schema = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:simpleType name="intOrString">
                    <xs:union memberTypes="xs:integer xs:string"/>
                </xs:simpleType>
                <xs:element name="e" type="intOrString"/>
            </xs:schema>"#,
        );
        let tk = element_type(&schema, "e");
        let result = validate_simple_type("42", tk, &schema).unwrap();

        let xpath_val: XPathValue<RoXmlNavigator<'static>> =
            result.typed_value.to_xpath_value(None);

        // Union unwraps to the inner atomic value
        let items = xpath_val.into_vec();
        assert_eq!(items.len(), 1, "union should unwrap to single item");
        assert!(items[0].is_atomic());
    }

    #[test]
    fn test_atomic_to_xpath_value() {
        let schema = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="e" type="xs:integer"/>
            </xs:schema>"#,
        );
        let tk = element_type(&schema, "e");
        let result = validate_simple_type("42", tk, &schema).unwrap();

        let xpath_val: XPathValue<RoXmlNavigator<'static>> =
            result.typed_value.to_xpath_value(None);

        let items = xpath_val.into_vec();
        assert_eq!(items.len(), 1, "atomic should produce single item");
        assert!(items[0].is_atomic());
        assert_eq!(
            items[0].as_atomic().unwrap().type_code,
            XmlTypeCode::Integer
        );
    }

    // -----------------------------------------------------------------------
    // Assertion facet tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_assertion_even_integer() {
        let schema = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:simpleType name="evenInt">
                    <xs:restriction base="xs:integer">
                        <xs:assertion test="$value mod 2 = 0"/>
                    </xs:restriction>
                </xs:simpleType>
                <xs:element name="e" type="evenInt"/>
            </xs:schema>"#,
        );
        let tk = element_type(&schema, "e");
        assert!(validate_simple_type("4", tk, &schema).is_ok());
        assert!(validate_simple_type("0", tk, &schema).is_ok());
        assert!(validate_simple_type("-2", tk, &schema).is_ok());

        let err = validate_simple_type("3", tk, &schema).unwrap_err();
        assert_eq!(err.constraint, "cvc-assertion");

        let err = validate_simple_type("7", tk, &schema).unwrap_err();
        assert_eq!(err.constraint, "cvc-assertion");
    }

    #[test]
    fn test_assertion_positive_value() {
        let schema = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:simpleType name="posInt">
                    <xs:restriction base="xs:integer">
                        <xs:assertion test="$value gt 0"/>
                    </xs:restriction>
                </xs:simpleType>
                <xs:element name="e" type="posInt"/>
            </xs:schema>"#,
        );
        let tk = element_type(&schema, "e");
        assert!(validate_simple_type("1", tk, &schema).is_ok());
        assert!(validate_simple_type("100", tk, &schema).is_ok());

        let err = validate_simple_type("0", tk, &schema).unwrap_err();
        assert_eq!(err.constraint, "cvc-assertion");

        let err = validate_simple_type("-5", tk, &schema).unwrap_err();
        assert_eq!(err.constraint, "cvc-assertion");
    }

    #[test]
    fn test_assertion_string_length() {
        let schema = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:simpleType name="shortStr">
                    <xs:restriction base="xs:string">
                        <xs:assertion test="string-length($value) le 5"/>
                    </xs:restriction>
                </xs:simpleType>
                <xs:element name="e" type="shortStr"/>
            </xs:schema>"#,
        );
        let tk = element_type(&schema, "e");
        assert!(validate_simple_type("hello", tk, &schema).is_ok());
        assert!(validate_simple_type("", tk, &schema).is_ok());
        assert!(validate_simple_type("abcde", tk, &schema).is_ok());

        let err = validate_simple_type("toolong", tk, &schema).unwrap_err();
        assert_eq!(err.constraint, "cvc-assertion");
    }

    #[test]
    fn test_assertion_inherited() {
        // Assertion inherited from base type through derivation chain
        let schema = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:simpleType name="positiveInt">
                    <xs:restriction base="xs:integer">
                        <xs:assertion test="$value gt 0"/>
                    </xs:restriction>
                </xs:simpleType>
                <xs:simpleType name="smallPositiveInt">
                    <xs:restriction base="positiveInt">
                        <xs:maxInclusive value="10"/>
                    </xs:restriction>
                </xs:simpleType>
                <xs:element name="e" type="smallPositiveInt"/>
            </xs:schema>"#,
        );
        let tk = element_type(&schema, "e");
        assert!(validate_simple_type("5", tk, &schema).is_ok());
        assert!(validate_simple_type("10", tk, &schema).is_ok());

        // Fails maxInclusive
        assert!(validate_simple_type("11", tk, &schema).is_err());
        // Fails inherited assertion ($value gt 0)
        let err = validate_simple_type("0", tk, &schema).unwrap_err();
        assert_eq!(err.constraint, "cvc-assertion");
    }

    #[test]
    fn test_assertion_compile_error() {
        // Invalid XPath in assertion test → cvc-assertion error
        let schema = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:simpleType name="badAssert">
                    <xs:restriction base="xs:integer">
                        <xs:assertion test="$value @@@ invalid"/>
                    </xs:restriction>
                </xs:simpleType>
                <xs:element name="e" type="badAssert"/>
            </xs:schema>"#,
        );
        let tk = element_type(&schema, "e");
        let err = validate_simple_type("42", tk, &schema).unwrap_err();
        assert_eq!(err.constraint, "cvc-assertion");
        assert!(err.message.contains("compile"));
    }

    #[test]
    fn test_assertion_with_other_facets() {
        // Assertion combined with pattern and range facets
        let schema = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:simpleType name="evenScore">
                    <xs:restriction base="xs:integer">
                        <xs:minInclusive value="0"/>
                        <xs:maxInclusive value="100"/>
                        <xs:assertion test="$value mod 2 = 0"/>
                    </xs:restriction>
                </xs:simpleType>
                <xs:element name="e" type="evenScore"/>
            </xs:schema>"#,
        );
        let tk = element_type(&schema, "e");
        assert!(validate_simple_type("50", tk, &schema).is_ok());
        assert!(validate_simple_type("0", tk, &schema).is_ok());
        assert!(validate_simple_type("100", tk, &schema).is_ok());

        // Fails assertion (odd number)
        let err = validate_simple_type("51", tk, &schema).unwrap_err();
        assert_eq!(err.constraint, "cvc-assertion");

        // Fails range (out of bounds)
        assert!(validate_simple_type("102", tk, &schema).is_err());
        assert!(validate_simple_type("-2", tk, &schema).is_err());
    }

    #[test]
    fn test_assertion_multiple() {
        // Multiple assertion facets on the same type
        let schema = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:simpleType name="constrained">
                    <xs:restriction base="xs:integer">
                        <xs:assertion test="$value gt 0"/>
                        <xs:assertion test="$value mod 2 = 0"/>
                    </xs:restriction>
                </xs:simpleType>
                <xs:element name="e" type="constrained"/>
            </xs:schema>"#,
        );
        let tk = element_type(&schema, "e");
        // Must satisfy both: positive AND even
        assert!(validate_simple_type("2", tk, &schema).is_ok());
        assert!(validate_simple_type("4", tk, &schema).is_ok());

        // Positive but odd -> fails second assertion
        let err = validate_simple_type("3", tk, &schema).unwrap_err();
        assert_eq!(err.constraint, "cvc-assertion");

        // Even but not positive -> fails first assertion
        let err = validate_simple_type("0", tk, &schema).unwrap_err();
        assert_eq!(err.constraint, "cvc-assertion");
    }

    #[test]
    fn test_assertion_boolean_value() {
        // Assertion on xs:boolean type
        let schema = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:simpleType name="mustBeTrue">
                    <xs:restriction base="xs:boolean">
                        <xs:assertion test="$value"/>
                    </xs:restriction>
                </xs:simpleType>
                <xs:element name="e" type="mustBeTrue"/>
            </xs:schema>"#,
        );
        let tk = element_type(&schema, "e");
        assert!(validate_simple_type("true", tk, &schema).is_ok());
        assert!(validate_simple_type("1", tk, &schema).is_ok());

        let err = validate_simple_type("false", tk, &schema).unwrap_err();
        assert_eq!(err.constraint, "cvc-assertion");
    }

    #[test]
    fn test_assertion_union_with_list_member_item_typing() {
        // Union whose member is a list of xs:integer.
        // The assertion uses `instance of` on each list item, which requires
        // item_schema_type to be propagated through the union validation path.
        let schema = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:simpleType name="intList">
                    <xs:list itemType="xs:integer"/>
                </xs:simpleType>
                <xs:simpleType name="unionWithList">
                    <xs:union memberTypes="intList">
                        <xs:simpleType>
                            <xs:restriction base="xs:string"/>
                        </xs:simpleType>
                    </xs:union>
                </xs:simpleType>
                <xs:simpleType name="checkedUnion">
                    <xs:restriction base="unionWithList">
                        <xs:assertion test="every $i in $value satisfies $i instance of xs:integer"/>
                    </xs:restriction>
                </xs:simpleType>
                <xs:element name="e" type="checkedUnion"/>
            </xs:schema>"#,
        );
        let tk = element_type(&schema, "e");
        // Space-separated integers should match the intList member and pass the assertion
        assert!(validate_simple_type("1 2 3", tk, &schema).is_ok());
    }

    // -----------------------------------------------------------------------
    // xpathDefaultNamespace cascade for assertion facets
    // -----------------------------------------------------------------------

    #[test]
    fn test_assertion_xpath_default_ns_schema_level_fallback() {
        // Schema-level xpathDefaultNamespace set to the XS namespace.
        // The assertion uses an unprefixed type name `integer` which should
        // resolve to xs:integer via the default element namespace cascade.
        let schema = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
                        xpathDefaultNamespace="http://www.w3.org/2001/XMLSchema">
                <xs:simpleType name="checkedInt">
                    <xs:restriction base="xs:integer">
                        <xs:assertion test="$value instance of integer"/>
                    </xs:restriction>
                </xs:simpleType>
                <xs:element name="e" type="checkedInt"/>
            </xs:schema>"#,
        );
        let tk = element_type(&schema, "e");
        // Unprefixed `integer` resolves to xs:integer → assertion passes
        assert!(validate_simple_type("42", tk, &schema).is_ok());
    }

    #[test]
    fn test_assertion_xpath_default_ns_assertion_level_overrides_schema() {
        // Schema-level xpathDefaultNamespace is the XS namespace, but the
        // assertion element overrides it with ##local.  Now unprefixed
        // `integer` resolves to no-namespace, which has no matching type,
        // so the assertion must fail.
        let schema = load_schema(
            r###"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
                        xpathDefaultNamespace="http://www.w3.org/2001/XMLSchema">
                <xs:simpleType name="checkedInt">
                    <xs:restriction base="xs:integer">
                        <xs:assertion test="$value instance of integer"
                                      xpathDefaultNamespace="##local"/>
                    </xs:restriction>
                </xs:simpleType>
                <xs:element name="e" type="checkedInt"/>
            </xs:schema>"###,
        );
        let tk = element_type(&schema, "e");
        // ##local overrides → `integer` resolves to no-namespace → assertion fails
        let err = validate_simple_type("42", tk, &schema).unwrap_err();
        assert_eq!(err.constraint, "cvc-assertion");
    }

    #[test]
    fn test_assertion_xpath_default_ns_target_namespace_token() {
        // xpathDefaultNamespace="##targetNamespace" with a non-XS target namespace.
        // Unprefixed `integer` resolves to http://example.com/ns (the target
        // namespace), not to the XS namespace, so `instance of integer` must
        // fail — proving the token is correctly resolved.
        let schema = load_schema(
            r###"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
                        targetNamespace="http://example.com/ns"
                        xmlns:tns="http://example.com/ns"
                        xpathDefaultNamespace="##targetNamespace">
                <xs:simpleType name="checkedInt">
                    <xs:restriction base="xs:integer">
                        <xs:assertion test="$value instance of integer"/>
                    </xs:restriction>
                </xs:simpleType>
                <xs:element name="e" type="tns:checkedInt"/>
            </xs:schema>"###,
        );
        let ns_id = schema.name_table.add("http://example.com/ns");
        let name_id = schema.name_table.add("e");
        let elem_key = schema
            .lookup_element(Some(ns_id), name_id)
            .expect("element not found");
        let tk = schema.arenas.elements[elem_key]
            .resolved_type
            .expect("element has no resolved type");
        // ##targetNamespace → http://example.com/ns → `integer` is NOT xs:integer → fails
        let err = validate_simple_type("42", tk, &schema).unwrap_err();
        assert_eq!(err.constraint, "cvc-assertion");
    }
}
