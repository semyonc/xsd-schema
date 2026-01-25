//! XPath 2.0 sequence functions.
//!
//! This module implements sequence functions from the XPath 2.0 specification:
//! - fn:index-of
//! - fn:remove
//! - fn:insert-before
//! - fn:subsequence
//! - fn:unordered
//! - fn:deep-equal

use num_bigint::BigInt;
use rust_decimal::prelude::ToPrimitive;

use crate::types::value::XmlValue;
use crate::types::XmlTypeCode;
use crate::xpath::context::DynamicContext;
use crate::xpath::error::XPathError;
use crate::xpath::iterator::{VecNodeIterator, XmlItem};
use crate::xpath::tree_comparer::TreeComparer;
use crate::xpath::DomNavigator;

use super::{atomize_sequence, atomize_to_double, atomize_to_single, atomize_to_single_opt, atomize_to_string_opt, materialize, XPathValue};

/// Default collation URI (codepoint collation).
const DEFAULT_COLLATION: &str = "http://www.w3.org/2005/xpath-functions/collation/codepoint";

/// Validate collation URI - only default collation is supported.
/// Returns Ok(()) if collation is valid (default or empty), FOCH0002 otherwise.
fn validate_collation(collation: Option<&str>) -> Result<(), XPathError> {
    match collation {
        None => Ok(()),
        Some(c) if c.is_empty() || c == DEFAULT_COLLATION => Ok(()),
        Some(c) => Err(XPathError::unknown_collation(c)),
    }
}

// ============================================================================
// fn:index-of($seq as xs:anyAtomicType*, $search as xs:anyAtomicType,
//             $collation as xs:string?) as xs:integer*
// ============================================================================

/// Implements fn:index-of - returns positions of matching items in a sequence.
///
/// Returns a sequence of positive integers giving the positions of items in $seq
/// that are equal to $search.
pub fn index_of<N: DomNavigator>(
    _context: &mut DynamicContext<'_, N>,
    mut args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if args.len() < 2 || args.len() > 3 {
        return Err(XPathError::wrong_number_of_arguments("index-of", 2, args.len()));
    }

    // Get the sequence (arg 0) and search value (arg 1)
    let seq = args.remove(0);
    let search_arg = args.remove(0);
    // Collation (arg 2) is ignored for now

    // Atomize both
    let seq_values = atomize_sequence(seq)?;
    let search_value = match atomize_to_single_opt(search_arg)? {
        None => return Ok(XPathValue::Empty),
        Some(value) => value,
    };

    // Find matching positions (1-based)
    let mut positions = Vec::new();
    for (idx, item) in seq_values.iter().enumerate() {
        if values_equal(item, &search_value) {
            positions.push(XmlItem::Atomic(XmlValue::integer(BigInt::from(idx + 1))));
        }
    }

    Ok(XPathValue::from_sequence(positions))
}

/// Compare two atomic values for equality (used by index-of and distinct-values).
/// Normalizes UntypedAtomic and AnyUri to string for comparison.
/// Applies numeric type promotion for comparing different numeric types.
fn values_equal(left: &XmlValue, right: &XmlValue) -> bool {
    let left_norm = normalize_for_comparison(left);
    let right_norm = normalize_for_comparison(right);

    // Numeric type promotion: compare numerics as doubles
    if left_norm.type_code.is_numeric() && right_norm.type_code.is_numeric() {
        return numeric_values_equal(&left_norm, &right_norm);
    }

    // Use value equality for non-numeric types
    left_norm == right_norm
}

/// Compare two numeric values for equality using double promotion.
/// NaN is not equal to NaN for value comparison per XPath spec.
fn numeric_values_equal(left: &XmlValue, right: &XmlValue) -> bool {
    match (left.as_double(), right.as_double()) {
        (Some(l), Some(r)) => {
            // NaN != NaN for value comparison
            if l.is_nan() || r.is_nan() {
                return false;
            }
            // Compare with epsilon for floating point tolerance
            (l - r).abs() < f64::EPSILON || l == r
        }
        _ => false,
    }
}

/// Normalize a value for comparison (UntypedAtomic and AnyUri become string).
fn normalize_for_comparison(value: &XmlValue) -> XmlValue {
    match value.type_code {
        XmlTypeCode::UntypedAtomic | XmlTypeCode::AnyUri => {
            XmlValue::string(value.to_string_value())
        }
        _ => value.clone(),
    }
}

