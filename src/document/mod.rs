//! Page-based XML document buffer for XPath 2.0 evaluation.
//!
//! This module implements `BufferDocument`, a compact, cache-friendly XML document
//! representation built on a flat array of 16-byte [`Node`] structs with power-of-2
//! page addressing.
//!
//! # Feature gate
//!
//! The entire module is compiled only when the `xsd11` feature is enabled.

pub mod buffer;
pub mod builder;
pub mod error;
pub mod navigator;
pub mod node;
pub mod page;
pub mod qname;
pub mod strings;
pub mod type_remap;
pub mod namespace;
pub mod element_index;
pub mod source_spans;

pub use error::BufferDocumentError;
pub use node::{
    Node, NodeType,
    PAGE_SHIFT, PAGE_SIZE, PAGE_MASK, NULL,
    page_of, slot_of, node_ref_from,
};
pub use page::NodePages;
pub use qname::{QNameAtom, QNameTable, EMPTY_QNAME};
pub use strings::StringStore;
pub use type_remap::{BindingRemapTable, NodeSchemaBinding};
pub use namespace::{
    NamespaceChain, NamespaceNode, NamespacePageFactory, NsRef,
    NS_PAGE_MASK, NS_PAGE_SHIFT, NS_PAGE_SIZE,
};
pub use element_index::ElementIndex;
pub use source_spans::NodeSourceSpans;
pub use buffer::BufferDocument;
pub use builder::BufferDocumentBuilder;
pub use navigator::BufferDocNavigator;

/// Whether the document is a complete XML document or a validation fragment.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DocumentKind {
    /// Complete XML document (default).
    #[default]
    Full,
    /// Assertion-evaluation fragment (synthetic root wrapping a single element).
    Fragment,
}

/// Configuration for [`BufferDocument`] construction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BufferDocumentOptions {
    /// Document mode.
    pub kind: DocumentKind,
    /// Whether to record source byte-offsets per node.
    pub track_source_locations: bool,
}

impl Default for BufferDocumentOptions {
    fn default() -> Self {
        Self {
            kind: DocumentKind::Full,
            track_source_locations: false,
        }
    }
}

impl BufferDocumentOptions {
    /// Full-document mode with source location tracking enabled.
    pub fn full() -> Self {
        Self {
            kind: DocumentKind::Full,
            track_source_locations: true,
        }
    }

    /// Fragment mode with source location tracking disabled.
    pub fn fragment() -> Self {
        Self {
            kind: DocumentKind::Fragment,
            track_source_locations: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_kind_default_is_full() {
        assert_eq!(DocumentKind::default(), DocumentKind::Full);
    }

    #[test]
    fn options_default() {
        let opts = BufferDocumentOptions::default();
        assert_eq!(opts.kind, DocumentKind::Full);
        assert!(!opts.track_source_locations);
    }

    #[test]
    fn options_full() {
        let opts = BufferDocumentOptions::full();
        assert_eq!(opts.kind, DocumentKind::Full);
        assert!(opts.track_source_locations);
    }

    #[test]
    fn options_fragment() {
        let opts = BufferDocumentOptions::fragment();
        assert_eq!(opts.kind, DocumentKind::Fragment);
        assert!(!opts.track_source_locations);
    }
}
