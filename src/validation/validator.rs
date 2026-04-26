//! Core `SchemaValidator` — immutable validation configuration.
//!
//! `SchemaValidator` holds the compiled schema reference, substitution groups,
//! and validation flags. It is reusable across multiple validation runs.
//!
//! Callers create a per-run [`super::runtime::ValidationRuntime`] via
//! [`SchemaValidator::start_run()`] to perform actual validation.

use crate::compiler::{build_substitution_group_map, SubstitutionGroupMap};
use crate::schema::SchemaSet;

use super::errors::ValidationError;
use super::info::ValidationFlags;
use super::runtime::ValidationRuntime;

// ---------------------------------------------------------------------------
// ValidationSink trait
// ---------------------------------------------------------------------------

/// Sink for validation errors and warnings
///
/// Implement this trait to receive validation messages from `SchemaValidator`.
pub trait ValidationSink {
    /// Report a validation error
    fn on_error(&mut self, error: ValidationError);
    /// Report a validation warning
    fn on_warning(&mut self, warning: ValidationWarning);
}

/// A validation warning (non-fatal)
#[derive(Debug, Clone)]
pub struct ValidationWarning {
    /// Warning code
    pub code: &'static str,
    /// Human-readable message
    pub message: String,
    /// Source location in the instance document
    pub location: Option<crate::parser::location::SourceLocation>,
}

impl std::fmt::Display for ValidationWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)?;
        if let Some(loc) = &self.location {
            write!(f, " at {}", loc)?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Built-in sinks
// ---------------------------------------------------------------------------

/// Collects errors into a `Vec<ValidationError>` and warnings into a `Vec<ValidationWarning>`
pub struct CollectingValidationSink<'a> {
    pub errors: &'a mut Vec<ValidationError>,
    pub warnings: &'a mut Vec<ValidationWarning>,
}

impl<'a> ValidationSink for CollectingValidationSink<'a> {
    fn on_error(&mut self, error: ValidationError) {
        self.errors.push(error);
    }
    fn on_warning(&mut self, warning: ValidationWarning) {
        self.warnings.push(warning);
    }
}

/// Collects errors only; discards warnings
pub struct ErrorOnlySink<'a> {
    pub errors: &'a mut Vec<ValidationError>,
}

impl<'a> ValidationSink for ErrorOnlySink<'a> {
    fn on_error(&mut self, error: ValidationError) {
        self.errors.push(error);
    }
    fn on_warning(&mut self, _warning: ValidationWarning) {
        // Discarded
    }
}

// ---------------------------------------------------------------------------
// AssertionSource — mutual exclusion for assertion evaluation paths
// ---------------------------------------------------------------------------

/// Selects which assertion evaluation path is active.
///
/// XSD 1.1 assertions can be evaluated via two mutually exclusive paths:
/// - `FragmentBuffer` — inline fragment buffering during streaming validation
/// - `MainDocument` — external `BufferDocument`, assertions deferred to Phase 2
///
/// The `Disabled` default means no assertion evaluation occurs.
#[cfg(feature = "xsd11")]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum AssertionSource {
    /// No assertion evaluation. `PROCESS_ASSERTIONS` must NOT be set.
    #[default]
    Disabled,
    /// Inline fragment buffering. `PROCESS_ASSERTIONS` MUST be set.
    FragmentBuffer,
    /// External `BufferDocument`. `PROCESS_ASSERTIONS` must NOT be set.
    MainDocument,
}

// ---------------------------------------------------------------------------
// SchemaValidator — immutable configuration
// ---------------------------------------------------------------------------

