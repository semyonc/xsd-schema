//! A `fixed` value of a type whose values can be QNames is compared by value.
//!
//! Structures §3.3.4.3 (Element Locally Valid (Element)) clause 5.2.2.2.2:
//! "the ·actual value· of E is equal or identical to D.{value
//! constraint}.{value}"; §3.2.4.3 (Attribute Locally Valid) clause 4: "A's
//! ·actual value· matches the value of the {value constraint}". The {value}
//! is the ·actual value· of the `fixed` attribute where the declaration
//! stands (§3.3.2), so a prefixed name in it is read with the bindings of the
//! schema document.
//!
//! For a type with `xs:QName` values the spelling says little. `q:k` is one
//! expanded name in the schema document and another in an instance that
//! binds `q` elsewhere; and a union of a QName enumeration with `xs:string`
//! reads `q:k` as a QName under one binding and as a string under another.
//! Equal spellings can be two values, and two spellings one value.

use xsd_schema::validation::{
    drive_quick_xml, CollectingValidationSink, SchemaValidator, ValidationFlags,
};
use xsd_schema::{SchemaSet, SchemaSetBuilder, XsdVersion};

const XSI: &str = r#"xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance""#;

/// `QE` is the one QName `{urn:a}k`; `U` is `QE` or `member`. The schema
/// document binds `q` to `urn:a`; the declarations `e`, `a`, `d` and `da`
/// bind it to `urn:b`, so their `q:k` is not in the enumeration: it is the
/// `member` value `q:k`. In `n`, `b`, `p`, `c` and `l` it is the QName. `w`
/// and `wa` have the `member` value written with white space: the string
/// ` q:k `, or the token `q:k`.
fn schema(v11: bool, member: &str) -> SchemaSet {
    let xsd = format!(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:q="urn:a">
  <xs:simpleType name="QE"><xs:restriction base="xs:QName"><xs:enumeration value="q:k"/></xs:restriction></xs:simpleType>
  <xs:simpleType name="U"><xs:union memberTypes="QE {member}"/></xs:simpleType>
  <xs:simpleType name="U2"><xs:restriction base="U"/></xs:simpleType>
  <xs:simpleType name="L"><xs:list itemType="U"/></xs:simpleType>
  <xs:element name="doc"><xs:complexType><xs:sequence>
    <xs:element name="e" type="U" fixed="q:k" xmlns:q="urn:b" minOccurs="0"/>
    <xs:element name="n" type="U" fixed="q:k" minOccurs="0"/>
    <xs:element name="p" type="xs:QName" fixed="q:k" minOccurs="0"/>
    <xs:element name="l" type="L" fixed="q:k x q:k" minOccurs="0"/>
    <xs:element name="d" type="U" default=" q:k " xmlns:q="urn:b" minOccurs="0"/>
    <xs:element name="w" type="U" fixed=" q:k " xmlns:q="urn:b" minOccurs="0"/>
  </xs:sequence>
  <xs:attribute name="a" type="U" fixed="q:k" xmlns:q="urn:b"/>
  <xs:attribute name="wa" type="U" fixed=" q:k " xmlns:q="urn:b"/>
  <xs:attribute name="b" type="U" fixed="q:k"/>
  <xs:attribute name="c" type="xs:QName" fixed="q:k"/>
  <xs:attribute name="da" type="U" default=" q:k " xmlns:q="urn:b"/>
  </xs:complexType></xs:element>
</xs:schema>"#
    );
    let builder = if v11 {
        SchemaSetBuilder::with_version(XsdVersion::V1_1)
    } else {
        SchemaSetBuilder::new()
    };
    builder
        .add_source(&xsd, "file:///fixed_qname_values.xsd")
        .and_then(|builder| builder.compile())
        .expect("the schema compiles")
        .into_schema_set()
}

/// The constraints a streaming run reports for `xml`, in order.
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

const ATTRIBUTE: &str = "cvc-attribute.4";
const ELEMENT: &str = "cvc-elt.5.2.2";

