//! Cursor-based [`DomNavigator`] over [`BufferDocument`].
//!
//! [`BufferDocNavigator`] is a lightweight, cloneable cursor that enables
//! XPath 2.0 evaluation over the flat node array.  It implements a
//! three-state cursor model:
//!
//! | State | `virtual_parent` | `current_ns` | Meaning |
//! |-------|------------------|--------------|---------|
//! | Real node | `NULL` | `NsRef::NULL` | Main array node |
//! | Attribute | element ref | `NsRef::NULL` | Attribute virtual node |
//! | Namespace | element ref | non-NULL | Namespace virtual node |

use std::collections::HashSet;

use crate::ids::{NameId, SimpleTypeKey, TypeKey};
use crate::navigator::{
    DomNavigator, DomNodeType, NamespaceAxisScope, NavigatorError, XmlNodeOrder,
};
use crate::types::value::XmlValue;

use super::buffer::BufferDocument;
use super::node::{Node, NodeType};
use super::type_remap::NodeSchemaBinding;
use super::{NsRef, NULL};

/// Lightweight cursor for XPath navigation over [`BufferDocument`].
#[derive(Clone)]
pub struct BufferDocNavigator<'a> {
    doc: &'a BufferDocument<'a>,
    /// Current node position in the main node array.
    current: u32,
    /// Non-NULL when positioned on an attribute or namespace (= owning element).
    virtual_parent: u32,
    /// Non-NULL when positioned on a namespace node.
    current_ns: NsRef,
    /// Sub-index for document-order comparison of virtual nodes.
    attr_index: u16,
    /// Collected namespaces for All/ExcludeXml traversal.
    ns_list: Vec<NsRef>,
}

impl<'a> BufferDocNavigator<'a> {
    /// Creates a navigator positioned at the given node.
    pub fn new(doc: &'a BufferDocument<'a>, node: u32) -> Self {
        Self {
            doc,
            current: node,
            virtual_parent: NULL,
            current_ns: NsRef::NULL,
            attr_index: 0,
            ns_list: Vec::new(),
        }
    }

    /// Returns the underlying document.
    #[inline]
    pub fn document(&self) -> &'a BufferDocument<'a> {
        self.doc
    }

    /// Returns the current flat node index.
    #[inline]
    pub fn current_ref(&self) -> u32 {
        self.current
    }

    // ── Internal helpers ──────────────────────────────────────────────

    #[inline]
    fn is_on_namespace(&self) -> bool {
        !self.current_ns.is_null()
    }

    #[inline]
    fn is_on_attribute(&self) -> bool {
        self.virtual_parent != NULL && self.current_ns.is_null()
    }

    #[inline]
    fn node(&self) -> Node {
        self.doc.nodes.get(self.current)
    }

    fn ns_node(&self) -> super::NamespaceNode {
        self.doc.namespace_pages.get(self.current_ns)
    }

    fn clear_virtual(&mut self) {
        self.virtual_parent = NULL;
        self.current_ns = NsRef::NULL;
        self.attr_index = 0;
        self.ns_list.clear();
    }

    /// Restore cursor state from saved values (for position-unchanged-on-false).
    fn restore_cursor(&mut self, current: u32, virtual_parent: u32, current_ns: NsRef, attr_index: u16) {
        self.current = current;
        self.virtual_parent = virtual_parent;
        self.current_ns = current_ns;
        self.attr_index = attr_index;
        self.ns_list.clear();
    }

    /// Document-order key for position comparison.
    ///
    /// Within an element: namespaces (1) < attributes (2) < real children (0 at higher index).
    fn order_key(&self) -> (u32, u8, u16) {
        if self.is_on_namespace() {
            (self.virtual_parent, 1, self.attr_index)
        } else if self.is_on_attribute() {
            (self.virtual_parent, 2, self.attr_index)
        } else {
            (self.current, 0, 0)
        }
    }

    fn matches_kind(&self, kind: DomNodeType) -> bool {
        kind == DomNodeType::All || self.node_type() == kind
    }

    fn past_end(&self, end: Option<&Self>) -> bool {
        if let Some(end) = end {
            self.compare_position(end) != XmlNodeOrder::Before
        } else {
            false
        }
    }

    /// Concatenates all descendant text content (XPath string-value for elements/root).
    fn compute_element_value(&self) -> String {
        let end = self.doc.subtree_end(self.current);
        let mut result = String::new();
        let mut i = self.current + 1;
        while i < end {
            let node = self.doc.nodes.get(i);
            match node.node_type() {
                NodeType::Text | NodeType::Whitespace | NodeType::SignificantWhitespace => {
                    result.push_str(self.doc.strings.get(node.value));
                }
                _ => {}
            }
            i += 1;
        }
        result
    }

    /// Walks ancestors looking for `xml:base` attribute.
    fn resolve_base_uri(&self) -> &str {
        let mut nav = self.clone();
        loop {
            let node = nav.node();
            if node.node_type() == NodeType::Element && node.has_flag(Node::HAS_ATTRIBUTE) {
                let mut attr = nav.current + 1;
                loop {
                    let attr_node = self.doc.nodes.get(attr);
                    if attr_node.node_type() != NodeType::Attribute {
                        break;
                    }
                    let qname = self.doc.qname_table.get(attr_node.value);
                    let local = self.doc.names.resolve_ref(qname.local_name);
                    if local == "base" {
                        let ns = self.doc.names.resolve_ref(qname.namespace_uri);
                        if ns == "http://www.w3.org/XML/1998/namespace" {
                            let val_node = self.doc.nodes.get(attr + 1);
                            return self.doc.strings.get(val_node.value);
                        }
                    }
                    if attr_node.next_sibling == NULL {
                        break;
                    }
                    attr = attr_node.next_sibling;
                }
            }
            if !nav.move_to_parent() {
                break;
            }
        }
        ""
    }

