//! roxmltree adapter for DomNavigator
//!
//! Provides [`RoXmlNavigator`] - an untyped navigator over roxmltree documents.
//! Since roxmltree is a read-only, schema-unaware parser, typed value hooks
//! always return `None`.

use std::collections::HashSet;

use ::roxmltree::{Document, Node, NodeType};

use crate::ids::SimpleTypeKey;
use crate::types::value::XmlValue;

use super::{DomNavigator, DomNodeType, NamespaceAxisScope, XmlNodeOrder};

/// Internal cursor state for RoXmlNavigator
#[derive(Clone)]
enum RoCursor<'a> {
    /// Positioned on a regular node (root, element, text, comment, PI)
    Node(Node<'a, 'a>),
    /// Positioned on an attribute of an element
    Attribute { owner: Node<'a, 'a>, index: usize },
    /// Positioned on a namespace declaration of an element
    Namespace {
        owner: Node<'a, 'a>,
        index: usize,
        /// Cached list of namespaces for this owner (based on scope)
        /// Tuple of (prefix_option, uri)
        namespaces: Vec<(Option<String>, String)>,
    },
}

/// Navigator adapter for roxmltree documents
///
/// This is an untyped navigator - `schema_type()` and `typed_value()`
/// always return `None` by default. For testing purposes, a schema type
/// can be set via `with_schema_type()`.
#[derive(Clone)]
pub struct RoXmlNavigator<'a> {
    /// Reference to the source document
    doc: &'a Document<'a>,
    /// Current cursor position
    cursor: RoCursor<'a>,
    /// Base URI for the document (optional, provided by caller)
    base_uri: String,
    /// Cached qualified name (prefix:local)
    name_cache: String,
    /// Schema type override for testing (normally None)
    #[cfg(test)]
    schema_type_override: Option<SimpleTypeKey>,
}

impl<'a> RoXmlNavigator<'a> {
    /// Create a navigator positioned at the document root
    pub fn new(doc: &'a Document<'a>) -> Self {
        Self {
            doc,
            cursor: RoCursor::Node(doc.root()),
            base_uri: String::new(),
            name_cache: String::new(),
            #[cfg(test)]
            schema_type_override: None,
        }
    }

    /// Create a navigator with a base URI
    pub fn with_base_uri(doc: &'a Document<'a>, base_uri: impl Into<String>) -> Self {
        Self {
            doc,
            cursor: RoCursor::Node(doc.root()),
            base_uri: base_uri.into(),
            name_cache: String::new(),
            #[cfg(test)]
            schema_type_override: None,
        }
    }

    /// Create a navigator positioned at a specific node
    pub fn at_node(doc: &'a Document<'a>, node: Node<'a, 'a>) -> Self {
        Self {
            doc,
            cursor: RoCursor::Node(node),
            base_uri: String::new(),
            name_cache: String::new(),
            #[cfg(test)]
            schema_type_override: None,
        }
    }

    /// Set a schema type for testing purposes
    #[cfg(test)]
    pub fn with_schema_type(mut self, schema_type: SimpleTypeKey) -> Self {
        self.schema_type_override = Some(schema_type);
        self
    }

    /// Get the underlying roxmltree node (if positioned on a node)
    pub fn as_node(&self) -> Option<Node<'a, 'a>> {
        match &self.cursor {
            RoCursor::Node(n) => Some(*n),
            _ => None,
        }
    }

    /// Get the owning element for attribute/namespace cursors, or the node itself
    fn owner_node(&self) -> Node<'a, 'a> {
        match &self.cursor {
            RoCursor::Node(n) => *n,
            RoCursor::Attribute { owner, .. } => *owner,
            RoCursor::Namespace { owner, .. } => *owner,
        }
    }

    /// Get document order key for comparison
    /// Returns (node_id, cursor_kind, sub_index)
    /// cursor_kind: 0 = node, 1 = namespace, 2 = attribute
    fn order_key(&self) -> (u32, u8, usize) {
        match &self.cursor {
            RoCursor::Node(n) => (n.id().get(), 0, 0),
            RoCursor::Namespace { owner, index, .. } => (owner.id().get(), 1, *index),
            RoCursor::Attribute { owner, index } => (owner.id().get(), 2, *index),
        }
    }

