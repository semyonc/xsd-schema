//! Unit tests of the plan: small schemas, the plan's verdict against the
//! dynamic validator's, and the tripwires.

use super::*;
use crate::pipeline::load_and_process_schema;
use crate::schema::SchemaSet;
use crate::validation::{SchemaValidator, ValidationFlags};

fn load(xsd: &str) -> SchemaSet {
    let mut set = SchemaSet::new();
    load_and_process_schema(xsd.as_bytes(), "test.xsd", &mut set, None).expect("schema");
    set
}

/// The two verdicts of a document: (shipped, delegating), each `true` for
/// `Valid`. Asserts the one property: `Valid` only where the dynamic
/// validator accepts.
fn verdicts(set: &SchemaSet, plan: &PlanData, doc: &str) -> (bool, bool) {
    let validator = SchemaValidator::new(set, ValidationFlags::default());
    let dynamic = dynamic_valid(&validator, doc.as_bytes());
    let mut scratch = Scratch::new();
    let shipped = check(plan.as_ref(), doc.as_bytes(), &mut scratch) == Verdict::Valid;
    let mut tier = Delegating::new(set, plan);
    let delegating =
        check_with(plan.as_ref(), doc.as_bytes(), &mut scratch, &mut tier) == Verdict::Valid;
    assert!(!shipped || dynamic, "false Valid (shipped): {doc}");
    assert!(!delegating || dynamic, "false Valid (delegating): {doc}");
    (shipped, delegating)
}

const ORDER: &str = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
    xmlns:o="urn:order" targetNamespace="urn:order" elementFormDefault="qualified">
  <xs:element name="order">
    <xs:complexType>
      <xs:sequence>
        <xs:element name="id" type="o:Code"/>
        <xs:choice>
          <xs:element name="person" type="xs:string"/>
          <xs:element name="company" type="o:Name"/>
        </xs:choice>
        <xs:element name="line" type="o:Line" minOccurs="1" maxOccurs="3"/>
        <xs:element name="note" type="xs:token" minOccurs="0"/>
      </xs:sequence>
      <xs:attribute name="kind" type="o:Kind" use="required"/>
      <xs:attribute name="ref" type="xs:string"/>
    </xs:complexType>
  </xs:element>
  <xs:complexType name="Line">
    <xs:sequence>
      <xs:element name="sku" type="o:Code"/>
      <xs:element name="qty" type="xs:positiveInteger"/>
    </xs:sequence>
  </xs:complexType>
  <xs:simpleType name="Code">
    <xs:restriction base="xs:string">
      <xs:pattern value="[A-Z]{2}\d{3}"/>
    </xs:restriction>
  </xs:simpleType>
  <xs:simpleType name="Name">
    <xs:restriction base="xs:token">
      <xs:minLength value="2"/>
      <xs:maxLength value="5"/>
    </xs:restriction>
  </xs:simpleType>
  <xs:simpleType name="Kind">
    <xs:restriction base="xs:token">
      <xs:enumeration value="retail"/>
      <xs:enumeration value="trade"/>
    </xs:restriction>
  </xs:simpleType>
</xs:schema>"#;

