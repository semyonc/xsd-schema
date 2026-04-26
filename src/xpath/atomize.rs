//! Atomization operations for XPath evaluation.
//!
//! This module implements XPath 2.0 atomization rules for converting
//! values to their atomic representations.
//!
//! ## Atomization Rules
//!
//! Atomization extracts atomic values from items:
//!
//! - For atomic values, returns the value itself
//! - For nodes, returns the typed value of the node
//! - For empty sequences, returns None
//! - For sequences with more than one item, raises XPDY0050

use super::error::XPathError;
use super::functions::XPathValue;
use super::iterator::XmlItem;
use super::{DomNavigator, DomNodeType};
use crate::navigator::TypedValue;
use crate::types::value::{XmlAtomicValue, XmlValue, XmlValueKind};
use crate::types::XmlTypeCode;

/// Atomize a navigator node to its XDM atomic value.
///
/// Interprets [`TypedValue`] with proper error handling:
/// - `Value(v)` → `Ok(Some(v))`
/// - `Untyped` → `Ok(Some(untypedAtomic(string-value)))` (or `xs:string` for comment/PI)
/// - `Nilled` → `Ok(None)` (empty sequence)
/// - `Absent` → `Err(FOTY0012)`
pub fn atomize_node<N: DomNavigator>(nav: &N) -> Result<Option<XmlValue>, XPathError> {
    match nav.typed_value() {
        TypedValue::Value(v) => Ok(Some(v)),
        TypedValue::Untyped => {
            let v = match nav.node_type() {
                DomNodeType::Comment | DomNodeType::ProcessingInstruction => {
                    XmlValue::string(nav.value())
                }
                _ => XmlValue::untyped(nav.value()),
            };
            Ok(Some(v))
        }
        TypedValue::Nilled => Ok(None),
        TypedValue::Absent => Err(XPathError::no_typed_value()),
    }
}

/// Atomize an XmlValue, returning its atomic representation.
///
/// For atomic values, this returns a clone of the value.
/// For union values, this unwraps and atomizes the inner value.
/// For list values, this returns an error (multiple items).
///
/// # Arguments
///
/// * `value` - The value to atomize
///
/// # Returns
///
/// * `Ok(XmlValue)` - The atomized value
/// * `Err(XPathError)` - If atomization fails
pub fn atomize(value: &XmlValue) -> Result<XmlValue, XPathError> {
    match &value.value {
        // Atomic values return themselves
        XmlValueKind::Atomic(_) | XmlValueKind::UntypedAtomic(_) => Ok(value.clone()),

        // Union: unwrap and atomize
        XmlValueKind::Union(inner) => atomize(inner),

        // List values represent multiple items - error
        XmlValueKind::List { items, .. } if items.len() > 1 => {
            Err(XPathError::more_than_one_item())
        }

        // Single-item list: return the item
        XmlValueKind::List { items, item_type } if items.len() == 1 => Ok(XmlValue::new(
            *item_type,
            XmlValueKind::Atomic(items[0].clone()),
        )),

        // Empty list: conceptually empty sequence
        XmlValueKind::List { .. } => Err(XPathError::type_mismatch("item()", "empty-sequence()")),
    }
}

/// Atomize an optional value.
///
/// Returns None for None (empty sequence), otherwise atomizes the value.
///
/// # Arguments
///
/// * `value` - Optional value to atomize
///
/// # Returns
///
/// * `Ok(None)` - If input is None (empty sequence)
/// * `Ok(Some(XmlValue))` - The atomized value
/// * `Err(XPathError)` - If atomization fails
pub fn atomize_opt(value: Option<&XmlValue>) -> Result<Option<XmlValue>, XPathError> {
    match value {
        None => Ok(None),
        Some(v) => atomize(v).map(Some),
    }
}

/// Atomize a value, requiring a non-empty result.
///
/// This is equivalent to `Atomize<T>` in C# - it requires the result to exist.
///
/// # Arguments
///
/// * `value` - Optional value to atomize
///
/// # Returns
///
/// * `Ok(XmlValue)` - The atomized value
/// * `Err(XPathError)` - XPTY0004 if empty, or other atomization errors
pub fn atomize_required(value: Option<&XmlValue>) -> Result<XmlValue, XPathError> {
    match value {
        None => Err(XPathError::type_mismatch("item()", "empty-sequence()")),
        Some(v) => atomize(v),
    }
}

