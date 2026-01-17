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

use crate::types::value::{XmlValue, XmlValueKind, XmlAtomicValue};
use crate::types::XmlTypeCode;
use super::error::XPathError;

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
        XmlValueKind::List { items, item_type } if items.len() == 1 => {
            Ok(XmlValue::new(
                *item_type,
                XmlValueKind::Atomic(items[0].clone()),
            ))
        }

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
        XmlAtomicValue::Boolean(b) => if *b { 1.0 } else { 0.0 },
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
        assert_eq!(to_number(&XmlValue::double(3.14)), 3.14);
        assert_eq!(to_number(&XmlValue::float(2.5)), 2.5);
        assert_eq!(to_number(&XmlValue::integer(BigInt::from(42))), 42.0);
        assert_eq!(to_number(&XmlValue::decimal(Decimal::new(125, 2))), 1.25);
        assert_eq!(to_number(&XmlValue::string("3.14")), 3.14);
        assert!(to_number(&XmlValue::string("not a number")).is_nan());
    }

    #[test]
    fn test_to_number_opt_none() {
        assert!(to_number_opt(None).is_nan());
    }

    #[test]
    fn test_to_number_untyped() {
        assert_eq!(to_number(&XmlValue::untyped("42.5")), 42.5);
        assert_eq!(to_number(&XmlValue::untyped("  3.14  ")), 3.14); // Trimmed
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
}
