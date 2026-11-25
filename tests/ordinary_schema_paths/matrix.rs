//! The matrix of `tests/ordinary_schema_paths.rs`: schemas, documents and the
//! observation of one validation.
//!
//! Nothing here depends on the current dateTime, and nothing uses an item the
//! crate did not have before validation runs got a clock of their own, so the
//! same file also builds against that earlier crate: a probe that prints
//! [`observe`] for the whole matrix from both builds shows that ordinary
//! schemas behave as they did.

use bumpalo::Bump;
use xsd_schema::document::{
    build_typed_document_with, BufferDocument, BufferDocumentBuilder, BufferDocumentOptions,
    TypedBuildOptions, TypedBuilderHandler,
};
use xsd_schema::navigator::{DomNavigator, DomNodeType, TypedValue};
use xsd_schema::types::value::{XmlValue, XmlValueKind};
use xsd_schema::validation::{
    drive_quick_xml, drive_quick_xml_with, CollectingValidationSink, NoopHandler, SchemaValidator,
    ValidationError, ValidationFlags,
};
use xsd_schema::{SchemaSet, SchemaSetBuilder, TypeKey, XsdVersion};

/// One fixed / default value of the matrix.
pub struct Case {
    #[allow(dead_code)]
    pub label: &'static str,
    /// The type of the declarations.
    pub ty: &'static str,
    /// The value constraint's literal.
    pub literal: &'static str,
    /// Another spelling of the same value, when the type has one.
    pub alt: Option<&'static str>,
    /// A valid literal of the type with another value (or, for a type with
    /// one value only, an invalid one).
    pub wrong: &'static str,
    /// Namespace declarations on the declarations that carry the literal.
    pub decl_ns: &'static str,
    /// Whether attributes can have the type (a simple type).
    pub attributes: bool,
    /// The type an `xsi:type` names instead: `Some("")` for a restriction of
    /// `ty` generated for the case, `Some(name)` for that type, `None` for
    /// no override.
    pub xsi: Option<&'static str>,
}

const fn case(
    label: &'static str,
    ty: &'static str,
    literal: &'static str,
    alt: Option<&'static str>,
    wrong: &'static str,
) -> Case {
    Case {
        label,
        ty,
        literal,
        alt,
        wrong,
        decl_ns: "",
        attributes: true,
        xsi: Some(""),
    }
}

