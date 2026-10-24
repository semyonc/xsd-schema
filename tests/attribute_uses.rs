//! The {attribute uses} of a complex type, as validation sees them: required
//! attributes (cvc-complex-type.4) and defaulted attributes in identity
//! constraints and ID/IDREF checks, when the uses come from attribute groups
//! and base types.
//!
//! Structures 1.1 §3.4.2.4 (Mapping Rule for Attribute Uses Property): the
//! {attribute uses} are "1 The set of attribute uses corresponding to the
//! <attribute> [children], if any. 2 The {attribute uses} of the attribute
//! groups … 3 The attribute uses "inherited" from the {base type definition}
//! T" — for an extension (3.1) all of T's, for a restriction (3.2) all
//! "with the exception of those with an {attribute declaration} whose
//! expanded name is one of the following: 3.2.1 the expanded name of the
//! {attribute declaration} of an attribute use which has already been
//! included in the set, following the rules in clause 1 or clause 2 above;
//! 3.2.2 the expanded name of the {attribute declaration} of what would have
//! been an attribute use corresponding to an <attribute> [child], if the
//! <attribute> had not had use = prohibited." Its note: `prohibited` "serves to
//! prevent inheritance of an identically named attribute use from the {base
//! type definition}. Such an <attribute> does not correspond to any
//! component", and elsewhere — "in complex type extensions or named model
//! group definitions" — "the <attribute> element is simply ignored". XSD 1.0
//! §3.4.2 has the same rule (the prohibition clause names "the relevant
//! <attribute> among the [children] of <restriction>"); an attribute group's
//! prohibited `<attribute>` "corresponds to nothing at all" (XSD 1.0 §3.2.2).
//!
//! Every case runs in XSD 1.0 and XSD 1.1 mode, on streaming validation with
//! and without PSVI typed values, on the navigator driver and — with the
//! `xsd11` feature — on the buffer-document driver and the typed build.

use xsd_schema::validation::{
    drive_navigator, drive_quick_xml, CollectingValidationSink, SchemaValidator, SchemaValidity,
    ValidationError, ValidationFlags,
};
use xsd_schema::{RoXmlNavigator, SchemaSet, SchemaSetBuilder, XsdVersion};

use SchemaValidity::{Invalid, Valid};

const VERSIONS: [XsdVersion; 2] = [XsdVersion::V1_0, XsdVersion::V1_1];

// ── Harness ───────────────────────────────────────────────────────────────

fn schema(version: XsdVersion, body: &str) -> SchemaSet {
    let xsd =
        format!(r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">{body}</xs:schema>"#);
    SchemaSetBuilder::with_version(version)
        .add_source(&xsd, "file:///attribute_uses.xsd")
        .expect("schema source accepted")
        .compile()
        .unwrap_or_else(|e| panic!("{version:?}: schema compiles: {e}"))
        .into_schema_set()
}

fn validator(ss: &SchemaSet, psvi: bool) -> SchemaValidator<'_> {
    let mut flags = ValidationFlags::default() | ValidationFlags::PROCESS_IDENTITY_CONSTRAINTS;
    if !psvi {
        // The lexical fast paths (no typed values) must reach the same verdicts.
        flags &= !ValidationFlags::BUILD_PSVI_TYPED_VALUES;
    }
    SchemaValidator::new(ss, flags)
}

/// Root validity and reported constraint codes from one driver.
type Outcome = (Option<SchemaValidity>, Vec<String>);

fn codes(errors: &[ValidationError]) -> Vec<String> {
    errors.iter().map(|e| e.constraint.to_string()).collect()
}

fn stream(ss: &SchemaSet, xml: &str, psvi: bool) -> Outcome {
    let v = validator(ss, psvi);
    let (mut errors, mut warnings) = (Vec::new(), Vec::new());
    let validity = {
        let mut rt = v.start_run(CollectingValidationSink {
            errors: &mut errors,
            warnings: &mut warnings,
        });
        drive_quick_xml(xml.as_bytes(), &mut rt, ss)
            .expect("stream drive completes")
            .root_validity
    };
    (validity, codes(&errors))
}

