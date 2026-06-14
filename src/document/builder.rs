//! Document builder — core push API and quick-xml adapter.
//!
//! [`BufferDocumentBuilder`] constructs a [`BufferDocument`] either through
//! its low-level push API (`start_element`, `attribute`, `text`, …) or via
//! the `build()` method which drives the push API from a quick-xml event stream.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::io::BufRead;

use bumpalo::Bump;
use quick_xml::events::Event;
use quick_xml::Reader;

use crate::namespace::table::XML_NAMESPACE;
use crate::namespace::NameTable;
use crate::parser::location::SourceSpan;
use crate::schema::SchemaSet;

use super::buffer::BufferDocument;
use super::error::BufferDocumentError;
use super::{
    BindingRemapTable, BufferDocumentOptions, DocumentKind, ElementIndex, NamespaceNode,
    NamespacePageFactory, Node, NodePages, NodeSchemaBinding, NodeSourceSpans, NodeType, NsRef,
    QNameAtom, QNameTable, StringStore, NULL,
};

// ── ElementBuildState ─────────────────────────────────────────────────

/// Tracks per-element state during document construction.
#[derive(Clone, Copy, Debug)]
struct ElementBuildState {
    #[allow(dead_code)] // used by navigator in Step 7
    node_ref: u32,
    #[allow(dead_code)] // used by navigator in Step 7
    has_attrs: bool,
}

// ── hash_name ─────────────────────────────────────────────────────────

/// Compute a `u32` hash of a local name string (same hasher as `QNameTable`).
fn hash_name(name: &str) -> u32 {
    let mut hasher = DefaultHasher::new();
    name.hash(&mut hasher);
    hasher.finish() as u32
}

// ── BufferDocumentBuilder ─────────────────────────────────────────────

/// Builds a [`BufferDocument`] incrementally via a push API.
pub struct BufferDocumentBuilder<'a> {
    doc: BufferDocument<'a>,
    parent: u32,
    last_sibling: u32,
    last_attr: u32,
    namespace_stack: Vec<(u32, NsRef)>,
    text_buffer: String,
    text_type: Option<NodeType>,
    current_namespace: NsRef,
    element_stack: Vec<ElementBuildState>,
    pending_spans: Vec<(u32, usize)>,
    #[allow(dead_code)]
    options: BufferDocumentOptions,
}

impl<'a> BufferDocumentBuilder<'a> {
    // ── Constructor ───────────────────────────────────────────────────

