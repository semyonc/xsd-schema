//! Instance validation against XSD schemas
//!
//! This module provides validation error types, a push-based `SchemaValidator`,
//! and supporting types for XML instance validation with spec-aligned error codes.

pub mod active_axis;
#[cfg(feature = "xsd11")]
pub mod alternatives;
#[cfg(feature = "xsd11")]
pub mod assertions;
pub mod asttree;
pub mod content;
pub mod context;
pub mod errors;
pub mod hint_loader;
pub mod identity;
pub mod identity_lexer;
pub mod identity_parser;
pub mod info;
pub mod runtime;
pub mod simple;
pub mod validator;

pub use errors::{
    error, error_with_path, facet_constraint_code, from_facet_error, from_value_error,
    value_error_constraint_code, ValidationError, ValidationResult,
};

#[cfg(feature = "xsd11")]
pub use info::{AssertionOutcome, InheritedAttribute};
pub use info::{
    ContentProcessing, ContentType, DefaultAttribute, ExpectedAttribute, ExpectedElement,
    NoNamespaceSchemaLocationHint, NodeIdentity, SchemaInfo, SchemaLocationHint, SchemaValidity,
    TypeSource, ValidationAttempted, ValidationFlags,
};

pub use content::{ContentValidatorState, ElementMatchInfo};

pub use context::{ElementValidationState, ValidatorState};

pub use simple::{validate_simple_type, SimpleTypeResult};

pub use hint_loader::{enrich_schema_set, load_hints_into_builder, HintLoadResult};
pub use identity::{KeyFieldValue, KeySequence, KeyTable};
pub use runtime::ValidationRuntime;
pub use validator::{
    CollectingValidationSink, ErrorOnlySink, SchemaValidator, ValidationSink, ValidationWarning,
};
