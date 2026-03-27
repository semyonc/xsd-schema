//! Instance validation against XSD schemas
//!
//! This module provides validation error types, a push-based `SchemaValidator`,
//! and supporting types for XML instance validation with spec-aligned error codes.

pub mod errors;
pub mod info;
pub mod content;
pub mod context;
pub mod simple;
pub mod validator;
pub mod identity_lexer;
pub mod asttree;
pub mod identity_parser;
pub mod active_axis;
pub mod identity;
pub mod hint_loader;
pub mod runtime;
#[cfg(feature = "xsd11")]
pub mod assertions;
#[cfg(feature = "xsd11")]
pub mod alternatives;

pub use errors::{
    ValidationError, ValidationResult,
    error, error_with_path,
    from_value_error, from_facet_error,
    facet_constraint_code, value_error_constraint_code,
};

pub use info::{
    SchemaInfo, SchemaValidity, ContentType, NodeIdentity,
    ValidationFlags, ValidationAttempted, ContentProcessing, TypeSource,
    ExpectedElement, ExpectedAttribute, DefaultAttribute,
    SchemaLocationHint, NoNamespaceSchemaLocationHint,
};
#[cfg(feature = "xsd11")]
pub use info::{AssertionOutcome, InheritedAttribute};

pub use content::{ContentValidatorState, ElementMatchInfo};

pub use context::{ElementValidationState, ValidatorState};

pub use simple::{validate_simple_type, SimpleTypeResult};

pub use validator::{
    ValidationSink, ValidationWarning,
    CollectingValidationSink, ErrorOnlySink,
    SchemaValidator,
};
pub use runtime::ValidationRuntime;
pub use identity::{KeyTable, KeySequence, KeyFieldValue};
pub use hint_loader::{load_hints_into_builder, HintLoadResult};