fn order(body: &str, kind: &str) -> String {
    format!(r#"<order xmlns="urn:order" kind="{kind}">{body}</order>"#)
}

const LINE: &str = "<line><sku>AB123</sku><qty>2</qty></line>";

#[test]
fn structure_names_and_string_values() {
    let set = load(ORDER);
    let (plan, report) = build(&set);
    assert!(
        report.patterns_refused.is_empty(),
        "{:?}",
        report.patterns_refused
    );
    let doc = order(
        &format!("<id>XY001</id><person>Ann</person>{LINE}"),
        "retail",
    );
    assert_eq!(verdicts(&set, &plan, &doc), (true, true));
    for doc in [
        order(
            &format!("<id>XY001</id><company> ACME </company>{LINE}{LINE}<note>n</note>"),
            "trade",
        ),
        format!(
            r#"<o:order xmlns:o="urn:order" kind=" trade "><o:id>XY001</o:id><o:person/>{}</o:order>"#,
            LINE.replace("<", "<o:").replace("<o:/", "</o:")
        ),
    ] {
        assert_eq!(verdicts(&set, &plan, &doc), (true, true), "{doc}");
    }
    // Not valid: every one must stay Unknown.
    for doc in [
        order(
            &format!("<id>XY01</id><person>Ann</person>{LINE}"),
            "retail",
        ),
        order(
            &format!("<id>XY001</id><person>Ann</person>{LINE}"),
            "other",
        ),
        order(
            &format!("<id>XY001</id><company>A</company>{LINE}"),
            "retail",
        ),
        order(
            &format!("<id>XY001</id><company>ABCDEF</company>{LINE}"),
            "retail",
        ),
        order(
            &format!("<id>XY001</id><person>Ann</person><company>AC</company>{LINE}"),
            "retail",
        ),
        order(
            &format!("<id>XY001</id><person>Ann</person>{LINE}{LINE}{LINE}{LINE}"),
            "retail",
        ),
        order("<id>XY001</id><person>Ann</person>", "retail"),
        format!(r#"<order xmlns="urn:order"><id>XY001</id><person>A</person>{LINE}</order>"#),
        format!(
            r#"<order xmlns="urn:order" kind="retail" x="1"><id>XY001</id><person>A</person>{LINE}</order>"#
        ),
        format!(
            r#"<order xmlns="urn:other" kind="retail"><id>XY001</id><person>A</person>{LINE}</order>"#
        ),
        order(
            &format!(
                "<id>XY001</id><person>Ann</person>{}",
                LINE.replace(">2<", ">0<")
            ),
            "retail",
        ),
        order(
            &format!("<id>XY001</id>x<person>Ann</person>{LINE}"),
            "retail",
        ),
    ] {
        assert!(!verdicts(&set, &plan, &doc).1, "{doc}");
    }
}

const STRINGS: &str = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="r">
    <xs:complexType>
      <xs:sequence>
        <xs:element name="s" type="xs:string" maxOccurs="unbounded"/>
      </xs:sequence>
      <xs:attribute name="a" type="xs:string"/>
    </xs:complexType>
  </xs:element>
</xs:schema>"#;

#[test]
fn string_family_as_shipped() {
    let set = load(STRINGS);
    let (plan, _) = build(&set);
    for doc in [
        "<r><s>x</s></r>",
        "<?xml version=\"1.0\" encoding=\"utf-8\"?><r a='1'>\n  <s>a &amp; b</s><s/><!-- c --><?pi x?></r>",
        "<r><s>&#x1F600;é</s></r>",
        "\u{FEFF}<r><s>x</s></r>",
    ] {
        assert_eq!(verdicts(&set, &plan, doc), (true, true), "{doc}");
    }
    // Unknown, though valid: constructs the plan leaves to the dynamic path.
    for doc in [
        "<!DOCTYPE r><r><s>x</s></r>",
        "<?xml version=\"1.0\" encoding=\"ISO-8859-1\"?><r><s>x</s></r>",
        "<?xml version=\"1.1\"?><r><s>x</s></r>",
        "<r><s><![CDATA[x]]></s></r>",
        "<r><s>a<!--c-->b</s></r>",
        "<r xmlns:xsi='http://www.w3.org/2001/XMLSchema-instance'><s xsi:noNamespaceSchemaLocation='x.xsd'>x</s></r>",
        "<r xmlns:xsi='http://www.w3.org/2001/XMLSchema-instance' xsi:schemaLocation='urn:a'><s>x</s></r>",
        "<r xmlns:xsi='http://www.w3.org/2001/XMLSchema-instance' xsi:type='xs:string'><s>x</s></r>",
        "<r xml:lang='en'><s>x</s></r>",
    ] {
        assert_eq!(verdicts(&set, &plan, doc), (false, false), "{doc}");
    }
    // A schema-location hint on the root, checked as the runtime checks it.
    for doc in [
        "<r xmlns:xsi='http://www.w3.org/2001/XMLSchema-instance' xsi:noNamespaceSchemaLocation='x.xsd'><s>x</s></r>",
        "<r xmlns:xsi='http://www.w3.org/2001/XMLSchema-instance' xsi:schemaLocation='urn:a a.xsd urn:b b.xsd'><s>x</s></r>",
        "<r xmlns:xsi='http://www.w3.org/2001/XMLSchema-instance' xsi:schemaLocation=' urn:a\n\ta.xsd '><s>x</s></r>",
    ] {
        assert_eq!(verdicts(&set, &plan, doc), (true, true), "{doc}");
    }
    for doc in [
        "<r></r>",
        "<r><s>x</s><t/></r>",
        "<r><s><t/></s></r>",
        "<r><s>x</s>",
        "<r><s>x</s></r><r/>",
    ] {
        assert!(!verdicts(&set, &plan, doc).1, "{doc}");
    }
}

#[test]
fn scratch_reuse_leaves_no_state() {
    let set = load(STRINGS);
    let (plan, _) = build(&set);
    let mut scratch = Scratch::new();
    assert!(matches!(
        check(plan.as_ref(), b"<r><s>x</s><s><t/></s></r>", &mut scratch),
        Verdict::Unknown(_)
    ));
    assert_eq!(
        check(plan.as_ref(), b"<r><s>x</s></r>", &mut scratch),
        Verdict::Valid
    );
    assert!(matches!(
        check(plan.as_ref(), b"<r><s>x", &mut scratch),
        Verdict::Unknown(_)
    ));
    assert_eq!(
        check(plan.as_ref(), b"<r><s>y</s></r>", &mut scratch),
        Verdict::Valid
    );
}

const IDS: &str = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="r">
    <xs:complexType>
      <xs:sequence>
        <xs:element name="i" maxOccurs="unbounded">
          <xs:complexType>
            <xs:attribute name="id" type="xs:ID"/>
            <xs:attribute name="n" type="xs:string"/>
            <xs:attribute name="d" type="xs:decimal"/>
          </xs:complexType>
        </xs:element>
        <xs:element name="v" type="xs:string" default="dflt" minOccurs="0"/>
      </xs:sequence>
    </xs:complexType>
    <xs:unique name="u"><xs:selector xpath="./*"/><xs:field xpath="@n"/></xs:unique>
  </xs:element>
</xs:schema>"#;

#[test]
fn identity_and_defaults() {
    let set = load(IDS);
    let (plan, report) = build(&set);
    assert!(report.uncovered.is_empty(), "{:?}", report.uncovered);
    for doc in [
        "<r><i id='a' n='1'/><i id='b' n='2' d='1.50'/><v>x</v></r>",
        "<r><i id=' a '/><i id='b'/><v/></r>",
        "<r><i/><v></v></r>",
    ] {
        assert_eq!(verdicts(&set, &plan, doc), (true, true), "{doc}");
    }
    for doc in [
        "<r><i id='a'/><i id='a'/></r>",
        "<r><i id='a'/><i id=' a'/></r>",
        "<r><i n='1'/><i n='1'/></r>",
        "<r><i id='1a'/></r>",
        "<r><i d='x'/></r>",
    ] {
        assert!(!verdicts(&set, &plan, doc).1, "{doc}");
    }
}

#[test]
fn unique_compares_normalized_values() {
    // A token field: "a b" and "a  b" are one value.
    let xsd = IDS.replace(
        r#"name="n" type="xs:string""#,
        r#"name="n" type="xs:token""#,
    );
    let set = load(&xsd);
    let (plan, report) = build(&set);
    assert!(report.uncovered.is_empty(), "{:?}", report.uncovered);
    assert_eq!(
        verdicts(&set, &plan, "<r><i n='a b'/><i n='a  c'/></r>"),
        (true, true)
    );
    assert_eq!(
        verdicts(&set, &plan, "<r><i n='a b'/><i n='a  b'/></r>"),
        (false, false)
    );
    assert_eq!(
        verdicts(&set, &plan, "<r><i n=' a'/><i n='a'/></r>"),
        (false, false)
    );
}

#[test]
fn unique_over_a_decimal_attribute_is_uncovered() {
    // 1.0 and 1.00 are one decimal: literal equality is not value equality.
    let xsd = IDS.replace(
        r#"name="n" type="xs:string""#,
        r#"name="n" type="xs:decimal""#,
    );
    let set = load(&xsd);
    let (plan, report) = build(&set);
    assert!(
        report.uncovered.iter().any(|u| u.contains("xs:unique")),
        "{:?}",
        report.uncovered
    );
    assert_eq!(
        verdicts(&set, &plan, "<r><i n='1.0'/><i n='1.00'/></r>"),
        (false, false)
    );
}

#[test]
fn patterns_against_their_alphabet() {
    let xsd = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
      <xs:element name="r">
        <xs:complexType><xs:sequence>
          <xs:element name="w" maxOccurs="unbounded">
            <xs:simpleType><xs:restriction base="xs:string"><xs:pattern value="\S+\s\i\c*"/></xs:restriction></xs:simpleType>
          </xs:element>
        </xs:sequence></xs:complexType>
      </xs:element></xs:schema>"#;
    let set = load(xsd);
    let (plan, _) = build(&set);
    assert_eq!(verdicts(&set, &plan, "<r><w>ab x1</w></r>"), (true, true));
    // U+00A0 is `\S` for XSD and `\s` for `regex`; `é` is `\c` for XSD only.
    for doc in [
        "<r><w>a\u{A0}b x</w></r>",
        "<r><w>ab é</w></r>",
        "<r><w>ab\u{2003}x</w></r>",
    ] {
        let (shipped, _) = verdicts(&set, &plan, doc);
        assert!(!shipped, "{doc}");
    }
}

#[test]
fn plan_data_is_send_and_sync() {
    fn sync<T: Send + Sync>() {}
    sync::<PlanData>();
    sync::<PlanRef<'static>>();
}

/// A name with two declarations, one of them still being built when the
/// other content model is determinized: its placeholder must not pass as
/// equal to the other (uncovered) declaration.
#[test]
fn a_child_plan_being_built_is_equal_to_nothing() {
    let xsd = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
      <xs:group name="G"><xs:sequence><xs:element name="a" type="T"/></xs:sequence></xs:group>
      <xs:element name="r"><xs:complexType><xs:group ref="G"/></xs:complexType></xs:element>
      <xs:element name="a" type="Missing"/>
      <xs:complexType name="T">
        <xs:sequence minOccurs="0"><xs:group ref="G"/><xs:element ref="a"/></xs:sequence>
      </xs:complexType>
    </xs:schema>"#;
    let set = load(xsd);
    let (plan, _) = build(&set);
    for doc in ["<r><a><a/><a/></a></r>", "<r><a/></r>"] {
        verdicts(&set, &plan, doc);
    }
}

/// Simple content whose values the runtime also checks at document level or
/// against namespace bindings (IDs, references, QNames, unions that may hold
/// them) is not delegated whole.
#[test]
fn simple_content_of_ids_references_and_qnames_is_uncovered() {
    for (base, docs) in [
        (
            "xs:ID",
            &["<r><e>a</e><e>a</e></r>", "<r><e>a</e><e>b</e></r>"][..],
        ),
        ("xs:IDREF", &["<r><e>nowhere</e></r>"][..]),
        ("xs:IDREFS", &["<r><e>a b</e></r>"][..]),
        ("xs:QName", &["<r><e>p:x</e></r>", "<r><e>x</e></r>"][..]),
        ("U", &["<r><e>q:x</e></r>"][..]),
    ] {
        let xsd = format!(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
              <xs:simpleType name="U"><xs:union memberTypes="xs:QName xs:int"/></xs:simpleType>
              <xs:complexType name="C"><xs:simpleContent><xs:extension base="{base}">
                <xs:attribute name="x" type="xs:string"/>
              </xs:extension></xs:simpleContent></xs:complexType>
              <xs:complexType name="R"><xs:simpleContent><xs:restriction base="C"/></xs:simpleContent></xs:complexType>
              <xs:element name="r"><xs:complexType><xs:sequence>
                <xs:element name="e" type="R" maxOccurs="unbounded"/>
              </xs:sequence></xs:complexType></xs:element>
            </xs:schema>"#
        );
        let set = load(&xsd);
        let (plan, _) = build(&set);
        for doc in docs {
            assert_eq!(verdicts(&set, &plan, doc), (false, false), "{base}: {doc}");
        }
    }
}

/// A subtrahend written like an ASCII class (`[:^alpha:]`)
/// was read as one by the `regex` crate, so the table accepted `a`, which
/// XSD (and regexml, the dynamic path with `xsd11`) refuses. In an `xsd11`
/// build this asserts against regexml.
#[test]
fn a_subtrahend_that_looks_like_an_ascii_class_is_its_characters() {
    let xsd = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
      <xs:simpleType name="T"><xs:restriction base="xs:string">
        <xs:pattern value="[a-z-[:^alpha:]]"/>
      </xs:restriction></xs:simpleType>
      <xs:element name="r"><xs:complexType><xs:sequence>
        <xs:element name="v" type="T" minOccurs="0"/>
      </xs:sequence><xs:attribute name="a" type="T"/></xs:complexType></xs:element>
    </xs:schema>"#;
    for set in [load(xsd), {
        let mut set = SchemaSet::xsd11();
        load_and_process_schema(xsd.as_bytes(), "test.xsd", &mut set, None).expect("schema");
        set
    }] {
        let (plan, _) = build(&set);
        for c in ["a", "l", "p", "h", ":", "^"] {
            assert_eq!(
                verdicts(&set, &plan, &format!("<r><v>{c}</v></r>")),
                (false, false),
                "{c}"
            );
            assert_eq!(
                verdicts(&set, &plan, &format!("<r a='{c}'/>")),
                (false, false),
                "{c}"
            );
        }
        assert_eq!(verdicts(&set, &plan, "<r><v>b</v></r>"), (true, true));
        assert_eq!(verdicts(&set, &plan, "<r a='z'/>"), (true, true));
    }
}

/// XSD 1.0 lets an `<attribute use="prohibited">` carry a fixed value, and
/// the runtime validates a present attribute of that name against that use
/// (type and fixed value), while the attribute-use walk skips it and takes a
/// base type's or a group's use instead. The plan must follow what a present
/// attribute is validated against, so such a type is not covered. Each case:
/// the types, the root's type, documents; `verdicts` asserts that no `Valid`
/// contradicts the runtime.
fn prohibited_fixed_case(types: &str, root: &str, docs: &[&str]) {
    let xsd = format!(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"><xs:element name="r" type="{root}"/>{types}</xs:schema>"#
    );
    let set = load(&xsd);
    let (plan, _) = build(&set);
    for doc in docs {
        verdicts(&set, &plan, doc);
    }
}

const PF_BASE: &str =
    r#"<xs:complexType name="B"><xs:attribute name="a" type="xs:string"/></xs:complexType>"#;

fn pf_extension(ty: &str, fixed: &str) -> String {
    format!(
        r#"<xs:complexType name="D"><xs:complexContent><xs:extension base="B">
             <xs:attribute name="a" type="{ty}" use="prohibited" fixed="{fixed}"/>
           </xs:extension></xs:complexContent></xs:complexType>"#
    )
}

const PF_DOCS: &[&str] = &["<r/>", "<r a='x'/>", "<r a='wrong'/>"];

#[test]
fn prohibited_fixed_in_an_extension() {
    prohibited_fixed_case(
        &format!("{PF_BASE}{}", pf_extension("xs:string", "x")),
        "D",
        PF_DOCS,
    );
}

#[test]
fn prohibited_fixed_beside_a_group_use() {
    prohibited_fixed_case(
        r#"<xs:attributeGroup name="G"><xs:attribute name="a" type="xs:string"/></xs:attributeGroup>
           <xs:complexType name="D"><xs:attribute name="a" type="xs:string" use="prohibited" fixed="x"/>
             <xs:attributeGroup ref="G"/></xs:complexType>"#,
        "D",
        PF_DOCS,
    );
}

#[test]
fn prohibited_fixed_two_extensions_down() {
    prohibited_fixed_case(
        &format!(
            r#"{PF_BASE}{}<xs:complexType name="E"><xs:complexContent><xs:extension base="D">
                 <xs:attribute name="b" type="xs:string"/></xs:extension></xs:complexContent></xs:complexType>"#,
            pf_extension("xs:string", "x")
        ),
        "E",
        PF_DOCS,
    );
}

#[test]
fn prohibited_fixed_of_another_type() {
    prohibited_fixed_case(
        &format!("{PF_BASE}{}", pf_extension("xs:int", "5")),
        "D",
        &["<r a='5'/>", "<r a='abc'/>"],
    );
}

#[test]
fn prohibited_fixed_over_a_base_default() {
    prohibited_fixed_case(
        &format!(
            r#"<xs:complexType name="B"><xs:attribute name="a" type="xs:string" default="d"/></xs:complexType>{}"#,
            pf_extension("xs:string", "x")
        ),
        "D",
        &["<r/>", "<r a='d'/>", "<r a='x'/>", "<r a='wrong'/>"],
    );
}

#[test]
fn prohibited_fixed_in_a_restriction_or_without_a_base() {
    prohibited_fixed_case(
        &format!(
            r#"{PF_BASE}<xs:complexType name="D"><xs:complexContent><xs:restriction base="B">
                 <xs:attribute name="a" type="xs:string" use="prohibited" fixed="x"/>
               </xs:restriction></xs:complexContent></xs:complexType>"#
        ),
        "D",
        PF_DOCS,
    );
    prohibited_fixed_case(
        r#"<xs:complexType name="D"><xs:attribute name="a" type="xs:string" use="prohibited" fixed="x"/></xs:complexType>"#,
        "D",
        PF_DOCS,
    );
}

// ---------------------------------------------------------------------------
// Value checks
// ---------------------------------------------------------------------------

/// A small xorshift generator for the lexical differentials.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn pick<'a>(&mut self, v: &[&'a str]) -> &'a str {
        v[(self.next() % v.len() as u64) as usize]
    }
}

