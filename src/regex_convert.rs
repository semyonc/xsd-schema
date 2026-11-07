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

use crate::regex_xsd_blocks::{block_ranges, push_ranges_body};
use crate::regex_xsd_unicode::{
    expand_xsd_category_body, xsd10_non_digit_neg_body, xsd10_non_word_char_body,
    xsd10_private_use_block_body, xsd10_word_char_body,
};
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
        Self {
            anchor: true,
            xsd_version: XsdVersion::V1_1,
        }
    }

    /// Create options for XSD 1.0 pattern facets (anchored, Unicode-3.0 pin).
    pub fn xsd_v1_0() -> Self {
        Self {
            anchor: true,
            xsd_version: XsdVersion::V1_0,
        }
    }

    /// Create options for XPath regex functions (unanchored, modern Unicode).
    pub fn xpath() -> Self {
        Self {
            anchor: false,
            xsd_version: XsdVersion::V1_1,
        }
    }
}

/// Apply MS dialect leniencies to a pattern when the schema set is
/// configured with [`RegexCompat::LenientMs`](crate::RegexCompat::LenientMs).
///
/// The textual preprocess is intentionally narrow. It only rewrites
/// constructs that *no* runtime backend accepts natively, so they would
/// otherwise fail at compile time even with the strict §F/§G grammar
/// gate skipped:
///
/// - `(?#…)` inline comments. Stripped (including the closing `)`).
///   Both Rust `regex` and regexml reject `(?#` as an unrecognized
///   group prefix. .NET treats it as a comment per its native syntax.
///
/// Other MS dialect constructs (`^`/`$` anchors outside char class,
/// non-capturing `(?:…)`, backreferences `\1`, reluctant quantifiers
/// `*?`/`+?`) are left alone — the runtime backend handles them
/// natively once the strict grammar gate is bypassed:
///
/// - Rust `regex` (default features) natively accepts `^`/`$` as
///   anchors, `(?:…)`, named groups, reluctant quantifiers; it does
///   *not* support backreferences or lookaround.
/// - regexml `xpath()` (xsd11 feature) natively accepts `^`/`$`,
///   backreferences (`op_back_reference.rs`), `(?:…)`, reluctant
///   quantifiers; it does not implement lookaround at all.
///
/// Constructs neither backend supports (lookahead `(?=…)`, lookbehind
/// `(?<=…)`) still fail at compile time even under `LenientMs` — that
/// is an engine limit, not a grammar choice.
///
/// Returns the (possibly rewritten) pattern. When [`RegexCompat::Strict`](crate::RegexCompat::Strict)
/// is in effect, callers should not invoke this.
pub fn lenient_ms_preprocess(pattern: &str) -> std::borrow::Cow<'_, str> {
    if !pattern.contains("(?#") {
        return std::borrow::Cow::Borrowed(pattern);
    }
    std::borrow::Cow::Owned(strip_inline_comments(pattern))
}

/// Strip `(?#…)` comments. Skips comment-like sequences inside character
/// classes (where `(` has no special meaning). A `\` escapes the following
/// character so `\(?#x)` is preserved.
fn strip_inline_comments(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len());
    let mut in_class = false;
    let mut chars = pattern.char_indices().peekable();
    while let Some((idx, ch)) = chars.next() {
        if ch == '\\' {
            out.push(ch);
            if let Some((_, next)) = chars.next() {
                out.push(next);
            }
            continue;
        }
        if ch == '[' {
            in_class = true;
            out.push(ch);
            continue;
        }
        if ch == ']' {
            in_class = false;
            out.push(ch);
            continue;
        }
        if !in_class && ch == '(' && pattern[idx..].starts_with("(?#") {
            // Skip past matching `)`. Comments cannot be nested per
            // .NET / PCRE conventions, but we still respect `\)`.
            let after = idx + "(?#".len();
            let remainder = &pattern[after..];
            let mut close = None;
            let mut j = 0;
            let rb = remainder.as_bytes();
            while j < rb.len() {
                if rb[j] == b'\\' && j + 1 < rb.len() {
                    j += 2;
                    continue;
                }
                if rb[j] == b')' {
                    close = Some(j);
                    break;
                }
                j += 1;
            }
            if let Some(c) = close {
                let consume_to = after + c + 1;
                while let Some(&(next_idx, _)) = chars.peek() {
                    if next_idx < consume_to {
                        chars.next();
                    } else {
                        break;
                    }
                }
                continue;
            }
            // Unterminated `(?#` — fall through and emit literally.
        }
        out.push(ch);
    }
    out
}

