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
//! - Datatypes 1.1 §4.3.6.1: with `collapse`, "all occurrences of #x9 (tab),
//!   #xA (line feed) and #xD (carriage return) are replaced with #x20
//!   (space)", contiguous #x20s are collapsed and "initial and/or final #x20s
//!   are deleted" — no other white space (U+00A0, U+3000, U+2028), which
//!   `str::trim` would also remove.
//! - Structures §4.2.2: `vc:minVersion` and `vc:maxVersion` "are treated as if
//!   declared with type xs:decimal, and their ·actual values· are compared to
//!   a decimal value representing the version of XSD supported by the
//!   processor"; their "·initial value· must be locally ·valid· with respect
//!   to xs:decimal" (Conditional Inclusion Constraints). An element is kept
//!   "if vc:minVersion ≤ V < vc:maxVersion".
//! - F&O 1.0 §17.1.1 (casts from `xs:string` / `xs:untypedAtomic`):
//!   "Whitespace normalization is applied as indicated by the whiteSpace facet
//!   for the datatype. The resulting whitespace-normalized string must be a
//!   valid lexical form for the datatype."

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

fn compile(version: XsdVersion, body: &str) -> Result<SchemaSet, String> {
    let src = format!(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
             xmlns:vc="http://www.w3.org/2007/XMLSchema-versioning">{body}</xs:schema>"#
    );
    SchemaSetBuilder::with_version(version)
        .add_source(&src, "file:///numeric_lexical_literals.xsd")
        .map_err(|e| e.to_string())?
        .compile()
        .map(|c| c.into_schema_set())
        .map_err(|e| e.to_string())
}

fn valid(ss: &SchemaSet, xml: &str) -> bool {
    let v = SchemaValidator::new(ss, ValidationFlags::default());
    let (mut errors, mut warnings) = (Vec::new(), Vec::new());
    let mut rt = v.start_run(CollectingValidationSink {
        errors: &mut errors,
        warnings: &mut warnings,
    });
    drive_quick_xml(xml.as_bytes(), &mut rt, ss)
        .expect("drive")
        .root_validity
        == Some(SchemaValidity::Valid)
}

/// `xs:dateTimeStamp` (XSD 1.1) has the end-of-day rule of `xs:dateTime`.
#[test]
fn date_time_stamp_end_of_day() {
    let ss = compile(
        XsdVersion::V1_1,
        r#"<xs:element name="s" type="xs:dateTimeStamp"/>"#,
    )
    .unwrap();
    assert!(!valid(
        &ss,
        &format!("<s>2000-01-01T24:00:00.{FRAC29}Z</s>")
    ));
    assert!(valid(
        &ss,
        &format!("<s>2000-01-01T24:00:00.{ZEROS40}Z</s>")
    ));
    assert!(valid(&ss, &format!("<s>2000-01-01T23:59:59.{FRAC29}Z</s>")));
}