/// Whether the dynamic path's validator for `code` accepts `v` (with the XSD
/// 1.0 rules of `validate_atomic_type`: no year 0000, no `+INF`).
fn runtime_accepts(code: crate::types::XmlTypeCode, v: &str, xsd10: bool) -> bool {
    use crate::types::validators::VALIDATOR_REGISTRY;
    use crate::types::value::{XmlAtomicValue as A, XmlValueKind as K};
    let Ok(val) = VALIDATOR_REGISTRY.validate(code, v) else {
        return false;
    };
    if xsd10 {
        let year_zero = match &val.value {
            K::Atomic(A::DateTime(d)) => d.year == 0,
            K::Atomic(A::Date(d)) => d.year == 0,
            K::Atomic(A::GYear(d)) => d.year == 0,
            K::Atomic(A::GYearMonth(d)) => d.year == 0,
            _ => false,
        };
        let collapsed = crate::types::facets::normalize_whitespace(
            v,
            crate::types::facets::WhitespaceMode::Collapse,
        );
        if year_zero
            || (matches!(
                code,
                crate::types::XmlTypeCode::Float | crate::types::XmlTypeCode::Double
            ) && collapsed == "+INF")
        {
            return false;
        }
    }
    true
}

/// A `Value` check of one lexical family with no facets.
fn bare_check(lex: model::Lex, xsd10: bool) -> (PlanData, model::CheckPlan) {
    let mut plan = PlanData::default();
    plan.bounds.push(model::Bounds {
        min_incl: model::Bound::NONE,
        min_excl: model::Bound::NONE,
        max_incl: model::Bound::NONE,
        max_excl: model::Bound::NONE,
        int_lo: i128::MIN,
        int_hi: i128::MAX,
        total_digits: model::NONE,
        fraction_digits: model::NONE,
        zoned: false,
    });
    let c = model::CheckPlan {
        kind: model::CheckKind::Value,
        lex,
        ws: model::Ws::Collapse,
        flags: if xsd10 { model::CHECK_XSD10 } else { 0 },
        min_len: 0,
        max_len: model::NONE,
        enum_start: model::NONE,
        enum_end: model::NONE,
        pat_start: 0,
        pat_end: 0,
        bounds: if lex == model::Lex::Integer {
            0
        } else {
            model::NONE
        },
        next: model::NONE,
        delegate: model::NONE,
    };
    (plan, c)
}

