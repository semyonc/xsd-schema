//! An element declaration's {value constraint} for hosts:
//! `SchemaSet::element_value_constraint` and
//! `SchemaSet::element_value_constraint_for_type`, and the
//! `SchemaInfo::typed_value` guarantee with `BUILD_PSVI_TYPED_VALUES` cleared.
//!
//! Structures §3.3.2.1 maps `default` / `fixed` to a Value Constraint: "Use
//! the name effective simple type definition for the declared {type
//! definition}, if it is a simple type definition, or, if {type
//! definition}.{content type}.{variety} = simple, for {type
//! definition}.{content type}.{simple type definition}, or else for the
//! built-in string simple type definition)"; {variety} "either default or
//! fixed, as appropriate", {value} "the ·actual value· (with respect to the
//! ·effective simple type definition·) of the [attribute]", {lexical form}
//! "the ·normalized value· (with respect to the ·effective simple type
//! definition·) of the [attribute]". Datatypes §3.3.18: the lexical mapping of
//! a QName "depends on the namespace bindings in scope where the literal
//! occurs" — the schema element that carries the `default` / `fixed`
//! attribute.
//!
//! An element with neither element nor character children takes the value
//! (§3.3.4.3 clause 5.1: "The element information item with D.{value
//! constraint}.{lexical form} used as its ·normalized value· is locally
//! ·valid· with respect to the ·governing type definition·"), so a host that
//! builds the element itself can read the {value} before the content and
//! declare a prefix for a QName in it on the start tag.
//!
//! Every case runs in XSD 1.0 and 1.1 mode, with and without PSVI typed
//! values.

use xsd_schema::namespace::context::NamespaceContextSnapshot;
use xsd_schema::schema::{ElementValueConstraint, ValueConstraintVariety};
use xsd_schema::types::value::{XmlAtomicValue, XmlValue, XmlValueKind};
use xsd_schema::validation::{
    drive_quick_xml_with, AttributeView, CollectingValidationSink, SchemaInfo, SchemaValidator,
    SchemaValidity, ValidationEventHandler, ValidationFlags,
};
use xsd_schema::{ElementKey, SchemaSet, SchemaSetBuilder, TypeKey, XsdVersion};

use ValueConstraintVariety::{Default, Fixed};

const VERSIONS: [XsdVersion; 2] = [XsdVersion::V1_0, XsdVersion::V1_1];

/// Top-level and local declarations with value constraints of every shape.
/// `p` is bound to `urn:p` on the schema element only, `x` on one
/// declaration only; the default namespace of the document is `urn:t`.
const SCHEMA: &str = r###"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
    xmlns="urn:t" xmlns:t="urn:t" xmlns:p="urn:p" targetNamespace="urn:t"
    elementFormDefault="qualified">
  <xs:notation name="gif" public="image/gif"/>
  <xs:simpleType name="fmt"><xs:restriction base="xs:NOTATION">
    <xs:enumeration value="gif"/>
  </xs:restriction></xs:simpleType>
  <xs:simpleType name="qnames"><xs:list itemType="xs:QName"/></xs:simpleType>
  <xs:simpleType name="intOrQName"><xs:union memberTypes="xs:integer xs:QName"/></xs:simpleType>
  <xs:complexType name="scq"><xs:simpleContent><xs:extension base="xs:QName">
    <xs:attribute name="a" type="xs:string"/>
  </xs:extension></xs:simpleContent></xs:complexType>
  <xs:complexType name="mixed" mixed="true"><xs:sequence>
    <xs:element name="i" minOccurs="0"/>
  </xs:sequence></xs:complexType>

  <xs:element name="qe" type="xs:QName" default="p:dvalue"/>
  <xs:element name="qf" type="xs:QName" fixed=" p:fvalue "/>
  <xs:element name="qd" type="xs:QName" default="dvalue"/>
  <xs:element name="qx" type="xs:QName" default="x:xvalue" xmlns:x="urn:x"/>
  <xs:element name="nt" type="fmt" default="gif"/>
  <xs:element name="ql" type="qnames" default="  p:a   t:b  "/>
  <xs:element name="qu" type="intOrQName" default="p:u"/>
  <xs:element name="iu" type="intOrQName" default=" 12 "/>
  <xs:element name="sc" type="scq" default="p:scvalue"/>
  <xs:element name="tk" type="xs:token" default="  a   b  "/>
  <xs:element name="st" type="xs:string" default="  a   b  "/>
  <xs:element name="mx" type="mixed" default="  some text "/>
  <xs:element name="an" type="xs:anySimpleType" default="p:svalue"/>
  <xs:element name="none" type="xs:QName"/>
  <xs:element name="r"><xs:complexType><xs:sequence>
    <xs:element name="lq" type="xs:QName" default="p:local" minOccurs="0"/>
    <xs:element name="lf" type="qnames" fixed="p:l1 p:l2" minOccurs="0"/>
    <xs:element ref="t:qe" minOccurs="0"/>
  </xs:sequence></xs:complexType></xs:element>
</xs:schema>"###;

/// A second schema document that binds `p` to another namespace.
const OTHER: &str = r###"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
    xmlns:p="urn:p2" targetNamespace="urn:o">
  <xs:element name="oq" type="xs:QName" default="p:ovalue"/>
</xs:schema>"###;

fn compile(version: XsdVersion) -> SchemaSet {
    SchemaSetBuilder::with_version(version)
        .add_source(SCHEMA, "file:///element_value_constraint.xsd")
        .expect("schema source accepted")
        .add_source(OTHER, "file:///element_value_constraint_other.xsd")
        .expect("schema source accepted")
        .compile()
        .unwrap_or_else(|e| panic!("{version:?}: {e}"))
        .into_schema_set()
}

fn flags(psvi: bool) -> ValidationFlags {
    if psvi {
        ValidationFlags::default()
    } else {
        ValidationFlags::default() & !ValidationFlags::BUILD_PSVI_TYPED_VALUES
    }
}

/// A namespace snapshot from `(prefix, uri)` pairs; the empty prefix is the
/// default namespace.
fn snapshot(ss: &SchemaSet, bindings: &[(&str, &str)]) -> NamespaceContextSnapshot {
    let mut ns = NamespaceContextSnapshot::default();
    for (prefix, uri) in bindings {
        if prefix.is_empty() {
            ns.default_ns = Some(ss.name_table.add(uri));
        } else {
            ns.bindings
                .push((ss.name_table.add(prefix), ss.name_table.add(uri)));
        }
    }
    ns
}

/// The instance bindings every comparison runs under: `p` bound to another
/// namespace, and `p` unbound.
const INSTANCE_BINDINGS: [&[(&str, &str)]; 2] =
    [&[("", "urn:t"), ("p", "urn:other")], &[("", "urn:t")]];

