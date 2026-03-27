//! SchemaSet builder with compile() pattern
//!
//! Provides a fluent API for loading multiple schemas before compilation,
//! similar to .NET's XmlSchemaSet pattern.
//!
//! XSD version is set on the builder — the parser derives it automatically.
//! Use `SchemaSetBuilder::xsd11()` for XSD 1.1, `SchemaSetBuilder::new()` for XSD 1.0.
//!
//! # Example
//!
//! ```
//! use xsd_schema::SchemaSetBuilder;
//!
//! let compiled = SchemaSetBuilder::new()
//!     .add_source(r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
//!         <xs:element name="root" type="xs:string"/>
//!     </xs:schema>"#, "schema.xsd")
//!     .expect("parse failed")
//!     .compile()
//!     .expect("compile failed");
//!
//! println!("Loaded {} documents", compiled.stats.documents_loaded);
//! ```

use crate::error::{SchemaError, SchemaResult};
use crate::ids::DocumentId;
use crate::parser::parse::parse_schema_with_config;
use crate::parser::resolver::{
    resolve_all_directives, fixup_composition_edges, ResolverConfig, SchemaLoader, SchemaResolver,
};
#[cfg(feature = "async")]
use crate::parser::resolver::{resolve_all_directives_async, AsyncSchemaLoader};
use crate::pipeline::process_loaded_schemas;
use crate::schema::model::XsdVersion;
use crate::schema::SchemaSet;
use std::path::{Path, PathBuf};

/// Builder for creating compiled schema sets.
///
/// Implements the C# XmlSchemaSet pattern where schemas are added first,
/// then compiled together as a group.
///
/// # Example
///
/// ```
/// use xsd_schema::SchemaSetBuilder;
///
/// let compiled = SchemaSetBuilder::new()
///     .add_source(r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
///         <xs:element name="root" type="xs:string"/>
///     </xs:schema>"#, "inline.xsd")
///     .expect("parse failed")
///     .compile()
///     .expect("compile failed");
///
/// assert_eq!(compiled.stats.documents_loaded, 1);
/// ```
pub struct SchemaSetBuilder {
    schema_set: SchemaSet,
    resolver: SchemaResolver,
    pending_docs: Vec<DocumentId>,
    errors: Vec<SchemaError>,
}

impl SchemaSetBuilder {
    /// Create a new builder with default configuration.
    ///
    /// Uses the default loader chain (embedded + filesystem) and
    /// automatically adds the XML catalog for well-known namespaces.
    pub fn new() -> Self {
        let mut resolver = SchemaResolver::new();
        resolver.catalog_mut().add_xml_catalog();

        Self {
            schema_set: SchemaSet::new(),
            resolver,
            pending_docs: Vec::new(),
            errors: Vec::new(),
        }
    }

    /// Create builder with custom resolver configuration.
    pub fn with_config(config: ResolverConfig) -> Self {
        let mut resolver = SchemaResolver::with_config(config);
        resolver.catalog_mut().add_xml_catalog();

        Self {
            schema_set: SchemaSet::new(),
            resolver,
            pending_docs: Vec::new(),
            errors: Vec::new(),
        }
    }

    /// Create builder with custom loader.
    pub fn with_loader(loader: Box<dyn SchemaLoader>) -> Self {
        let mut resolver = SchemaResolver::with_loader(loader);
        resolver.catalog_mut().add_xml_catalog();

        Self {
            schema_set: SchemaSet::new(),
            resolver,
            pending_docs: Vec::new(),
            errors: Vec::new(),
        }
    }

    /// Create a builder configured for a specific XSD version.
    pub fn with_version(version: XsdVersion) -> Self {
        let mut resolver = SchemaResolver::new();
        resolver.catalog_mut().add_xml_catalog();

        Self {
            schema_set: SchemaSet::with_version(version),
            resolver,
            pending_docs: Vec::new(),
            errors: Vec::new(),
        }
    }

    /// Create a builder configured for XSD 1.1.
    pub fn xsd11() -> Self {
        Self::with_version(XsdVersion::V1_1)
    }

    /// Create a builder with a custom async loader for non-blocking I/O.
    ///
    /// The async loader is used by [`add_async`](SchemaSetBuilder::add_async)
    /// and [`compile_async`](SchemaSetBuilder::compile_async).
    #[cfg(feature = "async")]
    pub fn with_async_loader(loader: Box<dyn AsyncSchemaLoader>) -> Self {
        let mut resolver = SchemaResolver::with_async_loader(loader);
        resolver.catalog_mut().add_xml_catalog();

        Self {
            schema_set: SchemaSet::new(),
            resolver,
            pending_docs: Vec::new(),
            errors: Vec::new(),
        }
    }