    /// Creates a new builder.
    ///
    /// If `schema_set` is `Some`, uses its `name_table`; otherwise uses `names`.
    pub fn new(
        arena: &'a Bump,
        names: &'a NameTable,
        schema_set: Option<&'a SchemaSet>,
        options: BufferDocumentOptions,
    ) -> Result<Self, BufferDocumentError> {
        let effective_names = schema_set
            .map(|ss| &ss.name_table as &'a NameTable)
            .unwrap_or(names);

        // Namespace pages + implicit xml: binding at slot 0
        let mut namespace_pages = NamespacePageFactory::new(arena);
        let xml_ns_ref = namespace_pages
            .alloc()
            .ok_or(BufferDocumentError::Overflow)?;
        let xml_prefix_id = effective_names.add("xml");
        let xml_uri_id = effective_names.add(XML_NAMESPACE);
        namespace_pages.set(
            xml_ns_ref,
            NamespaceNode::new(xml_prefix_id, xml_uri_id, NsRef::NULL),
        );

        // Node pages + root node at index 0
        let mut nodes = NodePages::new(arena);
        let root_ref = nodes.alloc()?;
        let mut root_node = Node::default();
        root_node.set_node_type(NodeType::Root);
        root_node.parent = NULL;
        root_node.next_sibling = NULL;
        nodes.set(root_ref, root_node);

        let doc = BufferDocument {
            arena,
            kind: options.kind,
            names: effective_names,
            nodes,
            qname_table: QNameTable::new(),
            strings: StringStore::new(arena),
            binding_remap: BindingRemapTable::new(),
            root: root_ref,
            options,
            namespace_pages,
            xml_namespace: xml_ns_ref,
            element_namespaces: HashMap::new(),
            element_index: ElementIndex::new(),
            source_spans: NodeSourceSpans::new(),
            id_elements: HashMap::new(),
            schema_set,
            fragment_base_uri: None,
        };

        Ok(Self {
            doc,
            parent: root_ref,
            last_sibling: NULL,
            last_attr: NULL,
            namespace_stack: Vec::new(),
            text_buffer: String::new(),
            text_type: None,
            current_namespace: NsRef::NULL,
            element_stack: Vec::new(),
            pending_spans: Vec::new(),
            options,
        })
    }

    // ── Core push API ─────────────────────────────────────────────────

    /// Opens an element node.
    ///
    /// `ns_declarations` is a list of `(prefix, namespace_uri)` pairs for xmlns
    /// declarations on this element.
    pub fn start_element(
        &mut self,
        local_name: &str,
        ns_uri: &str,
        prefix: &str,
        ns_declarations: &[(&str, &str)],
    ) -> Result<u32, BufferDocumentError> {
        self.flush_text()?;

        // Save previous namespace head
        let prev_namespace = self.current_namespace;

        // Process namespace declarations
        for &(ns_prefix, ns_uri_decl) in ns_declarations {
            self.handle_namespace_decl(ns_prefix, ns_uri_decl)?;
        }

        // Intern names
        let local_id = self.doc.names.add(local_name);
        let uri_id = self.doc.names.add(ns_uri);
        let prefix_id = self.doc.names.add(prefix);
        let local_hash = hash_name(local_name);

        let qualified_name_idx = if prefix.is_empty() {
            self.doc.strings.store(local_name)
        } else {
            self.doc.strings.store(&format!("{prefix}:{local_name}"))
        };
        let qname = QNameAtom {
            local_name: local_id,
            namespace_uri: uri_id,
            prefix: prefix_id,
            local_name_hash: local_hash,
            qualified_name_idx,
        };
        let qname_idx = self.doc.qname_table.atomize(qname);

        // Allocate element node
        let elem_ref = self.doc.nodes.alloc()?;
        let mut elem_node = Node::default();
        elem_node.set_node_type(NodeType::Element);
        elem_node.parent = self.parent;
        elem_node.next_sibling = NULL;
        elem_node.value = qname_idx;
        self.doc.nodes.set(elem_ref, elem_node);

        // Link from last sibling
        if self.last_sibling != NULL {
            self.doc.nodes.update(self.last_sibling, |n| {
                n.next_sibling = elem_ref;
            });
        }

        // Set HAS_CHILDREN on parent
        self.doc
            .nodes
            .update(self.parent, |n| n.set_flag(Node::HAS_CHILDREN));

        // Element index (Full mode)
        if self.doc.kind == DocumentKind::Full {
            self.doc.element_index.add(local_hash, elem_ref);
        }

        // Namespace scope changed?
        if self.current_namespace != prev_namespace {
            self.namespace_stack.push((elem_ref, prev_namespace));
            self.doc.nodes.update(elem_ref, |n| {
                n.set_flag(Node::HAS_NMSP_DECLS);
            });
            self.doc
                .element_namespaces
                .insert(elem_ref, self.current_namespace);
        }

        // Push element state, descend
        self.element_stack.push(ElementBuildState {
            node_ref: elem_ref,
            has_attrs: false,
        });
        self.parent = elem_ref;
        self.last_sibling = NULL;
        self.last_attr = NULL;

        Ok(elem_ref)
    }

    /// Adds an attribute to the current element (two-node pair).
    pub fn attribute(
        &mut self,
        local_name: &str,
        ns_uri: &str,
        prefix: &str,
        value: &str,
    ) -> Result<u32, BufferDocumentError> {
        let local_id = self.doc.names.add(local_name);
        let uri_id = self.doc.names.add(ns_uri);
        let prefix_id = self.doc.names.add(prefix);

        let qualified_name_idx = if prefix.is_empty() {
            self.doc.strings.store(local_name)
        } else {
            self.doc.strings.store(&format!("{prefix}:{local_name}"))
        };
        let qname = QNameAtom {
            local_name: local_id,
            namespace_uri: uri_id,
            prefix: prefix_id,
            local_name_hash: 0, // attrs not indexed
            qualified_name_idx,
        };
        let qname_idx = self.doc.qname_table.atomize(qname);

        // Attribute node
        let attr_ref = self.doc.nodes.alloc()?;
        let mut attr_node = Node::default();
        attr_node.set_node_type(NodeType::Attribute);
        attr_node.parent = self.parent;
        attr_node.next_sibling = NULL;
        attr_node.value = qname_idx;
        self.doc.nodes.set(attr_ref, attr_node);

        // ChildValue node
        let val_idx = self.doc.strings.store(value);
        let cv_ref = self.doc.nodes.alloc()?;
        let mut cv_node = Node::default();
        cv_node.set_node_type(NodeType::ChildValue);
        cv_node.parent = attr_ref; // parent is the Attribute node
        cv_node.next_sibling = NULL;
        cv_node.value = val_idx;
        self.doc.nodes.set(cv_ref, cv_node);

        // Chain attributes
        if self.last_attr != NULL {
            self.doc.nodes.update(self.last_attr, |n| {
                n.next_sibling = attr_ref;
            });
        }
        self.last_attr = attr_ref;

        // Set HAS_ATTRIBUTE on parent
        self.doc
            .nodes
            .update(self.parent, |n| n.set_flag(Node::HAS_ATTRIBUTE));

        // Mark element as having attrs
        if let Some(state) = self.element_stack.last_mut() {
            state.has_attrs = true;
        }

        Ok(attr_ref)
    }

    /// Marks the end of attributes; subsequent content nodes are children.
    pub fn end_of_attributes(&mut self) {
        self.last_sibling = NULL;
        self.last_attr = NULL;
    }

    /// Accumulates text content; coalesced on the next structural event.
    pub fn text(&mut self, value: &str) {
        self.text_buffer.push_str(value);
        if self.text_type.is_none() {
            self.text_type = Some(NodeType::Text);
        }
    }

    /// Adds a comment node.
    pub fn comment(&mut self, value: &str) -> Result<(), BufferDocumentError> {
        self.flush_text()?;
        self.add_content_node(NodeType::Comment, value)?;
        Ok(())
    }

    /// Adds a processing instruction (two-node pair: PI + ChildValue).
    pub fn processing_instruction(
        &mut self,
        target: &str,
        data: &str,
    ) -> Result<(), BufferDocumentError> {
        self.flush_text()?;

        let target_idx = self.doc.strings.store(target);
        let pi_ref = self.doc.nodes.alloc()?;
        let mut pi_node = Node::default();
        pi_node.set_node_type(NodeType::ProcessingInstruction);
        pi_node.parent = self.parent;
        pi_node.next_sibling = NULL;
        pi_node.value = target_idx;
        self.doc.nodes.set(pi_ref, pi_node);

        let data_idx = self.doc.strings.store(data);
        let cv_ref = self.doc.nodes.alloc()?;
        let mut cv_node = Node::default();
        cv_node.set_node_type(NodeType::ChildValue);
        cv_node.parent = pi_ref;
        cv_node.next_sibling = NULL;
        cv_node.value = data_idx;
        self.doc.nodes.set(cv_ref, cv_node);

        // Link sibling
        if self.last_sibling != NULL {
            self.doc.nodes.update(self.last_sibling, |n| {
                n.next_sibling = pi_ref;
            });
        }
        self.last_sibling = pi_ref;

        // Set HAS_CHILDREN on parent
        self.doc
            .nodes
            .update(self.parent, |n| n.set_flag(Node::HAS_CHILDREN));

        Ok(())
    }

    /// Closes the current element.
    pub fn end_element(&mut self) -> Result<(), BufferDocumentError> {
        self.flush_text()?;

        let _state = self
            .element_stack
            .pop()
            .ok_or(BufferDocumentError::UnmatchedEndElement)?;

        // If element has namespace declarations, restore previous scope
        let elem_node = self.doc.nodes.get(self.parent);
        if elem_node.has_flag(Node::HAS_NMSP_DECLS) {
            if let Some((_elem_ref, prev_ns)) = self.namespace_stack.pop() {
                self.current_namespace = prev_ns;
            }
        }

        self.last_sibling = self.parent;
        self.parent = elem_node.parent;
        self.last_attr = NULL;

        Ok(())
    }

    /// Finalizes the document, appending the Nul sentinel.
    pub fn finalize(mut self) -> Result<BufferDocument<'a>, BufferDocumentError> {
        self.flush_text()?;

        // Allocate Nul sentinel
        let nul_ref = self.doc.nodes.alloc()?;
        let nul_node = Node::default(); // NodeType::Nul by default
        self.doc.nodes.set(nul_ref, nul_node);

        Ok(self.doc)
    }

