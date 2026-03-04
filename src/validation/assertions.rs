//! XSD 1.1 complex-type assertion evaluation.
//!
//! Complex types can carry `xs:assert` elements whose XPath 2.0 expressions
//! are evaluated against the element subtree. This module provides:
//!
//! - [`AssertionBufferFrame`] — per-element bookkeeping for assertion buffering
//! - [`has_inherited_assertions`] — cheap hot-path check for any assertions
//! - [`collect_inherited_assertions`] — full base-first collection with owner keys
//! - [`resolve_ct_assertion_default_ns`] — xpathDefaultNamespace cascade
//! - [`evaluate_complex_type_assertions`] — core XPath evaluation

use crate::document::buffer::BufferDocument;
use crate::document::navigator::BufferDocNavigator;
use crate::ids::{ComplexTypeKey, NameId, TypeKey};
use crate::parser::frames::AssertResult;
use crate::parser::location::SourceLocation;
use crate::schema::SchemaSet;
use crate::validation::errors::{self, ValidationError};
use crate::xpath::api::XPathExpr;
use crate::xpath::functions::effective_boolean_value;
use crate::xpath::XPathContext;

use crate::arenas::SchemaArenas;

// ---------------------------------------------------------------------------
// AssertionBufferFrame
// ---------------------------------------------------------------------------

/// Per-element assertion buffer frame.
///
/// Created when a complex type with assertions is encountered during streaming
/// validation. Tracks the node reference in the fragment document and the
/// owning complex type, so assertions can be evaluated at element close.
pub(crate) struct AssertionBufferFrame {
    /// Node ref of this element in the fragment document.
    pub element_ref: u32,
    /// ComplexType key whose assertions triggered this frame.
    pub complex_type_key: ComplexTypeKey,
    /// Element path at the time this frame's element closed (for error reporting).
    /// Populated when the frame is popped at its own end-element (before deferral).
    pub element_path: String,
    /// Source location at the time this frame's element closed.
    pub location: Option<SourceLocation>,
}

// ---------------------------------------------------------------------------
// has_inherited_assertions — cheap hot-path check
// ---------------------------------------------------------------------------

/// Returns `true` if the complex type (or any base in its derivation chain)
/// has non-empty `assertions`. No allocation. Used in
/// `validate_element_by_id` to decide whether to start assertion buffering.
pub(crate) fn has_inherited_assertions(
    ct_key: ComplexTypeKey,
    arenas: &SchemaArenas,
) -> bool {
    let ct = &arenas.complex_types[ct_key];
    if !ct.assertions.is_empty() {
        return true;
    }
    // Walk the derivation chain
    let mut current = ct.resolved_base_type;
    while let Some(TypeKey::Complex(base_key)) = current {
        let base = &arenas.complex_types[base_key];
        if !base.assertions.is_empty() {
            return true;
        }
        current = base.resolved_base_type;
    }
    false
}

// ---------------------------------------------------------------------------
// collect_inherited_assertions — full collection
// ---------------------------------------------------------------------------

/// Collects all assertions from the complex type and its base types,
/// ordered base-first. Each assertion is paired with its **defining** type's
/// key — essential for the xpathDefaultNamespace cascade, which must use the
/// type-level default from the type that declared the assertion.
pub(crate) fn collect_inherited_assertions(
    ct_key: ComplexTypeKey,
    arenas: &SchemaArenas,
) -> Vec<(&AssertResult, ComplexTypeKey)> {
    // Collect chain of complex type keys from derived to base
    let mut chain = vec![ct_key];
    let mut current = arenas.complex_types[ct_key].resolved_base_type;
    while let Some(TypeKey::Complex(base_key)) = current {
        chain.push(base_key);
        current = arenas.complex_types[base_key].resolved_base_type;
    }

    // Reverse for base-first order, then collect assertions
    let mut result = Vec::new();
    for &key in chain.iter().rev() {
        let ct = &arenas.complex_types[key];
        for assertion in &ct.assertions {
            result.push((assertion, key));
        }
    }
    result
}