fn dom(ss: &SchemaSet, xml: &str) -> Outcome {
    let v = validator(ss, true);
    let doc = roxmltree::Document::parse(xml).expect("well-formed instance");
    let nav = RoXmlNavigator::new(&doc);
    let (mut errors, mut warnings) = (Vec::new(), Vec::new());
    let validity = {
        let mut rt = v.start_run(CollectingValidationSink {
            errors: &mut errors,
            warnings: &mut warnings,
        });
        drive_navigator(&nav, &mut rt, ss)
            .expect("navigator drive completes")
            .root_validity
    };
    (validity, codes(&errors))
}

#[cfg(feature = "xsd11")]
fn buffer(ss: &SchemaSet, xml: &str) -> Outcome {
    use xsd_schema::document::{BufferDocument, BufferDocumentOptions};
    use xsd_schema::validation::drive_buffer_document;
    let v = validator(ss, true);
    let arena = bumpalo::Bump::new();
    let doc = BufferDocument::from_reader(
        xml.as_bytes(),
        &arena,
        &ss.name_table,
        BufferDocumentOptions::default(),
        None,
    )
    .expect("buffer build");
    let (mut errors, mut warnings) = (Vec::new(), Vec::new());
    let validity = {
        let mut rt = v.start_run(CollectingValidationSink {
            errors: &mut errors,
            warnings: &mut warnings,
        });
        drive_buffer_document(&doc, &mut rt, ss)
            .expect("buffer drive completes")
            .root_validity
    };
    (validity, codes(&errors))
}

#[cfg(feature = "xsd11")]
fn typed_build(ss: &SchemaSet, xml: &str) -> Outcome {
    use xsd_schema::document::{build_typed_document_with, TypedBuildOptions};
    use xsd_schema::validation::NoopHandler;
    let v = validator(ss, true);
    let arena = bumpalo::Bump::new();
    let (mut errors, mut warnings) = (Vec::new(), Vec::new());
    let validity = {
        let mut rt = v.start_run(CollectingValidationSink {
            errors: &mut errors,
            warnings: &mut warnings,
        });
        let built = build_typed_document_with(
            xml.as_bytes(),
            &arena,
            ss,
            &mut rt,
            &mut NoopHandler,
            TypedBuildOptions::default(),
        )
        .expect("typed build completes");
        rt.end_validation().expect("end_validation");
        built.outcome.root_validity
    };
    (validity, codes(&errors))
}

/// Every driver: the root's `[validity]` is `expected`, and an error whose
/// constraint code starts with `code` was reported (`None`: no error at all).
fn check(ss: &SchemaSet, label: &str, xml: &str, expected: SchemaValidity, code: Option<&str>) {
    let runs = [
        ("stream", stream(ss, xml, true)),
        ("stream/no-psvi", stream(ss, xml, false)),
        ("navigator", dom(ss, xml)),
    ];
    #[cfg(feature = "xsd11")]
    let runs = runs.into_iter().chain([
        ("buffer", buffer(ss, xml)),
        ("typed build", typed_build(ss, xml)),
    ]);
    for (driver, (validity, errors)) in runs {
        assert_eq!(
            validity,
            Some(expected),
            "{label} {driver}: root [validity] of {xml} (errors {errors:?})"
        );
        match code {
            Some(code) => assert!(
                errors.iter().any(|e| e.starts_with(code)),
                "{label} {driver}: expected a {code} error for {xml}, got {errors:?}"
            ),
            None => assert!(
                errors.is_empty(),
                "{label} {driver}: {xml}: errors {errors:?}"
            ),
        }
    }
}

/// `check` for every version of `body`.
fn check_all(body: &str, cases: &[(&str, SchemaValidity, Option<&str>)]) {
    for version in VERSIONS {
        let ss = schema(version, body);
        for (xml, expected, code) in cases {
            check(&ss, &format!("{version:?}"), xml, *expected, *code);
        }
    }
}

// ── C78: required attributes and prohibitions ─────────────────────────────

/// A base type with a required attribute `r`, and a group that prohibits `r`.
const REQUIRED_BASE: &str = r#"
  <xs:complexType name="B"><xs:attribute name="r" type="xs:int" use="required"/></xs:complexType>
  <xs:attributeGroup name="prohibitsR"><xs:attribute name="r" use="prohibited"/></xs:attributeGroup>"#;