    /// Sets the schema binding on a node, returning `true` if the type is complex.
    ///
    /// Returns [`BufferDocumentError::Overflow`] if the binding table is full.
    pub fn set_node_binding(
        &mut self,
        node_ref: u32,
        binding: NodeSchemaBinding,
    ) -> Result<bool, BufferDocumentError> {
        let idx = self.doc.binding_remap.register(binding)?;
        let is_complex = matches!(binding.type_key, crate::ids::TypeKey::Complex(_));
        self.doc.nodes.update(node_ref, |n| {
            n.set_binding_index(idx);
            if is_complex {
                n.set_flag(Node::IS_COMPLEX_TYPE);
            } else {
                n.clear_flag(Node::IS_COMPLEX_TYPE);
            }
        });
        Ok(is_complex)
    }

    /// Sets the `IS_NIL` flag on a node (xsi:nil="true").
    pub fn set_nil(&mut self, node_ref: u32) {
        self.doc.nodes.update(node_ref, |n| {
            n.set_flag(Node::IS_NIL);
        });
    }

    /// Registers an `xml:id` value for the given element.
    ///
    /// Returns [`BufferDocumentError::DuplicateId`] if the id has already
    /// been registered.  This is a no-op in `Fragment` mode.
    pub fn register_xml_id(&mut self, id: &str, elem_ref: u32) -> Result<(), BufferDocumentError> {
        if self.doc.kind != DocumentKind::Full {
            return Ok(());
        }
        let id_val: Box<str> = id.into();
        if self.doc.id_elements.contains_key(&id_val) {
            return Err(BufferDocumentError::DuplicateId(id_val.into_string()));
        }
        self.doc.id_elements.insert(id_val, elem_ref);
        Ok(())
    }

    /// Returns `true` when source location tracking is enabled.
    #[inline]
    pub fn track_source_locations(&self) -> bool {
        self.options.track_source_locations
    }

    /// Records a completed source span for a node.
    pub fn set_source_span(&mut self, node_ref: u32, span: SourceSpan) {
        self.doc.source_spans.set(node_ref, span);
    }

    // ── Internal helpers ──────────────────────────────────────────────

    /// Flushes accumulated text into a content node.
    fn flush_text(&mut self) -> Result<(), BufferDocumentError> {
        if let Some(nt) = self.text_type.take() {
            let value = std::mem::take(&mut self.text_buffer);
            if !value.is_empty() {
                self.add_content_node(nt, &value)?;
            }
        }
        Ok(())
    }

    /// Allocates a content node (Text, Comment, etc.) and links it.
    fn add_content_node(
        &mut self,
        node_type: NodeType,
        value: &str,
    ) -> Result<u32, BufferDocumentError> {
        let str_idx = self.doc.strings.store(value);
        let node_ref = self.doc.nodes.alloc()?;
        let mut node = Node::default();
        node.set_node_type(node_type);
        node.parent = self.parent;
        node.next_sibling = NULL;
        node.value = str_idx;
        self.doc.nodes.set(node_ref, node);

        if self.last_sibling != NULL {
            self.doc.nodes.update(self.last_sibling, |n| {
                n.next_sibling = node_ref;
            });
        }
        self.last_sibling = node_ref;

        self.doc
            .nodes
            .update(self.parent, |n| n.set_flag(Node::HAS_CHILDREN));

        Ok(node_ref)
    }

    /// Allocates a namespace node and chains it to `current_namespace`.
    fn handle_namespace_decl(
        &mut self,
        prefix: &str,
        uri: &str,
    ) -> Result<(), BufferDocumentError> {
        let prefix_id = self.doc.names.add(prefix);
        let uri_id = self.doc.names.add(uri);

        let ns_ref = self
            .doc
            .namespace_pages
            .alloc()
            .ok_or(BufferDocumentError::Overflow)?;
        self.doc.namespace_pages.set(
            ns_ref,
            NamespaceNode::new(prefix_id, uri_id, self.current_namespace),
        );
        self.current_namespace = ns_ref;

        Ok(())
    }

    // ── quick-xml adapter ─────────────────────────────────────────────

    /// Builds the document from a quick-xml event stream.
    pub fn build<R: BufRead>(
        mut self,
        reader: R,
    ) -> Result<BufferDocument<'a>, BufferDocumentError> {
        let mut xml_reader = Reader::from_reader(reader);
        xml_reader.trim_text(false);

        // Transient prefix → URI mapping for namespace resolution
        let mut prefix_map: HashMap<Box<[u8]>, Vec<String>> = HashMap::new();
        prefix_map
            .entry(b"xml".to_vec().into_boxed_slice())
            .or_default()
            .push(XML_NAMESPACE.to_string());
        prefix_map
            .entry(b"".to_vec().into_boxed_slice())
            .or_default()
            .push(String::new());

        // Per-element declared prefixes for cleanup on close
        let mut scope_decls: Vec<Vec<Box<[u8]>>> = Vec::new();

        let track = self.options.track_source_locations;
        let mut buf = Vec::with_capacity(1024);

