//! Schema resolution for include, import, and redefine directives
//!
//! This module handles resolving and loading external schema documents
//! referenced by `xs:include`, `xs:import`, and `xs:redefine` directives.
//!
//! # Resolution Process
//!
//! 1. **Include** - Same target namespace, required schemaLocation
//!    - Loads the referenced schema
//!    - Merges components into the same namespace
//!    - Supports chameleon includes (no targetNamespace)
//!
//! 2. **Import** - Different namespace, optional schemaLocation
//!    - Loads schema for the specified namespace
//!    - Components remain in their declared namespace
//!    - Without schemaLocation, relies on catalog or pre-loaded schemas
//!
//! 3. **Redefine** - Same namespace, extends/restricts existing types
//!    - Deprecated in XSD 1.1 (use override instead)
//!    - Allows redefining types/groups from included schema
//!
//! # Circular Dependencies
//!
//! The resolver tracks loaded schema locations to:
//! - Detect circular includes (allowed, just skip)
//! - Prevent infinite loops
//! - Enable caching of resolved schemas
//!
//! # URI Resolution
//!
//! The resolver supports:
//! - Absolute file paths
//! - Relative paths (resolved against base URI)
//! - HTTP/HTTPS URLs (via async trait)
//! - Catalog-based resolution
//!
//! # Customizable Loading
//!
//! The [`SchemaLoader`] trait allows custom loading strategies:
//! - [`FileSystemLoader`] - Loads from local file system
//! - [`EmbeddedLoader`] - Loads from embedded static assets
//! - [`LoaderChain`] - Combines multiple loaders with priority

use std::collections::HashSet;
use std::fmt::Debug;
use std::path::{Path, PathBuf};

use crate::error::{SchemaError, SchemaResult};
use crate::ids::{DocumentId, NameId};
use crate::parser::parse::{parse_schema_with_config, ParserConfig};
use crate::SchemaSet;

// ============================================================================
// SchemaLoader Trait and Implementations
// ============================================================================

/// Trait for loading schema content from various sources.
///
/// Implementations can support file systems, HTTP, embedded resources, etc.
/// The loader chain uses priority to determine which loader handles a request.
pub trait SchemaLoader: Send + Sync + Debug {
    /// Load schema content from the given location.
    ///
    /// Returns the schema content as a string, or an error if loading fails.
    fn load(&self, location: &str) -> SchemaResult<String>;

    /// Check if this loader can handle the given location.
    ///
    /// Used by [`LoaderChain`] to find an appropriate loader.
    fn can_load(&self, location: &str) -> bool;

    /// Priority for loader chain (higher = checked first).
    ///
    /// Default is 0. Embedded loader uses 100 to be checked before file system.
    fn priority(&self) -> i32 {
        0
    }
}

/// File system schema loader (default).
///
/// Loads schemas from local file system paths.
#[derive(Debug, Clone, Default)]
pub struct FileSystemLoader {
    /// Base directory for resolving relative paths (not currently used directly)
    pub base_dir: Option<PathBuf>,
}

impl FileSystemLoader {
    /// Create a new file system loader.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a file system loader with a base directory.
    pub fn with_base_dir(base_dir: PathBuf) -> Self {
        Self {
            base_dir: Some(base_dir),
        }
    }
}

impl SchemaLoader for FileSystemLoader {
    fn load(&self, location: &str) -> SchemaResult<String> {
        let path = Path::new(location);
        std::fs::read_to_string(path).map_err(|e| {
            SchemaError::resolution(format!("Failed to read file '{}': {}", location, e))
        })
    }

    fn can_load(&self, location: &str) -> bool {
        !location.starts_with("http://")
            && !location.starts_with("https://")
            && !location.starts_with("embedded://")
    }

    fn priority(&self) -> i32 {
        0
    }
}

/// Embedded resource loader for built-in schemas.
///
/// Loads schemas from static assets embedded in the binary using the
/// `embedded://` URI scheme.
#[derive(Debug, Clone, Default)]
pub struct EmbeddedLoader;

impl EmbeddedLoader {
    /// Create a new embedded loader.
    pub fn new() -> Self {
        Self
    }
}