    /// Collect namespaces for a node based on scope
    fn collect_namespaces(
        &self,
        node: Node<'a, 'a>,
        scope: NamespaceAxisScope,
    ) -> Vec<(Option<String>, String)> {
        let mut result = Vec::new();

        match scope {
            NamespaceAxisScope::Local => {
                // Only locally declared namespaces
                // roxmltree's namespaces() returns all in-scope namespaces including inherited
                // We need to find which ones are NOT present in the parent
                let parent_prefixes: HashSet<Option<&str>> = if let Some(parent) = node.parent() {
                    if parent.is_element() {
                        parent.namespaces().map(|ns| ns.name()).collect()
                    } else {
                        HashSet::new()
                    }
                } else {
                    HashSet::new()
                };

                for ns in node.namespaces() {
                    let prefix = ns.name();
                    // If this prefix exists in parent, check if URI changed (redeclaration)
                    // If prefix doesn't exist in parent, it's a new local declaration
                    let is_local = if parent_prefixes.contains(&prefix) {
                        // Check if this is a redeclaration with different URI
                        if let Some(parent) = node.parent() {
                            parent.namespaces()
                                .find(|p_ns| p_ns.name() == prefix)
                                .map(|p_ns| p_ns.uri() != ns.uri())
                                .unwrap_or(true)
                        } else {
                            true
                        }
                    } else {
                        // Prefix not in parent, so it's locally declared
                        true
                    };

                    if is_local {
                        result.push((prefix.map(String::from), ns.uri().to_string()));
                    }
                }
            }
            NamespaceAxisScope::All | NamespaceAxisScope::ExcludeXml => {
                // All in-scope namespaces - roxmltree already provides this
                let mut seen_prefixes: HashSet<Option<&str>> = HashSet::new();

                for ns in node.namespaces() {
                    let prefix = ns.name();
                    if !seen_prefixes.contains(&prefix) {
                        seen_prefixes.insert(prefix);
                        result.push((prefix.map(String::from), ns.uri().to_string()));
                    }
                }

                // Filter out xml namespace if ExcludeXml
                if scope == NamespaceAxisScope::ExcludeXml {
                    result.retain(|(prefix, _)| {
                        prefix.as_ref().map(|p| p != "xml").unwrap_or(true)
                    });
                }
            }
        }

        result
    }

    /// Try to move to a matching node in document order
    fn try_move_to_matching(
        &mut self,
        start: Node<'a, 'a>,
        kind: DomNodeType,
        end_id: Option<u32>,
    ) -> bool {
        // Check if we've reached the end boundary
        if let Some(end) = end_id {
            if start.id().get() >= end {
                return false;
            }
        }

        // Check if current node matches
        let node_type = match start.node_type() {
            NodeType::Root => DomNodeType::Root,
            NodeType::Element => DomNodeType::Element,
            NodeType::Text => DomNodeType::Text,
            NodeType::Comment => DomNodeType::Comment,
            NodeType::PI => DomNodeType::ProcessingInstruction,
        };

        if kind == DomNodeType::All || kind == node_type {
            self.cursor = RoCursor::Node(start);
            return true;
        }

        // Check descendants
        for descendant in start.descendants().skip(1) {
            if let Some(end) = end_id {
                if descendant.id().get() >= end {
                    return false;
                }
            }

            let desc_type = match descendant.node_type() {
                NodeType::Root => DomNodeType::Root,
                NodeType::Element => DomNodeType::Element,
                NodeType::Text => DomNodeType::Text,
                NodeType::Comment => DomNodeType::Comment,
                NodeType::PI => DomNodeType::ProcessingInstruction,
            };

            if kind == DomNodeType::All || kind == desc_type {
                self.cursor = RoCursor::Node(descendant);
                return true;
            }
        }

        false
    }
}

impl<'a> DomNavigator for RoXmlNavigator<'a> {
    fn is_same_position(&self, other: &Self) -> bool {
        match (&self.cursor, &other.cursor) {
            (RoCursor::Node(a), RoCursor::Node(b)) => a.id() == b.id(),
            (
                RoCursor::Attribute {
                    owner: a,
                    index: i,
                },
                RoCursor::Attribute {
                    owner: b,
                    index: j,
                },
            ) => a.id() == b.id() && i == j,
            (
                RoCursor::Namespace {
                    owner: a, index: i, ..
                },
                RoCursor::Namespace {
                    owner: b, index: j, ..
                },
            ) => a.id() == b.id() && i == j,
            _ => false,
        }
    }