/// `value` as text: its type code, then each QName / NOTATION as
/// `{namespace}local`, any other atomic value as its string.
fn describe(ss: &SchemaSet, value: &XmlValue) -> String {
    fn body(ss: &SchemaSet, value: &XmlValue) -> String {
        match &value.value {
            XmlValueKind::Atomic(XmlAtomicValue::QName(qn) | XmlAtomicValue::Notation(qn)) => {
                let name = qn.name();
                let ns = name
                    .namespace_uri
                    .map_or("", |n| ss.name_table.resolve_ref(n));
                format!("{{{ns}}}{}", ss.name_table.resolve_ref(name.local_name))
            }
            XmlValueKind::Union(member) => body(ss, member),
            XmlValueKind::List { items, .. } => items
                .iter()
                .map(|item| body(ss, item))
                .collect::<Vec<_>>()
                .join(" "),
            _ => value.to_string_value(),
        }
    }
    format!("{:?} {}", value.type_code, body(ss, value))
}

fn top_level(ss: &SchemaSet, ns: &str, local: &str) -> ElementKey {
    ss.lookup_element(Some(ss.name_table.add(ns)), ss.name_table.add(local))
        .unwrap_or_else(|| panic!("a top-level declaration {local}"))
}

/// The declarations of `r`'s children `lq`, `lf` and the reference to `qe`,
/// as validation names them.
fn local_keys(ss: &SchemaSet) -> Vec<(&'static str, ElementKey)> {
    let validator = SchemaValidator::new(ss, ValidationFlags::default());
    let (mut errors, mut warnings) = (Vec::new(), Vec::new());
    let mut rt = validator.start_run(CollectingValidationSink {
        errors: &mut errors,
        warnings: &mut warnings,
    });
    let ns = snapshot(ss, &[("", "urn:t")]);
    let _ = rt.validate_element("r", "urn:t", None, None, &ns);
    let _ = rt.validate_end_of_attributes();
    let mut keys = Vec::new();
    for name in ["lq", "lf", "qe"] {
        let info = rt.validate_element(name, "urn:t", None, None, &ns);
        keys.push((name, info.element_decl.expect("a governing declaration")));
        let _ = rt.validate_end_of_attributes();
        let _ = rt.validate_end_element();
    }
    let _ = rt.validate_end_element();
    rt.end_validation().expect("no operational failure");
    drop(rt);
    assert!(errors.is_empty(), "{errors:?}");
    keys
}

/// `(variety, lexical form, described value)` of a value constraint.
fn shape(ss: &SchemaSet, vc: &ElementValueConstraint) -> (ValueConstraintVariety, String, String) {
    (
        vc.variety,
        vc.lexical_form.clone(),
        vc.typed_value
            .as_ref()
            .map_or("none".to_string(), |v| describe(ss, v)),
    )
}

/// What each top-level declaration's value constraint is.
const TOP_LEVEL: &[(&str, &str, ValueConstraintVariety, &str, &str)] = &[
    ("urn:t", "qe", Default, "p:dvalue", "QName {urn:p}dvalue"),
    ("urn:t", "qf", Fixed, "p:fvalue", "QName {urn:p}fvalue"),
    // An unprefixed QName is in the schema document's default namespace.
    ("urn:t", "qd", Default, "dvalue", "QName {urn:t}dvalue"),
    // `x` is bound on the declaration only.
    ("urn:t", "qx", Default, "x:xvalue", "QName {urn:x}xvalue"),
    ("urn:t", "nt", Default, "gif", "Notation {urn:t}gif"),
    ("urn:t", "ql", Default, "p:a t:b", "QName {urn:p}a {urn:t}b"),
    ("urn:t", "qu", Default, "p:u", "QName {urn:p}u"),
    ("urn:t", "iu", Default, "12", "Integer 12"),
    ("urn:t", "sc", Default, "p:scvalue", "QName {urn:p}scvalue"),
    ("urn:t", "tk", Default, "a b", "Token a b"),
    ("urn:t", "st", Default, "  a   b  ", "String   a   b  "),
    // Mixed content: the effective simple type definition is xs:string.
    (
        "urn:t",
        "mx",
        Default,
        "  some text ",
        "String   some text ",
    ),
    ("urn:t", "an", Default, "p:svalue", "UntypedAtomic p:svalue"),
    // Another schema document binds `p` to `urn:p2`.
    ("urn:o", "oq", Default, "p:ovalue", "QName {urn:p2}ovalue"),
];

/// The local declarations (and the reference) of `r`'s children.
const LOCAL: &[(&str, ValueConstraintVariety, &str, &str)] = &[
    ("lq", Default, "p:local", "QName {urn:p}local"),
    ("lf", Fixed, "p:l1 p:l2", "QName {urn:p}l1 {urn:p}l2"),
    ("qe", Default, "p:dvalue", "QName {urn:p}dvalue"),
];

#[test]
fn top_level_declarations_give_variety_lexical_form_and_value() {
    for version in VERSIONS {
        let ss = compile(version);
        let mut wrong = Vec::new();
        for (ns, local, variety, lexical, value) in TOP_LEVEL {
            let key = top_level(&ss, ns, local);
            match ss.element_value_constraint(key) {
                Some(vc) => {
                    let got = shape(&ss, &vc);
                    let want = (*variety, lexical.to_string(), value.to_string());
                    if got != want {
                        wrong.push(format!("{local}: {got:?}, want {want:?}"));
                    }
                }
                None => wrong.push(format!("{local}: no value constraint")),
            }
        }
        assert!(wrong.is_empty(), "{version:?}: {wrong:#?}");
        let none = top_level(&ss, "urn:t", "none");
        assert!(
            ss.element_value_constraint(none).is_none(),
            "{version:?}: a declaration without default or fixed"
        );
    }
}

#[test]
fn local_declarations_and_references_give_the_same() {
    for version in VERSIONS {
        let ss = compile(version);
        let keys = local_keys(&ss);
        for ((name, key), (lname, variety, lexical, value)) in keys.iter().zip(LOCAL) {
            assert_eq!(name, lname);
            let vc = ss
                .element_value_constraint(*key)
                .unwrap_or_else(|| panic!("{version:?} {name}: a value constraint"));
            assert_eq!(
                shape(&ss, &vc),
                (*variety, lexical.to_string(), value.to_string()),
                "{version:?} {name}"
            );
        }
        // The particle `<xs:element ref="t:qe"/>` names the top-level
        // declaration; a reference entry in the arena, where there is one,
        // reads the declaration it resolves to.
        let qe = top_level(&ss, "urn:t", "qe");
        assert_eq!(keys[2].1, qe, "{version:?}: the reference names qe");
        let references: Vec<ElementKey> = ss
            .arenas
            .elements
            .iter()
            .filter(|(_, decl)| decl.resolved_ref == Some(qe))
            .map(|(key, _)| key)
            .collect();
        for key in references {
            let vc = ss
                .element_value_constraint(key)
                .expect("the referenced value");
            assert_eq!(
                shape(&ss, &vc),
                (Default, "p:dvalue".into(), "QName {urn:p}dvalue".into()),
                "{version:?}: the reference"
            );
        }
    }
}