impl SchemaLoader for EmbeddedLoader {
    fn load(&self, location: &str) -> SchemaResult<String> {
        if let Some(rest) = location.strip_prefix("embedded://") {
            match rest {
                "xml.xsd" => {
                    let bytes = crate::embedded::XML_XSD;
                    String::from_utf8(bytes.to_vec()).map_err(|e| {
                        SchemaError::resolution(format!("Invalid UTF-8 in embedded schema: {}", e))
                    })
                }
                _ => Err(SchemaError::resolution(format!(
                    "Unknown embedded schema: {}",
                    rest
                ))),
            }
        } else {
            Err(SchemaError::resolution(format!(
                "Not an embedded location: {}",
                location
            )))
        }
    }

    fn can_load(&self, location: &str) -> bool {
        location.starts_with("embedded://")
    }

    fn priority(&self) -> i32 {
        100 // High priority - checked before file system
    }
}

/// Composite loader that chains multiple loaders.
///
/// Loaders are tried in priority order (highest first) until one can handle
/// the requested location.
#[derive(Debug, Default)]
pub struct LoaderChain {
    loaders: Vec<Box<dyn SchemaLoader>>,
}

impl LoaderChain {
    /// Create a new empty loader chain.
    pub fn new() -> Self {
        Self {
            loaders: Vec::new(),
        }
    }

    /// Create a loader chain with default loaders (embedded + filesystem).
    pub fn with_defaults() -> Self {
        let mut chain = Self::new();
        chain.add(Box::new(EmbeddedLoader::new()));
        chain.add(Box::new(FileSystemLoader::new()));
        chain
    }

    /// Add a loader to the chain.
    ///
    /// Loaders are automatically sorted by priority (highest first).
    pub fn add(&mut self, loader: Box<dyn SchemaLoader>) {
        self.loaders.push(loader);
        self.loaders
            .sort_by_key(|b| std::cmp::Reverse(b.priority()));
    }

    /// Get the number of loaders in the chain.
    pub fn len(&self) -> usize {
        self.loaders.len()
    }

    /// Check if the chain is empty.
    pub fn is_empty(&self) -> bool {
        self.loaders.is_empty()
    }
}

impl SchemaLoader for LoaderChain {
    fn load(&self, location: &str) -> SchemaResult<String> {
        for loader in &self.loaders {
            if loader.can_load(location) {
                return loader.load(location);
            }
        }
        Err(SchemaError::resolution(format!(
            "No loader available for: {}",
            location
        )))
    }

    fn can_load(&self, location: &str) -> bool {
        self.loaders.iter().any(|l| l.can_load(location))
    }

    fn priority(&self) -> i32 {
        // Chain priority is max of all loaders
        self.loaders.iter().map(|l| l.priority()).max().unwrap_or(0)
    }
}

// ============================================================================
// Schema Resolver
// ============================================================================

/// Schema resolver for loading external schema documents.
///
/// Uses a [`SchemaLoader`] chain to support multiple loading strategies
/// (file system, embedded assets, HTTP, etc.).
pub struct SchemaResolver {
    /// Configuration for resolution
    pub config: ResolverConfig,
    /// Set of locations currently being resolved (for cycle detection)
    resolving: HashSet<String>,
    /// Catalog for namespace-to-location mapping
    catalog: SchemaCatalog,
    /// Schema loader chain
    loader: Box<dyn SchemaLoader>,
}

/// Configuration for schema resolution
#[derive(Debug, Clone)]
pub struct ResolverConfig {
    /// Base directory for resolving relative paths
    pub base_dir: Option<PathBuf>,
    /// Whether to allow network access for HTTP URLs
    pub allow_network: bool,
    /// Maximum depth for nested includes
    pub max_depth: usize,
    /// Parser configuration to use for resolved schemas
    pub parser_config: ParserConfig,
}

impl Default for ResolverConfig {
    fn default() -> Self {
        Self {
            base_dir: None,
            allow_network: false,
            max_depth: 100,
            parser_config: ParserConfig::default(),
        }
    }
}

/// Catalog for mapping namespaces to schema locations
#[derive(Debug, Clone, Default)]
pub struct SchemaCatalog {
    /// Namespace URI to schema location mapping
    entries: Vec<CatalogEntry>,
}

/// A single catalog entry
#[derive(Debug, Clone)]
pub struct CatalogEntry {
    /// Namespace URI
    pub namespace: String,
    /// Schema location (file path or URL)
    pub location: String,
}

impl SchemaCatalog {
    /// Create a new empty catalog
    pub fn new() -> Self {
        Self::default()
    }

