//! Values of list and union types end to end, through the public drivers and in
//! both XSD versions: `xs:ENTITY` values inside unions, enumeration of list
//! values, and identity constraints over list keys.
//!
//! Every case runs on streaming validation with and without PSVI typed values
//! and on the navigator driver, in XSD 1.0 and XSD 1.1 mode.

use std::collections::HashSet;

use xsd_schema::validation::{
    drive_navigator, drive_quick_xml, CollectingValidationSink, SchemaValidator, SchemaValidity,
    ValidationFlags,
};
use xsd_schema::{RoXmlNavigator, SchemaSet, SchemaSetBuilder, XsdVersion};

use SchemaValidity::{Invalid, Valid};

const VERSIONS: [XsdVersion; 2] = [XsdVersion::V1_0, XsdVersion::V1_1];

// ── Harness ───────────────────────────────────────────────────────────────

fn schema(version: XsdVersion, body: &str) -> SchemaSet {
    let xsd =
        format!(r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">{body}</xs:schema>"#);
    SchemaSetBuilder::with_version(version)
        .add_source(&xsd, "file:///list_union_values.xsd")
        .expect("schema source accepted")
        .compile()
        .expect("schema compiles")
        .into_schema_set()
}

fn validator(ss: &SchemaSet, flags: ValidationFlags) -> SchemaValidator<'_> {
    let flags = flags | ValidationFlags::PROCESS_IDENTITY_CONSTRAINTS;
    #[cfg(feature = "xsd11")]
    {
        SchemaValidator::new_fragment_buffer(ss, flags)
    }
    #[cfg(not(feature = "xsd11"))]
    {
        SchemaValidator::new(ss, flags)
    }
}

/// Root validity and error constraint codes of one run on `driver`.
fn run(
    ss: &SchemaSet,
    xml: &str,
    driver: &str,
    entities: Option<&[&str]>,
) -> (Option<SchemaValidity>, Vec<String>) {
    let flags = match driver {
        "stream/no-psvi" => ValidationFlags::default() & !ValidationFlags::BUILD_PSVI_TYPED_VALUES,
        _ => ValidationFlags::default(),
    };
    let v = validator(ss, flags);
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let validity = {
        let mut rt = v.start_run(CollectingValidationSink {
            errors: &mut errors,
            warnings: &mut warnings,
        });
        if let Some(names) = entities {
            rt.set_unparsed_entities(names.iter().map(|s| s.to_string()).collect::<HashSet<_>>());
        }
        if driver == "navigator" {
            let doc = roxmltree::Document::parse(xml).expect("well-formed instance");
            let nav = RoXmlNavigator::new(&doc);
            drive_navigator(&nav, &mut rt, ss)
                .expect("navigator drive completes")
                .root_validity
        } else {
            drive_quick_xml(xml.as_bytes(), &mut rt, ss)
                .expect("stream drive completes")
                .root_validity
        }
    };
    (
        validity,
        errors.iter().map(|e| e.constraint.to_string()).collect(),
    )
}

/// Every driver reaches `expected`; with `code`, some error starts with it,
/// and without, there is no error at all.
fn check(
    ss: &SchemaSet,
    xml: &str,
    expected: SchemaValidity,
    code: Option<&str>,
    entities: Option<&[&str]>,
) {
    for driver in ["stream", "stream/no-psvi", "navigator"] {
        let (validity, errors) = run(ss, xml, driver, entities);
        assert_eq!(
            validity,
            Some(expected),
            "{driver}: root [validity] of {xml} (errors {errors:?})"
        );
        match code {
            Some(code) => assert!(
                errors.iter().any(|e| e.starts_with(code)),
                "{driver}: expected a {code} error for {xml}, got {errors:?}"
            ),
            None => assert!(errors.is_empty(), "{driver}: {xml}: errors {errors:?}"),
        }
    }
}

// ── ENTITY values inside unions ───────────────────────────────────────────

