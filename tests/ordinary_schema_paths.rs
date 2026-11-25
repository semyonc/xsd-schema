#![cfg(feature = "xsd11")]
//! An ordinary schema takes one path, whatever else its schema set holds and
//! however a document is driven through it.
//!
//! A validation run reads the current dateTime once (XSD 1.1 Structures
//! §3.13.4.2 clause 6), and a schema set reads its own literals — a `fixed`
//! or `default` value, an enumeration — at one instant. Only a type one of
//! whose `xs:assertion` facets calls `fn:current-dateTime` can tell the two
//! instants apart, so only such a type is handled otherwise. This test is
//! about every other type: for the same declarations and the same document,
//!
//! - a schema set with no XPath expression at all,
//! - the same set with an unrelated `<xs:assert test="true()"/>`,
//! - with an unrelated `xs:assertion` facet,
//! - with an unrelated `xs:assertion` facet that reads the current dateTime,
//!
//! each with and without `BUILD_PSVI_TYPED_VALUES`, through `drive_quick_xml`,
//! `build_typed_document_with` and a `TypedBuilderHandler` driven with
//! `drive_quick_xml_with`, give the same errors, the same validity and the
//! same typed document.
//!
//! The matrix (`ordinary_schema_paths/matrix.rs`): 37 fixed / default values —
//! floats, strings, dates, durations, decimals, binaries, QName, NOTATION,
//! lists, unions (one of a QName enumeration and `xs:string`), simple content
//! — on elements and on attributes (the attribute use's value, the global
//! declaration's, a use-level `fixed` on a reference, a defaulted attribute, a
//! global attribute a wildcard matches), written as the literal, in another
//! spelling, as another value, left empty, nilled, and under `xsi:type`; with
//! the documents binding the QName prefix three ways; and elements and
//! attributes of ID and IDREF types.

#[path = "ordinary_schema_paths/matrix.rs"]
mod matrix;

use matrix::{
    compile, document, flag_sets, observe, Driver, Extra, Shape, CASES, DRIVERS, EXTRAS,
    Q_NAMESPACES, SHAPES,
};
use xsd_schema::SchemaSet;

/// The first line two texts differ in, for the failure message.
fn first_difference(a: &str, b: &str) -> String {
    let (mut left, mut right) = (a.lines(), b.lines());
    let mut line = 1;
    loop {
        match (left.next(), right.next()) {
            (None, None) => return "no difference".to_string(),
            (l, r) if l != r => {
                return format!(
                    "line {line}:\n  first:  {}\n  second: {}",
                    l.unwrap_or("(end)"),
                    r.unwrap_or("(end)")
                )
            }
            _ => line += 1,
        }
    }
}

fn schema_sets() -> Vec<(Extra, SchemaSet)> {
    EXTRAS
        .iter()
        .map(|&extra| (extra, compile(extra)))
        .collect()
}

/// Errors, validity and the typed document are the same in every schema set
/// of the matrix, with either flag set, through every driver.
#[test]
fn an_ordinary_schema_behaves_the_same_on_every_path() {
    let sets = schema_sets();
    let mut compared = 0;
    for shape in SHAPES {
        for q_ns in Q_NAMESPACES {
            let xml = document(shape, q_ns);
            // The verdict of the first observation, and per `defaults_as_nodes`
            // the document of the first typed build.
            let mut verdict: Option<(String, String)> = None;
            let mut documents: [Option<(String, String)>; 2] = [None, None];
            for (extra, ss) in &sets {
                for (flag_label, flags) in flag_sets() {
                    for driver in DRIVERS {
                        let what =
                            format!("{shape:?}, q = {q_ns}, {extra:?}, {flag_label}, {driver:?}");
                        let seen = observe(ss, &xml, flags, driver);
                        match &verdict {
                            None => verdict = Some((what.clone(), seen.verdict)),
                            Some((first, expected)) => assert!(
                                *expected == seen.verdict,
                                "the errors differ between\n  {first}\nand\n  {what}\nat {}",
                                first_difference(expected, &seen.verdict)
                            ),
                        }
                        let slot = match driver {
                            Driver::Stream => continue,
                            Driver::TypedBuild { defaults_as_nodes }
                            | Driver::DirectHandler { defaults_as_nodes } => {
                                &mut documents[usize::from(defaults_as_nodes)]
                            }
                        };
                        let built = seen.document.expect("a typed build has a document");
                        match slot {
                            None => *slot = Some((what, built)),
                            Some((first, expected)) => assert!(
                                *expected == built,
                                "the documents differ between\n  {first}\nand\n  {what}\nat {}",
                                first_difference(expected, &built)
                            ),
                        }
                        compared += 1;
                    }
                }
            }
        }
    }
    // 6 shapes, 3 bindings of `q`, 4 schema sets, 2 flag sets, 4 typed builds.
    assert_eq!(compared, 6 * 3 * 4 * 2 * 4);
}

