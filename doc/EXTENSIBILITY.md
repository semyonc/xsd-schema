# xsd-schema Extensibility Guide

This document covers the four main extensibility surfaces of the crate:
annotations/appinfo, schema document loaders, DOM navigation for XPath,
and custom XPath functions.

## Table of Contents

1. [Annotations and AppInfo](#1-annotations-and-appinfo)
2. [Document Resolution and Schema Loaders](#2-document-resolution-and-schema-loaders)
3. [DomNavigator -- Adding New DOM Model Support](#3-domnavigator----adding-new-dom-model-support)
4. [Custom XPath Functions](#4-custom-xpath-functions)

---

## 1. Annotations and AppInfo

### 1.1 Overview

XSD annotations provide two extensibility surfaces:

- **`xs:documentation`** -- human-readable text (with optional `xml:lang` and `source` attributes)
- **`xs:appinfo`** -- machine-readable content for tools (JAXB bindings, XBRL taxonomies, etc.)

Both can contain arbitrary mixed XML content. The crate preserves this content
as raw byte-range references (`XmlFragment`) into the original source document,
deferring interpretation to consumers.

### 1.2 Data Model

All annotation types live in `src/schema/annotation.rs`.

```text
Annotation
 +-- id: Option<String>
 +-- attributes: Vec<ForeignAttribute>    -- non-XSD attrs on <xs:annotation>
 +-- items: Vec<AnnotationItem>           -- ordered appinfo + documentation
 +-- source: Option<SourceRef>            -- source location

AnnotationItem = AppInfo(AppInfoElement) | Documentation(DocumentationElement)

AppInfoElement
 +-- source: Option<String>               -- @source URI
 +-- attributes: Vec<ForeignAttribute>
 +-- namespaces: NamespaceContextSnapshot  -- in-scope NS bindings at parse time
 +-- content: XmlFragment                  -- raw XML content reference
 +-- source_ref: Option<SourceRef>

DocumentationElement
 +-- source: Option<String>               -- @source URI
 +-- lang: Option<String>                 -- xml:lang
 +-- attributes: Vec<ForeignAttribute>
 +-- namespaces: NamespaceContextSnapshot
 +-- content: XmlFragment
 +-- source_ref: Option<SourceRef>

XmlFragment
 +-- doc_id: DocumentId                   -- which schema document
 +-- span: SourceSpan { start, end }      -- byte range in source
```

### 1.3 Foreign Attributes

Any attribute on a schema element whose namespace is neither XSD nor XSI is
classified as a "foreign attribute" and captured in a `ForeignAttribute`:

```text
ForeignAttribute
 +-- namespace: Option<NameId>
 +-- local_name: NameId
 +-- prefix: Option<NameId>
 +-- value: String
 +-- source: Option<SourceRef>
```

The helper `is_foreign_attribute(ns, xsd_ns, xsi_ns) -> bool` classifies
attributes. Unqualified (no-namespace) attributes are *not* foreign -- they are
XSD-defined attributes.

### 1.4 Implicit Annotations

When a schema element carries foreign attributes but has no explicit
`<xs:annotation>`, the parser creates an *implicit* annotation to hold them.
The function `merge_foreign_attributes()` handles this logic transparently in
frame `finish()` methods.

### 1.5 Where Annotations Are Stored

Annotations are stored on schema components in the `arenas` module
(`src/arenas.rs`). Each major component type has an
`annotation: Option<Annotation>` field:

| Component | Arena field |
|-----------|-------------|
| `SimpleTypeDefData` | `arenas.simple_types[key].annotation` |
| `ComplexTypeDefData` | `arenas.complex_types[key].annotation` |
| `ElementData` | `arenas.elements[key].annotation` |
| `AttributeData` | `arenas.attributes[key].annotation` |
| `ModelGroupDefData` | `arenas.model_groups[key].annotation` |
| `AttributeGroupData` | `arenas.attribute_groups[key].annotation` |
| `NotationData` | `arenas.notations[key].annotation` |
| `IdentityConstraintData` | `arenas.identity_constraints[key].annotation` |

### 1.6 Parsing Flow

Annotations are parsed by three parser frames in `src/parser/frames/annotations.rs`:

1. **`AnnotationFrame`** -- handles `<xs:annotation>`, collects child appinfo/documentation items
2. **`AppinfoFrame`** -- handles `<xs:appinfo>`, captures raw XML content via `XmlFragment`
3. **`DocumentationFrame`** -- handles `<xs:documentation>`, captures content + `xml:lang`

Key design decisions:

- **Content is NOT parsed** -- appinfo/documentation content is captured as a byte
  span (`XmlFragment`) referencing the original document. This avoids imposing a
  DOM model on arbitrary extension content.
- **Namespace context is preserved** -- the `NamespaceContextSnapshot` at parse time
  is saved on each appinfo/documentation element, enabling correct QName resolution
  when consumers later interpret the content.
- Child elements inside appinfo/documentation are skipped by the parser
  (`SkipFrame`), contributing only to the span range.

### 1.7 Accessing Annotations

#### Querying Annotation Data

```rust
use xsd_schema::schema::annotation::{Annotation, AnnotationItem};

fn inspect_element_annotation(schema_set: &xsd_schema::SchemaSet) {
    for (_, element) in &schema_set.arenas.elements {
        if let Some(ref ann) = element.annotation {
            // Iterate appinfo elements
            for appinfo in ann.appinfos() {
                println!("appinfo source={:?}", appinfo.source);
                let range = appinfo.content.byte_range();
                // Retrieve raw XML from source document bytes:
                // source_bytes[range.start..range.end]
            }

            // Iterate documentation elements
            for doc in ann.documentations() {
                println!("doc lang={:?} source={:?}", doc.lang, doc.source);
            }

            // Find documentation by language
            if let Some(en_doc) = ann.documentation_for_lang("en") {
                // ...
            }

            // Inspect foreign attributes
            for attr in &ann.attributes {
                println!("foreign attr: {}={}", attr.local_name.0, attr.value);
            }
        }
    }
}
```

#### Recovering Raw XML Content

Since `XmlFragment` stores byte offsets, you need the original source bytes:

```rust
fn get_appinfo_xml(
    schema_set: &xsd_schema::SchemaSet,
    appinfo: &xsd_schema::schema::annotation::AppInfoElement,
    source_bytes: &[u8],
) -> String {
    let range = appinfo.content.byte_range();
    String::from_utf8_lossy(&source_bytes[range]).to_string()
}
```

For interpreting the XML content, use the preserved `appinfo.namespaces` to
resolve any QNames found within the fragment.

### 1.8 Extension Patterns

**Custom AppInfo Processor** -- filter by a known namespace or `source` URI:

```rust
fn extract_jaxb_bindings(ann: &Annotation) -> Vec<String> {
    ann.appinfos()
        .filter(|a| a.source.as_deref() == Some("urn:jaxb:bindings"))
        .map(|a| {
            // Parse the XmlFragment content with the preserved namespace context
            // a.namespaces contains the bindings needed for QName resolution
            format!("JAXB binding at {:?}", a.content.byte_range())
        })
        .collect()
}
```

**Documentation Extraction** -- for code-generation tools:

```rust
fn get_best_documentation(ann: &Annotation, preferred_lang: &str) -> Option<String> {
    if let Some(doc) = ann.documentation_for_lang(preferred_lang) {
        return Some(format!("{:?}", doc.content.byte_range()));
    }
    ann.documentations().next().map(|d| format!("{:?}", d.content.byte_range()))
}
```

### 1.9 Limitations

1. **No DOM for annotation content** -- consumers must parse the raw XML themselves.
   Annotation content may use arbitrary vocabularies.
2. **Source retention mode matters** -- `LocationRetention::DropAll` makes spans
   unretrievable. Use `Retain` (default) for full access.
3. **Foreign attributes are namespace-qualified** -- can appear on any schema element,
   not just `<xs:annotation>`.
4. **Schema-level annotations** -- the `<xs:schema>` element itself can have
   annotations, stored in the schema document's metadata.

---

## 2. Document Resolution and Schema Loaders

### 2.1 Overview

Schema loading has two layers:

1. **Document resolution** -- locating and fetching schema content by URI
2. **Component resolution** -- resolving QName references within loaded schemas

Document resolution is driven by the `SchemaLoader` trait (sync) and
`AsyncSchemaLoader` trait (async feature). Multiple loaders are composed
via `LoaderChain`.

### 2.2 Architecture

```text
SchemaSetBuilder / load_and_process_schema()
        |
   SchemaResolver
        |
   +----+----+
   |         |
 SchemaCatalog   LoaderChain
   (ns->URI)      |
              +---+---+---+
              |       |       |
         EmbeddedLoader  FileSystemLoader  (YourCustomLoader)
         (embedded://)   (file paths)       (http://, s3://, etc.)
```

Key types in `src/parser/resolver.rs`:

| Type | Role |
|------|------|
| `SchemaLoader` | Trait: sync content loading from a URI |
| `AsyncSchemaLoader` | Trait: async content loading (feature `async`) |
| `FileSystemLoader` | Built-in: loads from local filesystem |
| `EmbeddedLoader` | Built-in: loads from `embedded://` URIs (compiled-in schemas) |
| `LoaderChain` | Composite: chains loaders by priority |
| `SchemaResolver` | Orchestrator: URI resolution, cycle detection, catalog lookup |
| `SchemaCatalog` | Namespace-to-URI mapping (XML catalog) |
| `ResolverConfig` | Configuration: base dir, network access, max depth |
| `LoadOutcome` | Result: `Loaded(id)`, `AlreadyLoaded(id)`, or `Cycle(uri)` |

### 2.3 The `SchemaLoader` Trait

```rust
pub trait SchemaLoader: Send + Sync + Debug {
    /// Load schema content from the given location.
    /// Returns the schema XML as a string.
    fn load(&self, location: &str) -> SchemaResult<String>;

    /// Check if this loader can handle the given location.
    /// Used by LoaderChain to find an appropriate loader.
    fn can_load(&self, location: &str) -> bool;

    /// Priority for loader chain (higher = checked first).
    /// Default is 0. EmbeddedLoader uses 100.
    fn priority(&self) -> i32 { 0 }
}
```

The three required decisions when implementing a loader:

1. **`can_load`** -- which URI schemes/patterns this loader handles
2. **`load`** -- how to fetch content (return XML as `String`)
3. **`priority`** -- order relative to other loaders (higher = tried first)

### 2.4 Implementing a Custom Loader

#### Example: HTTP Loader

```rust
use std::fmt::Debug;
use xsd_schema::error::{SchemaError, SchemaResult};
use xsd_schema::parser::resolver::SchemaLoader;

#[derive(Debug, Clone)]
pub struct HttpLoader {
    timeout: std::time::Duration,
}

impl HttpLoader {
    pub fn new(timeout: std::time::Duration) -> Self {
        Self { timeout }
    }
}

impl SchemaLoader for HttpLoader {
    fn load(&self, location: &str) -> SchemaResult<String> {
        let response = ureq::get(location)
            .timeout(self.timeout)
            .call()
            .map_err(|e| SchemaError::resolution(
                format!("HTTP request failed for '{}': {}", location, e)
            ))?;

        response.into_string().map_err(|e| SchemaError::resolution(
            format!("Failed to read response body for '{}': {}", location, e)
        ))
    }

    fn can_load(&self, location: &str) -> bool {
        location.starts_with("http://") || location.starts_with("https://")
    }

    fn priority(&self) -> i32 {
        50  // Between embedded (100) and filesystem (0)
    }
}
```

#### Example: Database Loader

```rust
#[derive(Debug)]
pub struct DatabaseLoader {
    connection_string: String,
}

impl SchemaLoader for DatabaseLoader {
    fn load(&self, location: &str) -> SchemaResult<String> {
        if let Some(schema_id) = location.strip_prefix("db://schemas/") {
            fetch_schema_from_db(&self.connection_string, schema_id)
                .map_err(|e| SchemaError::resolution(
                    format!("Database lookup failed for '{}': {}", schema_id, e)
                ))
        } else {
            Err(SchemaError::resolution(format!("Not a database URI: {}", location)))
        }
    }

    fn can_load(&self, location: &str) -> bool {
        location.starts_with("db://schemas/")
    }

    fn priority(&self) -> i32 { 200 }
}
```

### 2.5 Composing Loaders with LoaderChain

`LoaderChain` tries loaders in priority order (highest first). Loaders are
automatically sorted on `add()`.

```rust
use xsd_schema::{
    FileSystemLoader, EmbeddedLoader, LoaderChain, SchemaSetBuilder,
};

let mut loaders = LoaderChain::new();
loaders.add(Box::new(EmbeddedLoader::new()));           // priority 100
loaders.add(Box::new(HttpLoader::new(timeout)));         // priority 50
loaders.add(Box::new(FileSystemLoader::new()));          // priority 0

let compiled = SchemaSetBuilder::with_loader(Box::new(loaders))
    .add("urn:books", "schemas/books.xsd")?             // -> FileSystemLoader
    .add("urn:remote", "https://example.com/types.xsd")? // -> HttpLoader
    .compile()?;
```

`LoaderChain::with_defaults()` creates a chain with `EmbeddedLoader` + `FileSystemLoader`.

### 2.6 SchemaResolver

`SchemaResolver` orchestrates the full resolution pipeline:

1. **URI resolution** -- resolves relative URIs against base URI
2. **Catalog lookup** -- maps namespace URIs to schema locations via `SchemaCatalog`
3. **Duplicate detection** -- skips already-loaded locations (chameleon-aware)
4. **Cycle detection** -- detects circular `xs:include` chains (allowed, just skipped)
5. **Content loading** -- delegates to the loader chain
6. **Parsing** -- parses loaded content into the `SchemaSet`

#### Using SchemaResolver Directly

```rust
use xsd_schema::{SchemaResolver, SchemaSet, LoaderChain};

let mut loaders = LoaderChain::new();
loaders.add(Box::new(HttpLoader::new(timeout)));
loaders.add(Box::new(xsd_schema::FileSystemLoader::new()));
loaders.add(Box::new(xsd_schema::EmbeddedLoader::new()));

let mut resolver = SchemaResolver::with_loader(Box::new(loaders));
resolver.catalog_mut().add_xml_catalog();  // Add standard XML namespace mappings

// Optionally add custom catalog entries
resolver.catalog_mut().add("urn:my-types", "schemas/my-types.xsd");

let mut schema_set = SchemaSet::new();
let outcome = resolver.load_schema("schemas/main.xsd", ".", &mut schema_set, None)?;
```

#### ResolverConfig

```rust
let config = ResolverConfig {
    base_dir: Some(PathBuf::from("/schemas")),
    allow_network: true,          // Required for http:// URLs
    max_depth: 100,               // Maximum include nesting
    parser_config: ParserConfig::default(),
};
let resolver = SchemaResolver::with_config(config);
```

**Important**: `allow_network` defaults to `false`. HTTP/HTTPS URLs are rejected
unless this is explicitly enabled.

### 2.7 SchemaCatalog

The catalog provides namespace-to-location mapping, similar to XML Catalogs:

```rust
let mut catalog = resolver.catalog_mut();

// Standard XML namespaces (xml:lang, etc.) -> embedded schemas
catalog.add_xml_catalog();

// Custom mappings
catalog.add("http://www.w3.org/1999/xhtml", "schemas/xhtml.xsd");
catalog.add("urn:company:types", "https://schemas.company.com/types.xsd");
```

Catalog lookup happens in `process_import()` when `xs:import` has no
`schemaLocation` attribute -- the resolver looks up the namespace in the
catalog to find a location.

### 2.8 Async Loading (feature `async`)

The `AsyncSchemaLoader` trait enables truly non-blocking I/O:

```rust
#[cfg(feature = "async")]
pub trait AsyncSchemaLoader: Send + Sync + Debug {
    fn load_async(
        &self,
        location: &str,
    ) -> Pin<Box<dyn Future<Output = SchemaResult<String>> + Send + '_>>;

    fn can_load(&self, location: &str) -> bool;
}
```

#### Example: Async HTTP Loader

```rust
#[cfg(feature = "async")]
use xsd_schema::parser::resolver::AsyncSchemaLoader;

#[derive(Debug)]
struct AsyncHttpLoader;

#[cfg(feature = "async")]
impl AsyncSchemaLoader for AsyncHttpLoader {
    fn load_async(
        &self,
        location: &str,
    ) -> Pin<Box<dyn Future<Output = SchemaResult<String>> + Send + '_>> {
        let url = location.to_string();
        Box::pin(async move {
            let response = reqwest::get(&url).await
                .map_err(|e| SchemaError::resolution(format!("HTTP error: {}", e)))?;
            response.text().await
                .map_err(|e| SchemaError::resolution(format!("Body error: {}", e)))
        })
    }

    fn can_load(&self, location: &str) -> bool {
        location.starts_with("http://") || location.starts_with("https://")
    }
}
```

Wire it up via `SchemaResolver::with_async_loader()` or
`SchemaSetBuilder::with_async_loader()`.

### 2.9 Directive Processing

| Directive | Method | Chameleon? | Notes |
|-----------|--------|------------|-------|
| `xs:include` | `process_include()` | Yes | Same namespace; chameleon adoption for no-namespace schemas |
| `xs:import` | `process_import()` | No | Different namespace; catalog fallback if no `schemaLocation` |
| `xs:redefine` | `process_redefine()` | Yes | Like include + type redefinition |
| `xs:override` | (XSD 1.1) | Yes | Replaces redefine; uses override composition |

Chameleon namespace adoption: when an included schema has no `targetNamespace`,
it adopts the includer's namespace per XSD spec section 4.2.3.

### 2.10 Error Handling

All loader methods return `SchemaResult<String>`, using `SchemaError::resolution()`
for resolution-specific errors.

Non-fatal conditions:
- Already-loaded schemas: `LoadOutcome::AlreadyLoaded`
- Circular includes: `LoadOutcome::Cycle` (logged, not an error)
- Missing catalog entries with no `schemaLocation`: `Ok(None)` from `process_import`

Fatal conditions:
- Loader returns an error (file not found, HTTP failure, etc.)
- Parse error in loaded content
- Network access attempted when `allow_network` is false

---

## 3. DomNavigator -- Adding New DOM Model Support

### 3.1 Overview

The XPath engine is fully generic over the `DomNavigator` trait -- a cursor-based
read-only interface modeled after C#'s `XPathNavigator`. This decouples XPath
evaluation from any specific DOM implementation.

Two implementations ship with the crate:

| Navigator | Module | Backing Store | Typed Values |
|-----------|--------|---------------|--------------|
| `RoXmlNavigator` | `src/navigator/roxmltree.rs` | `roxmltree::Document` | No (always `Untyped`) |
| `BufferDocNavigator` | `src/document/navigator.rs` | `BufferDocument` | Yes (schema-aware) |

### 3.2 The `DomNavigator` Trait

Defined in `src/navigator/mod.rs`:

```rust
pub trait DomNavigator: Clone {
    // --- Node identity and order ---
    fn is_same_position(&self, other: &Self) -> bool;
    fn compare_position(&self, other: &Self) -> XmlNodeOrder;
    fn move_to(&mut self, other: &Self) -> bool;

    // --- Navigation ---
    fn move_to_root(&mut self);
    fn move_to_parent(&mut self) -> bool;
    fn move_to_first_child(&mut self) -> bool;
    fn move_to_next_sibling(&mut self) -> bool;
    fn move_to_prev_sibling(&mut self) -> bool;
    fn move_to_first_attribute(&mut self) -> bool;
    fn move_to_next_attribute(&mut self) -> bool;
    fn move_to_first_namespace(&mut self, scope: NamespaceAxisScope) -> bool;
    fn move_to_next_namespace(&mut self, scope: NamespaceAxisScope) -> bool;
    fn move_to_following(&mut self, kind: DomNodeType, end: Option<&Self>) -> bool;

    // --- Node information ---
    fn node_type(&self) -> DomNodeType;
    fn local_name(&self) -> &str;
    fn name(&self) -> &str;              // prefix:local (or just local)
    fn namespace_uri(&self) -> &str;
    fn prefix(&self) -> &str;
    fn value(&self) -> String;
    fn base_uri(&self) -> &str;

    // --- Typed value hooks ---
    fn schema_type(&self) -> Option<SimpleTypeKey>;
    fn typed_value(&self) -> TypedValue;

    // --- Default helper methods (override-optional) ---
    fn has_attributes(&mut self) -> bool;
    fn has_children(&mut self) -> bool;
    fn move_to_child_kind(&mut self, kind: DomNodeType) -> bool;
    fn move_to_child_name(&mut self, local: &str, ns: &str) -> bool;
    fn find_element_by_id(&self, id: &str) -> Result<Option<Self>, NavigatorError>;
}
```

The trait requires `Clone` because the XPath engine creates checkpoint clones
for iterator branching, predicate evaluation, and ancestor-axis traversal.
Clone should be cheap -- typically copying a cursor index or pointer.

### 3.3 Supporting Types

```rust
pub enum DomNodeType {
    Root, Element, Attribute, Namespace,
    Text, Whitespace, SignificantWhitespace,
    Comment, ProcessingInstruction, All,
}

pub enum XmlNodeOrder { Before, After, Same, Unknown }

pub enum NamespaceAxisScope {
    All,          // All in-scope namespaces (including inherited)
    Local,        // Only locally declared namespaces
    ExcludeXml,   // All except the xml namespace
}

pub enum TypedValue {
    Value(XmlValue),   // Schema-validated typed value
    Untyped,           // No schema -- atomizes to xs:untypedAtomic
    Nilled,            // xsi:nil="true" -- empty sequence
    Absent,            // Element-only complex content (FOTY0012)
}
```

### 3.4 Implementing a Custom Navigator

1. **Define your cursor struct** implementing `Clone`
2. **Implement all required trait methods**
3. **Use it with `XPathExpr` and `XPathContext`**

#### Example: Navigator over a Custom DOM

```rust
use xsd_schema::navigator::{
    DomNavigator, DomNodeType, NamespaceAxisScope,
    NavigatorError, TypedValue, XmlNodeOrder,
};
use xsd_schema::ids::SimpleTypeKey;

struct MyNode { /* ... */ }
struct MyDocument { nodes: Vec<MyNode> }

#[derive(Clone)]
pub struct MyNavigator<'a> {
    doc: &'a MyDocument,
    cursor: usize,
    on_attribute: bool,
    attr_index: usize,
}

impl<'a> MyNavigator<'a> {
    pub fn new(doc: &'a MyDocument) -> Self {
        Self { doc, cursor: 0, on_attribute: false, attr_index: 0 }
    }
}

impl<'a> DomNavigator for MyNavigator<'a> {
    fn is_same_position(&self, other: &Self) -> bool {
        std::ptr::eq(self.doc, other.doc)
            && self.cursor == other.cursor
            && self.on_attribute == other.on_attribute
            && self.attr_index == other.attr_index
    }

    fn compare_position(&self, other: &Self) -> XmlNodeOrder {
        if !std::ptr::eq(self.doc, other.doc) {
            return XmlNodeOrder::Unknown;
        }
        match self.cursor.cmp(&other.cursor) {
            std::cmp::Ordering::Less => XmlNodeOrder::Before,
            std::cmp::Ordering::Greater => XmlNodeOrder::After,
            std::cmp::Ordering::Equal => XmlNodeOrder::Same,
        }
    }

    fn move_to(&mut self, other: &Self) -> bool {
        self.cursor = other.cursor;
        self.on_attribute = other.on_attribute;
        self.attr_index = other.attr_index;
        true
    }

    // ... implement remaining methods ...

    // For an untyped DOM, the typed value hooks are simple:
    fn schema_type(&self) -> Option<SimpleTypeKey> { None }
    fn typed_value(&self) -> TypedValue { TypedValue::Untyped }
}
```

#### Using the Custom Navigator

```rust
use xsd_schema::xpath::{XPathContext, XPathExpr};
use xsd_schema::namespace::table::NameTable;

let doc = MyDocument::parse("<root><item>hello</item></root>");
let nav = MyNavigator::new(&doc);

let names = NameTable::new();
let ctx = XPathContext::new(&names);
let expr = XPathExpr::compile("//item/text()", &ctx)?;

let result = expr.evaluator(&ctx)
    .run_with_node(nav)?;
```

### 3.5 Implementation Contracts

#### Cursor Navigation Rules

1. **Attributes are virtual children** -- `move_to_first_attribute()` enters the
   attribute axis. From an attribute, `move_to_parent()` returns to the owning
   element. Sibling navigation (`move_to_next_sibling`) does not visit attributes.

2. **Namespaces are virtual children** -- `move_to_first_namespace()` enters the
   namespace axis. Namespace nodes exist conceptually but may not correspond to
   any real node in your DOM.

3. **Navigation returns `bool`** -- `false` means "no such node; cursor unchanged."
   This is critical: on failure, the cursor must remain at its previous position.

4. **`move_to_root()`** always succeeds and positions at the document root.

5. **`move_to_following(kind, end)`** -- advances to the next node in the
   following axis (excludes descendants). If `end` is provided, stop before
   reaching that position. Return `false` and leave cursor unchanged if no
   such node exists.

#### String Value Rules (XPath Data Model)

- **Element**: concatenation of all descendant text nodes
- **Attribute**: attribute value
- **Text**: the text content
- **Root**: concatenation of all descendant text nodes
- **Namespace**: the namespace URI
- **Comment**: comment text (without `<!--` and `-->`)
- **PI**: PI content (without `<?target` and `?>`)

#### Document Order

`compare_position()` must implement XPath document order:
1. Document root first
2. Elements before their attributes
3. Attributes before the element's children
4. Namespace nodes before attributes (if supported)
5. Children in document order

#### Performance Considerations

- **Clone must be O(1)** -- the XPath engine clones navigators frequently during
  axis iteration, predicate evaluation, and path step processing.
- **`value()` returns `String`** -- the only allocation in the trait. For elements,
  concatenating descendant text can be expensive. Cache if needed.
- **`local_name()`, `namespace_uri()`, `prefix()` return `&str`** -- must be
  borrowable from the navigator. String interning or arena allocation helps.

### 3.6 Existing Implementations as Reference

**RoXmlNavigator** (`src/navigator/roxmltree.rs`) -- the simpler implementation.
Uses a three-state cursor enum (`Node`, `Attribute { owner, index }`,
`Namespace { owner, index, namespaces }`). Namespace list is computed lazily.
All `&str` returns borrow directly from the roxmltree document (zero-copy).
`typed_value()` always returns `TypedValue::Untyped`.

**BufferDocNavigator** (`src/document/navigator.rs`) -- the schema-aware
implementation. Uses a flat node-array cursor (`current: u32`) with virtual
attribute/namespace states. Typed values come from schema validation results.
`find_element_by_id()` uses the element ID index built during validation.

Use `RoXmlNavigator` as a starting template for untyped DOMs, and
`BufferDocNavigator` as reference for schema-aware implementations.

---

## 4. Custom XPath Functions

### 4.1 Overview

The XPath function system uses two traits to separate concerns:

- **`FunctionCatalog`** -- bind-time: looks up functions by `(namespace, name, arity)`,
  returns an opaque `FunctionHandle`
- **`FunctionEvaluator`** -- eval-time: dispatches a `FunctionHandle` with arguments,
  returns a result

The default implementations (`BuiltinCatalog`, `BuiltinEvaluator`) provide all
standard XPath 2.0 functions. Custom functions are added via `FunctionSet`.

### 4.2 Architecture

```text
                    Bind-time                       Eval-time
                    ---------                       ---------
XPathContext                                DynamicContext
  .function_catalog()                         .function_evaluator()
        |                                              |
   FunctionCatalog::lookup()                  FunctionEvaluator::eval()
        |                                              |
   FunctionHandle (opaque u32)                  FunctionHandle -> dispatch
        |                                              |
  stored in AST node                           result: XPathValue<N>
```

Key types in `src/xpath/functions/extensible.rs`:

| Type | Role |
|------|------|
| `FunctionHandle` | Opaque identifier (u32); built-in < `0x1000_0000`, custom >= `0x1000_0000` |
| `FunctionCatalog` | Trait: bind-time lookup by `(namespace, name, arity)` |
| `FunctionEvaluator` | Trait: eval-time dispatch by `FunctionHandle` |
| `BuiltinCatalog` | Default catalog: wraps the static `FUNCTION_REGISTRY` |
| `BuiltinEvaluator` | Default evaluator: routes to `eval_function` |
| `FunctionSet<N>` | Combined catalog + evaluator with custom function support |
| `DynamicFunctionSignature` | Owned signature for external registration |
| `CustomFn<N>` | Type alias for custom function closures |
| `XPath10Catalog` | Restricts lookup to the 27 XPath 1.0 core functions |
| `XPath10Evaluator` | Wraps built-in functions with 1.0 semantics |

### 4.3 FunctionHandle

```rust
pub struct FunctionHandle(pub(crate) u32);

impl FunctionHandle {
    pub fn is_builtin(&self) -> bool;  // value < 0x1000_0000
    pub fn is_custom(&self) -> bool;   // value >= 0x1000_0000
}
```

Built-in handles map directly to `FunctionId` discriminant values. Custom handles
are offset by `CUSTOM_HANDLE_BASE` (0x1000_0000) plus their index in `FunctionSet`.

### 4.4 Registering Custom Functions

`FunctionSet<N>` is the primary API. It implements both `FunctionCatalog` and
`FunctionEvaluator<N>`, so a single object provides both bind-time and eval-time support.

```rust
use xsd_schema::xpath::functions::{
    FunctionSet, DynamicFunctionSignature, XPathValue,
};
use xsd_schema::types::sequence::SequenceType;

let mut functions: FunctionSet<RoXmlNavigator<'_>> = FunctionSet::with_builtins();

let sig = DynamicFunctionSignature::new(
    "http://example.com/ext",      // namespace URI
    "double-value",                // local name
    vec![SequenceType::double()],  // parameter types
    SequenceType::double(),        // return type
);

functions.register(sig, |_ctx, mut args| {
    let val = args.remove(0);
    let d = val.as_f64().unwrap_or(0.0);
    Ok(XPathValue::double(d * 2.0))
});
```

#### DynamicFunctionSignature

Unlike built-in `FunctionSignature` (which uses `&'static str`),
`DynamicFunctionSignature` owns its strings via `Arc<str>`:

```rust
pub struct DynamicFunctionSignature {
    pub namespace: Arc<str>,
    pub local_name: Arc<str>,
    pub arity: FunctionArity,
    pub param_types: Vec<SequenceType>,
    pub return_type: SequenceType,
}
```

Constructors:

```rust
DynamicFunctionSignature::new(ns, name, params, return_type)       // exact arity
DynamicFunctionSignature::range(ns, name, min, max, params, ret)   // range arity
DynamicFunctionSignature::variadic(ns, name, min_args, params, ret) // variadic
```

#### FunctionArity

```rust
pub enum FunctionArity {
    Exact(usize),            // Fixed arg count
    Range(usize, usize),     // Min..=Max
    Variadic(usize),         // Min.. (no max)
}
```

### 4.5 Wiring Custom Functions into XPath

Custom functions need to be connected at two points:

1. **Bind-time** -- via `XPathContext::with_function_catalog()`
2. **Eval-time** -- via `DynamicContext::with_function_evaluator()`

Since `FunctionSet` implements both traits, a single object serves both:

```rust
use xsd_schema::xpath::{XPathContext, XPathExpr, RoXmlNavigator};
use xsd_schema::namespace::table::NameTable;
use xsd_schema::namespace::context::NamespaceContextSnapshot;

// 1. Create and populate function set
let mut functions: FunctionSet<RoXmlNavigator<'_>> = FunctionSet::with_builtins();
let sig = DynamicFunctionSignature::new(
    "http://example.com/ext",
    "greet",
    vec![SequenceType::string()],
    SequenceType::string(),
);
functions.register(sig, |_ctx, mut args| {
    let name = args.remove(0).as_string().unwrap_or_default();
    Ok(XPathValue::string(format!("Hello, {}!", name)))
});

// 2. Create context with the function catalog
let names = NameTable::new();

let ext_ns = names.add("http://example.com/ext");
let ext_prefix = names.add("ext");
let ns_bindings = NamespaceContextSnapshot {
    default_ns: None,
    bindings: vec![(ext_prefix, ext_ns)],
};

let ctx = XPathContext::new(&names)
    .with_namespaces(ns_bindings)
    .with_function_catalog(&functions);  // bind-time lookup

// 3. Compile expression using custom function
let expr = XPathExpr::compile("ext:greet('World')", &ctx)?;

// 4. Evaluate with the function evaluator
let doc = roxmltree::Document::parse("<root/>")?;
let nav = RoXmlNavigator::new(&doc);

let result = expr.evaluator(&ctx)
    .run_with_node_and_setup(nav, |dyn_ctx| {
        dyn_ctx.with_function_evaluator(&functions)  // eval-time dispatch
    })?;
// result contains XPathValue::string("Hello, World!")
```

### 4.6 The CustomFn Type

```rust
pub type CustomFn<N> = Arc<
    dyn Fn(
        &mut DynamicContext<'_, N>,
        Vec<XPathValue<N>>,
    ) -> Result<XPathValue<N>, XPathError>
        + Send
        + Sync,
>;
```

The closure receives:
- **`ctx`** -- access to context item, position, variables, schema set, base URI
- **`args`** -- already-evaluated argument values

#### Working with XPathValue

```rust
// Constructors
XPathValue::string("hello")
XPathValue::double(3.14)
XPathValue::integer(42)
XPathValue::boolean(true)
XPathValue::empty()  // empty sequence

// Extractors
value.as_string() -> Option<String>
value.as_f64() -> Option<f64>
value.as_integer() -> Option<BigInt>
value.as_bool() -> Option<bool>
value.len() -> usize
value.is_empty() -> bool
```

#### Accessing Context in Custom Functions

```rust
functions.register(sig, |ctx, args| {
    let item = ctx.require_context_item()?;       // Current context item
    let pos = ctx.context_position;                // Position (1-based)
    let size = ctx.context_size;                   // Context size
    let base = ctx.base_uri.as_deref().unwrap_or(""); // Base URI

    if let Some(schema_set) = ctx.static_context.schema_set {
        // Access schema information
    }

    Ok(XPathValue::string("result"))
});
```

#### Error Handling

Custom functions return `Result<XPathValue<N>, XPathError>`. Use XPath error codes:

```rust
use xsd_schema::xpath::error::XPathError;

functions.register(sig, |_ctx, args| {
    let val = args[0].as_f64().ok_or_else(|| {
        XPathError::XPTY0004 {
            expected: "xs:double".to_string(),
            found: "non-numeric value".to_string(),
        }
    })?;

    if val < 0.0 {
        return Err(XPathError::FOAR0002 {
            message: "Value must be non-negative".to_string(),
        });
    }

    Ok(XPathValue::double(val.sqrt()))
});
```

### 4.7 Overriding Built-in Functions

Custom functions registered in `FunctionSet` take precedence over built-in
functions with the same namespace/name/arity:

```rust
let sig = DynamicFunctionSignature::new(
    "http://www.w3.org/2005/xpath-functions",  // fn: namespace
    "string-length",
    vec![SequenceType::string_optional()],
    SequenceType::integer(),
);
functions.register(sig, |_ctx, mut args| {
    let s = args.remove(0).as_string().unwrap_or_default();
    Ok(XPathValue::integer(s.len() as i64))
});
```

### 4.8 Function Registration Summary

| Component | Role | Trait |
|-----------|------|-------|
| `FunctionSet::register()` | Register custom function | -- |
| `XPathContext::with_function_catalog()` | Wire catalog for binding | `FunctionCatalog` |
| `DynamicContext::with_function_evaluator()` | Wire evaluator for execution | `FunctionEvaluator<N>` |

### 4.9 Built-in Function Registry

The global `FUNCTION_REGISTRY` (`src/xpath/functions/registry.rs`) contains all
~70 built-in XPath 2.0 functions, organized by category:

- Boolean (true, false, not, boolean)
- Context (position, last)
- Sequence (count, empty, exists, reverse, distinct-values, etc.)
- Aggregate (sum, avg, min, max)
- String (concat, substring, contains, starts-with, etc.)
- Numeric (abs, ceiling, floor, round, round-half-to-even)
- Node (name, local-name, namespace-uri, nilled, root, id, lang, etc.)
- DateTime (current-dateTime, year-from-dateTime, etc.)
- QName (resolve-QName, QName, prefix-from-QName, etc.)
- URI (resolve-uri, static-base-uri)
- Regex (matches, replace, tokenize)
- Special (trace, data, default-collation)
- Conversion (string, number)

The registry supports lookup by namespace alias (`FN_2010_NAMESPACE` falls back
to `FN_NAMESPACE`).
