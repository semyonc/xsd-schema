//! Open content preparation helpers (XSD 1.1)
//!
//! Full open content validation is implemented in Phase 6. This module provides
//! placeholder structures and entrypoints used by the compiler.

use crate::error::SchemaResult;
pub use crate::types::complex::{OpenContent, OpenContentMode, WildcardRef};

use super::all_group::AllGroupModel;
use super::nfa::NfaTable;

/// Strategy for matching compiled content models.
#[derive(Debug, Clone)]
pub enum ContentModelMatcher {
    /// Standard NFA-based content model.
    Nfa(NfaTable),
    /// All-group content model.
    AllGroup(AllGroupModel),
    /// NFA content model with open content wildcard.
    WithOpenContent {
        nfa: NfaTable,
        mode: OpenContentMode,
        wildcard: Option<WildcardRef>,
    },
}

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
