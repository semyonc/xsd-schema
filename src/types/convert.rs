//! XPath2 type conversion API
//!
//! This module provides type conversion functions following XPath2/XQuery
//! type promotion and casting rules.
//!
//! ## Conversion Types
//!
//! - **Casting**: Explicit type conversion via `cast as` (strict)
//! - **Type Promotion**: Implicit conversion for numeric operations
//! - **Atomization**: Converting nodes to atomic values
//!
//! ## Reference
//!
//! Based on XPath2 type conversion rules and the C# XPath2Convert implementation.

use num_bigint::BigInt;
use rust_decimal::Decimal;

use super::value::{XmlValue, XmlValueKind, XmlAtomicValue};
#[cfg(feature = "xsd11")]
use super::sequence::{SequenceType, ItemType};
use super::{XmlTypeCode, PrimitiveTypeCode};
use super::validators::{ValidatorRegistry, ValidationError};

/// Error type for conversion operations
#[derive(Debug, Clone)]
pub enum ConversionError {
    /// Type conversion not allowed (XPTY0004)
    TypeMismatch {
        from: XmlTypeCode,
        to: XmlTypeCode,
    },
    /// Invalid value for target type (FORG0001)
    InvalidValue {
        value: String,
        target_type: &'static str,
        message: String,
    },
    /// Overflow during conversion (FOAR0002)
    Overflow {
        value: String,
        target_type: &'static str,
    },
    /// Empty sequence where value expected
    EmptySequence,
    /// Validation error during conversion
    Validation(ValidationError),
}

impl std::fmt::Display for ConversionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TypeMismatch { from, to } => {
                write!(f, "XPTY0004: Cannot convert {:?} to {:?}", from, to)
            }
            Self::InvalidValue { value, target_type, message } => {
                write!(f, "FORG0001: Invalid {} value '{}': {}", target_type, value, message)
            }
            Self::Overflow { value, target_type } => {
                write!(f, "FOAR0002: Overflow converting '{}' to {}", value, target_type)
            }
            Self::EmptySequence => {
                write!(f, "XPTY0004: Empty sequence where value expected")
            }
            Self::Validation(e) => write!(f, "{}", e),
        }
    }
}

impl std::error::Error for ConversionError {}

impl From<ValidationError> for ConversionError {
    fn from(e: ValidationError) -> Self {
        Self::Validation(e)
    }
}

/// Result type for conversion operations
pub type ConversionResult<T> = Result<T, ConversionError>;

/// XPath2-compatible type converter
///
/// Handles type conversions following XPath2/XQuery rules.
pub struct TypeConverter {
    validators: ValidatorRegistry,
}

impl TypeConverter {
    /// Create a new type converter
    pub fn new() -> Self {
        Self {
            validators: ValidatorRegistry::new(),
        }
    }

    /// Create with a custom validator registry
    pub fn with_validators(validators: ValidatorRegistry) -> Self {
        Self { validators }
    }

    /// Cast a value to the target type
    ///
    /// This is the strict casting operation used by `cast as`.
    pub fn cast(&self, value: &XmlValue, target: XmlTypeCode) -> ConversionResult<XmlValue> {
        // Same type - no conversion needed
        if value.type_code == target {
            return Ok(value.clone());
        }

        // Handle untyped atomic - cast via string
        if value.is_untyped() {
            let str_val = value.to_string_value();
            return self.validators.validate(target, &str_val)
                .map_err(ConversionError::from);
        }

        // Check if casting is allowed
        if !can_cast(value.type_code, target) {
            return Err(ConversionError::TypeMismatch {
                from: value.type_code,
                to: target,
            });
        }

        // Perform the cast
        self.perform_cast(value, target)
    }

    /// Convert value to string
    pub fn to_string(&self, value: &XmlValue) -> String {
        value.to_string_value()
    }

