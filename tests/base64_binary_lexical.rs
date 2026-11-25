//! `xs:base64Binary` takes Base64 wrapped in lines.
//!
//! Datatypes §3.3.16.2: "The ·lexical representations· of base64Binary values
//! are limited to the 65 characters of the Base64 Alphabet … together with
//! the space character (#x20)", by the productions \[27\] to \[38\] — a space
//! may follow each character but the last (\[33\] `B64 ::= B64char #x20?`).
//! And: "Any string compatible with either RFC can occur in an element or
//! attribute validated by this type, because the ·whiteSpace· facet of this
//! type is fixed to collapse".

use xsd_schema::validation::{
    drive_quick_xml, CollectingValidationSink, SchemaValidator, ValidationFlags,
};
use xsd_schema::{SchemaSet, SchemaSetBuilder, XsdVersion};

fn schema(v11: bool) -> SchemaSet {
    let xsd = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:simpleType name="Sixteen"><xs:restriction base="xs:base64Binary"><xs:length value="16"/></xs:restriction></xs:simpleType>
  <xs:simpleType name="One"><xs:restriction base="xs:base64Binary"><xs:enumeration value="YWJj ZGVm Z2hp amts bW5v cA=="/></xs:restriction></xs:simpleType>
  <xs:simpleType name="Lines"><xs:restriction base="xs:base64Binary"><xs:pattern value="[^ ]{12}( [^ ]{1,12})*"/></xs:restriction></xs:simpleType>
  <xs:element name="doc"><xs:complexType><xs:sequence>
    <xs:element name="b" type="xs:base64Binary" minOccurs="0" maxOccurs="unbounded"/>
    <xs:element name="s" type="Sixteen" minOccurs="0" maxOccurs="unbounded"/>
    <xs:element name="o" type="One" minOccurs="0" maxOccurs="unbounded"/>
    <xs:element name="l" type="Lines" minOccurs="0" maxOccurs="unbounded"/>
    <xs:element name="f" type="xs:base64Binary" fixed="YWJjZGVmZ2hpamtsbW5vcA==" minOccurs="0" maxOccurs="unbounded"/>
  </xs:sequence><xs:attribute name="a" type="xs:base64Binary"/></xs:complexType>
  <xs:unique name="u"><xs:selector xpath="b"/><xs:field xpath="."/></xs:unique>
  </xs:element>
</xs:schema>"#;
    let builder = if v11 {
        SchemaSetBuilder::with_version(XsdVersion::V1_1)
    } else {
        SchemaSetBuilder::new()
    };
    builder
        .add_source(xsd, "file:///base64_binary_lexical.xsd")
        .and_then(|builder| builder.compile())
        .expect("the schema compiles")
        .into_schema_set()
}

fn codes(schema_set: &SchemaSet, xml: &str) -> Vec<&'static str> {
    let validator = SchemaValidator::new(schema_set, ValidationFlags::default());
    let (mut errors, mut warnings) = (Vec::new(), Vec::new());
    {
        let mut runtime = validator.start_run(CollectingValidationSink {
            errors: &mut errors,
            warnings: &mut warnings,
        });
        drive_quick_xml(xml.as_bytes(), &mut runtime, schema_set).expect("well-formed");
    }
    errors.iter().map(|error| error.constraint).collect()
}

/// The sixteen octets `abcdefghijklmnop`, in one line and wrapped.
const ONE_LINE: &str = "YWJjZGVmZ2hpamtsbW5vcA==";
const WRAPPED: &str = "\n    YWJjZGVmZ2hp\r\n    amtsbW5vcA==\n  ";

#[test]
fn base64_wrapped_in_lines_is_valid() {
    for v11 in [false, true] {
        let schema_set = schema(v11);
        for value in [
            ONE_LINE,
            WRAPPED,
            "YWJjZGVmZ2hp amtsbW5vcA==",
            "Y W J j Z G V m Z 2 h p a m t s b W 5 v c A = =",
        ] {
            // The built-in type, in an element and in an attribute; the
            // length facet counts the octets; the enumeration and the fixed
            // value are compared as octets.
            let xml = format!(
                "<doc a=\"{value}\"><b>{value}</b><s>{value}</s><o>{value}</o><f>{value}</f></doc>"
            );
            assert_eq!(
                codes(&schema_set, &xml),
                Vec::<&str>::new(),
                "{v11} {value:?}"
            );
        }
    }
}

#[test]
fn what_the_grammar_excludes_stays_invalid() {
    for v11 in [false, true] {
        let schema_set = schema(v11);
        for value in ["YW J", "YQ== YQ==", "YWJ=", "Y R = =", "YW*j", "YW&#xA0;Jj"] {
            let xml = format!("<doc><b>{value}</b></doc>");
            assert_eq!(codes(&schema_set, &xml).len(), 1, "{v11} {value:?}");
        }
        // Fifteen octets are not sixteen, wrapped or not.
        let xml = "<doc><s>YWJjZGVmZ2hp amtsbW5v</s></doc>";
        assert_eq!(codes(&schema_set, xml).len(), 1, "{v11}");
    }
}

/// A pattern is matched against the collapsed literal, spaces included
/// (Datatypes §4.3.4.4), and two spellings of the same octets are one value
/// for an identity constraint.
#[test]
fn the_spaces_are_lexical_only() {
    for v11 in [false, true] {
        let schema_set = schema(v11);
        let lines = format!("<doc><l>{WRAPPED}</l></doc>");
        assert_eq!(codes(&schema_set, &lines), Vec::<&str>::new(), "{v11}");
        let one_line = format!("<doc><l>{ONE_LINE}</l></doc>");
        assert_eq!(codes(&schema_set, &one_line).len(), 1, "{v11}");
        let twice = format!("<doc><b>{ONE_LINE}</b><b>{WRAPPED}</b></doc>");
        assert_eq!(
            codes(&schema_set, &twice),
            ["cvc-identity-constraint.4.2.2"],
            "{v11}"
        );
    }
}
