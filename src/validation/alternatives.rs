//! XSD 1.1 type alternative (conditional type assignment) evaluation.
//!
//! When an element declaration has `xs:alternative` children, the governing
//! type is selected at runtime based on XPath test expressions evaluated
//! against the element's attributes. This module provides:
//!
//! - [`evaluate_type_alternatives`] — core evaluation: builds a minimal
//!   document fragment, evaluates each alternative's test, and returns
//!   the first matching alternative's resolved type.

use bumpalo::Bump;

use crate::document::builder::BufferDocumentBuilder;
use crate::document::navigator::BufferDocNavigator;
use crate::document::{BufferDocumentOptions, DocumentKind};
use crate::ids::{ElementKey, NameId, TypeKey};
use crate::parser::frames::AlternativeResult;
use crate::schema::SchemaSet;
use crate::xpath::api::XPathExpr;
use crate::xpath::functions::effective_boolean_value;
use crate::xpath::XPathContext;

// ---------------------------------------------------------------------------
// resolve_alternative_default_ns — xpathDefaultNamespace cascade
// ---------------------------------------------------------------------------

/// Two-level cascade: **alternative-level > schema-document-level**.
fn resolve_alternative_default_ns(
    alt: &AlternativeResult,
    elem_key: ElementKey,
    schema_set: &SchemaSet,
) -> Option<NameId> {
    let elem = &schema_set.arenas.elements[elem_key];

    // Look up the schema document that defines the element
    let doc = elem
        .source
        .as_ref()
        .and_then(|s| schema_set.documents.get(s.doc_id as usize));

    // Cascade: alternative-level > schema-document-level
    let effective = if let Some(raw) = &alt.xpath_default_namespace {
        Some(raw.clone())
    } else {
        doc.and_then(|d| d.xpath_default_namespace)
            .map(|id| schema_set.name_table.resolve(id))
    };

    match effective.as_deref() {
        Some("##defaultNamespace") => alt.ns_snapshot.default_ns,
        Some("##targetNamespace") => doc.and_then(|d| d.target_namespace),
        Some("##local") | None => None,
        Some(uri) => Some(schema_set.name_table.add(uri)),
    }
}

// ---------------------------------------------------------------------------
// evaluate_type_alternatives — core evaluation
// ---------------------------------------------------------------------------

