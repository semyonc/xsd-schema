//! XPath node test matching helpers.
//!
//! Provides a unified node test type that can be used by axis iterators
//! and type-based filters, aligning with `XPATH_ITERATOR_PORT_PLAN.md`.

use crate::ids::TypeKey;
use crate::namespace::qname::QualifiedName;
use crate::schema::model::DerivationSet;
use crate::types::{ItemType, NameTest, SequenceType};

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
        NameTest::NamespaceWildcard(ns) => nav.namespace_uri() == ns,
        NameTest::LocalWildcard(local) => nav.local_name() == local,
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
                && target.as_ref().map_or(true, |name| nav.local_name() == name)
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
        let ctx = XPathContext::new(&table);
        let test = NameTest::LocalWildcard("root".to_string());

        assert!(matches_name_test(&test, &nav, &ctx));
    }

    #[test]
    fn test_name_test_namespace_wildcard() {
        let doc = roxmltree::Document::parse("<root xmlns=\"urn:test\"/>").expect("parse xml");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child();
        let table = NameTable::new();
        let ctx = XPathContext::new(&table);
        let test = NameTest::NamespaceWildcard("urn:test".to_string());

        assert!(matches_name_test(&test, &nav, &ctx));
    }

    #[test]
    fn test_name_test_qname() {
        let doc = roxmltree::Document::parse("<root xmlns=\"urn:test\"/>").expect("parse xml");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child();
        let mut table = NameTable::new();
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
        let mut table = NameTable::new();
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
        let mut table = NameTable::new();
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