    fn compare_position(&self, other: &Self) -> XmlNodeOrder {
        // Check same document (by comparing document pointers)
        if !std::ptr::eq(self.doc, other.doc) {
            return XmlNodeOrder::Unknown;
        }

        let self_key = self.order_key();
        let other_key = other.order_key();

        match self_key.cmp(&other_key) {
            std::cmp::Ordering::Less => XmlNodeOrder::Before,
            std::cmp::Ordering::Greater => XmlNodeOrder::After,
            std::cmp::Ordering::Equal => XmlNodeOrder::Same,
        }
    }

    fn move_to(&mut self, other: &Self) -> bool {
        self.cursor = other.cursor.clone();
        self.name_cache.clear();
        true
    }

    fn move_to_root(&mut self) {
        self.cursor = RoCursor::Node(self.doc.root());
        self.name_cache.clear();
    }

    fn move_to_parent(&mut self) -> bool {
        match &self.cursor {
            RoCursor::Node(n) => {
                if let Some(parent) = n.parent() {
                    self.cursor = RoCursor::Node(parent);
                    self.name_cache.clear();
                    true
                } else {
                    false
                }
            }
            RoCursor::Attribute { owner, .. } | RoCursor::Namespace { owner, .. } => {
                self.cursor = RoCursor::Node(*owner);
                self.name_cache.clear();
                true
            }
        }
    }

    fn move_to_first_child(&mut self) -> bool {
        match &self.cursor {
            RoCursor::Node(n) => {
                if let Some(child) = n.first_child() {
                    self.cursor = RoCursor::Node(child);
                    self.name_cache.clear();
                    true
                } else {
                    false
                }
            }
            _ => false,
        }
    }

    fn move_to_next_sibling(&mut self) -> bool {
        match &self.cursor {
            RoCursor::Node(n) => {
                if let Some(sibling) = n.next_sibling() {
                    self.cursor = RoCursor::Node(sibling);
                    self.name_cache.clear();
                    true
                } else {
                    false
                }
            }
            _ => false,
        }
    }

    fn move_to_prev_sibling(&mut self) -> bool {
        match &self.cursor {
            RoCursor::Node(n) => {
                if let Some(sibling) = n.prev_sibling() {
                    self.cursor = RoCursor::Node(sibling);
                    self.name_cache.clear();
                    true
                } else {
                    false
                }
            }
            _ => false,
        }
    }

    fn move_to_first_attribute(&mut self) -> bool {
        match &self.cursor {
            RoCursor::Node(n) if n.is_element() => {
                if n.attributes().len() > 0 {
                    self.cursor = RoCursor::Attribute {
                        owner: *n,
                        index: 0,
                    };
                    self.name_cache.clear();
                    true
                } else {
                    false
                }
            }
            _ => false,
        }
    }

    fn move_to_next_attribute(&mut self) -> bool {
        match &self.cursor {
            RoCursor::Attribute { owner, index } => {
                let next_index = index + 1;
                if next_index < owner.attributes().len() {
                    self.cursor = RoCursor::Attribute {
                        owner: *owner,
                        index: next_index,
                    };
                    self.name_cache.clear();
                    true
                } else {
                    false
                }
            }
            _ => false,
        }
    }

    fn move_to_first_namespace(&mut self, scope: NamespaceAxisScope) -> bool {
        match &self.cursor {
            RoCursor::Node(n) if n.is_element() => {
                let namespaces = self.collect_namespaces(*n, scope);
                if !namespaces.is_empty() {
                    self.cursor = RoCursor::Namespace {
                        owner: *n,
                        index: 0,
                        namespaces,
                    };
                    self.name_cache.clear();
                    true
                } else {
                    false
                }
            }
            _ => false,
        }
    }

    fn move_to_next_namespace(&mut self, _scope: NamespaceAxisScope) -> bool {
        match &self.cursor {
            RoCursor::Namespace {
                owner,
                index,
                namespaces,
            } => {
                let next_index = index + 1;
                if next_index < namespaces.len() {
                    self.cursor = RoCursor::Namespace {
                        owner: *owner,
                        index: next_index,
                        namespaces: namespaces.clone(),
                    };
                    self.name_cache.clear();
                    true
                } else {
                    false
                }
            }
            _ => false,
        }
    }