    /// Finds the NsRef boundary for Local namespace scope on `elem`.
    ///
    /// Walks ancestors to find the nearest element with `HAS_NMSP_DECLS`
    /// and returns its namespace chain head.  Returns `NsRef::NULL` if no
    /// ancestor has namespace declarations.
    fn find_local_ns_boundary(&self, elem: u32) -> NsRef {
        let mut cursor = self.doc.nodes.get(elem).parent;
        while cursor != NULL {
            let node = self.doc.nodes.get(cursor);
            if node.node_type() == NodeType::Element && node.has_flag(Node::HAS_NMSP_DECLS) {
                if let Some(&head) = self.doc.element_namespaces.get(&cursor) {
                    return head;
                }
            }
            cursor = node.parent;
        }
        NsRef::NULL
    }

    /// Collects namespace NsRefs for the given scope.
    ///
    /// - **Local**: Only namespace nodes declared on `elem` itself (stops at
    ///   the nearest ancestor's namespace chain boundary).
    /// - **All**: All in-scope namespaces including inherited, plus the
    ///   implicit `xml:` binding.
    /// - **ExcludeXml**: Like All but without the `xml:` namespace.
    fn collect_namespaces(
        &self,
        elem: u32,
        scope: NamespaceAxisScope,
    ) -> Vec<NsRef> {
        match scope {
            NamespaceAxisScope::Local => {
                let node = self.doc.nodes.get(elem);
                if !node.has_flag(Node::HAS_NMSP_DECLS) {
                    return Vec::new();
                }
                let head = match self.doc.element_namespaces.get(&elem) {
                    Some(&h) => h,
                    None => return Vec::new(),
                };
                let boundary = self.find_local_ns_boundary(elem);
                let mut result = Vec::new();
                let mut ns_ref = head;
                while !ns_ref.is_null() && ns_ref != boundary {
                    result.push(ns_ref);
                    ns_ref = self.doc.namespace_pages.get(ns_ref).next;
                }
                result
            }
            NamespaceAxisScope::All | NamespaceAxisScope::ExcludeXml => {
                let mut result = Vec::new();
                let mut seen_prefixes: HashSet<NameId> = HashSet::new();

                let mut cursor = elem;
                loop {
                    let node = self.doc.nodes.get(cursor);
                    if node.node_type() == NodeType::Element
                        && node.has_flag(Node::HAS_NMSP_DECLS)
                    {
                        if let Some(&ns_head) = self.doc.element_namespaces.get(&cursor) {
                            let mut ns_ref = ns_head;
                            while !ns_ref.is_null() {
                                let ns_node = self.doc.namespace_pages.get(ns_ref);
                                if seen_prefixes.insert(ns_node.prefix) {
                                    if scope == NamespaceAxisScope::ExcludeXml {
                                        let prefix_str =
                                            self.doc.names.resolve_ref(ns_node.prefix);
                                        if prefix_str == "xml" {
                                            ns_ref = ns_node.next;
                                            continue;
                                        }
                                    }
                                    result.push(ns_ref);
                                }
                                ns_ref = ns_node.next;
                            }
                        }
                    }
                    if node.parent == NULL {
                        break;
                    }
                    cursor = node.parent;
                }

                // For All scope: append implicit xml: namespace if not yet seen
                if scope == NamespaceAxisScope::All {
                    let xml_ns = self.doc.xml_namespace;
                    if !xml_ns.is_null() {
                        let xml_node = self.doc.namespace_pages.get(xml_ns);
                        if seen_prefixes.insert(xml_node.prefix) {
                            result.push(xml_ns);
                        }
                    }
                }

                result
            }
        }
    }

    // ── Schema binding accessors ──────────────────────────────────────

    /// Returns the full [`TypeKey`] from the current node's schema binding.
    ///
    /// Returns `None` when positioned on a namespace virtual node.
    pub fn element_type_key(&self) -> Option<TypeKey> {
        if self.is_on_namespace() {
            return None;
        }
        let idx = self.node().binding_index();
        self.doc.binding_remap.get(idx).map(|b| b.type_key)
    }

    /// Returns the full [`NodeSchemaBinding`] for the current node.
    ///
    /// Returns `None` when positioned on a namespace virtual node.
    pub fn schema_binding(&self) -> Option<&NodeSchemaBinding> {
        if self.is_on_namespace() {
            return None;
        }
        let idx = self.node().binding_index();
        self.doc.binding_remap.get(idx)
    }
}

// ── DomNavigator impl ─────────────────────────────────────────────────

impl<'a> DomNavigator for BufferDocNavigator<'a> {
    fn is_same_position(&self, other: &Self) -> bool {
        self.current == other.current
            && self.virtual_parent == other.virtual_parent
            && self.current_ns == other.current_ns
            && self.attr_index == other.attr_index
    }