/// An extension's prohibited `<attribute>` is "simply ignored" (§3.4.2.4
/// note; clause 3.1 inherits every base use): `r` stays required — and
/// allowed.
#[test]
fn an_extension_prohibition_keeps_a_required_base_attribute() {
    let body = format!(
        r#"{REQUIRED_BASE}
  <xs:complexType name="E"><xs:complexContent><xs:extension base="B">
    <xs:attribute name="r" use="prohibited"/>
  </xs:extension></xs:complexContent></xs:complexType>
  <xs:element name="e" type="E"/>"#
    );
    check_all(
        &body,
        &[
            ("<e/>", Invalid, Some("cvc-complex-type.4")),
            ("<e r='1'/>", Valid, None),
            ("<e r='x'/>", Invalid, Some("cvc-datatype-valid")),
        ],
    );
}

/// An attribute group's prohibited `<attribute>` is no attribute use (XSD 1.0
/// §3.2.2 "corresponds to nothing at all"; W3C Bugzilla #4043): the base's
/// required `r` is inherited by the extension. (The same group in a
/// restriction of `B` is refused at schema construction today —
/// `derivation-ok-restriction` reads the group's prohibition as one; a
/// separate defect, not exercised here.)
#[test]
fn a_group_prohibition_keeps_a_required_base_attribute() {
    let body = format!(
        r#"{REQUIRED_BASE}
  <xs:complexType name="D"><xs:complexContent><xs:extension base="B">
    <xs:attributeGroup ref="prohibitsR"/>
  </xs:extension></xs:complexContent></xs:complexType>
  <xs:element name="d" type="D"/>"#
    );
    check_all(
        &body,
        &[
            ("<d/>", Invalid, Some("cvc-complex-type.4")),
            ("<d r='1'/>", Valid, None),
        ],
    );
}

/// Two levels: an extension in the middle prohibits `r` (ignored), a
/// restriction below it inherits `r` from the extension, which inherited it
/// from `B`.
#[test]
fn a_prohibition_in_a_middle_extension_does_not_stop_inheritance() {
    let body = format!(
        r#"{REQUIRED_BASE}
  <xs:complexType name="E"><xs:complexContent><xs:extension base="B">
    <xs:attribute name="r" use="prohibited"/>
    <xs:attribute name="o" type="xs:string"/>
  </xs:extension></xs:complexContent></xs:complexType>
  <xs:complexType name="R"><xs:complexContent><xs:restriction base="E">
    <xs:attribute name="o" type="xs:string"/>
  </xs:restriction></xs:complexContent></xs:complexType>
  <xs:element name="r" type="R"/>"#
    );
    check_all(
        &body,
        &[
            ("<r/>", Invalid, Some("cvc-complex-type.4")),
            ("<r r='1'/>", Valid, None),
        ],
    );
}

/// Guards: what a prohibition does decide. A restriction's own prohibited
/// `<attribute>` keeps an optional base use out (clause 3.2.2): it is no
/// longer allowed (cvc-complex-type.3.2.2) and not required; a restriction's
/// own required use of a name is required.
#[test]
fn a_restriction_prohibition_still_removes_the_base_use() {
    let body = r#"
  <xs:complexType name="B">
    <xs:attribute name="p" type="xs:int"/>
    <xs:attribute name="q" type="xs:int"/>
  </xs:complexType>
  <xs:complexType name="R"><xs:complexContent><xs:restriction base="B">
    <xs:attribute name="p" use="prohibited"/>
    <xs:attribute name="q" type="xs:int" use="required"/>
  </xs:restriction></xs:complexContent></xs:complexType>
  <xs:element name="r" type="R"/>"#;
    check_all(
        body,
        &[
            ("<r q='1'/>", Valid, None),
            ("<r/>", Invalid, Some("cvc-complex-type.4")),
            ("<r q='1' p='2'/>", Invalid, Some("cvc-complex-type.3.2.2")),
        ],
    );
}

// ── C75: defaulted attributes in identity constraints and ID/IDREF ────────