/// The 22 literals of the review's `fixedraw` probe, the union of a QName
/// enumeration and `xs:string` of its `fixedunion` probe, and QName, NOTATION,
/// list, union and simple-content types around them.
pub const CASES: &[Case] = &[
    case("float NaN", "xs:float", "NaN", None, "1"),
    case("double NaN", "xs:double", "NaN", None, "1"),
    case("double INF", "xs:double", "INF", Some("+INF"), "-INF"),
    case("double -0", "xs:double", "-0", Some("0"), "1"),
    case("string spaces", "xs:string", "  a  b  ", None, "a b"),
    case("token spaces", "xs:token", "  a  b  ", Some("a b"), "a c"),
    case(
        "date tz",
        "xs:date",
        "2000-01-01+14:00",
        Some("1999-12-31-10:00"),
        "2000-01-02",
    ),
    case(
        "dateTime 24h",
        "xs:dateTime",
        "2000-01-01T24:00:00",
        Some("2000-01-02T00:00:00"),
        "2000-01-01T00:00:00",
    ),
    case(
        "time frac",
        "xs:time",
        "12:00:00.123456789012345678901234567890",
        None,
        "12:00:00",
    ),
    case(
        "duration",
        "xs:duration",
        "P1Y2M3DT4H5M6.7S",
        Some("P14M3DT4H5M6.7S"),
        "P1Y",
    ),
    case("gMonthDay", "xs:gMonthDay", "--02-29", None, "--02-28"),
    case(
        "decimal big",
        "xs:decimal",
        "123456789012345678901234567890123456789012345678901234567890.5",
        Some("123456789012345678901234567890123456789012345678901234567890.50"),
        "1",
    ),
    case(
        "integer big",
        "xs:integer",
        "123456789012345678901234567890123456789012345678901234567890",
        Some("+123456789012345678901234567890123456789012345678901234567890"),
        "1",
    ),
    case("hexBinary", "xs:hexBinary", "0aFf", Some("0AFF"), "00"),
    case("base64", "xs:base64Binary", "YWJj", None, "YWJk"),
    case(
        "anyURI",
        "xs:anyURI",
        "http://example.com/a b",
        None,
        "http://example.com/",
    ),
    case("boolean", "xs:boolean", "1", Some("true"), "0"),
    case("NMTOKENS", "xs:NMTOKENS", "a  b   c", Some("a b c"), "a b"),
    case(
        "QName",
        "xs:QName",
        "xs:string",
        Some("x2:string"),
        "xs:int",
    ),
    case("union", "U", " 1 ", Some("+1"), "2"),
    case("list of union", "LU", "1 a 2", Some(" 01  a  2 "), "1 a"),
    Case {
        xsi: Some("xs:string"),
        ..case("anySimpleType", "xs:anySimpleType", " x ", None, "x")
    },
    // The review's `fixedunion`: the literal `q:k` selects the string member
    // where it is written (`q` is `urn:b` there, the enumeration's is `urn:a`).
    Case {
        decl_ns: r#" xmlns:q="urn:b""#,
        ..case("union of QName enumeration", "UQ", "q:k", None, "q:j")
    },
    Case {
        decl_ns: r#" xmlns:q="urn:b""#,
        ..case(
            "list of that union",
            "LUQ",
            "q:k x q:k",
            Some(" q:k  x  q:k "),
            "q:k x",
        )
    },
    // The same with white space around the literal, which the string member
    // keeps and the QName member collapses.
    Case {
        decl_ns: r#" xmlns:q="urn:b""#,
        ..case(
            "union of QName enumeration, spaces",
            "UQ",
            " q:k ",
            None,
            "q:k",
        )
    },
    case(
        "union, QName member, spaces",
        "UQ",
        " q:k ",
        Some("q:k"),
        " q:j ",
    ),
    // The same union with the literal selecting the QName member.
    case("union, QName member", "UQ", "q:k", None, "q:j"),
    case("QName enumeration", "QE", "q:k", None, "q:j"),
    case(
        "list of QName",
        "LQ",
        "xs:string xs:int",
        Some("x2:string  x2:int"),
        "xs:string",
    ),
    case("NOTATION", "N", "gif", None, "png"),
    case(
        "enumerated token",
        "Color",
        "dark red",
        Some(" dark   red "),
        "red",
    ),
    case(
        "union of date and token",
        "UT",
        " 2000-01-01 ",
        Some("2000-01-01"),
        "x",
    ),
    case(
        "union with a list member",
        "UL",
        "1 2",
        Some(" 01  2 "),
        "1 x",
    ),
    case("int", "xs:int", "007", Some("7"), "8"),
    case("gYear", "xs:gYear", "2000Z", Some("2000+00:00"), "2001Z"),
    Case {
        attributes: false,
        xsi: None,
        ..case("simple content", "CS", "1.0", Some("1.00"), "2")
    },
    Case {
        attributes: false,
        xsi: None,
        ..case(
            "simple content restriction of a union",
            "CR",
            "1",
            Some("+1"),
            "a",
        )
    },
];

/// What a schema set of the matrix has besides its declarations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Extra {
    /// No XPath expression at all.
    None,
    /// An unrelated `<xs:assert test="true()"/>` on the document's type.
    Assert,
    /// An `xs:assertion` facet on a type the documents do not use.
    UnusedFacet,
    /// Both, and an unused type whose `xs:assertion` facet reads the current
    /// dateTime (and holds at every instant).
    UnusedClockFacet,
    /// An assertion on the document's type that always fails, with a message
    /// that tells what its data model holds: the string value of every leaf
    /// element and attribute, and what `fn:id` and `fn:idref` find. Not one
    /// of [`EXTRAS`] (it adds an error); it shows the tree the run builds for
    /// its assertions.
    Reveal,
}