    fn move_to_following(&mut self, kind: DomNodeType, end: Option<&Self>) -> bool {
        // Start from the current node (or owner if on attribute/namespace)
        let start = self.owner_node();
        let end_id = end.map(|e| e.owner_node().id().get());

        // First try siblings of current node
        let mut current = start;

        loop {
            // Try next sibling and its descendants
            if let Some(next) = current.next_sibling() {
                if self.try_move_to_matching(next, kind, end_id) {
                    return true;
                }
                current = next;
            } else if let Some(parent) = current.parent() {
                // No more siblings, go up to parent and try its next sibling
                current = parent;
            } else {
                // Reached root without finding a match
                break;
            }
        }

        false
    }

    fn node_type(&self) -> DomNodeType {
        match &self.cursor {
            RoCursor::Node(n) => match n.node_type() {
                NodeType::Root => DomNodeType::Root,
                NodeType::Element => DomNodeType::Element,
                NodeType::Text => DomNodeType::Text,
                NodeType::Comment => DomNodeType::Comment,
                NodeType::PI => DomNodeType::ProcessingInstruction,
            },
            RoCursor::Attribute { .. } => DomNodeType::Attribute,
            RoCursor::Namespace { .. } => DomNodeType::Namespace,
        }
    }

    fn local_name(&self) -> &str {
        match &self.cursor {
            RoCursor::Node(n) => {
                // For PI nodes, the local name is the target
                if let Some(pi) = n.pi() {
                    pi.target
                } else {
                    n.tag_name().name()
                }
            }
            RoCursor::Attribute { owner, index } => owner
                .attributes()
                .nth(*index)
                .map(|a| a.name())
                .unwrap_or(""),
            RoCursor::Namespace {
                namespaces, index, ..
            } => namespaces
                .get(*index)
                .and_then(|(prefix, _)| prefix.as_deref())
                .unwrap_or(""), // Empty string for default namespace
        }
    }

    fn name(&self) -> &str {
        // Build the qualified name (prefix:local)
        // Note: This method has a mutable borrow issue - we'll return local_name for now
        // In a proper implementation, we'd cache this
        match &self.cursor {
            RoCursor::Node(n) => {
                // For PI nodes, the name is the target
                if let Some(pi) = n.pi() {
                    pi.target
                } else {
                    // roxmltree doesn't provide a combined qualified name directly
                    // Return local name for now (proper impl would use name_cache)
                    n.tag_name().name()
                }
            }
            RoCursor::Attribute { owner, index } => owner
                .attributes()
                .nth(*index)
                .map(|a| a.name())
                .unwrap_or(""),
            RoCursor::Namespace {
                namespaces, index, ..
            } => namespaces
                .get(*index)
                .and_then(|(prefix, _)| prefix.as_deref())
                .unwrap_or(""),
        }
    }

    fn namespace_uri(&self) -> &str {
        match &self.cursor {
            RoCursor::Node(n) => n.tag_name().namespace().unwrap_or(""),
            RoCursor::Attribute { owner, index } => owner
                .attributes()
                .nth(*index)
                .and_then(|a| a.namespace())
                .unwrap_or(""),
            RoCursor::Namespace { .. } => {
                // Namespace nodes themselves don't have a namespace URI
                ""
            }
        }
    }

    fn prefix(&self) -> &str {
        match &self.cursor {
            RoCursor::Node(n) => {
                // roxmltree provides lookup_prefix for a URI, but not the prefix of the current element
                // We need to extract it from the raw tag name if present
                if n.is_element() {
                    // The tag_name().name() returns local name only
                    // We don't have direct access to the prefix in roxmltree's API
                    // Return empty string for now
                    ""
                } else {
                    ""
                }
            }
            RoCursor::Attribute { owner, index } => {
                // roxmltree Attribute doesn't expose prefix directly
                let _attr = owner.attributes().nth(*index);
                ""
            }
            RoCursor::Namespace { .. } => "",
        }
    }

