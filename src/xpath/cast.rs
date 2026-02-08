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
use crate::types::value::{
    XmlAtomicValue, XmlValue, XmlValueKind,
    DateTimeValue, DateValue, TimeValue,
    GYearMonthValue, GYearValue, GMonthDayValue, GDayValue, GMonthValue,
    DurationValue, YearMonthDurationValue, DayTimeDurationValue,
};
use crate::types::{XmlTypeCode, VALIDATOR_REGISTRY};
use crate::xpath::ast::OccurrenceIndicator;
use super::error::XPathError;

/// Check if a cast from `source` to `target` is allowed by the XPath 2.0 casting table.
///
/// Implements the casting rules from XPath Functions and Operators Section 17.1.
/// Returns `true` if the cast is permitted, `false` if it should raise XPTY0004.
fn can_cast(source: XmlTypeCode, target: XmlTypeCode) -> bool {
    // Same type is always allowed
    if source == target {
        return true;
    }

    // Casting to NOTATION is never allowed
    if target == XmlTypeCode::Notation {
        return false;
    }

    // untypedAtomic and string can cast to anything (except NOTATION, handled above)
    if source == XmlTypeCode::UntypedAtomic || source == XmlTypeCode::String
        || source.is_string_derived()
    {
        return true;
    }

    // Determine the effective source category (map integer subtypes to numeric)
    let source_numeric = source.is_numeric();
    let source_boolean = source == XmlTypeCode::Boolean;
    let source_duration = matches!(
        source,
        XmlTypeCode::Duration | XmlTypeCode::YearMonthDuration | XmlTypeCode::DayTimeDuration
    );
    let source_datetime = source == XmlTypeCode::DateTime || source == XmlTypeCode::DateTimeStamp;
    let source_date = source == XmlTypeCode::Date;
    let source_time = source == XmlTypeCode::Time;
    let source_gyearmonth = source == XmlTypeCode::GYearMonth;
    let source_gyear = source == XmlTypeCode::GYear;
    let source_gmonthday = source == XmlTypeCode::GMonthDay;
    let source_gday = source == XmlTypeCode::GDay;
    let source_gmonth = source == XmlTypeCode::GMonth;
    let source_binary = matches!(
        source,
        XmlTypeCode::Base64Binary | XmlTypeCode::HexBinary
    );
    let source_anyuri = source == XmlTypeCode::AnyUri;

    let target_ua_or_string = target == XmlTypeCode::UntypedAtomic
        || target == XmlTypeCode::String
        || target.is_string_derived();
    let target_numeric = target.is_numeric() || target == XmlTypeCode::Boolean;

    // numeric types → uA, string, float, double, decimal, integer (subtypes), boolean
    if source_numeric {
        return target_ua_or_string || target_numeric;
    }

    // boolean → uA, string, float, double, decimal, integer (subtypes), boolean
    if source_boolean {
        return target_ua_or_string || target_numeric;
    }

    // duration types → uA, string, duration, yearMonthDuration, dayTimeDuration
    if source_duration {
        return target_ua_or_string
            || matches!(
                target,
                XmlTypeCode::Duration
                    | XmlTypeCode::YearMonthDuration
                    | XmlTypeCode::DayTimeDuration
            );
    }

    // dateTime/dateTimeStamp → uA, string, dateTime, dateTimeStamp, date, time, gYearMonth, gYear, gMonthDay, gDay, gMonth
    if source_datetime {
        return target_ua_or_string
            || matches!(
                target,
                XmlTypeCode::DateTime
                    | XmlTypeCode::DateTimeStamp
                    | XmlTypeCode::Date
                    | XmlTypeCode::Time
                    | XmlTypeCode::GYearMonth
                    | XmlTypeCode::GYear
                    | XmlTypeCode::GMonthDay
                    | XmlTypeCode::GDay
                    | XmlTypeCode::GMonth
            );
    }

    // date → uA, string, dateTime, date, gYearMonth, gYear, gMonthDay, gDay, gMonth
    if source_date {
        return target_ua_or_string
            || matches!(
                target,
                XmlTypeCode::DateTime
                    | XmlTypeCode::Date
                    | XmlTypeCode::GYearMonth
                    | XmlTypeCode::GYear
                    | XmlTypeCode::GMonthDay
                    | XmlTypeCode::GDay
                    | XmlTypeCode::GMonth
            );
    }

    // time → uA, string, time
    if source_time {
        return target_ua_or_string || target == XmlTypeCode::Time;
    }

    // gYearMonth → uA, string, gYearMonth
    if source_gyearmonth {
        return target_ua_or_string || target == XmlTypeCode::GYearMonth;
    }

    // gYear → uA, string, gYear
    if source_gyear {
        return target_ua_or_string || target == XmlTypeCode::GYear;
    }

    // gMonthDay → uA, string, gMonthDay
    if source_gmonthday {
        return target_ua_or_string || target == XmlTypeCode::GMonthDay;
    }

    // gDay → uA, string, gDay
    if source_gday {
        return target_ua_or_string || target == XmlTypeCode::GDay;
    }

    // gMonth → uA, string, gMonth
    if source_gmonth {
        return target_ua_or_string || target == XmlTypeCode::GMonth;
    }

    // base64Binary / hexBinary → uA, string, base64Binary, hexBinary
    if source_binary {
        return target_ua_or_string
            || matches!(
                target,
                XmlTypeCode::Base64Binary | XmlTypeCode::HexBinary
            );
    }

    // anyURI → uA, string, anyURI
    if source_anyuri {
        return target_ua_or_string || target == XmlTypeCode::AnyUri;
    }

    false
}

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

    // Check the XPath 2.0 casting table
    if !can_cast(value.type_code, target_type) {
        return Err(XPathError::type_mismatch(
            format!("{:?}", value.type_code),
            format!("{:?}", target_type),
        ));
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

        // Integer-derived types: cast to integer first, then validate range
        target if is_integer_derived(target) => cast_to_integer_subtype(value, target),

        // Date/time types: try direct cross-casting first, then parse via VALIDATOR_REGISTRY
        XmlTypeCode::DateTime
        | XmlTypeCode::Date
        | XmlTypeCode::Time
        | XmlTypeCode::Duration
        | XmlTypeCode::YearMonthDuration
        | XmlTypeCode::DayTimeDuration
        | XmlTypeCode::GYearMonth
        | XmlTypeCode::GYear
        | XmlTypeCode::GMonthDay
        | XmlTypeCode::GDay
        | XmlTypeCode::GMonth
        | XmlTypeCode::DateTimeStamp => {
            // Try direct cross-casting between date/time/duration types
            if let Some(result) = cast_datetime_cross(value, target_type) {
                return result;
            }
            let type_name = target_type.local_name().unwrap_or("unknown");
            VALIDATOR_REGISTRY
                .validate(target_type, string_val.trim())
                .map_err(|_| {
                    XPathError::invalid_cast_value(&string_val, format!("xs:{}", type_name))
                })
        }

        // Binary and URI types: try direct binary cross-casting, then parse via VALIDATOR_REGISTRY
        XmlTypeCode::AnyUri | XmlTypeCode::HexBinary | XmlTypeCode::Base64Binary => {
            // Direct binary cross-casting (base64Binary ↔ hexBinary)
            if let Some(result) = cast_binary_cross(value, target_type) {
                return result;
            }
            let type_name = target_type.local_name().unwrap_or("unknown");
            VALIDATOR_REGISTRY
                .validate(target_type, string_val.trim())
                .map_err(|_| {
                    XPathError::invalid_cast_value(&string_val, format!("xs:{}", type_name))
                })
        }

        // String-derived types: parse via VALIDATOR_REGISTRY
        XmlTypeCode::NormalizedString
        | XmlTypeCode::Token
        | XmlTypeCode::Language
        | XmlTypeCode::NmToken
        | XmlTypeCode::Name
        | XmlTypeCode::NCName
        | XmlTypeCode::Id
        | XmlTypeCode::IdRef
        | XmlTypeCode::Entity => {
            let type_name = target_type.local_name().unwrap_or("unknown");
            VALIDATOR_REGISTRY
                .validate(target_type, &string_val)
                .map_err(|_| {
                    XPathError::invalid_cast_value(&string_val, format!("xs:{}", type_name))
                })
        }

        // QName and NOTATION require special handling (not castable from string)
        XmlTypeCode::QName | XmlTypeCode::Notation => Err(XPathError::type_mismatch(
            format!("{:?}", value.type_code),
            format!("{:?}", target_type),
        )),

        // Unsupported cast
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
        XmlValueKind::Atomic(XmlAtomicValue::Integer(i)) => *i != BigInt::from(0),
        XmlValueKind::Atomic(XmlAtomicValue::Decimal(d)) => !d.is_zero(),
        XmlValueKind::Atomic(XmlAtomicValue::Float(f)) => *f != 0.0 && !f.is_nan(),
        XmlValueKind::Atomic(XmlAtomicValue::Double(d)) => *d != 0.0 && !d.is_nan(),
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
            if *f == 0.0 {
                Decimal::ZERO
            } else {
                Decimal::try_from(*f)
                    .map_err(|_| XPathError::invalid_cast_value(string_val, "xs:decimal"))?
            }
        }
        XmlValueKind::Atomic(XmlAtomicValue::Double(d)) => {
            if d.is_nan() || d.is_infinite() {
                return Err(XPathError::invalid_cast_value(string_val, "xs:decimal"));
            }
            if *d == 0.0 {
                Decimal::ZERO
            } else {
                Decimal::try_from(*d)
                    .map_err(|_| XPathError::invalid_cast_value(string_val, "xs:decimal"))?
            }
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
            let truncated = f.trunc() as f64;
            // Use string round-trip to handle values outside i64 range
            let s = format!("{:.0}", truncated);
            s.parse::<BigInt>()
                .map_err(|_| XPathError::FOCA0003 {
                    message: format!("Value {} is too large for xs:integer", string_val),
                })?
        }
        XmlValueKind::Atomic(XmlAtomicValue::Double(d)) => {
            if d.is_nan() || d.is_infinite() {
                return Err(XPathError::invalid_cast_value(string_val, "xs:integer"));
            }
            // Use string round-trip to handle values outside i64 range
            let s = format!("{:.0}", d.trunc());
            s.parse::<BigInt>()
                .map_err(|_| XPathError::FOCA0003 {
                    message: format!("Value {} is too large for xs:integer", string_val),
                })?
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

/// Cross-cast between date/time/duration types by directly extracting fields.
/// Returns None if no direct cross-cast applies (fall through to string parsing).
fn cast_datetime_cross(
    value: &XmlValue,
    target: XmlTypeCode,
) -> Option<Result<XmlValue, XPathError>> {
    match (&value.value, target) {
        // DateTime → Date
        (XmlValueKind::Atomic(XmlAtomicValue::DateTime(dt)), XmlTypeCode::Date) => {
            Some(Ok(XmlValue::new(
                XmlTypeCode::Date,
                XmlValueKind::Atomic(XmlAtomicValue::Date(DateValue {
                    year: dt.year,
                    month: dt.month,
                    day: dt.day,
                    timezone: dt.timezone,
                })),
            )))
        }
        // DateTime → Time
        (XmlValueKind::Atomic(XmlAtomicValue::DateTime(dt)), XmlTypeCode::Time) => {
            Some(Ok(XmlValue::new(
                XmlTypeCode::Time,
                XmlValueKind::Atomic(XmlAtomicValue::Time(TimeValue {
                    hour: dt.hour,
                    minute: dt.minute,
                    second: dt.second,
                    timezone: dt.timezone,
                })),
            )))
        }
        // Date → DateTime (time defaults to 00:00:00)
        (XmlValueKind::Atomic(XmlAtomicValue::Date(d)), XmlTypeCode::DateTime) => {
            Some(Ok(XmlValue::new(
                XmlTypeCode::DateTime,
                XmlValueKind::Atomic(XmlAtomicValue::DateTime(DateTimeValue {
                    year: d.year,
                    month: d.month,
                    day: d.day,
                    hour: 0,
                    minute: 0,
                    second: Decimal::ZERO,
                    timezone: d.timezone,
                })),
            )))
        }
        // DateTime → gYearMonth
        (XmlValueKind::Atomic(XmlAtomicValue::DateTime(dt)), XmlTypeCode::GYearMonth) => {
            Some(Ok(XmlValue::new(
                XmlTypeCode::GYearMonth,
                XmlValueKind::Atomic(XmlAtomicValue::GYearMonth(GYearMonthValue {
                    year: dt.year,
                    month: dt.month,
                    timezone: dt.timezone,
                })),
            )))
        }
        // DateTime → gYear
        (XmlValueKind::Atomic(XmlAtomicValue::DateTime(dt)), XmlTypeCode::GYear) => {
            Some(Ok(XmlValue::new(
                XmlTypeCode::GYear,
                XmlValueKind::Atomic(XmlAtomicValue::GYear(GYearValue {
                    year: dt.year,
                    timezone: dt.timezone,
                })),
            )))
        }
        // DateTime → gMonthDay
        (XmlValueKind::Atomic(XmlAtomicValue::DateTime(dt)), XmlTypeCode::GMonthDay) => {
            Some(Ok(XmlValue::new(
                XmlTypeCode::GMonthDay,
                XmlValueKind::Atomic(XmlAtomicValue::GMonthDay(GMonthDayValue {
                    month: dt.month,
                    day: dt.day,
                    timezone: dt.timezone,
                })),
            )))
        }
        // DateTime → gDay
        (XmlValueKind::Atomic(XmlAtomicValue::DateTime(dt)), XmlTypeCode::GDay) => {
            Some(Ok(XmlValue::new(
                XmlTypeCode::GDay,
                XmlValueKind::Atomic(XmlAtomicValue::GDay(GDayValue {
                    day: dt.day,
                    timezone: dt.timezone,
                })),
            )))
        }
        // DateTime → gMonth
        (XmlValueKind::Atomic(XmlAtomicValue::DateTime(dt)), XmlTypeCode::GMonth) => {
            Some(Ok(XmlValue::new(
                XmlTypeCode::GMonth,
                XmlValueKind::Atomic(XmlAtomicValue::GMonth(GMonthValue {
                    month: dt.month,
                    timezone: dt.timezone,
                })),
            )))
        }
        // Date → gYearMonth
        (XmlValueKind::Atomic(XmlAtomicValue::Date(d)), XmlTypeCode::GYearMonth) => {
            Some(Ok(XmlValue::new(
                XmlTypeCode::GYearMonth,
                XmlValueKind::Atomic(XmlAtomicValue::GYearMonth(GYearMonthValue {
                    year: d.year,
                    month: d.month,
                    timezone: d.timezone,
                })),
            )))
        }
        // Date → gYear
        (XmlValueKind::Atomic(XmlAtomicValue::Date(d)), XmlTypeCode::GYear) => {
            Some(Ok(XmlValue::new(
                XmlTypeCode::GYear,
                XmlValueKind::Atomic(XmlAtomicValue::GYear(GYearValue {
                    year: d.year,
                    timezone: d.timezone,
                })),
            )))
        }
        // Date → gMonthDay
        (XmlValueKind::Atomic(XmlAtomicValue::Date(d)), XmlTypeCode::GMonthDay) => {
            Some(Ok(XmlValue::new(
                XmlTypeCode::GMonthDay,
                XmlValueKind::Atomic(XmlAtomicValue::GMonthDay(GMonthDayValue {
                    month: d.month,
                    day: d.day,
                    timezone: d.timezone,
                })),
            )))
        }
        // Date → gDay
        (XmlValueKind::Atomic(XmlAtomicValue::Date(d)), XmlTypeCode::GDay) => {
            Some(Ok(XmlValue::new(
                XmlTypeCode::GDay,
                XmlValueKind::Atomic(XmlAtomicValue::GDay(GDayValue {
                    day: d.day,
                    timezone: d.timezone,
                })),
            )))
        }
        // Date → gMonth
        (XmlValueKind::Atomic(XmlAtomicValue::Date(d)), XmlTypeCode::GMonth) => {
            Some(Ok(XmlValue::new(
                XmlTypeCode::GMonth,
                XmlValueKind::Atomic(XmlAtomicValue::GMonth(GMonthValue {
                    month: d.month,
                    timezone: d.timezone,
                })),
            )))
        }
        // Duration → YearMonthDuration (extract year/month parts)
        (XmlValueKind::Atomic(XmlAtomicValue::Duration(d)), XmlTypeCode::YearMonthDuration) => {
            Some(Ok(XmlValue::new(
                XmlTypeCode::YearMonthDuration,
                XmlValueKind::Atomic(XmlAtomicValue::YearMonthDuration(YearMonthDurationValue {
                    negative: d.negative,
                    years: d.years,
                    months: d.months,
                })),
            )))
        }
        // Duration → DayTimeDuration (extract day/time parts)
        (XmlValueKind::Atomic(XmlAtomicValue::Duration(d)), XmlTypeCode::DayTimeDuration) => {
            Some(Ok(XmlValue::new(
                XmlTypeCode::DayTimeDuration,
                XmlValueKind::Atomic(XmlAtomicValue::DayTimeDuration(DayTimeDurationValue {
                    negative: d.negative,
                    days: d.days,
                    hours: d.hours,
                    minutes: d.minutes,
                    seconds: d.seconds,
                })),
            )))
        }
        // YearMonthDuration → Duration
        (XmlValueKind::Atomic(XmlAtomicValue::YearMonthDuration(ym)), XmlTypeCode::Duration) => {
            Some(Ok(XmlValue::new(
                XmlTypeCode::Duration,
                XmlValueKind::Atomic(XmlAtomicValue::Duration(DurationValue {
                    negative: ym.negative,
                    years: ym.years,
                    months: ym.months,
                    days: 0,
                    hours: 0,
                    minutes: 0,
                    seconds: Decimal::ZERO,
                })),
            )))
        }
        // DayTimeDuration → Duration
        (XmlValueKind::Atomic(XmlAtomicValue::DayTimeDuration(dt)), XmlTypeCode::Duration) => {
            Some(Ok(XmlValue::new(
                XmlTypeCode::Duration,
                XmlValueKind::Atomic(XmlAtomicValue::Duration(DurationValue {
                    negative: dt.negative,
                    years: 0,
                    months: 0,
                    days: dt.days,
                    hours: dt.hours,
                    minutes: dt.minutes,
                    seconds: dt.seconds,
                })),
            )))
        }
        // YearMonthDuration → DayTimeDuration: yields zero per XPath 2.0 F&O §17.1.5.
        // Cast goes through xs:duration as intermediate; yearMonthDuration has no day/time
        // components, so extracting the day-time part always produces PT0S.
        (XmlValueKind::Atomic(XmlAtomicValue::YearMonthDuration(_)), XmlTypeCode::DayTimeDuration) => {
            Some(Ok(XmlValue::new(
                XmlTypeCode::DayTimeDuration,
                XmlValueKind::Atomic(XmlAtomicValue::DayTimeDuration(DayTimeDurationValue {
                    negative: false,
                    days: 0,
                    hours: 0,
                    minutes: 0,
                    seconds: Decimal::ZERO,
                })),
            )))
        }
        // DayTimeDuration → YearMonthDuration: yields zero per XPath 2.0 F&O §17.1.5.
        // Cast goes through xs:duration as intermediate; dayTimeDuration has no year/month
        // components, so extracting the year-month part always produces P0M.
        (XmlValueKind::Atomic(XmlAtomicValue::DayTimeDuration(_)), XmlTypeCode::YearMonthDuration) => {
            Some(Ok(XmlValue::new(
                XmlTypeCode::YearMonthDuration,
                XmlValueKind::Atomic(XmlAtomicValue::YearMonthDuration(YearMonthDurationValue {
                    negative: false,
                    years: 0,
                    months: 0,
                })),
            )))
        }
        _ => None,
    }
}

/// Cross-cast between binary types (base64Binary ↔ hexBinary).
fn cast_binary_cross(
    value: &XmlValue,
    target: XmlTypeCode,
) -> Option<Result<XmlValue, XPathError>> {
    match (&value.value, target) {
        (XmlValueKind::Atomic(XmlAtomicValue::Base64Binary(bytes)), XmlTypeCode::HexBinary) => {
            Some(Ok(XmlValue::new(
                XmlTypeCode::HexBinary,
                XmlValueKind::Atomic(XmlAtomicValue::HexBinary(bytes.clone())),
            )))
        }
        (XmlValueKind::Atomic(XmlAtomicValue::HexBinary(bytes)), XmlTypeCode::Base64Binary) => {
            Some(Ok(XmlValue::new(
                XmlTypeCode::Base64Binary,
                XmlValueKind::Atomic(XmlAtomicValue::Base64Binary(bytes.clone())),
            )))
        }
        _ => None,
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

    #[test]
    fn test_cast_numeric_to_boolean() {
        // Integer non-zero → true
        assert_eq!(
            cast_to(&XmlValue::integer(BigInt::from(10)), XmlTypeCode::Boolean)
                .unwrap().as_boolean(), Some(true));
        // Integer zero → false
        assert_eq!(
            cast_to(&XmlValue::integer(BigInt::from(0)), XmlTypeCode::Boolean)
                .unwrap().as_boolean(), Some(false));
        // Double NaN → false
        assert_eq!(
            cast_to(&XmlValue::double(f64::NAN), XmlTypeCode::Boolean)
                .unwrap().as_boolean(), Some(false));
        // Double -0 → false
        assert_eq!(
            cast_to(&XmlValue::double(-0.0), XmlTypeCode::Boolean)
                .unwrap().as_boolean(), Some(false));
        // Float non-zero → true
        assert_eq!(
            cast_to(&XmlValue::float(1.5), XmlTypeCode::Boolean)
                .unwrap().as_boolean(), Some(true));
        // Decimal → true
        assert_eq!(
            cast_to(&XmlValue::decimal(Decimal::new(-11234, 4)), XmlTypeCode::Boolean)
                .unwrap().as_boolean(), Some(true));
    }

    #[test]
    fn test_cast_datetime_to_date() {
        let dt = XmlValue::new(
            XmlTypeCode::DateTime,
            XmlValueKind::Atomic(XmlAtomicValue::DateTime(DateTimeValue {
                year: 1999, month: 5, day: 31,
                hour: 13, minute: 20, second: Decimal::ZERO,
                timezone: Some(crate::types::value::TimezoneOffset(-300)),
            })),
        );
        let result = cast_to(&dt, XmlTypeCode::Date).unwrap();
        assert_eq!(result.type_code, XmlTypeCode::Date);
        assert_eq!(result.to_string_value(), "1999-05-31-05:00");
    }

    #[test]
    fn test_cast_datetime_to_time() {
        let dt = XmlValue::new(
            XmlTypeCode::DateTime,
            XmlValueKind::Atomic(XmlAtomicValue::DateTime(DateTimeValue {
                year: 1999, month: 5, day: 31,
                hour: 13, minute: 20, second: Decimal::ZERO,
                timezone: Some(crate::types::value::TimezoneOffset(-300)),
            })),
        );
        let result = cast_to(&dt, XmlTypeCode::Time).unwrap();
        assert_eq!(result.type_code, XmlTypeCode::Time);
        assert_eq!(result.to_string_value(), "13:20:00-05:00");
    }

    #[test]
    fn test_cast_date_to_datetime() {
        let d = XmlValue::new(
            XmlTypeCode::Date,
            XmlValueKind::Atomic(XmlAtomicValue::Date(DateValue {
                year: 1999, month: 5, day: 31,
                timezone: Some(crate::types::value::TimezoneOffset::UTC),
            })),
        );
        let result = cast_to(&d, XmlTypeCode::DateTime).unwrap();
        assert_eq!(result.type_code, XmlTypeCode::DateTime);
        assert_eq!(result.to_string_value(), "1999-05-31T00:00:00Z");
    }

    #[test]
    fn test_cast_datetime_to_gyear() {
        let dt = XmlValue::new(
            XmlTypeCode::DateTime,
            XmlValueKind::Atomic(XmlAtomicValue::DateTime(DateTimeValue {
                year: 1999, month: 5, day: 31,
                hour: 13, minute: 20, second: Decimal::ZERO,
                timezone: None,
            })),
        );
        let result = cast_to(&dt, XmlTypeCode::GYear).unwrap();
        assert_eq!(result.to_string_value(), "1999");

        let result = cast_to(&dt, XmlTypeCode::GMonth).unwrap();
        assert_eq!(result.to_string_value(), "--05");

        let result = cast_to(&dt, XmlTypeCode::GDay).unwrap();
        assert_eq!(result.to_string_value(), "---31");

        let result = cast_to(&dt, XmlTypeCode::GMonthDay).unwrap();
        assert_eq!(result.to_string_value(), "--05-31");

        let result = cast_to(&dt, XmlTypeCode::GYearMonth).unwrap();
        assert_eq!(result.to_string_value(), "1999-05");
    }

    #[test]
    fn test_cast_duration_to_yearmonth() {
        let dur = XmlValue::new(
            XmlTypeCode::Duration,
            XmlValueKind::Atomic(XmlAtomicValue::Duration(DurationValue {
                negative: false, years: 1, months: 2,
                days: 3, hours: 10, minutes: 30, seconds: Decimal::new(23, 0),
            })),
        );
        let result = cast_to(&dur, XmlTypeCode::YearMonthDuration).unwrap();
        assert_eq!(result.to_string_value(), "P1Y2M");

        let result = cast_to(&dur, XmlTypeCode::DayTimeDuration).unwrap();
        assert_eq!(result.to_string_value(), "P3DT10H30M23S");
    }

    #[test]
    fn test_cast_binary_cross() {
        // base64Binary → hexBinary
        let b64 = XmlValue::new(
            XmlTypeCode::Base64Binary,
            XmlValueKind::Atomic(XmlAtomicValue::Base64Binary(vec![0xAB, 0xCD])),
        );
        let result = cast_to(&b64, XmlTypeCode::HexBinary).unwrap();
        assert_eq!(result.type_code, XmlTypeCode::HexBinary);
        assert_eq!(result.to_string_value(), "ABCD");

        // hexBinary → base64Binary
        let hex = XmlValue::new(
            XmlTypeCode::HexBinary,
            XmlValueKind::Atomic(XmlAtomicValue::HexBinary(vec![0xFF, 0x00])),
        );
        let result = cast_to(&hex, XmlTypeCode::Base64Binary).unwrap();
        assert_eq!(result.type_code, XmlTypeCode::Base64Binary);
    }

    #[test]
    fn test_cast_string_to_time_with_timezone() {
        let value = XmlValue::string("13:20:00-05:00");
        let result = cast_to(&value, XmlTypeCode::Time).unwrap();
        assert_eq!(result.type_code, XmlTypeCode::Time);
        assert_eq!(result.to_string_value(), "13:20:00-05:00");
    }

    #[test]
    fn test_cast_string_to_yearmonth_duration_zero() {
        let value = XmlValue::string("P0Y0M");
        let result = cast_to(&value, XmlTypeCode::YearMonthDuration).unwrap();
        assert_eq!(result.to_string_value(), "P0M");
    }
}