/// Convert XSD/XPath regex pattern to Rust regex syntax.
///
/// Handles XSD-specific character class escapes:
/// - `\i` -> `[A-Za-z_:]` (XML initial name character)
/// - `\I` -> `[^A-Za-z_:]` (not initial name character)
/// - `\c` -> `[A-Za-z0-9._:\-]` (XML name character)
/// - `\C` -> `[^A-Za-z0-9._:\-]` (not name character)
///
/// Block escapes `\p{IsX}` / `\P{IsX}` are written out as the code-point
/// ranges of block `X`, which the `regex` crate cannot name, and a character
/// class subtraction `[G-[C]]` as the `regex` set difference `[G--[C]]`.
/// Under `XsdVersion::V1_0`, category escapes `\p{X}` and `\P{X}` for
/// recognized general-category names are expanded to Unicode-3.0 ranges.
/// Under `XsdVersion::V1_1` a block escape whose name is an IsBlock name
/// (`'Is' [a-zA-Z0-9-]+`) but no recognized block denotes every character
/// (XSD 1.1 Part 2 §G.4.2.4). Other unknown names, an unrecognized block
/// name under `V1_0` included, are passed through verbatim for the regex
/// engine to judge.
///
/// # Arguments
/// - `pattern`: The XSD/XPath regex pattern
/// - `options`: Conversion options (anchoring, XSD version)
///
/// # Returns
/// A pattern in the syntax of the `regex` crate.
pub fn convert_xml_pattern(pattern: &str, options: ConvertOptions) -> String {
    convert(pattern, options).0
}

/// [`convert_xml_pattern`], with two kinds of mistake reported as errors
/// rather than passed through. A block escape whose block name is not
/// recognized under XSD 1.0, which lists the recognized block names
/// (Appendix F), or that is no IsBlock name (`'Is' [a-zA-Z0-9-]+`) at all;
/// under XSD 1.1 an unrecognized IsBlock name denotes every character
/// (§G.4.2.4), as in [`convert_xml_pattern`]. And a
/// character class that the class translation would otherwise turn into a
/// valid `regex` class: an empty one (`[]`, `[^]`) or a subtraction with
/// nothing before its `-` (`[-[a]]`); a character group holds at least one
/// member in both versions of the grammar.
// The pattern facets of the build without `xsd11` use it; that build alone
// runs patterns on the `regex` crate.
#[cfg_attr(feature = "xsd11", allow(dead_code))]
pub(crate) fn try_convert_xml_pattern(
    pattern: &str,
    options: ConvertOptions,
) -> Result<String, String> {
    match convert(pattern, options) {
        (converted, None) => Ok(converted),
        (_, Some(error)) => Err(error),
    }
}

/// A character class of the pattern being converted that is still open.
struct OpenClass {
    /// Where its `[` is in the output.
    start: usize,
    /// Where its first member is (or will be) in the output, after `[` or `[^`.
    content_start: usize,
    /// Whether it is a negative character group (`[^…]`).
    negated: bool,
}

/// [`convert_xml_pattern`], also returning the first mistake
/// [`try_convert_xml_pattern`] reports (the converted text carries it as
/// written).
fn convert(pattern: &str, options: ConvertOptions) -> (String, Option<String>) {
    let extra_capacity = if options.anchor { 4 } else { 0 };
    // Under V1_0, `\d` / `\D` / `\w` / `\W` and `\p{X}` expand to multi-KB
    // explicit ranges; over-allocate to avoid repeated reallocations
    // (mirrors `rewrite_xsd10_category_escapes` at line 184).
    let initial_capacity = match options.xsd_version {
        XsdVersion::V1_0 => pattern.len() * 4 + extra_capacity,
        XsdVersion::V1_1 => pattern.len() + extra_capacity,
    };
    let mut result = String::with_capacity(initial_capacity);

    if options.anchor {
        result.push('^');
    }

    let mut error = None;
    // The character classes open at this point, innermost last: the
    // subtrahend of a class subtraction is a class inside the class.
    let mut classes: Vec<OpenClass> = Vec::new();
    let mut chars = pattern.chars().peekable();
    while let Some(ch) = chars.next() {
        let in_class = !classes.is_empty();
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
                // XSD 1.0 multi-character class escapes \d, \D, \w, \W —
                // expand to explicit Unicode-3.0 ranges so the regex engine
                // (which uses modern Unicode for \d / \w / \D / \W) cannot
                // disagree with the MS reS/reT/reU test expectations. Inside
                // a character class only the positive forms expand inline;
                // the negated forms fall through to the engine's native
                // escape (set complementation isn't expressible inline).
                'd' | 'D' | 'w' | 'W'
                    if options.xsd_version == XsdVersion::V1_0
                        && expand_xsd10_class_escape(&mut result, next, in_class) =>
                {
                    chars.next();
                }
                // Standard escapes - pass through
                'd' | 'D' | 's' | 'S' | 'w' | 'W' | 'n' | 'r' | 't' | '\\' | '|' | '.' | '?'
                | '*' | '+' | '{' | '}' | '(' | ')' | '[' | ']' | '^' | '$' | '-' => {
                    result.push('\\');
                    result.push(next);
                    chars.next();
                }
                // Unicode category escapes \p{...} / \P{...}
                'p' | 'P' => {
                    let negated = next == 'P';
                    chars.next();
                    convert_property_escape(
                        &mut result,
                        &mut chars,
                        negated,
                        in_class,
                        options.xsd_version,
                        &mut error,
                    );
                }
                // Other escapes - pass through
                _ => {
                    result.push('\\');
                    result.push(next);
                    chars.next();
                }
            }
        } else if ch == '[' {
            let negated = chars.next_if_eq(&'^').is_some();
            let start = result.len();
            result.push_str(if negated { "[^" } else { "[" });
            classes.push(OpenClass {
                start,
                content_start: result.len(),
                negated,
            });
        } else if ch == ']' {
            if classes
                .pop()
                .is_some_and(|open| result.len() == open.content_start)
            {
                error.get_or_insert_with(|| "empty character class".to_string());
            }
            result.push(']');
        } else if ch == '-' && in_class {
            push_class_hyphen(&mut result, &mut classes, &chars, &mut error);
        } else {
            result.push(ch);
        }
    }

    if options.anchor {
        result.push('$');
    }
    (result, error)
}

