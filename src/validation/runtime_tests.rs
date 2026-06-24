use super::super::info::{NoNamespaceSchemaLocationHint, SchemaLocationHint};
use super::super::validator::SchemaValidator;
use super::*;
use crate::namespace::context::NamespaceContextSnapshot;
use crate::pipeline::load_and_process_schema;

/// A simple test sink that collects errors
struct TestSink {
    errors: Vec<ValidationError>,
    warnings: Vec<ValidationWarning>,
}

impl TestSink {
    fn new() -> Self {
        TestSink {
            errors: Vec::new(),
            warnings: Vec::new(),
        }
    }
}

impl ValidationSink for TestSink {
    fn on_error(&mut self, error: ValidationError) {
        self.errors.push(error);
    }
    fn on_warning(&mut self, warning: ValidationWarning) {
        self.warnings.push(warning);
    }
}

fn empty_ns_context() -> NamespaceContextSnapshot {
    NamespaceContextSnapshot {
        default_ns: None,
        bindings: Vec::new(),
    }
}

fn load_schema(xsd: &str) -> SchemaSet {
    let mut schema_set = SchemaSet::new();
    load_and_process_schema(xsd.as_bytes(), "test.xsd", &mut schema_set, None)
        .expect("failed to load schema");
    schema_set
}

#[cfg(feature = "xsd11")]
fn load_schema_xsd11(xsd: &str) -> SchemaSet {
    let mut schema_set = SchemaSet::xsd11();
    load_and_process_schema(xsd.as_bytes(), "test.xsd", &mut schema_set, None)
        .expect("failed to load schema");
    schema_set
}

#[test]
fn test_simple_element_valid() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root" type="xs:string"/>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    let info = v.validate_element("root", "", None, None, &ns);
    assert_eq!(info.validity, SchemaValidity::Valid);
    assert!(info.element_decl.is_some());
    assert!(info.schema_type.is_some());

    v.validate_end_of_attributes();
    v.validate_text("hello world");

    let end_info = v.validate_end_element();
    assert_eq!(end_info.validity, SchemaValidity::Valid);

    assert!(v.end_validation().is_ok());
    assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
}

#[test]
fn test_unknown_element_strict() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root" type="xs:string"/>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    let info = v.validate_element("unknown", "", None, None, &ns);
    assert_eq!(info.validity, SchemaValidity::Invalid);

    // Should have cvc-elt.1 error
    assert!(v.sink.errors.iter().any(|e| e.constraint == "cvc-elt.1"));

    v.validate_end_of_attributes();
    v.validate_end_element();
    assert!(v.end_validation().is_ok());
}

#[test]
fn test_sequence_content_model() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element name="a" type="xs:string"/>
                        <xs:element name="b" type="xs:string"/>
                    </xs:sequence>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    // Open root
    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    // Children in correct order
    v.validate_element("a", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_text("hello");
    v.validate_end_element();

    v.validate_element("b", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_text("world");
    v.validate_end_element();

    // Close root
    v.validate_end_element();
    assert!(v.end_validation().is_ok());
    assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
}

#[test]
fn test_sequence_wrong_order() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element name="a" type="xs:string"/>
                        <xs:element name="b" type="xs:string"/>
                    </xs:sequence>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    // Wrong order: b before a
    v.validate_element("b", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_end_element();

    v.validate_element("a", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_end_element();

    v.validate_end_element();
    v.end_validation().ok();

    // Should have content model error
    assert!(
        v.sink
            .errors
            .iter()
            .any(|e| e.constraint == "cvc-complex-type.2.4"),
        "errors: {:?}",
        v.sink.errors
    );
}

#[test]
fn test_required_attribute_missing() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:simpleContent>
                        <xs:extension base="xs:string">
                            <xs:attribute name="id" type="xs:string" use="required"/>
                        </xs:extension>
                    </xs:simpleContent>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    // Don't provide any attributes
    v.validate_end_of_attributes();
    v.validate_text("hello");
    v.validate_end_element();
    v.end_validation().ok();

    assert!(
        v.sink
            .errors
            .iter()
            .any(|e| e.constraint == "cvc-complex-type.4"),
        "expected required attribute error, got: {:?}",
        v.sink.errors
    );
}

#[test]
fn test_duplicate_attribute() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:simpleContent>
                        <xs:extension base="xs:string">
                            <xs:attribute name="id" type="xs:string"/>
                        </xs:extension>
                    </xs:simpleContent>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_attribute("id", "", "val1");
    v.validate_attribute("id", "", "val2"); // duplicate
    v.validate_end_of_attributes();
    v.validate_text("hello");
    v.validate_end_element();
    v.end_validation().ok();

    assert!(
        v.sink
            .errors
            .iter()
            .any(|e| e.constraint == "cvc-complex-type.3"),
        "expected duplicate attribute error, got: {:?}",
        v.sink.errors
    );
}

#[test]
fn test_text_in_empty_content() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType/>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_text("not allowed");
    v.validate_end_element();
    v.end_validation().ok();

    assert!(
        v.sink
            .errors
            .iter()
            .any(|e| e.constraint == "cvc-complex-type.2.1"),
        "expected empty content error, got: {:?}",
        v.sink.errors
    );
}

#[test]
fn test_state_machine_attribute_before_element() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root" type="xs:string"/>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());

    // Try to validate attribute before any element — should error
    let info = v.validate_attribute("id", "", "val");
    assert_eq!(info.validity, SchemaValidity::Invalid);
    assert!(!v.sink.errors.is_empty());
}

#[test]
fn test_xsi_type_override() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root" type="xs:anyType"/>
            <xs:complexType name="myType">
                <xs:sequence>
                    <xs:element name="child" type="xs:string"/>
                </xs:sequence>
            </xs:complexType>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    // Use xsi:type to override the element type
    let info = v.validate_element("root", "", Some("myType"), None, &ns);
    assert_eq!(info.validity, SchemaValidity::Valid);
    // The schema_type should be the overridden type, not anyType
    assert!(info.schema_type.is_some());

    v.validate_end_of_attributes();
    v.validate_element("child", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_text("hello");
    v.validate_end_element();
    v.validate_end_element();
    v.end_validation().ok();

    assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
}

#[test]
fn test_xsi_nil_on_nillable_element() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element name="child" type="xs:string" nillable="true"/>
                    </xs:sequence>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    let info = v.validate_element("child", "", None, Some("true"), &ns);
    assert!(info.is_nil);
    assert_eq!(info.validity, SchemaValidity::Valid);

    v.validate_end_of_attributes();
    // Empty content is valid for nilled element
    v.validate_end_element();
    v.validate_end_element();
    v.end_validation().ok();

    assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
}

#[test]
fn test_end_validation_with_unclosed_elements() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root" type="xs:string"/>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    // Don't close the element — end_validation should fail
    let result = v.end_validation();
    assert!(result.is_err());
}

#[test]
fn test_local_element_with_complex_type() {
    // Local element with type="addressType" (a named complex type).
    // Verify schema_type is resolved and children are validated.
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:complexType name="addressType">
                <xs:sequence>
                    <xs:element name="street" type="xs:string"/>
                    <xs:element name="city" type="xs:string"/>
                </xs:sequence>
            </xs:complexType>
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element name="address" type="addressType"/>
                    </xs:sequence>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    let info = v.validate_element("address", "", None, None, &ns);
    assert_eq!(info.validity, SchemaValidity::Valid);
    assert!(
        info.schema_type.is_some(),
        "local element should have resolved type"
    );
    assert!(
        matches!(info.content_type, Some(ContentType::ElementOnly)),
        "addressType has element-only content, got {:?}",
        info.content_type,
    );

    v.validate_end_of_attributes();

    // Children should be validated against the content model
    v.validate_element("street", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_text("123 Main St");
    v.validate_end_element();

    v.validate_element("city", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_text("Springfield");
    v.validate_end_element();

    v.validate_end_element(); // close address
    v.validate_end_element(); // close root
    assert!(v.end_validation().is_ok());
    assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
}

#[test]
fn test_local_element_with_simple_type_resolved() {
    // Local element with type="xs:integer". Verify schema_type is set.
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element name="count" type="xs:integer"/>
                    </xs:sequence>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    let info = v.validate_element("count", "", None, None, &ns);
    assert_eq!(info.validity, SchemaValidity::Valid);
    assert!(
        info.schema_type.is_some(),
        "local element should have resolved type for xs:integer"
    );
    assert_eq!(info.content_type, Some(ContentType::TextOnly));

    v.validate_end_of_attributes();
    v.validate_text("42");
    v.validate_end_element();

    v.validate_end_element();
    assert!(v.end_validation().is_ok());
    assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
}

#[test]
fn test_local_element_complex_type_rejects_wrong_children() {
    // Local element with type="myType" containing wrong child element.
    // Verify content model error is reported.
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:complexType name="myType">
                <xs:sequence>
                    <xs:element name="expected" type="xs:string"/>
                </xs:sequence>
            </xs:complexType>
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element name="item" type="myType"/>
                    </xs:sequence>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    v.validate_element("item", "", None, None, &ns);
    v.validate_end_of_attributes();

    // Wrong child element - should trigger content model error
    v.validate_element("wrong", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_end_element();

    v.validate_end_element(); // close item
    v.validate_end_element(); // close root
    v.end_validation().ok();

    assert!(
        v.sink
            .errors
            .iter()
            .any(|e| e.constraint == "cvc-complex-type.2.4"),
        "expected content model error for wrong child, got: {:?}",
        v.sink.errors
    );
}

#[test]
fn test_local_element_with_inline_type() {
    // Local element with inline <xs:simpleType> — verify that the inline
    // type is resolved and facets are enforced.
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element name="code">
                            <xs:simpleType>
                                <xs:restriction base="xs:string">
                                    <xs:maxLength value="10"/>
                                </xs:restriction>
                            </xs:simpleType>
                        </xs:element>
                    </xs:sequence>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    // Verify schema internals: inline type is assembled and propagated
    let root_name = schema_set
        .name_table
        .get("root")
        .expect("name 'root' not interned");
    let root_key = schema_set
        .lookup_element(None, root_name)
        .expect("root element not found");
    let root_type = schema_set.arenas.elements[root_key]
        .resolved_type
        .expect("root element has no resolved_type");
    let ct_key = match root_type {
        crate::ids::TypeKey::Complex(k) => k,
        _ => panic!("root type is not complex"),
    };
    let ct = &schema_set.arenas.complex_types[ct_key];
    assert!(
        !ct.resolved_content_particle_types.is_empty(),
        "resolved_content_particle_types is empty"
    );
    assert!(
        ct.resolved_content_particle_types[0].is_some(),
        "resolved_content_particle_types[0] is None"
    );

    // Valid value (within maxLength=10)
    {
        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);
        v.validate_end_of_attributes();

        let info = v.validate_element("code", "", None, None, &ns);
        assert_eq!(info.validity, SchemaValidity::Valid);
        assert!(info.schema_type.is_some(), "inline type not resolved");
        assert_eq!(info.content_type, Some(ContentType::TextOnly));

        v.validate_end_of_attributes();
        v.validate_text("ABC");
        v.validate_end_element();

        v.validate_end_element();
        assert!(v.end_validation().is_ok());
        assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
    }

    // Invalid value (exceeds maxLength=10) — facet must be enforced
    {
        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);
        v.validate_end_of_attributes();

        v.validate_element("code", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_text("this text exceeds maxLength of 10");
        v.validate_end_element();

        v.validate_end_element();
        v.end_validation().ok();

        assert!(
            !v.sink.errors.is_empty(),
            "expected facet error for text exceeding maxLength=10"
        );
    }
}

#[test]
fn test_xsi_type_on_local_element() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:complexType name="baseType">
                <xs:sequence>
                    <xs:element name="name" type="xs:string"/>
                </xs:sequence>
            </xs:complexType>
            <xs:complexType name="derivedType">
                <xs:complexContent>
                    <xs:extension base="baseType">
                        <xs:sequence>
                            <xs:element name="extra" type="xs:string"/>
                        </xs:sequence>
                    </xs:extension>
                </xs:complexContent>
            </xs:complexType>
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element name="item" type="baseType"/>
                    </xs:sequence>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    let info = v.validate_element("item", "", Some("derivedType"), None, &ns);
    assert_eq!(info.validity, SchemaValidity::Valid);
    assert!(
        info.schema_type.is_some(),
        "schema_type should reflect overridden type"
    );

    v.validate_end_of_attributes();

    // derivedType = sequence(name, extra)
    v.validate_element("name", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_text("test");
    v.validate_end_element();

    v.validate_element("extra", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_text("additional");
    v.validate_end_element();

    v.validate_end_element(); // close item
    v.validate_end_element(); // close root
    assert!(v.end_validation().is_ok());
    assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
}

#[test]
fn test_group_ref_with_nillable_fixed_default() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:group name="fields">
                <xs:sequence>
                    <xs:element name="nillableField" type="xs:string" nillable="true"/>
                    <xs:element name="fixedField" type="xs:string" fixed="LOCKED"/>
                    <xs:element name="defaultField" type="xs:string" default="fallback"/>
                </xs:sequence>
            </xs:group>
            <xs:element name="root">
                <xs:complexType>
                    <xs:group ref="fields"/>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    // 1. Nillable from group — xsi:nil="true" should be accepted
    let info = v.validate_element("nillableField", "", None, Some("true"), &ns);
    assert!(info.is_nil, "nillableField should report is_nil=true");
    assert_eq!(info.validity, SchemaValidity::Valid);
    v.validate_end_of_attributes();
    v.validate_end_element();

    // 2. Fixed value mismatch from group — wrong text should produce cvc-elt.5.2.2
    v.validate_element("fixedField", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_text("WRONG");
    let end_info = v.validate_end_element();
    assert!(
        v.sink
            .errors
            .iter()
            .any(|e| e.constraint == "cvc-elt.5.2.2"),
        "expected cvc-elt.5.2.2 for fixed value mismatch, errors: {:?}",
        v.sink.errors
    );
    assert_eq!(end_info.validity, SchemaValidity::Invalid);

    // 3. Default value from group — empty content should set is_default
    v.validate_element("defaultField", "", None, None, &ns);
    v.validate_end_of_attributes();
    let end_info = v.validate_end_element();
    assert!(
        end_info.is_default,
        "defaultField with no text should report is_default=true"
    );

    v.validate_end_element(); // close root
    assert!(v.end_validation().is_ok());
    // Only the fixed-value error is expected
    assert_eq!(
        v.sink.errors.len(),
        1,
        "expected exactly 1 error (cvc-elt.5.2.2), got: {:?}",
        v.sink.errors
    );
}

// -----------------------------------------------------------------------
// Attribute group tests
// -----------------------------------------------------------------------

#[test]
fn test_attribute_group_basic() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:attributeGroup name="myAttrs">
                <xs:attribute name="color" type="xs:string"/>
                <xs:attribute name="size" type="xs:integer"/>
            </xs:attributeGroup>
            <xs:element name="root">
                <xs:complexType>
                    <xs:attributeGroup ref="myAttrs"/>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    let info = v.validate_attribute("color", "", "red");
    assert_eq!(info.validity, SchemaValidity::Valid);

    let info = v.validate_attribute("size", "", "42");
    assert_eq!(info.validity, SchemaValidity::Valid);

    v.validate_end_of_attributes();
    v.validate_end_element();
    assert!(v.end_validation().is_ok());
    assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
}

#[test]
fn test_attribute_group_nested() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:attributeGroup name="inner">
                <xs:attribute name="depth" type="xs:integer"/>
            </xs:attributeGroup>
            <xs:attributeGroup name="outer">
                <xs:attribute name="width" type="xs:string"/>
                <xs:attributeGroup ref="inner"/>
            </xs:attributeGroup>
            <xs:element name="root">
                <xs:complexType>
                    <xs:attributeGroup ref="outer"/>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    let info = v.validate_attribute("width", "", "100px");
    assert_eq!(info.validity, SchemaValidity::Valid);

    // "depth" comes from the nested inner group
    let info = v.validate_attribute("depth", "", "5");
    assert_eq!(info.validity, SchemaValidity::Valid);

    v.validate_end_of_attributes();
    v.validate_end_element();
    assert!(v.end_validation().is_ok());
    assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
}

#[test]
fn test_attribute_group_required_missing() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:attributeGroup name="myAttrs">
                <xs:attribute name="id" type="xs:string" use="required"/>
            </xs:attributeGroup>
            <xs:element name="root">
                <xs:complexType>
                    <xs:attributeGroup ref="myAttrs"/>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    // Do NOT supply the required "id" attribute
    v.validate_end_of_attributes();
    v.validate_end_element();
    assert!(v.end_validation().is_ok());

    assert!(
        v.sink
            .errors
            .iter()
            .any(|e| e.constraint == "cvc-complex-type.4"),
        "expected cvc-complex-type.4 for missing required attribute from group, errors: {:?}",
        v.sink.errors
    );
}

// -----------------------------------------------------------------------
// Wildcard tests
// -----------------------------------------------------------------------