    fn compare_position(&self, other: &Self) -> XmlNodeOrder {
        if !std::ptr::eq(self.doc, other.doc) {
            let self_ptr = self.doc as *const _ as usize;
            let other_ptr = other.doc as *const _ as usize;
            return if self_ptr < other_ptr {
                XmlNodeOrder::Before
            } else {
                XmlNodeOrder::After
            };
        }
        match self.order_key().cmp(&other.order_key()) {
            std::cmp::Ordering::Less => XmlNodeOrder::Before,
            std::cmp::Ordering::Equal => XmlNodeOrder::Same,
            std::cmp::Ordering::Greater => XmlNodeOrder::After,
        }
    }

    fn move_to(&mut self, other: &Self) -> bool {
        self.current = other.current;
        self.virtual_parent = other.virtual_parent;
        self.current_ns = other.current_ns;
        self.attr_index = other.attr_index;
        self.ns_list.clone_from(&other.ns_list);
        true
    }

    fn move_to_root(&mut self) {
        self.current = self.doc.root;
        self.clear_virtual();
    }

    fn move_to_parent(&mut self) -> bool {
        if self.virtual_parent != NULL {
            self.current = self.virtual_parent;
            self.clear_virtual();
            return true;
        }
        let parent = self.node().parent;
        if parent == NULL {
            return false;
        }
        self.current = parent;
        true
    }

    fn move_to_first_child(&mut self) -> bool {
        if self.virtual_parent != NULL {
            return false;
        }
        if let Some(child) = self.doc.first_content_child_of(self.current) {
            self.current = child;
            true
        } else {
            false
        }
    }

    fn move_to_next_sibling(&mut self) -> bool {
        if self.virtual_parent != NULL {
            return false;
        }
        let sib = self.node().next_sibling;
        if sib == NULL {
            return false;
        }
        self.current = sib;
        true
    }

    fn move_to_prev_sibling(&mut self) -> bool {
        if self.virtual_parent != NULL {
            return false;
        }
        let parent_ref = self.node().parent;
        if parent_ref == NULL {
            return false;
        }
        let first = match self.doc.first_content_child_of(parent_ref) {
            Some(f) => f,
            None => return false,
        };
        if first == self.current {
            return false;
        }
        let mut child = first;
        loop {
            let next = self.doc.nodes.get(child).next_sibling;
            if next == self.current {
                self.current = child;
                return true;
            }
            if next == NULL {
                return false;
            }
            child = next;
        }
    }

    fn move_to_first_attribute(&mut self) -> bool {
        if self.virtual_parent != NULL {
            return false;
        }
        let node = self.node();
        if node.node_type() != NodeType::Element || !node.has_flag(Node::HAS_ATTRIBUTE) {
            return false;
        }
        let first_attr = self.current + 1;
        debug_assert_eq!(
            self.doc.nodes.get(first_attr).node_type(),
            NodeType::Attribute,
        );
        self.virtual_parent = self.current;
        self.current = first_attr;
        self.current_ns = NsRef::NULL;
        self.attr_index = 0;
        true
    }

    fn move_to_next_attribute(&mut self) -> bool {
        if !self.is_on_attribute() {
            return false;
        }
        let next = self.node().next_sibling;
        if next == NULL {
            return false;
        }
        if self.doc.nodes.get(next).node_type() != NodeType::Attribute {
            return false;
        }
        self.current = next;
        self.attr_index += 1;
        true
    }

    fn move_to_first_namespace(&mut self, scope: NamespaceAxisScope) -> bool {
        if self.virtual_parent != NULL {
            return false;
        }
        let elem = self.current;
        if self.doc.nodes.get(elem).node_type() != NodeType::Element {
            return false;
        }
        let collected = self.collect_namespaces(elem, scope);
        if collected.is_empty() {
            return false;
        }
        self.virtual_parent = elem;
        self.current = elem;
        self.current_ns = collected[0];
        self.attr_index = 0;
        self.ns_list = collected;
        true
    }

    fn move_to_next_namespace(&mut self, _scope: NamespaceAxisScope) -> bool {
        if !self.is_on_namespace() {
            return false;
        }
        let next_idx = self.attr_index as usize + 1;
        if next_idx >= self.ns_list.len() {
            return false;
        }
        self.attr_index = next_idx as u16;
        self.current_ns = self.ns_list[next_idx];
        true
    }

    fn move_to_following(&mut self, kind: DomNodeType, end: Option<&Self>) -> bool {
        // Save cursor so we can restore on failure (position unchanged on false).
        let saved_current = self.current;
        let saved_virtual_parent = self.virtual_parent;
        let saved_current_ns = self.current_ns;
        let saved_attr_index = self.attr_index;

        // Following axis: skip descendants of the current node.
        // First escape to the next sibling, or walk up to an ancestor's
        // next sibling, then do a depth-first scan from there.
        if self.virtual_parent != NULL {
            // On attribute/namespace — move back to the owning element first
            self.current = self.virtual_parent;
            self.clear_virtual();
        }

        // Escape the current subtree: find next sibling or ancestor's next sibling
        loop {
            if self.move_to_next_sibling() {
                break;
            }
            if !self.move_to_parent() {
                self.restore_cursor(saved_current, saved_virtual_parent, saved_current_ns, saved_attr_index);
                return false;
            }
        }

        // Now do a depth-first walk from here (this node and its descendants
        // are all in the following axis).
        loop {
            if self.matches_kind(kind) {
                if self.past_end(end) {
                    self.restore_cursor(saved_current, saved_virtual_parent, saved_current_ns, saved_attr_index);
                    return false;
                }
                return true;
            }
            // Depth-first: child, then sibling, then ancestor's sibling
            if self.move_to_first_child() {
                continue;
            }
            if self.move_to_next_sibling() {
                continue;
            }
            loop {
                if !self.move_to_parent() {
                    self.restore_cursor(saved_current, saved_virtual_parent, saved_current_ns, saved_attr_index);
                    return false;
                }
                if self.move_to_next_sibling() {
                    break;
                }
            }
        }
    }