/// Validate an empty `local` (in `ns`) through the push API — inside `r` for
/// the local declarations — and return its end-element `SchemaInfo` and the
/// errors.
fn validate_empty(
    ss: &SchemaSet,
    psvi: bool,
    bindings: &[(&str, &str)],
    ns_uri: &str,
    local: &str,
    inside_r: bool,
    xsi_type: Option<&str>,
) -> (SchemaInfo, Vec<String>) {
    let validator = SchemaValidator::new(ss, flags(psvi));
    let (mut errors, mut warnings) = (Vec::new(), Vec::new());
    let mut rt = validator.start_run(CollectingValidationSink {
        errors: &mut errors,
        warnings: &mut warnings,
    });
    let ns = snapshot(ss, bindings);
    if inside_r {
        let _ = rt.validate_element("r", "urn:t", None, None, &ns);
        let _ = rt.validate_end_of_attributes();
    }
    let _ = rt.validate_element(local, ns_uri, xsi_type, None, &ns);
    let _ = rt.validate_end_of_attributes();
    let info = rt.validate_end_element();
    if inside_r {
        let _ = rt.validate_end_element();
    }
    rt.end_validation().expect("no operational failure");
    drop(rt);
    (info, errors.iter().map(|e| e.to_string()).collect())
}

/// The accessor's {value} is the value validation gives an empty element
/// (`SchemaInfo::typed_value`, with and without PSVI typed values), and its
/// {lexical form} the [schema normalized value] (§3.3.5.4 clause 1.1: "If
/// clause 5.1 of Element Locally Valid (Element) (§3.3.4.3) above has applied,
/// then the {lexical form} of the {value constraint}"), whatever the instance
/// binds the prefix to.
#[test]
fn the_value_is_the_one_validation_applies() {
    for version in VERSIONS {
        let ss = compile(version);
        let locals = local_keys(&ss);
        let mut cases: Vec<(&str, &str, bool, ElementKey)> = TOP_LEVEL
            .iter()
            // Mixed content has no [schema actual value] (§3.3.5.4 clause 2).
            .filter(|(_, local, ..)| *local != "mx")
            .map(|(ns, local, ..)| (*ns, *local, false, top_level(&ss, ns, local)))
            .collect();
        cases.extend(
            locals
                .iter()
                .map(|(name, key)| ("urn:t", *name, true, *key)),
        );
        for bindings in INSTANCE_BINDINGS {
            for psvi in [true, false] {
                for (ns, local, inside_r, key) in &cases {
                    let label = format!("{version:?} psvi={psvi} {bindings:?} {local}");
                    let vc = ss
                        .element_value_constraint(*key)
                        .expect("a value constraint");
                    let (info, errors) =
                        validate_empty(&ss, psvi, bindings, ns, local, *inside_r, None);
                    assert!(errors.is_empty(), "{label}: {errors:?}");
                    assert_eq!(info.validity, SchemaValidity::Valid, "{label}");
                    assert!(info.is_default, "{label}: took its value constraint");
                    assert_eq!(info.element_decl, Some(*key), "{label}");
                    assert!(vc.typed_value.is_some(), "{label}: a value");
                    assert_eq!(info.typed_value, vc.typed_value, "{label}: the value");
                    if psvi {
                        assert_eq!(
                            info.normalized_value.as_deref(),
                            Some(vc.lexical_form.as_str()),
                            "{label}: the lexical form"
                        );
                    }
                    // The declared type is the governing type here.
                    let governing = info.schema_type.expect("a governing type");
                    let applied = ss
                        .element_value_constraint_for_type(*key, governing)
                        .expect("a value constraint");
                    assert_eq!(applied.typed_value, vc.typed_value, "{label}: for its type");
                    assert_eq!(applied.lexical_form, vc.lexical_form, "{label}");
                    assert_eq!(applied.variety, vc.variety, "{label}");
                }
            }
        }
    }
}

/// An `xsi:type` makes another type governing (§3.3.4.1): the element takes
/// D.{value constraint}.{lexical form} as its normalized value, validated
/// against that type (clause 5.1.2) — `element_value_constraint_for_type`
/// gives that value.
#[test]
fn an_instance_specified_type_governs_the_value() {
    for version in VERSIONS {
        let ss = compile(version);
        let an = top_level(&ss, "urn:t", "an");
        let qname = TypeKey::Simple(ss.builtin_types().qname);
        let applied = ss
            .element_value_constraint_for_type(an, qname)
            .expect("a value constraint");
        assert_eq!(applied.variety, Default, "{version:?}");
        assert_eq!(applied.lexical_form, "p:svalue", "{version:?}");
        assert_eq!(
            applied
                .typed_value
                .as_ref()
                .map(|v| describe(&ss, v))
                .as_deref(),
            Some("QName {urn:p}svalue"),
            "{version:?}"
        );
        for bindings in [
            &[
                ("", "urn:t"),
                ("p", "urn:other"),
                ("xs", "http://www.w3.org/2001/XMLSchema"),
            ][..],
            &[("", "urn:t"), ("xs", "http://www.w3.org/2001/XMLSchema")][..],
        ] {
            for psvi in [true, false] {
                let label = format!("{version:?} psvi={psvi} {bindings:?}");
                let (info, errors) =
                    validate_empty(&ss, psvi, bindings, "urn:t", "an", false, Some("xs:QName"));
                assert!(errors.is_empty(), "{label}: {errors:?}");
                assert_eq!(info.validity, SchemaValidity::Valid, "{label}");
                assert!(info.is_default, "{label}");
                assert_eq!(info.schema_type, Some(qname), "{label}");
                assert_eq!(info.typed_value, applied.typed_value, "{label}");
            }
        }
        // The declaration's own {value} is with respect to xs:anySimpleType.
        let declared = ss.element_value_constraint(an).expect("a value constraint");
        assert_eq!(
            declared
                .typed_value
                .as_ref()
                .map(|v| describe(&ss, v))
                .as_deref(),
            Some("UntypedAtomic p:svalue"),
            "{version:?}"
        );
        // A type the lexical form is not valid for: no value.
        let int = TypeKey::Simple(
            ss.get_built_in_simple_type_by_code(xsd_schema::XmlTypeCode::Int)
                .expect("xs:int"),
        );
        let invalid = ss
            .element_value_constraint_for_type(an, int)
            .expect("a value constraint");
        assert!(invalid.typed_value.is_none(), "{version:?}");
        assert_eq!(invalid.lexical_form, "p:svalue", "{version:?}");
    }
}