    fn value(&self) -> String {
        match &self.cursor {
            RoCursor::Node(n) => match n.node_type() {
                NodeType::Text | NodeType::Comment => n.text().unwrap_or("").to_string(),
                NodeType::PI => {
                    // PI value is the content after the target
                    n.text().unwrap_or("").to_string()
                }
                NodeType::Element | NodeType::Root => {
                    // String value is concatenation of all text descendants
                    let mut result = String::new();
                    for descendant in n.descendants() {
                        if descendant.is_text() {
                            if let Some(text) = descendant.text() {
                                result.push_str(text);
                            }
                        }
                    }
                    result
                }
            },
            RoCursor::Attribute { owner, index } => owner
                .attributes()
                .nth(*index)
                .map(|a| a.value().to_string())
                .unwrap_or_default(),
            RoCursor::Namespace {
                namespaces, index, ..
            } => namespaces
                .get(*index)
                .map(|(_, uri)| uri.clone())
                .unwrap_or_default(),
        }
    }

    fn base_uri(&self) -> &str {
        &self.base_uri
    }

    fn schema_type(&self) -> Option<SimpleTypeKey> {
        // roxmltree is schema-unaware, but can be overridden for testing
        #[cfg(test)]
        {
            if self.schema_type_override.is_some() {
                return self.schema_type_override;
            }
        }
        None
    }

    fn typed_value(&self) -> Option<XmlValue> {
        // roxmltree is schema-unaware
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(xml: &str) -> Document<'_> {
        Document::parse(xml).unwrap()
    }

    #[test]
    fn test_navigator_at_root() {
        let doc = parse("<root/>");
        let nav = RoXmlNavigator::new(&doc);
        assert_eq!(nav.node_type(), DomNodeType::Root);
    }

    #[test]
    fn test_move_to_first_child() {
        let doc = parse("<root><child/></root>");
        let mut nav = RoXmlNavigator::new(&doc);

        // Move from root to document element
        assert!(nav.move_to_first_child());
        assert_eq!(nav.node_type(), DomNodeType::Element);
        assert_eq!(nav.local_name(), "root");

        // Move to child element
        assert!(nav.move_to_first_child());
        assert_eq!(nav.local_name(), "child");
    }

    #[test]
    fn test_move_to_parent() {
        let doc = parse("<root><child/></root>");
        let mut nav = RoXmlNavigator::new(&doc);

        nav.move_to_first_child(); // to root element
        nav.move_to_first_child(); // to child

        assert!(nav.move_to_parent());
        assert_eq!(nav.local_name(), "root");

        assert!(nav.move_to_parent());
        assert_eq!(nav.node_type(), DomNodeType::Root);
    }

    #[test]
    fn test_sibling_navigation() {
        let doc = parse("<root><a/><b/><c/></root>");
        let mut nav = RoXmlNavigator::new(&doc);

        nav.move_to_first_child(); // to root
        nav.move_to_first_child(); // to a

        assert_eq!(nav.local_name(), "a");
        assert!(nav.move_to_next_sibling());
        assert_eq!(nav.local_name(), "b");
        assert!(nav.move_to_next_sibling());
        assert_eq!(nav.local_name(), "c");
        assert!(!nav.move_to_next_sibling());

        assert!(nav.move_to_prev_sibling());
        assert_eq!(nav.local_name(), "b");
    }

