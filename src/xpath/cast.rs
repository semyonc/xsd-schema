//! Type casting operations for XPath evaluation.
//!
//! This module implements XPath 2.0 type casting rules for converting
//! values between different types.
//!
//! ## Casting Rules
//!
//! - `cast_to`: Explicit cast expression (`value cast as type`)
//! - `treat_as`: Type assertion without conversion (`value treat as type`)
//! - `instance_of`: Type test (`value instance of type`)
//! - `castable`: Castability test (`value castable as type`)

use num_bigint::BigInt;
use rust_decimal::Decimal;

use crate::namespace::qname::QualifiedName;
use crate::namespace::table::{well_known, NameTable};
use crate::types::value::{XmlAtomicValue, XmlValue, XmlValueKind};
use crate::types::XmlTypeCode;
use crate::xpath::ast::OccurrenceIndicator;
use super::error::XPathError;

/// Cast an atomic value to a target type.
///
/// This implements the XPath `cast as` expression for atomic values.
///
/// # Arguments
///
/// * `value` - The value to cast
/// * `target_type` - The target type code
///
/// # Returns
///
/// * `Ok(XmlValue)` - The cast value
/// * `Err(XPathError)` - If casting fails (FORG0001) or types are incompatible (XPTY0004)
pub fn cast_to(value: &XmlValue, target_type: XmlTypeCode) -> Result<XmlValue, XPathError> {
    // Same type - no conversion needed
    if value.type_code == target_type {
        return Ok(value.clone());
    }

    let string_val = value.to_string_value();

    match target_type {
        XmlTypeCode::String => Ok(XmlValue::string(string_val)),

        XmlTypeCode::Boolean => cast_to_boolean(value, &string_val),

        XmlTypeCode::Decimal => cast_to_decimal(value, &string_val),

        XmlTypeCode::Integer => cast_to_integer(value, &string_val),

        XmlTypeCode::Float => cast_to_float(value, &string_val),

        XmlTypeCode::Double => cast_to_double(value, &string_val),

        XmlTypeCode::UntypedAtomic => Ok(XmlValue::untyped(string_val)),

        // For other types, we need more complex casting logic
        _ => Err(XPathError::type_mismatch(
            format!("{:?}", value.type_code),
            format!("{:?}", target_type),
        )),
    }
}

/// Cast a value to boolean.
fn cast_to_boolean(value: &XmlValue, string_val: &str) -> Result<XmlValue, XPathError> {
    let result = match &value.value {
        XmlValueKind::Atomic(XmlAtomicValue::Boolean(b)) => *b,
        _ => {
            let s = string_val.trim();
            match s {
                "true" | "1" => true,
                "false" | "0" => false,
                _ => {
                    return Err(XPathError::invalid_cast_value(string_val, "xs:boolean"));
                }
            }
        }
    };
    Ok(XmlValue::boolean(result))
}

/// Cast a value to decimal.
fn cast_to_decimal(value: &XmlValue, string_val: &str) -> Result<XmlValue, XPathError> {
    let result = match &value.value {
        XmlValueKind::Atomic(XmlAtomicValue::Decimal(d)) => *d,
        XmlValueKind::Atomic(XmlAtomicValue::Integer(i)) => {
            i.to_string()
                .parse::<Decimal>()
                .map_err(|_| XPathError::invalid_cast_value(string_val, "xs:decimal"))?
        }
        XmlValueKind::Atomic(XmlAtomicValue::Float(f)) => {
            if f.is_nan() || f.is_infinite() {
                return Err(XPathError::invalid_cast_value(string_val, "xs:decimal"));
            }
            Decimal::try_from(*f)
                .map_err(|_| XPathError::invalid_cast_value(string_val, "xs:decimal"))?
        }
        XmlValueKind::Atomic(XmlAtomicValue::Double(d)) => {
            if d.is_nan() || d.is_infinite() {
                return Err(XPathError::invalid_cast_value(string_val, "xs:decimal"));
            }
            Decimal::try_from(*d)
                .map_err(|_| XPathError::invalid_cast_value(string_val, "xs:decimal"))?
        }
        XmlValueKind::Atomic(XmlAtomicValue::Boolean(b)) => {
            if *b {
                Decimal::ONE
            } else {
                Decimal::ZERO
            }
        }
        _ => string_val
            .trim()
            .parse::<Decimal>()
            .map_err(|_| XPathError::invalid_cast_value(string_val, "xs:decimal"))?,
    };
    Ok(XmlValue::decimal(result))
}