/// Evaluate type alternatives for an element declaration.
///
/// Builds a minimal `BufferDocument` fragment containing just the element
/// and its attributes, then evaluates each alternative's XPath test.
/// Returns the `TypeKey` of the first matching alternative, or `None`
/// if no alternative matches (caller should use the element's declared type).
pub(crate) fn evaluate_type_alternatives(
    elem_key: ElementKey,
    elem_local_name: NameId,
    elem_namespace: Option<NameId>,
    collected_attributes: &[(Option<NameId>, NameId, String)],
    schema_set: &SchemaSet,
) -> Option<TypeKey> {
    let alternatives = &schema_set.arenas.elements[elem_key].alternatives;
    if alternatives.is_empty() {
        return None;
    }

    // Build a minimal fragment document with just the element + attributes
    let arena = Bump::new();
    let opts = BufferDocumentOptions {
        kind: DocumentKind::Fragment,
        track_source_locations: false,
    };
    let mut builder = match BufferDocumentBuilder::new(&arena, &schema_set.name_table, None, opts) {
        Ok(b) => b,
        Err(_) => return None,
    };

    // Start element
    let local_str = schema_set.name_table.resolve(elem_local_name);
    let ns_str = elem_namespace
        .map(|id| schema_set.name_table.resolve(id))
        .unwrap_or_default();
    let elem_ref = match builder.start_element(&local_str, &ns_str, "", &[]) {
        Ok(r) => r,
        Err(_) => return None,
    };

    // Add attributes
    for (ns, name, value) in collected_attributes {
        let attr_local = schema_set.name_table.resolve(*name);
        let attr_ns = ns
            .map(|id| schema_set.name_table.resolve(id))
            .unwrap_or_default();
        if builder.attribute(&attr_local, &attr_ns, "", value).is_err() {
            return None;
        }
    }

    builder.end_of_attributes();
    if builder.end_element().is_err() {
        return None;
    }

    let doc = match builder.finalize() {
        Ok(d) => d,
        Err(_) => return None,
    };

    // Evaluate each alternative
    for alt in alternatives {
        if let Some(ref test_expr) = alt.test {
            // Build XPath context with the alternative's namespace snapshot
            let ctx = XPathContext::new(&schema_set.name_table)
                .with_namespaces(alt.ns_snapshot.clone())
                .with_schema_set(schema_set);

            let ctx = if let Some(default_ns) =
                resolve_alternative_default_ns(alt, elem_key, schema_set)
            {
                ctx.with_default_element_ns(default_ns)
            } else {
                ctx
            };

            let expr = match XPathExpr::compile(test_expr, &ctx) {
                Ok(e) => e,
                Err(_) => continue,
            };

            let nav = BufferDocNavigator::new(&doc, elem_ref);
            let result = match expr.evaluator(&ctx).run_with_node(nav) {
                Ok(r) => r,
                Err(_) => continue,
            };

            match effective_boolean_value(&result) {
                Ok(true) => return alt.resolved_type,
                _ => continue,
            }
        } else {
            // No test expression — this is the default fallback (must be last)
            return alt.resolved_type;
        }
    }

    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::load_and_process_schema;

    fn load_schema(xsd: &str) -> SchemaSet {
        let mut schema_set = SchemaSet::xsd11();
        load_and_process_schema(xsd.as_bytes(), "test.xsd", &mut schema_set, None)
            .expect("failed to load schema");
        schema_set
    }

    /// Find an element key by name.
    fn find_elem_key(schema_set: &SchemaSet, name: &str) -> ElementKey {
        let name_id = schema_set.name_table.add(name);
        for (key, elem) in &schema_set.arenas.elements {
            if elem.name == Some(name_id) {
                return key;
            }
        }
        panic!("Element '{}' not found", name);
    }

    /// Find a type key by name.
    fn find_type_key(schema_set: &SchemaSet, name: &str) -> TypeKey {
        let name_id = schema_set.name_table.add(name);
        for (key, ct) in &schema_set.arenas.complex_types {
            if ct.name == Some(name_id) {
                return TypeKey::Complex(key);
            }
        }
        for (key, st) in &schema_set.arenas.simple_types {
            if st.name == Some(name_id) {
                return TypeKey::Simple(key);
            }
        }
        panic!("Type '{}' not found", name);
    }

    #[test]
    fn test_no_alternatives_returns_none() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root" type="xs:string"/>
            </xs:schema>"#,
        );
        let elem_key = find_elem_key(&schema_set, "root");
        let local_name = schema_set.name_table.add("root");
        let result = evaluate_type_alternatives(elem_key, local_name, None, &[], &schema_set);
        assert!(result.is_none());
    }

    #[test]
    fn test_alternatives_parsed_and_resolved() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:complexType name="intType">
                    <xs:sequence>
                        <xs:element name="val" type="xs:integer"/>
                    </xs:sequence>
                </xs:complexType>
                <xs:complexType name="strType">
                    <xs:sequence>
                        <xs:element name="val" type="xs:string"/>
                    </xs:sequence>
                </xs:complexType>
                <xs:element name="root">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="val" type="xs:string"/>
                        </xs:sequence>
                        <xs:attribute name="kind" type="xs:string"/>
                    </xs:complexType>
                    <xs:alternative test="@kind='int'" type="intType"/>
                    <xs:alternative test="@kind='str'" type="strType"/>
                    <xs:alternative/>
                </xs:element>
            </xs:schema>"#,
        );
        let elem_key = find_elem_key(&schema_set, "root");
        let elem = &schema_set.arenas.elements[elem_key];
        assert_eq!(elem.alternatives.len(), 3);

        // First two should have tests and resolved types
        assert!(elem.alternatives[0].test.is_some());
        assert!(elem.alternatives[0].resolved_type.is_some());
        assert!(elem.alternatives[1].test.is_some());
        assert!(elem.alternatives[1].resolved_type.is_some());

        // Third is the default (no test)
        assert!(elem.alternatives[2].test.is_none());

        // Resolved types should match declared types
        let int_type = find_type_key(&schema_set, "intType");
        let str_type = find_type_key(&schema_set, "strType");
        assert_eq!(elem.alternatives[0].resolved_type, Some(int_type));
        assert_eq!(elem.alternatives[1].resolved_type, Some(str_type));
    }

    #[test]
    fn test_first_matching_alternative_wins() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:complexType name="typeA">
                    <xs:sequence>
                        <xs:element name="a" type="xs:string"/>
                    </xs:sequence>
                </xs:complexType>
                <xs:complexType name="typeB">
                    <xs:sequence>
                        <xs:element name="b" type="xs:string"/>
                    </xs:sequence>
                </xs:complexType>
                <xs:element name="root">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="x" type="xs:string"/>
                        </xs:sequence>
                        <xs:attribute name="kind" type="xs:string"/>
                    </xs:complexType>
                    <xs:alternative test="@kind='A'" type="typeA"/>
                    <xs:alternative test="@kind='B'" type="typeB"/>
                </xs:element>
            </xs:schema>"#,
        );
        let elem_key = find_elem_key(&schema_set, "root");
        let local_name = schema_set.name_table.add("root");
        let kind_name = schema_set.name_table.add("kind");

        // kind='A' -> typeA
        let attrs_a = vec![(None, kind_name, "A".to_string())];
        let result_a = evaluate_type_alternatives(elem_key, local_name, None, &attrs_a, &schema_set);
        let type_a = find_type_key(&schema_set, "typeA");
        assert_eq!(result_a, Some(type_a));

        // kind='B' -> typeB
        let attrs_b = vec![(None, kind_name, "B".to_string())];
        let result_b = evaluate_type_alternatives(elem_key, local_name, None, &attrs_b, &schema_set);
        let type_b = find_type_key(&schema_set, "typeB");
        assert_eq!(result_b, Some(type_b));
    }

    #[test]
    fn test_no_match_returns_none() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:complexType name="typeA">
                    <xs:sequence>
                        <xs:element name="a" type="xs:string"/>
                    </xs:sequence>
                </xs:complexType>
                <xs:element name="root">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="x" type="xs:string"/>
                        </xs:sequence>
                        <xs:attribute name="kind" type="xs:string"/>
                    </xs:complexType>
                    <xs:alternative test="@kind='A'" type="typeA"/>
                </xs:element>
            </xs:schema>"#,
        );
        let elem_key = find_elem_key(&schema_set, "root");
        let local_name = schema_set.name_table.add("root");
        let kind_name = schema_set.name_table.add("kind");

        // kind='X' -> no match, no default -> None
        let attrs = vec![(None, kind_name, "X".to_string())];
        let result = evaluate_type_alternatives(elem_key, local_name, None, &attrs, &schema_set);
        assert!(result.is_none());
    }

    #[test]
    fn test_default_fallback_no_test() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:complexType name="typeA">
                    <xs:sequence>
                        <xs:element name="a" type="xs:string"/>
                    </xs:sequence>
                </xs:complexType>
                <xs:complexType name="defaultType">
                    <xs:sequence>
                        <xs:element name="d" type="xs:string"/>
                    </xs:sequence>
                </xs:complexType>
                <xs:element name="root">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="x" type="xs:string"/>
                        </xs:sequence>
                        <xs:attribute name="kind" type="xs:string"/>
                    </xs:complexType>
                    <xs:alternative test="@kind='A'" type="typeA"/>
                    <xs:alternative type="defaultType"/>
                </xs:element>
            </xs:schema>"#,
        );
        let elem_key = find_elem_key(&schema_set, "root");
        let local_name = schema_set.name_table.add("root");
        let kind_name = schema_set.name_table.add("kind");

        // kind='X' -> no test match -> default fallback
        let attrs = vec![(None, kind_name, "X".to_string())];
        let result = evaluate_type_alternatives(elem_key, local_name, None, &attrs, &schema_set);
        let default_type = find_type_key(&schema_set, "defaultType");
        assert_eq!(result, Some(default_type));
    }

    #[test]
    fn test_inline_type_alternative() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="x" type="xs:string"/>
                        </xs:sequence>
                        <xs:attribute name="mode" type="xs:string"/>
                    </xs:complexType>
                    <xs:alternative test="@mode='simple'">
                        <xs:complexType>
                            <xs:sequence>
                                <xs:element name="s" type="xs:string"/>
                            </xs:sequence>
                        </xs:complexType>
                    </xs:alternative>
                </xs:element>
            </xs:schema>"#,
        );
        let elem_key = find_elem_key(&schema_set, "root");
        let elem = &schema_set.arenas.elements[elem_key];
        assert_eq!(elem.alternatives.len(), 1);
        // The inline type should have been resolved
        assert!(elem.alternatives[0].resolved_type.is_some());

        // Evaluate with matching attribute
        let local_name = schema_set.name_table.add("root");
        let mode_name = schema_set.name_table.add("mode");
        let attrs = vec![(None, mode_name, "simple".to_string())];
        let result = evaluate_type_alternatives(elem_key, local_name, None, &attrs, &schema_set);
        assert!(result.is_some());
        assert_eq!(result, elem.alternatives[0].resolved_type);
    }

    #[test]
    fn test_numeric_attribute_test() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:complexType name="smallType">
                    <xs:sequence>
                        <xs:element name="v" type="xs:string"/>
                    </xs:sequence>
                </xs:complexType>
                <xs:complexType name="largeType">
                    <xs:sequence>
                        <xs:element name="v" type="xs:string"/>
                    </xs:sequence>
                </xs:complexType>
                <xs:element name="item">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="v" type="xs:string"/>
                        </xs:sequence>
                        <xs:attribute name="size" type="xs:integer"/>
                    </xs:complexType>
                    <xs:alternative test="@size &lt; 10" type="smallType"/>
                    <xs:alternative test="@size >= 10" type="largeType"/>
                </xs:element>
            </xs:schema>"#,
        );
        let elem_key = find_elem_key(&schema_set, "item");
        let local_name = schema_set.name_table.add("item");
        let size_name = schema_set.name_table.add("size");

        // size=5 -> smallType
        let attrs_small = vec![(None, size_name, "5".to_string())];
        let result_small =
            evaluate_type_alternatives(elem_key, local_name, None, &attrs_small, &schema_set);
        let small_type = find_type_key(&schema_set, "smallType");
        assert_eq!(result_small, Some(small_type));

        // size=100 -> largeType
        let attrs_large = vec![(None, size_name, "100".to_string())];
        let result_large =
            evaluate_type_alternatives(elem_key, local_name, None, &attrs_large, &schema_set);
        let large_type = find_type_key(&schema_set, "largeType");
        assert_eq!(result_large, Some(large_type));
    }
}
