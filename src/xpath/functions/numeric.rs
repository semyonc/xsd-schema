//! XPath 2.0 numeric functions.
//!
//! This module implements numeric functions from the XPath 2.0 specification:
//! - fn:abs
//! - fn:ceiling
//! - fn:floor
//! - fn:round
//! - fn:round-half-to-even

use num_bigint::BigInt;
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use rust_decimal::Decimal;

use crate::types::value::{XmlAtomicValue, XmlValue, XmlValueKind};
use crate::types::XmlTypeCode;
use crate::xpath::context::DynamicContext;
use crate::xpath::error::XPathError;
use crate::xpath::DomNavigator;

use super::{atomize_to_single_opt, convert, XPathValue};

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

/// Extract float value from XmlValue.
fn get_float(value: &XmlValue) -> Option<f32> {
    match &value.value {
        XmlValueKind::Atomic(XmlAtomicValue::Float(f)) => Some(*f),
        _ => None,
    }
}

// ============================================================================
// fn:abs($arg as numeric?) as numeric?
// ============================================================================

/// Implements fn:abs - returns the absolute value of the argument.
///
/// The function preserves the numeric type of the input.
pub fn abs<N: DomNavigator>(
    _context: &mut DynamicContext<'_, N>,
    mut args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if args.len() != 1 {
        return Err(XPathError::wrong_number_of_arguments("abs", 1, args.len()));
    }

    let arg = args.remove(0);
    let value = match atomize_to_single_opt(arg)? {
        None => return Ok(XPathValue::Empty),
        Some(v) => v,
    };
    // Function conversion rules, XPath 2.0 §3.1.5: an xs:untypedAtomic
    // argument of a `numeric` parameter is cast to xs:double.
    let value = convert::promote_untyped_to_double(value, "abs")?;

    let result = numeric_abs(&value)?;
    Ok(XPathValue::from_atomic(result))
}

fn numeric_abs(value: &XmlValue) -> Result<XmlValue, XPathError> {
    match value.type_code {
        XmlTypeCode::Double => {
            let d = value.as_double().ok_or_else(|| XPathError::XPTY0004 {
                expected: "xs:double".to_string(),
                found: format!("{:?}", value.type_code),
            })?;
            Ok(XmlValue::double(d.abs()))
        }
        XmlTypeCode::Float => {
            let f = get_float(value).ok_or_else(|| XPathError::XPTY0004 {
                expected: "xs:float".to_string(),
                found: format!("{:?}", value.type_code),
            })?;
            Ok(XmlValue::float(f.abs()))
        }
        XmlTypeCode::Decimal => {
            let d = value.as_decimal().ok_or_else(|| XPathError::XPTY0004 {
                expected: "xs:decimal".to_string(),
                found: format!("{:?}", value.type_code),
            })?;
            Ok(XmlValue::decimal(d.abs()))
        }
        _ if is_integer_type(value.type_code) => {
            let i = value.as_integer().ok_or_else(|| XPathError::XPTY0004 {
                expected: "xs:integer".to_string(),
                found: format!("{:?}", value.type_code),
            })?;
            // For BigInt, we need to handle negative numbers
            let abs_val = if *i < BigInt::from(0) {
                -i.clone()
            } else {
                i.clone()
            };
            Ok(XmlValue::integer(abs_val))
        }
        _ => Err(XPathError::XPTY0004 {
            expected: "xs:numeric".to_string(),
            found: format!("{:?}", value.type_code),
        }),
    }
}

// ============================================================================
// fn:ceiling($arg as numeric?) as numeric?
// ============================================================================

/// Implements fn:ceiling - returns the smallest integer greater than or equal to the argument.
///
/// The function preserves the numeric type of the input.
pub fn ceiling<N: DomNavigator>(
    _context: &mut DynamicContext<'_, N>,
    mut args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if args.len() != 1 {
        return Err(XPathError::wrong_number_of_arguments(
            "ceiling",
            1,
            args.len(),
        ));
    }

    let arg = args.remove(0);
    let value = match atomize_to_single_opt(arg)? {
        None => return Ok(XPathValue::Empty),
        Some(v) => v,
    };
    // Function conversion rules, XPath 2.0 §3.1.5: an xs:untypedAtomic
    // argument of a `numeric` parameter is cast to xs:double.
    let value = convert::promote_untyped_to_double(value, "ceiling")?;

    let result = numeric_ceiling(&value)?;
    Ok(XPathValue::from_atomic(result))
}