    // ── Node information ──────────────────────────────────────────────

    fn node_type(&self) -> DomNodeType {
        if self.is_on_namespace() {
            return DomNodeType::Namespace;
        }
        DomNodeType::from(self.node().node_type())
    }

    fn local_name(&self) -> &str {
        if self.is_on_namespace() {
            let ns = self.ns_node();
            return self.doc.names.resolve_ref(ns.prefix);
        }
        let node = self.node();
        match node.node_type() {
            NodeType::Element | NodeType::Attribute => {
                let qname = self.doc.qname_table.get(node.value);
                self.doc.names.resolve_ref(qname.local_name)
            }
            NodeType::ProcessingInstruction => self.doc.strings.get(node.value),
            _ => "",
        }
    }

    fn name(&self) -> &str {
        if self.is_on_namespace() {
            let ns = self.ns_node();
            return self.doc.names.resolve_ref(ns.prefix);
        }
        let node = self.node();
        match node.node_type() {
            NodeType::Element | NodeType::Attribute => {
                let qname = self.doc.qname_table.get(node.value);
                self.doc.strings.get(qname.qualified_name_idx)
            }
            NodeType::ProcessingInstruction => self.doc.strings.get(node.value),
            _ => "",
        }
    }

    fn namespace_uri(&self) -> &str {
        if self.is_on_namespace() {
            // Namespace nodes themselves don't have a namespace URI
            return "";
        }
        let node = self.node();
        match node.node_type() {
            NodeType::Element | NodeType::Attribute => {
                let qname = self.doc.qname_table.get(node.value);
                self.doc.names.resolve_ref(qname.namespace_uri)
            }
            _ => "",
        }
    }

    fn prefix(&self) -> &str {
        if self.is_on_namespace() {
            return "";
        }
        let node = self.node();
        match node.node_type() {
            NodeType::Element | NodeType::Attribute => {
                let qname = self.doc.qname_table.get(node.value);
                self.doc.names.resolve_ref(qname.prefix)
            }
            _ => "",
        }
    }

    fn value(&self) -> String {
        if self.is_on_namespace() {
            // Namespace node value = the bound namespace URI
            let ns = self.ns_node();
            return self.doc.names.resolve_ref(ns.namespace_uri).to_string();
        }
        let node = self.node();
        match node.node_type() {
            NodeType::Element | NodeType::Root => self.compute_element_value(),
            NodeType::Attribute | NodeType::ProcessingInstruction => {
                let val_node = self.doc.nodes.get(self.current + 1);
                self.doc.strings.get(val_node.value).to_string()
            }
            NodeType::Text
            | NodeType::Whitespace
            | NodeType::SignificantWhitespace
            | NodeType::Comment => self.doc.strings.get(node.value).to_string(),
            _ => String::new(),
        }
    }

    fn base_uri(&self) -> &str {
        self.resolve_base_uri()
    }

    // ── Schema type hooks ─────────────────────────────────────────────

    fn schema_type(&self) -> Option<SimpleTypeKey> {
        if self.is_on_namespace() {
            return None;
        }
        let idx = self.node().binding_index();
        let binding = self.doc.binding_remap.get(idx)?;
        match binding.type_key {
            TypeKey::Simple(k) => Some(k),
            TypeKey::Complex(_) => None,
        }
    }

    fn typed_value(&self) -> Option<XmlValue> {
        if self.node().has_flag(Node::IS_NIL) {
            return None;
        }
        // Not yet available — requires parse_value/get_simple_content on SchemaSet (Step 8).
        None
    }

