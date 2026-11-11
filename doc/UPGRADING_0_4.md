# Upgrading from 0.3.x to 0.4

0.4.0 raises the minimum supported Rust version, moves to quick-xml 0.42,
lalrpop 0.23 and regexml-fork 0.4, and takes quick-xml's types out of the
public API. Code that names none of the items in section 2 and builds with
Rust 1.88 needs no change. This guide has, in that order:

1. [The minimum Rust version](#1-minimum-rust-version);
2. [Code changes](#2-code-changes): the public items that carried quick-xml
   types, each with the 0.3.x code and the 0.4 code;
3. [Behaviour changes](#3-behaviour-changes): what the new dependencies change
   in matching and what they do not.

The [CHANGELOG](../CHANGELOG.md) lists every change. In the examples below, a
block marked as 0.3.x code is not compiled (it no longer builds); every other
block is compiled and run with the crate.

## 1. Minimum Rust version

0.4 needs Rust 1.88 or later (0.3.x: 1.85). quick-xml 0.42 and lalrpop 0.23
need 1.86, and the ICU4X 2.4 crates behind regexml-fork 0.4 (the `xsd11`
feature) need 1.88. 0.3.1 stays the release for Rust 1.85 to 1.87: a project on
those toolchains keeps `xsd-schema = "0.3"`.

## 2. Code changes

No public item names a quick-xml type any more, so a later quick-xml release
does not change this crate's API. In 0.3 five did; all five are gone or no
longer public:

| 0.3.x | 0.4 |
| --- | --- |
| `parser::TrackedEvent::event` (a public `quick_xml::events::Event`) | not public; the event's kind (`is_start`, `is_empty`, `is_end`, `is_text`, `is_eof`) and `span` stay |
| `parser::TrackedEvent::new` | not public (only the reader creates events) |
| `parser::TrackedReader::inner()` (a `&quick_xml::Reader`) | removed; the reader's state is `buffer_position`, `last_position`, `error_position`, `xml_version` |
| `impl From<quick_xml::Error> for SchemaError` | removed; build the error with `SchemaError::xml` |
| `parser::attrs::parse_attributes` (also `parser::parse_attributes`), which took quick-xml attributes | removed |

The validation drivers (`drive_quick_xml` and its variants), `BufferDocument`
and the schema loaders take a `BufRead` or a byte slice, as before.

### 2.1 `TrackedReader` and `TrackedEvent`

`TrackedReader` reads a schema document through the crate's well-formedness
layer and gives each event's kind and byte span; what an event contains is the
schema-document parser's business. Code that matched on the quick-xml event
asks for its kind instead, and reads the content with quick-xml itself, at the
span, if it needs it.

```rust,ignore
// 0.3.x
let ev = reader.read_event(&mut buf)?;
if let quick_xml::events::Event::Start(e) = &ev.event {
    starts.push((ev.span.start, e.name().as_ref().to_vec()));
}
```

```rust
use xsd_schema::parser::TrackedReader;

let xml = br#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"><xs:element name="a"/></xs:schema>"#;
let mut reader = TrackedReader::from_bytes(xml);
let mut buf = Vec::new();
let mut starts = Vec::new();
loop {
    let ev = reader.read_event(&mut buf).expect("well-formed");
    if ev.is_eof() {
        break;
    }
    if ev.is_start() || ev.is_empty() {
        starts.push(ev.span.start);
    }
}
assert_eq!(starts, [0, 55]);
assert_eq!(&xml[55..66], b"<xs:element");
```

### 2.2 `SchemaError` from a quick-xml error

`SchemaError` no longer converts from `quick_xml::Error`, so `?` on a quick-xml
result in a function that returns `SchemaResult` does not compile. The
conversion made an `XmlError` with the error's message and no location; write
that.

```rust,ignore
// 0.3.x
fn next(reader: &mut quick_xml::Reader<&[u8]>, buf: &mut Vec<u8>) -> xsd_schema::error::SchemaResult<()> {
    reader.read_event_into(buf)?;
    Ok(())
}
```

```rust
use xsd_schema::SchemaError;

/// What `From<quick_xml::Error>` gave in 0.3.
fn xml_error(e: impl std::fmt::Display) -> SchemaError {
    SchemaError::xml(e.to_string(), None)
}

let e = xml_error("ill-formed document: closing tag `b` does not match `a`");
assert!(matches!(e, SchemaError::XmlError { location: None, .. }));
```

### 2.3 `parse_attributes`

`parse_attributes` was the schema-document parser's step from quick-xml's
attributes to `ParsedAttribute`s; the parser keeps it internally. Code that
called it reads the attributes with quick-xml (0.42: `Attribute::normalized_value`
normalizes the value) and builds `ParsedAttribute` itself — its fields are
public — then uses `categorize_attributes` and `AttributeMap` as before.

## 3. Behaviour changes

Validation verdicts do not change: the W3C XSD 1.0 and 1.1 suites, the XPath 2.0
tests of XQTS and the GAEB and WSDL corpora give the same results as with 0.3.1,
failures included by name.

- **Input that is not UTF-8** is refused by quick-xml 0.42 while it reads; the
  readers report it as in 0.3, with the same well-formedness rule, message and
  byte position.
- **XSD 1.1 patterns and XPath regular expressions (`xsd11`)** use the
  character data of Unicode 18.0 instead of 15.1 (regexml-fork 0.4 with ICU4X
  2.4): `\p{…}`, `\P{…}`, `\d`, `\w` and their complements, and case-blind
  matching, see the characters assigned since 15.1 — `\p{L}` holds 158,172
  code points instead of 136,726, `\p{Nd}` 770 instead of 680. Of the
  characters assigned in 15.1 two change category: U+0295 from `Ll` to `Lo`
  and U+1171E from `Mn` to `Mc`. `\s`, `\i`, `\c`, `.` and the block escapes
  (`\p{IsBasicLatin}`) do not change. XSD 1.0 patterns are pinned to Unicode
  3.0 by this crate and do not change either.
- **Case-blind matching** (the `i` flag of `fn:matches`, `fn:replace` and
  `fn:tokenize`) no longer rejects matches after a repetition followed by a
  literal: `fn:matches("file.XML", ".*\.xml", "i")` is true (it was false), so
  are `.*1` on "a1" and `\S+@\S+` on "a@b".
- A pattern whose repetition counts are near the largest `usize`
  (`(ab){1,9223372036854775807}`) no longer gives wrong answers or panics: the
  lengths computed from the counts saturate instead of wrapping around.

```rust
# #[cfg(feature = "xsd11")] {
use xsd_schema::namespace::table::NameTable;
use xsd_schema::xpath::{RoXmlNavigator, XPathContext, XPathExpr};

let names = NameTable::new();
let ctx = XPathContext::new(&names);
let run = |xpath: &str| -> Result<String, Box<dyn std::error::Error>> {
    let expr = XPathExpr::compile(xpath, &ctx)?;
    Ok(expr.evaluator(&ctx).run_string::<RoXmlNavigator<'static>>()?)
};
// false in 0.3: the repetition took the ".XML" the literal needed
assert_eq!(run(r#"string(matches("file.XML", ".*\.xml", "i"))"#)?, "true");
// U+1C89 CYRILLIC CAPITAL LETTER TJE, assigned after Unicode 15.1: false in 0.3
assert_eq!(run("string(matches(\"\u{1C89}\", '^\\p{Lu}$'))")?, "true");
# }
# Ok::<(), Box<dyn std::error::Error>>(())
```