fn numeric_ceiling(value: &XmlValue) -> Result<XmlValue, XPathError> {
    match value.type_code {
        XmlTypeCode::Double => {
            let d = value.as_double().ok_or_else(|| XPathError::XPTY0004 {
                expected: "xs:double".to_string(),
                found: format!("{:?}", value.type_code),
            })?;
            Ok(XmlValue::double(d.ceil()))
        }
        XmlTypeCode::Float => {
            let f = get_float(value).ok_or_else(|| XPathError::XPTY0004 {
                expected: "xs:float".to_string(),
                found: format!("{:?}", value.type_code),
            })?;
            Ok(XmlValue::float(f.ceil()))
        }
        XmlTypeCode::Decimal => {
            let d = value.as_decimal().ok_or_else(|| XPathError::XPTY0004 {
                expected: "xs:decimal".to_string(),
                found: format!("{:?}", value.type_code),
            })?;
            // Decimal doesn't have ceil(), use manual calculation
            let truncated = d.trunc();
            let result = if d > truncated {
                truncated + Decimal::ONE
            } else {
                truncated
            };
            Ok(XmlValue::decimal(result))
        }
        _ if is_integer_type(value.type_code) => {
            // For integers, ceiling is identity
            Ok(value.clone())
        }
        _ => Err(XPathError::XPTY0004 {
            expected: "xs:numeric".to_string(),
            found: format!("{:?}", value.type_code),
        }),
    }
}

// ============================================================================
// fn:floor($arg as numeric?) as numeric?
// ============================================================================

/// Implements fn:floor - returns the largest integer less than or equal to the argument.
///
/// The function preserves the numeric type of the input.
pub fn floor<N: DomNavigator>(
    _context: &mut DynamicContext<'_, N>,
    mut args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if args.len() != 1 {
        return Err(XPathError::wrong_number_of_arguments(
            "floor",
            1,
            args.len(),
        ));
    }

    let arg = args.remove(0);
    let value = match atomize_to_single_opt(arg)? {
        None => return Ok(XPathValue::Empty),
        Some(v) => v,
    };
    // Function conversion rules, XPath 2.0 §3.1.5: an xs:untypedAtomic
    // argument of a `numeric` parameter is cast to xs:double.
    let value = convert::promote_untyped_to_double(value, "floor")?;

    let result = numeric_floor(&value)?;
    Ok(XPathValue::from_atomic(result))
}

fn numeric_floor(value: &XmlValue) -> Result<XmlValue, XPathError> {
    match value.type_code {
        XmlTypeCode::Double => {
            let d = value.as_double().ok_or_else(|| XPathError::XPTY0004 {
                expected: "xs:double".to_string(),
                found: format!("{:?}", value.type_code),
            })?;
            Ok(XmlValue::double(d.floor()))
        }
        XmlTypeCode::Float => {
            let f = get_float(value).ok_or_else(|| XPathError::XPTY0004 {
                expected: "xs:float".to_string(),
                found: format!("{:?}", value.type_code),
            })?;
            Ok(XmlValue::float(f.floor()))
        }
        XmlTypeCode::Decimal => {
            let d = value.as_decimal().ok_or_else(|| XPathError::XPTY0004 {
                expected: "xs:decimal".to_string(),
                found: format!("{:?}", value.type_code),
            })?;
            // Decimal doesn't have floor(), use manual calculation
            let truncated = d.trunc();
            let result = if d < truncated {
                truncated - Decimal::ONE
            } else {
                truncated
            };
            Ok(XmlValue::decimal(result))
        }
        _ if is_integer_type(value.type_code) => {
            // For integers, floor is identity
            Ok(value.clone())
        }
        _ => Err(XPathError::XPTY0004 {
            expected: "xs:numeric".to_string(),
            found: format!("{:?}", value.type_code),
        }),
    }
}

