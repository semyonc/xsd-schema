//! XML Schema / XPath 2.0 regex pattern conversion.
//!
//! This module provides shared regex pattern conversion for both XSD pattern facets
//! and XPath 2.0 regex functions. XPath 2.0 and XSD use the same regex dialect
//! (XML Schema regex), which differs from standard regex in several ways:
//!
//! - XSD-specific character class escapes: `\i`, `\I`, `\c`, `\C`
//! - XSD patterns are implicitly anchored (must match entire string)
//! - XPath regex functions do not anchor patterns
//!
//! For XSD 1.0 patterns, category escapes `\p{X}` / `\P{X}` are expanded to
//! explicit Unicode-3.0 ranges before being handed to the underlying regex
//! engine. This pins the interpretation of `\p{Lu}` etc. to the Unicode version
//! the MS `msData/regex/reJ*` conformance tests were authored against. See
//! `regex_xsd_unicode` for the motivation.

use crate::regex_xsd_unicode::expand_xsd_category_body;
use crate::schema::model::XsdVersion;

/// Options for pattern conversion.
#[derive(Debug, Clone, Copy)]
pub struct ConvertOptions {
    /// Whether to anchor the pattern with `^...$` (XSD = true, XPath = false)
    pub anchor: bool,
    /// XSD version — selects `\p{X}` lowering. `V1_0` expands recognized
    /// general-category names to Unicode-3.0 ranges; `V1_1` passes through.
    pub xsd_version: XsdVersion,
}

impl Default for ConvertOptions {
    fn default() -> Self {
        Self {
            anchor: false,
            xsd_version: XsdVersion::V1_1,
        }
    }
}

impl ConvertOptions {
    /// Create options for XSD 1.1 pattern facets (anchored, modern Unicode).
    pub fn xsd() -> Self {
        Self { anchor: true, xsd_version: XsdVersion::V1_1 }
    }

    /// Create options for XSD 1.0 pattern facets (anchored, Unicode-3.0 pin).
    pub fn xsd_v1_0() -> Self {
        Self { anchor: true, xsd_version: XsdVersion::V1_0 }
    }

    /// Create options for XPath regex functions (unanchored, modern Unicode).
    pub fn xpath() -> Self {
        Self { anchor: false, xsd_version: XsdVersion::V1_1 }
    }
}

/// Convert XSD/XPath regex pattern to Rust regex syntax.
///
/// Handles XSD-specific character class escapes:
/// - `\i` -> `[A-Za-z_:]` (XML initial name character)
/// - `\I` -> `[^A-Za-z_:]` (not initial name character)
/// - `\c` -> `[A-Za-z0-9._:\-]` (XML name character)
/// - `\C` -> `[^A-Za-z0-9._:\-]` (not name character)
///
/// Under `XsdVersion::V1_0`, category escapes `\p{X}` and `\P{X}` for
/// recognized general-category names are expanded to Unicode-3.0 ranges;
/// block escapes `\p{Is...}` and unknown names are passed through.
///
/// # Arguments
/// - `pattern`: The XSD/XPath regex pattern
/// - `options`: Conversion options (anchoring, XSD version)
///
/// # Returns
/// A regex pattern string compatible with both the `regex` crate and `regexml`.
pub fn convert_xml_pattern(pattern: &str, options: ConvertOptions) -> String {
    let extra_capacity = if options.anchor { 4 } else { 0 };
    let mut result = String::with_capacity(pattern.len() + extra_capacity);

    if options.anchor {
        result.push('^');
    }

    let mut in_class = false;
    let mut chars = pattern.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            let Some(&next) = chars.peek() else {
                result.push('\\');
                continue;
            };
            match next {
                // XSD-specific character class escapes
                'i' => {
                    chars.next();
                    result.push_str(r"[A-Za-z_:]");
                }
                'I' => {
                    chars.next();
                    result.push_str(r"[^A-Za-z_:]");
                }
                'c' => {
                    chars.next();
                    result.push_str(r"[A-Za-z0-9._:\-]");
                }
                'C' => {
                    chars.next();
                    result.push_str(r"[^A-Za-z0-9._:\-]");
                }
                // Standard escapes - pass through
                'd' | 'D' | 's' | 'S' | 'w' | 'W' | 'n' | 'r' | 't' | '\\' | '|' | '.'
                | '?' | '*' | '+' | '{' | '}' | '(' | ')' | '[' | ']' | '^' | '$' | '-' => {
                    result.push('\\');
                    result.push(next);
                    chars.next();
                }
                // Unicode category escapes \p{...} / \P{...}
                'p' | 'P' => {
                    let negated = next == 'P';
                    chars.next();
                    if chars.peek() != Some(&'{') {
                        result.push('\\');
                        result.push(next);
                        continue;
                    }
                    chars.next();
                    let mut name = String::new();
                    let mut closed = false;
                    for c in chars.by_ref() {
                        if c == '}' {
                            closed = true;
                            break;
                        }
                        name.push(c);
                    }
                    if closed
                        && options.xsd_version == XsdVersion::V1_0
                        && try_expand_category(&mut result, &name, negated, in_class)
                    {
                        continue;
                    }
                    // Fallback: pass through unchanged (block escapes, unknown
                    // categories, XSD 1.1, or unterminated escape).
                    result.push('\\');
                    result.push(next);
                    result.push('{');
                    result.push_str(&name);
                    if closed {
                        result.push('}');
                    }
                }
                // Other escapes - pass through
                _ => {
                    result.push('\\');
                    result.push(next);
                    chars.next();
                }
            }
        } else {
            if ch == '[' {
                in_class = true;
            } else if ch == ']' {
                in_class = false;
            }
            result.push(ch);
        }
    }

    if options.anchor {
        result.push('$');
    }
    result
}

