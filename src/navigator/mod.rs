//! DOM navigation trait and types
//!
//! This module provides a read-only navigation interface for XML documents,
//! independent of the underlying DOM implementation.
//!
//! ## Core Types
//!
//! - [`DomNavigator`] - Trait for cursor-based XML navigation
//! - [`DomNodeType`] - Node type enumeration (Element, Text, Attribute, etc.)
//! - [`XmlNodeOrder`] - Document order comparison result
//! - [`NamespaceAxisScope`] - Scope filter for namespace axis traversal
//! - [`NavigatorError`] - Error type for navigator operations
//!
//! ## Adapters
//!
//! - [`RoXmlNavigator`] - Adapter for roxmltree backend (untyped)

pub mod roxmltree;

pub use self::roxmltree::RoXmlNavigator;

use crate::ids::SimpleTypeKey;
use crate::types::value::XmlValue;

/// Error type for navigator operations
#[derive(Debug, Clone, thiserror::Error)]
pub enum NavigatorError {
    #[error("{0}")]
    Other(String),
}

/// XML node types for XPath navigation
///
/// Maps to XPathNodeType in the C# implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DomNodeType {
    /// Document root node
    Root,
    /// Element node
    Element,
    /// Attribute node
    Attribute,
    /// Namespace declaration node
    Namespace,
    /// Text node
    Text,
    /// Whitespace-only text node
    Whitespace,
    /// Significant whitespace text node
    SignificantWhitespace,
    /// Comment node
    Comment,
    /// Processing instruction node
    ProcessingInstruction,
    /// Wildcard - matches any node type (for traversal methods)
    All,
}

impl DomNodeType {
    /// Check if this is a text-like node type
    pub fn is_text_like(self) -> bool {
        matches!(
            self,
            DomNodeType::Text | DomNodeType::Whitespace | DomNodeType::SignificantWhitespace
        )
    }
}

/// Result of document order comparison between two nodes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XmlNodeOrder {
    /// First node comes before second in document order
    Before,
    /// First node comes after second in document order
    After,
    /// Nodes are the same position
    Same,
    /// Order cannot be determined (different documents)
    Unknown,
}

/// Scope filter for namespace axis traversal
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamespaceAxisScope {
    /// Include all in-scope namespaces (including inherited)
    All,
    /// Only locally declared namespaces
    Local,
    /// All namespaces except the xml namespace
    ExcludeXml,
}

/// Read-only cursor-based navigator for XPath2 evaluation
///
/// Provides cursor movement and node information access.
/// Implementations must be `Clone` to support iterator branching.
pub trait DomNavigator: Clone {
    // ----- Node identity and order -----

    /// Check if this navigator is at the same position as another
    fn is_same_position(&self, other: &Self) -> bool;

    /// Compare document order of this position with another
    fn compare_position(&self, other: &Self) -> XmlNodeOrder;

    /// Move this navigator to the position of another
    fn move_to(&mut self, other: &Self) -> bool;

    // ----- Navigation -----

    /// Move to the document root
    fn move_to_root(&mut self);

    /// Move to the parent node (returns false if at root)
    fn move_to_parent(&mut self) -> bool;

    /// Move to the first child (returns false if no children)
    fn move_to_first_child(&mut self) -> bool;

    /// Move to the next sibling (returns false if no next sibling)
    fn move_to_next_sibling(&mut self) -> bool;

    /// Move to the previous sibling (returns false if no prev sibling)
    fn move_to_prev_sibling(&mut self) -> bool;

    /// Move to the first attribute (returns false if no attributes)
    fn move_to_first_attribute(&mut self) -> bool;

    /// Move to the next attribute (returns false if no more attributes)
    fn move_to_next_attribute(&mut self) -> bool;

    /// Move to the first namespace in scope
    fn move_to_first_namespace(&mut self, scope: NamespaceAxisScope) -> bool;

    /// Move to the next namespace in scope
    fn move_to_next_namespace(&mut self, scope: NamespaceAxisScope) -> bool;

    /// Move to the next following node of the given type
    ///
    /// If `end` is provided, stop before reaching that position.
    fn move_to_following(&mut self, kind: DomNodeType, end: Option<&Self>) -> bool;