// ============================================================================
// fn:round($arg as numeric?) as numeric?
// ============================================================================

/// Implements fn:round - returns the number with no fractional part that is
/// closest to the argument.
///
/// F&O §6.4.4 `fn:round`: "Returns the number with no fractional part that is
/// closest to the argument. If there are two such numbers, then the one that is
/// closest to positive infinity is returned." A half value therefore rounds
/// *towards positive infinity* in both directions — `round(2.5)` is 3 and
/// `round(-2.5)` is -2, not -3 — and the spec's own examples spell the negative
/// case out: "round(-2.5) returns -2 (not the possible alternative, -3)".
///
/// The function preserves the numeric type of the input.
pub fn round<N: DomNavigator>(
    _context: &mut DynamicContext<'_, N>,
    mut args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if args.len() != 1 {
        return Err(XPathError::wrong_number_of_arguments(
            "round",
            1,
            args.len(),
        ));
    }

    let arg = args.remove(0);
    let value = match atomize_to_single_opt(arg)? {
        None => return Ok(XPathValue::Empty),
        Some(v) => v,
    };
    // Function conversion rules, XPath 2.0 §3.1.5: an xs:untypedAtomic
    // argument of a `numeric` parameter is cast to xs:double.
    let value = convert::promote_untyped_to_double(value, "round")?;

    let result = numeric_round(&value)?;
    Ok(XPathValue::from_atomic(result))
}

fn numeric_round(value: &XmlValue) -> Result<XmlValue, XPathError> {
    match value.type_code {
        XmlTypeCode::Double => {
            let d = value.as_double().ok_or_else(|| XPathError::XPTY0004 {
                expected: "xs:double".to_string(),
                found: format!("{:?}", value.type_code),
            })?;
            Ok(XmlValue::double(round_half_toward_positive_infinity_f64(d)))
        }
        XmlTypeCode::Float => {
            let f = get_float(value).ok_or_else(|| XPathError::XPTY0004 {
                expected: "xs:float".to_string(),
                found: format!("{:?}", value.type_code),
            })?;
            Ok(XmlValue::float(round_half_toward_positive_infinity_f32(f)))
        }
        XmlTypeCode::Decimal => {
            let d = value.as_decimal().ok_or_else(|| XPathError::XPTY0004 {
                expected: "xs:decimal".to_string(),
                found: format!("{:?}", value.type_code),
            })?;
            Ok(XmlValue::decimal(
                round_half_toward_positive_infinity_decimal(d),
            ))
        }
        _ if is_integer_type(value.type_code) => {
            // For integers, round is identity
            Ok(value.clone())
        }
        _ => Err(XPathError::XPTY0004 {
            expected: "xs:numeric".to_string(),
            found: format!("{:?}", value.type_code),
        }),
    }
}

/// Round half toward positive infinity for `f64` (`fn:round` semantics).
///
/// Shared with `fn:subsequence`, whose positions F&O §15.1.10 defines through
/// `fn:round`, so that the two cannot drift apart.
///
/// F&O §6.4.4 lists the special values of the floating-point types explicitly:
/// NaN, positive and negative infinity and positive and negative zero are
/// returned unchanged, and an argument "less than zero, but greater than or
/// equal to -0.5" returns *negative* zero.
///
/// The rounding itself is `floor(x) + 1` when the fractional part is at least
/// a half, rather than the more compact `(x + 0.5).floor()`: the fractional
/// part `x - floor(x)` is always exact — it is a multiple of `ulp(x)` smaller
/// than one — whereas `x + 0.5` is not, and for the largest `f64` below a half
/// it rounds up to `1.0` and would answer 1 where the nearest number with no
/// fractional part is 0. Writing it this way also needs no magnitude guard: at
/// 2^52 and above every `f64` is already integral, so `floor(x)` is `x` and the
/// fractional part is zero.
pub(crate) fn round_half_toward_positive_infinity_f64(d: f64) -> f64 {
    // `d == 0.0` matches negative zero too, and returning `d` keeps its sign.
    if d.is_nan() || d.is_infinite() || d == 0.0 {
        return d;
    }
    let floored = d.floor();
    let rounded = if d - floored >= 0.5 {
        floored + 1.0
    } else {
        floored
    };
    if rounded == 0.0 && d < 0.0 {
        // -0.5 <= d < 0: the result is negative zero, not positive zero.
        -0.0
    } else {
        rounded
    }
}