// ============================================================================
// fn:reverse($arg as item()*) as item()*
// ============================================================================

/// Implements fn:reverse - reverses the order of items in a sequence.
pub fn reverse<N: DomNavigator>(
    _context: &mut DynamicContext<'_, N>,
    mut args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if args.len() != 1 {
        return Err(XPathError::wrong_number_of_arguments("reverse", 1, args.len()));
    }

    let mut items = materialize(args.remove(0));
    items.reverse();
    Ok(XPathValue::from_sequence(items))
}

// ============================================================================
// fn:zero-or-one($arg as item()*) as item()?
// ============================================================================

/// Implements fn:zero-or-one - returns the argument if it contains zero or one items.
///
/// Raises FORG0003 if the argument contains more than one item.
pub fn zero_or_one<N: DomNavigator>(
    _context: &mut DynamicContext<'_, N>,
    mut args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if args.len() != 1 {
        return Err(XPathError::wrong_number_of_arguments("zero-or-one", 1, args.len()));
    }

    let arg = args.remove(0);
    if arg.len() > 1 {
        return Err(XPathError::FORG0003);
    }
    Ok(arg)
}

// ============================================================================
// fn:one-or-more($arg as item()*) as item()+
// ============================================================================

/// Implements fn:one-or-more - returns the argument if it contains one or more items.
///
/// Raises FORG0004 if the argument is an empty sequence.
pub fn one_or_more<N: DomNavigator>(
    _context: &mut DynamicContext<'_, N>,
    mut args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if args.len() != 1 {
        return Err(XPathError::wrong_number_of_arguments("one-or-more", 1, args.len()));
    }

    let arg = args.remove(0);
    if arg.is_empty() {
        return Err(XPathError::FORG0004);
    }
    Ok(arg)
}

// ============================================================================
// fn:exactly-one($arg as item()*) as item()
// ============================================================================

/// Implements fn:exactly-one - returns the argument if it contains exactly one item.
///
/// Raises FORG0005 if the argument does not contain exactly one item.
pub fn exactly_one<N: DomNavigator>(
    _context: &mut DynamicContext<'_, N>,
    mut args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if args.len() != 1 {
        return Err(XPathError::wrong_number_of_arguments("exactly-one", 1, args.len()));
    }

    let arg = args.remove(0);
    if arg.len() != 1 {
        return Err(XPathError::FORG0005);
    }
    Ok(arg)
}

// ============================================================================
// fn:distinct-values($arg as xs:anyAtomicType*, $collation as xs:string?) as xs:anyAtomicType*
// ============================================================================

/// Implements fn:distinct-values - returns unique values from a sequence.
///
/// Returns the values that appear in the argument with duplicates removed.
/// Uses value equality with numeric type promotion.
pub fn distinct_values<N: DomNavigator>(
    _context: &mut DynamicContext<'_, N>,
    mut args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if args.is_empty() || args.len() > 2 {
        return Err(XPathError::wrong_number_of_arguments("distinct-values", 1, args.len()));
    }

    let seq = args.remove(0);
    // Collation (arg 1) is ignored for now

    // Atomize the sequence
    let values = atomize_sequence(seq)?;

    if values.is_empty() {
        return Ok(XPathValue::Empty);
    }

    // Remove duplicates using values_equal for comparison
    let mut distinct: Vec<XmlValue> = Vec::new();
    for value in values {
        let is_duplicate = distinct.iter().any(|existing| values_equal(existing, &value));
        if !is_duplicate {
            distinct.push(value);
        }
    }

    // Convert back to XPathValue
    let items: Vec<XmlItem<N>> = distinct
        .into_iter()
        .map(XmlItem::Atomic)
        .collect();

    Ok(XPathValue::from_sequence(items))
}

// ============================================================================
// fn:remove($target as item()*, $position as xs:integer) as item()*
// ============================================================================

