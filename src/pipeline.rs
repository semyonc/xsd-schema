//! Schema processing pipeline
//!
//! This module provides a high-level orchestration function that coordinates
//! all phases of schema processing:
//!
//! 1. **Parse Phase**: Parse the primary XSD document
//! 2. **Directive Resolution Phase**: Process include/import/redefine/override directives
//! 3. **Redefine/Override Application Phase**: Apply component replacements
//! 4. **Inline Type Assembly Phase**: Materialize inline type definitions
//! 5. **Reference Resolution Phase**: Resolve QName references to component keys
//!
//! # Usage
//!
//! ```
//! use xsd_schema::{SchemaSet, load_and_process_schema};
//!
//! let mut schema_set = SchemaSet::new();
//! let xsd = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
//!     <xs:element name="root" type="xs:string"/>
//! </xs:schema>"#;
//!
//! // XSD version is derived from schema_set (V1_0 by default).
//! // Use SchemaSet::xsd11() for XSD 1.1 schemas.
//! let result = load_and_process_schema(xsd.as_bytes(), "test.xsd", &mut schema_set, None)
//!     .expect("failed to process schema");
//! println!("Processed {} inline types", result.inline_stats.unwrap().total_inline_types);
//! println!("Resolved {} type references", result.resolution_stats.unwrap().types_resolved);
//! ```

use crate::error::SchemaResult;
use crate::ids::DocumentId;
use crate::parser::parse::{parse_schema_with_config, ParserConfig};
use crate::parser::resolver::{resolve_all_directives, fixup_composition_edges, ResolverConfig, SchemaResolver, ResolutionResult};
#[cfg(feature = "async")]
use crate::parser::resolver::resolve_all_directives_async;
use crate::schema::{
    allocate_content_particle_elements, allocate_model_group_particle_elements,
    assemble_inline_types, resolve_all_references, InlineAssemblyStats, ResolutionStats,
    build_dependency_graph, validate_all_derivations, compile_all_patterns,
};
use crate::SchemaSet;

/// Configuration for the schema processing pipeline
#[derive(Debug, Clone)]
pub struct PipelineConfig {
    /// Parser configuration
    pub parser: ParserConfig,
    /// Resolver configuration for include/import handling
    pub resolver: ResolverConfig,
    /// Whether to load external schemas via include/import/redefine/override directives.
    /// When false, no I/O is performed and redefine/override application is deferred
    /// (callers should use `process_loaded_schemas` after all schemas are parsed).
    pub resolve_directives: bool,
    /// Whether to assemble inline types
    pub assemble_inline_types: bool,
    /// Whether to resolve QName references
    pub resolve_references: bool,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            parser: ParserConfig::default(),
            resolver: ResolverConfig::default(),
            resolve_directives: true,
            assemble_inline_types: true,
            resolve_references: true,
        }
    }
}

impl PipelineConfig {
    /// Create a minimal configuration that only parses (no directive/type resolution)
    pub fn parse_only() -> Self {
        Self {
            parser: ParserConfig::default(),
            resolver: ResolverConfig::default(),
            resolve_directives: false,
            assemble_inline_types: false,
            resolve_references: false,
        }
    }

    /// Create a configuration for full processing
    pub fn full() -> Self {
        Self::default()
    }
}

/// Statistics from processing the entire pipeline
#[derive(Debug, Default)]
pub struct PipelineStats {
    /// The primary document ID
    pub doc_id: DocumentId,
    /// Document IDs loaded via include/import directives
    pub loaded_docs: Vec<DocumentId>,
    /// Directive resolution result
    pub directive_result: Option<DirectiveStats>,
    /// Inline type assembly statistics
    pub inline_stats: Option<InlineAssemblyStats>,
    /// Reference resolution statistics
    pub resolution_stats: Option<ResolutionStats>,
}

/// Statistics from directive resolution
#[derive(Debug, Default)]
pub struct DirectiveStats {
    /// Number of schemas loaded successfully
    pub loaded_count: usize,
    /// Number of schemas skipped (already loaded/circular)
    pub skipped_count: usize,
    /// Number of errors during directive resolution
    pub error_count: usize,
}

