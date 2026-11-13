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
    // xs:positiveInteger is a G2 check: Unknown as shipped, Valid delegating.
    let doc = order(
        &format!("<id>XY001</id><person>Ann</person>{LINE}"),
        "retail",
    );
    assert_eq!(verdicts(&set, &plan, &doc), (false, true));
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
        assert_eq!(verdicts(&set, &plan, &doc), (false, true), "{doc}");
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
    // A schema-location hint on the root: G2 as shipped, the delegating tier
    // checks it as the runtime does.
    for doc in [
        "<r xmlns:xsi='http://www.w3.org/2001/XMLSchema-instance' xsi:noNamespaceSchemaLocation='x.xsd'><s>x</s></r>",
        "<r xmlns:xsi='http://www.w3.org/2001/XMLSchema-instance' xsi:schemaLocation='urn:a a.xsd urn:b b.xsd'><s>x</s></r>",
    ] {
        assert_eq!(verdicts(&set, &plan, doc), (false, true), "{doc}");
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
fn identity_and_defaults_in_the_delegating_tier() {
    let set = load(IDS);
    let (plan, report) = build(&set);
    assert!(report.uncovered.is_empty(), "{:?}", report.uncovered);
    assert_eq!(
        verdicts(
            &set,
            &plan,
            "<r><i id='a' n='1'/><i id='b' n='2' d='1.50'/><v>x</v></r>"
        ),
        (false, true)
    );
    for doc in [
        "<r><i id='a'/><i id='a'/></r>",
        "<r><i n='1'/><i n='1'/></r>",
        "<r><i id='1a'/></r>",
        "<r><i d='x'/></r>",
        "<r><i/><v/></r>",
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
        (false, true)
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