/// XSD 1.1: a type alternative selects the governing type (§3.12.4); the value
/// is read against it.
#[cfg(feature = "xsd11")]
#[test]
fn a_type_alternative_governs_the_value() {
    const CTA: &str = r###"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
        xmlns:t="urn:t" xmlns:p="urn:p" targetNamespace="urn:t">
      <xs:complexType name="base"><xs:simpleContent>
        <xs:extension base="xs:anySimpleType">
          <xs:attribute name="kind" type="xs:string"/>
        </xs:extension>
      </xs:simpleContent></xs:complexType>
      <xs:complexType name="qn"><xs:simpleContent>
        <xs:restriction base="t:base"><xs:simpleType>
          <xs:restriction base="xs:QName"/>
        </xs:simpleType></xs:restriction>
      </xs:simpleContent></xs:complexType>
      <xs:element name="c" type="t:base" default="p:cvalue">
        <xs:alternative test="@kind = 'q'" type="t:qn"/>
      </xs:element>
    </xs:schema>"###;
    let ss = SchemaSetBuilder::with_version(XsdVersion::V1_1)
        .add_source(CTA, "file:///element_value_constraint_cta.xsd")
        .expect("schema source accepted")
        .compile()
        .unwrap_or_else(|e| panic!("{e}"))
        .into_schema_set();
    let c = top_level(&ss, "urn:t", "c");
    let qn = ss
        .lookup_type(Some(ss.name_table.add("urn:t")), ss.name_table.add("qn"))
        .expect("the alternative's type");
    let applied = ss
        .element_value_constraint_for_type(c, qn)
        .expect("a value constraint");
    assert_eq!(
        applied
            .typed_value
            .as_ref()
            .map(|v| describe(&ss, v))
            .as_deref(),
        Some("QName {urn:p}cvalue")
    );
    for bindings in INSTANCE_BINDINGS {
        for psvi in [true, false] {
            let label = format!("psvi={psvi} {bindings:?}");
            let validator = SchemaValidator::new(&ss, flags(psvi));
            let (mut errors, mut warnings) = (Vec::new(), Vec::new());
            let mut rt = validator.start_run(CollectingValidationSink {
                errors: &mut errors,
                warnings: &mut warnings,
            });
            let ns = snapshot(&ss, bindings);
            let _ = rt.validate_element("c", "urn:t", None, None, &ns);
            let _ = rt.validate_attribute("kind", "", "q");
            let start = rt.validate_end_of_attributes();
            let _ = rt.take_deferred_attribute_results();
            assert_eq!(start.schema_type, Some(qn), "{label}: the alternative");
            let info = rt.validate_end_element();
            rt.end_validation().expect("no operational failure");
            drop(rt);
            assert!(errors.is_empty(), "{label}: {errors:?}");
            assert!(info.is_default, "{label}");
            assert_eq!(info.typed_value, applied.typed_value, "{label}");
        }
    }
}

// ── `SchemaInfo::typed_value` without PSVI retention ─────────────────

const IDS: &str = r###"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
    xmlns="urn:t" xmlns:t="urn:t" xmlns:p="urn:p" targetNamespace="urn:t"
    elementFormDefault="qualified">
  <xs:notation name="gif" public="image/gif"/>
  <xs:simpleType name="fmt"><xs:restriction base="xs:NOTATION">
    <xs:enumeration value="gif"/>
  </xs:restriction></xs:simpleType>
  <xs:simpleType name="qnames"><xs:list itemType="xs:QName"/></xs:simpleType>
  <xs:simpleType name="intOrQName"><xs:union memberTypes="xs:integer xs:QName"/></xs:simpleType>
  <xs:simpleType name="intOrId"><xs:union memberTypes="xs:integer xs:ID"/></xs:simpleType>
  <xs:simpleType name="kid"><xs:restriction base="xs:ID">
    <xs:pattern value="k.*"/>
  </xs:restriction></xs:simpleType>
  <xs:complexType name="scid"><xs:simpleContent><xs:extension base="xs:IDREF">
    <xs:attribute name="a" type="xs:string"/>
  </xs:extension></xs:simpleContent></xs:complexType>
  <xs:element name="doc"><xs:complexType>
    <xs:sequence>
      <xs:element name="id" type="xs:ID"/>
      <xs:element name="ref" type="xs:IDREF"/>
      <xs:element name="refs" type="xs:IDREFS"/>
      <xs:element name="kid" type="kid"/>
      <xs:element name="scid" type="scid"/>
      <xs:element name="q" type="xs:QName"/>
      <xs:element name="nt" type="fmt"/>
      <xs:element name="ql" type="qnames"/>
      <xs:element name="qu" type="intOrQName"/>
      <xs:element name="iu" type="intOrId"/>
      <xs:element name="ds" type="xs:string" default="dflt"/>
      <xs:element name="dt" type="xs:token" fixed="  t  "/>
      <xs:element name="dq" type="xs:QName" default="p:dq"/>
    </xs:sequence>
    <xs:attribute name="aid" type="xs:ID"/>
    <xs:attribute name="aref" type="xs:IDREF"/>
    <xs:attribute name="arefs" type="xs:IDREFS"/>
    <xs:attribute name="aq" type="xs:QName"/>
    <xs:attribute name="ant" type="fmt"/>
    <xs:attribute name="aql" type="qnames"/>
    <xs:attribute name="aqu" type="intOrQName"/>
    <xs:attribute name="aiu" type="intOrId"/>
  </xs:complexType></xs:element>
</xs:schema>"###;

const ATTRIBUTES: &[(&str, &str)] = &[
    ("aid", "a1"),
    ("aref", "e1"),
    ("arefs", "a1 e1"),
    ("aq", "p:x"),
    ("ant", "gif"),
    ("aql", "p:x p:y"),
    ("aqu", "p:z"),
    ("aiu", "a2"),
];

/// Child elements of `doc`: name and text (`None` for an empty one that takes
/// its value constraint).
const CHILDREN: &[(&str, Option<&str>)] = &[
    ("id", Some("e1")),
    ("ref", Some("a1")),
    ("refs", Some("e1 a1")),
    ("kid", Some("k2")),
    ("scid", Some("e1")),
    ("q", Some("p:q")),
    ("nt", Some("gif")),
    ("ql", Some("p:a p:b")),
    ("qu", Some("p:u")),
    ("iu", Some("e2")),
    ("ds", None),
    ("dt", None),
    ("dq", None),
];

