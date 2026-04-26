//! String operations for XPath evaluation.
//!
//! This module implements XPath 2.0 string functions and normalization:
//! - `normalize-space`
//! - `normalize-unicode`
//! - Entity reference handling
//! - Whitespace normalization

use super::error::XPathError;

/// Normalize whitespace in a string (XPath fn:normalize-space).
///
/// - Strips leading and trailing whitespace
/// - Replaces sequences of whitespace with a single space
///
/// # Arguments
///
/// * `value` - The string to normalize
///
/// # Returns
///
/// The normalized string.
pub fn normalize_space(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    let mut prev_was_space = true; // Start true to skip leading spaces

    for ch in value.chars() {
        if is_xml_whitespace(ch) {
            if !prev_was_space {
                result.push(' ');
                prev_was_space = true;
            }
        } else {
            result.push(ch);
            prev_was_space = false;
        }
    }

    // Remove trailing space if present
    if result.ends_with(' ') {
        result.pop();
    }

    result
}

/// Check if a character is XML whitespace.
///
/// XML defines whitespace as: space (0x20), tab (0x09), newline (0x0A), carriage return (0x0D)
#[inline]
pub fn is_xml_whitespace(ch: char) -> bool {
    matches!(ch, ' ' | '\t' | '\n' | '\r')
}

/// Check if a string consists entirely of XML whitespace characters.
///
/// Returns `true` for the empty string (vacuously all-whitespace).
#[inline]
pub fn is_xml_whitespace_str(s: &str) -> bool {
    s.bytes().all(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
}

/// Normalize a string value with entity reference handling.
///
/// Handles standard XML entity references:
/// - `&lt;` -> `<`
/// - `&gt;` -> `>`
/// - `&amp;` -> `&`
/// - `&quot;` -> `"`
/// - `&apos;` -> `'`
/// - `&#xNN;` -> character by hex code
/// - `&#NN;` -> character by decimal code
///
/// # Arguments
///
/// * `value` - The string to normalize
/// * `is_attr` - Whether this is an attribute value (applies additional normalization)
/// * `raise_on_error` - Whether to raise an error on invalid entity references
///
/// # Returns
///
/// The normalized string, or an error for invalid entity references.
pub fn normalize_string_value(
    value: &str,
    is_attr: bool,
    raise_on_error: bool,
) -> Result<String, XPathError> {
    let mut result = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '&' {
            // Parse entity reference
            let mut entity = String::new();

            loop {
                match chars.next() {
                    Some(';') => break,
                    Some(c) => entity.push(c),
                    None => {
                        if raise_on_error {
                            return Err(XPathError::syntax_error(
                                "Entity reference not terminated by semicolon",
                            ));
                        }
                        result.push('&');
                        result.push_str(&entity);
                        break;
                    }
                }
            }

            match resolve_entity(&entity) {
                Some(resolved) => result.push(resolved),
                None => {
                    if raise_on_error {
                        return Err(XPathError::syntax_error(format!(
                            "Unknown entity reference '&{};'",
                            entity
                        )));
                    }
                    result.push('&');
                    result.push_str(&entity);
                    result.push(';');
                }
            }
        } else if is_attr && (ch == '\t' || ch == '\n' || ch == '\r') {
            // In attribute values, normalize newlines and tabs to space
            result.push(' ');
        } else if ch == '\r' {
            // Normalize \r\n to \n, and standalone \r to \n
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            result.push('\n');
        } else {
            result.push(ch);
        }
    }

    Ok(result)
}