    /// Add an entry to the catalog
    pub fn add(&mut self, namespace: impl Into<String>, location: impl Into<String>) {
        self.entries.push(CatalogEntry {
            namespace: namespace.into(),
            location: location.into(),
        });
    }

    /// Look up a location by namespace
    pub fn lookup(&self, namespace: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|e| e.namespace == namespace)
            .map(|e| e.location.as_str())
    }

    /// Add well-known XML namespaces with embedded schema locations.
    ///
    /// Maps standard XML namespaces to `embedded://` URIs that are resolved
    /// by the [`EmbeddedLoader`].
    pub fn add_xml_catalog(&mut self) {
        // XML namespace (xml:lang, xml:space, xml:base) - uses embedded schema
        self.add(
            "http://www.w3.org/XML/1998/namespace",
            "embedded://xml.xsd",
        );

        // XML Schema instance namespace (xsi:type, xsi:nil, etc.)
        // Note: This could be embedded in the future
        self.add(
            "http://www.w3.org/2001/XMLSchema-instance",
            "http://www.w3.org/2001/XMLSchema-instance.xsd",
        );
    }
}

impl SchemaResolver {
    /// Create a new resolver with default configuration and loader chain.
    ///
    /// Uses [`LoaderChain::with_defaults()`] which includes:
    /// - [`EmbeddedLoader`] for `embedded://` URIs
    /// - [`FileSystemLoader`] for file paths
    pub fn new() -> Self {
        Self {
            config: ResolverConfig::default(),
            resolving: HashSet::new(),
            catalog: SchemaCatalog::new(),
            loader: Box::new(LoaderChain::with_defaults()),
        }
    }

    /// Create a resolver with the specified configuration.
    ///
    /// Uses the default loader chain.
    pub fn with_config(config: ResolverConfig) -> Self {
        Self {
            config,
            resolving: HashSet::new(),
            catalog: SchemaCatalog::new(),
            loader: Box::new(LoaderChain::with_defaults()),
        }
    }

    /// Create a resolver with a custom loader.
    ///
    /// # Example
    /// ```
    /// use xsd_schema::{SchemaResolver, LoaderChain};
    ///
    /// let loader = LoaderChain::with_defaults();
    /// let resolver = SchemaResolver::with_loader(Box::new(loader));
    /// ```
    pub fn with_loader(loader: Box<dyn SchemaLoader>) -> Self {
        Self {
            config: ResolverConfig::default(),
            resolving: HashSet::new(),
            catalog: SchemaCatalog::new(),
            loader,
        }
    }

    /// Create a resolver with custom configuration and loader.
    pub fn with_config_and_loader(config: ResolverConfig, loader: Box<dyn SchemaLoader>) -> Self {
        Self {
            config,
            resolving: HashSet::new(),
            catalog: SchemaCatalog::new(),
            loader,
        }
    }

    /// Get a mutable reference to the catalog
    pub fn catalog_mut(&mut self) -> &mut SchemaCatalog {
        &mut self.catalog
    }

    /// Resolve a schema location to an absolute path or URL
    pub fn resolve_location(
        &self,
        schema_location: &str,
        base_uri: &str,
    ) -> SchemaResult<String> {
        // Check if it's already absolute
        if is_absolute_uri(schema_location) {
            return Ok(schema_location.to_string());
        }

        // Try to resolve relative to base URI
        let resolved = resolve_relative_uri(schema_location, base_uri)?;
        Ok(resolved)
    }

    /// Load and parse a schema from a location
    ///
    /// Returns the document ID if the schema was loaded, or None if it was
    /// already loaded (circular reference).
    pub fn load_schema(
        &mut self,
        location: &str,
        base_uri: &str,
        schema_set: &mut SchemaSet,
    ) -> SchemaResult<Option<DocumentId>> {
        // Resolve the location
        let resolved = self.resolve_location(location, base_uri)?;

        // Check if already loaded
        if schema_set.is_loaded(&resolved) {
            return Ok(schema_set.loaded_locations.get(&resolved).copied());
        }

        // Check for circular resolution
        if self.resolving.contains(&resolved) {
            // Circular include is allowed, just skip
            return Ok(None);
        }

        // Mark as being resolved
        self.resolving.insert(resolved.clone());

        // Load the schema content
        let content = self.load_content(&resolved)?;

        // Parse the schema
        let doc_id = parse_schema_with_config(
            content.as_bytes(),
            &resolved,
            schema_set,
            &self.config.parser_config,
        )?;

        // Mark as loaded
        schema_set.mark_loaded(resolved.clone(), doc_id);

        // Remove from resolving set
        self.resolving.remove(&resolved);

        Ok(Some(doc_id))
    }

