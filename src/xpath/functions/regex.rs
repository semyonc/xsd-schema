//! XPath 2.0 regex functions.
//!
//! This module implements:
//! - fn:matches($input, $pattern, $flags?) - test if string matches pattern
//! - fn:replace($input, $pattern, $replacement, $flags?) - replace matches
//! - fn:tokenize($input, $pattern, $flags?) - split string by pattern
//!
//! Uses the `regexml` crate for native XML Schema 1.1 regex with full Unicode support.
//!
//! All three compile their `$pattern` and `$flags` arguments through one private
//! helper, which reuses the program of a `(pattern, flags)` pair it has already
//! compiled during the current evaluation run. A pattern that does not change —
//! a literal in a predicate, say — is therefore compiled once, however many
//! items the predicate is evaluated for. The reuse is invisible: a compiled
//! `regexml::Regex` is immutable and builds a fresh matcher for every call, and
//! a compile that fails yields the same error every time.

use regexml::Regex;

use crate::xpath::context::DynamicContext;
use crate::xpath::error::XPathError;
use crate::xpath::DomNavigator;

use super::{atomize_to_string, atomize_to_string_opt, atomize_to_string_required, XPathValue};
use crate::types::value::XmlValue;
use crate::xpath::iterator::XmlItem;

/// fn:matches($input as xs:string?, $pattern as xs:string, $flags as xs:string?) as xs:boolean
///
/// Returns true if $input matches the regular expression $pattern.
///
/// - If $input is empty, it is treated as empty string.
/// - FORX0001 if $flags contains invalid characters.
/// - FORX0002 if $pattern is not a valid regular expression.
pub fn matches<N: DomNavigator>(
    context: &mut DynamicContext<'_, N>,
    mut args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if args.len() < 2 || args.len() > 3 {
        return Err(XPathError::wrong_number_of_arguments(
            "matches",
            2,
            args.len(),
        ));
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

    let flags_str = flags.as_deref().unwrap_or("");

    // Build the regex
    let regex = build_regex(context, &pattern, flags_str)?;

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
    context: &mut DynamicContext<'_, N>,
    mut args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if args.len() < 3 || args.len() > 4 {
        return Err(XPathError::wrong_number_of_arguments(
            "replace",
            3,
            args.len(),
        ));
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

    // Build the regex
    let regex = build_regex(context, &pattern, flags.as_deref().unwrap_or(""))?;

    // regexml handles FORX0003 (zero-length match) and FORX0004 (invalid replacement) internally
    let result = regex
        .replace_all(&input, &replacement)
        .map_err(|e| match e {
            regexml::Error::MatchesEmptyString => XPathError::regex_matches_zero_length(&pattern),
            regexml::Error::InvalidReplacementString(_) => {
                XPathError::invalid_replacement_string(&replacement)
            }
            _ => XPathError::invalid_regex_pattern(&pattern),
        })?;

    Ok(XPathValue::string(result))
}

/// fn:tokenize($input as xs:string?, $pattern as xs:string, $flags as xs:string?) as xs:string*
///
/// Splits $input into a sequence of strings using $pattern as delimiter.
///
/// - FORX0001 if $flags contains invalid characters.
/// - FORX0002 if $pattern is not a valid regular expression.
/// - FORX0003 if $pattern matches a zero-length string.
pub fn tokenize<N: DomNavigator>(
    context: &mut DynamicContext<'_, N>,
    mut args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if args.len() < 2 || args.len() > 3 {
        return Err(XPathError::wrong_number_of_arguments(
            "tokenize",
            2,
            args.len(),
        ));
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
    let regex = build_regex(context, &pattern, flags.as_deref().unwrap_or(""))?;

    // regexml handles FORX0003 (zero-length match) internally
    let token_iter = regex.tokenize(&input).map_err(|e| match e {
        regexml::Error::MatchesEmptyString => XPathError::regex_matches_zero_length(&pattern),
        _ => XPathError::invalid_regex_pattern(&pattern),
    })?;

    // Every gap between two adjacent separators is a token, including the
    // zero-length ones produced by a leading separator, a trailing separator
    // and two adjacent separators.
    let items: Vec<XmlItem<N>> = token_iter
        .map(|s| XmlItem::Atomic(XmlValue::string(&s)))
        .collect();

    Ok(XPathValue::from_sequence(items))
}

// ============================================================================
// Helper Functions
// ============================================================================

/// The regular-expression flags XPath 2.0 defines: `s`, `m`, `i` and `x`.
const XPATH20_REGEX_FLAGS: &str = "smix";

/// Reject regular-expression syntax that the XPath 2.0 dialect does not have.
///
/// The `regexml` backend implements the later XPath dialect, which added the
/// non-capturing group `(?:…)` (and the other `(?…)` forms) together with the
/// `q` flag. In the XPath 2.0 grammar a `(` always opens a capturing group and
/// is followed by a branch; `?` is a quantifier and a quantifier needs an atom
/// in front of it, so an unescaped `(` can never be followed by `?`. The only
/// defined flags are `s`, `m`, `i` and `x`.
///
/// This is deliberately a pre-pass over the *source* pattern rather than a
/// change to the backend: only the three XPath regular-expression functions go
/// through here, so `xs:pattern` facets — which compile the same backend
/// directly and wrap their own value in `^(?:…)$` — keep working unchanged.
///
/// Returns FORX0001 for an undefined flag and FORX0002 for a `(?` occurrence
/// outside a character class.
fn check_xpath20_regex_dialect(pattern: &str, flags: &str) -> Result<(), XPathError> {
    for f in flags.chars() {
        if !XPATH20_REGEX_FLAGS.contains(f) {
            return Err(XPathError::invalid_regex_flags(flags));
        }
    }

    // With the `x` flag the whitespace characters #x9, #xA, #xD and #x20 are
    // removed from the pattern before it is parsed, so `( ?:a)` is `(?:a)`.
    let ignore_whitespace = flags.contains('x');

    let chars: Vec<char> = pattern.chars().collect();
    // `[` opens a character class and, after `-`, a nested subtracted class;
    // inside a class an unescaped `[` or `]` is not allowed otherwise, so a
    // plain depth counter tracks `[a-z-[(?]]` correctly.
    let mut class_depth: usize = 0;
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            // A backslash escapes the character that follows it, so `\(?` is a
            // literal `(` with a `?` quantifier on it.
            '\\' => {
                i += 2;
                continue;
            }
            '[' => class_depth += 1,
            ']' => class_depth = class_depth.saturating_sub(1),
            '(' if class_depth == 0 => {
                let mut j = i + 1;
                if ignore_whitespace {
                    while matches!(chars.get(j), Some('\u{9}' | '\u{A}' | '\u{D}' | ' ')) {
                        j += 1;
                    }
                }
                if chars.get(j) == Some(&'?') {
                    return Err(XPathError::invalid_regex_pattern(pattern));
                }
            }
            _ => {}
        }
        i += 1;
    }

    Ok(())
}

/// The Regex for an XPath pattern and flags, compiled at most once per
/// `(pattern, flags)` pair per evaluation run.
///
/// The program is compiled by [`compile_regex`] the first time this run asks for
/// the pair and is kept in the run's
/// [`regex_cache`](crate::xpath::regex_cache) afterwards, so a pattern that does
/// not change — a literal in a predicate, say — is compiled once however many
/// items the predicate is evaluated for. Reuse is invisible: a compiled
/// `regexml::Regex` is immutable and builds a fresh matcher for each call, and a
/// compile that fails yields the same error every time because that error is
/// built from the pattern and the flags alone.
fn build_regex<'run, N: DomNavigator>(
    context: &'run mut DynamicContext<'_, N>,
    pattern: &str,
    flags: &str,
) -> Result<&'run Regex, XPathError> {
    context
        .regex_cache_mut()
        .get_or_compile(pattern, flags, || compile_regex(pattern, flags))
}