/// Get the string value of an XmlValue.
///
/// For atomic values, this returns the canonical string representation.
/// For union values, this unwraps and gets the string value.
/// For list values, this joins the item strings with spaces.
///
/// # Arguments
///
/// * `value` - The value to convert to string
///
/// # Returns
///
/// The string representation of the value
pub fn string_value(value: &XmlValue) -> String {
    value.to_string_value()
}

/// Get the string value of an optional value.
///
/// Returns empty string for None (empty sequence).
///
/// # Arguments
///
/// * `value` - Optional value to convert
///
/// # Returns
///
/// The string representation, or empty string for None
pub fn string_value_opt(value: Option<&XmlValue>) -> String {
    match value {
        None => String::new(),
        Some(v) => string_value(v),
    }
}

/// Convert a value to a double (numeric).
///
/// Implements the fn:number() behavior:
/// - Returns NaN for invalid conversions
/// - Handles UntypedAtomic by parsing as double
/// - Handles numeric types by conversion
///
/// # Arguments
///
/// * `value` - The value to convert
///
/// # Returns
///
/// The numeric value as f64, or NaN if conversion fails
pub fn to_number(value: &XmlValue) -> f64 {
    match &value.value {
        XmlValueKind::Atomic(atom) => atomic_to_number(atom),
        XmlValueKind::UntypedAtomic(s) => s.trim().parse().unwrap_or(f64::NAN),
        XmlValueKind::Union(inner) => to_number(inner),
        XmlValueKind::List { .. } => f64::NAN,
    }
}

/// Convert an atomic value to a double.
fn atomic_to_number(atom: &XmlAtomicValue) -> f64 {
    match atom {
        XmlAtomicValue::Double(d) => *d,
        XmlAtomicValue::Float(f) => *f as f64,
        XmlAtomicValue::Decimal(d) => d.to_string().parse().unwrap_or(f64::NAN),
        XmlAtomicValue::Integer(i) => i.to_string().parse().unwrap_or(f64::NAN),
        XmlAtomicValue::Boolean(b) => {
            if *b {
                1.0
            } else {
                0.0
            }
        }
        XmlAtomicValue::String(s) => s.trim().parse().unwrap_or(f64::NAN),
        _ => f64::NAN,
    }
}

/// Convert an optional value to a double.
///
/// Returns NaN for None (empty sequence).
pub fn to_number_opt(value: Option<&XmlValue>) -> f64 {
    match value {
        None => f64::NAN,
        Some(v) => to_number(v),
    }
}

/// Check if a value is empty (represents an empty sequence).
///
/// Note: XmlValue itself doesn't have an "empty" variant.
/// This checks for empty lists or None optionals.
pub fn is_empty_list(value: &XmlValue) -> bool {
    matches!(&value.value, XmlValueKind::List { items, .. } if items.is_empty())
}

/// Get the type code of the underlying atomic value.
///
/// For union types, returns the type code of the actual member type.
pub fn effective_type_code(value: &XmlValue) -> XmlTypeCode {
    match &value.value {
        XmlValueKind::Union(inner) => effective_type_code(inner),
        _ => value.type_code,
    }
}

/// Check if a value is a node (in XPath terms).
///
/// Returns true if the type code indicates a node type.
pub fn is_node_type(type_code: XmlTypeCode) -> bool {
    matches!(
        type_code,
        XmlTypeCode::Node
            | XmlTypeCode::Document
            | XmlTypeCode::Element
            | XmlTypeCode::Attribute
            | XmlTypeCode::Namespace
            | XmlTypeCode::ProcessingInstruction
            | XmlTypeCode::Comment
            | XmlTypeCode::Text
    )
}

/// Check if a value represents a node.
pub fn is_node(value: &XmlValue) -> bool {
    is_node_type(effective_type_code(value))
}

/// Unwrap a union value to its member value.
///
/// Recursively unwraps nested unions.
pub fn unwrap_union(value: &XmlValue) -> &XmlValue {
    match &value.value {
        XmlValueKind::Union(inner) => unwrap_union(inner),
        _ => value,
    }
}

