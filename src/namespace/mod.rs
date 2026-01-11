//! Namespace management and string interning
//!
//! This module provides:
//! - `NameTable` - String interning for names and namespace URIs
//! - `NamespaceContext` - Scoped prefix-to-namespace mappings
//! - `QualifiedName` - Parsed QName with resolved namespace
//!
//! All strings in the schema model pass through `NameTable` for deduplication
//! and fast equality checks via `NameId`.

pub mod table;
pub mod qname;
pub mod context;

// Re-exports
pub use table::{NameTable, well_known, XS_NAMESPACE, XSI_NAMESPACE, XML_NAMESPACE, XMLNS_NAMESPACE};
pub use qname::{QualifiedName, QNameError, parse_qname, is_ncname};
pub use context::{NamespaceContext, NamespaceScope, NamespaceContextSnapshot};
