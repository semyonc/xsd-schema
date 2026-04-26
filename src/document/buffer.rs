//! Top-level `BufferDocument` struct assembling all storage primitives.

use std::collections::HashMap;

use bumpalo::Bump;

use crate::namespace::NameTable;
use crate::schema::SchemaSet;

use super::{
    BindingRemapTable, BufferDocumentOptions, DocumentKind, ElementIndex, NamespacePageFactory,
    Node, NodePages, NodeSourceSpans, NsRef, QNameTable, StringStore, NULL,
};

/// Compact, cache-friendly XML document representation.
///
/// Built on a flat array of 16-byte [`Node`] structs with power-of-2
/// page addressing.  All string data lives in the arena or in the
/// [`StringStore`]; qualified names are deduplicated via [`QNameTable`].
#[allow(dead_code)] // fields used by builder/navigator in later steps
pub struct BufferDocument<'a> {
    pub(crate) arena: &'a Bump,
    pub(crate) kind: DocumentKind,
    pub(crate) names: &'a NameTable,
    pub(crate) nodes: NodePages<'a>,
    pub(crate) qname_table: QNameTable,
    pub(crate) strings: StringStore<'a>,
    pub(crate) binding_remap: BindingRemapTable,
    pub(crate) root: u32,
    pub(crate) options: BufferDocumentOptions,
    // Side tables
    pub(crate) namespace_pages: NamespacePageFactory<'a>,
    pub(crate) xml_namespace: NsRef,
    pub(crate) element_namespaces: HashMap<u32, NsRef>,
    pub(crate) element_index: ElementIndex,
    pub(crate) source_spans: NodeSourceSpans,
    pub(crate) id_elements: HashMap<Box<str>, u32>,
    pub(crate) schema_set: Option<&'a SchemaSet>,
    /// Document-level base URI surfaced by `BufferDocNavigator::base_uri()`
    /// when no `xml:base` is found and the cursor reaches the document root.
    /// Used by CTA fragment evaluation to expose the instance file URI to
    /// `fn:base-uri(.)` while leaving the static base URI in
    /// `XPathContext::base_uri` free to carry the schema document URI.
    pub(crate) fragment_base_uri: Option<&'a str>,
}

impl<'a> BufferDocument<'a> {
    // ── Accessors ──────────────────────────────────────────────────────

    /// Returns the document kind (full or fragment).
    #[inline]
    pub fn kind(&self) -> DocumentKind {
        self.kind
    }

    /// Returns the construction options.
    #[inline]
    pub fn options(&self) -> &BufferDocumentOptions {
        &self.options
    }

    /// Returns the root node index.
    #[inline]
    pub fn root(&self) -> u32 {
        self.root
    }