/// Resolve an entity reference name to its character.
fn resolve_entity(entity: &str) -> Option<char> {
    match entity {
        "lt" => Some('<'),
        "gt" => Some('>'),
        "amp" => Some('&'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        _ if entity.starts_with('#') => resolve_numeric_entity(&entity[1..]),
        _ => None,
    }
}

/// Resolve a numeric entity reference (decimal or hex).
fn resolve_numeric_entity(entity: &str) -> Option<char> {
    let code = if let Some(hex) = entity.strip_prefix('x') {
        u32::from_str_radix(hex, 16).ok()?
    } else {
        entity.parse::<u32>().ok()?
    };

    char::from_u32(code)
}

/// Concatenate strings.
pub fn concat(values: &[&str]) -> String {
    values.concat()
}

/// Check if a string starts with a prefix.
pub fn starts_with(value: &str, prefix: &str) -> bool {
    value.starts_with(prefix)
}

/// Check if a string ends with a suffix.
pub fn ends_with(value: &str, suffix: &str) -> bool {
    value.ends_with(suffix)
}

/// Check if a string contains a substring.
pub fn contains(value: &str, substring: &str) -> bool {
    value.contains(substring)
}

/// Get the substring before the first occurrence of a pattern.
pub fn substring_before(value: &str, pattern: &str) -> String {
    match value.find(pattern) {
        Some(pos) => value[..pos].to_string(),
        None => String::new(),
    }
}

/// Get the substring after the first occurrence of a pattern.
pub fn substring_after(value: &str, pattern: &str) -> String {
    match value.find(pattern) {
        Some(pos) => value[pos + pattern.len()..].to_string(),
        None => String::new(),
    }
}

/// Get the length of a string in characters.
pub fn string_length(value: &str) -> usize {
    value.chars().count()
}

/// Get a substring (XPath-style 1-based indexing).
///
/// XPath spec: Returns characters whose position p satisfies:
/// `round(start) <= p < round(start) + round(length)`
///
/// # Arguments
///
/// * `value` - The source string
/// * `start` - Start position (1-based, can be negative or fractional)
/// * `length` - Optional length
pub fn substring(value: &str, start: f64, length: Option<f64>) -> String {
    // Handle NaN cases
    if start.is_nan() {
        return String::new();
    }

    let chars: Vec<char> = value.chars().collect();
    let str_len = chars.len() as i64;

    // XPath uses round() for positions (round half away from zero)
    let start_rounded = start.round() as i64;

    match length {
        Some(len) => {
            if len.is_nan() {
                return String::new();
            }
            let len_rounded = len.round() as i64;

            // XPath condition: round(start) <= p < round(start) + round(length)
            // Convert to 0-based: positions [start_rounded, start_rounded + len_rounded)
            // In 0-based indices: [start_rounded - 1, start_rounded + len_rounded - 1)

            // Handle start < 1 (reduces effective length from the beginning)
            let first_pos = start_rounded.max(1); // First valid position (1-based)
            let last_pos = start_rounded + len_rounded; // Exclusive end position (1-based)

            if last_pos <= 1 || first_pos > str_len {
                return String::new();
            }

            let begin_idx = (first_pos - 1) as usize;
            let end_idx = ((last_pos - 1) as usize).min(chars.len());

            if begin_idx >= end_idx {
                return String::new();
            }

            chars[begin_idx..end_idx].iter().collect()
        }
        None => {
            // No length: from start to end of string
            if start_rounded > str_len {
                return String::new();
            }
            let begin_idx = (start_rounded.max(1) - 1) as usize;
            chars[begin_idx..].iter().collect()
        }
    }
}

/// Convert string to uppercase.
pub fn upper_case(value: &str) -> String {
    value.to_uppercase()
}

/// Convert string to lowercase.
pub fn lower_case(value: &str) -> String {
    value.to_lowercase()
}

/// Translate characters in a string.
///
/// Implements XPath `translate(string, map-from, map-to)`.
pub fn translate(value: &str, map_from: &str, map_to: &str) -> String {
    let from_chars: Vec<char> = map_from.chars().collect();
    let to_chars: Vec<char> = map_to.chars().collect();

    value
        .chars()
        .filter_map(|ch| {
            match from_chars.iter().position(|&c| c == ch) {
                Some(pos) => {
                    if pos < to_chars.len() {
                        Some(to_chars[pos])
                    } else {
                        None // Remove character if no replacement
                    }
                }
                None => Some(ch),
            }
        })
        .collect()
}

/// Convert a string to a sequence of codepoints.
pub fn string_to_codepoints(value: &str) -> Vec<u32> {
    value.chars().map(|c| c as u32).collect()
}

/// Convert a sequence of codepoints to a string.
pub fn codepoints_to_string(codepoints: &[u32]) -> Option<String> {
    codepoints
        .iter()
        .map(|&cp| char::from_u32(cp))
        .collect::<Option<String>>()
}

/// Compare two strings.
///
/// Returns -1, 0, or 1 for less than, equal, or greater than.
pub fn compare(a: &str, b: &str) -> i32 {
    match a.cmp(b) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

/// Join strings with a separator.
///
/// Implements XPath `fn:string-join($strings, $separator)`.
pub fn string_join(values: &[&str], separator: &str) -> String {
    values.join(separator)
}

/// Unicode normalization forms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnicodeNormalizationForm {
    /// NFC (Canonical Decomposition, followed by Canonical Composition)
    NFC,
    /// NFD (Canonical Decomposition)
    NFD,
    /// NFKC (Compatibility Decomposition, followed by Canonical Composition)
    NFKC,
    /// NFKD (Compatibility Decomposition)
    NFKD,
}

impl UnicodeNormalizationForm {
    /// Parse normalization form from string (case-insensitive).
    pub fn parse(s: &str) -> Option<Self> {
        let trimmed = s.trim();
        if trimmed.eq_ignore_ascii_case("NFC") {
            Some(Self::NFC)
        } else if trimmed.eq_ignore_ascii_case("NFD") {
            Some(Self::NFD)
        } else if trimmed.eq_ignore_ascii_case("NFKC") {
            Some(Self::NFKC)
        } else if trimmed.eq_ignore_ascii_case("NFKD") {
            Some(Self::NFKD)
        } else if trimmed.is_empty() {
            // Empty string means no normalization
            None
        } else {
            None
        }
    }
}

/// Normalize a string using Unicode normalization.
///
/// Uses the `unicode-normalization` crate for actual normalization.
/// If form is None (empty string input), returns the input unchanged.
#[cfg(feature = "unicode-normalization")]
pub fn normalize_unicode(value: &str, form: Option<UnicodeNormalizationForm>) -> String {
    use unicode_normalization::UnicodeNormalization;

    match form {
        Some(UnicodeNormalizationForm::NFC) => value.nfc().collect(),
        Some(UnicodeNormalizationForm::NFD) => value.nfd().collect(),
        Some(UnicodeNormalizationForm::NFKC) => value.nfkc().collect(),
        Some(UnicodeNormalizationForm::NFKD) => value.nfkd().collect(),
        None => value.to_string(),
    }
}

/// Normalize a string using Unicode normalization (fallback without feature).
///
/// Without the unicode-normalization feature, this only handles the no-op case.
#[cfg(not(feature = "unicode-normalization"))]
pub fn normalize_unicode(
    value: &str,
    form: Option<UnicodeNormalizationForm>,
) -> Result<String, super::error::XPathError> {
    match form {
        None => Ok(value.to_string()),
        Some(f) => Err(super::error::XPathError::not_implemented(format!(
            "Unicode normalization form {:?} requires unicode-normalization feature",
            f
        ))),
    }
}

/// Encode a string for use in a URI per RFC 3986.
///
/// Only alphanumeric characters and `-`, `_`, `.`, `~` are left unescaped.
/// All other characters are percent-encoded using UTF-8.
pub fn encode_for_uri(value: &str) -> String {
    let mut result = String::with_capacity(value.len() * 3);
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric()
            || byte == b'-'
            || byte == b'_'
            || byte == b'.'
            || byte == b'~'
        {
            result.push(byte as char);
        } else {
            result.push('%');
            result.push(to_hex_digit(byte >> 4));
            result.push(to_hex_digit(byte & 0x0F));
        }
    }
    result
}

