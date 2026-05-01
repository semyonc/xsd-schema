# xsd-schema Architecture Overview

**Crate**: `rust/xsd-schema`  

This document is the architectural map for contributors. It explains how data
moves through the crate, which module owns which phase, and which invariants
must be preserved when changing behavior.

Use 
* [`INTRODUCTION.md`](INTRODUCTION.md) for public API examples, 
* [`EXTENSIBILITY.md`](EXTENSIBILITY.md) for
extension points, 
* [`BUFFER_DOCUMENT_OVERVIEW.md`](BUFFER_DOCUMENT_OVERVIEW.md) for the schema-aware
  XPath/assertion document store, and 
* [`UNSAFE.md`](UNSAFE.md) for unsafe-code invariants.

---

## Purpose

`xsd-schema` is a Rust implementation of XSD 1.0 and XSD 1.1 schema processing
and instance validation. It parses schema documents, composes include/import/
redefine/override graphs, resolves QName references into typed component keys,
compiles content models, and validates XML instances through a push-event API.

With the `xsd11` feature, the crate also provides:

- XPath 2.0 evaluation.
- XSD 1.1 assertions and type alternatives.
- `BufferDocument`, a compact schema-aware XML document store used by XPath and
  assertion evaluation.
- `regexml`-backed XSD/XPath regex support.

---

## Feature And Version Boundaries

Cargo features control which code is compiled. `SchemaSet` controls which XSD
semantics are used at runtime.

| Boundary | Meaning |
| --- | --- |
| `SchemaSet::new()` | Runtime XSD 1.0 semantics. Use this even with `--features xsd11` when running the XSD 1.0 conformance suite. |
| `SchemaSet::xsd11()` | Runtime XSD 1.1 semantics. Enables XSD 1.1 rules in the schema model and validator. |
| default features | XSD 1.0 processing plus default Unicode normalization support; no XPath/document modules. |
| `xsd11` feature | Compiles XPath 2.0, `document/`, XSD 1.1 code paths, and `regexml`. |
| `async` feature | Adds async schema-loading/directive-resolution I/O. It does not make parsing, resolution, compilation, or validation async. |

Conformance tests should normally be run with `--features xsd11` for both XSD
versions because the feature enables the conformance-grade regex backend. The
runtime version is still selected by the test harness through `SchemaSet::new()`
or `SchemaSet::xsd11()`.

---

## Processing Pipeline

The central object is `SchemaSet`. Before processing completes it is mutable
and accumulates parsed documents, components, namespace tables, composition
edges, and source maps. After processing, validation treats it as immutable and
stores per-instance mutable state in `ValidationRuntime`. The XPath/document
layer (compiled with `xsd11`) hangs off the same `SchemaSet` for assertions,
type alternatives, identity helpers, and standalone XPath use.

```text
schema bytes / locations
        |
        v
parse + assemble
src/parser/parse.rs
src/parser/frames/
src/parser/assemble.rs
        |
        v
SchemaSet with documents, arenas, namespace tables,
composition directives, SourceRef-backed diagnostics
        |
        v
directive loading and composition
src/parser/resolver.rs
src/schema/composition.rs
src/schema/redefine.rs
src/schema/override_dir.rs
        |
        v
inline type and local particle allocation
src/schema/inline.rs
        |
        v
QName reference resolution
src/schema/resolver.rs
        |
        v
schema-time checks and content model compilation
src/pipeline.rs
src/schema/derivation.rs
src/compiler/
        |
        v
push-event instance validation
src/validation/validator.rs
src/validation/runtime.rs
```

### Phase Ownership

| Phase | Main code | Input | Output / mutation |
| --- | --- | --- | --- |
| Parse | `src/parser/parse.rs`, `src/parser/frames/` | XML schema bytes | Frame results with names, namespace snapshots, source locations. |
| Assemble | `src/parser/assemble.rs` | Parser frame results | `SchemaDocument`, top-level arena components, namespace tables, unresolved `QNameRef`s. |
| Directive resolution | `src/parser/resolver.rs` | `xs:include`, `xs:import`, `xs:redefine`, `xs:override` | Additional documents, loaded-location caches, composition edges. |
| Composition | `src/schema/redefine.rs`, `src/schema/override_dir.rs`, `src/schema/composition.rs` | Parsed documents and edges | Effective component graph after redefine/override rules. |
| Inline allocation | `src/schema/inline.rs` | Inline type definitions and local particles | Arena keys for inline types and local element declarations. |
| Reference resolution | `src/schema/resolver.rs` | `QNameRef`s | Typed keys in `resolved_*` fields, plus `src-resolve` diagnostics. |
| Schema checks | `src/pipeline.rs`, `src/schema/derivation.rs`, `src/schema/edc.rs` | Resolved schema model | Derivation, value constraint, EDC, UPA, and structural errors. |
| Compilation | `src/compiler/` | Resolved particles and groups | `NfaTable`, `AllGroupModel`, substitution-group maps, open-content matchers. |
| Validation | `src/validation/` | Compiled `SchemaSet` + XML events | `SchemaInfo`, typed values, validation diagnostics, ID/IDREF and identity-constraint state. |