// ---------------------------------------------------------------------------
// resolve_ct_assertion_default_ns — xpathDefaultNamespace cascade
// ---------------------------------------------------------------------------

/// Three-level cascade: **assertion-level > owner-type-level > schema-document-level**.
///
/// Takes the **owner** `ComplexTypeKey` (from `collect_inherited_assertions`),
/// not the derived type, so inherited assertions get the correct type-level default.
fn resolve_ct_assertion_default_ns(
    assertion: &AssertResult,
    owner_ct_key: ComplexTypeKey,
    schema_set: &SchemaSet,
) -> Option<NameId> {
    let ct = &schema_set.arenas.complex_types[owner_ct_key];

    // Look up the schema document that defines the owning type
    let doc = ct
        .source
        .as_ref()
        .and_then(|s| schema_set.documents.get(s.doc_id as usize));

    // Cascade: assertion-level > type-level > schema-document-level
    let effective = if let Some(raw) = &assertion.xpath_default_namespace {
        Some(raw.clone())
    } else if let Some(raw) = &ct.xpath_default_namespace {
        Some(raw.clone())
    } else {
        doc.and_then(|d| d.xpath_default_namespace)
            .map(|id| schema_set.name_table.resolve(id))
    };

    match effective.as_deref() {
        Some("##defaultNamespace") => assertion.ns_snapshot.default_ns,
        Some("##targetNamespace") => doc.and_then(|d| d.target_namespace),
        Some("##local") | None => None,
        Some(uri) => Some(schema_set.name_table.add(uri)),
    }
}

// ---------------------------------------------------------------------------
// evaluate_complex_type_assertions — core evaluation
// ---------------------------------------------------------------------------