/// Every string the plan reads in a family's lexical space, the runtime's
/// validator reads too (in both XSD versions).
#[test]
fn plan_lexical_forms_lie_inside_the_runtimes() {
    use crate::types::XmlTypeCode as C;
    use model::Lex as L;
    let mut r = Rng(0x2545_F491_4F6C_DD1D);
    let years = [
        "0000", "0001", "1900", "1970", "2000", "2004", "2100", "9999", "10000", "-0001", "200",
        "02000", "+2000",
    ];
    let months = ["00", "01", "02", "04", "12", "13", "1", "001"];
    let days = ["00", "01", "28", "29", "30", "31", "32", "1"];
    let hours = ["00", "09", "23", "24", "25", "1", "000"];
    let minutes = ["00", "01", "59", "60", "5"];
    let seconds = [
        "00",
        "01",
        "59",
        "60",
        "00.5",
        "00.000",
        "59.999999999999999999999999999999",
        "00.",
        "1",
        "00.1.2",
        "0.5",
        ".5",
    ];
    let zones = [
        "", "Z", "+00:00", "-00:00", "+14:00", "-14:00", "+14:01", "+13:59", "+15:00", "+05", "z",
        "+05:60", "-1:00", "Z Z",
    ];
    let mut cases: Vec<(L, C, String)> = Vec::new();
    for _ in 0..40_000 {
        let (y, m, d) = (r.pick(&years), r.pick(&months), r.pick(&days));
        let (h, mi, s, z) = (
            r.pick(&hours),
            r.pick(&minutes),
            r.pick(&seconds),
            r.pick(&zones),
        );
        cases.push((
            L::DateTime,
            C::DateTime,
            format!("{y}-{m}-{d}T{h}:{mi}:{s}{z}"),
        ));
        cases.push((L::Date, C::Date, format!("{y}-{m}-{d}{z}")));
        cases.push((L::Time, C::Time, format!("{h}:{mi}:{s}{z}")));
        cases.push((L::GYearMonth, C::GYearMonth, format!("{y}-{m}{z}")));
        cases.push((L::GYear, C::GYear, format!("{y}{z}")));
        cases.push((L::GMonthDay, C::GMonthDay, format!("--{m}-{d}{z}")));
        cases.push((L::GDay, C::GDay, format!("---{d}{z}")));
        cases.push((L::GMonth, C::GMonth, format!("--{m}{z}")));
    }
    let num = [
        "",
        "0",
        "1",
        "00",
        "999999999",
        "4294967296",
        "0000000001",
        "1.5",
        ".5",
        "5.",
        "99999999999999999999",
        "999999999999999999",
    ];
    let des = ["Y", "M", "D", "H", "S", "W", ""];
    for _ in 0..40_000 {
        let mut s = String::from(r.pick(&["P", "-P", "+P", "", "PT", "-PT", "pT"]));
        for _ in 0..(r.next() % 5) {
            if r.next().is_multiple_of(5) {
                s.push('T');
            }
            s.push_str(r.pick(&num));
            s.push_str(r.pick(&des));
        }
        cases.push((L::Duration, C::Duration, s.clone()));
        cases.push((L::YearMonthDuration, C::YearMonthDuration, s.clone()));
        cases.push((L::DayTimeDuration, C::DayTimeDuration, s));
    }
    let digits = [
        "", "0", "1", "9", "00", "12", "007", "1e3", "1E-2", "e", ".", "INF", "NaN", "_", " ", "x",
    ];
    for _ in 0..40_000 {
        let s = format!(
            "{}{}{}{}",
            r.pick(&["", "+", "-", "--"]),
            r.pick(&digits),
            r.pick(&["", ".", ".0", ".5", "..", "e5", "E+1", "e-1"]),
            r.pick(&digits)
        );
        for (lex, code) in [
            (L::Decimal, C::Decimal),
            (L::Integer, C::Integer),
            (L::Float, C::Float),
            (L::Double, C::Double),
            (L::Boolean, C::Boolean),
        ] {
            cases.push((lex, code, s.clone()));
        }
    }
    for s in [
        "true", "false", "1", "0", " true ", "TRUE", "", "+INF", "INF", "-INF", "NaN", "1e999",
        "-0",
    ] {
        for (lex, code) in [
            (L::Boolean, C::Boolean),
            (L::Float, C::Float),
            (L::Double, C::Double),
        ] {
            cases.push((lex, code, s.to_string()));
        }
    }
    let b64 = [
        "A", "Q", "g", "w", "+", "/", "=", " ", "a", "Z", "9", "-", "_",
    ];
    let hex = ["0", "a", "F", "g", " ", "x", "00"];
    for _ in 0..40_000 {
        let n = r.next() % 14;
        let s: String = (0..n).map(|_| r.pick(&b64)).collect();
        cases.push((L::Base64Binary, C::Base64Binary, s));
        let n = r.next() % 7;
        let s: String = (0..n).map(|_| r.pick(&hex)).collect();
        cases.push((L::HexBinary, C::HexBinary, s));
    }
    let names = [
        "a", "1", "-", ".", ":", "_", "ä", "\u{b7}", "\u{300}", " ", "x",
    ];
    for _ in 0..20_000 {
        let n = r.next() % 4;
        let s: String = (0..n).map(|_| r.pick(&names)).collect();
        for (lex, code) in [
            (L::Name, C::Name),
            (L::NCName, C::NCName),
            (L::NmToken, C::NmToken),
            (L::Language, C::Language),
        ] {
            cases.push((lex, code, s.clone()));
        }
    }
    let mut accepted = 0;
    for xsd10 in [false, true] {
        for (lex, code, v) in &cases {
            let (plan, c) = bare_check(*lex, xsd10);
            let mut buf = String::new();
            let plan_ok = values::check_value(&plan.as_ref(), &c, v, false, &mut buf).is_ok();
            if plan_ok {
                accepted += 1;
                assert!(
                    runtime_accepts(*code, v, xsd10),
                    "the plan reads {v:?} as {lex:?} (XSD 1.0: {xsd10}), the runtime does not"
                );
            }
        }
    }
    assert!(accepted > 50_000, "too few accepted forms: {accepted}");
}

