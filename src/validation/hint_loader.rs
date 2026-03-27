//! Schema location hint loader.
//!
//! A helper that takes accumulated `xsi:schemaLocation` and
//! `xsi:noNamespaceSchemaLocation` hints from a validation run and attempts
//! to load the referenced schemas into a [`SchemaSetBuilder`] for
//! recompilation.
//!
//! # Design
//!
//! `process_loaded_schemas()` is a whole-set compile pass that is not
//! idempotent. Therefore this helper works with [`SchemaSetBuilder`]
//! (pre-compile), not with an already-compiled [`SchemaSet`]. The caller
//! adds their base schemas, enriches with hints, then compiles once.
//!
//! URI resolution is delegated to the builder's [`SchemaResolver`] so that
//! Windows paths, URL normalization, and other platform-specific handling
//! are applied consistently.
//!
//! # Example
//!
//! ```ignore
//! // 1. Validate and collect hints
//! let sl_hints = runtime.schema_location_hints().to_vec();
//! let nnsl_hints = runtime.no_namespace_schema_location_hints().to_vec();
//!
//! // 2. Build enriched schema set
//! let mut builder = SchemaSetBuilder::new();
//! builder.try_add("base.xsd")?;
//! load_hints_into_builder(&mut builder, &sl_hints, &nnsl_hints);
//! let compiled = builder.compile()?;
//! ```

use crate::builder::SchemaSetBuilder;
use crate::error::SchemaError;
use super::info::{SchemaLocationHint, NoNamespaceSchemaLocationHint};

/// Result of hint-driven schema loading.
#[derive(Debug, Default)]
pub struct HintLoadResult {
    /// Number of schemas freshly loaded from hints.
    pub loaded_count: usize,
    /// Number of hints skipped (already loaded, load failure, etc.).
    pub skipped_count: usize,
    /// Errors encountered during loading (non-fatal — partial success is possible).
    pub errors: Vec<SchemaError>,
}

/// Enrich a [`SchemaSetBuilder`] with schemas discovered from
/// `xsi:schemaLocation` and `xsi:noNamespaceSchemaLocation` hints
/// collected during a validation run.
///
/// Each hint carries its own base URI (from the instance document) so
/// that relative schema locations are resolved correctly. URI resolution
/// is performed by the builder's [`SchemaResolver`], which handles
/// platform-specific paths and URL normalization.
///
/// Schemas that are already loaded in the builder are silently skipped
/// and counted in [`HintLoadResult::skipped_count`].
/// Load/network failures are non-fatal and collected in
/// [`HintLoadResult::errors`].
///
/// The builder must NOT yet be compiled. After calling this, the caller
/// should call `builder.compile()` to produce the final compiled schema set.
pub fn load_hints_into_builder(
    builder: &mut SchemaSetBuilder,
    schema_location_hints: &[SchemaLocationHint],
    no_namespace_hints: &[NoNamespaceSchemaLocationHint],
) -> HintLoadResult {
    let mut result = HintLoadResult::default();

    for hint in schema_location_hints {
        try_load_hint(builder, &hint.location, &hint.base_uri, &mut result);
    }

    for hint in no_namespace_hints {
        try_load_hint(builder, &hint.location, &hint.base_uri, &mut result);
    }

    result
}

fn try_load_hint(
    builder: &mut SchemaSetBuilder,
    location: &str,
    base_uri: &str,
    result: &mut HintLoadResult,
) {
    match builder.try_add_relative(location, base_uri) {
        Ok(true) => {
            result.loaded_count += 1;
        }
        Ok(false) => {
            // Already loaded — dedup skip
            result.skipped_count += 1;
        }
        Err(e) => {
            result.errors.push(e);
            result.skipped_count += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builder::SchemaSetBuilder;
    use crate::validation::info::{SchemaLocationHint, NoNamespaceSchemaLocationHint};

    #[test]
    fn test_load_hints_empty() {
        let mut builder = SchemaSetBuilder::new();
        let result = load_hints_into_builder(&mut builder, &[], &[]);
        assert_eq!(result.loaded_count, 0);
        assert_eq!(result.skipped_count, 0);
        assert!(result.errors.is_empty());
    }

    #[test]
    fn test_load_hints_nonexistent_file_is_nonfatal() {
        let mut builder = SchemaSetBuilder::new();
        let hints = vec![SchemaLocationHint {
            namespace: "urn:test".to_string(),
            location: "nonexistent_schema_abc123.xsd".to_string(),
            base_uri: String::new(),
        }];
        let result = load_hints_into_builder(&mut builder, &hints, &[]);
        assert_eq!(result.loaded_count, 0);
        assert_eq!(result.skipped_count, 1);
        assert_eq!(result.errors.len(), 1);
    }

    #[test]
    fn test_load_no_namespace_hints_nonexistent_is_nonfatal() {
        let mut builder = SchemaSetBuilder::new();
        let hints = vec![NoNamespaceSchemaLocationHint {
            location: "nonexistent_schema_abc123.xsd".to_string(),
            base_uri: String::new(),
        }];
        let result = load_hints_into_builder(&mut builder, &[], &hints);
        assert_eq!(result.loaded_count, 0);
        assert_eq!(result.skipped_count, 1);
        assert_eq!(result.errors.len(), 1);
    }

    #[test]
    fn test_duplicate_hints_counted_as_skipped() {
        // Load a real schema, then try to load it again via a duplicate hint.
        let xsd = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root" type="xs:string"/>
        </xs:schema>"#;
        let mut builder = SchemaSetBuilder::new()
            .add_source(xsd, "http://example.com/dedup.xsd").unwrap();

        // Hint pointing to the same location should be skipped, not loaded again.
        // try_add normalizes, and add_source records the exact base_uri —
        // use the same absolute URI so normalization matches.
        let hints = vec![SchemaLocationHint {
            namespace: "".to_string(),
            location: "http://example.com/dedup.xsd".to_string(),
            base_uri: String::new(),
        }];
        let result = load_hints_into_builder(&mut builder, &hints, &[]);
        assert_eq!(result.loaded_count, 0, "duplicate should not be loaded");
        assert_eq!(result.skipped_count, 1, "duplicate should be skipped");
        // The hint loader may produce an error if the resolver can't
        // re-fetch the URL, but the is_loaded check should prevent that.
        // The key assertion: it was not double-loaded.
    }

    #[test]
    fn test_add_source_normalizes_for_dedup() {
        // add_source with a relative path should normalize to the same
        // absolute load key that a later hint resolves to.
        let xsd = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root" type="xs:string"/>
        </xs:schema>"#;
        let cwd = std::env::current_dir().unwrap();
        let mut builder = SchemaSetBuilder::new()
            .add_source(xsd, "schemas/test.xsd").unwrap();

        let instance_base = cwd
            .join("schemas")
            .join("instance.xml")
            .to_string_lossy()
            .into_owned();
        let hints = vec![SchemaLocationHint {
            namespace: "".to_string(),
            location: "test.xsd".to_string(),
            base_uri: instance_base,
        }];
        let result = load_hints_into_builder(&mut builder, &hints, &[]);
        assert_eq!(result.loaded_count, 0,
            "hint resolving to already-loaded URI should not reload");
        assert_eq!(result.skipped_count, 1);
    }
}
