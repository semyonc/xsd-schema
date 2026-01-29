//! XPath 2.0 regex functions.
//!
//! This module implements:
//! - fn:matches($input, $pattern, $flags?) - test if string matches pattern
//! - fn:replace($input, $pattern, $replacement, $flags?) - replace matches
//! - fn:tokenize($input, $pattern, $flags?) - split string by pattern
//!
//! Uses the `regex` crate for pattern matching.

use regex::{Regex, RegexBuilder};

use crate::regex_convert::{convert_xml_pattern, ConvertOptions};
use crate::xpath::context::DynamicContext;
use crate::xpath::error::XPathError;
use crate::xpath::DomNavigator;

use super::{atomize_to_string, atomize_to_string_opt, atomize_to_string_required, XPathValue};
use crate::xpath::iterator::XmlItem;
use crate::types::value::XmlValue;

/// fn:matches($input as xs:string?, $pattern as xs:string, $flags as xs:string?) as xs:boolean
///
/// Returns true if $input matches the regular expression $pattern.
///
/// - If $input is empty, it is treated as empty string.
/// - FORX0001 if $flags contains invalid characters.
/// - FORX0002 if $pattern is not a valid regular expression.
pub fn matches<N: DomNavigator>(
    _context: &mut DynamicContext<'_, N>,
    mut args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if args.len() < 2 || args.len() > 3 {
        return Err(XPathError::wrong_number_of_arguments("matches", 2, args.len()));
    }

    // Get flags (optional third argument)
    let flags = if args.len() == 3 {
        atomize_to_string_opt(args.pop().unwrap())?
    } else {
        None
    };

    // Get pattern (second argument)
    let pattern = atomize_to_string_required(args.pop().unwrap())?;

    // Get input (first argument)
    let input = atomize_to_string(args.pop().unwrap())?;

    // Build the regex
    let regex = build_regex(&pattern, flags.as_deref().unwrap_or(""))?;

    // Test if input matches
    let result = regex.is_match(&input);

    Ok(XPathValue::boolean(result))
}

/// fn:replace($input as xs:string?, $pattern as xs:string, $replacement as xs:string,
///            $flags as xs:string?) as xs:string
///
/// Replaces all occurrences of $pattern in $input with $replacement.
///
/// - FORX0001 if $flags contains invalid characters.
/// - FORX0002 if $pattern is not a valid regular expression.
/// - FORX0003 if $pattern matches a zero-length string.
/// - FORX0004 if $replacement has invalid syntax.
pub fn replace<N: DomNavigator>(
    _context: &mut DynamicContext<'_, N>,
    mut args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if args.len() < 3 || args.len() > 4 {
        return Err(XPathError::wrong_number_of_arguments("replace", 3, args.len()));
    }

    // Get flags (optional fourth argument)
    let flags = if args.len() == 4 {
        atomize_to_string_opt(args.pop().unwrap())?
    } else {
        None
    };

    // Get replacement (third argument)
    let replacement = atomize_to_string_required(args.pop().unwrap())?;

    // Get pattern (second argument)
    let pattern = atomize_to_string_required(args.pop().unwrap())?;

    // Get input (first argument)
    let input = atomize_to_string(args.pop().unwrap())?;

    // Validate replacement string syntax
    validate_replacement_string(&replacement)?;

    // Build the regex
    let regex = build_regex(&pattern, flags.as_deref().unwrap_or(""))?;

    // Check if pattern matches zero-length string
    if regex.is_match("") {
        return Err(XPathError::regex_matches_zero_length(&pattern));
    }

    // Convert XPath replacement syntax to Rust regex syntax
    let rust_replacement = convert_replacement(&replacement);

    // Perform replacement
    let result = regex.replace_all(&input, rust_replacement.as_str());

    Ok(XPathValue::string(result.into_owned()))
}