    /// Returns the associated schema set, if any.
    #[inline]
    pub fn schema_set(&self) -> Option<&'a SchemaSet> {
        self.schema_set
    }

    /// Returns the shared name table.
    #[inline]
    pub fn names(&self) -> &'a NameTable {
        self.names
    }

    // ── Navigation helpers ─────────────────────────────────────────────

    /// Returns the first child of `parent` (always `parent + 1`).
    ///
    /// This relies on the document-order layout: the first child node
    /// is stored immediately after its parent.
    #[inline]
    pub fn first_child_of(&self, parent: u32) -> u32 {
        parent + 1
    }

    /// Returns the first content (non-attribute) child of `parent`, or
    /// `None` if the element has no children.
    ///
    /// Attribute pairs precede content children in document order.
    /// This method skips over them by walking the `next_sibling` chain
    /// of attribute nodes until a non-attribute child is found.
    pub fn first_content_child_of(&self, parent: u32) -> Option<u32> {
        let node = self.nodes.get(parent);
        if !node.has_flag(Node::HAS_CHILDREN) {
            return None;
        }
        if !node.has_flag(Node::HAS_ATTRIBUTE) {
            return Some(parent + 1);
        }
        // Walk the attribute next_sibling chain.
        // Each attribute is a 2-node pair (Attribute + ChildValue).
        let mut cursor = parent + 1; // first attribute
        loop {
            let attr = self.nodes.get(cursor);
            if attr.next_sibling == NULL {
                // Last attribute pair — content starts after its ChildValue node.
                return Some(cursor + 2);
            }
            cursor = attr.next_sibling;
        }
    }

    /// Returns the flat index one past the last node in the subtree
    /// rooted at `elem`.
    ///
    /// Walks ancestors until a node with a `next_sibling` is found and
    /// returns that sibling.  If the root is reached without finding a
    /// sibling, returns `self.nodes.len()` (end of document).
    pub fn subtree_end(&self, elem: u32) -> u32 {
        let mut cursor = elem;
        loop {
            let node = self.nodes.get(cursor);
            if node.next_sibling != NULL {
                return node.next_sibling;
            }
            if node.parent == NULL {
                return self.nodes.len();
            }
            cursor = node.parent;
        }
    }

    /// Looks up an element node by its `xml:id` value.
    pub fn get_element_by_id(&self, id: &str) -> Option<u32> {
        self.id_elements.get(id).copied()
    }

    // ── CTA fragment configuration ─────────────────────────────────────

    /// Reconfigure this document so that `elem_ref` is the root of the
    /// XDM tree visible to the navigator (XSD 1.1 §3.12.4 CTA XDM
    /// instance shape).
    ///
    /// After this call, `move_to_parent()` from `elem_ref` returns `false`
    /// (the synthetic Root node at index 0 is severed from the tree),
    /// `move_to_root()` lands on `elem_ref`, and the `following::*` /
    /// `preceding::*` axes cannot escape the subtree anchored at
    /// `elem_ref`. The optional `base_uri` argument is surfaced by
    /// `BufferDocNavigator::base_uri()` when no `xml:base` is found,
    /// so `fn:base-uri(.)` can return the instance file URI even
    /// though the static base URI in the XPath context is set to the
    /// schema document URI.
    pub(crate) fn set_cta_fragment(&mut self, elem_ref: u32, base_uri: Option<&'a str>) {
        self.kind = DocumentKind::Fragment;
        self.root = elem_ref;
        self.nodes.update(elem_ref, |n| n.parent = NULL);
        self.fragment_base_uri = base_uri;
    }

    // ── Navigator factory ─────────────────────────────────────────────

    /// Creates a navigator positioned at the document root.
    pub fn create_navigator(&self) -> super::navigator::BufferDocNavigator<'_> {
        super::navigator::BufferDocNavigator::new(self, self.root)
    }

    /// Creates a navigator positioned at the given node reference.
    pub fn create_navigator_at(&self, node_ref: u32) -> super::navigator::BufferDocNavigator<'_> {
        super::navigator::BufferDocNavigator::new(self, node_ref)
    }

    // ── Parsing helpers ───────────────────────────────────────────────

    /// Parses an XML document from a reader into a `BufferDocument`.
    pub fn from_reader<R: std::io::BufRead>(
        reader: R,
        arena: &'a Bump,
        names: &'a NameTable,
        options: BufferDocumentOptions,
        schema_set: Option<&'a SchemaSet>,
    ) -> Result<Self, super::BufferDocumentError> {
        let builder =
            super::builder::BufferDocumentBuilder::new(arena, names, schema_set, options)?;
        builder.build(reader)
    }

    /// Parses an XML document from a reader with default options.
    pub fn from_reader_default<R: std::io::BufRead>(
        reader: R,
        arena: &'a Bump,
        names: &'a NameTable,
    ) -> Result<Self, super::BufferDocumentError> {
        Self::from_reader(reader, arena, names, BufferDocumentOptions::default(), None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{Node, NodeType, NULL};

    /// Helper: builds a minimal `BufferDocument` for testing.
    fn make_doc<'a>(arena: &'a Bump, names: &'a NameTable) -> BufferDocument<'a> {
        BufferDocument {
            arena,
            kind: DocumentKind::default(),
            names,
            nodes: NodePages::new(arena),
            qname_table: QNameTable::new(),
            strings: StringStore::new(arena),
            binding_remap: BindingRemapTable::new(),
            root: 0,
            options: BufferDocumentOptions::default(),
            namespace_pages: NamespacePageFactory::new(arena),
            xml_namespace: NsRef::NULL,
            element_namespaces: HashMap::new(),
            element_index: ElementIndex::new(),
            source_spans: NodeSourceSpans::new(),
            id_elements: HashMap::new(),
            schema_set: None,
            fragment_base_uri: None,
        }
    }

    /// Helper: allocate a node and write it.
    fn push_node(doc: &mut BufferDocument<'_>, node: Node) -> u32 {
        let idx = doc.nodes.alloc().unwrap();
        doc.nodes.set(idx, node);
        idx
    }

    /// Helper: create a node with specific type and flags.
    fn make_node(nt: NodeType, parent: u32, next_sibling: u32, flags: u32) -> Node {
        let mut n = Node::default();
        n.set_node_type(nt);
        n.parent = parent;
        n.next_sibling = next_sibling;
        n.props_type |= flags;
        n
    }

    #[test]
    fn first_child_of() {
        let arena = Bump::new();
        let names = NameTable::new();
        let doc = make_doc(&arena, &names);
        assert_eq!(doc.first_child_of(0), 1);
        assert_eq!(doc.first_child_of(5), 6);
        assert_eq!(doc.first_child_of(100), 101);
    }

    #[test]
    fn first_content_child_of_no_children() {
        let arena = Bump::new();
        let names = NameTable::new();
        let mut doc = make_doc(&arena, &names);

        // Element without HAS_CHILDREN
        let elem = make_node(NodeType::Element, NULL, NULL, 0);
        push_node(&mut doc, elem);

        assert_eq!(doc.first_content_child_of(0), None);
    }

    #[test]
    fn first_content_child_of_no_attrs() {
        let arena = Bump::new();
        let names = NameTable::new();
        let mut doc = make_doc(&arena, &names);

        // Element with children but no attributes
        let elem = make_node(NodeType::Element, NULL, NULL, Node::HAS_CHILDREN);
        push_node(&mut doc, elem); // 0

        // First child is text
        let text = make_node(NodeType::Text, 0, NULL, 0);
        push_node(&mut doc, text); // 1

        assert_eq!(doc.first_content_child_of(0), Some(1));
    }

    #[test]
    fn first_content_child_of_with_attrs() {
        let arena = Bump::new();
        let names = NameTable::new();
        let mut doc = make_doc(&arena, &names);

        // Element with both attributes and children
        let elem = make_node(
            NodeType::Element,
            NULL,
            NULL,
            Node::HAS_CHILDREN | Node::HAS_ATTRIBUTE,
        );
        push_node(&mut doc, elem); // 0

        // Attribute 1 (pair: Attribute + ChildValue)
        // next_sibling points to the next attribute at index 3
        let attr1 = make_node(NodeType::Attribute, 0, 3, 0);
        push_node(&mut doc, attr1); // 1

        let val1 = make_node(NodeType::ChildValue, 0, NULL, 0);
        push_node(&mut doc, val1); // 2

        // Attribute 2 (pair: Attribute + ChildValue)
        // next_sibling = NULL → last attribute
        let attr2 = make_node(NodeType::Attribute, 0, NULL, 0);
        push_node(&mut doc, attr2); // 3

        let val2 = make_node(NodeType::ChildValue, 0, NULL, 0);
        push_node(&mut doc, val2); // 4

        // Content child (text) at index 5
        let text = make_node(NodeType::Text, 0, NULL, 0);
        push_node(&mut doc, text); // 5

        // first_content_child_of should skip both attribute pairs → index 5
        assert_eq!(doc.first_content_child_of(0), Some(5));
    }

    #[test]
    fn subtree_end_with_sibling() {
        let arena = Bump::new();
        let names = NameTable::new();
        let mut doc = make_doc(&arena, &names);

        // Root at 0
        let root = make_node(NodeType::Root, NULL, NULL, Node::HAS_CHILDREN);
        push_node(&mut doc, root); // 0

        // Element at 1, has sibling at 2
        let elem = make_node(NodeType::Element, 0, 2, 0);
        push_node(&mut doc, elem); // 1

        // Sibling element at 2
        let sib = make_node(NodeType::Element, 0, NULL, 0);
        push_node(&mut doc, sib); // 2

        assert_eq!(doc.subtree_end(1), 2);
    }

    #[test]
    fn subtree_end_walks_ancestors() {
        let arena = Bump::new();
        let names = NameTable::new();
        let mut doc = make_doc(&arena, &names);

        // Root at 0, has sibling at 5 (hypothetical)
        let root = make_node(NodeType::Root, NULL, NULL, Node::HAS_CHILDREN);
        push_node(&mut doc, root); // 0

        // Parent element at 1, has next_sibling at 4
        let parent = make_node(NodeType::Element, 0, 4, Node::HAS_CHILDREN);
        push_node(&mut doc, parent); // 1

        // Nested child element at 2, no sibling
        let child = make_node(NodeType::Element, 1, NULL, 0);
        push_node(&mut doc, child); // 2

        // Text at 3 (unused, just to fill space)
        let text = make_node(NodeType::Text, 1, NULL, 0);
        push_node(&mut doc, text); // 3

        // Sibling of parent at 4
        let uncle = make_node(NodeType::Element, 0, NULL, 0);
        push_node(&mut doc, uncle); // 4

        // child(2) has no sibling → walk to parent(1) which has sibling 4
        assert_eq!(doc.subtree_end(2), 4);
    }

    #[test]
    fn subtree_end_at_document_end() {
        let arena = Bump::new();
        let names = NameTable::new();
        let mut doc = make_doc(&arena, &names);

        // Root at 0, no sibling, parent = NULL
        let root = make_node(NodeType::Root, NULL, NULL, Node::HAS_CHILDREN);
        push_node(&mut doc, root); // 0

        // Single child at 1, no sibling
        let elem = make_node(NodeType::Element, 0, NULL, 0);
        push_node(&mut doc, elem); // 1

        // elem(1) has no sibling → walk to root(0) which has parent = NULL → nodes.len()
        assert_eq!(doc.subtree_end(1), doc.nodes.len());
        assert_eq!(doc.subtree_end(1), 2);
    }

    #[test]
    fn get_element_by_id_found() {
        let arena = Bump::new();
        let names = NameTable::new();
        let mut doc = make_doc(&arena, &names);
        doc.id_elements.insert("foo".into(), 42);

        assert_eq!(doc.get_element_by_id("foo"), Some(42));
    }

    #[test]
    fn get_element_by_id_not_found() {
        let arena = Bump::new();
        let names = NameTable::new();
        let doc = make_doc(&arena, &names);

        assert_eq!(doc.get_element_by_id("nonexistent"), None);
    }
}