/// Literals in a schema: facet values, value constraints, occurrence bounds.
#[test]
fn schema_literals_have_the_xsd_lexical_forms() {
    let st = |base: &str, facet: &str| {
        format!(
            r#"<xs:simpleType name="t"><xs:restriction base="xs:{base}">{facet}</xs:restriction></xs:simpleType>"#
        )
    };
    let sequence = |attrs: &str| {
        format!(
            r#"<xs:element name="r"><xs:complexType><xs:sequence>
                 <xs:element name="c" {attrs}/></xs:sequence></xs:complexType></xs:element>"#
        )
    };
    let cases: Vec<(String, bool)> = vec![
        // Through the type's validator.
        (st("integer", r#"<xs:minInclusive value="1_0"/>"#), false),
        (
            st("nonNegativeInteger", r#"<xs:maxExclusive value="1_0"/>"#),
            false,
        ),
        (
            st("positiveInteger", r#"<xs:enumeration value="1_0"/>"#),
            false,
        ),
        (st("integer", r#"<xs:enumeration value="+10"/>"#), true),
        (
            st(
                "time",
                &format!(r#"<xs:enumeration value="24:00:00.{FRAC29}"/>"#),
            ),
            false,
        ),
        (
            st(
                "dateTime",
                &format!(r#"<xs:maxInclusive value="2000-01-01T24:00:00.{FRAC29}"/>"#),
            ),
            false,
        ),
        (
            r#"<xs:element name="e" type="xs:integer" default="1_0"/>"#.into(),
            false,
        ),
        (
            r#"<xs:attribute name="a" type="xs:negativeInteger" fixed="-1_0"/>"#.into(),
            false,
        ),
        (
            format!(r#"<xs:element name="e" type="xs:time" fixed="24:00:00.{FRAC29}"/>"#),
            false,
        ),
        // The integer-valued facets.
        (st("string", r#"<xs:length value="++5"/>"#), false),
        (st("string", r#"<xs:minLength value="++5"/>"#), false),
        (st("string", r#"<xs:maxLength value="5_0"/>"#), false),
        (st("string", "<xs:length value=\"\u{A0}5\"/>"), false),
        (st("string", "<xs:length value=\"5\u{3000}\"/>"), false),
        (st("decimal", r#"<xs:totalDigits value="++5"/>"#), false),
        (st("decimal", r#"<xs:fractionDigits value="++2"/>"#), false),
        (st("string", r#"<xs:length value=" 5 "/>"#), true),
        (st("string", r#"<xs:length value="+5"/>"#), true),
        (st("string", r#"<xs:length value="-0"/>"#), true),
        (st("decimal", r#"<xs:totalDigits value="+5"/>"#), true),
        // Occurrence bounds.
        (sequence(r#"minOccurs="++1""#), false),
        (sequence(r#"maxOccurs="1_0""#), false),
        (sequence("maxOccurs=\"\u{A0}2\""), false),
        (sequence(r#"minOccurs=" 1 ""#), true),
        (sequence(r#"maxOccurs=" unbounded ""#), true),
        (sequence(r#"maxOccurs="+2""#), true),
        (
            sequence(r#"maxOccurs="+79228162514264337593543950335""#),
            true,
        ),
    ];
    let mut wrong = Vec::new();
    for version in VERSIONS {
        for (body, ok) in &cases {
            let got = compile(version, body);
            if got.is_ok() != *ok {
                wrong.push(format!("{version:?} {body}: {:?}", got.err()));
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// `vc:minVersion` / `vc:maxVersion` are `xs:decimal` literals, compared with
/// the processor's version exactly.
#[test]
fn vc_version_bounds_are_exact_decimals() {
    let body = |attr: &str| format!(r#"<xs:element name="e" type="xs:string" {attr}/>"#);
    let mut wrong = Vec::new();
    // (attribute, kept under XSD 1.1)
    for (attr, kept) in [
        (r#"vc:minVersion="1.1""#, true),
        (r#"vc:minVersion="1.10""#, true),
        (r#"vc:minVersion=" +1.1 ""#, true),
        (r#"vc:minVersion="1.0999999999999999999""#, true),
        (r#"vc:minVersion="1.1000000000000000001""#, false),
        (r#"vc:maxVersion="1.1000000000000000001""#, true),
        (r#"vc:maxVersion="1.0999999999999999999""#, false),
        (r#"vc:minVersion=".5""#, true),
    ] {
        match compile(XsdVersion::V1_1, &body(attr)) {
            Ok(ss) if valid(&ss, "<e>x</e>") == kept => {}
            other => wrong.push(format!(
                "{attr}: {:?}",
                other.map(|ss| valid(&ss, "<e>x</e>"))
            )),
        }
    }
    // Not `xs:decimal` literals: a schema error under XSD 1.1, ignored (the
    // element kept) under XSD 1.0.
    for attr in [
        r#"vc:minVersion="inf""#,
        r#"vc:minVersion="NaN""#,
        r#"vc:minVersion="1e0""#,
        r#"vc:maxVersion="9E9""#,
        r#"vc:minVersion="1_0""#,
        "vc:minVersion=\"\u{A0}1.0\"",
    ] {
        if compile(XsdVersion::V1_1, &body(attr)).is_ok() {
            wrong.push(format!("{attr}: accepted under XSD 1.1"));
        }
        match compile(XsdVersion::V1_0, &body(attr)) {
            Ok(ss) if valid(&ss, "<e>x</e>") => {}
            other => wrong.push(format!(
                "{attr} (1.0): {:?}",
                other.map(|ss| valid(&ss, "<e>x</e>"))
            )),
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// An element is kept "if vc:minVersion ≤ V < vc:maxVersion": the upper bound
/// is exclusive, so `vc:maxVersion="1.1"` and `vc:minVersion="1.1"` split the
/// versions without overlap.
#[test]
fn vc_max_version_is_an_exclusive_bound() {
    let two = r#"<xs:element name="e" type="xs:string" vc:maxVersion="1.1"/>
                 <xs:element name="e" type="xs:int" vc:minVersion="1.1"/>"#;
    let mut wrong = Vec::new();
    // (version, `<e>x</e>` valid: the `xs:string` declaration is the one kept)
    for (version, string_kept) in [(XsdVersion::V1_0, true), (XsdVersion::V1_1, false)] {
        match compile(version, two) {
            Ok(ss) => {
                if valid(&ss, "<e>x</e>") != string_kept || !valid(&ss, "<e>1</e>") {
                    wrong.push(format!(
                        "{version:?}: <e>x</e> {}, <e>1</e> {}",
                        valid(&ss, "<e>x</e>"),
                        valid(&ss, "<e>1</e>")
                    ));
                }
            }
            Err(e) => wrong.push(format!("{version:?}: {e}")),
        }
    }

    let body = |attrs: &str| format!(r#"<xs:element name="e" type="xs:string" {attrs}/>"#);
    for (version, v, below) in [
        (XsdVersion::V1_0, "1.0", "0.9999999999999999999"),
        (XsdVersion::V1_1, "1.1", "1.0999999999999999999"),
    ] {
        // (attributes, kept)
        let cases = [
            (format!(r#"vc:maxVersion="{below}""#), false),
            (format!(r#"vc:maxVersion="{v}""#), false),
            (format!(r#"vc:maxVersion="{v}0""#), false),
            (format!(r#"vc:maxVersion="{v}000000000000000001""#), true),
            (format!(r#"vc:minVersion="{below}""#), true),
            (format!(r#"vc:minVersion="{v}""#), true),
            (format!(r#"vc:minVersion="{v}000000000000000001""#), false),
            (format!(r#"vc:minVersion="{v}" vc:maxVersion="{v}""#), false),
            (
                r#"vc:minVersion="1.0" vc:maxVersion="1.1""#.to_string(),
                v == "1.0",
            ),
            (
                r#"vc:minVersion="1.1" vc:maxVersion="1.2""#.to_string(),
                v == "1.1",
            ),
        ];
        for (attrs, kept) in cases {
            match compile(version, &body(&attrs)) {
                Ok(ss) if valid(&ss, "<e>x</e>") == kept => {}
                other => wrong.push(format!(
                    "{version:?} {attrs}: {:?}",
                    other.map(|ss| valid(&ss, "<e>x</e>"))
                )),
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// The public `TypeConverter` reads an `xs:untypedAtomic` with the XSD lexical
/// spaces too.
#[test]
fn type_converter_reads_untyped_values_as_xsd() {
    use xsd_schema::types::value::XmlValue;
    use xsd_schema::types::TypeConverter;
    let tc = TypeConverter::new();
    let u = |s: &str| XmlValue::untyped(s.to_string());
    for bad in ["1_000", "\u{A0}1", "1\u{3000}", "+-1", "0x1"] {
        assert!(tc.to_integer(&u(bad)).is_err(), "to_integer {bad:?}");
        assert!(tc.to_decimal(&u(bad)).is_err(), "to_decimal {bad:?}");
        assert!(tc.to_double(&u(bad)).is_err(), "to_double {bad:?}");
    }
    for bad in ["inf", "infinity", "Infinity", "nan", "-NaN", "+inf"] {
        assert!(tc.to_double(&u(bad)).is_err(), "to_double {bad:?}");
    }
    assert_eq!(tc.to_integer(&u(" +1000\n")).unwrap(), 1000.into());
    assert_eq!(tc.to_decimal(&u("\t1.5 ")).unwrap().to_string(), "1.5");
    assert_eq!(tc.to_double(&u(" 1e3 ")).unwrap(), 1000.0);
    assert!(tc.to_double(&u("-INF")).unwrap().is_infinite());
    assert!(tc.to_double(&u("NaN")).unwrap().is_nan());
    // F&O §17.1.3.4: a double cast to `xs:integer` keeps every digit of its
    // integral part (it saturated at the range of `i64`).
    assert_eq!(
        tc.to_integer(&XmlValue::double(1e20)).unwrap().to_string(),
        "100000000000000000000"
    );
}

#[cfg(feature = "xsd11")]
mod xpath {
    use xsd_schema::namespace::context::NamespaceContextSnapshot;
    use xsd_schema::namespace::table::NameTable;
    use xsd_schema::xpath::{RoXmlNavigator, XPathContext, XPathExpr};
    use xsd_schema::XmlItem;

    fn with_ctx<R>(f: impl FnOnce(&XPathContext<'_>) -> R) -> R {
        let names = NameTable::new();
        let mut namespaces = NamespaceContextSnapshot::default();
        namespaces.bindings.push((
            names.add("xs"),
            names.add("http://www.w3.org/2001/XMLSchema"),
        ));
        let ctx = XPathContext::new(&names).with_namespaces(namespaces);
        f(&ctx)
    }

    fn run(ctx: &XPathContext<'_>, expr: &str) -> String {
        let compiled = match XPathExpr::compile(expr, ctx) {
            Ok(c) => c,
            Err(e) => return format!("ERR:{}", e.error_code().unwrap_or("?")),
        };
        match compiled.evaluator(ctx).run::<RoXmlNavigator<'static>>() {
            Ok(v) => v
                .into_vec()
                .into_iter()
                .map(|item| match item {
                    XmlItem::Atomic(a) => a.to_string_value(),
                    XmlItem::Node(_) => "<node>".to_string(),
                })
                .collect::<Vec<_>>()
                .join(" "),
            Err(e) => format!("ERR:{}", e.error_code().unwrap_or("?")),
        }
    }

    /// The constructor function, `cast as` and `castable as`, from a string
    /// and from an `xs:untypedAtomic`.
    #[test]
    fn casts_read_the_xsd_lexical_spaces() {
        let f29 = super::FRAC29;
        // (target type, literal, castable)
        let rows: Vec<(&str, String, bool)> = vec![
            ("integer", "1_000".into(), false),
            ("integer", "1_".into(), false),
            ("long", "1_000".into(), false),
            ("int", "0_0".into(), false),
            ("unsignedByte", "1__0".into(), false),
            ("positiveInteger", "10_000".into(), false),
            ("integer", "\u{A0}1".into(), false),
            ("integer", "1\u{3000}".into(), false),
            ("byte", "\u{2028}1".into(), false),
            ("decimal", "\u{A0}1.5".into(), false),
            ("double", "\u{A0}1".into(), false),
            ("float", "1\u{3000}".into(), false),
            ("boolean", "\u{A0}true".into(), false),
            ("date", "\u{A0}2000-01-01".into(), false),
            ("duration", "P1D\u{3000}".into(), false),
            ("hexBinary", "\u{A0}0F".into(), false),
            ("time", format!("24:00:00.{f29}"), false),
            ("dateTime", format!("2000-01-01T24:00:00.{f29}Z"), false),
            (
                "dateTimeStamp",
                format!("2000-01-01T24:00:00.{f29}Z"),
                false,
            ),
            // Valid forms stay valid.
            ("integer", " +1000\n".into(), true),
            ("unsignedLong", "-0".into(), true),
            ("decimal", "\t-1.5 ".into(), true),
            ("double", " -INF ".into(), true),
            ("boolean", " true ".into(), true),
            ("date", " 2000-01-01\n".into(), true),
            ("time", format!("24:00:00.{}", super::ZEROS40), true),
            (
                "dateTimeStamp",
                format!("2000-01-01T24:00:00.{}Z", super::ZEROS40),
                true,
            ),
        ];
        let wrong = with_ctx(|ctx| {
            let mut wrong = Vec::new();
            for (ty, lit, ok) in &rows {
                for expr in [
                    format!("'{lit}' castable as xs:{ty}"),
                    format!("xs:untypedAtomic('{lit}') castable as xs:{ty}"),
                ] {
                    if run(ctx, &expr) != ok.to_string() {
                        wrong.push(format!("{expr:?}: {}", run(ctx, &expr)));
                    }
                }
                for expr in [
                    format!("xs:{ty}('{lit}')"),
                    format!("'{lit}' cast as xs:{ty}"),
                ] {
                    let got = run(ctx, &expr);
                    if (got == "ERR:FORG0001") == *ok {
                        wrong.push(format!("{expr:?}: {got}"));
                    }
                }
            }
            wrong
        });
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    /// `run_number` reads a string as `fn:number` does.
    #[test]
    fn run_number_reads_strings_as_fn_number() {
        with_ctx(|ctx| {
            let number = |expr: &str| {
                XPathExpr::compile(expr, ctx)
                    .unwrap()
                    .evaluator(ctx)
                    .run_number::<RoXmlNavigator<'static>>()
                    .unwrap()
            };
            for s in [
                "inf",
                "infinity",
                "Infinity",
                "-infinity",
                "nan",
                "1_0",
                "\u{A0}1",
            ] {
                assert!(number(&format!("'{s}'")).is_nan(), "{s:?}");
                assert!(number(&format!("number('{s}')")).is_nan(), "number({s:?})");
            }
            assert!(number("'INF'").is_infinite());
            // F&O §17.1.3.2: true casts to 1.0E0 and false to 0.0E0.
            assert_eq!(number("true()"), 1.0);
            assert_eq!(number("false()"), 0.0);
            assert_eq!(number("' 1e3 '"), 1000.0);
        });
    }
}
