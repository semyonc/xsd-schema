# xsd-schema Introduction

This document is a short introduction to the public API in `xsd-schema`.
It is intentionally smaller than rustdoc: use it to find the right entry
points first, then drill into rustdoc for method-level details.

## Feature Sets

Before choosing an API entry point, decide which crate configuration you want.
The public surface changes mainly through the `xsd11` and `async` features.

| Mode | Cargo setup | What you get |
| --- | --- | --- |
| XSD 1.0 only | `default-features = false` | Most lightweight mode. XSD 1.0 schema parsing, compilation, and validation only. No XPath engine, no `BufferDocument` support, no `regexml`-backed XSD/XPath regex support. |
| XSD 1.0 + async | `default-features = false, features = ["async"]` | Same XSD 1.0-only surface, plus async schema-loading/directive-resolution APIs. |
| XSD 1.1 | `features = ["xsd11"]` | Full XSD 1.1 mode: XSD 1.1 processing, XPath 2.0 engine, `regexml`, Unicode normalization, and `BufferDocument` / typed document support. |
| XSD 1.1 + async | `features = ["xsd11", "async"]` | Full XSD 1.1 surface plus async schema-loading/directive-resolution APIs. |

Notes:

- `xsd11` is the feature that unlocks the `xpath` and `document` modules.
- `async` does not make the whole pipeline async. It only affects schema loading
  and directive resolution I/O.
- If you just want the smallest build, use XSD 1.0 without default features.
- If you need XSD 1.0 validation with the XPath engine (or full `regexml`-backed
  XSD regex features like character-class subtraction), enable the `xsd11`
  feature and create your schema set with `SchemaSet::new()` (XSD 1.0 mode).
  The `xsd11` feature controls which code is *compiled*; `SchemaSet::new()`
  vs `SchemaSet::xsd11()` controls which *semantics* are applied at runtime.
  Note: pattern-facet `\p{X}` category escapes are version-gated — XSD 1.0
  mode pins the tables to Unicode 3.0 (matching the W3C XSD 1.0 test corpus,
  see `src/regex_xsd_unicode.rs`) regardless of whether the engine is the
  `regex` crate or `regexml`, while XSD 1.1 mode passes through to regexml's
  current Unicode tables per §G.4.2's "or in some later version" clause.

## Main Entry Points

| Task | Start here | Async variant (feature `async`) |
| --- | --- | --- |
| Load one schema | `load_and_process_schema`, `load_schema` | `load_and_process_schema_async`, `load_schema_async` |
| Load many related schemas | `SchemaSetBuilder`, or `parse_schema_only` + `process_loaded_schemas` | `SchemaSetBuilder::compile_async`, `add_async` |
| Switch between XSD 1.0 and 1.1 | `SchemaSet::new()` / `SchemaSet::xsd11()` | — |
| Stream XML validation | `validation::SchemaValidator` + `ValidationRuntime` | — |
| XPath evaluation | `xpath::XPathContext`, `xpath::XPathExpr`, `RoXmlNavigator` | — |
| External schema resolution | `SchemaResolver`, `SchemaLoader`, `LoaderChain`, `SchemaCatalog` | `AsyncSchemaLoader`, `SchemaResolver::load_schema_async` |
| Traverse compiled schema model | `SchemaSet`, `schema_set.namespaces`, `schema_set.arenas`, `schema::build_dependency_graph` | — |

## 1. Schema Loading

For most callers, the easiest API is:

- `SchemaSet::new()` for XSD 1.0
- `SchemaSet::xsd11()` for XSD 1.1
- `load_and_process_schema()` for a single schema document
- `SchemaSetBuilder` when you want a fluent multi-document load/compile flow

`load_and_process_schema()` runs the full pipeline:

1. Parse the primary schema.
2. Resolve `xs:include` / `xs:import` / `xs:redefine` / `xs:override`.
3. Apply redefine/override composition.
4. Assemble inline types.
5. Resolve QName references.
6. Allocate particle-local element/type bindings.

```rust
use xsd_schema::{SchemaSet, load_and_process_schema};

let mut xsd10 = SchemaSet::new();
load_and_process_schema(
    br#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root" type="xs:string"/>
        </xs:schema>"#,
    "memory.xsd",
    &mut xsd10,
    None,
)?;

let mut xsd11 = SchemaSet::xsd11();
load_and_process_schema(
    br#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
                    xmlns:vc="http://www.w3.org/2007/XMLSchema-versioning"
                    vc:minVersion="1.1">
            <xs:element name="root" type="xs:string"/>
        </xs:schema>"#,
    "memory-11.xsd",
    &mut xsd11,
    None,
)?;
# Ok::<(), xsd_schema::SchemaError>(())
```