    /// Convert value to boolean (effective boolean value)
    pub fn to_boolean(&self, value: &XmlValue) -> ConversionResult<bool> {
        match &value.value {
            XmlValueKind::Atomic(XmlAtomicValue::Boolean(b)) => Ok(*b),
            XmlValueKind::Atomic(XmlAtomicValue::String(s)) => Ok(!s.is_empty()),
            XmlValueKind::Atomic(XmlAtomicValue::Integer(i)) => Ok(*i != BigInt::from(0)),
            XmlValueKind::Atomic(XmlAtomicValue::Decimal(d)) => Ok(!d.is_zero()),
            XmlValueKind::Atomic(XmlAtomicValue::Float(f)) => Ok(*f != 0.0 && !f.is_nan()),
            XmlValueKind::Atomic(XmlAtomicValue::Double(d)) => Ok(*d != 0.0 && !d.is_nan()),
            XmlValueKind::UntypedAtomic(s) => Ok(!s.is_empty()),
            _ => Err(ConversionError::TypeMismatch {
                from: value.type_code,
                to: XmlTypeCode::Boolean,
            }),
        }
    }

    /// Convert value to double (numeric promotion)
    pub fn to_double(&self, value: &XmlValue) -> ConversionResult<f64> {
        match &value.value {
            XmlValueKind::Atomic(XmlAtomicValue::Double(d)) => Ok(*d),
            XmlValueKind::Atomic(XmlAtomicValue::Float(f)) => Ok(*f as f64),
            XmlValueKind::Atomic(XmlAtomicValue::Decimal(d)) => {
                d.to_string().parse().map_err(|_| ConversionError::Overflow {
                    value: d.to_string(),
                    target_type: "double",
                })
            }
            XmlValueKind::Atomic(XmlAtomicValue::Integer(i)) => {
                i.to_string().parse().map_err(|_| ConversionError::Overflow {
                    value: i.to_string(),
                    target_type: "double",
                })
            }
            XmlValueKind::Atomic(XmlAtomicValue::Boolean(b)) => {
                Ok(if *b { 1.0 } else { 0.0 })
            }
            XmlValueKind::UntypedAtomic(s) => {
                s.trim().parse().map_err(|_| ConversionError::InvalidValue {
                    value: s.clone(),
                    target_type: "double",
                    message: "Not a valid number".to_string(),
                })
            }
            _ => Err(ConversionError::TypeMismatch {
                from: value.type_code,
                to: XmlTypeCode::Double,
            }),
        }
    }

    /// Convert value to decimal
    pub fn to_decimal(&self, value: &XmlValue) -> ConversionResult<Decimal> {
        match &value.value {
            XmlValueKind::Atomic(XmlAtomicValue::Decimal(d)) => Ok(*d),
            XmlValueKind::Atomic(XmlAtomicValue::Integer(i)) => {
                i.to_string().parse().map_err(|_| ConversionError::Overflow {
                    value: i.to_string(),
                    target_type: "decimal",
                })
            }
            XmlValueKind::Atomic(XmlAtomicValue::Float(f)) => {
                if f.is_nan() || f.is_infinite() {
                    return Err(ConversionError::InvalidValue {
                        value: f.to_string(),
                        target_type: "decimal",
                        message: "Cannot convert NaN or Infinity to decimal".to_string(),
                    });
                }
                Decimal::try_from(*f).map_err(|_| ConversionError::Overflow {
                    value: f.to_string(),
                    target_type: "decimal",
                })
            }
            XmlValueKind::Atomic(XmlAtomicValue::Double(d)) => {
                if d.is_nan() || d.is_infinite() {
                    return Err(ConversionError::InvalidValue {
                        value: d.to_string(),
                        target_type: "decimal",
                        message: "Cannot convert NaN or Infinity to decimal".to_string(),
                    });
                }
                Decimal::try_from(*d).map_err(|_| ConversionError::Overflow {
                    value: d.to_string(),
                    target_type: "decimal",
                })
            }
            XmlValueKind::Atomic(XmlAtomicValue::Boolean(b)) => {
                Ok(if *b { Decimal::ONE } else { Decimal::ZERO })
            }
            XmlValueKind::UntypedAtomic(s) => {
                s.trim().parse().map_err(|_| ConversionError::InvalidValue {
                    value: s.clone(),
                    target_type: "decimal",
                    message: "Not a valid decimal".to_string(),
                })
            }
            _ => Err(ConversionError::TypeMismatch {
                from: value.type_code,
                to: XmlTypeCode::Decimal,
            }),
        }
    }