---

## Core Data Structures

### SchemaSet

`SchemaSet` in `src/schema/model.rs` owns all schema state:

- `name_table`: interned strings for names and namespace URIs.
- `source_maps`: document bytes and byte/line mapping for diagnostics.
- `documents`: every parsed schema document.
- `namespaces`: per-target-namespace component lookup tables.
- `arenas`: flat storage for schema components.
- `loaded_locations` and `chameleon_cache`: directive-resolution caches.
- `composition_edges`: include/import/redefine/override relationships.
- `xsd_version`: runtime XSD 1.0 vs 1.1 semantics.
- `regex_compatibility`: strict XSD regex or narrow compatibility mode.

### SchemaArenas

`SchemaArenas` in `src/arenas.rs` stores each component family in a separate
`SlotMap` with typed keys:

- simple and complex types
- elements and attributes
- model groups and attribute groups
- notations
- identity constraints

The design avoids Rust reference cycles. Components store unresolved lexical
references as `QNameRef` during parse/assemble phases, then store resolved
typed keys in parallel `resolved_*` fields after `resolve_all_references`.

### SourceRef

`SourceRef` ties diagnostics back to the lexical schema document and is load-
bearing for per-document QName visibility: a QName resolves in the context of
the document that lexically contained it, not any document sharing the same
`SchemaSet`.

### NameTable

`NameTable` interns strings and returns stable `NameId`s. It holds one of the
crate's two `unsafe` blocks (`resolve_ref` returns `&str` into interned data
without allocating, so `DomNavigator` methods don't need to allocate). The
other lives in `ValidationRuntime::begin_assertion_buffering`, which extends
the lifetime of a `Bump` arena borrow used by the self-referential fragment
builder. Both contracts are documented in `doc/UNSAFE.md` and verified with
Miri.

---

## Validation Architecture

Validation is event-driven. `SchemaValidator` owns immutable configuration and
starts a `ValidationRuntime` for each instance validation run.

```text
SchemaValidator
    |
    v
ValidationRuntime
    |
    +-- element stack and SchemaInfo
    +-- content-model state (NFA, all-group, open content)
    +-- attribute and wildcard validation
    +-- simple type and facet validation
    +-- identity constraint tables
    +-- ID/IDREF registries
    +-- optional XSD 1.1 assertion buffering
```

Important runtime modules:

| Path | Responsibility |
| --- | --- |
| `src/validation/validator.rs` | Public push-event API and validator configuration. |
| `src/validation/runtime.rs` | Main per-run validation engine. |
| `src/validation/content.rs` | Content-model state transitions. |
| `src/validation/simple.rs` | Simple type, list/union, atomic value, and facet validation. |
| `src/validation/identity.rs` | Streaming `key`, `unique`, and `keyref` evaluation. |
| `src/validation/active_axis.rs` | Efficient selector/field axis matching for identity constraints. |
| `src/validation/assertions.rs` | XSD 1.1 assertion evaluation. |
| `src/validation/alternatives.rs` | XSD 1.1 type alternative selection. |
| `src/validation/hint_loader.rs` | `xsi:schemaLocation` / `xsi:noNamespaceSchemaLocation` enrichment. |

`ValidationFlags` controls optional runtime behavior. Identity constraints are
parsed unconditionally but enforced only when `PROCESS_IDENTITY_CONSTRAINTS` is
set. XSD 1.1 assertions require an assertion-capable validator constructor such
as `SchemaValidator::new_fragment_buffer`.

### XSD 1.1 Assertion Buffering

`xs:assert` does not require preloading the instance into a DOM. When an
element whose governing complex type has assertions is opened,
`ValidationRuntime` starts a scoped `BufferDocumentBuilder` in
`DocumentKind::Fragment` mode and feeds subsequent events for that subtree
into it. When the element closes, the fragment is finalized, XPath
assertions are evaluated with the asserted element as context item, and the
temporary arena is released.

The public API stays push-based; XPath only sees a navigable fragment for
the scope that needs one. A fully built typed `BufferDocument` is an
optional alternative for consumers that already own a document model.