/// Implements fn:remove - removes an item from a sequence at a given position.
///
/// Returns a new sequence with the item at the specified position removed.
/// If $position is less than 1 or greater than the length of $target,
/// the sequence is returned unchanged.
pub fn remove<N: DomNavigator>(
    _context: &mut DynamicContext<'_, N>,
    mut args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if args.len() != 2 {
        return Err(XPathError::wrong_number_of_arguments("remove", 2, args.len()));
    }

    let target = args.remove(0);
    let position_arg = args.remove(0);

    // Get position as integer
    let position_value = atomize_to_single(position_arg)?;
    let position = position_value
        .as_integer()
        .and_then(|i| i.to_i64())
        .ok_or_else(|| XPathError::XPTY0004 {
            expected: "xs:integer".to_string(),
            found: format!("{:?}", position_value.type_code),
        })?;

    // Materialize target sequence
    let mut items = materialize(target);

    // If position is out of range, return sequence unchanged
    if position < 1 || position as usize > items.len() {
        return Ok(XPathValue::from_sequence(items));
    }

    // Remove item at position (convert 1-based to 0-based)
    items.remove((position - 1) as usize);

    Ok(XPathValue::from_sequence(items))
}

// ============================================================================
// fn:insert-before($target as item()*, $position as xs:integer,
//                  $inserts as item()*) as item()*
// ============================================================================

/// Implements fn:insert-before - inserts items into a sequence.
///
/// Returns a new sequence with $inserts inserted before the item at $position.
/// If $position < 1, inserts at the beginning.
/// If $position > length + 1, inserts at the end.
pub fn insert_before<N: DomNavigator>(
    _context: &mut DynamicContext<'_, N>,
    mut args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if args.len() != 3 {
        return Err(XPathError::wrong_number_of_arguments("insert-before", 3, args.len()));
    }

    let target = args.remove(0);
    let position_arg = args.remove(0);
    let inserts = args.remove(0);

    // Get position as integer
    let position_value = atomize_to_single(position_arg)?;
    let position = position_value
        .as_integer()
        .and_then(|i| i.to_i64())
        .ok_or_else(|| XPathError::XPTY0004 {
            expected: "xs:integer".to_string(),
            found: format!("{:?}", position_value.type_code),
        })?;

    // Materialize both sequences
    let mut target_items = materialize(target);
    let insert_items = materialize(inserts);

    // Adjust position: if < 1, use 1; if > len+1, use len+1
    let len = target_items.len();
    let adjusted_pos = if position < 1 {
        0
    } else if position as usize > len {
        len
    } else {
        (position - 1) as usize
    };

    // Build result: items[0..pos] + inserts + items[pos..]
    let mut result = Vec::with_capacity(target_items.len() + insert_items.len());
    result.extend(target_items.drain(..adjusted_pos));
    result.extend(insert_items);
    result.extend(target_items);

    Ok(XPathValue::from_sequence(result))
}

// ============================================================================
// fn:subsequence($sourceSeq as item()*, $startingLoc as xs:double,
//                $length as xs:double?) as item()*
// ============================================================================

/// Implements fn:subsequence - returns a contiguous subsequence.
///
/// Returns items from $sourceSeq starting at position $startingLoc
/// and continuing for $length items (or to the end if $length is omitted).
///
/// Uses XPath 2.0 rounding rules:
/// - Positions are doubles, rounded to integers
/// - NaN startingLoc or length -> empty
/// - +Infinity startingLoc -> empty
/// - -Infinity length -> empty
pub fn subsequence<N: DomNavigator>(
    _context: &mut DynamicContext<'_, N>,
    mut args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if args.is_empty() || args.len() > 3 {
        return Err(XPathError::wrong_number_of_arguments("subsequence", 2, args.len()));
    }

    let source = args.remove(0);
    let starting_loc_arg = args.remove(0);
    let length_arg = if !args.is_empty() {
        Some(args.remove(0))
    } else {
        None
    };

    // Get starting location as double
    let starting_loc = atomize_to_double(starting_loc_arg)?;

    // Get optional length as double
    let length = match length_arg {
        Some(arg) => Some(atomize_to_double(arg)?),
        None => None,
    };

    // Handle NaN cases
    if starting_loc.is_nan() {
        return Ok(XPathValue::Empty);
    }
    if let Some(len) = length {
        if len.is_nan() {
            return Ok(XPathValue::Empty);
        }
    }

    // Handle infinity cases
    if starting_loc.is_infinite() && starting_loc.is_sign_positive() {
        return Ok(XPathValue::Empty);
    }
    if let Some(len) = length {
        if len.is_infinite() && len.is_sign_negative() {
            return Ok(XPathValue::Empty);
        }
    }

    // Materialize source sequence
    let items = materialize(source);

    // Round starting location (XPath uses round-half-to-even, but round() is close enough)
    let start_rounded = round_half_away_from_zero(starting_loc);

    // Calculate effective start and end positions
    let (start_idx, end_idx) = match length {
        Some(len) => {
            let len_rounded = round_half_away_from_zero(len);
            // Per spec: items where round(startingLoc) <= position < round(startingLoc) + round(length)
            // Note: position is 1-based, so item at position p has index p-1

            // Handle negative start adjusting length
            let effective_start = if start_rounded < 1.0 {
                // If start is negative, we skip fewer items but the length is reduced
                1.0
            } else {
                start_rounded
            };

            // Calculate length adjustment for negative start
            let adjusted_len = if start_rounded < 1.0 {
                len_rounded + start_rounded - 1.0
            } else {
                len_rounded
            };

            if adjusted_len <= 0.0 {
                return Ok(XPathValue::Empty);
            }

            let start = (effective_start - 1.0).max(0.0) as usize;
            let end = (effective_start - 1.0 + adjusted_len).min(items.len() as f64) as usize;
            (start, end)
        }
        None => {
            // No length specified - go to end
            if start_rounded < 1.0 {
                (0, items.len())
            } else {
                let start = (start_rounded - 1.0).max(0.0) as usize;
                (start, items.len())
            }
        }
    };

    // Handle out of range
    if start_idx >= items.len() {
        return Ok(XPathValue::Empty);
    }

    // Extract subsequence
    let result: Vec<XmlItem<N>> = items.into_iter()
        .skip(start_idx)
        .take(end_idx.saturating_sub(start_idx))
        .collect();

    Ok(XPathValue::from_sequence(result))
}