    fn find_element_by_id(&self, id: &str) -> Result<Option<Self>, NavigatorError> {
        Ok(self
            .doc
            .get_element_by_id(id)
            .map(|r| BufferDocNavigator::new(self.doc, r)))
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::namespace::NameTable;
    use bumpalo::Bump;

    fn build_doc<'a>(
        xml: &str,
        arena: &'a Bump,
        names: &'a NameTable,
    ) -> BufferDocument<'a> {
        BufferDocument::from_reader_default(xml.as_bytes(), arena, names).unwrap()
    }

    // ── 1. Basic navigation ──────────────────────────────────────────

    #[test]
    fn root_to_first_child_and_back() {
        let arena = Bump::new();
        let names = NameTable::new();
        let doc = build_doc("<root><child/></root>", &arena, &names);
        let mut nav = doc.create_navigator();

        assert_eq!(nav.node_type(), DomNodeType::Root);
        assert!(nav.move_to_first_child());
        assert_eq!(nav.node_type(), DomNodeType::Element);
        assert_eq!(nav.local_name(), "root");

        assert!(nav.move_to_first_child());
        assert_eq!(nav.local_name(), "child");

        assert!(nav.move_to_parent());
        assert_eq!(nav.local_name(), "root");

        assert!(nav.move_to_parent());
        assert_eq!(nav.node_type(), DomNodeType::Root);

        assert!(!nav.move_to_parent());
    }

    #[test]
    fn sibling_navigation() {
        let arena = Bump::new();
        let names = NameTable::new();
        let doc = build_doc("<root><a/><b/><c/></root>", &arena, &names);
        let mut nav = doc.create_navigator();

        nav.move_to_first_child(); // root
        nav.move_to_first_child(); // a

        assert_eq!(nav.local_name(), "a");
        assert!(nav.move_to_next_sibling());
        assert_eq!(nav.local_name(), "b");
        assert!(nav.move_to_next_sibling());
        assert_eq!(nav.local_name(), "c");
        assert!(!nav.move_to_next_sibling());
    }

    // ── 2. Attribute axis ────────────────────────────────────────────

    #[test]
    fn attribute_navigation() {
        let arena = Bump::new();
        let names = NameTable::new();
        let doc = build_doc(
            r#"<root attr1="v1" attr2="v2"/>"#,
            &arena,
            &names,
        );
        let mut nav = doc.create_navigator();
        nav.move_to_first_child(); // root

        assert!(nav.move_to_first_attribute());
        assert_eq!(nav.node_type(), DomNodeType::Attribute);
        assert_eq!(nav.local_name(), "attr1");
        assert_eq!(nav.value(), "v1");

        assert!(nav.move_to_next_attribute());
        assert_eq!(nav.local_name(), "attr2");
        assert_eq!(nav.value(), "v2");

        assert!(!nav.move_to_next_attribute());

        assert!(nav.move_to_parent());
        assert_eq!(nav.node_type(), DomNodeType::Element);
        assert_eq!(nav.local_name(), "root");
    }

    // ── 3. Namespace axis ────────────────────────────────────────────

    #[test]
    fn namespace_local() {
        let arena = Bump::new();
        let names = NameTable::new();
        let doc = build_doc(
            r#"<root xmlns="http://default" xmlns:p="http://prefixed"/>"#,
            &arena,
            &names,
        );
        let mut nav = doc.create_navigator();
        nav.move_to_first_child(); // root

        assert!(nav.move_to_first_namespace(NamespaceAxisScope::Local));
        assert_eq!(nav.node_type(), DomNodeType::Namespace);

        let mut uris = std::collections::HashSet::new();
        uris.insert(nav.value());
        while nav.move_to_next_namespace(NamespaceAxisScope::Local) {
            uris.insert(nav.value());
        }

        assert!(uris.contains("http://default"));
        assert!(uris.contains("http://prefixed"));
    }

    #[test]
    fn namespace_all_includes_xml() {
        let arena = Bump::new();
        let names = NameTable::new();
        let doc = build_doc(
            r#"<root xmlns:p="http://prefixed"/>"#,
            &arena,
            &names,
        );
        let mut nav = doc.create_navigator();
        nav.move_to_first_child(); // root

        assert!(nav.move_to_first_namespace(NamespaceAxisScope::All));

        let mut uris = std::collections::HashSet::new();
        uris.insert(nav.value());
        while nav.move_to_next_namespace(NamespaceAxisScope::All) {
            uris.insert(nav.value());
        }

        assert!(
            uris.contains("http://prefixed"),
            "Should see declared namespace"
        );
        assert!(
            uris.contains("http://www.w3.org/XML/1998/namespace"),
            "Should see implicit xml namespace"
        );
    }

    #[test]
    fn namespace_exclude_xml() {
        let arena = Bump::new();
        let names = NameTable::new();
        let doc = build_doc(
            r#"<root xmlns:p="http://prefixed"/>"#,
            &arena,
            &names,
        );
        let mut nav = doc.create_navigator();
        nav.move_to_first_child(); // root

        assert!(nav.move_to_first_namespace(NamespaceAxisScope::ExcludeXml));

        let mut uris = std::collections::HashSet::new();
        uris.insert(nav.value());
        while nav.move_to_next_namespace(NamespaceAxisScope::ExcludeXml) {
            uris.insert(nav.value());
        }

        assert!(uris.contains("http://prefixed"));
        assert!(
            !uris.contains("http://www.w3.org/XML/1998/namespace"),
            "Should NOT see xml namespace with ExcludeXml"
        );
    }

    #[test]
    fn namespace_inherited() {
        let arena = Bump::new();
        let names = NameTable::new();
        let doc = build_doc(
            r#"<root xmlns:ns="http://example.com"><child xmlns:local="http://local.com"/></root>"#,
            &arena,
            &names,
        );
        let mut nav = doc.create_navigator();
        nav.move_to_first_child(); // root
        nav.move_to_first_child(); // child

        // All scope: both inherited and local
        assert!(nav.move_to_first_namespace(NamespaceAxisScope::All));
        let mut all_uris = std::collections::HashSet::new();
        all_uris.insert(nav.value());
        while nav.move_to_next_namespace(NamespaceAxisScope::All) {
            all_uris.insert(nav.value());
        }

        assert!(all_uris.contains("http://example.com"), "inherited");
        assert!(all_uris.contains("http://local.com"), "local");

        // Local scope: only local
        nav.move_to_parent(); // back to child
        assert!(nav.move_to_first_namespace(NamespaceAxisScope::Local));
        let mut local_uris = std::collections::HashSet::new();
        local_uris.insert(nav.value());
        while nav.move_to_next_namespace(NamespaceAxisScope::Local) {
            local_uris.insert(nav.value());
        }

        assert!(local_uris.contains("http://local.com"), "local");
        assert!(
            !local_uris.contains("http://example.com"),
            "inherited should not be in Local scope"
        );
    }

    // ── 4. Element value ─────────────────────────────────────────────

    #[test]
    fn element_value_concatenated_text() {
        let arena = Bump::new();
        let names = NameTable::new();
        let doc = build_doc(
            "<root>Hello <b>World</b>!</root>",
            &arena,
            &names,
        );
        let mut nav = doc.create_navigator();
        nav.move_to_first_child();

        assert_eq!(nav.value(), "Hello World!");
    }

    // ── 5. Attribute / PI value ──────────────────────────────────────

    #[test]
    fn attribute_value() {
        let arena = Bump::new();
        let names = NameTable::new();
        let doc = build_doc(r#"<root key="val"/>"#, &arena, &names);
        let mut nav = doc.create_navigator();
        nav.move_to_first_child();
        nav.move_to_first_attribute();

        assert_eq!(nav.value(), "val");
    }

    #[test]
    fn pi_value() {
        let arena = Bump::new();
        let names = NameTable::new();
        let doc = build_doc(
            "<root><?target data?></root>",
            &arena,
            &names,
        );
        let mut nav = doc.create_navigator();
        nav.move_to_first_child(); // root
        nav.move_to_first_child(); // PI

        assert_eq!(nav.node_type(), DomNodeType::ProcessingInstruction);
        assert_eq!(nav.local_name(), "target");
        assert_eq!(nav.value(), "data");
    }

    // ── 6. move_to_prev_sibling ──────────────────────────────────────

    #[test]
    fn prev_sibling() {
        let arena = Bump::new();
        let names = NameTable::new();
        let doc = build_doc("<root><a/><b/><c/></root>", &arena, &names);
        let mut nav = doc.create_navigator();
        nav.move_to_first_child(); // root
        nav.move_to_first_child(); // a
        nav.move_to_next_sibling(); // b
        nav.move_to_next_sibling(); // c

        assert_eq!(nav.local_name(), "c");
        assert!(nav.move_to_prev_sibling());
        assert_eq!(nav.local_name(), "b");
        assert!(nav.move_to_prev_sibling());
        assert_eq!(nav.local_name(), "a");
        assert!(!nav.move_to_prev_sibling());
    }

    // ── 7. move_to_following ─────────────────────────────────────────

    #[test]
    fn move_to_following_elements() {
        let arena = Bump::new();
        let names = NameTable::new();
        let doc = build_doc(
            "<root><a><b/></a><c/></root>",
            &arena,
            &names,
        );
        let mut nav = doc.create_navigator();
        nav.move_to_first_child(); // root
        nav.move_to_first_child(); // a

        // Following axis from <a> skips descendants of <a> (so skips <b>)
        // and moves to next sibling <c>.
        assert!(nav.move_to_following(DomNodeType::Element, None));
        assert_eq!(nav.local_name(), "c");

        // No more following elements after <c>
        assert!(!nav.move_to_following(DomNodeType::Element, None));
    }

    #[test]
    fn move_to_following_skips_descendants() {
        let arena = Bump::new();
        let names = NameTable::new();
        let doc = build_doc(
            "<root><a><d/><e/></a><b/><c/></root>",
            &arena,
            &names,
        );
        let mut nav = doc.create_navigator();
        nav.move_to_first_child(); // root
        nav.move_to_first_child(); // a

        // Following from <a> should skip <d> and <e> (descendants)
        assert!(nav.move_to_following(DomNodeType::Element, None));
        assert_eq!(nav.local_name(), "b");

        assert!(nav.move_to_following(DomNodeType::Element, None));
        assert_eq!(nav.local_name(), "c");

        assert!(!nav.move_to_following(DomNodeType::Element, None));
    }

    #[test]
    fn move_to_following_from_deep_node() {
        let arena = Bump::new();
        let names = NameTable::new();
        let doc = build_doc(
            "<root><a><b><d/></b></a><c/></root>",
            &arena,
            &names,
        );
        let mut nav = doc.create_navigator();
        nav.move_to_first_child(); // root
        nav.move_to_first_child(); // a
        nav.move_to_first_child(); // b

        // Following from <b> skips <d> (descendant), walks up to <a>'s sibling <c>
        assert!(nav.move_to_following(DomNodeType::Element, None));
        assert_eq!(nav.local_name(), "c");

        assert!(!nav.move_to_following(DomNodeType::Element, None));
    }

    #[test]
    fn move_to_following_preserves_position_on_false() {
        let arena = Bump::new();
        let names = NameTable::new();
        let doc = build_doc(
            "<root><a/></root>",
            &arena,
            &names,
        );
        let mut nav = doc.create_navigator();
        nav.move_to_first_child(); // root
        nav.move_to_first_child(); // a

        let before = nav.clone();
        assert!(!nav.move_to_following(DomNodeType::Element, None));
        // Position must be unchanged after returning false
        assert!(nav.is_same_position(&before));
        assert_eq!(nav.local_name(), "a");
    }

    // ── 8. Document order ────────────────────────────────────────────

    #[test]
    fn compare_position_real_nodes() {
        let arena = Bump::new();
        let names = NameTable::new();
        let doc = build_doc("<root><a/><b/></root>", &arena, &names);
        let mut nav1 = doc.create_navigator();
        let mut nav2 = doc.create_navigator();

        nav1.move_to_first_child(); // root
        nav1.move_to_first_child(); // a

        nav2.move_to_first_child(); // root
        nav2.move_to_first_child(); // a
        nav2.move_to_next_sibling(); // b

        assert_eq!(nav1.compare_position(&nav2), XmlNodeOrder::Before);
        assert_eq!(nav2.compare_position(&nav1), XmlNodeOrder::After);
        assert_eq!(nav1.compare_position(&nav1), XmlNodeOrder::Same);
    }

    #[test]
    fn compare_position_attr_vs_element() {
        let arena = Bump::new();
        let names = NameTable::new();
        let doc = build_doc(
            r#"<root x="1"><child/></root>"#,
            &arena,
            &names,
        );
        let mut nav_attr = doc.create_navigator();
        let mut nav_child = doc.create_navigator();

        nav_attr.move_to_first_child(); // root
        nav_attr.move_to_first_attribute(); // x

        nav_child.move_to_first_child(); // root
        nav_child.move_to_first_child(); // child

        // Attribute of root should come before child of root
        assert_eq!(
            nav_attr.compare_position(&nav_child),
            XmlNodeOrder::Before
        );
    }

    // ── 9. is_same_position / move_to ────────────────────────────────

    #[test]
    fn is_same_position_and_move_to() {
        let arena = Bump::new();
        let names = NameTable::new();
        let doc = build_doc("<root><a/><b/></root>", &arena, &names);
        let mut nav1 = doc.create_navigator();
        let mut nav2 = doc.create_navigator();

        assert!(nav1.is_same_position(&nav2));

        nav2.move_to_first_child();
        nav2.move_to_first_child();
        nav2.move_to_next_sibling(); // b

        assert!(!nav1.is_same_position(&nav2));
        assert!(nav1.move_to(&nav2));
        assert!(nav1.is_same_position(&nav2));
        assert_eq!(nav1.local_name(), "b");
    }

    // ── 10. base_uri ─────────────────────────────────────────────────

    #[test]
    fn base_uri_from_xml_base() {
        let arena = Bump::new();
        let names = NameTable::new();
        let doc = build_doc(
            r#"<root xml:base="http://example.com/"><child/></root>"#,
            &arena,
            &names,
        );
        let mut nav = doc.create_navigator();
        nav.move_to_first_child(); // root
        nav.move_to_first_child(); // child

        assert_eq!(nav.base_uri(), "http://example.com/");
    }

    #[test]
    fn base_uri_empty_when_absent() {
        let arena = Bump::new();
        let names = NameTable::new();
        let doc = build_doc("<root><child/></root>", &arena, &names);
        let mut nav = doc.create_navigator();
        nav.move_to_first_child();

        assert_eq!(nav.base_uri(), "");
    }

    // ── 11. schema_type ──────────────────────────────────────────────

    #[test]
    fn schema_type_untyped() {
        let arena = Bump::new();
        let names = NameTable::new();
        let doc = build_doc("<root/>", &arena, &names);
        let mut nav = doc.create_navigator();
        nav.move_to_first_child();

        assert!(nav.schema_type().is_none());
    }

    #[test]
    fn element_type_key_untyped() {
        let arena = Bump::new();
        let names = NameTable::new();
        let doc = build_doc("<root/>", &arena, &names);
        let mut nav = doc.create_navigator();
        nav.move_to_first_child();

        assert!(nav.element_type_key().is_none());
    }

    #[test]
    fn schema_binding_untyped() {
        let arena = Bump::new();
        let names = NameTable::new();
        let doc = build_doc("<root/>", &arena, &names);
        let mut nav = doc.create_navigator();
        nav.move_to_first_child();

        assert!(nav.schema_binding().is_none());
    }

    #[test]
    fn typed_value_nil_returns_none() {
        let arena = Bump::new();
        let names = NameTable::new();
        let mut builder = super::super::builder::BufferDocumentBuilder::new(
            &arena,
            &names,
            None,
            super::super::BufferDocumentOptions::default(),
        )
        .unwrap();

        let elem = builder.start_element("root", "", "", &[]).unwrap();
        builder.end_of_attributes();
        builder.set_nil(elem);
        builder.end_element().unwrap();
        let doc = builder.finalize().unwrap();

        let mut nav = doc.create_navigator();
        nav.move_to_first_child();

        assert!(nav.typed_value().is_none());
    }

    // ── 12. find_element_by_id ───────────────────────────────────────

    #[test]
    fn find_element_by_id_not_found() {
        let arena = Bump::new();
        let names = NameTable::new();
        let doc = build_doc("<root/>", &arena, &names);
        let nav = doc.create_navigator();

        assert!(nav.find_element_by_id("missing").unwrap().is_none());
    }

    // ── 13. Virtual parent ───────────────────────────────────────────

    #[test]
    fn attribute_parent_returns_element() {
        let arena = Bump::new();
        let names = NameTable::new();
        let doc = build_doc(r#"<root x="1"/>"#, &arena, &names);
        let mut nav = doc.create_navigator();
        nav.move_to_first_child(); // root
        nav.move_to_first_attribute(); // x

        assert_eq!(nav.node_type(), DomNodeType::Attribute);
        assert!(nav.move_to_parent());
        assert_eq!(nav.node_type(), DomNodeType::Element);
        assert_eq!(nav.local_name(), "root");
    }

    #[test]
    fn namespace_parent_returns_element() {
        let arena = Bump::new();
        let names = NameTable::new();
        let doc = build_doc(
            r#"<root xmlns:p="http://example.com"/>"#,
            &arena,
            &names,
        );
        let mut nav = doc.create_navigator();
        nav.move_to_first_child(); // root
        nav.move_to_first_namespace(NamespaceAxisScope::Local);

        assert_eq!(nav.node_type(), DomNodeType::Namespace);
        assert!(nav.move_to_parent());
        assert_eq!(nav.node_type(), DomNodeType::Element);
        assert_eq!(nav.local_name(), "root");
    }

    // ── 14. name() — qualified name ──────────────────────────────────

    #[test]
    fn qualified_name_with_prefix() {
        let arena = Bump::new();
        let names = NameTable::new();
        let doc = build_doc(
            r#"<ns:root xmlns:ns="http://example.com"/>"#,
            &arena,
            &names,
        );
        let mut nav = doc.create_navigator();
        nav.move_to_first_child();

        assert_eq!(nav.name(), "ns:root");
        assert_eq!(nav.local_name(), "root");
        assert_eq!(nav.prefix(), "ns");
        assert_eq!(nav.namespace_uri(), "http://example.com");
    }

    #[test]
    fn qualified_name_without_prefix() {
        let arena = Bump::new();
        let names = NameTable::new();
        let doc = build_doc("<root/>", &arena, &names);
        let mut nav = doc.create_navigator();
        nav.move_to_first_child();

        assert_eq!(nav.name(), "root");
        assert_eq!(nav.local_name(), "root");
    }

    // ── 15. Empty, text-only, mixed ──────────────────────────────────

    #[test]
    fn empty_element() {
        let arena = Bump::new();
        let names = NameTable::new();
        let doc = build_doc("<root/>", &arena, &names);
        let mut nav = doc.create_navigator();
        nav.move_to_first_child();

        assert!(!nav.move_to_first_child());
        assert_eq!(nav.value(), "");
    }

    #[test]
    fn text_only_element() {
        let arena = Bump::new();
        let names = NameTable::new();
        let doc = build_doc("<root>hello</root>", &arena, &names);
        let mut nav = doc.create_navigator();
        nav.move_to_first_child(); // root

        assert_eq!(nav.value(), "hello");

        nav.move_to_first_child(); // text node
        assert_eq!(nav.node_type(), DomNodeType::Text);
        assert_eq!(nav.value(), "hello");
    }

    #[test]
    fn mixed_content_value() {
        let arena = Bump::new();
        let names = NameTable::new();
        let doc = build_doc(
            "<root>a<b>c</b>d</root>",
            &arena,
            &names,
        );
        let mut nav = doc.create_navigator();
        nav.move_to_first_child();

        assert_eq!(nav.value(), "acd");
    }

    #[test]
    fn comment_node() {
        let arena = Bump::new();
        let names = NameTable::new();
        let doc = build_doc(
            "<root><!-- comment --></root>",
            &arena,
            &names,
        );
        let mut nav = doc.create_navigator();
        nav.move_to_first_child(); // root
        nav.move_to_first_child(); // comment

        assert_eq!(nav.node_type(), DomNodeType::Comment);
        assert_eq!(nav.value(), " comment ");
    }

    #[test]
    fn clone_semantics() {
        let arena = Bump::new();
        let names = NameTable::new();
        let doc = build_doc("<root><a/></root>", &arena, &names);
        let mut nav = doc.create_navigator();
        nav.move_to_first_child();

        let nav_clone = nav.clone();
        assert!(nav.is_same_position(&nav_clone));

        nav.move_to_first_child();
        assert!(!nav.is_same_position(&nav_clone));
    }

    #[test]
    fn no_attributes_returns_false() {
        let arena = Bump::new();
        let names = NameTable::new();
        let doc = build_doc("<root/>", &arena, &names);
        let mut nav = doc.create_navigator();
        nav.move_to_first_child();

        assert!(!nav.move_to_first_attribute());
    }

    #[test]
    fn no_namespaces_local_returns_false() {
        let arena = Bump::new();
        let names = NameTable::new();
        let doc = build_doc("<root/>", &arena, &names);
        let mut nav = doc.create_navigator();
        nav.move_to_first_child();

        assert!(!nav.move_to_first_namespace(NamespaceAxisScope::Local));
    }

    #[test]
    fn move_to_root_from_deep_node() {
        let arena = Bump::new();
        let names = NameTable::new();
        let doc = build_doc(
            "<root><a><b><c/></b></a></root>",
            &arena,
            &names,
        );
        let mut nav = doc.create_navigator();
        nav.move_to_first_child(); // root
        nav.move_to_first_child(); // a
        nav.move_to_first_child(); // b
        nav.move_to_first_child(); // c

        assert_eq!(nav.local_name(), "c");
        nav.move_to_root();
        assert_eq!(nav.node_type(), DomNodeType::Root);
    }

    #[test]
    fn create_navigator_at() {
        let arena = Bump::new();
        let names = NameTable::new();
        let doc = build_doc("<root><child/></root>", &arena, &names);

        // Find the child ref through normal navigation
        let mut nav = doc.create_navigator();
        nav.move_to_first_child(); // root
        nav.move_to_first_child(); // child
        let child_ref = nav.current_ref();

        // Create navigator directly at child
        let nav2 = doc.create_navigator_at(child_ref);
        assert_eq!(nav2.local_name(), "child");
        assert!(nav.is_same_position(&nav2));
    }
}