    /// Load content from a location using the configured loader chain.
    ///
    /// Supports embedded://, file paths, and potentially HTTP (if configured).
    pub fn load_content(&self, location: &str) -> SchemaResult<String> {
        // Check network access for HTTP URLs
        if (location.starts_with("http://") || location.starts_with("https://"))
            && !self.config.allow_network
        {
            return Err(SchemaError::resolution(format!(
                "Network access not allowed for: {}",
                location
            )));
        }

        // Use the loader chain
        self.loader.load(location)
    }

    /// Process an include directive
    pub fn process_include(
        &mut self,
        schema_location: &str,
        base_uri: &str,
        _target_namespace: Option<NameId>,
        schema_set: &mut SchemaSet,
    ) -> SchemaResult<Option<DocumentId>> {
        self.load_schema(schema_location, base_uri, schema_set)
    }

    /// Process an import directive
    pub fn process_import(
        &mut self,
        namespace: Option<&str>,
        schema_location: Option<&str>,
        base_uri: &str,
        schema_set: &mut SchemaSet,
    ) -> SchemaResult<Option<DocumentId>> {
        // If schemaLocation is provided, use it
        if let Some(location) = schema_location {
            return self.load_schema(location, base_uri, schema_set);
        }

        // Otherwise, try catalog lookup
        if let Some(ns) = namespace {
            if let Some(location) = self.catalog.lookup(ns) {
                let location = location.to_string(); // Clone to avoid borrow issues
                return self.load_schema(&location, base_uri, schema_set);
            }
        }

        // Import without schemaLocation and no catalog entry is allowed
        // (the namespace might already be loaded or provided externally)
        Ok(None)
    }

    /// Process a redefine directive
    pub fn process_redefine(
        &mut self,
        schema_location: &str,
        base_uri: &str,
        schema_set: &mut SchemaSet,
    ) -> SchemaResult<Option<DocumentId>> {
        // Loading is sufficient here; apply_redefine handles component
        // replacement later, after all schemas are loaded.
        self.load_schema(schema_location, base_uri, schema_set)
    }

    /// Process an override directive (XSD 1.1)
    #[cfg(feature = "xsd11")]
    pub fn process_override(
        &mut self,
        schema_location: &str,
        base_uri: &str,
        schema_set: &mut SchemaSet,
    ) -> SchemaResult<Option<DocumentId>> {
        // Loading is sufficient here; apply_override handles component
        // replacement later, after all schemas are loaded.
        self.load_schema(schema_location, base_uri, schema_set)
    }
}

impl Default for SchemaResolver {
    fn default() -> Self {
        Self::new()
    }
}

/// Check if a URI is absolute (has a scheme)
fn is_absolute_uri(uri: &str) -> bool {
    // Check for common schemes
    uri.starts_with("http://")
        || uri.starts_with("https://")
        || uri.starts_with("file://")
        || (cfg!(windows) && uri.len() >= 2 && &uri[1..2] == ":")
        || uri.starts_with('/')
}

/// Resolve a relative URI against a base URI
fn resolve_relative_uri(relative: &str, base: &str) -> SchemaResult<String> {
    // Simple implementation for file paths
    if base.starts_with("http://") || base.starts_with("https://") {
        // URL base
        resolve_relative_url(relative, base)
    } else {
        // File path base
        resolve_relative_path(relative, base)
    }
}

/// Resolve a relative URL against a base URL
fn resolve_relative_url(relative: &str, base: &str) -> SchemaResult<String> {
    // Find the last slash in the base URL (excluding protocol slashes)
    let base_without_file = if let Some(pos) = base.rfind('/') {
        // Check if this slash is after the protocol
        if pos > base.find("://").map_or(0, |p| p + 2) {
            &base[..=pos]
        } else {
            base
        }
    } else {
        base
    };

    Ok(format!("{}{}", base_without_file, relative))
}