impl From<&ResolutionResult> for DirectiveStats {
    fn from(result: &ResolutionResult) -> Self {
        Self {
            loaded_count: result.loaded.len(),
            skipped_count: result.skipped.len(),
            error_count: result.errors.len(),
        }
    }
}

/// Load and fully process an XSD schema document
///
/// This is the main entry point for schema processing. It orchestrates all
/// phases of schema handling:
///
/// 1. **Parse**: Parse the primary XSD document
/// 2. **Directives**: Load and parse included/imported/redefined/overridden schemas
/// 3. **Redefine/Override**: Apply component replacements from redefine/override directives
/// 4. **Inline Assembly**: Allocate inline type definitions in arenas
/// 5. **Reference Resolution**: Resolve QName references to component keys
///
/// # Arguments
///
/// * `xml` - Raw XML bytes of the schema document
/// * `base_uri` - Base URI for this document (for error messages and directive resolution)
/// * `schema_set` - Schema set to add the parsed document to
/// * `config` - Optional pipeline configuration (uses defaults if None)
///
/// # Returns
///
/// Pipeline statistics including document IDs and processing counts, or an error.
///
/// # Example
///
/// ```
/// use xsd_schema::{SchemaSet, load_and_process_schema};
///
/// let mut schema_set = SchemaSet::new();
/// let xsd = r#"<?xml version="1.0"?>
/// <xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
///     <xs:element name="root">
///         <xs:complexType>
///             <xs:sequence>
///                 <xs:element name="child" type="xs:string"/>
///             </xs:sequence>
///         </xs:complexType>
///     </xs:element>
/// </xs:schema>"#;
///
/// let stats = load_and_process_schema(xsd.as_bytes(), "schema.xsd", &mut schema_set, None)
///     .expect("failed to process schema");
/// assert!(stats.inline_stats.unwrap().total_inline_types > 0);
/// ```
pub fn load_and_process_schema(
    xml: &[u8],
    base_uri: &str,
    schema_set: &mut SchemaSet,
    config: Option<PipelineConfig>,
) -> SchemaResult<PipelineStats> {
    let config = config.unwrap_or_default();
    let mut stats = PipelineStats::default();

    // Phase 1: Parse the primary schema document
    let doc_id = parse_schema_with_config(xml, base_uri, schema_set, &config.parser)?;
    stats.doc_id = doc_id;

    // Phase 2: Resolve directives (include/import/redefine)
    if config.resolve_directives {
        let mut resolver = SchemaResolver::with_config(config.resolver.clone());

        // Process directives for the primary document
        let dir_result = resolve_all_directives(doc_id, &mut resolver, schema_set);

        // Collect loaded document IDs
        stats.loaded_docs.extend(dir_result.loaded.iter().copied());
        stats.directive_result = Some(DirectiveStats::from(&dir_result));

        // Recursively process directives in loaded documents
        let mut pending_docs = dir_result.loaded.clone();
        while !pending_docs.is_empty() {
            let current_batch: Vec<_> = std::mem::take(&mut pending_docs);
            for loaded_doc_id in current_batch {
                let nested_result = resolve_all_directives(loaded_doc_id, &mut resolver, schema_set);
                stats.loaded_docs.extend(nested_result.loaded.iter().copied());
                pending_docs.extend(nested_result.loaded.iter().copied());

                // Accumulate stats
                if let Some(ref mut dir_stats) = stats.directive_result {
                    dir_stats.loaded_count += nested_result.loaded.len();
                    dir_stats.skipped_count += nested_result.skipped.len();
                    dir_stats.error_count += nested_result.errors.len();
                }
            }
        }

        // Fixup cycle edges now that all documents have been loaded
        fixup_composition_edges(schema_set);

        // If there were directive errors and error_recovery is off, return first error
        if !config.parser.error_recovery {
            if let Some(ref dir_stats) = stats.directive_result {
                if dir_stats.error_count > 0 {
                    // We already collected stats; the error details were logged
                    // In strict mode we could return an error here
                }
            }
        }
    }

    // Fail early if parsing collected structural errors (error-recovery mode)
    if !schema_set.parsing_errors.is_empty() {
        let errors = std::mem::take(&mut schema_set.parsing_errors);
        return Err(errors.into_iter().next().unwrap());
    }

    // Phase 2.5: Apply redefine/override directives (operates on already-parsed
    // data, no I/O). Skipped in parse-only mode because not all schemas may be
    // loaded yet; callers use process_loaded_schemas() to apply later.
    if config.assemble_inline_types || config.resolve_references {
        crate::schema::apply_redefine_override(schema_set)?;
    }

    // Phase 3: Assemble inline types (global operation across all documents)
    if config.assemble_inline_types {
        let inline_stats = assemble_inline_types(schema_set)?;
        stats.inline_stats = Some(inline_stats);
    }

    // Phase 4: Resolve all QName references (global operation across all documents)
    if config.resolve_references {
        let resolution_stats = resolve_all_references(schema_set)?;
        stats.resolution_stats = Some(resolution_stats);
    }

    // Phase 4.5: Compile all deferred pattern facets
    if config.resolve_references {
        compile_all_patterns(schema_set)?;
    }

    // Phase 4.6 (XSD 1.1): Validate default open content declarations
    #[cfg(feature = "xsd11")]
    if config.resolve_references {
        crate::compiler::validate_all_default_open_content(schema_set)?;
    }

    // Phase 4.7: Validate type derivation constraints (cos-ct-extends, derivation-ok-restriction, etc.)
    if config.resolve_references {
        let (dep_graph, _dep_stats) = build_dependency_graph(schema_set)?;
        validate_all_derivations(schema_set, &dep_graph)?;
    }

    // Phase 5: Allocate arena element declarations for local elements in content particles
    if config.assemble_inline_types && config.resolve_references {
        allocate_content_particle_elements(schema_set)?;
        allocate_model_group_particle_elements(schema_set)?;
        validate_all_upa_constraints(schema_set)?;
    }

    Ok(stats)
}