---

## Content Models

Complex-type content models are compiled in `src/compiler/`.

| Concept | Implementation |
| --- | --- |
| sequence/choice/general particles | NFA fragments and `NfaTable` in `nfa.rs`, `particle.rs`, and `compile.rs`. |
| `xs:all` | Dedicated `AllGroupModel` bitmask/counter matcher in `all_group.rs`; not lowered to an ordinary NFA at runtime. |
| UPA / `cos-nonambig` | `upa.rs`, with conformance-specific handling for counters and substitution groups. |
| substitution groups | `substitution.rs`; runtime and UPA maps are built from resolved head/member links. |
| XSD 1.1 open content | `open_content.rs`; supports interleave and suffix modes. |

Content-model changes often require checking both schema-time compilation and
runtime matching. In particular, local element declarations and model-group
particles have their own allocation/resolution paths in `src/schema/inline.rs`
and `src/schema/resolver.rs`.

---

## XPath And Document Layer

The XPath/document layer is compiled only with `xsd11`.

### DomNavigator

`DomNavigator` is the abstraction that lets XPath run over different XML
backends. It is defined in `src/navigator/mod.rs` (compiled unconditionally,
since validation also uses it). The crate ships:

- `RoXmlNavigator` (`src/navigator/roxmltree.rs`): adapter over `roxmltree`,
  used heavily in tests and conformance harnesses.
- `BufferDocNavigator` (`src/document/navigator.rs`, `xsd11`): adapter over
  `BufferDocument`, used for schema-aware document and assertion evaluation.

### BufferDocument

`BufferDocument` in `src/document/` is a compact flat-array XML store with
schema binding metadata. It is read-only after construction and exists to make
XPath 2.0 navigation and XSD 1.1 assertion evaluation efficient without tying
the XPath engine to `roxmltree`.

It supports two modes: a **fragment** built on the fly during streaming
validation (see *XSD 1.1 Assertion Buffering* above), and a **full document**
when a caller already owns one and wants post-parse traversal.

The `BufferDocument` is not just a
structural DOM. Each node carries schema type information through the
20-bit `binding_index` into a per-document `BindingRemapTable`, where each
entry pins down the resolved type, the governing element/attribute
declaration, and the content type. Because of this, `BufferDocNavigator`
exposes *typed* values — `XmlAtomicValue` / `XmlValue` from `src/types/`,
not just lexical strings — and `typed_value()` honours declaration-level
default and fixed values for empty elements (cvc-elt.5.2) without
synthetic text nodes. Consequences for callers:

- XPath 2.0 expressions and XSD 1.1 assertions consume schema-typed values
  directly; no re-parsing per query, and no separate cast layer.
- The shared datatype layer in `src/types/` (see the *One datatype layer
  for both engines* invariant) is what makes this work — atomic values
  produced by the validator and atomic values pulled by the navigator are
  the same objects.
- `RoXmlNavigator` over `roxmltree` is structurally equivalent but
  *untyped*: it cannot answer typed-value queries. Code that needs typed
  navigation must use `BufferDocNavigator`.

### XPath 2.0 Engine

`src/xpath/` contains:

- LALRPOP grammar and lexer.
- AST and name binder.
- static/dynamic context.
- evaluator and axis iterators.
- function registry and built-in functions.
- typed value and sequence/operator logic.

XPath functions use `DomNavigator` and share the datatype layer in
`src/types/` with the XSD validator (atomic values, type codes, casting,
equality, facets, `VALIDATOR_REGISTRY`). They should not depend on a concrete
DOM implementation, and they should not reimplement type machinery — see the
*One datatype layer for both engines* invariant below.

---

## Module Map