/// The plan's binary readers count the octets the validators decode.
#[test]
fn binary_octets_are_the_decoders() {
    use base64::Engine;
    let mut r = Rng(0x9E37_79B9_7F4A_7C15);
    let b64 = ["A", "Q", "g", "w", "B", "+", "/", "=", " ", "z", "9"];
    for _ in 0..200_000 {
        let n = r.next() % 13;
        let s: String = (0..n).map(|_| r.pick(&b64)).collect();
        let plan = values::base64_octets(&s);
        let runtime = base64::engine::general_purpose::STANDARD.decode(s.as_bytes());
        assert_eq!(plan, runtime.ok().map(|o| o.len()), "{s:?}");
    }
    // Longer than one chunk, padding only at the end.
    let long = "QUJD".repeat(700) + "QQ==";
    assert_eq!(values::base64_octets(&long), Some(2101));
    let padded_inside = "QQ==".repeat(400);
    assert_eq!(values::base64_octets(&padded_inside), None);
    let hex = ["0", "a", "F", "g", " ", "9"];
    for _ in 0..100_000 {
        let n = r.next() % 7;
        let s: String = (0..n).map(|_| r.pick(&hex)).collect();
        assert_eq!(
            values::hex_octets(&s),
            hex::decode(&s).ok().map(|o| o.len()),
            "{s:?}"
        );
    }
    assert_eq!(values::hex_octets(&"0a".repeat(1000)), Some(1000));
}

