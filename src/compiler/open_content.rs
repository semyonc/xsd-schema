//! Open content preparation helpers (XSD 1.1)
//!
//! Full open content validation is implemented in Phase 6. This module provides
//! placeholder structures and entrypoints used by the compiler.

use crate::error::SchemaResult;
pub use crate::types::complex::{OpenContent, OpenContentMode};

use super::ContentModelMatcher;

/// Stub for interleaved open content validation (Phase 6).
pub fn validate_interleave(
    _matcher: &ContentModelMatcher,
    _open_content: &OpenContent,
) -> SchemaResult<()> {
    Ok(())
}

/// Stub for suffix open content validation (Phase 6).
pub fn validate_suffix(
    _matcher: &ContentModelMatcher,
    _open_content: &OpenContent,
) -> SchemaResult<()> {
    Ok(())
}