/// Rewrite XSD 1.0 general-category escapes `\p{X}` / `\P{X}` to explicit
/// Unicode 3.0 range classes, leaving every other character (including
/// `\i`, `\I`, `\c`, `\C`, standard escapes, and nested character classes)
/// untouched.
///
/// Intended for the `xsd11` feature path, where regexml handles all other
/// XSD regex constructs natively but we still need to pin category-escape
/// semantics to Unicode 3.0 for XSD 1.0 patterns. Block escapes (`Is...`)
/// and unknown category names pass through unchanged.
pub fn rewrite_xsd10_category_escapes(pattern: &str) -> String {
    let mut result = String::with_capacity(pattern.len() * 4);
    let mut in_class = false;
    let mut chars = pattern.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            if ch == '[' {
                in_class = true;
            } else if ch == ']' {
                in_class = false;
            }
            result.push(ch);
            continue;
        }
        let Some(&next) = chars.peek() else {
            result.push('\\');
            continue;
        };
        if next != 'p' && next != 'P' {
            result.push('\\');
            result.push(next);
            chars.next();
            continue;
        }
        let negated = next == 'P';
        chars.next();
        if chars.peek() != Some(&'{') {
            result.push('\\');
            result.push(next);
            continue;
        }
        chars.next();
        let mut name = String::new();
        let mut closed = false;
        for c in chars.by_ref() {
            if c == '}' {
                closed = true;
                break;
            }
            name.push(c);
        }
        if closed && try_expand_category(&mut result, &name, negated, in_class) {
            continue;
        }
        result.push('\\');
        result.push(next);
        result.push('{');
        result.push_str(&name);
        if closed {
            result.push('}');
        }
    }
    result
}