/// Escape an IRI to produce a valid URI.
///
/// Less restrictive than encode-for-uri: allows most ASCII printable characters
/// except space, `<`, `>`, `"`, `{`, `}`, `|`, `\`, `^`, and `` ` ``.
pub fn iri_to_uri(value: &str) -> String {
    let mut result = String::with_capacity(value.len() * 3);
    for byte in value.bytes() {
        // Space is always encoded
        if byte == b' ' {
            result.push_str("%20");
        } else if (0x20..0x7F).contains(&byte)
            && byte != b'<'
            && byte != b'>'
            && byte != b'"'
            && byte != b'{'
            && byte != b'}'
            && byte != b'|'
            && byte != b'\\'
            && byte != b'^'
            && byte != b'`'
        {
            result.push(byte as char);
        } else {
            result.push('%');
            result.push(to_hex_digit(byte >> 4));
            result.push(to_hex_digit(byte & 0x0F));
        }
    }
    result
}

/// Escape a URI for use in HTML.
///
/// Escapes characters outside the ASCII printable range (0x20-0x7E).
pub fn escape_html_uri(value: &str) -> String {
    let mut result = String::with_capacity(value.len() * 3);
    for byte in value.bytes() {
        if (0x20..0x7F).contains(&byte) {
            result.push(byte as char);
        } else {
            result.push('%');
            result.push(to_hex_digit(byte >> 4));
            result.push(to_hex_digit(byte & 0x0F));
        }
    }
    result
}