/// `root` holds `e` children of type `{e_type}` under a `<xs:unique>` on
/// `@a` (and `k` children under a `<xs:key>` on `@a`); `body` declares the
/// types.
fn constrained(body: &str, e_type: &str) -> String {
    format!(
        r#"{body}
  <xs:element name="root">
    <xs:complexType><xs:sequence>
      <xs:element name="e" type="{e_type}" minOccurs="0" maxOccurs="unbounded"/>
      <xs:element name="k" type="{e_type}" minOccurs="0" maxOccurs="unbounded"/>
    </xs:sequence></xs:complexType>
    <xs:unique name="u"><xs:selector xpath="e"/><xs:field xpath="@a"/></xs:unique>
    <xs:key name="key"><xs:selector xpath="k"/><xs:field xpath="@a"/></xs:key>
  </xs:element>"#
    )
}

/// Structures §3.11.4 (Identity-constraint Satisfied): "The use of [schema
/// actual value] in the definition of ·key sequence· above means that default
/// or fixed value constraints may play a part in ·key sequence·s." A
/// defaulted attribute joins E.[attributes] (§3.4.5.1), wherever its
/// attribute use comes from: two `e` without `a` share the default — a
/// duplicate under `<xs:unique>` — and one `k` without `a` has its key field.
fn the_default_is_a_key_value(body: &str, e_type: &str) {
    check_all(
        &constrained(body, e_type),
        &[
            (
                "<root><e/><e/></root>",
                Invalid,
                Some("cvc-identity-constraint.4.2.2"),
            ),
            ("<root><e/><e a='other'/></root>", Valid, None),
            ("<root><k/></root>", Valid, None),
        ],
    );
}

/// The guard: a default declared in the type itself already counted.
#[test]
fn a_default_of_the_type_itself_is_a_key_value() {
    the_default_is_a_key_value(
        r#"<xs:complexType name="T"><xs:attribute name="a" type="xs:string" default="d"/></xs:complexType>"#,
        "T",
    );
}

/// Clause 2 of §3.4.2.4: the {attribute uses} of the type's attribute groups,
/// a nested group included.
#[test]
fn a_default_from_an_attribute_group_is_a_key_value() {
    let body = r#"
  <xs:attributeGroup name="inner"><xs:attribute name="a" type="xs:string" default="d"/></xs:attributeGroup>
  <xs:attributeGroup name="outer"><xs:attributeGroup ref="inner"/></xs:attributeGroup>
  <xs:complexType name="T"><xs:attributeGroup ref="outer"/></xs:complexType>"#;
    the_default_is_a_key_value(body, "T");
}

/// Clause 3: uses inherited from the base type — by extension, and by a
/// restriction that does not mention `a`, two levels down.
#[test]
fn a_default_inherited_from_a_base_type_is_a_key_value() {
    let base = r#"
  <xs:attribute name="a" type="xs:string" default="d"/>
  <xs:complexType name="B"><xs:attribute ref="a"/></xs:complexType>
  <xs:complexType name="E"><xs:complexContent><xs:extension base="B">
    <xs:attribute name="x" type="xs:string"/>
  </xs:extension></xs:complexContent></xs:complexType>
  <xs:complexType name="R"><xs:complexContent><xs:restriction base="E">
    <xs:attribute name="x" type="xs:string"/>
  </xs:restriction></xs:complexContent></xs:complexType>"#;
    the_default_is_a_key_value(base, "E");
    the_default_is_a_key_value(base, "R");
}

/// Prohibitions: a restriction's own prohibited `<attribute>` removes the
/// inherited use and its default (clause 3.2.2) — two `e` without `a` are not
/// duplicates, and a `k` without `a` lacks its key field; an extension's, or
/// an attribute group's, prohibition does not (the default still counts).
#[test]
fn only_a_restriction_prohibition_removes_an_inherited_default() {
    let base = r#"
  <xs:complexType name="B"><xs:attribute name="a" type="xs:string" default="d"/></xs:complexType>
  <xs:attributeGroup name="prohibitsA"><xs:attribute name="a" use="prohibited"/></xs:attributeGroup>
  <xs:complexType name="R"><xs:complexContent><xs:restriction base="B">
    <xs:attribute name="a" use="prohibited"/>
  </xs:restriction></xs:complexContent></xs:complexType>
  <xs:complexType name="E"><xs:complexContent><xs:extension base="B">
    <xs:attribute name="a" use="prohibited"/>
  </xs:extension></xs:complexContent></xs:complexType>
  <xs:complexType name="G"><xs:complexContent><xs:restriction base="B">
    <xs:attributeGroup ref="prohibitsA"/>
  </xs:restriction></xs:complexContent></xs:complexType>"#;
    check_all(
        &constrained(base, "R"),
        &[
            ("<root><e/><e/></root>", Valid, None),
            (
                "<root><k/></root>",
                Invalid,
                Some("cvc-identity-constraint.4.2.1"),
            ),
        ],
    );
    the_default_is_a_key_value(base, "E");
    the_default_is_a_key_value(base, "G");
}