/// The push API: every attribute's and child element's `SchemaInfo`, as
/// `(name, validity, typed value)`.
fn push_values(
    ss: &SchemaSet,
    psvi: bool,
) -> Vec<(String, SchemaValidity, Option<XmlValue>, bool)> {
    let validator = SchemaValidator::new(ss, flags(psvi));
    let (mut errors, mut warnings) = (Vec::new(), Vec::new());
    let mut rt = validator.start_run(CollectingValidationSink {
        errors: &mut errors,
        warnings: &mut warnings,
    });
    let ns = snapshot(ss, &[("", "urn:t"), ("p", "urn:p")]);
    let mut out = Vec::new();
    let _ = rt.validate_element("doc", "urn:t", None, None, &ns);
    for (name, value) in ATTRIBUTES {
        let info = rt.validate_attribute(name, "", value);
        out.push((format!("@{name}"), info.validity, info.typed_value, false));
    }
    let _ = rt.validate_end_of_attributes();
    for (name, text) in CHILDREN {
        let _ = rt.validate_element(name, "urn:t", None, None, &ns);
        let _ = rt.validate_end_of_attributes();
        if let Some(text) = text {
            rt.validate_text(text);
        }
        let info = rt.validate_end_element();
        out.push((
            name.to_string(),
            info.validity,
            info.typed_value,
            info.is_default,
        ));
    }
    let _ = rt.validate_end_element();
    rt.end_validation().expect("no operational failure");
    drop(rt);
    assert!(errors.is_empty(), "psvi={psvi}: {errors:?}");
    out
}

/// `ValidationFlags::BUILD_PSVI_TYPED_VALUES` cleared: the typed value is
/// still handed out — the same as with the flag set — for an element that
/// took its value constraint, whatever its type, and for elements and
/// attributes whose type is or holds `xs:ID`, `xs:IDREF`, `xs:QName` or
/// `xs:NOTATION` values (derived, list and union types included).
#[test]
fn typed_values_are_kept_without_psvi_retention_on_the_push_api() {
    for version in VERSIONS {
        let ss = SchemaSetBuilder::with_version(version)
            .add_source(IDS, "file:///element_value_constraint_ids.xsd")
            .expect("schema source accepted")
            .compile()
            .unwrap_or_else(|e| panic!("{version:?}: {e}"))
            .into_schema_set();
        let with = push_values(&ss, true);
        let without = push_values(&ss, false);
        assert_eq!(with.len(), without.len());
        for (a, b) in with.iter().zip(&without) {
            let label = format!("{version:?} {}", a.0);
            assert_eq!(a.1, SchemaValidity::Valid, "{label}");
            assert_eq!(b.1, SchemaValidity::Valid, "{label}");
            assert!(a.2.is_some(), "{label}: a typed value with the flag");
            assert_eq!(a.2, b.2, "{label}: the same typed value without it");
        }
        let defaulted: Vec<&str> = without
            .iter()
            .filter(|row| row.3)
            .map(|row| row.0.as_str())
            .collect();
        assert_eq!(defaulted, ["ds", "dt", "dq"], "{version:?}");
    }
}

/// The streaming driver hands each attribute's `SchemaInfo` to
/// `after_attribute`: the same guarantee there.
#[test]
fn typed_values_are_kept_without_psvi_retention_on_the_streaming_driver() {
    struct Attributes<'s> {
        ss: &'s SchemaSet,
        seen: Vec<(String, Option<String>)>,
    }
    impl ValidationEventHandler for Attributes<'_> {
        type Error = std::convert::Infallible;
        fn after_attribute(
            &mut self,
            view: AttributeView<'_>,
            info: &SchemaInfo,
        ) -> Result<(), Self::Error> {
            if view.namespace_uri.is_empty() {
                self.seen.push((
                    view.local_name.to_string(),
                    info.typed_value.as_ref().map(|v| describe(self.ss, v)),
                ));
            }
            Ok(())
        }
    }
    let attributes: String = ATTRIBUTES
        .iter()
        .map(|(name, value)| format!(" {name}=\"{value}\""))
        .collect();
    let children: String = CHILDREN
        .iter()
        .map(|(name, text)| match text {
            Some(text) => format!("<{name}>{text}</{name}>"),
            None => format!("<{name}/>"),
        })
        .collect();
    let xml = format!(r#"<doc xmlns="urn:t" xmlns:p="urn:p"{attributes}>{children}</doc>"#);
    for version in VERSIONS {
        let ss = SchemaSetBuilder::with_version(version)
            .add_source(IDS, "file:///element_value_constraint_ids.xsd")
            .expect("schema source accepted")
            .compile()
            .unwrap_or_else(|e| panic!("{version:?}: {e}"))
            .into_schema_set();
        let mut runs = Vec::new();
        for psvi in [true, false] {
            let validator = SchemaValidator::new(&ss, flags(psvi));
            let (mut errors, mut warnings) = (Vec::new(), Vec::new());
            let mut rt = validator.start_run(CollectingValidationSink {
                errors: &mut errors,
                warnings: &mut warnings,
            });
            let mut handler = Attributes {
                ss: &ss,
                seen: Vec::new(),
            };
            let outcome = drive_quick_xml_with(xml.as_bytes(), &mut rt, &ss, &mut handler)
                .expect("the pass completes");
            drop(rt);
            assert!(errors.is_empty(), "{version:?} psvi={psvi}: {errors:?}");
            assert_eq!(outcome.root_validity, Some(SchemaValidity::Valid));
            runs.push(handler.seen);
        }
        assert_eq!(runs[0].len(), ATTRIBUTES.len(), "{version:?}");
        for (row, (name, _)) in runs[0].iter().zip(ATTRIBUTES) {
            assert_eq!(row.0, *name);
            assert!(row.1.is_some(), "{version:?} {name}: a typed value");
        }
        assert_eq!(
            runs[0], runs[1],
            "{version:?}: the same without PSVI retention"
        );
    }
}

// ── Under a governing type (fix round 1) ──────────────────────────────
//
// Structures §3.3.4.3 clause 5.1: "5.1.1 If E's ·governing type definition·
// is an ·instance-specified type definition·, then D.{value constraint} is a
// valid default for the ·governing type definition· as defined in Element
// Default Valid (Immediate) (§3.3.6.2). 5.1.2 The element information item
// with D.{value constraint}.{lexical form} used as its ·normalized value· is
// locally ·valid· with respect to the ·governing type definition· as defined
// by Element Locally Valid (Type) (§3.3.4.4)." §3.3.6.2: for a simple type or
// simple content, Simple Default Valid (§3.2.6.2: "1 V.{lexical form} is
// ·valid· with respect to T … 2 V.{lexical form} maps to V.{value} in the
// value space of T"); otherwise "2.1 T.{content type}.{variety} = mixed. 2.2
// The particle T.{content type}.{particle} is ·emptiable·".