/// Structures §3.16.4 String Valid clause 3: "Every ·ENTITY value· in V is a
/// ·declared entity name·". The ENTITY values of a union value are those of
/// the member type that validated it — for a member type that is a list of
/// `xs:ENTITY` (`xs:ENTITIES`), its items.
#[test]
fn entity_values_inside_unions_must_be_declared() {
    const ENTITIES: &[&str] = &["logo"];
    for version in VERSIONS {
        let ss = schema(
            version,
            r#"<xs:simpleType name="intOrEnts"><xs:union memberTypes="xs:integer xs:ENTITIES"/></xs:simpleType>
               <xs:simpleType name="intOrEnt"><xs:union memberTypes="xs:integer xs:ENTITY"/></xs:simpleType>
               <xs:simpleType name="nested"><xs:union memberTypes="xs:date intOrEnts"/></xs:simpleType>
               <xs:element name="r"><xs:complexType><xs:sequence>
                 <xs:element name="e" type="intOrEnts" minOccurs="0"/>
               </xs:sequence>
               <xs:attribute name="us" type="intOrEnts"/>
               <xs:attribute name="u" type="intOrEnt"/>
               <xs:attribute name="n" type="nested"/>
               </xs:complexType></xs:element>"#,
        );
        let undeclared = Some("cvc-datatype-valid.1.2.1");
        let cases: [(&str, SchemaValidity, Option<&str>); 9] = [
            (r#"<r us="nologo logo"/>"#, Invalid, undeclared),
            (r#"<r us="logo nologo"/>"#, Invalid, undeclared),
            (r#"<r us="logo"/>"#, Valid, None),
            (r#"<r us="logo logo"/>"#, Valid, None),
            (r#"<r us="5"/>"#, Valid, None),
            (r#"<r u="nologo"/>"#, Invalid, undeclared),
            (r#"<r u="logo"/>"#, Valid, None),
            (r#"<r n="nologo"/>"#, Invalid, undeclared),
            (r#"<r><e>logo nologo</e></r>"#, Invalid, undeclared),
        ];
        for (xml, expected, code) in cases {
            check(&ss, xml, expected, code, Some(ENTITIES));
        }
        // Without unparsed-entity information nothing is checked.
        check(&ss, r#"<r us="nologo logo"/>"#, Valid, None, None);
    }
}

// ── Identity constraints over list keys (both versions) ──────────────────

fn list_key_schema(version: XsdVersion, key_item: &str, ref_item: &str) -> SchemaSet {
    schema(
        version,
        &format!(
            r#"<xs:simpleType name="intOrString"><xs:union memberTypes="xs:integer xs:string"/></xs:simpleType>
               <xs:element name="root"><xs:complexType><xs:sequence>
                 <xs:element name="item" minOccurs="0" maxOccurs="unbounded"><xs:complexType>
                   <xs:attribute name="k"><xs:simpleType><xs:list itemType="{key_item}"/></xs:simpleType></xs:attribute>
                 </xs:complexType></xs:element>
                 <xs:element name="ref" minOccurs="0" maxOccurs="unbounded"><xs:complexType>
                   <xs:attribute name="r"><xs:simpleType><xs:list itemType="{ref_item}"/></xs:simpleType></xs:attribute>
                 </xs:complexType></xs:element>
               </xs:sequence></xs:complexType>
               <xs:key name="itemKey"><xs:selector xpath="item"/><xs:field xpath="@k"/></xs:key>
               <xs:keyref name="itemRef" refer="itemKey"><xs:selector xpath="ref"/><xs:field xpath="@r"/></xs:keyref>
               </xs:element>"#
        ),
    )
}

fn keys(items: &[&str], refs: &[&str]) -> String {
    let mut xml = String::from("<root>");
    for k in items {
        xml.push_str(&format!(r#"<item k="{k}"/>"#));
    }
    for r in refs {
        xml.push_str(&format!(r#"<ref r="{r}"/>"#));
    }
    xml.push_str("</root>");
    xml
}

/// Structures §3.11.4 clauses 4.2.2 and 4.3 compare key-sequences "pairwise
/// equal or identical, as defined by Equality and Identity in [XML Schema:
/// Datatypes]"; Datatypes §2.2.1 makes two lists identical when "they have the
/// same length and … the items of A are pairwise identical to the items of B",
/// and values of different primitive datatypes distinct.
#[test]
fn list_keys_compare_member_for_member_in_both_versions() {
    const DUP: &str = "cvc-identity-constraint.4.2.2";
    const NO_MATCH: &str = "cvc-identity-constraint.4.3";
    for version in VERSIONS {
        let union = list_key_schema(version, "intOrString", "intOrString");
        check(
            &union,
            &keys(&["1 x", "01 x"], &[]),
            Invalid,
            Some(DUP),
            None,
        );
        check(
            &union,
            &keys(&["x 1", "x +1"], &[]),
            Invalid,
            Some(DUP),
            None,
        );
        for distinct in [["1 x", "1 y"], ["1 x", "1.0 x"], ["1 x", "1 x 2"]] {
            check(&union, &keys(&distinct, &[]), Valid, None, None);
        }
        check(&union, &keys(&["1 x"], &["01 x"]), Valid, None, None);
        // The integer 1 is not the string "1".
        let strings = list_key_schema(version, "intOrString", "xs:string");
        check(
            &strings,
            &keys(&["1 x"], &["1 x"]),
            Invalid,
            Some(NO_MATCH),
            None,
        );
        check(&strings, &keys(&["a x"], &["a x"]), Valid, None, None);
        // xs:integer and xs:decimal share the decimal value space.
        let numbers = list_key_schema(version, "xs:integer", "xs:decimal");
        check(&numbers, &keys(&["1 2"], &["1.0 2"]), Valid, None, None);
        check(
            &numbers,
            &keys(&["1 2", "01 2"], &[]),
            Invalid,
            Some(DUP),
            None,
        );
    }
}

// ── Enumeration of list values ────────────────────────────────────────────

/// Datatypes §4.3.5.4: a value is facet-valid with respect to `enumeration`
/// "if and only if the value is equal or identical to one of the values
/// specified in {value}"; §2.4.1.2: "enumerated values are compared to the
/// entire ·list·", and two lists are equal when they have the same length and
/// their items are pairwise equal. So a list enumeration compares values — each
/// member in the value space of the type that validated it — while `pattern`
/// stays lexical (§4.3.4: it constrains the ·lexical space·).
#[test]
fn list_enumeration_compares_values_and_patterns_stay_lexical() {
    const ENUM: Option<&str> = Some("cvc-enumeration-valid");
    const PATTERN: Option<&str> = Some("cvc-pattern-valid");
    for version in VERSIONS {
        let ss = schema(
            version,
            r#"<xs:simpleType name="intOrString"><xs:union memberTypes="xs:integer xs:string"/></xs:simpleType>
               <xs:simpleType name="decOrString"><xs:union memberTypes="xs:decimal xs:string"/></xs:simpleType>
               <xs:simpleType name="ints"><xs:restriction>
                 <xs:simpleType><xs:list itemType="xs:integer"/></xs:simpleType>
                 <xs:enumeration value="1 2"/><xs:enumeration value=" 7 "/></xs:restriction></xs:simpleType>
               <xs:simpleType name="mixed"><xs:restriction>
                 <xs:simpleType><xs:list itemType="intOrString"/></xs:simpleType>
                 <xs:enumeration value="1 x"/></xs:restriction></xs:simpleType>
               <xs:simpleType name="decs"><xs:restriction>
                 <xs:simpleType><xs:list itemType="decOrString"/></xs:simpleType>
                 <xs:enumeration value="1.0 x"/></xs:restriction></xs:simpleType>
               <xs:simpleType name="lexical"><xs:restriction>
                 <xs:simpleType><xs:list itemType="xs:integer"/></xs:simpleType>
                 <xs:pattern value="1 2"/></xs:restriction></xs:simpleType>
               <xs:element name="r"><xs:complexType><xs:sequence>
                 <xs:element name="e" type="ints" minOccurs="0"/>
               </xs:sequence>
               <xs:attribute name="i" type="ints"/>
               <xs:attribute name="m" type="mixed"/>
               <xs:attribute name="d" type="decs"/>
               <xs:attribute name="p" type="lexical"/>
               </xs:complexType></xs:element>"#,
        );
        let cases: [(&str, SchemaValidity, Option<&str>); 18] = [
            (r#"<r i="1 2"/>"#, Valid, None),
            (r#"<r i="01 2"/>"#, Valid, None),
            (r#"<r i=" +1   2 "/>"#, Valid, None),
            (r#"<r i="7"/>"#, Valid, None),
            (r#"<r i="2 1"/>"#, Invalid, ENUM),
            (r#"<r i="1 2 3"/>"#, Invalid, ENUM),
            (r#"<r i="1"/>"#, Invalid, ENUM),
            (r#"<r><e>01 2</e></r>"#, Valid, None),
            (r#"<r><e>2 1</e></r>"#, Invalid, ENUM),
            (r#"<r m="1 x"/>"#, Valid, None),
            (r#"<r m="01 x"/>"#, Valid, None),
            (r#"<r m="+1 x"/>"#, Valid, None),
            // `1.0` is not an xs:integer, so the string member takes it.
            (r#"<r m="1.0 x"/>"#, Invalid, ENUM),
            (r#"<r m="1 y"/>"#, Invalid, ENUM),
            (r#"<r d="1 x"/>"#, Valid, None),
            (r#"<r d="1.00 x"/>"#, Valid, None),
            (r#"<r p="1 2"/>"#, Valid, None),
            (r#"<r p="01 2"/>"#, Invalid, PATTERN),
        ];
        for (xml, expected, code) in cases {
            check(&ss, xml, expected, code, None);
        }
    }
}

/// The same rule for a union whose member type that validated the value is a
/// list type: the enumeration value is mapped through the member types, as
/// the union's lexical mapping does, and compared as a whole list.
#[test]
fn union_enumeration_compares_a_list_member_as_a_list() {
    const ENUM: Option<&str> = Some("cvc-enumeration-valid");
    for version in VERSIONS {
        let ss = schema(
            version,
            r#"<xs:simpleType name="ints"><xs:list itemType="xs:integer"/></xs:simpleType>
               <xs:simpleType name="u"><xs:restriction>
                 <xs:simpleType><xs:union memberTypes="xs:boolean ints"/></xs:simpleType>
                 <xs:enumeration value="1 2"/><xs:enumeration value="true"/></xs:restriction></xs:simpleType>
               <xs:element name="r"><xs:complexType><xs:attribute name="u" type="u"/></xs:complexType></xs:element>"#,
        );
        let cases: [(&str, SchemaValidity, Option<&str>); 5] = [
            (r#"<r u="1 2"/>"#, Valid, None),
            (r#"<r u="01 2"/>"#, Valid, None),
            (r#"<r u="true"/>"#, Valid, None),
            // `1` is an xs:boolean (the first member type), so it is `true`.
            (r#"<r u="1"/>"#, Valid, None),
            (r#"<r u="2 1"/>"#, Invalid, ENUM),
        ];
        for (xml, expected, code) in cases {
            check(&ss, xml, expected, code, None);
        }
    }
}

// ── QName members of lists: the prefix must be in scope ──────────────────

/// Datatypes §3.3.18: the lexical mapping of `xs:QName` "depends on the
/// namespace bindings in scope where the literal occurs", so a member of a
/// list that a QName (member) type validated, with a prefix bound nowhere, is
/// not in the lexical space: `cvc-datatype-valid.1.2.1`, as for an atomic
/// value — for a list of `xs:QName`, for a list of a union with an `xs:QName`
/// member (first or last), and for a nested union; on attributes and on
/// element content.
#[test]
fn qname_members_of_lists_need_a_declared_prefix() {
    const UNDECLARED: Option<&str> = Some("cvc-datatype-valid.1.2.1");
    for version in VERSIONS {
        let ss = schema(
            version,
            r#"<xs:simpleType name="lq"><xs:list itemType="xs:QName"/></xs:simpleType>
               <xs:simpleType name="qi"><xs:union memberTypes="xs:QName xs:integer"/></xs:simpleType>
               <xs:simpleType name="iq"><xs:union memberTypes="xs:integer xs:QName"/></xs:simpleType>
               <xs:simpleType name="nested"><xs:union memberTypes="xs:date qi"/></xs:simpleType>
               <xs:simpleType name="lqi"><xs:list itemType="qi"/></xs:simpleType>
               <xs:simpleType name="liq"><xs:list itemType="iq"/></xs:simpleType>
               <xs:simpleType name="ln"><xs:list itemType="nested"/></xs:simpleType>
               <xs:element name="r"><xs:complexType><xs:sequence>
                 <xs:element name="eq" type="lq" minOccurs="0"/>
                 <xs:element name="e" type="lqi" minOccurs="0"/>
                 <xs:element name="en" type="ln" minOccurs="0"/>
               </xs:sequence>
               <xs:attribute name="q" type="lq"/>
               <xs:attribute name="a" type="lqi"/>
               <xs:attribute name="b" type="liq"/>
               <xs:attribute name="n" type="ln"/>
               <xs:attribute name="u" type="qi"/>
               </xs:complexType></xs:element>"#,
        );
        let cases: [(&str, SchemaValidity, Option<&str>); 22] = [
            // Attributes.
            (r#"<r q="p:a"/>"#, Invalid, UNDECLARED),
            (r#"<r xmlns:x="urn:x" q="x:a p:b"/>"#, Invalid, UNDECLARED),
            (r#"<r xmlns:p="urn:p" q="p:a p:b local"/>"#, Valid, None),
            (r#"<r a="p:a"/>"#, Invalid, UNDECLARED),
            (r#"<r a="5 p:a"/>"#, Invalid, UNDECLARED),
            (r#"<r xmlns:p="urn:p" a="p:a 5"/>"#, Valid, None),
            (r#"<r a="local 5 xml:lang"/>"#, Valid, None),
            (r#"<r b="p:a"/>"#, Invalid, UNDECLARED),
            (r#"<r b="5 p:a"/>"#, Invalid, UNDECLARED),
            (r#"<r xmlns:p="urn:p" b="5 p:a"/>"#, Valid, None),
            (r#"<r n="2020-01-01 p:a"/>"#, Invalid, UNDECLARED),
            (r#"<r xmlns:p="urn:p" n="2020-01-01 p:a 5"/>"#, Valid, None),
            // The atomic union (unchanged).
            (r#"<r u="p:a"/>"#, Invalid, UNDECLARED),
            (r#"<r xmlns:p="urn:p" u="p:a"/>"#, Valid, None),
            // Element content.
            (r#"<r><eq>p:a</eq></r>"#, Invalid, UNDECLARED),
            (
                r#"<r xmlns:x="urn:x"><eq>x:a p:b</eq></r>"#,
                Invalid,
                UNDECLARED,
            ),
            (r#"<r xmlns:p="urn:p"><eq>p:a p:b</eq></r>"#, Valid, None),
            (r#"<r><e>p:a</e></r>"#, Invalid, UNDECLARED),
            (r#"<r><e>5 p:a</e></r>"#, Invalid, UNDECLARED),
            (r#"<r xmlns:p="urn:p"><e>p:a 5</e></r>"#, Valid, None),
            (r#"<r><en>p:a</en></r>"#, Invalid, UNDECLARED),
            (
                r#"<r xmlns:p="urn:p"><en>2020-01-01 p:a</en></r>"#,
                Valid,
                None,
            ),
        ];
        let mut wrong = Vec::new();
        for (xml, expected, code) in cases {
            for driver in ["stream", "stream/no-psvi", "navigator"] {
                let (validity, errors) = run(&ss, xml, driver, None);
                let ok = validity == Some(expected)
                    && match code {
                        Some(code) => errors.iter().any(|e| e.starts_with(code)),
                        None => errors.is_empty(),
                    };
                if !ok {
                    wrong.push(format!(
                        "{version:?} {driver}: {xml} => {validity:?} {errors:?}"
                    ));
                }
            }
        }
        assert!(
            wrong.is_empty(),
            "{} wrong:\n{}",
            wrong.len(),
            wrong.join("\n")
        );
    }
}

// ── fixed values of lists and unions compare by value ─────────────────────

/// Structures §3.3.4.3 clause 5.2.2.2.2 and Attribute Locally Valid (Use):
/// the actual value "is equal or identical to" the fixed value — a value
/// comparison, and Datatypes §2.2.1: restrictions "do not create new values",
/// so it does not matter which member type of a union (a pattern-restricted
/// `xs:integer` or `xs:integer` itself) validated a list member. On elements,
/// declared attributes and attributes a wildcard admits.
#[test]
fn fixed_list_and_union_values_compare_by_value() {
    const ELT: Option<&str> = Some("cvc-elt.5.2.2");
    const ATT: Option<&str> = Some("cvc-attribute.4");
    for version in VERSIONS {
        let ss = schema(
            version,
            r#"<xs:simpleType name="signed"><xs:restriction base="xs:integer">
                 <xs:pattern value="\+[0-9]+"/></xs:restriction></xs:simpleType>
               <xs:simpleType name="si"><xs:union memberTypes="signed xs:integer"/></xs:simpleType>
               <xs:simpleType name="lsi"><xs:list itemType="si"/></xs:simpleType>
               <xs:attribute name="w" type="lsi" fixed="1 2"/>
               <xs:element name="r"><xs:complexType><xs:sequence>
                 <xs:element name="f" type="lsi" fixed="1 2" minOccurs="0"/>
                 <xs:element name="u" type="si" fixed="1" minOccurs="0"/>
               </xs:sequence>
               <xs:attribute name="g" type="lsi" fixed="1 2"/>
               <xs:attribute name="h" type="si" fixed="1"/>
               <xs:anyAttribute processContents="strict"/>
               </xs:complexType></xs:element>"#,
        );
        let cases: [(&str, SchemaValidity, Option<&str>); 14] = [
            (r#"<r g="1 2"/>"#, Valid, None),
            (r#"<r g="+1 2"/>"#, Valid, None),
            (r#"<r g="01 +2"/>"#, Valid, None),
            (r#"<r g="1 3"/>"#, Invalid, ATT),
            (r#"<r g="1"/>"#, Invalid, ATT),
            (r#"<r><f>+1 2</f></r>"#, Valid, None),
            (r#"<r><f/></r>"#, Valid, None),
            (r#"<r><f>1 3</f></r>"#, Invalid, ELT),
            (r#"<r h="+1"/>"#, Valid, None),
            (r#"<r h="2"/>"#, Invalid, ATT),
            (r#"<r><u>+1</u></r>"#, Valid, None),
            (r#"<r><u>+2</u></r>"#, Invalid, ELT),
            (r#"<r w="+1 2"/>"#, Valid, None),
            (r#"<r w="1 3"/>"#, Invalid, ATT),
        ];
        let mut wrong = Vec::new();
        for (xml, expected, code) in cases {
            for driver in ["stream", "stream/no-psvi", "navigator"] {
                let (validity, errors) = run(&ss, xml, driver, None);
                let ok = validity == Some(expected)
                    && match code {
                        Some(code) => errors.iter().any(|e| e.starts_with(code)),
                        None => errors.is_empty(),
                    };
                if !ok {
                    wrong.push(format!(
                        "{version:?} {driver}: {xml} => {validity:?} {errors:?}"
                    ));
                }
            }
        }
        assert!(
            wrong.is_empty(),
            "{} wrong:\n{}",
            wrong.len(),
            wrong.join("\n")
        );
    }
}
