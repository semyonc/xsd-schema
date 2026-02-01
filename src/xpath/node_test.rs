//! XPath node test matching helpers.
//!
//! Provides a unified node test type that can be used by axis iterators
//! and type-based filters, aligning with `XPATH_ITERATOR_PORT_PLAN.md`.

use crate::ids::TypeKey;
use crate::namespace::qname::QualifiedName;
use crate::schema::model::DerivationSet;
use crate::types::value::XmlValue;
use crate::types::{ItemType, NameTest, SequenceType};
use crate::xpath::ast::{ItemTypeNode, KindTest};
use crate::xpath::cast::type_matches;
use crate::xpath::iterator::XmlItem;

use super::{DomNavigator, DomNodeType};
use super::context::XPathContext;

/// Unified node test for axis iterators.
#[derive(Debug, Clone)]
pub enum NodeTest {
    /// Name test (`*`, `*:local`, `prefix:*`, or QName).
    Name(NameTest),
    /// Sequence type test (`node()`, `element(...)`, etc.).
    Type(SequenceType),
}

impl NodeTest {
    pub fn matches<N: DomNavigator>(&self, nav: &N, ctx: &XPathContext<'_>) -> bool {
        match self {
            NodeTest::Name(test) => matches_name_test(test, nav, ctx),
            NodeTest::Type(seq) => matches_sequence_type(seq, nav, ctx),
        }
    }
}

pub fn matches_name_test<N: DomNavigator>(
    test: &NameTest,
    nav: &N,
    ctx: &XPathContext<'_>,
) -> bool {
    if nav.node_type() != DomNodeType::Element && nav.node_type() != DomNodeType::Attribute {
        return false;
    }

    match test {
        NameTest::Wildcard => true,
        NameTest::NamespaceWildcard(local_id) => {
            // *:local - match any namespace with specific local name
            match ctx.resolve_name(*local_id) {
                Some(local) => nav.local_name() == local,
                None => false,
            }
        }
        NameTest::LocalWildcard(ns_id) => {
            // prefix:* - match any local name in specific namespace
            match ctx.resolve_name(*ns_id) {
                Some(ns) => nav.namespace_uri() == ns,
                None => false,
            }
        }
        NameTest::QName(qname) => qname_matches(qname, nav, ctx),
    }
}

pub fn matches_sequence_type<N: DomNavigator>(
    sequence: &SequenceType,
    nav: &N,
    ctx: &XPathContext<'_>,
) -> bool {
    matches_item_type(&sequence.item_type, nav, ctx)
}

fn matches_item_type<N: DomNavigator>(
    item_type: &ItemType,
    nav: &N,
    ctx: &XPathContext<'_>,
) -> bool {
    match item_type {
        ItemType::AnyItem | ItemType::AnyNode => true,
        ItemType::Document(None) => nav.node_type() == DomNodeType::Root,
        ItemType::Document(Some(inner)) => match_document_with_inner(inner, nav, ctx),
        ItemType::Element(name_test, schema_type) => {
            if nav.node_type() != DomNodeType::Element {
                return false;
            }
            if let Some(test) = name_test {
                if !matches_name_test(test, nav, ctx) {
                    return false;
                }
            }
            if let Some(expected) = schema_type {
                // Use derivation checking if schema_set is available
                if let Some(actual) = nav.schema_type() {
                    if let Some(schema_set) = ctx.schema_set {
                        // Check if actual type is derived from expected type
                        // Using empty DerivationSet means any derivation method is allowed
                        if !schema_set.is_type_derived_from(
                            TypeKey::Simple(actual),
                            TypeKey::Simple(*expected),
                            DerivationSet::empty(),
                        ) {
                            return false;
                        }
                    } else {
                        // Fallback to equality without schema set
                        if actual != *expected {
                            return false;
                        }
                    }
                } else {
                    // No schema type on node, fail the type match
                    return false;
                }
            }
            true
        }
        ItemType::Attribute(name_test, schema_type) => {
            if nav.node_type() != DomNodeType::Attribute {
                return false;
            }
            if let Some(test) = name_test {
                if !matches_name_test(test, nav, ctx) {
                    return false;
                }
            }
            if let Some(expected) = schema_type {
                // Use derivation checking if schema_set is available
                if let Some(actual) = nav.schema_type() {
                    if let Some(schema_set) = ctx.schema_set {
                        // Check if actual type is derived from expected type
                        if !schema_set.is_type_derived_from(
                            TypeKey::Simple(actual),
                            TypeKey::Simple(*expected),
                            DerivationSet::empty(),
                        ) {
                            return false;
                        }
                    } else {
                        // Fallback to equality without schema set
                        if actual != *expected {
                            return false;
                        }
                    }
                } else {
                    // No schema type on node, fail the type match
                    return false;
                }
            }
            true
        }
        ItemType::SchemaElement(name) => {
            if nav.node_type() != DomNodeType::Element {
                return false;
            }
            // Check element name matches
            if !qname_matches(name, nav, ctx) {
                return false;
            }
            // If schema_set available, validate declaration exists and type derivation
            if let Some(schema_set) = ctx.schema_set {
                // Lookup element declaration - must exist for schema-element() to match
                let ns_id = name.namespace_uri;
                let Some(elem_key) = schema_set.lookup_element(ns_id, name.local_name) else {
                    // Declaration not found in schema - no match
                    return false;
                };
                let Some(elem_data) = schema_set.arenas.elements.get(elem_key) else {
                    return false;
                };
                // Check type derivation if declaration has resolved_type
                if let Some(expected_type) = elem_data.resolved_type {
                    let Some(actual_type) = nav.schema_type() else {
                        // Node has no type annotation but declaration expects one
                        return false;
                    };
                    // Node type must derive from declaration type
                    return schema_set.is_type_derived_from(
                        TypeKey::Simple(actual_type),
                        expected_type,
                        DerivationSet::empty(),
                    );
                }
                // Declaration found, no type constraint - match
                return true;
            }
            // No schema context - fall back to name-only match
            true
        }
        ItemType::SchemaAttribute(name) => {
            if nav.node_type() != DomNodeType::Attribute {
                return false;
            }
            // Check attribute name matches
            if !qname_matches(name, nav, ctx) {
                return false;
            }
            // If schema_set available, validate declaration exists and type derivation
            if let Some(schema_set) = ctx.schema_set {
                // Lookup attribute declaration - must exist for schema-attribute() to match
                let ns_id = name.namespace_uri;
                let Some(attr_key) = schema_set.lookup_attribute(ns_id, name.local_name) else {
                    // Declaration not found in schema - no match
                    return false;
                };
                let Some(attr_data) = schema_set.arenas.attributes.get(attr_key) else {
                    return false;
                };
                // Check type derivation if declaration has resolved_type
                if let Some(expected_type) = attr_data.resolved_type {
                    let Some(actual_type) = nav.schema_type() else {
                        // Node has no type annotation but declaration expects one
                        return false;
                    };
                    // Node type must derive from declaration type
                    return schema_set.is_type_derived_from(
                        TypeKey::Simple(actual_type),
                        expected_type,
                        DerivationSet::empty(),
                    );
                }
                // Declaration found, no type constraint - match
                return true;
            }
            // No schema context - fall back to name-only match
            true
        }
        ItemType::Text => nav.node_type().is_text_like(),
        ItemType::Comment => nav.node_type() == DomNodeType::Comment,
        ItemType::ProcessingInstruction(target) => {
            nav.node_type() == DomNodeType::ProcessingInstruction
                && target.as_ref().is_none_or(|name| nav.local_name() == name)
        }
        ItemType::NamespaceNode => nav.node_type() == DomNodeType::Namespace,
        ItemType::AtomicType(_) | ItemType::SchemaAtomicType(_) => false,
    }
}

