//! Complex-content restriction that names a member of a substitution group
//! where the base names the head — the pattern of the XBRL 2.1 linkbase
//! schema (`link:presentationArc` for `xl:arc`, `link:loc` for `xl:locator`).
//!
//! The base's particle for the head stands for every member of the group, so
//! the declaration the restriction's particle answers to is the member it
//! names, never the head:
//!
//! * XSD 1.0 §3.9.6 Particle Valid (Restriction) clause 2.1: the head's
//!   particle "is treated as if it were a choice group whose {min occurs} and
//!   {max occurs} are those of the particle, and whose {particles} consists of
//!   one particle with {min occurs} and {max occurs} of 1 for each of the
//!   declarations in its substitution group". Elt:Elt -- NameAndTypeOK then
//!   compares the restriction's particle with the member's own particle in
//!   that choice: the same declaration, with a range of 1..1.
//! * XSD 1.1 §3.4.6.4 clause 2: the base's default binding for each element
//!   subsumes the restriction's. For an element matched through a
//!   substitution group the governing declaration is the member's
//!   (§3.9.4.3 Element Sequence Accepted (Particle) clause 2.3.2), on both
//!   sides.
//!
//! What the member's type owes the head's is Element Declaration Properties
//! Correct (§3.3.6), which allows derivation by extension.
//!
//! Every case runs under XSD 1.0 and XSD 1.1.

use xsd_schema::validation::{
    drive_quick_xml, CollectingValidationSink, SchemaValidator, ValidationFlags,
};
use xsd_schema::{SchemaSet, SchemaSetBuilder, XsdVersion};

const VERSIONS: [XsdVersion; 2] = [XsdVersion::V1_0, XsdVersion::V1_1];

/// A schema with the head `arc` of type `arcType`, the top-level declarations
/// `globals` (the members), the base type `baseLinkType` with content `base`
/// and the element `link`, whose type restricts it to `restriction`.
fn schema_with_head(head: &str, globals: &str, base: &str, restriction: &str) -> String {
    format!(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:t="urn:t"
    targetNamespace="urn:t" elementFormDefault="qualified">

  <xs:complexType name="arcType">
    <xs:attribute name="from" type="xs:string"/>
  </xs:complexType>

  <xs:complexType name="extendedArcType">
    <xs:complexContent>
      <xs:extension base="t:arcType">
        <xs:attribute name="order" type="xs:decimal"/>
      </xs:extension>
    </xs:complexContent>
  </xs:complexType>

  {head}
  {globals}

  <xs:complexType name="baseLinkType">
    {base}
  </xs:complexType>

  <xs:element name="link">
    <xs:complexType>
      <xs:complexContent>
        <xs:restriction base="t:baseLinkType">
          {restriction}
        </xs:restriction>
      </xs:complexContent>
    </xs:complexType>
  </xs:element>
</xs:schema>"#
    )
}

/// The abstract head of the reported schema.
const HEAD: &str = r#"<xs:element name="arc" type="t:arcType" abstract="true"/>"#;

fn schema(globals: &str, base: &str, restriction: &str) -> String {
    schema_with_head(HEAD, globals, base, restriction)
}

/// A member whose type is the type of the head.
const SAME_TYPE: &str =
    r#"<xs:element name="presentationArc" type="t:arcType" substitutionGroup="t:arc"/>"#;

/// A member whose type extends the type of the head, declared as in the
/// report: with an anonymous type.
const EXTENDED_TYPE: &str = r#"<xs:element name="presentationArc" substitutionGroup="t:arc">
    <xs:complexType>
      <xs:complexContent>
        <xs:extension base="t:arcType">
          <xs:attribute name="order" type="xs:decimal"/>
        </xs:extension>
      </xs:complexContent>
    </xs:complexType>
  </xs:element>"#;

/// The same member with a named type.
const EXTENDED_NAMED_TYPE: &str =
    r#"<xs:element name="presentationArc" type="t:extendedArcType" substitutionGroup="t:arc"/>"#;

/// An element with the member's type that is in no substitution group.
const NOT_A_MEMBER: &str = r#"<xs:element name="presentationArc" type="t:extendedArcType"/>"#;