/// The tree a run builds for its assertions — the string value of every leaf
/// element and attribute, the nodes `fn:id` and `fn:idref` find — is the same
/// with either flag set, through every driver: an assertion on the document's
/// type that always fails tells it in its message.
#[test]
fn the_data_model_of_an_assertion_does_not_follow_the_flags() {
    let ss = compile(Extra::Reveal);
    for shape in SHAPES {
        for q_ns in Q_NAMESPACES {
            let xml = document(shape, q_ns);
            let mut first: Option<(String, String)> = None;
            for (flag_label, flags) in flag_sets() {
                for driver in DRIVERS {
                    let what = format!("{shape:?}, q = {q_ns}, {flag_label}, {driver:?}");
                    let seen = observe(&ss, &xml, flags, driver).verdict;
                    assert!(
                        seen.contains("cvc-assertion") && seen.contains("ids: k1="),
                        "{seen}"
                    );
                    match &first {
                        None => first = Some((what, seen)),
                        Some((first, expected)) => assert!(
                            *expected == seen,
                            "the errors differ between\n  {first}\nand\n  {what}\nat {}",
                            first_difference(expected, &seen)
                        ),
                    }
                }
            }
        }
    }
}

/// The matrix is not vacuous: the documents written as the schema's literals
/// and in their other spellings are valid, the empty document takes every
/// value constraint, and every other value is reported.
#[test]
fn the_matrix_validates_what_it_says() {
    let ss = compile(Extra::None);
    let (_, flags) = flag_sets()[0];
    let stream = |shape, q_ns| observe(&ss, &document(shape, q_ns), flags, Driver::Stream).verdict;
    let errors = |verdict: &str| verdict.lines().count() - 1;

    // The cases an error is reported in, by label.
    let reported_in = |verdict: &str| -> Vec<&'static str> {
        let mut cases: Vec<usize> = verdict
            .lines()
            .skip(1)
            .map(|line| {
                let at = line
                    .find("Q{}c")
                    .unwrap_or_else(|| panic!("an error of a case: {line}"));
                let digits: String = line[at + 4..]
                    .chars()
                    .take_while(char::is_ascii_digit)
                    .collect();
                digits
                    .parse()
                    .unwrap_or_else(|_| panic!("an error of a case: {line}"))
            })
            .collect();
        cases.dedup();
        cases.into_iter().map(|i| CASES[i].label).collect()
    };

    // Written as the literal, in its other spelling and under `xsi:type`,
    // every value is valid — whatever the document binds `q` to, but for the
    // cases whose literal is a QName of the enumeration or selects a member
    // of the union by it: what those read depends on that binding. (They are
    // in the matrix for the comparison above, not for this one.)
    let depends_on_q = [
        "union of QName enumeration",
        "list of that union",
        "union of QName enumeration, spaces",
        "union, QName member, spaces",
        "union, QName member",
        "QName enumeration",
    ];
    for shape in [Shape::Literal, Shape::Alternate, Shape::XsiType] {
        for q_ns in Q_NAMESPACES {
            let verdict = stream(shape, q_ns);
            let others: Vec<&str> = reported_in(&verdict)
                .into_iter()
                .filter(|label| !depends_on_q.contains(label))
                .collect();
            assert_eq!(others, [""; 0], "{shape:?}, q = {q_ns}:\n{verdict}");
        }
    }
    // Those six, written as the literals, are valid where the document's `q`
    // makes the text the value the declaration's `q` made the literal
    // (Structures §3.3.4.3 clause 5.2.2.2.2, §3.2.4.3 clause 4: the ·actual
    // values· are compared, not the spellings). Three are declared where `q`
    // is `urn:b`: their literal is the string member's value, and a document
    // that binds `q` to the enumeration's `urn:a` writes a QName, another
    // value. The other three are declared where `q` is `urn:a`: their
    // literal is the QName, and a document that binds `q` elsewhere writes
    // the string member's value, or no value of the enumeration.
    let declared_elsewhere = [
        "union of QName enumeration",
        "list of that union",
        "union of QName enumeration, spaces",
    ];
    let qnames = [
        "union, QName member, spaces",
        "union, QName member",
        "QName enumeration",
    ];
    for q_ns in Q_NAMESPACES {
        let verdict = stream(Shape::Literal, q_ns);
        let expected = if q_ns == "urn:a" {
            declared_elsewhere
        } else {
            qnames
        };
        assert_eq!(reported_in(&verdict), expected, "q = {q_ns}:\n{verdict}");
    }
    assert_eq!(errors(&stream(Shape::Empty, "urn:a")), 0);

    // Another value: each fixed element and each fixed attribute is reported
    // (three attributes by their uses, one through the wildcard).
    let wrong = stream(Shape::Wrong, "urn:a");
    let fixed_elements = wrong.matches("cvc-elt.5.2.2 ").count();
    let fixed_attributes =
        wrong.matches("cvc-attribute.4 ").count() + wrong.matches("cvc-au ").count();
    let with_attributes = CASES.iter().filter(|c| c.attributes).count();
    assert!(
        fixed_elements >= CASES.len() - 2,
        "{fixed_elements} fixed elements reported of {}:\n{wrong}",
        CASES.len()
    );
    assert!(
        fixed_attributes >= 4 * (with_attributes - 2),
        "{fixed_attributes} fixed attributes reported of {}:\n{wrong}",
        4 * with_attributes
    );
    assert!(wrong.starts_with("root validity: Some(Invalid)"), "{wrong}");

    // A typed build of the empty document has every default as a node.
    let built = observe(
        &ss,
        &document(Shape::Empty, "urn:a"),
        flags,
        Driver::TypedBuild {
            defaults_as_nodes: true,
        },
    )
    .document
    .unwrap();
    assert!(built.contains("@da=[  a  b  ] type=string"), "{built}");
    assert!(built.contains("@da=[a b] type=token"), "{built}");
    assert!(built.contains("id(k1): true"), "{built}");
}

