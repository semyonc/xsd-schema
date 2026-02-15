//! Instance validation against XSD schemas
//!
//! This module provides validation error types and helpers for XML instance
//! validation with spec-aligned error codes (cvc-*, cos-*, src-*).

pub mod errors;

pub use errors::{
    ValidationError, ValidationResult,
    error, error_with_path,
    from_value_error, from_facet_error,
    facet_constraint_code, value_error_constraint_code,
};
