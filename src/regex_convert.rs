//! XML Schema / XPath 2.0 regex pattern conversion.
//!
//! This module provides shared regex pattern conversion for both XSD pattern facets
//! and XPath 2.0 regex functions. XPath 2.0 and XSD use the same regex dialect
//! (XML Schema regex), which differs from standard regex in several ways:
//!
//! - XSD-specific character class escapes: `\i`, `\I`, `\c`, `\C`
//! - XSD patterns are implicitly anchored (must match entire string)
//! - XPath regex functions do not anchor patterns

/// Options for pattern conversion.
#[derive(Debug, Clone, Copy, Default)]
pub struct ConvertOptions {
    /// Whether to anchor the pattern with `^...$` (XSD = true, XPath = false)
    pub anchor: bool,
}

impl ConvertOptions {
    /// Create options for XSD pattern facets (anchored).
    pub fn xsd() -> Self {
        Self { anchor: true }
    }

    /// Create options for XPath regex functions (unanchored).
    pub fn xpath() -> Self {
        Self { anchor: false }
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
/// # Arguments
/// - `pattern`: The XSD/XPath regex pattern
/// - `options`: Conversion options (anchoring, etc.)
///
/// # Returns
/// A Rust-compatible regex pattern string.
pub fn convert_xml_pattern(pattern: &str, options: ConvertOptions) -> String {
    let extra_capacity = if options.anchor { 4 } else { 0 };
    let mut result = String::with_capacity(pattern.len() + extra_capacity);

    if options.anchor {
        result.push('^');
    }

    let mut chars = pattern.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if let Some(&next) = chars.peek() {
                match next {
                    // XSD-specific character class escapes
                    'i' => {
                        // Initial name character: Letter | '_' | ':'
                        chars.next();
                        result.push_str(r"[A-Za-z_:]");
                    }
                    'I' => {
                        // Not initial name character
                        chars.next();
                        result.push_str(r"[^A-Za-z_:]");
                    }
                    'c' => {
                        // Name character: Letter | Digit | '.' | '-' | '_' | ':' | CombiningChar | Extender
                        chars.next();
                        result.push_str(r"[A-Za-z0-9._:\-]");
                    }
                    'C' => {
                        // Not name character
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
                    // Unicode category escapes \p{...} - pass through (Rust regex supports these)
                    'p' | 'P' => {
                        result.push('\\');
                        result.push(next);
                        chars.next();
                        // Copy the block name including braces
                        if chars.peek() == Some(&'{') {
                            for c in chars.by_ref() {
                                result.push(c);
                                if c == '}' {
                                    break;
                                }
                            }
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
                // Trailing backslash
                result.push('\\');
            }
        } else {
            result.push(ch);
        }
    }

    if options.anchor {
        result.push('$');
    }
    result
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
    fn test_unicode_category_escapes() {
        let result = convert_xml_pattern(r"\p{L}\P{N}", ConvertOptions::xpath());
        assert_eq!(result, r"\p{L}\P{N}");
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
        // Edge case: trailing backslash is preserved
        let result = convert_xml_pattern(r"abc\", ConvertOptions::xpath());
        assert_eq!(result, r"abc\");
    }
}