    /// Convert value to integer
    pub fn to_integer(&self, value: &XmlValue) -> ConversionResult<BigInt> {
        match &value.value {
            XmlValueKind::Atomic(XmlAtomicValue::Integer(i)) => Ok(i.clone()),
            XmlValueKind::Atomic(XmlAtomicValue::Decimal(d)) => {
                // Truncate to integer
                let truncated = d.trunc();
                truncated.to_string().parse().map_err(|_| ConversionError::Overflow {
                    value: d.to_string(),
                    target_type: "integer",
                })
            }
            XmlValueKind::Atomic(XmlAtomicValue::Float(f)) => {
                if f.is_nan() || f.is_infinite() {
                    return Err(ConversionError::InvalidValue {
                        value: f.to_string(),
                        target_type: "integer",
                        message: "Cannot convert NaN or Infinity to integer".to_string(),
                    });
                }
                let truncated = f.trunc();
                (truncated as i64).to_string().parse().map_err(|_| ConversionError::Overflow {
                    value: f.to_string(),
                    target_type: "integer",
                })
            }
            XmlValueKind::Atomic(XmlAtomicValue::Double(d)) => {
                if d.is_nan() || d.is_infinite() {
                    return Err(ConversionError::InvalidValue {
                        value: d.to_string(),
                        target_type: "integer",
                        message: "Cannot convert NaN or Infinity to integer".to_string(),
                    });
                }
                let truncated = d.trunc();
                (truncated as i64).to_string().parse().map_err(|_| ConversionError::Overflow {
                    value: d.to_string(),
                    target_type: "integer",
                })
            }
            XmlValueKind::Atomic(XmlAtomicValue::Boolean(b)) => {
                Ok(if *b { BigInt::from(1) } else { BigInt::from(0) })
            }
            XmlValueKind::UntypedAtomic(s) => {
                s.trim().parse().map_err(|_| ConversionError::InvalidValue {
                    value: s.clone(),
                    target_type: "integer",
                    message: "Not a valid integer".to_string(),
                })
            }
            _ => Err(ConversionError::TypeMismatch {
                from: value.type_code,
                to: XmlTypeCode::Integer,
            }),
        }
    }

    /// Check if value matches the given sequence type
    #[cfg(feature = "xsd11")]
    pub fn matches(&self, value: &XmlValue, seq_type: &SequenceType) -> bool {
        match &seq_type.item_type {
            ItemType::AnyItem => true,
            ItemType::AtomicType(XmlTypeCode::AnyAtomicType) => value.is_atomic(),
            ItemType::AtomicType(code) => {
                if value.type_code == *code {
                    return true;
                }
                // Check derivation
                derives_from(value.type_code, *code)
            }
            ItemType::AnyNode => false, // XmlValue doesn't hold nodes
            _ => false,
        }
    }

    /// Apply type promotion for numeric operations
    ///
    /// Returns the promoted type for two operands in an arithmetic operation.
    pub fn promote_numeric(&self, left: XmlTypeCode, right: XmlTypeCode) -> Option<XmlTypeCode> {
        if !left.is_numeric() || !right.is_numeric() {
            return None;
        }

        // Promotion rules:
        // - If either is double, result is double
        // - If either is float, result is float
        // - If either is decimal, result is decimal
        // - Otherwise result is integer
        if left == XmlTypeCode::Double || right == XmlTypeCode::Double {
            Some(XmlTypeCode::Double)
        } else if left == XmlTypeCode::Float || right == XmlTypeCode::Float {
            Some(XmlTypeCode::Float)
        } else if left == XmlTypeCode::Decimal || right == XmlTypeCode::Decimal {
            Some(XmlTypeCode::Decimal)
        } else {
            Some(XmlTypeCode::Integer)
        }
    }