/// The plan's white-space normalization is the dynamic path's, byte for byte.
#[test]
fn normalization_is_the_runtimes() {
    use crate::types::facets::{normalize_whitespace, WhitespaceMode};
    let mut r = Rng(0x1234_5678_9ABC_DEF1);
    let parts = [" ", "\t", "\n", "\r", "a", "b", "\u{a0}", "é", "  "];
    for _ in 0..100_000 {
        let n = r.next() % 7;
        let s: String = (0..n).map(|_| r.pick(&parts)).collect();
        for (ws, mode) in [
            (model::Ws::Preserve, WhitespaceMode::Preserve),
            (model::Ws::Replace, WhitespaceMode::Replace),
            (model::Ws::Collapse, WhitespaceMode::Collapse),
        ] {
            let mut buf = String::new();
            assert_eq!(
                values::normalize(&s, ws, &mut buf),
                normalize_whitespace(&s, mode).as_ref(),
                "{s:?} {ws:?}"
            );
        }
    }
}

const NUMBERS: &str = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="r">
    <xs:complexType>
      <xs:sequence>
        <xs:element name="q" type="Q" minOccurs="0"/>
        <xs:element name="b" type="xs:unsignedByte" minOccurs="0"/>
        <xs:element name="p" type="P" minOccurs="0"/>
        <xs:element name="d" type="xs:date" minOccurs="0"/>
        <xs:element name="t" type="T" minOccurs="0"/>
      </xs:sequence>
      <xs:attribute name="rate" type="P"/>
    </xs:complexType>
  </xs:element>
  <xs:simpleType name="Q">
    <xs:restriction base="xs:integer">
      <xs:minInclusive value="1"/>
      <xs:maxExclusive value="1000"/>
      <xs:totalDigits value="3"/>
    </xs:restriction>
  </xs:simpleType>
  <xs:simpleType name="P">
    <xs:restriction base="xs:decimal">
      <xs:maxInclusive value="100.00"/>
      <xs:fractionDigits value="2"/>
      <xs:pattern value="-?\d{1,3}\.\d{2}"/>
    </xs:restriction>
  </xs:simpleType>
  <xs:simpleType name="T">
    <xs:restriction base="xs:time">
      <xs:maxInclusive value="24:00:00"/>
    </xs:restriction>
  </xs:simpleType>