/// Each case with the constraints it violates under XSD 1.1.
fn cases(token: bool) -> Vec<(&'static str, String, Vec<&'static str>)> {
    let both = vec![ATTRIBUTE, ELEMENT];
    vec![
        // The {value} is the member value `q:k`.
        (
            "the QName {urn:a}k, spelled as the literal: another value",
            r#"<doc xmlns:q="urn:a" a="q:k"><e>q:k</e></doc>"#.into(),
            both.clone(),
        ),
        (
            "the QName {urn:a}k with white space: another value",
            r#"<doc xmlns:q="urn:a" a=" q:k "><e> q:k </e></doc>"#.into(),
            both.clone(),
        ),
        (
            "the member value q:k: the same value",
            r#"<doc xmlns:q="urn:c" a="q:k"><e>q:k</e></doc>"#.into(),
            vec![],
        ),
        (
            "the member value with white space: the same token, another string",
            r#"<doc xmlns:q="urn:c" a=" q:k "><e> q:k </e></doc>"#.into(),
            if token { vec![] } else { both.clone() },
        ),
        (
            "the member value q:k where q is not bound: the same value",
            r#"<doc a="q:k"><e>q:k</e></doc>"#.into(),
            vec![],
        ),
        (
            "the QName {urn:a}k under xsi:type",
            format!(r#"<doc xmlns:q="urn:a" {XSI}><e xsi:type="U2">q:k</e></doc>"#),
            vec![ELEMENT],
        ),
        (
            "the member value q:k under xsi:type",
            format!(r#"<doc xmlns:q="urn:c" {XSI}><e xsi:type="U2">q:k</e></doc>"#),
            vec![],
        ),
        // The {value} is the member value written ` q:k `: a string keeps
        // the white space, a token does not. The same bindings as the
        // declaration's, so both are the member's values.
        (
            "member {value} with white space: the literal as written",
            r#"<doc xmlns:q="urn:b" wa=" q:k "><w> q:k </w></doc>"#.into(),
            vec![],
        ),
        (
            "member {value} with white space: without it, another string, the same token",
            r#"<doc xmlns:q="urn:b" wa="q:k"><w>q:k</w></doc>"#.into(),
            if token { vec![] } else { both.clone() },
        ),
        (
            "member {value} with white space: the QName {urn:a}k",
            r#"<doc xmlns:q="urn:a" wa=" q:k "><w> q:k </w></doc>"#.into(),
            both.clone(),
        ),
        // The {value} is the QName {urn:a}k.
        (
            "QName {value}: the same name, spelled as the literal",
            r#"<doc xmlns:q="urn:a" b="q:k" c="q:k"><n>q:k</n><p>q:k</p></doc>"#.into(),
            vec![],
        ),
        (
            "QName {value}: the same name with another prefix",
            r#"<doc xmlns:z="urn:a" b=" z:k " c=" z:k "><n> z:k </n><p> z:k </p></doc>"#.into(),
            vec![],
        ),
        (
            "QName {value}: the member value q:k, spelled as the literal",
            r#"<doc xmlns:q="urn:c" b="q:k"><n>q:k</n></doc>"#.into(),
            both.clone(),
        ),
        (
            "QName {value}: another name, spelled as the literal",
            r#"<doc xmlns:q="urn:c" c="q:k"><p>q:k</p></doc>"#.into(),
            both.clone(),
        ),
        // A list of the union: {value} = ({urn:a}k, x, {urn:a}k).
        (
            "list: the same items",
            r#"<doc xmlns:z="urn:a"><l>z:k  x z:k</l></doc>"#.into(),
            vec![],
        ),
        (
            "list: member values spelled as the QNames",
            r#"<doc xmlns:q="urn:c"><l>q:k x q:k</l></doc>"#.into(),
            vec![ELEMENT],
        ),
    ]
}

#[test]
fn a_fixed_value_with_qname_values_is_compared_by_value() {
    for (member, token) in [("xs:string", false), ("xs:token", true)] {
        let schema_set = schema(true, member);
        for (what, xml, expected) in cases(token) {
            assert_eq!(
                codes(&schema_set, &xml),
                expected,
                "{member}: {what}: {xml}"
            );
        }
    }
}

/// XSD 1.0 compares an attribute as 1.1 does (Attribute Locally Valid clause
/// 4: "The item's ·actual value· must match the value of the {value
/// constraint}"), and an element with "the canonical lexical representation
/// of the {value constraint} value" (Element Locally Valid (Element) clause
/// 5.2.2.2.2), read by the datatype of the item's value: there a QName
/// {value} and a string item match when the canonical form `q:k` is the
/// string, where 1.1 has two values of two datatypes.
#[test]
fn xsd_1_0_compares_an_element_with_the_canonical_form() {
    let by_canonical_form = [
        "QName {value}: the member value q:k, spelled as the literal",
        "list: member values spelled as the QNames",
    ];
    for (member, token) in [("xs:string", false), ("xs:token", true)] {
        let schema_set = schema(false, member);
        for (what, xml, expected) in cases(token) {
            let expected: Vec<&str> = if by_canonical_form.contains(&what) {
                expected
                    .into_iter()
                    .filter(|code| *code != ELEMENT)
                    .collect()
            } else {
                expected
            };
            assert_eq!(
                codes(&schema_set, &xml),
                expected,
                "{member}: {what}: {xml}"
            );
        }
    }
}

/// The typed document holds each value as the member type of the run read
/// it: Datatypes §4.3.6, "the normalization behavior of ·union· types is
/// controlled by the value of whiteSpace on that one of the ·basic members·
/// against which the ·union· is successfully validated". A second validation
/// of the text, without the bindings, would take a prefixed name for the
/// QName member and collapse a string, or keep the white space of a QName.
#[cfg(feature = "xsd11")]
#[test]
fn the_typed_document_keeps_the_white_space_of_the_member_the_run_took() {
    use xsd_schema::document::{
        build_typed_document_with, BufferDocumentOptions, TypedBuildOptions,
    };
    use xsd_schema::validation::NoopHandler;
    use xsd_schema::xpath::{api::XPathExpr, XPathContext};

    let text = |schema_set: &SchemaSet, xml: &str| -> String {
        let validator = SchemaValidator::new(schema_set, ValidationFlags::default());
        let (mut errors, mut warnings) = (Vec::new(), Vec::new());
        let arena = bumpalo::Bump::new();
        let built = {
            let mut runtime = validator.start_run(CollectingValidationSink {
                errors: &mut errors,
                warnings: &mut warnings,
            });
            build_typed_document_with(
                xml.as_bytes(),
                &arena,
                schema_set,
                &mut runtime,
                &mut NoopHandler,
                TypedBuildOptions::default()
                    .with_document_options(BufferDocumentOptions::full())
                    .with_defaults_as_nodes(true),
            )
            .expect("the document builds")
        };
        assert!(errors.is_empty(), "{xml}: {errors:?}");
        let context = XPathContext::new(&schema_set.name_table);
        let nodes = "string-join(for $n in (/doc/*, /doc/@*) \
                     return concat(name($n), '=[', string($n), ']'), ' ')";
        XPathExpr::compile(nodes, &context)
            .expect("the expression compiles")
            .evaluator(&context)
            .run_with_node(built.document.create_navigator())
            .expect("the expression evaluates")
            .to_owned_string()
            .expect("a string")
    };

    for v11 in [false, true] {
        // A string member keeps its white space, in a value and in a default.
        let strings = schema(v11, "xs:string");
        assert_eq!(
            text(&strings, r#"<doc xmlns:q="urn:c"><d> q:k </d></doc>"#),
            "d=[ q:k ] a=[q:k] wa=[ q:k ] b=[ns0:k] c=[ns0:k] da=[ q:k ]",
        );
        assert_eq!(
            text(&strings, r#"<doc xmlns:q="urn:a"><d/></doc>"#),
            "d=[ q:k ] a=[q:k] wa=[ q:k ] b=[q:k] c=[q:k] da=[ q:k ]",
        );
        // The QName member collapses, whatever the prefix.
        assert_eq!(
            text(
                &strings,
                r#"<doc xmlns:z="urn:a" b=" z:k "><n> z:k </n></doc>"#
            ),
            "n=[z:k] b=[z:k] a=[q:k] wa=[ q:k ] c=[z:k] da=[ q:k ]",
        );
        // A token member collapses too.
        let tokens = schema(v11, "xs:token");
        assert_eq!(
            text(
                &tokens,
                r#"<doc xmlns:q="urn:c" a=" q:k "><e> q:k </e><d/></doc>"#
            ),
            "e=[q:k] d=[q:k] a=[q:k] wa=[q:k] b=[ns0:k] c=[ns0:k] da=[q:k]",
        );
    }
}

/// Attribute Use Properties Correct (Structures §3.5.6) clause 2: a use of a
/// declaration with a fixed {value constraint} that has one itself has "the
/// same" value — "identical" in XSD 1.1 (clause 2.2), "must match" in 1.0
/// (au-props-correct.2). The two literals are read each with the bindings
/// where it stands: `q:k` is the QName on the declaration and, where `q` is
/// bound elsewhere, the string member's value on the use.
#[test]
fn a_use_cannot_repeat_a_fixed_qname_as_another_value() {
    let schema = |use_bindings: &str, use_value: &str, v11: bool| {
        let xsd = format!(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:q="urn:a" xmlns:z="urn:a">
  <xs:simpleType name="QE"><xs:restriction base="xs:QName"><xs:enumeration value="q:k"/></xs:restriction></xs:simpleType>
  <xs:simpleType name="U"><xs:union memberTypes="QE xs:string"/></xs:simpleType>
  <xs:attribute name="g" type="U" fixed="q:k"/>
  <xs:element name="doc"><xs:complexType>
    <xs:attribute ref="g" fixed="{use_value}"{use_bindings}/>
  </xs:complexType></xs:element>
</xs:schema>"#
        );
        let builder = if v11 {
            SchemaSetBuilder::with_version(XsdVersion::V1_1)
        } else {
            SchemaSetBuilder::new()
        };
        builder
            .add_source(&xsd, "file:///fixed_qname_use.xsd")
            .and_then(|builder| builder.compile())
            .map(|_| ())
            .map_err(|error| error.to_string())
    };
    for v11 in [false, true] {
        // The same QName, also with another prefix.
        assert_eq!(schema("", "q:k", v11), Ok(()), "{v11}");
        assert_eq!(schema("", "z:k", v11), Ok(()), "{v11}");
        // The string `q:k`, and the QName under a prefix that spells another
        // string: neither is the declaration's value.
        let rebound = schema(r#" xmlns:q="urn:b""#, "q:k", v11);
        assert!(
            rebound
                .as_ref()
                .is_err_and(|e| e.contains("au-props-correct")),
            "{v11}: {rebound:?}"
        );
        let other = schema(r#" xmlns:z="urn:b""#, "z:k", v11);
        assert!(other.is_err(), "{v11}: {other:?}");
    }
}

/// A list with a QName and a dateTime whose seconds have more fraction
/// digits than a value holds: the fixed value is compared with the seconds
/// as written, for an element and for an attribute alike.
#[test]
fn a_fixed_list_with_a_qname_keeps_its_seconds_as_written() {
    let zeros = "0".repeat(29);
    let xsd = format!(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:q="urn:a">
  <xs:simpleType name="QD"><xs:union memberTypes="xs:QName xs:dateTime"/></xs:simpleType>
  <xs:simpleType name="L"><xs:list itemType="QD"/></xs:simpleType>
  <xs:element name="doc"><xs:complexType><xs:sequence>
    <xs:element name="e" type="L" fixed="q:k 2000-01-01T00:00:00.{zeros}1Z" minOccurs="0"/>
  </xs:sequence>
  <xs:attribute name="a" type="L" fixed="q:k 2000-01-01T00:00:00.{zeros}1Z"/>
  </xs:complexType></xs:element>
</xs:schema>"#
    );
    for v11 in [false, true] {
        let builder = if v11 {
            SchemaSetBuilder::with_version(XsdVersion::V1_1)
        } else {
            SchemaSetBuilder::new()
        };
        let schema_set = builder
            .add_source(&xsd, "file:///fixed_qname_list.xsd")
            .and_then(|builder| builder.compile())
            .expect("the schema compiles")
            .into_schema_set();
        let document = |digit: char| {
            let value = format!("z:k 2000-01-01T00:00:00.{zeros}{digit}Z");
            format!(r#"<doc xmlns:z="urn:a" a="{value}"><e>{value}</e></doc>"#)
        };
        assert_eq!(
            codes(&schema_set, &document('1')),
            Vec::<&str>::new(),
            "{v11}"
        );
        assert_eq!(
            codes(&schema_set, &document('2')),
            [ATTRIBUTE, ELEMENT],
            "{v11}"
        );
    }
}