/// ID/IDREF (§3.17.5.2, Validation Root Valid (ID/IDREF) §3.3.4.5): a
/// defaulted IDREF from a base type that names no ID leaves the binding empty
/// (`cvc-id.1`); one that names an ID resolves.
#[test]
fn a_defaulted_idref_from_a_base_type_must_resolve() {
    let body = r#"
  <xs:complexType name="B"><xs:attribute name="to" type="xs:IDREF" default="target"/></xs:complexType>
  <xs:complexType name="E"><xs:complexContent><xs:extension base="B"/></xs:complexContent></xs:complexType>
  <xs:element name="root"><xs:complexType><xs:sequence>
    <xs:element name="ref" type="E" maxOccurs="unbounded"/>
    <xs:element name="id" minOccurs="0"><xs:complexType>
      <xs:attribute name="id" type="xs:ID"/>
    </xs:complexType></xs:element>
  </xs:sequence></xs:complexType></xs:element>"#;
    check_all(
        body,
        &[
            ("<root><ref/></root>", Invalid, Some("cvc-id.1")),
            ("<root><ref/><id id='target'/></root>", Valid, None),
        ],
    );
}

/// XSD 1.1 (an `xs:ID` attribute may have a value constraint; XSD 1.0's
/// a-props-correct.3 forbade it): a defaulted ID from an attribute group, on
/// two elements, is a duplicate (`cvc-id.2`).
#[test]
fn a_defaulted_id_from_an_attribute_group_is_counted() {
    let body = r#"
  <xs:attributeGroup name="ids"><xs:attribute name="id" type="xs:ID" default="same"/></xs:attributeGroup>
  <xs:complexType name="T"><xs:attributeGroup ref="ids"/></xs:complexType>
  <xs:element name="root"><xs:complexType><xs:sequence>
    <xs:element name="e" type="T" maxOccurs="unbounded"/>
  </xs:sequence></xs:complexType></xs:element>"#;
    let ss = schema(XsdVersion::V1_1, body);
    check(
        &ss,
        "V1_1",
        "<root><e/><e/></root>",
        Invalid,
        Some("cvc-id.2"),
    );
    check(&ss, "V1_1", "<root><e/><e id='other'/></root>", Valid, None);
}

/// C78, the attribute lookup: a prohibited `<attribute>` beside an attribute
/// group that uses the same attribute is no attribute use and keeps nothing
/// out of the type's own uses (it only stops inheritance in a restriction) —
/// the group's `a` governs `a` (required, typed `xs:int`).
#[test]
fn a_prohibited_attribute_beside_a_group_does_not_hide_the_group_use() {
    let body = r#"
  <xs:attribute name="a" type="xs:int"/>
  <xs:attributeGroup name="g"><xs:attribute ref="a" use="required"/></xs:attributeGroup>
  <xs:complexType name="T">
    <xs:attribute ref="a" use="prohibited"/>
    <xs:attributeGroup ref="g"/>
  </xs:complexType>
  <xs:element name="e" type="T"/>"#;
    check_all(
        body,
        &[
            ("<e a='1'/>", Valid, None),
            ("<e a='x'/>", Invalid, Some("cvc-datatype-valid")),
            ("<e/>", Invalid, Some("cvc-complex-type.4")),
        ],
    );
}

// ── Fix round: a default's value in its type; the use's own value constraint ──

