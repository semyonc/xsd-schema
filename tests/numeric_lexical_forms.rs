//! Lexical forms that a library parser accepts and XSD does not, and checks
//! that must see a literal as written rather than the number it was read as.
//!
//! - Datatypes 1.1 §3.4.13.1: "integer has a lexical representation
//!   consisting of a finite-length sequence of one or more decimal digits
//!   (#x30-#x39) with an optional leading sign." (`noDecimalPtNumeral ::=
//!   ('+' | '-')? unsignedNoDecimalPtNumeral`, `unsignedNoDecimalPtNumeral
//!   ::= digit+`, §D.1.1 \[46\]–\[47\].) `1_000` is not one, although
//!   `num_bigint` reads it as 1000.
//! - Datatypes 1.1 §D.2.2 \[62\]: "endOfDayFrag ::= '24:00:00' ('.' '0'+)?"
//!   — hour 24 only with zero minutes and seconds and a fraction of zeros
//!   only, however many digits it has, also beyond the 28 a `Decimal` keeps.

use xsd_schema::validation::{
    drive_quick_xml, validate_simple_type, CollectingValidationSink, SchemaValidator,
    SchemaValidity, ValidationFlags,
};
use xsd_schema::{SchemaSet, SchemaSetBuilder, TypeKey, XsdVersion};

const VERSIONS: [XsdVersion; 2] = [XsdVersion::V1_0, XsdVersion::V1_1];

const SCHEMA: &str = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="integer" type="xs:integer"/>
  <xs:element name="nonNegativeInteger" type="xs:nonNegativeInteger"/>
  <xs:element name="positiveInteger" type="xs:positiveInteger"/>
  <xs:element name="nonPositiveInteger" type="xs:nonPositiveInteger"/>
  <xs:element name="negativeInteger" type="xs:negativeInteger"/>
  <xs:element name="long" type="xs:long"/>
  <xs:element name="unsignedLong" type="xs:unsignedLong"/>
  <xs:element name="decimal" type="xs:decimal"/>
  <xs:element name="dateTime" type="xs:dateTime"/>
  <xs:element name="time" type="xs:time"/>
</xs:schema>"#;

/// 29 fraction digits, the last one not zero: one more than a `Decimal` keeps.
const FRAC29: &str = "00000000000000000000000000001";
/// 40 zeros: beyond a `Decimal`, and still a fraction of zeros only.
const ZEROS40: &str = "0000000000000000000000000000000000000000";

fn schema(version: XsdVersion) -> SchemaSet {
    SchemaSetBuilder::with_version(version)
        .add_source(SCHEMA, "file:///numeric_lexical_forms.xsd")
        .expect("schema source accepted")
        .compile()
        .unwrap_or_else(|e| panic!("{version:?}: {e}"))
        .into_schema_set()
}

/// (element = type name, literal, valid)
fn rows() -> Vec<(&'static str, String, bool)> {
    let mut rows = Vec::new();
    for ty in [
        "integer",
        "nonNegativeInteger",
        "positiveInteger",
        "long",
        "unsignedLong",
        "decimal",
    ] {
        for bad in ["1_000", "1_", "1__0", "+1_0", "10_000_000"] {
            rows.push((ty, bad.to_string(), false));
        }
        for good in ["1000", "+1000", "0001000"] {
            rows.push((ty, good.to_string(), true));
        }
        // Beyond i128, where only the `BigInt` path decides.
        rows.push((
            ty,
            "1_000000000000000000000000000000000000000".into(),
            false,
        ));
        rows.push((
            ty,
            "1000000000000000000000000000000000000000".into(),
            ty != "long" && ty != "unsignedLong",
        ));
    }
    for ty in ["nonPositiveInteger", "negativeInteger"] {
        for bad in ["-1_000", "-1_", "-1__0"] {
            rows.push((ty, bad.to_string(), false));
        }
        rows.push((ty, "-1000".to_string(), true));
    }
    for (bad, good) in [("-_1", "-1"), ("+_1", "+1")] {
        rows.push(("integer", bad.to_string(), false));
        rows.push(("integer", good.to_string(), true));
    }
    // The end of the day, with a fraction of zeros only or not.
    rows.push(("time", format!("24:00:00.{FRAC29}"), false));
    rows.push(("time", format!("24:00:00.{FRAC29}Z"), false));
    rows.push(("time", format!("24:00:00.{ZEROS40}"), true));
    rows.push(("time", format!("24:00:00.{ZEROS40}+01:00"), true));
    rows.push(("time", "24:00:00.0".to_string(), true));
    rows.push(("time", "24:00:00".to_string(), true));
    rows.push(("time", "24:00:00.1".to_string(), false));
    rows.push(("time", format!("23:59:59.{FRAC29}"), true));
    rows.push(("dateTime", format!("2000-01-01T24:00:00.{FRAC29}"), false));
    rows.push((
        "dateTime",
        format!("1999-12-31T24:00:00.{FRAC29}-05:00"),
        false,
    ));
    rows.push(("dateTime", format!("2000-01-01T24:00:00.{ZEROS40}"), true));
    rows.push(("dateTime", format!("2000-01-01T23:59:59.{FRAC29}"), true));
    rows
}