/// The test of [`Extra::Reveal`].
const REVEAL: &str = "error((), concat('ids: ', string-join(for $i in ('k1', 'k2', 'k3', 'kx', 'k9', 'kf', 'kg', 'kh', '7', 'bad') return concat($i, '=', string(count(id($i))), '/', string(count(idref($i)))), ' '), ' values: ', string-join(for $n in (//*[not(*)] | //@*) return concat(name($n), '=[', string($n), ']'), '|')))";

pub const EXTRAS: [Extra; 4] = [
    Extra::None,
    Extra::Assert,
    Extra::UnusedFacet,
    Extra::UnusedClockFacet,
];

/// The schema of the matrix: per case `c{i}` with a fixed element `e`, a
/// defaulted element `d`, a nillable defaulted element `n`, and — for a
/// simple type — a local attribute `a` with a fixed value (the attribute
/// use's), a reference to the global `g{i}` whose declaration fixes it, a
/// reference to the global `h{i}` fixed by the use, a defaulted attribute
/// `da`, and a `##local` strict wildcard that matches the global fixed
/// attribute `w{i}`; and `ids`, elements and attributes of ID and IDREF types.
pub fn schema(extra: Extra) -> String {
    let mut globals = String::new();
    let mut children = String::new();
    for (i, c) in CASES.iter().enumerate() {
        let (ty, lit, ns) = (c.ty, c.literal, c.decl_ns);
        if c.xsi == Some("") {
            globals.push_str(&format!(
                r#"<xs:simpleType name="R{i}"><xs:restriction base="{ty}"/></xs:simpleType>"#
            ));
        }
        let mut attributes = String::new();
        if c.attributes {
            globals.push_str(&format!(
                r#"<xs:attribute name="g{i}" type="{ty}" fixed="{lit}"{ns}/>
<xs:attribute name="h{i}" type="{ty}"/>
<xs:attribute name="w{i}" type="{ty}" fixed="{lit}"{ns}/>"#
            ));
            attributes = format!(
                r###"<xs:attribute name="a" type="{ty}" fixed="{lit}"{ns}/>
<xs:attribute ref="g{i}"/>
<xs:attribute ref="h{i}" fixed="{lit}"{ns}/>
<xs:attribute name="da" type="{ty}" default="{lit}"{ns}/>
<xs:anyAttribute namespace="##local" processContents="strict"/>"###
            );
        }
        children.push_str(&format!(
            r#"<xs:element name="c{i}" minOccurs="0"><xs:complexType><xs:sequence>
<xs:element name="e" type="{ty}" fixed="{lit}"{ns} minOccurs="0" maxOccurs="unbounded"/>
<xs:element name="d" type="{ty}" default="{lit}"{ns} minOccurs="0" maxOccurs="unbounded"/>
<xs:element name="n" type="{ty}" default="{lit}"{ns} nillable="true" minOccurs="0"/>
</xs:sequence>{attributes}</xs:complexType></xs:element>
"#
        ));
    }
    let assert = match extra {
        Extra::Assert | Extra::UnusedClockFacet => r#"<xs:assert test="true()"/>"#.to_string(),
        Extra::Reveal => format!(r#"<xs:assert test="{REVEAL}"/>"#),
        _ => String::new(),
    };
    let unused = match extra {
        Extra::None | Extra::Assert | Extra::Reveal => "",
        Extra::UnusedFacet => {
            r#"<xs:simpleType name="Even"><xs:restriction base="xs:int"><xs:assertion test="$value mod 2 = 0"/></xs:restriction></xs:simpleType>
<xs:element name="unused" type="Even"/>"#
        }
        Extra::UnusedClockFacet => {
            r#"<xs:simpleType name="Even"><xs:restriction base="xs:int"><xs:assertion test="$value mod 2 = 0"/></xs:restriction></xs:simpleType>
<xs:simpleType name="Timed"><xs:restriction base="xs:string"><xs:assertion test="current-dateTime() eq current-dateTime()"/></xs:restriction></xs:simpleType>
<xs:element name="unused" type="Even"/><xs:element name="timed" type="Timed" fixed="t"/>"#
        }
    };
    format!(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:q="urn:a">
<xs:notation name="gif" public="image/gif"/><xs:notation name="png" public="image/png"/>
<xs:simpleType name="U"><xs:union memberTypes="xs:int xs:string"/></xs:simpleType>
<xs:simpleType name="LU"><xs:list itemType="U"/></xs:simpleType>
<xs:simpleType name="QE"><xs:restriction base="xs:QName"><xs:enumeration value="q:k"/><xs:enumeration value="q:j"/></xs:restriction></xs:simpleType>
<xs:simpleType name="UQ"><xs:union memberTypes="QE xs:string"/></xs:simpleType>
<xs:simpleType name="LUQ"><xs:list itemType="UQ"/></xs:simpleType>
<xs:simpleType name="LQ"><xs:list itemType="xs:QName"/></xs:simpleType>
<xs:simpleType name="N"><xs:restriction base="xs:NOTATION"><xs:enumeration value="gif"/><xs:enumeration value="png"/></xs:restriction></xs:simpleType>
<xs:simpleType name="Color"><xs:restriction base="xs:token"><xs:enumeration value="red"/><xs:enumeration value="dark red"/></xs:restriction></xs:simpleType>
<xs:simpleType name="UT"><xs:union memberTypes="xs:date xs:token"/></xs:simpleType>
<xs:simpleType name="IntList"><xs:list itemType="xs:int"/></xs:simpleType>
<xs:simpleType name="UL"><xs:union memberTypes="IntList xs:string"/></xs:simpleType>
<xs:complexType name="CS"><xs:simpleContent><xs:extension base="xs:decimal"><xs:attribute name="unit" type="xs:string"/></xs:extension></xs:simpleContent></xs:complexType>
<xs:complexType name="CU"><xs:simpleContent><xs:extension base="U"/></xs:simpleContent></xs:complexType>
<xs:complexType name="CR"><xs:simpleContent><xs:restriction base="CU"><xs:enumeration value="1"/><xs:enumeration value="a"/></xs:restriction></xs:simpleContent></xs:complexType>
<xs:simpleType name="Code"><xs:restriction base="xs:ID"><xs:pattern value="k.*"/></xs:restriction></xs:simpleType>
<xs:simpleType name="UI"><xs:union memberTypes="xs:ID xs:int"/></xs:simpleType>
{globals}
{unused}
<xs:element name="doc"><xs:complexType><xs:sequence>
{children}
<xs:element name="ids" minOccurs="0"><xs:complexType><xs:sequence>
  <xs:element name="i" type="Code" maxOccurs="unbounded"/>
  <xs:element name="ui" type="UI" maxOccurs="unbounded"/>
  <xs:element name="r" type="xs:IDREF" minOccurs="0"/>
  <xs:element name="rs" type="xs:IDREFS" minOccurs="0"/>
  <xs:element name="t" type="xs:token" minOccurs="0"/>
  <xs:element name="s" type="xs:string" default=" s " minOccurs="0"/>
  <xs:element name="fi" type="xs:ID" fixed="kf" minOccurs="0"/>
</xs:sequence>
<xs:attribute name="x" type="Code"/><xs:attribute name="refs" type="xs:IDREFS"/>
<xs:attribute name="dr" type="xs:IDREF" default="k1"/>
<xs:attribute name="fx" type="UI" fixed="7"/>
</xs:complexType></xs:element>
</xs:sequence>{assert}</xs:complexType></xs:element>
</xs:schema>"#
    )
}

pub fn compile(extra: Extra) -> SchemaSet {
    SchemaSetBuilder::with_version(XsdVersion::V1_1)
        .add_source(&schema(extra), "file:///ordinary_schema_paths.xsd")
        .expect("schema source accepted")
        .compile()
        .unwrap_or_else(|e| panic!("the schema of the matrix compiles ({extra:?}): {e}"))
        .into_schema_set()
}

/// The shapes of the documents of the matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// Every value written as the literal.
    Literal,
    /// Every value in its other spelling.
    Alternate,
    /// Every value another value.
    Wrong,
    /// Empty elements, a nilled one, no attribute: every value constraint is
    /// taken.
    Empty,
    /// `xsi:type` on the fixed and defaulted elements: the literal, its
    /// other spelling, and empty elements.
    XsiType,
    /// `xsi:type` on fixed elements with another value.
    XsiTypeWrong,
}