    /// Promote a value to the target numeric type
    pub fn promote_to(&self, value: &XmlValue, target: XmlTypeCode) -> ConversionResult<XmlValue> {
        if value.type_code == target {
            return Ok(value.clone());
        }

        match target {
            XmlTypeCode::Double => {
                let d = self.to_double(value)?;
                Ok(XmlValue::double(d))
            }
            XmlTypeCode::Float => {
                let d = self.to_double(value)?;
                Ok(XmlValue::float(d as f32))
            }
            XmlTypeCode::Decimal => {
                let d = self.to_decimal(value)?;
                Ok(XmlValue::decimal(d))
            }
            XmlTypeCode::Integer => {
                let i = self.to_integer(value)?;
                Ok(XmlValue::integer(i))
            }
            _ => Err(ConversionError::TypeMismatch {
                from: value.type_code,
                to: target,
            }),
        }
    }

    /// Perform the actual cast operation
    fn perform_cast(&self, value: &XmlValue, target: XmlTypeCode) -> ConversionResult<XmlValue> {
        let str_val = value.to_string_value();

        match target {
            // Numeric targets - use conversion methods for better precision
            XmlTypeCode::Double => {
                let d = self.to_double(value)?;
                Ok(XmlValue::double(d))
            }
            XmlTypeCode::Float => {
                let d = self.to_double(value)?;
                Ok(XmlValue::float(d as f32))
            }
            XmlTypeCode::Decimal => {
                let d = self.to_decimal(value)?;
                Ok(XmlValue::decimal(d))
            }
            XmlTypeCode::Integer => {
                let i = self.to_integer(value)?;
                Ok(XmlValue::integer(i))
            }
            XmlTypeCode::Boolean => {
                let b = self.to_boolean(value)?;
                Ok(XmlValue::boolean(b))
            }
            XmlTypeCode::String => {
                Ok(XmlValue::string(str_val))
            }
            XmlTypeCode::UntypedAtomic => {
                Ok(XmlValue::untyped(str_val))
            }
            // For other types, use the validator
            _ => {
                self.validators.validate(target, &str_val)
                    .map_err(ConversionError::from)
            }
        }
    }
}

impl Default for TypeConverter {
    fn default() -> Self {
        Self::new()
    }
}

/// Check if casting from one type to another is allowed
fn can_cast(from: XmlTypeCode, to: XmlTypeCode) -> bool {
    // Same type always allowed
    if from == to {
        return true;
    }

    // Get primitive types
    let from_prim = PrimitiveTypeCode::from_type_code(from);
    let to_prim = PrimitiveTypeCode::from_type_code(to);

    match (from_prim, to_prim) {
        // String can cast to anything (via parsing)
        (Some(PrimitiveTypeCode::String), _) => true,

        // Anything can cast to string
        (_, Some(PrimitiveTypeCode::String)) => true,

        // Numeric types can cast to each other
        (Some(p1), Some(p2)) if p1.is_numeric() && p2.is_numeric() => true,

        // Boolean can cast to numeric and vice versa
        (Some(PrimitiveTypeCode::Boolean), Some(p)) if p.is_numeric() => true,
        (Some(p), Some(PrimitiveTypeCode::Boolean)) if p.is_numeric() => true,

        // Date/time types have limited casting
        (Some(PrimitiveTypeCode::DateTime), Some(PrimitiveTypeCode::Date)) => true,
        (Some(PrimitiveTypeCode::DateTime), Some(PrimitiveTypeCode::Time)) => true,
        (Some(PrimitiveTypeCode::Date), Some(PrimitiveTypeCode::DateTime)) => true,

        // Duration subtypes
        (Some(PrimitiveTypeCode::Duration), Some(PrimitiveTypeCode::Duration)) => true,

        // Binary types can cast to each other
        (Some(PrimitiveTypeCode::HexBinary), Some(PrimitiveTypeCode::Base64Binary)) => true,
        (Some(PrimitiveTypeCode::Base64Binary), Some(PrimitiveTypeCode::HexBinary)) => true,

        // UntypedAtomic can cast to anything
        _ if from == XmlTypeCode::UntypedAtomic => true,

        // Anything can cast to untypedAtomic
        _ if to == XmlTypeCode::UntypedAtomic => true,

        // Derivation within same primitive
        _ if from_prim == to_prim => true,

        _ => false,
    }
}