/// Round half toward positive infinity for `f32`.
///
/// Identical to [`round_half_toward_positive_infinity_f64`], including the
/// negative-zero rule of F&O §6.4.4; the exactness argument holds at 2^23,
/// where every `f32` is already integral.
fn round_half_toward_positive_infinity_f32(f: f32) -> f32 {
    if f.is_nan() || f.is_infinite() || f == 0.0 {
        return f;
    }
    let floored = f.floor();
    let rounded = if f - floored >= 0.5 {
        floored + 1.0
    } else {
        floored
    };
    if rounded == 0.0 && f < 0.0 {
        -0.0
    } else {
        rounded
    }
}

/// Round half toward positive infinity for `Decimal`, in exact decimal
/// arithmetic.
///
/// `xs:decimal` has no NaN, no infinities and no negative zero (Datatypes
/// §3.3.4), so the floating-point special cases do not arise and `round(-0.5)`
/// is plain `0`.
fn round_half_toward_positive_infinity_decimal(d: Decimal) -> Decimal {
    let floored = d.floor();
    if d - floored >= Decimal::new(5, 1) {
        floored + Decimal::ONE
    } else {
        floored
    }
}

// ============================================================================
// fn:round-half-to-even($arg as numeric?, $precision as integer?) as numeric?
// ============================================================================

/// Implements fn:round-half-to-even - banker's rounding.
///
/// Rounds to the specified precision using half-to-even rounding mode.
/// If precision is omitted, rounds to the nearest integer.
pub fn round_half_to_even<N: DomNavigator>(
    _context: &mut DynamicContext<'_, N>,
    mut args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if args.is_empty() || args.len() > 2 {
        return Err(XPathError::wrong_number_of_arguments(
            "round-half-to-even",
            1,
            args.len(),
        ));
    }

    // Get precision (default 0)
    let precision: i32 = if args.len() == 2 {
        let prec_arg = args.remove(1);
        match atomize_to_single_opt(prec_arg)? {
            None => return Ok(XPathValue::Empty),
            Some(v) => {
                let v = convert::expect_atomic_as(v, XmlTypeCode::Integer, "round-half-to-even")?;
                v.as_integer()
                    .and_then(|i| i.to_i32())
                    .ok_or_else(|| XPathError::XPTY0004 {
                        expected: "xs:integer".to_string(),
                        found: format!("{:?}", v.type_code),
                    })?
            }
        }
    } else {
        0
    };

    let arg = args.remove(0);
    let value = match atomize_to_single_opt(arg)? {
        None => return Ok(XPathValue::Empty),
        Some(v) => v,
    };
    // Function conversion rules, XPath 2.0 §3.1.5: an xs:untypedAtomic
    // argument of a `numeric` parameter is cast to xs:double.
    let value = convert::promote_untyped_to_double(value, "round-half-to-even")?;

    let result = numeric_round_half_to_even(&value, precision)?;
    Ok(XPathValue::from_atomic(result))
}