/// Immutable validation configuration — reusable across runs.
///
/// Holds the compiled schema reference, substitution groups, and validation
/// flags. Create a per-run [`ValidationRuntime`] via [`Self::start_run()`].
pub struct SchemaValidator<'a> {
    /// The compiled schema set to validate against
    pub(crate) schema_set: &'a SchemaSet,
    /// Pre-built substitution group map (if any)
    pub(crate) subst_groups: Option<SubstitutionGroupMap>,
    /// Validation flags controlling behaviour
    pub(crate) flags: ValidationFlags,
    /// Which assertion evaluation path is active (XSD 1.1 only)
    #[cfg(feature = "xsd11")]
    pub(crate) assertion_source: AssertionSource,
}

impl<'a> SchemaValidator<'a> {
    /// Create a new `SchemaValidator` with default assertion mode (`Disabled`).
    ///
    /// `PROCESS_ASSERTIONS` is silently stripped from `flags` because the
    /// default mode is `Disabled`, and the two must agree. Use
    /// [`Self::new_fragment_buffer()`] or [`Self::new_main_document()`] to
    /// enable assertion processing.
    pub fn new(schema_set: &'a SchemaSet, flags: ValidationFlags) -> Self {
        #[cfg(feature = "xsd11")]
        let flags = flags & !ValidationFlags::PROCESS_ASSERTIONS;
        let subst_groups = build_substitution_group_map(schema_set);
        SchemaValidator {
            schema_set,
            subst_groups: Some(subst_groups),
            flags,
            #[cfg(feature = "xsd11")]
            assertion_source: AssertionSource::default(),
        }
    }

    /// Create a new `SchemaValidator` with pre-built substitution groups.
    pub fn with_substitution_groups(
        schema_set: &'a SchemaSet,
        flags: ValidationFlags,
        subst_groups: SubstitutionGroupMap,
    ) -> Self {
        SchemaValidator {
            subst_groups: Some(subst_groups),
            ..Self::new(schema_set, flags)
        }
    }

    /// XSD 1.1: forces `PROCESS_ASSERTIONS` flag, sets `FragmentBuffer` mode.
    #[cfg(feature = "xsd11")]
    pub fn new_fragment_buffer(schema_set: &'a SchemaSet, flags: ValidationFlags) -> Self {
        let mut v = Self::new(schema_set, flags);
        v.flags |= ValidationFlags::PROCESS_ASSERTIONS;
        v.assertion_source = AssertionSource::FragmentBuffer;
        v
    }

    /// XSD 1.1: clears `PROCESS_ASSERTIONS` flag, sets `MainDocument` mode.
    #[cfg(feature = "xsd11")]
    pub fn new_main_document(schema_set: &'a SchemaSet, flags: ValidationFlags) -> Self {
        let flags = flags & !ValidationFlags::PROCESS_ASSERTIONS;
        let mut v = Self::new(schema_set, flags);
        v.assertion_source = AssertionSource::MainDocument;
        v
    }

    /// Set the assertion evaluation source.
    ///
    /// Enforces the mutual exclusion contract between `PROCESS_ASSERTIONS`
    /// flag and the chosen `AssertionSource` mode:
    /// - `FragmentBuffer` requires `PROCESS_ASSERTIONS` to be set
    /// - `Disabled` and `MainDocument` require it to NOT be set
    ///
    /// # Panics
    /// Panics if the flag/mode combination is invalid.
    #[cfg(feature = "xsd11")]
    #[allow(dead_code)] // Future non-test callers will use this
    pub(crate) fn set_assertion_source(&mut self, source: AssertionSource) -> &mut Self {
        let has_flag = self.flags.contains(ValidationFlags::PROCESS_ASSERTIONS);
        match source {
            AssertionSource::FragmentBuffer => {
                assert!(
                    has_flag,
                    "AssertionSource::FragmentBuffer requires ValidationFlags::PROCESS_ASSERTIONS"
                );
            }
            AssertionSource::Disabled | AssertionSource::MainDocument => {
                assert!(
                    !has_flag,
                    "AssertionSource::{:?} requires PROCESS_ASSERTIONS to NOT be set",
                    source
                );
            }
        }
        self.assertion_source = source;
        self
    }