| Path | Responsibility |
| --- | --- |
| `src/lib.rs` | Public API surface and re-exports. |
| `src/builder.rs` | `SchemaSetBuilder` fluent load/compile API. |
| `src/pipeline.rs` | High-level orchestration for parse, load, compose, resolve, and checks. |
| `src/error.rs` | `SchemaError`, `ValidationError`, and diagnostic codes. |
| `src/ids.rs` | Typed arena keys. |
| `src/arenas.rs` | Flat component storage. |
| `src/embedded.rs` | Embedded catalog schemas such as XML and XLink. |
| `src/regex_convert.rs` | XSD regex translation for the Rust `regex` backend. |
| `src/regex_xsd_unicode.rs` | Unicode 3.0 tables for XSD 1.0 regex compatibility. |
| `src/parser/` | Schema XML parsing, frame state machine, assembly, loaders, source maps. |
| `src/schema/` | Schema model, composition, inline allocation, reference resolution, derivation, EDC. |
| `src/types/` | Built-in type registry, XSD atomic values, facets, conversions, equality. |
| `src/compiler/` | Content-model compilation, UPA, all-groups, substitution groups, open content. |
| `src/validation/` | Push-event instance validation and PSVI output. |
| `src/namespace/` | `NameTable`, QName helpers, namespace context snapshots. |
| `src/navigator/` | DOM adapters exposed outside the XPath module. |
| `src/xpath/` | XPath 2.0 parser, binder, evaluator, functions, operators (`xsd11`). |
| `src/document/` | `BufferDocument` and typed document support (`xsd11`). |
| `tests/conformance/` | W3C XSD conformance driver and reporting. |
| `tests/xqts/` | XPath/XQuery test-suite driver (`xsd11`). |

---

## Architectural Invariants

- **Runtime version is not the cargo feature**: `xsd11` compiles code; `SchemaSet`
  selects XSD 1.0 or 1.1 semantics.
- **QName visibility is lexical-document scoped**: references must be checked
  using the `SourceRef` document that contained the QName.
- **Arena keys are the component identity**: do not store long-lived references
  to arena values; store typed keys.
- **Parser output may contain unresolved names**: parse/assemble should preserve
  `QNameRef`, namespace snapshots, annotations, and source locations. Resolution
  belongs in later phases.
- **Bypassing `ReferenceResolver` is risky**: any fast path that calls
  `lookup_type`, `lookup_element`, or namespace tables directly must still
  enforce namespace visibility and version semantics.
- **Validation state is per run**: never store instance-validation state in
  `SchemaSet`.
- **Leniency must be explicit**: compatibility behavior belongs behind a named
  flag or narrow internal rule with tests; strict conformance is the default.
- **XSD 1.1 code must be feature-gated**: public and internal dependencies on
  XPath, `BufferDocument`, `regexml`, and assertion machinery require
  `#[cfg(feature = "xsd11")]`.
- **Unsafe code is exceptional**: new unsafe blocks require a documented invariant
  in `doc/UNSAFE.md` and Miri coverage.
- **One datatype layer for both engines**: `src/types/` is the single source of
  atomic values, type codes, casting, equality, facets, and the validator
  registry — shared by the XSD validator and the XPath 2.0 engine. XPath does
  not own its own atomic-value or cast hierarchy: when XPath needs `xs:date`
  arithmetic, a `Decimal` cast, a facet-aware comparison, or a validator,
  it goes through `XmlAtomicValue`, `XmlTypeCode`, `types::convert`,
  `types::equality`, and `VALIDATOR_REGISTRY`. Before adding type, cast, or
  comparison logic under `src/xpath/`, check whether the equivalent already
  exists in `src/types/` and extend that instead. Reason: XSD 1.1 assertions
  evaluate XPath against schema-typed values, so divergence between the two
  layers shows up as conformance bugs that are hard to localize.

---

## Where To Start

| Task | Start with | Usually also inspect |
| --- | --- | --- |
| Public API use or examples | `doc/INTRODUCTION.md` | `src/lib.rs`, `src/builder.rs` |
| Schema loading/import bugs | `src/parser/resolver.rs` | `src/pipeline.rs`, `src/schema/composition.rs`, `src/embedded.rs` |
| Parser support for an XSD element | `src/parser/frames/` | `src/parser/assemble.rs`, `src/schema/model.rs` |
| QName resolution / `src-resolve` | `src/schema/resolver.rs` | `src/parser/location.rs`, `src/schema/model.rs` |
| Inline type or local particle issues | `src/schema/inline.rs` | `src/schema/resolver.rs`, `src/compiler/compile.rs` |
| Type derivation or value constraints | `src/schema/derivation.rs` | `src/types/`, `src/validation/simple.rs` |
| Content model or UPA failures | `src/compiler/` | `src/validation/content.rs`, `src/pipeline.rs` |
| Instance validation behavior | `src/validation/runtime.rs` | `src/validation/simple.rs`, `src/validation/info.rs` |
| Identity constraints | `src/validation/identity.rs` | `src/validation/active_axis.rs`, `src/validation/identity_parser.rs` |
| XSD 1.1 assertions | `src/validation/assertions.rs` | `src/document/`, `src/xpath/` |
| XPath engine work | `src/xpath/` | `src/navigator/`, `src/document/` |
| Unsafe-code review | `doc/UNSAFE.md` | `src/namespace/table.rs`, `src/validation/runtime.rs` |