</xs:schema>"#;

#[test]
fn numbers_dates_and_times_as_shipped() {
    let set = load(NUMBERS);
    let (plan, report) = build(&set);
    assert!(report.uncovered.is_empty(), "{:?}", report.uncovered);
    for doc in [
        "<r><q>1</q><b>255</b><p>100.00</p><d>2000-02-29</d><t>00:00:00</t></r>",
        "<r rate=' 0.50 '><q> 999 </q><b>-0</b><p>-5.00</p><d>1970-01-01Z</d></r>",
        "<r><q>+007</q><b>+0</b></r>",
    ] {
        assert_eq!(verdicts(&set, &plan, doc), (true, true), "{doc}");
    }
    for doc in [
        "<r><q>0</q></r>",
        "<r><q>1000</q></r>",
        "<r><q>1.0</q></r>",
        "<r><b>256</b></r>",
        "<r><b>-1</b></r>",
        "<r><p>100.01</p></r>",
        "<r><p>1.5</p></r>",
        "<r><p>1.500</p></r>",
        "<r><d>1900-02-29</d></r>",
        "<r><d>0000-01-01</d></r>",
        // `24:00:00` of a time is `00:00:00`: anything later is out of range.
        "<r><t>12:00:00</t></r>",
        "<r><t>24:00:00</t><t/></r>",
    ] {
        assert!(!verdicts(&set, &plan, doc).0, "{doc}");
    }
}

const CONSTRAINTS: &str = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="r">
    <xs:complexType>
      <xs:sequence>
        <xs:element name="f" type="xs:decimal" fixed="1.50" minOccurs="0"/>
        <xs:element name="d" type="xs:date" default="2000-01-01" minOccurs="0"/>
        <xs:element name="s" minOccurs="0">
          <xs:complexType>
            <xs:simpleContent>
              <xs:extension base="xs:base64Binary">
                <xs:attribute name="Id" type="xs:ID"/>
                <xs:attribute name="Algorithm" type="xs:anyURI" fixed="urn:a"/>
              </xs:extension>
            </xs:simpleContent>
          </xs:complexType>
        </xs:element>
      </xs:sequence>
      <xs:attribute name="Id" type="xs:ID"/>
    </xs:complexType>
  </xs:element>
</xs:schema>"#;

#[test]
fn value_constraints_and_ids_as_shipped() {
    let set = load(CONSTRAINTS);
    let (plan, report) = build(&set);
    assert!(report.uncovered.is_empty(), "{:?}", report.uncovered);
    for doc in [
        "<r><f/><d/></r>",
        "<r><f>1.50</f><d>2001-12-31</d></r>",
        "<r Id='a'><s Id='b'>QUJD</s></r>",
        "<r Id='a'><s Id='b' Algorithm='urn:a'>QUJDRA==</s></r>",
    ] {
        assert_eq!(verdicts(&set, &plan, doc), (true, true), "{doc}");
    }
    for doc in [
        // Equal values, other lexical forms: the dynamic path accepts them,
        // the plan compares literals.
        "<r><f>1.5</f></r>",
        "<r><f> 1.50 </f></r>",
        "<r Id='a'><s Id='a'>QUJD</s></r>",
        "<r><s Algorithm='urn:b'>QUJD</s></r>",
        "<r><s>QUJD RA==</s></r>",
    ] {
        assert!(!verdicts(&set, &plan, doc).0, "{doc}");
    }
}

/// The facets a simple-content restriction declares are not compiled with
/// the schema: the dynamic path compiles their patterns when it matches them,
/// as XSD 1.1 patterns, also in an XSD 1.0 schema. Read as a 1.0 pattern,
/// `[^\w]` (`\w` pinned to Unicode 3.0) would admit U+0221, a letter since
/// Unicode 4.0, which the dynamic path refuses.
#[cfg(feature = "xsd11")]
#[test]
fn simple_content_restriction_patterns_are_read_as_the_dynamic_path_reads_them() {
    let xsd = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
      <xs:complexType name="C"><xs:simpleContent><xs:extension base="xs:string">
        <xs:attribute name="a" type="xs:string"/></xs:extension></xs:simpleContent></xs:complexType>
      <xs:complexType name="R"><xs:simpleContent><xs:restriction base="C">
        <xs:pattern value="[^\w]"/></xs:restriction></xs:simpleContent></xs:complexType>
      <xs:element name="r" type="R"/>
    </xs:schema>"#;
    let set = load(xsd);
    let (plan, _) = build(&set);
    assert!(!verdicts(&set, &plan, "<r>\u{221}</r>").0);
    assert_eq!(verdicts(&set, &plan, "<r>-</r>"), (true, true));
}

fn load11(xsd: &str) -> SchemaSet {
    let mut set = SchemaSet::xsd11();
    load_and_process_schema(xsd.as_bytes(), "test.xsd", &mut set, None).expect("schema");
    set
}