/// Shared lowering for `\p{X}` / `\P{X}` under XSD 1.0 Unicode-3.0 pinning.
///
/// Returns `true` if `name` is a recognized general-category code and the
/// appropriate expansion was appended to `out`. Returns `false` otherwise
/// (block escape, unknown name, or a negated escape inside a character class
/// — which would require set subtraction that isn't expressible here), in
/// which case the caller is expected to emit the original `\p{...}` /
/// `\P{...}` tokens verbatim.
///
/// - Positive `\p{X}` inside `[...]`: appends just the expanded body (no
///   nested brackets).
/// - Positive `\p{X}` outside: wraps the body with `[...]`.
/// - Negated `\P{X}` outside: wraps with `[^...]`.
fn try_expand_category(out: &mut String, name: &str, negated: bool, in_class: bool) -> bool {
    let Some(body) = expand_xsd_category_body(name) else {
        return false;
    };
    if in_class {
        if negated {
            return false;
        }
        out.push_str(body);
        return true;
    }
    if negated {
        out.push_str("[^");
    } else {
        out.push('[');
    }
    out.push_str(body);
    out.push(']');
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use regex::Regex;

    #[test]
    fn test_initial_name_char_escape() {
        let result = convert_xml_pattern(r"\i", ConvertOptions::xpath());
        assert_eq!(result, r"[A-Za-z_:]");
        let regex = Regex::new(&result).unwrap();
        assert!(regex.is_match("A"));
        assert!(regex.is_match("_"));
        assert!(!regex.is_match("1"));
    }

    #[test]
    fn test_not_initial_name_char_escape() {
        let result = convert_xml_pattern(r"\I", ConvertOptions::xpath());
        assert_eq!(result, r"[^A-Za-z_:]");
        let regex = Regex::new(&result).unwrap();
        assert!(!regex.is_match("A"));
        assert!(regex.is_match("1"));
        assert!(regex.is_match(" "));
    }

    #[test]
    fn test_name_char_escape() {
        let result = convert_xml_pattern(r"\c", ConvertOptions::xpath());
        assert_eq!(result, r"[A-Za-z0-9._:\-]");
        let regex = Regex::new(&result).unwrap();
        assert!(regex.is_match("A"));
        assert!(regex.is_match("1"));
        assert!(regex.is_match("-"));
        assert!(!regex.is_match(" "));
    }

    #[test]
    fn test_not_name_char_escape() {
        let result = convert_xml_pattern(r"\C", ConvertOptions::xpath());
        assert_eq!(result, r"[^A-Za-z0-9._:\-]");
        let regex = Regex::new(&result).unwrap();
        assert!(!regex.is_match("A"));
        assert!(!regex.is_match("1"));
        assert!(regex.is_match(" "));
    }

    #[test]
    fn test_xsd_anchoring() {
        let result = convert_xml_pattern("abc", ConvertOptions::xsd());
        assert_eq!(result, "^abc$");
    }

    #[test]
    fn test_xpath_no_anchoring() {
        let result = convert_xml_pattern("abc", ConvertOptions::xpath());
        assert_eq!(result, "abc");
    }

    #[test]
    fn test_xml_name_pattern() {
        let result = convert_xml_pattern(r"\i\c*", ConvertOptions::xsd());
        assert_eq!(result, r"^[A-Za-z_:][A-Za-z0-9._:\-]*$");
        let regex = Regex::new(&result).unwrap();
        assert!(regex.is_match("foo"));
        assert!(regex.is_match("foo:bar"));
        assert!(regex.is_match("_bar"));
        assert!(!regex.is_match("123"));
    }

    #[test]
    fn test_standard_escapes_preserved() {
        let result = convert_xml_pattern(r"\d+\s*\w+", ConvertOptions::xpath());
        assert_eq!(result, r"\d+\s*\w+");
    }

    #[test]
    fn test_v1_1_preserves_p_escape() {
        let result = convert_xml_pattern(r"\p{L}\P{N}", ConvertOptions::xpath());
        assert_eq!(result, r"\p{L}\P{N}");
    }

    #[test]
    fn test_v1_0_expands_p_category_escape() {
        let result = convert_xml_pattern(r"\p{Lu}*", ConvertOptions::xsd_v1_0());
        assert!(result.starts_with("^["));
        assert!(result.ends_with("]*$"));
        assert!(!result.contains("\\p{"));
        let regex = Regex::new(&result).unwrap();
        assert!(regex.is_match("A"));
        assert!(regex.is_match("ABC"));
        assert!(!regex.is_match("a"));
        // reJ11 contract: U+1D7A8 is Lu in modern Unicode but
        // unassigned in Unicode 3.0, so it must NOT match here.
        let s = format!("A{}", char::from_u32(0x1D7A8).unwrap());
        assert!(!regex.is_match(&s));
    }

    #[test]
    fn test_v1_0_expands_negated_p_category_escape() {
        let result = convert_xml_pattern(r"\P{N}*", ConvertOptions::xsd_v1_0());
        assert!(result.contains("[^"));
        assert!(!result.contains("\\P{"));
        let regex = Regex::new(&result).unwrap();
        assert!(regex.is_match("abc"));
        assert!(!regex.is_match("123"));
    }

    #[test]
    fn test_v1_0_passes_through_block_escape() {
        let result = convert_xml_pattern(r"\p{IsBasicLatin}*", ConvertOptions::xsd_v1_0());
        // Block escapes are not expanded — left for the regex engine.
        assert!(result.contains(r"\p{IsBasicLatin}"));
    }

    #[test]
    fn test_v1_0_passes_through_unknown_category() {
        let result = convert_xml_pattern(r"\p{Xx}", ConvertOptions::xsd_v1_0());
        assert!(result.contains(r"\p{Xx}"));
    }

    #[test]
    fn test_mixed_pattern() {
        let result = convert_xml_pattern(r"\i\c*:\d+", ConvertOptions::xsd());
        assert_eq!(result, r"^[A-Za-z_:][A-Za-z0-9._:\-]*:\d+$");
        let regex = Regex::new(&result).unwrap();
        assert!(regex.is_match("item:123"));
        assert!(!regex.is_match("123:abc"));
    }

    #[test]
    fn test_empty_pattern() {
        let result = convert_xml_pattern("", ConvertOptions::xsd());
        assert_eq!(result, "^$");

        let result = convert_xml_pattern("", ConvertOptions::xpath());
        assert_eq!(result, "");
    }

    #[test]
    fn test_trailing_backslash() {
        let result = convert_xml_pattern(r"abc\", ConvertOptions::xpath());
        assert_eq!(result, r"abc\");
    }

    #[test]
    fn test_rewrite_xsd10_expands_p_but_keeps_name_escapes() {
        let result = rewrite_xsd10_category_escapes(r"\i\c*\p{Lu}+");
        assert!(result.starts_with(r"\i\c*["), "unexpected: {}", result);
        assert!(result.ends_with("]+"), "unexpected: {}", result);
        assert!(!result.contains(r"\p{"));
    }

    #[test]
    fn test_rewrite_xsd10_passes_block_escapes() {
        let result = rewrite_xsd10_category_escapes(r"\p{IsBasicLatin}+");
        assert_eq!(result, r"\p{IsBasicLatin}+");
    }

    #[test]
    fn test_rewrite_xsd10_passes_unknown_names() {
        let result = rewrite_xsd10_category_escapes(r"\p{Xx}");
        assert_eq!(result, r"\p{Xx}");
    }

    #[test]
    fn test_rewrite_xsd10_negated_category() {
        let result = rewrite_xsd10_category_escapes(r"\P{N}+");
        assert!(result.starts_with("[^"));
        assert!(result.ends_with("]+"));
    }
}
