//! Content models must not accept child sequences their particles forbid.
//!
//! Kleene star and plus drew their loop edge between the body's own start and
//! end states. Combinators attach edges to those boundary states — the bypass
//! of an optional group, the link of a sequence, an enclosing loop — which then
//! entered or left the loop: `(X, Y*)?, Z` accepted `<Y/><Z/>` although `Y`
//! may only follow `X`. A real schema hit it: GAEB DA XML 3.3 DA85 `tgItem`,
//! `choice{0,1}(sequence(Qty, QtySplit*))`.
//!
//! Every case runs in XSD 1.0 and XSD 1.1 mode through the streaming driver.

use xsd_schema::validation::{
    drive_quick_xml, CollectingValidationSink, SchemaValidator, SchemaValidity, ValidationFlags,
};
use xsd_schema::{SchemaSet, SchemaSetBuilder};

fn schema(xsd: &str, xsd11: bool) -> SchemaSet {
    let builder = if xsd11 {
        SchemaSetBuilder::xsd11()
    } else {
        SchemaSetBuilder::new()
    };
    builder
        .add_source(xsd, "file:///content_model_soundness.xsd")
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

/// `<root>` whose content is `body` inside a top-level `xs:sequence`.
fn root_schema(body: &str) -> String {
    format!(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="root"><xs:complexType><xs:sequence>{body}</xs:sequence></xs:complexType></xs:element>
</xs:schema>"#
    )
}

fn check(label: &str, xsd: &str, valid: &[&str], invalid: &[&str]) {
    for xsd11 in [false, true] {
        let set = schema(xsd, xsd11);
        let version = if xsd11 { "1.1" } else { "1.0" };
        for xml in valid {
            assert_eq!(
                validity(&set, xml),
                Some(SchemaValidity::Valid),
                "{label} [{version}]: {xml} must be valid"
            );
        }
        for xml in invalid {
            assert_eq!(
                validity(&set, xml),
                Some(SchemaValidity::Invalid),
                "{label} [{version}]: {xml} must be invalid"
            );
        }
    }
}

const X: &str = r#"<xs:element name="X"/>"#;
const Y_STAR: &str = r#"<xs:element name="Y" minOccurs="0" maxOccurs="unbounded"/>"#;
const Y_PLUS: &str = r#"<xs:element name="Y" maxOccurs="unbounded"/>"#;
const Z: &str = r#"<xs:element name="Z"/>"#;

#[test]
fn loop_inside_optional_group_is_not_entered_by_the_bypass() {
    let xsd = root_schema(&format!(
        r#"<xs:sequence minOccurs="0">{X}{Y_STAR}</xs:sequence>{Z}"#
    ));
    check(
        "(X, Y*)?, Z",
        &xsd,
        &[
            "<root><Z/></root>",
            "<root><X/><Z/></root>",
            "<root><X/><Y/><Y/><Z/></root>",
        ],
        &["<root><Y/><Z/></root>", "<root><Y/><X/><Z/></root>"],
    );
}

#[test]
fn plus_inside_optional_group_is_not_entered_by_the_bypass() {
    let xsd = root_schema(&format!(
        r#"<xs:sequence minOccurs="0">{X}{Y_PLUS}</xs:sequence>{Z}"#
    ));
    check(
        "(X, Y+)?, Z",
        &xsd,
        &["<root><Z/></root>", "<root><X/><Y/><Z/></root>"],
        &["<root><Y/><Z/></root>", "<root><X/><Z/></root>"],
    );
}

#[test]
fn loop_at_the_start_of_an_optional_group_cannot_skip_the_rest() {
    let xsd = root_schema(&format!(
        r#"<xs:sequence minOccurs="0">{Y_STAR}{X}</xs:sequence>{Z}"#
    ));
    check(
        "(Y*, X)?, Z",
        &xsd,
        &[
            "<root><Z/></root>",
            "<root><X/><Z/></root>",
            "<root><Y/><Y/><X/><Z/></root>",
        ],
        &["<root><Y/><Z/></root>", "<root><Y/><Y/><Z/></root>"],
    );
}

#[test]
fn loop_inside_repeated_groups() {
    let unrolled = root_schema(&format!(
        r#"<xs:sequence minOccurs="0" maxOccurs="3">{X}{Y_STAR}</xs:sequence>{Z}"#
    ));
    check(
        "(X, Y*){{0,3}}, Z",
        &unrolled,
        &["<root><Z/></root>", "<root><X/><Y/><X/><Z/></root>"],
        &["<root><Y/><Z/></root>", "<root><X/><X/><X/><X/><Z/></root>"],
    );

    // Above the unroll threshold the group becomes a counted loop.
    let counted = root_schema(&format!(
        r#"<xs:sequence minOccurs="0" maxOccurs="20">{X}{Y_STAR}</xs:sequence>{Z}"#
    ));
    check(
        "(X, Y*){{0,20}}, Z",
        &counted,
        &["<root><Z/></root>", "<root><X/><Y/><X/><Z/></root>"],
        &["<root><Y/><Z/></root>"],
    );

    let starred = root_schema(&format!(
        r#"<xs:sequence minOccurs="0" maxOccurs="unbounded">{X}{Y_STAR}</xs:sequence>{Z}"#
    ));
    check(
        "(X, Y*)*, Z",
        &starred,
        &["<root><Z/></root>", "<root><X/><Y/><X/><Y/><Y/><Z/></root>"],
        &["<root><Y/><Z/></root>", "<root><Y/><X/><Z/></root>"],
    );

    let plussed = root_schema(&format!(
        r#"<xs:sequence maxOccurs="unbounded">{X}{Y_STAR}</xs:sequence>{Z}"#
    ));
    check(
        "(X, Y*)+, Z",
        &plussed,
        &["<root><X/><Z/></root>", "<root><X/><Y/><X/><Z/></root>"],
        &["<root><Y/><Z/></root>", "<root><Z/></root>"],
    );
}

/// The shape of GAEB DA XML 3.3 DA85 `tgItem`: `QtySplit` may only follow `Qty`.
#[test]
fn optional_choice_of_a_sequence_ending_in_a_loop() {
    let xsd = root_schema(
        r#"<xs:choice minOccurs="0">
             <xs:sequence>
               <xs:element name="Qty"/>
               <xs:element name="QtySplit" minOccurs="0" maxOccurs="unbounded"/>
             </xs:sequence>
           </xs:choice>
           <xs:element name="QU"/>"#,
    );
    check(
        "choice{0,1}(Qty, QtySplit*), QU",
        &xsd,
        &[
            "<root><QU/></root>",
            "<root><Qty/><QU/></root>",
            "<root><Qty/><QtySplit/><QtySplit/><QU/></root>",
        ],
        &["<root><QtySplit/><QU/></root>"],
    );
}

#[test]
fn controls_without_an_enclosing_optional_group() {
    check(
        "X, Y*, Z",
        &root_schema(&format!("{X}{Y_STAR}{Z}")),
        &["<root><X/><Z/></root>", "<root><X/><Y/><Z/></root>"],
        &["<root><Y/><Z/></root>"],
    );
    check(
        "(X, Y*), Z",
        &root_schema(&format!("<xs:sequence>{X}{Y_STAR}</xs:sequence>{Z}")),
        &["<root><X/><Y/><Z/></root>"],
        &["<root><Y/><Z/></root>"],
    );
}