const GOVERNED: &str = r###"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
    xmlns:t="urn:t" targetNamespace="urn:t" elementFormDefault="qualified">
  <xs:simpleType name="tokOrStr"><xs:union memberTypes="xs:token xs:string"/></xs:simpleType>
  <xs:simpleType name="ab"><xs:restriction base="xs:string">
    <xs:enumeration value="a b"/>
  </xs:restriction></xs:simpleType>
  <xs:simpleType name="intOrStr"><xs:union memberTypes="xs:integer xs:string"/></xs:simpleType>
  <xs:simpleType name="tokens"><xs:list itemType="xs:token"/></xs:simpleType>
  <xs:simpleType name="twoTokens"><xs:restriction base="t:tokens">
    <xs:length value="2"/>
  </xs:restriction></xs:simpleType>
  <xs:complexType name="eo"><xs:sequence>
    <xs:element name="c" minOccurs="0"/>
  </xs:sequence></xs:complexType>
  <xs:complexType name="nothing"/>
  <xs:complexType name="mixedE" mixed="true"><xs:sequence>
    <xs:element name="c" minOccurs="0"/>
  </xs:sequence></xs:complexType>
  <xs:complexType name="mixedNE" mixed="true"><xs:sequence>
    <xs:element name="c"/>
  </xs:sequence></xs:complexType>
  <xs:element name="u" type="t:tokOrStr" default="  a   b  "/>
  <xs:element name="uf" type="t:tokOrStr" fixed="  a   b  "/>
  <xs:element name="n" type="t:intOrStr" default="01"/>
  <xs:element name="s" type="xs:anySimpleType" default="  a   b  "/>
  <xs:element name="l" type="xs:anySimpleType" default=" x   y "/>
  <xs:element name="any" default="some text"/>
</xs:schema>"###;

/// One row: element, `xsi:type` (prefix `t` = `urn:t`, `xs` = XML Schema),
/// then what `element_value_constraint_for_type` gives under that type
/// (`typed_value` described, `valid_default`), and what validation of the
/// empty element gives (`[validity]`, typed value, normalized value, an error
/// code it must report).
struct Governed {
    element: &'static str,
    xsi_type: &'static str,
    typed: Option<&'static str>,
    valid_default: bool,
    valid: bool,
    normalized: Option<&'static str>,
    code: Option<&'static str>,
}

const GOVERNED_ROWS: &[Governed] = &[
    // The reviewer's case: the declaration's {lexical form} "a b" (the union's
    // xs:token member normalizes it) is the normalized value under xs:string.
    Governed {
        element: "u",
        xsi_type: "xs:string",
        typed: Some("String a b"),
        valid_default: true,
        valid: true,
        normalized: Some("a b"),
        code: None,
    },
    // The reviewer's second case: an override restricted to "a b".
    Governed {
        element: "u",
        xsi_type: "t:ab",
        typed: Some("String a b"),
        valid_default: true,
        valid: true,
        normalized: Some("a b"),
        code: None,
    },
    // A fixed value, the same way.
    Governed {
        element: "uf",
        xsi_type: "xs:string",
        typed: Some("String a b"),
        valid_default: true,
        valid: true,
        normalized: Some("a b"),
        code: None,
    },
    // "01" is the integer 1 in the declaration's union, the string "01" under
    // xs:string: valid under the type (5.1.2), but it does not map to the
    // {value} (Simple Default Valid clause 2), so not a valid default (5.1.1).
    Governed {
        element: "n",
        xsi_type: "xs:string",
        typed: Some("String 01"),
        valid_default: false,
        valid: false,
        normalized: Some("01"),
        code: Some("cvc-elt.5.1.1"),
    },
    // xs:anySimpleType declared ({lexical form} kept as written), a type with
    // another whiteSpace governing: its value of that form.
    Governed {
        element: "s",
        xsi_type: "xs:token",
        typed: Some("Token a b"),
        valid_default: true,
        valid: true,
        normalized: Some("  a   b  "),
        code: None,
    },
    // A list type with a facet.
    Governed {
        element: "l",
        xsi_type: "t:twoTokens",
        typed: Some("Token x y"),
        valid_default: true,
        valid: true,
        normalized: Some(" x   y "),
        code: None,
    },
    // Element-only and empty content do not admit a default (clause 2.1).
    Governed {
        element: "any",
        xsi_type: "t:eo",
        typed: None,
        valid_default: false,
        valid: false,
        normalized: None,
        code: Some("cvc-elt.5.1.1"),
    },
    Governed {
        element: "any",
        xsi_type: "t:nothing",
        typed: None,
        valid_default: false,
        valid: false,
        normalized: None,
        code: Some("cvc-elt.5.1.1"),
    },
    // Mixed content with an emptiable particle does; the element has no
    // [schema actual value] (§3.3.5.4 clause 2).
    Governed {
        element: "any",
        xsi_type: "t:mixedE",
        typed: None,
        valid_default: true,
        valid: true,
        normalized: None,
        code: None,
    },
    // Mixed content whose particle is not emptiable does not (clause 2.2).
    Governed {
        element: "any",
        xsi_type: "t:mixedNE",
        typed: None,
        valid_default: false,
        valid: false,
        normalized: None,
        code: None,
    },
];

fn governed_set(version: XsdVersion) -> SchemaSet {
    SchemaSetBuilder::with_version(version)
        .add_source(GOVERNED, "file:///element_value_constraint_governed.xsd")
        .expect("schema source accepted")
        .compile()
        .unwrap_or_else(|e| panic!("{version:?}: {e}"))
        .into_schema_set()
}

/// The type an `xsi:type` value of a row names.
fn named_type(ss: &SchemaSet, qname: &str) -> TypeKey {
    let (prefix, local) = qname.split_once(':').expect("a prefixed name");
    let ns = match prefix {
        "xs" => "http://www.w3.org/2001/XMLSchema",
        _ => "urn:t",
    };
    let (ns, local) = (ss.name_table.add(ns), ss.name_table.add(local));
    ss.lookup_type(Some(ns), local)
        .or_else(|| ss.get_built_in_type_by_qname(Some(ns), local))
        .unwrap_or_else(|| panic!("the type {qname}"))
}

/// A row's typed value as text: the type code and the string (list members
/// joined).
fn governed_text(value: &XmlValue) -> String {
    format!("{:?} {}", value.type_code, value.to_string_value())
}

#[test]
fn the_value_under_a_governing_type_is_the_lexical_form_read_under_it() {
    for version in VERSIONS {
        let ss = governed_set(version);
        let mut wrong = Vec::new();
        for row in GOVERNED_ROWS {
            let key = top_level(&ss, "urn:t", row.element);
            let ty = named_type(&ss, row.xsi_type);
            let vc = ss
                .element_value_constraint_for_type(key, ty)
                .expect("a value constraint");
            let got = (vc.typed_value.as_ref().map(governed_text), vc.valid_default);
            let want = (row.typed.map(str::to_string), row.valid_default);
            if got != want {
                wrong.push(format!(
                    "{} xsi:type={}: {got:?}, want {want:?}",
                    row.element, row.xsi_type
                ));
            }
        }
        assert!(wrong.is_empty(), "{version:?}: {wrong:#?}");
    }
}