    /// Add a schema by namespace and location.
    ///
    /// Matches the C# `XmlSchemaSet.Add(namespace, location)` pattern.
    /// The namespace parameter is for documentation/validation purposes;
    /// the actual namespace comes from the schema's targetNamespace attribute.
    ///
    /// # Arguments
    ///
    /// * `_namespace` - Expected namespace (for documentation; not enforced)
    /// * `location` - File path or URI to load the schema from
    ///
    /// # Example
    ///
    /// ```
    /// use xsd_schema::SchemaSetBuilder;
    ///
    /// let builder = SchemaSetBuilder::new()
    ///     .add("urn:books", "examples/books.xsd")
    ///     .expect("failed to load books.xsd");
    ///
    /// assert_eq!(builder.schema_count(), 1);
    /// ```
    pub fn add(mut self, _namespace: &str, location: &str) -> SchemaResult<Self> {
        self.try_add(location)?;
        Ok(self)
    }

    /// Add a schema by location without consuming the builder.
    ///
    /// Returns `Ok(true)` if the schema was freshly loaded, `Ok(false)` if
    /// it was already present (dedup). Returns `Err` on load/parse failure.
    ///
    /// The location is first normalized via the resolver so that relative
    /// and absolute forms of the same path are correctly deduplicated.
    pub fn try_add(&mut self, location: &str) -> SchemaResult<bool> {
        let normalized = normalize_loaded_location(&self.resolver, location, "");
        if self.schema_set.is_loaded(&normalized) {
            return Ok(false);
        }
        let content = self.resolver.load_content(&normalized)?;
        let doc_id = parse_schema_with_config(
            content.as_bytes(),
            &normalized,
            &mut self.schema_set,
            &self.resolver.config.parser_config,
        )?;
        self.pending_docs.push(doc_id);
        self.schema_set.mark_loaded(normalized, doc_id);
        Ok(true)
    }

    /// Add a schema by resolving a relative location against a base URI.
    ///
    /// Uses the builder's resolver for URI resolution (handles Windows
    /// paths, URL normalization, etc.). The resolved absolute URI is used
    /// for loading and dedup tracking.
    ///
    /// Returns `Ok(true)` if freshly loaded, `Ok(false)` if already present.
    pub fn try_add_relative(&mut self, location: &str, base_uri: &str) -> SchemaResult<bool> {
        let normalized = normalize_loaded_location(&self.resolver, location, base_uri);
        self.try_add(&normalized)
    }

    /// Add a schema from XML source string.
    ///
    /// # Arguments
    ///
    /// * `xml` - The schema XML content as a string
    /// * `base_uri` - Base URI for resolving relative references
    ///
    /// # Example
    ///
    /// ```
    /// use xsd_schema::SchemaSetBuilder;
    ///
    /// let builder = SchemaSetBuilder::new()
    ///     .add_source(r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
    ///         <xs:element name="root" type="xs:string"/>
    ///     </xs:schema>"#, "inline.xsd")
    ///     .expect("parse failed");
    ///
    /// assert_eq!(builder.schema_count(), 1);
    /// ```
    pub fn add_source(mut self, xml: &str, base_uri: &str) -> SchemaResult<Self> {
        let normalized = normalize_loaded_location(&self.resolver, base_uri, "");
        let doc_id = parse_schema_with_config(
            xml.as_bytes(),
            &normalized,
            &mut self.schema_set,
            &self.resolver.config.parser_config,
        )?;
        self.pending_docs.push(doc_id);
        self.schema_set.mark_loaded(normalized, doc_id);
        Ok(self)
    }

    /// Add a schema from bytes.
    ///
    /// # Arguments
    ///
    /// * `xml` - The schema XML content as bytes
    /// * `base_uri` - Base URI for resolving relative references
    pub fn add_bytes(mut self, xml: &[u8], base_uri: &str) -> SchemaResult<Self> {
        let normalized = normalize_loaded_location(&self.resolver, base_uri, "");
        let doc_id = parse_schema_with_config(
            xml,
            &normalized,
            &mut self.schema_set,
            &self.resolver.config.parser_config,
        )?;
        self.pending_docs.push(doc_id);
        self.schema_set.mark_loaded(normalized, doc_id);
        Ok(self)
    }

    /// Get the number of schemas added so far.
    pub fn schema_count(&self) -> usize {
        self.pending_docs.len()
    }

    /// Check if a schema location has already been loaded.
    pub fn is_loaded(&self, location: &str) -> bool {
        self.schema_set.is_loaded(location)
    }