#[test]
fn numeric_and_end_of_day_lexical_forms() {
    let mut wrong = Vec::new();
    for version in VERSIONS {
        let ss = schema(version);
        let b = ss.builtin_types();
        for (ty, literal, valid) in rows() {
            let key = match ty {
                "integer" => b.integer,
                "nonNegativeInteger" => b.non_negative_integer,
                "positiveInteger" => b.positive_integer,
                "nonPositiveInteger" => b.non_positive_integer,
                "negativeInteger" => b.negative_integer,
                "long" => b.long,
                "unsignedLong" => b.unsigned_long,
                "decimal" => b.decimal,
                "dateTime" => b.datetime,
                "time" => b.time,
                _ => unreachable!(),
            };
            let got = validate_simple_type(&literal, TypeKey::Simple(key), &ss).is_ok();
            if got != valid {
                wrong.push(format!(
                    "{version:?} validate_simple_type({ty}, {literal:?}): {got}"
                ));
            }
            for psvi in [true, false] {
                let mut flags = ValidationFlags::default();
                if !psvi {
                    flags &= !ValidationFlags::BUILD_PSVI_TYPED_VALUES;
                }
                let v = SchemaValidator::new(&ss, flags);
                let (mut errors, mut warnings) = (Vec::new(), Vec::new());
                let root = {
                    let mut rt = v.start_run(CollectingValidationSink {
                        errors: &mut errors,
                        warnings: &mut warnings,
                    });
                    let xml = format!("<{ty}>{literal}</{ty}>");
                    drive_quick_xml(xml.as_bytes(), &mut rt, &ss)
                        .expect("drive")
                        .root_validity
                };
                let got = root == Some(SchemaValidity::Valid);
                if got != valid {
                    wrong.push(format!(
                        "{version:?} psvi={psvi} <{ty}>{literal}</{ty}>: {got} {errors:?}"
                    ));
                }
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// A valid time of day whose seconds have more digits than the typed value
/// keeps still warns; an end of the day written with many zeros does not
/// (its value is exact).
#[test]
fn the_seconds_capacity_warning_stays_for_valid_values() {
    const NOTICE: &str = "xs-seconds-capacity";
    for version in VERSIONS {
        let ss = schema(version);
        for (xml, warned) in [
            (format!("<time>23:59:59.{FRAC29}</time>"), true),
            (
                format!("<dateTime>2000-01-01T23:59:59.{FRAC29}Z</dateTime>"),
                true,
            ),
            (format!("<time>24:00:00.{ZEROS40}</time>"), false),
            (
                format!("<dateTime>2000-01-01T24:00:00.{ZEROS40}</dateTime>"),
                false,
            ),
        ] {
            let v = SchemaValidator::new(&ss, ValidationFlags::default());
            let (mut errors, mut warnings) = (Vec::new(), Vec::new());
            let root = {
                let mut rt = v.start_run(CollectingValidationSink {
                    errors: &mut errors,
                    warnings: &mut warnings,
                });
                drive_quick_xml(xml.as_bytes(), &mut rt, &ss)
                    .expect("drive")
                    .root_validity
            };
            let notices = warnings.iter().filter(|w| w.code == NOTICE).count();
            assert_eq!(
                root,
                Some(SchemaValidity::Valid),
                "{version:?} {xml}: {errors:?}"
            );
            assert_eq!(notices == 1, warned, "{version:?} {xml}: {warnings:?}");
        }
    }
}