/// Check if a type derives from another (for type matching)
#[cfg(feature = "xsd11")]
fn derives_from(derived: XmlTypeCode, base: XmlTypeCode) -> bool {
    if derived == base {
        return true;
    }

    // Check derivation hierarchy
    match base {
        XmlTypeCode::AnySimpleType => true,
        XmlTypeCode::AnyAtomicType => {
            derived != XmlTypeCode::AnySimpleType && derived != XmlTypeCode::AnyType
        }

        // String hierarchy
        XmlTypeCode::String => matches!(
            derived,
            XmlTypeCode::NormalizedString
                | XmlTypeCode::Token
                | XmlTypeCode::Language
                | XmlTypeCode::NmToken
                | XmlTypeCode::Name
                | XmlTypeCode::NCName
                | XmlTypeCode::Id
                | XmlTypeCode::IdRef
                | XmlTypeCode::Entity
        ),
        XmlTypeCode::NormalizedString => matches!(
            derived,
            XmlTypeCode::Token
                | XmlTypeCode::Language
                | XmlTypeCode::NmToken
                | XmlTypeCode::Name
                | XmlTypeCode::NCName
                | XmlTypeCode::Id
                | XmlTypeCode::IdRef
                | XmlTypeCode::Entity
        ),
        XmlTypeCode::Token => matches!(
            derived,
            XmlTypeCode::Language
                | XmlTypeCode::NmToken
                | XmlTypeCode::Name
                | XmlTypeCode::NCName
                | XmlTypeCode::Id
                | XmlTypeCode::IdRef
                | XmlTypeCode::Entity
        ),
        XmlTypeCode::Name => matches!(
            derived,
            XmlTypeCode::NCName | XmlTypeCode::Id | XmlTypeCode::IdRef | XmlTypeCode::Entity
        ),
        XmlTypeCode::NCName => matches!(
            derived,
            XmlTypeCode::Id | XmlTypeCode::IdRef | XmlTypeCode::Entity
        ),

        // Numeric hierarchy
        XmlTypeCode::Decimal => matches!(
            derived,
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
        ),
        XmlTypeCode::Integer => matches!(
            derived,
            XmlTypeCode::NonPositiveInteger
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
        ),
        XmlTypeCode::Long => matches!(
            derived,
            XmlTypeCode::Int | XmlTypeCode::Short | XmlTypeCode::Byte
        ),
        XmlTypeCode::Int => matches!(derived, XmlTypeCode::Short | XmlTypeCode::Byte),
        XmlTypeCode::Short => matches!(derived, XmlTypeCode::Byte),
        XmlTypeCode::NonNegativeInteger => matches!(
            derived,
            XmlTypeCode::UnsignedLong
                | XmlTypeCode::UnsignedInt
                | XmlTypeCode::UnsignedShort
                | XmlTypeCode::UnsignedByte
                | XmlTypeCode::PositiveInteger
        ),
        XmlTypeCode::UnsignedLong => matches!(
            derived,
            XmlTypeCode::UnsignedInt | XmlTypeCode::UnsignedShort | XmlTypeCode::UnsignedByte
        ),
        XmlTypeCode::UnsignedInt => {
            matches!(derived, XmlTypeCode::UnsignedShort | XmlTypeCode::UnsignedByte)
        }
        XmlTypeCode::UnsignedShort => matches!(derived, XmlTypeCode::UnsignedByte),
        XmlTypeCode::NonPositiveInteger => matches!(derived, XmlTypeCode::NegativeInteger),

        // Duration hierarchy
        XmlTypeCode::Duration => {
            matches!(derived, XmlTypeCode::YearMonthDuration | XmlTypeCode::DayTimeDuration)
        }

        // DateTime hierarchy
        XmlTypeCode::DateTime => matches!(derived, XmlTypeCode::DateTimeStamp),

        _ => false,
    }
}

/// Convert a Rust value to XmlValue
pub trait IntoXmlValue {
    fn into_xml_value(self) -> XmlValue;
}

