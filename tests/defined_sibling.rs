//! `notQName="##definedSibling"` (XSD 1.1 §3.10.4.2): the wildcard admits no
//! element with the name of an element declared next to it, nor one with the
//! name of a member of such an element's substitution group, however long
//! the chain of affiliations.
//!
//! The sibling names are collected only for the groups that have a wildcard
//! to read them, and the members of a substitution group are found through
//! an index of the declarations by head. These cases hold the wildcard to
//! the same names wherever it gets its siblings from: its own group, the
//! `xs:all` that a referenced group is flattened into, and the content model
//! an open content wildcard is attached to.
#![cfg(feature = "xsd11")]

use xsd_schema::validation::{
    drive_quick_xml, CollectingValidationSink, SchemaValidator, SchemaValidity, ValidationFlags,
};
use xsd_schema::{SchemaSet, SchemaSetBuilder};

/// A head, a member, a member of that member, a member declared with two
/// heads, and an element in no group; `doc` has the complex type `content`.
fn schema(content: &str) -> SchemaSet {
    let xsd = format!(
        r###"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="head" type="xs:string"/>
  <xs:element name="m1" type="xs:string" substitutionGroup="head"/>
  <xs:element name="m2" type="xs:string" substitutionGroup="m1"/>
  <xs:element name="other" type="xs:string"/>
  <xs:element name="both" type="xs:string" substitutionGroup="other m2"/>
  <xs:element name="free" type="xs:string"/>
  <xs:group name="tail">
    <xs:all>
      <xs:element name="inner" type="xs:string" minOccurs="0"/>
      <xs:any notQName="##definedSibling" processContents="lax" minOccurs="0"/>
    </xs:all>
  </xs:group>
  <xs:element name="doc"><xs:complexType>{content}</xs:complexType></xs:element>
</xs:schema>"###
    );
    SchemaSetBuilder::xsd11()
        .add_source(&xsd, "file:///defined_sibling.xsd")
        .expect("schema source accepted")
        .compile()
        .expect("schema compiles")
        .into_schema_set()
}

fn validity(schema_set: &SchemaSet, xml: &str) -> Option<SchemaValidity> {
    let validator = SchemaValidator::new(schema_set, ValidationFlags::default());
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let sink = CollectingValidationSink {
        errors: &mut errors,
        warnings: &mut warnings,
    };
    let mut runtime = validator.start_run(sink);
    drive_quick_xml(xml.as_bytes(), &mut runtime, schema_set)
        .expect("driver completes")
        .root_validity
}

fn check(label: &str, content: &str, valid: &[&str], invalid: &[&str]) {
    let set = schema(content);
    for xml in valid {
        assert_eq!(
            validity(&set, xml),
            Some(SchemaValidity::Valid),
            "{label}: {xml}"
        );
    }
    for xml in invalid {
        assert_eq!(
            validity(&set, xml),
            Some(SchemaValidity::Invalid),
            "{label}: {xml}"
        );
    }
}

const WILDCARD: &str =
    r###"<xs:any notQName="##definedSibling" processContents="lax" minOccurs="0"/>"###;

/// The wildcard is a particle of the sequence that has the head.
#[test]
fn wildcard_in_the_group_of_the_head() {
    let content = format!(r#"<xs:sequence><xs:element ref="head"/>{WILDCARD}</xs:sequence>"#);
    check(
        "sequence",
        &content,
        &[
            "<doc><head/><free/></doc>",
            "<doc><m2/><free/></doc>",
            "<doc><both/><other/></doc>",
        ],
        &[
            // The head's particle is used; the wildcard takes no sibling
            // name, and no name of a member of the head's group.
            "<doc><head/><head/></doc>",
            "<doc><head/><m1/></doc>",
            "<doc><m1/><m2/></doc>",
            "<doc><head/><both/></doc>",
        ],
    );
}

/// Without `##definedSibling` nothing is collected, and nothing is excluded.
#[test]
fn wildcard_without_defined_sibling() {
    check(
        "plain wildcard",
        r#"<xs:sequence><xs:element ref="head"/><xs:any processContents="lax" minOccurs="0"/></xs:sequence>"#,
        &[
            "<doc><head/><free/></doc>",
            "<doc><head/><m2/></doc>",
            "<doc><m1/><head/></doc>",
        ],
        &["<doc><free/></doc>"],
    );
}

/// The wildcard is in a group referenced from the `xs:all` that has the
/// head. The group is flattened into that `xs:all`, whose elements are the
/// wildcard's siblings: no particle of the `xs:all` itself is a wildcard.
#[test]
fn wildcard_in_a_group_referenced_from_all() {
    check(
        "all with a group reference",
        r#"<xs:all><xs:element ref="head" minOccurs="0"/><xs:group ref="tail"/></xs:all>"#,
        &[
            "<doc><m2/><inner/><free/></doc>",
            "<doc><free/><head/></doc>",
            "<doc><both/><other/></doc>",
        ],
        &[
            "<doc><head/><m1/></doc>",
            "<doc><m1/><inner/><both/></doc>",
            "<doc><inner/><inner/></doc>",
        ],
    );
}

/// The wildcard is a particle of the `xs:all` that has the head.
#[test]
fn wildcard_in_all() {
    let content = format!(r#"<xs:all><xs:element ref="head" minOccurs="0"/>{WILDCARD}</xs:all>"#);
    check(
        "all",
        &content,
        &["<doc><free/><m2/></doc>", "<doc><other/></doc>"],
        &["<doc><m1/><both/></doc>", "<doc><head/><m2/></doc>"],
    );
}

/// An open content wildcard takes its siblings from the whole content model.
#[test]
fn open_content_wildcard() {
    let open = r###"<xs:openContent mode="interleave"><xs:any notQName="##definedSibling" processContents="lax"/></xs:openContent>"###;
    for (label, model) in [
        (
            "open content, sequence",
            r#"<xs:sequence><xs:element ref="head"/></xs:sequence>"#,
        ),
        (
            "open content, all",
            r#"<xs:all><xs:element ref="head"/></xs:all>"#,
        ),
    ] {
        check(
            label,
            &format!("{open}{model}"),
            &[
                "<doc><free/><m2/><other/></doc>",
                "<doc><head/><free/></doc>",
            ],
            &["<doc><head/><m1/></doc>", "<doc><free/><m2/><both/></doc>"],
        );
    }
}
