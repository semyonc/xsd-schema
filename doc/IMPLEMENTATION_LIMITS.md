# Implementation Limits

Where the specifications leave a value space unbounded and this crate stores
values in a bounded type, and what happens at the bound. Datatypes 1.1 §5.4
allows such limits on two conditions: a value beyond them "must not be treated
as invalid", and it "must not be quietly changed to a supported value".

## 1. `xs:decimal`

### Validation is exact

The value space of `xs:decimal` is unbounded, and every value is validated
exactly: `totalDigits`, `fractionDigits`, the bound facets, `enumeration`,
fixed/default values and identity-constraint fields are decided on the exact
value, however large or precise. The same holds for the integer types derived
from `xs:decimal`.

### The typed value

The typed value (`XmlAtomicValue::Decimal`, a `rust_decimal::Decimal`) holds a
value exactly when it is `m / 10^s` for integers with

- `|m| ≤ 2^96 − 1 = 79228162514264337593543950335`, and
- `0 ≤ s ≤ 28`

(trailing fraction zeros do not count). Every value that a `totalDigits` facet
of 28 accepts qualifies: Datatypes §4.3.11.3 makes it `i / 10^n` with
`|i| < 10^28 < 2^96` and `n ≤ 28`. A count of significant digits does not
decide it — `10^−29` and `10^29` have one each and do not qualify.

A valid value beyond that is never rounded:

- its typed value is its canonical representation (Datatypes §3.3.3.2), an
  `XmlAtomicValue::String` with the `xs:decimal` type code, so
  `XmlValue::as_decimal` returns `None`;
- the validator reports a warning with code `xs-decimal-capacity` (Datatypes 1.1
  §5.4) while `ValidationFlags::REPORT_WARNINGS` is set, which it is by default.

### In XPath

A decimal held in canonical form:

- compares exactly;
- casts to `xs:string` (the canonical form), `xs:boolean`, `xs:integer`
  (truncation), `xs:double` and `xs:float` (nearest value);
- raises `FOAR0002` in arithmetic, rather than computing with an approximation.

An XSD 1.1 assertion (`xs:assert`, `xs:assertion`) whose evaluation fails only
because it computed with such a value is not counted as false: its outcome is
unknown, and the value carries the `xs-decimal-capacity` warning. Any other
error still fails the assertion (Structures §3.13.4.1), also when such a value
occurs elsewhere in the instance.

Casting a string to `xs:decimal`, or writing a decimal literal, follows F&O
§17.1.1 and §6.2:

| Value | Result |
| --- | --- |
| magnitude larger than held | `FOAR0002` |
| nonzero magnitude below `10^−28`, the smallest positive value held | `0` (underflow) |
| any other value with more precision than is held | `FOCA0006` |

## 2. `xs:integer`

`xs:integer` values have no size limit (`BigInt`).

## 3. Regular expressions

Groups and character classes may be nested 64 levels deep (`(`…`)` and
`[`…`]`, a subtracted class `[a-z-[aeiou]]` included, counted together). The
regular-expression engine compiles and matches by recursion, one level of the
stack per level of nesting, so a deeper expression is refused rather than
allowed to overflow the stack: an `xs:pattern` facet is an invalid pattern
(the message names the limit), and `fn:matches`, `fn:replace` and
`fn:tokenize` raise `FORX0002`. Real expressions nest a few levels; the
deepest in the W3C XML Schema test suite nests 15.

## 4. XML input

Every reader of XML text in the crate — the streaming validation driver, the
typed build, `BufferDocument::from_reader` and the schema-document parser —
reads through one well-formedness layer (`xsd_schema::xml`). A document that
is not well-formed is refused with `xml::NotWellFormed`, naming the rule and
the byte position (XML 1.0 §1.2: a fatal error); the checks are always on.

### Versions

- A document declared `version="1.1"` is read with XML 1.1 (Second Edition)
  and Namespaces in XML 1.1 (Second Edition): character references to the
  restricted control characters are allowed and the characters themselves
  refused raw, NEL and LINE SEPARATOR are line ends in character data and
  attribute values, and a prefix may be undeclared (`xmlns:p=""`).
- Every other document is read with XML 1.0 (Fifth Edition) and Namespaces in
  XML 1.0 (Third Edition): no declaration (XML 1.1 §4.3.4: "treated as if
  labeled as version 1.0"), `version="1.0"`, and any other `1.x` (XML 1.0
  §2.8: "it will process it as a 1.0 document").
- Not supported: a NEL or LINE SEPARATOR used as white space *inside markup*
  of an XML 1.1 document (between attributes, in an end tag, in a declaration)
  is refused (`WellFormednessRule::Xml11LineEndInMarkup`), although XML 1.1
  reads it as a line feed; the optional XML 1.1 normalization checks (§2.13)
  are not made.

### Encodings

- The readers read UTF-8. A UTF-8 byte-order mark is skipped. Bytes that are
  not well-formed UTF-8 are refused (XML 1.0 §4.3.3).
- The XML declaration's `encoding` does not change how the bytes are read: a
  host that decodes another encoding first (UTF-16 with `decode_xml_bytes`,
  say) passes UTF-8 text that keeps its original declaration. The
  declaration is reported to `ValidationEventHandler::on_xml_declaration` and
  kept by `BufferDocument::xml_declaration`; a host that receives bytes it did
  not decode refuses an `encoding` other than UTF-8 there.
- UTF-16 or another encoding passed undecoded is not read correctly: most such
  input is refused as ill-formed UTF-8, but bytes of a single-byte encoding
  can happen to form valid UTF-8.

### Document type declarations

- A document type declaration is accepted only in the prolog, once; its name,
  external identifier and internal subset are checked for well-formedness to
  the extent that the subset's markup is balanced (literals, comments,
  processing instructions, markup declarations, parameter-entity references).
- It is reported to `ValidationEventHandler::on_doctype`; a host refuses DTDs
  there.
- Its declarations are not processed — not yet supported: the external subset
  is never read, a reference to an entity the internal subset declares is
  refused (only `amp`, `lt`, `gt`, `apos` and `quot` are known), and
  attribute defaults and types declared in it are not applied.

### Parse limits

- `ParseLimits` (no limits by default): element depth, name length,
  namespace-URI length, attributes and namespace declarations per element,
  namespaces in scope, attribute-value size, text size (also white space
  outside the root element and the document type declaration) and the node
  count (attributes and namespace declarations included). Set on the
  validation runtime (`set_parse_limits`) or the document builder
  (`with_parse_limits`).
- Each is checked on the raw markup before anything of it is interned or
  built: nothing of the construct that breaks a limit — or a well-formedness
  rule — reaches the name table. What came before it in the document was read
  normally.
- They do not bound peak memory: the underlying reader holds a whole tag, text
  run, comment, processing instruction or document type declaration in its
  buffer before it is checked, and a text run is gathered before validation.
  Bound the size of the input as well.

### Known gaps

- Line ends inside CDATA sections, comments and processing instructions are
  passed as written: a carriage return (CR LF, or a lone CR) is not
  normalized to a line feed, and in an XML 1.1 document NEL and LINE
  SEPARATOR stay as they are there. Character data and attribute values are
  normalized.