    /// Create a mutable runtime for one validation pass.
    pub fn start_run<S: ValidationSink>(&self, sink: S) -> ValidationRuntime<'_, S> {
        ValidationRuntime::new(
            self.schema_set,
            &self.subst_groups,
            self.flags,
            sink,
            #[cfg(feature = "xsd11")]
            self.assertion_source,
        )
    }
}

// ---------------------------------------------------------------------------
// Tests (config-only — no validation method calls)
// ---------------------------------------------------------------------------

#[cfg(test)]
#[cfg(feature = "xsd11")]
mod assertion_source_tests {
    use super::*;
    use crate::pipeline::load_and_process_schema;

    fn load_schema(xsd: &str) -> SchemaSet {
        let mut schema_set = SchemaSet::new();
        load_and_process_schema(xsd.as_bytes(), "test.xsd", &mut schema_set, None)
            .expect("failed to load schema");
        schema_set
    }

    #[test]
    fn test_assertion_source_default_is_disabled() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root" type="xs:string"/>
            </xs:schema>"#,
        );
        let v = SchemaValidator::new(&schema_set, ValidationFlags::default());
        assert_eq!(v.assertion_source, AssertionSource::Disabled);
    }

    #[test]
    fn test_fragment_buffer_constructor() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root" type="xs:string"/>
            </xs:schema>"#,
        );
        let v = SchemaValidator::new_fragment_buffer(&schema_set, ValidationFlags::default());
        assert_eq!(v.assertion_source, AssertionSource::FragmentBuffer);
        assert!(v.flags.contains(ValidationFlags::PROCESS_ASSERTIONS));
    }

    #[test]
    fn test_new_strips_process_assertions_flag() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root" type="xs:string"/>
            </xs:schema>"#,
        );
        // Passing PROCESS_ASSERTIONS to new() is silently stripped
        let flags = ValidationFlags::default() | ValidationFlags::PROCESS_ASSERTIONS;
        let v = SchemaValidator::new(&schema_set, flags);
        assert!(!v.flags.contains(ValidationFlags::PROCESS_ASSERTIONS));
        assert_eq!(v.assertion_source, AssertionSource::Disabled);
    }

    #[test]
    #[should_panic(expected = "PROCESS_ASSERTIONS")]
    fn test_fragment_buffer_without_flag_panics() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root" type="xs:string"/>
            </xs:schema>"#,
        );
        let mut v = SchemaValidator::new(&schema_set, ValidationFlags::default());
        v.set_assertion_source(AssertionSource::FragmentBuffer);
    }

    #[test]
    #[should_panic(expected = "PROCESS_ASSERTIONS")]
    fn test_disabled_with_flag_panics() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root" type="xs:string"/>
            </xs:schema>"#,
        );
        // Use new_fragment_buffer to get PROCESS_ASSERTIONS set, then
        // attempt to switch to Disabled — should panic.
        let mut v = SchemaValidator::new_fragment_buffer(&schema_set, ValidationFlags::default());
        v.set_assertion_source(AssertionSource::Disabled);
    }

    #[test]
    #[should_panic(expected = "PROCESS_ASSERTIONS")]
    fn test_main_document_with_flag_panics() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root" type="xs:string"/>
            </xs:schema>"#,
        );
        // Use new_fragment_buffer to get PROCESS_ASSERTIONS set, then
        // attempt to switch to MainDocument — should panic.
        let mut v = SchemaValidator::new_fragment_buffer(&schema_set, ValidationFlags::default());
        v.set_assertion_source(AssertionSource::MainDocument);
    }

    #[test]
    fn test_main_document_without_flag_ok() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root" type="xs:string"/>
            </xs:schema>"#,
        );
        let mut v = SchemaValidator::new(&schema_set, ValidationFlags::default());
        v.set_assertion_source(AssertionSource::MainDocument);
        assert_eq!(v.assertion_source, AssertionSource::MainDocument);
    }
}
