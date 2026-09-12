# BufferDocument Overview

`BufferDocument` is a compact, flat-array XML store optimized for XPath 2.0
navigation with XSD type information. It lives in `src/document/` and is
gated by the `xsd11` feature.

---

## Why It Exists

Two consumers need an in-memory XML model:

- The XPath 2.0 engine, both for standalone use and for evaluating XSD 1.1
  `xs:assert` and type alternatives.
- Schema-aware traversal that needs to ask, per node, "what is the resolved
  type and declaration that governs this element/attribute?"

It implements the `DomNavigator` trait (defined in `src/navigator/mod.rs`)
through `BufferDocNavigator`, sharing the same trait with `RoXmlNavigator`.
XPath functions stay DOM-agnostic.

---

## Two Modes

```rust
pub enum DocumentKind {
    Full,        // Whole document; element index, IDs, source spans active
    Fragment,    // Synthetic root wrapping a single asserted element subtree
}
```

Both variants are unconditional. The whole `src/document/` module — and all
of XPath with it — is `xsd11`-gated at the crate root, so per-variant
`#[cfg(feature = "xsd11")]` would be redundant. Earlier draft designs showed
`Fragment` gated; the shipping code does not.

`Full` is what callers use when they own the document. It does not insist on
a single document element: the builder accepts several top-level elements
under the root, which is what a constructed node sequence or a temporary
tree needs. `Fragment` is what `ValidationRuntime` builds on the fly during streaming validation so that
XSD 1.1 assertions can run XPath against the asserted element's subtree
without preloading the instance into a DOM. Side tables (element index,
source spans, ID map) are skipped in `Fragment` mode.

---

## Node Layout

Each node is exactly 16 bytes: four `u32` words.

```rust
pub struct Node {
    pub next_sibling: u32,   // NULL = u32::MAX
    pub parent:       u32,
    pub props_type:   u32,   // packed: node type + flags + 20-bit binding index
    pub value:        u32,   // QNameAtom index OR string-store index, depending on type
}
```

`props_type` bit layout:

| Bits | Meaning |
| --- | --- |
| `[3:0]` | `NodeType` (Root, Element, Attribute, ChildValue, Text, Whitespace, SignificantWhitespace, Comment, PI, Nul) |
| `[7:4]` | Flags: `HAS_ATTRIBUTE`, `HAS_CHILDREN`, `IS_COMPLEX_TYPE`, `HAS_NMSP_DECLS` |
| `[8]` | `IS_NIL` — per-instance `xsi:nil="true"` flag |
| `[31:12]` | 20-bit `binding_index` into the document-local `BindingRemapTable` |

### Two-node pattern

Element, Attribute, and Processing Instruction all use a uniform encoding:

- `Element` / `Attribute` / `ProcessingInstruction` nodes carry the *name*
  (a `QNameAtom` index for elements/attributes; a string index for PI targets).
- The associated *value* is a separate `ChildValue` child node.
- Element content children are then ordinary nodes in document order.

This uniformity simplifies forward scans: a single node-type dispatch handles
all three cases.

---

## Addressing

`NodeRef` is a plain `u32` index into a flat array of fixed-size pages:

- Page size: 4096 nodes (`PAGE_SHIFT = 12`), so each page is 64 KB.
- Page/slot decoding is a bitshift / mask — no bit-packed addressing.
- `NULL = u32::MAX`; `Nul` node type marks end-of-document for forward scans.
- First child of `parent` is always `parent + 1` (when `HAS_CHILDREN`).
- Subtree end is found by walking `next_sibling`/`parent` chains until a
  sibling exists or the document ends.

Capacity (~4 billion nodes, ~64 GB) is well past any practical document.

### Identity and order across documents

A `NodeRef` is only meaningful inside its own document, so a navigator's
identity is the pair (document, cursor): `is_same_position` compares the
document pointer before the node index, the virtual attribute/namespace
cursor and the namespace chain position. Every `BufferDocument` also gets a
`serial()` — a creation ordinal from a process-wide counter, unique within
the process and strictly increasing — and `compare_position` orders nodes of
different documents by it. XPath 2.0 §2.4.1 leaves the order of distinct
trees implementation-dependent but requires it to be stable and to keep each
tree contiguous; the serial gives a reproducible order where the heap address
would not.

---

## Schema Binding

XSD validation needs more than a type per
node: default and fixed values, nillability, identity-constraint membership,
and substitution-group membership are all *declaration-level*, not
type-level. Two element declarations can share one complex type but
different defaults.

Each typed node therefore carries a binding, not just a type:

```rust
pub struct NodeSchemaBinding {
    pub type_key:        TypeKey,
    pub element_decl:    Option<ElementKey>,    // for elements
    pub attribute_decl:  Option<AttributeKey>,  // for attributes
    pub content_type:    Option<ContentType>,   // for elements
}
```

Bindings live in a per-document `BindingRemapTable` and are referenced by a
20-bit `binding_index` packed into `props_type`. The table deduplicates:
identical `NodeSchemaBinding`s collapse to one entry. Index `0` means
*unbound*. The 20-bit field comfortably exceeds the typical document's
~100–1000 unique bindings.

Two flags optimize hot paths off this design:

- `IS_COMPLEX_TYPE` (bit 6) records simple-vs-complex without a table lookup.
- `IS_NIL` is kept *out* of the binding so dedup works — nil state is
  per-instance, not per-declaration.

The binding's `type_key` is what `DomNavigator::type_annotation()` returns
for element and attribute nodes — complex or simple — while `schema_type()`
is its simple-type projection (`None` for a complex type). Both are `None`
on unbound nodes.