/// Extract the string value of the first node in an XPathValue (XPath 1.0 rule).
///
/// In XPath 1.0, converting a node-set to string returns the string-value
/// of the first node in document order, or "" if empty.
/// For atomic values, delegates to the standard string conversion.
pub(crate) fn first_node_string_value<N: DomNavigator>(value: &XPathValue<N>) -> String {
    match value {
        XPathValue::Empty => String::new(),
        XPathValue::Item(XmlItem::Node(n)) => n.value(),
        XPathValue::Item(XmlItem::Atomic(v)) => v.to_string_value(),
        XPathValue::Sequence(items) => {
            // Find the document-order-first node in a single pass
            let mut first_node: Option<&N> = None;
            for item in items {
                if let XmlItem::Node(n) = item {
                    if let Some(current) = first_node {
                        if crate::xpath::node_ops::compare_document_order(n, current)
                            == std::cmp::Ordering::Less
                        {
                            first_node = Some(n);
                        }
                    } else {
                        first_node = Some(n);
                    }
                }
            }
            if let Some(n) = first_node {
                return n.value();
            }
            // Fallback: if no nodes, use first atomic's string value
            if let Some(XmlItem::Atomic(v)) = items.first() {
                v.to_string_value()
            } else {
                String::new()
            }
        }
    }
}

/// Convert an XPathValue to string using XPath 1.0 rules.
///
/// Same as `first_node_string_value` — for node-sets, uses first node.
/// For atomics, uses canonical string form.
pub(crate) fn to_string_10<N: DomNavigator>(value: &XPathValue<N>) -> String {
    first_node_string_value(value)
}