/// Cast a value to integer.
fn cast_to_integer(value: &XmlValue, string_val: &str) -> Result<XmlValue, XPathError> {
    let result = match &value.value {
        XmlValueKind::Atomic(XmlAtomicValue::Integer(i)) => i.clone(),
        XmlValueKind::Atomic(XmlAtomicValue::Decimal(d)) => {
            // Truncate decimal to integer
            let truncated = d.trunc();
            truncated
                .to_string()
                .parse::<BigInt>()
                .map_err(|_| XPathError::invalid_cast_value(string_val, "xs:integer"))?
        }
        XmlValueKind::Atomic(XmlAtomicValue::Float(f)) => {
            if f.is_nan() || f.is_infinite() {
                return Err(XPathError::invalid_cast_value(string_val, "xs:integer"));
            }
            BigInt::from(f.trunc() as i64)
        }
        XmlValueKind::Atomic(XmlAtomicValue::Double(d)) => {
            if d.is_nan() || d.is_infinite() {
                return Err(XPathError::invalid_cast_value(string_val, "xs:integer"));
            }
            BigInt::from(d.trunc() as i64)
        }
        XmlValueKind::Atomic(XmlAtomicValue::Boolean(b)) => {
            BigInt::from(if *b { 1 } else { 0 })
        }
        _ => string_val
            .trim()
            .parse::<BigInt>()
            .map_err(|_| XPathError::invalid_cast_value(string_val, "xs:integer"))?,
    };
    Ok(XmlValue::integer(result))
}

/// Cast a value to float.
fn cast_to_float(value: &XmlValue, string_val: &str) -> Result<XmlValue, XPathError> {
    let result = match &value.value {
        XmlValueKind::Atomic(XmlAtomicValue::Float(f)) => *f,
        XmlValueKind::Atomic(XmlAtomicValue::Double(d)) => *d as f32,
        XmlValueKind::Atomic(XmlAtomicValue::Decimal(d)) => d
            .to_string()
            .parse::<f32>()
            .map_err(|_| XPathError::invalid_cast_value(string_val, "xs:float"))?,
        XmlValueKind::Atomic(XmlAtomicValue::Integer(i)) => i
            .to_string()
            .parse::<f32>()
            .map_err(|_| XPathError::invalid_cast_value(string_val, "xs:float"))?,
        XmlValueKind::Atomic(XmlAtomicValue::Boolean(b)) => {
            if *b {
                1.0
            } else {
                0.0
            }
        }
        _ => parse_float_with_special(string_val.trim())
            .map_err(|_| XPathError::invalid_cast_value(string_val, "xs:float"))?,
    };
    Ok(XmlValue::float(result))
}

/// Cast a value to double.
fn cast_to_double(value: &XmlValue, string_val: &str) -> Result<XmlValue, XPathError> {
    let result = match &value.value {
        XmlValueKind::Atomic(XmlAtomicValue::Double(d)) => *d,
        XmlValueKind::Atomic(XmlAtomicValue::Float(f)) => *f as f64,
        XmlValueKind::Atomic(XmlAtomicValue::Decimal(d)) => d
            .to_string()
            .parse::<f64>()
            .map_err(|_| XPathError::invalid_cast_value(string_val, "xs:double"))?,
        XmlValueKind::Atomic(XmlAtomicValue::Integer(i)) => i
            .to_string()
            .parse::<f64>()
            .map_err(|_| XPathError::invalid_cast_value(string_val, "xs:double"))?,
        XmlValueKind::Atomic(XmlAtomicValue::Boolean(b)) => {
            if *b {
                1.0
            } else {
                0.0
            }
        }
        _ => parse_double_with_special(string_val.trim())
            .map_err(|_| XPathError::invalid_cast_value(string_val, "xs:double"))?,
    };
    Ok(XmlValue::double(result))
}

/// Parse a float string, handling special values like INF and NaN.
fn parse_float_with_special(s: &str) -> Result<f32, ()> {
    match s {
        "INF" => Ok(f32::INFINITY),
        "-INF" => Ok(f32::NEG_INFINITY),
        "NaN" => Ok(f32::NAN),
        _ => s.parse::<f32>().map_err(|_| ()),
    }
}