/// The review's regression, by name: a fixed attribute of a union of a QName
/// enumeration and `xs:string`, declaration and instance both selecting the
/// string member, is accepted — also when the schema set has an unrelated
/// assertion.
#[test]
fn a_fixed_union_attribute_is_accepted_whatever_else_the_set_holds() {
    for assertion in [
        "",
        r#"<xs:assert test="true()"/>"#,
        r#"<xs:assert test="count(e) le 1"/>"#,
        r#"<xs:assert test="current-dateTime() eq current-dateTime()"/>"#,
    ] {
        let ss = xsd_schema::SchemaSetBuilder::with_version(xsd_schema::XsdVersion::V1_1)
            .add_source(
                &format!(
                    r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:q="urn:a">
  <xs:simpleType name="QE"><xs:restriction base="xs:QName"><xs:enumeration value="q:k"/></xs:restriction></xs:simpleType>
  <xs:simpleType name="U"><xs:union memberTypes="QE xs:string"/></xs:simpleType>
  <xs:element name="doc"><xs:complexType><xs:sequence>
    <xs:element name="e" type="U" fixed="q:k" xmlns:q="urn:b" minOccurs="0"/>
  </xs:sequence>
  <xs:attribute name="a" type="U" fixed="q:k" xmlns:q="urn:b"/>{assertion}</xs:complexType></xs:element>
</xs:schema>"#
                ),
                "file:///fixed_union.xsd",
            )
            .expect("schema source accepted")
            .compile()
            .expect("schema compiles")
            .into_schema_set();
        for xml in [
            r#"<doc xmlns:q="urn:c" a="q:k"/>"#,
            r#"<doc xmlns:q="urn:c"><e>q:k</e></doc>"#,
            r#"<doc xmlns:q="urn:b" a="q:k"><e>q:k</e></doc>"#,
        ] {
            for (flag_label, flags) in flag_sets() {
                let seen = observe(&ss, xml, flags, Driver::Stream);
                assert_eq!(
                    seen.verdict, "root validity: Some(Valid)\n",
                    "{xml} with {assertion:?}, {flag_label}"
                );
            }
        }
    }
}