/// Convert a `-` inside a character class.
///
/// XSD 1.1 §G.4.2 \[76\]: "A '-' character is recognized as a subtraction
/// operator (and hence, as terminating the posCharGroup or negCharGroup) if
/// it is immediately followed by a '[' character." The `regex` crate reads
/// `[G-[C]]` as the union of `G`, `-` and `C`; its set difference is
/// `[G--[C]]`. In `[^G--[C]]` the `^` would negate the difference, while XSD
/// subtracts `C` from the negative group, so `[^G-[C]]` becomes
/// `[[^G]--[C]]`. A literal hyphen just before the operator (`[a--[b]]`, `a`
/// and `-` minus `b`) is escaped, since `regex` reads `---` otherwise. Any
/// other hyphen is copied. A `-[` with no member before it is an `error`
/// (the group before a subtraction is not empty, and a `[` is no member);
/// its hyphen is copied.
fn push_class_hyphen(
    out: &mut String,
    classes: &mut [OpenClass],
    rest: &std::iter::Peekable<std::str::Chars<'_>>,
    error: &mut Option<String>,
) {
    let mut ahead = rest.clone();
    match (ahead.next(), ahead.next()) {
        (Some('['), _)
            if classes
                .last()
                .is_some_and(|open| out.len() == open.content_start) =>
        {
            error.get_or_insert_with(|| {
                "a character class subtraction needs a character group before its '-'".to_string()
            });
            out.push('-');
        }
        (Some('['), _) => {
            if let Some(open) = classes.last_mut().filter(|open| open.negated) {
                out.insert(open.start, '[');
                out.push(']');
                // The class is now the positive wrapper `[[^G]--[C]]`.
                open.negated = false;
            }
            out.push_str("--");
        }
        (Some('-'), Some('[')) => out.push_str(r"\-"),
        _ => out.push('-'),
    }
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
        if matches!(next, 'd' | 'D' | 'w' | 'W')
            && expand_xsd10_class_escape(&mut result, next, in_class)
        {
            chars.next();
            continue;
        }
        if next != 'p' && next != 'P' {
            result.push('\\');
            result.push(next);
            chars.next();
            continue;
        }
        let negated = next == 'P';
        chars.next();
        handle_category_escape(&mut result, &mut chars, negated, in_class, true);
    }
    result
}