/// Parse a double string, handling special values like INF and NaN.
fn parse_double_with_special(s: &str) -> Result<f64, ()> {
    match s {
        "INF" => Ok(f64::INFINITY),
        "-INF" => Ok(f64::NEG_INFINITY),
        "NaN" => Ok(f64::NAN),
        _ => s.parse::<f64>().map_err(|_| ()),
    }
}

/// Treat a value as a specific type (type assertion without conversion).
///
/// This implements the XPath `treat as` expression. Unlike `cast`, this
/// does not perform any conversion - it just validates that the value
/// already has the expected type.
///
/// # Arguments
///
/// * `value` - The value to check
/// * `target_type` - The expected type code
///
/// # Returns
///
/// * `Ok(XmlValue)` - The original value if it matches
/// * `Err(XPathError)` - XPTY0004 if type doesn't match
pub fn treat_as(value: &XmlValue, target_type: XmlTypeCode) -> Result<XmlValue, XPathError> {
    if type_matches(value.type_code, target_type) {
        Ok(value.clone())
    } else {
        Err(XPathError::type_mismatch(
            format!("{:?}", target_type),
            format!("{:?}", value.type_code),
        ))
    }
}

/// Check if a value is an instance of a type.
///
/// This implements the XPath `instance of` expression.
///
/// # Arguments
///
/// * `value` - The value to check
/// * `target_type` - The type to check against
///
/// # Returns
///
/// `true` if the value matches the type, `false` otherwise
pub fn instance_of(value: &XmlValue, target_type: XmlTypeCode) -> bool {
    type_matches(value.type_code, target_type)
}

/// Check if a value is an instance of a type (optional value version).
///
/// Returns true for None if target type allows empty sequence.
pub fn instance_of_opt(
    value: Option<&XmlValue>,
    target_type: XmlTypeCode,
    allow_empty: bool,
) -> bool {
    match value {
        None => allow_empty,
        Some(v) => instance_of(v, target_type),
    }
}

/// Check if a value can be cast to a type.
///
/// This implements the XPath `castable as` expression.
///
/// # Arguments
///
/// * `value` - The value to check
/// * `target_type` - The target type
///
/// # Returns
///
/// `true` if the cast would succeed, `false` otherwise
pub fn castable(value: &XmlValue, target_type: XmlTypeCode) -> bool {
    cast_to(value, target_type).is_ok()
}

/// Check if a value can be cast to a type (optional value version).
pub fn castable_opt(value: Option<&XmlValue>, target_type: XmlTypeCode, allow_empty: bool) -> bool {
    match value {
        None => allow_empty,
        Some(v) => castable(v, target_type),
    }
}

/// Check if a type code is an integer-derived type.
fn is_integer_derived(code: XmlTypeCode) -> bool {
    matches!(
        code,
        XmlTypeCode::Integer
            | XmlTypeCode::NonPositiveInteger
            | XmlTypeCode::NegativeInteger
            | XmlTypeCode::Long
            | XmlTypeCode::Int
            | XmlTypeCode::Short
            | XmlTypeCode::Byte
            | XmlTypeCode::NonNegativeInteger
            | XmlTypeCode::UnsignedLong
            | XmlTypeCode::UnsignedInt
            | XmlTypeCode::UnsignedShort
            | XmlTypeCode::UnsignedByte
            | XmlTypeCode::PositiveInteger
    )
}

/// Check if a source type matches a target type for type checking.
///
/// This handles type compatibility rules including:
/// - Exact type match
/// - anyAtomicType matches any atomic type
/// - String derived types match string
/// - Integer derived types match integer
pub fn type_matches(source: XmlTypeCode, target: XmlTypeCode) -> bool {
    if source == target {
        return true;
    }

    // anyAtomicType matches any atomic type
    if target == XmlTypeCode::AnyAtomicType {
        return source.is_atomic();
    }

    // Item matches everything
    if target == XmlTypeCode::Item {
        return true;
    }

    // String type hierarchy
    if target == XmlTypeCode::String {
        return source.is_string_derived() || source == XmlTypeCode::UntypedAtomic;
    }

    // Integer type hierarchy
    if target == XmlTypeCode::Integer {
        return is_integer_derived(source);
    }

    // Decimal type hierarchy (includes integer)
    if target == XmlTypeCode::Decimal {
        return source == XmlTypeCode::Decimal || is_integer_derived(source);
    }

    // Numeric check
    if target == XmlTypeCode::Double || target == XmlTypeCode::Float {
        return source.is_numeric();
    }

    false
}

