//! Top-level `BufferDocument` struct assembling all storage primitives.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use bumpalo::Bump;

use crate::namespace::NameTable;
use crate::schema::SchemaSet;

use super::{
    BindingRemapTable, BufferDocumentOptions, DocumentKind, ElementIndex, NamespacePageFactory,
    Node, NodePages, NodeSourceSpans, NsRef, QNameTable, StringStore, NULL,
};

/// Monotonic source of [`BufferDocument::serial`] values.
///
/// Starts at 1 so that `0` can be used by callers as a "no document" sentinel.
static NEXT_DOCUMENT_SERIAL: AtomicU64 = AtomicU64::new(1);

/// Hands out the next document creation ordinal.
///
/// Called once per [`BufferDocument`] construction; see
/// [`BufferDocument::serial`] for the guarantees this provides.
pub(crate) fn next_document_serial() -> u64 {
    NEXT_DOCUMENT_SERIAL.fetch_add(1, Ordering::Relaxed)
}

/// Compact, cache-friendly XML document representation.
///
/// Built on a flat array of 16-byte [`Node`] structs with power-of-2
/// page addressing.  All string data lives in the arena or in the
/// [`StringStore`]; qualified names are deduplicated via [`QNameTable`].
#[allow(dead_code)] // fields used by builder/navigator in later steps
pub struct BufferDocument<'a> {
    pub(crate) arena: &'a Bump,
    /// Creation ordinal, unique within the process — see [`Self::serial`].
    pub(crate) serial: u64,
    pub(crate) kind: DocumentKind,
    pub(crate) names: &'a NameTable,
    pub(crate) nodes: NodePages<'a>,
    pub(crate) qname_table: QNameTable,
    pub(crate) strings: StringStore<'a>,
    pub(crate) binding_remap: BindingRemapTable,
    /// Whether any element or attribute node of this document carries a schema
    /// type annotation — see [`BufferDocument::has_type_annotations`].
    ///
    /// Maintained by
    /// [`BufferDocumentBuilder::set_node_binding`](super::builder::BufferDocumentBuilder::set_node_binding),
    /// the single place a [`NodeSchemaBinding`](super::NodeSchemaBinding) is
    /// ever attached to a node, so it is exact rather than a hint.
    pub(crate) has_type_annotations: bool,
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
    /// `XPathContext::base_uri` free to carry the schema document URI, and by
    /// [`BufferDocument::set_document_base_uri`] for a host that knows where
    /// the document came from.
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

    /// Returns this document's **creation ordinal**.
    ///
    /// The serial is assigned once, when the document is constructed, from a
    /// process-wide counter. It is therefore:
    ///
    /// * **unique within the process** — no two live or dead
    ///   `BufferDocument`s of this process share a serial;
    /// * **strictly increasing in creation order** — if `a` was created
    ///   before `b`, then `a.serial() < b.serial()`;
    /// * **not** stable across processes, and **not** derived from the
    ///   document's content.
    ///
    /// It is intended for reproducible cross-document ordering (see
    /// [`BufferDocNavigator::compare_position`], which orders nodes from
    /// distinct trees by serial rather than by heap address) and for
    /// `generate-id()`-style identifiers, where a caller can combine the
    /// serial with a node reference to obtain a cheap unique node name.
    ///
    /// [`BufferDocNavigator::compare_position`]: super::navigator::BufferDocNavigator
    #[inline]
    pub fn serial(&self) -> u64 {
        self.serial
    }

    /// Returns the byte range of `node_ref` in the original XML source.
    ///
    /// Spans are only recorded when the document was built with
    /// [`BufferDocumentOptions::track_source_locations`] enabled; otherwise
    /// this always returns `None`, and only element nodes carry one. The span
    /// *starts* at the element's `<`; it ends after the end tag for an element
    /// written with one, and after the tag itself for an empty-element tag. A
    /// host embedding the parser uses `span.start` to report an error at a line
    /// and column of its own copy of the source text.
    ///
    /// ```no_run
    /// # use xsd_schema::document::{BufferDocument, BufferDocumentOptions};
    /// # use xsd_schema::namespace::NameTable;
    /// # let arena = bumpalo::Bump::new();
    /// # let names = NameTable::new();
    /// let options = BufferDocumentOptions { track_source_locations: true, ..Default::default() };
    /// let doc = BufferDocument::from_reader(b"<a/>".as_slice(), &arena, &names, options, None)?;
    /// let span = doc.source_span(doc.root() + 1);
    /// # Ok::<(), xsd_schema::document::BufferDocumentError>(())
    /// ```
    #[inline]
    pub fn source_span(&self, node_ref: u32) -> Option<crate::parser::location::SourceSpan> {
        self.source_spans.get(node_ref)
    }

    /// Whether this document recorded source spans at all
    /// ([`BufferDocumentOptions::track_source_locations`]).
    #[inline]
    pub fn has_source_spans(&self) -> bool {
        !self.source_spans.is_empty()
    }

    /// Whether **any** element or attribute node of this document carries a
    /// schema type annotation.
    ///
    /// This is the O(1) form of the walk an XPath host would otherwise have to
    /// perform — visiting every element and attribute and asking each one for
    /// [`DomNavigator::type_annotation`] — when it has to reject a typed (or an
    /// untyped) tree, or take a different path for one.
    ///
    /// The answer is **exact**, not a hint: it is `true` if and only if at
    /// least one node of the document would report
    /// `DomNavigator::type_annotation() == Some(_)`. It is maintained at the
    /// one place a binding is attached to a node, so no walk can disagree with
    /// it.
    ///
    /// "Carries a type annotation" means the same thing here as it does for
    /// [`DomNavigator::type_annotation`]: a node of a document that was never
    /// schema-validated reports `None`, which is the XDM `xs:untyped` /
    /// `xs:untypedAtomic` default. Those defaults are therefore *not* counted,
    /// and a freshly parsed document answers `false`. Only element and
    /// attribute nodes can be annotated; every other kind reports `None`
    /// unconditionally and is never counted.
    ///
    /// A document built by copying ([`copy_subtree`]) answers according to the
    /// copy's [`Annotations`] mode: `Annotations::Preserve` carries the
    /// source's bindings over and can make this `true`, while
    /// `Annotations::Strip` — the default — always leaves it `false`.
    ///
    /// [`DomNavigator::type_annotation`]: crate::navigator::DomNavigator::type_annotation
    /// [`copy_subtree`]: super::builder::BufferDocumentBuilder::copy_subtree
    /// [`Annotations`]: super::Annotations
    ///
    /// ```
    /// use xsd_schema::document::BufferDocument;
    /// use xsd_schema::namespace::NameTable;
    ///
    /// let arena = bumpalo::Bump::new();
    /// let names = NameTable::new();
    /// let doc = BufferDocument::from_reader_default(b"<a n=\"1\"/>".as_slice(), &arena, &names)?;
    /// // Parsed, never validated: no node carries an annotation.
    /// assert!(!doc.has_type_annotations());
    /// # Ok::<(), xsd_schema::document::BufferDocumentError>(())
    /// ```
    #[inline]
    pub fn has_type_annotations(&self) -> bool {
        self.has_type_annotations
    }

    /// Sets the **document-level base URI**: the base URI a node of this
    /// document reports once the walk up its `xml:base` ancestors reaches the
    /// document node without finding one.
    ///
    /// A parser has no way to know the URI a document was retrieved from — it
    /// is handed bytes — so a document built by [`BufferDocument::from_reader`]
    /// or by a [`BufferDocumentBuilder`] starts with none, and `fn:base-uri`
    /// on its nodes falls back to the static base URI of the expression. A
    /// host that *does* know where the document came from records it here, and
    /// `fn:base-uri` then reports that URI (still overridden by any `xml:base`
    /// attribute on the node or an ancestor, which is resolved against it).
    ///
    /// The URI must live at least as long as the document; allocate it in the
    /// same arena when it is computed at run time.
    ///
    /// [`BufferDocumentBuilder`]: super::builder::BufferDocumentBuilder
    ///
    /// ```
    /// # use xsd_schema::document::BufferDocument;
    /// # use xsd_schema::namespace::NameTable;
    /// # use xsd_schema::navigator::DomNavigator;
    /// # let arena = bumpalo::Bump::new();
    /// # let names = NameTable::new();
    /// let mut doc = BufferDocument::from_reader_default(b"<a/>".as_slice(), &arena, &names)?;
    /// doc.set_document_base_uri(Some("file:///tmp/a.xml"));
    /// assert_eq!(doc.create_navigator().base_uri(), "file:///tmp/a.xml");
    /// # Ok::<(), xsd_schema::document::BufferDocumentError>(())
    /// ```
    #[inline]
    pub fn set_document_base_uri(&mut self, uri: Option<&'a str>) {
        self.fragment_base_uri = uri;
    }

    /// The document-level base URI, if one was recorded.
    ///
    /// See [`set_document_base_uri`](Self::set_document_base_uri).
    #[inline]
    pub fn document_base_uri(&self) -> Option<&'a str> {
        self.fragment_base_uri
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
            serial: next_document_serial(),
            kind: DocumentKind::default(),
            names,
            nodes: NodePages::new(arena),
            qname_table: QNameTable::new(),
            strings: StringStore::new(arena),
            binding_remap: BindingRemapTable::new(),
            has_type_annotations: false,
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

    #[test]
    fn source_span_is_readable_when_tracking_is_on() {
        let xml = "<a>\n  <b/>\n</a>";
        let arena = Bump::new();
        let names = NameTable::new();
        let options = BufferDocumentOptions {
            track_source_locations: true,
            ..Default::default()
        };
        let doc =
            BufferDocument::from_reader(xml.as_bytes(), &arena, &names, options, None).unwrap();

        assert!(doc.has_source_spans());
        // The document element is the node right after the root. Its span runs
        // from its `<` to the end of its end tag.
        let a = doc.root() + 1;
        let span_a = doc.source_span(a).expect("the root element has a span");
        assert_eq!(&xml[span_a.start..span_a.end], xml);
        assert!(xml[span_a.start..].starts_with("<a>"));

        // An empty-element tag's span is exactly that tag.
        use crate::navigator::{DomNavigator, DomNodeType};
        let mut nav = doc.create_navigator_at(a);
        assert!(nav.move_to_first_child());
        while nav.node_type() != DomNodeType::Element {
            assert!(nav.move_to_next_sibling());
        }
        let span_b = doc
            .source_span(nav.current_ref())
            .expect("the child element has a span");
        assert!(span_b.start > span_a.start);
        assert_eq!(&xml[span_b.start..span_b.end], "<b/>");
    }

    #[test]
    fn a_document_level_base_uri_is_reported_and_xml_base_still_wins() {
        use crate::navigator::DomNavigator;
        let arena = Bump::new();
        let names = NameTable::new();
        let mut doc = BufferDocument::from_reader_default(
            br#"<a><plain/><based xml:base="sub/"/></a>"#.as_slice(),
            &arena,
            &names,
        )
        .unwrap();

        // Nothing is recorded by default, which is what every document built
        // before this accessor existed reports.
        assert_eq!(doc.document_base_uri(), None);
        assert_eq!(doc.create_navigator().base_uri(), "");

        doc.set_document_base_uri(Some("file:///tmp/a.xml"));
        assert_eq!(doc.document_base_uri(), Some("file:///tmp/a.xml"));

        // The document node and any node without an `xml:base` ancestor
        // report it.
        let mut nav = doc.create_navigator();
        assert_eq!(nav.base_uri(), "file:///tmp/a.xml");
        assert!(nav.move_to_first_child());
        assert!(nav.move_to_first_child());
        assert_eq!(nav.local_name(), "plain");
        assert_eq!(nav.base_uri(), "file:///tmp/a.xml");

        // An `xml:base` attribute still takes precedence at the node itself.
        assert!(nav.move_to_next_sibling());
        assert_eq!(nav.local_name(), "based");
        assert_eq!(nav.base_uri(), "sub/");

        doc.set_document_base_uri(None);
        assert_eq!(doc.create_navigator().base_uri(), "");
    }

    #[test]
    fn source_span_is_absent_without_tracking() {
        let arena = Bump::new();
        let names = NameTable::new();
        let doc = BufferDocument::from_reader_default(b"<a/>".as_slice(), &arena, &names).unwrap();

        assert!(!doc.has_source_spans());
        assert_eq!(doc.source_span(doc.root() + 1), None);
    }
}