fn numeric_round_half_to_even(value: &XmlValue, precision: i32) -> Result<XmlValue, XPathError> {
    match value.type_code {
        XmlTypeCode::Double => {
            let d = value.as_double().ok_or_else(|| XPathError::XPTY0004 {
                expected: "xs:double".to_string(),
                found: format!("{:?}", value.type_code),
            })?;
            Ok(XmlValue::double(round_half_to_even_f64(d, precision)))
        }
        XmlTypeCode::Float => {
            let f = get_float(value).ok_or_else(|| XPathError::XPTY0004 {
                expected: "xs:float".to_string(),
                found: format!("{:?}", value.type_code),
            })?;
            Ok(XmlValue::float(round_half_to_even_f32(f, precision)))
        }
        XmlTypeCode::Decimal => {
            let d = value.as_decimal().ok_or_else(|| XPathError::XPTY0004 {
                expected: "xs:decimal".to_string(),
                found: format!("{:?}", value.type_code),
            })?;
            Ok(XmlValue::decimal(round_half_to_even_decimal(d, precision)?))
        }
        _ if is_integer_type(value.type_code) => {
            // For integers with non-negative precision, return as-is
            if precision >= 0 {
                return Ok(value.clone());
            }

            // For negative precision, round to powers of 10
            let i = value.as_integer().ok_or_else(|| XPathError::XPTY0004 {
                expected: "xs:integer".to_string(),
                found: format!("{:?}", value.type_code),
            })?;

            let result = round_half_to_even_integer(i, precision);
            Ok(XmlValue::integer(result))
        }
        _ => Err(XPathError::XPTY0004 {
            expected: "xs:numeric".to_string(),
            found: format!("{:?}", value.type_code),
        }),
    }
}

/// Round half to even for f64 with given precision.
fn round_half_to_even_f64(d: f64, precision: i32) -> f64 {
    if d.is_nan() || d.is_infinite() {
        return d;
    }

    if precision < 0 {
        // Round to powers of 10 (e.g., precision -1 rounds to nearest 10)
        let scale = 10_f64.powi(-precision);
        let scaled = d / scale;
        // Use round_ties_even
        round_ties_even_f64(scaled) * scale
    } else {
        let scale = 10_f64.powi(precision);
        let scaled = d * scale;
        round_ties_even_f64(scaled) / scale
    }
}

/// Round ties to even for f64 (banker's rounding).
fn round_ties_even_f64(d: f64) -> f64 {
    let floored = d.floor();
    let frac = d - floored;

    if frac < 0.5 {
        floored
    } else if frac > 0.5 {
        floored + 1.0
    } else {
        // Exactly 0.5 - round to even
        if floored as i64 % 2 == 0 {
            floored
        } else {
            floored + 1.0
        }
    }
}

/// Round half to even for f32 with given precision.
fn round_half_to_even_f32(f: f32, precision: i32) -> f32 {
    if f.is_nan() || f.is_infinite() {
        return f;
    }

    if precision < 0 {
        let scale = 10_f32.powi(-precision);
        let scaled = f / scale;
        round_ties_even_f32(scaled) * scale
    } else {
        let scale = 10_f32.powi(precision);
        let scaled = f * scale;
        round_ties_even_f32(scaled) / scale
    }
}

/// Round ties to even for f32.
fn round_ties_even_f32(f: f32) -> f32 {
    let floored = f.floor();
    let frac = f - floored;

    if frac < 0.5 {
        floored
    } else if frac > 0.5 {
        floored + 1.0
    } else if floored as i32 % 2 == 0 {
        floored
    } else {
        floored + 1.0
    }
}

/// Round half to even for Decimal with given precision.
fn round_half_to_even_decimal(d: Decimal, precision: i32) -> Result<Decimal, XPathError> {
    if precision < 0 {
        // For negative precision, we need to round to powers of 10
        let abs_precision = (-precision) as u32;
        let scale = Decimal::from_i64(10_i64.pow(abs_precision))
            .ok_or_else(|| XPathError::internal("Failed to create decimal scale"))?;

        // Divide, round, multiply
        let scaled = d / scale;
        let rounded =
            scaled.round_dp_with_strategy(0, rust_decimal::RoundingStrategy::MidpointNearestEven);
        Ok(rounded * scale)
    } else {
        Ok(d.round_dp_with_strategy(
            precision as u32,
            rust_decimal::RoundingStrategy::MidpointNearestEven,
        ))
    }
}