    #[test]
    fn test_attribute_navigation() {
        let doc = parse(r#"<root attr1="v1" attr2="v2"/>"#);
        let mut nav = RoXmlNavigator::new(&doc);

        nav.move_to_first_child(); // to root

        assert!(nav.move_to_first_attribute());
        assert_eq!(nav.node_type(), DomNodeType::Attribute);
        assert_eq!(nav.local_name(), "attr1");
        assert_eq!(nav.value(), "v1");

        assert!(nav.move_to_next_attribute());
        assert_eq!(nav.local_name(), "attr2");
        assert_eq!(nav.value(), "v2");

        assert!(!nav.move_to_next_attribute());

        // Return to parent element
        assert!(nav.move_to_parent());
        assert_eq!(nav.node_type(), DomNodeType::Element);
    }

    #[test]
    fn test_namespace_navigation_local() {
        let doc = parse(r#"<root xmlns="http://default" xmlns:p="http://prefixed"/>"#);
        let mut nav = RoXmlNavigator::new(&doc);

        nav.move_to_first_child(); // to root

        assert!(nav.move_to_first_namespace(NamespaceAxisScope::Local));
        assert_eq!(nav.node_type(), DomNodeType::Namespace);

        // Collect all namespaces
        let mut uris = HashSet::new();
        uris.insert(nav.value());

        while nav.move_to_next_namespace(NamespaceAxisScope::Local) {
            uris.insert(nav.value());
        }

        // Should have two namespaces
        assert!(uris.contains("http://default"));
        assert!(uris.contains("http://prefixed"));
    }

    #[test]
    fn test_element_string_value() {
        let doc = parse("<root>Hello <b>World</b>!</root>");
        let mut nav = RoXmlNavigator::new(&doc);

        nav.move_to_first_child();
        assert_eq!(nav.value(), "Hello World!");
    }

    #[test]
    fn test_is_same_position() {
        let doc = parse("<root><a/><b/></root>");
        let mut nav1 = RoXmlNavigator::new(&doc);
        let mut nav2 = RoXmlNavigator::new(&doc);

        assert!(nav1.is_same_position(&nav2));

        nav1.move_to_first_child();
        assert!(!nav1.is_same_position(&nav2));

        nav2.move_to_first_child();
        assert!(nav1.is_same_position(&nav2));
    }

    #[test]
    fn test_compare_position() {
        let doc = parse("<root><a/><b/></root>");
        let mut nav1 = RoXmlNavigator::new(&doc);
        let mut nav2 = RoXmlNavigator::new(&doc);

        nav1.move_to_first_child(); // to root
        nav1.move_to_first_child(); // to a

        nav2.move_to_first_child(); // to root
        nav2.move_to_first_child(); // to a
        nav2.move_to_next_sibling(); // to b

        assert_eq!(nav1.compare_position(&nav2), XmlNodeOrder::Before);
        assert_eq!(nav2.compare_position(&nav1), XmlNodeOrder::After);
        assert_eq!(nav1.compare_position(&nav1), XmlNodeOrder::Same);
    }

    #[test]
    fn test_move_to() {
        let doc = parse("<root><a/><b/></root>");
        let mut nav1 = RoXmlNavigator::new(&doc);
        let mut nav2 = RoXmlNavigator::new(&doc);

        nav2.move_to_first_child();
        nav2.move_to_first_child();
        nav2.move_to_next_sibling(); // nav2 at <b/>

        assert!(nav1.move_to(&nav2));
        assert!(nav1.is_same_position(&nav2));
        assert_eq!(nav1.local_name(), "b");
    }

    #[test]
    fn test_clone() {
        let doc = parse("<root><a/></root>");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child();

        let nav_clone = nav.clone();
        assert!(nav.is_same_position(&nav_clone));

        nav.move_to_first_child();
        assert!(!nav.is_same_position(&nav_clone));
    }

    #[test]
    fn test_typed_value_returns_none() {
        let doc = parse("<root>text</root>");
        let nav = RoXmlNavigator::new(&doc);

        assert!(nav.schema_type().is_none());
        assert!(nav.typed_value().is_none());
    }

    #[test]
    fn test_atomized_value() {
        let doc = parse("<root>text</root>");
        let mut nav = RoXmlNavigator::new(&doc);

        nav.move_to_first_child();
        let value = nav.atomized_value();

        // Should be untyped atomic
        assert!(value.is_untyped());
        assert_eq!(value.to_string_value(), "text");
    }

    #[test]
    fn test_namespaced_element() {
        let doc = parse(r#"<root xmlns:ns="http://example.com"><ns:child/></root>"#);
        let mut nav = RoXmlNavigator::new(&doc);

        nav.move_to_first_child(); // root
        nav.move_to_first_child(); // ns:child

        assert_eq!(nav.local_name(), "child");
        assert_eq!(nav.namespace_uri(), "http://example.com");
    }

    #[test]
    fn test_text_node() {
        let doc = parse("<root>hello</root>");
        let mut nav = RoXmlNavigator::new(&doc);

        nav.move_to_first_child(); // root
        nav.move_to_first_child(); // text node

        assert_eq!(nav.node_type(), DomNodeType::Text);
        assert_eq!(nav.value(), "hello");
    }

    #[test]
    fn test_comment_node() {
        let doc = parse("<root><!-- comment --></root>");
        let mut nav = RoXmlNavigator::new(&doc);

        nav.move_to_first_child(); // root
        nav.move_to_first_child(); // comment node

        assert_eq!(nav.node_type(), DomNodeType::Comment);
        assert_eq!(nav.value(), " comment ");
    }

    #[test]
    fn test_processing_instruction() {
        let doc = parse("<root><?target data?></root>");
        let mut nav = RoXmlNavigator::new(&doc);

        nav.move_to_first_child(); // root
        nav.move_to_first_child(); // PI node

        assert_eq!(nav.node_type(), DomNodeType::ProcessingInstruction);
        assert_eq!(nav.local_name(), "target");
    }

    #[test]
    fn test_no_attributes() {
        let doc = parse("<root/>");
        let mut nav = RoXmlNavigator::new(&doc);

        nav.move_to_first_child(); // root

        assert!(!nav.move_to_first_attribute());
    }

    #[test]
    fn test_no_namespaces() {
        let doc = parse("<root/>");
        let mut nav = RoXmlNavigator::new(&doc);

        nav.move_to_first_child(); // root

        assert!(!nav.move_to_first_namespace(NamespaceAxisScope::Local));
    }

    #[test]
    fn test_has_attributes_helper() {
        let doc = parse(r#"<root attr="val"/>"#);
        let mut nav = RoXmlNavigator::new(&doc);

        nav.move_to_first_child(); // root

        assert!(nav.has_attributes());
        // Should be back at element
        assert_eq!(nav.node_type(), DomNodeType::Element);
    }

    #[test]
    fn test_has_children_helper() {
        let doc = parse("<root><child/></root>");
        let mut nav = RoXmlNavigator::new(&doc);

        nav.move_to_first_child(); // root

        assert!(nav.has_children());
        // Should be back at element
        assert_eq!(nav.node_type(), DomNodeType::Element);
        assert_eq!(nav.local_name(), "root");
    }

    #[test]
    fn test_move_to_child_kind() {
        let doc = parse("<root>text<child/></root>");
        let mut nav = RoXmlNavigator::new(&doc);

        nav.move_to_first_child(); // root

        // Find element child
        assert!(nav.move_to_child_kind(DomNodeType::Element));
        assert_eq!(nav.local_name(), "child");

        nav.move_to_parent();

        // Find text child
        assert!(nav.move_to_child_kind(DomNodeType::Text));
        assert_eq!(nav.value(), "text");
    }

    #[test]
    fn test_move_to_child_name() {
        let doc = parse("<root><a/><b/><c/></root>");
        let mut nav = RoXmlNavigator::new(&doc);

        nav.move_to_first_child(); // root

        assert!(nav.move_to_child_name("b", ""));
        assert_eq!(nav.local_name(), "b");
    }

    #[test]
    fn test_different_documents_compare() {
        let doc1 = parse("<root1/>");
        let doc2 = parse("<root2/>");

        let nav1 = RoXmlNavigator::new(&doc1);
        let nav2 = RoXmlNavigator::new(&doc2);

        assert_eq!(nav1.compare_position(&nav2), XmlNodeOrder::Unknown);
    }

    #[test]
    fn test_inherited_namespaces() {
        let doc = parse(r#"<root xmlns:ns="http://example.com"><child xmlns:local="http://local.com"/></root>"#);
        let mut nav = RoXmlNavigator::new(&doc);

        nav.move_to_first_child(); // root
        nav.move_to_first_child(); // child

        // Child should see inherited namespace with All scope
        // Both ns and local should be visible
        assert!(nav.move_to_first_namespace(NamespaceAxisScope::All));

        // Collect all namespace URIs in All scope
        let mut all_uris = HashSet::new();
        all_uris.insert(nav.value());
        while nav.move_to_next_namespace(NamespaceAxisScope::All) {
            all_uris.insert(nav.value());
        }

        // Should have both inherited (ns) and local (local) namespaces
        assert!(all_uris.contains("http://example.com"), "Should see inherited namespace");
        assert!(all_uris.contains("http://local.com"), "Should see local namespace");

        // Now test Local scope - should only have local namespace
        let mut nav2 = RoXmlNavigator::new(&doc);
        nav2.move_to_first_child(); // root
        nav2.move_to_first_child(); // child

        assert!(nav2.move_to_first_namespace(NamespaceAxisScope::Local));

        // Collect all local namespace URIs
        let mut local_uris = HashSet::new();
        local_uris.insert(nav2.value());
        while nav2.move_to_next_namespace(NamespaceAxisScope::Local) {
            local_uris.insert(nav2.value());
        }

        // Should only have the local namespace, not inherited
        assert!(local_uris.contains("http://local.com"), "Should see local namespace");
        // The inherited namespace from root should NOT be visible in Local scope
        assert!(!local_uris.contains("http://example.com"), "Should NOT see inherited namespace in Local scope");
    }
}