fn match_document_with_inner<N: DomNavigator>(
    inner: &ItemType,
    nav: &N,
    ctx: &XPathContext<'_>,
) -> bool {
    if nav.node_type() != DomNodeType::Root {
        return false;
    }

    let mut cursor = nav.clone();
    if !cursor.move_to_first_child() {
        return false;
    }

    loop {
        if matches_item_type(inner, &cursor, ctx) {
            return true;
        }
        if !cursor.move_to_next_sibling() {
            break;
        }
    }

    false
}

fn qname_matches<N: DomNavigator>(
    qname: &QualifiedName,
    nav: &N,
    ctx: &XPathContext<'_>,
) -> bool {
    let local = match ctx.resolve_name(qname.local_name) {
        Some(local) => local,
        None => return false,
    };
    let ns = match qname.namespace_uri {
        Some(id) => match ctx.resolve_name(id) {
            Some(ns) => ns,
            None => return false,
        },
        None => String::new(),
    };

    nav.local_name() == local && nav.namespace_uri() == ns
}

// ============================================================================
// AST KindTest and ItemTypeNode Matching
// ============================================================================

/// Check if an XmlItem matches an AST ItemTypeNode.
///
/// This is used for `instance of` and `treat as` expressions to check
/// if a value matches the target type specification.
///
/// # Arguments
///
/// * `item` - The item to check (node or atomic value)
/// * `item_type` - The AST item type node to match against
/// * `resolved_atomic_type` - The resolved QualifiedName for atomic types (from binding)
/// * `ctx` - The XPath context for name resolution
///
/// # Returns
///
/// `true` if the item matches the item type, `false` otherwise.
pub fn matches_item_type_node<N: DomNavigator>(
    item: &XmlItem<N>,
    item_type: &ItemTypeNode,
    resolved_atomic_type: Option<&QualifiedName>,
    ctx: &XPathContext<'_>,
) -> bool {
    match item_type {
        ItemTypeNode::Item => {
            // item() matches any item (node or atomic)
            true
        }
        ItemTypeNode::Atomic(_) => {
            // Atomic type - item must be an atomic value matching the type
            match item {
                XmlItem::Node(_) => false,
                XmlItem::Atomic(value) => {
                    // Use the resolved atomic type from binding
                    if let Some(qname) = resolved_atomic_type {
                        matches_atomic_type(value, qname, ctx)
                    } else {
                        // No resolved type - this shouldn't happen after binding
                        false
                    }
                }
            }
        }
        ItemTypeNode::Kind(kind_test) => {
            // Kind test - item must be a node matching the kind test
            match item {
                XmlItem::Node(nav) => matches_kind_test(nav, kind_test, ctx),
                XmlItem::Atomic(_) => false,
            }
        }
    }
}

/// Check if an atomic value matches a resolved atomic type QualifiedName.
fn matches_atomic_type(
    value: &XmlValue,
    qname: &QualifiedName,
    ctx: &XPathContext<'_>,
) -> bool {
    use crate::namespace::table::well_known;
    use crate::xpath::cast::resolved_type_to_type_code;

    // Verify it's in XS namespace
    match qname.namespace_uri {
        Some(ns_id) if ns_id == well_known::XS_NAMESPACE => {}
        _ => return false,
    }

    // Get the target type code
    let target_type = match resolved_type_to_type_code(qname, ctx.names) {
        Ok(tc) => tc,
        Err(_) => return false,
    };

    // Check if the value's type matches
    type_matches(value.type_code, target_type)
}

