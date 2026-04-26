//! XPath 2.0 aggregate functions.
//!
//! This module implements aggregate functions from the XPath 2.0 specification:
//! - fn:sum
//! - fn:avg
//! - fn:min
//! - fn:max

use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;

use crate::types::value::{XmlAtomicValue, XmlValue, XmlValueKind};
use crate::types::XmlTypeCode;
use crate::xpath::ast::BinaryOpKind;
use crate::xpath::context::DynamicContext;
use crate::xpath::error::XPathError;
use crate::xpath::operators::{eval_binary, value_gt, value_lt};
use crate::xpath::DomNavigator;

use super::{atomize_sequence, atomize_to_single_opt, XPathValue};

// ============================================================================
// fn:sum($arg as xs:anyAtomicType*, $zero as xs:anyAtomicType?) as xs:anyAtomicType?
// ============================================================================

/// Implements fn:sum - returns the sum of the values in the argument sequence.
///
/// If the sequence is empty, returns $zero (default: integer 0).
/// Supports numeric types and duration types.
pub fn sum<N: DomNavigator>(
    _context: &mut DynamicContext<'_, N>,
    mut args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if args.is_empty() || args.len() > 2 {
        return Err(XPathError::wrong_number_of_arguments("sum", 1, args.len()));
    }

    let seq = args.remove(0);
    let zero = if !args.is_empty() {
        atomize_to_single_opt(args.remove(0))?
    } else {
        None
    };

    // Atomize the sequence
    let values = atomize_sequence(seq)?;

    if values.is_empty() {
        // Return $zero if provided, otherwise integer 0
        return Ok(match zero {
            Some(z) => XPathValue::from_atomic(z),
            None => XPathValue::integer(0),
        });
    }

    // Accumulate the sum using operators::eval_binary
    let mut accumulator = promote_for_sum(&values[0])?;

    for value in values.iter().skip(1) {
        let promoted = promote_for_sum(value)?;
        accumulator = eval_binary(BinaryOpKind::Add, &accumulator, &promoted)?;
    }

    Ok(XPathValue::from_atomic(accumulator))
}

// ============================================================================
// fn:avg($arg as xs:anyAtomicType*) as xs:anyAtomicType?
// ============================================================================

/// Implements fn:avg - returns the average of the values in the argument sequence.
///
/// If the sequence is empty, returns the empty sequence.
/// Supports numeric types and duration types.
pub fn avg<N: DomNavigator>(
    _context: &mut DynamicContext<'_, N>,
    mut args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if args.len() != 1 {
        return Err(XPathError::wrong_number_of_arguments("avg", 1, args.len()));
    }

    let seq = args.remove(0);
    let values = atomize_sequence(seq)?;

    if values.is_empty() {
        return Ok(XPathValue::Empty);
    }

    let count = values.len();

    // Accumulate the sum using operators::eval_binary
    let mut accumulator = promote_for_sum(&values[0])?;

    for value in values.iter().skip(1) {
        let promoted = promote_for_sum(value)?;
        accumulator = eval_binary(BinaryOpKind::Add, &accumulator, &promoted)?;
    }

    // Divide by count
    let result = numeric_divide(&accumulator, count)?;
    Ok(XPathValue::from_atomic(result))
}

// ============================================================================
// fn:min($arg as xs:anyAtomicType*, $collation as xs:string?) as xs:anyAtomicType?
// ============================================================================

/// Implements fn:min - returns the minimum value in the argument sequence.
///
/// If the sequence is empty, returns the empty sequence.
/// Per XPath 2.0: If the converted sequence contains NaN, NaN is returned.
pub fn min<N: DomNavigator>(
    _context: &mut DynamicContext<'_, N>,
    mut args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if args.is_empty() || args.len() > 2 {
        return Err(XPathError::wrong_number_of_arguments("min", 1, args.len()));
    }

    let seq = args.remove(0);
    // Collation argument (arg 1) is ignored for now

    let values = atomize_sequence(seq)?;

    if values.is_empty() {
        return Ok(XPathValue::Empty);
    }

    // Promote all values first
    let mut promoted: Vec<XmlValue> = values
        .iter()
        .map(promote_for_comparison)
        .collect::<Result<Vec<_>, _>>()?;
    promote_to_common_numeric_type(&mut promoted);

    // Per XPath 2.0: If sequence contains NaN, return NaN
    if contains_nan(&promoted) {
        return Ok(XPathValue::double(f64::NAN));
    }

    let mut min_value = promoted[0].clone();

    for value in promoted.iter().skip(1) {
        // Use operators::value_lt for comparison
        if value_lt(value, &min_value)? {
            min_value = value.clone();
        }
    }

    Ok(XPathValue::from_atomic(min_value))
}