        loop {
            let event_start = if track {
                xml_reader.buffer_position()
            } else {
                0
            };

            match xml_reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) => {
                    let elem_ref =
                        self.handle_start_or_empty(e, false, &mut prefix_map, &mut scope_decls)?;
                    if track {
                        self.pending_spans.push((elem_ref, event_start));
                    }
                }
                Ok(Event::Empty(ref e)) => {
                    let elem_ref =
                        self.handle_start_or_empty(e, true, &mut prefix_map, &mut scope_decls)?;
                    if track {
                        self.doc.source_spans.set(
                            elem_ref,
                            SourceSpan::new(event_start, xml_reader.buffer_position()),
                        );
                    }
                }
                Ok(Event::End(_)) => {
                    if track {
                        if let Some((elem_ref, start)) = self.pending_spans.pop() {
                            self.doc.source_spans.set(
                                elem_ref,
                                SourceSpan::new(start, xml_reader.buffer_position()),
                            );
                        }
                    }
                    // Pop namespace scope
                    if let Some(decls) = scope_decls.pop() {
                        for prefix_key in &decls {
                            if let Some(stack) = prefix_map.get_mut(prefix_key.as_ref()) {
                                stack.pop();
                            }
                        }
                    }
                    self.end_element()?;
                }
                Ok(Event::Text(ref e)) => {
                    if !self.element_stack.is_empty() {
                        let text = e.unescape()?;
                        self.text(&text);
                    }
                }
                Ok(Event::CData(ref e)) => {
                    if !self.element_stack.is_empty() {
                        let text = std::str::from_utf8(e)?;
                        self.text(text);
                    }
                }
                Ok(Event::Comment(ref e)) => {
                    let text = std::str::from_utf8(e)?;
                    self.comment(text)?;
                }
                Ok(Event::PI(ref e)) => {
                    let raw = std::str::from_utf8(e)?;
                    let (target, data) = parse_pi_content(raw);
                    self.processing_instruction(target, data)?;
                }
                Ok(Event::Decl(_) | Event::DocType(_)) => {}
                Ok(Event::Eof) => break,
                Err(e) => return Err(e.into()),
            }
            buf.clear();
        }

        self.finalize()
    }

    /// Handles `Event::Start` and `Event::Empty` elements.
    fn handle_start_or_empty(
        &mut self,
        e: &quick_xml::events::BytesStart<'_>,
        is_empty: bool,
        prefix_map: &mut HashMap<Box<[u8]>, Vec<String>>,
        scope_decls: &mut Vec<Vec<Box<[u8]>>>,
    ) -> Result<u32, BufferDocumentError> {
        let mut local_decls: Vec<Box<[u8]>> = Vec::new();
        let mut ns_decls_str: Vec<(String, String)> = Vec::new();

        // First pass: collect xmlns declarations
        for attr_result in e.attributes() {
            let attr = attr_result?;
            let key = attr.key.as_ref();

            if key == b"xmlns" {
                // Default namespace declaration
                let value = attr.unescape_value()?;
                let uri = value.to_string();
                let prefix_key: Box<[u8]> = b"".to_vec().into_boxed_slice();
                prefix_map
                    .entry(prefix_key.clone())
                    .or_default()
                    .push(uri.clone());
                local_decls.push(prefix_key);
                ns_decls_str.push((String::new(), uri));
            } else if key.starts_with(b"xmlns:") {
                let prefix_bytes = &key[6..];
                let value = attr.unescape_value()?;
                let uri = value.to_string();
                let prefix_key: Box<[u8]> = prefix_bytes.to_vec().into_boxed_slice();
                prefix_map
                    .entry(prefix_key.clone())
                    .or_default()
                    .push(uri.clone());
                local_decls.push(prefix_key);
                let prefix_str =
                    std::str::from_utf8(prefix_bytes).map_err(BufferDocumentError::Utf8)?;
                ns_decls_str.push((prefix_str.to_string(), uri));
            }
        }

        scope_decls.push(local_decls);

        // Build ns_declarations slice for start_element
        let ns_decl_refs: Vec<(&str, &str)> = ns_decls_str
            .iter()
            .map(|(p, u)| (p.as_str(), u.as_str()))
            .collect();

        // Resolve element name
        let full_name = e.name();
        let full_name_bytes = full_name.as_ref();
        let (elem_prefix_bytes, elem_local_bytes) = split_prefix_local(full_name_bytes);

        let elem_local =
            std::str::from_utf8(elem_local_bytes).map_err(BufferDocumentError::Utf8)?;
        let elem_prefix_str =
            std::str::from_utf8(elem_prefix_bytes).map_err(BufferDocumentError::Utf8)?;

        // Resolve element namespace
        let elem_ns_uri = match prefix_map.get(elem_prefix_bytes) {
            Some(stack) if !stack.is_empty() => stack.last().unwrap().as_str().to_string(),
            _ if elem_prefix_bytes.is_empty() => String::new(),
            _ => {
                return Err(BufferDocumentError::UnboundPrefix(
                    elem_prefix_str.to_string(),
                ))
            }
        };

        let elem_ref =
            self.start_element(elem_local, &elem_ns_uri, elem_prefix_str, &ns_decl_refs)?;

        // Second pass: non-xmlns attributes
        for attr_result in e.attributes() {
            let attr = attr_result?;
            let key = attr.key.as_ref();

            // Skip xmlns declarations
            if key == b"xmlns" || key.starts_with(b"xmlns:") {
                continue;
            }

            let (attr_prefix_bytes, attr_local_bytes) = split_prefix_local(key);
            let attr_local =
                std::str::from_utf8(attr_local_bytes).map_err(BufferDocumentError::Utf8)?;
            let attr_prefix_str =
                std::str::from_utf8(attr_prefix_bytes).map_err(BufferDocumentError::Utf8)?;

            // Resolve attr namespace: unprefixed → empty, prefixed → lookup
            let attr_ns_uri = if attr_prefix_bytes.is_empty() {
                String::new()
            } else {
                match prefix_map.get(attr_prefix_bytes) {
                    Some(stack) if !stack.is_empty() => stack.last().unwrap().as_str().to_string(),
                    _ => {
                        return Err(BufferDocumentError::UnboundPrefix(
                            attr_prefix_str.to_string(),
                        ))
                    }
                }
            };

            let unescaped = attr.unescape_value()?;
            self.attribute(attr_local, &attr_ns_uri, attr_prefix_str, &unescaped)?;

            // Detect xml:id
            if self.doc.kind == DocumentKind::Full
                && attr_local == "id"
                && attr_ns_uri == XML_NAMESPACE
            {
                let id_val: Box<str> = unescaped.as_ref().into();
                if self.doc.id_elements.contains_key(&id_val) {
                    return Err(BufferDocumentError::DuplicateId(id_val.into_string()));
                }
                self.doc.id_elements.insert(id_val, elem_ref);
            }
        }

        self.end_of_attributes();

        if is_empty {
            // Pop scope for empty element
            if let Some(decls) = scope_decls.pop() {
                for prefix_key in &decls {
                    if let Some(stack) = prefix_map.get_mut(prefix_key.as_ref()) {
                        stack.pop();
                    }
                }
            }
            self.end_element()?;
        }

        Ok(elem_ref)
    }
}

// ── Free functions ────────────────────────────────────────────────────

/// Splits `b"prefix:local"` into `(b"prefix", b"local")`.
/// If no colon, returns `(b"", full_name)`.
pub(crate) fn split_prefix_local(name: &[u8]) -> (&[u8], &[u8]) {
    match name.iter().position(|&b| b == b':') {
        Some(pos) => (&name[..pos], &name[pos + 1..]),
        None => (b"", name),
    }
}

