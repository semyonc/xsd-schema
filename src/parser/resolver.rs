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
#[cfg(feature = "async")]
use std::pin::Pin;

use crate::error::{SchemaError, SchemaResult};
use crate::ids::{DocumentId, NameId};
use crate::parser::parse::ParserConfig;
use crate::schema::composition::{CompositionEdge, CompositionEdgeKind};
use crate::SchemaSet;

/// Result of a single `load_schema` call, distinguishing three outcomes.
#[derive(Debug)]
pub enum LoadOutcome {
    /// Schema was freshly loaded and parsed.
    Loaded(DocumentId),
    /// Schema was already in `loaded_locations`.
    AlreadyLoaded(DocumentId),
    /// Schema is currently mid-parse (in the `resolving` set). Contains the
    /// resolved URI so the caller can record a cycle edge and fix it up later.
    Cycle(String),
}


// ============================================================================
// Encoding-Aware Decoding
// ============================================================================

const UTF8_BOM: &[u8] = &[0xEF, 0xBB, 0xBF];
const UTF16_LE_BOM: &[u8] = &[0xFF, 0xFE];
const UTF16_BE_BOM: &[u8] = &[0xFE, 0xFF];

#[derive(Copy, Clone)]
enum Endian {
    Le,
    Be,
}

/// Decode raw XML bytes into UTF-8 bytes, sniffing common Unicode encodings
/// per XML 1.0 §F.1.
///
/// Recognizes UTF-8 with/without BOM and UTF-16 LE/BE with/without BOM. The
/// returned `Vec<u8>` is the input buffer unchanged when it is already UTF-8
/// with no BOM (zero-copy fast path).
pub fn decode_xml_to_utf8_bytes(bytes: Vec<u8>) -> SchemaResult<Vec<u8>> {
    if bytes.starts_with(UTF8_BOM) {
        return Ok(bytes[UTF8_BOM.len()..].to_vec());
    }
    if bytes.starts_with(UTF16_LE_BOM) {
        return Ok(decode_utf16(&bytes[UTF16_LE_BOM.len()..], Endian::Le)?.into_bytes());
    }
    if bytes.starts_with(UTF16_BE_BOM) {
        return Ok(decode_utf16(&bytes[UTF16_BE_BOM.len()..], Endian::Be)?.into_bytes());
    }
    if let Some(endian) = sniff_utf16_no_bom(&bytes) {
        return Ok(decode_utf16(&bytes, endian)?.into_bytes());
    }
    Ok(bytes)
}

/// Decode raw XML bytes into a UTF-8 `String`, sniffing common Unicode
/// encodings per XML 1.0 §F.1. See [`decode_xml_to_utf8_bytes`].
pub fn decode_xml_bytes(bytes: Vec<u8>) -> SchemaResult<String> {
    let utf8 = decode_xml_to_utf8_bytes(bytes)?;
    String::from_utf8(utf8)
        .map_err(|e| SchemaError::resolution(format!("Invalid UTF-8 content: {}", e)))
}

fn sniff_utf16_no_bom(bytes: &[u8]) -> Option<Endian> {
    // XML 1.0 §F.1: with no BOM, the first four bytes of '<?' in UTF-16 LE
    // are `3C 00 ?? 00` and in UTF-16 BE are `00 3C 00 ??`. The non-null
    // third/fourth byte distinguishes UTF-16 from UTF-32.
    if bytes.len() < 4 {
        return None;
    }
    match (bytes[0], bytes[1]) {
        (0x3C, 0x00) if bytes[2] != 0x00 && bytes[3] == 0x00 => Some(Endian::Le),
        (0x00, 0x3C) if bytes[2] == 0x00 && bytes[3] != 0x00 => Some(Endian::Be),
        _ => None,
    }
}

