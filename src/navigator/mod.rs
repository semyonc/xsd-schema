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

use std::borrow::Cow;

use crate::ids::SimpleTypeKey;
use crate::types::value::XmlValue;

/// Error type for navigator operations
#[derive(Debug, Clone, thiserror::Error)]
pub enum NavigatorError {
    #[error("{0}")]
    Other(String),
}

/// XDM typed-value result for a node.
///
/// Distinguishes the four states that the old `Option<XmlValue>` conflated:
///
/// | Variant   | XDM meaning                           |
/// |-----------|---------------------------------------|
/// | `Value`   | Schema-validated typed atomic value    |
/// | `Untyped` | No schema — atomizes to untypedAtomic |
/// | `Nilled`  | `xsi:nil="true"` — empty sequence     |
/// | `Absent`  | Element-only complex content (FOTY0012)|
#[derive(Debug, Clone, PartialEq)]
pub enum TypedValue {
    /// Schema-validated typed atomic value.
    Value(XmlValue),
    /// Untyped node (no schema) — atomizes to `xs:untypedAtomic`.
    Untyped,
    /// Nilled element (`xsi:nil="true"`) — typed value is empty sequence.
    Nilled,
    /// No typed value (element-only complex content) — FOTY0012 on atomization.
    Absent,
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

/// Scope filter for namespace axis traversal.
///
/// Contract shared by every [`DomNavigator`] backend (BufferDoc, roxmltree):
///
/// * `All` follows the **XDM data model** — it yields every in-scope namespace,
///   inherited declarations included, and **always** the implicit `xml:` binding
///   (even on an element with no declarations of its own). This is what the
///   `namespace::` axis needs. Use it only where XDM semantics are required;
///   it allocates at least the `xml:` node on every element.
/// * `ExcludeXml` is `All` minus the `xml:` binding (implicit or explicit). It is
///   the right scope for callers that treat `xml:` specially or discard it —
///   e.g. validation's namespace snapshot — and it returns nothing (heap-free)
///   on a namespace-free element.
/// * `Local` yields only the namespaces declared on the element itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamespaceAxisScope {
    /// Every in-scope namespace (inherited included) plus the implicit `xml:`
    /// binding — the XDM `namespace::` axis contract.
    All,
    /// Only locally declared namespaces.
    Local,
    /// All in-scope namespaces *except* the `xml:` binding.
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

    /// Move to the appropriate starting position for forward document-order
    /// traversal of the visible tree. In normal scope this is the document
    /// root; in XSD 1.1 assertion scope it is the asserter element (the
    /// "fragment root"), so reverse-axis iterators that need to walk forward
    /// from the visible root stay inside the asserted subtree instead of
    /// being blocked at the synthetic root, whose children are deliberately
    /// hidden by `move_to_first_child`.
    ///
    /// Default implementation calls `move_to_root`. Implementations that
    /// support assertion scope should override.
    fn move_to_visible_root(&mut self) {
        self.move_to_root();
    }

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

    /// Move to the next node in the **following axis** (excludes descendants).
    ///
    /// If `end` is provided, stop before reaching that position.
    /// On `false` the cursor position is unchanged.
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

    /// Borrowed string value of the current node when the backing store can
    /// supply one without allocating; owned otherwise.
    ///
    /// The default delegates to [`value`](Self::value) (always owned), so existing
    /// implementors need no change. DOM backends whose text/attribute values are
    /// interned contiguously (e.g. `BufferDocNavigator`, `RoXmlNavigator`) override
    /// this to borrow, eliminating a per-value allocation in the validation walk.
    fn value_ref(&self) -> Cow<'_, str> {
        Cow::Owned(self.value())
    }

    /// Get the base URI of the current node
    fn base_uri(&self) -> &str;

    // ----- Typed value hooks -----

    /// Get the schema type of the current node (if known)
    fn schema_type(&self) -> Option<SimpleTypeKey>;

    /// Get the typed value of the current node.
    ///
    /// Returns a [`TypedValue`] that distinguishes validated values, untyped
    /// nodes, nilled elements, and element-only complex content.
    fn typed_value(&self) -> TypedValue;

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
        assert_eq!(
            NamespaceAxisScope::ExcludeXml,
            NamespaceAxisScope::ExcludeXml
        );
    }
}