/// Expand the XSD 1.0 multi-character class escapes `\d`, `\D`, `\w`, `\W`
/// to explicit Unicode-3.0 ranges. Returns `true` if the expansion was
/// emitted; `false` means the caller should fall back to passing the escape
/// through verbatim.
///
/// All four expansions are BMP-bounded, matching MS test expectations
/// authored against pre-Unicode-3.1 / UTF-16-unit semantics:
///   - `\d` → `[<Nd>]` (positive, BMP)
///   - `\D` → `[^<Nd>U+10000-U+10FFFD]` (negation excludes supplementary plane)
///   - `\w` → `[<L+M+N+S>]` (positive, BMP — excludes Cn / supplementary)
///   - `\W` → `[<P+Z+C>]` (positive, BMP — excludes supplementary)
///
/// Inside a character class only `\d` and `\w` expand inline (their bodies
/// merge cleanly into the surrounding class); `\D` / `\W` would need set
/// complementation, so they are passed through to the engine in that
/// position.
fn expand_xsd10_class_escape(out: &mut String, escape: char, in_class: bool) -> bool {
    let (body, negated): (&str, bool) = match escape {
        'd' => (expand_xsd_category_body("Nd").unwrap_or(""), false),
        'D' => (xsd10_non_digit_neg_body(), true),
        'w' => (xsd10_word_char_body(), false),
        'W' => (xsd10_non_word_char_body(), false),
        _ => return false,
    };
    if body.is_empty() {
        return false;
    }
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

/// Validate XSD 1.0 regex character-class hyphen rules — stricter than the backend
/// parsers and stricter than XSD 1.1.
///
/// Per XSD 1.0 Datatypes §F (regex grammar productions \[14\]–\[22\]) under longest-match
/// disambiguation, an unescaped `-` inside a character class must be (a) the first
/// atom (immediately after `[` or `[^`), (b) the last atom (immediately before `]`),
/// (c) the middle character of an `seRange` (e.g. `a-z`), or (d) the subtraction
/// operator separating a `posCharGroup` from a nested `charClassExpr` (`...-[...]`).
/// Any other position — e.g. `[a-c-1]`, `[^a-d-b-c]`, `[a-z-+]`, `[--z]` — is
/// ambiguous and a syntax error in XSD 1.0. XSD 1.1 (Datatypes 1.1 §G) relaxed
/// these rules, allowing literal hyphens elsewhere via `XmlCharIncDash`, so this
/// validator must only be invoked for XSD 1.0.
pub fn validate_xml_pattern_syntax(pattern: &str) -> Result<(), String> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        match chars[index] {
            '\\' => index = skip_escape(&chars, index + 1),
            '[' => index = validate_char_class(&chars, index + 1)?,
            _ => index += 1,
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct ClassAtom {
    available_for_range: bool,
    unescaped_hyphen: bool,
}

fn validate_char_class(chars: &[char], mut index: usize) -> Result<usize, String> {
    let mut prev_atom: Option<ClassAtom> = None;
    let mut at_group_start = true;
    let mut allow_nested_class = false;

    if chars.get(index) == Some(&'^') {
        index += 1;
    }

    while index < chars.len() {
        match chars[index] {
            '\\' => {
                let (is_single_char, next_index) = consume_class_escape(chars, index + 1);
                prev_atom = Some(ClassAtom {
                    available_for_range: is_single_char,
                    unescaped_hyphen: false,
                });
                at_group_start = false;
                allow_nested_class = false;
                index = next_index;
            }
            '[' => {
                if !allow_nested_class {
                    return Err("unescaped '[' in character class".to_string());
                }
                index = validate_char_class(chars, index + 1)?;
                prev_atom = Some(ClassAtom {
                    available_for_range: false,
                    unescaped_hyphen: false,
                });
                at_group_start = false;
                allow_nested_class = false;
            }
            ']' => return Ok(index + 1),
            '-' => {
                let next = chars.get(index + 1).copied();
                let next_after = chars.get(index + 2).copied();

                if next == Some('[') {
                    allow_nested_class = true;
                    prev_atom = None;
                    at_group_start = false;
                    index += 1;
                    continue;
                }

                if at_group_start
                    || next == Some(']')
                    || (next == Some('-') && next_after == Some('['))
                {
                    prev_atom = Some(ClassAtom {
                        available_for_range: true,
                        unescaped_hyphen: true,
                    });
                    at_group_start = false;
                    allow_nested_class = false;
                    index += 1;
                    continue;
                }

                let Some(prev) = prev_atom else {
                    return Err("hyphen is not a valid character range operator".to_string());
                };
                if !prev.available_for_range || prev.unescaped_hyphen {
                    return Err("hyphen is not a valid character range operator".to_string());
                }

                let Some((range_end, next_index)) = peek_single_class_atom(chars, index + 1) else {
                    return Err("hyphen is not followed by a valid range endpoint".to_string());
                };
                if range_end.unescaped_hyphen {
                    return Err("unescaped hyphen cannot be a character range endpoint".to_string());
                }

                prev_atom = Some(ClassAtom {
                    available_for_range: false,
                    unescaped_hyphen: false,
                });
                at_group_start = false;
                allow_nested_class = false;
                index = next_index;
            }
            _ => {
                prev_atom = Some(ClassAtom {
                    available_for_range: true,
                    unescaped_hyphen: false,
                });
                at_group_start = false;
                allow_nested_class = false;
                index += 1;
            }
        }
    }

    Err("unterminated character class".to_string())
}

fn skip_escape(chars: &[char], index: usize) -> usize {
    if matches!(chars.get(index), Some('p' | 'P')) && chars.get(index + 1) == Some(&'{') {
        let mut cursor = index + 2;
        while cursor < chars.len() {
            if chars[cursor] == '}' {
                return cursor + 1;
            }
            cursor += 1;
        }
        return cursor;
    }
    index.saturating_add(1).min(chars.len())
}

fn consume_class_escape(chars: &[char], index: usize) -> (bool, usize) {
    let is_single_char = matches!(
        chars.get(index),
        Some(
            'n' | 'r'
                | 't'
                | '\\'
                | '|'
                | '.'
                | '?'
                | '*'
                | '+'
                | '('
                | ')'
                | '{'
                | '}'
                | '-'
                | '['
                | ']'
                | '^'
        )
    );
    (is_single_char, skip_escape(chars, index))
}

fn peek_single_class_atom(chars: &[char], index: usize) -> Option<(ClassAtom, usize)> {
    match chars.get(index).copied()? {
        '\\' => {
            let (is_single_char, next_index) = consume_class_escape(chars, index + 1);
            is_single_char.then_some((
                ClassAtom {
                    available_for_range: false,
                    unescaped_hyphen: false,
                },
                next_index,
            ))
        }
        '[' | ']' => None,
        '-' => Some((
            ClassAtom {
                available_for_range: false,
                unescaped_hyphen: true,
            },
            index + 1,
        )),
        _ => Some((
            ClassAtom {
                available_for_range: false,
                unescaped_hyphen: false,
            },
            index + 1,
        )),
    }
}

/// Look up the XSD 1.0 / Unicode 3.0 char-class body for `\p{name}`,
/// covering both general-category codes and block names. Returns `None`
/// for names handled by the engine natively (other `IsX` blocks, unknown
/// names, Cn/Cs).
///
/// `IsPrivateUse` is overridden here because regexml's block lookup
/// follows the DIS XSD 1.1 backwards-compatibility table and unions the
/// BMP PUA with the supplementary PUAs (Plane 15 / 16). Those areas did
/// not exist in Unicode 3.0 and the W3C MS reL/reM/reN tests require them
/// to be excluded under XSD 1.0.
fn xsd10_category_or_block_body(name: &str) -> Option<&'static str> {
    if name == "IsPrivateUse" {
        return Some(xsd10_private_use_block_body());
    }
    expand_xsd_category_body(name)
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
    let Some(body) = xsd10_category_or_block_body(name) else {
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

/// Parse `{name}` (the body of a `\p{…}` / `\P{…}` escape) and append either
/// the expanded character class (when `try_expand` is true and `name` is a
/// recognized general-category code) or the verbatim original token.
///
/// Caller has already consumed `\` and the `p`/`P`; `chars` is positioned
/// just before the opening `{` (or at a stray `\p`/`\P` if `{` is absent).
fn handle_category_escape(
    out: &mut String,
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    negated: bool,
    in_class: bool,
    try_expand: bool,
) {
    let marker = if negated { 'P' } else { 'p' };
    let Some((name, closed)) = read_property_name(chars) else {
        out.push('\\');
        out.push(marker);
        return;
    };
    if try_expand && closed && try_expand_category(out, &name, negated, in_class) {
        return;
    }
    push_verbatim_property(out, marker, &name, closed);
}

/// [`handle_category_escape`] for [`convert_xml_pattern`]: a block escape
/// `\p{IsX}` / `\P{IsX}` becomes the ranges of block `X` (see
/// [`push_block`]) under both XSD versions, since the `regex` crate knows no
/// block names. An unrecognized block name denotes every character under
/// XSD 1.1, as the `xsd11` build has it; under XSD 1.0, or when the name is
/// not an IsBlock name at all, the escape is copied verbatim and reported in
/// `error` (unless an earlier mistake is). Category escapes are handled as
/// in [`handle_category_escape`], expanded under XSD 1.0 only.
fn convert_property_escape(
    out: &mut String,
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    negated: bool,
    in_class: bool,
    xsd_version: XsdVersion,
    error: &mut Option<String>,
) {
    let marker = if negated { 'P' } else { 'p' };
    let Some((name, closed)) = read_property_name(chars) else {
        out.push('\\');
        out.push(marker);
        return;
    };
    if closed {
        if let Some(block) = name.strip_prefix("Is").filter(|block| !block.is_empty()) {
            if let Some(ranges) = block_ranges(block, xsd_version) {
                push_block(out, ranges, negated, in_class);
                return;
            }
            let is_block_name = block.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
            if xsd_version == XsdVersion::V1_1 && is_block_name {
                // XSD 1.1 §G.4.2.4: "If a string "IsX" matches the
                // non-terminal IsBlock but X is not a recognized block name,
                // then the expressions "\p{IsX}" and "\P{IsX}" each denote
                // the set of all characters."
                push_block(out, &[(0, 0x10FFFF)], false, in_class);
                return;
            }
            error.get_or_insert_with(|| {
                if is_block_name {
                    format!(
                        "\\{marker}{{{name}}}: \"{block}\" is not a recognized Unicode block name"
                    )
                } else {
                    format!(
                        "\\{marker}{{{name}}}: \"{block}\" is not a block name \
                         ('Is' followed by ASCII letters, digits and hyphens)"
                    )
                }
            });
        } else if xsd_version == XsdVersion::V1_0
            && try_expand_category(out, &name, negated, in_class)
        {
            return;
        }
    }
    push_verbatim_property(out, marker, &name, closed);
}

/// Write the block escape with the code-point ranges `ranges` out as a
/// class: `[ranges]` or `[^ranges]`; inside a class the bare ranges, or a
/// nested `[^ranges]` for `\P{IsX}`, which the `regex` crate unions into the
/// enclosing class. A block of surrogates holds no character a string can
/// have, so its `\p{IsX}` is written as the complement of every character
/// and its `\P{IsX}` as every character.
fn push_block(out: &mut String, ranges: &[(u32, u32)], negated: bool, in_class: bool) {
    let mut body = String::new();
    let negated = if push_ranges_body(&mut body, ranges) {
        negated
    } else {
        push_ranges_body(&mut body, &[(0, 0x10FFFF)]);
        !negated
    };
    if in_class && !negated {
        out.push_str(&body);
        return;
    }
    out.push_str(if negated { "[^" } else { "[" });
    out.push_str(&body);
    out.push(']');
}

/// Read the `{name}` of a `\p{…}` / `\P{…}` escape, the `\` and `p`/`P`
/// consumed. `None` if no `{` follows; otherwise the name and whether its
/// closing `}` was found.
fn read_property_name(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
) -> Option<(String, bool)> {
    chars.next_if_eq(&'{')?;
    let mut name = String::new();
    for c in chars.by_ref() {
        if c == '}' {
            return Some((name, true));
        }
        name.push(c);
    }
    Some((name, false))
}

/// Copy a `\p{name}` / `\P{name}` escape as written.
fn push_verbatim_property(out: &mut String, marker: char, name: &str, closed: bool) {
    out.push('\\');
    out.push(marker);
    out.push('{');
    out.push_str(name);
    if closed {
        out.push('}');
    }
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

    /// Convert an XSD pattern under both versions and compile it with the
    /// `regex` crate, as the pattern facets of the build without `xsd11` do.
    fn both_versions(pattern: &str) -> [Regex; 2] {
        [ConvertOptions::xsd_v1_0(), ConvertOptions::xsd()].map(|options| {
            let converted = try_convert_xml_pattern(pattern, options)
                .unwrap_or_else(|e| panic!("{pattern}: {e}"));
            Regex::new(&converted).unwrap_or_else(|e| panic!("{pattern}: {e}"))
        })
    }

    #[test]
    fn test_block_escape() {
        for regex in both_versions(r"\p{IsBasicLatin}+") {
            assert!(regex.is_match("abc ~\u{0}\u{7F}"));
            assert!(!regex.is_match("abcé"));
            assert!(!regex.is_match(""));
        }
        // FatturaPA's string types.
        for regex in both_versions(r"[\p{IsBasicLatin}\p{IsLatin-1Supplement}]{1,10}") {
            assert!(regex.is_match("Società"));
            assert!(!regex.is_match("Società€"));
            assert!(!regex.is_match("01234567890"));
        }
        // Names are taken as written: no case folding, hyphen kept.
        for regex in both_versions(r"\p{IsLatinExtended-A}") {
            assert!(regex.is_match("\u{100}") && regex.is_match("\u{17F}"));
            assert!(!regex.is_match("\u{FF}") && !regex.is_match("\u{180}"));
        }
        // The Unicode 3.1 names superseded since.
        for regex in both_versions(r"\p{IsGreek}\p{IsCombiningMarksforSymbols}") {
            assert!(regex.is_match("α\u{20D0}"));
            assert!(!regex.is_match("a\u{20D0}"));
        }
        // A block, not the script of the same name, which the `regex` crate
        // would take `IsGreek` for: Coptic letters are in the Greek block,
        // Greek Extended is not.
        for regex in both_versions(r"\p{IsGreek}") {
            assert!(regex.is_match("\u{3E2}"));
            assert!(!regex.is_match("\u{1F00}"));
        }
    }

    #[test]
    fn test_negated_block_escape() {
        for regex in both_versions(r"\P{IsBasicLatin}+") {
            assert!(regex.is_match("éß€\u{10000}"));
            assert!(!regex.is_match("éa"));
        }
    }

    #[test]
    fn test_block_escape_in_class() {
        for regex in both_versions(r"[\p{IsGreek}0-9]+") {
            assert!(regex.is_match("αβ12"));
            assert!(!regex.is_match("αb"));
        }
        // A negated block inside a class, and a negative group of a block.
        for regex in both_versions(r"[\P{IsBasicLatin}x]+") {
            assert!(regex.is_match("éxß"));
            assert!(!regex.is_match("éy"));
        }
        for regex in both_versions(r"[^\p{IsBasicLatin}]+") {
            assert!(regex.is_match("éß"));
            assert!(!regex.is_match("éa"));
        }
        for regex in both_versions(r"[^\P{IsBasicLatin}]+") {
            assert!(regex.is_match("ab"));
            assert!(!regex.is_match("aé"));
        }
    }

    #[test]
    fn test_block_escape_with_subtraction() {
        for regex in both_versions(r"[\p{IsBasicLatin}-[a-z]]+") {
            assert!(regex.is_match("ABC 123"));
            assert!(!regex.is_match("ABc"));
            assert!(!regex.is_match("ABÉ"));
        }
        for regex in both_versions(r"[\P{IsBasicLatin}-[é]]+") {
            assert!(regex.is_match("àü"));
            assert!(!regex.is_match("àé"));
            assert!(!regex.is_match("àa"));
        }
        for regex in both_versions(r"[a-z-[\p{IsBasicLatin}]]") {
            assert!(!regex.is_match("a"));
        }
    }

    /// `[G-[C]]` is a subtraction, which the `regex` crate reads as a union
    /// unless written `[G--[C]]`.
    #[test]
    fn test_class_subtraction() {
        for regex in both_versions(r"[a-z-[aeiou]]+") {
            assert!(regex.is_match("xyz"));
            assert!(!regex.is_match("xa"));
            assert!(!regex.is_match("x-"));
        }
        // From a negative group: (not a-z) minus the digits.
        for regex in both_versions(r"[^a-z-[0-9]]+") {
            assert!(regex.is_match("AB_"));
            assert!(!regex.is_match("Ab"));
            assert!(!regex.is_match("A1"));
        }
        // Nested: a-z minus (b-y minus c).
        for regex in both_versions(r"[a-z-[b-y-[c]]]+") {
            assert!(regex.is_match("acz"));
            assert!(!regex.is_match("ab"));
        }
        // A literal hyphen before the operator: a and - minus b.
        for regex in both_versions(r"[a--[b]]+") {
            assert!(regex.is_match("a-a"));
            assert!(!regex.is_match("ab"));
        }
        // Hyphens that are not the operator are untouched.
        for regex in both_versions(r"[-a-c\-]+") {
            assert!(regex.is_match("-abc-"));
            assert!(!regex.is_match("d"));
        }
    }

    /// A class the translation would turn into a valid `regex` class, but
    /// which no XSD grammar derives, is an error: a character group has at
    /// least one member, so neither an empty class nor a subtraction with
    /// nothing before its `-` is a class. (`regex` reads `[]a]` as `]` and
    /// `a`, and `[-[a]]` as a union.)
    #[test]
    fn test_malformed_classes_are_errors() {
        let empty = "empty character class";
        let subtraction = "a character class subtraction needs a character group before its '-'";
        for options in [ConvertOptions::xsd_v1_0(), ConvertOptions::xsd()] {
            for (pattern, error) in [
                // W3C msData/regex RegexTest_448..451, expected invalid.
                (r"[ab\-\[cd-[-[]]]]", subtraction),
                (r"[-[a]]", subtraction),
                (r"[^-[a]]", subtraction),
                ("[]", empty),
                ("[^]", empty),
                ("[a-[]]", empty),
            ] {
                assert_eq!(
                    try_convert_xml_pattern(pattern, options),
                    Err(error.to_string()),
                    "{pattern}"
                );
            }
        }
        // Leading and trailing hyphens are members.
        for regex in both_versions(r"[-a]+[a-]+[^-]") {
            assert!(regex.is_match("-a-ab"));
            assert!(!regex.is_match("-a-a-"));
        }
    }

    /// The surrogate blocks hold no character a string can have.
    #[test]
    fn test_surrogate_block_escape() {
        for regex in both_versions(r"a\p{IsHighSurrogates}?") {
            assert!(regex.is_match("a"));
            assert!(!regex.is_match("a\u{D7FF}"));
        }
        for regex in both_versions(r"[a\p{IsLowSurrogates}]") {
            assert!(regex.is_match("a") && !regex.is_match("b"));
        }
        for regex in both_versions(r"\P{IsHighPrivateUseSurrogates}") {
            assert!(regex.is_match("a") && regex.is_match("\u{10FFFF}"));
        }
        for regex in both_versions(r"[^\P{IsLowSurrogates}]?") {
            assert!(regex.is_match("") && !regex.is_match("a"));
        }
    }

    /// `IsPrivateUse` is the BMP area under XSD 1.0 and adds the planes 15 and
    /// 16 areas under XSD 1.1, as in the `xsd11` build.
    #[test]
    fn test_private_use_block() {
        let [v10, v11] = both_versions(r"\p{IsPrivateUse}");
        for regex in [&v10, &v11] {
            assert!(regex.is_match("\u{E000}") && regex.is_match("\u{F8FF}"));
            assert!(!regex.is_match("a"));
        }
        assert!(!v10.is_match("\u{F0000}") && !v10.is_match("\u{10FFFD}"));
        assert!(v11.is_match("\u{F0000}") && v11.is_match("\u{10FFFD}"));
    }

    /// Under XSD 1.0 an unrecognized block name is rejected, never matched
    /// silently: Appendix F lists the recognized names.
    #[test]
    fn test_unknown_block_name_is_an_error_in_xsd10() {
        let options = ConvertOptions::xsd_v1_0();
        for (pattern, escape, name) in [
            (r"\p{IsNoSuchBlock}", r"\p{IsNoSuchBlock}", "NoSuchBlock"),
            (r"a[\P{Isbasiclatin}]", r"\P{Isbasiclatin}", "basiclatin"),
            (
                r"\p{IsBasicLatin}\p{IsLatin1Supplement}",
                r"\p{IsLatin1Supplement}",
                "Latin1Supplement",
            ),
        ] {
            let error = try_convert_xml_pattern(pattern, options).unwrap_err();
            assert_eq!(
                error,
                format!("{escape}: \"{name}\" is not a recognized Unicode block name")
            );
            // Passed through by the infallible form, so the engine refuses it too.
            assert!(
                Regex::new(&convert_xml_pattern(pattern, options)).is_err(),
                "{pattern}"
            );
        }
        // A property of the `regex` crate, which reads `\p{IsAlphabetic}` as
        // `\p{Alphabetic}`, but no block.
        assert_eq!(
            try_convert_xml_pattern(r"\p{IsAlphabetic}", options).unwrap_err(),
            r#"\p{IsAlphabetic}: "Alphabetic" is not a recognized Unicode block name"#
        );
    }

    /// XSD 1.1 §G.4.2.4: an unrecognized IsBlock name denotes every
    /// character, for `\p` and `\P` alike, as in the `xsd11` build; a name
    /// that is no IsBlock name (`'Is' [a-zA-Z0-9-]+`) stays an error.
    #[test]
    fn test_unknown_block_name_is_every_character_in_xsd11() {
        let options = ConvertOptions::xsd();
        let compile = |pattern: &str| {
            let converted = try_convert_xml_pattern(pattern, options)
                .unwrap_or_else(|e| panic!("{pattern}: {e}"));
            Regex::new(&converted).unwrap_or_else(|e| panic!("{pattern}: {e}"))
        };
        for pattern in [
            r"\p{IsNoSuchBlock}",
            r"\P{IsNoSuchBlock}",
            r"[\p{IsNo-Such-9}]",
            r"[\P{Isbasiclatin}]",
            r"\p{IsAlphabetic}",
        ] {
            let regex = compile(pattern);
            for input in ["a", "\u{10FFFF}", "\n", "\u{D7FF}", "\u{E000}"] {
                assert!(regex.is_match(input), "{pattern} on {input:?}");
            }
            assert!(!regex.is_match("ab"), "{pattern}");
        }
        // The complement of every character is no character.
        let regex = compile(r"[^\P{IsNoSuchBlock}]?");
        assert!(regex.is_match("") && !regex.is_match("a"));
        // Known and unknown blocks together.
        let regex = compile(r"\p{IsFooBar}+[\P{IsBazQux}\p{IsBasicLatin}]\p{IsGreek}");
        assert!(regex.is_match("xyzα") && !regex.is_match("xyzq"));
        for (pattern, escape, name) in [
            (r"\p{IsNo_Such}", r"\p{IsNo_Such}", "No_Such"),
            (r"[\P{IsNo Such}]", r"\P{IsNo Such}", "No Such"),
        ] {
            assert_eq!(
                try_convert_xml_pattern(pattern, options).unwrap_err(),
                format!(
                    "{escape}: \"{name}\" is not a block name \
                     ('Is' followed by ASCII letters, digits and hyphens)"
                )
            );
        }
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

    /// The rewrite for `regexml`, which knows the blocks, leaves them alone.
    #[test]
    fn test_rewrite_xsd10_passes_block_escapes() {
        let result = rewrite_xsd10_category_escapes(r"\p{IsBasicLatin}+");
        assert_eq!(result, r"\p{IsBasicLatin}+");
        let result = rewrite_xsd10_category_escapes(r"[\P{IsGreek}a-z-[aeiou]]");
        assert_eq!(result, r"[\P{IsGreek}a-z-[aeiou]]");
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

    #[test]
    fn test_validate_xsd10_character_class_hyphen_rules() {
        for valid in [
            r"[a-d]",
            r"[-a]+",
            r"[-]",
            r"[a-]",
            r"[a-\}-]+",
            r"[a-z--[b-z]]",
            r"[a-b-[0-9]]+",
        ] {
            assert!(
                validate_xml_pattern_syntax(valid).is_ok(),
                "expected valid XSD 1.0 regex: {valid}",
            );
        }

        // Invalid forms drawn from W3C msData reF20-23, reG26-33, reH19-21 and
        // saxonData/Simple/simple045 — each is listed as XSD-1.0-invalid in the
        // suite manifest, regardless of whether XSD 1.1 accepts the same form.
        for invalid in [
            r"[^a-d-b-c]",
            r"[a-c-1-4x-z-7-9]*",
            r"[a-a-x-x]+",
            r"[a-z-+]*",
            r"[a--b]",
            r"[--z]",
        ] {
            assert!(
                validate_xml_pattern_syntax(invalid).is_err(),
                "expected invalid XSD 1.0 regex: {invalid}",
            );
        }
    }

    #[test]
    fn lenient_ms_strips_inline_comments() {
        assert_eq!(lenient_ms_preprocess("a(?#note)b"), "ab");
        assert_eq!(lenient_ms_preprocess("(?#start)abc(?#end)"), "abc");
    }

    #[test]
    fn lenient_ms_passthrough_when_clean() {
        // No `(?#` — should return Borrowed without copying.
        let p = "^abc[0-9]+$";
        let result = lenient_ms_preprocess(p);
        assert!(matches!(result, std::borrow::Cow::Borrowed(_)));
        assert_eq!(result, p);
    }

    #[test]
    fn lenient_ms_keeps_anchors_for_engine() {
        // Anchors are handled natively by both backends (Rust `regex`, and
        // regexml's XPath dialect matched against the whole value);
        // preprocess no longer strips them.
        assert_eq!(lenient_ms_preprocess("^abc$"), "^abc$");
        assert_eq!(lenient_ms_preprocess("[^abc]"), "[^abc]");
    }
}