/// fn:tokenize($input as xs:string?, $pattern as xs:string, $flags as xs:string?) as xs:string*
///
/// Splits $input into a sequence of strings using $pattern as delimiter.
///
/// - FORX0001 if $flags contains invalid characters.
/// - FORX0002 if $pattern is not a valid regular expression.
/// - FORX0003 if $pattern matches a zero-length string.
pub fn tokenize<N: DomNavigator>(
    _context: &mut DynamicContext<'_, N>,
    mut args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if args.len() < 2 || args.len() > 3 {
        return Err(XPathError::wrong_number_of_arguments("tokenize", 2, args.len()));
    }

    // Get flags (optional third argument)
    let flags = if args.len() == 3 {
        atomize_to_string_opt(args.pop().unwrap())?
    } else {
        None
    };

    // Get pattern (second argument)
    let pattern = atomize_to_string_required(args.pop().unwrap())?;

    // Get input (first argument)
    let input = atomize_to_string(args.pop().unwrap())?;

    // If input is empty, return empty sequence
    if input.is_empty() {
        return Ok(XPathValue::Empty);
    }

    // Build the regex
    let regex = build_regex(&pattern, flags.as_deref().unwrap_or(""))?;

    // Check if pattern matches zero-length string
    if regex.is_match("") {
        return Err(XPathError::regex_matches_zero_length(&pattern));
    }

    // Split the input
    let tokens: Vec<&str> = regex.split(&input).collect();

    // Convert to XPathValue sequence, filtering out empty tokens
    // (as per XPath 2.0 spec and C# behavior)
    let items: Vec<XmlItem<N>> = tokens
        .into_iter()
        .filter(|s| !s.is_empty())
        .map(|s| XmlItem::Atomic(XmlValue::string(s)))
        .collect();

    Ok(XPathValue::from_sequence(items))
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Build a Regex from an XPath pattern and flags.
///
/// Converts XSD/XPath regex pattern syntax to Rust regex, handling XSD-specific
/// character class escapes like `\i` (initial XML name char) and `\c` (XML name char).
fn build_regex(pattern: &str, flags: &str) -> Result<Regex, XPathError> {
    // Validate flags
    for c in flags.chars() {
        if !matches!(c, 's' | 'm' | 'i' | 'x') {
            return Err(XPathError::invalid_regex_flags(flags));
        }
    }

    // Convert XSD/XPath pattern to Rust regex (unanchored for XPath)
    let rust_pattern = convert_xml_pattern(pattern, ConvertOptions::xpath());

    // Build regex with flags
    let mut builder = RegexBuilder::new(&rust_pattern);

    for c in flags.chars() {
        match c {
            's' => { builder.dot_matches_new_line(true); }
            'm' => { builder.multi_line(true); }
            'i' => { builder.case_insensitive(true); }
            'x' => { builder.ignore_whitespace(true); }
            _ => {} // Already validated above
        }
    }

    builder
        .build()
        .map_err(|_| XPathError::invalid_regex_pattern(pattern))
}

/// Validate XPath replacement string syntax.
///
/// In XPath:
/// - $0, $1-$9 reference captured groups (must have digits)
/// - $$ is a literal $
/// - \\ is a literal \
/// - \$ is a literal $
/// - A $ not followed by digit or $ is an error (FORX0004)
/// - A $ at end of string is an error (FORX0004)
/// - A \ not followed by \ or $ is an error (FORX0004)
/// - A \ at end of string is an error (FORX0004)
fn validate_replacement_string(replacement: &str) -> Result<(), XPathError> {
    let chars: Vec<char> = replacement.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        match chars[i] {
            '$' => {
                // Must be followed by digit or $
                if i + 1 >= chars.len() {
                    // Trailing $ is invalid
                    return Err(XPathError::invalid_replacement_string(replacement));
                }
                let next = chars[i + 1];
                if next == '$' {
                    // $$ - literal $
                    i += 2;
                } else if next.is_ascii_digit() {
                    // $N - group reference, consume all following digits
                    i += 2;
                    while i < chars.len() && chars[i].is_ascii_digit() {
                        i += 1;
                    }
                } else {
                    // $ followed by invalid character
                    return Err(XPathError::invalid_replacement_string(replacement));
                }
            }
            '\\' => {
                // Must be followed by \ or $
                if i + 1 >= chars.len() {
                    // Trailing \ is invalid
                    return Err(XPathError::invalid_replacement_string(replacement));
                }
                let next = chars[i + 1];
                if next != '\\' && next != '$' {
                    return Err(XPathError::invalid_replacement_string(replacement));
                }
                i += 2;
            }
            _ => {
                i += 1;
            }
        }
    }

    Ok(())
}