/// Round half to even for BigInt with negative precision.
fn round_half_to_even_integer(i: &BigInt, precision: i32) -> BigInt {
    if precision >= 0 {
        return i.clone();
    }

    // For negative precision, round to powers of 10
    let abs_precision = (-precision) as u32;
    let scale = BigInt::from(10).pow(abs_precision);
    let half_scale = &scale / 2;

    // Compute: round(i / scale) * scale using half-to-even
    let (quotient, remainder) = (i / &scale, i % &scale);
    let abs_remainder = if remainder < BigInt::from(0) {
        -&remainder
    } else {
        remainder.clone()
    };

    let rounded = if abs_remainder < half_scale {
        quotient.clone()
    } else if abs_remainder > half_scale {
        if *i >= BigInt::from(0) {
            &quotient + 1
        } else {
            &quotient - 1
        }
    } else {
        // Exactly half - round to even
        if &quotient % 2 == BigInt::from(0) {
            quotient.clone()
        } else if *i >= BigInt::from(0) {
            &quotient + 1
        } else {
            &quotient - 1
        }
    };

    rounded * scale
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::namespace::table::NameTable;
    use crate::xpath::context::XPathContext;
    use crate::xpath::RoXmlNavigator;

    fn make_context<'a>() -> DynamicContext<'a, RoXmlNavigator<'a>> {
        let table = Box::leak(Box::new(NameTable::new()));
        let xpath_ctx = Box::leak(Box::new(XPathContext::new(table)));
        DynamicContext::new(xpath_ctx, 0)
    }

    #[test]
    fn test_abs_double() {
        let mut ctx = make_context();
        let args = vec![XPathValue::double(-3.5)];
        let result = abs(&mut ctx, args).unwrap();
        match result {
            XPathValue::Item(item) => {
                if let crate::xpath::iterator::XmlItem::Atomic(v) = item {
                    assert_eq!(v.as_double().unwrap(), 3.5);
                } else {
                    panic!("Expected atomic value");
                }
            }
            _ => panic!("Expected single item"),
        }
    }

    #[test]
    fn test_abs_empty() {
        let mut ctx = make_context();
        let args = vec![XPathValue::Empty];
        let result = abs(&mut ctx, args).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_ceiling_double() {
        let mut ctx = make_context();
        let args = vec![XPathValue::double(3.2)];
        let result = ceiling(&mut ctx, args).unwrap();
        match result {
            XPathValue::Item(item) => {
                if let crate::xpath::iterator::XmlItem::Atomic(v) = item {
                    assert_eq!(v.as_double().unwrap(), 4.0);
                } else {
                    panic!("Expected atomic value");
                }
            }
            _ => panic!("Expected single item"),
        }
    }

    #[test]
    fn test_floor_double() {
        let mut ctx = make_context();
        let args = vec![XPathValue::double(3.8)];
        let result = floor(&mut ctx, args).unwrap();
        match result {
            XPathValue::Item(item) => {
                if let crate::xpath::iterator::XmlItem::Atomic(v) = item {
                    assert_eq!(v.as_double().unwrap(), 3.0);
                } else {
                    panic!("Expected atomic value");
                }
            }
            _ => panic!("Expected single item"),
        }
    }

    /// Evaluate `fn:round` on one atomic argument and hand back the result.
    fn round_one(arg: XPathValue<RoXmlNavigator<'static>>) -> XmlValue {
        let mut ctx = make_context();
        match round(&mut ctx, vec![arg]).expect("fn:round of a numeric argument") {
            XPathValue::Item(crate::xpath::iterator::XmlItem::Atomic(v)) => v,
            _ => panic!("Expected a single atomic item"),
        }
    }

    fn round_double(d: f64) -> f64 {
        let value = round_one(XPathValue::double(d));
        assert_eq!(value.type_code, XmlTypeCode::Double);
        value.as_double().expect("xs:double result")
    }

    fn round_float(f: f32) -> f32 {
        let value = round_one(XPathValue::from_atomic(XmlValue::float(f)));
        assert_eq!(value.type_code, XmlTypeCode::Float);
        get_float(&value).expect("xs:float result")
    }

    fn round_decimal(literal: &str) -> Decimal {
        let d: Decimal = literal.parse().expect("decimal literal");
        let value = round_one(XPathValue::decimal(d));
        assert_eq!(value.type_code, XmlTypeCode::Decimal);
        value.as_decimal().expect("xs:decimal result")
    }

    /// F&O §6.4.4: "If there are two such numbers, then the one that is closest
    /// to positive infinity is returned", with the example `round(-2.5)` = -2.
    #[test]
    fn round_double_takes_halves_toward_positive_infinity() {
        assert_eq!(round_double(2.5), 3.0);
        assert_eq!(round_double(-2.5), -2.0);
        assert_eq!(round_double(3.5), 4.0);
        assert_eq!(round_double(-3.5), -3.0);
        assert_eq!(round_double(2.4999), 2.0);
        assert_eq!(round_double(-2.4999), -2.0);
        assert_eq!(round_double(0.5), 1.0);
    }

    /// F&O §6.4.4: "If the argument is less than zero, but greater than or equal
    /// to -0.5, then negative zero is returned."
    #[test]
    fn round_double_returns_negative_zero_just_below_zero() {
        for arg in [-0.3, -0.5, -f64::MIN_POSITIVE] {
            let rounded = round_double(arg);
            assert_eq!(rounded, 0.0, "round({arg})");
            assert!(
                rounded.is_sign_negative(),
                "round({arg}) is not negative zero"
            );
        }
    }

    /// F&O §6.4.4 returns NaN, both infinities and both zeroes unchanged.
    #[test]
    fn round_double_returns_special_values_unchanged() {
        assert!(round_double(f64::NAN).is_nan());
        assert_eq!(round_double(f64::INFINITY), f64::INFINITY);
        assert_eq!(round_double(f64::NEG_INFINITY), f64::NEG_INFINITY);

        let positive_zero = round_double(0.0);
        assert_eq!(positive_zero, 0.0);
        assert!(positive_zero.is_sign_positive(), "round(0) lost its sign");

        let negative_zero = round_double(-0.0);
        assert_eq!(negative_zero, 0.0);
        assert!(negative_zero.is_sign_negative(), "round(-0) lost its sign");
    }

    /// At 2^52 and above every `f64` is already integral and `x + 0.5` is no
    /// longer exact, so such an argument must come back untouched.
    #[test]
    fn round_double_leaves_large_integral_values_alone() {
        for arg in [
            4_503_599_627_370_496.0_f64,  // 2^52
            4_503_599_627_370_497.0_f64,  // 2^52 + 1, an odd integer
            -4_503_599_627_370_497.0_f64, // and its negation
            1.797_693_134_862_315_7e308,  // xs:double's largest finite value
        ] {
            assert_eq!(round_double(arg), arg, "round({arg})");
        }
    }

    /// The largest `f64` strictly below a half: the nearest number with no
    /// fractional part is 0, even though `x + 0.5` rounds up to `1.0`.
    #[test]
    fn round_double_does_not_round_up_the_largest_value_below_a_half() {
        assert_eq!(round_double(0.499_999_999_999_999_94), 0.0);
    }

    #[test]
    fn round_float_takes_halves_toward_positive_infinity() {
        assert_eq!(round_float(2.5), 3.0);
        assert_eq!(round_float(-2.5), -2.0);
        assert_eq!(round_float(3.5), 4.0);
        assert_eq!(round_float(-3.5), -3.0);
        assert_eq!(round_float(2.4999), 2.0);
        assert_eq!(round_float(-2.4999), -2.0);
        assert_eq!(round_float(0.5), 1.0);
    }

    #[test]
    fn round_float_handles_zeroes_infinities_and_large_values() {
        for arg in [-0.3_f32, -0.5_f32] {
            let rounded = round_float(arg);
            assert_eq!(rounded, 0.0);
            assert!(
                rounded.is_sign_negative(),
                "round({arg}) is not negative zero"
            );
        }
        assert!(round_float(f32::NAN).is_nan());
        assert_eq!(round_float(f32::INFINITY), f32::INFINITY);
        assert_eq!(round_float(f32::NEG_INFINITY), f32::NEG_INFINITY);
        assert!(round_float(-0.0).is_sign_negative());
        assert!(round_float(0.0).is_sign_positive());
        // 2^23: every f32 at or above this magnitude is already integral.
        assert_eq!(round_float(8_388_609.0), 8_388_609.0);
        assert_eq!(round_float(0.499_999_97), 0.0);
    }

    /// `xs:decimal` has no negative zero, so `round(-0.5)` is plain `0`.
    #[test]
    fn round_decimal_takes_halves_toward_positive_infinity() {
        assert_eq!(round_decimal("2.5"), Decimal::from(3));
        assert_eq!(round_decimal("-2.5"), Decimal::from(-2));
        assert_eq!(round_decimal("3.5"), Decimal::from(4));
        assert_eq!(round_decimal("-3.5"), Decimal::from(-3));
        assert_eq!(round_decimal("2.4999"), Decimal::from(2));
        assert_eq!(round_decimal("-2.4999"), Decimal::from(-2));
        assert_eq!(round_decimal("0.5"), Decimal::from(1));
        assert_eq!(round_decimal("-0.5"), Decimal::ZERO);
        assert_eq!(round_decimal("-0.3"), Decimal::ZERO);
        assert_eq!(round_decimal("0"), Decimal::ZERO);
        assert_eq!(
            round_decimal("999999999999999999"),
            Decimal::from(999_999_999_999_999_999_i64)
        );
    }

    /// `fn:round` of an `xs:integer` is the identity, and keeps its type.
    #[test]
    fn round_integer_is_the_identity() {
        for literal in [0_i64, 1, -1, 2, -3, 999_999_999_999_999_999] {
            let value = round_one(XPathValue::integer(literal));
            assert!(is_integer_type(value.type_code), "{:?}", value.type_code);
            assert_eq!(value.as_integer(), Some(&BigInt::from(literal)));
        }
    }

    #[test]
    fn test_round_half_to_even_double() {
        let mut ctx = make_context();

        // Test 2.5 -> 2 (half to even)
        let args = vec![XPathValue::double(2.5)];
        let result = round_half_to_even(&mut ctx, args).unwrap();
        match result {
            XPathValue::Item(item) => {
                if let crate::xpath::iterator::XmlItem::Atomic(v) = item {
                    assert_eq!(v.as_double().unwrap(), 2.0);
                } else {
                    panic!("Expected atomic value");
                }
            }
            _ => panic!("Expected single item"),
        }

        // Test 3.5 -> 4 (half to even)
        let args = vec![XPathValue::double(3.5)];
        let result = round_half_to_even(&mut ctx, args).unwrap();
        match result {
            XPathValue::Item(item) => {
                if let crate::xpath::iterator::XmlItem::Atomic(v) = item {
                    assert_eq!(v.as_double().unwrap(), 4.0);
                } else {
                    panic!("Expected atomic value");
                }
            }
            _ => panic!("Expected single item"),
        }
    }

    #[test]
    fn test_round_half_to_even_with_precision() {
        let mut ctx = make_context();

        // Test 3.567 with precision 2 -> 3.57
        let args = vec![XPathValue::double(3.567), XPathValue::integer(2)];
        let result = round_half_to_even(&mut ctx, args).unwrap();
        match result {
            XPathValue::Item(item) => {
                if let crate::xpath::iterator::XmlItem::Atomic(v) = item {
                    let d = v.as_double().unwrap();
                    assert!((d - 3.57).abs() < 0.001);
                } else {
                    panic!("Expected atomic value");
                }
            }
            _ => panic!("Expected single item"),
        }
    }

    #[test]
    fn test_round_half_to_even_negative_precision() {
        let mut ctx = make_context();

        // Test 35612 with precision -2 -> 35600
        let args = vec![XPathValue::double(35612.0), XPathValue::integer(-2)];
        let result = round_half_to_even(&mut ctx, args).unwrap();
        match result {
            XPathValue::Item(item) => {
                if let crate::xpath::iterator::XmlItem::Atomic(v) = item {
                    let d = v.as_double().unwrap();
                    assert_eq!(d, 35600.0);
                } else {
                    panic!("Expected atomic value");
                }
            }
            _ => panic!("Expected single item"),
        }
    }
}