For multi-file loading, `SchemaSetBuilder` is usually the cleanest API:

```rust
use xsd_schema::SchemaSetBuilder;

let compiled = SchemaSetBuilder::xsd11()
    .add("urn:books", "examples/books.xsd")?
    .compile()?;

println!("loaded {} document(s)", compiled.stats.documents_loaded);
let schema_set = compiled.schema_set();
# let _ = schema_set;
# Ok::<(), xsd_schema::SchemaError>(())
```

Use the lower-level two-phase API when you need to control document loading
yourself, for example when schemas come from a database, cache, or custom
loader:

```rust
use xsd_schema::{SchemaSet, parse_schema_only, process_loaded_schemas};

let primary_bytes = br#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"/>"#;
let shared_bytes = br#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"/>"#;

let mut schema_set = SchemaSet::new();

parse_schema_only(primary_bytes, "primary.xsd", &mut schema_set)?;
parse_schema_only(shared_bytes, "shared.xsd", &mut schema_set)?;

let (_inline_stats, _resolution_stats) = process_loaded_schemas(&mut schema_set)?;
# Ok::<(), xsd_schema::SchemaError>(())
```

Use `PipelineConfig::parse_only()` or `parse_schema_only()` if you want to stop
before directive loading and reference resolution.

### Async loading note

When the `async` feature is enabled, the crate also exposes async variants of
the top-level loading API:

- `load_schema_async(...)`
- `load_and_process_schema_async(...)`

For builder-style loading, the async counterparts are:

- `SchemaSetBuilder::with_async_loader(...)`
- `SchemaSetBuilder::add_async(...)`
- `SchemaSetBuilder::compile_async(...)`

Important detail: these APIs are async only for schema loading and directive
resolution I/O. Parsing, inline type assembly, QName resolution, and particle
allocation still run synchronously because they are CPU-bound phases.

## 2. XML Validation With Event Driving

The validation API is push-based. The usual call order is:

1. `SchemaValidator::new(...)`
2. `start_run(...)`
3. For each element:
   `validate_element` -> `validate_attribute*` -> `validate_end_of_attributes`
4. For content:
   `validate_text` / `validate_whitespace`
5. On close:
   `validate_end_element`
6. After EOF:
   `end_validation`

Each event returns `SchemaInfo`, which can be used to inspect the selected
declaration, resolved type, content model, typed value, and XSD 1.1 type
alternative/assertion results.

### ValidationFlags