/// Key sequences hold [schema actual value]s compared "pairwise equal or
/// identical, as defined by Equality and Identity in [XML Schema: Datatypes]"
/// (Structures §3.11.4 clause 4.1); Datatypes §2.2.2: "values from different
/// primitive data spaces are made artificially unequal even if they might
/// otherwise be considered equal". A defaulted attribute's value is its
/// declaration's typed value, as for an attribute the instance carries.
/// `I` takes `a` (`xs:integer`, default `1`) from a group, `I2` inherits
/// `a` (`xs:integer`, default `01`), `S` inherits `a` (`xs:string`, default
/// `1`), `S2` takes it (`xs:string`, `1`) from a group.
const TYPED_DEFAULTS: &str = r#"
  <xs:attributeGroup name="intA"><xs:attribute name="a" type="xs:integer" default="1"/></xs:attributeGroup>
  <xs:attributeGroup name="strA"><xs:attribute name="a" type="xs:string" default="1"/></xs:attributeGroup>
  <xs:complexType name="IntBase"><xs:attribute name="a" type="xs:integer" default="01"/></xs:complexType>
  <xs:complexType name="StrBase"><xs:attribute name="a" type="xs:string" default="1"/></xs:complexType>
  <xs:complexType name="I"><xs:attributeGroup ref="intA"/></xs:complexType>
  <xs:complexType name="I2"><xs:complexContent><xs:extension base="IntBase"/></xs:complexContent></xs:complexType>
  <xs:complexType name="S"><xs:complexContent><xs:extension base="StrBase"/></xs:complexContent></xs:complexType>
  <xs:complexType name="S2"><xs:attributeGroup ref="strA"/></xs:complexType>
  <xs:complexType name="DI"><xs:attribute name="a" type="xs:integer" default="1"/></xs:complexType>
  <xs:complexType name="DS"><xs:attribute name="a" type="xs:string" default="1"/></xs:complexType>
  <xs:element name="root">
    <xs:complexType><xs:choice minOccurs="0" maxOccurs="unbounded">
      <xs:element name="i" type="I"/>
      <xs:element name="i2" type="I2"/>
      <xs:element name="s" type="S"/>
      <xs:element name="s2" type="S2"/>
      <xs:element name="di" type="DI"/>
      <xs:element name="ds" type="DS"/>
    </xs:choice></xs:complexType>
    <xs:unique name="u"><xs:selector xpath="*"/><xs:field xpath="@a"/></xs:unique>
  </xs:element>"#;

/// The reported case: an integer default `1` and a string default `"1"` are
/// different values — a group default against an inherited one, both ways.
#[test]
fn defaults_of_different_types_are_different_key_values() {
    check_all(
        TYPED_DEFAULTS,
        &[
            ("<root><i/><s/></root>", Valid, None),
            ("<root><i2/><s2/></root>", Valid, None),
            // A default against an attribute the instance carries.
            ("<root><i/><s a='1'/></root>", Valid, None),
            ("<root><s/><i a='1'/></root>", Valid, None),
        ],
    );
}

/// Values of one type still collide — by value, not by lexical form: the
/// group's `1` and the inherited `01` are the same integer.
#[test]
fn defaults_of_one_type_collide_by_value() {
    check_all(
        TYPED_DEFAULTS,
        &[
            (
                "<root><i/><i2/></root>",
                Invalid,
                Some("cvc-identity-constraint"),
            ),
            (
                "<root><i/><i/></root>",
                Invalid,
                Some("cvc-identity-constraint"),
            ),
            (
                "<root><s/><s2/></root>",
                Invalid,
                Some("cvc-identity-constraint"),
            ),
            (
                "<root><i2/><i a='1'/></root>",
                Invalid,
                Some("cvc-identity-constraint"),
            ),
        ],
    );
}

/// The type's own defaults compare by value too (they reached the identity
/// constraint before C75, as strings).
#[test]
fn own_defaults_of_different_types_are_different_key_values() {
    check_all(
        TYPED_DEFAULTS,
        &[
            ("<root><di/><ds/></root>", Valid, None),
            (
                "<root><di/><i2/></root>",
                Invalid,
                Some("cvc-identity-constraint"),
            ),
        ],
    );
}