/// Build a Regex from an XPath pattern and flags using regexml.
///
/// regexml natively handles XML Schema regex syntax including:
/// - Character class subtraction `[A-Z-[OI]]`
/// - XSD-specific escapes `\i`, `\c`, `\I`, `\C`
/// - Unicode categories `\p{Lu}`, `\P{Lu}`
/// - Flag handling (s, m, i, x)
fn compile_regex(pattern: &str, flags: &str) -> Result<Regex, XPathError> {
    check_xpath20_regex_dialect(pattern, flags)?;

    Regex::xpath(pattern, flags).map_err(|e| match e {
        regexml::Error::InvalidFlags(_) => XPathError::invalid_regex_flags(flags),
        regexml::Error::Syntax(_) => XPathError::invalid_regex_pattern(pattern),
        _ => XPathError::invalid_regex_pattern(pattern),
    })
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
            vec![XPathValue::string("abracadabra"), XPathValue::string("bra")],
        )
        .unwrap();

        assert!(
            matches!(result, XPathValue::Item(XmlItem::Atomic(v)) if v.as_boolean() == Some(true))
        );
    }

    #[test]
    fn test_matches_no_match() {
        let names = NameTable::new();
        let mut ctx = create_context(&names);

        let result = matches(
            &mut ctx,
            vec![XPathValue::string("abracadabra"), XPathValue::string("xyz")],
        )
        .unwrap();

        assert!(
            matches!(result, XPathValue::Item(XmlItem::Atomic(v)) if v.as_boolean() == Some(false))
        );
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
        )
        .unwrap();

        assert!(
            matches!(result, XPathValue::Item(XmlItem::Atomic(v)) if v.as_boolean() == Some(true))
        );
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
        )
        .unwrap();

        assert!(
            matches!(result, XPathValue::Item(XmlItem::Atomic(v)) if v.as_boolean() == Some(true))
        );
    }

    #[test]
    fn test_matches_multiline_empty_line_trailing_newline() {
        let names = NameTable::new();
        let mut ctx = create_context(&names);

        let result = matches(
            &mut ctx,
            vec![
                XPathValue::string("abcd\ndefg\n"),
                XPathValue::string("^$"),
                XPathValue::string("m"),
            ],
        )
        .unwrap();

        assert!(
            matches!(result, XPathValue::Item(XmlItem::Atomic(v)) if v.as_boolean() == Some(false))
        );
    }

    #[test]
    fn test_matches_multiline_empty_line_in_middle() {
        let names = NameTable::new();
        let mut ctx = create_context(&names);

        let result = matches(
            &mut ctx,
            vec![
                XPathValue::string("abcd\n\ndefg\n"),
                XPathValue::string("^$"),
                XPathValue::string("m"),
            ],
        )
        .unwrap();

        assert!(
            matches!(result, XPathValue::Item(XmlItem::Atomic(v)) if v.as_boolean() == Some(true))
        );
    }

    #[test]
    fn test_matches_class_subtraction_with_i_flag() {
        let names = NameTable::new();
        let mut ctx = create_context(&names);

        let match_x = matches(
            &mut ctx,
            vec![
                XPathValue::string("X"),
                XPathValue::string("[A-Z-[OI]]"),
                XPathValue::string("i"),
            ],
        )
        .unwrap();
        assert!(
            matches!(match_x, XPathValue::Item(XmlItem::Atomic(v)) if v.as_boolean() == Some(true))
        );

        let match_o = matches(
            &mut ctx,
            vec![
                XPathValue::string("O"),
                XPathValue::string("[A-Z-[OI]]"),
                XPathValue::string("i"),
            ],
        )
        .unwrap();
        assert!(
            matches!(match_o, XPathValue::Item(XmlItem::Atomic(v)) if v.as_boolean() == Some(false))
        );

        let match_i = matches(
            &mut ctx,
            vec![
                XPathValue::string("i"),
                XPathValue::string("[A-Z-[OI]]"),
                XPathValue::string("i"),
            ],
        )
        .unwrap();
        assert!(
            matches!(match_i, XPathValue::Item(XmlItem::Atomic(v)) if v.as_boolean() == Some(false))
        );
    }

    #[test]
    fn test_matches_unicode_categories_with_i_flag() {
        let names = NameTable::new();
        let mut ctx = create_context(&names);

        let upper = matches(
            &mut ctx,
            vec![
                XPathValue::string("m"),
                XPathValue::string(r"\p{Lu}"),
                XPathValue::string("i"),
            ],
        )
        .unwrap();
        assert!(
            matches!(upper, XPathValue::Item(XmlItem::Atomic(v)) if v.as_boolean() == Some(false))
        );

        let not_upper = matches(
            &mut ctx,
            vec![
                XPathValue::string("m"),
                XPathValue::string(r"\P{Lu}"),
                XPathValue::string("i"),
            ],
        )
        .unwrap();
        assert!(
            matches!(not_upper, XPathValue::Item(XmlItem::Atomic(v)) if v.as_boolean() == Some(true))
        );
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
            vec![XPathValue::string("test"), XPathValue::string("[invalid")],
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
        )
        .unwrap();

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
                XPathValue::string("([a-z]+) ([a-z]+)"),
                XPathValue::string("$2 $1"),
            ],
        )
        .unwrap();

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
            vec![XPathValue::string("a,b,c"), XPathValue::string(",")],
        )
        .unwrap();

        match result {
            XPathValue::Sequence(items) => {
                assert_eq!(items.len(), 3);
                let strs: Vec<String> = items
                    .iter()
                    .map(|item| {
                        if let XmlItem::Atomic(v) = item {
                            v.to_string_value()
                        } else {
                            panic!("Expected atomic")
                        }
                    })
                    .collect();
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
        )
        .unwrap();

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
            vec![XPathValue::string(""), XPathValue::string(",")],
        )
        .unwrap();

        assert!(matches!(result, XPathValue::Empty));
    }

    /// Collect a tokenize() result as plain strings.
    fn token_strings(value: XPathValue<RoXmlNavigator<'_>>) -> Vec<String> {
        match value {
            XPathValue::Empty => Vec::new(),
            XPathValue::Item(XmlItem::Atomic(v)) => vec![v.to_string_value()],
            XPathValue::Sequence(items) => items
                .iter()
                .map(|item| {
                    if let XmlItem::Atomic(v) = item {
                        v.to_string_value()
                    } else {
                        panic!("Expected atomic")
                    }
                })
                .collect(),
            _ => panic!("Expected a sequence of atomic values"),
        }
    }

    fn tokenize_strings(input: &str, pattern: &str) -> Vec<String> {
        let names = NameTable::new();
        let mut ctx = create_context(&names);
        let result = tokenize(
            &mut ctx,
            vec![XPathValue::string(input), XPathValue::string(pattern)],
        )
        .unwrap();
        token_strings(result)
    }

    #[test]
    fn test_tokenize_leading_separator_keeps_empty_token() {
        // A separator at the start of the input yields a zero-length first token.
        assert_eq!(tokenize_strings(",a,b", ","), vec!["", "a", "b"]);
        assert_eq!(tokenize_strings("/a/b", "/"), vec!["", "a", "b"]);
    }

    #[test]
    fn test_tokenize_trailing_separator_keeps_empty_token() {
        // A separator at the end of the input yields a zero-length last token.
        assert_eq!(tokenize_strings("a,b,", ","), vec!["a", "b", ""]);
        assert_eq!(tokenize_strings("a/b/", "/"), vec!["a", "b", ""]);
    }

    #[test]
    fn test_tokenize_adjacent_separators_keep_empty_token() {
        // Two adjacent separators have a zero-length token between them.
        assert_eq!(tokenize_strings("a,,b", ","), vec!["a", "", "b"]);
        assert_eq!(tokenize_strings(",a,", ","), vec!["", "a", ""]);
        assert_eq!(tokenize_strings(",", ","), vec!["", ""]);
    }

    #[test]
    fn test_tokenize_all_gaps_are_tokens() {
        // Every gap between matches is a token: "abracadabra" split on
        // "(ab)|(a)" starts and ends with a zero-length token.
        assert_eq!(
            tokenize_strings("abracadabra", "(ab)|(a)"),
            vec!["", "r", "c", "d", "r", ""]
        );
    }

    // =========================================================================
    // XPath 2.0 regular-expression dialect
    // =========================================================================

    fn matches_result(
        input: &str,
        pattern: &str,
        flags: Option<&str>,
    ) -> Result<XPathValue<RoXmlNavigator<'static>>, XPathError> {
        let names = Box::leak(Box::new(NameTable::new()));
        let mut ctx = create_context(names);
        let mut args = vec![XPathValue::string(input), XPathValue::string(pattern)];
        if let Some(f) = flags {
            args.push(XPathValue::string(f));
        }
        matches(&mut ctx, args)
    }

    #[test]
    fn test_group_with_question_mark_is_rejected() {
        // `(?:` and the other `(?…)` forms belong to a later dialect; XPath 2.0
        // has no atom that starts with `(?`.
        for pattern in ["(?:a)", "a(?:b)c", "(?i)a", "(?=a)", "(?!a)", "((?:a))"] {
            assert!(
                matches!(
                    matches_result("a", pattern, None),
                    Err(XPathError::FORX0002 { .. })
                ),
                "expected FORX0002 for {pattern}"
            );
        }
    }

    #[test]
    fn test_escaped_paren_followed_by_quantifier_is_accepted() {
        // `\(?` is an escaped `(` carrying a `?` quantifier, which is legal.
        let result = matches_result("x", r"\(?x", None).unwrap();
        assert!(
            matches!(result, XPathValue::Item(XmlItem::Atomic(v)) if v.as_boolean() == Some(true))
        );

        let result = matches_result("(x", r"\(?x", None).unwrap();
        assert!(
            matches!(result, XPathValue::Item(XmlItem::Atomic(v)) if v.as_boolean() == Some(true))
        );
    }

    #[test]
    fn test_question_mark_in_character_class_is_accepted() {
        // A `(` and a `?` are ordinary members of a character class.
        let result = matches_result("?", "[(?]", None).unwrap();
        assert!(
            matches!(result, XPathValue::Item(XmlItem::Atomic(v)) if v.as_boolean() == Some(true))
        );

        // …including inside a subtracted nested class.
        let result = matches_result("b", "[a-z-[(?]]", None).unwrap();
        assert!(
            matches!(result, XPathValue::Item(XmlItem::Atomic(v)) if v.as_boolean() == Some(true))
        );
        let result = matches_result("?", "[a-z-[(?]]", None).unwrap();
        assert!(
            matches!(result, XPathValue::Item(XmlItem::Atomic(v)) if v.as_boolean() == Some(false))
        );

        // A class closed before the `(?` does not shield it.
        assert!(matches!(
            matches_result("a", "[ab](?:c)", None),
            Err(XPathError::FORX0002 { .. })
        ));
    }

    #[test]
    fn test_group_with_question_mark_rejected_under_x_flag() {
        // With `x`, whitespace is removed before the pattern is parsed, so
        // `( ?:a)` is the same pattern as `(?:a)`.
        assert!(matches!(
            matches_result("a", "( ?:a)", Some("x")),
            Err(XPathError::FORX0002 { .. })
        ));
        assert!(matches!(
            matches_result("a", "(\t\n?:a)", Some("x")),
            Err(XPathError::FORX0002 { .. })
        ));
        // Without `x` the space is a literal, so the pattern is a plain group.
        assert!(matches_result("a", "( ?:a)", None).is_ok());
    }

    #[test]
    fn test_q_flag_is_rejected() {
        // The `q` (literal) flag was introduced after XPath 2.0.
        assert!(matches!(
            matches_result("a.b", "a.b", Some("q")),
            Err(XPathError::FORX0001 { .. })
        ));
        for flag in ["q", "z", "smixq", " "] {
            assert!(
                matches!(
                    matches_result("a", "a", Some(flag)),
                    Err(XPathError::FORX0001 { .. })
                ),
                "expected FORX0001 for flags {flag:?}"
            );
        }
    }

    #[test]
    fn test_defined_flags_are_accepted() {
        for flag in ["", "s", "m", "i", "x", "smix", "ii"] {
            assert!(
                matches_result("a", "a", Some(flag)).is_ok(),
                "expected flags {flag:?} to be accepted"
            );
        }
    }

    #[test]
    fn test_dialect_check_applies_to_replace_and_tokenize() {
        let names = NameTable::new();
        let mut ctx = create_context(&names);
        let result = replace(
            &mut ctx,
            vec![
                XPathValue::string("abc"),
                XPathValue::string("(?:b)"),
                XPathValue::string("X"),
            ],
        );
        assert!(matches!(result, Err(XPathError::FORX0002 { .. })));

        let result = tokenize(
            &mut ctx,
            vec![XPathValue::string("abc"), XPathValue::string("(?:b)")],
        );
        assert!(matches!(result, Err(XPathError::FORX0002 { .. })));
    }

    // =========================================================================
    // One compilation per (pattern, flags) pair per run
    //
    // Every test here asserts the observable result first and the cache
    // counters second, so that it fails both when the cache changes an answer
    // and when it silently stops engaging.
    // =========================================================================

    /// The `true`/`false` a `matches()` result carries.
    fn boolean_of(value: XPathValue<RoXmlNavigator<'_>>) -> bool {
        match value {
            XPathValue::Item(XmlItem::Atomic(v)) => v.as_boolean().expect("xs:boolean"),
            _ => panic!("expected a single xs:boolean"),
        }
    }

    #[test]
    fn test_a_constant_pattern_is_compiled_once_per_run() {
        // The shape this cache exists for: one pattern, one call per item.
        let names = NameTable::new();
        let mut ctx = create_context(&names);

        for i in 0..200 {
            let result = matches(
                &mut ctx,
                vec![
                    XPathValue::string(format!("item-{i}")),
                    XPathValue::string(r"\p{Ll}"),
                ],
            )
            .unwrap();
            assert!(boolean_of(result), "item-{i} has lowercase letters in it");
        }

        assert_eq!(ctx.regex_cache().compiles(), 1);
        assert_eq!(ctx.regex_cache().hits(), 199);
    }

    #[test]
    fn test_the_three_functions_share_one_compiled_pattern() {
        let names = NameTable::new();
        let mut ctx = create_context(&names);

        let hit = matches(
            &mut ctx,
            vec![XPathValue::string("a,b"), XPathValue::string(",")],
        )
        .unwrap();
        assert!(boolean_of(hit));

        let replaced = replace(
            &mut ctx,
            vec![
                XPathValue::string("a,b"),
                XPathValue::string(","),
                XPathValue::string(";"),
            ],
        )
        .unwrap();
        assert!(
            matches!(replaced, XPathValue::Item(XmlItem::Atomic(ref v)) if v.as_string() == Some("a;b"))
        );

        let tokens = tokenize(
            &mut ctx,
            vec![XPathValue::string("a,b"), XPathValue::string(",")],
        )
        .unwrap();
        assert_eq!(token_strings(tokens), vec!["a", "b"]);

        // One program, built by the `matches()` call and reused by the other two.
        assert_eq!(ctx.regex_cache().compiles(), 1);
        assert_eq!(ctx.regex_cache().hits(), 2);
    }

    #[test]
    fn test_flags_are_part_of_the_cache_key() {
        let names = NameTable::new();
        let mut ctx = create_context(&names);

        for _ in 0..2 {
            let plain = matches(
                &mut ctx,
                vec![XPathValue::string("A"), XPathValue::string("a")],
            )
            .unwrap();
            assert!(!boolean_of(plain));

            let folded = matches(
                &mut ctx,
                vec![
                    XPathValue::string("A"),
                    XPathValue::string("a"),
                    XPathValue::string("i"),
                ],
            )
            .unwrap();
            assert!(boolean_of(folded));
        }

        // Same pattern, two flag strings, two programs.
        assert_eq!(ctx.regex_cache().compiles(), 2);
        assert_eq!(ctx.regex_cache().hits(), 2);
    }

    #[test]
    fn test_an_invalid_pattern_reports_the_same_error_every_time() {
        let names = NameTable::new();
        let mut ctx = create_context(&names);

        let mut seen = Vec::new();
        for _ in 0..5 {
            let result = matches(
                &mut ctx,
                vec![XPathValue::string("test"), XPathValue::string("[invalid")],
            );
            let err = result.err().expect("an invalid pattern must raise");
            assert!(matches!(err, XPathError::FORX0002 { .. }));
            seen.push(err.to_string());
        }
        assert!(seen.windows(2).all(|pair| pair[0] == pair[1]), "{seen:?}");
        assert_eq!(ctx.regex_cache().compiles(), 1);
        assert_eq!(ctx.regex_cache().hits(), 4);
    }

    #[test]
    fn test_an_invalid_flag_reports_the_same_error_every_time() {
        let names = NameTable::new();
        let mut ctx = create_context(&names);

        let mut seen = Vec::new();
        for _ in 0..5 {
            let result = matches(
                &mut ctx,
                vec![
                    XPathValue::string("test"),
                    XPathValue::string("test"),
                    XPathValue::string("z"),
                ],
            );
            let err = result.err().expect("an undefined flag must raise");
            assert!(matches!(err, XPathError::FORX0001 { .. }));
            seen.push(err.to_string());
        }
        assert!(seen.windows(2).all(|pair| pair[0] == pair[1]), "{seen:?}");
        assert_eq!(ctx.regex_cache().compiles(), 1);
    }

    #[test]
    fn test_a_reused_program_still_raises_forx0003() {
        // `a?` matches the zero-length string. `matches()` is happy with it and
        // is what puts it in the cache; `replace()` and `tokenize()` must still
        // raise FORX0003 from the *reused* program, on every call.
        let names = NameTable::new();
        let mut ctx = create_context(&names);

        let hit = matches(
            &mut ctx,
            vec![XPathValue::string("test"), XPathValue::string("a?")],
        )
        .unwrap();
        assert!(boolean_of(hit));

        for _ in 0..3 {
            let result = replace(
                &mut ctx,
                vec![
                    XPathValue::string("test"),
                    XPathValue::string("a?"),
                    XPathValue::string("X"),
                ],
            );
            assert!(matches!(result, Err(XPathError::FORX0003 { .. })));

            let result = tokenize(
                &mut ctx,
                vec![XPathValue::string("test"), XPathValue::string("a?")],
            );
            assert!(matches!(result, Err(XPathError::FORX0003 { .. })));
        }
        assert_eq!(ctx.regex_cache().compiles(), 1);
    }

    #[test]
    fn test_a_reused_program_still_raises_forx0004() {
        // The replacement string is not part of the key, so a cache hit must
        // not carry the previous call's verdict on it.
        let names = NameTable::new();
        let mut ctx = create_context(&names);

        let good = replace(
            &mut ctx,
            vec![
                XPathValue::string("test"),
                XPathValue::string("t"),
                XPathValue::string("X"),
            ],
        )
        .unwrap();
        assert!(
            matches!(good, XPathValue::Item(XmlItem::Atomic(ref v)) if v.as_string() == Some("XesX"))
        );

        for _ in 0..3 {
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
        assert_eq!(ctx.regex_cache().compiles(), 1);
    }

    #[test]
    fn test_the_cache_does_not_outlive_its_run() {
        let names = NameTable::new();
        for _ in 0..3 {
            let mut ctx = create_context(&names);
            for _ in 0..2 {
                let hit = matches(
                    &mut ctx,
                    vec![XPathValue::string("abc"), XPathValue::string("b")],
                )
                .unwrap();
                assert!(boolean_of(hit));
            }
            // Reused inside the run, and a fresh context starts empty again:
            // the cache belongs to the run, not to the process.
            assert_eq!(ctx.regex_cache().compiles(), 1);
            assert_eq!(ctx.regex_cache().hits(), 1);
        }
    }

    #[test]
    fn test_a_pattern_computed_per_item_does_not_grow_the_cache() {
        // `matches($s, $row/@pattern)` — a new pattern for every item. The cache
        // must stay bounded and must keep answering correctly.
        let names = NameTable::new();
        let mut ctx = create_context(&names);

        let rounds = crate::xpath::regex_cache::MAX_ENTRIES * 8;
        for i in 0..rounds {
            let pattern = format!("^item-{i}$");
            let hit = matches(
                &mut ctx,
                vec![
                    XPathValue::string(format!("item-{i}")),
                    XPathValue::string(pattern),
                ],
            )
            .unwrap();
            assert!(boolean_of(hit));
            assert!(
                ctx.regex_cache().len() <= crate::xpath::regex_cache::MAX_ENTRIES,
                "{} entries resident after {i} patterns",
                ctx.regex_cache().len()
            );
        }
        assert_eq!(ctx.regex_cache().compiles() as usize, rounds);
        assert_eq!(ctx.regex_cache().hits(), 0);
        assert_eq!(
            ctx.regex_cache().len(),
            crate::xpath::regex_cache::MAX_ENTRIES
        );
    }

    // =========================================================================
    // XSD/XPath character class escape tests (\i, \c)
    // =========================================================================

    #[test]
    fn test_matches_initial_name_char() {
        // Test \i matches initial XML name characters
        let names = NameTable::new();
        let mut ctx = create_context(&names);
        let result = matches(
            &mut ctx,
            vec![XPathValue::string("_foo"), XPathValue::string(r"\i")],
        )
        .unwrap();
        assert!(
            matches!(result, XPathValue::Item(XmlItem::Atomic(v)) if v.as_boolean() == Some(true))
        );
    }

    #[test]
    fn test_matches_xml_name_pattern() {
        // Test \i\c* matches XML names
        let names = NameTable::new();
        let mut ctx = create_context(&names);
        let result = matches(
            &mut ctx,
            vec![XPathValue::string("foo:bar"), XPathValue::string(r"\i\c*")],
        )
        .unwrap();
        assert!(
            matches!(result, XPathValue::Item(XmlItem::Atomic(v)) if v.as_boolean() == Some(true))
        );
    }

    #[test]
    fn test_matches_digit_not_initial() {
        // Test \i does NOT match digits
        let names = NameTable::new();
        let mut ctx = create_context(&names);
        let result = matches(
            &mut ctx,
            vec![XPathValue::string("123"), XPathValue::string(r"^\i")],
        )
        .unwrap();
        assert!(
            matches!(result, XPathValue::Item(XmlItem::Atomic(v)) if v.as_boolean() == Some(false))
        );
    }

    #[test]
    fn test_matches_name_char_with_digits() {
        // Test \c matches digits and other name characters
        let names = NameTable::new();
        let mut ctx = create_context(&names);
        let result = matches(
            &mut ctx,
            vec![XPathValue::string("abc123"), XPathValue::string(r"\c+")],
        )
        .unwrap();
        assert!(
            matches!(result, XPathValue::Item(XmlItem::Atomic(v)) if v.as_boolean() == Some(true))
        );
    }

    #[test]
    fn test_replace_with_name_char_pattern() {
        // Test replace with \c pattern
        let names = NameTable::new();
        let mut ctx = create_context(&names);
        let result = replace(
            &mut ctx,
            vec![
                XPathValue::string("hello world"),
                XPathValue::string(r"\c+"),
                XPathValue::string("X"),
            ],
        )
        .unwrap();

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
        let result = tokenize(
            &mut ctx,
            vec![
                XPathValue::string("foo bar baz"),
                XPathValue::string(r"\C+"),
            ],
        )
        .unwrap();

        match result {
            XPathValue::Sequence(items) => {
                assert_eq!(items.len(), 3);
                let strs: Vec<String> = items
                    .iter()
                    .map(|item| {
                        if let XmlItem::Atomic(v) = item {
                            v.to_string_value()
                        } else {
                            panic!("Expected atomic")
                        }
                    })
                    .collect();
                assert_eq!(strs, vec!["foo", "bar", "baz"]);
            }
            _ => panic!("Expected sequence"),
        }
    }
}