/// Convert XPath replacement syntax to Rust regex replacement syntax.
///
/// XPath: $1, $2, etc. for groups; $$ for literal $; \\ for literal \; \$ for literal $
/// Rust:  $1, ${1}, etc. for groups; $$ for literal $
fn convert_replacement(replacement: &str) -> String {
    let chars: Vec<char> = replacement.chars().collect();
    let mut result = String::new();
    let mut i = 0;

    while i < chars.len() {
        match chars[i] {
            '$' => {
                if i + 1 < chars.len() {
                    let next = chars[i + 1];
                    if next.is_ascii_digit() {
                        // $N -> ${N} for clarity in Rust regex
                        result.push('$');
                        // Collect all digits for the group number
                        let mut j = i + 1;
                        while j < chars.len() && chars[j].is_ascii_digit() {
                            result.push(chars[j]);
                            j += 1;
                        }
                        i = j;
                    } else if next == '$' {
                        // $$ -> $$ (literal $)
                        result.push_str("$$");
                        i += 2;
                    } else {
                        // Should not happen if validate_replacement_string was called
                        result.push(chars[i]);
                        i += 1;
                    }
                } else {
                    result.push(chars[i]);
                    i += 1;
                }
            }
            '\\' => {
                if i + 1 < chars.len() {
                    let next = chars[i + 1];
                    if next == '\\' {
                        // \\ -> \ (literal backslash)
                        result.push('\\');
                        i += 2;
                    } else if next == '$' {
                        // \$ -> $$ (literal $, escaped in Rust as $$)
                        result.push_str("$$");
                        i += 2;
                    } else {
                        // Should not happen if validate_replacement_string was called
                        result.push(chars[i]);
                        i += 1;
                    }
                } else {
                    result.push(chars[i]);
                    i += 1;
                }
            }
            _ => {
                result.push(chars[i]);
                i += 1;
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::namespace::table::NameTable;
    use crate::xpath::context::XPathContext;
    use crate::xpath::RoXmlNavigator;

    fn create_context<'a>(names: &'a NameTable) -> DynamicContext<'a, RoXmlNavigator<'a>> {
        let static_ctx = XPathContext::new(names);
        let static_ctx = Box::leak(Box::new(static_ctx));
        DynamicContext::new(static_ctx, 0)
    }

    #[test]
    fn test_matches_basic() {
        let names = NameTable::new();
        let mut ctx = create_context(&names);

        let result = matches(
            &mut ctx,
            vec![
                XPathValue::string("abracadabra"),
                XPathValue::string("bra"),
            ],
        ).unwrap();

        assert!(matches!(result, XPathValue::Item(XmlItem::Atomic(v)) if v.as_boolean() == Some(true)));
    }

    #[test]
    fn test_matches_no_match() {
        let names = NameTable::new();
        let mut ctx = create_context(&names);

        let result = matches(
            &mut ctx,
            vec![
                XPathValue::string("abracadabra"),
                XPathValue::string("xyz"),
            ],
        ).unwrap();

        assert!(matches!(result, XPathValue::Item(XmlItem::Atomic(v)) if v.as_boolean() == Some(false)));
    }

    #[test]
    fn test_matches_case_insensitive() {
        let names = NameTable::new();
        let mut ctx = create_context(&names);

        let result = matches(
            &mut ctx,
            vec![
                XPathValue::string("HELLO"),
                XPathValue::string("hello"),
                XPathValue::string("i"),
            ],
        ).unwrap();

        assert!(matches!(result, XPathValue::Item(XmlItem::Atomic(v)) if v.as_boolean() == Some(true)));
    }

    #[test]
    fn test_matches_multiline() {
        let names = NameTable::new();
        let mut ctx = create_context(&names);

        let result = matches(
            &mut ctx,
            vec![
                XPathValue::string("line1\nline2"),
                XPathValue::string("^line2"),
                XPathValue::string("m"),
            ],
        ).unwrap();

        assert!(matches!(result, XPathValue::Item(XmlItem::Atomic(v)) if v.as_boolean() == Some(true)));
    }

    #[test]
    fn test_matches_invalid_flags() {
        let names = NameTable::new();
        let mut ctx = create_context(&names);

        let result = matches(
            &mut ctx,
            vec![
                XPathValue::string("test"),
                XPathValue::string("test"),
                XPathValue::string("z"),
            ],
        );

        assert!(matches!(result, Err(XPathError::FORX0001 { .. })));
    }

    #[test]
    fn test_matches_invalid_pattern() {
        let names = NameTable::new();
        let mut ctx = create_context(&names);

        let result = matches(
            &mut ctx,
            vec![
                XPathValue::string("test"),
                XPathValue::string("[invalid"),
            ],
        );

        assert!(matches!(result, Err(XPathError::FORX0002 { .. })));
    }

    #[test]
    fn test_replace_basic() {
        let names = NameTable::new();
        let mut ctx = create_context(&names);

        let result = replace(
            &mut ctx,
            vec![
                XPathValue::string("abracadabra"),
                XPathValue::string("a"),
                XPathValue::string("X"),
            ],
        ).unwrap();

        if let XPathValue::Item(XmlItem::Atomic(v)) = result {
            assert_eq!(v.as_string(), Some("XbrXcXdXbrX"));
        } else {
            panic!("Expected string");
        }
    }

    #[test]
    fn test_replace_with_groups() {
        let names = NameTable::new();
        let mut ctx = create_context(&names);

        let result = replace(
            &mut ctx,
            vec![
                XPathValue::string("hello world"),
                XPathValue::string("(\\w+) (\\w+)"),
                XPathValue::string("$2 $1"),
            ],
        ).unwrap();

        if let XPathValue::Item(XmlItem::Atomic(v)) = result {
            assert_eq!(v.as_string(), Some("world hello"));
        } else {
            panic!("Expected string");
        }
    }

    #[test]
    fn test_replace_zero_length_match() {
        let names = NameTable::new();
        let mut ctx = create_context(&names);

        let result = replace(
            &mut ctx,
            vec![
                XPathValue::string("test"),
                XPathValue::string("a?"),
                XPathValue::string("X"),
            ],
        );

        assert!(matches!(result, Err(XPathError::FORX0003 { .. })));
    }

    #[test]
    fn test_replace_invalid_replacement() {
        let names = NameTable::new();
        let mut ctx = create_context(&names);

        // $ not followed by digit or $
        let result = replace(
            &mut ctx,
            vec![
                XPathValue::string("test"),
                XPathValue::string("t"),
                XPathValue::string("$x"),
            ],
        );

        assert!(matches!(result, Err(XPathError::FORX0004 { .. })));
    }

    #[test]
    fn test_tokenize_basic() {
        let names = NameTable::new();
        let mut ctx = create_context(&names);

        let result = tokenize(
            &mut ctx,
            vec![
                XPathValue::string("a,b,c"),
                XPathValue::string(","),
            ],
        ).unwrap();

        match result {
            XPathValue::Sequence(items) => {
                assert_eq!(items.len(), 3);
                let strs: Vec<String> = items.iter().map(|item| {
                    if let XmlItem::Atomic(v) = item {
                        v.to_string_value()
                    } else {
                        panic!("Expected atomic")
                    }
                }).collect();
                assert_eq!(strs, vec!["a", "b", "c"]);
            }
            _ => panic!("Expected sequence"),
        }
    }

    #[test]
    fn test_tokenize_whitespace() {
        let names = NameTable::new();
        let mut ctx = create_context(&names);

        let result = tokenize(
            &mut ctx,
            vec![
                XPathValue::string("red   green   blue"),
                XPathValue::string("\\s+"),
            ],
        ).unwrap();

        match result {
            XPathValue::Sequence(items) => {
                assert_eq!(items.len(), 3);
            }
            _ => panic!("Expected sequence"),
        }
    }

    #[test]
    fn test_tokenize_empty_input() {
        let names = NameTable::new();
        let mut ctx = create_context(&names);

        let result = tokenize(
            &mut ctx,
            vec![
                XPathValue::string(""),
                XPathValue::string(","),
            ],
        ).unwrap();

        assert!(matches!(result, XPathValue::Empty));
    }

    #[test]
    fn test_validate_replacement_valid() {
        assert!(validate_replacement_string("hello").is_ok());
        assert!(validate_replacement_string("$1$2").is_ok());
        assert!(validate_replacement_string("$$").is_ok());
        assert!(validate_replacement_string("\\\\").is_ok());
        assert!(validate_replacement_string("\\$").is_ok());
        assert!(validate_replacement_string("a$1b").is_ok());
    }

    #[test]
    fn test_validate_replacement_invalid() {
        assert!(validate_replacement_string("$").is_err());
        assert!(validate_replacement_string("$x").is_err());
        assert!(validate_replacement_string("\\").is_err());
        assert!(validate_replacement_string("\\x").is_err());
    }

    #[test]
    fn test_convert_replacement() {
        assert_eq!(convert_replacement("hello"), "hello");
        assert_eq!(convert_replacement("$1"), "$1");
        assert_eq!(convert_replacement("$12"), "$12");
        assert_eq!(convert_replacement("$$"), "$$");
        assert_eq!(convert_replacement("\\\\"), "\\");
        assert_eq!(convert_replacement("\\$"), "$$");
        assert_eq!(convert_replacement("$1 and $2"), "$1 and $2");
    }

    #[test]
    fn test_tokenize_filters_empty_tokens() {
        // Test that tokenize filters out empty tokens from leading/trailing delimiters
        let names = NameTable::new();
        let mut ctx = create_context(&names);

        // Leading delimiter - should not produce empty token at start
        let result = tokenize(
            &mut ctx,
            vec![
                XPathValue::string(",a,b"),
                XPathValue::string(","),
            ],
        ).unwrap();

        match result {
            XPathValue::Sequence(items) => {
                assert_eq!(items.len(), 2); // "a" and "b" only, no leading empty
                let strs: Vec<String> = items.iter().map(|item| {
                    if let XmlItem::Atomic(v) = item {
                        v.to_string_value()
                    } else {
                        panic!("Expected atomic")
                    }
                }).collect();
                assert_eq!(strs, vec!["a", "b"]);
            }
            _ => panic!("Expected sequence"),
        }
    }

    #[test]
    fn test_tokenize_trailing_delimiter() {
        // Trailing delimiter - should not produce empty token at end
        let names = NameTable::new();
        let mut ctx = create_context(&names);

        let result = tokenize(
            &mut ctx,
            vec![
                XPathValue::string("a,b,"),
                XPathValue::string(","),
            ],
        ).unwrap();

        match result {
            XPathValue::Sequence(items) => {
                assert_eq!(items.len(), 2); // "a" and "b" only, no trailing empty
            }
            _ => panic!("Expected sequence"),
        }
    }

    #[test]
    fn test_validate_replacement_trailing_dollar() {
        // Trailing $ at end of string should be rejected
        assert!(validate_replacement_string("hello$").is_err());
        assert!(validate_replacement_string("a$1$").is_err());
    }

    // =========================================================================
    // XSD/XPath character class escape tests (\i, \c)
    // =========================================================================

    #[test]
    fn test_matches_initial_name_char() {
        // Test \i matches initial XML name characters
        let names = NameTable::new();
        let mut ctx = create_context(&names);
        let result = matches(&mut ctx, vec![
            XPathValue::string("_foo"),
            XPathValue::string(r"\i"),
        ]).unwrap();
        assert!(matches!(result, XPathValue::Item(XmlItem::Atomic(v)) if v.as_boolean() == Some(true)));
    }

    #[test]
    fn test_matches_xml_name_pattern() {
        // Test \i\c* matches XML names
        let names = NameTable::new();
        let mut ctx = create_context(&names);
        let result = matches(&mut ctx, vec![
            XPathValue::string("foo:bar"),
            XPathValue::string(r"\i\c*"),
        ]).unwrap();
        assert!(matches!(result, XPathValue::Item(XmlItem::Atomic(v)) if v.as_boolean() == Some(true)));
    }

    #[test]
    fn test_matches_digit_not_initial() {
        // Test \i does NOT match digits
        let names = NameTable::new();
        let mut ctx = create_context(&names);
        let result = matches(&mut ctx, vec![
            XPathValue::string("123"),
            XPathValue::string(r"^\i"),
        ]).unwrap();
        assert!(matches!(result, XPathValue::Item(XmlItem::Atomic(v)) if v.as_boolean() == Some(false)));
    }

    #[test]
    fn test_matches_name_char_with_digits() {
        // Test \c matches digits and other name characters
        let names = NameTable::new();
        let mut ctx = create_context(&names);
        let result = matches(&mut ctx, vec![
            XPathValue::string("abc123"),
            XPathValue::string(r"\c+"),
        ]).unwrap();
        assert!(matches!(result, XPathValue::Item(XmlItem::Atomic(v)) if v.as_boolean() == Some(true)));
    }

    #[test]
    fn test_replace_with_name_char_pattern() {
        // Test replace with \c pattern
        let names = NameTable::new();
        let mut ctx = create_context(&names);
        let result = replace(&mut ctx, vec![
            XPathValue::string("hello world"),
            XPathValue::string(r"\c+"),
            XPathValue::string("X"),
        ]).unwrap();

        if let XPathValue::Item(XmlItem::Atomic(v)) = result {
            assert_eq!(v.as_string(), Some("X X"));
        } else {
            panic!("Expected string");
        }
    }

    #[test]
    fn test_tokenize_with_non_name_char() {
        // Test tokenize using \C (non-name character) as delimiter
        let names = NameTable::new();
        let mut ctx = create_context(&names);
        let result = tokenize(&mut ctx, vec![
            XPathValue::string("foo bar baz"),
            XPathValue::string(r"\C+"),
        ]).unwrap();

        match result {
            XPathValue::Sequence(items) => {
                assert_eq!(items.len(), 3);
                let strs: Vec<String> = items.iter().map(|item| {
                    if let XmlItem::Atomic(v) = item {
                        v.to_string_value()
                    } else {
                        panic!("Expected atomic")
                    }
                }).collect();
                assert_eq!(strs, vec!["foo", "bar", "baz"]);
            }
            _ => panic!("Expected sequence"),
        }
    }
}
