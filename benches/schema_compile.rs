//! Benchmark for the compilation of a schema with a large substitution group:
//! one head, many members, and many complex types whose content refers to
//! the head — the shape of an XBRL taxonomy, where thousands of concepts are
//! in the substitution group of `xbrli:item`.
//!
//! Run the full sweep with
//!
//! ```text
//! cargo bench --bench schema_compile --features xsd11
//! ```
//!
//! Without `--bench` (for instance under `cargo test --all-targets`) it does a
//! small smoke pass instead, so that the target is still type-checked and
//! exercised without spending minutes on it.
//!
//! Each row gives two times: `compile`, from the schema text to a compiled
//! `SchemaSet` (parsing, resolution, the schema-level constraints), and
//! `validator`, the construction of a `SchemaValidator` with every content
//! model compiled. A host that compiles a slightly different set of schemas
//! for every document — a conformance suite, a taxonomy with per-filing
//! extensions — pays both each time.
//!
//! The rows:
//!
//! * `ref`: each type has a choice of the head and a local element. Nothing
//!   reads the names of the members here, so no content model pays for them
//!   and the validator costs next to nothing. What is left in `compile` is
//!   the Unique Particle Attribution check, which has the name of every
//!   member for each reference to the head.
//! * `sibling`: each type has the head next to a wildcard with
//!   `notQName="##definedSibling"` (XSD 1.1), which excludes the name of
//!   every member of the group. The names are collected once per type, in
//!   time proportional to the size of the group.
//!
//! A row is at most proportional to the members times the types: doubling
//! the members must double it, not quadruple it.

use std::fmt::Write;
use std::time::{Duration, Instant};

use xsd_schema::validation::{SchemaValidator, ValidationFlags};
use xsd_schema::SchemaSetBuilder;

/// The head, `members` members of its group and `types` complex types that
/// refer to the head, each with an element of that type.
fn schema(members: usize, types: usize, sibling: bool) -> String {
    let mut text = String::from(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:t="urn:t"
    targetNamespace="urn:t" elementFormDefault="qualified">
<xs:element name="head" type="xs:string" abstract="true"/>
"#,
    );
    for i in 0..members {
        writeln!(
            text,
            r#"<xs:element name="m{i}" type="xs:string" substitutionGroup="t:head"/>"#
        )
        .unwrap();
    }
    for i in 0..types {
        let content = if sibling {
            concat!(
                r#"<xs:element ref="t:head" minOccurs="0" maxOccurs="unbounded"/>"#,
                r###"<xs:any minOccurs="0" namespace="##other" notQName="##definedSibling" processContents="lax"/>"###
            )
            .to_string()
        } else {
            format!(
                r#"<xs:choice minOccurs="0" maxOccurs="unbounded"><xs:element ref="t:head"/><xs:element name="other{i}" type="xs:string"/></xs:choice>"#
            )
        };
        writeln!(
            text,
            r#"<xs:complexType name="c{i}"><xs:sequence><xs:element name="first" type="xs:string" minOccurs="0"/>{content}</xs:sequence></xs:complexType>
<xs:element name="e{i}" type="t:c{i}"/>"#
        )
        .unwrap();
    }
    text.push_str("</xs:schema>");
    text
}

/// The shortest of `runs` compilations of `text`: the schema set, then the
/// validator.
fn time(text: &str, xsd11: bool, runs: usize) -> (Duration, Duration) {
    let (mut compile, mut validator) = (Duration::MAX, Duration::MAX);
    for _ in 0..runs {
        let builder = if xsd11 {
            SchemaSetBuilder::xsd11()
        } else {
            SchemaSetBuilder::new()
        };
        let started = Instant::now();
        let compiled = builder
            .add_source(text, "file:///schema_compile.xsd")
            .and_then(|builder| builder.compile())
            .expect("the schema compiles");
        compile = compile.min(started.elapsed());

        let schema_set = compiled.schema_set();
        let started = Instant::now();
        let built = SchemaValidator::new(schema_set, ValidationFlags::default());
        let failures = built.content_model_failures().len();
        validator = validator.min(started.elapsed());
        assert_eq!(failures, 0, "every content model compiles");
    }
    (compile, validator)
}

fn row(label: &str, members: usize, types: usize, xsd11: bool, runs: usize) {
    let text = schema(members, types, label == "sibling");
    let (compile, validator) = time(&text, xsd11, runs);
    println!(
        "  {label:<8} XSD {}  {members:>6} members {types:>5} types   compile {:>9.2} ms   validator {:>9.2} ms",
        if xsd11 { "1.1" } else { "1.0" },
        compile.as_secs_f64() * 1e3,
        validator.as_secs_f64() * 1e3,
    );
}

fn main() {
    let full = std::env::args().any(|arg| arg == "--bench");
    let (sizes, runs): (&[(usize, usize)], usize) = if full {
        (&[(1_000, 100), (2_000, 100), (4_000, 100), (4_000, 400)], 5)
    } else {
        (&[(40, 4)], 1)
    };

    println!("== a reference to the head of a large substitution group ==");
    for &(members, types) in sizes {
        row("ref", members, types, false, runs);
        row("ref", members, types, true, runs);
    }

    println!();
    println!("== the head next to a ##definedSibling wildcard ==");
    for &(members, types) in sizes {
        row("sibling", members, types, true, runs);
    }
}