// ============================================================================
// fn:max($arg as xs:anyAtomicType*, $collation as xs:string?) as xs:anyAtomicType?
// ============================================================================

/// Implements fn:max - returns the maximum value in the argument sequence.
///
/// If the sequence is empty, returns the empty sequence.
/// Per XPath 2.0: If the converted sequence contains NaN, NaN is returned.
pub fn max<N: DomNavigator>(
    _context: &mut DynamicContext<'_, N>,
    mut args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if args.is_empty() || args.len() > 2 {
        return Err(XPathError::wrong_number_of_arguments("max", 1, args.len()));
    }

    let seq = args.remove(0);
    // Collation argument (arg 1) is ignored for now

    let values = atomize_sequence(seq)?;

    if values.is_empty() {
        return Ok(XPathValue::Empty);
    }

    // Promote all values first
    let mut promoted: Vec<XmlValue> = values
        .iter()
        .map(promote_for_comparison)
        .collect::<Result<Vec<_>, _>>()?;
    promote_to_common_numeric_type(&mut promoted);

    // Per XPath 2.0: If sequence contains NaN, return NaN
    if contains_nan(&promoted) {
        return Ok(XPathValue::double(f64::NAN));
    }

    let mut max_value = promoted[0].clone();

    for value in promoted.iter().skip(1) {
        // Use operators::value_gt for comparison
        if value_gt(value, &max_value)? {
            max_value = value.clone();
        }
    }

    Ok(XPathValue::from_atomic(max_value))
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Check if a type code is an integer-derived type.
fn is_integer_type(code: XmlTypeCode) -> bool {
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

/// Promote a value for sum/avg operations.
///
/// Integers are promoted to decimal for accumulation.
/// UntypedAtomic is promoted to double.
fn promote_for_sum(value: &XmlValue) -> Result<XmlValue, XPathError> {
    match value.type_code {
        XmlTypeCode::Double | XmlTypeCode::Float | XmlTypeCode::Decimal => Ok(value.clone()),
        XmlTypeCode::YearMonthDuration | XmlTypeCode::DayTimeDuration => Ok(value.clone()),
        code if is_integer_type(code) => {
            // Promote integers to decimal for accumulation
            let i = value.as_integer().ok_or_else(|| XPathError::FORG0006 {
                message: "Expected integer value".to_string(),
            })?;
            let d: Decimal = i.to_string().parse().map_err(|_| XPathError::FORG0006 {
                message: "Failed to convert integer to decimal".to_string(),
            })?;
            Ok(XmlValue::decimal(d))
        }
        XmlTypeCode::UntypedAtomic => {
            // Promote untyped to double - per XPath 2.0, throw FORG0001 for invalid values
            let s = value.to_string_value();
            let d: f64 = s.trim().parse().map_err(|_| XPathError::FORG0001 {
                value: s.clone(),
                target_type: "xs:double".to_string(),
            })?;
            Ok(XmlValue::double(d))
        }
        _ => Err(XPathError::FORG0006 {
            message: format!(
                "fn:sum/avg requires numeric or duration type, got {:?}",
                value.type_code
            ),
        }),
    }
}

/// Promote all numeric values to the common numeric type.
/// Per F&O §15.4.3/4: if any value is double, all become double; if any is float, all become float.
fn promote_to_common_numeric_type(values: &mut [XmlValue]) {
    let has_double = values.iter().any(|v| v.type_code == XmlTypeCode::Double);
    let has_float = !has_double && values.iter().any(|v| v.type_code == XmlTypeCode::Float);

    if has_double {
        for v in values.iter_mut() {
            if v.type_code != XmlTypeCode::Double && v.type_code.is_numeric() {
                if let Some(d) = v.as_double() {
                    *v = XmlValue::double(d);
                }
            }
        }
    } else if has_float {
        for v in values.iter_mut() {
            if v.type_code != XmlTypeCode::Float && v.type_code.is_numeric() {
                if let Some(d) = v.as_double() {
                    *v = XmlValue::float(d as f32);
                }
            }
        }
    }
}

/// Promote a value for comparison (min/max).
///
/// UntypedAtomic is promoted to double for numeric context, string otherwise.
fn promote_for_comparison(value: &XmlValue) -> Result<XmlValue, XPathError> {
    match value.type_code {
        XmlTypeCode::Double
        | XmlTypeCode::Float
        | XmlTypeCode::Decimal
        | XmlTypeCode::String
        | XmlTypeCode::DayTimeDuration
        | XmlTypeCode::YearMonthDuration => Ok(value.clone()),
        code if is_integer_type(code) => Ok(value.clone()),
        XmlTypeCode::UntypedAtomic => {
            // Treat as double for min/max - throw FORG0001 for invalid values
            let s = value.to_string_value();
            let d: f64 = s.trim().parse().map_err(|_| XPathError::FORG0001 {
                value: s.clone(),
                target_type: "xs:double".to_string(),
            })?;
            Ok(XmlValue::double(d))
        }
        XmlTypeCode::AnyUri => {
            // Per XPath 2.0: AnyUri promoted to string for comparison
            Ok(XmlValue::string(value.to_string_value()))
        }
        _ => Err(XPathError::FORG0006 {
            message: format!(
                "fn:min/max requires comparable type, got {:?}",
                value.type_code
            ),
        }),
    }
}

/// Divide a numeric or duration value by a count.
///
/// Note: This is specific to fn:avg which needs division by integer count,
/// not general numeric division which is handled by operators::eval_binary.
fn numeric_divide(value: &XmlValue, count: usize) -> Result<XmlValue, XPathError> {
    let count_f64 = count as f64;

    match value.type_code {
        XmlTypeCode::Double => {
            let v = value.as_double().unwrap();
            Ok(XmlValue::double(v / count_f64))
        }
        XmlTypeCode::Float => {
            let v = get_float(value).unwrap();
            Ok(XmlValue::float(v / count as f32))
        }
        XmlTypeCode::Decimal => {
            let v = value.as_decimal().unwrap();
            let count_decimal = Decimal::from(count as u64);
            Ok(XmlValue::decimal(v / count_decimal))
        }
        XmlTypeCode::YearMonthDuration => {
            let dur = get_year_month_duration(value)?;
            let total_months = year_month_total_months(&dur);
            let avg_months = (total_months as f64 / count_f64).round() as i64;
            let negative = avg_months < 0;
            let abs_months = avg_months.unsigned_abs();
            Ok(XmlValue::new(
                XmlTypeCode::YearMonthDuration,
                XmlValueKind::Atomic(XmlAtomicValue::YearMonthDuration(
                    crate::types::value::YearMonthDurationValue {
                        negative,
                        years: (abs_months / 12) as u32,
                        months: (abs_months % 12) as u32,
                    },
                )),
            ))
        }
        XmlTypeCode::DayTimeDuration => {
            let dur = get_day_time_duration(value)?;
            let total_secs = day_time_total_seconds(&dur);
            let avg_secs = total_secs / count_f64;
            Ok(seconds_to_day_time_duration(avg_secs))
        }
        _ => Err(XPathError::FORG0006 {
            message: format!("Cannot divide {:?} by count", value.type_code),
        }),
    }
}

/// Check if any value in the sequence is NaN.
fn contains_nan(values: &[XmlValue]) -> bool {
    values.iter().any(|v| {
        if let Some(d) = v.as_double() {
            return d.is_nan();
        }
        if let Some(f) = get_float(v) {
            return f.is_nan();
        }
        false
    })
}

/// Extract float value from XmlValue.
fn get_float(value: &XmlValue) -> Option<f32> {
    match &value.value {
        XmlValueKind::Atomic(XmlAtomicValue::Float(f)) => Some(*f),
        _ => None,
    }
}

/// Extract YearMonthDuration from XmlValue.
fn get_year_month_duration(
    value: &XmlValue,
) -> Result<crate::types::value::YearMonthDurationValue, XPathError> {
    match &value.value {
        XmlValueKind::Atomic(XmlAtomicValue::YearMonthDuration(d)) => Ok(d.clone()),
        _ => Err(XPathError::FORG0006 {
            message: "Expected yearMonthDuration".to_string(),
        }),
    }
}

/// Extract DayTimeDuration from XmlValue.
fn get_day_time_duration(
    value: &XmlValue,
) -> Result<crate::types::value::DayTimeDurationValue, XPathError> {
    match &value.value {
        XmlValueKind::Atomic(XmlAtomicValue::DayTimeDuration(d)) => Ok(d.clone()),
        _ => Err(XPathError::FORG0006 {
            message: "Expected dayTimeDuration".to_string(),
        }),
    }
}

/// Get total months from YearMonthDuration.
fn year_month_total_months(d: &crate::types::value::YearMonthDurationValue) -> i64 {
    let total = d.years as i64 * 12 + d.months as i64;
    if d.negative {
        -total
    } else {
        total
    }
}

/// Convert DayTimeDuration to total seconds.
fn day_time_total_seconds(d: &crate::types::value::DayTimeDurationValue) -> f64 {
    let total = d.days as f64 * 86400.0
        + d.hours as f64 * 3600.0
        + d.minutes as f64 * 60.0
        + d.seconds.to_f64().unwrap_or(0.0);
    if d.negative {
        -total
    } else {
        total
    }
}

/// Convert total seconds to DayTimeDuration.
fn seconds_to_day_time_duration(secs: f64) -> XmlValue {
    let negative = secs < 0.0;
    let abs_secs = secs.abs();
    let days = (abs_secs / 86400.0).floor() as u32;
    let remaining = abs_secs - days as f64 * 86400.0;
    let hours = (remaining / 3600.0).floor() as u32;
    let remaining = remaining - hours as f64 * 3600.0;
    let minutes = (remaining / 60.0).floor() as u32;
    let seconds = remaining - minutes as f64 * 60.0;

    XmlValue::new(
        XmlTypeCode::DayTimeDuration,
        XmlValueKind::Atomic(XmlAtomicValue::DayTimeDuration(
            crate::types::value::DayTimeDurationValue {
                negative,
                days,
                hours,
                minutes,
                seconds: Decimal::from_f64_retain(seconds).unwrap_or(Decimal::ZERO),
            },
        )),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::namespace::table::NameTable;
    use crate::xpath::context::XPathContext;
    use crate::xpath::iterator::XmlItem;
    use crate::xpath::RoXmlNavigator;
    use num_bigint::BigInt;

    fn make_context<'a>() -> DynamicContext<'a, RoXmlNavigator<'a>> {
        let table = Box::leak(Box::new(NameTable::new()));
        let xpath_ctx = Box::leak(Box::new(XPathContext::new(table)));
        DynamicContext::new(xpath_ctx, 0)
    }

    fn integer_seq<N: DomNavigator>(values: &[i64]) -> XPathValue<N> {
        let items: Vec<XmlItem<N>> = values
            .iter()
            .map(|&v| XmlItem::Atomic(XmlValue::integer(BigInt::from(v))))
            .collect();
        XPathValue::from_sequence(items)
    }

    fn double_seq<N: DomNavigator>(values: &[f64]) -> XPathValue<N> {
        let items: Vec<XmlItem<N>> = values
            .iter()
            .map(|&v| XmlItem::Atomic(XmlValue::double(v)))
            .collect();
        XPathValue::from_sequence(items)
    }

    fn extract_double<N: DomNavigator>(value: XPathValue<N>) -> Option<f64> {
        match value {
            XPathValue::Item(XmlItem::Atomic(v)) => v.as_double(),
            _ => None,
        }
    }

    fn extract_decimal<N: DomNavigator>(value: XPathValue<N>) -> Option<Decimal> {
        match value {
            XPathValue::Item(XmlItem::Atomic(v)) => v.as_decimal(),
            _ => None,
        }
    }

    // ========== sum tests ==========

    #[test]
    fn test_sum_integers() {
        let mut ctx = make_context();
        let seq = integer_seq::<RoXmlNavigator>(&[1, 2, 3, 4, 5]);
        let args = vec![seq];
        let result = sum(&mut ctx, args).unwrap();
        // Integers promoted to decimal
        let d = extract_decimal(result).unwrap();
        assert_eq!(d, Decimal::from(15));
    }

    #[test]
    fn test_sum_doubles() {
        let mut ctx = make_context();
        let seq = double_seq::<RoXmlNavigator>(&[1.0, 2.0, 3.0]);
        let args = vec![seq];
        let result = sum(&mut ctx, args).unwrap();
        let d = extract_double(result).unwrap();
        assert!((d - 6.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_sum_empty_returns_zero() {
        let mut ctx = make_context();
        let seq = XPathValue::<RoXmlNavigator>::Empty;
        let args = vec![seq];
        let result = sum(&mut ctx, args).unwrap();
        // Should return integer 0
        match result {
            XPathValue::Item(XmlItem::Atomic(v)) => {
                assert_eq!(*v.as_integer().unwrap(), BigInt::from(0));
            }
            _ => panic!("Expected integer 0"),
        }
    }

    #[test]
    fn test_sum_empty_with_zero() {
        let mut ctx = make_context();
        let seq = XPathValue::<RoXmlNavigator>::Empty;
        let zero = XPathValue::double(42.0);
        let args = vec![seq, zero];
        let result = sum(&mut ctx, args).unwrap();
        let d = extract_double(result).unwrap();
        assert!((d - 42.0).abs() < f64::EPSILON);
    }

    // ========== avg tests ==========

    #[test]
    fn test_avg_integers() {
        let mut ctx = make_context();
        let seq = integer_seq::<RoXmlNavigator>(&[1, 2, 3, 4, 5]);
        let args = vec![seq];
        let result = avg(&mut ctx, args).unwrap();
        // Integers promoted to decimal, result is decimal
        let d = extract_decimal(result).unwrap();
        assert_eq!(d, Decimal::from(3));
    }

    #[test]
    fn test_avg_doubles() {
        let mut ctx = make_context();
        let seq = double_seq::<RoXmlNavigator>(&[1.0, 2.0, 3.0]);
        let args = vec![seq];
        let result = avg(&mut ctx, args).unwrap();
        let d = extract_double(result).unwrap();
        assert!((d - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_avg_empty_returns_empty() {
        let mut ctx = make_context();
        let seq = XPathValue::<RoXmlNavigator>::Empty;
        let args = vec![seq];
        let result = avg(&mut ctx, args).unwrap();
        assert!(result.is_empty());
    }

    // ========== min tests ==========

    #[test]
    fn test_min_integers() {
        let mut ctx = make_context();
        let seq = integer_seq::<RoXmlNavigator>(&[5, 3, 7, 1, 9]);
        let args = vec![seq];
        let result = min(&mut ctx, args).unwrap();
        match result {
            XPathValue::Item(XmlItem::Atomic(v)) => {
                assert_eq!(*v.as_integer().unwrap(), BigInt::from(1));
            }
            _ => panic!("Expected integer"),
        }
    }

    #[test]
    fn test_min_doubles() {
        let mut ctx = make_context();
        let seq = double_seq::<RoXmlNavigator>(&[5.0, 3.0, 7.0, 1.0, 9.0]);
        let args = vec![seq];
        let result = min(&mut ctx, args).unwrap();
        let d = extract_double(result).unwrap();
        assert!((d - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_min_with_nan() {
        let mut ctx = make_context();
        let seq = double_seq::<RoXmlNavigator>(&[f64::NAN, 3.0, 1.0]);
        let args = vec![seq];
        let result = min(&mut ctx, args).unwrap();
        let d = extract_double(result).unwrap();
        // Per XPath 2.0: If sequence contains NaN, return NaN
        assert!(d.is_nan());
    }

    #[test]
    fn test_min_empty_returns_empty() {
        let mut ctx = make_context();
        let seq = XPathValue::<RoXmlNavigator>::Empty;
        let args = vec![seq];
        let result = min(&mut ctx, args).unwrap();
        assert!(result.is_empty());
    }

    // ========== max tests ==========

    #[test]
    fn test_max_integers() {
        let mut ctx = make_context();
        let seq = integer_seq::<RoXmlNavigator>(&[5, 3, 7, 1, 9]);
        let args = vec![seq];
        let result = max(&mut ctx, args).unwrap();
        match result {
            XPathValue::Item(XmlItem::Atomic(v)) => {
                assert_eq!(*v.as_integer().unwrap(), BigInt::from(9));
            }
            _ => panic!("Expected integer"),
        }
    }

    #[test]
    fn test_max_doubles() {
        let mut ctx = make_context();
        let seq = double_seq::<RoXmlNavigator>(&[5.0, 3.0, 7.0, 1.0, 9.0]);
        let args = vec![seq];
        let result = max(&mut ctx, args).unwrap();
        let d = extract_double(result).unwrap();
        assert!((d - 9.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_max_with_nan() {
        let mut ctx = make_context();
        let seq = double_seq::<RoXmlNavigator>(&[f64::NAN, 3.0, 9.0]);
        let args = vec![seq];
        let result = max(&mut ctx, args).unwrap();
        let d = extract_double(result).unwrap();
        // Per XPath 2.0: If sequence contains NaN, return NaN
        assert!(d.is_nan());
    }

    #[test]
    fn test_max_empty_returns_empty() {
        let mut ctx = make_context();
        let seq = XPathValue::<RoXmlNavigator>::Empty;
        let args = vec![seq];
        let result = max(&mut ctx, args).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_min_strings() {
        let mut ctx = make_context();
        let items: Vec<XmlItem<RoXmlNavigator>> = vec!["banana", "apple", "cherry"]
            .into_iter()
            .map(|s| XmlItem::Atomic(XmlValue::string(s)))
            .collect();
        let seq = XPathValue::from_sequence(items);
        let args = vec![seq];
        let result = min(&mut ctx, args).unwrap();
        match result {
            XPathValue::Item(XmlItem::Atomic(v)) => {
                assert_eq!(v.as_string().unwrap(), "apple");
            }
            _ => panic!("Expected string"),
        }
    }

    #[test]
    fn test_max_strings() {
        let mut ctx = make_context();
        let items: Vec<XmlItem<RoXmlNavigator>> = vec!["banana", "apple", "cherry"]
            .into_iter()
            .map(|s| XmlItem::Atomic(XmlValue::string(s)))
            .collect();
        let seq = XPathValue::from_sequence(items);
        let args = vec![seq];
        let result = max(&mut ctx, args).unwrap();
        match result {
            XPathValue::Item(XmlItem::Atomic(v)) => {
                assert_eq!(v.as_string().unwrap(), "cherry");
            }
            _ => panic!("Expected string"),
        }
    }

    #[test]
    fn test_min_anyuri() {
        use crate::types::value::{XmlAtomicValue, XmlValueKind};
        let mut ctx = make_context();
        let items: Vec<XmlItem<RoXmlNavigator>> = vec![
            XmlItem::Atomic(XmlValue::new(
                XmlTypeCode::AnyUri,
                XmlValueKind::Atomic(XmlAtomicValue::AnyUri("http://example.com/b".to_string())),
            )),
            XmlItem::Atomic(XmlValue::new(
                XmlTypeCode::AnyUri,
                XmlValueKind::Atomic(XmlAtomicValue::AnyUri("http://example.com/a".to_string())),
            )),
        ];
        let seq = XPathValue::from_sequence(items);
        let args = vec![seq];
        let result = min(&mut ctx, args).unwrap();
        match result {
            XPathValue::Item(XmlItem::Atomic(v)) => {
                // AnyUri promoted to string for comparison
                assert_eq!(v.as_string().unwrap(), "http://example.com/a");
            }
            _ => panic!("Expected string"),
        }
    }

    #[test]
    fn test_sum_untyped_invalid_forg0001() {
        let mut ctx = make_context();
        let items: Vec<XmlItem<RoXmlNavigator>> =
            vec![XmlItem::Atomic(XmlValue::untyped("not a number"))];
        let seq = XPathValue::from_sequence(items);
        let args = vec![seq];
        let result = sum(&mut ctx, args);
        match result {
            Err(XPathError::FORG0001 { value, target_type }) => {
                assert_eq!(value, "not a number");
                assert_eq!(target_type, "xs:double");
            }
            _ => panic!("Expected FORG0001 error"),
        }
    }
}