/// Load and process a schema with full processing (convenience function)
///
/// This is a simplified version of `load_and_process_schema` that uses
/// default configuration for full processing.
pub fn load_schema(
    xml: &[u8],
    base_uri: &str,
    schema_set: &mut SchemaSet,
) -> SchemaResult<PipelineStats> {
    load_and_process_schema(xml, base_uri, schema_set, Some(PipelineConfig::full()))
}

/// Parse a schema without processing directives or resolving references
///
/// This is useful when you want to manually control the processing phases
/// or when parsing multiple schemas before batch processing.
pub fn parse_schema_only(
    xml: &[u8],
    base_uri: &str,
    schema_set: &mut SchemaSet,
) -> SchemaResult<DocumentId> {
    let config = PipelineConfig::parse_only();
    let stats = load_and_process_schema(xml, base_uri, schema_set, Some(config))?;
    Ok(stats.doc_id)
}

/// Process inline types and references for schemas already loaded
///
/// Call this after manually loading multiple schemas to perform
/// the redefine/override application, inline assembly, and reference resolution phases.
///
/// **Precondition**: All participating schemas — including redefine/override targets —
/// must have been parsed and loaded into the schema set before calling this function.
pub fn process_loaded_schemas(schema_set: &mut SchemaSet) -> SchemaResult<(InlineAssemblyStats, ResolutionStats)> {
    // Fail early if parsing collected structural errors (error-recovery mode)
    if !schema_set.parsing_errors.is_empty() {
        let errors = std::mem::take(&mut schema_set.parsing_errors);
        return Err(errors.into_iter().next().unwrap());
    }

    // Apply redefine/override directives before assembly
    crate::schema::apply_redefine_override(schema_set)?;

    let inline_stats = assemble_inline_types(schema_set)?;
    let resolution_stats = resolve_all_references(schema_set)?;

    // Compile all deferred pattern facets
    compile_all_patterns(schema_set)?;

    // XSD 1.1: Validate default open content declarations
    #[cfg(feature = "xsd11")]
    crate::compiler::validate_all_default_open_content(schema_set)?;

    // Validate type derivation constraints
    let (dep_graph, _dep_stats) = build_dependency_graph(schema_set)?;
    validate_all_derivations(schema_set, &dep_graph)?;

    allocate_content_particle_elements(schema_set)?;
    allocate_model_group_particle_elements(schema_set)?;
    validate_all_group_outer_occurs(schema_set)?;
    validate_all_upa_constraints(schema_set)?;
    Ok((inline_stats, resolution_stats))
}