pub const SHAPES: [Shape; 6] = [
    Shape::Literal,
    Shape::Alternate,
    Shape::Wrong,
    Shape::Empty,
    Shape::XsiType,
    Shape::XsiTypeWrong,
];

/// The namespaces the documents bind `q` to: the enumeration's, the fixed
/// declarations', and neither.
pub const Q_NAMESPACES: [&str; 3] = ["urn:a", "urn:b", "urn:c"];

/// The document of `shape` with `q` bound to `q_ns`.
pub fn document(shape: Shape, q_ns: &str) -> String {
    let mut body = String::new();
    for (i, c) in CASES.iter().enumerate() {
        let lit = c.literal;
        let alt = c.alt.unwrap_or(lit);
        let xsi = match c.xsi {
            Some("") => Some(format!("R{i}")),
            Some(name) => Some(name.to_string()),
            None => None,
        };
        let attributes = |value: &str| {
            if c.attributes {
                format!(r#" a="{value}" g{i}="{value}" h{i}="{value}" w{i}="{value}" da="{value}""#)
            } else {
                String::new()
            }
        };
        let content = |value: &str| format!("<e>{value}</e><d>{value}</d><n>{value}</n>");
        body.push_str(&match (shape, xsi) {
            (Shape::Literal, _) => format!("<c{i}{}>{}</c{i}>", attributes(lit), content(lit)),
            (Shape::Alternate, _) => format!("<c{i}{}>{}</c{i}>", attributes(alt), content(alt)),
            (Shape::Wrong, _) => {
                format!("<c{i}{}>{}</c{i}>", attributes(c.wrong), content(c.wrong))
            }
            (Shape::Empty, _) => format!(r#"<c{i}><e/><d/><n xsi:nil="true"/></c{i}>"#),
            (Shape::XsiType, Some(t)) => format!(
                r#"<c{i}><e xsi:type="{t}">{lit}</e><e xsi:type="{t}">{alt}</e><e xsi:type="{t}"/><d xsi:type="{t}">{alt}</d><d xsi:type="{t}"/></c{i}>"#
            ),
            (Shape::XsiTypeWrong, Some(t)) => format!(
                r#"<c{i}><e xsi:type="{t}">{}</e></c{i}>"#,
                c.wrong
            ),
            (Shape::XsiType | Shape::XsiTypeWrong, None) => String::new(),
        });
    }
    let ids = match shape {
        Shape::Wrong => {
            r#"<ids x="k1" refs="k1 nowhere" fx="kg"><i>bad</i><i>k1</i><ui>k1</ui><r>k9</r><t>  a  b  </t><fi>kh</fi></ids>"#
        }
        Shape::Empty => r#"<ids><i>k1</i><ui>7</ui><s/></ids>"#,
        _ => {
            r#"<ids x="kx" refs="k1  kx" fx=" 7 "><i> k1 </i><i>k2</i><ui>k3</ui><ui> 7 </ui><r>k2</r><rs>k1  k3</rs><t>  a  b  </t><s/><fi> kf </fi></ids>"#
        }
    };
    format!(
        r#"<doc xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:x2="http://www.w3.org/2001/XMLSchema" xmlns:q="{q_ns}">{body}{ids}</doc>"#
    )
}

/// How a document is validated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Driver {
    /// `drive_quick_xml`: no handler.
    Stream,
    /// `build_typed_document_with`.
    TypedBuild { defaults_as_nodes: bool },
    /// A `TypedBuilderHandler` the caller drives with `drive_quick_xml_with`.
    DirectHandler { defaults_as_nodes: bool },
}

pub const DRIVERS: [Driver; 5] = [
    Driver::Stream,
    Driver::TypedBuild {
        defaults_as_nodes: true,
    },
    Driver::DirectHandler {
        defaults_as_nodes: true,
    },
    Driver::TypedBuild {
        defaults_as_nodes: false,
    },
    Driver::DirectHandler {
        defaults_as_nodes: false,
    },
];

/// `PROCESS_ASSERTIONS`, with and without `BUILD_PSVI_TYPED_VALUES`.
pub fn flag_sets() -> [(&'static str, ValidationFlags); 2] {
    let base = ValidationFlags::default() | ValidationFlags::PROCESS_ASSERTIONS;
    [
        (
            "typed values kept",
            base | ValidationFlags::BUILD_PSVI_TYPED_VALUES,
        ),
        (
            "typed values not kept",
            base & !ValidationFlags::BUILD_PSVI_TYPED_VALUES,
        ),
    ]
}

/// What one validation of a document shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    /// The root's `[validity]` and every error, in order: its constraint,
    /// its instance path and its message.
    pub verdict: String,
    /// The typed document, node by node; `None` for [`Driver::Stream`].
    pub document: Option<String>,
}

pub fn observe(ss: &SchemaSet, xml: &str, flags: ValidationFlags, driver: Driver) -> Observation {
    let validator = SchemaValidator::new(ss, flags);
    let (mut errors, mut warnings) = (Vec::new(), Vec::new());
    let arena = Bump::new();
    let (validity, document) = {
        let mut runtime = validator.start_run(CollectingValidationSink {
            errors: &mut errors,
            warnings: &mut warnings,
        });
        match driver {
            Driver::Stream => {
                let outcome = drive_quick_xml(xml.as_bytes(), &mut runtime, ss).expect("drive");
                (outcome.root_validity, None)
            }
            Driver::TypedBuild { defaults_as_nodes } => {
                let built = build_typed_document_with(
                    xml.as_bytes(),
                    &arena,
                    ss,
                    &mut runtime,
                    &mut NoopHandler,
                    TypedBuildOptions::default()
                        .with_document_options(BufferDocumentOptions::full())
                        .with_defaults_as_nodes(defaults_as_nodes),
                )
                .expect("typed build");
                // The handler-driven layer leaves the end of the run (the
                // IDREF check) to its caller.
                let _ = runtime.end_validation();
                (built.outcome.root_validity, Some(dump(&built.document, ss)))
            }
            Driver::DirectHandler { defaults_as_nodes } => {
                let mut builder = BufferDocumentBuilder::new(
                    &arena,
                    &ss.name_table,
                    Some(ss),
                    BufferDocumentOptions::full(),
                )
                .expect("builder");
                let outcome = {
                    let mut handler = TypedBuilderHandler::new(&mut builder)
                        .with_defaults_as_nodes(defaults_as_nodes);
                    drive_quick_xml_with(xml.as_bytes(), &mut runtime, ss, &mut handler)
                        .unwrap_or_else(|_| panic!("drive with a handler"))
                };
                let _ = runtime.end_validation();
                let document = builder.finalize().expect("finalize");
                (outcome.root_validity, Some(dump(&document, ss)))
            }
        }
    };
    let mut verdict = format!("root validity: {validity:?}\n");
    for e in &errors {
        verdict.push_str(&error_line(e));
    }
    Observation { verdict, document }
}

fn error_line(e: &ValidationError) -> String {
    let path = e.instance_path().map(|p| p.to_string()).unwrap_or_default();
    format!("{} at {}: {}\n", e.constraint, path, e.message)
}

/// The typed document, one line per node: elements with their type, nilled
/// and is-idrefs properties and typed value, attributes likewise with their
/// string value, texts; then which of a few ids the document's index holds.
pub fn dump(doc: &BufferDocument<'_>, ss: &SchemaSet) -> String {
    let mut out = String::new();
    let mut nav = doc.create_navigator();
    nav.move_to_root();
    dump_children(&mut nav, ss, 0, &mut out);
    for id in ["k1", "k2", "k3", "kx", "k9", "kf", "kg", "kh", "7", "bad"] {
        out.push_str(&format!(
            "id({id}): {}\n",
            doc.get_element_by_id(id).is_some()
        ));
    }
    out
}

fn dump_children<N: DomNavigator>(nav: &mut N, ss: &SchemaSet, depth: usize, out: &mut String) {
    if !nav.move_to_first_child() {
        return;
    }
    loop {
        let pad = "  ".repeat(depth);
        match nav.node_type() {
            DomNodeType::Element => {
                out.push_str(&format!("{pad}<{}> {}\n", nav.name(), properties(nav, ss)));
                if nav.move_to_first_attribute() {
                    loop {
                        out.push_str(&format!(
                            "{pad}  @{}=[{}] {}\n",
                            nav.name(),
                            nav.value(),
                            properties(nav, ss)
                        ));
                        if !nav.move_to_next_attribute() {
                            break;
                        }
                    }
                    nav.move_to_parent();
                }
                dump_children(nav, ss, depth + 1, out);
            }
            DomNodeType::Text => out.push_str(&format!("{pad}text [{}]\n", nav.value())),
            other => out.push_str(&format!("{pad}{other:?} [{}]\n", nav.value())),
        }
        if !nav.move_to_next_sibling() {
            break;
        }
    }
    nav.move_to_parent();
}

fn properties<N: DomNavigator>(nav: &N, ss: &SchemaSet) -> String {
    let ty = match nav.type_annotation() {
        Some(TypeKey::Simple(sk)) => ss
            .arenas
            .simple_types
            .get(sk)
            .and_then(|t| t.name)
            .map_or("(anonymous simple type)".to_string(), |n| {
                ss.name_table.resolve(n)
            }),
        Some(TypeKey::Complex(ck)) => ss
            .arenas
            .complex_types
            .get(ck)
            .and_then(|t| t.name)
            .map_or("(anonymous complex type)".to_string(), |n| {
                ss.name_table.resolve(n)
            }),
        None => "(untyped)".to_string(),
    };
    let typed = match nav.typed_value() {
        TypedValue::Value(value) => show(&value, ss),
        TypedValue::Untyped => "untyped".to_string(),
        TypedValue::Nilled => "nilled".to_string(),
        TypedValue::Absent => "absent".to_string(),
    };
    let idrefs = if nav.is_idrefs() { " is-idrefs" } else { "" };
    format!("type={ty}{idrefs} typed={typed}")
}

/// A typed value without the schema set's keys: per atom its type code and
/// string value, a QName's namespace name.
fn show(value: &XmlValue, ss: &SchemaSet) -> String {
    match &value.value {
        XmlValueKind::Union(member) => format!("union({})", show(member, ss)),
        XmlValueKind::List { items, .. } => {
            let items: Vec<String> = items.iter().map(|item| show(item, ss)).collect();
            format!("list[{}]", items.join(", "))
        }
        _ => {
            let namespace = value.as_qname().map(|name| {
                let ns = name
                    .namespace_uri
                    .map(|ns| ss.name_table.resolve(ns))
                    .unwrap_or_default();
                format!(" in {{{ns}}}")
            });
            format!(
                "{:?}'{}'{}",
                value.type_code,
                value.to_string_value(),
                namespace.unwrap_or_default()
            )
        }
    }
}
