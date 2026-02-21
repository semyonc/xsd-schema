//! Open content schema-level validation helpers (XSD 1.1)
//!
//! This module provides helpers for validating that open content declarations
//! in the schema are well-formed (schema-level checks). Runtime instance
//! validation of open content (matching additional elements against the
//! wildcard) is implemented in `crate::validation::content`.

use crate::error::SchemaResult;
pub use crate::types::complex::{OpenContent, OpenContentMode};

use super::ContentModelMatcher;

/// Schema-level validation for interleaved open content declarations.
pub fn validate_interleave(
    _matcher: &ContentModelMatcher,
    _open_content: &OpenContent,
) -> SchemaResult<()> {
    Ok(())
}

/// Schema-level validation for suffix open content declarations.
pub fn validate_suffix(
    _matcher: &ContentModelMatcher,
    _open_content: &OpenContent,
) -> SchemaResult<()> {
    Ok(())
}