/// A repeated choice of `particles`.
fn repeated_choice(particles: &str) -> String {
    format!(r#"<xs:choice minOccurs="0" maxOccurs="unbounded">{particles}</xs:choice>"#)
}

fn sequence(particles: &str) -> String {
    format!("<xs:sequence>{particles}</xs:sequence>")
}

const REF_HEAD: &str = r#"<xs:element ref="t:arc"/>"#;
const REF_MEMBER: &str = r#"<xs:element ref="t:presentationArc"/>"#;

fn compile(version: XsdVersion, text: &str) -> Result<SchemaSet, String> {
    let builder = match version {
        XsdVersion::V1_0 => SchemaSetBuilder::new(),
        XsdVersion::V1_1 => SchemaSetBuilder::xsd11(),
    };
    builder
        .add_source(text, "t.xsd")
        .and_then(|builder| builder.compile())
        .map(|compiled| compiled.into_schema_set())
        .map_err(|error| error.to_string())
}

/// The schema compiles under `version`.
fn accepted(version: XsdVersion, text: &str) -> SchemaSet {
    compile(version, text).unwrap_or_else(|error| panic!("{version:?}: {error}"))
}

/// The schema is rejected under `version`, as an invalid restriction.
fn rejected(version: XsdVersion, text: &str) {
    match compile(version, text) {
        Ok(_) => panic!("{version:?}: the schema compiled"),
        Err(error) => assert!(
            error.contains("derivation-ok-restriction"),
            "{version:?}: {error}"
        ),
    }
}

/// The number of validation errors of `instance`.
fn instance_errors(schema_set: &SchemaSet, instance: &str) -> usize {
    let validator = SchemaValidator::new(schema_set, ValidationFlags::default());
    let (mut errors, mut warnings) = (Vec::new(), Vec::new());
    let mut runtime = validator.start_run(CollectingValidationSink {
        errors: &mut errors,
        warnings: &mut warnings,
    });
    drive_quick_xml(instance.as_bytes(), &mut runtime, schema_set).unwrap();
    drop(runtime);
    errors.len()
}

// ── The reported schema ────────────────────────────────────────────────────

/// The control of the report: the member has the head's type.
#[test]
fn member_with_the_heads_type_restricts_the_head() {
    let text = schema(
        SAME_TYPE,
        &repeated_choice(REF_HEAD),
        &repeated_choice(REF_MEMBER),
    );
    for version in VERSIONS {
        let set = accepted(version, &text);
        let instance = r#"<link xmlns="urn:t"><presentationArc from="a"/></link>"#;
        assert_eq!(instance_errors(&set, instance), 0, "{version:?}");
    }
}

/// The reported schema: the member's type extends the head's.
#[test]
fn member_with_an_extended_type_restricts_the_head() {
    for member in [EXTENDED_TYPE, EXTENDED_NAMED_TYPE] {
        let text = schema(
            member,
            &repeated_choice(REF_HEAD),
            &repeated_choice(REF_MEMBER),
        );
        for version in VERSIONS {
            let set = accepted(version, &text);
            let valid = r#"<link xmlns="urn:t"><presentationArc from="a" order="1"/><presentationArc from="b"/></link>"#;
            assert_eq!(instance_errors(&set, valid), 0, "{version:?}");
            // The restricted type is in force: the attribute is typed, and
            // the content is the member only.
            let bad_attribute =
                r#"<link xmlns="urn:t"><presentationArc from="a" order="first"/></link>"#;
            assert_ne!(instance_errors(&set, bad_attribute), 0, "{version:?}");
            let other_child = r#"<link xmlns="urn:t"><other/></link>"#;
            assert_ne!(instance_errors(&set, other_child), 0, "{version:?}");
        }
    }
}

/// The shape of the XBRL linkbase schema: a repeated choice of several heads
/// (`xl:title`, `xl:locator`, `xl:arc`, `xl:resource`), restricted to a
/// repeated choice of members, some with extended types, some heads left out.
#[test]
fn choice_of_heads_restricted_to_a_choice_of_members() {
    let globals = r#"
  <xs:complexType name="locatorType">
    <xs:attribute name="href" type="xs:anyURI"/>
  </xs:complexType>
  <xs:element name="title" type="xs:string" abstract="true"/>
  <xs:element name="locator" type="t:locatorType" abstract="true"/>
  <xs:element name="resource" type="xs:string" abstract="true"/>
  <xs:element name="loc" substitutionGroup="t:locator">
    <xs:complexType>
      <xs:complexContent>
        <xs:extension base="t:locatorType">
          <xs:attribute name="label" type="xs:NCName"/>
        </xs:extension>
      </xs:complexContent>
    </xs:complexType>
  </xs:element>
  <xs:element name="presentationArc" type="t:extendedArcType" substitutionGroup="t:arc"/>
  <xs:element name="label" substitutionGroup="t:resource">
    <xs:complexType>
      <xs:simpleContent>
        <xs:extension base="xs:string">
          <xs:attribute name="lang" type="xs:language"/>
        </xs:extension>
      </xs:simpleContent>
    </xs:complexType>
  </xs:element>"#;
    let base = repeated_choice(
        r#"<xs:element ref="t:title"/>
           <xs:element ref="t:locator"/>
           <xs:element ref="t:arc"/>
           <xs:element ref="t:resource"/>"#,
    );
    let restriction = repeated_choice(
        r#"<xs:element ref="t:loc"/>
           <xs:element ref="t:presentationArc"/>
           <xs:element ref="t:label"/>"#,
    );
    for version in VERSIONS {
        let set = accepted(version, &schema(globals, &base, &restriction));
        let instance = r#"<link xmlns="urn:t"><loc href="a.xml" label="a"/><presentationArc from="a" order="2"/><label lang="en">A</label><loc href="b.xml"/></link>"#;
        assert_eq!(instance_errors(&set, instance), 0, "{version:?}");
    }
}

/// The member where a sequence has the head, once (W3C particlesZ008, with
/// an extended type).
#[test]
fn member_in_a_sequence_restricts_the_head() {
    let tail = r#"<xs:element name="e2" type="xs:string"/>"#;
    let text = schema(
        EXTENDED_TYPE,
        &sequence(&format!("{REF_HEAD}{tail}")),
        &sequence(&format!("{REF_MEMBER}{tail}")),
    );
    for version in VERSIONS {
        let set = accepted(version, &text);
        let instance = r#"<link xmlns="urn:t"><presentationArc order="1"/><e2/></link>"#;
        assert_eq!(instance_errors(&set, instance), 0, "{version:?}");
    }
}

// ── Occurrence ranges ──────────────────────────────────────────────────────

/// W3C particlesV020 (expected invalid; Bugzilla 4147): the base has the head
/// 1..3, the restriction the member 1..2. In the choice XSD 1.0 puts in the
/// head's place the member's particle is 1..1, and 1..2 is not within it,
/// whatever the types. XSD 1.1 compares the sequences the two accept.
#[test]
fn repeated_member_particle_against_a_repeated_head_particle() {
    let tail = r#"<xs:element name="e2" type="xs:string"/>"#;
    let text = schema(
        EXTENDED_TYPE,
        &sequence(&format!(r#"<xs:element ref="t:arc" maxOccurs="3"/>{tail}"#)),
        &sequence(&format!(
            r#"<xs:element ref="t:presentationArc" maxOccurs="2"/>{tail}"#
        )),
    );
    rejected(XsdVersion::V1_0, &text);
    accepted(XsdVersion::V1_1, &text);
}

/// The same content with the repetition on a choice around the member:
/// Choice:Choice -- RecurseLax compares the two choices' ranges, and the
/// member's particle is 1..1.
#[test]
fn repeated_choice_of_the_member_against_a_repeated_head_particle() {
    let tail = r#"<xs:element name="e2" type="xs:string"/>"#;
    let text = schema(
        EXTENDED_TYPE,
        &sequence(&format!(r#"<xs:element ref="t:arc" maxOccurs="3"/>{tail}"#)),
        &sequence(&format!(
            r#"<xs:choice maxOccurs="2">{REF_MEMBER}</xs:choice>{tail}"#
        )),
    );
    for version in VERSIONS {
        accepted(version, &text);
    }
}

/// A repeated choice of two members, each with an extended type, where the
/// base has the head as one repeated particle: Choice:Choice -- RecurseLax
/// against the choice that stands for the head.
#[test]
fn repeated_choice_of_two_members_against_a_repeated_head_particle() {
    let globals = format!(
        r#"{EXTENDED_TYPE}
  <xs:element name="calculationArc" type="t:extendedArcType" substitutionGroup="t:arc"/>"#
    );
    let base = sequence(r#"<xs:element ref="t:arc" minOccurs="0" maxOccurs="unbounded"/>"#);
    let members = format!(r#"{REF_MEMBER}<xs:element ref="t:calculationArc"/>"#);
    for version in VERSIONS {
        let set = accepted(
            version,
            &schema(&globals, &base, &repeated_choice(&members)),
        );
        let instance = r#"<link xmlns="urn:t"><calculationArc order="1"/><presentationArc/><calculationArc/></link>"#;
        assert_eq!(instance_errors(&set, instance), 0, "{version:?}");
    }
    // One more iteration than the head may occur.
    let base = sequence(r#"<xs:element ref="t:arc" minOccurs="0" maxOccurs="2"/>"#);
    let restriction = format!(r#"<xs:choice minOccurs="0" maxOccurs="3">{members}</xs:choice>"#);
    for version in VERSIONS {
        rejected(version, &schema(&globals, &base, &restriction));
    }
}

/// More members than the base has heads.
#[test]
fn more_members_than_the_base_allows() {
    let text = schema(
        EXTENDED_TYPE,
        &format!(r#"<xs:choice minOccurs="0" maxOccurs="2">{REF_HEAD}</xs:choice>"#),
        &format!(r#"<xs:choice minOccurs="0" maxOccurs="3">{REF_MEMBER}</xs:choice>"#),
    );
    for version in VERSIONS {
        rejected(version, &text);
    }
}

// ── Not a member ───────────────────────────────────────────────────────────

/// An element with the same type that is in no substitution group.
#[test]
fn element_outside_the_group_does_not_restrict_the_head() {
    let text = schema(
        NOT_A_MEMBER,
        &repeated_choice(REF_HEAD),
        &repeated_choice(REF_MEMBER),
    );
    for version in VERSIONS {
        rejected(version, &text);
    }
}

/// A head that blocks substitution, or substitution by extended types, has
/// no such member in its group: the base accepts no `presentationArc`.
#[test]
fn blocked_member_does_not_restrict_the_head() {
    for block in ["substitution", "extension"] {
        let head = format!(r#"<xs:element name="arc" type="t:arcType" block="{block}"/>"#);
        let text = schema_with_head(
            &head,
            EXTENDED_NAMED_TYPE,
            &repeated_choice(REF_HEAD),
            &repeated_choice(REF_MEMBER),
        );
        for version in VERSIONS {
            rejected(version, &text);
        }
    }
}
