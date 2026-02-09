//! XPath tree comparison helpers.
//!
//! Port of `xpath2/XPath20Api/XPath20Api/TreeComparer.cs`.
//! Aligns with `DOM_NAVIGATOR_DESIGN.md` and `XML_NODE_ITERATOR_DESIGN.md`.

use crate::types::{normalize_whitespace, WhitespaceMode, XmlAtomicValue, XmlValue, XmlValueKind};
use crate::types::XmlTypeCode;

use super::ast::BinaryOpKind;
use super::error::XPathError;
use super::iterator::{XmlItemRef, XmlNodeIterator};
use super::operators::eval_binary;
use super::{DomNavigator, DomNodeType};

/// Compares XPath nodes and sequences for deep equality.
#[derive(Debug, Clone)]
#[derive(Default)]
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
            DomNodeType::Text
            | DomNodeType::Whitespace
            | DomNodeType::SignificantWhitespace
            | DomNodeType::Comment => self.text_equal(&left.value(), &right.value()),
            DomNodeType::ProcessingInstruction => self.processing_instruction_equal(left, right),
            _ => self.deep_equal(left, right),
        }
    }

    fn element_equal<N: DomNavigator>(&self, left: &N, right: &N) -> bool {
        if left.local_name() != right.local_name() || left.namespace_uri() != right.namespace_uri() {
            return false;
        }

        let mut left_nav = left.clone();
        let mut right_nav = right.clone();

        self.element_attributes_equal(&mut left_nav, &mut right_nav)
            && self.deep_equal(left, right)
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

    fn attribute_equal<N: DomNavigator>(&self, left: &N, right: &N) -> bool {
        if left.local_name() != right.local_name() || left.namespace_uri() != right.namespace_uri() {
            return false;
        }

        let left_value = left.atomized_value();
        let right_value = right.atomized_value();
        self.values_equal_or_nan(&left_value, &right_value)
    }

    /// Deep equality for two navigator positions (node comparison).
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

fn is_xml_whitespace(ch: char) -> bool {
    matches!(ch, ' ' | '\t' | '\n' | '\r')
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

    use crate::xpath::iterator::{VecNodeIterator, XmlItem};
    use crate::navigator::RoXmlNavigator;

    #[test]
    fn test_deep_equal_ignores_whitespace_nodes() {
        let comparer = TreeComparer::with_ignore_whitespace(true);
        let doc1 = roxmltree::Document::parse("<root>\n  <a>1</a>\n  <b>2</b>\n</root>")
            .expect("parse xml");
        let doc2 = roxmltree::Document::parse("<root><a>1</a><b>2</b></root>")
            .expect("parse xml");
        let nav1 = RoXmlNavigator::new(&doc1);
        let nav2 = RoXmlNavigator::new(&doc2);

        assert!(comparer.deep_equal(&nav1, &nav2));
    }

    #[test]
    fn test_deep_equal_detects_whitespace_when_enabled() {
        let comparer = TreeComparer::new();
        let doc1 = roxmltree::Document::parse("<root>\n  <a>1</a>\n</root>")
            .expect("parse xml");
        let doc2 = roxmltree::Document::parse("<root><a>1</a></root>")
            .expect("parse xml");
        let nav1 = RoXmlNavigator::new(&doc1);
        let nav2 = RoXmlNavigator::new(&doc2);

        assert!(!comparer.deep_equal(&nav1, &nav2));
    }

    #[test]
    fn test_deep_equal_attributes_order_insensitive() {
        let comparer = TreeComparer::new();
        let doc1 = roxmltree::Document::parse("<root b=\"2\" a=\"1\"/>")
            .expect("parse xml");
        let doc2 = roxmltree::Document::parse("<root a=\"1\" b=\"2\"/>")
            .expect("parse xml");
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
}
