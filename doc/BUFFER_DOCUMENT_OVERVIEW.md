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

`Full` is what callers use when they own the document. `Fragment` is what
`ValidationRuntime` builds on the fly during streaming validation so that
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
let doc = BufferDocument::from_reader_default(&arena, &names, reader)?;
let nav = doc.create_navigator();
```

Schema binding assignment is performed by an external driver
(`SchemaValidator` / `ValidationRuntime`), not by the builder itself —
the document module stays free of validation logic.

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