    // ----- Node information -----

    /// Get the type of the current node
    fn node_type(&self) -> DomNodeType;

    /// Get the local name of the current node
    fn local_name(&self) -> &str;

    /// Get the qualified name (prefix:local) of the current node
    fn name(&self) -> &str;

    /// Get the namespace URI of the current node
    fn namespace_uri(&self) -> &str;

    /// Get the prefix of the current node
    fn prefix(&self) -> &str;

    /// Get the string value of the current node
    fn value(&self) -> String;

    /// Get the base URI of the current node
    fn base_uri(&self) -> &str;

    // ----- Typed value hooks -----

    /// Get the schema type of the current node (if known)
    fn schema_type(&self) -> Option<SimpleTypeKey>;

    /// Get the typed value of the current node (if known)
    fn typed_value(&self) -> Option<XmlValue>;

    // ----- Default helper methods -----

    /// Check if the current element has attributes
    fn has_attributes(&mut self) -> bool {
        let ok = self.move_to_first_attribute();
        if ok {
            self.move_to_parent();
        }
        ok
    }

    /// Check if the current node has children
    fn has_children(&mut self) -> bool {
        let ok = self.move_to_first_child();
        if ok {
            self.move_to_parent();
        }
        ok
    }

    /// Move to the first child of the given node type
    fn move_to_child_kind(&mut self, kind: DomNodeType) -> bool {
        if self.move_to_first_child() {
            loop {
                if self.node_type() == kind || kind == DomNodeType::All {
                    return true;
                }
                if !self.move_to_next_sibling() {
                    break;
                }
            }
            self.move_to_parent();
        }
        false
    }

    /// Move to the first child element with the given name
    fn move_to_child_name(&mut self, local: &str, ns: &str) -> bool {
        if self.move_to_first_child() {
            loop {
                if self.node_type() == DomNodeType::Element
                    && self.local_name() == local
                    && self.namespace_uri() == ns
                {
                    return true;
                }
                if !self.move_to_next_sibling() {
                    break;
                }
            }
            self.move_to_parent();
        }
        false
    }

    /// Get the atomized value of the current node
    ///
    /// Uses typed_value if available, otherwise falls back to untyped rules.
    fn atomized_value(&self) -> XmlValue {
        if let Some(value) = self.typed_value() {
            return value;
        }
        // Fallback per XPath2 Data Model
        match self.node_type() {
            DomNodeType::Comment | DomNodeType::ProcessingInstruction => {
                XmlValue::string(self.value())
            }
            _ => XmlValue::untyped(self.value()),
        }
    }

    /// Find an element by its ID attribute value.
    ///
    /// Returns `Ok(Some(navigator))` positioned at the matching element,
    /// `Ok(None)` if no element with this ID exists.
    ///
    /// The default implementation always returns `Ok(None)`, which is
    /// spec-compliant for documents without DTD/schema ID declarations.
    /// Schema-aware navigators should override this method.
    fn find_element_by_id(&self, id: &str) -> Result<Option<Self>, NavigatorError> {
        let _ = id;
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dom_node_type_equality() {
        assert_eq!(DomNodeType::Element, DomNodeType::Element);
        assert_ne!(DomNodeType::Element, DomNodeType::Attribute);
    }

    #[test]
    fn test_dom_node_type_is_text_like() {
        assert!(DomNodeType::Text.is_text_like());
        assert!(DomNodeType::Whitespace.is_text_like());
        assert!(DomNodeType::SignificantWhitespace.is_text_like());
        assert!(!DomNodeType::Element.is_text_like());
        assert!(!DomNodeType::Attribute.is_text_like());
        assert!(!DomNodeType::Comment.is_text_like());
    }

    #[test]
    fn test_xml_node_order() {
        assert_ne!(XmlNodeOrder::Before, XmlNodeOrder::After);
        assert_eq!(XmlNodeOrder::Same, XmlNodeOrder::Same);
    }

    #[test]
    fn test_namespace_axis_scope() {
        assert_ne!(NamespaceAxisScope::All, NamespaceAxisScope::Local);
        assert_eq!(NamespaceAxisScope::ExcludeXml, NamespaceAxisScope::ExcludeXml);
    }
}
