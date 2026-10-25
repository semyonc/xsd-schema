//! Complex-content restriction (`derivation-ok-restriction`): a restricted
//! type's content must admit no child sequence its base's content rejects.
//!
//! * **Nested base sequences.** The content of an extension is
//!   `sequence(inherited, own)` with occurrence range 1..1 (XSD 1.0 §3.4.2
//!   clause 3.2.3, XSD 1.1 §3.4.2.3.3 clause 4.2.3.3). XSD 1.0 §3.9.6 clause
//!   2.2 ignores such "pointless" sequences when it checks a restriction;
//!   XSD 1.1 §3.4.6.4 compares languages, which a unit sequence inside a
//!   sequence does not change. A restriction that lists the base's elements
//!   one after another must therefore be checked against the flattened base,
//!   whether the nesting comes from extension (one level is enough), from
//!   nested `xs:sequence` elements or from a model-group reference.
//!
//! Every case runs under XSD 1.0 and XSD 1.1.

use xsd_schema::{SchemaResult, SchemaSet, SchemaSetBuilder, XsdVersion};

// ── Schema building blocks ─────────────────────────────────────────────────

/// The occurrence attributes for `{min, max}` (defaults omitted).
fn occurs(min: u32, max: Option<u32>) -> String {
    let mut s = String::new();
    if min != 1 {
        s.push_str(&format!(r#" minOccurs="{min}""#));
    }
    match max {
        Some(1) => {}
        Some(n) => s.push_str(&format!(r#" maxOccurs="{n}""#)),
        None => s.push_str(r#" maxOccurs="unbounded""#),
    }
    s
}

/// A local element `name{min,max}` of type `xs:string`.
fn e(name: &str, min: u32, max: Option<u32>) -> String {
    format!(
        r#"<xs:element name="{name}" type="xs:string"{}/>"#,
        occurs(min, max)
    )
}

/// A model group `compositor{min,max}(children)`.
fn g(compositor: &str, min: u32, max: Option<u32>, children: &[String]) -> String {
    format!(
        "<xs:{compositor}{}>{}</xs:{compositor}>",
        occurs(min, max),
        children.concat()
    )
}

fn seq(children: &[String]) -> String {
    g("sequence", 1, Some(1), children)
}

fn complex_type(name: &str, content: &str) -> String {
    format!(r#"<xs:complexType name="{name}">{content}</xs:complexType>"#)
}

fn extension(name: &str, base: &str, own: &str) -> String {
    format!(
        r#"<xs:complexType name="{name}"><xs:complexContent><xs:extension base="t:{base}">{own}</xs:extension></xs:complexContent></xs:complexType>"#
    )
}

fn restriction(name: &str, base: &str, content: &str) -> String {
    format!(
        r#"<xs:complexType name="{name}"><xs:complexContent><xs:restriction base="t:{base}">{content}</xs:restriction></xs:complexContent></xs:complexType>"#
    )
}

fn document(body: &str) -> String {
    format!(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:t="urn:t" targetNamespace="urn:t">{body}</xs:schema>"#
    )
}

fn compile(version: XsdVersion, xsd: &str) -> SchemaResult<SchemaSet> {
    SchemaSetBuilder::with_version(version)
        .add_source(xsd, "file:///content_restriction.xsd")?
        .compile()
        .map(|compiled| compiled.into_schema_set())
}

// ── Cases ──────────────────────────────────────────────────────────────────

const VERSIONS: [XsdVersion; 2] = [XsdVersion::V1_0, XsdVersion::V1_1];

/// One restriction: the base `B` (with the type definitions it derives
/// from), the restriction's content, and whether the restriction is valid
/// under XSD 1.0 and XSD 1.1 (`None`: not asserted).
struct Case {
    label: &'static str,
    /// Type definitions; the base type is named `B`.
    base: String,
    /// The content of the restriction `R` of `B`.
    restricted: String,
    valid: [Option<bool>; 2],
}

impl Case {
    fn new(label: &'static str, base: String, restricted: String, v10: bool, v11: bool) -> Self {
        Case {
            label,
            base,
            restricted,
            valid: [Some(v10), Some(v11)],
        }
    }

    /// The base is the single complex type `B` with content `content`.
    fn plain(
        label: &'static str,
        content: String,
        restricted: String,
        v10: bool,
        v11: bool,
    ) -> Self {
        Case::new(label, complex_type("B", &content), restricted, v10, v11)
    }

    /// Every way this case's expectations fail, as messages.
    fn problems(&self) -> Vec<String> {
        let mut problems = Vec::new();
        let xsd = document(&format!(
            "{}{}",
            self.base,
            restriction("R", "B", &self.restricted)
        ));
        for (version, expected) in VERSIONS.iter().zip(self.valid) {
            let Some(expected) = expected else { continue };
            match (compile(*version, &xsd), expected) {
                (Ok(_), true) => {}
                (Err(err), false) => {
                    let err = err.to_string();
                    if !err.contains("derivation-ok-restriction") {
                        problems.push(format!(
                            "{} [{version:?}]: expected derivation-ok-restriction, got: {err}",
                            self.label
                        ));
                    }
                }
                (Ok(_), false) => problems.push(format!(
                    "{} [{version:?}]: the restriction must be rejected (derivation-ok-restriction)",
                    self.label
                )),
                (Err(err), true) => problems.push(format!(
                    "{} [{version:?}]: the restriction must load: {err}",
                    self.label
                )),
            }
        }
        problems
    }
}

/// Checks every case and reports all failures together.
fn check_all(cases: &[Case]) {
    let problems: Vec<String> = cases.iter().flat_map(Case::problems).collect();
    assert!(problems.is_empty(), "\n{}", problems.join("\n"));
}

// ── Nested base sequences (C21) ────────────────────────────────────────────

/// `A(a?, b?) ⊕ (d?, e?) ⊕ (f?)`: the content of `B` is
/// `sequence(sequence(sequence(a?, b?), sequence(d?, e?)), sequence(f?))`.
#[test]
fn restriction_of_a_three_level_extension_chain() {
    let base = [
        complex_type("A", &seq(&[e("a", 0, Some(1)), e("b", 0, Some(1))])),
        extension("M", "A", &seq(&[e("d", 0, Some(1)), e("e", 0, Some(1))])),
        extension("B", "M", &seq(&[e("f", 0, Some(1))])),
    ]
    .concat();
    let restricted = seq(&[e("b", 0, Some(1)), e("d", 0, Some(1)), e("f", 0, Some(1))]);
    check_all(&[Case::new("three-level", base, restricted, true, true)]);
}

/// The same nesting written out literally.
#[test]
fn restriction_of_literally_nested_sequences() {
    let content = seq(&[
        seq(&[
            seq(&[e("a", 0, Some(1)), e("b", 0, Some(1))]),
            seq(&[e("d", 0, Some(1))]),
        ]),
        seq(&[e("f", 0, Some(1))]),
    ]);
    let restricted = seq(&[e("b", 0, Some(1)), e("d", 0, Some(1)), e("f", 0, Some(1))]);
    check_all(&[Case::plain(
        "nested-literal",
        content,
        restricted,
        true,
        true,
    )]);
}

/// One extension level is enough: `A(a?, b?) ⊕ (d?)` restricted to
/// `(a?, b?, d?)` — the shape of a restriction that repeats an all-optional
/// base element by element.
#[test]
fn restriction_of_a_one_level_extension() {
    let base = [
        complex_type("A", &seq(&[e("a", 0, Some(1)), e("b", 0, Some(1))])),
        extension("B", "A", &seq(&[e("d", 0, Some(1))])),
    ]
    .concat();
    let restricted = seq(&[e("a", 0, Some(1)), e("b", 0, Some(1)), e("d", 0, Some(1))]);
    check_all(&[Case::new(
        "one level, all optional",
        base,
        restricted,
        true,
        true,
    )]);
}

/// A model-group reference nested in the base's sequence.
#[test]
fn restriction_of_a_sequence_with_a_group_reference() {
    let base = format!(
        r#"<xs:group name="g">{}</xs:group>{}"#,
        seq(&[e("a", 0, Some(1)), e("b", 0, Some(1)), e("c", 0, Some(1))]),
        complex_type(
            "B",
            &seq(&[r#"<xs:group ref="t:g"/>"#.to_string(), e("d", 0, Some(1))])
        )
    );
    let restricted = seq(&[e("a", 0, Some(1)), e("b", 0, Some(1)), e("d", 0, Some(1))]);
    check_all(&[Case::new("group reference", base, restricted, true, true)]);
}

/// Controls that load already without flattening: two extension levels
/// restricted to one element of each part, and required elements, which
/// the restriction repeats as whole units.
#[test]
fn restriction_of_extension_chains_controls() {
    let two_level = [
        complex_type("A", &seq(&[e("a", 0, Some(1)), e("b", 0, Some(1))])),
        extension("B", "A", &seq(&[e("d", 0, Some(1)), e("e", 0, Some(1))])),
    ]
    .concat();
    let required = [
        complex_type("A", &seq(&[e("a", 1, Some(1)), e("b", 1, Some(1))])),
        extension("B", "A", &seq(&[e("d", 1, Some(1))])),
    ]
    .concat();
    check_all(&[
        Case::new(
            "two-level",
            two_level,
            seq(&[e("b", 0, Some(1)), e("d", 0, Some(1))]),
            true,
            true,
        ),
        Case::new(
            "one level, required",
            required,
            seq(&[e("a", 1, Some(1)), e("b", 1, Some(1)), e("d", 1, Some(1))]),
            true,
            true,
        ),
    ]);
}
