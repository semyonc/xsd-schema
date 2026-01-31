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
            nav.node_type() == DomNodeType::Element && qname_matches(name, nav, ctx)
        }
        ItemType::SchemaAttribute(name) => {
            nav.node_type() == DomNodeType::Attribute && qname_matches(name, nav, ctx)
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
            nav.local_name() == *name
        }
        KindTest::SchemaAttribute(name) => {
            if nav.node_type() != DomNodeType::Attribute {
                return false;
            }
            nav.local_name() == *name
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
}