/// An `xs:ID` type stays an ID type when the plan cannot check its facets
/// (a `maxLength` past what the plan holds, an assertion): it is uncovered,
/// never handed to the delegating tier as a plain value. Otherwise an ID
/// attribute's `default` (XSD 1.1) would be counted by the runtime and not by
/// the plan, and the delegating tier would not keep such IDs.
#[test]
fn an_id_type_the_plan_cannot_check_stays_an_id() {
    let big = r#"<xs:simpleType name="BigId"><xs:restriction base="xs:ID"><xs:maxLength value="4294967295"/></xs:restriction></xs:simpleType>"#;
    let defaulted = format!(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">{big}
      <xs:element name="r"><xs:complexType><xs:sequence>
        <xs:element name="f" minOccurs="0" maxOccurs="unbounded"><xs:complexType>
          <xs:attribute name="id" type="BigId" default="a"/></xs:complexType></xs:element>
        <xs:element name="g" minOccurs="0" maxOccurs="unbounded"><xs:complexType>
          <xs:attribute name="id" type="xs:ID"/></xs:complexType></xs:element>
        <xs:element name="h" type="xs:ID" minOccurs="0" maxOccurs="unbounded"/>
      </xs:sequence></xs:complexType></xs:element></xs:schema>"#
    );
    let set = load11(&defaulted);
    let (plan, _) = build(&set);
    for doc in [
        "<r><f/><f/></r>",
        "<r><f/><g id='a'/></r>",
        "<r><f/><h>a</h></r>",
    ] {
        // `verdicts` asserts that neither tier answers `Valid`.
        let _ = verdicts(&set, &plan, doc);
    }
    let fixed = defaulted.replace(r#"default="a""#, r#"fixed="a""#);
    let set = load11(&fixed);
    let (plan, _) = build(&set);
    let _ = verdicts(&set, &plan, "<r><f/><f/></r>");

    // The delegating tier keeps such IDs too, both versions.
    let plain = format!(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">{big}
      <xs:complexType name="C"><xs:simpleContent><xs:extension base="BigId"/></xs:simpleContent></xs:complexType>
      <xs:element name="r"><xs:complexType><xs:sequence>
        <xs:element name="e" type="BigId" minOccurs="0" maxOccurs="unbounded"/>
        <xs:element name="f" minOccurs="0" maxOccurs="unbounded"><xs:complexType><xs:attribute name="id" type="BigId"/></xs:complexType></xs:element>
        <xs:element name="g" minOccurs="0" maxOccurs="unbounded"><xs:complexType><xs:attribute name="id" type="xs:ID"/></xs:complexType></xs:element>
        <xs:element name="c" type="C" minOccurs="0" maxOccurs="unbounded"/>
      </xs:sequence></xs:complexType></xs:element></xs:schema>"#
    );
    for set in [load(&plain), load11(&plain)] {
        let (plan, _) = build(&set);
        for doc in [
            "<r><e>a</e><e>a</e></r>",
            "<r><f id='a'/><f id='a'/></r>",
            "<r><e>a</e><f id='a'/></r>",
            "<r><f id='a'/><g id='a'/></r>",
            "<r><c>a</c><c>a</c></r>",
        ] {
            let _ = verdicts(&set, &plan, doc);
        }
    }
}

/// The same with an assertion on the ID type (XSD 1.1, the `xsd11` build).
#[cfg(feature = "xsd11")]
#[test]
fn an_id_type_with_an_assertion_stays_an_id() {
    let xsd = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
      <xs:simpleType name="AId"><xs:restriction base="xs:ID"><xs:assertion test="string-length($value) lt 10"/></xs:restriction></xs:simpleType>
      <xs:element name="r"><xs:complexType><xs:sequence>
        <xs:element name="f" minOccurs="0" maxOccurs="unbounded"><xs:complexType>
          <xs:attribute name="id" type="AId" default="a"/></xs:complexType></xs:element>
      </xs:sequence></xs:complexType></xs:element></xs:schema>"#;
    let set = load11(xsd);
    let (plan, _) = build(&set);
    let _ = verdicts(&set, &plan, "<r><f/><f/></r>");
}

/// A schema document may define types in the XML Schema namespace itself. A
/// restriction of `xs:IDREF` (or `xs:QName`, …) named there is no built-in:
/// the families the plan leaves out are told by the built-in type code the
/// type derives from, not by the name of its nearest ancestor in that
/// namespace.
#[test]
fn types_defined_in_the_xsd_namespace_are_told_by_their_built_in() {
    let main = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:t="urn:t" targetNamespace="urn:t">
      <xs:import namespace="http://www.w3.org/2001/XMLSchema"/>
      <xs:element name="r"><xs:complexType><xs:sequence>
        <xs:element name="f" minOccurs="0"><xs:complexType>
          <xs:attribute name="x" type="xs:myRef" default="q"/></xs:complexType></xs:element>
        <xs:element name="e" type="xs:myRef" minOccurs="0" form="unqualified"/>
        <xs:element name="q" type="xs:myQName" minOccurs="0" form="unqualified"/>
      </xs:sequence></xs:complexType></xs:element>
    </xs:schema>"#;
    let xs = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" targetNamespace="http://www.w3.org/2001/XMLSchema">
      <xs:simpleType name="myRef"><xs:restriction base="xs:IDREF"/></xs:simpleType>
      <xs:simpleType name="myQName"><xs:restriction base="xs:QName"/></xs:simpleType>
    </xs:schema>"#;
    for v11 in [false, true] {
        let builder = if v11 {
            crate::SchemaSetBuilder::xsd11()
        } else {
            crate::SchemaSetBuilder::new()
        };
        let set = builder
            .add_bytes(xs.as_bytes(), "xs.xsd")
            .and_then(|b| b.add_bytes(main.as_bytes(), "main.xsd"))
            .and_then(|b| b.compile())
            .expect("schema")
            .into_schema_set();
        let (plan, _) = build(&set);
        for doc in [
            r#"<t:r xmlns:t="urn:t"><f/></t:r>"#,
            r#"<t:r xmlns:t="urn:t"><e>q</e></t:r>"#,
            r#"<t:r xmlns:t="urn:t"><q>zz:a</q></t:r>"#,
        ] {
            // `verdicts` asserts that neither tier answers `Valid`.
            let _ = verdicts(&set, &plan, doc);
        }
    }
}