/// Validate outer occurrence constraints on top-level all-groups.
///
/// XSD 1.0 (cos-all-limited.2): a particle whose term is an all-group must
/// have minOccurs in {0, 1} and maxOccurs = 1. This check runs on every
/// complex type independently of UPA validation.
fn validate_all_group_outer_occurs(schema_set: &SchemaSet) -> SchemaResult<()> {
    use crate::compiler::{
        is_top_level_all_group, resolve_top_level_all_group_ref,
        validate_outer_all_group_occurs,
    };

    for (_, type_def) in schema_set.arenas.complex_types.iter() {
        let Some(particle) = (match &type_def.content {
            crate::parser::frames::ComplexContentResult::Complex(content) => content.particle.as_ref(),
            crate::parser::frames::ComplexContentResult::Empty
            | crate::parser::frames::ComplexContentResult::Simple(_) => None,
        }) else {
            continue;
        };

        let is_all = is_top_level_all_group(particle).is_some()
            || resolve_top_level_all_group_ref(particle, schema_set).is_some();
        if !is_all {
            continue;
        }

        validate_outer_all_group_occurs(particle, schema_set.xsd_version).map_err(|error| {
            let location = error
                .location()
                .and_then(|source| schema_set.source_maps.locate(source));
            crate::error::SchemaError::structural(
                "cos-all-limited",
                format!("{}", error),
                location,
            )
        })?;
    }

    Ok(())
}

fn validate_all_upa_constraints(schema_set: &SchemaSet) -> SchemaResult<()> {
    for (_, type_def) in schema_set.arenas.complex_types.iter() {
        let Some(particle) = (match &type_def.content {
            crate::parser::frames::ComplexContentResult::Complex(content) => content.particle.as_ref(),
            crate::parser::frames::ComplexContentResult::Empty
            | crate::parser::frames::ComplexContentResult::Simple(_) => None,
        }) else {
            continue;
        };
        if !supports_simple_upa_validation(particle) {
            continue;
        }

        let matcher = crate::compiler::compile_content_model_matcher(schema_set, type_def)
            .map_err(|error| {
                let location = error
                    .location()
                    .and_then(|source| schema_set.source_maps.locate(source));
                crate::error::SchemaError::structural(
                    "cos-nonambig",
                    format!("Failed to compile content model for UPA checking: {}", error),
                    location,
                )
            })?;

        let nfa = match matcher {
            crate::compiler::ContentModelMatcher::Nfa(nfa)
            | crate::compiler::ContentModelMatcher::WithOpenContent { nfa, .. } => Some(nfa),
            crate::compiler::ContentModelMatcher::AllGroup(_) => None,
            #[cfg(feature = "xsd11")]
            crate::compiler::ContentModelMatcher::AllGroupExtension { .. } => None,
        };
        if let Some(nfa) = nfa {
            crate::compiler::check_upa(&nfa, schema_set, type_def.target_namespace)?;
        }
    }

    Ok(())
}

fn supports_simple_upa_validation(
    particle: &crate::parser::frames::ParticleResult,
) -> bool {
    if particle.max_occurs != Some(1) {
        return false;
    }

    match &particle.term {
        crate::parser::frames::ParticleTerm::Element(_)
        | crate::parser::frames::ParticleTerm::Any(_) => true,
        crate::parser::frames::ParticleTerm::Group(group) => {
            if group.ref_name.is_some() {
                return false;
            }

            group
                .particles
                .iter()
                .all(supports_simple_upa_validation)
        }
    }
}

// ============================================================================
// Async Pipeline Functions (feature = "async")
// ============================================================================

