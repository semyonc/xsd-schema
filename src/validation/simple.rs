//! Simple type value validation
//!
//! Validates text content against XSD simple types (atomic, list, union).
//! Reuses `VALIDATOR_REGISTRY` for lexical validation and `FacetSet` for
//! facet constraint checking.

use crate::ids::{SimpleTypeKey, TypeKey};
use crate::parser::frames::SimpleTypeVariety;
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
            // Complex type with simpleContent — walk resolved_base_type to find the simple type
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
            if let Some(base_key) = ct_data.resolved_base_type {
                validate_simple_type_inner(value, base_key, schema_set)
            } else {
                // No base type — treat as anySimpleType (accept any value)
                Ok(SimpleTypeResult {
                    typed_value: XmlValue::untyped(value),
                    member_type: None,
                })
            }
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
    let typed_value = if facets.is_empty() {
        validator.validate(value)
    } else {
        validator.validate_with_facets(value, &facets)
    };

    match typed_value {
        Ok(val) => Ok(SimpleTypeResult {
            typed_value: val,
            member_type: None,
        }),
        Err(type_err) => {
            let code = errors::value_error_constraint_code(&type_err);
            Err(errors::from_value_error(code, type_err, None))
        }
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
            // No item type resolved — cannot validate list items, accept as untyped
            return Ok(SimpleTypeResult {
                typed_value: XmlValue::untyped(value),
                member_type: None,
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
        facets.validate_string_patterns_enums(&normalized).map_err(|e| {
            let code = errors::facet_constraint_code(&e);
            errors::from_facet_error(code, e, None)
        })?;
    }

    Ok(SimpleTypeResult {
        typed_value: XmlValue::new(
            item_type_code,
            XmlValueKind::List {
                item_type: item_type_code,
                items,
            },
        ),
        member_type: None,
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
                facets.validate_string(&check_value).map_err(|e| {
                    let code = errors::facet_constraint_code(&e);
                    errors::from_facet_error(code, e, None)
                })?;
            }

            return Ok(SimpleTypeResult {
                typed_value: XmlValue::new(
                    result.typed_value.type_code,
                    XmlValueKind::Union(Box::new(result.typed_value)),
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
            assert!(validate_simple_type(v, tk, &schema).is_ok(), "failed for '{}'", v);
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