/// Check if a DOM node matches an AST KindTest.
///
/// This converts the AST KindTest to runtime type checks.
pub fn matches_kind_test<N: DomNavigator>(
    nav: &N,
    kind_test: &KindTest,
    ctx: &XPathContext<'_>,
) -> bool {
    match kind_test {
        KindTest::AnyKind => {
            // node() matches any node
            true
        }
        KindTest::Text => {
            nav.node_type().is_text_like()
        }
        KindTest::Comment => {
            nav.node_type() == DomNodeType::Comment
        }
        KindTest::ProcessingInstruction(target) => {
            if nav.node_type() != DomNodeType::ProcessingInstruction {
                return false;
            }
            match target {
                None => true,
                Some(name) => nav.local_name() == *name,
            }
        }
        KindTest::Document(inner) => {
            if nav.node_type() != DomNodeType::Root {
                return false;
            }
            match inner {
                None => true,
                Some(inner_kind) => {
                    // document-node(element(...)) - check if document has matching element
                    let mut cursor = nav.clone();
                    if !cursor.move_to_first_child() {
                        return false;
                    }
                    loop {
                        if matches_kind_test(&cursor, inner_kind, ctx) {
                            return true;
                        }
                        if !cursor.move_to_next_sibling() {
                            break;
                        }
                    }
                    false
                }
            }
        }
        KindTest::Element(elem_test) => {
            if nav.node_type() != DomNodeType::Element {
                return false;
            }
            // Check element name if specified
            if let Some(ref qname) = elem_test.name {
                if !ast_qname_matches(qname, nav, ctx) {
                    return false;
                }
            }
            // TODO: Check type annotation if specified (elem_test.type_name)
            true
        }
        KindTest::Attribute(attr_test) => {
            if nav.node_type() != DomNodeType::Attribute {
                return false;
            }
            // Check attribute name if specified
            if let Some(ref qname) = attr_test.name {
                if !ast_qname_matches(qname, nav, ctx) {
                    return false;
                }
            }
            // TODO: Check type annotation if specified (attr_test.type_name)
            true
        }
        KindTest::SchemaElement(name) => {
            if nav.node_type() != DomNodeType::Element {
                return false;
            }
            // Parse the QName string to extract prefix and local name
            use crate::xpath::functions::qname::parse_lexical_qname;
            let Ok((prefix_opt, local_name)) = parse_lexical_qname(name) else {
                return false; // Invalid QName syntax
            };
            // Check local name matches
            if nav.local_name() != local_name {
                return false;
            }
            // Resolve namespace: use prefix if provided, otherwise default element namespace
            let expected_ns = if let Some(prefix) = &prefix_opt {
                ctx.resolve_prefix(prefix).unwrap_or_default()
            } else {
                ctx.default_element_ns
                    .and_then(|id| ctx.names.try_resolve(id))
                    .unwrap_or_default()
            };
            // Verify node's namespace matches expected
            if nav.namespace_uri() != expected_ns {
                return false;
            }
            // If schema_set available, validate declaration exists and type
            if let Some(schema_set) = ctx.schema_set {
                // Get local name as NameId - if not found, declaration doesn't exist
                let Some(local_id) = ctx.names.get(&local_name) else {
                    return false;
                };
                // Get namespace as NameId
                let ns_id = if expected_ns.is_empty() {
                    None
                } else {
                    ctx.names.get(&expected_ns)
                };
                // Lookup element declaration - must exist for schema-element() to match
                let Some(elem_key) = schema_set.lookup_element(ns_id, local_id) else {
                    return false;
                };
                let Some(elem_data) = schema_set.arenas.elements.get(elem_key) else {
                    return false;
                };
                // Check type derivation if declaration has resolved_type
                if let Some(expected_type) = elem_data.resolved_type {
                    let Some(actual_type) = nav.schema_type() else {
                        return false;
                    };
                    return schema_set.is_type_derived_from(
                        TypeKey::Simple(actual_type),
                        expected_type,
                        DerivationSet::empty(),
                    );
                }
                // Declaration found, no type constraint - match
                return true;
            }
            // No schema context - name and namespace already verified
            true
        }
        KindTest::SchemaAttribute(name) => {
            if nav.node_type() != DomNodeType::Attribute {
                return false;
            }
            // Parse the QName string to extract prefix and local name
            use crate::xpath::functions::qname::parse_lexical_qname;
            let Ok((prefix_opt, local_name)) = parse_lexical_qname(name) else {
                return false; // Invalid QName syntax
            };
            // Check local name matches
            if nav.local_name() != local_name {
                return false;
            }
            // Resolve namespace: use prefix if provided, otherwise empty (attributes default to no namespace)
            let expected_ns = if let Some(prefix) = &prefix_opt {
                ctx.resolve_prefix(prefix).unwrap_or_default()
            } else {
                String::new() // Unprefixed attributes have no namespace
            };
            // Verify node's namespace matches expected
            if nav.namespace_uri() != expected_ns {
                return false;
            }
            // If schema_set available, validate declaration exists and type
            if let Some(schema_set) = ctx.schema_set {
                // Get local name as NameId - if not found, declaration doesn't exist
                let Some(local_id) = ctx.names.get(&local_name) else {
                    return false;
                };
                // Get namespace as NameId
                let ns_id = if expected_ns.is_empty() {
                    None
                } else {
                    ctx.names.get(&expected_ns)
                };
                // Lookup attribute declaration - must exist for schema-attribute() to match
                let Some(attr_key) = schema_set.lookup_attribute(ns_id, local_id) else {
                    return false;
                };
                let Some(attr_data) = schema_set.arenas.attributes.get(attr_key) else {
                    return false;
                };
                // Check type derivation if declaration has resolved_type
                if let Some(expected_type) = attr_data.resolved_type {
                    let Some(actual_type) = nav.schema_type() else {
                        return false;
                    };
                    return schema_set.is_type_derived_from(
                        TypeKey::Simple(actual_type),
                        expected_type,
                        DerivationSet::empty(),
                    );
                }
                // Declaration found, no type constraint - match
                return true;
            }
            // No schema context - name and namespace already verified
            true
        }
    }
}