/// Round half away from zero (XPath round semantics).
fn round_half_away_from_zero(d: f64) -> f64 {
    if d.is_nan() || d.is_infinite() {
        return d;
    }
    if d >= 0.0 {
        (d + 0.5).floor()
    } else {
        (d - 0.5).ceil()
    }
}

// ============================================================================
// fn:unordered($sourceSeq as item()*) as item()*
// ============================================================================

/// Implements fn:unordered - returns the sequence in implementation-defined order.
///
/// This is an optimization hint; implementations may return items in any order.
/// Our implementation simply returns the input unchanged.
pub fn unordered<N: DomNavigator>(
    _context: &mut DynamicContext<'_, N>,
    mut args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if args.len() != 1 {
        return Err(XPathError::wrong_number_of_arguments("unordered", 1, args.len()));
    }

    // Simply return the input unchanged
    Ok(args.remove(0))
}

// ============================================================================
// fn:deep-equal($parameter1 as item()*, $parameter2 as item()*,
//               $collation as xs:string?) as xs:boolean
// ============================================================================

/// Implements fn:deep-equal - tests whether two sequences are deep-equal.
///
/// Two sequences are deep-equal if they have the same length and each pair
/// of corresponding items are deep-equal.
pub fn deep_equal<N: DomNavigator>(
    _context: &mut DynamicContext<'_, N>,
    mut args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if args.len() < 2 || args.len() > 3 {
        return Err(XPathError::wrong_number_of_arguments("deep-equal", 2, args.len()));
    }

    // Validate collation if provided (third argument)
    if args.len() == 3 {
        let collation_arg = args.pop().unwrap();
        let collation = atomize_to_string_opt(collation_arg)?;
        validate_collation(collation.as_deref())?;
    }

    let param1 = args.remove(0);
    let param2 = args.remove(0);

    // Materialize both sequences
    let items1 = materialize(param1);
    let items2 = materialize(param2);

    // Create VecNodeIterators for comparison
    let iter1: VecNodeIterator<N> = VecNodeIterator::new(items1);
    let iter2: VecNodeIterator<N> = VecNodeIterator::new(items2);

    // Use TreeComparer for deep equality
    let comparer = TreeComparer::default();
    let result = comparer.deep_equal_iter(&iter1, &iter2)?;

    Ok(XPathValue::boolean(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::namespace::table::NameTable;
    use crate::xpath::RoXmlNavigator;
    use crate::xpath::context::XPathContext;
    use crate::xpath::iterator::XmlItem;

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

    fn extract_integers<N: DomNavigator>(value: XPathValue<N>) -> Vec<i64> {
        match value {
            XPathValue::Empty => vec![],
            XPathValue::Item(item) => {
                if let XmlItem::Atomic(v) = item {
                    vec![v.as_integer().and_then(|i| i.to_i64()).unwrap()]
                } else {
                    vec![]
                }
            }
            XPathValue::Sequence(items) => items
                .into_iter()
                .filter_map(|item| {
                    if let XmlItem::Atomic(v) = item {
                        v.as_integer().and_then(|i| i.to_i64())
                    } else {
                        None
                    }
                })
                .collect(),
        }
    }

    fn extract_bool<N: DomNavigator>(value: XPathValue<N>) -> bool {
        match value {
            XPathValue::Item(XmlItem::Atomic(v)) => v.as_boolean().unwrap_or(false),
            _ => false,
        }
    }

    // ========== index-of tests ==========

    #[test]
    fn test_index_of_multiple_matches() {
        let mut ctx = make_context();
        let seq = integer_seq::<RoXmlNavigator>(&[10, 20, 30, 20]);
        let search = XPathValue::integer(20);
        let args = vec![seq, search];
        let result = index_of(&mut ctx, args).unwrap();
        assert_eq!(extract_integers(result), vec![2, 4]);
    }

    #[test]
    fn test_index_of_no_match() {
        let mut ctx = make_context();
        let seq = integer_seq::<RoXmlNavigator>(&[10, 20, 30]);
        let search = XPathValue::integer(40);
        let args = vec![seq, search];
        let result = index_of(&mut ctx, args).unwrap();
        assert_eq!(extract_integers(result), Vec::<i64>::new());
    }

    #[test]
    fn test_index_of_string_matches() {
        let mut ctx = make_context();
        let items: Vec<XmlItem<RoXmlNavigator>> = vec!["a", "b", "c", "b"]
            .into_iter()
            .map(|s| XmlItem::Atomic(XmlValue::string(s)))
            .collect();
        let seq = XPathValue::from_sequence(items);
        let search = XPathValue::string("b");
        let args = vec![seq, search];
        let result = index_of(&mut ctx, args).unwrap();
        assert_eq!(extract_integers(result), vec![2, 4]);
    }

    #[test]
    fn test_index_of_empty_sequence() {
        let mut ctx = make_context();
        let seq = XPathValue::<RoXmlNavigator>::Empty;
        let search = XPathValue::integer(1);
        let args = vec![seq, search];
        let result = index_of(&mut ctx, args).unwrap();
        assert!(result.is_empty());
    }

    // ========== remove tests ==========

    #[test]
    fn test_remove_middle() {
        let mut ctx = make_context();
        let seq = integer_seq::<RoXmlNavigator>(&[1, 2, 3]);
        let pos = XPathValue::integer(2);
        let args = vec![seq, pos];
        let result = remove(&mut ctx, args).unwrap();
        assert_eq!(extract_integers(result), vec![1, 3]);
    }

    #[test]
    fn test_remove_out_of_range_low() {
        let mut ctx = make_context();
        let seq = integer_seq::<RoXmlNavigator>(&[1, 2, 3]);
        let pos = XPathValue::integer(0);
        let args = vec![seq, pos];
        let result = remove(&mut ctx, args).unwrap();
        assert_eq!(extract_integers(result), vec![1, 2, 3]);
    }

    #[test]
    fn test_remove_out_of_range_high() {
        let mut ctx = make_context();
        let seq = integer_seq::<RoXmlNavigator>(&[1, 2, 3]);
        let pos = XPathValue::integer(10);
        let args = vec![seq, pos];
        let result = remove(&mut ctx, args).unwrap();
        assert_eq!(extract_integers(result), vec![1, 2, 3]);
    }

    // ========== insert-before tests ==========

    #[test]
    fn test_insert_before_middle() {
        let mut ctx = make_context();
        let target = integer_seq::<RoXmlNavigator>(&[1, 3]);
        let pos = XPathValue::integer(2);
        let inserts = XPathValue::integer(2);
        let args = vec![target, pos, inserts];
        let result = insert_before(&mut ctx, args).unwrap();
        assert_eq!(extract_integers(result), vec![1, 2, 3]);
    }

    #[test]
    fn test_insert_before_position_less_than_one() {
        let mut ctx = make_context();
        let target = integer_seq::<RoXmlNavigator>(&[2, 3]);
        let pos = XPathValue::integer(0);
        let inserts = XPathValue::integer(1);
        let args = vec![target, pos, inserts];
        let result = insert_before(&mut ctx, args).unwrap();
        assert_eq!(extract_integers(result), vec![1, 2, 3]);
    }

    #[test]
    fn test_insert_before_position_beyond_end() {
        let mut ctx = make_context();
        let target = integer_seq::<RoXmlNavigator>(&[1, 2]);
        let pos = XPathValue::integer(10);
        let inserts = XPathValue::integer(3);
        let args = vec![target, pos, inserts];
        let result = insert_before(&mut ctx, args).unwrap();
        assert_eq!(extract_integers(result), vec![1, 2, 3]);
    }

    // ========== subsequence tests ==========

    #[test]
    fn test_subsequence_with_length() {
        let mut ctx = make_context();
        let seq = integer_seq::<RoXmlNavigator>(&[1, 2, 3, 4, 5]);
        let start = XPathValue::double(2.0);
        let len = XPathValue::double(3.0);
        let args = vec![seq, start, len];
        let result = subsequence(&mut ctx, args).unwrap();
        assert_eq!(extract_integers(result), vec![2, 3, 4]);
    }

    #[test]
    fn test_subsequence_without_length() {
        let mut ctx = make_context();
        let seq = integer_seq::<RoXmlNavigator>(&[1, 2, 3, 4, 5]);
        let start = XPathValue::double(3.0);
        let args = vec![seq, start];
        let result = subsequence(&mut ctx, args).unwrap();
        assert_eq!(extract_integers(result), vec![3, 4, 5]);
    }

    #[test]
    fn test_subsequence_negative_start() {
        let mut ctx = make_context();
        let seq = integer_seq::<RoXmlNavigator>(&[1, 2, 3]);
        let start = XPathValue::double(-1.0);
        let len = XPathValue::double(4.0);
        let args = vec![seq, start, len];
        let result = subsequence(&mut ctx, args).unwrap();
        // start=-1, len=4: positions where -1 <= pos < 3, i.e., pos 1 and 2
        assert_eq!(extract_integers(result), vec![1, 2]);
    }

    #[test]
    fn test_subsequence_nan_start() {
        let mut ctx = make_context();
        let seq = integer_seq::<RoXmlNavigator>(&[1, 2, 3]);
        let start = XPathValue::double(f64::NAN);
        let len = XPathValue::double(2.0);
        let args = vec![seq, start, len];
        let result = subsequence(&mut ctx, args).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_subsequence_rounding() {
        let mut ctx = make_context();
        // subsequence((1,2,3,4,5), 1.5, 2.6) should round to start=2, len=3 -> items 2,3,4
        let seq = integer_seq::<RoXmlNavigator>(&[1, 2, 3, 4, 5]);
        let start = XPathValue::double(1.5);
        let len = XPathValue::double(2.6);
        let args = vec![seq, start, len];
        let result = subsequence(&mut ctx, args).unwrap();
        assert_eq!(extract_integers(result), vec![2, 3, 4]);
    }

    // ========== unordered tests ==========

    #[test]
    fn test_unordered_passthrough() {
        let mut ctx = make_context();
        let seq = integer_seq::<RoXmlNavigator>(&[1, 2, 3]);
        let args = vec![seq];
        let result = unordered(&mut ctx, args).unwrap();
        assert_eq!(extract_integers(result), vec![1, 2, 3]);
    }

    // ========== index-of with numeric type promotion tests ==========

    #[test]
    fn test_index_of_integer_matches_double() {
        let mut ctx = make_context();
        // Sequence of integers
        let seq = integer_seq::<RoXmlNavigator>(&[10, 20, 30]);
        // Search for 20.0 (double)
        let search = XPathValue::double(20.0);
        let args = vec![seq, search];
        let result = index_of(&mut ctx, args).unwrap();
        // Should find 20 at position 2
        assert_eq!(extract_integers(result), vec![2]);
    }

    #[test]
    fn test_index_of_double_matches_integer() {
        let mut ctx = make_context();
        // Sequence with a double
        let items: Vec<XmlItem<RoXmlNavigator>> = vec![
            XmlItem::Atomic(XmlValue::double(10.0)),
            XmlItem::Atomic(XmlValue::double(20.0)),
            XmlItem::Atomic(XmlValue::double(30.0)),
        ];
        let seq = XPathValue::from_sequence(items);
        // Search for 20 (integer)
        let search = XPathValue::integer(20);
        let args = vec![seq, search];
        let result = index_of(&mut ctx, args).unwrap();
        // Should find 20.0 at position 2
        assert_eq!(extract_integers(result), vec![2]);
    }

    #[test]
    fn test_index_of_nan_not_equal() {
        let mut ctx = make_context();
        // Sequence with NaN
        let items: Vec<XmlItem<RoXmlNavigator>> = vec![
            XmlItem::Atomic(XmlValue::double(f64::NAN)),
            XmlItem::Atomic(XmlValue::double(1.0)),
        ];
        let seq = XPathValue::from_sequence(items);
        // Search for NaN
        let search = XPathValue::double(f64::NAN);
        let args = vec![seq, search];
        let result = index_of(&mut ctx, args).unwrap();
        // NaN should not match NaN for value comparison
        assert!(result.is_empty());
    }

    // ========== reverse tests ==========

    #[test]
    fn test_reverse_sequence() {
        let mut ctx = make_context();
        let seq = integer_seq::<RoXmlNavigator>(&[1, 2, 3, 4, 5]);
        let args = vec![seq];
        let result = reverse(&mut ctx, args).unwrap();
        assert_eq!(extract_integers(result), vec![5, 4, 3, 2, 1]);
    }

    #[test]
    fn test_reverse_empty() {
        let mut ctx = make_context();
        let seq = XPathValue::<RoXmlNavigator>::Empty;
        let args = vec![seq];
        let result = reverse(&mut ctx, args).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_reverse_single() {
        let mut ctx = make_context();
        let seq = XPathValue::integer(42);
        let args = vec![seq];
        let result = reverse(&mut ctx, args).unwrap();
        assert_eq!(extract_integers(result), vec![42]);
    }

    // ========== zero-or-one tests ==========

    #[test]
    fn test_zero_or_one_empty() {
        let mut ctx = make_context();
        let seq = XPathValue::<RoXmlNavigator>::Empty;
        let args = vec![seq];
        let result = zero_or_one(&mut ctx, args).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_zero_or_one_single() {
        let mut ctx = make_context();
        let seq = XPathValue::integer(42);
        let args = vec![seq];
        let result = zero_or_one(&mut ctx, args).unwrap();
        assert_eq!(extract_integers(result), vec![42]);
    }

    #[test]
    fn test_zero_or_one_multiple_fails() {
        let mut ctx = make_context();
        let seq = integer_seq::<RoXmlNavigator>(&[1, 2]);
        let args = vec![seq];
        let result = zero_or_one(&mut ctx, args);
        match result {
            Err(e) => assert_eq!(e.error_code(), Some("FORG0003")),
            Ok(_) => panic!("Expected FORG0003 error"),
        }
    }

    // ========== one-or-more tests ==========

    #[test]
    fn test_one_or_more_single() {
        let mut ctx = make_context();
        let seq = XPathValue::integer(42);
        let args = vec![seq];
        let result = one_or_more(&mut ctx, args).unwrap();
        assert_eq!(extract_integers(result), vec![42]);
    }

    #[test]
    fn test_one_or_more_multiple() {
        let mut ctx = make_context();
        let seq = integer_seq::<RoXmlNavigator>(&[1, 2, 3]);
        let args = vec![seq];
        let result = one_or_more(&mut ctx, args).unwrap();
        assert_eq!(extract_integers(result), vec![1, 2, 3]);
    }

    #[test]
    fn test_one_or_more_empty_fails() {
        let mut ctx = make_context();
        let seq = XPathValue::<RoXmlNavigator>::Empty;
        let args = vec![seq];
        let result = one_or_more(&mut ctx, args);
        match result {
            Err(e) => assert_eq!(e.error_code(), Some("FORG0004")),
            Ok(_) => panic!("Expected FORG0004 error"),
        }
    }

    // ========== exactly-one tests ==========

    #[test]
    fn test_exactly_one_single() {
        let mut ctx = make_context();
        let seq = XPathValue::integer(42);
        let args = vec![seq];
        let result = exactly_one(&mut ctx, args).unwrap();
        assert_eq!(extract_integers(result), vec![42]);
    }

    #[test]
    fn test_exactly_one_empty_fails() {
        let mut ctx = make_context();
        let seq = XPathValue::<RoXmlNavigator>::Empty;
        let args = vec![seq];
        let result = exactly_one(&mut ctx, args);
        match result {
            Err(e) => assert_eq!(e.error_code(), Some("FORG0005")),
            Ok(_) => panic!("Expected FORG0005 error"),
        }
    }

    #[test]
    fn test_exactly_one_multiple_fails() {
        let mut ctx = make_context();
        let seq = integer_seq::<RoXmlNavigator>(&[1, 2]);
        let args = vec![seq];
        let result = exactly_one(&mut ctx, args);
        match result {
            Err(e) => assert_eq!(e.error_code(), Some("FORG0005")),
            Ok(_) => panic!("Expected FORG0005 error"),
        }
    }

    // ========== distinct-values tests ==========

    #[test]
    fn test_distinct_values_integers() {
        let mut ctx = make_context();
        let seq = integer_seq::<RoXmlNavigator>(&[1, 2, 1, 3, 2, 1]);
        let args = vec![seq];
        let result = distinct_values(&mut ctx, args).unwrap();
        assert_eq!(extract_integers(result), vec![1, 2, 3]);
    }

    #[test]
    fn test_distinct_values_empty() {
        let mut ctx = make_context();
        let seq = XPathValue::<RoXmlNavigator>::Empty;
        let args = vec![seq];
        let result = distinct_values(&mut ctx, args).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_distinct_values_mixed_numeric() {
        let mut ctx = make_context();
        // Mix of integers and doubles that are equal
        let items: Vec<XmlItem<RoXmlNavigator>> = vec![
            XmlItem::Atomic(XmlValue::integer(BigInt::from(1))),
            XmlItem::Atomic(XmlValue::double(2.0)),
            XmlItem::Atomic(XmlValue::integer(BigInt::from(1))), // duplicate of 1
            XmlItem::Atomic(XmlValue::double(2.0)), // duplicate of 2.0
            XmlItem::Atomic(XmlValue::integer(BigInt::from(3))),
        ];
        let seq = XPathValue::from_sequence(items);
        let args = vec![seq];
        let result = distinct_values(&mut ctx, args).unwrap();
        // Should have 3 distinct values: 1, 2.0, 3
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn test_distinct_values_strings() {
        let mut ctx = make_context();
        let items: Vec<XmlItem<RoXmlNavigator>> = vec!["a", "b", "a", "c", "b"]
            .into_iter()
            .map(|s| XmlItem::Atomic(XmlValue::string(s)))
            .collect();
        let seq = XPathValue::from_sequence(items);
        let args = vec![seq];
        let result = distinct_values(&mut ctx, args).unwrap();
        // Should have 3 distinct values: a, b, c
        assert_eq!(result.len(), 3);
    }

    // ========== deep-equal tests ==========

    #[test]
    fn test_deep_equal_same_integers() {
        let mut ctx = make_context();
        let seq1 = integer_seq::<RoXmlNavigator>(&[1, 2, 3]);
        let seq2 = integer_seq::<RoXmlNavigator>(&[1, 2, 3]);
        let args = vec![seq1, seq2];
        let result = deep_equal(&mut ctx, args).unwrap();
        assert!(extract_bool(result));
    }

    #[test]
    fn test_deep_equal_different_integers() {
        let mut ctx = make_context();
        let seq1 = integer_seq::<RoXmlNavigator>(&[1, 2, 3]);
        let seq2 = integer_seq::<RoXmlNavigator>(&[1, 2, 4]);
        let args = vec![seq1, seq2];
        let result = deep_equal(&mut ctx, args).unwrap();
        assert!(!extract_bool(result));
    }

    #[test]
    fn test_deep_equal_nan() {
        let mut ctx = make_context();
        let seq1: XPathValue<RoXmlNavigator> = XPathValue::double(f64::NAN);
        let seq2: XPathValue<RoXmlNavigator> = XPathValue::double(f64::NAN);
        let args = vec![seq1, seq2];
        let result = deep_equal(&mut ctx, args).unwrap();
        // XPath deep-equal treats NaN as equal to NaN
        assert!(extract_bool(result));
    }

    #[test]
    fn test_deep_equal_empty_sequences() {
        let mut ctx = make_context();
        let seq1 = XPathValue::<RoXmlNavigator>::Empty;
        let seq2 = XPathValue::<RoXmlNavigator>::Empty;
        let args = vec![seq1, seq2];
        let result = deep_equal(&mut ctx, args).unwrap();
        assert!(extract_bool(result));
    }

    #[test]
    fn test_deep_equal_different_lengths() {
        let mut ctx = make_context();
        let seq1 = integer_seq::<RoXmlNavigator>(&[1, 2]);
        let seq2 = integer_seq::<RoXmlNavigator>(&[1, 2, 3]);
        let args = vec![seq1, seq2];
        let result = deep_equal(&mut ctx, args).unwrap();
        assert!(!extract_bool(result));
    }
}