`SchemaValidator::new(&schema_set, flags)` takes a `ValidationFlags` bitset
that turns runtime features on and off. The type is a
[`bitflags`](https://docs.rs/bitflags)-style set, so compose with `|`, remove
with `-`, and test with `.contains(...)`.

| Flag | Default | Purpose |
| --- | --- | --- |
| `REPORT_WARNINGS` | **on** | Emit XSD warnings (non-fatal diagnostics) to the sink alongside errors. Clear this if you only want hard errors. |
| `BUILD_PSVI_TYPED_VALUES` | **on** | Retain the typed value and whitespace-normalized value (PSVI `[schema actual value]` / `[schema normalized value]`) on the returned `SchemaInfo`, so callers can read `typed_value` / `normalized_value`. On by default for back-compat. **Clear it for validity-only streaming**: the runtime then skips building and keeping values the caller would discard — including an allocation-free fast path for the string family and for the integer hierarchy / `xs:decimal` (no per-value `BigInt`). **Validity and diagnostics are never affected** — values are still materialized internally wherever something *needs* one: a value-space facet (numeric range, `totalDigits`, value-space `enumeration`), a `fixed`/`default` comparison, ID/IDREF/ENTITY, an identity constraint, QName/NOTATION prefix binding, or an XSD 1.1 assertion. The fast paths only *accept* values they can prove valid; anything else defers to the normal path, so the emitted error codes are identical too. Only the *retained output* is dropped: with the bit clear, `SchemaInfo.typed_value` / `normalized_value` are `None` for nodes nothing else consumed. The W3C suite pass/fail set is identical with the bit on or off. |
| `PROCESS_IDENTITY_CONSTRAINTS` | off | Evaluate `xs:key`, `xs:unique`, and `xs:keyref` during instance validation. Declarations are always *parsed*; this bit controls whether their constraints are *enforced*. Leave off when you only need type-level validation — saves the per-element key/keyref bookkeeping. |
| `ALLOW_XML_ATTRIBUTES` | off | Accept every attribute in the reserved `http://www.w3.org/XML/1998/namespace` namespace (`xml:lang`, `xml:space`, `xml:base`, `xml:id`) **without** checking the element's complex type for an allowing declaration or wildcard. This is a lenient-parser convenience and is **not** XSD-conformant: the spec requires every attribute (including those in the xml namespace) to be matched by a declared `{attribute use}` or an `{attribute wildcard}` whose namespace constraint admits the xml namespace. Set this bit when you want `xml:lang` to "just work" against schemas that don't explicitly import the xml namespace; leave it clear for strict conformance (e.g. when running the W3C XSD test suite). `xml:base` base-URI tracking for `xsi:schemaLocation` hint resolution happens **regardless** of this flag — the flag only affects whether the attribute itself participates in type-level attribute validation. |
| `STRICT_MODE` | off | Promote warnings to errors. Combine with `REPORT_WARNINGS`. |
| `PROCESS_ASSERTIONS` (`xsd11`) | off | Enable XSD 1.1 `xs:assert` processing. **Must** be paired with a fragment-buffering validator constructed via `SchemaValidator::new_fragment_buffer(...)` — that constructor sets the bit for you. Passing `PROCESS_ASSERTIONS` to plain `SchemaValidator::new(...)` does **not** error; the flag is silently stripped (the constructor ensures the flag and `AssertionSource::Disabled` agree), so assertions will not run. If you build XSD 1.1 instance validation by hand and forget to use `new_fragment_buffer`, every `xs:assert` is skipped — negative instances will appear valid. |

The default — `ValidationFlags::default()` — enables `REPORT_WARNINGS` and
`BUILD_PSVI_TYPED_VALUES`. This matches the strict-conformance posture:
identity constraints, `xml:*` leniency, and strict-mode warning promotion are
all opt-in, and the PSVI typed values are populated for back-compat. Two common
recipes:

```rust
use xsd_schema::validation::ValidationFlags;

// Strict XSD conformance with key/unique/keyref enforcement:
let strict = ValidationFlags::default()
    | ValidationFlags::PROCESS_IDENTITY_CONSTRAINTS;

// Lenient mode for hand-written schemas: accept xml:lang / xml:space / xml:base
// on any element even if the schema does not declare them.
let lenient = ValidationFlags::default()
    | ValidationFlags::PROCESS_IDENTITY_CONSTRAINTS
    | ValidationFlags::ALLOW_XML_ATTRIBUTES;

// Lean streaming: only need valid/invalid + errors, not the PSVI typed values.
// Drops per-value materialization (incl. BigInt per integer) where nothing
// else needs it; validity is unchanged.
let lean = ValidationFlags::default()
    & !ValidationFlags::BUILD_PSVI_TYPED_VALUES;
```

For XSD 1.1 assertion-backed types, use the fragment-buffering validator
constructor. `new_fragment_buffer` sets `PROCESS_ASSERTIONS` for you:

```rust
use xsd_schema::validation::{SchemaValidator, ValidationFlags};

let flags = ValidationFlags::default()
    | ValidationFlags::PROCESS_IDENTITY_CONSTRAINTS;
// new_fragment_buffer adds PROCESS_ASSERTIONS internally; pass-through
// works equally well, but the plain `SchemaValidator::new(...)`
// constructor silently strips it — assertions would then never run.
let validator = SchemaValidator::new_fragment_buffer(&schema_set, flags);
```

The trap to know about: `SchemaValidator::new(&schema_set, flags)` removes
`PROCESS_ASSERTIONS` without erroring. A schema with `<xs:assert>` will then
load and validate every instance as if the assertion did not exist —
negative instances appear valid. Always pick a constructor explicitly when
you ship XSD 1.1 validation:

| Goal | Constructor |
| --- | --- |
| XSD 1.0, or XSD 1.1 without `xs:assert` | `SchemaValidator::new(...)` |
| XSD 1.1 with `xs:assert` evaluation, streaming | `SchemaValidator::new_fragment_buffer(...)` |
| XSD 1.1 with `xs:assert` against an external `BufferDocument` | `SchemaValidator::new_main_document(...)` |

### Driving validation from a `quick-xml` stream

The push-based runtime in §2 needs an XML event source. Hand-rolling that
plumbing means tracking xmlns scopes, scanning `xsi:type` / `xsi:nil`,
building a `NamespaceContextSnapshot` per element, and dispatching whitespace
vs character data correctly. Almost no caller wants to write that twice. The
`validation::quick_xml_driver` module ships a reusable driver in two layers:

| Layer | Entry points | Use when |
| --- | --- | --- |
| 1 — turn-key | `drive_quick_xml`, `drive_quick_xml_in` | All you want is for the runtime's sink to receive every diagnostic. The helper calls `runtime.end_validation()` for you. |
| 2 — hooks | `drive_quick_xml_with`, `drive_quick_xml_with_in` | You need to interleave work between validator events — building a typed DOM, tracking source spans, mirroring events into a custom store. The caller is responsible for calling `end_validation` after collecting any post-stream runtime state (e.g. `schema_location_hints`). |

Layer 1 in full:

```rust
use xsd_schema::{SchemaSet, load_and_process_schema};
use xsd_schema::validation::{
    drive_quick_xml, CollectingValidationSink, SchemaValidator, ValidationFlags,
};

fn validate(schema_xml: &str, instance_xml: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut schema_set = SchemaSet::new();
    load_and_process_schema(schema_xml.as_bytes(), "memory.xsd", &mut schema_set, None)?;

    let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let sink = CollectingValidationSink { errors: &mut errors, warnings: &mut warnings };
    let mut runtime = validator.start_run(sink);

    // Layer 1 internally: pushes xmlns scopes, scans xsi:type / xsi:nil,
    // dispatches whitespace vs text, and calls runtime.end_validation()
    // once the stream is drained. Comments and PIs are dropped.
    let _outcome = drive_quick_xml(instance_xml.as_bytes(), &mut runtime, &schema_set)?;

    if !errors.is_empty() {
        for error in &errors {
            eprintln!("{error}");
        }
    }
    Ok(())
}
```

Layer 1 returns a `DriveOutcome { root_validity, max_depth }` and reports
parse / UTF-8 / unbound-prefix / unexpected-EOF as `DriveError` variants;
validation diagnostics flow through the sink, not the return type.

#### Layer 2 — handler-driven (advanced)

Layer 2 is `drive_quick_xml_with(reader, &mut runtime, &schema_set, &mut handler)`
plus a `ValidationEventHandler` you implement. Every method has a default
no-op, so a handler that only cares about (say) end-of-element fires exactly
that one method. Hook ordering is documented on the trait; the most-used
hooks are:

| Hook | Fires when |
| --- | --- |
| `before_element(view)` | After the new element's xmlns scope is pushed and the namespace-context snapshot is built, but **before** `runtime.validate_element` is called. |
| `after_element(view, info)` | After `validate_element` returns. `info.schema_type`, `info.is_nil`, `info.element_decl`, and `info.content_type` are stable here. |
| `before_attribute(view)` / `after_attribute(view, info)` | Per-attribute, in document order. |
| `after_end_of_attributes(view)` | After `validate_end_of_attributes`. `view.info` is the EOA `SchemaInfo` (CTA-selected type lives here); under `xsd11`, `view.deferred_attribute_results` carries any CTA-revalidated per-attribute results, in original encounter order. |
| `after_end_element(end_info, depth)` | After `validate_end_element`, with the depth of the element that just closed (1 = root close). |
| `on_text(kind, text)` / `on_comment(text)` / `on_processing_instruction(target, data)` | Body events. `kind` distinguishes whitespace text, character text, and CDATA. |

Layer 2 does **not** call `runtime.end_validation()`; you do, after
draining whatever runtime state your caller cares about (e.g.
`schema_location_hints`).

#### Source-span tracking (advanced)

Two further hooks carry byte offsets from the reader's buffer:

- `on_element_start_offset(byte_pos)` — fires immediately before
  `before_element`; `byte_pos` is the `<` of the element being reported.
- `on_element_end_offset(byte_pos)` — fires immediately after
  `after_end_element`; `byte_pos` is one past the `>` of the closing tag,
  whether the close came from `Event::End` or `Event::Empty`.

Both default to no-ops, so non-span callers pay only the cost of two empty
calls per element. Override them when building a span-aware DOM
(`document::typed_builder` is the in-tree consumer; see
`source_span_tracking` in its tests for a regression net).

### Unparsed entity declarations (ENTITY / ENTITIES types)

XSD §3.16.4 requires that every `xs:ENTITY` or `xs:ENTITIES` value names a
declared unparsed entity from the document's DTD. Modern streaming XML parsers
like `quick-xml` deliberately omit DTD processing — DTDs are widely considered
a legacy attack surface (billion-laughs, external entity injection, etc.) and
most contemporary XML workflows avoid them entirely.

However, a conformant XSD validator must still enforce the entity-name constraint
when DTD information is available. To bridge this gap without coupling the
validator to a DTD parser, `ValidationRuntime` accepts an optional set of
declared unparsed entity names via `set_unparsed_entities()`:

```rust
use std::collections::HashSet;

let mut entities = HashSet::new();
entities.insert("logo".to_string());
entities.insert("photo".to_string());

runtime.set_unparsed_entities(entities);
```

When the set is provided, every `xs:ENTITY` / `xs:ENTITIES` value (including
defaulted attribute values) is checked against it. Undeclared names produce a
`cvc-datatype-valid.1.2.1` error. When the set is **not** provided (the
default), entity-name checking is skipped — the validator still validates
NCName syntax but does not require DTD context. This opt-in design keeps the
common case (no DTD) zero-cost while allowing conformance-critical deployments
to supply the information from whatever DTD source they have (pre-scan,
catalog, external parser, etc.).

Useful runtime helpers for editor or tooling scenarios:

- `get_expected_elements()` for content-model-aware completion/hints
- `get_expected_attributes()` for attribute completion
- `get_default_attributes()` for schema-supplied defaults
- `set_location()` if you want validation errors tied to stream positions

If you need a full namespace-aware `quick-xml` integration that also builds a
typed in-memory document, `src/document/typed_builder.rs` is a good reference.

### XSI attributes

The four built-in XSI attributes are validated with proper per-attribute
`SchemaInfo`:

| Attribute | Type | Notes |
| --- | --- | --- |
| `xsi:type` | `xs:QName` | Lexical QName validation; semantic resolution is in element validation |
| `xsi:nil` | `xs:boolean` | |
| `xsi:schemaLocation` | `list(xs:anyURI)` | Even token count enforced (namespace/location pairs) |
| `xsi:noNamespaceSchemaLocation` | `xs:anyURI` | |

Schema-location hints are accumulated during a validation run. Each hint
carries the instance document's base URI for correct relative URI resolution.
Set the base URI before starting validation:

```rust
runtime.set_instance_base_uri("/absolute/path/to/instance.xml");
```

Retrieve hints afterwards:

```rust
let sl_hints = runtime.schema_location_hints();    // &[SchemaLocationHint]
let nnsl_hints = runtime.no_namespace_schema_location_hints(); // &[NoNamespaceSchemaLocationHint]
```

Complete pairs are accumulated from every `xsi:schemaLocation` attribute,
even from values that failed even-token-count enforcement (the complete
pairs are still valid hints).

### Hint-driven schema enrichment (two-pass validation)

Because `ValidationRuntime` borrows `&SchemaSet` immutably, schemas named by
the `xsi:schemaLocation` hints collected above cannot be loaded mid-run. They
feed a *second* validation pass against an enriched schema set built with
`enrich_schema_set`:

```rust
use xsd_schema::enrich_schema_set;

let sl   = runtime.schema_location_hints().to_vec();
let nnsl = runtime.no_namespace_schema_location_hints().to_vec();

let outcome = enrich_schema_set(&schema_set, &sl, &nnsl);
let enriched = outcome.schema_set;   // Some(..) once hints loaded and recompiled
```

The wrinkle is *how to drive that second pass without parsing the instance
twice*. The push API is XML-source-agnostic, so alongside `drive_quick_xml`
there is a tree-walk driver that re-validates an already-parsed document:

| Driver | Module | Use when |
| --- | --- | --- |
| `drive_quick_xml` | `validation::quick_xml_driver` | Streaming from bytes/reader. For two-pass enrichment, call it again over the same bytes — simple, but parses the instance twice. |
| `drive_navigator` | `validation::navigator_driver` | Re-validate any `DomNavigator` tree (works with `RoXmlNavigator` without `xsd11`). Emits the *same* push-API sequence as the streaming driver. |
| `drive_buffer_document` | `validation::navigator_driver` (`xsd11`) | Convenience wrapper of `drive_navigator` for a `BufferDocument`. |

**Buffer once, validate many times.** Parse the instance a single time into a
`BufferDocument`, then validate it against the original *and* enriched schema
sets by re-walking the in-memory tree — no second parse:

```rust
use xsd_schema::validation::{drive_buffer_document, SchemaValidator, ValidationFlags};
use xsd_schema::document::{BufferDocument, BufferDocumentOptions};

// Parse once.
let doc = BufferDocument::from_reader(
    reader, &arena, &names, BufferDocumentOptions::default(), Some(&schema_set))?;

// Pass 1: collect hints.
let mut rt = SchemaValidator::new(&schema_set, ValidationFlags::default()).start_run(sink1);
drive_buffer_document(&doc, &mut rt, &schema_set)?;
let sl   = rt.schema_location_hints().to_vec();
let nnsl = rt.no_namespace_schema_location_hints().to_vec();

// Pass 2: the SAME tree against the enriched set — no re-parse.
if let Some(enriched) = enrich_schema_set(&schema_set, &sl, &nnsl).schema_set {
    let mut rt2 = SchemaValidator::new(&enriched, ValidationFlags::default()).start_run(sink2);
    drive_buffer_document(&doc, &mut rt2, &enriched)?;
}
```

`drive_navigator` walks the tree in document order and reproduces the exact
runtime call sequence the streaming driver emits, so both passes yield
identical diagnostics from one parse. Buffering needs the `xsd11` feature (the
`document` module is gated on it); without it, the two-pass re-parse with
`drive_quick_xml` is the straightforward route.

For the enrichment mechanics themselves — how `enrich_schema_set` re-loads the
original schemas, the `load_hints_into_builder` / `SchemaSetBuilder` path, the
`EnrichmentOutcome` failure modes, and the base-URI resolution rule — see
[*Hint-Driven Schema Enrichment (Two-Pass Validation)*](OVERVIEW.md#hint-driven-schema-enrichment-two-pass-validation)
in the Architecture Overview.

## 3. XPath

The XPath engine is available under the `xsd11` feature, so enable that
feature on your `xsd-schema` dependency before using the `xpath` module.

Main types:

- `xpath::XPathContext` for static context
- `xpath::XPathExpr` for compiled expressions
- `xpath::XPathEvaluator` for builder-style evaluation
- `RoXmlNavigator` (or another `DomNavigator`) for navigating XML trees

Basic usage:

```rust
#[cfg(feature = "xsd11")]
fn run_xpath() -> Result<(), Box<dyn std::error::Error>> {
    use roxmltree::Document;
    use xsd_schema::namespace::table::NameTable;
    use xsd_schema::xpath::{RoXmlNavigator, XPathContext, XPathExpr};

    let xml = r#"
        <books>
            <book><title>The First Book</title></book>
            <book><title>The Second Book</title></book>
        </books>
    "#;

    let names = NameTable::new();
    let ctx = XPathContext::new(&names);
    let expr = XPathExpr::compile("//book/title", &ctx)?;

    let doc = Document::parse(xml)?;
    let result = expr
        .evaluator(&ctx)
        .run_with_node(RoXmlNavigator::new(&doc))?;

    assert_eq!(result.len(), 2);
    Ok(())
}
```

### DomNavigator

The XPath engine is generic over the `DomNavigator` trait — a read-only
cursor-style interface (modelled after C#'s `XPathNavigator`) that decouples
XPath evaluation from any specific DOM representation. Two implementations
are provided:

| Navigator | Backing store | Use when |
| --- | --- | --- |
| `RoXmlNavigator` | `roxmltree::Document` | You already have a parsed `roxmltree` tree and need XPath queries on it. Zero-copy, no extra allocation. |
| `BufferDocNavigator` | `document::BufferDocument` | You need schema-aware typed values in the DOM (XSD type annotations, typed atomization). Built via `BufferDocumentBuilder` during validation. Requires `xsd11`. |

You can also implement `DomNavigator` for your own DOM to plug into the
XPath engine directly.

### XPath 1.0 mode

The engine defaults to XPath 2.0. Use `XPathContext::with_mode(XPathMode::XPath1_0)`
to switch to XPath 1.0 syntax and semantics. In 1.0 mode the parser rejects
2.0-only constructs (`for`, `some`, `every`, `if`/`then`/`else`, `instance of`,
`cast as`, etc.), comparisons use 1.0 coercion rules (node-set → first node's
string value), and arithmetic always coerces to `number` (double). The same
compiled AST, evaluator, and function library are reused — mode selection only
restricts syntax and adjusts operator semantics.

### Common extensions

- `XPathContext::with_mode(...)` to switch between XPath 1.0 and 2.0 parsing
- `XPathContext::with_namespaces(...)` for prefix bindings
- `XPathContext::with_schema_set(...)` for schema-aware evaluation
- `XPathExpr::compile_with_vars(...)` plus `with_variable(...)` for external variables
- `run_bool`, `run_string`, `run_number`, `run_nodes` for convenient result coercion
- `run_with(...)` / `run_with_node_and_setup(...)` when variables need full `XPathValue` binding

## 4. Schema Resolving

There are two related layers:

1. Document resolution:
   loading referenced schemas from files, embedded assets, catalogs, or custom loaders.
2. Component resolution:
   resolving QName references inside the compiled schema model.

For document resolution, start with:

- `SchemaSetBuilder::with_loader(...)`
- `SchemaSetBuilder::with_config(...)`
- `SchemaResolver`
- `SchemaLoader`, `FileSystemLoader`, `EmbeddedLoader`, `LoaderChain`
- `SchemaCatalog`

Example with a custom loader chain:

```rust
use std::path::PathBuf;
use xsd_schema::{
    FileSystemLoader, LoaderChain, SchemaSetBuilder,
};

let mut loaders = LoaderChain::new();
loaders.add(Box::new(FileSystemLoader::with_base_dir(PathBuf::from("schemas"))));

let compiled = SchemaSetBuilder::with_loader(Box::new(loaders))
    .add("urn:books", "schemas/books.xsd")?
    .compile()?;
# let _ = compiled;
# Ok::<(), xsd_schema::SchemaError>(())
```

If you need low-level control, use `SchemaResolver` directly. That is useful
when you want to inspect `LoadOutcome`, resolve relative locations yourself, or
drive `load_schema()` manually. When using `SchemaResolver` directly, remember
to populate the standard XML catalog yourself:

```rust
use xsd_schema::{SchemaResolver, SchemaSet};

let mut resolver = SchemaResolver::new();
resolver.catalog_mut().add_xml_catalog();

let mut schema_set = SchemaSet::new();
let _outcome = resolver.load_schema("schemas/books.xsd", ".", &mut schema_set, None)?;
# Ok::<(), xsd_schema::SchemaError>(())
```

QName/component resolution is normally handled by the high-level pipeline, but
the public API is also available directly:

- `resolve_all_references(&mut schema_set)` for the whole schema set
- `schema::ReferenceResolver` for point lookups against a compiled `SchemaSet`

With the `async` feature, the corresponding async APIs are available via
`AsyncSchemaLoader`, `load_schema_async`, `load_and_process_schema_async`, and
the async builder methods.

## 5. Traversal And Schema Analysis

After full processing, the compiled schema model is intentionally open for
application-specific analysis. The main surfaces are:

- `schema_set.documents` for document-level metadata and directives
- `schema_set.namespaces` for global component indexes
- `schema_set.arenas` for the actual component records
- `schema_set.composition_edges` for include/import/redefine/override graph data
- `schema_set.effective_components` for the final post-composition view

Simple inspection example:

```rust
use xsd_schema::schema::build_dependency_graph;

fn inspect(schema_set: &xsd_schema::SchemaSet) -> Result<(), xsd_schema::SchemaError> {
    for doc in &schema_set.documents {
        let ns = doc
            .target_namespace
            .and_then(|id| schema_set.name_table.try_resolve(id))
            .unwrap_or_else(|| "(no namespace)".to_string());

        println!("document {} -> {}", doc.base_uri, ns);
    }

    for (ns_id, table) in &schema_set.namespaces {
        let ns = ns_id
            .and_then(|id| schema_set.name_table.try_resolve(id))
            .unwrap_or_else(|| "(no namespace)".to_string());

        for (name_id, element_key) in &table.elements {
            let local_name = schema_set.name_table.resolve(*name_id);
            let element = &schema_set.arenas.elements[*element_key];

            println!(
                "global element {{{}}}{} -> {:?}",
                ns,
                local_name,
                element.resolved_type
            );
        }
    }

    let (mut graph, _stats) = build_dependency_graph(schema_set)?;
    graph.sort()?;
    println!("types in dependency order: {}", graph.compilation_order().len());

    Ok(())
}
```

Useful patterns for applications:

- Use `SchemaSet::lookup_type`, `lookup_element`, `lookup_attribute`, and friends
  when you already have namespace/name IDs.
- Use `name_table.add(...)` and `name_table.resolve(...)` to move between
  strings and interned IDs.
- Inspect `schema_set.effective_components` when you need the final visible
  components after `xs:include`, `xs:redefine`, and `xs:override`.
- Use `build_dependency_graph(...)` when you need stable analysis or code
  generation order for derived types.
- Use `SchemaDocument::is_chameleon()` to detect chameleon-adopted documents.
  The distinction is captured by two fields: `declared_target_namespace` (the
  literal `targetNamespace` from the `<xs:schema>` element, `None` if absent)
  and `target_namespace` (effective namespace after chameleon adoption).
  `is_chameleon()` returns `true` when `declared` is `None` but `effective`
  is `Some`.

## 6. Regex Compatibility Mode

By default, pattern facets are validated against the strict XSD Part 2
regex grammar (§F for XSD 1.0, §G for XSD 1.1). Schemas authored against
.NET's `XmlValidatingReader` often contain constructs that are valid in
.NET's regex engine but outside XSD's grammar. The `RegexCompat` enum on
`SchemaSet` lets a caller opt into a permissive mode that mirrors what
.NET actually does: skip the strict grammar validation and let the
runtime regex engine decide what is well-formed.

```rust
use xsd_schema::{RegexCompat, SchemaSetBuilder};

let mut builder = SchemaSetBuilder::new();
builder.set_regex_compatibility(RegexCompat::LenientMs);
let compiled = builder.add("urn:ms-schema", "ms-authored.xsd")?.compile()?;
```

### What `LenientMs` does

`LenientMs` matches .NET's behaviour: no XSD
grammar enforcement; the pattern is handed to the regex engine after
the standard `\d`/`\D`/`\w`/`\W`/`\i`/`\I`/`\c`/`\C` substitutions.

Concretely, `LenientMs` does three things:

1. **Skips** `validate_xml_pattern_syntax` — the XSD 1.0 §F hyphen rule
   gate (rejects forms like `[a-c-1]`).
2. **Skips** `regexml::Regex::xsd(...)` — the strict §F/§G grammar gate
   (rejects XPath-only constructs).
3. **Strips** `(?#...)` comments textually before compilation, because
   neither Rust `regex` nor regexml's `xpath()` recognise that .NET
   syntax. Comments inside character classes are left alone.

After those three changes, the pattern is compiled with the same
runtime engine used in `Strict` mode (Rust `regex` for default features,
regexml `xpath()` for `xsd11`). The Unicode-3.0 `\p{X}` pinning for
XSD 1.0 schemas applies in both modes — that is a versioning rule, not
a grammar choice.

### Constructs unlocked under `LenientMs`

| Construct | Default features (Rust `regex`) | `xsd11` feature (regexml `xpath()`) |
| --- | --- | --- |
| `^`/`$` anchors outside char class | works | works |
| `(?:non-capturing)` groups | works | works |
| Reluctant quantifiers `*?`, `+?`, `??`, `{n,m}?` | works | works |
| Backreferences `\1`, `\2`… | engine gap — still rejects | **works** |
| Named groups `(?<name>…)` | works | works |
| `(?#…)` inline comments | stripped before compilation | stripped before compilation |

### When to use it

Use `LenientMs` when you have a .NET-source schema you cannot edit and
that uses constructs which are valid in .NET but outside XSD's grammar.
A "passing" schema under `LenientMs` is *not* guaranteed to be portable
to a strictly-conformant XSD validator — that is the trade-off.

The W3C XSD Test Suite is run exclusively in `Strict` mode. The
`msData/regex` group includes tests like `RegexTest_119`, `_929`,
`_930`, `_1413`, `_1414` that verify a strict validator rejects
`(?#...)` comments, and the `expected=Valid` tests in that group do
not exercise the constructs unlocked by `LenientMs` — so wiring the
flag into the harness would be dead code.

## Recommended Reading Order

If you are new to the crate, this sequence usually works well:

1. Start with `load_and_process_schema` or `SchemaSetBuilder`.
2. Add `SchemaValidator` if you need instance validation.
3. Add `XPathExpr` and `XPathContext` if you need XPath/XSD 1.1 features.
4. Move down to `SchemaResolver`, `ReferenceResolver`, and the raw schema model
   only when you need custom loading or custom analysis.