/// An `xs:QName` default is read with the namespace bindings in scope at its
/// declaration (Datatypes §3.3.18: the lexical mapping "depends on the
/// namespace bindings in scope where the literal occurs"), not the instance
/// element's: `p:x` with `p` bound to `urn:a` there equals the instance's
/// `z:x` with `z` bound to `urn:a`, and differs from an instance `p:x` with `p`
/// bound elsewhere.
#[test]
fn a_qname_default_is_read_with_its_declaration_bindings() {
    let body = r#"
  <xs:complexType name="QB">
    <xs:attribute name="a" type="xs:QName" default="p:x" xmlns:p="urn:a"/>
  </xs:complexType>
  <xs:complexType name="Q"><xs:complexContent><xs:extension base="QB"/></xs:complexContent></xs:complexType>
  <xs:element name="root">
    <xs:complexType><xs:sequence>
      <xs:element name="q" type="Q" maxOccurs="unbounded"/>
    </xs:sequence></xs:complexType>
    <xs:unique name="u"><xs:selector xpath="q"/><xs:field xpath="@a"/></xs:unique>
  </xs:element>"#;
    check_all(
        body,
        &[
            (
                "<root><q/><q xmlns:z='urn:a' a='z:x'/></root>",
                Invalid,
                Some("cvc-identity-constraint"),
            ),
            ("<root><q/><q xmlns:p='urn:b' a='p:x'/></root>", Valid, None),
        ],
    );
}

/// The ·effective value constraint· of an attribute use "is U.{value
/// constraint}, if present, otherwise U.{attribute declaration}.{value
/// constraint}" (Structures §3.4.4.2 definition). The global `to` declares
/// `default="old"`; a reference in a group with `fixed="new"` makes the
/// defaulted IDREF `new`, one with `default="mid"` makes it `mid`, and one
/// with no constraint takes the declaration's `old` — in a group, a direct
/// reference and an inherited use alike.
#[test]
fn a_reference_s_own_value_constraint_wins_over_the_declaration_s() {
    let body = r#"
  <xs:attribute name="to" type="xs:IDREF" default="old"/>
  <xs:attributeGroup name="fixedNew"><xs:attribute ref="to" fixed="new"/></xs:attributeGroup>
  <xs:attributeGroup name="defaultMid"><xs:attribute ref="to" default="mid"/></xs:attributeGroup>
  <xs:attributeGroup name="plain"><xs:attribute ref="to"/></xs:attributeGroup>
  <xs:complexType name="GroupFixed"><xs:attributeGroup ref="fixedNew"/></xs:complexType>
  <xs:complexType name="GroupDefault"><xs:attributeGroup ref="defaultMid"/></xs:complexType>
  <xs:complexType name="GroupPlain"><xs:attributeGroup ref="plain"/></xs:complexType>
  <xs:complexType name="DirectFixed"><xs:attribute ref="to" fixed="new"/></xs:complexType>
  <xs:complexType name="InheritedFixed"><xs:complexContent><xs:extension base="GroupFixed"/></xs:complexContent></xs:complexType>
  <xs:element name="root"><xs:complexType><xs:sequence>
    <xs:choice>
      <xs:element name="gf" type="GroupFixed"/>
      <xs:element name="gd" type="GroupDefault"/>
      <xs:element name="gp" type="GroupPlain"/>
      <xs:element name="df" type="DirectFixed"/>
      <xs:element name="if" type="InheritedFixed"/>
    </xs:choice>
    <xs:element name="id" minOccurs="0"><xs:complexType>
      <xs:attribute name="id" type="xs:ID"/>
    </xs:complexType></xs:element>
  </xs:sequence></xs:complexType></xs:element>"#;
    let mut cases = Vec::new();
    for (element, value) in [
        ("gf", "new"),
        ("gd", "mid"),
        ("gp", "old"),
        ("df", "new"),
        ("if", "new"),
    ] {
        for target in ["new", "mid", "old"] {
            let xml = format!("<root><{element}/><id id='{target}'/></root>");
            let ok = target == value;
            cases.push((xml, ok));
        }
    }
    let cases: Vec<(&str, SchemaValidity, Option<&str>)> = cases
        .iter()
        .map(|(xml, ok)| {
            if *ok {
                (xml.as_str(), Valid, None)
            } else {
                (xml.as_str(), Invalid, Some("cvc-id.1"))
            }
        })
        .collect();
    check_all(body, &cases);
}