    /// Compile all added schemas.
    ///
    /// This performs the following phases:
    /// 1. **Directive Resolution** - Process include/import/redefine/override directives
    /// 2. **Redefine/Override Application** - Apply component replacements
    /// 3. **Inline Type Assembly** - Materialize inline type definitions
    /// 4. **Reference Resolution** - Resolve QName references to component keys
    /// 5. **Particle Allocation** - Allocate element declarations for content particles
    ///
    /// # Returns
    ///
    /// A [`CompiledSchemaSet`] containing the fully processed schema set and
    /// compilation statistics.
    ///
    /// # Errors
    ///
    /// Returns an error if any phase fails (invalid schema, missing references, etc.)
    pub fn compile(mut self) -> SchemaResult<CompiledSchemaSet> {
        // Phase 1: Resolve directives for all pending documents
        // Collect into a temp vec to avoid borrow issues
        let pending: Vec<_> = self.pending_docs.drain(..).collect();
        for doc_id in pending {
            self.resolve_directives_recursive(doc_id)?;
        }

        // Fixup cycle edges now that all documents have been loaded
        fixup_composition_edges(&mut self.schema_set);

        // Phases 2-5: Delegate to the pipeline's shared processing function
        // (redefine/override, inline assembly, reference resolution, particle allocation)
        let (inline_stats, resolution_stats) = process_loaded_schemas(&mut self.schema_set)?;

        let documents_loaded = self.schema_set.documents.len();
        Ok(CompiledSchemaSet {
            schema_set: self.schema_set,
            stats: CompilationStats {
                documents_loaded,
                inline_types_assembled: inline_stats.total_inline_types,
                types_resolved: resolution_stats.types_resolved,
                elements_resolved: resolution_stats.elements_resolved,
                attributes_resolved: resolution_stats.attributes_resolved,
                groups_resolved: resolution_stats.groups_resolved,
                attribute_groups_resolved: resolution_stats.attribute_groups_resolved,
            },
        })
    }

    /// Resolve directives recursively for a document and any loaded dependencies.
    fn resolve_directives_recursive(&mut self, doc_id: DocumentId) -> SchemaResult<()> {
        let result = resolve_all_directives(doc_id, &mut self.resolver, &mut self.schema_set);

        // Recursively process newly loaded documents
        for loaded_id in result.loaded {
            self.resolve_directives_recursive(loaded_id)?;
        }

        // Collect errors (but don't fail immediately - continue processing)
        if !result.errors.is_empty() {
            self.errors.extend(result.errors);
        }

        Ok(())
    }

    /// Resolve directives recursively using async loading.
    #[cfg(feature = "async")]
    async fn resolve_directives_recursive_async(&mut self, doc_id: DocumentId) -> SchemaResult<()> {
        let result =
            resolve_all_directives_async(doc_id, &mut self.resolver, &mut self.schema_set).await;

        // Recursively process newly loaded documents
        for loaded_id in result.loaded {
            Box::pin(self.resolve_directives_recursive_async(loaded_id)).await?;
        }

        if !result.errors.is_empty() {
            self.errors.extend(result.errors);
        }

        Ok(())
    }

    /// Add a schema by namespace and location, loading content asynchronously.
    ///
    /// Async variant of [`add`](SchemaSetBuilder::add).
    #[cfg(feature = "async")]
    pub async fn add_async(mut self, _namespace: &str, location: &str) -> SchemaResult<Self> {
        let content = self.resolver.load_content_async(location).await?;
        let doc_id = parse_schema_with_config(
            content.as_bytes(),
            location,
            &mut self.schema_set,
            &self.resolver.config.parser_config,
        )?;
        self.pending_docs.push(doc_id);
        self.schema_set.mark_loaded(location.to_string(), doc_id);
        Ok(self)
    }

    /// Compile all added schemas using async directive resolution.
    ///
    /// Async variant of [`compile`](SchemaSetBuilder::compile). Only directive
    /// resolution (I/O) is async; all computation phases remain synchronous.
    #[cfg(feature = "async")]
    pub async fn compile_async(mut self) -> SchemaResult<CompiledSchemaSet> {
        // Phase 1: Resolve directives asynchronously for all pending documents
        let pending: Vec<_> = self.pending_docs.drain(..).collect();
        for doc_id in pending {
            self.resolve_directives_recursive_async(doc_id).await?;
        }

        // Fixup cycle edges now that all documents have been loaded
        fixup_composition_edges(&mut self.schema_set);

        // Phases 2-5: Delegate to the pipeline's shared processing function (sync)
        let (inline_stats, resolution_stats) = process_loaded_schemas(&mut self.schema_set)?;

        let documents_loaded = self.schema_set.documents.len();
        Ok(CompiledSchemaSet {
            schema_set: self.schema_set,
            stats: CompilationStats {
                documents_loaded,
                inline_types_assembled: inline_stats.total_inline_types,
                types_resolved: resolution_stats.types_resolved,
                elements_resolved: resolution_stats.elements_resolved,
                attributes_resolved: resolution_stats.attributes_resolved,
                groups_resolved: resolution_stats.groups_resolved,
                attribute_groups_resolved: resolution_stats.attribute_groups_resolved,
            },
        })
    }
}