/// The declaration's own record: its type governs, and the record's {value}
/// for mixed content is an `xs:string` while the element has no typed value.
#[test]
fn the_declared_type_admits_its_own_value_constraint() {
    for version in VERSIONS {
        let ss = governed_set(version);
        for element in ["u", "uf", "n", "s", "l", "any"] {
            let key = top_level(&ss, "urn:t", element);
            let vc = ss
                .element_value_constraint(key)
                .expect("a value constraint");
            assert!(vc.valid_default, "{version:?} {element}");
        }
        let any = top_level(&ss, "urn:t", "any");
        let record = ss
            .element_value_constraint(any)
            .expect("a value constraint");
        assert_eq!(
            record.typed_value.as_ref().map(governed_text).as_deref(),
            Some("String some text"),
            "{version:?}: the record's value"
        );
        let any_type = TypeKey::Complex(ss.any_type_key());
        let applied = ss
            .element_value_constraint_for_type(any, any_type)
            .expect("a value constraint");
        assert!(applied.valid_default, "{version:?}");
        assert!(
            applied.typed_value.is_none(),
            "{version:?}: no actual value"
        );
        assert_eq!(applied.lexical_form, "some text", "{version:?}");
        let n = top_level(&ss, "urn:t", "n");
        let declared = ss.element_value_constraint(n).expect("a value constraint");
        assert_eq!(declared.lexical_form, "01", "{version:?}");
        assert_eq!(
            declared.typed_value.as_ref().map(governed_text).as_deref(),
            Some("Integer 1"),
            "{version:?}"
        );
    }
}

const XSI_DECLS: &str = r#"xmlns:t="urn:t" xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance""#;

