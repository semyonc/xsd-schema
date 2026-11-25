# Upgrading from 0.3.x to 0.4

0.4.0 raises the minimum supported Rust version, moves to quick-xml 0.42,
lalrpop 0.23 and regexml-fork 0.4, takes quick-xml's types out of the
public API, and drops `chrono`: the XPath engine's implicit timezone is UTC
when none is set. Code that names none of the items in section 2 and builds
with Rust 1.88 needs no change. This guide has, in that order:

1. [The minimum Rust version](#1-minimum-rust-version);
2. [Code changes](#2-code-changes): the public items that carried quick-xml
   types, each with the 0.3.x code and the 0.4 code, and one renamed method;
3. [Behaviour changes](#3-behaviour-changes): what the new dependencies change
   in matching and what they do not, and the implicit timezone.

The [CHANGELOG](../CHANGELOG.md) lists every change. In the examples below, a
block marked as 0.3.x code is not compiled (it no longer builds); every other
block is compiled and run with the crate.

## 1. Minimum Rust version

0.4 needs Rust 1.88 or later (0.3.x: 1.85). quick-xml 0.42 and lalrpop 0.23
need 1.86, and the ICU4X 2.4 crates behind regexml-fork 0.4 (the `xsd11`
feature) need 1.88. 0.3.2 stays the release for Rust 1.85 to 1.87: a project on
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

### 2.4 `DynamicContext::implicit_timezone_or_host`

Renamed `effective_implicit_timezone` (the default it falls back to is no
longer the host's offset but UTC, section 3). The old name still compiles, with
a deprecation warning, in 0.4.

```rust
# #[cfg(feature = "xsd11")] {
use xsd_schema::namespace::table::NameTable;
use xsd_schema::types::value::TimezoneOffset;
use xsd_schema::xpath::{DynamicContext, RoXmlNavigator, XPathContext};

let names = NameTable::new();
let ctx = XPathContext::new(&names);
let dynamic = DynamicContext::<RoXmlNavigator<'static>>::new(&ctx, 0);
// 0.3: dynamic.implicit_timezone_or_host()
assert_eq!(dynamic.effective_implicit_timezone(), TimezoneOffset::UTC);
# }
```

## 3. Behaviour changes

Validation verdicts do not change on the test suites: the W3C XSD 1.0 and 1.1
suites, the XPath 2.0 tests of XQTS and the GAEB and WSDL corpora give the same
results as with 0.3.2, failures included by name.

- **The implicit timezone (`xsd11`)** of an XPath evaluation that sets none is
  UTC, no longer the host's local offset (the process's `TZ`). A date/time
  value without a timezone is read in it when it is compared with or
  subtracted from one that has a timezone; it is `fn:implicit-timezone()` and
  the zone of `fn:current-dateTime()`; the functions without a dynamic context
  (`operators::value_eq`, `eval_binary`, …) use it as well. Schema validation
  sets none, so an XSD 1.1 assertion or type alternative such as
  `@date lt xs:date('2026-01-01Z')` on a date without a timezone gives the same
  verdict on every machine — in 0.3 it followed the machine's `TZ`. A host that
  wants its local time computes the offset with the time-zone library it uses
  and sets it:

  ```rust
  # #[cfg(feature = "xsd11")] {
  use xsd_schema::namespace::table::NameTable;
  use xsd_schema::types::value::TimezoneOffset;
  use xsd_schema::xpath::{RoXmlNavigator, XPathContext, XPathExpr};

  // The host's offset now, in minutes east of UTC, from its own time-zone
  // library: `jiff::Zoned::now().offset().seconds() / 60` (an `i32`),
  // `time::UtcOffset::current_local_offset()?.whole_minutes()` (an `i16`,
  // `time`'s `local-offset` feature), … Here a fixed +01:00.
  let minutes: i16 = 60;
  let names = NameTable::new();
  let local = XPathContext::new(&names).with_implicit_timezone(TimezoneOffset(minutes));
  let utc = XPathContext::new(&names);
  let xpath = "string(implicit-timezone())";
  let run = |ctx: &XPathContext| -> Result<String, Box<dyn std::error::Error>> {
      let expr = XPathExpr::compile(xpath, ctx)?;
      Ok(expr.evaluator(ctx).run_string::<RoXmlNavigator<'static>>()?)
  };
  assert_eq!(run(&local)?, "PT1H");
  assert_eq!(run(&utc)?, "PT0S"); // in 0.3: the offset of the machine's `TZ`
  # }
  # Ok::<(), Box<dyn std::error::Error>>(())
  ```

  `DynamicContext::with_implicit_timezone` does the same for a dynamic context
  the host builds itself.
- **The current dateTime** comes from `std::time::SystemTime`, read once per
  dynamic context as before. On `wasm32-unknown-unknown`, whose standard
  library has no clock, `fn:current-dateTime` without
  `DynamicContext::current_datetime` raises `XPDY0002` (0.3 read the
  browser's clock through `chrono`).
- **One current dateTime per validation run (XSD 1.1).** Every assertion,
  assertion facet and type alternative of a run reports one instant — read
  from the system clock the first time one of them asks, or given by the host
  with the new `ValidationRuntime::set_current_time` (also the way to give
  validation a clock on `wasm32-unknown-unknown`). In 0.3 each evaluation
  read the clock, so a schema whose assertions call `fn:current-dateTime`
  could get a verdict that depended on how fast the document arrived. A
  schema set's own literals (enumeration values, `default` and `fixed`
  values) are schema properties, read at one instant of the set's own, the
  same in every run. Nothing changes for a schema without an `xs:assertion`
  facet that calls `fn:current-dateTime`, `fn:current-date` or
  `fn:current-time`.
- **A handler that wraps a `TypedBuilderHandler`** in a
  `ValidationEventHandler` of its own forwards the two new methods of the
  trait to it, `wants_typed_values` and `after_end_element_values`, as it
  forwards the others; `after_end_element` is then no longer called for it.
  Without them the tree is built as in 0.3 (each element's content validated
  a second time at its end). A handler passed to `TypedBuilderHandler` as
  its inner handler, or to `build_typed_document_with`, needs no change.

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