/// Evaluate all assertions (own + inherited) for a complex type against
/// the element subtree in a `BufferDocument`.
///
/// Returns a `Vec` of all `cvc-assertion` errors (does not stop at first failure).
pub(crate) fn evaluate_complex_type_assertions(
    doc: &BufferDocument<'_>,
    element_ref: u32,
    ct_key: ComplexTypeKey,
    schema_set: &SchemaSet,
) -> Vec<ValidationError> {
    let assertions = collect_inherited_assertions(ct_key, &schema_set.arenas);
    let mut errors = Vec::new();

    for (assertion, owner_key) in assertions {
        if assertion.test.is_empty() {
            continue;
        }

        // Build XPath static context with schema-time namespace snapshot
        let ctx = XPathContext::new(&schema_set.name_table)
            .with_namespaces(assertion.ns_snapshot.clone())
            .with_schema_set(schema_set);

        // Apply xpathDefaultNamespace cascade
        let ctx = if let Some(default_ns) =
            resolve_ct_assertion_default_ns(assertion, owner_key, schema_set)
        {
            ctx.with_default_element_ns(default_ns)
        } else {
            ctx
        };

        // Compile XPath expression (no $value variable for complex-type assertions)
        let expr = match XPathExpr::compile(&assertion.test, &ctx) {
            Ok(e) => e,
            Err(e) => {
                errors.push(errors::error(
                    "cvc-assertion",
                    format!(
                        "Failed to compile assertion test '{}': {}",
                        assertion.test, e
                    ),
                    None,
                ));
                continue;
            }
        };

        // Create navigator with context node = the element
        let nav = BufferDocNavigator::new(doc, element_ref);

        // Evaluate
        let result = match expr.evaluator(&ctx).run_with_node(nav) {
            Ok(r) => r,
            Err(e) => {
                errors.push(errors::error(
                    "cvc-assertion",
                    format!(
                        "Failed to evaluate assertion test '{}': {}",
                        assertion.test, e
                    ),
                    None,
                ));
                continue;
            }
        };

        // Check effective boolean value
        match effective_boolean_value(&result) {
            Ok(true) => { /* assertion passed */ }
            Ok(false) => {
                errors.push(errors::error(
                    "cvc-assertion",
                    format!("Assertion '{}' failed", assertion.test),
                    None,
                ));
            }
            Err(e) => {
                errors.push(errors::error(
                    "cvc-assertion",
                    format!(
                        "Failed to compute boolean value for assertion '{}': {}",
                        assertion.test, e
                    ),
                    None,
                ));
            }
        }
    }

    errors
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::model::XsdVersion;
    use crate::pipeline::{load_and_process_schema, PipelineConfig};

    fn load_schema(xsd: &str) -> SchemaSet {
        let mut schema_set = SchemaSet::with_version(XsdVersion::V1_1);
        let mut config = PipelineConfig::default();
        config.parser.xsd_version = XsdVersion::V1_1;
        load_and_process_schema(xsd.as_bytes(), "test.xsd", &mut schema_set, Some(config))
            .expect("failed to load schema");
        schema_set
    }

    /// Find the first complex type key in the schema set by name.
    fn find_ct_key(schema_set: &SchemaSet, name: &str) -> ComplexTypeKey {
        let name_id = schema_set.name_table.add(name);
        for (key, ct) in &schema_set.arenas.complex_types {
            if ct.name == Some(name_id) {
                return key;
            }
        }
        panic!("Complex type '{}' not found", name);
    }

    #[test]
    fn test_has_inherited_assertions_none() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:complexType name="plain">
                    <xs:sequence>
                        <xs:element name="x" type="xs:string"/>
                    </xs:sequence>
                </xs:complexType>
            </xs:schema>"#,
        );
        let key = find_ct_key(&schema_set, "plain");
        assert!(!has_inherited_assertions(key, &schema_set.arenas));
    }

    #[test]
    fn test_has_inherited_assertions_own() {
        // xs:assert as direct child of complexType with attribute-only content
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:complexType name="withAssert">
                    <xs:attribute name="val" type="xs:integer"/>
                    <xs:assert test="@val >= 0"/>
                </xs:complexType>
            </xs:schema>"#,
        );
        let key = find_ct_key(&schema_set, "withAssert");
        assert!(has_inherited_assertions(key, &schema_set.arenas));
    }

    #[test]
    fn test_has_inherited_assertions_from_base() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:complexType name="base">
                    <xs:attribute name="val" type="xs:integer"/>
                    <xs:assert test="@val >= 0"/>
                </xs:complexType>
                <xs:complexType name="derived">
                    <xs:complexContent>
                        <xs:restriction base="base">
                            <xs:attribute name="val" type="xs:integer"/>
                        </xs:restriction>
                    </xs:complexContent>
                </xs:complexType>
            </xs:schema>"#,
        );
        let key = find_ct_key(&schema_set, "derived");
        assert!(has_inherited_assertions(key, &schema_set.arenas));
    }

    #[test]
    fn test_collect_inherited_assertions_ordering() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:complexType name="base">
                    <xs:attribute name="val" type="xs:integer"/>
                    <xs:assert test="@val >= 0"/>
                </xs:complexType>
                <xs:complexType name="derived">
                    <xs:complexContent>
                        <xs:restriction base="base">
                            <xs:attribute name="val" type="xs:integer"/>
                            <xs:assert test="@val &lt; 100"/>
                        </xs:restriction>
                    </xs:complexContent>
                </xs:complexType>
            </xs:schema>"#,
        );
        let derived_key = find_ct_key(&schema_set, "derived");
        let base_key = find_ct_key(&schema_set, "base");
        let assertions = collect_inherited_assertions(derived_key, &schema_set.arenas);

        // Base-first ordering: base assertion comes first
        assert_eq!(assertions.len(), 2);
        assert_eq!(assertions[0].1, base_key, "first assertion should be from base");
        assert_eq!(assertions[1].1, derived_key, "second assertion should be from derived");
        assert!(assertions[0].0.test.contains(">= 0"));
        assert!(assertions[1].0.test.contains("< 100"));
    }

    #[test]
    fn test_collect_inherited_assertions_no_assertions() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:complexType name="plain">
                    <xs:sequence>
                        <xs:element name="x" type="xs:string"/>
                    </xs:sequence>
                </xs:complexType>
            </xs:schema>"#,
        );
        let key = find_ct_key(&schema_set, "plain");
        let assertions = collect_inherited_assertions(key, &schema_set.arenas);
        assert!(assertions.is_empty());
    }
}
