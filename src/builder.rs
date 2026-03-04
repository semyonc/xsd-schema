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
    resolve_all_directives, ResolverConfig, SchemaLoader, SchemaResolver,
};
use crate::schema::model::XsdVersion;
use crate::schema::{assemble_inline_types, resolve_all_references, SchemaSet};

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
        let content = self.resolver.load_content(location)?;
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
        let doc_id = parse_schema_with_config(
            xml.as_bytes(),
            base_uri,
            &mut self.schema_set,
            &self.resolver.config.parser_config,
        )?;
        self.pending_docs.push(doc_id);
        self.schema_set.mark_loaded(base_uri.to_string(), doc_id);
        Ok(self)
    }

    /// Add a schema from bytes.
    ///
    /// # Arguments
    ///
    /// * `xml` - The schema XML content as bytes
    /// * `base_uri` - Base URI for resolving relative references
    pub fn add_bytes(mut self, xml: &[u8], base_uri: &str) -> SchemaResult<Self> {
        let doc_id = parse_schema_with_config(
            xml,
            base_uri,
            &mut self.schema_set,
            &self.resolver.config.parser_config,
        )?;
        self.pending_docs.push(doc_id);
        self.schema_set.mark_loaded(base_uri.to_string(), doc_id);
        Ok(self)
    }

    /// Get the number of schemas added so far.
    pub fn schema_count(&self) -> usize {
        self.pending_docs.len()
    }

    /// Compile all added schemas.
    ///
    /// This performs the following phases:
    /// 1. **Directive Resolution** - Process include/import/redefine/override directives
    /// 2. **Redefine/Override Application** - Apply component replacements
    /// 3. **Inline Type Assembly** - Materialize inline type definitions
    /// 4. **Reference Resolution** - Resolve QName references to component keys
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

        // Phase 2: Apply redefine/override semantics
        self.apply_redefine_override()?;

        // Phase 3: Inline type assembly
        let inline_stats = assemble_inline_types(&mut self.schema_set)?;

        // Phase 4: Reference resolution
        let resolution_stats = resolve_all_references(&mut self.schema_set)?;

        Ok(CompiledSchemaSet {
            schema_set: self.schema_set,
            stats: CompilationStats {
                documents_loaded: 0, // Will be set below
                inline_types_assembled: inline_stats.total_inline_types,
                types_resolved: resolution_stats.types_resolved,
                elements_resolved: resolution_stats.elements_resolved,
                attributes_resolved: resolution_stats.attributes_resolved,
                groups_resolved: resolution_stats.groups_resolved,
                attribute_groups_resolved: resolution_stats.attribute_groups_resolved,
            },
        })
        .map(|mut compiled| {
            compiled.stats.documents_loaded = compiled.schema_set.documents.len();
            compiled
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

    /// Apply redefine and override directives to the schema set.
    fn apply_redefine_override(&mut self) -> SchemaResult<()> {
        use crate::schema::redefine::apply_redefine;

        // Collect all redefine directives
        let redefines: Vec<_> = self
            .schema_set
            .documents
            .iter()
            .flat_map(|doc| doc.redefines.iter().cloned())
            .collect();

        // Apply redefines
        for redefine in redefines {
            apply_redefine(&mut self.schema_set, &redefine)?;
        }

        // Apply overrides (XSD 1.1 only)
        #[cfg(feature = "xsd11")]
        {
            use crate::schema::override_dir::apply_override;

            let overrides: Vec<_> = self
                .schema_set
                .documents
                .iter()
                .flat_map(|doc| doc.overrides.iter().cloned())
                .collect();

            for override_dir in overrides {
                apply_override(&mut self.schema_set, &override_dir)?;
            }
        }

        Ok(())
    }
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