/// Convert an XPathValue to number using XPath 1.0 rules.
///
/// Converts to string first (via `to_string_10`), then parses as f64.
pub(crate) fn to_number_10<N: DomNavigator>(value: &XPathValue<N>) -> f64 {
    match value {
        XPathValue::Empty => f64::NAN,
        XPathValue::Item(XmlItem::Atomic(v)) => to_number(v),
        _ => to_string_10(value).trim().parse().unwrap_or(f64::NAN),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use num_bigint::BigInt;
    use rust_decimal::Decimal;

    #[test]
    fn test_atomize_atomic() {
        let value = XmlValue::string("hello");
        let result = atomize(&value).unwrap();
        assert_eq!(result.to_string_value(), "hello");
    }

    #[test]
    fn test_atomize_untyped() {
        let value = XmlValue::untyped("test");
        let result = atomize(&value).unwrap();
        assert_eq!(result.to_string_value(), "test");
    }

    #[test]
    fn test_atomize_opt_none() {
        let result = atomize_opt(None).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_atomize_opt_some() {
        let value = XmlValue::integer(BigInt::from(42));
        let result = atomize_opt(Some(&value)).unwrap();
        assert!(result.is_some());
    }

    #[test]
    fn test_atomize_required_none() {
        let result = atomize_required(None);
        assert!(result.is_err());
        if let Err(XPathError::XPTY0004 { .. }) = result {
            // Expected
        } else {
            panic!("Expected XPTY0004 error");
        }
    }

    #[test]
    fn test_string_value() {
        assert_eq!(string_value(&XmlValue::string("hello")), "hello");
        assert_eq!(string_value(&XmlValue::boolean(true)), "true");
        assert_eq!(string_value(&XmlValue::integer(BigInt::from(123))), "123");
    }

    #[test]
    fn test_string_value_opt_none() {
        assert_eq!(string_value_opt(None), "");
    }

    #[test]
    fn test_to_number() {
        assert_eq!(to_number(&XmlValue::double(2.5)), 2.5);
        assert_eq!(to_number(&XmlValue::float(2.5)), 2.5);
        assert_eq!(to_number(&XmlValue::integer(BigInt::from(42))), 42.0);
        assert_eq!(to_number(&XmlValue::decimal(Decimal::new(125, 2))), 1.25);
        assert_eq!(to_number(&XmlValue::string("2.5")), 2.5);
        assert!(to_number(&XmlValue::string("not a number")).is_nan());
    }

    #[test]
    fn test_to_number_opt_none() {
        assert!(to_number_opt(None).is_nan());
    }

    #[test]
    fn test_to_number_untyped() {
        assert_eq!(to_number(&XmlValue::untyped("42.5")), 42.5);
        assert_eq!(to_number(&XmlValue::untyped("  2.5  ")), 2.5); // Trimmed
    }

    #[test]
    fn test_effective_type_code() {
        let value = XmlValue::string("test");
        assert_eq!(effective_type_code(&value), XmlTypeCode::String);

        let value = XmlValue::integer(BigInt::from(1));
        assert_eq!(effective_type_code(&value), XmlTypeCode::Integer);
    }

    #[test]
    fn test_is_node_type() {
        assert!(is_node_type(XmlTypeCode::Element));
        assert!(is_node_type(XmlTypeCode::Attribute));
        assert!(is_node_type(XmlTypeCode::Document));
        assert!(!is_node_type(XmlTypeCode::String));
        assert!(!is_node_type(XmlTypeCode::Integer));
    }

    #[test]
    fn test_is_node() {
        // Atomic values are not nodes
        assert!(!is_node(&XmlValue::string("test")));
        assert!(!is_node(&XmlValue::integer(BigInt::from(1))));

        // A value with node type code would be a node
        // (We can't easily create one without a navigator, but we test the type check)
        let node_value = XmlValue::new(
            XmlTypeCode::Element,
            XmlValueKind::UntypedAtomic("element content".to_string()),
        );
        assert!(is_node(&node_value));
    }

    // --- XPath 1.0 conversion tests ---

    use crate::xpath::RoXmlNavigator;

    #[test]
    fn test_first_node_string_value_empty() {
        let value: XPathValue<RoXmlNavigator<'static>> = XPathValue::empty();
        assert_eq!(first_node_string_value(&value), "");
    }

    #[test]
    fn test_first_node_string_value_single_atomic() {
        let value: XPathValue<RoXmlNavigator<'static>> = XPathValue::string("hello");
        assert_eq!(first_node_string_value(&value), "hello");
    }

    #[test]
    fn test_first_node_string_value_single_node() {
        let doc = roxmltree::Document::parse("<root>text content</root>").unwrap();
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child(); // move to <root>
        let value = XPathValue::from_node(nav);
        assert_eq!(first_node_string_value(&value), "text content");
    }

    #[test]
    fn test_first_node_string_value_multi_node_sequence() {
        let doc = roxmltree::Document::parse("<r><a>first</a><b>second</b></r>").unwrap();
        let mut nav_a = RoXmlNavigator::new(&doc);
        nav_a.move_to_first_child(); // <r>
        nav_a.move_to_first_child(); // <a>
        let mut nav_b = nav_a.clone();
        nav_b.move_to_next_sibling(); // <b>
        let value = XPathValue::from_sequence(vec![XmlItem::Node(nav_a), XmlItem::Node(nav_b)]);
        // XPath 1.0: first node's string value
        assert_eq!(first_node_string_value(&value), "first");
    }

    #[test]
    fn test_to_string_10_delegates() {
        let value: XPathValue<RoXmlNavigator<'static>> = XPathValue::string("abc");
        assert_eq!(to_string_10(&value), "abc");
    }

    #[test]
    fn test_to_number_10_empty() {
        let value: XPathValue<RoXmlNavigator<'static>> = XPathValue::empty();
        assert!(to_number_10(&value).is_nan());
    }

    #[test]
    fn test_to_number_10_atomic() {
        let value: XPathValue<RoXmlNavigator<'static>> = XPathValue::double(2.75);
        assert_eq!(to_number_10(&value), 2.75);
    }

    #[test]
    fn test_to_number_10_node_numeric() {
        let doc = roxmltree::Document::parse("<n>42.5</n>").unwrap();
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child(); // <n>
        let value = XPathValue::from_node(nav);
        assert_eq!(to_number_10(&value), 42.5);
    }

    #[test]
    fn test_to_number_10_node_non_numeric() {
        let doc = roxmltree::Document::parse("<n>not a number</n>").unwrap();
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child(); // <n>
        let value = XPathValue::from_node(nav);
        assert!(to_number_10(&value).is_nan());
    }
}