impl IntoXmlValue for bool {
    fn into_xml_value(self) -> XmlValue {
        XmlValue::boolean(self)
    }
}

impl IntoXmlValue for i32 {
    fn into_xml_value(self) -> XmlValue {
        XmlValue::integer(BigInt::from(self))
    }
}

impl IntoXmlValue for i64 {
    fn into_xml_value(self) -> XmlValue {
        XmlValue::integer(BigInt::from(self))
    }
}

impl IntoXmlValue for f64 {
    fn into_xml_value(self) -> XmlValue {
        XmlValue::double(self)
    }
}

impl IntoXmlValue for f32 {
    fn into_xml_value(self) -> XmlValue {
        XmlValue::float(self)
    }
}

impl IntoXmlValue for Decimal {
    fn into_xml_value(self) -> XmlValue {
        XmlValue::decimal(self)
    }
}

impl IntoXmlValue for BigInt {
    fn into_xml_value(self) -> XmlValue {
        XmlValue::integer(self)
    }
}

impl IntoXmlValue for String {
    fn into_xml_value(self) -> XmlValue {
        XmlValue::string(self)
    }
}

impl IntoXmlValue for &str {
    fn into_xml_value(self) -> XmlValue {
        XmlValue::string(self)
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_to_boolean() {
        let converter = TypeConverter::new();

        assert!(converter.to_boolean(&XmlValue::boolean(true)).unwrap());
        assert!(!converter.to_boolean(&XmlValue::boolean(false)).unwrap());
        assert!(converter.to_boolean(&XmlValue::string("hello")).unwrap());
        assert!(!converter.to_boolean(&XmlValue::string("")).unwrap());
        assert!(converter.to_boolean(&XmlValue::integer(BigInt::from(1))).unwrap());
        assert!(!converter.to_boolean(&XmlValue::integer(BigInt::from(0))).unwrap());
        assert!(converter.to_boolean(&XmlValue::double(1.5)).unwrap());
        assert!(!converter.to_boolean(&XmlValue::double(0.0)).unwrap());
        assert!(!converter.to_boolean(&XmlValue::double(f64::NAN)).unwrap());
    }

    #[test]
    fn test_to_double() {
        let converter = TypeConverter::new();

        assert_eq!(converter.to_double(&XmlValue::double(2.5)).unwrap(), 2.5);
        assert_eq!(converter.to_double(&XmlValue::float(2.5)).unwrap(), 2.5);
        assert_eq!(converter.to_double(&XmlValue::integer(BigInt::from(42))).unwrap(), 42.0);
        assert_eq!(converter.to_double(&XmlValue::decimal(Decimal::new(123, 1))).unwrap(), 12.3);
        assert_eq!(converter.to_double(&XmlValue::boolean(true)).unwrap(), 1.0);
        assert_eq!(converter.to_double(&XmlValue::boolean(false)).unwrap(), 0.0);
    }

    #[test]
    fn test_to_integer() {
        let converter = TypeConverter::new();

        assert_eq!(
            converter.to_integer(&XmlValue::integer(BigInt::from(42))).unwrap(),
            BigInt::from(42)
        );
        assert_eq!(
            converter.to_integer(&XmlValue::double(3.7)).unwrap(),
            BigInt::from(3) // Truncated
        );
        assert_eq!(
            converter.to_integer(&XmlValue::decimal(Decimal::new(99, 1))).unwrap(),
            BigInt::from(9) // Truncated from 9.9
        );
    }

    #[test]
    fn test_cast_string_to_integer() {
        let converter = TypeConverter::new();
        let str_val = XmlValue::untyped("42");

        let result = converter.cast(&str_val, XmlTypeCode::Integer).unwrap();
        assert_eq!(result.type_code, XmlTypeCode::Integer);
        assert_eq!(result.as_integer(), Some(&BigInt::from(42)));
    }

    #[test]
    fn test_cast_numeric_promotion() {
        let converter = TypeConverter::new();

        // Integer to double
        let int_val = XmlValue::integer(BigInt::from(42));
        let result = converter.cast(&int_val, XmlTypeCode::Double).unwrap();
        assert_eq!(result.as_double(), Some(42.0));

        // Double to string
        let dbl_val = XmlValue::double(2.5);
        let result = converter.cast(&dbl_val, XmlTypeCode::String).unwrap();
        assert!(result.to_string_value().starts_with("2.5"));
    }

    #[test]
    fn test_promote_numeric() {
        let converter = TypeConverter::new();

        // Integer + Integer = Integer
        assert_eq!(
            converter.promote_numeric(XmlTypeCode::Integer, XmlTypeCode::Integer),
            Some(XmlTypeCode::Integer)
        );

        // Integer + Decimal = Decimal
        assert_eq!(
            converter.promote_numeric(XmlTypeCode::Integer, XmlTypeCode::Decimal),
            Some(XmlTypeCode::Decimal)
        );

        // Decimal + Float = Float
        assert_eq!(
            converter.promote_numeric(XmlTypeCode::Decimal, XmlTypeCode::Float),
            Some(XmlTypeCode::Float)
        );

        // Float + Double = Double
        assert_eq!(
            converter.promote_numeric(XmlTypeCode::Float, XmlTypeCode::Double),
            Some(XmlTypeCode::Double)
        );

        // String + Integer = None (not numeric)
        assert_eq!(
            converter.promote_numeric(XmlTypeCode::String, XmlTypeCode::Integer),
            None
        );
    }

    #[cfg(feature = "xsd11")]
    #[test]
    fn test_derives_from() {
        // Integer hierarchy
        assert!(derives_from(XmlTypeCode::Integer, XmlTypeCode::Decimal));
        assert!(derives_from(XmlTypeCode::Long, XmlTypeCode::Integer));
        assert!(derives_from(XmlTypeCode::Int, XmlTypeCode::Long));
        assert!(derives_from(XmlTypeCode::Short, XmlTypeCode::Int));
        assert!(derives_from(XmlTypeCode::Byte, XmlTypeCode::Short));

        // String hierarchy
        assert!(derives_from(XmlTypeCode::NormalizedString, XmlTypeCode::String));
        assert!(derives_from(XmlTypeCode::Token, XmlTypeCode::NormalizedString));
        assert!(derives_from(XmlTypeCode::NCName, XmlTypeCode::Name));

        // Duration hierarchy
        assert!(derives_from(XmlTypeCode::YearMonthDuration, XmlTypeCode::Duration));
        assert!(derives_from(XmlTypeCode::DayTimeDuration, XmlTypeCode::Duration));

        // Negative cases
        assert!(!derives_from(XmlTypeCode::String, XmlTypeCode::Integer));
        assert!(!derives_from(XmlTypeCode::Decimal, XmlTypeCode::Integer));
    }

    #[test]
    fn test_can_cast() {
        // Same type
        assert!(can_cast(XmlTypeCode::String, XmlTypeCode::String));

        // String to/from anything
        assert!(can_cast(XmlTypeCode::String, XmlTypeCode::Integer));
        assert!(can_cast(XmlTypeCode::Integer, XmlTypeCode::String));

        // Numeric conversions
        assert!(can_cast(XmlTypeCode::Integer, XmlTypeCode::Double));
        assert!(can_cast(XmlTypeCode::Decimal, XmlTypeCode::Float));

        // Boolean and numeric
        assert!(can_cast(XmlTypeCode::Boolean, XmlTypeCode::Integer));
        assert!(can_cast(XmlTypeCode::Double, XmlTypeCode::Boolean));

        // UntypedAtomic
        assert!(can_cast(XmlTypeCode::UntypedAtomic, XmlTypeCode::Date));
        assert!(can_cast(XmlTypeCode::DateTime, XmlTypeCode::UntypedAtomic));
    }

    #[test]
    fn test_into_xml_value() {
        assert_eq!(true.into_xml_value().as_boolean(), Some(true));
        assert_eq!(42i32.into_xml_value().as_integer(), Some(&BigInt::from(42)));
        assert_eq!(2.5f64.into_xml_value().as_double(), Some(2.5));
        assert_eq!("hello".into_xml_value().as_string(), Some("hello"));
    }
}