/// Convert a resolved atomic type QualifiedName to XmlTypeCode.
///
/// The QualifiedName should have been resolved during binding phase
/// and must be in the XS (XML Schema) namespace.
///
/// # Errors
///
/// Returns XPST0051 if the type name is not a known atomic type.
pub fn resolved_type_to_type_code(
    qname: &QualifiedName,
    names: &NameTable,
) -> Result<XmlTypeCode, XPathError> {
    // Check namespace is XS_NAMESPACE
    match qname.namespace_uri {
        Some(ns_id) if ns_id == well_known::XS_NAMESPACE => {}
        _ => {
            let local = names.resolve(qname.local_name);
            return Err(XPathError::XPST0051 {
                type_name: local.to_string(),
            });
        }
    }

    // Get local name and convert to type code
    let local_name = names.resolve(qname.local_name);
    XmlTypeCode::from_local_name(&local_name).ok_or_else(|| XPathError::XPST0051 {
        type_name: local_name.to_string(),
    })
}

/// Check if an occurrence indicator allows the given item count.
///
/// This implements XPath 2.0 sequence type cardinality matching:
/// - `One` (no indicator): exactly 1 item
/// - `ZeroOrOne` (`?`): 0 or 1 items
/// - `ZeroOrMore` (`*`): any count
/// - `OneOrMore` (`+`): at least 1 item
pub fn occurrence_allows_count(occ: OccurrenceIndicator, count: usize) -> bool {
    match occ {
        OccurrenceIndicator::One => count == 1,
        OccurrenceIndicator::ZeroOrOne => count <= 1,
        OccurrenceIndicator::ZeroOrMore => true,
        OccurrenceIndicator::OneOrMore => count >= 1,
    }
}