`typed_value()` uses `element_decl` directly: when an element is empty and
its declaration carries `ValueConstraint::Default` or `Fixed`, the declared
value is parsed as the typed value. This is what lets assertions consume
default-aware values without synthetic text nodes or caller fallback logic.

---

## Side Tables

Kept off the main node array because their access patterns differ:

| Table | When | Purpose |
| --- | --- | --- |
| `NamespacePageFactory` | always | In-scope namespace chains (namespace axis has different semantics) |
| `ElementIndex` | `Full` only | Fast `//name` lookup |
| `NodeSourceSpans` | `Full` + tracking enabled | Line/column for error reporting |
| `id_elements: HashMap` | `Full` only | `xs:ID` → element node, for `id()` and IDREF resolution |

The shared `NameTable` (from `src/namespace/`) and a per-document
`QNameTable` / `StringStore` carry the actual string data.

---

## Construction

`BufferDocumentBuilder` exposes both a low-level push API and a
`quick-xml`-driven adapter (`from_reader`). The arena (`bumpalo::Bump`) is
**caller-owned**: the builder borrows it. This is what lets
`ValidationRuntime` reuse a single builder across nested asserted elements
without allocating per-element arenas, and what makes the second
`unsafe` block in the crate necessary (see [`UNSAFE.md`](UNSAFE.md)).

```rust
let arena = Bump::new();
let names = NameTable::new();
let doc = BufferDocument::from_reader_default(reader, &arena, &names)?;
let nav = doc.create_navigator();
```

Schema binding assignment is performed by an external driver
(`SchemaValidator` / `ValidationRuntime`), not by the builder itself —
the document module stays free of validation logic.

---

## Serialization

`src/document/serialize.rs` writes a tree back out as XML text. It is generic
over `DomNavigator`, not over `BufferDocument`, so the same code serializes a
`BufferDocNavigator`, a `RoXmlNavigator` and any navigator a host embedding the
engine brings of its own; it touches no `pub(crate)` node pages.

```rust
let xml = serialize::to_string(&doc.create_navigator(), &SerializeOptions::default())?;
```

Three entry points — `serialize_document` (a whole document, optionally with an
XML declaration), `serialize_node` (one node: a `Root` behaves as a document, an
element writes its subtree, an attribute or namespace node is refused) and
`to_string` — plus `SerializeOptions` and `SerializeError`.

**What is written.** Compact UTF-8 with no added whitespace, empty elements
collapsed to `<a/>`, attributes in stored document order. Escaping follows
*Canonical XML 1.0* §2.3: in text `&`, `<`, `>` and U+000D; in attribute values
(always `"`-quoted) `&`, `<`, `"` and the three whitespace characters as
references, so XML 1.0's end-of-line handling (§2.11) and attribute-value
normalization (§3.3.3) cannot change the value on a re-parse. Content that XML
cannot express — a character outside the `Char` production, a comment with `--`,
a PI target `xml`, a name whose prefix is not in scope — is an error, never
dropped or repaired.

**Namespace declarations** are written where they are *introduced*. The
serializer keeps its own in-scope stack while descending and diffs each
element's `namespace::ExcludeXml` axis against it: a binding that is not already
in scope is declared, one that merely repeats an inherited declaration is
dropped, and an element that leaves an inherited default namespace gets
`xmlns=""`. The `xml` prefix is never declared, prefixed undeclarations (XML 1.1
only) are never written. A name that does not resolve through that stack is
`SerializeError::UnboundName` — the serializer refuses to write a tree whose
names would not read back the same, and leaves repairing them to whoever built
it. Declarations on one element come out in prefix order, since the namespace
axis has no order of its own to preserve.

**What `from_reader` cannot give it back.** The parse keeps no XML declaration
(version, encoding, `standalone`), no `<!DOCTYPE` or internal entity
declarations, no CDATA section markers (the content is kept, as text), no text
outside the document element, and no trailing whitespace in processing-
instruction data (`parse_pi_content` trims the raw content). A round trip is
therefore an identity on the *tree*, not on the bytes;
`tests/serialize_roundtrip.rs` holds the whole XSD conformance corpus to that
standard.

---

## Fragment Mode And Assertions

When `ValidationRuntime` opens an element whose governing complex type has
assertions and `AssertionSource::FragmentBuffer` is active:

1. A `Bump` arena and a `BufferDocumentBuilder` in `DocumentKind::Fragment`
   mode are started, scoped to that element.
2. Subsequent push events for the subtree are fed into the same builder.
   Nested asserted elements share the builder and arena.
3. On close of the outermost asserted element, `builder.finalize()`
   produces a `BufferDocument` whose synthetic Root wraps the asserted
   element; navigation stops at the fragment boundary.
4. XPath assertions evaluate with the asserted element (not the synthetic
   Root) as context item. The arena is then dropped.

This is the streaming counterpart to the `Full` mode and is the assertion
path described in [`OVERVIEW.md`](OVERVIEW.md) under *XSD 1.1 Assertion Buffering*.

---

## Where Things Live

| Concern | Path |
| --- | --- |
| `Node`, node type, flags | `src/document/node.rs` |
| Pages, allocation | `src/document/page.rs` |
| QName / string interning | `src/document/qname.rs`, `strings.rs` |
| `NodeSchemaBinding`, remap | `src/document/type_remap.rs` |
| Namespaces | `src/document/namespace.rs` |
| Element index / source spans | `src/document/element_index.rs`, `source_spans.rs` |
| Document, navigator, builder | `src/document/document.rs`, `navigator.rs`, `builder.rs` |
| XML output | `src/document/serialize.rs` |