/// Parses PI content into `(target, data)`.
pub(crate) fn parse_pi_content(raw: &str) -> (&str, &str) {
    let trimmed = raw.trim();
    match trimmed.find(|c: char| c.is_ascii_whitespace()) {
        Some(pos) => (&trimmed[..pos], trimmed[pos..].trim_start()),
        None => (trimmed, ""),
    }
}

// Convert AttrError → quick_xml::Error (already has From impl in quick-xml 0.31)
impl From<quick_xml::events::attributes::AttrError> for BufferDocumentError {
    fn from(e: quick_xml::events::attributes::AttrError) -> Self {
        BufferDocumentError::Parse(quick_xml::Error::from(e))
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::TypeKey;
    use crate::navigator::DomNavigator;

    fn make_builder<'a>(arena: &'a Bump, names: &'a NameTable) -> BufferDocumentBuilder<'a> {
        BufferDocumentBuilder::new(arena, names, None, BufferDocumentOptions::default()).unwrap()
    }

    fn make_builder_full<'a>(arena: &'a Bump, names: &'a NameTable) -> BufferDocumentBuilder<'a> {
        BufferDocumentBuilder::new(arena, names, None, BufferDocumentOptions::full()).unwrap()
    }

    // ── Core push API tests ───────────────────────────────────────────

    #[test]
    fn test_empty_document() {
        let arena = Bump::new();
        let names = NameTable::new();
        let builder = make_builder(&arena, &names);
        let doc = builder.finalize().unwrap();

        // Root(0) + Nul(1)
        assert_eq!(doc.nodes.len(), 2);
        assert_eq!(doc.nodes.get(0).node_type(), NodeType::Root);
        assert_eq!(doc.nodes.get(1).node_type(), NodeType::Nul);
    }

    #[test]
    fn test_single_element() {
        let arena = Bump::new();
        let names = NameTable::new();
        let mut builder = make_builder(&arena, &names);

        let elem = builder.start_element("root", "", "", &[]).unwrap();
        builder.end_of_attributes();
        builder.end_element().unwrap();

        let doc = builder.finalize().unwrap();

        let elem_node = doc.nodes.get(elem);
        assert_eq!(elem_node.node_type(), NodeType::Element);
        assert_eq!(elem_node.parent, 0); // Root
        assert!(doc.nodes.get(0).has_flag(Node::HAS_CHILDREN));
    }

    #[test]
    fn test_element_with_text() {
        let arena = Bump::new();
        let names = NameTable::new();
        let mut builder = make_builder(&arena, &names);

        builder.start_element("root", "", "", &[]).unwrap();
        builder.end_of_attributes();
        builder.text("hello world");
        builder.end_element().unwrap();

        let doc = builder.finalize().unwrap();

        // Root(0), Element(1), Text(2), Nul(3)
        assert_eq!(doc.nodes.len(), 4);
        let text_node = doc.nodes.get(2);
        assert_eq!(text_node.node_type(), NodeType::Text);
        assert_eq!(doc.strings.get(text_node.value), "hello world");
    }

    #[test]
    fn test_text_coalescing() {
        let arena = Bump::new();
        let names = NameTable::new();
        let mut builder = make_builder(&arena, &names);

        builder.start_element("root", "", "", &[]).unwrap();
        builder.end_of_attributes();
        builder.text("hello ");
        builder.text("world");
        builder.end_element().unwrap();

        let doc = builder.finalize().unwrap();

        // Root(0), Element(1), Text(2), Nul(3) — single coalesced text
        assert_eq!(doc.nodes.len(), 4);
        let text_node = doc.nodes.get(2);
        assert_eq!(text_node.node_type(), NodeType::Text);
        assert_eq!(doc.strings.get(text_node.value), "hello world");
    }

    #[test]
    fn test_element_with_attributes() {
        let arena = Bump::new();
        let names = NameTable::new();
        let mut builder = make_builder(&arena, &names);

        builder.start_element("root", "", "", &[]).unwrap();
        let attr1 = builder.attribute("id", "", "", "123").unwrap();
        let attr2 = builder.attribute("name", "", "", "test").unwrap();
        builder.end_of_attributes();
        builder.end_element().unwrap();

        let doc = builder.finalize().unwrap();

        // Root(0), Element(1), Attr1(2), CV1(3), Attr2(4), CV2(5), Nul(6)
        assert_eq!(doc.nodes.len(), 7);

        let a1 = doc.nodes.get(attr1);
        assert_eq!(a1.node_type(), NodeType::Attribute);
        assert_eq!(a1.parent, 1); // Element
        assert_eq!(a1.next_sibling, attr2); // chained

        let cv1 = doc.nodes.get(attr1 + 1);
        assert_eq!(cv1.node_type(), NodeType::ChildValue);
        assert_eq!(cv1.parent, attr1); // parent is attr, not element
        assert_eq!(doc.strings.get(cv1.value), "123");

        let a2 = doc.nodes.get(attr2);
        assert_eq!(a2.node_type(), NodeType::Attribute);
        assert_eq!(a2.next_sibling, NULL);

        assert!(doc.nodes.get(1).has_flag(Node::HAS_ATTRIBUTE));
    }

    #[test]
    fn test_nested_elements() {
        let arena = Bump::new();
        let names = NameTable::new();
        let mut builder = make_builder(&arena, &names);

        builder.start_element("a", "", "", &[]).unwrap();
        builder.end_of_attributes();

        let b = builder.start_element("b", "", "", &[]).unwrap();
        builder.end_of_attributes();
        builder.end_element().unwrap();

        builder.end_element().unwrap();

        let doc = builder.finalize().unwrap();

        let b_node = doc.nodes.get(b);
        assert_eq!(b_node.parent, 1); // "a" element
    }

    #[test]
    fn test_sibling_elements() {
        let arena = Bump::new();
        let names = NameTable::new();
        let mut builder = make_builder(&arena, &names);

        builder.start_element("root", "", "", &[]).unwrap();
        builder.end_of_attributes();

        let a = builder.start_element("a", "", "", &[]).unwrap();
        builder.end_of_attributes();
        builder.end_element().unwrap();

        let b = builder.start_element("b", "", "", &[]).unwrap();
        builder.end_of_attributes();
        builder.end_element().unwrap();

        builder.end_element().unwrap();

        let doc = builder.finalize().unwrap();

        let a_node = doc.nodes.get(a);
        assert_eq!(a_node.next_sibling, b);

        let b_node = doc.nodes.get(b);
        assert_eq!(b_node.next_sibling, NULL);
    }

    #[test]
    fn test_comment_node() {
        let arena = Bump::new();
        let names = NameTable::new();
        let mut builder = make_builder(&arena, &names);

        builder.start_element("root", "", "", &[]).unwrap();
        builder.end_of_attributes();
        builder.comment("a comment").unwrap();
        builder.end_element().unwrap();

        let doc = builder.finalize().unwrap();

        // Root(0), Element(1), Comment(2), Nul(3)
        let comment = doc.nodes.get(2);
        assert_eq!(comment.node_type(), NodeType::Comment);
        assert_eq!(doc.strings.get(comment.value), "a comment");
    }

    #[test]
    fn test_processing_instruction() {
        let arena = Bump::new();
        let names = NameTable::new();
        let mut builder = make_builder(&arena, &names);

        builder.start_element("root", "", "", &[]).unwrap();
        builder.end_of_attributes();
        builder
            .processing_instruction("target", "data here")
            .unwrap();
        builder.end_element().unwrap();

        let doc = builder.finalize().unwrap();

        // Root(0), Element(1), PI(2), ChildValue(3), Nul(4)
        let pi = doc.nodes.get(2);
        assert_eq!(pi.node_type(), NodeType::ProcessingInstruction);
        assert_eq!(doc.strings.get(pi.value), "target");

        let cv = doc.nodes.get(3);
        assert_eq!(cv.node_type(), NodeType::ChildValue);
        assert_eq!(cv.parent, 2); // PI node
        assert_eq!(doc.strings.get(cv.value), "data here");
    }

    #[test]
    fn test_namespace_declarations() {
        let arena = Bump::new();
        let names = NameTable::new();
        let mut builder = make_builder(&arena, &names);

        builder
            .start_element(
                "root",
                "http://example.com",
                "ex",
                &[("ex", "http://example.com")],
            )
            .unwrap();
        builder.end_of_attributes();
        builder.end_element().unwrap();

        let doc = builder.finalize().unwrap();

        let elem = doc.nodes.get(1);
        assert!(elem.has_flag(Node::HAS_NMSP_DECLS));
        assert!(doc.element_namespaces.contains_key(&1));
    }

    #[test]
    fn test_namespace_scope_restore() {
        let arena = Bump::new();
        let names = NameTable::new();
        let mut builder = make_builder(&arena, &names);

        // Outer element declares ns
        builder
            .start_element(
                "outer",
                "http://outer.com",
                "o",
                &[("o", "http://outer.com")],
            )
            .unwrap();
        builder.end_of_attributes();

        // Inner element declares different ns with same prefix
        builder
            .start_element(
                "inner",
                "http://inner.com",
                "o",
                &[("o", "http://inner.com")],
            )
            .unwrap();
        builder.end_of_attributes();
        builder.end_element().unwrap();

        builder.end_element().unwrap();

        let doc = builder.finalize().unwrap();

        // Both elements should have HAS_NMSP_DECLS
        assert!(doc.nodes.get(1).has_flag(Node::HAS_NMSP_DECLS));
        // Inner element at index 2
        assert!(doc.nodes.get(2).has_flag(Node::HAS_NMSP_DECLS));
    }

    #[test]
    fn test_element_index_full_mode() {
        let arena = Bump::new();
        let names = NameTable::new();
        let mut builder = make_builder_full(&arena, &names);

        let elem = builder.start_element("item", "", "", &[]).unwrap();
        builder.end_of_attributes();
        builder.end_element().unwrap();

        let doc = builder.finalize().unwrap();

        let h = hash_name("item");
        let found = doc.element_index.find(h);
        assert_eq!(found, &[elem]);
    }

    #[test]
    fn test_set_node_binding() {
        let arena = Bump::new();
        let names = NameTable::new();
        let mut builder = make_builder(&arena, &names);

        let elem = builder.start_element("root", "", "", &[]).unwrap();
        builder.end_of_attributes();

        // Create a complex type key
        use slotmap::SlotMap;
        let mut sm: SlotMap<crate::ids::ComplexTypeKey, ()> = SlotMap::with_key();
        let ck = sm.insert(());

        let binding = NodeSchemaBinding {
            type_key: TypeKey::Complex(ck),
            element_decl: None,
            attribute_decl: None,
            content_type: None,
        };

        let is_complex = builder.set_node_binding(elem, binding).unwrap();
        assert!(is_complex);

        builder.end_element().unwrap();
        let doc = builder.finalize().unwrap();

        let node = doc.nodes.get(elem);
        assert!(node.has_flag(Node::IS_COMPLEX_TYPE));
        assert!(node.binding_index() > 0);
    }

    #[test]
    fn test_set_nil() {
        let arena = Bump::new();
        let names = NameTable::new();
        let mut builder = make_builder(&arena, &names);

        let elem = builder.start_element("root", "", "", &[]).unwrap();
        builder.end_of_attributes();

        builder.set_nil(elem);

        builder.end_element().unwrap();
        let doc = builder.finalize().unwrap();

        let node = doc.nodes.get(elem);
        assert!(node.has_flag(Node::IS_NIL));
    }

    // ── quick-xml adapter tests ───────────────────────────────────────

    fn build_from_str(xml: &str) -> BufferDocument<'_> {
        let arena = Bump::new();
        let names = NameTable::new();
        // We need to leak arena/names for the lifetime to work in tests.
        // Use Box::leak for test convenience.
        let arena = Box::leak(Box::new(arena));
        let names = Box::leak(Box::new(names));
        let builder =
            BufferDocumentBuilder::new(arena, names, None, BufferDocumentOptions::default())
                .unwrap();
        builder.build(xml.as_bytes()).unwrap()
    }

