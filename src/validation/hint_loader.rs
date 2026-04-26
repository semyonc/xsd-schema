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

use super::info::{NoNamespaceSchemaLocationHint, SchemaLocationHint};
use crate::builder::SchemaSetBuilder;
use crate::error::SchemaError;

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

/// Build an enriched [`SchemaSet`] by re-loading the original schemas and
/// adding any `xsi:schemaLocation` / `xsi:noNamespaceSchemaLocation` hints
/// collected during a validation run.
///
/// Returns `Some(enriched_set)` if hints were present and compilation
/// succeeded, `None` if there were no hints or compilation failed.
///
/// This is the recommended way to handle schema-location hints without
/// manually tracking original schema file paths:
///
/// ```rust,ignore
/// // After first validation pass:
/// let sl = runtime.schema_location_hints().to_vec();
/// let nnsl = runtime.no_namespace_schema_location_hints().to_vec();
///
/// if let Some(enriched) = enrich_schema_set(&schema_set, &sl, &nnsl) {
///     // Re-validate with enriched schema set
/// }
/// ```
pub fn enrich_schema_set(
    original: &crate::schema::SchemaSet,
    schema_location_hints: &[SchemaLocationHint],
    no_namespace_hints: &[NoNamespaceSchemaLocationHint],
) -> Option<crate::schema::SchemaSet> {
    if schema_location_hints.is_empty() && no_namespace_hints.is_empty() {
        return None;
    }

    let mut builder = if original.xsd_version == crate::schema::model::XsdVersion::V1_1 {
        SchemaSetBuilder::xsd11()
    } else {
        SchemaSetBuilder::new()
    };

    builder.add_from(original);
    load_hints_into_builder(&mut builder, schema_location_hints, no_namespace_hints);
    builder.compile().ok().map(|c| c.into_schema_set())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builder::SchemaSetBuilder;
    use crate::validation::info::{NoNamespaceSchemaLocationHint, SchemaLocationHint};

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
            .add_source(xsd, "http://example.com/dedup.xsd")
            .unwrap();

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
            .add_source(xsd, "schemas/test.xsd")
            .unwrap();

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
        assert_eq!(
            result.loaded_count, 0,
            "hint resolving to already-loaded URI should not reload"
        );
        assert_eq!(result.skipped_count, 1);
    }

    #[test]
    fn test_enrich_schema_set_returns_none_without_hints() {
        let xsd = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root" type="xs:string"/>
        </xs:schema>"#;
        let compiled = SchemaSetBuilder::new()
            .add_source(xsd, "test.xsd")
            .unwrap()
            .compile()
            .unwrap();

        let result = enrich_schema_set(compiled.schema_set(), &[], &[]);
        assert!(result.is_none(), "should return None when no hints");
    }

    #[test]
    fn test_enrich_schema_set_preserves_original_elements() {
        // Write a temp schema file so add_from can re-load from disk.
        let dir = std::env::temp_dir().join("xsd_hint_test_enrich");
        let _ = std::fs::create_dir_all(&dir);
        let schema_path = dir.join("base.xsd");
        std::fs::write(
            &schema_path,
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root" type="xs:string"/>
        </xs:schema>"#,
        )
        .unwrap();

        let compiled = SchemaSetBuilder::new()
            .add("", &schema_path.to_string_lossy())
            .unwrap()
            .compile()
            .unwrap();
        let original = compiled.schema_set();

        // Provide a hint that fails to load — enrichment should still
        // succeed because add_from re-loaded the original schema.
        let hints = vec![SchemaLocationHint {
            namespace: "urn:test".to_string(),
            location: "nonexistent_42.xsd".to_string(),
            base_uri: String::new(),
        }];

        let enriched = enrich_schema_set(original, &hints, &[]);
        assert!(enriched.is_some(), "should return Some even if hint fails");

        let enriched = enriched.unwrap();
        let name = enriched.name_table.add("root");
        assert!(
            enriched.lookup_element(None, name).is_some(),
            "original element 'root' should still be present after enrichment"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_enrich_schema_set_preserves_xsd_version() {
        let dir = std::env::temp_dir().join("xsd_hint_test_version");
        let _ = std::fs::create_dir_all(&dir);
        let schema_path = dir.join("test.xsd");
        std::fs::write(
            &schema_path,
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root" type="xs:string"/>
        </xs:schema>"#,
        )
        .unwrap();

        let compiled = SchemaSetBuilder::xsd11()
            .add("", &schema_path.to_string_lossy())
            .unwrap()
            .compile()
            .unwrap();
        let original = compiled.schema_set();
        assert_eq!(original.xsd_version, crate::schema::model::XsdVersion::V1_1);

        let hints = vec![SchemaLocationHint {
            namespace: "urn:test".to_string(),
            location: "nonexistent_42.xsd".to_string(),
            base_uri: String::new(),
        }];
        let enriched = enrich_schema_set(original, &hints, &[]).unwrap();
        assert_eq!(
            enriched.xsd_version,
            crate::schema::model::XsdVersion::V1_1,
            "enriched set should preserve XSD 1.1 version"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_add_from_seeds_builder_with_loaded_locations() {
        let dir = std::env::temp_dir().join("xsd_hint_test_add_from");
        let _ = std::fs::create_dir_all(&dir);
        let schema_path = dir.join("original.xsd");
        std::fs::write(
            &schema_path,
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root" type="xs:string"/>
        </xs:schema>"#,
        )
        .unwrap();

        let compiled = SchemaSetBuilder::new()
            .add("", &schema_path.to_string_lossy())
            .unwrap()
            .compile()
            .unwrap();

        let mut builder = SchemaSetBuilder::new();
        builder.add_from(compiled.schema_set());

        // Verify the builder loaded the schema (has at least one document)
        assert!(builder.schema_count() > 0, "add_from should load schemas");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