/// Check if a node matches an AST QName (from paths.rs).
fn ast_qname_matches<N: DomNavigator>(
    qname: &crate::xpath::ast::QName,
    nav: &N,
    ctx: &XPathContext<'_>,
) -> bool {
    // For AST QName, prefix is stored directly as a string
    // Local name must match
    if nav.local_name() != qname.local {
        return false;
    }

    // Resolve prefix to namespace URI
    if qname.prefix.is_empty() {
        // No prefix - match empty namespace
        nav.namespace_uri().is_empty()
    } else {
        // Resolve the prefix to namespace URI
        match ctx.resolve_prefix(&qname.prefix) {
            Some(ns_uri) => nav.namespace_uri() == ns_uri,
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::namespace::table::NameTable;
    use crate::types::XmlTypeCode;
    use crate::xpath::roxmltree::RoXmlNavigator;
    use crate::xpath::context::XPathContext;

    // Helper to create ElementDeclData for tests
    fn make_element_data(
        name: Option<crate::ids::NameId>,
        target_namespace: Option<crate::ids::NameId>,
    ) -> crate::arenas::ElementDeclData {
        crate::arenas::ElementDeclData {
            name,
            target_namespace,
            ref_name: None,
            type_ref: None,
            inline_type: None,
            substitution_group: Vec::new(),
            default_value: None,
            fixed_value: None,
            nillable: false,
            is_abstract: false,
            min_occurs: 1,
            max_occurs: Some(1),
            block: DerivationSet::empty(),
            final_derivation: DerivationSet::empty(),
            form: None,
            id: None,
            alternatives: Vec::new(),
            identity_constraints: Vec::new(),
            annotation: None,
            source: None,
            resolved_type: None,
            resolved_ref: None,
            resolved_substitution_groups: Vec::new(),
        }
    }

    // Helper to create AttributeDeclData for tests
    fn make_attribute_data(
        name: Option<crate::ids::NameId>,
        target_namespace: Option<crate::ids::NameId>,
    ) -> crate::arenas::AttributeDeclData {
        crate::arenas::AttributeDeclData {
            name,
            target_namespace,
            ref_name: None,
            type_ref: None,
            inline_type: None,
            default_value: None,
            fixed_value: None,
            use_kind: None,
            form: None,
            inheritable: false,
            id: None,
            annotation: None,
            source: None,
            resolved_type: None,
            resolved_ref: None,
        }
    }

    #[test]
    fn test_name_test_wildcard() {
        let doc = roxmltree::Document::parse("<root xmlns=\"urn:test\"/>").expect("parse xml");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child();
        let table = NameTable::new();
        let ctx = XPathContext::new(&table);
        let test = NameTest::Wildcard;

        assert!(matches_name_test(&test, &nav, &ctx));
    }

    #[test]
    fn test_name_test_local_wildcard() {
        let doc = roxmltree::Document::parse("<root xmlns=\"urn:test\"/>").expect("parse xml");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child();
        let table = NameTable::new();
        let ns_id = table.add("urn:test");
        let ctx = XPathContext::new(&table);
        // LocalWildcard takes namespace URI - matches any local name in that namespace
        let test = NameTest::LocalWildcard(ns_id);

        assert!(matches_name_test(&test, &nav, &ctx));
    }

    #[test]
    fn test_name_test_namespace_wildcard() {
        let doc = roxmltree::Document::parse("<root xmlns=\"urn:test\"/>").expect("parse xml");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child();
        let table = NameTable::new();
        let local_id = table.add("root");
        let ctx = XPathContext::new(&table);
        // NamespaceWildcard takes local name - matches any namespace with that local name
        let test = NameTest::NamespaceWildcard(local_id);

        assert!(matches_name_test(&test, &nav, &ctx));
    }

    #[test]
    fn test_name_test_qname() {
        let doc = roxmltree::Document::parse("<root xmlns=\"urn:test\"/>").expect("parse xml");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child();
        let table = NameTable::new();
        let ns_id = table.add("urn:test");
        let local_id = table.add("root");
        let ctx = XPathContext::new(&table);
        let qname = QualifiedName::new(Some(ns_id), local_id, None);
        let test = NameTest::QName(qname);

        assert!(matches_name_test(&test, &nav, &ctx));
    }

    #[test]
    fn test_sequence_type_element_name() {
        let doc = roxmltree::Document::parse("<root xmlns=\"urn:test\"/>").expect("parse xml");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child();
        let table = NameTable::new();
        let ns_id = table.add("urn:test");
        let local_id = table.add("root");
        let ctx = XPathContext::new(&table);
        let qname = QualifiedName::new(Some(ns_id), local_id, None);
        let name_test = NameTest::QName(qname);
        let seq = SequenceType::one(ItemType::Element(Some(name_test), None));

        assert!(matches_sequence_type(&seq, &nav, &ctx));
    }

    #[test]
    fn test_sequence_type_document_with_element() {
        let doc = roxmltree::Document::parse("<root xmlns=\"urn:test\"><child/></root>")
            .expect("parse xml");
        let nav = RoXmlNavigator::new(&doc);
        let table = NameTable::new();
        let ns_id = table.add("urn:test");
        let local_id = table.add("root");
        let ctx = XPathContext::new(&table);
        let qname = QualifiedName::new(Some(ns_id), local_id, None);
        let name_test = NameTest::QName(qname);
        let inner = ItemType::Element(Some(name_test), None);
        let seq = SequenceType::one(ItemType::Document(Some(Box::new(inner))));

        assert!(matches_sequence_type(&seq, &nav, &ctx));
    }

    #[test]
    fn test_sequence_type_processing_instruction_target() {
        let doc = roxmltree::Document::parse("<?target data?><root/>").expect("parse xml");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child();
        let table = NameTable::new();
        let ctx = XPathContext::new(&table);
        let seq = SequenceType::one(ItemType::ProcessingInstruction(Some("target".to_string())));

        assert!(matches_sequence_type(&seq, &nav, &ctx));
    }

    #[test]
    fn test_sequence_type_text_like() {
        let doc = roxmltree::Document::parse("<root>text</root>").expect("parse xml");
        let mut text_nav = RoXmlNavigator::new(&doc);
        text_nav.move_to_first_child();
        text_nav.move_to_first_child();

        let table = NameTable::new();
        let ctx = XPathContext::new(&table);
        let seq = SequenceType::one(ItemType::Text);

        assert!(matches_sequence_type(&seq, &text_nav, &ctx));
    }

    #[test]
    fn test_sequence_type_atomic_rejected_for_node() {
        let doc = roxmltree::Document::parse("<root/>").expect("parse xml");
        let nav = RoXmlNavigator::new(&doc);
        let table = NameTable::new();
        let ctx = XPathContext::new(&table);
        let seq = SequenceType::one(ItemType::AtomicType(XmlTypeCode::String));

        assert!(!matches_sequence_type(&seq, &nav, &ctx));
    }

    // ============================================================================
    // Schema-Aware Tests
    // ============================================================================

    #[test]
    fn test_schema_element_without_schema_context() {
        // Test that schema-element() falls back to name-only match without schema_set
        let doc = roxmltree::Document::parse("<root xmlns=\"urn:test\"/>").expect("parse xml");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child();

        let table = NameTable::new();
        let ns_id = table.add("urn:test");
        let local_id = table.add("root");
        let ctx = XPathContext::new(&table); // No schema_set

        let qname = QualifiedName::new(Some(ns_id), local_id, None);
        let seq = SequenceType::one(ItemType::SchemaElement(qname));

        // Should match based on name only (no schema context)
        assert!(matches_sequence_type(&seq, &nav, &ctx));
    }

    #[test]
    fn test_schema_element_name_mismatch() {
        // Test that schema-element() rejects non-matching names
        let doc = roxmltree::Document::parse("<other xmlns=\"urn:test\"/>").expect("parse xml");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child();

        let table = NameTable::new();
        let ns_id = table.add("urn:test");
        let local_id = table.add("root"); // Different from "other"
        let ctx = XPathContext::new(&table);

        let qname = QualifiedName::new(Some(ns_id), local_id, None);
        let seq = SequenceType::one(ItemType::SchemaElement(qname));

        // Should not match - different local name
        assert!(!matches_sequence_type(&seq, &nav, &ctx));
    }

    #[test]
    fn test_schema_element_namespace_mismatch() {
        // Test that schema-element() rejects non-matching namespaces
        let doc = roxmltree::Document::parse("<root xmlns=\"urn:other\"/>").expect("parse xml");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child();

        let table = NameTable::new();
        let ns_id = table.add("urn:test"); // Different from "urn:other"
        let local_id = table.add("root");
        let ctx = XPathContext::new(&table);

        let qname = QualifiedName::new(Some(ns_id), local_id, None);
        let seq = SequenceType::one(ItemType::SchemaElement(qname));

        // Should not match - different namespace
        assert!(!matches_sequence_type(&seq, &nav, &ctx));
    }

    #[test]
    fn test_schema_element_rejects_attribute() {
        // Test that schema-element() rejects attribute nodes
        let doc = roxmltree::Document::parse("<root attr=\"value\"/>").expect("parse xml");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child();
        nav.move_to_first_attribute();

        let table = NameTable::new();
        let local_id = table.add("attr");
        let ctx = XPathContext::new(&table);

        let qname = QualifiedName::new(None, local_id, None);
        let seq = SequenceType::one(ItemType::SchemaElement(qname));

        // Should not match - it's an attribute, not an element
        assert!(!matches_sequence_type(&seq, &nav, &ctx));
    }

    #[test]
    fn test_schema_attribute_without_schema_context() {
        // Test that schema-attribute() falls back to name-only match without schema_set
        let doc = roxmltree::Document::parse("<root attr=\"value\"/>").expect("parse xml");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child();
        nav.move_to_first_attribute();

        let table = NameTable::new();
        let local_id = table.add("attr");
        let ctx = XPathContext::new(&table); // No schema_set

        let qname = QualifiedName::new(None, local_id, None);
        let seq = SequenceType::one(ItemType::SchemaAttribute(qname));

        // Should match based on name only (no schema context)
        assert!(matches_sequence_type(&seq, &nav, &ctx));
    }

    #[test]
    fn test_schema_attribute_name_mismatch() {
        // Test that schema-attribute() rejects non-matching names
        let doc = roxmltree::Document::parse("<root other=\"value\"/>").expect("parse xml");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child();
        nav.move_to_first_attribute();

        let table = NameTable::new();
        let local_id = table.add("attr"); // Different from "other"
        let ctx = XPathContext::new(&table);

        let qname = QualifiedName::new(None, local_id, None);
        let seq = SequenceType::one(ItemType::SchemaAttribute(qname));

        // Should not match - different local name
        assert!(!matches_sequence_type(&seq, &nav, &ctx));
    }

    #[test]
    fn test_schema_attribute_rejects_element() {
        // Test that schema-attribute() rejects element nodes
        let doc = roxmltree::Document::parse("<root/>").expect("parse xml");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child();

        let table = NameTable::new();
        let local_id = table.add("root");
        let ctx = XPathContext::new(&table);

        let qname = QualifiedName::new(None, local_id, None);
        let seq = SequenceType::one(ItemType::SchemaAttribute(qname));

        // Should not match - it's an element, not an attribute
        assert!(!matches_sequence_type(&seq, &nav, &ctx));
    }

    // ============================================================================
    // KindTest Schema-Aware Tests
    // ============================================================================

    #[test]
    fn test_kind_test_schema_element_without_schema() {
        // Test KindTest::SchemaElement falls back to name-only match
        let doc = roxmltree::Document::parse("<root/>").expect("parse xml");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child();

        let table = NameTable::new();
        let ctx = XPathContext::new(&table);

        let kind_test = KindTest::SchemaElement("root".to_string());
        assert!(matches_kind_test(&nav, &kind_test, &ctx));
    }

    #[test]
    fn test_kind_test_schema_element_name_mismatch() {
        // Test KindTest::SchemaElement rejects non-matching names
        let doc = roxmltree::Document::parse("<other/>").expect("parse xml");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child();

        let table = NameTable::new();
        let ctx = XPathContext::new(&table);

        let kind_test = KindTest::SchemaElement("root".to_string());
        assert!(!matches_kind_test(&nav, &kind_test, &ctx));
    }

    #[test]
    fn test_kind_test_schema_element_rejects_attribute() {
        // Test KindTest::SchemaElement rejects attribute nodes
        let doc = roxmltree::Document::parse("<root attr=\"value\"/>").expect("parse xml");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child();
        nav.move_to_first_attribute();

        let table = NameTable::new();
        let ctx = XPathContext::new(&table);

        let kind_test = KindTest::SchemaElement("attr".to_string());
        assert!(!matches_kind_test(&nav, &kind_test, &ctx));
    }

    #[test]
    fn test_kind_test_schema_attribute_without_schema() {
        // Test KindTest::SchemaAttribute falls back to name-only match
        let doc = roxmltree::Document::parse("<root attr=\"value\"/>").expect("parse xml");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child();
        nav.move_to_first_attribute();

        let table = NameTable::new();
        let ctx = XPathContext::new(&table);

        let kind_test = KindTest::SchemaAttribute("attr".to_string());
        assert!(matches_kind_test(&nav, &kind_test, &ctx));
    }

    #[test]
    fn test_kind_test_schema_attribute_name_mismatch() {
        // Test KindTest::SchemaAttribute rejects non-matching names
        let doc = roxmltree::Document::parse("<root other=\"value\"/>").expect("parse xml");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child();
        nav.move_to_first_attribute();

        let table = NameTable::new();
        let ctx = XPathContext::new(&table);

        let kind_test = KindTest::SchemaAttribute("attr".to_string());
        assert!(!matches_kind_test(&nav, &kind_test, &ctx));
    }

    #[test]
    fn test_kind_test_schema_attribute_rejects_element() {
        // Test KindTest::SchemaAttribute rejects element nodes
        let doc = roxmltree::Document::parse("<root/>").expect("parse xml");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child();

        let table = NameTable::new();
        let ctx = XPathContext::new(&table);

        let kind_test = KindTest::SchemaAttribute("root".to_string());
        assert!(!matches_kind_test(&nav, &kind_test, &ctx));
    }

    // ============================================================================
    // Schema-Aware Tests with SchemaSet
    // ============================================================================

    #[test]
    fn test_schema_element_with_schema_context_declaration_found() {
        use crate::schema::model::SchemaSet;

        // Create a SchemaSet with an element declaration
        let mut schema_set = SchemaSet::new();
        let ns_uri = schema_set.name_table.add("urn:test");
        let elem_name = schema_set.name_table.add("root");

        // Register an element in the schema
        schema_set.get_or_create_namespace(Some(ns_uri));
        let elem_data = make_element_data(Some(elem_name), Some(ns_uri));
        // No resolved_type - so type check will be skipped
        let elem_key = schema_set.arenas.alloc_element(elem_data);
        schema_set
            .namespaces
            .get_mut(&Some(ns_uri))
            .unwrap()
            .elements
            .insert(elem_name, elem_key);

        // Now test
        let doc = roxmltree::Document::parse("<root xmlns=\"urn:test\"/>").expect("parse xml");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child();

        // Build context with schema_set reference
        let ctx = XPathContext::new(&schema_set.name_table).with_schema_set(&schema_set);

        let qname = QualifiedName::new(Some(ns_uri), elem_name, None);
        let seq = SequenceType::one(ItemType::SchemaElement(qname));

        // Should match - declaration found but no type constraint
        assert!(matches_sequence_type(&seq, &nav, &ctx));
    }

    #[test]
    fn test_schema_element_with_schema_context_type_requires_annotation() {
        use crate::schema::model::SchemaSet;

        // Create a SchemaSet with an element declaration that has a resolved_type
        let mut schema_set = SchemaSet::new();
        let ns_uri = schema_set.name_table.add("urn:test");
        let elem_name = schema_set.name_table.add("root");
        let string_type = schema_set.builtin_types().string;

        // Register an element with a resolved type
        schema_set.get_or_create_namespace(Some(ns_uri));
        let mut elem_data = make_element_data(Some(elem_name), Some(ns_uri));
        // Set a resolved type - this will require type annotation on node
        elem_data.resolved_type = Some(TypeKey::Simple(string_type));
        let elem_key = schema_set.arenas.alloc_element(elem_data);
        schema_set
            .namespaces
            .get_mut(&Some(ns_uri))
            .unwrap()
            .elements
            .insert(elem_name, elem_key);

        // Now test
        let doc = roxmltree::Document::parse("<root xmlns=\"urn:test\"/>").expect("parse xml");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child();

        // Build context with schema_set reference
        let ctx = XPathContext::new(&schema_set.name_table).with_schema_set(&schema_set);

        let qname = QualifiedName::new(Some(ns_uri), elem_name, None);
        let seq = SequenceType::one(ItemType::SchemaElement(qname));

        // Should NOT match - declaration has type but roxmltree navigator has no schema_type
        assert!(!matches_sequence_type(&seq, &nav, &ctx));
    }

    #[test]
    fn test_schema_attribute_with_schema_context_declaration_found() {
        use crate::schema::model::SchemaSet;

        // Create a SchemaSet with an attribute declaration
        let mut schema_set = SchemaSet::new();
        let attr_name = schema_set.name_table.add("attr");

        // Register an attribute in the no-namespace schema
        schema_set.get_or_create_namespace(None);
        let attr_data = make_attribute_data(Some(attr_name), None);
        // No resolved_type - so type check will be skipped
        let attr_key = schema_set.arenas.alloc_attribute(attr_data);
        schema_set
            .namespaces
            .get_mut(&None)
            .unwrap()
            .attributes
            .insert(attr_name, attr_key);

        // Now test
        let doc = roxmltree::Document::parse("<root attr=\"value\"/>").expect("parse xml");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child();
        nav.move_to_first_attribute();

        // Build context with schema_set reference
        let ctx = XPathContext::new(&schema_set.name_table).with_schema_set(&schema_set);

        let qname = QualifiedName::new(None, attr_name, None);
        let seq = SequenceType::one(ItemType::SchemaAttribute(qname));

        // Should match - declaration found but no type constraint
        assert!(matches_sequence_type(&seq, &nav, &ctx));
    }

    #[test]
    fn test_schema_attribute_with_schema_context_type_requires_annotation() {
        use crate::schema::model::SchemaSet;

        // Create a SchemaSet with an attribute declaration that has a resolved_type
        let mut schema_set = SchemaSet::new();
        let attr_name = schema_set.name_table.add("attr");
        let string_type = schema_set.builtin_types().string;

        // Register an attribute with a resolved type
        schema_set.get_or_create_namespace(None);
        let mut attr_data = make_attribute_data(Some(attr_name), None);
        // Set a resolved type - this will require type annotation on node
        attr_data.resolved_type = Some(TypeKey::Simple(string_type));
        let attr_key = schema_set.arenas.alloc_attribute(attr_data);
        schema_set
            .namespaces
            .get_mut(&None)
            .unwrap()
            .attributes
            .insert(attr_name, attr_key);

        // Now test
        let doc = roxmltree::Document::parse("<root attr=\"value\"/>").expect("parse xml");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child();
        nav.move_to_first_attribute();

        // Build context with schema_set reference
        let ctx = XPathContext::new(&schema_set.name_table).with_schema_set(&schema_set);

        let qname = QualifiedName::new(None, attr_name, None);
        let seq = SequenceType::one(ItemType::SchemaAttribute(qname));

        // Should NOT match - declaration has type but roxmltree navigator has no schema_type
        assert!(!matches_sequence_type(&seq, &nav, &ctx));
    }

    #[test]
    fn test_schema_element_declaration_not_found_rejects() {
        use crate::schema::model::SchemaSet;

        // Create an empty SchemaSet (no declarations)
        let schema_set = SchemaSet::new();
        let ns_uri = schema_set.name_table.add("urn:test");
        let elem_name = schema_set.name_table.add("root");

        // Now test
        let doc = roxmltree::Document::parse("<root xmlns=\"urn:test\"/>").expect("parse xml");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child();

        // Build context with schema_set reference
        let ctx = XPathContext::new(&schema_set.name_table).with_schema_set(&schema_set);

        let qname = QualifiedName::new(Some(ns_uri), elem_name, None);
        let seq = SequenceType::one(ItemType::SchemaElement(qname));

        // Should NOT match - declaration not found in schema context
        assert!(!matches_sequence_type(&seq, &nav, &ctx));
    }

    // ============================================================================
    // Schema-Aware Type Derivation Tests (using RoXmlNavigator.with_schema_type)
    // ============================================================================

    #[test]
    fn test_schema_element_type_derivation_match() {
        use crate::schema::model::SchemaSet;

        // Create a SchemaSet with an element declaration that has a resolved_type
        let mut schema_set = SchemaSet::new();
        let ns_uri = schema_set.name_table.add("urn:test");
        let elem_name = schema_set.name_table.add("root");
        let string_type = schema_set.builtin_types().string;

        // Register an element with xs:string type
        schema_set.get_or_create_namespace(Some(ns_uri));
        let mut elem_data = make_element_data(Some(elem_name), Some(ns_uri));
        elem_data.resolved_type = Some(TypeKey::Simple(string_type));
        let elem_key = schema_set.arenas.alloc_element(elem_data);
        schema_set
            .namespaces
            .get_mut(&Some(ns_uri))
            .unwrap()
            .elements
            .insert(elem_name, elem_key);

        // Create a navigator with the same schema type (exact match)
        let doc = roxmltree::Document::parse("<root xmlns=\"urn:test\"/>").expect("parse xml");
        let nav = {
            let mut nav = RoXmlNavigator::new(&doc);
            nav.move_to_first_child();
            nav.with_schema_type(string_type)
        };

        // Build context with schema_set reference
        let ctx = XPathContext::new(&schema_set.name_table).with_schema_set(&schema_set);

        let qname = QualifiedName::new(Some(ns_uri), elem_name, None);
        let seq = SequenceType::one(ItemType::SchemaElement(qname));

        // Should match - type derives from (is same as) declaration type
        assert!(matches_sequence_type(&seq, &nav, &ctx));
    }

    #[test]
    fn test_schema_element_type_derivation_derived_type() {
        use crate::schema::model::SchemaSet;

        // Create a SchemaSet with an element declaration
        let mut schema_set = SchemaSet::new();
        let ns_uri = schema_set.name_table.add("urn:test");
        let elem_name = schema_set.name_table.add("root");
        // normalizedString derives from string
        let string_type = schema_set.builtin_types().string;
        let normalized_string_type = schema_set.builtin_types().normalized_string;

        // Register an element with xs:string type (base type)
        schema_set.get_or_create_namespace(Some(ns_uri));
        let mut elem_data = make_element_data(Some(elem_name), Some(ns_uri));
        elem_data.resolved_type = Some(TypeKey::Simple(string_type));
        let elem_key = schema_set.arenas.alloc_element(elem_data);
        schema_set
            .namespaces
            .get_mut(&Some(ns_uri))
            .unwrap()
            .elements
            .insert(elem_name, elem_key);

        // Create a navigator with a derived type (normalizedString derives from string)
        let doc = roxmltree::Document::parse("<root xmlns=\"urn:test\"/>").expect("parse xml");
        let nav = {
            let mut nav = RoXmlNavigator::new(&doc);
            nav.move_to_first_child();
            nav.with_schema_type(normalized_string_type)
        };

        // Build context with schema_set reference
        let ctx = XPathContext::new(&schema_set.name_table).with_schema_set(&schema_set);

        let qname = QualifiedName::new(Some(ns_uri), elem_name, None);
        let seq = SequenceType::one(ItemType::SchemaElement(qname));

        // Should match - normalizedString derives from string
        assert!(matches_sequence_type(&seq, &nav, &ctx));
    }

    #[test]
    fn test_schema_element_type_derivation_mismatch() {
        use crate::schema::model::SchemaSet;

        // Create a SchemaSet with an element declaration
        let mut schema_set = SchemaSet::new();
        let ns_uri = schema_set.name_table.add("urn:test");
        let elem_name = schema_set.name_table.add("root");
        // integer does NOT derive from string
        let string_type = schema_set.builtin_types().string;
        let integer_type = schema_set.builtin_types().integer;

        // Register an element with xs:string type
        schema_set.get_or_create_namespace(Some(ns_uri));
        let mut elem_data = make_element_data(Some(elem_name), Some(ns_uri));
        elem_data.resolved_type = Some(TypeKey::Simple(string_type));
        let elem_key = schema_set.arenas.alloc_element(elem_data);
        schema_set
            .namespaces
            .get_mut(&Some(ns_uri))
            .unwrap()
            .elements
            .insert(elem_name, elem_key);

        // Create a navigator with integer type (NOT derived from string)
        let doc = roxmltree::Document::parse("<root xmlns=\"urn:test\"/>").expect("parse xml");
        let nav = {
            let mut nav = RoXmlNavigator::new(&doc);
            nav.move_to_first_child();
            nav.with_schema_type(integer_type)
        };

        // Build context with schema_set reference
        let ctx = XPathContext::new(&schema_set.name_table).with_schema_set(&schema_set);

        let qname = QualifiedName::new(Some(ns_uri), elem_name, None);
        let seq = SequenceType::one(ItemType::SchemaElement(qname));

        // Should NOT match - integer does not derive from string
        assert!(!matches_sequence_type(&seq, &nav, &ctx));
    }

    #[test]
    fn test_schema_attribute_type_derivation_match() {
        use crate::schema::model::SchemaSet;

        // Create a SchemaSet with an attribute declaration
        let mut schema_set = SchemaSet::new();
        let attr_name = schema_set.name_table.add("attr");
        let string_type = schema_set.builtin_types().string;

        // Register an attribute with xs:string type
        schema_set.get_or_create_namespace(None);
        let mut attr_data = make_attribute_data(Some(attr_name), None);
        attr_data.resolved_type = Some(TypeKey::Simple(string_type));
        let attr_key = schema_set.arenas.alloc_attribute(attr_data);
        schema_set
            .namespaces
            .get_mut(&None)
            .unwrap()
            .attributes
            .insert(attr_name, attr_key);

        // Create a navigator with matching schema type
        let doc = roxmltree::Document::parse("<root attr=\"value\"/>").expect("parse xml");
        let nav = {
            let mut nav = RoXmlNavigator::new(&doc);
            nav.move_to_first_child();
            nav.move_to_first_attribute();
            nav.with_schema_type(string_type)
        };

        // Build context with schema_set reference
        let ctx = XPathContext::new(&schema_set.name_table).with_schema_set(&schema_set);

        let qname = QualifiedName::new(None, attr_name, None);
        let seq = SequenceType::one(ItemType::SchemaAttribute(qname));

        // Should match - type matches declaration type
        assert!(matches_sequence_type(&seq, &nav, &ctx));
    }

    // ============================================================================
    // Prefixed QName Tests for KindTest::SchemaElement/SchemaAttribute
    // ============================================================================

    #[test]
    fn test_kind_test_schema_element_prefixed_qname() {
        use crate::namespace::context::NamespaceContextSnapshot;

        // Test KindTest::SchemaElement with prefixed QName (exercises prefix resolution path)
        let doc =
            roxmltree::Document::parse("<ns:root xmlns:ns=\"urn:test\"/>").expect("parse xml");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child();

        let table = NameTable::new();
        let prefix_id = table.add("ns");
        let ns_id = table.add("urn:test");

        // Create namespace context with prefix binding
        let ns_ctx = NamespaceContextSnapshot {
            default_ns: None,
            bindings: vec![(prefix_id, ns_id)],
        };
        let ctx = XPathContext::new(&table).with_namespaces(ns_ctx);

        // Use prefixed QName "ns:root" - this exercises the prefix resolution path
        let kind_test = KindTest::SchemaElement("ns:root".to_string());
        assert!(matches_kind_test(&nav, &kind_test, &ctx));
    }

    #[test]
    fn test_kind_test_schema_element_prefixed_qname_wrong_ns() {
        use crate::namespace::context::NamespaceContextSnapshot;

        // Test that prefixed QName rejects elements in different namespace
        let doc =
            roxmltree::Document::parse("<ns:root xmlns:ns=\"urn:other\"/>").expect("parse xml");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child();

        let table = NameTable::new();
        let prefix_id = table.add("ns");
        let ns_id = table.add("urn:test"); // Different from document's urn:other

        let ns_ctx = NamespaceContextSnapshot {
            default_ns: None,
            bindings: vec![(prefix_id, ns_id)],
        };
        let ctx = XPathContext::new(&table).with_namespaces(ns_ctx);

        let kind_test = KindTest::SchemaElement("ns:root".to_string());
        // Should not match - namespace mismatch
        assert!(!matches_kind_test(&nav, &kind_test, &ctx));
    }

    #[test]
    fn test_kind_test_schema_element_unresolved_prefix() {
        // Test that unresolved prefix returns false (no panic)
        let doc =
            roxmltree::Document::parse("<ns:root xmlns:ns=\"urn:test\"/>").expect("parse xml");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child();

        let table = NameTable::new();
        // No prefix bindings - "unknown" prefix won't resolve
        let ctx = XPathContext::new(&table);

        let kind_test = KindTest::SchemaElement("unknown:root".to_string());
        // Should not match - prefix cannot be resolved
        assert!(!matches_kind_test(&nav, &kind_test, &ctx));
    }

    #[test]
    fn test_kind_test_schema_attribute_prefixed_qname() {
        use crate::namespace::context::NamespaceContextSnapshot;

        // Test KindTest::SchemaAttribute with prefixed QName (exercises prefix resolution path)
        let doc = roxmltree::Document::parse("<root xmlns:ns=\"urn:test\" ns:attr=\"value\"/>")
            .expect("parse xml");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child();
        // Find the namespaced attribute (skip xmlns:ns)
        nav.move_to_first_attribute();
        while nav.local_name() != "attr" {
            if !nav.move_to_next_attribute() {
                panic!("ns:attr attribute not found");
            }
        }

        let table = NameTable::new();
        let prefix_id = table.add("ns");
        let ns_id = table.add("urn:test");

        // Create namespace context with prefix binding
        let ns_ctx = NamespaceContextSnapshot {
            default_ns: None,
            bindings: vec![(prefix_id, ns_id)],
        };
        let ctx = XPathContext::new(&table).with_namespaces(ns_ctx);

        // Use prefixed QName "ns:attr" - this exercises the prefix resolution path
        let kind_test = KindTest::SchemaAttribute("ns:attr".to_string());
        assert!(matches_kind_test(&nav, &kind_test, &ctx));
    }

    #[test]
    fn test_kind_test_schema_attribute_prefixed_qname_wrong_ns() {
        use crate::namespace::context::NamespaceContextSnapshot;

        // Test that prefixed QName rejects attributes in different namespace
        let doc = roxmltree::Document::parse("<root xmlns:ns=\"urn:other\" ns:attr=\"value\"/>")
            .expect("parse xml");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child();
        nav.move_to_first_attribute();
        while nav.local_name() != "attr" {
            if !nav.move_to_next_attribute() {
                panic!("ns:attr attribute not found");
            }
        }

        let table = NameTable::new();
        let prefix_id = table.add("ns");
        let ns_id = table.add("urn:test"); // Different from document's urn:other

        let ns_ctx = NamespaceContextSnapshot {
            default_ns: None,
            bindings: vec![(prefix_id, ns_id)],
        };
        let ctx = XPathContext::new(&table).with_namespaces(ns_ctx);

        let kind_test = KindTest::SchemaAttribute("ns:attr".to_string());
        // Should not match - namespace mismatch
        assert!(!matches_kind_test(&nav, &kind_test, &ctx));
    }

    #[test]
    fn test_kind_test_schema_attribute_unresolved_prefix() {
        // Test that unresolved prefix returns false (no panic)
        let doc = roxmltree::Document::parse("<root xmlns:ns=\"urn:test\" ns:attr=\"value\"/>")
            .expect("parse xml");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child();
        nav.move_to_first_attribute();
        while nav.local_name() != "attr" {
            if !nav.move_to_next_attribute() {
                panic!("ns:attr attribute not found");
            }
        }

        let table = NameTable::new();
        // No prefix bindings - "unknown" prefix won't resolve
        let ctx = XPathContext::new(&table);

        let kind_test = KindTest::SchemaAttribute("unknown:attr".to_string());
        // Should not match - prefix cannot be resolved
        assert!(!matches_kind_test(&nav, &kind_test, &ctx));
    }
}