fn normalize_loaded_location(resolver: &SchemaResolver, location: &str, base_uri: &str) -> String {
    let resolved = resolver
        .resolve_location(location, base_uri)
        .unwrap_or_else(|_| location.to_string());
    if is_absolute_location(&resolved) {
        return resolved;
    }

    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(_) => return resolved,
    };
    normalize_path(&cwd.join(&resolved))
        .to_string_lossy()
        .into_owned()
}

fn is_absolute_location(location: &str) -> bool {
    location.starts_with("http://")
        || location.starts_with("https://")
        || location.starts_with("file://")
        || Path::new(location).is_absolute()
        || (location.len() >= 2 && location.as_bytes().get(1) == Some(&b':'))
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();

    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                result.pop();
            }
            std::path::Component::CurDir => {}
            _ => result.push(component),
        }
    }

    result
}

impl Default for SchemaSetBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Compiled schema set ready for validation.
///
/// Contains the fully processed [`SchemaSet`] with all references resolved
/// and inline types assembled.
pub struct CompiledSchemaSet {
    /// The compiled schema set
    pub schema_set: SchemaSet,
    /// Compilation statistics
    pub stats: CompilationStats,
}

impl CompiledSchemaSet {
    /// Get a reference to the underlying schema set.
    pub fn schema_set(&self) -> &SchemaSet {
        &self.schema_set
    }

    /// Consume self and return the underlying schema set.
    pub fn into_schema_set(self) -> SchemaSet {
        self.schema_set
    }
}

/// Statistics from schema compilation.
#[derive(Debug, Default, Clone)]
pub struct CompilationStats {
    /// Number of schema documents loaded
    pub documents_loaded: usize,
    /// Number of inline types assembled
    pub inline_types_assembled: usize,
    /// Number of type references resolved
    pub types_resolved: usize,
    /// Number of element references resolved
    pub elements_resolved: usize,
    /// Number of attribute references resolved
    pub attributes_resolved: usize,
    /// Number of group references resolved
    pub groups_resolved: usize,
    /// Number of attribute group references resolved
    pub attribute_groups_resolved: usize,
}

impl CompilationStats {
    /// Get total number of references resolved
    pub fn total_references_resolved(&self) -> usize {
        self.types_resolved
            + self.elements_resolved
            + self.attributes_resolved
            + self.groups_resolved
            + self.attribute_groups_resolved
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builder_new() {
        let builder = SchemaSetBuilder::new();
        assert_eq!(builder.schema_count(), 0);
    }

    #[test]
    fn test_builder_add_source() {
        let xsd = r#"<?xml version="1.0" encoding="UTF-8"?>
            <xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root" type="xs:string"/>
            </xs:schema>"#;

        let builder = SchemaSetBuilder::new()
            .add_source(xsd, "test.xsd")
            .expect("Should parse schema");

        assert_eq!(builder.schema_count(), 1);
    }

    #[test]
    fn test_builder_compile() {
        let xsd = r#"<?xml version="1.0" encoding="UTF-8"?>
            <xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="person">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="name" type="xs:string"/>
                        </xs:sequence>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"#;

        let compiled = SchemaSetBuilder::new()
            .add_source(xsd, "test.xsd")
            .expect("Should parse schema")
            .compile()
            .expect("Should compile");

        assert_eq!(compiled.stats.documents_loaded, 1);
        assert!(compiled.stats.inline_types_assembled > 0);
    }

    #[test]
    fn test_builder_multiple_schemas() {
        let xsd1 = r#"<?xml version="1.0" encoding="UTF-8"?>
            <xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
                       targetNamespace="http://example.com/schema1">
                <xs:element name="item1" type="xs:string"/>
            </xs:schema>"#;

        let xsd2 = r#"<?xml version="1.0" encoding="UTF-8"?>
            <xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
                       targetNamespace="http://example.com/schema2">
                <xs:element name="item2" type="xs:int"/>
            </xs:schema>"#;

        let compiled = SchemaSetBuilder::new()
            .add_source(xsd1, "schema1.xsd")
            .expect("Should parse schema1")
            .add_source(xsd2, "schema2.xsd")
            .expect("Should parse schema2")
            .compile()
            .expect("Should compile");

        assert_eq!(compiled.stats.documents_loaded, 2);
    }

    #[test]
    fn test_compilation_stats() {
        let stats = CompilationStats {
            documents_loaded: 2,
            inline_types_assembled: 5,
            types_resolved: 10,
            elements_resolved: 8,
            attributes_resolved: 3,
            groups_resolved: 2,
            attribute_groups_resolved: 1,
        };

        assert_eq!(stats.total_references_resolved(), 24);
    }
}
