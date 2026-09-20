//! XPath tree comparison helpers.
//!
//! Port of `xpath2/XPath20Api/XPath20Api/TreeComparer.cs`.
//! Aligns with `DOM_NAVIGATOR_DESIGN.md` and `XML_NODE_ITERATOR_DESIGN.md`.

use crate::types::XmlTypeCode;
use crate::types::{normalize_whitespace, WhitespaceMode, XmlAtomicValue, XmlValue, XmlValueKind};

use super::ast::BinaryOpKind;
use super::error::XPathError;
use super::iterator::{XmlItemRef, XmlNodeIterator};
use super::operators::eval_binary;
use super::string_ops::is_xml_whitespace;
use super::{DomNavigator, DomNodeType};

/// Compares XPath nodes and sequences for deep equality.
#[derive(Debug, Clone, Default)]
pub struct TreeComparer {
    pub ignore_whitespace: bool,
}

impl TreeComparer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_ignore_whitespace(ignore_whitespace: bool) -> Self {
        Self { ignore_whitespace }
    }

    fn text_equal(&self, left: &str, right: &str) -> bool {
        if self.ignore_whitespace {
            normalize_whitespace(left, WhitespaceMode::Collapse)
                == normalize_whitespace(right, WhitespaceMode::Collapse)
        } else {
            left == right
        }
    }

    fn normalized_item_value(&self, value: &XmlValue) -> XmlValue {
        match value.type_code {
            XmlTypeCode::UntypedAtomic | XmlTypeCode::AnyUri => {
                XmlValue::string(value.to_string_value())
            }
            _ => value.clone(),
        }
    }

    fn is_nan_value(&self, value: &XmlValue) -> bool {
        match &value.value {
            XmlValueKind::Atomic(XmlAtomicValue::Float(f)) => f.is_nan(),
            XmlValueKind::Atomic(XmlAtomicValue::Double(d)) => d.is_nan(),
            _ => false,
        }
    }

    fn values_equal_or_nan(&self, left: &XmlValue, right: &XmlValue) -> bool {
        if self.is_nan_value(left) && self.is_nan_value(right) {
            return true;
        }
        left == right
    }

    fn item_equal(&self, left: &XmlValue, right: &XmlValue) -> bool {
        let left = self.normalized_item_value(left);
        let right = self.normalized_item_value(right);

        if self.values_equal_or_nan(&left, &right) {
            return true;
        }

        if let Ok(result) = eval_binary(BinaryOpKind::ValueEq, &left, &right) {
            return result.as_boolean().unwrap_or(false);
        }

        false
    }

    fn is_whitespace_node<N: DomNavigator>(&self, nav: &N) -> bool {
        if !self.ignore_whitespace || !nav.node_type().is_text_like() {
            return false;
        }
        nav.value().chars().all(is_xml_whitespace)
    }

    fn node_equal<N: DomNavigator>(&self, left: &N, right: &N) -> bool {
        if left.node_type() != right.node_type() {
            return false;
        }

        match left.node_type() {
            DomNodeType::Element => self.element_equal(left, right),
            DomNodeType::Attribute => self.attribute_equal(left, right),
            DomNodeType::Namespace => self.namespace_equal(left, right),
            DomNodeType::Text
            | DomNodeType::Whitespace
            | DomNodeType::SignificantWhitespace
            | DomNodeType::Comment => self.text_equal(&left.value(), &right.value()),
            DomNodeType::ProcessingInstruction => self.processing_instruction_equal(left, right),
            // Document nodes: their content is their children.
            _ => self.deep_equal(left, right),
        }
    }

    fn element_equal<N: DomNavigator>(&self, left: &N, right: &N) -> bool {
        if left.local_name() != right.local_name() || left.namespace_uri() != right.namespace_uri()
        {
            return false;
        }

        let mut left_nav = left.clone();
        let mut right_nav = right.clone();

        self.element_attributes_equal(&mut left_nav, &mut right_nav) && self.deep_equal(left, right)
    }

    fn element_attributes_equal<N: DomNavigator>(&self, left: &mut N, right: &mut N) -> bool {
        if left.has_attributes() != right.has_attributes() {
            return false;
        }

        if !left.has_attributes() {
            return true;
        }

        let left_count = count_attributes(left);
        let right_count = count_attributes(right);
        if left_count != right_count {
            return false;
        }

        if left.move_to_first_attribute() {
            loop {
                let mut found = false;
                if right.move_to_first_attribute() {
                    loop {
                        if self.attribute_equal(left, right) {
                            found = true;
                            break;
                        }
                        if !right.move_to_next_attribute() {
                            break;
                        }
                    }
                    right.move_to_parent();
                }

                if !found {
                    left.move_to_parent();
                    return false;
                }

                if !left.move_to_next_attribute() {
                    break;
                }
            }
            left.move_to_parent();
        }

        true
    }

    fn processing_instruction_equal<N: DomNavigator>(&self, left: &N, right: &N) -> bool {
        left.local_name() == right.local_name() && left.value() == right.value()
    }

    /// Deep equality for two namespace nodes.
    ///
    /// A namespace node has no children, so the generic child-sequence
    /// comparison would make every pair of them equal. Its content is its
    /// *name* — the prefix, an `xs:NCName`, absent (reported as the empty
    /// string by every [`DomNavigator`] backend) for a default-namespace
    /// binding — and its *string value*, the bound namespace URI. Both must
    /// match.
    ///
    /// Neither part is document text, so `ignore_whitespace` deliberately does
    /// not apply: a namespace URI that differs by whitespace is a different
    /// URI.
    fn namespace_equal<N: DomNavigator>(&self, left: &N, right: &N) -> bool {
        left.local_name() == right.local_name() && left.value_ref() == right.value_ref()
    }

    fn attribute_equal<N: DomNavigator>(&self, left: &N, right: &N) -> bool {
        if left.local_name() != right.local_name() || left.namespace_uri() != right.namespace_uri()
        {
            return false;
        }

        // Attributes cannot be Nilled/Absent; fall back to untyped on error.
        let left_value = crate::xpath::atomize::atomize_node(left)
            .ok()
            .flatten()
            .unwrap_or_else(|| XmlValue::untyped(left.value()));
        let right_value = crate::xpath::atomize::atomize_node(right)
            .ok()
            .flatten()
            .unwrap_or_else(|| XmlValue::untyped(right.value()));
        self.values_equal_or_nan(&left_value, &right_value)
    }

    /// Deep equality of the **children** of two navigator positions.
    ///
    /// This compares the two child sequences pairwise; it deliberately does
    /// *not* look at the two positions themselves, so their names, attributes
    /// and node kinds play no part. On two document nodes that is exactly
    /// `fn:deep-equal`, whose content is its children; on two elements it is
    /// a *content* comparison — `<a x="1">t</a>` and `<b y="2">t</b>` are
    /// reported equal here, while `fn:deep-equal` reports them different.
    ///
    /// To compare items the way `fn:deep-equal` does, including the node kind
    /// and name, use [`deep_equal_iter`](Self::deep_equal_iter) over the two
    /// sequences.
    pub fn deep_equal<N: DomNavigator>(&self, left: &N, right: &N) -> bool {
        let mut left_iter = ChildIter::new(left.clone());
        let mut right_iter = ChildIter::new(right.clone());

        loop {
            let left_child = self.next_significant_child(&mut left_iter);
            let right_child = self.next_significant_child(&mut right_iter);

            match (left_child, right_child) {
                (None, None) => return true,
                (Some(_), None) | (None, Some(_)) => return false,
                (Some(left_node), Some(right_node)) => {
                    if !self.node_equal(&left_node, &right_node) {
                        return false;
                    }
                }
            }
        }
    }

    /// Deep equality for two XPath item iterators.
    pub fn deep_equal_iter<I>(&self, left: &I, right: &I) -> Result<bool, XPathError>
    where
        I: XmlNodeIterator,
    {
        let mut left_iter = left.clone();
        let mut right_iter = right.clone();

        loop {
            let left_has = left_iter.move_next()?;
            let right_has = right_iter.move_next()?;
            if left_has != right_has {
                return Ok(false);
            }
            if !left_has {
                return Ok(true);
            }

            let left_item = left_iter.current();
            let right_item = right_iter.current();

            match (left_item, right_item) {
                (Some(XmlItemRef::Node(left_node)), Some(XmlItemRef::Node(right_node))) => {
                    if !self.node_equal(left_node, right_node) {
                        return Ok(false);
                    }
                }
                (Some(XmlItemRef::Atomic(left_value)), Some(XmlItemRef::Atomic(right_value))) => {
                    if !self.item_equal(left_value, right_value) {
                        return Ok(false);
                    }
                }
                _ => return Ok(false),
            }
        }
    }

    fn next_significant_child<N: DomNavigator>(&self, iter: &mut ChildIter<N>) -> Option<N> {
        let mut current = iter.next();
        while let Some(ref nav) = current {
            if !self.is_whitespace_node(nav) {
                return current;
            }
            current = iter.next();
        }
        None
    }
}