/// Load and fully process an XSD schema document asynchronously.
///
/// Async variant of [`load_and_process_schema`]. Only the directive resolution
/// phase (I/O) is async; all computation phases (parse, assembly, resolution)
/// remain synchronous.
///
/// # Arguments
///
/// * `xml` - Raw XML bytes of the schema document
/// * `base_uri` - Base URI for this document
/// * `schema_set` - Schema set to add the parsed document to
/// * `config` - Optional pipeline configuration (uses defaults if None)
#[cfg(feature = "async")]
pub async fn load_and_process_schema_async(
    xml: &[u8],
    base_uri: &str,
    schema_set: &mut SchemaSet,
    config: Option<PipelineConfig>,
) -> SchemaResult<PipelineStats> {
    let config = config.unwrap_or_default();
    let mut stats = PipelineStats::default();

    // Phase 1: Parse the primary schema document (sync — CPU-bound)
    let doc_id = parse_schema_with_config(xml, base_uri, schema_set, &config.parser)?;
    stats.doc_id = doc_id;

    // Phase 2: Resolve directives asynchronously
    if config.resolve_directives {
        let mut resolver = SchemaResolver::with_config(config.resolver.clone());

        let dir_result = resolve_all_directives_async(doc_id, &mut resolver, schema_set).await;

        stats.loaded_docs.extend(dir_result.loaded.iter().copied());
        stats.directive_result = Some(DirectiveStats::from(&dir_result));

        // Recursively process directives in loaded documents
        let mut pending_docs = dir_result.loaded.clone();
        while !pending_docs.is_empty() {
            let current_batch: Vec<_> = std::mem::take(&mut pending_docs);
            for loaded_doc_id in current_batch {
                let nested_result =
                    resolve_all_directives_async(loaded_doc_id, &mut resolver, schema_set).await;
                stats.loaded_docs.extend(nested_result.loaded.iter().copied());
                pending_docs.extend(nested_result.loaded.iter().copied());

                if let Some(ref mut dir_stats) = stats.directive_result {
                    dir_stats.loaded_count += nested_result.loaded.len();
                    dir_stats.skipped_count += nested_result.skipped.len();
                    dir_stats.error_count += nested_result.errors.len();
                }
            }
        }

        // Fixup cycle edges now that all documents have been loaded
        fixup_composition_edges(schema_set);

        if !config.parser.error_recovery {
            if let Some(ref dir_stats) = stats.directive_result {
                if dir_stats.error_count > 0 {
                    // Errors were collected in stats
                }
            }
        }
    }

    // Phase 2.5: Apply redefine/override directives (sync)
    if config.assemble_inline_types || config.resolve_references {
        crate::schema::apply_redefine_override(schema_set)?;
    }

    // Phase 3: Assemble inline types (sync)
    if config.assemble_inline_types {
        let inline_stats = assemble_inline_types(schema_set)?;
        stats.inline_stats = Some(inline_stats);
    }

    // Phase 4: Resolve all QName references (sync)
    if config.resolve_references {
        let resolution_stats = resolve_all_references(schema_set)?;
        stats.resolution_stats = Some(resolution_stats);
    }

    // Phase 4.5: Compile all deferred pattern facets (sync)
    if config.resolve_references {
        compile_all_patterns(schema_set)?;
    }

    // Phase 4.6 (XSD 1.1): Validate default open content declarations
    #[cfg(feature = "xsd11")]
    if config.resolve_references {
        crate::compiler::validate_all_default_open_content(schema_set)?;
    }

    // Phase 4.7: Validate type derivation constraints
    if config.resolve_references {
        let (dep_graph, _dep_stats) = build_dependency_graph(schema_set)?;
        validate_all_derivations(schema_set, &dep_graph)?;
    }

    // Phase 5: Allocate arena element declarations (sync)
    if config.assemble_inline_types && config.resolve_references {
        allocate_content_particle_elements(schema_set)?;
        allocate_model_group_particle_elements(schema_set)?;
    }

    Ok(stats)
}

/// Load and process a schema asynchronously with full processing (convenience function).
///
/// Async variant of [`load_schema`].
#[cfg(feature = "async")]
pub async fn load_schema_async(
    xml: &[u8],
    base_uri: &str,
    schema_set: &mut SchemaSet,
) -> SchemaResult<PipelineStats> {
    load_and_process_schema_async(xml, base_uri, schema_set, Some(PipelineConfig::full())).await
}

#[cfg(test)]
#[path = "pipeline_tests.rs"]
mod tests;