/// Convert a nibble (0-15) to a hex digit character.
#[inline]
fn to_hex_digit(nibble: u8) -> char {
    if nibble < 10 {
        (b'0' + nibble) as char
    } else {
        (b'A' + nibble - 10) as char
    }
}

/// Compare two strings by codepoint (ordinal comparison).
///
/// Returns true if the strings are equal by codepoint comparison.
pub fn codepoint_equal(a: &str, b: &str) -> bool {
    a == b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_space() {
        assert_eq!(normalize_space("  hello   world  "), "hello world");
        assert_eq!(normalize_space("\t\nhello\r\nworld\t"), "hello world");
        assert_eq!(normalize_space(""), "");
        assert_eq!(normalize_space("   "), "");
        assert_eq!(normalize_space("no extra spaces"), "no extra spaces");
    }

    #[test]
    fn test_is_xml_whitespace() {
        assert!(is_xml_whitespace(' '));
        assert!(is_xml_whitespace('\t'));
        assert!(is_xml_whitespace('\n'));
        assert!(is_xml_whitespace('\r'));
        assert!(!is_xml_whitespace('a'));
    }

    #[test]
    fn test_is_xml_whitespace_str() {
        assert!(is_xml_whitespace_str(""));
        assert!(is_xml_whitespace_str(" "));
        assert!(is_xml_whitespace_str(" \t\n\r"));
        assert!(!is_xml_whitespace_str("hello"));
        assert!(!is_xml_whitespace_str(" a "));
    }

    #[test]
    fn test_normalize_string_value_entities() {
        assert_eq!(
            normalize_string_value("&lt;&gt;&amp;&quot;&apos;", false, true).unwrap(),
            "<>&\"'"
        );
    }

    #[test]
    fn test_normalize_string_value_numeric_entities() {
        assert_eq!(
            normalize_string_value("&#65;&#x42;", false, true).unwrap(),
            "AB"
        );
    }

    #[test]
    fn test_normalize_string_value_attr() {
        assert_eq!(
            normalize_string_value("a\tb\nc", true, true).unwrap(),
            "a b c"
        );
    }

    #[test]
    fn test_normalize_string_value_newlines() {
        assert_eq!(
            normalize_string_value("a\r\nb\rc\n", false, true).unwrap(),
            "a\nb\nc\n"
        );
    }

    #[test]
    fn test_concat() {
        assert_eq!(concat(&["a", "b", "c"]), "abc");
        assert_eq!(concat(&[]), "");
    }

    #[test]
    fn test_starts_ends_with() {
        assert!(starts_with("hello", "he"));
        assert!(!starts_with("hello", "lo"));
        assert!(ends_with("hello", "lo"));
        assert!(!ends_with("hello", "he"));
    }

    #[test]
    fn test_substring_before_after() {
        assert_eq!(substring_before("hello world", " "), "hello");
        assert_eq!(substring_after("hello world", " "), "world");
        assert_eq!(substring_before("hello", " "), "");
        assert_eq!(substring_after("hello", " "), "");
    }

    #[test]
    fn test_string_length() {
        assert_eq!(string_length("hello"), 5);
        assert_eq!(string_length(""), 0);
        assert_eq!(string_length("日本語"), 3); // Multi-byte chars
    }

    #[test]
    fn test_substring() {
        assert_eq!(substring("hello", 2.0, Some(3.0)), "ell");
        assert_eq!(substring("hello", 2.0, None), "ello");
        assert_eq!(substring("hello", 1.0, Some(5.0)), "hello");
        assert_eq!(substring("hello", 0.0, Some(3.0)), "he");
    }

    #[test]
    fn test_case_conversion() {
        assert_eq!(upper_case("Hello World"), "HELLO WORLD");
        assert_eq!(lower_case("Hello World"), "hello world");
    }

    #[test]
    fn test_translate() {
        assert_eq!(translate("bar", "abc", "ABC"), "BAr");
        assert_eq!(translate("--aaa--", "abc-", "ABC"), "AAA");
    }

    #[test]
    fn test_codepoints() {
        assert_eq!(string_to_codepoints("ABC"), vec![65, 66, 67]);
        assert_eq!(codepoints_to_string(&[65, 66, 67]).unwrap(), "ABC");
    }

    #[test]
    fn test_compare() {
        assert_eq!(compare("abc", "abd"), -1);
        assert_eq!(compare("abc", "abc"), 0);
        assert_eq!(compare("abd", "abc"), 1);
    }
}