/// Resolve a relative file path against a base file path
fn resolve_relative_path(relative: &str, base: &str) -> SchemaResult<String> {
    let base_path = Path::new(base);
    let base_dir = base_path.parent().unwrap_or(Path::new("."));
    let resolved = base_dir.join(relative);

    // Normalize the path
    let normalized = normalize_path(&resolved);

    Ok(normalized.to_string_lossy().into_owned())
}

/// Normalize a path by resolving . and .. components
fn normalize_path(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();

    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                result.pop();
            }
            std::path::Component::CurDir => {
                // Skip current dir
            }
            _ => {
                result.push(component);
            }
        }
    }

    result
}

/// Result of resolving all directives in a schema
#[derive(Debug, Default)]
pub struct ResolutionResult {
    /// Document IDs of successfully loaded schemas
    pub loaded: Vec<DocumentId>,
    /// Errors encountered during resolution
    pub errors: Vec<SchemaError>,
    /// Schemas that were already loaded (circular references)
    pub skipped: Vec<String>,
}

impl ResolutionResult {
    /// Check if resolution was fully successful
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }

    /// Check if any schemas were loaded
    pub fn has_loaded(&self) -> bool {
        !self.loaded.is_empty()
    }
}

/// Resolve all directives in a schema document
pub fn resolve_all_directives(
    doc_id: DocumentId,
    resolver: &mut SchemaResolver,
    schema_set: &mut SchemaSet,
) -> ResolutionResult {
    let mut result = ResolutionResult::default();

    // Get the document
    let doc = match schema_set.documents.get(doc_id as usize) {
        Some(d) => d,
        None => {
            result.errors.push(SchemaError::internal(format!(
                "Document {} not found",
                doc_id
            )));
            return result;
        }
    };

    let base_uri = doc.base_uri.clone();
    let target_namespace = doc.target_namespace;

    // Clone directives to avoid borrow issues
    let includes: Vec<_> = doc.includes.to_vec();
    let imports: Vec<_> = doc.imports.to_vec();
    let redefines: Vec<_> = doc.redefines.to_vec();
    #[cfg(feature = "xsd11")]
    let overrides: Vec<_> = doc.overrides.to_vec();

    // Process includes
    for include in includes {
        match resolver.process_include(
            &include.schema_location,
            &base_uri,
            target_namespace,
            schema_set,
        ) {
            Ok(Some(id)) => result.loaded.push(id),
            Ok(None) => result.skipped.push(include.schema_location.clone()),
            Err(e) => result.errors.push(e),
        }
    }

    // Process imports
    for import in imports {
        match resolver.process_import(
            import.namespace.as_deref(),
            import.schema_location.as_deref(),
            &base_uri,
            schema_set,
        ) {
            Ok(Some(id)) => result.loaded.push(id),
            Ok(None) => {
                if let Some(loc) = import.schema_location {
                    result.skipped.push(loc);
                }
            }
            Err(e) => result.errors.push(e),
        }
    }

    // Process redefines
    for redefine in redefines {
        match resolver.process_redefine(&redefine.schema_location, &base_uri, schema_set) {
            Ok(Some(id)) => result.loaded.push(id),
            Ok(None) => result.skipped.push(redefine.schema_location.clone()),
            Err(e) => result.errors.push(e),
        }
    }

    // Process overrides (XSD 1.1)
    #[cfg(feature = "xsd11")]
    for override_dir in overrides {
        match resolver.process_override(
            &override_dir.schema_location,
            &base_uri,
            schema_set,
        ) {
            Ok(Some(id)) => result.loaded.push(id),
            Ok(None) => result.skipped.push(override_dir.schema_location.clone()),
            Err(e) => result.errors.push(e),
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_absolute_uri() {
        assert!(is_absolute_uri("http://example.com/schema.xsd"));
        assert!(is_absolute_uri("https://example.com/schema.xsd"));
        assert!(is_absolute_uri("/absolute/path/schema.xsd"));
        assert!(!is_absolute_uri("relative/path/schema.xsd"));
        assert!(!is_absolute_uri("../parent/schema.xsd"));
    }

    #[test]
    fn test_resolve_relative_path() {
        let resolved = resolve_relative_path("types.xsd", "/home/user/schema.xsd").unwrap();
        assert!(resolved.contains("types.xsd"));
    }

    #[test]
    fn test_resolve_relative_path_parent() {
        let resolved = resolve_relative_path("../common/types.xsd", "/home/user/schemas/main.xsd")
            .unwrap();
        // Should resolve to something like /home/user/common/types.xsd
        assert!(resolved.contains("common"));
        assert!(resolved.contains("types.xsd"));
    }

    #[test]
    fn test_resolve_relative_url() {
        let resolved =
            resolve_relative_url("types.xsd", "http://example.com/schemas/main.xsd").unwrap();
        assert_eq!(resolved, "http://example.com/schemas/types.xsd");
    }

    #[test]
    fn test_catalog_lookup() {
        let mut catalog = SchemaCatalog::new();
        catalog.add("http://example.com/ns", "/path/to/schema.xsd");

        assert_eq!(
            catalog.lookup("http://example.com/ns"),
            Some("/path/to/schema.xsd")
        );
        assert_eq!(catalog.lookup("http://other.com/ns"), None);
    }

    #[test]
    fn test_resolver_config_default() {
        let config = ResolverConfig::default();
        assert!(!config.allow_network);
        assert_eq!(config.max_depth, 100);
    }

    #[test]
    fn test_resolver_new() {
        let resolver = SchemaResolver::new();
        assert!(resolver.resolving.is_empty());
    }

    #[test]
    fn test_normalize_path() {
        let path = Path::new("/home/user/../other/./schema.xsd");
        let normalized = normalize_path(path);
        assert!(!normalized.to_string_lossy().contains(".."));
        assert!(!normalized.to_string_lossy().contains("./"));
    }

    #[test]
    fn test_resolution_result_default() {
        let result = ResolutionResult::default();
        assert!(result.is_ok());
        assert!(!result.has_loaded());
    }

    #[test]
    fn test_catalog_xml_namespaces() {
        let mut catalog = SchemaCatalog::new();
        catalog.add_xml_catalog();

        assert_eq!(
            catalog.lookup("http://www.w3.org/XML/1998/namespace"),
            Some("embedded://xml.xsd")
        );
        assert!(catalog
            .lookup("http://www.w3.org/2001/XMLSchema-instance")
            .is_some());
    }

    #[test]
    fn test_embedded_loader() {
        let loader = EmbeddedLoader::new();

        // Can load embedded URIs
        assert!(loader.can_load("embedded://xml.xsd"));
        assert!(!loader.can_load("/path/to/file.xsd"));
        assert!(!loader.can_load("http://example.com/schema.xsd"));

        // Load xml.xsd
        let content = loader.load("embedded://xml.xsd").unwrap();
        assert!(content.contains("targetNamespace=\"http://www.w3.org/XML/1998/namespace\""));

        // Unknown embedded schema
        assert!(loader.load("embedded://unknown.xsd").is_err());
    }

    #[test]
    fn test_file_system_loader() {
        let loader = FileSystemLoader::new();

        // Can load file paths, not embedded or HTTP
        assert!(loader.can_load("/path/to/file.xsd"));
        assert!(loader.can_load("relative/path.xsd"));
        assert!(!loader.can_load("embedded://xml.xsd"));
        assert!(!loader.can_load("http://example.com/schema.xsd"));
        assert!(!loader.can_load("https://example.com/schema.xsd"));
    }

    #[test]
    fn test_loader_chain() {
        let chain = LoaderChain::with_defaults();

        // Can load both embedded and file paths
        assert!(chain.can_load("embedded://xml.xsd"));
        assert!(chain.can_load("/path/to/file.xsd"));

        // Load embedded schema through chain
        let content = chain.load("embedded://xml.xsd").unwrap();
        assert!(content.contains("http://www.w3.org/XML/1998/namespace"));

        // Chain has expected number of loaders
        assert_eq!(chain.len(), 2);
    }

    #[test]
    fn test_loader_chain_priority() {
        let mut chain = LoaderChain::new();
        chain.add(Box::new(FileSystemLoader::new())); // priority 0
        chain.add(Box::new(EmbeddedLoader::new())); // priority 100

        // EmbeddedLoader should be first due to higher priority
        assert_eq!(chain.priority(), 100);
    }

    #[test]
    fn test_resolver_with_embedded_loader() {
        let resolver = SchemaResolver::new();

        // Load embedded xml.xsd
        let content = resolver.load_content("embedded://xml.xsd").unwrap();
        assert!(content.contains("http://www.w3.org/XML/1998/namespace"));
    }
}