/// Cast a numeric value to a specific integer subtype.
///
/// This handles casting to types like xs:int, xs:short, xs:byte, etc.
/// with range checking.
pub fn cast_to_integer_subtype(
    value: &XmlValue,
    target_type: XmlTypeCode,
) -> Result<XmlValue, XPathError> {
    // First cast to integer
    let int_val = cast_to(value, XmlTypeCode::Integer)?;
    let bigint = int_val
        .as_integer()
        .ok_or_else(|| XPathError::internal("Expected integer after cast"))?;

    // Then validate range for the specific subtype
    let (min, max): (i128, i128) = match target_type {
        XmlTypeCode::Byte => (i8::MIN as i128, i8::MAX as i128),
        XmlTypeCode::Short => (i16::MIN as i128, i16::MAX as i128),
        XmlTypeCode::Int => (i32::MIN as i128, i32::MAX as i128),
        XmlTypeCode::Long => (i64::MIN as i128, i64::MAX as i128),
        XmlTypeCode::UnsignedByte => (0, u8::MAX as i128),
        XmlTypeCode::UnsignedShort => (0, u16::MAX as i128),
        XmlTypeCode::UnsignedInt => (0, u32::MAX as i128),
        XmlTypeCode::UnsignedLong => (0, u64::MAX as i128),
        XmlTypeCode::PositiveInteger => (1, i128::MAX),
        XmlTypeCode::NonNegativeInteger => (0, i128::MAX),
        XmlTypeCode::NegativeInteger => (i128::MIN, -1),
        XmlTypeCode::NonPositiveInteger => (i128::MIN, 0),
        XmlTypeCode::Integer => return Ok(int_val),
        _ => {
            return Err(XPathError::type_mismatch(
                format!("{:?}", value.type_code),
                format!("{:?}", target_type),
            ))
        }
    };

    // Check range
    let val_i128: i128 = bigint
        .to_string()
        .parse()
        .map_err(|_| XPathError::invalid_cast_value(bigint.to_string(), format!("{:?}", target_type)))?;

    if val_i128 < min || val_i128 > max {
        return Err(XPathError::invalid_cast_value(
            bigint.to_string(),
            format!("{:?}", target_type),
        ));
    }

    Ok(XmlValue::new(
        target_type,
        XmlValueKind::Atomic(XmlAtomicValue::Integer(bigint.clone())),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cast_string_to_integer() {
        let value = XmlValue::string("42");
        let result = cast_to(&value, XmlTypeCode::Integer).unwrap();
        assert_eq!(result.type_code, XmlTypeCode::Integer);
        assert_eq!(result.as_integer().unwrap(), &BigInt::from(42));
    }

    #[test]
    fn test_cast_string_to_decimal() {
        let value = XmlValue::string("2.5");
        let result = cast_to(&value, XmlTypeCode::Decimal).unwrap();
        assert_eq!(result.type_code, XmlTypeCode::Decimal);
    }

    #[test]
    fn test_cast_string_to_boolean() {
        assert_eq!(
            cast_to(&XmlValue::string("true"), XmlTypeCode::Boolean)
                .unwrap()
                .as_boolean(),
            Some(true)
        );
        assert_eq!(
            cast_to(&XmlValue::string("false"), XmlTypeCode::Boolean)
                .unwrap()
                .as_boolean(),
            Some(false)
        );
        assert_eq!(
            cast_to(&XmlValue::string("1"), XmlTypeCode::Boolean)
                .unwrap()
                .as_boolean(),
            Some(true)
        );
        assert_eq!(
            cast_to(&XmlValue::string("0"), XmlTypeCode::Boolean)
                .unwrap()
                .as_boolean(),
            Some(false)
        );
    }

    #[test]
    fn test_cast_invalid_string_to_boolean() {
        let result = cast_to(&XmlValue::string("yes"), XmlTypeCode::Boolean);
        assert!(result.is_err());
    }

    #[test]
    fn test_cast_integer_to_double() {
        let value = XmlValue::integer(BigInt::from(42));
        let result = cast_to(&value, XmlTypeCode::Double).unwrap();
        assert_eq!(result.as_double(), Some(42.0));
    }

    #[test]
    fn test_cast_double_to_integer() {
        let value = XmlValue::double(42.7);
        let result = cast_to(&value, XmlTypeCode::Integer).unwrap();
        assert_eq!(result.as_integer().unwrap(), &BigInt::from(42)); // Truncated
    }

    #[test]
    fn test_cast_nan_to_integer_fails() {
        let value = XmlValue::double(f64::NAN);
        let result = cast_to(&value, XmlTypeCode::Integer);
        assert!(result.is_err());
    }

    #[test]
    fn test_cast_inf_to_decimal_fails() {
        let value = XmlValue::double(f64::INFINITY);
        let result = cast_to(&value, XmlTypeCode::Decimal);
        assert!(result.is_err());
    }

    #[test]
    fn test_cast_same_type() {
        let value = XmlValue::string("hello");
        let result = cast_to(&value, XmlTypeCode::String).unwrap();
        assert_eq!(result.to_string_value(), "hello");
    }

    #[test]
    fn test_instance_of() {
        assert!(instance_of(&XmlValue::string("test"), XmlTypeCode::String));
        assert!(instance_of(
            &XmlValue::integer(BigInt::from(1)),
            XmlTypeCode::Integer
        ));
        assert!(!instance_of(
            &XmlValue::string("test"),
            XmlTypeCode::Integer
        ));

        // anyAtomicType should match any atomic
        assert!(instance_of(
            &XmlValue::string("test"),
            XmlTypeCode::AnyAtomicType
        ));
    }

    #[test]
    fn test_castable() {
        assert!(castable(&XmlValue::string("42"), XmlTypeCode::Integer));
        assert!(!castable(
            &XmlValue::string("not a number"),
            XmlTypeCode::Integer
        ));
    }

    #[test]
    fn test_treat_as_matching() {
        let value = XmlValue::string("test");
        let result = treat_as(&value, XmlTypeCode::String);
        assert!(result.is_ok());
    }

    #[test]
    fn test_treat_as_non_matching() {
        let value = XmlValue::string("test");
        let result = treat_as(&value, XmlTypeCode::Integer);
        assert!(result.is_err());
    }

    #[test]
    fn test_cast_special_float_values() {
        let inf = XmlValue::string("INF");
        let result = cast_to(&inf, XmlTypeCode::Float).unwrap();
        assert!(result.as_double().unwrap().is_infinite());

        let nan = XmlValue::string("NaN");
        let result = cast_to(&nan, XmlTypeCode::Double).unwrap();
        assert!(result.as_double().unwrap().is_nan());
    }

    #[test]
    fn test_cast_to_integer_subtype() {
        let value = XmlValue::string("100");

        // Should succeed for byte
        let result = cast_to_integer_subtype(&value, XmlTypeCode::Byte).unwrap();
        assert_eq!(result.type_code, XmlTypeCode::Byte);

        // Should fail for byte (out of range)
        let big = XmlValue::string("500");
        let result = cast_to_integer_subtype(&big, XmlTypeCode::Byte);
        assert!(result.is_err());
    }
}