fn count_attributes<N: DomNavigator>(nav: &mut N) -> usize {
    let mut count = 0;
    if nav.move_to_first_attribute() {
        loop {
            count += 1;
            if !nav.move_to_next_attribute() {
                break;
            }
        }
        nav.move_to_parent();
    }
    count
}

#[derive(Clone)]
struct ChildIter<N: DomNavigator> {
    nav: N,
    started: bool,
    done: bool,
}

impl<N: DomNavigator> ChildIter<N> {
    fn new(nav: N) -> Self {
        Self {
            nav,
            started: false,
            done: false,
        }
    }

    fn next(&mut self) -> Option<N> {
        if self.done {
            return None;
        }

        if !self.started {
            self.started = true;
            if !self.nav.move_to_first_child() {
                self.done = true;
                return None;
            }
            return Some(self.nav.clone());
        }

        if self.nav.move_to_next_sibling() {
            Some(self.nav.clone())
        } else {
            self.done = true;
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use num_bigint::BigInt;
    use rust_decimal::Decimal;

    use crate::navigator::{NamespaceAxisScope, RoXmlNavigator};
    use crate::xpath::iterator::{VecNodeIterator, XmlItem};

    /// The document element of `doc`.
    fn ro_element<'d>(doc: &'d roxmltree::Document<'d>) -> RoXmlNavigator<'d> {
        let mut nav = RoXmlNavigator::new(doc);
        assert!(nav.move_to_first_child(), "a document element");
        nav
    }

    /// The namespace node with prefix `prefix` (`""` = the default binding) on
    /// the document element of `doc`.
    fn ro_namespace<'d>(doc: &'d roxmltree::Document<'d>, prefix: &str) -> RoXmlNavigator<'d> {
        let mut nav = ro_element(doc);
        assert!(
            nav.move_to_first_namespace(NamespaceAxisScope::Local),
            "a locally declared namespace",
        );
        loop {
            if nav.local_name() == prefix {
                return nav;
            }
            assert!(
                nav.move_to_next_namespace(NamespaceAxisScope::Local),
                "a namespace node with prefix {prefix:?}",
            );
        }
    }

    /// `fn:deep-equal` over two one-item node sequences, through the same
    /// entry point the function itself uses.
    fn nodes_deep_equal<N: DomNavigator>(left: N, right: N) -> bool {
        let left: VecNodeIterator<N> = VecNodeIterator::new(vec![XmlItem::Node(left)]);
        let right: VecNodeIterator<N> = VecNodeIterator::new(vec![XmlItem::Node(right)]);
        TreeComparer::new()
            .deep_equal_iter(&left, &right)
            .expect("comparing two node sequences does not raise")
    }

    #[test]
    fn test_deep_equal_ignores_whitespace_nodes() {
        let comparer = TreeComparer::with_ignore_whitespace(true);
        let doc1 = roxmltree::Document::parse("<root>\n  <a>1</a>\n  <b>2</b>\n</root>")
            .expect("parse xml");
        let doc2 = roxmltree::Document::parse("<root><a>1</a><b>2</b></root>").expect("parse xml");
        let nav1 = RoXmlNavigator::new(&doc1);
        let nav2 = RoXmlNavigator::new(&doc2);

        assert!(comparer.deep_equal(&nav1, &nav2));
    }

    #[test]
    fn test_deep_equal_detects_whitespace_when_enabled() {
        let comparer = TreeComparer::new();
        let doc1 = roxmltree::Document::parse("<root>\n  <a>1</a>\n</root>").expect("parse xml");
        let doc2 = roxmltree::Document::parse("<root><a>1</a></root>").expect("parse xml");
        let nav1 = RoXmlNavigator::new(&doc1);
        let nav2 = RoXmlNavigator::new(&doc2);

        assert!(!comparer.deep_equal(&nav1, &nav2));
    }

    #[test]
    fn test_deep_equal_attributes_order_insensitive() {
        let comparer = TreeComparer::new();
        let doc1 = roxmltree::Document::parse("<root b=\"2\" a=\"1\"/>").expect("parse xml");
        let doc2 = roxmltree::Document::parse("<root a=\"1\" b=\"2\"/>").expect("parse xml");
        let nav1 = RoXmlNavigator::new(&doc1);
        let nav2 = RoXmlNavigator::new(&doc2);

        assert!(comparer.deep_equal(&nav1, &nav2));
    }

    #[test]
    fn test_deep_equal_iter_uses_value_eq() {
        let comparer = TreeComparer::new();
        let left: VecNodeIterator<RoXmlNavigator<'static>> =
            VecNodeIterator::new(vec![XmlItem::Atomic(XmlValue::integer(BigInt::from(1)))]);
        let right: VecNodeIterator<RoXmlNavigator<'static>> =
            VecNodeIterator::new(vec![XmlItem::Atomic(XmlValue::decimal(Decimal::new(1, 0)))]);

        assert!(comparer.deep_equal_iter(&left, &right).unwrap());
    }

    #[test]
    fn test_deep_equal_iter_nan() {
        let comparer = TreeComparer::new();
        let left: VecNodeIterator<RoXmlNavigator<'static>> =
            VecNodeIterator::new(vec![XmlItem::Atomic(XmlValue::double(f64::NAN))]);
        let right: VecNodeIterator<RoXmlNavigator<'static>> =
            VecNodeIterator::new(vec![XmlItem::Atomic(XmlValue::float(f32::NAN))]);

        assert!(comparer.deep_equal_iter(&left, &right).unwrap());
    }

    /// Pins the contract of the public [`TreeComparer::deep_equal`]: it
    /// compares the *children* of the two positions, not the positions
    /// themselves. Two callers in this crate depend on that — a document-root
    /// comparison, where it coincides with `fn:deep-equal`, and a
    /// copied-subtree content check. This test characterises existing
    /// behaviour; it is not a defect test.
    #[test]
    fn deep_equal_compares_children_not_the_two_positions() {
        let left = roxmltree::Document::parse(r#"<a x="1">t</a>"#).expect("parse xml");
        let right = roxmltree::Document::parse(r#"<b y="2">t</b>"#).expect("parse xml");

        // Different name, different attributes — same children.
        assert!(TreeComparer::new().deep_equal(&ro_element(&left), &ro_element(&right)));
        // Compared as items, the same pair is not deep-equal.
        assert!(!nodes_deep_equal(ro_element(&left), ro_element(&right)));
    }

    // ── Namespace nodes ───────────────────────────────────────────────
    //
    // Two namespace nodes are deep-equal exactly when their names — the
    // prefix, absent for a default-namespace binding — are equal and their
    // string values, the bound namespace URI, are equal.

    #[test]
    fn namespace_nodes_with_the_same_prefix_and_uri_are_deep_equal() {
        let left = roxmltree::Document::parse(r#"<r xmlns:a="http://x/"/>"#).expect("parse xml");
        let right = roxmltree::Document::parse(r#"<r xmlns:a="http://x/"/>"#).expect("parse xml");

        assert!(nodes_deep_equal(
            ro_namespace(&left, "a"),
            ro_namespace(&right, "a"),
        ));
    }

    #[test]
    fn namespace_nodes_differing_only_in_prefix_are_not_deep_equal() {
        let doc = roxmltree::Document::parse(r#"<r xmlns:a="http://x/" xmlns:b="http://x/"/>"#)
            .expect("parse xml");

        assert!(!nodes_deep_equal(
            ro_namespace(&doc, "a"),
            ro_namespace(&doc, "b"),
        ));
    }

    #[test]
    fn namespace_nodes_differing_only_in_uri_are_not_deep_equal() {
        let left = roxmltree::Document::parse(r#"<r xmlns:a="http://x/"/>"#).expect("parse xml");
        let right = roxmltree::Document::parse(r#"<r xmlns:a="http://y/"/>"#).expect("parse xml");

        assert!(!nodes_deep_equal(
            ro_namespace(&left, "a"),
            ro_namespace(&right, "a"),
        ));
    }

    #[test]
    fn a_default_namespace_node_is_not_deep_equal_to_a_prefixed_one() {
        let doc = roxmltree::Document::parse(r#"<r xmlns="http://x/" xmlns:a="http://x/"/>"#)
            .expect("parse xml");

        assert!(!nodes_deep_equal(
            ro_namespace(&doc, ""),
            ro_namespace(&doc, "a"),
        ));
    }

    #[test]
    fn default_namespace_nodes_with_the_same_uri_are_deep_equal() {
        let left = roxmltree::Document::parse(r#"<r xmlns="http://x/"/>"#).expect("parse xml");
        let right = roxmltree::Document::parse(r#"<r xmlns="http://x/"/>"#).expect("parse xml");

        assert!(nodes_deep_equal(
            ro_namespace(&left, ""),
            ro_namespace(&right, ""),
        ));
    }

    #[test]
    fn default_namespace_nodes_with_different_uris_are_not_deep_equal() {
        let left = roxmltree::Document::parse(r#"<r xmlns="http://x/"/>"#).expect("parse xml");
        let right = roxmltree::Document::parse(r#"<r xmlns="http://y/"/>"#).expect("parse xml");

        assert!(!nodes_deep_equal(
            ro_namespace(&left, ""),
            ro_namespace(&right, ""),
        ));
    }

    #[test]
    fn a_namespace_node_is_not_deep_equal_to_another_kind_with_the_same_string_value() {
        let doc =
            roxmltree::Document::parse(r#"<r xmlns:a="http://x/" a="http://x/">http://x/</r>"#)
                .expect("parse xml");
        let namespace = ro_namespace(&doc, "a");

        let mut attribute = RoXmlNavigator::new(&doc);
        assert!(attribute.move_to_first_child(), "a document element");
        assert!(attribute.move_to_first_attribute(), "an attribute");

        let mut text = RoXmlNavigator::new(&doc);
        assert!(text.move_to_first_child(), "a document element");
        assert!(text.move_to_first_child(), "a text node");

        assert_eq!(namespace.value(), attribute.value());
        assert_eq!(namespace.value(), text.value());
        assert!(!nodes_deep_equal(namespace.clone(), attribute));
        assert!(!nodes_deep_equal(namespace, text));
    }

    #[test]
    fn a_sequence_of_namespace_nodes_is_compared_item_by_item() {
        let doc = roxmltree::Document::parse(r#"<r xmlns:a="http://x/" xmlns:b="http://y/"/>"#)
            .expect("parse xml");
        let comparer = TreeComparer::new();

        let pair = |first: &str, second: &str| -> VecNodeIterator<RoXmlNavigator<'_>> {
            VecNodeIterator::new(vec![
                XmlItem::Node(ro_namespace(&doc, first)),
                XmlItem::Node(ro_namespace(&doc, second)),
            ])
        };

        assert!(comparer
            .deep_equal_iter(&pair("a", "b"), &pair("a", "b"))
            .unwrap());
        assert!(!comparer
            .deep_equal_iter(&pair("a", "b"), &pair("b", "a"))
            .unwrap());
    }

    #[test]
    fn namespace_nodes_of_a_buffer_document_follow_the_same_rule() {
        use crate::document::BufferDocument;
        use crate::namespace::NameTable;
        use bumpalo::Bump;

        let arena = Bump::new();
        let names = NameTable::new();
        let parse = |xml: &'static str| {
            BufferDocument::from_reader_default(xml.as_bytes(), &arena, &names)
                .expect("the fixture parses")
        };

        // The navigator borrows the document, so keep every document alive.
        let docs = [
            parse(r#"<r xmlns:a="http://x/"/>"#),
            parse(r#"<r xmlns:a="http://x/"/>"#),
            parse(r#"<r xmlns:b="http://x/"/>"#),
            parse(r#"<r xmlns:a="http://y/"/>"#),
            parse(r#"<r xmlns="http://x/"/>"#),
            parse(r#"<r xmlns="http://x/"/>"#),
        ];

        let namespace = |index: usize| {
            let mut nav = docs[index].create_navigator();
            assert!(nav.move_to_first_child(), "a document element");
            assert!(
                nav.move_to_first_namespace(NamespaceAxisScope::Local),
                "a locally declared namespace",
            );
            nav
        };

        assert!(
            nodes_deep_equal(namespace(0), namespace(1)),
            "same prefix, same URI"
        );
        assert!(
            !nodes_deep_equal(namespace(0), namespace(2)),
            "different prefix"
        );
        assert!(
            !nodes_deep_equal(namespace(0), namespace(3)),
            "different URI"
        );
        assert!(
            !nodes_deep_equal(namespace(0), namespace(4)),
            "prefixed vs default"
        );
        assert!(
            nodes_deep_equal(namespace(4), namespace(5)),
            "default binding, same URI"
        );
    }
}