fn decode_utf16(bytes: &[u8], endian: Endian) -> SchemaResult<String> {
    if !bytes.len().is_multiple_of(2) {
        return Err(SchemaError::resolution(
            "UTF-16 byte stream has an odd number of bytes".to_string(),
        ));
    }
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| match endian {
            Endian::Le => u16::from_le_bytes([c[0], c[1]]),
            Endian::Be => u16::from_be_bytes([c[0], c[1]]),
        })
        .collect();
    String::from_utf16(&units)
        .map_err(|e| SchemaError::resolution(format!("Invalid UTF-16 sequence: {}", e)))
}

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
        let bytes = std::fs::read(path).map_err(|e| {
            SchemaError::resolution(format!("Failed to read file '{}': {}", location, e))
        })?;
        decode_xml_bytes(bytes)
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
                "xlink.xsd" => {
                    let bytes = crate::embedded::XLINK_XSD;
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
    /// Optional async loader for non-blocking I/O (HTTP, cloud storage, etc.)
    ///
    /// When set, async methods use this loader instead of wrapping the sync
    /// loader. When `None`, async methods fall back to the sync `loader`.
    #[cfg(feature = "async")]
    async_loader: Option<Box<dyn AsyncSchemaLoader>>,
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

        // XLink namespace (xlink:type, xlink:href, etc.) - uses embedded schema
        self.add(
            "http://www.w3.org/1999/xlink",
            "embedded://xlink.xsd",
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
            #[cfg(feature = "async")]
            async_loader: None,
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
            #[cfg(feature = "async")]
            async_loader: None,
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
            #[cfg(feature = "async")]
            async_loader: None,
        }
    }

    /// Create a resolver with custom configuration and loader.
    pub fn with_config_and_loader(config: ResolverConfig, loader: Box<dyn SchemaLoader>) -> Self {
        Self {
            config,
            resolving: HashSet::new(),
            catalog: SchemaCatalog::new(),
            loader,
            #[cfg(feature = "async")]
            async_loader: None,
        }
    }

    /// Create a resolver with a custom async loader for non-blocking I/O.
    ///
    /// The async loader is used by `load_content_async` and `load_schema_async`.
    /// The default sync loader chain is still used for sync methods.
    #[cfg(feature = "async")]
    pub fn with_async_loader(async_loader: Box<dyn AsyncSchemaLoader>) -> Self {
        Self {
            config: ResolverConfig::default(),
            resolving: HashSet::new(),
            catalog: SchemaCatalog::new(),
            loader: Box::new(LoaderChain::with_defaults()),
            async_loader: Some(async_loader),
        }
    }

    /// Create a resolver with custom configuration and an async loader.
    #[cfg(feature = "async")]
    pub fn with_config_and_async_loader(
        config: ResolverConfig,
        async_loader: Box<dyn AsyncSchemaLoader>,
    ) -> Self {
        Self {
            config,
            resolving: HashSet::new(),
            catalog: SchemaCatalog::new(),
            loader: Box::new(LoaderChain::with_defaults()),
            async_loader: Some(async_loader),
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

    /// Load and parse a schema from a location.
    ///
    /// Returns a [`LoadOutcome`] distinguishing freshly loaded, already loaded,
    /// and cycle-in-progress cases.
    ///
    /// If `chameleon_namespace` is `Some` and the loaded schema has no
    /// `targetNamespace`, the chameleon namespace is adopted per §4.2.3.
    pub fn load_schema(
        &mut self,
        location: &str,
        base_uri: &str,
        schema_set: &mut SchemaSet,
        chameleon_namespace: Option<NameId>,
    ) -> SchemaResult<LoadOutcome> {
        // Resolve the location
        let resolved = self.resolve_location(location, base_uri)?;

        // Check if already loaded (chameleon-aware).
        if let Some(id) = check_loaded_cache(schema_set, &resolved, chameleon_namespace) {
            return Ok(LoadOutcome::AlreadyLoaded(id));
        }

        // Check for circular resolution
        if self.resolving.contains(&resolved) {
            // Circular include is allowed, just skip
            return Ok(LoadOutcome::Cycle(resolved));
        }

        // Mark as being resolved (cycle detection)
        self.resolving.insert(resolved.clone());

        // Load the schema content — clean up resolving set on error
        let content = match self.load_content(&resolved) {
            Ok(c) => c,
            Err(e) => {
                self.resolving.remove(&resolved);
                return Err(e);
            }
        };

        // Parse the schema — clean up resolving set on error.
        // Apply chameleon namespace adoption if specified.
        let doc_id = match crate::parser::parse::parse_schema_with_chameleon(
            content.as_bytes(),
            &resolved,
            schema_set,
            &self.config.parser_config,
            chameleon_namespace,
        ) {
            Ok(id) => id,
            Err(e) => {
                self.resolving.remove(&resolved);
                return Err(e);
            }
        };

        // Mark as loaded (chameleon-aware).
        mark_loaded_chameleon_aware(schema_set, &resolved, doc_id, chameleon_namespace);

        // Remove from resolving set
        self.resolving.remove(&resolved);

        Ok(LoadOutcome::Loaded(doc_id))
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

    /// Process an include directive.
    ///
    /// Passes `target_namespace` as the chameleon namespace: if the included
    /// schema has no `targetNamespace`, it adopts the includer's (§4.2.3).
    pub fn process_include(
        &mut self,
        schema_location: &str,
        base_uri: &str,
        target_namespace: Option<NameId>,
        schema_set: &mut SchemaSet,
    ) -> SchemaResult<LoadOutcome> {
        self.load_schema(schema_location, base_uri, schema_set, target_namespace)
    }

    /// Process an import directive.
    ///
    /// Returns `Ok(None)` only when there is no `schemaLocation` and no
    /// catalog match (namespace-only import). All other paths return
    /// `Ok(Some(LoadOutcome))`.
    pub fn process_import(
        &mut self,
        namespace: Option<&str>,
        schema_location: Option<&str>,
        base_uri: &str,
        schema_set: &mut SchemaSet,
    ) -> SchemaResult<Option<LoadOutcome>> {
        // Import does not do chameleon namespace adoption.
        //
        // Catalog takes priority: if the namespace has a catalog entry, use it
        // instead of the schemaLocation hint.  This follows standard XML Catalog
        // semantics and lets embedded/local schemas override remote HTTP URLs.
        if let Some(ns) = namespace {
            if let Some(location) = self.catalog.lookup(ns) {
                let location = location.to_string();
                let catalog_already_loaded = self
                    .resolve_location(&location, base_uri)
                    .ok()
                    .is_some_and(|r| schema_set.loaded_locations.contains_key(&r));
                if !catalog_already_loaded {
                    return Ok(Some(self.load_schema(&location, base_uri, schema_set, None)?));
                }
                // Catalog entry already loaded — done.
                return Ok(None);
            }
        }

        // No catalog match — try schemaLocation if provided.
        if let Some(location) = schema_location {
            return Ok(Some(self.load_schema(location, base_uri, schema_set, None)?));
        }

        // Import without schemaLocation and no catalog entry is allowed
        // (the namespace might already be loaded or provided externally)
        Ok(None)
    }

    /// Process a redefine directive.
    ///
    /// Passes `target_namespace` as the chameleon namespace: if the redefined
    /// schema has no `targetNamespace`, it adopts the redefiner's (§4.2.4).
    pub fn process_redefine(
        &mut self,
        schema_location: &str,
        base_uri: &str,
        target_namespace: Option<NameId>,
        schema_set: &mut SchemaSet,
    ) -> SchemaResult<LoadOutcome> {
        self.load_schema(schema_location, base_uri, schema_set, target_namespace)
    }

    /// Process an override directive (XSD 1.1).
    ///
    /// Passes `target_namespace` as the chameleon namespace: if the overridden
    /// schema has no `targetNamespace`, it adopts the overrider's (§4.2.5).
    #[cfg(feature = "xsd11")]
    pub fn process_override(
        &mut self,
        schema_location: &str,
        base_uri: &str,
        target_namespace: Option<NameId>,
        schema_set: &mut SchemaSet,
    ) -> SchemaResult<LoadOutcome> {
        self.load_schema(schema_location, base_uri, schema_set, target_namespace)
    }
}

impl Default for SchemaResolver {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Async Schema Loading (feature = "async")
// ============================================================================

/// Trait for loading schema content asynchronously.
///
/// Implementations can provide truly non-blocking I/O for HTTP, cloud storage,
/// or other async sources. Pass a `Box<dyn AsyncSchemaLoader>` to
/// [`SchemaResolver::with_async_loader`] to enable async loading.
///
/// When no async loader is configured, async resolver methods fall back to the
/// sync [`SchemaLoader`] (blocking the current task).
///
/// The trait is object-safe (`Pin<Box<dyn Future>>`), so it can be stored as
/// `Box<dyn AsyncSchemaLoader>` without conflicting with sync trait impls.
#[cfg(feature = "async")]
pub trait AsyncSchemaLoader: Send + Sync + Debug {
    /// Load schema content asynchronously from the given location.
    fn load_async(
        &self,
        location: &str,
    ) -> Pin<Box<dyn std::future::Future<Output = SchemaResult<String>> + Send + '_>>;

    /// Check if this loader can handle the given location.
    fn can_load(&self, location: &str) -> bool;
}

#[cfg(feature = "async")]
impl SchemaResolver {
    /// Load content asynchronously from a location.
    ///
    /// Uses the [`AsyncSchemaLoader`] if one was provided via
    /// [`with_async_loader`](SchemaResolver::with_async_loader); otherwise
    /// falls back to the sync [`SchemaLoader`].
    pub async fn load_content_async(&self, location: &str) -> SchemaResult<String> {
        // Check network access for HTTP URLs
        if (location.starts_with("http://") || location.starts_with("https://"))
            && !self.config.allow_network
        {
            return Err(SchemaError::resolution(format!(
                "Network access not allowed for: {}",
                location
            )));
        }

        // Use the async loader only when it can handle this location;
        // otherwise fall back to the sync loader chain (embedded, filesystem, etc.)
        if let Some(ref async_loader) = self.async_loader {
            if async_loader.can_load(location) {
                return async_loader.load_async(location).await;
            }
        }
        self.loader.load(location)
    }

    /// Load and parse a schema asynchronously from a location.
    ///
    /// Returns a [`LoadOutcome`] distinguishing freshly loaded, already loaded,
    /// and cycle-in-progress cases.
    pub async fn load_schema_async(
        &mut self,
        location: &str,
        base_uri: &str,
        schema_set: &mut SchemaSet,
        chameleon_namespace: Option<NameId>,
    ) -> SchemaResult<LoadOutcome> {
        // Resolve the location
        let resolved = self.resolve_location(location, base_uri)?;

        // Check if already loaded (chameleon-aware).
        if let Some(id) = check_loaded_cache(schema_set, &resolved, chameleon_namespace) {
            return Ok(LoadOutcome::AlreadyLoaded(id));
        }

        // Check for circular resolution
        if self.resolving.contains(&resolved) {
            return Ok(LoadOutcome::Cycle(resolved));
        }

        // Mark as being resolved (cycle detection)
        self.resolving.insert(resolved.clone());

        // Load the schema content asynchronously — clean up on error
        let content = match self.load_content_async(&resolved).await {
            Ok(c) => c,
            Err(e) => {
                self.resolving.remove(&resolved);
                return Err(e);
            }
        };

        // Parse the schema (sync — CPU-bound) — clean up on error.
        // Apply chameleon namespace adoption if specified.
        let doc_id = match crate::parser::parse::parse_schema_with_chameleon(
            content.as_bytes(),
            &resolved,
            schema_set,
            &self.config.parser_config,
            chameleon_namespace,
        ) {
            Ok(id) => id,
            Err(e) => {
                self.resolving.remove(&resolved);
                return Err(e);
            }
        };

        // Mark as loaded (chameleon-aware).
        mark_loaded_chameleon_aware(schema_set, &resolved, doc_id, chameleon_namespace);

        // Remove from resolving set
        self.resolving.remove(&resolved);

        Ok(LoadOutcome::Loaded(doc_id))
    }
}

/// Resolve all directives in a schema document asynchronously.
///
/// Same structure as [`resolve_all_directives`] but uses async loading.
#[cfg(feature = "async")]
pub async fn resolve_all_directives_async(
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

    // Process includes (pass chameleon namespace)
    for (i, include) in includes.iter().enumerate() {
        match resolver.load_schema_async(
            &include.schema_location,
            &base_uri,
            schema_set,
            target_namespace,
        ).await {
            Ok(ref outcome) => {
                if let LoadOutcome::Loaded(id) | LoadOutcome::AlreadyLoaded(id) = outcome {
                    result.loaded.push(*id);
                    schema_set.documents[doc_id as usize].includes[i].resolved_doc_id = Some(*id);
                } else {
                    result.skipped.push(include.schema_location.clone());
                }
                record_edge(
                    schema_set, doc_id, outcome, CompositionEdgeKind::Include,
                    include.source.as_ref(), &include.schema_location,
                );
            }
            Err(e) => result.errors.push(e),
        }
    }

    // Process imports — catalog takes priority over schemaLocation
    for (i, import) in imports.iter().enumerate() {
        // Check catalog first (namespace mapping overrides location hints)
        let catalog_location = import.namespace.as_deref()
            .and_then(|ns| resolver.catalog.lookup(ns).map(|l| l.to_string()));

        if let Some(location) = catalog_location {
            let catalog_already_loaded = resolver
                .resolve_location(&location, &base_uri)
                .ok()
                .is_some_and(|r| schema_set.loaded_locations.contains_key(&r));
            if catalog_already_loaded {
                continue;
            }
            match resolver.load_schema_async(&location, &base_uri, schema_set, None).await {
                Ok(ref outcome) => {
                    if let LoadOutcome::Loaded(id) | LoadOutcome::AlreadyLoaded(id) = outcome {
                        result.loaded.push(*id);
                        schema_set.documents[doc_id as usize].imports[i].resolved_doc_id = Some(*id);
                    } else {
                        result.skipped.push(location.clone());
                    }
                    record_edge(
                        schema_set, doc_id, outcome, CompositionEdgeKind::Import,
                        import.source.as_ref(), &location,
                    );
                }
                Err(e) => result.import_errors.push(e),
            }
        } else if let Some(location) = import.schema_location.as_deref() {
            match resolver.load_schema_async(location, &base_uri, schema_set, None).await {
                Ok(ref outcome) => {
                    if let LoadOutcome::Loaded(id) | LoadOutcome::AlreadyLoaded(id) = outcome {
                        result.loaded.push(*id);
                        schema_set.documents[doc_id as usize].imports[i].resolved_doc_id = Some(*id);
                    } else {
                        result.skipped.push(location.to_string());
                    }
                    record_edge(
                        schema_set, doc_id, outcome, CompositionEdgeKind::Import,
                        import.source.as_ref(), location,
                    );
                }
                Err(e) => result.import_errors.push(e),
            }
        }
    }

    // Process redefines (pass chameleon namespace)
    for (i, redefine) in redefines.iter().enumerate() {
        match resolver.load_schema_async(
            &redefine.schema_location,
            &base_uri,
            schema_set,
            target_namespace,
        ).await {
            Ok(ref outcome) => {
                if let LoadOutcome::Loaded(id) | LoadOutcome::AlreadyLoaded(id) = outcome {
                    result.loaded.push(*id);
                    schema_set.documents[doc_id as usize].redefines[i].resolved_doc_id = Some(*id);
                } else {
                    result.skipped.push(redefine.schema_location.clone());
                }
                record_edge(
                    schema_set, doc_id, outcome, CompositionEdgeKind::Redefine,
                    redefine.source.as_ref(), &redefine.schema_location,
                );
            }
            Err(e) => result.errors.push(e),
        }
    }

    // Process overrides (XSD 1.1, pass chameleon namespace)
    #[cfg(feature = "xsd11")]
    for (i, override_dir) in overrides.iter().enumerate() {
        match resolver.load_schema_async(
            &override_dir.schema_location,
            &base_uri,
            schema_set,
            target_namespace,
        ).await {
            Ok(ref outcome) => {
                if let LoadOutcome::Loaded(id) | LoadOutcome::AlreadyLoaded(id) = outcome {
                    result.loaded.push(*id);
                    schema_set.documents[doc_id as usize].overrides[i].resolved_doc_id = Some(*id);
                } else {
                    result.skipped.push(override_dir.schema_location.clone());
                }
                record_edge(
                    schema_set, doc_id, outcome, CompositionEdgeKind::Override,
                    override_dir.source.as_ref(), &override_dir.schema_location,
                );
            }
            Err(e) => result.errors.push(e),
        }
    }

    result
}

/// Check if a URI is absolute (has a scheme)
fn is_absolute_uri(uri: &str) -> bool {
    // Check for common schemes
    uri.starts_with("http://")
        || uri.starts_with("https://")
        || uri.starts_with("file://")
        || uri.starts_with("embedded://")
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
    /// Errors from include/redefine/override directives
    pub errors: Vec<SchemaError>,
    /// Errors from xs:import directives
    pub import_errors: Vec<SchemaError>,
    /// Schemas that were already loaded (circular references)
    pub skipped: Vec<String>,
}

impl ResolutionResult {
    /// Check if resolution was fully successful
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty() && self.import_errors.is_empty()
    }

    /// Check if any schemas were loaded
    pub fn has_loaded(&self) -> bool {
        !self.loaded.is_empty()
    }
}

/// Record a composition edge from a [`LoadOutcome`].
///
/// Edges are always recorded. For `Cycle` outcomes, `target_doc` is `None`
/// and will be filled in by [`fixup_composition_edges`] after resolution.
fn record_edge(
    schema_set: &mut SchemaSet,
    source_doc: DocumentId,
    outcome: &LoadOutcome,
    kind: CompositionEdgeKind,
    source: Option<&crate::parser::location::SourceRef>,
    schema_location: &str,
) {
    let (target_doc, resolved_location) = match outcome {
        LoadOutcome::Loaded(id) | LoadOutcome::AlreadyLoaded(id) => {
            // The resolved URI is stored as the document's base_uri during parsing.
            let loc = schema_set.documents[*id as usize].base_uri.clone();
            (Some(*id), loc)
        }
        LoadOutcome::Cycle(resolved) => (None, resolved.clone()),
    };
    schema_set.composition_edges.push(CompositionEdge {
        source_doc,
        target_doc,
        resolved_location,
        kind,
        source: source.cloned(),
        schema_location: schema_location.to_string(),
    });
}

/// Check the chameleon-aware loaded-location caches for a previously loaded
/// document.  Returns `Some(doc_id)` when the cached document is compatible
/// with the requested `chameleon_namespace`, `None` otherwise.
///
/// Shared by both `load_schema` (sync) and `load_schema_async`.
fn check_loaded_cache(
    schema_set: &SchemaSet,
    resolved: &str,
    chameleon_namespace: Option<NameId>,
) -> Option<DocumentId> {
    // Check chameleon-specific cache first.
    if let Some(ns) = chameleon_namespace {
        if let Some(&id) = schema_set.chameleon_cache.get(&(resolved.to_owned(), ns)) {
            return Some(id);
        }
    }
    // Then check primary cache with reusability check.
    if let Some(&id) = schema_set.loaded_locations.get(resolved) {
        let reusable = schema_set.documents.get(id as usize).is_none_or(|doc| {
            if doc.is_chameleon() {
                false
            } else if doc.target_namespace.is_some() {
                true
            } else {
                // Raw no-namespace document — reusable only when no
                // chameleon adoption is requested (§4.2.3).
                chameleon_namespace.is_none()
            }
        });
        if reusable {
            return Some(id);
        }
    }
    None
}

/// Record a freshly loaded document in the appropriate caches.
///
/// Chameleon variants are stored in `chameleon_cache`; the primary
/// `loaded_locations` only gets the first entry per URI.
///
/// Shared by both `load_schema` (sync) and `load_schema_async`.
fn mark_loaded_chameleon_aware(
    schema_set: &mut SchemaSet,
    resolved: &str,
    doc_id: DocumentId,
    chameleon_namespace: Option<NameId>,
) {
    let doc_is_chameleon = schema_set
        .documents
        .get(doc_id as usize)
        .is_some_and(|doc| doc.is_chameleon());
    if doc_is_chameleon {
        if let Some(ns) = chameleon_namespace {
            schema_set
                .chameleon_cache
                .insert((resolved.to_owned(), ns), doc_id);
        }
    }
    if !schema_set.loaded_locations.contains_key(resolved) {
        schema_set.mark_loaded(resolved.to_owned(), doc_id);
    }
}

/// Fixup pass: fill in `target_doc` on cycle edges whose target has since
/// been loaded. Call after all directive resolution rounds complete.
pub fn fixup_composition_edges(schema_set: &mut SchemaSet) {
    for edge in &mut schema_set.composition_edges {
        if edge.target_doc.is_none() {
            edge.target_doc = schema_set
                .loaded_locations
                .get(&edge.resolved_location)
                .copied();
        }
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
    for (i, include) in includes.iter().enumerate() {
        match resolver.process_include(
            &include.schema_location,
            &base_uri,
            target_namespace,
            schema_set,
        ) {
            Ok(ref outcome) => {
                match outcome {
                    LoadOutcome::Loaded(id) => {
                        result.loaded.push(*id);
                        schema_set.documents[doc_id as usize].includes[i].resolved_doc_id = Some(*id);
                    }
                    LoadOutcome::AlreadyLoaded(id) => {
                        // Already processed — record doc_id but don't add to loaded
                        // to avoid re-processing in recursive directive resolution.
                        schema_set.documents[doc_id as usize].includes[i].resolved_doc_id = Some(*id);
                    }
                    _ => {
                        result.skipped.push(include.schema_location.clone());
                    }
                }
                record_edge(
                    schema_set, doc_id, outcome, CompositionEdgeKind::Include,
                    include.source.as_ref(), &include.schema_location,
                );
            }
            Err(e) => result.errors.push(e),
        }
    }

    // Process imports
    for (i, import) in imports.iter().enumerate() {
        match resolver.process_import(
            import.namespace.as_deref(),
            import.schema_location.as_deref(),
            &base_uri,
            schema_set,
        ) {
            Ok(Some(ref outcome)) => {
                match outcome {
                    LoadOutcome::Loaded(id) => {
                        result.loaded.push(*id);
                        schema_set.documents[doc_id as usize].imports[i].resolved_doc_id = Some(*id);
                    }
                    LoadOutcome::AlreadyLoaded(id) => {
                        // Already processed — record doc_id but don't add to loaded.
                        schema_set.documents[doc_id as usize].imports[i].resolved_doc_id = Some(*id);
                    }
                    _ => {
                        if let Some(loc) = &import.schema_location {
                            result.skipped.push(loc.clone());
                        }
                    }
                }
                record_edge(
                    schema_set, doc_id, outcome, CompositionEdgeKind::Import,
                    import.source.as_ref(),
                    import.schema_location.as_deref().unwrap_or_default(),
                );
            }
            Ok(None) => {
                // No schemaLocation and no catalog match — no edge to record
            }
            Err(e) => result.import_errors.push(e),
        }
    }

    // Process redefines
    for (i, redefine) in redefines.iter().enumerate() {
        match resolver.process_redefine(&redefine.schema_location, &base_uri, target_namespace, schema_set) {
            Ok(ref outcome) => {
                match outcome {
                    LoadOutcome::Loaded(id) => {
                        result.loaded.push(*id);
                        schema_set.documents[doc_id as usize].redefines[i].resolved_doc_id = Some(*id);
                    }
                    LoadOutcome::AlreadyLoaded(id) => {
                        schema_set.documents[doc_id as usize].redefines[i].resolved_doc_id = Some(*id);
                    }
                    _ => {
                        result.skipped.push(redefine.schema_location.clone());
                    }
                }
                record_edge(
                    schema_set, doc_id, outcome, CompositionEdgeKind::Redefine,
                    redefine.source.as_ref(), &redefine.schema_location,
                );
            }
            Err(e) => result.errors.push(e),
        }
    }

    // Process overrides (XSD 1.1)
    #[cfg(feature = "xsd11")]
    for (i, override_dir) in overrides.iter().enumerate() {
        match resolver.process_override(
            &override_dir.schema_location,
            &base_uri,
            target_namespace,
            schema_set,
        ) {
            Ok(ref outcome) => {
                match outcome {
                    LoadOutcome::Loaded(id) => {
                        result.loaded.push(*id);
                        schema_set.documents[doc_id as usize].overrides[i].resolved_doc_id = Some(*id);
                    }
                    LoadOutcome::AlreadyLoaded(id) => {
                        schema_set.documents[doc_id as usize].overrides[i].resolved_doc_id = Some(*id);
                    }
                    _ => {
                        result.skipped.push(override_dir.schema_location.clone());
                    }
                }
                record_edge(
                    schema_set, doc_id, outcome, CompositionEdgeKind::Override,
                    override_dir.source.as_ref(), &override_dir.schema_location,
                );
            }
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

    #[test]
    fn test_composition_edges_recorded() {
        use crate::parser::parse::parse_schema;
        use crate::schema::SchemaSet;
        use crate::schema::composition::CompositionEdgeKind;

        let tmp = std::env::temp_dir().join("xsd_test_composition_edges");
        std::fs::create_dir_all(&tmp).unwrap();

        // Base schema with a simple type
        let base_xsd = r#"<?xml version="1.0" encoding="UTF-8"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
    <xs:simpleType name="MyString">
        <xs:restriction base="xs:string"/>
    </xs:simpleType>
</xs:schema>"#;
        let base_path = tmp.join("comp_base.xsd");
        std::fs::write(&base_path, base_xsd).unwrap();

        // Main schema with include + redefine
        let main_xsd = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
    <xs:include schemaLocation="{loc}"/>
    <xs:redefine schemaLocation="{loc}">
        <xs:simpleType name="MyString">
            <xs:restriction base="MyString">
                <xs:maxLength value="50"/>
            </xs:restriction>
        </xs:simpleType>
    </xs:redefine>
</xs:schema>"#,
            loc = base_path.to_string_lossy()
        );

        let mut schema_set = SchemaSet::new();
        let main_path = tmp.join("comp_main.xsd").to_string_lossy().to_string();
        let doc_id = parse_schema(main_xsd.as_bytes(), &main_path, &mut schema_set).unwrap();

        let mut resolver = SchemaResolver::new();
        let result = resolve_all_directives(doc_id, &mut resolver, &mut schema_set);
        assert!(result.is_ok(), "Resolution should succeed");

        // Should have edges for include and redefine
        let edges = &schema_set.composition_edges;
        assert!(
            edges.len() >= 2,
            "Expected at least 2 edges, got {}",
            edges.len()
        );

        let include_edges: Vec<_> = edges
            .iter()
            .filter(|e| e.kind == CompositionEdgeKind::Include)
            .collect();
        assert!(!include_edges.is_empty(), "Should have an include edge");
        assert_eq!(include_edges[0].source_doc, doc_id);

        let redefine_edges: Vec<_> = edges
            .iter()
            .filter(|e| e.kind == CompositionEdgeKind::Redefine)
            .collect();
        assert!(!redefine_edges.is_empty(), "Should have a redefine edge");
        assert_eq!(redefine_edges[0].source_doc, doc_id);

        // Both edges should point to the same target document
        assert!(include_edges[0].target_doc.is_some());
        assert_eq!(include_edges[0].target_doc, redefine_edges[0].target_doc);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_composition_edges_cycle() {
        use crate::parser::parse::parse_schema;
        use crate::schema::SchemaSet;
        use crate::schema::composition::CompositionEdgeKind;

        let tmp = std::env::temp_dir().join("xsd_test_composition_cycle");
        std::fs::create_dir_all(&tmp).unwrap();

        let a_path = tmp.join("cycle_a.xsd");
        let b_path = tmp.join("cycle_b.xsd");

        // a.xsd includes b.xsd
        let a_xsd = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
    <xs:include schemaLocation="{}"/>
    <xs:element name="A" type="xs:string"/>
</xs:schema>"#,
            b_path.to_string_lossy()
        );

        // b.xsd includes a.xsd (creates cycle)
        let b_xsd = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
    <xs:include schemaLocation="{}"/>
    <xs:element name="B" type="xs:string"/>
</xs:schema>"#,
            a_path.to_string_lossy()
        );

        std::fs::write(&a_path, &a_xsd).unwrap();
        std::fs::write(&b_path, &b_xsd).unwrap();

        let mut schema_set = SchemaSet::new();
        let a_uri = a_path.to_string_lossy().to_string();
        let a_doc_id = parse_schema(
            std::fs::read_to_string(&a_path).unwrap().as_bytes(),
            &a_uri,
            &mut schema_set,
        )
        .unwrap();

        // Mark a.xsd as loaded so cycle detection works
        schema_set.mark_loaded(a_uri, a_doc_id);

        let mut resolver = SchemaResolver::new();

        // First resolution: a.xsd's directives (loads b.xsd)
        let result_a = resolve_all_directives(a_doc_id, &mut resolver, &mut schema_set);
        assert!(result_a.is_ok(), "Resolution of a.xsd should succeed");
        assert_eq!(result_a.loaded.len(), 1, "Should have loaded b.xsd");

        let b_doc_id = result_a.loaded[0];

        // Second resolution: b.xsd's directives (a.xsd already loaded)
        let result_b = resolve_all_directives(b_doc_id, &mut resolver, &mut schema_set);
        assert!(result_b.is_ok(), "Resolution of b.xsd should succeed");

        // Should have edges for both directions
        let edges = &schema_set.composition_edges;

        // a→b edge (from first resolution, Loaded branch)
        let a_to_b: Vec<_> = edges
            .iter()
            .filter(|e| e.source_doc == a_doc_id && e.target_doc == Some(b_doc_id))
            .collect();
        assert_eq!(a_to_b.len(), 1, "Should have exactly one a→b edge");
        assert_eq!(a_to_b[0].kind, CompositionEdgeKind::Include);

        // b→a edge (from second resolution, AlreadyLoaded branch)
        let b_to_a: Vec<_> = edges
            .iter()
            .filter(|e| e.source_doc == b_doc_id && e.target_doc == Some(a_doc_id))
            .collect();
        assert_eq!(b_to_a.len(), 1, "Should have exactly one b→a edge");
        assert_eq!(b_to_a[0].kind, CompositionEdgeKind::Include);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_resolved_doc_id_populated() {
        use crate::parser::parse::parse_schema;
        use crate::schema::SchemaSet;

        let tmp = std::env::temp_dir().join("xsd_test_resolved_doc_id");
        std::fs::create_dir_all(&tmp).unwrap();

        // Base schema with a simple type
        let base_xsd = r#"<?xml version="1.0" encoding="UTF-8"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
    <xs:simpleType name="MyString">
        <xs:restriction base="xs:string"/>
    </xs:simpleType>
</xs:schema>"#;
        let base_path = tmp.join("base.xsd");
        std::fs::write(&base_path, base_xsd).unwrap();

        // Main schema that includes and redefines the base
        let main_xsd = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
    <xs:include schemaLocation="{loc}"/>
    <xs:redefine schemaLocation="{loc}">
        <xs:simpleType name="MyString">
            <xs:restriction base="MyString">
                <xs:maxLength value="50"/>
            </xs:restriction>
        </xs:simpleType>
    </xs:redefine>
</xs:schema>"#,
            loc = base_path.to_string_lossy()
        );

        let mut schema_set = SchemaSet::new();
        let main_path = tmp.join("main.xsd").to_string_lossy().to_string();
        let doc_id = parse_schema(main_xsd.as_bytes(), &main_path, &mut schema_set).unwrap();

        let mut resolver = SchemaResolver::new();
        let result = resolve_all_directives(doc_id, &mut resolver, &mut schema_set);
        assert!(result.is_ok(), "Resolution should succeed");

        let doc = &schema_set.documents[doc_id as usize];
        assert!(
            doc.includes[0].resolved_doc_id.is_some(),
            "Include should have resolved_doc_id"
        );
        assert!(
            doc.redefines[0].resolved_doc_id.is_some(),
            "Redefine should have resolved_doc_id"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_document_component_index_populated() {
        use crate::parser::parse::parse_schema;
        use crate::schema::SchemaSet;

        let xsd = r#"<?xml version="1.0" encoding="UTF-8"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
    <xs:simpleType name="MyString">
        <xs:restriction base="xs:string"/>
    </xs:simpleType>
    <xs:element name="root" type="MyString"/>
</xs:schema>"#;

        let mut schema_set = SchemaSet::new();
        let doc_id = parse_schema(xsd.as_bytes(), "test.xsd", &mut schema_set).unwrap();

        let doc = &schema_set.documents[doc_id as usize];
        assert!(
            !doc.component_index.is_empty(),
            "Component index should be populated"
        );

        // Should find the simple type
        assert!(
            doc.component_index.lookup_type(None, schema_set.name_table.get("MyString").unwrap()).is_some(),
            "Should find MyString type in document component index"
        );

        // Should find the element
        assert!(
            doc.component_index.lookup_element(None, schema_set.name_table.get("root").unwrap()).is_some(),
            "Should find root element in document component index"
        );

        // Should NOT find a non-existent component
        assert!(
            doc.component_index.lookup_type(None, schema_set.name_table.get("root").unwrap()).is_none(),
            "Should not find 'root' as a type"
        );
    }

    #[test]
    fn test_redefine_uses_document_scoped_lookup() {
        use crate::parser::parse::parse_schema;
        use crate::schema::SchemaSet;

        let tmp = std::env::temp_dir().join("xsd_test_redefine_doc_scoped");
        std::fs::create_dir_all(&tmp).unwrap();

        // Base schema with a simple type
        let base_xsd = r#"<?xml version="1.0" encoding="UTF-8"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
    <xs:simpleType name="MyString">
        <xs:restriction base="xs:string"/>
    </xs:simpleType>
</xs:schema>"#;
        let base_path = tmp.join("redef_base.xsd");
        std::fs::write(&base_path, base_xsd).unwrap();

        // Main schema that redefines the base type
        let main_xsd = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
    <xs:redefine schemaLocation="{loc}">
        <xs:simpleType name="MyString">
            <xs:restriction base="MyString">
                <xs:maxLength value="50"/>
            </xs:restriction>
        </xs:simpleType>
    </xs:redefine>
</xs:schema>"#,
            loc = base_path.to_string_lossy()
        );

        let mut schema_set = SchemaSet::new();
        let main_path = tmp.join("redef_main.xsd").to_string_lossy().to_string();
        let doc_id = parse_schema(main_xsd.as_bytes(), &main_path, &mut schema_set).unwrap();

        // Resolve directives (loads base.xsd, populates resolved_doc_id)
        let mut resolver = SchemaResolver::new();
        let result = resolve_all_directives(doc_id, &mut resolver, &mut schema_set);
        assert!(result.is_ok(), "Resolution should succeed");

        let main_doc = &schema_set.documents[doc_id as usize];
        let target_doc_id = main_doc.redefines[0].resolved_doc_id;
        assert!(target_doc_id.is_some(), "Redefine should have resolved_doc_id");

        // Verify the target document's component index has MyString
        let target_doc = &schema_set.documents[target_doc_id.unwrap() as usize];
        let my_string_name = schema_set.name_table.get("MyString").unwrap();
        assert!(
            target_doc.component_index.lookup_type(None, my_string_name).is_some(),
            "Target document should have MyString in component index"
        );

        // Apply redefine — should succeed using document-scoped lookup
        crate::schema::apply_redefine_override(&mut schema_set).unwrap();

        // Verify the namespace table now has the redefined type
        let type_key = schema_set.lookup_type(None, my_string_name);
        assert!(type_key.is_some(), "MyString should still be in namespace table after redefine");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_effective_components_provenance_populated() {
        use crate::parser::parse::parse_schema;
        use crate::schema::SchemaSet;
        use crate::schema::composition::CompositionAction;

        let tmp = std::env::temp_dir().join("xsd_test_provenance");
        std::fs::create_dir_all(&tmp).unwrap();

        let base_xsd = r#"<?xml version="1.0" encoding="UTF-8"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
    <xs:simpleType name="MyStr">
        <xs:restriction base="xs:string"/>
    </xs:simpleType>
    <xs:element name="root" type="MyStr"/>
</xs:schema>"#;
        let base_path = tmp.join("prov_base.xsd");
        std::fs::write(&base_path, base_xsd).unwrap();

        let main_xsd = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
    <xs:redefine schemaLocation="{loc}">
        <xs:simpleType name="MyStr">
            <xs:restriction base="MyStr">
                <xs:maxLength value="50"/>
            </xs:restriction>
        </xs:simpleType>
    </xs:redefine>
</xs:schema>"#,
            loc = base_path.to_string_lossy()
        );

        let mut schema_set = SchemaSet::new();
        let main_path = tmp.join("prov_main.xsd").to_string_lossy().to_string();
        let doc_id = parse_schema(main_xsd.as_bytes(), &main_path, &mut schema_set).unwrap();

        let mut resolver = SchemaResolver::new();
        let result = resolve_all_directives(doc_id, &mut resolver, &mut schema_set);
        assert!(result.is_ok());

        // Apply composition — builds effective components
        crate::schema::apply_redefine_override(&mut schema_set).unwrap();

        assert!(
            !schema_set.effective_components.is_empty(),
            "Effective components should be populated after composition"
        );

        // The redefined component (MyStr) should have Redefined action,
        // NOT a separate Declared entry — redefine replaces the declared entry.
        let my_str_name = schema_set.name_table.get("MyStr").unwrap();
        let my_str_identity = crate::schema::composition::ComponentIdentity {
            kind: crate::schema::composition::ComponentKind::SimpleType,
            name: my_str_name,
            namespace: None,
        };
        let my_str_eff = schema_set.effective_components.get(&my_str_identity);
        assert!(my_str_eff.is_some(), "MyStr should be in effective components");
        let my_str_eff = my_str_eff.unwrap();
        assert!(
            matches!(my_str_eff.action, CompositionAction::Redefined { .. }),
            "MyStr should have Redefined action, not Declared"
        );
        // origin should point at the redefining document (main), not the target
        assert_eq!(
            my_str_eff.origin.owner_doc, Some(doc_id),
            "Redefined component origin should be the redefining document"
        );

        // The other component (root element) from base.xsd should still be Declared
        let declared_count = schema_set
            .effective_components
            .values()
            .filter(|c| matches!(c.action, CompositionAction::Declared))
            .count();
        assert!(declared_count > 0, "Should have declared components for non-redefined items");

        let _ = std::fs::remove_dir_all(&tmp);
    }



    /// When resolved_doc_id is Some but the target document does NOT declare
    /// the component, redefine must fail — it must not fall back to a
    /// same-name component from another document in the global namespace table.
    #[test]
    fn test_redefine_no_fallback_to_global_when_scoped() {
        use crate::parser::parse::parse_schema;
        use crate::schema::model::RedefineDirective;
        use crate::schema::redefine::apply_redefine;
        use crate::schema::SchemaSet;

        let tmp = std::env::temp_dir().join("xsd_test_redefine_no_fallback");
        std::fs::create_dir_all(&tmp).unwrap();

        // doc_a.xsd declares MyType (simple type)
        let doc_a_xsd = r#"<?xml version="1.0" encoding="UTF-8"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
    <xs:simpleType name="MyType">
        <xs:restriction base="xs:string"/>
    </xs:simpleType>
</xs:schema>"#;
        let doc_a_path = tmp.join("no_fallback_a.xsd");
        std::fs::write(&doc_a_path, doc_a_xsd).unwrap();

        // doc_b.xsd declares a DIFFERENT type (not MyType)
        let doc_b_xsd = r#"<?xml version="1.0" encoding="UTF-8"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
    <xs:simpleType name="OtherType">
        <xs:restriction base="xs:string"/>
    </xs:simpleType>
</xs:schema>"#;
        let doc_b_path = tmp.join("no_fallback_b.xsd");
        std::fs::write(&doc_b_path, doc_b_xsd).unwrap();

        // Parse both documents
        let mut schema_set = SchemaSet::new();
        let _doc_a_id = parse_schema(
            std::fs::read_to_string(&doc_a_path).unwrap().as_bytes(),
            &doc_a_path.to_string_lossy(),
            &mut schema_set,
        )
        .unwrap();
        let doc_b_id = parse_schema(
            std::fs::read_to_string(&doc_b_path).unwrap().as_bytes(),
            &doc_b_path.to_string_lossy(),
            &mut schema_set,
        )
        .unwrap();

        // MyType IS in global namespace table (from doc_a)
        let my_type_name = schema_set.name_table.get("MyType").unwrap();
        assert!(
            schema_set.lookup_type(None, my_type_name).is_some(),
            "MyType should be in global namespace table from doc_a"
        );

        // Create a fake redefine that points resolved_doc_id at doc_b
        // (which does NOT declare MyType). The redefine's replacement type
        // needs to exist in the arena with the right name.
        let redef_key = schema_set.arenas.alloc_simple_type(
            crate::arenas::SimpleTypeDefData {
                name: Some(my_type_name),
                target_namespace: None,
                variety: crate::parser::frames::SimpleTypeVariety::Atomic,
                base_type: Some(crate::parser::frames::TypeRefResult::QName(
                    crate::parser::frames::QNameRef {
                        namespace: None,
                        local_name: my_type_name,
                        prefix: None,
                    },
                )),
                item_type: None,
                member_types: Vec::new(),
                facets: Default::default(),
                final_derivation: crate::schema::model::DerivationSet::empty(),
                id: None,
                derivation_id: None,
                annotation: None,
                source: None,
                resolved_base_type: None,
                resolved_item_type: None,
                resolved_member_types: Vec::new(),
                redefine_original: None,
            },
        );

        let redefine = RedefineDirective {
            source: None,
            schema_location: doc_b_path.to_string_lossy().to_string(),
            resolved_doc_id: Some(doc_b_id), // points at doc_b, which has no MyType
            simple_types: vec![redef_key],
            complex_types: Vec::new(),
            groups: Vec::new(),
            attribute_groups: Vec::new(),
        };

        // This MUST fail: doc_b does not declare MyType, and the lookup
        // must not fall back to the global table where doc_a's MyType lives.
        let result = apply_redefine(&mut schema_set, &redefine);
        assert!(
            result.is_err(),
            "Redefine should fail when target document lacks the component (no global fallback)"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// When the target document has a complex type named "Foo" but the
    /// redefine is for a simple type named "Foo", it must not match —
    /// kind-sensitive lookup must reject the cross-kind match.
    #[test]
    fn test_redefine_simple_vs_complex_kind_mismatch() {
        use crate::parser::parse::parse_schema;
        use crate::schema::model::RedefineDirective;
        use crate::schema::redefine::apply_redefine;
        use crate::schema::SchemaSet;

        let tmp = std::env::temp_dir().join("xsd_test_redefine_kind_mismatch");
        std::fs::create_dir_all(&tmp).unwrap();

        // target.xsd declares Foo as a COMPLEX type
        let target_xsd = r#"<?xml version="1.0" encoding="UTF-8"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
    <xs:complexType name="Foo">
        <xs:sequence>
            <xs:element name="bar" type="xs:string"/>
        </xs:sequence>
    </xs:complexType>
</xs:schema>"#;
        let target_path = tmp.join("kind_target.xsd");
        std::fs::write(&target_path, target_xsd).unwrap();

        let mut schema_set = SchemaSet::new();
        let target_id = parse_schema(
            std::fs::read_to_string(&target_path).unwrap().as_bytes(),
            &target_path.to_string_lossy(),
            &mut schema_set,
        )
        .unwrap();

        let foo_name = schema_set.name_table.get("Foo").unwrap();

        // Verify target doc has Foo as complex type, NOT simple type
        let target_doc = &schema_set.documents[target_id as usize];
        assert!(
            target_doc.component_index.lookup_complex_type(None, foo_name).is_some(),
            "Target should have Foo as complex type"
        );
        assert!(
            target_doc.component_index.lookup_simple_type(None, foo_name).is_none(),
            "Target should NOT have Foo as simple type"
        );

        // Create a simple type redefine for "Foo" pointing at target doc
        let redef_key = schema_set.arenas.alloc_simple_type(
            crate::arenas::SimpleTypeDefData {
                name: Some(foo_name),
                target_namespace: None,
                variety: crate::parser::frames::SimpleTypeVariety::Atomic,
                base_type: Some(crate::parser::frames::TypeRefResult::QName(
                    crate::parser::frames::QNameRef {
                        namespace: None,
                        local_name: foo_name,
                        prefix: None,
                    },
                )),
                item_type: None,
                member_types: Vec::new(),
                facets: Default::default(),
                final_derivation: crate::schema::model::DerivationSet::empty(),
                id: None,
                derivation_id: None,
                annotation: None,
                source: None,
                resolved_base_type: None,
                resolved_item_type: None,
                resolved_member_types: Vec::new(),
                redefine_original: None,
            },
        );

        let redefine = RedefineDirective {
            source: None,
            schema_location: target_path.to_string_lossy().to_string(),
            resolved_doc_id: Some(target_id),
            simple_types: vec![redef_key],
            complex_types: Vec::new(),
            groups: Vec::new(),
            attribute_groups: Vec::new(),
        };

        // Must fail: target has complex type "Foo", not simple type "Foo"
        let result = apply_redefine(&mut schema_set, &redefine);
        assert!(
            result.is_err(),
            "Simple type redefine must not match a same-name complex type in target document"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Chameleon include: a no-namespace schema included by a namespace-bearing
    /// schema should adopt the includer's targetNamespace (§4.2.3 clause 2.3).
    #[test]
    fn test_chameleon_include_adopts_namespace() {
        use crate::parser::parse::parse_schema;
        use crate::schema::SchemaSet;

        let tmp = std::env::temp_dir().join("xsd_test_chameleon_include");
        std::fs::create_dir_all(&tmp).unwrap();

        // chameleon.xsd: no targetNamespace — declares MyType
        let chameleon_xsd = r#"<?xml version="1.0" encoding="UTF-8"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
    <xs:simpleType name="MyType">
        <xs:restriction base="xs:string"/>
    </xs:simpleType>
</xs:schema>"#;
        let chameleon_path = tmp.join("chameleon.xsd");
        std::fs::write(&chameleon_path, chameleon_xsd).unwrap();

        // main.xsd: has targetNamespace, includes chameleon.xsd
        let main_xsd = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
           targetNamespace="http://example.com/main">
    <xs:include schemaLocation="{}"/>
    <xs:element name="root" type="tns:MyType" xmlns:tns="http://example.com/main"/>
</xs:schema>"#,
            chameleon_path.to_string_lossy()
        );

        let mut schema_set = SchemaSet::new();
        let main_path = tmp.join("main.xsd").to_string_lossy().to_string();
        let doc_id = parse_schema(main_xsd.as_bytes(), &main_path, &mut schema_set).unwrap();

        // Resolve directives — this triggers chameleon namespace adoption
        let mut resolver = SchemaResolver::new();
        let result = resolve_all_directives(doc_id, &mut resolver, &mut schema_set);
        assert!(result.is_ok(), "Resolution should succeed");
        assert!(!result.loaded.is_empty(), "Should have loaded chameleon.xsd");

        // The chameleon document should have adopted the includer's namespace
        let chameleon_doc_id = result.loaded[0];
        let chameleon_doc = &schema_set.documents[chameleon_doc_id as usize];
        let main_ns = schema_set.name_table.get("http://example.com/main").unwrap();
        assert_eq!(
            chameleon_doc.target_namespace, Some(main_ns),
            "Chameleon document should adopt includer's targetNamespace"
        );

        // MyType should be registered in the main namespace, not no-namespace
        let my_type_name = schema_set.name_table.get("MyType").unwrap();
        assert!(
            schema_set.lookup_type(Some(main_ns), my_type_name).is_some(),
            "MyType should be in the includer's namespace after chameleon adoption"
        );
        assert!(
            schema_set.lookup_type(None, my_type_name).is_none(),
            "MyType should NOT be in no-namespace after chameleon adoption"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Chameleon redefine: a no-namespace schema redefined by a namespace-bearing
    /// schema should adopt the redefiner's targetNamespace (§4.2.4).
    #[test]
    fn test_chameleon_redefine_adopts_namespace() {
        use crate::parser::parse::parse_schema;
        use crate::schema::SchemaSet;

        let tmp = std::env::temp_dir().join("xsd_test_chameleon_redefine");
        std::fs::create_dir_all(&tmp).unwrap();

        // chameleon.xsd: no targetNamespace
        let chameleon_xsd = r#"<?xml version="1.0" encoding="UTF-8"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
    <xs:simpleType name="MyStr">
        <xs:restriction base="xs:string"/>
    </xs:simpleType>
</xs:schema>"#;
        let chameleon_path = tmp.join("cham_redef.xsd");
        std::fs::write(&chameleon_path, chameleon_xsd).unwrap();

        // main.xsd: has targetNamespace, redefines from chameleon.xsd
        let main_xsd = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
           targetNamespace="http://example.com/ns">
    <xs:redefine schemaLocation="{}">
        <xs:simpleType name="MyStr">
            <xs:restriction base="MyStr">
                <xs:maxLength value="50"/>
            </xs:restriction>
        </xs:simpleType>
    </xs:redefine>
</xs:schema>"#,
            chameleon_path.to_string_lossy()
        );

        let mut schema_set = SchemaSet::new();
        let main_path = tmp.join("cham_main.xsd").to_string_lossy().to_string();
        let doc_id = parse_schema(main_xsd.as_bytes(), &main_path, &mut schema_set).unwrap();

        let mut resolver = SchemaResolver::new();
        let result = resolve_all_directives(doc_id, &mut resolver, &mut schema_set);
        assert!(result.is_ok(), "Resolution should succeed");

        // The chameleon document should have adopted the namespace
        let chameleon_doc_id = result.loaded[0];
        let chameleon_doc = &schema_set.documents[chameleon_doc_id as usize];
        let ns = schema_set.name_table.get("http://example.com/ns").unwrap();
        assert_eq!(
            chameleon_doc.target_namespace, Some(ns),
            "Chameleon redefine target should adopt redefiner's namespace"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// A no-namespace (chameleon) schema included from two schemas with
    /// different target namespaces must produce two separate document views,
    /// each adopting the includer's namespace.  Previously the second
    /// include returned the first document's ID unchanged (§4.2.3 violation).
    #[test]
    fn test_chameleon_multi_namespace_creates_separate_views() {
        use crate::parser::parse::parse_schema;
        use crate::schema::SchemaSet;

        let tmp = std::env::temp_dir().join("xsd_test_chameleon_multi_ns");
        std::fs::create_dir_all(&tmp).unwrap();

        // chameleon.xsd: no targetNamespace
        let chameleon_xsd = r#"<?xml version="1.0" encoding="UTF-8"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
    <xs:simpleType name="SharedType">
        <xs:restriction base="xs:string"/>
    </xs:simpleType>
</xs:schema>"#;
        let chameleon_path = tmp.join("multi_ns_chameleon.xsd");
        std::fs::write(&chameleon_path, chameleon_xsd).unwrap();

        // ns_a.xsd: targetNamespace="urn:a", includes chameleon
        let ns_a_xsd = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
           targetNamespace="urn:a">
    <xs:include schemaLocation="{}"/>
</xs:schema>"#,
            chameleon_path.to_string_lossy()
        );
        let ns_a_path = tmp.join("multi_ns_a.xsd");
        std::fs::write(&ns_a_path, &ns_a_xsd).unwrap();

        // ns_b.xsd: targetNamespace="urn:b", includes same chameleon
        let ns_b_xsd = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
           targetNamespace="urn:b">
    <xs:include schemaLocation="{}"/>
</xs:schema>"#,
            chameleon_path.to_string_lossy()
        );
        let ns_b_path = tmp.join("multi_ns_b.xsd");
        std::fs::write(&ns_b_path, &ns_b_xsd).unwrap();

        let mut schema_set = SchemaSet::new();

        // Parse ns_a and resolve its include (loads chameleon as urn:a)
        let ns_a_uri = ns_a_path.to_string_lossy().to_string();
        let doc_a = parse_schema(
            std::fs::read_to_string(&ns_a_path).unwrap().as_bytes(),
            &ns_a_uri,
            &mut schema_set,
        )
        .unwrap();
        let mut resolver = SchemaResolver::new();
        let res_a = resolve_all_directives(doc_a, &mut resolver, &mut schema_set);
        assert!(res_a.is_ok(), "ns_a resolution should succeed");
        let chameleon_a_id = res_a.loaded[0];

        // Parse ns_b and resolve its include (must re-parse chameleon as urn:b)
        let ns_b_uri = ns_b_path.to_string_lossy().to_string();
        let doc_b = parse_schema(
            std::fs::read_to_string(&ns_b_path).unwrap().as_bytes(),
            &ns_b_uri,
            &mut schema_set,
        )
        .unwrap();
        let res_b = resolve_all_directives(doc_b, &mut resolver, &mut schema_set);
        assert!(res_b.is_ok(), "ns_b resolution should succeed");
        let chameleon_b_id = res_b.loaded[0];

        // The two chameleon loads must produce DIFFERENT document IDs
        assert_ne!(
            chameleon_a_id, chameleon_b_id,
            "Chameleon schema included from different namespaces must produce separate documents"
        );

        // Each must adopt its includer's namespace
        let ns_a_name = schema_set.name_table.get("urn:a").unwrap();
        let ns_b_name = schema_set.name_table.get("urn:b").unwrap();
        assert_eq!(
            schema_set.documents[chameleon_a_id as usize].target_namespace,
            Some(ns_a_name),
            "First chameleon copy should have urn:a namespace"
        );
        assert_eq!(
            schema_set.documents[chameleon_b_id as usize].target_namespace,
            Some(ns_b_name),
            "Second chameleon copy should have urn:b namespace"
        );

        // Both should be flagged as chameleon
        assert!(schema_set.documents[chameleon_a_id as usize].is_chameleon());
        assert!(schema_set.documents[chameleon_b_id as usize].is_chameleon());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// A no-namespace schema first loaded without chameleon adoption (from a
    /// no-namespace context) must NOT be reused when a later include requests
    /// chameleon adoption into a namespace.  schema(chameleon(tns,D2)) ≠
    /// schema(D2) per §4.2.3.
    #[test]
    fn test_raw_no_namespace_not_reused_for_chameleon() {
        use crate::parser::parse::parse_schema;
        use crate::schema::SchemaSet;

        let tmp = std::env::temp_dir().join("xsd_test_raw_no_ns_chameleon");
        std::fs::create_dir_all(&tmp).unwrap();

        // shared.xsd: no targetNamespace
        let shared_xsd = r#"<?xml version="1.0" encoding="UTF-8"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
    <xs:element name="Shared" type="xs:string"/>
</xs:schema>"#;
        let shared_path = tmp.join("raw_shared.xsd");
        std::fs::write(&shared_path, shared_xsd).unwrap();

        // no_ns.xsd: no targetNamespace, includes shared (raw, no adoption)
        let no_ns_xsd = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
    <xs:include schemaLocation="{}"/>
</xs:schema>"#,
            shared_path.to_string_lossy()
        );
        let no_ns_path = tmp.join("raw_no_ns.xsd");
        std::fs::write(&no_ns_path, &no_ns_xsd).unwrap();

        // with_ns.xsd: targetNamespace="urn:test", includes same shared
        let with_ns_xsd = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
           targetNamespace="urn:test">
    <xs:include schemaLocation="{}"/>
</xs:schema>"#,
            shared_path.to_string_lossy()
        );
        let with_ns_path = tmp.join("raw_with_ns.xsd");
        std::fs::write(&with_ns_path, &with_ns_xsd).unwrap();

        let mut schema_set = SchemaSet::new();
        let mut resolver = SchemaResolver::new();

        // First: load from no-namespace context (raw, no chameleon adoption)
        let no_ns_uri = no_ns_path.to_string_lossy().to_string();
        let doc_no_ns = parse_schema(
            std::fs::read_to_string(&no_ns_path).unwrap().as_bytes(),
            &no_ns_uri,
            &mut schema_set,
        )
        .unwrap();
        let res1 = resolve_all_directives(doc_no_ns, &mut resolver, &mut schema_set);
        assert!(res1.is_ok());
        let raw_id = res1.loaded[0];

        // The raw load should NOT be chameleon
        assert!(!schema_set.documents[raw_id as usize].is_chameleon());
        assert!(schema_set.documents[raw_id as usize].target_namespace.is_none());

        // Second: load from namespace-bearing context (chameleon adoption)
        let with_ns_uri = with_ns_path.to_string_lossy().to_string();
        let doc_with_ns = parse_schema(
            std::fs::read_to_string(&with_ns_path).unwrap().as_bytes(),
            &with_ns_uri,
            &mut schema_set,
        )
        .unwrap();
        let res2 = resolve_all_directives(doc_with_ns, &mut resolver, &mut schema_set);
        assert!(res2.is_ok());
        let chameleon_id = res2.loaded[0];

        // Must be a DIFFERENT document — the raw no-namespace copy must not
        // leak into the namespace-bearing context.
        assert_ne!(
            raw_id, chameleon_id,
            "Raw no-namespace document must not be reused for chameleon adoption"
        );
        let ns_name = schema_set.name_table.get("urn:test").unwrap();
        assert_eq!(
            schema_set.documents[chameleon_id as usize].target_namespace,
            Some(ns_name),
            "Chameleon copy should adopt urn:test namespace"
        );
        assert!(schema_set.documents[chameleon_id as usize].is_chameleon());

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