#[test]
fn test_wildcard_namespace_other_rejects_same_ns() {
    // anyAttribute namespace="##other" should reject attributes in the same
    // (target) namespace.
    let schema_set = load_schema(
        r###"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
                    targetNamespace="http://example.com/ns"
                    xmlns:tns="http://example.com/ns">
            <xs:element name="root">
                <xs:complexType>
                    <xs:anyAttribute namespace="##other" processContents="skip"/>
                </xs:complexType>
            </xs:element>
        </xs:schema>"###,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let tns_id = schema_set.name_table.add("http://example.com/ns");
    let tns_prefix = schema_set.name_table.add("tns");
    let ns = NamespaceContextSnapshot {
        default_ns: Some(tns_id),
        bindings: vec![(tns_prefix, tns_id)],
    };

    v.validate_element("root", "http://example.com/ns", None, None, &ns);

    // Attribute in a *different* namespace should be accepted (skip → NotKnown)
    let info = v.validate_attribute("foreign", "http://other.com/ns", "val");
    assert_ne!(info.validity, SchemaValidity::Invalid);

    // Attribute in the *same* (target) namespace should be rejected
    let info = v.validate_attribute("local", "http://example.com/ns", "val");
    assert_eq!(info.validity, SchemaValidity::Invalid);
    assert!(
        v.sink
            .errors
            .iter()
            .any(|e| e.constraint == "cvc-complex-type.3.2.2"),
        "expected cvc-complex-type.3.2.2, errors: {:?}",
        v.sink.errors
    );

    v.validate_end_of_attributes();
    v.validate_end_element();
    assert!(v.end_validation().is_ok());
}

#[test]
fn test_wildcard_process_contents_strict() {
    // processContents="strict" with a global attribute declaration
    let schema_set = load_schema(
        r###"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:attribute name="globalAttr" type="xs:integer"/>
            <xs:element name="root">
                <xs:complexType>
                    <xs:anyAttribute namespace="##any" processContents="strict"/>
                </xs:complexType>
            </xs:element>
        </xs:schema>"###,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);

    // Valid global attribute with correct value
    let info = v.validate_attribute("globalAttr", "", "42");
    assert_eq!(info.validity, SchemaValidity::Valid);
    assert!(info.attribute_decl.is_some());

    v.validate_end_of_attributes();
    v.validate_end_element();
    assert!(v.end_validation().is_ok());
    assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
}

#[test]
fn test_wildcard_process_contents_strict_unknown() {
    // processContents="strict" with an unknown attribute -> error
    let schema_set = load_schema(
        r###"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:anyAttribute namespace="##any" processContents="strict"/>
                </xs:complexType>
            </xs:element>
        </xs:schema>"###,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);

    let info = v.validate_attribute("unknownAttr", "", "anything");
    assert_eq!(info.validity, SchemaValidity::Invalid);
    assert!(
        v.sink
            .errors
            .iter()
            .any(|e| e.constraint == "cvc-assess-attr.1.2"),
        "expected cvc-assess-attr.1.2 for strict wildcard with unknown attr, errors: {:?}",
        v.sink.errors
    );

    v.validate_end_of_attributes();
    v.validate_end_element();
    assert!(v.end_validation().is_ok());
}

#[test]
fn test_wildcard_process_contents_lax() {
    // processContents="lax" with an unknown attribute -> no error
    let schema_set = load_schema(
        r###"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:anyAttribute namespace="##any" processContents="lax"/>
                </xs:complexType>
            </xs:element>
        </xs:schema>"###,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);

    // Unknown attr with lax → accepted (NotKnown, no error)
    let info = v.validate_attribute("whatever", "", "anything");
    assert_ne!(info.validity, SchemaValidity::Invalid);

    v.validate_end_of_attributes();
    v.validate_end_element();
    assert!(v.end_validation().is_ok());
    assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
}

#[test]
fn test_wildcard_process_contents_skip() {
    // processContents="skip" should accept anything without validation
    let schema_set = load_schema(
        r###"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:attribute name="globalAttr" type="xs:integer"/>
            <xs:element name="root">
                <xs:complexType>
                    <xs:anyAttribute namespace="##any" processContents="skip"/>
                </xs:complexType>
            </xs:element>
        </xs:schema>"###,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);

    // Even an invalid value for a known global attr should pass with skip (NotKnown)
    let info = v.validate_attribute("globalAttr", "", "not_an_integer");
    assert_ne!(info.validity, SchemaValidity::Invalid);

    // Unknown attributes also accepted (NotKnown)
    let info = v.validate_attribute("madeUp", "", "anything");
    assert_ne!(info.validity, SchemaValidity::Invalid);

    v.validate_end_of_attributes();
    v.validate_end_element();
    assert!(v.end_validation().is_ok());
    assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
}

// -----------------------------------------------------------------------
// Issue fix tests: attribute ref, prohibited, group wildcard, defaults
// -----------------------------------------------------------------------

#[test]
fn test_attribute_ref_basic() {
    // Issue 1: <xs:attribute ref="globalAttr"/> should match and validate
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:attribute name="globalAttr" type="xs:integer"/>
            <xs:element name="root">
                <xs:complexType>
                    <xs:simpleContent>
                        <xs:extension base="xs:string">
                            <xs:attribute ref="globalAttr"/>
                        </xs:extension>
                    </xs:simpleContent>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    let info = v.validate_attribute("globalAttr", "", "42");
    assert_eq!(
        info.validity,
        SchemaValidity::Valid,
        "attribute ref should match by resolved name; errors: {:?}",
        v.sink.errors
    );
    assert!(
        info.attribute_decl.is_some(),
        "should resolve attribute decl key"
    );

    v.validate_end_of_attributes();
    v.validate_text("hello");
    v.validate_end_element();
    assert!(v.end_validation().is_ok());
    assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
}

#[test]
fn test_attribute_ref_required_missing() {
    // Issue 1: required attribute ref should be checked properly
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:attribute name="reqAttr" type="xs:string"/>
            <xs:element name="root">
                <xs:complexType>
                    <xs:simpleContent>
                        <xs:extension base="xs:string">
                            <xs:attribute ref="reqAttr" use="required"/>
                        </xs:extension>
                    </xs:simpleContent>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    // Don't provide the required attribute
    v.validate_end_of_attributes();
    v.validate_text("hello");
    v.validate_end_element();
    v.end_validation().ok();

    assert!(
        v.sink
            .errors
            .iter()
            .any(|e| e.constraint == "cvc-complex-type.4"),
        "expected cvc-complex-type.4 for missing required ref attribute, errors: {:?}",
        v.sink.errors
    );
}

#[test]
fn test_prohibited_attribute_wildcard_rescue() {
    // cvc-complex-type.3.2.2: clause 3.2.2 (wildcard) is independent of 3.2.1.
    // A matching wildcard rescues a prohibited attribute (attZ002 / addB034 pattern).

    // Case A: prohibited + matching anyAttribute ##any → VALID (wildcard rescues)
    let schema_a = load_schema(
        r###"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:attribute name="blocked" use="prohibited"/>
                    <xs:anyAttribute namespace="##any" processContents="skip"/>
                </xs:complexType>
            </xs:element>
        </xs:schema>"###,
    );
    let validator = SchemaValidator::new(&schema_a, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();
    v.validate_element("root", "", None, None, &ns);
    let _info = v.validate_attribute("blocked", "", "value");
    // skip wildcard → no error (validity is NotKnown, which is correct PSVI for skip)
    assert!(
        !v.sink
            .errors
            .iter()
            .any(|e| e.constraint == "cvc-complex-type.3.2.2"),
        "wildcard should rescue prohibited attribute; errors: {:?}",
        v.sink.errors
    );
    v.validate_end_of_attributes();
    v.validate_end_element();
    assert!(v.end_validation().is_ok());

    // Case B: prohibited with no matching wildcard → INVALID (truly prohibited)
    let schema_b = load_schema(
        r###"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:attribute name="blocked" use="prohibited"/>
                </xs:complexType>
            </xs:element>
        </xs:schema>"###,
    );
    let validator = SchemaValidator::new(&schema_b, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();
    v.validate_element("root", "", None, None, &ns);
    let info = v.validate_attribute("blocked", "", "value");
    assert_eq!(
        info.validity,
        SchemaValidity::Invalid,
        "prohibited without wildcard must be rejected"
    );
    assert!(
        v.sink
            .errors
            .iter()
            .any(|e| e.constraint == "cvc-complex-type.3.2.2" && e.message.contains("prohibited")),
        "expected 'prohibited' error, errors: {:?}",
        v.sink.errors
    );
}

#[test]
fn test_group_wildcard_honored() {
    // Issue 3: anyAttribute inside attributeGroup should be honored
    let schema_set = load_schema(
        r###"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:attributeGroup name="flexAttrs">
                <xs:attribute name="known" type="xs:string"/>
                <xs:anyAttribute namespace="##any" processContents="skip"/>
            </xs:attributeGroup>
            <xs:element name="root">
                <xs:complexType>
                    <xs:attributeGroup ref="flexAttrs"/>
                </xs:complexType>
            </xs:element>
        </xs:schema>"###,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);

    // Known attribute from the group
    let info = v.validate_attribute("known", "", "hello");
    assert_eq!(info.validity, SchemaValidity::Valid);

    // Unknown attribute should be accepted via the group's anyAttribute
    let info = v.validate_attribute("extra", "", "anything");
    assert_ne!(
        info.validity,
        SchemaValidity::Invalid,
        "group wildcard should accept unknown attributes; errors: {:?}",
        v.sink.errors
    );

    v.validate_end_of_attributes();
    v.validate_end_element();
    assert!(v.end_validation().is_ok());
    assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
}

#[test]
fn test_default_from_global_declaration() {
    // Issue 4: default value from global attribute decl should be exposed
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:attribute name="lang" type="xs:string" default="en"/>
            <xs:element name="root">
                <xs:complexType>
                    <xs:simpleContent>
                        <xs:extension base="xs:string">
                            <xs:attribute ref="lang"/>
                        </xs:extension>
                    </xs:simpleContent>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    // Do NOT provide the "lang" attribute — it should appear as a default
    v.validate_end_of_attributes();

    let defaults = v.get_default_attributes();
    assert!(
        defaults.iter().any(|d| {
            let name = schema_set.name_table.resolve(d.local_name);
            name == "lang" && d.value == "en"
        }),
        "expected default attribute lang='en', got: {:?}",
        defaults
            .iter()
            .map(|d| (schema_set.name_table.resolve(d.local_name), &d.value))
            .collect::<Vec<_>>()
    );

    v.validate_text("hello");
    v.validate_end_element();
    assert!(v.end_validation().is_ok());
    assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
}

#[test]
fn test_default_from_global_declaration_in_group() {
    // Issue 4: default from global decl via attributeGroup ref
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:attribute name="lang" type="xs:string" default="en"/>
            <xs:attributeGroup name="grp">
                <xs:attribute ref="lang"/>
            </xs:attributeGroup>
            <xs:element name="root">
                <xs:complexType>
                    <xs:attributeGroup ref="grp"/>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    let defaults = v.get_default_attributes();
    assert!(
        defaults.iter().any(|d| {
            let name = schema_set.name_table.resolve(d.local_name);
            name == "lang" && d.value == "en"
        }),
        "expected default attribute lang='en' from group, got: {:?}",
        defaults
            .iter()
            .map(|d| (schema_set.name_table.resolve(d.local_name), &d.value))
            .collect::<Vec<_>>()
    );

    v.validate_end_element();
    assert!(v.end_validation().is_ok());
    assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
}

// ── Mixed content tests ─────────────────────────────────────────────

#[test]
fn test_mixed_content_text_allowed() {
    // A mixed complex type with a sequence of child elements.
    // Text between child elements should be valid.
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType mixed="true">
                    <xs:sequence>
                        <xs:element name="a" type="xs:string"/>
                        <xs:element name="b" type="xs:string"/>
                    </xs:sequence>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    let info = v.validate_element("root", "", None, None, &ns);
    assert_eq!(info.content_type, Some(ContentType::Mixed));
    v.validate_end_of_attributes();

    // Text before first child
    v.validate_text("hello ");

    // Child <a>
    v.validate_element("a", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_text("val_a");
    v.validate_end_element();

    // Text between children
    v.validate_text(" middle ");

    // Child <b>
    v.validate_element("b", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_text("val_b");
    v.validate_end_element();

    // Text after last child
    v.validate_text(" world");

    v.validate_end_element();
    assert!(v.end_validation().is_ok());
    assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
}

#[test]
fn test_mixed_content_text_only_incomplete_model() {
    // A mixed complex type with required children in a sequence.
    // Pushing only text (no child elements) → content model incomplete error.
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType mixed="true">
                    <xs:sequence>
                        <xs:element name="a" type="xs:string"/>
                    </xs:sequence>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    // Only text, no child elements
    v.validate_text("just text");

    v.validate_end_element();
    v.end_validation().ok();

    // Content model is incomplete because required child <a> was never provided
    assert!(
        v.sink
            .errors
            .iter()
            .any(|e| e.constraint == "cvc-complex-type.2.4"),
        "expected content model incomplete error, got: {:?}",
        v.sink.errors
    );
}

#[test]
fn test_mixed_content_whitespace_accumulated() {
    // A mixed complex type should accumulate whitespace (not discard it
    // like element-only content does). We push whitespace between
    // required children to verify it is accepted without error.
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType mixed="true">
                    <xs:sequence>
                        <xs:element name="a" type="xs:string"/>
                    </xs:sequence>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    let info = v.validate_element("root", "", None, None, &ns);
    assert_eq!(info.content_type, Some(ContentType::Mixed));
    v.validate_end_of_attributes();

    // Whitespace before the child — accumulated in mixed, discarded in element-only
    v.validate_whitespace("   ");

    v.validate_element("a", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_text("val");
    v.validate_end_element();

    // Whitespace after the child
    v.validate_whitespace("  \n  ");

    v.validate_end_element();
    assert!(v.end_validation().is_ok());
    assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
}

#[test]
fn test_element_only_rejects_non_whitespace_text() {
    // A non-mixed complex type with a sequence. Pushing non-whitespace
    // text should produce a cvc-complex-type.2.3 error.
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element name="a" type="xs:string"/>
                    </xs:sequence>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    let info = v.validate_element("root", "", None, None, &ns);
    assert_eq!(info.content_type, Some(ContentType::ElementOnly));
    v.validate_end_of_attributes();

    // Non-whitespace text in element-only content
    v.validate_text("not allowed here");

    v.validate_element("a", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_text("val");
    v.validate_end_element();

    v.validate_end_element();
    v.end_validation().ok();

    assert!(
        v.sink
            .errors
            .iter()
            .any(|e| e.constraint == "cvc-complex-type.2.3"),
        "expected element-only text error, got: {:?}",
        v.sink.errors
    );
}

#[test]
fn test_mixed_content_wrong_child_order() {
    // A mixed complex type with xs:sequence(a, b). Children in wrong
    // order should still produce a content model error — mixed allows
    // text but still enforces child element order.
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType mixed="true">
                    <xs:sequence>
                        <xs:element name="a" type="xs:string"/>
                        <xs:element name="b" type="xs:string"/>
                    </xs:sequence>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    v.validate_text("some text ");

    // Wrong order: b before a
    v.validate_element("b", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_end_element();

    v.validate_text(" more text ");

    v.validate_element("a", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_end_element();

    v.validate_end_element();
    v.end_validation().ok();

    assert!(
        v.sink
            .errors
            .iter()
            .any(|e| e.constraint == "cvc-complex-type.2.4"),
        "expected content model error for wrong child order, got: {:?}",
        v.sink.errors
    );
}

#[test]
fn test_mixed_content_model_complete() {
    // A mixed complex type where all required children are provided.
    // Text is interleaved; content model should be complete → valid.
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType mixed="true">
                    <xs:sequence>
                        <xs:element name="a" type="xs:string"/>
                    </xs:sequence>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    let info = v.validate_element("root", "", None, None, &ns);
    assert_eq!(info.content_type, Some(ContentType::Mixed));
    v.validate_end_of_attributes();

    // Text before required child
    v.validate_text("prefix ");

    // Provide the required child
    v.validate_element("a", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_text("child value");
    v.validate_end_element();

    // Text after child — content model should be complete
    v.validate_text(" suffix");

    let end_info = v.validate_end_element();
    assert_eq!(end_info.validity, SchemaValidity::Valid);

    assert!(v.end_validation().is_ok());
    assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
}

#[test]
fn test_minoccurs_zero_element_in_sequence() {
    // An element with minOccurs="0" inside a sequence.
    // Omitting the optional element should produce no errors.
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element name="a" type="xs:string" minOccurs="0"/>
                    </xs:sequence>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();
    // Do NOT push child <a> — it is optional
    let end_info = v.validate_end_element();
    assert_eq!(end_info.validity, SchemaValidity::Valid);

    assert!(v.end_validation().is_ok());
    assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
}

#[test]
fn test_maxoccurs_unbounded_element_in_sequence() {
    // An element with maxOccurs="unbounded" inside a sequence.
    // Pushing multiple children should produce no errors.
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element name="a" type="xs:string" maxOccurs="unbounded"/>
                    </xs:sequence>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    // Push three <a> children — all should be accepted
    for _ in 0..3 {
        v.validate_element("a", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_text("value");
        v.validate_end_element();
    }

    let end_info = v.validate_end_element();
    assert_eq!(end_info.validity, SchemaValidity::Valid);

    assert!(v.end_validation().is_ok());
    assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
}

#[test]
fn test_mixed_content_optional_children_text_only() {
    // Mixed complex type where all children are optional.
    // Pushing only text (no child elements) should be valid.
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType mixed="true">
                    <xs:sequence>
                        <xs:element name="a" type="xs:string" minOccurs="0"/>
                        <xs:element name="b" type="xs:string" minOccurs="0"/>
                    </xs:sequence>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    // Only text, no child elements
    v.validate_text("just text content");

    let end_info = v.validate_end_element();
    assert_eq!(end_info.validity, SchemaValidity::Valid);

    assert!(v.end_validation().is_ok());
    assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
}

#[test]
fn test_nil_element_rejects_child_elements() {
    // cvc-elt.3.2.1: A nilled element must not have child element content
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element name="parent" nillable="true">
                            <xs:complexType>
                                <xs:sequence>
                                    <xs:element name="child" type="xs:string"/>
                                </xs:sequence>
                            </xs:complexType>
                        </xs:element>
                    </xs:sequence>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    // Open "parent" with xsi:nil="true"
    let info = v.validate_element("parent", "", None, Some("true"), &ns);
    assert!(info.is_nil);
    v.validate_end_of_attributes();

    // Try to add a child element — should trigger cvc-elt.3.2.1
    v.validate_element("child", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_end_element();

    v.validate_end_element(); // close parent
    v.validate_end_element(); // close root
    v.end_validation().ok();

    assert!(
        v.sink
            .errors
            .iter()
            .any(|e| e.constraint == "cvc-elt.3.2.1"),
        "expected cvc-elt.3.2.1 error for child element in nilled parent, got: {:?}",
        v.sink.errors
    );
}

#[test]
fn test_nil_element_allows_attributes_only() {
    // A nilled element with only attributes (no child elements, no text) is valid
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element name="item" nillable="true">
                            <xs:complexType>
                                <xs:sequence>
                                    <xs:element name="child" type="xs:string"/>
                                </xs:sequence>
                                <xs:attribute name="id" type="xs:string"/>
                            </xs:complexType>
                        </xs:element>
                    </xs:sequence>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    let info = v.validate_element("item", "", None, Some("true"), &ns);
    assert!(info.is_nil);
    // Attribute on nilled element is valid
    v.validate_attribute("id", "", "123");
    v.validate_end_of_attributes();

    // No child elements, no text — just close
    v.validate_end_element(); // close item
    v.validate_end_element(); // close root
    v.end_validation().ok();

    assert!(
        v.sink.errors.is_empty(),
        "nilled element with attributes only should be valid, got: {:?}",
        v.sink.errors
    );
}

// -----------------------------------------------------------------------
// Identity constraint regression tests
// -----------------------------------------------------------------------

/// Test 1: Simple key constraint — duplicate detection (cvc-identity-constraint.4.2.2)
#[test]
fn test_ic_key_duplicate() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element name="item" maxOccurs="unbounded">
                            <xs:complexType>
                                <xs:attribute name="id" type="xs:string" use="required"/>
                            </xs:complexType>
                        </xs:element>
                    </xs:sequence>
                </xs:complexType>
                <xs:key name="itemKey">
                    <xs:selector xpath="./item"/>
                    <xs:field xpath="@id"/>
                </xs:key>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    // First item: @id="A"
    v.validate_element("item", "", None, None, &ns);
    v.validate_attribute("id", "", "A");
    v.validate_end_of_attributes();
    v.validate_end_element();

    // Second item: @id="A" — duplicate
    v.validate_element("item", "", None, None, &ns);
    v.validate_attribute("id", "", "A");
    v.validate_end_of_attributes();
    v.validate_end_element();

    v.validate_end_element(); // </root>
    v.end_validation().ok();

    assert!(
        v.sink
            .errors
            .iter()
            .any(|e| e.constraint == "cvc-identity-constraint.4.2.2"),
        "Expected duplicate key error, got: {:?}",
        v.sink.errors
    );
}

/// Test 2: Unique constraint — incomplete allowed, duplicates rejected
#[test]
fn test_ic_unique_incomplete_ok_duplicate_rejected() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element name="item" maxOccurs="unbounded">
                            <xs:complexType>
                                <xs:attribute name="id" type="xs:string"/>
                            </xs:complexType>
                        </xs:element>
                    </xs:sequence>
                </xs:complexType>
                <xs:unique name="itemUnique">
                    <xs:selector xpath="./item"/>
                    <xs:field xpath="@id"/>
                </xs:unique>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    // Item without @id (incomplete key sequence — ok for unique)
    v.validate_element("item", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_end_element();

    // Item with @id="X"
    v.validate_element("item", "", None, None, &ns);
    v.validate_attribute("id", "", "X");
    v.validate_end_of_attributes();
    v.validate_end_element();

    // Item with @id="X" — duplicate
    v.validate_element("item", "", None, None, &ns);
    v.validate_attribute("id", "", "X");
    v.validate_end_of_attributes();
    v.validate_end_element();

    v.validate_end_element(); // </root>
    v.end_validation().ok();

    let dup_errors: Vec<_> = v
        .sink
        .errors
        .iter()
        .filter(|e| e.constraint == "cvc-identity-constraint.4.2.2")
        .collect();
    assert_eq!(
        dup_errors.len(),
        1,
        "Expected exactly 1 duplicate error, got: {:?}",
        dup_errors
    );
}

/// Test 3: Keyref cross-reference — matching + missing (cvc-identity-constraint.4.3)
#[test]
fn test_ic_keyref_matching_and_missing() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element name="item" maxOccurs="unbounded">
                            <xs:complexType>
                                <xs:attribute name="id" type="xs:string" use="required"/>
                            </xs:complexType>
                        </xs:element>
                        <xs:element name="ref" maxOccurs="unbounded">
                            <xs:complexType>
                                <xs:attribute name="ref" type="xs:string" use="required"/>
                            </xs:complexType>
                        </xs:element>
                    </xs:sequence>
                </xs:complexType>
                <xs:key name="itemKey">
                    <xs:selector xpath="./item"/>
                    <xs:field xpath="@id"/>
                </xs:key>
                <xs:keyref name="itemRef" refer="itemKey">
                    <xs:selector xpath="./ref"/>
                    <xs:field xpath="@ref"/>
                </xs:keyref>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    // Item with @id="A"
    v.validate_element("item", "", None, None, &ns);
    v.validate_attribute("id", "", "A");
    v.validate_end_of_attributes();
    v.validate_end_element();

    // Ref with @ref="A" — matches
    v.validate_element("ref", "", None, None, &ns);
    v.validate_attribute("ref", "", "A");
    v.validate_end_of_attributes();
    v.validate_end_element();

    // Ref with @ref="B" — no match
    v.validate_element("ref", "", None, None, &ns);
    v.validate_attribute("ref", "", "B");
    v.validate_end_of_attributes();
    v.validate_end_element();

    v.validate_end_element(); // </root>
    v.end_validation().ok();

    let keyref_errors: Vec<_> = v
        .sink
        .errors
        .iter()
        .filter(|e| e.constraint == "cvc-identity-constraint.4.3")
        .collect();
    assert_eq!(
        keyref_errors.len(),
        1,
        "Expected 1 keyref error for missing 'B', got: {:?}",
        keyref_errors
    );
}

/// Test 4: Element field value — field matches element text content
#[test]
fn test_ic_element_field_value() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element name="item" maxOccurs="unbounded">
                            <xs:complexType>
                                <xs:sequence>
                                    <xs:element name="code" type="xs:string"/>
                                </xs:sequence>
                            </xs:complexType>
                        </xs:element>
                    </xs:sequence>
                </xs:complexType>
                <xs:key name="codeKey">
                    <xs:selector xpath="./item"/>
                    <xs:field xpath="code"/>
                </xs:key>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    // First item with code="X"
    v.validate_element("item", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_element("code", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_text("X");
    v.validate_end_element(); // </code>
    v.validate_end_element(); // </item>

    // Second item with code="X" — duplicate
    v.validate_element("item", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_element("code", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_text("X");
    v.validate_end_element(); // </code>
    v.validate_end_element(); // </item>

    v.validate_end_element(); // </root>
    v.end_validation().ok();

    assert!(
        v.sink
            .errors
            .iter()
            .any(|e| e.constraint == "cvc-identity-constraint.4.2.2"),
        "Expected duplicate key error for element field, got: {:?}",
        v.sink.errors
    );
}

/// Test 5: Attribute field value — field matches @attr
#[test]
fn test_ic_attribute_field_value() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element name="item" maxOccurs="unbounded">
                            <xs:complexType>
                                <xs:attribute name="val" type="xs:string" use="required"/>
                            </xs:complexType>
                        </xs:element>
                    </xs:sequence>
                </xs:complexType>
                <xs:unique name="valUnique">
                    <xs:selector xpath="./item"/>
                    <xs:field xpath="@val"/>
                </xs:unique>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    // Two items with different values — should be fine
    v.validate_element("item", "", None, None, &ns);
    v.validate_attribute("val", "", "alpha");
    v.validate_end_of_attributes();
    v.validate_end_element();

    v.validate_element("item", "", None, None, &ns);
    v.validate_attribute("val", "", "beta");
    v.validate_end_of_attributes();
    v.validate_end_element();

    v.validate_end_element();
    v.end_validation().ok();

    assert!(
        v.sink.errors.is_empty(),
        "Expected no errors for unique values, got: {:?}",
        v.sink.errors
    );
}

/// Test 7: ID duplicate detection (cvc-id.2)
#[test]
fn test_id_duplicate() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element name="item" maxOccurs="unbounded">
                            <xs:complexType>
                                <xs:attribute name="id" type="xs:ID" use="required"/>
                            </xs:complexType>
                        </xs:element>
                    </xs:sequence>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    // First item: @id="a1"
    v.validate_element("item", "", None, None, &ns);
    v.validate_attribute("id", "", "a1");
    v.validate_end_of_attributes();
    v.validate_end_element();

    // Second item: @id="a1" — duplicate ID
    v.validate_element("item", "", None, None, &ns);
    v.validate_attribute("id", "", "a1");
    v.validate_end_of_attributes();
    v.validate_end_element();

    v.validate_end_element();
    v.end_validation().ok();

    assert!(
        v.sink.errors.iter().any(|e| e.constraint == "cvc-id.2"),
        "Expected duplicate ID error, got: {:?}",
        v.sink.errors
    );
}

/// Test 8: IDREF validation — valid + missing reference (cvc-id.1)
#[test]
fn test_idref_valid_and_missing() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element name="item" maxOccurs="unbounded">
                            <xs:complexType>
                                <xs:attribute name="id" type="xs:ID" use="required"/>
                            </xs:complexType>
                        </xs:element>
                        <xs:element name="link" maxOccurs="unbounded">
                            <xs:complexType>
                                <xs:attribute name="ref" type="xs:IDREF" use="required"/>
                            </xs:complexType>
                        </xs:element>
                    </xs:sequence>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    // Define ID
    v.validate_element("item", "", None, None, &ns);
    v.validate_attribute("id", "", "x1");
    v.validate_end_of_attributes();
    v.validate_end_element();

    // Valid IDREF
    v.validate_element("link", "", None, None, &ns);
    v.validate_attribute("ref", "", "x1");
    v.validate_end_of_attributes();
    v.validate_end_element();

    // Missing IDREF
    v.validate_element("link", "", None, None, &ns);
    v.validate_attribute("ref", "", "missing");
    v.validate_end_of_attributes();
    v.validate_end_element();

    v.validate_end_element();
    v.end_validation().ok();

    let idref_errors: Vec<_> = v
        .sink
        .errors
        .iter()
        .filter(|e| e.constraint == "cvc-id.1")
        .collect();
    assert_eq!(
        idref_errors.len(),
        1,
        "Expected 1 IDREF error for 'missing', got: {:?}",
        idref_errors
    );
}

/// Test 9: Nested selector matches (.//item with nested items)
#[test]
fn test_ic_nested_selector() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element name="item" maxOccurs="unbounded">
                            <xs:complexType>
                                <xs:sequence>
                                    <xs:element name="item" minOccurs="0" maxOccurs="unbounded">
                                        <xs:complexType>
                                            <xs:attribute name="id" type="xs:string" use="required"/>
                                        </xs:complexType>
                                    </xs:element>
                                </xs:sequence>
                                <xs:attribute name="id" type="xs:string" use="required"/>
                            </xs:complexType>
                        </xs:element>
                    </xs:sequence>
                </xs:complexType>
                <xs:unique name="allItems">
                    <xs:selector xpath=".//item"/>
                    <xs:field xpath="@id"/>
                </xs:unique>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    // Outer item @id="1"
    v.validate_element("item", "", None, None, &ns);
    v.validate_attribute("id", "", "1");
    v.validate_end_of_attributes();

    // Inner item @id="2" (nested)
    v.validate_element("item", "", None, None, &ns);
    v.validate_attribute("id", "", "2");
    v.validate_end_of_attributes();
    v.validate_end_element(); // </inner item>

    v.validate_end_element(); // </outer item>

    v.validate_end_element(); // </root>
    v.end_validation().ok();

    assert!(
        v.sink.errors.is_empty(),
        "Expected no errors for unique nested items, got: {:?}",
        v.sink.errors
    );
}

/// Test 10: Keyref + key on same element, scope-local resolution
#[test]
fn test_ic_keyref_key_same_scope() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element name="dept" maxOccurs="unbounded">
                            <xs:complexType>
                                <xs:attribute name="id" type="xs:string" use="required"/>
                            </xs:complexType>
                        </xs:element>
                        <xs:element name="emp" maxOccurs="unbounded">
                            <xs:complexType>
                                <xs:attribute name="dept" type="xs:string" use="required"/>
                            </xs:complexType>
                        </xs:element>
                    </xs:sequence>
                </xs:complexType>
                <xs:key name="deptKey">
                    <xs:selector xpath="./dept"/>
                    <xs:field xpath="@id"/>
                </xs:key>
                <xs:keyref name="empDeptRef" refer="deptKey">
                    <xs:selector xpath="./emp"/>
                    <xs:field xpath="@dept"/>
                </xs:keyref>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    // Departments
    v.validate_element("dept", "", None, None, &ns);
    v.validate_attribute("id", "", "sales");
    v.validate_end_of_attributes();
    v.validate_end_element();

    v.validate_element("dept", "", None, None, &ns);
    v.validate_attribute("id", "", "eng");
    v.validate_end_of_attributes();
    v.validate_end_element();

    // Employee referencing existing dept — valid
    v.validate_element("emp", "", None, None, &ns);
    v.validate_attribute("dept", "", "sales");
    v.validate_end_of_attributes();
    v.validate_end_element();

    // Employee referencing non-existing dept — invalid
    v.validate_element("emp", "", None, None, &ns);
    v.validate_attribute("dept", "", "hr");
    v.validate_end_of_attributes();
    v.validate_end_element();

    v.validate_end_element(); // </root>
    v.end_validation().ok();

    let keyref_errors: Vec<_> = v
        .sink
        .errors
        .iter()
        .filter(|e| e.constraint == "cvc-identity-constraint.4.3")
        .collect();
    assert_eq!(
        keyref_errors.len(),
        1,
        "Expected 1 keyref error for 'hr', got: {:?}",
        keyref_errors
    );
}

/// Test: Key constraint with no duplicates — valid document
#[test]
fn test_ic_key_no_duplicates_valid() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element name="item" maxOccurs="unbounded">
                            <xs:complexType>
                                <xs:attribute name="id" type="xs:string" use="required"/>
                            </xs:complexType>
                        </xs:element>
                    </xs:sequence>
                </xs:complexType>
                <xs:key name="pk">
                    <xs:selector xpath="./item"/>
                    <xs:field xpath="@id"/>
                </xs:key>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    v.validate_element("item", "", None, None, &ns);
    v.validate_attribute("id", "", "A");
    v.validate_end_of_attributes();
    v.validate_end_element();

    v.validate_element("item", "", None, None, &ns);
    v.validate_attribute("id", "", "B");
    v.validate_end_of_attributes();
    v.validate_end_element();

    v.validate_end_element();
    v.end_validation().ok();

    assert!(
        v.sink.errors.is_empty(),
        "Expected no errors for unique keys, got: {:?}",
        v.sink.errors
    );
}

#[cfg(feature = "xsd11")]
mod assertion_runtime_tests {
    use super::*;

    #[test]
    fn test_disabled_mode_no_overhead() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root" type="xs:string"/>
            </xs:schema>"#,
        );
        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
        assert_eq!(v.assertion_source, AssertionSource::Disabled);

        let ns = empty_ns_context();
        let info = v.validate_element("root", "", None, None, &ns);
        assert_eq!(info.validity, SchemaValidity::Valid);
        v.validate_end_of_attributes();
        v.validate_text("hello");
        let end_info = v.validate_end_element();
        assert_eq!(end_info.validity, SchemaValidity::Valid);
        v.end_validation().ok();

        assert!(
            v.sink.errors.is_empty(),
            "Expected no errors in Disabled mode, got: {:?}",
            v.sink.errors
        );
    }

    #[test]
    fn test_new_strips_process_assertions_flag() {
        // SchemaValidator::new() silently strips PROCESS_ASSERTIONS,
        // preventing the flag/source mismatch that would panic at runtime.
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root" type="xs:string"/>
            </xs:schema>"#,
        );
        let flags = ValidationFlags::default() | ValidationFlags::PROCESS_ASSERTIONS;
        let validator = SchemaValidator::new(&schema_set, flags);
        assert!(!validator
            .flags
            .contains(ValidationFlags::PROCESS_ASSERTIONS));
        // Validation proceeds without panic
        let mut v = validator.start_run(TestSink::new());
        let ns = empty_ns_context();
        let info = v.validate_element("root", "", None, None, &ns);
        assert_eq!(info.validity, SchemaValidity::Valid);
    }

    #[test]
    fn test_main_document_full_roundtrip() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root" type="xs:string"/>
            </xs:schema>"#,
        );
        let mut validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        validator.set_assertion_source(AssertionSource::MainDocument);
        let mut v = validator.start_run(TestSink::new());
        assert_eq!(v.assertion_source, AssertionSource::MainDocument);

        let ns = empty_ns_context();
        let info = v.validate_element("root", "", None, None, &ns);
        assert_eq!(info.validity, SchemaValidity::Valid);
        v.validate_end_of_attributes();
        v.validate_text("hello");
        let end_info = v.validate_end_element();
        assert_eq!(end_info.validity, SchemaValidity::Valid);
        v.end_validation().ok();

        assert!(
            v.sink.errors.is_empty(),
            "Expected no errors in MainDocument mode, got: {:?}",
            v.sink.errors
        );
    }

    // ── Complex-type assertion behavior tests ───────────────────────

    /// Helper: validate a full element lifecycle via fragment buffer mode.
    fn validate_with_fragment_buffer(
        xsd: &str,
        element: &str,
        attrs: &[(&str, &str)],
        text: Option<&str>,
    ) -> Vec<ValidationError> {
        let schema_set = load_schema_xsd11(xsd);
        let validator =
            SchemaValidator::new_fragment_buffer(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
        let ns = empty_ns_context();
        v.validate_element(element, "", None, None, &ns);
        for &(name, value) in attrs {
            v.validate_attribute(name, "", value);
        }
        v.validate_end_of_attributes();
        if let Some(t) = text {
            v.validate_text(t);
        }
        v.validate_end_element();
        v.end_validation().ok();
        v.sink.errors
    }

    #[test]
    fn test_assertion_pass() {
        // xs:assert on inline complexType — assertion passes
        let errors = validate_with_fragment_buffer(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="item">
                    <xs:complexType>
                        <xs:attribute name="val" type="xs:integer"/>
                        <xs:assert test="@val >= 0"/>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"#,
            "item",
            &[("val", "25")],
            None,
        );
        assert!(
            errors.is_empty(),
            "Expected no assertion errors, got: {:?}",
            errors
        );
    }

    #[test]
    fn test_assertion_fail() {
        // xs:assert on inline complexType — assertion fails
        let errors = validate_with_fragment_buffer(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="item">
                    <xs:complexType>
                        <xs:attribute name="val" type="xs:integer"/>
                        <xs:assert test="@val >= 0"/>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"#,
            "item",
            &[("val", "-5")],
            None,
        );
        let has_assertion_error = errors.iter().any(|e| e.constraint == "cvc-assertion");
        assert!(
            has_assertion_error,
            "Expected cvc-assertion error for negative @val, got: {:?}",
            errors
        );
    }

    #[test]
    fn test_assertion_multiple_one_fails() {
        // Two assertions on same type: first passes, second fails
        let errors = validate_with_fragment_buffer(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="item">
                    <xs:complexType>
                        <xs:attribute name="val" type="xs:integer"/>
                        <xs:assert test="@val >= 0"/>
                        <xs:assert test="@val &lt; 100"/>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"#,
            "item",
            &[("val", "150")],
            None,
        );
        // Value 150 passes "@val >= 0" but fails "@val < 100"
        let assertion_errors: Vec<_> = errors
            .iter()
            .filter(|e| e.constraint == "cvc-assertion")
            .collect();
        assert_eq!(
            assertion_errors.len(),
            1,
            "Expected exactly 1 assertion failure, got: {:?}",
            assertion_errors
        );
    }

    #[test]
    fn test_no_assertion_type_no_buffering_overhead() {
        // A type without assertions should not trigger buffering at all,
        // even in FragmentBuffer mode.
        let errors = validate_with_fragment_buffer(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="plain" type="xs:string"/>
            </xs:schema>"#,
            "plain",
            &[],
            Some("hello"),
        );
        assert!(
            errors.is_empty(),
            "No assertion type should produce no errors, got: {:?}",
            errors
        );
    }

    #[test]
    fn test_assertion_attribute_check() {
        // Assertion checking string-length of attribute
        let errors = validate_with_fragment_buffer(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="item">
                    <xs:complexType>
                        <xs:attribute name="code" type="xs:string" use="required"/>
                        <xs:assert test="string-length(@code) > 0"/>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"#,
            "item",
            &[("code", "ABC")],
            None,
        );
        assert!(
            errors.is_empty(),
            "Assertion on non-empty @code should pass, got: {:?}",
            errors
        );
    }

    #[test]
    fn test_assertion_on_element_content() {
        // Assertion using element-only content with child elements
        let schema_set = load_schema_xsd11(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="order">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="qty" type="xs:integer"/>
                        </xs:sequence>
                        <xs:assert test="qty > 0"/>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"#,
        );
        let validator =
            SchemaValidator::new_fragment_buffer(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
        let ns = empty_ns_context();

        // <order><qty>5</qty></order>
        v.validate_element("order", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_element("qty", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_text("5");
        v.validate_end_element(); // </qty>
        v.validate_end_element(); // </order>
        v.end_validation().ok();

        let assertion_errors: Vec<_> = v
            .sink
            .errors
            .iter()
            .filter(|e| e.constraint == "cvc-assertion")
            .collect();
        assert!(
            assertion_errors.is_empty(),
            "qty=5 should pass qty > 0 assertion, got: {:?}",
            assertion_errors
        );
    }

    // ── Assertion on element content — failure ──────────────────────

    #[test]
    fn test_assertion_on_element_content_fail() {
        // qty=0 violates qty > 0
        let schema_set = load_schema_xsd11(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="order">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="qty" type="xs:integer"/>
                        </xs:sequence>
                        <xs:assert test="qty > 0"/>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"#,
        );
        let validator =
            SchemaValidator::new_fragment_buffer(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
        let ns = empty_ns_context();

        // <order><qty>0</qty></order>
        v.validate_element("order", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_element("qty", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_text("0");
        v.validate_end_element();
        v.validate_end_element();
        v.end_validation().ok();

        let assertion_errors: Vec<_> = v
            .sink
            .errors
            .iter()
            .filter(|e| e.constraint == "cvc-assertion")
            .collect();
        assert_eq!(
            assertion_errors.len(),
            1,
            "qty=0 should fail qty > 0, got: {:?}",
            v.sink.errors
        );
    }

    // ── Inherited assertions: base assertion evaluated on derived type ──

    #[test]
    fn test_inherited_assertion_pass() {
        // Base type has assertion @val >= 0; derived type restricts further.
        // Value 50 satisfies both base (@val >= 0) and derived (@val < 100).
        let schema_set = load_schema_xsd11(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:complexType name="baseType">
                    <xs:attribute name="val" type="xs:integer"/>
                    <xs:assert test="@val >= 0"/>
                </xs:complexType>
                <xs:complexType name="derivedType">
                    <xs:complexContent>
                        <xs:restriction base="baseType">
                            <xs:attribute name="val" type="xs:integer"/>
                            <xs:assert test="@val &lt; 100"/>
                        </xs:restriction>
                    </xs:complexContent>
                </xs:complexType>
                <xs:element name="item" type="derivedType"/>
            </xs:schema>"#,
        );
        let validator =
            SchemaValidator::new_fragment_buffer(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
        let ns = empty_ns_context();

        v.validate_element("item", "", None, None, &ns);
        v.validate_attribute("val", "", "50");
        v.validate_end_of_attributes();
        v.validate_end_element();
        v.end_validation().ok();

        let assertion_errors: Vec<_> = v
            .sink
            .errors
            .iter()
            .filter(|e| e.constraint == "cvc-assertion")
            .collect();
        assert!(
            assertion_errors.is_empty(),
            "val=50 should satisfy both base and derived assertions, got: {:?}",
            assertion_errors
        );
    }

    #[test]
    fn test_inherited_assertion_base_fails() {
        // Value -5 fails the base assertion @val >= 0
        let schema_set = load_schema_xsd11(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:complexType name="baseType">
                    <xs:attribute name="val" type="xs:integer"/>
                    <xs:assert test="@val >= 0"/>
                </xs:complexType>
                <xs:complexType name="derivedType">
                    <xs:complexContent>
                        <xs:restriction base="baseType">
                            <xs:attribute name="val" type="xs:integer"/>
                            <xs:assert test="@val &lt; 100"/>
                        </xs:restriction>
                    </xs:complexContent>
                </xs:complexType>
                <xs:element name="item" type="derivedType"/>
            </xs:schema>"#,
        );
        let validator =
            SchemaValidator::new_fragment_buffer(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
        let ns = empty_ns_context();

        v.validate_element("item", "", None, None, &ns);
        v.validate_attribute("val", "", "-5");
        v.validate_end_of_attributes();
        v.validate_end_element();
        v.end_validation().ok();

        let assertion_errors: Vec<_> = v
            .sink
            .errors
            .iter()
            .filter(|e| e.constraint == "cvc-assertion")
            .collect();
        assert!(
            !assertion_errors.is_empty(),
            "val=-5 should fail inherited @val >= 0 assertion"
        );
    }

    #[test]
    fn test_inherited_assertion_derived_fails() {
        // Value 200 passes base (@val >= 0) but fails derived (@val < 100)
        let schema_set = load_schema_xsd11(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:complexType name="baseType">
                    <xs:attribute name="val" type="xs:integer"/>
                    <xs:assert test="@val >= 0"/>
                </xs:complexType>
                <xs:complexType name="derivedType">
                    <xs:complexContent>
                        <xs:restriction base="baseType">
                            <xs:attribute name="val" type="xs:integer"/>
                            <xs:assert test="@val &lt; 100"/>
                        </xs:restriction>
                    </xs:complexContent>
                </xs:complexType>
                <xs:element name="item" type="derivedType"/>
            </xs:schema>"#,
        );
        let validator =
            SchemaValidator::new_fragment_buffer(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
        let ns = empty_ns_context();

        v.validate_element("item", "", None, None, &ns);
        v.validate_attribute("val", "", "200");
        v.validate_end_of_attributes();
        v.validate_end_element();
        v.end_validation().ok();

        let assertion_errors: Vec<_> = v
            .sink
            .errors
            .iter()
            .filter(|e| e.constraint == "cvc-assertion")
            .collect();
        assert_eq!(
            assertion_errors.len(),
            1,
            "val=200 should fail only derived @val < 100, got: {:?}",
            assertion_errors
        );
    }

    #[test]
    fn test_inherited_assertion_both_fail() {
        // Value -200 fails both base (@val >= 0) and derived (@val < 100)
        // (well, -200 < 100 passes, so use @val > 10 for derived instead)
        let schema_set = load_schema_xsd11(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:complexType name="baseType">
                    <xs:attribute name="val" type="xs:integer"/>
                    <xs:assert test="@val >= 0"/>
                </xs:complexType>
                <xs:complexType name="derivedType">
                    <xs:complexContent>
                        <xs:restriction base="baseType">
                            <xs:attribute name="val" type="xs:integer"/>
                            <xs:assert test="@val > 10"/>
                        </xs:restriction>
                    </xs:complexContent>
                </xs:complexType>
                <xs:element name="item" type="derivedType"/>
            </xs:schema>"#,
        );
        let validator =
            SchemaValidator::new_fragment_buffer(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
        let ns = empty_ns_context();

        // val=-5: fails base (>= 0) and fails derived (> 10)
        v.validate_element("item", "", None, None, &ns);
        v.validate_attribute("val", "", "-5");
        v.validate_end_of_attributes();
        v.validate_end_element();
        v.end_validation().ok();

        let assertion_errors: Vec<_> = v
            .sink
            .errors
            .iter()
            .filter(|e| e.constraint == "cvc-assertion")
            .collect();
        assert_eq!(
            assertion_errors.len(),
            2,
            "val=-5 should fail both inherited assertions, got: {:?}",
            assertion_errors
        );
    }

    // ── Nested element with its own assertions ──────────────────────

    #[test]
    fn test_nested_element_assertions() {
        // Parent and child both have assertions; both should be evaluated
        let schema_set = load_schema_xsd11(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="parent">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="child">
                                <xs:complexType>
                                    <xs:attribute name="x" type="xs:integer"/>
                                    <xs:assert test="@x > 0"/>
                                </xs:complexType>
                            </xs:element>
                        </xs:sequence>
                        <xs:attribute name="total" type="xs:integer"/>
                        <xs:assert test="@total >= 0"/>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"#,
        );
        let validator =
            SchemaValidator::new_fragment_buffer(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
        let ns = empty_ns_context();

        // <parent total="10"><child x="5"/></parent> — both pass
        v.validate_element("parent", "", None, None, &ns);
        v.validate_attribute("total", "", "10");
        v.validate_end_of_attributes();

        v.validate_element("child", "", None, None, &ns);
        v.validate_attribute("x", "", "5");
        v.validate_end_of_attributes();
        v.validate_end_element(); // </child>

        v.validate_end_element(); // </parent>
        v.end_validation().ok();

        let assertion_errors: Vec<_> = v
            .sink
            .errors
            .iter()
            .filter(|e| e.constraint == "cvc-assertion")
            .collect();
        assert!(
            assertion_errors.is_empty(),
            "Both assertions should pass, got: {:?}",
            assertion_errors
        );
    }

    #[test]
    fn test_nested_element_child_assertion_fails() {
        // Parent assertion passes, child assertion fails
        let schema_set = load_schema_xsd11(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="parent">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="child">
                                <xs:complexType>
                                    <xs:attribute name="x" type="xs:integer"/>
                                    <xs:assert test="@x > 0"/>
                                </xs:complexType>
                            </xs:element>
                        </xs:sequence>
                        <xs:attribute name="total" type="xs:integer"/>
                        <xs:assert test="@total >= 0"/>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"#,
        );
        let validator =
            SchemaValidator::new_fragment_buffer(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
        let ns = empty_ns_context();

        // <parent total="10"><child x="-1"/></parent>
        v.validate_element("parent", "", None, None, &ns);
        v.validate_attribute("total", "", "10");
        v.validate_end_of_attributes();

        v.validate_element("child", "", None, None, &ns);
        v.validate_attribute("x", "", "-1");
        v.validate_end_of_attributes();
        v.validate_end_element(); // </child>

        v.validate_end_element(); // </parent>
        v.end_validation().ok();

        let assertion_errors: Vec<_> = v
            .sink
            .errors
            .iter()
            .filter(|e| e.constraint == "cvc-assertion")
            .collect();
        assert_eq!(
            assertion_errors.len(),
            1,
            "Only child assertion should fail, got: {:?}",
            assertion_errors
        );
    }

    // ── Named complex type with assertions ──────────────────────────

    #[test]
    fn test_named_type_assertion_pass() {
        // Global element references named type with assertion
        let errors = validate_with_fragment_buffer(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:complexType name="positiveType">
                    <xs:attribute name="val" type="xs:integer"/>
                    <xs:assert test="@val > 0"/>
                </xs:complexType>
                <xs:element name="item" type="positiveType"/>
            </xs:schema>"#,
            "item",
            &[("val", "42")],
            None,
        );
        assert!(
            errors.is_empty(),
            "Named type assertion should pass for val=42, got: {:?}",
            errors
        );
    }

    #[test]
    fn test_named_type_assertion_fail() {
        let errors = validate_with_fragment_buffer(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:complexType name="positiveType">
                    <xs:attribute name="val" type="xs:integer"/>
                    <xs:assert test="@val > 0"/>
                </xs:complexType>
                <xs:element name="item" type="positiveType"/>
            </xs:schema>"#,
            "item",
            &[("val", "-1")],
            None,
        );
        let has_assertion_error = errors.iter().any(|e| e.constraint == "cvc-assertion");
        assert!(
            has_assertion_error,
            "Named type assertion should fail for val=-1, got: {:?}",
            errors
        );
    }

    // ── Assertion with child element content on named type ──────────

    #[test]
    fn test_named_type_child_element_assertion() {
        // Named type with sequence + assertion referencing child element
        let schema_set = load_schema_xsd11(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:complexType name="orderType">
                    <xs:sequence>
                        <xs:element name="qty" type="xs:integer"/>
                        <xs:element name="price" type="xs:decimal"/>
                    </xs:sequence>
                    <xs:assert test="qty > 0 and price > 0"/>
                </xs:complexType>
                <xs:element name="order" type="orderType"/>
            </xs:schema>"#,
        );
        let validator =
            SchemaValidator::new_fragment_buffer(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
        let ns = empty_ns_context();

        // <order><qty>3</qty><price>9.99</price></order>
        v.validate_element("order", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_element("qty", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_text("3");
        v.validate_end_element();
        v.validate_element("price", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_text("9.99");
        v.validate_end_element();
        v.validate_end_element();
        v.end_validation().ok();

        let assertion_errors: Vec<_> = v
            .sink
            .errors
            .iter()
            .filter(|e| e.constraint == "cvc-assertion")
            .collect();
        assert!(
            assertion_errors.is_empty(),
            "qty=3, price=9.99 should pass assertion, got: {:?}",
            assertion_errors
        );
    }

    #[test]
    fn test_named_type_child_element_assertion_fail() {
        let schema_set = load_schema_xsd11(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:complexType name="orderType">
                    <xs:sequence>
                        <xs:element name="qty" type="xs:integer"/>
                        <xs:element name="price" type="xs:decimal"/>
                    </xs:sequence>
                    <xs:assert test="qty > 0 and price > 0"/>
                </xs:complexType>
                <xs:element name="order" type="orderType"/>
            </xs:schema>"#,
        );
        let validator =
            SchemaValidator::new_fragment_buffer(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
        let ns = empty_ns_context();

        // <order><qty>0</qty><price>9.99</price></order> — qty=0 fails
        v.validate_element("order", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_element("qty", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_text("0");
        v.validate_end_element();
        v.validate_element("price", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_text("9.99");
        v.validate_end_element();
        v.validate_end_element();
        v.end_validation().ok();

        let assertion_errors: Vec<_> = v
            .sink
            .errors
            .iter()
            .filter(|e| e.constraint == "cvc-assertion")
            .collect();
        assert_eq!(
            assertion_errors.len(),
            1,
            "qty=0 should fail 'qty > 0 and price > 0', got: {:?}",
            assertion_errors
        );
    }

    // ── xpathDefaultNamespace on assertion ──────────────────────────

    #[test]
    fn test_assertion_xpath_default_namespace() {
        // Schema with target namespace; assertion uses
        // xpathDefaultNamespace="##targetNamespace" so unqualified
        // element steps match the target namespace.
        let schema_set = load_schema_xsd11(
            r###"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
                        targetNamespace="http://example.com/ns"
                        xmlns:tns="http://example.com/ns"
                        elementFormDefault="qualified">
                <xs:element name="order">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="qty" type="xs:integer"/>
                        </xs:sequence>
                        <xs:assert test="qty > 0"
                                   xpathDefaultNamespace="##targetNamespace"/>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"###,
        );
        let validator =
            SchemaValidator::new_fragment_buffer(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
        let ns = empty_ns_context();
        let tns = "http://example.com/ns";

        // <tns:order xmlns:tns="..."><tns:qty>5</tns:qty></tns:order>
        v.validate_element("order", tns, None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_element("qty", tns, None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_text("5");
        v.validate_end_element();
        v.validate_end_element();
        v.end_validation().ok();

        let assertion_errors: Vec<_> = v
            .sink
            .errors
            .iter()
            .filter(|e| e.constraint == "cvc-assertion")
            .collect();
        assert!(
            assertion_errors.is_empty(),
            "xpathDefaultNamespace=##targetNamespace should allow unqualified 'qty' to match, got: {:?}",
            assertion_errors
        );
    }

    // ── Extension-derived type inherits base assertions ─────────────

    #[test]
    fn test_extension_inherits_base_assertion() {
        let schema_set = load_schema_xsd11(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:complexType name="baseType">
                    <xs:sequence>
                        <xs:element name="name" type="xs:string"/>
                    </xs:sequence>
                    <xs:assert test="string-length(name) > 0"/>
                </xs:complexType>
                <xs:complexType name="extType">
                    <xs:complexContent>
                        <xs:extension base="baseType">
                            <xs:sequence>
                                <xs:element name="extra" type="xs:string"/>
                            </xs:sequence>
                        </xs:extension>
                    </xs:complexContent>
                </xs:complexType>
                <xs:element name="item" type="extType"/>
            </xs:schema>"#,
        );
        let validator =
            SchemaValidator::new_fragment_buffer(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
        let ns = empty_ns_context();

        // <item><name>hello</name><extra>world</extra></item>
        v.validate_element("item", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_element("name", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_text("hello");
        v.validate_end_element();
        v.validate_element("extra", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_text("world");
        v.validate_end_element();
        v.validate_end_element();
        v.end_validation().ok();

        let assertion_errors: Vec<_> = v
            .sink
            .errors
            .iter()
            .filter(|e| e.constraint == "cvc-assertion")
            .collect();
        assert!(
            assertion_errors.is_empty(),
            "Extension type should inherit and pass base assertion, got: {:?}",
            assertion_errors
        );
    }
}

// ── Fragment arena lifecycle tests ────────────────────────────────

#[cfg(feature = "xsd11")]
mod fragment_arena_tests {
    use super::*;

    #[test]
    fn fragment_arena_lifecycle() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root" type="xs:string"/>
            </xs:schema>"#,
        );
        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());

        // Initially None
        assert!(v.fragment_arena().is_none());

        // Lazy allocation via fragment_arena_mut()
        let _arena = v.fragment_arena_mut();

        // Now Some
        assert!(v.fragment_arena().is_some());
    }

    #[test]
    fn fragment_arena_allocate_and_drop() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root" type="xs:string"/>
            </xs:schema>"#,
        );
        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());

        // Allocate something into the arena
        let arena = v.fragment_arena_mut();
        let _s = arena.alloc_str("hello fragment");

        // Drop validator — arena drops cleanly (Miri-safe)
        drop(v);
    }
}

/// Test: global element with named complex type reference (type="itemType")
#[test]
fn test_global_element_with_named_complex_type_ref() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:complexType name="itemType">
                <xs:sequence>
                    <xs:element name="name" type="xs:string"/>
                    <xs:element name="value" type="xs:integer"/>
                </xs:sequence>
            </xs:complexType>
            <xs:element name="item" type="itemType"/>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    // Open root element "item" (global, type="itemType")
    let info = v.validate_element("item", "", None, None, &ns);
    assert_eq!(info.validity, SchemaValidity::Valid, "item should be valid");
    assert!(info.schema_type.is_some(), "item should have a schema type");

    v.validate_end_of_attributes();

    // Child "name"
    let name_info = v.validate_element("name", "", None, None, &ns);
    assert_eq!(
        name_info.validity,
        SchemaValidity::Valid,
        "name should be valid"
    );
    v.validate_end_of_attributes();
    v.validate_text("Widget");
    v.validate_end_element();

    // Child "value"
    let value_info = v.validate_element("value", "", None, None, &ns);
    assert_eq!(
        value_info.validity,
        SchemaValidity::Valid,
        "value should be valid"
    );
    v.validate_end_of_attributes();
    v.validate_text("42");
    v.validate_end_element();

    // Close root
    v.validate_end_element();
    assert!(v.end_validation().is_ok());
    assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
}

#[cfg(feature = "xsd11")]
mod type_alternatives_tests {
    use super::*;

    /// Helper: run a full validation pass and return the collected errors.
    fn validate_errors(
        schema_set: &SchemaSet,
        run: impl FnOnce(&mut ValidationRuntime<'_, TestSink>),
    ) -> Vec<ValidationError> {
        let validator = SchemaValidator::new(schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
        run(&mut v);
        v.end_validation().ok();
        v.sink.errors
    }

    const ALTERNATIVES_SCHEMA: &str = r#"
        <xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:complexType name="intContent">
                <xs:sequence>
                    <xs:element name="val" type="xs:integer"/>
                </xs:sequence>
                <xs:attribute name="kind" type="xs:string"/>
            </xs:complexType>
            <xs:complexType name="strContent">
                <xs:sequence>
                    <xs:element name="val" type="xs:string"/>
                </xs:sequence>
                <xs:attribute name="kind" type="xs:string"/>
            </xs:complexType>
            <xs:element name="data">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element name="val" type="xs:string"/>
                    </xs:sequence>
                    <xs:attribute name="kind" type="xs:string"/>
                </xs:complexType>
                <xs:alternative test="@kind='int'" type="intContent"/>
                <xs:alternative test="@kind='str'" type="strContent"/>
            </xs:element>
        </xs:schema>"#;

    #[test]
    fn test_alternative_selects_int_type() {
        let schema_set = load_schema_xsd11(ALTERNATIVES_SCHEMA);
        let ns = empty_ns_context();
        let errors = validate_errors(&schema_set, |v| {
            v.validate_element("data", "", None, None, &ns);
            v.validate_attribute("kind", "", "int");
            v.validate_end_of_attributes();

            v.validate_element("val", "", None, None, &ns);
            v.validate_end_of_attributes();
            v.validate_text("42");
            v.validate_end_element();

            v.validate_end_element();
        });
        assert!(errors.is_empty(), "Expected no errors, got: {:?}", errors);
    }

    #[test]
    fn test_alternative_selects_str_type() {
        let schema_set = load_schema_xsd11(ALTERNATIVES_SCHEMA);
        let ns = empty_ns_context();
        let errors = validate_errors(&schema_set, |v| {
            v.validate_element("data", "", None, None, &ns);
            v.validate_attribute("kind", "", "str");
            v.validate_end_of_attributes();

            v.validate_element("val", "", None, None, &ns);
            v.validate_end_of_attributes();
            v.validate_text("hello");
            v.validate_end_element();

            v.validate_end_element();
        });
        assert!(errors.is_empty(), "Expected no errors, got: {:?}", errors);
    }

    #[test]
    fn test_alternative_int_rejects_non_integer() {
        let schema_set = load_schema_xsd11(ALTERNATIVES_SCHEMA);
        let ns = empty_ns_context();
        let errors = validate_errors(&schema_set, |v| {
            v.validate_element("data", "", None, None, &ns);
            v.validate_attribute("kind", "", "int");
            v.validate_end_of_attributes();

            v.validate_element("val", "", None, None, &ns);
            v.validate_end_of_attributes();
            // "hello" is not a valid integer
            v.validate_text("hello");
            v.validate_end_element();

            v.validate_end_element();
        });
        assert!(
            !errors.is_empty(),
            "Expected validation error for non-integer value"
        );
    }

    #[test]
    fn test_no_matching_alternative_uses_declared_type() {
        let schema_set = load_schema_xsd11(ALTERNATIVES_SCHEMA);
        let ns = empty_ns_context();
        // kind='other' doesn't match any alternative — use element's declared type
        let errors = validate_errors(&schema_set, |v| {
            v.validate_element("data", "", None, None, &ns);
            v.validate_attribute("kind", "", "other");
            v.validate_end_of_attributes();

            // Declared type has <val> as xs:string
            v.validate_element("val", "", None, None, &ns);
            v.validate_end_of_attributes();
            v.validate_text("anything");
            v.validate_end_element();

            v.validate_end_element();
        });
        assert!(
            errors.is_empty(),
            "Expected no errors with declared type, got: {:?}",
            errors
        );
    }

    #[test]
    fn test_alternative_with_default_fallback() {
        let schema_set = load_schema_xsd11(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:complexType name="specialType">
                    <xs:sequence>
                        <xs:element name="s" type="xs:integer"/>
                    </xs:sequence>
                    <xs:attribute name="mode" type="xs:string"/>
                </xs:complexType>
                <xs:complexType name="defaultType">
                    <xs:sequence>
                        <xs:element name="d" type="xs:string"/>
                    </xs:sequence>
                    <xs:attribute name="mode" type="xs:string"/>
                </xs:complexType>
                <xs:element name="item">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="x" type="xs:string"/>
                        </xs:sequence>
                        <xs:attribute name="mode" type="xs:string"/>
                    </xs:complexType>
                    <xs:alternative test="@mode='special'" type="specialType"/>
                    <xs:alternative type="defaultType"/>
                </xs:element>
            </xs:schema>"#,
        );
        let ns = empty_ns_context();

        // mode='special' -> specialType (expects integer child)
        let errors_special = validate_errors(&schema_set, |v| {
            v.validate_element("item", "", None, None, &ns);
            v.validate_attribute("mode", "", "special");
            v.validate_end_of_attributes();
            v.validate_element("s", "", None, None, &ns);
            v.validate_end_of_attributes();
            v.validate_text("42");
            v.validate_end_element();
            v.validate_end_element();
        });
        assert!(
            errors_special.is_empty(),
            "Expected no errors for special mode, got: {:?}",
            errors_special
        );

        // mode='other' -> defaultType (expects string child "d")
        let errors_default = validate_errors(&schema_set, |v| {
            v.validate_element("item", "", None, None, &ns);
            v.validate_attribute("mode", "", "other");
            v.validate_end_of_attributes();
            v.validate_element("d", "", None, None, &ns);
            v.validate_end_of_attributes();
            v.validate_text("hello");
            v.validate_end_element();
            v.validate_end_element();
        });
        assert!(
            errors_default.is_empty(),
            "Expected no errors for default mode, got: {:?}",
            errors_default
        );
    }

    #[test]
    fn test_alternative_wrong_child_for_selected_type() {
        let schema_set = load_schema_xsd11(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:complexType name="typeA">
                    <xs:sequence>
                        <xs:element name="a" type="xs:string"/>
                    </xs:sequence>
                    <xs:attribute name="kind" type="xs:string"/>
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
        let ns = empty_ns_context();

        // kind='A' selects typeA which expects child "a", but we provide "x"
        let errors = validate_errors(&schema_set, |v| {
            v.validate_element("root", "", None, None, &ns);
            v.validate_attribute("kind", "", "A");
            v.validate_end_of_attributes();
            v.validate_element("x", "", None, None, &ns);
            v.validate_end_of_attributes();
            v.validate_text("hello");
            v.validate_end_element();
            v.validate_end_element();
        });
        assert!(
            !errors.is_empty(),
            "Expected content model error for wrong child element"
        );
    }

    #[test]
    fn test_alternative_no_attribute_no_match() {
        // When no attributes are present, XPath test @kind='A' should be false
        let schema_set = load_schema_xsd11(ALTERNATIVES_SCHEMA);
        let ns = empty_ns_context();
        let errors = validate_errors(&schema_set, |v| {
            v.validate_element("data", "", None, None, &ns);
            // No kind attribute
            v.validate_end_of_attributes();
            v.validate_element("val", "", None, None, &ns);
            v.validate_end_of_attributes();
            v.validate_text("anything");
            v.validate_end_element();
            v.validate_end_element();
        });
        // Falls through to declared type (xs:string child), should be valid
        assert!(errors.is_empty(), "Expected no errors, got: {:?}", errors);
    }

    #[test]
    fn test_alternative_schema_info_reflects_selected_type() {
        let schema_set = load_schema_xsd11(ALTERNATIVES_SCHEMA);
        let ns = empty_ns_context();
        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());

        v.validate_element("data", "", None, None, &ns);
        v.validate_attribute("kind", "", "int");
        let eoa_info = v.validate_end_of_attributes();
        // CTA switched the type — SchemaInfo should carry the new type
        assert!(
            eoa_info.schema_type.is_some(),
            "validate_end_of_attributes() should return updated type after CTA switch"
        );

        v.validate_element("val", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_text("123");
        v.validate_end_element();
        v.validate_end_element();
        v.end_validation().ok();
        assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
    }

    // Issue 1: Attribute validation deferred until after CTA selection
    #[test]
    fn test_deferred_attr_validation_rejects_prohibited_attr() {
        // The selected type does not declare "extra" — should be rejected
        let schema_set = load_schema_xsd11(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:complexType name="strict">
                    <xs:sequence>
                        <xs:element name="v" type="xs:string"/>
                    </xs:sequence>
                    <xs:attribute name="kind" type="xs:string"/>
                </xs:complexType>
                <xs:element name="root">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="v" type="xs:string"/>
                        </xs:sequence>
                        <xs:attribute name="kind" type="xs:string"/>
                        <xs:attribute name="extra" type="xs:string"/>
                    </xs:complexType>
                    <xs:alternative test="@kind='strict'" type="strict"/>
                </xs:element>
            </xs:schema>"#,
        );
        let ns = empty_ns_context();
        let errors = validate_errors(&schema_set, |v| {
            v.validate_element("root", "", None, None, &ns);
            // "extra" is declared on element's own type, but not on "strict"
            v.validate_attribute("kind", "", "strict");
            v.validate_attribute("extra", "", "foo");
            v.validate_end_of_attributes();
            v.validate_element("v", "", None, None, &ns);
            v.validate_end_of_attributes();
            v.validate_text("hello");
            v.validate_end_element();
            v.validate_end_element();
        });
        // "extra" should be rejected because CTA selected "strict" type
        assert!(
            errors.iter().any(|e| e.message.contains("extra")),
            "Expected error for undeclared 'extra' attribute in selected type, got: {:?}",
            errors
        );
    }

    #[test]
    fn test_deferred_attr_validation_checks_fixed_value() {
        // The selected type has a fixed value for an attribute
        let schema_set = load_schema_xsd11(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:complexType name="fixed">
                    <xs:sequence>
                        <xs:element name="v" type="xs:string"/>
                    </xs:sequence>
                    <xs:attribute name="kind" type="xs:string"/>
                    <xs:attribute name="code" type="xs:string" fixed="ABC"/>
                </xs:complexType>
                <xs:element name="root">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="v" type="xs:string"/>
                        </xs:sequence>
                        <xs:attribute name="kind" type="xs:string"/>
                        <xs:attribute name="code" type="xs:string"/>
                    </xs:complexType>
                    <xs:alternative test="@kind='fixed'" type="fixed"/>
                </xs:element>
            </xs:schema>"#,
        );
        let ns = empty_ns_context();

        // Wrong fixed value
        let errors = validate_errors(&schema_set, |v| {
            v.validate_element("root", "", None, None, &ns);
            v.validate_attribute("kind", "", "fixed");
            v.validate_attribute("code", "", "XYZ");
            v.validate_end_of_attributes();
            v.validate_element("v", "", None, None, &ns);
            v.validate_end_of_attributes();
            v.validate_text("hello");
            v.validate_end_element();
            v.validate_end_element();
        });
        assert!(
            errors.iter().any(|e| e.constraint == "cvc-attribute.4"),
            "Expected cvc-attribute.4 error for fixed value mismatch, got: {:?}",
            errors
        );

        // Correct fixed value
        let errors_ok = validate_errors(&schema_set, |v| {
            v.validate_element("root", "", None, None, &ns);
            v.validate_attribute("kind", "", "fixed");
            v.validate_attribute("code", "", "ABC");
            v.validate_end_of_attributes();
            v.validate_element("v", "", None, None, &ns);
            v.validate_end_of_attributes();
            v.validate_text("hello");
            v.validate_end_element();
            v.validate_end_element();
        });
        assert!(
            errors_ok.is_empty(),
            "Expected no errors, got: {:?}",
            errors_ok
        );
    }

    #[test]
    fn test_deferred_attr_validates_type_against_selected() {
        // The selected type declares attr as xs:integer — value "abc" should fail
        let schema_set = load_schema_xsd11(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:complexType name="numType">
                    <xs:sequence>
                        <xs:element name="v" type="xs:string"/>
                    </xs:sequence>
                    <xs:attribute name="kind" type="xs:string"/>
                    <xs:attribute name="val" type="xs:integer"/>
                </xs:complexType>
                <xs:element name="root">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="v" type="xs:string"/>
                        </xs:sequence>
                        <xs:attribute name="kind" type="xs:string"/>
                        <xs:attribute name="val" type="xs:string"/>
                    </xs:complexType>
                    <xs:alternative test="@kind='num'" type="numType"/>
                </xs:element>
            </xs:schema>"#,
        );
        let ns = empty_ns_context();

        // "abc" is valid xs:string (declared type) but not xs:integer (selected type)
        let errors = validate_errors(&schema_set, |v| {
            v.validate_element("root", "", None, None, &ns);
            v.validate_attribute("kind", "", "num");
            v.validate_attribute("val", "", "abc");
            v.validate_end_of_attributes();
            v.validate_element("v", "", None, None, &ns);
            v.validate_end_of_attributes();
            v.validate_text("hello");
            v.validate_end_element();
            v.validate_end_element();
        });
        assert!(
            !errors.is_empty(),
            "Expected type error for 'abc' against xs:integer in selected type"
        );

        // "42" should be valid
        let errors_ok = validate_errors(&schema_set, |v| {
            v.validate_element("root", "", None, None, &ns);
            v.validate_attribute("kind", "", "num");
            v.validate_attribute("val", "", "42");
            v.validate_end_of_attributes();
            v.validate_element("v", "", None, None, &ns);
            v.validate_end_of_attributes();
            v.validate_text("hello");
            v.validate_end_element();
            v.validate_end_element();
        });
        assert!(
            errors_ok.is_empty(),
            "Expected no errors, got: {:?}",
            errors_ok
        );
    }

    // Regression: when CTA evaluates but selects the same type (or no
    // match), deferred attributes must still be validated.
    #[test]
    fn test_cta_no_switch_still_validates_attributes() {
        // Schema where element "data" has alternatives but we'll supply
        // kind='other' which matches neither, so no CTA switch occurs.
        // The default fallback selects the declared type.
        // The attribute "unknown" is not declared and should be reported.
        let schema_set = load_schema_xsd11(ALTERNATIVES_SCHEMA);
        let ns = empty_ns_context();

        let errors = validate_errors(&schema_set, |v| {
            v.validate_element("data", "", None, None, &ns);
            v.validate_attribute("kind", "", "other"); // no alternative matches
            v.validate_attribute("unknown", "", "val"); // undeclared attribute
            v.validate_end_of_attributes();
            v.validate_end_element();
        });
        assert!(
            errors
                .iter()
                .any(|e| e.constraint == "cvc-complex-type.3.2.2"),
            "Undeclared attribute 'unknown' should be reported even when CTA \
             doesn't switch type, got: {:?}",
            errors
        );
    }

    // Issue 3: validate_end_of_attributes returns empty SchemaInfo when no CTA
    #[test]
    fn test_no_cta_returns_empty_schema_info() {
        let schema_set = load_schema_xsd11(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root" type="xs:string"/>
            </xs:schema>"#,
        );
        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
        let ns = empty_ns_context();
        v.validate_element("root", "", None, None, &ns);
        let eoa_info = v.validate_end_of_attributes();
        // No CTA — schema_type should be None (empty SchemaInfo)
        assert!(
            eoa_info.schema_type.is_none(),
            "No CTA switch should return empty SchemaInfo, got type: {:?}",
            eoa_info.schema_type
        );
    }

    /// Helper: run a full validation pass with PROCESS_ASSERTIONS enabled
    /// (fragment buffer mode) and return the collected errors.
    fn validate_errors_with_assertions(
        schema_set: &SchemaSet,
        run: impl FnOnce(&mut ValidationRuntime<'_, TestSink>),
    ) -> Vec<ValidationError> {
        let validator =
            SchemaValidator::new_fragment_buffer(schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
        run(&mut v);
        v.end_validation().ok();
        v.sink.errors
    }

    // ── CTA + assertion interaction tests ───────────────────────────

    #[test]
    fn test_cta_non_asserted_to_asserted() {
        // Default type has NO assertions; CTA-selected type has xs:assert.
        // Assertion should fire and see the attributes.
        let schema_set = load_schema_xsd11(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:complexType name="assertedType">
                    <xs:sequence>
                        <xs:element name="v" type="xs:string"/>
                    </xs:sequence>
                    <xs:attribute name="kind" type="xs:string"/>
                    <xs:attribute name="val" type="xs:integer"/>
                    <xs:assert test="@val > 0"/>
                </xs:complexType>
                <xs:element name="item">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="v" type="xs:string"/>
                        </xs:sequence>
                        <xs:attribute name="kind" type="xs:string"/>
                        <xs:attribute name="val" type="xs:integer"/>
                    </xs:complexType>
                    <xs:alternative test="@kind='checked'" type="assertedType"/>
                </xs:element>
            </xs:schema>"#,
        );
        let ns = empty_ns_context();

        // val=-1 violates @val > 0 on the CTA-selected type
        let errors = validate_errors_with_assertions(&schema_set, |v| {
            v.validate_element("item", "", None, None, &ns);
            v.validate_attribute("kind", "", "checked");
            v.validate_attribute("val", "", "-1");
            v.validate_end_of_attributes();
            v.validate_element("v", "", None, None, &ns);
            v.validate_end_of_attributes();
            v.validate_text("hello");
            v.validate_end_element();
            v.validate_end_element();
        });
        assert!(
            errors.iter().any(|e| e.constraint == "cvc-assertion"),
            "Expected assertion error for @val > 0 with val=-1, got: {:?}",
            errors
        );

        // val=5 satisfies @val > 0
        let errors_ok = validate_errors_with_assertions(&schema_set, |v| {
            v.validate_element("item", "", None, None, &ns);
            v.validate_attribute("kind", "", "checked");
            v.validate_attribute("val", "", "5");
            v.validate_end_of_attributes();
            v.validate_element("v", "", None, None, &ns);
            v.validate_end_of_attributes();
            v.validate_text("hello");
            v.validate_end_element();
            v.validate_end_element();
        });
        assert!(
            errors_ok.iter().all(|e| e.constraint != "cvc-assertion"),
            "Expected no assertion errors for @val > 0 with val=5, got: {:?}",
            errors_ok
        );
    }

    #[test]
    fn test_cta_asserted_to_non_asserted() {
        // Default type has xs:assert; CTA-selected type has none.
        // The old assertion should NOT be evaluated.
        let schema_set = load_schema_xsd11(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:complexType name="plainType">
                    <xs:sequence>
                        <xs:element name="v" type="xs:string"/>
                    </xs:sequence>
                    <xs:attribute name="kind" type="xs:string"/>
                    <xs:attribute name="val" type="xs:integer"/>
                </xs:complexType>
                <xs:element name="item">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="v" type="xs:string"/>
                        </xs:sequence>
                        <xs:attribute name="kind" type="xs:string"/>
                        <xs:attribute name="val" type="xs:integer"/>
                        <xs:assert test="@val > 100"/>
                    </xs:complexType>
                    <xs:alternative test="@kind='plain'" type="plainType"/>
                </xs:element>
            </xs:schema>"#,
        );
        let ns = empty_ns_context();

        // val=1 would fail @val > 100 on the default type, but CTA selects
        // plainType which has no assertions — no assertion error expected.
        let errors = validate_errors_with_assertions(&schema_set, |v| {
            v.validate_element("item", "", None, None, &ns);
            v.validate_attribute("kind", "", "plain");
            v.validate_attribute("val", "", "1");
            v.validate_end_of_attributes();
            v.validate_element("v", "", None, None, &ns);
            v.validate_end_of_attributes();
            v.validate_text("hello");
            v.validate_end_element();
            v.validate_end_element();
        });
        assert!(
            errors.iter().all(|e| e.constraint != "cvc-assertion"),
            "Expected NO assertion errors (CTA selected non-asserted type), got: {:?}",
            errors
        );
    }

    #[test]
    fn test_cta_asserted_to_asserted() {
        // Both default type and CTA-selected type have assertions.
        // Only the selected type's assertion should run.
        let schema_set = load_schema_xsd11(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:complexType name="strictType">
                    <xs:sequence>
                        <xs:element name="v" type="xs:string"/>
                    </xs:sequence>
                    <xs:attribute name="kind" type="xs:string"/>
                    <xs:attribute name="val" type="xs:integer"/>
                    <xs:assert test="@val > 10"/>
                </xs:complexType>
                <xs:element name="item">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="v" type="xs:string"/>
                        </xs:sequence>
                        <xs:attribute name="kind" type="xs:string"/>
                        <xs:attribute name="val" type="xs:integer"/>
                        <xs:assert test="@val > 0"/>
                    </xs:complexType>
                    <xs:alternative test="@kind='strict'" type="strictType"/>
                </xs:element>
            </xs:schema>"#,
        );
        let ns = empty_ns_context();

        // val=5 passes default @val > 0 but fails strict @val > 10
        let errors = validate_errors_with_assertions(&schema_set, |v| {
            v.validate_element("item", "", None, None, &ns);
            v.validate_attribute("kind", "", "strict");
            v.validate_attribute("val", "", "5");
            v.validate_end_of_attributes();
            v.validate_element("v", "", None, None, &ns);
            v.validate_end_of_attributes();
            v.validate_text("hello");
            v.validate_end_element();
            v.validate_end_element();
        });
        assert!(
            errors.iter().any(|e| e.constraint == "cvc-assertion"),
            "Expected assertion error from strict @val > 10 with val=5, got: {:?}",
            errors
        );

        // val=20 passes strict @val > 10
        let errors_ok = validate_errors_with_assertions(&schema_set, |v| {
            v.validate_element("item", "", None, None, &ns);
            v.validate_attribute("kind", "", "strict");
            v.validate_attribute("val", "", "20");
            v.validate_end_of_attributes();
            v.validate_element("v", "", None, None, &ns);
            v.validate_end_of_attributes();
            v.validate_text("hello");
            v.validate_end_element();
            v.validate_end_element();
        });
        assert!(
            errors_ok.iter().all(|e| e.constraint != "cvc-assertion"),
            "Expected no assertion errors for @val > 10 with val=20, got: {:?}",
            errors_ok
        );
    }
}

// -----------------------------------------------------------------------
// Schema-level defaultAttributes tests (XSD 1.1)
// -----------------------------------------------------------------------

#[test]
#[cfg(feature = "xsd11")]
fn test_default_attributes_applied() {
    let schema_set = load_schema_xsd11(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
                     defaultAttributes="commonAttrs">
            <xs:attributeGroup name="commonAttrs">
                <xs:attribute name="lang" type="xs:string"/>
            </xs:attributeGroup>
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element name="a" type="xs:string"/>
                    </xs:sequence>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_attribute("lang", "", "en");
    v.validate_end_of_attributes();
    v.validate_element("a", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_text("hello");
    v.validate_end_element();
    v.validate_end_element();
    v.end_validation().ok();

    assert!(
        v.sink.errors.is_empty(),
        "Default attribute group attribute 'lang' should be accepted, got: {:?}",
        v.sink.errors
    );
}

#[test]
#[cfg(feature = "xsd11")]
fn test_default_attributes_opt_out() {
    let schema_set = load_schema_xsd11(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
                     defaultAttributes="commonAttrs">
            <xs:attributeGroup name="commonAttrs">
                <xs:attribute name="lang" type="xs:string"/>
            </xs:attributeGroup>
            <xs:element name="root">
                <xs:complexType defaultAttributesApply="false">
                    <xs:sequence>
                        <xs:element name="a" type="xs:string"/>
                    </xs:sequence>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_attribute("lang", "", "en");
    v.validate_end_of_attributes();
    v.validate_element("a", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_text("hello");
    v.validate_end_element();
    v.validate_end_element();
    v.end_validation().ok();

    // 'lang' should be rejected because the type opted out
    assert!(
        v.sink
            .errors
            .iter()
            .any(|e| e.constraint.starts_with("cvc-complex-type.3")),
        "Attribute 'lang' should be rejected when defaultAttributesApply=false, got: {:?}",
        v.sink.errors
    );
}

#[test]
#[cfg(feature = "xsd11")]
fn test_default_attributes_contributes_defaults() {
    let schema_set = load_schema_xsd11(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
                     defaultAttributes="commonAttrs">
            <xs:attribute name="lang" type="xs:string" default="en"/>
            <xs:attributeGroup name="commonAttrs">
                <xs:attribute ref="lang"/>
            </xs:attributeGroup>
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element name="a" type="xs:string"/>
                    </xs:sequence>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    // get_default_attributes should include 'lang' with value "en"
    let defaults = v.get_default_attributes();
    assert!(
        defaults.iter().any(|d| {
            let name = schema_set.name_table.resolve(d.local_name);
            name == "lang" && d.value == "en"
        }),
        "Default attributes should include 'lang' with value 'en', got: {:?}",
        defaults
            .iter()
            .map(|d| (schema_set.name_table.resolve(d.local_name), &d.value))
            .collect::<Vec<_>>()
    );

    v.validate_element("a", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_text("hello");
    v.validate_end_element();
    v.validate_end_element();
    v.end_validation().ok();
}

#[test]
#[cfg(feature = "xsd11")]
fn test_default_attributes_required() {
    let schema_set = load_schema_xsd11(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
                     defaultAttributes="commonAttrs">
            <xs:attributeGroup name="commonAttrs">
                <xs:attribute name="lang" type="xs:string" use="required"/>
            </xs:attributeGroup>
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element name="a" type="xs:string"/>
                    </xs:sequence>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    // Don't provide 'lang' attribute
    v.validate_end_of_attributes();
    v.validate_element("a", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_text("hello");
    v.validate_end_element();
    v.validate_end_element();
    v.end_validation().ok();

    assert!(
        v.sink
            .errors
            .iter()
            .any(|e| e.constraint == "cvc-complex-type.4"),
        "Required attribute from default group should cause cvc-complex-type.4 error, got: {:?}",
        v.sink.errors
    );
}

#[test]
#[cfg(feature = "xsd11")]
fn test_default_attributes_any_attribute() {
    let schema_set = load_schema_xsd11(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
                     defaultAttributes="commonAttrs">
            <xs:attributeGroup name="commonAttrs">
                <xs:anyAttribute processContents="lax"/>
            </xs:attributeGroup>
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element name="a" type="xs:string"/>
                    </xs:sequence>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_attribute("unknown", "", "value");
    v.validate_end_of_attributes();
    v.validate_element("a", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_text("hello");
    v.validate_end_element();
    v.validate_end_element();
    v.end_validation().ok();

    assert!(
        v.sink.errors.is_empty(),
        "anyAttribute in default group should allow unknown attributes, got: {:?}",
        v.sink.errors
    );
}

// -----------------------------------------------------------------------
// Attribute form / attributeFormDefault tests
// -----------------------------------------------------------------------

/// Build a namespace context for `http://example.com/ns` with `tns` prefix.
fn tns_ns_context(schema_set: &SchemaSet) -> NamespaceContextSnapshot {
    let tns_id = schema_set.name_table.add("http://example.com/ns");
    let tns_prefix = schema_set.name_table.add("tns");
    NamespaceContextSnapshot {
        default_ns: Some(tns_id),
        bindings: vec![(tns_prefix, tns_id)],
    }
}

/// Validate a single attribute on `<root>` and assert accept/reject.
///
/// `accept_ns` is the attribute namespace that should be accepted.
/// `reject_ns` is the attribute namespace that should be rejected.
fn assert_attribute_form(
    schema_set: &SchemaSet,
    accept_ns: &str,
    reject_ns: &str,
    accept_msg: &str,
    reject_msg: &str,
) {
    let validator = SchemaValidator::new(schema_set, ValidationFlags::default());
    let ns = tns_ns_context(schema_set);

    // --- Accept case
    let mut v = validator.start_run(TestSink::new());
    v.validate_element("root", "http://example.com/ns", None, None, &ns);
    let info = v.validate_attribute("id", accept_ns, "val");
    assert_ne!(
        info.validity,
        SchemaValidity::Invalid,
        "{accept_msg}, errors: {:?}",
        v.sink.errors
    );
    v.validate_end_of_attributes();
    v.validate_end_element();
    v.end_validation().ok();
    assert!(
        v.sink.errors.is_empty(),
        "expected no errors, got: {:?}",
        v.sink.errors
    );

    // --- Reject case
    let mut v2 = validator.start_run(TestSink::new());
    v2.validate_element("root", "http://example.com/ns", None, None, &ns);
    let info = v2.validate_attribute("id", reject_ns, "val");
    assert_eq!(info.validity, SchemaValidity::Invalid, "{reject_msg}");
    v2.validate_end_of_attributes();
    v2.validate_end_element();
    v2.end_validation().ok();
    assert!(
        v2.sink
            .errors
            .iter()
            .any(|e| e.constraint == "cvc-complex-type.3.2.2"),
        "expected cvc-complex-type.3.2.2, got: {:?}",
        v2.sink.errors
    );
}

const TNS: &str = "http://example.com/ns";

#[test]
fn test_attribute_form_default_qualified() {
    let schema_set = load_schema(
        r###"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
                     targetNamespace="http://example.com/ns"
                     attributeFormDefault="qualified"
                     xmlns:tns="http://example.com/ns">
            <xs:element name="root">
                <xs:complexType>
                    <xs:attribute name="id" type="xs:string"/>
                </xs:complexType>
            </xs:element>
        </xs:schema>"###,
    );
    assert_attribute_form(
        &schema_set,
        TNS,
        "",
        "qualified attribute should be valid",
        "unqualified attribute should be rejected when attributeFormDefault=qualified",
    );
}

#[test]
fn test_attribute_form_qualified_explicit() {
    let schema_set = load_schema(
        r###"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
                     targetNamespace="http://example.com/ns"
                     xmlns:tns="http://example.com/ns">
            <xs:element name="root">
                <xs:complexType>
                    <xs:attribute name="id" type="xs:string" form="qualified"/>
                </xs:complexType>
            </xs:element>
        </xs:schema>"###,
    );
    assert_attribute_form(
        &schema_set,
        TNS,
        "",
        "form=qualified attribute should be valid",
        "unqualified attribute should be rejected when form=qualified",
    );
}

#[test]
fn test_attribute_form_unqualified_explicit() {
    let schema_set = load_schema(
        r###"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
                     targetNamespace="http://example.com/ns"
                     attributeFormDefault="qualified"
                     xmlns:tns="http://example.com/ns">
            <xs:element name="root">
                <xs:complexType>
                    <xs:attribute name="id" type="xs:string" form="unqualified"/>
                </xs:complexType>
            </xs:element>
        </xs:schema>"###,
    );
    assert_attribute_form(
        &schema_set,
        "",
        TNS,
        "form=unqualified attribute should be valid",
        "qualified attribute should be rejected when form=unqualified",
    );
}

#[test]
fn test_attribute_form_default_unqualified() {
    let schema_set = load_schema(
        r###"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
                     targetNamespace="http://example.com/ns"
                     xmlns:tns="http://example.com/ns">
            <xs:element name="root">
                <xs:complexType>
                    <xs:attribute name="id" type="xs:string"/>
                </xs:complexType>
            </xs:element>
        </xs:schema>"###,
    );
    assert_attribute_form(
        &schema_set,
        "",
        TNS,
        "default unqualified attribute should be valid",
        "qualified attribute should be rejected when default is unqualified",
    );
}

#[test]
fn test_attribute_group_form_qualified() {
    let schema_set = load_schema(
        r###"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
                     targetNamespace="http://example.com/ns"
                     attributeFormDefault="qualified"
                     xmlns:tns="http://example.com/ns">
            <xs:attributeGroup name="myAttrs">
                <xs:attribute name="id" type="xs:string"/>
            </xs:attributeGroup>
            <xs:element name="root">
                <xs:complexType>
                    <xs:attributeGroup ref="tns:myAttrs"/>
                </xs:complexType>
            </xs:element>
        </xs:schema>"###,
    );
    assert_attribute_form(
        &schema_set,
        TNS,
        "",
        "qualified attribute from group should be valid",
        "unqualified attribute should be rejected for qualified group attribute",
    );
}

#[cfg(feature = "xsd11")]
#[test]
fn test_attribute_explicit_target_namespace() {
    let schema_set = load_schema_xsd11(
        r###"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
                     targetNamespace="http://example.com/ns"
                     xmlns:tns="http://example.com/ns"
                     xmlns:other="http://other.com/ns">
            <xs:element name="root">
                <xs:complexType>
                    <xs:attribute name="id" type="xs:string"
                                  targetNamespace="http://other.com/ns"/>
                </xs:complexType>
            </xs:element>
        </xs:schema>"###,
    );
    assert_attribute_form(
        &schema_set,
        "http://other.com/ns",
        TNS,
        "explicit targetNamespace attribute should be valid",
        "attribute with wrong namespace should be rejected",
    );
}

// -----------------------------------------------------------------------
// ID / IDREF / IDREFS correctness proof tests
// -----------------------------------------------------------------------

/// Helper schema for ID/IDREF attribute tests.
fn id_idref_attr_schema() -> crate::schema::SchemaSet {
    load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element name="item" maxOccurs="unbounded">
                            <xs:complexType>
                                <xs:attribute name="id" type="xs:ID" use="required"/>
                            </xs:complexType>
                        </xs:element>
                        <xs:element name="link" minOccurs="0" maxOccurs="unbounded">
                            <xs:complexType>
                                <xs:attribute name="ref" type="xs:IDREF" use="required"/>
                            </xs:complexType>
                        </xs:element>
                        <xs:element name="multi" minOccurs="0" maxOccurs="unbounded">
                            <xs:complexType>
                                <xs:attribute name="refs" type="xs:IDREFS" use="required"/>
                            </xs:complexType>
                        </xs:element>
                    </xs:sequence>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    )
}

/// IDREF valid forward reference — reference appears before the ID definition.
#[test]
fn test_idref_forward_reference() {
    // Use xs:choice so link can appear before item
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:choice maxOccurs="unbounded">
                        <xs:element name="item">
                            <xs:complexType>
                                <xs:attribute name="id" type="xs:ID" use="required"/>
                            </xs:complexType>
                        </xs:element>
                        <xs:element name="link">
                            <xs:complexType>
                                <xs:attribute name="ref" type="xs:IDREF" use="required"/>
                            </xs:complexType>
                        </xs:element>
                    </xs:choice>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );
    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    // Forward reference: link before item
    v.validate_element("link", "", None, None, &ns);
    v.validate_attribute("ref", "", "future");
    v.validate_end_of_attributes();
    v.validate_end_element();

    // Now define the ID
    v.validate_element("item", "", None, None, &ns);
    v.validate_attribute("id", "", "future");
    v.validate_end_of_attributes();
    v.validate_end_element();

    v.validate_end_element();
    v.end_validation().ok();

    assert!(
        v.sink.errors.is_empty(),
        "Forward IDREF reference should be valid, got: {:?}",
        v.sink.errors
    );
}

/// IDREFS with all tokens valid — no errors expected.
#[test]
fn test_idrefs_all_valid() {
    let schema_set = id_idref_attr_schema();
    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    v.validate_element("item", "", None, None, &ns);
    v.validate_attribute("id", "", "a1");
    v.validate_end_of_attributes();
    v.validate_end_element();

    v.validate_element("item", "", None, None, &ns);
    v.validate_attribute("id", "", "a2");
    v.validate_end_of_attributes();
    v.validate_end_element();

    v.validate_element("multi", "", None, None, &ns);
    v.validate_attribute("refs", "", "a1 a2");
    v.validate_end_of_attributes();
    v.validate_end_element();

    v.validate_end_element();
    v.end_validation().ok();

    assert!(
        v.sink.errors.is_empty(),
        "IDREFS with all valid tokens should succeed, got: {:?}",
        v.sink.errors
    );
}

/// IDREFS with one missing token and one valid token.
#[test]
fn test_idrefs_one_missing_one_valid() {
    let schema_set = id_idref_attr_schema();
    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    v.validate_element("item", "", None, None, &ns);
    v.validate_attribute("id", "", "exists");
    v.validate_end_of_attributes();
    v.validate_end_element();

    v.validate_element("multi", "", None, None, &ns);
    v.validate_attribute("refs", "", "exists ghost");
    v.validate_end_of_attributes();
    v.validate_end_element();

    v.validate_end_element();
    v.end_validation().ok();

    let idref_errors: Vec<_> = v
        .sink
        .errors
        .iter()
        .filter(|e| e.constraint == "cvc-id.1")
        .collect();
    assert_eq!(
        idref_errors.len(),
        1,
        "Expected 1 IDREF error for 'ghost', got: {:?}",
        idref_errors
    );
}

/// IDREFS with multiple missing tokens.
#[test]
fn test_idrefs_multiple_missing() {
    let schema_set = id_idref_attr_schema();
    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    v.validate_element("item", "", None, None, &ns);
    v.validate_attribute("id", "", "only");
    v.validate_end_of_attributes();
    v.validate_end_element();

    v.validate_element("multi", "", None, None, &ns);
    v.validate_attribute("refs", "", "nope1 nope2 nope3");
    v.validate_end_of_attributes();
    v.validate_end_element();

    v.validate_end_element();
    v.end_validation().ok();

    let idref_errors: Vec<_> = v
        .sink
        .errors
        .iter()
        .filter(|e| e.constraint == "cvc-id.1")
        .collect();
    assert_eq!(
        idref_errors.len(),
        3,
        "Expected 3 IDREF errors for nope1/nope2/nope3, got: {:?}",
        idref_errors
    );
}

/// IDREFS empty after whitespace collapse is a lexical error.
#[test]
fn test_idrefs_empty_after_collapse() {
    let schema_set = id_idref_attr_schema();
    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    v.validate_element("multi", "", None, None, &ns);
    v.validate_attribute("refs", "", "   ");
    v.validate_end_of_attributes();
    v.validate_end_element();

    v.validate_end_element();
    v.end_validation().ok();

    // Should have a validation error (lexical), no cvc-id.1 errors
    assert!(
        !v.sink.errors.is_empty(),
        "IDREFS with only whitespace should produce an error"
    );
    let idref_errors: Vec<_> = v
        .sink
        .errors
        .iter()
        .filter(|e| e.constraint == "cvc-id.1")
        .collect();
    assert!(
        idref_errors.is_empty(),
        "Empty IDREFS should not produce cvc-id.1 errors (lexical rejection), got: {:?}",
        idref_errors
    );
}

/// ID lexical rejection for invalid NCName.
#[test]
fn test_id_invalid_ncname() {
    let schema_set = id_idref_attr_schema();
    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    // "1bad" starts with digit — not a valid NCName
    v.validate_element("item", "", None, None, &ns);
    v.validate_attribute("id", "", "1bad");
    v.validate_end_of_attributes();
    v.validate_end_element();

    v.validate_end_element();
    v.end_validation().ok();

    assert!(
        !v.sink.errors.is_empty(),
        "Invalid NCName for ID should produce an error"
    );
    // Should NOT appear in ID table (no duplicate detection)
    let id_dup_errors: Vec<_> = v
        .sink
        .errors
        .iter()
        .filter(|e| e.constraint == "cvc-id.2")
        .collect();
    assert!(
        id_dup_errors.is_empty(),
        "Invalid NCName should not produce cvc-id.2, got: {:?}",
        id_dup_errors
    );
}

/// IDREF lexical rejection for invalid NCName.
#[test]
fn test_idref_invalid_ncname() {
    let schema_set = id_idref_attr_schema();
    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    v.validate_element("link", "", None, None, &ns);
    v.validate_attribute("ref", "", "bad:name");
    v.validate_end_of_attributes();
    v.validate_end_element();

    v.validate_end_element();
    v.end_validation().ok();

    assert!(
        !v.sink.errors.is_empty(),
        "Invalid NCName for IDREF should produce an error"
    );
    // The invalid value should NOT end up in pending_idrefs
    let idref_errors: Vec<_> = v
        .sink
        .errors
        .iter()
        .filter(|e| e.constraint == "cvc-id.1")
        .collect();
    assert!(
        idref_errors.is_empty(),
        "Invalid IDREF should not produce cvc-id.1 (no runtime tracking), got: {:?}",
        idref_errors
    );
}

/// IDREFS lexical rejection when one token is invalid NCName.
#[test]
fn test_idrefs_one_invalid_ncname_token() {
    let schema_set = id_idref_attr_schema();
    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    // Second token "2bad" is invalid NCName
    v.validate_element("multi", "", None, None, &ns);
    v.validate_attribute("refs", "", "good 2bad");
    v.validate_end_of_attributes();
    v.validate_end_element();

    v.validate_end_element();
    v.end_validation().ok();

    assert!(
        !v.sink.errors.is_empty(),
        "IDREFS with one invalid token should produce an error"
    );
    // No tokens should be tracked (lexical validation rejects entire value)
    let idref_errors: Vec<_> = v
        .sink
        .errors
        .iter()
        .filter(|e| e.constraint == "cvc-id.1")
        .collect();
    assert!(
        idref_errors.is_empty(),
        "Invalid IDREFS should not produce cvc-id.1 errors, got: {:?}",
        idref_errors
    );
}

/// Element text typed as xs:ID participates in duplicate detection.
#[test]
fn test_element_text_id_duplicate() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element name="id" type="xs:ID" maxOccurs="unbounded"/>
                    </xs:sequence>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );
    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    v.validate_element("id", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_text("alpha");
    v.validate_end_element();

    v.validate_element("id", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_text("alpha"); // duplicate
    v.validate_end_element();

    v.validate_end_element();
    v.end_validation().ok();

    assert!(
        v.sink.errors.iter().any(|e| e.constraint == "cvc-id.2"),
        "Duplicate ID in element text should raise cvc-id.2, got: {:?}",
        v.sink.errors
    );
}

/// Element text typed as xs:IDREF participates in end-of-document resolution.
#[test]
fn test_element_text_idref_resolution() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element name="id" type="xs:ID" maxOccurs="unbounded"/>
                        <xs:element name="ref" type="xs:IDREF" maxOccurs="unbounded"/>
                    </xs:sequence>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );
    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    v.validate_element("id", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_text("x1");
    v.validate_end_element();

    // Valid reference
    v.validate_element("ref", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_text("x1");
    v.validate_end_element();

    // Missing reference
    v.validate_element("ref", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_text("missing");
    v.validate_end_element();

    v.validate_end_element();
    v.end_validation().ok();

    let idref_errors: Vec<_> = v
        .sink
        .errors
        .iter()
        .filter(|e| e.constraint == "cvc-id.1")
        .collect();
    assert_eq!(
        idref_errors.len(),
        1,
        "Expected 1 cvc-id.1 error for element-text IDREF 'missing', got: {:?}",
        idref_errors
    );
}

/// Derived type from xs:ID still contributes to duplicate detection.
#[test]
fn test_derived_id_duplicate_detection() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:simpleType name="myID">
                <xs:restriction base="xs:ID">
                    <xs:maxLength value="20"/>
                </xs:restriction>
            </xs:simpleType>
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element name="item" maxOccurs="unbounded">
                            <xs:complexType>
                                <xs:attribute name="id" type="myID" use="required"/>
                            </xs:complexType>
                        </xs:element>
                    </xs:sequence>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );
    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    v.validate_element("item", "", None, None, &ns);
    v.validate_attribute("id", "", "dup");
    v.validate_end_of_attributes();
    v.validate_end_element();

    v.validate_element("item", "", None, None, &ns);
    v.validate_attribute("id", "", "dup"); // duplicate
    v.validate_end_of_attributes();
    v.validate_end_element();

    v.validate_end_element();
    v.end_validation().ok();

    assert!(
        v.sink.errors.iter().any(|e| e.constraint == "cvc-id.2"),
        "Derived xs:ID should still detect duplicates, got: {:?}",
        v.sink.errors
    );
}

/// Derived type from xs:IDREF still contributes to reference tracking.
#[test]
fn test_derived_idref_tracking() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:simpleType name="myIDREF">
                <xs:restriction base="xs:IDREF">
                    <xs:maxLength value="20"/>
                </xs:restriction>
            </xs:simpleType>
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element name="item" maxOccurs="unbounded">
                            <xs:complexType>
                                <xs:attribute name="id" type="xs:ID" use="required"/>
                            </xs:complexType>
                        </xs:element>
                        <xs:element name="link" maxOccurs="unbounded">
                            <xs:complexType>
                                <xs:attribute name="ref" type="myIDREF" use="required"/>
                            </xs:complexType>
                        </xs:element>
                    </xs:sequence>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );
    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    v.validate_element("item", "", None, None, &ns);
    v.validate_attribute("id", "", "ok");
    v.validate_end_of_attributes();
    v.validate_end_element();

    // Valid derived IDREF
    v.validate_element("link", "", None, None, &ns);
    v.validate_attribute("ref", "", "ok");
    v.validate_end_of_attributes();
    v.validate_end_element();

    // Missing derived IDREF
    v.validate_element("link", "", None, None, &ns);
    v.validate_attribute("ref", "", "nope");
    v.validate_end_of_attributes();
    v.validate_end_element();

    v.validate_end_element();
    v.end_validation().ok();

    let idref_errors: Vec<_> = v
        .sink
        .errors
        .iter()
        .filter(|e| e.constraint == "cvc-id.1")
        .collect();
    assert_eq!(
        idref_errors.len(),
        1,
        "Derived xs:IDREF should track references, got: {:?}",
        idref_errors
    );
}

/// Derived type from xs:IDREFS still tracks each token.
#[test]
fn test_derived_idrefs_tracking() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:simpleType name="myIDREFS">
                <xs:restriction base="xs:IDREFS">
                    <xs:maxLength value="5"/>
                </xs:restriction>
            </xs:simpleType>
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element name="item" maxOccurs="unbounded">
                            <xs:complexType>
                                <xs:attribute name="id" type="xs:ID" use="required"/>
                            </xs:complexType>
                        </xs:element>
                        <xs:element name="multi" maxOccurs="unbounded">
                            <xs:complexType>
                                <xs:attribute name="refs" type="myIDREFS" use="required"/>
                            </xs:complexType>
                        </xs:element>
                    </xs:sequence>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );
    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    v.validate_element("item", "", None, None, &ns);
    v.validate_attribute("id", "", "x");
    v.validate_end_of_attributes();
    v.validate_end_element();

    // "x" valid, "y" missing — derived IDREFS should track each token
    v.validate_element("multi", "", None, None, &ns);
    v.validate_attribute("refs", "", "x y");
    v.validate_end_of_attributes();
    v.validate_end_element();

    v.validate_end_element();
    v.end_validation().ok();

    let idref_errors: Vec<_> = v
        .sink
        .errors
        .iter()
        .filter(|e| e.constraint == "cvc-id.1")
        .collect();
    assert_eq!(
        idref_errors.len(),
        1,
        "Derived xs:IDREFS should track each token, got: {:?}",
        idref_errors
    );
}

/// Valid repeated IDREF values do not raise duplicate-style errors.
#[test]
fn test_repeated_idref_no_false_duplicate() {
    let schema_set = id_idref_attr_schema();
    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    v.validate_element("item", "", None, None, &ns);
    v.validate_attribute("id", "", "target");
    v.validate_end_of_attributes();
    v.validate_end_element();

    // Multiple references to the same ID — all valid
    v.validate_element("link", "", None, None, &ns);
    v.validate_attribute("ref", "", "target");
    v.validate_end_of_attributes();
    v.validate_end_element();

    v.validate_element("link", "", None, None, &ns);
    v.validate_attribute("ref", "", "target");
    v.validate_end_of_attributes();
    v.validate_end_element();

    v.validate_element("multi", "", None, None, &ns);
    v.validate_attribute("refs", "", "target target");
    v.validate_end_of_attributes();
    v.validate_end_element();

    v.validate_end_element();
    v.end_validation().ok();

    assert!(
        v.sink.errors.is_empty(),
        "Repeated IDREF to same ID should not error, got: {:?}",
        v.sink.errors
    );
}

/// Invalid lexical ID / IDREF values do not poison runtime tracking state.
#[test]
fn test_invalid_lexical_does_not_poison_tracking() {
    let schema_set = id_idref_attr_schema();
    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    // Invalid ID (not NCName) — should not be tracked
    v.validate_element("item", "", None, None, &ns);
    v.validate_attribute("id", "", "123bad");
    v.validate_end_of_attributes();
    v.validate_end_element();

    // Valid ID
    v.validate_element("item", "", None, None, &ns);
    v.validate_attribute("id", "", "good");
    v.validate_end_of_attributes();
    v.validate_end_element();

    // IDREF to the invalid one — should raise cvc-id.1
    v.validate_element("link", "", None, None, &ns);
    v.validate_attribute("ref", "", "123bad");
    v.validate_end_of_attributes();
    v.validate_end_element();

    // IDREF to the valid one — should be fine
    v.validate_element("link", "", None, None, &ns);
    v.validate_attribute("ref", "", "good");
    v.validate_end_of_attributes();
    v.validate_end_element();

    v.validate_end_element();
    v.end_validation().ok();

    // Should have lexical errors for the invalid ID + IDREF,
    // but the valid ID/IDREF pair should work
    let dup_errors: Vec<_> = v
        .sink
        .errors
        .iter()
        .filter(|e| e.constraint == "cvc-id.2")
        .collect();
    assert!(
        dup_errors.is_empty(),
        "Invalid lexical values should not cause cvc-id.2, got: {:?}",
        dup_errors
    );
    // "good" should resolve, "123bad" IDREF also fails lexically so no cvc-id.1 for it
    let idref_errors: Vec<_> = v
        .sink
        .errors
        .iter()
        .filter(|e| e.constraint == "cvc-id.1")
        .collect();
    assert!(
        idref_errors.is_empty(),
        "Invalid IDREF '123bad' should fail lexically, not produce cvc-id.1, got: {:?}",
        idref_errors
    );
}

/// User-defined <xs:list itemType="xs:IDREF"> tracks each token individually.
///
/// This proves that custom IDREF-list types (not just built-in xs:IDREFS)
/// correctly decompose into per-token tracking, even though
/// validate_list_type produces type_code==IdRef (the item code).
#[test]
fn test_custom_idref_list_tracking() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:simpleType name="myRefList">
                <xs:list itemType="xs:IDREF"/>
            </xs:simpleType>
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element name="item" maxOccurs="unbounded">
                            <xs:complexType>
                                <xs:attribute name="id" type="xs:ID" use="required"/>
                            </xs:complexType>
                        </xs:element>
                        <xs:element name="refs" minOccurs="0" maxOccurs="unbounded">
                            <xs:complexType>
                                <xs:attribute name="targets" type="myRefList" use="required"/>
                            </xs:complexType>
                        </xs:element>
                    </xs:sequence>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );
    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    v.validate_element("item", "", None, None, &ns);
    v.validate_attribute("id", "", "a1");
    v.validate_end_of_attributes();
    v.validate_end_element();

    // "a1" exists, "missing1" and "missing2" do not
    v.validate_element("refs", "", None, None, &ns);
    v.validate_attribute("targets", "", "a1 missing1 missing2");
    v.validate_end_of_attributes();
    v.validate_end_element();

    v.validate_end_element();
    v.end_validation().ok();

    let idref_errors: Vec<_> = v
        .sink
        .errors
        .iter()
        .filter(|e| e.constraint == "cvc-id.1")
        .collect();
    assert_eq!(
        idref_errors.len(), 2,
        "Custom IDREF-list should track each token; expected 2 cvc-id.1 errors for missing1/missing2, got: {:?}",
        idref_errors
    );
}

/// Whitespace normalization regression: ID and IDREF with surrounding
/// whitespace must match after collapse, and IDREFS cross-references
/// must resolve against the collapsed ID value.
#[test]
fn test_whitespace_normalization_id_idref_match() {
    let schema_set = id_idref_attr_schema();
    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    // ID with surrounding whitespace — collapsed to "foo"
    v.validate_element("item", "", None, None, &ns);
    v.validate_attribute("id", "", "  foo  ");
    v.validate_end_of_attributes();
    v.validate_end_element();

    // IDREF without whitespace — must match the collapsed ID
    v.validate_element("link", "", None, None, &ns);
    v.validate_attribute("ref", "", "foo");
    v.validate_end_of_attributes();
    v.validate_end_element();

    // IDREF with whitespace — must also match
    v.validate_element("link", "", None, None, &ns);
    v.validate_attribute("ref", "", "  foo  ");
    v.validate_end_of_attributes();
    v.validate_end_element();

    // IDREFS where the token matches the collapsed ID
    v.validate_element("multi", "", None, None, &ns);
    v.validate_attribute("refs", "", "  foo  ");
    v.validate_end_of_attributes();
    v.validate_end_element();

    v.validate_end_element();
    v.end_validation().ok();

    let idref_errors: Vec<_> = v
        .sink
        .errors
        .iter()
        .filter(|e| e.constraint == "cvc-id.1")
        .collect();
    assert!(
        idref_errors.is_empty(),
        "Whitespace-padded ID/IDREF/IDREFS should all resolve after collapse, got: {:?}",
        idref_errors
    );
    let dup_errors: Vec<_> = v
        .sink
        .errors
        .iter()
        .filter(|e| e.constraint == "cvc-id.2")
        .collect();
    assert!(
        dup_errors.is_empty(),
        "Single whitespace-padded ID should not produce duplicates, got: {:?}",
        dup_errors
    );
}

/// Whitespace normalization regression for element text content:
/// ID defined via element text with whitespace must be found by IDREF.
#[test]
fn test_whitespace_normalization_element_text() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element name="id" type="xs:ID" maxOccurs="unbounded"/>
                        <xs:element name="ref" type="xs:IDREF" maxOccurs="unbounded"/>
                    </xs:sequence>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );
    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    // ID element with whitespace text
    v.validate_element("id", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_text("  bar  ");
    v.validate_end_element();

    // IDREF element referencing collapsed value
    v.validate_element("ref", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_text("bar");
    v.validate_end_element();

    v.validate_end_element();
    v.end_validation().ok();

    let idref_errors: Vec<_> = v
        .sink
        .errors
        .iter()
        .filter(|e| e.constraint == "cvc-id.1")
        .collect();
    assert!(
        idref_errors.is_empty(),
        "Element-text ID '  bar  ' collapsed to 'bar' should match IDREF 'bar', got: {:?}",
        idref_errors
    );
}

// -----------------------------------------------------------------------
// xsi:type validation fallback semantics tests
// -----------------------------------------------------------------------

#[test]
fn test_xsi_type_unresolved_on_global_element() {
    // Global element + unknown xsi:type → Invalid, declared type used
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root" type="xs:string"/>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    let info = v.validate_element("root", "", Some("noSuchType"), None, &ns);
    assert_eq!(info.validity, SchemaValidity::Invalid);
    // schema_type should be the declared type (xs:string), not None
    assert!(info.schema_type.is_some());

    // Should have cvc-elt.4.1 error
    assert!(
        v.sink.errors.iter().any(|e| e.constraint == "cvc-elt.4.1"),
        "Expected cvc-elt.4.1 error, got: {:?}",
        v.sink.errors
    );

    // Text should still validate against the declared type (xs:string)
    v.validate_end_of_attributes();
    v.validate_text("hello");
    let end_info = v.validate_end_element();
    // End element should not produce additional type errors
    let type_errors: Vec<_> = v
        .sink
        .errors
        .iter()
        .filter(|e| e.constraint != "cvc-elt.4.1")
        .collect();
    assert!(
        type_errors.is_empty(),
        "Expected only cvc-elt.4.1 error, but got additional: {:?}",
        type_errors
    );
    // end_info preserves invalidity from the xsi:type error
    assert_eq!(end_info.validity, SchemaValidity::Invalid);
    v.end_validation().ok();
}

#[test]
fn test_xsi_type_invalid_derivation_on_global_element() {
    // Global element + xsi:type that doesn't derive → Invalid, declared type used
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root" type="xs:string"/>
            <xs:complexType name="unrelatedType">
                <xs:sequence>
                    <xs:element name="child" type="xs:string"/>
                </xs:sequence>
            </xs:complexType>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    let info = v.validate_element("root", "", Some("unrelatedType"), None, &ns);
    assert_eq!(info.validity, SchemaValidity::Invalid);
    // schema_type should be the declared type (xs:string), not unrelatedType
    assert!(info.schema_type.is_some());

    // Should have cvc-elt.4.2 error
    assert!(
        v.sink.errors.iter().any(|e| e.constraint == "cvc-elt.4.2"),
        "Expected cvc-elt.4.2 error, got: {:?}",
        v.sink.errors
    );

    // Assessment uses declared type (xs:string), so text content should be fine
    v.validate_end_of_attributes();
    v.validate_text("hello");
    v.validate_end_element();
    v.end_validation().ok();

    // No additional errors beyond cvc-elt.4.2
    let other_errors: Vec<_> = v
        .sink
        .errors
        .iter()
        .filter(|e| e.constraint != "cvc-elt.4.2")
        .collect();
    assert!(
        other_errors.is_empty(),
        "Expected only cvc-elt.4.2 error, but got additional: {:?}",
        other_errors
    );
}

#[test]
fn test_xsi_type_unresolved_on_local_element_with_type() {
    // Local element with type + unknown xsi:type → Invalid, falls back to matched type
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element name="item" type="xs:string"/>
                    </xs:sequence>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    let info = v.validate_element("item", "", Some("noSuchType"), None, &ns);
    assert_eq!(info.validity, SchemaValidity::Invalid);
    // Falls back to matched type (xs:string)
    assert!(info.schema_type.is_some());

    assert!(
        v.sink.errors.iter().any(|e| e.constraint == "cvc-elt.4.1"),
        "Expected cvc-elt.4.1 error, got: {:?}",
        v.sink.errors
    );

    v.validate_end_of_attributes();
    v.validate_text("hello");
    v.validate_end_element();
    v.validate_end_element();
    v.end_validation().ok();
}

#[test]
fn test_xsi_type_unresolved_lax_assessment() {
    // Local element without type + bad xsi:type → Invalid, lax assessment, children accepted
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:any processContents="lax"/>
                    </xs:sequence>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    // Unknown element matched by lax wildcard, with bad xsi:type
    let info = v.validate_element("unknown", "", Some("noSuchType"), None, &ns);
    // schema_type stays None (no governing type)
    assert!(info.schema_type.is_none());

    v.validate_end_of_attributes();
    // Nested child should be accepted via lax assessment (xs:anyType content model)
    v.validate_element("nested", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_text("hello");
    v.validate_end_element();

    v.validate_end_element(); // close unknown
    v.validate_end_element(); // close root
    v.end_validation().ok();

    // Should have cvc-elt.4.1 for the bad xsi:type, but no content model errors
    assert!(
        v.sink.errors.iter().any(|e| e.constraint == "cvc-elt.4.1"),
        "Expected cvc-elt.4.1 error, got: {:?}",
        v.sink.errors
    );
    let content_errors: Vec<_> = v
        .sink
        .errors
        .iter()
        .filter(|e| e.constraint == "cvc-complex-type.2.4")
        .collect();
    assert!(
        content_errors.is_empty(),
        "Lax assessment should not produce content model errors, got: {:?}",
        content_errors
    );
}

#[test]
fn test_undeclared_element_lax_allows_children() {
    // Lax wildcard + nested children → no errors, xs:anyType content model accepts children
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:any processContents="lax"/>
                    </xs:sequence>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    // Wildcard matches in content model → content_model_accepted path
    let info = v.validate_element("unknown", "", None, None, &ns);
    // Element accepted by content model, no governing type → schema_type = None
    assert!(info.schema_type.is_none());

    v.validate_end_of_attributes();
    // Nested children should be accepted via xs:anyType content model
    v.validate_element("child1", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_text("text1");
    v.validate_end_element();

    v.validate_element("child2", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_end_element();

    v.validate_end_element(); // close unknown
    v.validate_end_element(); // close root
    v.end_validation().ok();

    assert!(
        v.sink.errors.is_empty(),
        "Lax undeclared element should accept children without errors, got: {:?}",
        v.sink.errors
    );
}

#[test]
fn test_undeclared_element_skip_no_assessment() {
    // Skip wildcard + nested children → no errors, skip bypass prevents content model errors
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:any processContents="skip"/>
                    </xs:sequence>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    v.validate_element("anything", "", None, None, &ns);
    v.validate_end_of_attributes();

    // Deeply nested children should be accepted (skip bypass)
    v.validate_element("nested1", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_element("nested2", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_text("deep");
    v.validate_end_element(); // close nested2
    v.validate_end_element(); // close nested1

    v.validate_end_element(); // close anything
    v.validate_end_element(); // close root
    v.end_validation().ok();

    assert!(
        v.sink.errors.is_empty(),
        "Skip wildcard should accept all nested content without errors, got: {:?}",
        v.sink.errors
    );
}

#[test]
fn test_strict_undeclared_same_assessment_as_lax() {
    // Strict wildcard: element is matched by wildcard in content model with
    // processContents=strict, but has no global declaration → cvc-elt.1.
    // Children should still be accepted via lax assessment.
    //
    // Use namespace-based wildcard to get strict processContents on
    // an element that is NOT globally declared.
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
                     targetNamespace="http://test">
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:any namespace="http://other" processContents="strict"/>
                    </xs:sequence>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "http://test", None, None, &ns);
    v.validate_end_of_attributes();

    let info = v.validate_element("unknown", "http://other", None, None, &ns);
    assert_eq!(info.validity, SchemaValidity::Invalid);

    // cvc-elt.1 for undeclared element under strict processing
    assert!(
        v.sink.errors.iter().any(|e| e.constraint == "cvc-elt.1"),
        "Expected cvc-elt.1 error, got: {:?}",
        v.sink.errors
    );

    v.validate_end_of_attributes();
    // Children should still be accepted (lax assessment for content)
    v.validate_element("child", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_text("hello");
    v.validate_end_element();

    v.validate_end_element(); // close unknown
    v.validate_end_element(); // close root
    v.end_validation().ok();

    // No content model errors on the unknown element's children
    let content_errors: Vec<_> = v
        .sink
        .errors
        .iter()
        .filter(|e| e.constraint == "cvc-complex-type.2.4")
        .collect();
    assert!(
        content_errors.is_empty(),
        "Strict undeclared element should use lax assessment for children, got: {:?}",
        content_errors
    );
}

#[cfg(feature = "xsd11")]
#[test]
fn test_cta_preserves_xsi_type_invalidity() {
    // CTA switch after bad xsi:type → type switches, validity stays Invalid
    // typeB extends typeA so the CTA alternative is validly substitutable
    // (§3.12.6) — the test exercises switch-after-xsi:type-failure, not
    // substitutability itself.
    let schema_set = load_schema_xsd11(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:complexType name="typeA">
                <xs:sequence>
                    <xs:element name="a" type="xs:string"/>
                </xs:sequence>
            </xs:complexType>
            <xs:complexType name="typeB">
                <xs:complexContent>
                    <xs:extension base="typeA">
                        <xs:sequence>
                            <xs:element name="b" type="xs:string"/>
                        </xs:sequence>
                    </xs:extension>
                </xs:complexContent>
            </xs:complexType>
            <xs:element name="root" type="typeA">
                <xs:alternative test="@kind = 'B'" type="typeB"/>
            </xs:element>
        </xs:schema>"#,
    );

    let flags = ValidationFlags::default() | ValidationFlags::PROCESS_ASSERTIONS;
    let validator = SchemaValidator::new(&schema_set, flags);
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    // Bad xsi:type (unrelated to typeA) + CTA trigger attribute
    let info = v.validate_element("root", "", Some("noSuchType"), None, &ns);
    assert_eq!(info.validity, SchemaValidity::Invalid);

    // Supply CTA-triggering attribute
    v.validate_attribute("kind", "", "B");
    let eoa_info = v.validate_end_of_attributes();

    // CTA should switch to typeB, but validity should stay Invalid
    assert_eq!(
        eoa_info.validity,
        SchemaValidity::Invalid,
        "CTA switch should preserve prior invalidity from bad xsi:type"
    );

    // Validate content against typeB
    v.validate_element("b", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_text("hello");
    v.validate_end_element();

    v.validate_end_element(); // close root
    v.end_validation().ok();

    assert!(
        v.sink.errors.iter().any(|e| e.constraint == "cvc-elt.4.1"),
        "Expected cvc-elt.4.1 for bad xsi:type, got: {:?}",
        v.sink.errors
    );
}

// -----------------------------------------------------------------------
// Reviewer finding regression tests (P1/P2)
// -----------------------------------------------------------------------

/// P1(a): Lax-assessment elements must assess attributes against xs:anyType's
/// anyAttribute wildcard, not skip them entirely.
#[test]
fn test_lax_assessment_validates_attributes_against_any_type() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:any processContents="lax"/>
                    </xs:sequence>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    // Undeclared element matched by lax wildcard → lax assessment, schema_type=None
    let info = v.validate_element("unknown", "", None, None, &ns);
    assert!(info.schema_type.is_none());

    // Attributes should be accepted (xs:anyType's anyAttribute lax wildcard)
    let attr_info = v.validate_attribute("myattr", "", "some-value");
    assert_ne!(
        attr_info.validity,
        SchemaValidity::Invalid,
        "Lax assessment should accept attributes via xs:anyType's anyAttribute wildcard"
    );

    v.validate_end_of_attributes();
    v.validate_end_element();
    v.validate_end_element();
    v.end_validation().ok();

    // No errors about unexpected attributes
    let attr_errors: Vec<_> = v
        .sink
        .errors
        .iter()
        .filter(|e| e.constraint.contains("cvc-complex-type"))
        .collect();
    assert!(
        attr_errors.is_empty(),
        "Lax assessment should not produce attribute errors, got: {:?}",
        attr_errors
    );
}

/// P1(b): Descendants of a skip wildcard must remain unassessed even when
/// globally declared.
#[test]
fn test_skip_descendant_globally_declared_not_validated() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:any processContents="skip"/>
                    </xs:sequence>
                </xs:complexType>
            </xs:element>
            <xs:element name="known" type="xs:integer"/>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    // Enter skipped subtree
    v.validate_element("wrapper", "", None, None, &ns);
    v.validate_end_of_attributes();

    // "known" is globally declared as xs:integer, but inside a skip subtree
    // it must remain unassessed — invalid text should NOT produce errors
    v.validate_element("known", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_text("not-an-integer");
    v.validate_end_element();

    v.validate_end_element(); // close wrapper
    v.validate_end_element(); // close root
    v.end_validation().ok();

    assert!(
        v.sink.errors.is_empty(),
        "Globally declared element inside skip subtree should not be validated, got: {:?}",
        v.sink.errors
    );
}

/// P2: Strict wildcard with valid xsi:type should use that type for
/// assessment instead of rejecting with cvc-elt.1.
#[test]
fn test_strict_wildcard_xsi_type_supplies_governing_type() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
                     targetNamespace="http://test">
            <xs:complexType name="myType">
                <xs:sequence>
                    <xs:element name="child" type="xs:string"/>
                </xs:sequence>
            </xs:complexType>
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:any namespace="http://other" processContents="strict"/>
                    </xs:sequence>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    // Need namespace binding for xsi:type resolution
    let tns_prefix = schema_set.name_table.add("tns");
    let tns_uri = schema_set.name_table.add("http://test");
    let ns = NamespaceContextSnapshot {
        default_ns: None,
        bindings: vec![(tns_prefix, tns_uri)],
    };

    v.validate_element("root", "http://test", None, None, &ns);
    v.validate_end_of_attributes();

    // Element "foo" in http://other is NOT globally declared, matched by
    // strict wildcard. But xsi:type supplies tns:myType as governing type.
    let info = v.validate_element("foo", "http://other", Some("tns:myType"), None, &ns);
    // xsi:type supplied a valid governing type — element should be valid
    assert!(
        info.schema_type.is_some(),
        "xsi:type should supply governing type even without global declaration"
    );

    // No cvc-elt.1 error — xsi:type provided the governing type
    let elt1_errors: Vec<_> = v
        .sink
        .errors
        .iter()
        .filter(|e| e.constraint == "cvc-elt.1")
        .collect();
    assert!(
        elt1_errors.is_empty(),
        "Strict wildcard should not report cvc-elt.1 when xsi:type supplies a type, got: {:?}",
        elt1_errors
    );

    v.validate_end_of_attributes();
    v.validate_element("child", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_text("hello");
    v.validate_end_element();
    v.validate_end_element(); // close foo
    v.validate_end_element(); // close root
    v.end_validation().ok();

    assert!(
        v.sink.errors.is_empty(),
        "No errors expected when xsi:type supplies valid governing type, got: {:?}",
        v.sink.errors
    );
}

// ── PSVI TypeSource / CTA / AssertionOutcome tests ──────────────────

#[test]
fn test_type_source_declaration() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root" type="xs:string"/>
        </xs:schema>"#,
    );
    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    let info = v.validate_element("root", "", None, None, &ns);
    assert_eq!(info.type_source, Some(TypeSource::Declaration));
    v.validate_end_of_attributes();
    v.validate_text("hello");
    let end_info = v.validate_end_element();
    assert_eq!(end_info.type_source, Some(TypeSource::Declaration));
    v.end_validation().ok();
}

#[test]
fn test_type_source_xsi_type() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root" type="xs:anyType"/>
            <xs:complexType name="myType">
                <xs:sequence>
                    <xs:element name="child" type="xs:string"/>
                </xs:sequence>
            </xs:complexType>
        </xs:schema>"#,
    );
    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    let info = v.validate_element("root", "", Some("myType"), None, &ns);
    assert_eq!(info.type_source, Some(TypeSource::XsiType));
    v.validate_end_of_attributes();
    v.validate_element("child", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_text("hello");
    v.validate_end_element();
    let end_info = v.validate_end_element();
    assert_eq!(end_info.type_source, Some(TypeSource::XsiType));
    v.end_validation().ok();
    assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
}

#[cfg(feature = "xsd11")]
#[test]
fn test_type_source_cta() {
    let schema_set = load_schema_xsd11(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:complexType name="intContent">
                <xs:sequence>
                    <xs:element name="val" type="xs:integer"/>
                </xs:sequence>
                <xs:attribute name="kind" type="xs:string"/>
            </xs:complexType>
            <xs:element name="data">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element name="val" type="xs:string"/>
                    </xs:sequence>
                    <xs:attribute name="kind" type="xs:string"/>
                </xs:complexType>
                <xs:alternative test="@kind='int'" type="intContent"/>
            </xs:element>
        </xs:schema>"#,
    );
    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    let info = v.validate_element("data", "", None, None, &ns);
    // Before CTA, type_source is Declaration
    assert_eq!(info.type_source, Some(TypeSource::Declaration));

    v.validate_attribute("kind", "", "int");
    let eoa_info = v.validate_end_of_attributes();
    // CTA switched → TypeAlternative
    assert_eq!(eoa_info.type_source, Some(TypeSource::TypeAlternative));
    assert!(eoa_info.cta_selected);

    v.validate_element("val", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_text("42");
    v.validate_end_element();

    let end_info = v.validate_end_element();
    assert_eq!(end_info.type_source, Some(TypeSource::TypeAlternative));
    assert!(end_info.cta_selected);

    v.end_validation().ok();
    assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
}

#[cfg(feature = "xsd11")]
#[test]
fn test_cta_selected_same_type() {
    // Schema where a CTA alternative selects the same type as the declared type.
    // cta_selected should still be true, type_source should be TypeAlternative.
    let schema_set = load_schema_xsd11(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:complexType name="baseType">
                <xs:sequence>
                    <xs:element name="val" type="xs:string"/>
                </xs:sequence>
                <xs:attribute name="kind" type="xs:string"/>
            </xs:complexType>
            <xs:element name="data" type="baseType">
                <xs:alternative test="@kind='same'" type="baseType"/>
            </xs:element>
        </xs:schema>"#,
    );
    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("data", "", None, None, &ns);
    v.validate_attribute("kind", "", "same");
    let eoa_info = v.validate_end_of_attributes();
    // CTA selected same type → cta_selected true, type_source TypeAlternative
    assert!(
        eoa_info.cta_selected,
        "cta_selected should be true even when type is unchanged"
    );
    assert_eq!(eoa_info.type_source, Some(TypeSource::TypeAlternative));

    v.validate_element("val", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_text("hello");
    v.validate_end_element();
    v.validate_end_element();
    v.end_validation().ok();
    assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
}

#[test]
fn test_type_source_end_element() {
    // Verify that end-element carries the type_source from start-element
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root" type="xs:integer"/>
        </xs:schema>"#,
    );
    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    let start_info = v.validate_element("root", "", None, None, &ns);
    assert_eq!(start_info.type_source, Some(TypeSource::Declaration));

    v.validate_end_of_attributes();
    v.validate_text("42");

    let end_info = v.validate_end_element();
    assert_eq!(end_info.type_source, Some(TypeSource::Declaration));
    assert_eq!(end_info.validity, SchemaValidity::Valid);

    v.end_validation().ok();
    assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
}

#[cfg(feature = "xsd11")]
#[test]
fn test_assertion_outcome_passed() {
    let schema_set = load_schema_xsd11(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:attribute name="val" type="xs:integer"/>
                    <xs:assert test="@val >= 0"/>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );
    let validator = SchemaValidator::new_fragment_buffer(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_attribute("val", "", "42");
    v.validate_end_of_attributes();
    let end_info = v.validate_end_element();
    assert_eq!(
        end_info.assertion_outcome,
        Some(AssertionOutcome::Passed),
        "Passing assertion should yield Passed"
    );
    v.end_validation().ok();
    assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
}

#[cfg(feature = "xsd11")]
#[test]
fn test_assertion_outcome_failed() {
    let schema_set = load_schema_xsd11(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:attribute name="val" type="xs:integer"/>
                    <xs:assert test="@val >= 0"/>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );
    let validator = SchemaValidator::new_fragment_buffer(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_attribute("val", "", "-5"); // -5 < 0, assertion fails
    v.validate_end_of_attributes();
    let end_info = v.validate_end_element();
    assert_eq!(
        end_info.assertion_outcome,
        Some(AssertionOutcome::Failed),
        "Failing assertion should yield Failed"
    );
    v.end_validation().ok();
    assert!(!v.sink.errors.is_empty(), "Should have assertion error");
}

#[cfg(feature = "xsd11")]
#[test]
fn test_assertion_outcome_not_evaluated() {
    // Use default flags (no PROCESS_ASSERTIONS) — assertions exist but won't be evaluated
    let schema_set = load_schema_xsd11(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:attribute name="val" type="xs:integer"/>
                    <xs:assert test="@val >= 0"/>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );
    // Default validator — no fragment buffer, PROCESS_ASSERTIONS not set
    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_attribute("val", "", "42");
    v.validate_end_of_attributes();
    let end_info = v.validate_end_element();
    assert_eq!(
        end_info.assertion_outcome,
        Some(AssertionOutcome::NotEvaluated),
        "Assertions exist but PROCESS_ASSERTIONS not set → NotEvaluated"
    );
    v.end_validation().ok();
}

#[cfg(feature = "xsd11")]
#[test]
fn test_no_assertions_outcome_none() {
    // Element without assertions → assertion_outcome should be None
    let schema_set = load_schema_xsd11(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root" type="xs:string"/>
        </xs:schema>"#,
    );
    let validator = SchemaValidator::new_fragment_buffer(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_text("hello");
    let end_info = v.validate_end_element();
    assert_eq!(
        end_info.assertion_outcome, None,
        "No assertions on type → None"
    );
    v.end_validation().ok();
    assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
}

// -----------------------------------------------------------------------
// Inheritable attribute tests (XSD 1.1 §3.3.5.6)
// -----------------------------------------------------------------------

#[test]
#[cfg(feature = "xsd11")]
fn test_inheritable_basic() {
    // Parent has inheritable="true" attr lang, child inherits the value
    let schema_set = load_schema_xsd11(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element name="child" type="xs:string"/>
                    </xs:sequence>
                    <xs:attribute name="lang" type="xs:string" inheritable="true"/>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_attribute("lang", "", "en");
    v.validate_end_of_attributes();

    v.validate_element("child", "", None, None, &ns);
    v.validate_end_of_attributes();

    let inherited = v.get_inherited_attributes();
    assert_eq!(inherited.len(), 1, "child should inherit 'lang'");
    let lang = &inherited[0];
    assert_eq!(v.schema_set.name_table.resolve(lang.local_name), "lang");
    assert_eq!(lang.value, "en");

    v.validate_text("hello");
    v.validate_end_element();
    v.validate_end_element();
    v.end_validation().ok();
    assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
}

#[test]
#[cfg(feature = "xsd11")]
fn test_inheritable_override() {
    // Parent lang="en", child overrides lang="fr", grandchild inherits "fr"
    let schema_set = load_schema_xsd11(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element name="mid">
                            <xs:complexType>
                                <xs:sequence>
                                    <xs:element name="leaf" type="xs:string"/>
                                </xs:sequence>
                                <xs:attribute name="lang" type="xs:string" inheritable="true"/>
                            </xs:complexType>
                        </xs:element>
                    </xs:sequence>
                    <xs:attribute name="lang" type="xs:string" inheritable="true"/>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_attribute("lang", "", "en");
    v.validate_end_of_attributes();

    v.validate_element("mid", "", None, None, &ns);

    // Before providing the override, mid's incoming_inherited has the ancestor value
    let mid_inherited = v.get_inherited_attributes();
    assert_eq!(mid_inherited.len(), 1);
    assert_eq!(
        mid_inherited[0].value, "en",
        "overriding element itself should still see ancestor value from incoming_inherited"
    );

    v.validate_attribute("lang", "", "fr"); // override → updates outgoing_inherited
    v.validate_end_of_attributes();

    // After attributes, mid's PSVI [inherited attributes] is unchanged (incoming)
    let mid_inherited_after = v.get_inherited_attributes();
    assert_eq!(
        mid_inherited_after[0].value, "en",
        "PSVI [inherited attributes] is the incoming snapshot, not affected by own attrs"
    );

    v.validate_element("leaf", "", None, None, &ns);
    v.validate_end_of_attributes();

    let inherited = v.get_inherited_attributes();
    assert_eq!(inherited.len(), 1);
    assert_eq!(
        inherited[0].value, "fr",
        "grandchild should see overridden value"
    );

    v.validate_text("text");
    v.validate_end_element();
    v.validate_end_element();
    v.validate_end_element();
    v.end_validation().ok();
    assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
}

#[test]
#[cfg(feature = "xsd11")]
fn test_inheritable_multi_level() {
    // Root lang="en" → mid (no lang) → leaf (no lang)
    // Both mid and leaf inherit lang="en"
    let schema_set = load_schema_xsd11(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element name="mid">
                            <xs:complexType>
                                <xs:sequence>
                                    <xs:element name="leaf" type="xs:string"/>
                                </xs:sequence>
                            </xs:complexType>
                        </xs:element>
                    </xs:sequence>
                    <xs:attribute name="lang" type="xs:string" inheritable="true"/>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_attribute("lang", "", "en");
    v.validate_end_of_attributes();

    v.validate_element("mid", "", None, None, &ns);
    v.validate_end_of_attributes();

    let mid_inherited = v.get_inherited_attributes();
    assert_eq!(mid_inherited.len(), 1, "mid should inherit 'lang'");
    assert_eq!(mid_inherited[0].value, "en");

    v.validate_element("leaf", "", None, None, &ns);
    v.validate_end_of_attributes();

    let leaf_inherited = v.get_inherited_attributes();
    assert_eq!(leaf_inherited.len(), 1, "leaf should inherit 'lang'");
    assert_eq!(leaf_inherited[0].value, "en");

    v.validate_text("text");
    v.validate_end_element();
    v.validate_end_element();
    v.validate_end_element();
    v.end_validation().ok();
    assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
}

#[test]
#[cfg(feature = "xsd11")]
fn test_inheritable_not_set() {
    // Attr without inheritable="true" is NOT inherited
    let schema_set = load_schema_xsd11(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element name="child" type="xs:string"/>
                    </xs:sequence>
                    <xs:attribute name="lang" type="xs:string"/>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_attribute("lang", "", "en");
    v.validate_end_of_attributes();

    v.validate_element("child", "", None, None, &ns);
    v.validate_end_of_attributes();

    let inherited = v.get_inherited_attributes();
    assert!(
        inherited.is_empty(),
        "non-inheritable attr should NOT be inherited"
    );

    v.validate_text("text");
    v.validate_end_element();
    v.validate_end_element();
    v.end_validation().ok();
    assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
}

#[test]
#[cfg(feature = "xsd11")]
fn test_inheritable_no_child_type_filter() {
    // Parent has inheritable lang, child type does NOT declare lang.
    // [inherited attributes] still includes lang (no child-type gate per spec).
    let schema_set = load_schema_xsd11(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element name="child">
                            <xs:complexType>
                                <xs:sequence>
                                    <xs:element name="x" type="xs:string"/>
                                </xs:sequence>
                            </xs:complexType>
                        </xs:element>
                    </xs:sequence>
                    <xs:attribute name="lang" type="xs:string" inheritable="true"/>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_attribute("lang", "", "en");
    v.validate_end_of_attributes();

    v.validate_element("child", "", None, None, &ns);
    v.validate_end_of_attributes();

    let inherited = v.get_inherited_attributes();
    assert_eq!(
        inherited.len(),
        1,
        "inherited attrs should be present even when child type doesn't declare it"
    );
    assert_eq!(inherited[0].value, "en");

    v.validate_element("x", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_text("text");
    v.validate_end_element();
    v.validate_end_element();
    v.validate_end_element();
    v.end_validation().ok();
    assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
}

#[test]
#[cfg(feature = "xsd11")]
fn test_inheritable_cta() {
    // Parent has inheritable lang="en", child has type alternatives using @lang.
    // CTA should see inherited lang="en" via §3.12.4 clause 1.1.3.
    let schema_set = load_schema_xsd11(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:complexType name="enType">
                <xs:sequence>
                    <xs:element name="val" type="xs:string"/>
                </xs:sequence>
            </xs:complexType>
            <xs:element name="item">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element name="other" type="xs:string"/>
                    </xs:sequence>
                </xs:complexType>
                <xs:alternative test="@lang='en'" type="enType"/>
            </xs:element>
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element ref="item"/>
                    </xs:sequence>
                    <xs:attribute name="lang" type="xs:string" inheritable="true"/>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_attribute("lang", "", "en");
    v.validate_end_of_attributes();

    // child "item" does not have explicit lang, but CTA should see inherited lang="en"
    v.validate_element("item", "", None, None, &ns);
    let eoa_info = v.validate_end_of_attributes();

    // CTA should have selected enType (which has <val>)
    assert!(
        eoa_info.cta_selected,
        "CTA should have selected a type using inherited lang"
    );

    // Validate with enType's content model (has <val>)
    v.validate_element("val", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_text("hello");
    v.validate_end_element();
    v.validate_end_element();
    v.validate_end_element();
    v.end_validation().ok();
    assert!(
        v.sink.errors.is_empty(),
        "CTA with inherited attr should produce no errors, got: {:?}",
        v.sink.errors
    );
}

#[test]
#[cfg(feature = "xsd11")]
fn test_inheritable_wildcard() {
    // Attribute matched via wildcard; global declaration has inheritable="true".
    // Child should inherit via §3.3.5.6 clause 3.2.
    // Global inheritable attribute + anyAttribute wildcard.
    // "lang" matches the wildcard, and the global declaration has
    // inheritable="true" → §3.3.5.6 clause 3.2 applies.
    let schema_set = load_schema_xsd11(
        r###"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:attribute name="lang" type="xs:string" inheritable="true"/>
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element name="child" type="xs:string"/>
                    </xs:sequence>
                    <xs:anyAttribute namespace="##any" processContents="lax"/>
                </xs:complexType>
            </xs:element>
        </xs:schema>"###,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    // "lang" is not declared in the complex type's attribute uses, but
    // matches the anyAttribute wildcard. The global declaration has
    // inheritable="true", so §3.3.5.6 clause 3.2 applies.
    v.validate_attribute("lang", "", "en");
    v.validate_end_of_attributes();

    v.validate_element("child", "", None, None, &ns);
    v.validate_end_of_attributes();

    let inherited = v.get_inherited_attributes();
    assert_eq!(
        inherited.len(),
        1,
        "wildcard-backed inheritable attr should be inherited"
    );
    assert_eq!(inherited[0].value, "en");

    v.validate_text("text");
    v.validate_end_element();
    v.validate_end_element();
    v.end_validation().ok();
    assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
}

#[test]
#[cfg(feature = "xsd11")]
fn test_inheritable_default_shadows_ancestor() {
    // Root provides explicit lang="en" (inheritable). Mid's type declares
    // lang with inheritable="true" and default="fr". Mid does NOT provide
    // lang explicitly, so the default "fr" applies and shadows the ancestor
    // "en" for mid's descendants. Leaf should see lang="fr".
    let schema_set = load_schema_xsd11(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element name="mid">
                            <xs:complexType>
                                <xs:sequence>
                                    <xs:element name="leaf" type="xs:string"/>
                                </xs:sequence>
                                <xs:attribute name="lang" type="xs:string"
                                              inheritable="true" default="fr"/>
                            </xs:complexType>
                        </xs:element>
                    </xs:sequence>
                    <xs:attribute name="lang" type="xs:string" inheritable="true"/>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_attribute("lang", "", "en");
    v.validate_end_of_attributes();

    // mid: no explicit lang — default "fr" kicks in
    v.validate_element("mid", "", None, None, &ns);
    v.validate_end_of_attributes();

    // mid's own PSVI [inherited attributes] is the ancestor's "en"
    let mid_inherited = v.get_inherited_attributes();
    assert_eq!(mid_inherited.len(), 1);
    assert_eq!(
        mid_inherited[0].value, "en",
        "mid's incoming inherited should be ancestor's en"
    );

    // leaf should see "fr" from mid's defaulted inheritable attribute
    v.validate_element("leaf", "", None, None, &ns);
    v.validate_end_of_attributes();

    let leaf_inherited = v.get_inherited_attributes();
    assert_eq!(leaf_inherited.len(), 1);
    assert_eq!(
        leaf_inherited[0].value, "fr",
        "leaf should see defaulted value fr, not ancestor en"
    );

    v.validate_text("text");
    v.validate_end_element();
    v.validate_end_element();
    v.validate_end_element();
    v.end_validation().ok();
    assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
}

// =========================================================================
// PSVI Exposure Tests (Phase 4)
// =========================================================================

#[test]
fn test_psvi_normalized_value_token() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root" type="xs:token"/>
        </xs:schema>"#,
    );
    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_text("  hello   world  ");

    let end_info = v.validate_end_element();
    assert_eq!(end_info.validity, SchemaValidity::Valid);
    assert_eq!(
        end_info.normalized_value.as_deref(),
        Some("hello world"),
        "xs:token should collapse whitespace"
    );
    v.end_validation().ok();
}

#[test]
fn test_psvi_normalized_value_string() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root" type="xs:string"/>
        </xs:schema>"#,
    );
    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_text("  hello   world  ");

    let end_info = v.validate_end_element();
    assert_eq!(end_info.validity, SchemaValidity::Valid);
    assert_eq!(
        end_info.normalized_value.as_deref(),
        Some("  hello   world  "),
        "xs:string should preserve whitespace"
    );
    v.end_validation().ok();
}

#[test]
fn test_psvi_normalized_value_attribute() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:attribute name="val" type="xs:token"/>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );
    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    let attr_info = v.validate_attribute("val", "", "  a  b  ");
    assert_eq!(
        attr_info.normalized_value.as_deref(),
        Some("a b"),
        "xs:token attribute should collapse whitespace"
    );
    v.validate_end_of_attributes();
    v.validate_end_element();
    v.end_validation().ok();
}

#[test]
fn test_psvi_schema_error_codes_invalid_type() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root" type="xs:integer"/>
        </xs:schema>"#,
    );
    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_text("not-a-number");

    let end_info = v.validate_end_element();
    assert_eq!(end_info.validity, SchemaValidity::Invalid);
    assert!(
        !end_info.schema_error_codes.is_empty(),
        "invalid element should have error codes"
    );
    v.end_validation().ok();
}

#[test]
fn test_psvi_schema_error_codes_valid_element() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root" type="xs:string"/>
        </xs:schema>"#,
    );
    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_text("hello");

    let end_info = v.validate_end_element();
    assert_eq!(end_info.validity, SchemaValidity::Valid);
    assert!(
        end_info.schema_error_codes.is_empty(),
        "valid element should have no error codes"
    );
    v.end_validation().ok();
}

#[test]
fn test_psvi_schema_error_codes_duplicate_attribute() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:attribute name="a" type="xs:string"/>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );
    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_attribute("a", "", "first");
    let dup_info = v.validate_attribute("a", "", "second");
    assert_eq!(dup_info.validity, SchemaValidity::Invalid);
    assert!(
        dup_info.schema_error_codes.contains(&"cvc-complex-type.3"),
        "duplicate attribute should have cvc-complex-type.3 error code, got {:?}",
        dup_info.schema_error_codes
    );
    v.validate_end_of_attributes();
    v.validate_end_element();
    v.end_validation().ok();
}

#[test]
fn test_psvi_validation_attempted_full() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root" type="xs:string"/>
        </xs:schema>"#,
    );
    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_text("hello");

    let end_info = v.validate_end_element();
    assert_eq!(
        end_info.validation_attempted,
        crate::validation::ValidationAttempted::Full,
        "fully validated element should report Full"
    );
    v.end_validation().ok();
}

#[test]
fn test_psvi_validation_attempted_attribute_full() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:attribute name="a" type="xs:string"/>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );
    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    let attr_info = v.validate_attribute("a", "", "val");
    assert_eq!(
        attr_info.validation_attempted,
        crate::validation::ValidationAttempted::Full,
        "declared attribute should report Full"
    );
    v.validate_end_of_attributes();
    v.validate_end_element();
    v.end_validation().ok();
}

#[test]
fn test_psvi_is_simple_complex_type() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="simple" type="xs:string"/>
            <xs:element name="complex">
                <xs:complexType>
                    <xs:sequence/>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );
    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    // Simple type element
    let info = v.validate_element("simple", "", None, None, &ns);
    assert!(
        info.is_simple_type(),
        "xs:string element should be simple type"
    );
    assert!(!info.is_complex_type());
    v.validate_end_of_attributes();
    v.validate_text("hi");
    v.validate_end_element();

    // Complex type element
    let info = v.validate_element("complex", "", None, None, &ns);
    assert!(
        info.is_complex_type(),
        "complexType element should be complex type"
    );
    assert!(!info.is_simple_type());
    v.validate_end_of_attributes();
    v.validate_end_element();

    v.end_validation().ok();
}

#[test]
fn test_psvi_identity_constraint_tables() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element name="item" maxOccurs="unbounded">
                            <xs:complexType>
                                <xs:attribute name="id" type="xs:string" use="required"/>
                            </xs:complexType>
                        </xs:element>
                    </xs:sequence>
                </xs:complexType>
                <xs:unique name="itemId">
                    <xs:selector xpath="item"/>
                    <xs:field xpath="@id"/>
                </xs:unique>
            </xs:element>
        </xs:schema>"#,
    );
    let flags = ValidationFlags::default() | ValidationFlags::PROCESS_IDENTITY_CONSTRAINTS;
    let validator = SchemaValidator::new(&schema_set, flags);
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    v.validate_element("item", "", None, None, &ns);
    v.validate_attribute("id", "", "a");
    v.validate_end_of_attributes();
    v.validate_end_element();

    v.validate_element("item", "", None, None, &ns);
    v.validate_attribute("id", "", "b");
    v.validate_end_of_attributes();
    v.validate_end_element();

    v.validate_end_element();
    v.end_validation().ok();
    assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);

    let tables = v.identity_constraint_tables();
    assert!(
        tables.is_some(),
        "IC tables should be available after validation"
    );
    let tables = tables.unwrap();
    assert!(!tables.is_empty(), "should have at least one IC table");
    // The unique constraint should have 2 sequences (one for each item)
    let table = tables.values().next().unwrap();
    assert_eq!(
        table.sequences.len(),
        2,
        "unique constraint should have 2 key sequences"
    );
}

#[test]
fn test_psvi_schema_error_codes_root_xsi_type() {
    // Root element with invalid xsi:type — error must land on the element, not be lost
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root" type="xs:string"/>
        </xs:schema>"#,
    );
    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    // xsi:type with a non-existent type on the root element
    let _start = v.validate_element("root", "", Some("xs:nonExistentType"), None, &ns);
    v.validate_end_of_attributes();
    v.validate_text("hello");

    let end_info = v.validate_end_element();
    // The error code must be on the element's schema_error_codes
    assert!(
        end_info.schema_error_codes.contains(&"cvc-elt.4.1"),
        "root xsi:type error should be in element schema_error_codes, got {:?}",
        end_info.schema_error_codes
    );
    v.end_validation().ok();
}

#[test]
fn test_psvi_schema_error_codes_wildcard_xsi_type() {
    // Wildcard child element with invalid xsi:type — error must land on the child
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:any processContents="lax"/>
                    </xs:sequence>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );
    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());

    let xsi_ns = "http://www.w3.org/2001/XMLSchema-instance";
    let xs_ns = "http://www.w3.org/2001/XMLSchema";
    let ns = NamespaceContextSnapshot {
        default_ns: None,
        bindings: vec![
            (
                v.schema_set.name_table.add("xsi"),
                v.schema_set.name_table.add(xsi_ns),
            ),
            (
                v.schema_set.name_table.add("xs"),
                v.schema_set.name_table.add(xs_ns),
            ),
        ],
    };

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    // Wildcard child with bad xsi:type
    v.validate_element("child", "", Some("xs:nonExistentType"), None, &ns);
    v.validate_end_of_attributes();
    let end_info = v.validate_end_element();

    // cvc-elt.4.1 must be on the child element, not the parent
    assert!(
        end_info.schema_error_codes.contains(&"cvc-elt.4.1"),
        "wildcard child xsi:type error should be in child schema_error_codes, got {:?}",
        end_info.schema_error_codes
    );

    // Parent should NOT carry the child's xsi:type error
    let parent_end = v.validate_end_element();
    assert!(
        !parent_end.schema_error_codes.contains(&"cvc-elt.4.1"),
        "parent should not carry child's xsi:type error code, got {:?}",
        parent_end.schema_error_codes
    );

    v.end_validation().ok();
}

// -----------------------------------------------------------------------
// XSI built-in attribute validation tests
// -----------------------------------------------------------------------

const XSI_NS: &str = "http://www.w3.org/2001/XMLSchema-instance";

#[test]
fn test_xsi_no_namespace_schema_location_valid() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root" type="xs:string"/>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    let info = v.validate_attribute("noNamespaceSchemaLocation", XSI_NS, "schema.xsd");

    assert_eq!(info.validity, SchemaValidity::Valid);
    assert_eq!(info.validation_attempted, ValidationAttempted::Full);
    assert_eq!(
        info.schema_type,
        Some(TypeKey::Simple(schema_set.builtin_types().any_uri))
    );
    assert!(info.typed_value.is_some());
    assert!(info.attribute_decl.is_some()); // built-in XSI attribute declaration

    assert_eq!(
        v.no_namespace_schema_location_hints(),
        &[NoNamespaceSchemaLocationHint {
            location: "schema.xsd".to_string(),
            base_uri: String::new(),
        }]
    );
    assert!(v.schema_location_hints().is_empty());

    v.validate_end_of_attributes();
    v.validate_text("hello");
    v.validate_end_element();
    v.end_validation().ok();
    assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
}

#[test]
fn test_xsi_schema_location_valid() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root" type="xs:string"/>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    let info = v.validate_attribute(
        "schemaLocation",
        XSI_NS,
        "http://example.com schema.xsd http://other.com other.xsd",
    );

    assert_eq!(info.validity, SchemaValidity::Valid);
    assert_eq!(info.validation_attempted, ValidationAttempted::Full);
    assert_eq!(
        info.schema_type,
        Some(TypeKey::Simple(
            schema_set.builtin_types().xsi_schema_location_type
        ))
    );

    assert_eq!(
        v.schema_location_hints(),
        &[
            SchemaLocationHint {
                namespace: "http://example.com".to_string(),
                location: "schema.xsd".to_string(),
                base_uri: String::new(),
            },
            SchemaLocationHint {
                namespace: "http://other.com".to_string(),
                location: "other.xsd".to_string(),
                base_uri: String::new(),
            },
        ]
    );

    v.validate_end_of_attributes();
    v.validate_text("hello");
    v.validate_end_element();
    v.end_validation().ok();
    assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
}

#[test]
fn test_xsi_schema_location_odd_token_count() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root" type="xs:string"/>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    let info = v.validate_attribute(
        "schemaLocation",
        XSI_NS,
        "http://example.com schema.xsd extra",
    );

    assert_eq!(info.validity, SchemaValidity::Invalid);
    assert_eq!(info.validation_attempted, ValidationAttempted::Full);

    // Should still have the first complete pair
    assert_eq!(
        v.schema_location_hints(),
        &[SchemaLocationHint {
            namespace: "http://example.com".to_string(),
            location: "schema.xsd".to_string(),
            base_uri: String::new(),
        }]
    );

    // Should have the cvc-schema-location error
    assert!(
        v.sink
            .errors
            .iter()
            .any(|e| e.constraint == "cvc-schema-location"),
        "expected cvc-schema-location error, got {:?}",
        v.sink.errors
    );

    v.validate_end_of_attributes();
    v.validate_text("hello");
    v.validate_end_element();
    v.end_validation().ok();
}

#[test]
fn test_xsi_type_attribute_schema_info() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root" type="xs:string"/>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    let info = v.validate_attribute("type", XSI_NS, "xs:string");

    // Attribute-level: lexical QName validation — type is xs:QName
    assert_eq!(info.validation_attempted, ValidationAttempted::Full);
    assert_eq!(
        info.schema_type,
        Some(TypeKey::Simple(schema_set.builtin_types().qname))
    );

    v.validate_end_of_attributes();
    v.validate_text("hello");
    v.validate_end_element();
    v.end_validation().ok();
}

#[test]
fn test_xsi_nil_attribute_schema_info() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root" type="xs:string" nillable="true"/>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, Some("true"), &ns);
    let info = v.validate_attribute("nil", XSI_NS, "true");

    assert_eq!(info.validity, SchemaValidity::Valid);
    assert_eq!(info.validation_attempted, ValidationAttempted::Full);
    assert_eq!(
        info.schema_type,
        Some(TypeKey::Simple(schema_set.builtin_types().boolean))
    );

    v.validate_end_of_attributes();
    v.validate_end_element();
    v.end_validation().ok();
    assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
}

#[test]
fn test_xsi_nil_invalid_value() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root" type="xs:string" nillable="true"/>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, Some("maybe"), &ns);
    let info = v.validate_attribute("nil", XSI_NS, "maybe");

    // "maybe" is not a valid xs:boolean
    assert_eq!(info.validity, SchemaValidity::Invalid);
    assert_eq!(info.validation_attempted, ValidationAttempted::Full);
    assert_eq!(
        info.schema_type,
        Some(TypeKey::Simple(schema_set.builtin_types().boolean))
    );

    v.validate_end_of_attributes();
    v.validate_end_element();
    v.end_validation().ok();
}

#[test]
fn test_xsi_attrs_per_attribute_schema_info_returned() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root" type="xs:string"/>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);

    // All four XSI attributes should return non-None schema_type
    let info_type = v.validate_attribute("type", XSI_NS, "xs:string");
    assert!(info_type.schema_type.is_some());

    let info_nil = v.validate_attribute("nil", XSI_NS, "false");
    assert!(info_nil.schema_type.is_some());

    let info_sl = v.validate_attribute("schemaLocation", XSI_NS, "http://ex.com a.xsd");
    assert!(info_sl.schema_type.is_some());

    let info_nnsl = v.validate_attribute("noNamespaceSchemaLocation", XSI_NS, "local.xsd");
    assert!(info_nnsl.schema_type.is_some());

    v.validate_end_of_attributes();
    v.validate_text("hello");
    v.validate_end_element();
    v.end_validation().ok();
}

#[test]
fn test_xsi_attrs_update_parent_validation_attempted() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root" type="xs:string"/>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);

    // Only XSI attributes on this element
    let attr_info = v.validate_attribute("noNamespaceSchemaLocation", XSI_NS, "schema.xsd");
    assert_eq!(attr_info.validation_attempted, ValidationAttempted::Full);

    v.validate_end_of_attributes();
    v.validate_text("hello");

    // End-element should reflect Full validation attempted (element is declared,
    // all attributes fully validated including XSI built-ins).
    let end_info = v.validate_end_element();
    assert_eq!(end_info.validation_attempted, ValidationAttempted::Full);

    v.end_validation().ok();
}

#[test]
fn test_unknown_xsi_attribute_goes_through_wildcard() {
    let schema_set = load_schema(
        r###"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:simpleContent>
                        <xs:extension base="xs:string">
                            <xs:anyAttribute namespace="##other" processContents="lax"/>
                        </xs:extension>
                    </xs:simpleContent>
                </xs:complexType>
            </xs:element>
        </xs:schema>"###,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);

    // Unknown xsi:* attribute should go through wildcard, not get blanket-accepted
    let info = v.validate_attribute("unknownAttr", XSI_NS, "value");
    // With lax wildcard and no declaration, this should be empty/lax-skipped
    assert_eq!(info.validation_attempted, ValidationAttempted::None);

    v.validate_end_of_attributes();
    v.validate_text("hello");
    v.validate_end_element();
    v.end_validation().ok();
}

#[test]
fn test_xsi_attribute_decl_returned_for_all_builtins() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root" type="xs:string"/>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);

    let info_type = v.validate_attribute("type", XSI_NS, "xs:string");
    assert!(
        info_type.attribute_decl.is_some(),
        "xsi:type should have attribute_decl"
    );

    let info_nil = v.validate_attribute("nil", XSI_NS, "false");
    assert!(
        info_nil.attribute_decl.is_some(),
        "xsi:nil should have attribute_decl"
    );

    let info_sl = v.validate_attribute("schemaLocation", XSI_NS, "http://ex.com a.xsd");
    assert!(
        info_sl.attribute_decl.is_some(),
        "xsi:schemaLocation should have attribute_decl"
    );

    let info_nnsl = v.validate_attribute("noNamespaceSchemaLocation", XSI_NS, "a.xsd");
    assert!(
        info_nnsl.attribute_decl.is_some(),
        "xsi:noNamespaceSchemaLocation should have attribute_decl"
    );

    v.validate_end_of_attributes();
    v.validate_text("hello");
    v.validate_end_element();
    v.end_validation().ok();
}

#[test]
fn test_xsi_declarations_visible_through_namespace_lookup() {
    use crate::namespace::table::well_known;

    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root" type="xs:string"/>
        </xs:schema>"#,
    );

    // All four XSI attributes should be discoverable via lookup_attribute
    let type_key =
        schema_set.lookup_attribute(Some(well_known::XSI_NAMESPACE), well_known::XSI_TYPE);
    assert!(type_key.is_some(), "xsi:type should be in namespace table");

    let nil_key = schema_set.lookup_attribute(Some(well_known::XSI_NAMESPACE), well_known::XSI_NIL);
    assert!(nil_key.is_some(), "xsi:nil should be in namespace table");

    let sl_key = schema_set.lookup_attribute(
        Some(well_known::XSI_NAMESPACE),
        well_known::XSI_SCHEMA_LOCATION,
    );
    assert!(
        sl_key.is_some(),
        "xsi:schemaLocation should be in namespace table"
    );

    let nnsl_key = schema_set.lookup_attribute(
        Some(well_known::XSI_NAMESPACE),
        well_known::XSI_NO_NAMESPACE_SCHEMA_LOCATION,
    );
    assert!(
        nnsl_key.is_some(),
        "xsi:noNamespaceSchemaLocation should be in namespace table"
    );
}

#[test]
fn test_xsi_schema_location_inherits_per_element_base_uri() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element name="child" type="xs:string"/>
                    </xs:sequence>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    // Set document-level base URI
    v.set_instance_base_uri("file:///docs/instance.xml");

    // Root element — should inherit document base URI
    v.validate_element("root", "", None, None, &ns);
    v.validate_attribute("noNamespaceSchemaLocation", XSI_NS, "root.xsd");
    v.validate_end_of_attributes();

    // Child element with absolute xml:base override
    v.validate_element("child", "", None, None, &ns);
    v.validate_attribute(
        "base",
        "http://www.w3.org/XML/1998/namespace",
        "http://other.com/schemas/",
    );
    v.validate_attribute("noNamespaceSchemaLocation", XSI_NS, "child.xsd");
    v.validate_end_of_attributes();
    v.validate_text("hello");
    v.validate_end_element();

    v.validate_end_element();
    v.end_validation().ok();

    let hints = v.no_namespace_schema_location_hints();
    assert_eq!(hints.len(), 2);
    // Root hint should have document base URI
    assert_eq!(hints[0].base_uri, "file:///docs/instance.xml");
    // Child hint should have xml:base override
    assert_eq!(hints[1].base_uri, "http://other.com/schemas/");
}

#[test]
fn test_xsi_relative_xml_base_resolved_against_parent() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element name="child" type="xs:string"/>
                    </xs:sequence>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.set_instance_base_uri("http://example.com/docs/instance.xml");

    v.validate_element("root", "", None, None, &ns);
    // Relative xml:base should resolve against inherited base
    v.validate_attribute("base", "http://www.w3.org/XML/1998/namespace", "subdir/");
    v.validate_attribute("noNamespaceSchemaLocation", XSI_NS, "sub.xsd");
    v.validate_end_of_attributes();

    v.validate_element("child", "", None, None, &ns);
    // Another relative xml:base on child — should compose
    v.validate_attribute("base", "http://www.w3.org/XML/1998/namespace", "deeper/");
    v.validate_attribute("noNamespaceSchemaLocation", XSI_NS, "deep.xsd");
    v.validate_end_of_attributes();
    v.validate_text("hello");
    v.validate_end_element();

    v.validate_end_element();
    v.end_validation().ok();

    let hints = v.no_namespace_schema_location_hints();
    assert_eq!(hints.len(), 2);
    // Root: "subdir/" resolved against "http://example.com/docs/instance.xml"
    //   -> "http://example.com/docs/subdir/"
    assert_eq!(hints[0].base_uri, "http://example.com/docs/subdir/");
    // Child: "deeper/" resolved against "http://example.com/docs/subdir/"
    //   -> "http://example.com/docs/subdir/deeper/"
    assert_eq!(hints[1].base_uri, "http://example.com/docs/subdir/deeper/");
}

#[test]
fn test_resolve_base_uri_windows_backslash() {
    // resolve_base_uri should handle Windows-style backslash paths
    let result = super::resolve_base_uri("sub.xsd", r"C:\docs\instance.xml");
    assert_eq!(result, r"C:\docs\sub.xsd");
}

#[test]
fn test_resolve_base_uri_absolute_replaces() {
    let result = super::resolve_base_uri("http://new.com/", "http://old.com/docs/");
    assert_eq!(result, "http://new.com/");
}

#[test]
fn test_resolve_base_uri_empty_inherits() {
    let result = super::resolve_base_uri("", "http://example.com/base/");
    assert_eq!(result, "http://example.com/base/");
}

#[test]
fn test_duplicate_xml_base_does_not_overwrite() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root" type="xs:string"/>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();
    v.set_instance_base_uri("http://example.com/docs/instance.xml");

    v.validate_element("root", "", None, None, &ns);
    // First xml:base — should be applied
    v.validate_attribute(
        "base",
        "http://www.w3.org/XML/1998/namespace",
        "http://first.com/",
    );
    // Duplicate xml:base — should be rejected and NOT overwrite base_uri
    v.validate_attribute(
        "base",
        "http://www.w3.org/XML/1998/namespace",
        "http://second.com/",
    );
    // Hint should use the first (valid) xml:base
    v.validate_attribute("noNamespaceSchemaLocation", XSI_NS, "schema.xsd");
    v.validate_end_of_attributes();
    v.validate_text("hello");
    v.validate_end_element();
    v.end_validation().ok();

    let hints = v.no_namespace_schema_location_hints();
    assert_eq!(hints.len(), 1);
    assert_eq!(
        hints[0].base_uri, "http://first.com/",
        "duplicate xml:base should not overwrite the first"
    );
}

#[test]
fn test_xml_base_after_xsi_hint_rebases_current_element_hints() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root" type="xs:string"/>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();
    v.set_instance_base_uri("http://example.com/docs/instance.xml");

    v.validate_element("root", "", None, None, &ns);
    // Deliberately validate the XSI hint before xml:base.
    v.validate_attribute("noNamespaceSchemaLocation", XSI_NS, "schema.xsd");
    v.validate_attribute("base", "http://www.w3.org/XML/1998/namespace", "subdir/");
    v.validate_end_of_attributes();
    v.validate_text("hello");
    v.validate_end_element();
    v.end_validation().ok();

    let hints = v.no_namespace_schema_location_hints();
    assert_eq!(hints.len(), 1);
    assert_eq!(
        hints[0].base_uri, "http://example.com/docs/subdir/",
        "xml:base should rebase earlier hints on the same element"
    );
}

#[test]
fn test_substitution_group_abstract_head_no_cvc_elt_2() {
    // An abstract head element with a concrete substitution group member.
    // Validating the member should NOT raise cvc-elt.2.
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element ref="head"/>
                    </xs:sequence>
                </xs:complexType>
            </xs:element>
            <xs:element name="head" abstract="true" type="xs:string"/>
            <xs:element name="member" substitutionGroup="head" type="xs:string"/>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());

    v.validate_element("root", "", None, None, &empty_ns_context());
    v.validate_end_of_attributes();
    v.validate_element("member", "", None, None, &empty_ns_context());
    v.validate_end_of_attributes();
    v.validate_text("hello");
    v.validate_end_element();
    v.validate_end_element();
    v.end_validation().ok();

    let cvc_elt_2_errors: Vec<_> = v
        .sink
        .errors
        .iter()
        .filter(|e| e.constraint == "cvc-elt.2")
        .collect();
    assert!(
        cvc_elt_2_errors.is_empty(),
        "substitution group member should not trigger cvc-elt.2, got: {:?}",
        cvc_elt_2_errors
    );
    assert!(
        v.sink.errors.is_empty(),
        "expected no validation errors, got: {:?}",
        v.sink.errors
    );
}

// ---------------------------------------------------------------------------
// Step 7 stabilisation tests
// ---------------------------------------------------------------------------

/// Wildcard ##targetNamespace resolves from the schema type, not the
/// instance element namespace. An unqualified local element with a
/// wildcard `namespace="##targetNamespace"` must accept children in the
/// schema's target namespace even when the parent has no namespace.
#[test]
fn test_wildcard_target_ns_from_schema_type() {
    let xsd = concat!(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema""#,
        r#" targetNamespace="http://example.com/ns""#,
        r#" xmlns:tns="http://example.com/ns">"#,
        r#"<xs:element name="doc"><xs:complexType><xs:sequence>"#,
        r#"<xs:element name="wrapper"><xs:complexType><xs:sequence>"#,
        r#"<xs:any namespace='##targetNamespace'/>"#,
        r#"</xs:sequence></xs:complexType></xs:element>"#,
        r#"</xs:sequence></xs:complexType></xs:element>"#,
        r#"<xs:element name="child" type="xs:string"/>"#,
        r#"</xs:schema>"#,
    );
    let schema_set = load_schema(xsd);

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());

    let mut ns = empty_ns_context();
    // Bind "tns" to the target namespace for the root element.
    let tns_id = schema_set.name_table.add("http://example.com/ns");
    ns.default_ns = Some(tns_id);

    // <tns:doc>
    v.validate_element("doc", "http://example.com/ns", None, None, &ns);
    v.validate_end_of_attributes();

    // <wrapper> — unqualified local element (namespace = "")
    let plain_ns = empty_ns_context();
    v.validate_element("wrapper", "", None, None, &plain_ns);
    v.validate_end_of_attributes();

    // <tns:child/> — should be accepted by ##targetNamespace wildcard
    v.validate_element("child", "http://example.com/ns", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_end_element(); // </tns:child>

    v.validate_end_element(); // </wrapper>
    v.validate_end_element(); // </tns:doc>
    let _ = v.end_validation();

    assert!(
        v.sink.errors.is_empty(),
        "##targetNamespace wildcard should accept element in schema target ns, errors: {:?}",
        v.sink.errors
    );
}

/// Base-type attribute inheritance: a derived type by restriction that
/// does not redeclare an attribute should still inherit it from the base.
#[test]
fn test_inherited_attribute_from_base_type() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:complexType name="Base">
                <xs:attribute name="inherited" type="xs:string"/>
            </xs:complexType>
            <xs:complexType name="Derived">
                <xs:complexContent>
                    <xs:restriction base="Base"/>
                </xs:complexContent>
            </xs:complexType>
            <xs:element name="root" type="Derived"/>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_attribute("inherited", "", "hello");
    v.validate_end_of_attributes();
    v.validate_end_element();
    let _ = v.end_validation();

    assert!(
        v.sink.errors.is_empty(),
        "attribute 'inherited' from base type should be accepted, errors: {:?}",
        v.sink.errors
    );
}

/// Base-type attribute inheritance across multi-level extension chain.
#[test]
fn test_inherited_attribute_through_extension_chain() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:complexType name="A">
                <xs:attribute name="fromA" type="xs:string" use="required"/>
            </xs:complexType>
            <xs:complexType name="B">
                <xs:complexContent>
                    <xs:extension base="A">
                        <xs:attribute name="fromB" type="xs:int"/>
                    </xs:extension>
                </xs:complexContent>
            </xs:complexType>
            <xs:complexType name="C">
                <xs:complexContent>
                    <xs:extension base="B">
                        <xs:attribute name="fromC" type="xs:boolean"/>
                    </xs:extension>
                </xs:complexContent>
            </xs:complexType>
            <xs:element name="root" type="C"/>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_attribute("fromA", "", "base");
    v.validate_attribute("fromB", "", "42");
    v.validate_attribute("fromC", "", "true");
    v.validate_end_of_attributes();
    v.validate_end_element();
    let _ = v.end_validation();

    assert!(
        v.sink.errors.is_empty(),
        "all inherited attributes should be accepted, errors: {:?}",
        v.sink.errors
    );
}

/// Prohibited attribute in derived type must not leak inherited base attr.
#[test]
fn test_prohibited_attribute_overrides_base() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:complexType name="Base">
                <xs:attribute name="forbidden" type="xs:string"/>
                <xs:attribute name="allowed" type="xs:string"/>
            </xs:complexType>
            <xs:complexType name="Derived">
                <xs:complexContent>
                    <xs:restriction base="Base">
                        <xs:attribute name="forbidden" use="prohibited"/>
                    </xs:restriction>
                </xs:complexContent>
            </xs:complexType>
            <xs:element name="root" type="Derived"/>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    // "allowed" should be inherited and accepted
    v.validate_element("root", "", None, None, &ns);
    v.validate_attribute("allowed", "", "ok");
    v.validate_end_of_attributes();
    v.validate_end_element();
    let _ = v.end_validation();
    assert!(
        v.sink.errors.is_empty(),
        "inherited 'allowed' should be accepted"
    );

    // "forbidden" should be rejected
    let mut v2 = validator.start_run(TestSink::new());
    v2.validate_element("root", "", None, None, &ns);
    v2.validate_attribute("forbidden", "", "nope");
    v2.validate_end_of_attributes();
    v2.validate_end_element();
    let _ = v2.end_validation();
    assert!(
        !v2.sink.errors.is_empty(),
        "prohibited 'forbidden' should be rejected"
    );
}

/// Substitution group member with a type that is a union member type
/// of the head element's type should be accepted in a content model.
#[test]
fn test_substitution_group_union_member_type() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:simpleType name="myUnion">
                <xs:union memberTypes="xs:integer xs:boolean"/>
            </xs:simpleType>
            <xs:element name="head" type="myUnion"/>
            <xs:element name="member" substitutionGroup="head" type="xs:integer"/>
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element ref="head"/>
                    </xs:sequence>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    // <member> substitutes for <head> — integer derives from union via 2.2.4
    v.validate_element("member", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_text("42");
    v.validate_end_element();

    v.validate_end_element();
    let _ = v.end_validation();

    assert!(
        v.sink.errors.is_empty(),
        "substitution group member with union-member type should be accepted, errors: {:?}",
        v.sink.errors
    );
}

// ======================================================================
// Fix 5: xsi:type block checking (cvc-elt.4.3)
// ======================================================================

#[test]
fn test_xsi_type_blocked_by_element_block_restriction() {
    // particlesIg003.v: element e2 has block="restriction", xsi:type="ext"
    // where ext extends base which (implicitly) restricts anyType.
    // The restriction step should be blocked.
    let schema_set = load_schema(
        r###"<xsd:schema xmlns:xsd="http://www.w3.org/2001/XMLSchema"
                      targetNamespace="http://test" xmlns:t="http://test"
                      elementFormDefault="qualified">
            <xsd:complexType name="base">
                <xsd:choice>
                    <xsd:element name="e1" block="restriction"/>
                    <xsd:element name="e2" block="restriction"/>
                </xsd:choice>
            </xsd:complexType>
            <xsd:complexType name="testing">
                <xsd:complexContent>
                    <xsd:restriction base="t:base">
                        <xsd:choice>
                            <xsd:element name="e1" block="restriction"/>
                            <xsd:element name="e2" block="restriction"/>
                        </xsd:choice>
                    </xsd:restriction>
                </xsd:complexContent>
            </xsd:complexType>
            <xsd:element name="doc" type="t:testing"/>
            <xsd:complexType name="ext">
                <xsd:complexContent>
                    <xsd:extension base="t:base"/>
                </xsd:complexContent>
            </xsd:complexType>
        </xsd:schema>"###,
    );

    let tns = "http://test";
    let tns_id = schema_set.name_table.get(tns).unwrap();
    let a_prefix = schema_set.name_table.add("a");
    let ns = NamespaceContextSnapshot {
        default_ns: Some(tns_id),
        bindings: vec![(a_prefix, tns_id)],
    };

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());

    // <a:doc>
    v.validate_element("doc", tns, None, None, &ns);
    v.validate_end_of_attributes();

    // <a:e2 xsi:type="a:ext">
    v.validate_element("e2", tns, Some("a:ext"), None, &ns);
    v.validate_end_of_attributes();

    // <a:e1/>
    v.validate_element("e1", tns, None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_end_element();

    // </a:e2>
    v.validate_end_element();
    // </a:doc>
    v.validate_end_element();
    let _ = v.end_validation();

    assert!(
        v.sink.errors.iter().any(|e| e.constraint == "cvc-elt.4.3"),
        "xsi:type should be rejected by element block='restriction': errors={:?}",
        v.sink.errors
    );
}

#[test]
fn test_xsi_type_allowed_when_no_block() {
    // Same schema but without block="restriction" — xsi:type should be accepted
    let schema_set = load_schema(
        r###"<xsd:schema xmlns:xsd="http://www.w3.org/2001/XMLSchema"
                      targetNamespace="http://test" xmlns:t="http://test"
                      elementFormDefault="qualified">
            <xsd:complexType name="base">
                <xsd:choice>
                    <xsd:element name="e1"/>
                    <xsd:element name="e2"/>
                </xsd:choice>
            </xsd:complexType>
            <xsd:complexType name="testing">
                <xsd:complexContent>
                    <xsd:restriction base="t:base">
                        <xsd:choice>
                            <xsd:element name="e1"/>
                            <xsd:element name="e2"/>
                        </xsd:choice>
                    </xsd:restriction>
                </xsd:complexContent>
            </xsd:complexType>
            <xsd:element name="doc" type="t:testing"/>
            <xsd:complexType name="ext">
                <xsd:complexContent>
                    <xsd:extension base="t:base"/>
                </xsd:complexContent>
            </xsd:complexType>
        </xsd:schema>"###,
    );

    let tns = "http://test";
    let tns_id = schema_set.name_table.get(tns).unwrap();
    let a_prefix = schema_set.name_table.add("a");
    let ns = NamespaceContextSnapshot {
        default_ns: Some(tns_id),
        bindings: vec![(a_prefix, tns_id)],
    };

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());

    v.validate_element("doc", tns, None, None, &ns);
    v.validate_end_of_attributes();

    v.validate_element("e2", tns, Some("a:ext"), None, &ns);
    v.validate_end_of_attributes();

    v.validate_element("e1", tns, None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_end_element();

    v.validate_end_element();
    v.validate_end_element();
    let _ = v.end_validation();

    assert!(
        !v.sink.errors.iter().any(|e| e.constraint == "cvc-elt.4.3"),
        "xsi:type should be accepted without block: errors={:?}",
        v.sink.errors
    );
}

// ======================================================================
// Fix 5 Part A+B: is_type_derived_from with exclusion + anyType
// ======================================================================

#[test]
fn test_xsi_type_blocked_when_declared_type_is_anytype() {
    // Element with no explicit type (implicit anyType) and block="restriction".
    // xsi:type specifying a type derived by restriction should be blocked.
    let schema_set = load_schema(
        r###"<xsd:schema xmlns:xsd="http://www.w3.org/2001/XMLSchema"
                      targetNamespace="http://test" xmlns:t="http://test"
                      elementFormDefault="qualified">
            <xsd:complexType name="myType">
                <xsd:sequence>
                    <xsd:element name="child"/>
                </xsd:sequence>
            </xsd:complexType>
            <xsd:element name="doc">
                <xsd:complexType>
                    <xsd:sequence>
                        <xsd:element name="item" block="restriction"/>
                    </xsd:sequence>
                </xsd:complexType>
            </xsd:element>
        </xsd:schema>"###,
    );

    let tns = "http://test";
    let tns_id = schema_set.name_table.get(tns).unwrap();
    let t_prefix = schema_set.name_table.add("t");
    let ns = NamespaceContextSnapshot {
        default_ns: Some(tns_id),
        bindings: vec![(t_prefix, tns_id)],
    };

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());

    v.validate_element("doc", tns, None, None, &ns);
    v.validate_end_of_attributes();

    // <item xsi:type="t:myType"> — myType restricts anyType, should be blocked
    v.validate_element("item", tns, Some("t:myType"), None, &ns);
    v.validate_end_of_attributes();

    v.validate_element("child", tns, None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_end_element();

    v.validate_end_element();
    v.validate_end_element();
    let _ = v.end_validation();

    assert!(
        v.sink.errors.iter().any(|e| e.constraint == "cvc-elt.4.3"),
        "xsi:type with implicit anyType + block='restriction' should fail: errors={:?}",
        v.sink.errors
    );
}

// ---------------------------------------------------------------------------
// Step 12 regressions: ##other wildcard + fixed-value synthesis
// ---------------------------------------------------------------------------

/// XSD 1.0: ##other must reject unqualified (absent-namespace) children
/// even when target namespace is present. (particlesC009/C010 regression)
#[test]
fn test_other_wildcard_rejects_unqualified_child_xsd10() {
    let schema_set = load_schema(
        r###"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
                    targetNamespace="http://test"
                    xmlns:t="http://test"
                    elementFormDefault="qualified">
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:any namespace="##other" processContents="lax"/>
                    </xs:sequence>
                </xs:complexType>
            </xs:element>
        </xs:schema>"###,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());

    let tns = "http://test";
    let ns = NamespaceContextSnapshot {
        default_ns: Some(schema_set.name_table.get(tns).unwrap()),
        bindings: Vec::new(),
    };

    v.validate_element("root", tns, None, None, &ns);
    v.validate_end_of_attributes();

    // Unqualified child <a/> — no namespace, should be rejected by ##other in XSD 1.0
    let empty_ns = empty_ns_context();
    v.validate_element("a", "", None, None, &empty_ns);
    v.validate_end_of_attributes();
    v.validate_end_element();

    v.validate_end_element();
    let _ = v.end_validation();

    assert!(
        !v.sink.errors.is_empty(),
        "##other in XSD 1.0 should reject unqualified (absent-namespace) child, errors={:?}",
        v.sink.errors,
    );
}

/// XSD 1.0: ##other must accept a child from a genuinely other namespace.
#[test]
fn test_other_wildcard_accepts_other_namespace_child_xsd10() {
    let schema_set = load_schema(
        r###"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
                    targetNamespace="http://test"
                    xmlns:t="http://test"
                    elementFormDefault="qualified">
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:any namespace="##other" processContents="lax"/>
                    </xs:sequence>
                </xs:complexType>
            </xs:element>
        </xs:schema>"###,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());

    let tns = "http://test";
    let other = "http://other";
    let tns_id = schema_set.name_table.get(tns).unwrap();
    let ns = NamespaceContextSnapshot {
        default_ns: Some(tns_id),
        bindings: Vec::new(),
    };

    v.validate_element("root", tns, None, None, &ns);
    v.validate_end_of_attributes();

    // Child from http://other — genuinely other, should be accepted
    let empty_ns = empty_ns_context();
    v.validate_element("a", other, None, None, &empty_ns);
    v.validate_end_of_attributes();
    v.validate_end_element();

    v.validate_end_element();
    let _ = v.end_validation();

    assert!(
        v.sink.errors.is_empty(),
        "##other should accept child from a different namespace, errors={:?}",
        v.sink.errors,
    );
}

/// Empty element with fixed value should have the fixed value synthesized.
/// (particlesZ040 regression)
#[test]
fn test_empty_element_fixed_value_synthesis() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element name="e" type="xs:string" fixed="hello"/>
                    </xs:sequence>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    // Empty <e/> — should synthesize fixed value "hello"
    v.validate_element("e", "", None, None, &ns);
    v.validate_end_of_attributes();
    let end_info = v.validate_end_element();

    v.validate_end_element();
    assert!(v.end_validation().is_ok());

    assert_eq!(
        end_info.validity,
        SchemaValidity::Valid,
        "empty element with fixed='hello' should be valid, errors={:?}",
        v.sink.errors,
    );
    assert!(
        v.sink.errors.is_empty(),
        "no cvc-elt.5.2.2 for empty element with fixed value, errors={:?}",
        v.sink.errors,
    );
}

// ===========================================================================
// XSD 1.1 identity-constraint stabilization tests
// ===========================================================================

/// Phase 1: attribute ref="..." must inherit type from the global declaration
/// for ID/IDREF tracking (fixes id012, id016).
#[test]
fn test_attribute_ref_inherits_type_for_id_tracking() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:attribute name="gid" type="xs:ID"/>
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element name="item" maxOccurs="unbounded">
                            <xs:complexType>
                                <xs:attribute ref="gid" use="required"/>
                            </xs:complexType>
                        </xs:element>
                    </xs:sequence>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    v.validate_element("item", "", None, None, &ns);
    v.validate_attribute("gid", "", "id1");
    v.validate_end_of_attributes();
    v.validate_end_element();

    // Duplicate — should trigger cvc-id.2
    v.validate_element("item", "", None, None, &ns);
    v.validate_attribute("gid", "", "id1");
    v.validate_end_of_attributes();
    v.validate_end_element();

    v.validate_end_element();
    v.end_validation().ok();

    assert!(
        v.sink.errors.iter().any(|e| e.constraint == "cvc-id.2"),
        "attribute ref should inherit xs:ID type for duplicate detection, errors={:?}",
        v.sink.errors,
    );
}

/// Phase 2: singleton list value must match atomic value in IC keyref (§3.11.4).
#[cfg(feature = "xsd11")]
#[test]
fn test_ic_singleton_list_equals_atomic() {
    use crate::types::value::{XmlAtomicValue, XmlValue, XmlValueKind};
    use crate::types::XmlTypeCode;
    use crate::validation::identity::KeyFieldValue;

    let atomic = KeyFieldValue {
        string_value: "alpha".to_string(),
        typed_value: Some(XmlValue::new(
            XmlTypeCode::Name,
            XmlValueKind::Atomic(XmlAtomicValue::String("alpha".to_string())),
        )),
    };
    let singleton_list = KeyFieldValue {
        string_value: "alpha".to_string(),
        typed_value: Some(XmlValue::new(
            XmlTypeCode::Name,
            XmlValueKind::List {
                item_type: XmlTypeCode::Name,
                items: vec![XmlAtomicValue::String("alpha".to_string())],
            },
        )),
    };

    assert_eq!(
        atomic, singleton_list,
        "atomic xs:Name must equal singleton list-of-xs:Name for IC"
    );
    assert_eq!(singleton_list, atomic, "equality must be symmetric");
}

/// Phase 3: XSD 1.1 allows same ID value on same element via multiple ID attributes.
#[cfg(feature = "xsd11")]
#[test]
fn test_xsd11_same_id_on_same_element() {
    let schema_set = load_schema_xsd11(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element name="item" maxOccurs="unbounded">
                            <xs:complexType>
                                <xs:attribute name="id1" type="xs:ID"/>
                                <xs:attribute name="id2" type="xs:ID"/>
                            </xs:complexType>
                        </xs:element>
                    </xs:sequence>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    // Same ID value on two attributes of the same element — valid in XSD 1.1
    v.validate_element("item", "", None, None, &ns);
    v.validate_attribute("id1", "", "eee");
    v.validate_attribute("id2", "", "eee");
    v.validate_end_of_attributes();
    v.validate_end_element();

    v.validate_end_element();
    v.end_validation().ok();

    let dup_errors: Vec<_> = v
        .sink
        .errors
        .iter()
        .filter(|e| e.constraint == "cvc-id.2")
        .collect();
    assert!(
        dup_errors.is_empty(),
        "XSD 1.1: same ID on same element must not be a duplicate, errors={:?}",
        dup_errors,
    );
}

/// Phase 3: Same ID on different elements is still an error in XSD 1.1.
#[cfg(feature = "xsd11")]
#[test]
fn test_xsd11_same_id_on_different_elements() {
    let schema_set = load_schema_xsd11(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element name="item" maxOccurs="unbounded">
                            <xs:complexType>
                                <xs:attribute name="id" type="xs:ID"/>
                            </xs:complexType>
                        </xs:element>
                    </xs:sequence>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    v.validate_element("item", "", None, None, &ns);
    v.validate_attribute("id", "", "aaa");
    v.validate_end_of_attributes();
    v.validate_end_element();

    // Same ID on a different element — error
    v.validate_element("item", "", None, None, &ns);
    v.validate_attribute("id", "", "aaa");
    v.validate_end_of_attributes();
    v.validate_end_element();

    v.validate_end_element();
    v.end_validation().ok();

    assert!(
        v.sink.errors.iter().any(|e| e.constraint == "cvc-id.2"),
        "XSD 1.1: same ID on different elements must be duplicate, errors={:?}",
        v.sink.errors,
    );
}

/// Phase 3: ID child element binds to parent (owner-element semantics).
#[cfg(feature = "xsd11")]
#[test]
fn test_xsd11_child_element_id_binds_to_parent() {
    let schema_set = load_schema_xsd11(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element name="node" maxOccurs="unbounded">
                            <xs:complexType>
                                <xs:sequence>
                                    <xs:element name="id" type="xs:ID" minOccurs="0" maxOccurs="unbounded"/>
                                </xs:sequence>
                                <xs:attribute name="a" type="xs:ID"/>
                            </xs:complexType>
                        </xs:element>
                    </xs:sequence>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    // <node a="aaa"><id>zzz</id><id>zzz</id><id>aaa</id></node>
    // All three <id> children bind to <node> (same owner) — must be valid
    v.validate_element("node", "", None, None, &ns);
    v.validate_attribute("a", "", "aaa");
    v.validate_end_of_attributes();

    v.validate_element("id", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_text("zzz");
    v.validate_end_element();

    v.validate_element("id", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_text("zzz");
    v.validate_end_element();

    v.validate_element("id", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_text("aaa");
    v.validate_end_element();

    v.validate_end_element();
    v.validate_end_element();
    v.end_validation().ok();

    let dup_errors: Vec<_> = v
        .sink
        .errors
        .iter()
        .filter(|e| e.constraint == "cvc-id.2")
        .collect();
    assert!(
        dup_errors.is_empty(),
        "child ID elements bind to parent; same owner means no duplicate, errors={:?}",
        dup_errors,
    );
}

/// Phase 4: custom list-of-ID type decomposes per-item for ID tracking.
#[cfg(feature = "xsd11")]
#[test]
fn test_xsd11_list_of_id_per_item_tracking() {
    let schema_set = load_schema_xsd11(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:simpleType name="idList">
                <xs:list itemType="xs:ID"/>
            </xs:simpleType>
            <xs:simpleType name="idrefList">
                <xs:list itemType="xs:IDREF"/>
            </xs:simpleType>
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element name="item" maxOccurs="unbounded">
                            <xs:complexType>
                                <xs:attribute name="ids" type="idList"/>
                                <xs:attribute name="refs" type="idrefList"/>
                            </xs:complexType>
                        </xs:element>
                    </xs:sequence>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    // Define IDs via list attribute
    v.validate_element("item", "", None, None, &ns);
    v.validate_attribute("ids", "", "aaa bbb ccc");
    v.validate_end_of_attributes();
    v.validate_end_element();

    // Reference them via list IDREF
    v.validate_element("item", "", None, None, &ns);
    v.validate_attribute("refs", "", "aaa bbb ccc");
    v.validate_end_of_attributes();
    v.validate_end_element();

    v.validate_end_element();
    v.end_validation().ok();

    let idref_errors: Vec<_> = v
        .sink
        .errors
        .iter()
        .filter(|e| e.constraint == "cvc-id.1")
        .collect();
    assert!(
        idref_errors.is_empty(),
        "list-of-ID items must be tracked individually; all IDREFs should resolve, errors={:?}",
        idref_errors,
    );
}

/// Phase 5: XSD 1.1 identity constraint @ref resolves to the global definition.
#[cfg(feature = "xsd11")]
#[test]
fn test_xsd11_ic_ref_resolves_global() {
    let schema_set = load_schema_xsd11(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element name="section" maxOccurs="unbounded">
                            <xs:complexType>
                                <xs:sequence>
                                    <xs:element name="item" maxOccurs="unbounded">
                                        <xs:complexType>
                                            <xs:attribute name="code" type="xs:string" use="required"/>
                                        </xs:complexType>
                                    </xs:element>
                                </xs:sequence>
                            </xs:complexType>
                            <xs:unique ref="unique-code"/>
                        </xs:element>
                    </xs:sequence>
                </xs:complexType>
                <xs:unique name="unique-code">
                    <xs:selector xpath="item"/>
                    <xs:field xpath="@code"/>
                </xs:unique>
            </xs:element>
        </xs:schema>"#,
    );

    let flags = ValidationFlags::default() | ValidationFlags::PROCESS_IDENTITY_CONSTRAINTS;
    let validator = SchemaValidator::new(&schema_set, flags);
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    // First section with duplicate codes — should violate the unique constraint
    v.validate_element("section", "", None, None, &ns);
    v.validate_end_of_attributes();

    v.validate_element("item", "", None, None, &ns);
    v.validate_attribute("code", "", "A");
    v.validate_end_of_attributes();
    v.validate_end_element();

    v.validate_element("item", "", None, None, &ns);
    v.validate_attribute("code", "", "A");
    v.validate_end_of_attributes();
    v.validate_end_element();

    v.validate_end_element();
    v.validate_end_element();
    v.end_validation().ok();

    assert!(
        v.sink
            .errors
            .iter()
            .any(|e| e.constraint == "cvc-identity-constraint.4.2.2"),
        "IC @ref must resolve and enforce the unique constraint, errors={:?}",
        v.sink.errors,
    );
}

/// Phase 6: ENTITY attribute value must name a declared unparsed entity.
#[test]
fn test_entity_undeclared_reports_error() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:attribute name="pic" type="xs:ENTITY" use="required"/>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    // Set empty entity set — no entities declared
    v.set_unparsed_entities(std::collections::HashSet::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_attribute("pic", "", "mylogo");
    v.validate_end_of_attributes();
    v.validate_end_element();
    v.end_validation().ok();

    assert!(
        v.sink
            .errors
            .iter()
            .any(|e| e.constraint == "cvc-datatype-valid.1.2.1"),
        "undeclared ENTITY must report error, errors={:?}",
        v.sink.errors,
    );
}

/// Phase 6: declared ENTITY value passes validation.
#[test]
fn test_entity_declared_passes() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:attribute name="pic" type="xs:ENTITY" use="required"/>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let mut entities = std::collections::HashSet::new();
    entities.insert("mylogo".to_string());
    v.set_unparsed_entities(entities);
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_attribute("pic", "", "mylogo");
    v.validate_end_of_attributes();
    v.validate_end_element();
    v.end_validation().ok();

    let entity_errors: Vec<_> = v
        .sink
        .errors
        .iter()
        .filter(|e| e.constraint == "cvc-datatype-valid.1.2.1")
        .collect();
    assert!(
        entity_errors.is_empty(),
        "declared ENTITY should pass, errors={:?}",
        entity_errors,
    );
}

// ── Root xsi:type promotion (XSD §5.2.2) ──────────────────────────────────
//
// When the document root has no governing element declaration but the
// instance carries an `xsi:type` attribute, the validator must lax-assess
// against the supplied type rather than rejecting with cvc-elt.1.
// Closes the Sun "two-schema instance" sub-cluster — sunData/{C,S}Type/...

#[test]
fn test_root_xsi_type_simple_type_governs_undeclared_root() {
    // Schema declares only a simpleType; the instance root element is not
    // declared but xsi:type points at the simpleType. Mirrors
    // sunData/SType/ST_name/ST_name00401m.
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
                     targetNamespace="urn:t" xmlns="urn:t">
            <xs:simpleType name="Test">
                <xs:restriction base="xs:string">
                    <xs:pattern value="1|2|3"/>
                </xs:restriction>
            </xs:simpleType>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let tns_prefix = schema_set.name_table.add("t");
    let tns_uri = schema_set.name_table.add("urn:t");
    let ns = NamespaceContextSnapshot {
        default_ns: Some(tns_uri),
        bindings: vec![(tns_prefix, tns_uri)],
    };

    let info = v.validate_element("test", "urn:t", Some("t:Test"), None, &ns);
    assert!(
        info.schema_type.is_some(),
        "xsi:type should supply governing type at root with no element decl"
    );
    assert_eq!(info.type_source, Some(TypeSource::XsiType));

    v.validate_end_of_attributes();
    v.validate_text("2");
    v.validate_end_element();
    v.end_validation().ok();

    let elt1: Vec<_> = v
        .sink
        .errors
        .iter()
        .filter(|e| e.constraint == "cvc-elt.1")
        .collect();
    assert!(
        elt1.is_empty(),
        "no cvc-elt.1 expected when root xsi:type resolves, got: {:?}",
        elt1
    );
    assert!(
        v.sink.errors.is_empty(),
        "instance value '2' matches pattern '1|2|3' — expected zero errors, got: {:?}",
        v.sink.errors
    );
}

#[test]
fn test_root_xsi_type_complex_type_governs_undeclared_root() {
    // Schema declares only a complexType; the instance uses xsi:type to
    // pick it for the otherwise-undeclared root. Mirrors
    // sunData/CType/targetNS/targetNS00101m.
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
                     targetNamespace="urn:t" xmlns="urn:t">
            <xs:complexType name="Test">
                <xs:sequence>
                    <xs:element name="abc" type="xs:string"/>
                </xs:sequence>
            </xs:complexType>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let tns_prefix = schema_set.name_table.add("t");
    let tns_uri = schema_set.name_table.add("urn:t");
    let ns = NamespaceContextSnapshot {
        default_ns: Some(tns_uri),
        bindings: vec![(tns_prefix, tns_uri)],
    };

    let info = v.validate_element("test", "urn:t", Some("t:Test"), None, &ns);
    assert!(
        info.schema_type.is_some(),
        "xsi:type should supply governing complex type at root"
    );
    assert_eq!(info.type_source, Some(TypeSource::XsiType));

    v.validate_end_of_attributes();
    // The complex type's content model expects an unqualified <abc>:
    // the local element is declared with elementFormDefault=unqualified
    // (the default) and no targetNamespace, so its namespace is empty.
    v.validate_element("abc", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_text("hello");
    v.validate_end_element();
    let end_info = v.validate_end_element();
    assert_eq!(
        end_info.validity,
        SchemaValidity::Valid,
        "errors: {:?}",
        v.sink.errors
    );
    v.end_validation().ok();

    let elt1: Vec<_> = v
        .sink
        .errors
        .iter()
        .filter(|e| e.constraint == "cvc-elt.1")
        .collect();
    assert!(
        elt1.is_empty(),
        "no cvc-elt.1 expected when root xsi:type resolves, got: {:?}",
        elt1
    );
    assert!(
        v.sink.errors.is_empty(),
        "no errors expected, got: {:?}",
        v.sink.errors
    );
}

#[test]
fn test_root_xsi_type_unresolved_emits_cvc_elt_1() {
    // xsi:type names a type that is not declared anywhere — the root
    // remains undeclared and cvc-elt.1 must fire (alongside the
    // xsi:type-specific cvc-elt.4.1 diagnostic).
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
                     targetNamespace="urn:t" xmlns="urn:t">
            <xs:simpleType name="Real">
                <xs:restriction base="xs:string"/>
            </xs:simpleType>
        </xs:schema>"#,
    );

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let tns_prefix = schema_set.name_table.add("t");
    let tns_uri = schema_set.name_table.add("urn:t");
    let ns = NamespaceContextSnapshot {
        default_ns: Some(tns_uri),
        bindings: vec![(tns_prefix, tns_uri)],
    };

    let info = v.validate_element("test", "urn:t", Some("t:Imaginary"), None, &ns);
    assert_eq!(info.validity, SchemaValidity::Invalid);
    assert!(
        info.schema_type.is_none(),
        "unresolved xsi:type must not supply a governing type"
    );

    v.validate_end_of_attributes();
    v.validate_end_element();
    v.end_validation().ok();

    let codes: std::collections::HashSet<&str> =
        v.sink.errors.iter().map(|e| e.constraint).collect();
    assert!(
        codes.contains("cvc-elt.1"),
        "expected cvc-elt.1 when root xsi:type fails to resolve, got: {:?}",
        codes
    );
    assert!(
        codes.contains("cvc-elt.4.1"),
        "expected cvc-elt.4.1 (xsi:type unresolved) alongside cvc-elt.1, got: {:?}",
        codes
    );
}

// ---------------------------------------------------------------------------
// PERF_LAZY_PSVI: BUILD_PSVI_TYPED_VALUES opt-out (Phase 2) + facet cache
// correctness (Phase 1). The contract: clearing the flag must never change
// validity — values are still materialized internally wherever a facet,
// fixed/default, ID, QName, or identity constraint needs them; the only
// observable change is that PSVI typed/normalized values may be `None` on the
// returned `SchemaInfo` for nodes whose value nothing else needed.
// ---------------------------------------------------------------------------

/// Flags with PSVI value retention opted out (everything else at defaults).
fn psvi_off_flags() -> ValidationFlags {
    ValidationFlags::default() & !ValidationFlags::BUILD_PSVI_TYPED_VALUES
}

#[test]
fn test_psvi_optout_string_typed_value_none() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root" type="xs:string"/>
        </xs:schema>"#,
    );
    let validator = SchemaValidator::new(&schema_set, psvi_off_flags());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_text("hello world");
    let end_info = v.validate_end_element();

    assert_eq!(end_info.validity, SchemaValidity::Valid);
    assert!(
        end_info.typed_value.is_none(),
        "opt-out: string typed_value should not be retained"
    );
    assert!(
        end_info.normalized_value.is_none(),
        "opt-out: normalized_value should not be retained"
    );
    assert!(v.end_validation().is_ok());
    assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
}

#[test]
fn test_psvi_default_retains_string_typed_value() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root" type="xs:string"/>
        </xs:schema>"#,
    );
    // Default flags keep BUILD_PSVI_TYPED_VALUES — back-compat contract.
    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_text("hello world");
    let end_info = v.validate_end_element();

    assert_eq!(end_info.validity, SchemaValidity::Valid);
    assert!(
        end_info.typed_value.is_some(),
        "default: typed_value must still be retained"
    );
    assert!(v.end_validation().is_ok());
}

#[test]
fn test_psvi_optout_string_maxlength_facet_enforced() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:simpleType>
                    <xs:restriction base="xs:string">
                        <xs:maxLength value="3"/>
                    </xs:restriction>
                </xs:simpleType>
            </xs:element>
        </xs:schema>"#,
    );

    // Violation is rejected even though no value is materialized.
    {
        let validator = SchemaValidator::new(&schema_set, psvi_off_flags());
        let mut v = validator.start_run(TestSink::new());
        let ns = empty_ns_context();
        v.validate_element("root", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_text("toolong");
        let end_info = v.validate_end_element();
        assert_eq!(
            end_info.validity,
            SchemaValidity::Invalid,
            "maxLength must be enforced with PSVI off"
        );
        v.end_validation().ok();
        assert!(!v.sink.errors.is_empty());
    }

    // In-bounds value passes, still without a retained typed value.
    {
        let validator = SchemaValidator::new(&schema_set, psvi_off_flags());
        let mut v = validator.start_run(TestSink::new());
        let ns = empty_ns_context();
        v.validate_element("root", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_text("ok");
        let end_info = v.validate_end_element();
        assert_eq!(end_info.validity, SchemaValidity::Valid);
        assert!(end_info.typed_value.is_none());
        v.end_validation().ok();
        assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
    }
}

#[test]
fn test_psvi_optout_element_fixed_value_enforced() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root" type="xs:string" fixed="yes"/>
        </xs:schema>"#,
    );

    // Mismatch: the fast path is gated off when a fixed value is present, so the
    // value is built and the fixed-value check (cvc-elt.5.2.2) still runs.
    {
        let validator = SchemaValidator::new(&schema_set, psvi_off_flags());
        let mut v = validator.start_run(TestSink::new());
        let ns = empty_ns_context();
        v.validate_element("root", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_text("no");
        let end_info = v.validate_end_element();
        assert_eq!(
            end_info.validity,
            SchemaValidity::Invalid,
            "fixed-value mismatch must fail with PSVI off"
        );
        v.end_validation().ok();
    }

    // Match passes.
    {
        let validator = SchemaValidator::new(&schema_set, psvi_off_flags());
        let mut v = validator.start_run(TestSink::new());
        let ns = empty_ns_context();
        v.validate_element("root", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_text("yes");
        let end_info = v.validate_end_element();
        assert_eq!(end_info.validity, SchemaValidity::Valid);
        v.end_validation().ok();
        assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
    }
}

#[test]
fn test_psvi_optout_attribute_facet_enforced() {
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:attribute name="code">
                        <xs:simpleType>
                            <xs:restriction base="xs:string">
                                <xs:maxLength value="3"/>
                            </xs:restriction>
                        </xs:simpleType>
                    </xs:attribute>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );
    let validator = SchemaValidator::new(&schema_set, psvi_off_flags());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    let attr_info = v.validate_attribute("code", "", "toolong");
    assert_eq!(
        attr_info.validity,
        SchemaValidity::Invalid,
        "attribute maxLength must be enforced with PSVI off"
    );
    assert!(
        attr_info.typed_value.is_none(),
        "opt-out: attribute typed_value should not be retained"
    );
    v.validate_end_of_attributes();
    v.validate_end_element();
    v.end_validation().ok();
}

#[test]
fn test_psvi_optout_id_duplicate_still_detected() {
    // xs:ID is not string-family, so it takes the full (value-building) path
    // even with PSVI off — ID collection must still fire.
    let schema_set = load_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element name="item" maxOccurs="unbounded">
                            <xs:complexType>
                                <xs:attribute name="id" type="xs:ID" use="required"/>
                            </xs:complexType>
                        </xs:element>
                    </xs:sequence>
                </xs:complexType>
            </xs:element>
        </xs:schema>"#,
    );
    let validator = SchemaValidator::new(&schema_set, psvi_off_flags());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();

    v.validate_element("root", "", None, None, &ns);
    v.validate_end_of_attributes();

    v.validate_element("item", "", None, None, &ns);
    v.validate_attribute("id", "", "a1");
    v.validate_end_of_attributes();
    v.validate_end_element();

    v.validate_element("item", "", None, None, &ns);
    v.validate_attribute("id", "", "a1"); // duplicate
    v.validate_end_of_attributes();
    v.validate_end_element();

    v.validate_end_element();
    v.end_validation().ok();

    assert!(
        v.sink.errors.iter().any(|e| e.constraint == "cvc-id.2"),
        "duplicate ID must be detected with PSVI off, got: {:?}",
        v.sink.errors
    );
}

/// Regression guard for the Phase-1 cache: a simple type added to a reused
/// `SchemaSet` *after* an earlier validation pass populated the effective-facet
/// cache must still have its facets enforced. A write-once snapshot cache would
/// miss the new type and silently skip its facets.
#[test]
fn test_effective_facets_cache_correct_after_incremental_load() {
    let mut schema_set = SchemaSet::new();
    load_and_process_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="alpha" type="xs:string"/>
        </xs:schema>"#
            .as_bytes(),
        "a.xsd",
        &mut schema_set,
        None,
    )
    .expect("failed to load schema A");

    // First pass — touches the effective-facets cache.
    {
        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
        let ns = empty_ns_context();
        v.validate_element("alpha", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_text("anything");
        v.validate_end_element();
        v.end_validation().ok();
        assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
    }

    // Add a NEW faceted simple type after the cache was populated.
    load_and_process_schema(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="beta">
                <xs:simpleType>
                    <xs:restriction base="xs:string">
                        <xs:maxLength value="3"/>
                    </xs:restriction>
                </xs:simpleType>
            </xs:element>
        </xs:schema>"#
            .as_bytes(),
        "b.xsd",
        &mut schema_set,
        None,
    )
    .expect("failed to load schema B");

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut v = validator.start_run(TestSink::new());
    let ns = empty_ns_context();
    v.validate_element("beta", "", None, None, &ns);
    v.validate_end_of_attributes();
    v.validate_text("toolong");
    let end_info = v.validate_end_element();
    v.end_validation().ok();

    assert_eq!(
        end_info.validity,
        SchemaValidity::Invalid,
        "maxLength on a type added after the cache was populated must still be enforced"
    );
}