/// Validation of `<t:element xsi:type="…"/>` on the three drivers: the push
/// API (with and without PSVI typed values), the streaming driver and the
/// navigator driver. Returns, per driver, the root's validity and the errors'
/// constraint codes, and the push API's typed and normalized values.
#[allow(clippy::type_complexity)]
fn validate_governed(
    ss: &SchemaSet,
    row: &Governed,
    psvi: bool,
) -> (
    Vec<(&'static str, bool, Vec<String>)>,
    Option<String>,
    Option<String>,
) {
    let validator = SchemaValidator::new(ss, flags(psvi));
    let mut drivers = Vec::new();

    // Push API.
    let (mut errors, mut warnings) = (Vec::new(), Vec::new());
    let mut rt = validator.start_run(CollectingValidationSink {
        errors: &mut errors,
        warnings: &mut warnings,
    });
    let ns = snapshot(
        ss,
        &[("t", "urn:t"), ("xs", "http://www.w3.org/2001/XMLSchema")],
    );
    let _ = rt.validate_element(row.element, "urn:t", Some(row.xsi_type), None, &ns);
    let _ = rt.validate_attribute(
        "type",
        "http://www.w3.org/2001/XMLSchema-instance",
        row.xsi_type,
    );
    let _ = rt.validate_end_of_attributes();
    let info = rt.validate_end_element();
    rt.end_validation().expect("no operational failure");
    drop(rt);
    let typed = info.typed_value.as_ref().map(governed_text);
    let normalized = info.normalized_value.clone();
    drivers.push((
        "push",
        info.validity == SchemaValidity::Valid,
        errors.iter().map(|e| e.constraint.to_string()).collect(),
    ));

    // Streaming and navigator drivers.
    let xml = format!(
        r#"<t:{0} {XSI_DECLS} xsi:type="{1}"/>"#,
        row.element, row.xsi_type
    );
    let (mut errors, mut warnings) = (Vec::new(), Vec::new());
    let mut rt = validator.start_run(CollectingValidationSink {
        errors: &mut errors,
        warnings: &mut warnings,
    });
    let outcome = xsd_schema::validation::drive_quick_xml(xml.as_bytes(), &mut rt, ss)
        .expect("the pass completes");
    drop(rt);
    drivers.push((
        "streaming",
        outcome.root_validity == Some(SchemaValidity::Valid) && errors.is_empty(),
        errors.iter().map(|e| e.constraint.to_string()).collect(),
    ));
    let doc = roxmltree::Document::parse(&xml).expect("well-formed");
    let nav = xsd_schema::RoXmlNavigator::new(&doc);
    let (mut errors, mut warnings) = (Vec::new(), Vec::new());
    let mut rt = validator.start_run(CollectingValidationSink {
        errors: &mut errors,
        warnings: &mut warnings,
    });
    let outcome =
        xsd_schema::validation::drive_navigator(&nav, &mut rt, ss).expect("the pass completes");
    drop(rt);
    drivers.push((
        "navigator",
        outcome.root_validity == Some(SchemaValidity::Valid) && errors.is_empty(),
        errors.iter().map(|e| e.constraint.to_string()).collect(),
    ));
    (drivers, typed, normalized)
}

#[test]
fn validation_applies_the_lexical_form_under_the_governing_type() {
    for version in VERSIONS {
        let ss = governed_set(version);
        let mut wrong = Vec::new();
        for row in GOVERNED_ROWS {
            for psvi in [true, false] {
                let label = format!("{} xsi:type={} psvi={psvi}", row.element, row.xsi_type);
                let (drivers, typed, normalized) = validate_governed(&ss, row, psvi);
                for (driver, valid, codes) in &drivers {
                    if *valid != row.valid {
                        wrong.push(format!("{label} {driver}: valid={valid} {codes:?}"));
                    }
                    if let Some(code) = row.code {
                        if !codes.iter().any(|c| c == code) {
                            wrong.push(format!("{label} {driver}: no {code} in {codes:?}"));
                        }
                    }
                }
                if typed.as_deref() != row.typed {
                    wrong.push(format!("{label}: typed {typed:?}, want {:?}", row.typed));
                }
                if psvi && normalized.as_deref() != row.normalized {
                    wrong.push(format!(
                        "{label}: normalized {normalized:?}, want {:?}",
                        row.normalized
                    ));
                }
            }
        }
        assert!(wrong.is_empty(), "{version:?}: {wrong:#?}");
    }
}

/// XSD 1.1: a type alternative is not an ·instance-specified type definition·,
/// so clause 5.1.1 does not apply — only 5.1.2: the lexical form is validated
/// under the selected type. A union member that reads "01" as a string, and an
/// element-only type (the element has no children; no value), are valid.
/// (Default alternatives: a simple-content restriction to a union member, which
/// Type Derivation OK (Simple) clause 2.2.4 allows, is refused by this crate's
/// derivation check — see the batch report.)
#[cfg(feature = "xsd11")]
#[test]
fn a_type_alternative_is_not_checked_as_a_valid_default() {
    // A type alternative without a test is the default alternative: it is
    // always selected (§3.12.4).
    const CTA: &str = r###"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
        xmlns:t="urn:t" targetNamespace="urn:t">
      <xs:simpleType name="intOrStr"><xs:union memberTypes="xs:integer xs:string"/></xs:simpleType>
      <xs:complexType name="m" mixed="true">
        <xs:sequence><xs:element name="c" minOccurs="0"/></xs:sequence>
      </xs:complexType>
      <xs:complexType name="eo"><xs:complexContent><xs:restriction base="t:m">
        <xs:sequence><xs:element name="c" minOccurs="0"/></xs:sequence>
      </xs:restriction></xs:complexContent></xs:complexType>
      <xs:element name="v" type="t:intOrStr" default="01">
        <xs:alternative type="xs:string"/>
      </xs:element>
      <xs:element name="w" type="t:m" default="text">
        <xs:alternative type="t:eo"/>
      </xs:element>
    </xs:schema>"###;
    let ss = SchemaSetBuilder::with_version(XsdVersion::V1_1)
        .add_source(CTA, "file:///element_value_constraint_cta2.xsd")
        .expect("schema source accepted")
        .compile()
        .unwrap_or_else(|e| panic!("{e}"))
        .into_schema_set();
    let t = |local: &str| {
        ss.lookup_type(Some(ss.name_table.add("urn:t")), ss.name_table.add(local))
            .expect("a type")
    };
    // The accessor: valid under the type (a value), not a valid default.
    let v = top_level(&ss, "urn:t", "v");
    let string = TypeKey::Simple(ss.builtin_types().string);
    let under_str = ss
        .element_value_constraint_for_type(v, string)
        .expect("a value constraint");
    assert_eq!(
        under_str.typed_value.as_ref().map(governed_text).as_deref(),
        Some("String 01")
    );
    assert!(!under_str.valid_default);
    let w = top_level(&ss, "urn:t", "w");
    let under_eo = ss
        .element_value_constraint_for_type(w, t("eo"))
        .expect("a value constraint");
    assert!(under_eo.typed_value.is_none());
    assert!(!under_eo.valid_default);
    // Validation: both elements valid.
    for (element, typed) in [("v", Some("String 01")), ("w", None)] {
        for psvi in [true, false] {
            let validator = SchemaValidator::new(&ss, flags(psvi));
            let (mut errors, mut warnings) = (Vec::new(), Vec::new());
            let mut rt = validator.start_run(CollectingValidationSink {
                errors: &mut errors,
                warnings: &mut warnings,
            });
            let ns = snapshot(&ss, &[("t", "urn:t")]);
            let _ = rt.validate_element(element, "urn:t", None, None, &ns);
            let selected = rt.validate_end_of_attributes();
            let _ = rt.take_deferred_attribute_results();
            assert_eq!(
                selected.schema_type,
                Some(if element == "v" { string } else { t("eo") }),
                "{element}: the alternative"
            );
            let info = rt.validate_end_element();
            rt.end_validation().expect("no operational failure");
            drop(rt);
            assert!(errors.is_empty(), "{element} psvi={psvi}: {errors:?}");
            assert_eq!(info.validity, SchemaValidity::Valid, "{element}");
            assert_eq!(
                info.typed_value.as_ref().map(governed_text).as_deref(),
                typed,
                "{element} psvi={psvi}"
            );
        }
    }
}

/// The typed build writes the value an element takes into the tree: under an
/// `xsi:type` the {lexical form} read under it (white space normalized for
/// it), so the node's string value and typed value are the ones validation
/// gives.
#[cfg(feature = "xsd11")]
#[test]
fn the_typed_build_writes_the_lexical_form_under_the_governing_type() {
    use bumpalo::Bump;
    use xsd_schema::document::{
        build_typed_document_with, BufferDocumentOptions, TypedBuildOptions,
    };
    use xsd_schema::validation::NoopHandler;
    use xsd_schema::xpath::api::XPathExpr;
    use xsd_schema::xpath::{XPathContext, XmlItem};

    for version in VERSIONS {
        let ss = governed_set(version);
        for (element, xsi_type, valid, string, data) in [
            ("u", "xs:string", true, "a b", "a b"),
            ("u", "t:ab", true, "a b", "a b"),
            ("uf", "xs:string", true, "a b", "a b"),
            ("s", "xs:token", true, "a b", "a b"),
            ("l", "t:twoTokens", true, "x y", "x|y"),
            ("n", "xs:string", false, "", ""),
            ("any", "t:eo", false, "", ""),
            ("any", "t:mixedE", true, "", ""),
        ] {
            let label = format!("{version:?} {element} xsi:type={xsi_type}");
            let xml = format!(r#"<t:{element} {XSI_DECLS} xsi:type="{xsi_type}"/>"#);
            let validator = SchemaValidator::new(&ss, ValidationFlags::default());
            let (mut errors, mut warnings) = (Vec::new(), Vec::new());
            let arena = Bump::new();
            let mut rt = validator.start_run(CollectingValidationSink {
                errors: &mut errors,
                warnings: &mut warnings,
            });
            let built = build_typed_document_with(
                xml.as_bytes(),
                &arena,
                &ss,
                &mut rt,
                &mut NoopHandler,
                TypedBuildOptions::default().with_document_options(BufferDocumentOptions::full()),
            )
            .expect("the build completes");
            rt.end_validation().expect("no operational failure");
            drop(rt);
            assert_eq!(
                built.outcome.root_validity == Some(SchemaValidity::Valid) && errors.is_empty(),
                valid,
                "{label}: {errors:?}"
            );
            if !valid {
                continue;
            }
            let ctx = XPathContext::new(&ss.name_table)
                .with_schema_set(&ss)
                .with_namespaces(snapshot(&ss, &[("t", "urn:t")]));
            let eval = |expr: &str| -> String {
                let compiled = XPathExpr::compile(expr, &ctx).expect("compiles");
                compiled
                    .evaluator(&ctx)
                    .run_with_node(built.document.create_navigator())
                    .expect("evaluates")
                    .into_vec()
                    .iter()
                    .map(|item| match item {
                        XmlItem::Atomic(v) => v.to_string_value(),
                        XmlItem::Node(_) => "node".to_string(),
                    })
                    .collect::<Vec<_>>()
                    .join("|")
            };
            assert_eq!(eval(&format!("string(/t:{element})")), string, "{label}");
            if !data.is_empty() {
                assert_eq!(eval(&format!("data(/t:{element})")), data, "{label}");
            }
        }
    }
}