    fn build_from_str_full(xml: &str) -> BufferDocument<'_> {
        let arena = Box::leak(Box::new(Bump::new()));
        let names = Box::leak(Box::new(NameTable::new()));
        let builder =
            BufferDocumentBuilder::new(arena, names, None, BufferDocumentOptions::full()).unwrap();
        builder.build(xml.as_bytes()).unwrap()
    }

    #[test]
    fn test_build_simple() {
        let doc = build_from_str("<root/>");
        // Root(0), Element(1), Nul(2)
        assert_eq!(doc.nodes.len(), 3);
        assert_eq!(doc.nodes.get(1).node_type(), NodeType::Element);
    }

    #[test]
    fn test_build_nested() {
        let doc = build_from_str("<a><b>text</b></a>");
        // Root(0), a(1), b(2), Text(3), Nul(4)
        assert_eq!(doc.nodes.len(), 5);
        assert_eq!(doc.nodes.get(2).parent, 1); // b's parent is a
        let text = doc.nodes.get(3);
        assert_eq!(text.node_type(), NodeType::Text);
        assert_eq!(doc.strings.get(text.value), "text");
    }

    #[test]
    fn test_build_attributes() {
        let doc = build_from_str(r#"<root attr="val"/>"#);
        // Root(0), Element(1), Attr(2), CV(3), Nul(4)
        assert_eq!(doc.nodes.len(), 5);
        assert!(doc.nodes.get(1).has_flag(Node::HAS_ATTRIBUTE));
        let cv = doc.nodes.get(3);
        assert_eq!(doc.strings.get(cv.value), "val");
    }

    #[test]
    fn test_build_namespace_prefixed() {
        let doc = build_from_str(r#"<ns:root xmlns:ns="http://example.com"/>"#);
        let elem = doc.nodes.get(1);
        assert_eq!(elem.node_type(), NodeType::Element);
        assert!(elem.has_flag(Node::HAS_NMSP_DECLS));

        let qname = doc.qname_table.get(elem.value);
        assert_eq!(doc.names.resolve(qname.local_name), "root");
        assert_eq!(doc.names.resolve(qname.namespace_uri), "http://example.com");
        assert_eq!(doc.names.resolve(qname.prefix), "ns");
    }

    #[test]
    fn test_build_default_namespace() {
        let doc = build_from_str(r#"<root xmlns="http://default.com"><child/></root>"#);
        // child inherits default namespace
        let child = doc.nodes.get(2);
        let child_qname = doc.qname_table.get(child.value);
        assert_eq!(
            doc.names.resolve(child_qname.namespace_uri),
            "http://default.com"
        );
    }

    #[test]
    fn test_build_namespace_override() {
        let doc = build_from_str(
            r#"<root xmlns="http://outer.com"><child xmlns="http://inner.com"/></root>"#,
        );
        let root = doc.nodes.get(1);
        let root_qname = doc.qname_table.get(root.value);
        assert_eq!(
            doc.names.resolve(root_qname.namespace_uri),
            "http://outer.com"
        );

        let child = doc.nodes.get(2);
        let child_qname = doc.qname_table.get(child.value);
        assert_eq!(
            doc.names.resolve(child_qname.namespace_uri),
            "http://inner.com"
        );
    }

    #[test]
    fn test_build_cdata_coalescing() {
        let doc = build_from_str("<root>hello <![CDATA[world]]></root>");
        // Text should be coalesced: "hello world"
        let text = doc.nodes.get(2);
        assert_eq!(text.node_type(), NodeType::Text);
        assert_eq!(doc.strings.get(text.value), "hello world");
    }

    #[test]
    fn test_build_comment() {
        let doc = build_from_str("<root><!-- a comment --></root>");
        let comment = doc.nodes.get(2);
        assert_eq!(comment.node_type(), NodeType::Comment);
        assert_eq!(doc.strings.get(comment.value), " a comment ");
    }

    #[test]
    fn test_build_pi() {
        let doc = build_from_str("<root><?target data?></root>");
        let pi = doc.nodes.get(2);
        assert_eq!(pi.node_type(), NodeType::ProcessingInstruction);
        assert_eq!(doc.strings.get(pi.value), "target");

        let cv = doc.nodes.get(3);
        assert_eq!(cv.node_type(), NodeType::ChildValue);
        assert_eq!(doc.strings.get(cv.value), "data");
    }

    #[test]
    fn test_build_mixed_content() {
        let doc = build_from_str("<root>text<!-- comment --><child/>more</root>");
        // Root(0), root(1), Text(2), Comment(3), child(4), Text(5), Nul(6)
        assert_eq!(doc.nodes.get(2).node_type(), NodeType::Text);
        assert_eq!(doc.strings.get(doc.nodes.get(2).value), "text");
        assert_eq!(doc.nodes.get(3).node_type(), NodeType::Comment);
        assert_eq!(doc.nodes.get(4).node_type(), NodeType::Element);
        assert_eq!(doc.nodes.get(5).node_type(), NodeType::Text);
        assert_eq!(doc.strings.get(doc.nodes.get(5).value), "more");
    }

    #[test]
    fn test_build_source_spans() {
        let doc = build_from_str_full("<root><child/></root>");
        // Elements should have spans
        assert!(doc.source_spans.get(1).is_some()); // root
        assert!(doc.source_spans.get(2).is_some()); // child (empty)
    }

    #[test]
    fn test_build_no_source_spans_when_disabled() {
        let doc = build_from_str("<root><child/></root>");
        assert!(doc.source_spans.is_empty());
    }

    #[test]
    fn test_build_xml_id() {
        let doc = build_from_str_full(r#"<root xml:id="myid"/>"#);
        assert_eq!(doc.get_element_by_id("myid"), Some(1));
    }

    #[test]
    fn test_build_xml_id_duplicate_error() {
        let arena = Box::leak(Box::new(Bump::new()));
        let names = Box::leak(Box::new(NameTable::new()));
        let builder =
            BufferDocumentBuilder::new(arena, names, None, BufferDocumentOptions::full()).unwrap();
        let result = builder.build(r#"<root><a xml:id="dup"/><b xml:id="dup"/></root>"#.as_bytes());
        assert!(matches!(result, Err(BufferDocumentError::DuplicateId(_))));
    }

    #[test]
    fn test_build_unbound_prefix_error() {
        let arena = Box::leak(Box::new(Bump::new()));
        let names = Box::leak(Box::new(NameTable::new()));
        let builder =
            BufferDocumentBuilder::new(arena, names, None, BufferDocumentOptions::default())
                .unwrap();
        let result = builder.build(r#"<ns:root/>"#.as_bytes());
        assert!(matches!(result, Err(BufferDocumentError::UnboundPrefix(_))));
    }

    #[test]
    fn test_build_nul_sentinel() {
        let doc = build_from_str("<root/>");
        let last = doc.nodes.len() - 1;
        assert_eq!(doc.nodes.get(last).node_type(), NodeType::Nul);
    }

    #[test]
    fn test_build_document_level_whitespace_ignored() {
        // Whitespace/text outside the document element is discarded per XPath data model,
        // but comments and PIs at document level are preserved.
        let doc = build_from_str("<!-- prolog -->\n<root/>\n<!-- epilog -->");
        // Root(0), Comment(1), Element(2), Comment(3), Nul(4)
        // The \n between constructs must NOT produce Text nodes.
        assert_eq!(doc.nodes.len(), 5);
        assert_eq!(doc.nodes.get(1).node_type(), NodeType::Comment);
        assert_eq!(doc.nodes.get(2).node_type(), NodeType::Element);
        assert_eq!(doc.nodes.get(3).node_type(), NodeType::Comment);
        assert_eq!(doc.nodes.get(4).node_type(), NodeType::Nul);
    }

    // ── Fragment mode helpers ────────────────────────────────────────────

    fn make_builder_fragment<'a>(
        arena: &'a Bump,
        names: &'a NameTable,
    ) -> BufferDocumentBuilder<'a> {
        BufferDocumentBuilder::new(arena, names, None, BufferDocumentOptions::fragment()).unwrap()
    }

    // ── Fragment mode tests ──────────────────────────────────────────────

    #[test]
    fn fragment_build_navigate() {
        let arena = Bump::new();
        let names = NameTable::new();
        let mut builder = make_builder_fragment(&arena, &names);

        let elem = builder.start_element("item", "", "", &[]).unwrap();
        builder.end_of_attributes();
        builder.text("value");
        builder.end_element().unwrap();

        let doc = builder.finalize().unwrap();

        // Navigate: Root → element → text
        let mut nav = doc.create_navigator();
        assert!(nav.move_to_first_child()); // element
        assert_eq!(nav.current_ref(), elem);
        assert!(nav.move_to_first_child()); // text child
    }

    #[test]
    fn fragment_root_is_synthetic() {
        let arena = Bump::new();
        let names = NameTable::new();
        let mut builder = make_builder_fragment(&arena, &names);

        builder.start_element("item", "", "", &[]).unwrap();
        builder.end_of_attributes();
        builder.end_element().unwrap();

        let doc = builder.finalize().unwrap();

        // Root node is synthetic
        let root = doc.nodes.get(0);
        assert_eq!(root.node_type(), NodeType::Root);

        // move_to_parent from Root returns false (boundary)
        let mut nav = doc.create_navigator(); // at root
        assert!(!nav.move_to_parent());
    }

    #[test]
    fn fragment_navigation_boundary() {
        let arena = Bump::new();
        let names = NameTable::new();
        let mut builder = make_builder_fragment(&arena, &names);

        builder.start_element("item", "", "", &[]).unwrap();
        builder.end_of_attributes();
        builder.end_element().unwrap();

        let doc = builder.finalize().unwrap();

        let mut nav = doc.create_navigator();
        assert!(nav.move_to_first_child()); // element
        assert!(nav.move_to_parent()); // back to Root
        assert!(!nav.move_to_parent()); // boundary — Root has parent=NULL
    }

    #[test]
    fn fragment_skips_element_index() {
        let arena = Bump::new();
        let names = NameTable::new();
        let mut builder = make_builder_fragment(&arena, &names);

        builder.start_element("item", "", "", &[]).unwrap();
        builder.end_of_attributes();
        builder.end_element().unwrap();

        let doc = builder.finalize().unwrap();

        let h = hash_name("item");
        assert!(
            doc.element_index.find(h).is_empty(),
            "Fragment mode should not populate element_index"
        );
    }

    #[test]
    fn fragment_skips_id_registration() {
        let arena = Bump::new();
        let names = NameTable::new();
        let mut builder = make_builder_fragment(&arena, &names);

        let elem = builder.start_element("item", "", "", &[]).unwrap();
        builder.end_of_attributes();
        // Manually register an xml:id — should be a no-op in fragment mode
        builder.register_xml_id("myid", elem).unwrap();
        builder.end_element().unwrap();

        let doc = builder.finalize().unwrap();
        assert_eq!(
            doc.get_element_by_id("myid"),
            None,
            "Fragment mode register_xml_id should be no-op"
        );
    }

    #[test]
    fn fragment_namespace_inheritance() {
        let arena = Bump::new();
        let names = NameTable::new();
        let mut builder = make_builder_fragment(&arena, &names);

        builder
            .start_element(
                "outer",
                "http://example.com",
                "ex",
                &[("ex", "http://example.com")],
            )
            .unwrap();
        builder.end_of_attributes();

        // Child should inherit the namespace
        let child = builder
            .start_element("inner", "http://example.com", "ex", &[])
            .unwrap();
        builder.end_of_attributes();
        builder.end_element().unwrap();

        builder.end_element().unwrap();

        let doc = builder.finalize().unwrap();

        let child_qname = doc.qname_table.get(doc.nodes.get(child).value);
        assert_eq!(
            doc.names.resolve(child_qname.namespace_uri),
            "http://example.com",
            "child should inherit parent namespace in fragment mode"
        );
    }

    #[test]
    fn fragment_push_api_parity() {
        // Build same structure in Full and Fragment mode — node types should match
        let arena_full = Bump::new();
        let names_full = NameTable::new();
        let mut b_full = make_builder(&arena_full, &names_full);

        let arena_frag = Bump::new();
        let names_frag = NameTable::new();
        let mut b_frag = make_builder_fragment(&arena_frag, &names_frag);

        for b in [&mut b_full as &mut BufferDocumentBuilder, &mut b_frag] {
            b.start_element("root", "", "", &[]).unwrap();
            b.attribute("id", "", "", "1").unwrap();
            b.end_of_attributes();
            b.text("hello");
            b.end_element().unwrap();
        }

        let doc_full = b_full.finalize().unwrap();
        let doc_frag = b_frag.finalize().unwrap();

        assert_eq!(doc_full.nodes.len(), doc_frag.nodes.len());
        for i in 0..doc_full.nodes.len() {
            assert_eq!(
                doc_full.nodes.get(i).node_type(),
                doc_frag.nodes.get(i).node_type(),
                "node type mismatch at index {i}"
            );
        }
    }
}
