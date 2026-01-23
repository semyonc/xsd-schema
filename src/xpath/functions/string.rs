//! String functions for XPath 2.0.
//!
//! This module implements XPath 2.0 string functions, delegating to
//! the implementations in `xpath::string_ops`.

use crate::types::value::XmlValue;
use crate::xpath::error::XPathError;
use crate::xpath::iterator::XmlItem;
use crate::xpath::string_ops;
use crate::xpath::DomNavigator;

use crate::xpath::context::DynamicContext;
use super::{atomize_to_string, atomize_to_string_opt, atomize_to_double, XPathValue};

// ============================================================================
// String Functions
// ============================================================================

/// fn:concat($arg1 as xs:anyAtomicType?, $arg2 as xs:anyAtomicType?, ...) as xs:string
///
/// Concatenates two or more strings.
pub fn concat<N: DomNavigator>(
    _context: &mut DynamicContext<'_, N>,
    args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if args.len() < 2 {
        return Err(XPathError::wrong_number_of_arguments("concat", 2, args.len()));
    }

    let strings: Result<Vec<String>, _> = args.into_iter()
        .map(atomize_to_string)
        .collect();
    let strings = strings?;
    let refs: Vec<&str> = strings.iter().map(|s| s.as_str()).collect();
    let result = string_ops::concat(&refs);
    Ok(XPathValue::string(result))
}

/// fn:string-join($arg1 as xs:string*, $arg2 as xs:string) as xs:string
///
/// Joins strings with a separator.
pub fn string_join<N: DomNavigator>(
    _context: &mut DynamicContext<'_, N>,
    mut args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if args.len() != 2 {
        return Err(XPathError::wrong_number_of_arguments("string-join", 2, args.len()));
    }

    let separator = atomize_to_string(args.pop().unwrap())?;
    let sequence = args.pop().unwrap();

    // Collect all string values from the sequence
    let strings: Result<Vec<String>, _> = sequence.into_vec()
        .into_iter()
        .map(|item| match item {
            XmlItem::Atomic(v) => Ok(v.to_string_value()),
            XmlItem::Node(n) => Ok(n.value()),
        })
        .collect();
    let strings = strings?;
    let refs: Vec<&str> = strings.iter().map(|s| s.as_str()).collect();
    let result = string_ops::string_join(&refs, &separator);
    Ok(XPathValue::string(result))
}

/// fn:substring($sourceString as xs:string?, $start as xs:double) as xs:string
/// fn:substring($sourceString as xs:string?, $start as xs:double, $length as xs:double) as xs:string
///
/// Returns a portion of the source string.
pub fn substring<N: DomNavigator>(
    _context: &mut DynamicContext<'_, N>,
    mut args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if args.len() < 2 || args.len() > 3 {
        return Err(XPathError::wrong_number_of_arguments("substring", 2, args.len()));
    }

    let source = atomize_to_string(args.remove(0))?;
    let start = atomize_to_double(args.remove(0))?;
    let length = if !args.is_empty() {
        Some(atomize_to_double(args.remove(0))?)
    } else {
        None
    };

    let result = string_ops::substring(&source, start, length);
    Ok(XPathValue::string(result))
}

/// fn:string-length($arg as xs:string?) as xs:integer
/// fn:string-length() as xs:integer (uses context item)
///
/// Returns the length of the string in characters.
pub fn string_length<N: DomNavigator>(
    context: &mut DynamicContext<'_, N>,
    args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if args.len() > 1 {
        return Err(XPathError::wrong_number_of_arguments("string-length", 1, args.len()));
    }

    let source = if args.is_empty() {
        // Use context item
        match &context.context_item {
            Some(item) => match item {
                XmlItem::Atomic(v) => v.to_string_value(),
                XmlItem::Node(n) => n.value(),
            },
            None => return Err(XPathError::XPDY0002 {
                message: "Context item is absent".to_string(),
            }),
        }
    } else {
        atomize_to_string(args.into_iter().next().unwrap())?
    };

    let len = string_ops::string_length(&source);
    Ok(XPathValue::integer(len as i64))
}

/// fn:normalize-space($arg as xs:string?) as xs:string
/// fn:normalize-space() as xs:string (uses context item)
///
/// Normalizes whitespace in a string.
pub fn normalize_space<N: DomNavigator>(
    context: &mut DynamicContext<'_, N>,
    args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if args.len() > 1 {
        return Err(XPathError::wrong_number_of_arguments("normalize-space", 1, args.len()));
    }

    let source = if args.is_empty() {
        // Use context item
        match &context.context_item {
            Some(item) => match item {
                XmlItem::Atomic(v) => v.to_string_value(),
                XmlItem::Node(n) => n.value(),
            },
            None => return Err(XPathError::XPDY0002 {
                message: "Context item is absent".to_string(),
            }),
        }
    } else {
        atomize_to_string(args.into_iter().next().unwrap())?
    };

    let result = string_ops::normalize_space(&source);
    Ok(XPathValue::string(result))
}

/// fn:normalize-unicode($arg as xs:string?) as xs:string
/// fn:normalize-unicode($arg as xs:string?, $normalizationForm as xs:string) as xs:string
///
/// Normalizes a string using Unicode normalization.
pub fn normalize_unicode<N: DomNavigator>(
    _context: &mut DynamicContext<'_, N>,
    mut args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if args.is_empty() || args.len() > 2 {
        return Err(XPathError::wrong_number_of_arguments("normalize-unicode", 1, args.len()));
    }

    let source = atomize_to_string(args.remove(0))?;

    let form = if !args.is_empty() {
        let form_str = atomize_to_string(args.remove(0))?;
        let trimmed = form_str.trim();
        if trimmed.is_empty() {
            None
        } else {
            match string_ops::UnicodeNormalizationForm::parse(trimmed) {
                Some(f) => Some(f),
                None => return Err(XPathError::FOCH0003 {
                    normalization_form: form_str,
                }),
            }
        }
    } else {
        // Default is NFC
        Some(string_ops::UnicodeNormalizationForm::NFC)
    };

    #[cfg(feature = "unicode-normalization")]
    let result = string_ops::normalize_unicode(&source, form);

    #[cfg(not(feature = "unicode-normalization"))]
    let result = string_ops::normalize_unicode(&source, form)?;

    Ok(XPathValue::string(result))
}

/// fn:upper-case($arg as xs:string?) as xs:string
///
/// Converts a string to uppercase.
pub fn upper_case<N: DomNavigator>(
    _context: &mut DynamicContext<'_, N>,
    mut args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if args.len() != 1 {
        return Err(XPathError::wrong_number_of_arguments("upper-case", 1, args.len()));
    }

    let source = atomize_to_string(args.remove(0))?;
    let result = string_ops::upper_case(&source);
    Ok(XPathValue::string(result))
}

/// fn:lower-case($arg as xs:string?) as xs:string
///
/// Converts a string to lowercase.
pub fn lower_case<N: DomNavigator>(
    _context: &mut DynamicContext<'_, N>,
    mut args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if args.len() != 1 {
        return Err(XPathError::wrong_number_of_arguments("lower-case", 1, args.len()));
    }

    let source = atomize_to_string(args.remove(0))?;
    let result = string_ops::lower_case(&source);
    Ok(XPathValue::string(result))
}

/// fn:translate($arg as xs:string?, $mapString as xs:string, $transString as xs:string) as xs:string
///
/// Translates characters in a string.
pub fn translate<N: DomNavigator>(
    _context: &mut DynamicContext<'_, N>,
    mut args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if args.len() != 3 {
        return Err(XPathError::wrong_number_of_arguments("translate", 3, args.len()));
    }

    let source = atomize_to_string(args.remove(0))?;
    let map_string = atomize_to_string(args.remove(0))?;
    let trans_string = atomize_to_string(args.remove(0))?;

    let result = string_ops::translate(&source, &map_string, &trans_string);
    Ok(XPathValue::string(result))
}

/// fn:encode-for-uri($uri-part as xs:string?) as xs:string
///
/// Encodes a string for use in a URI.
pub fn encode_for_uri<N: DomNavigator>(
    _context: &mut DynamicContext<'_, N>,
    mut args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if args.len() != 1 {
        return Err(XPathError::wrong_number_of_arguments("encode-for-uri", 1, args.len()));
    }

    let source = atomize_to_string(args.remove(0))?;
    let result = string_ops::encode_for_uri(&source);
    Ok(XPathValue::string(result))
}

/// fn:iri-to-uri($iri as xs:string?) as xs:string
///
/// Converts an IRI to a URI.
pub fn iri_to_uri<N: DomNavigator>(
    _context: &mut DynamicContext<'_, N>,
    mut args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if args.len() != 1 {
        return Err(XPathError::wrong_number_of_arguments("iri-to-uri", 1, args.len()));
    }

    let source = atomize_to_string(args.remove(0))?;
    let result = string_ops::iri_to_uri(&source);
    Ok(XPathValue::string(result))
}

/// fn:escape-html-uri($uri as xs:string?) as xs:string
///
/// Escapes a URI for use in HTML.
pub fn escape_html_uri<N: DomNavigator>(
    _context: &mut DynamicContext<'_, N>,
    mut args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if args.len() != 1 {
        return Err(XPathError::wrong_number_of_arguments("escape-html-uri", 1, args.len()));
    }

    let source = atomize_to_string(args.remove(0))?;
    let result = string_ops::escape_html_uri(&source);
    Ok(XPathValue::string(result))
}

/// fn:contains($arg1 as xs:string?, $arg2 as xs:string?) as xs:boolean
/// fn:contains($arg1 as xs:string?, $arg2 as xs:string?, $collation as xs:string) as xs:boolean
///
/// Checks if a string contains a substring.
pub fn contains<N: DomNavigator>(
    _context: &mut DynamicContext<'_, N>,
    mut args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if args.len() < 2 || args.len() > 3 {
        return Err(XPathError::wrong_number_of_arguments("contains", 2, args.len()));
    }

    let source = atomize_to_string(args.remove(0))?;
    let substring = atomize_to_string(args.remove(0))?;
    // Collation argument is ignored for now (uses default Unicode codepoint collation)

    let result = string_ops::contains(&source, &substring);
    Ok(XPathValue::boolean(result))
}

/// fn:starts-with($arg1 as xs:string?, $arg2 as xs:string?) as xs:boolean
/// fn:starts-with($arg1 as xs:string?, $arg2 as xs:string?, $collation as xs:string) as xs:boolean
///
/// Checks if a string starts with a prefix.
pub fn starts_with<N: DomNavigator>(
    _context: &mut DynamicContext<'_, N>,
    mut args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if args.len() < 2 || args.len() > 3 {
        return Err(XPathError::wrong_number_of_arguments("starts-with", 2, args.len()));
    }

    let source = atomize_to_string(args.remove(0))?;
    let prefix = atomize_to_string(args.remove(0))?;
    // Collation argument is ignored for now

    let result = string_ops::starts_with(&source, &prefix);
    Ok(XPathValue::boolean(result))
}

/// fn:ends-with($arg1 as xs:string?, $arg2 as xs:string?) as xs:boolean
/// fn:ends-with($arg1 as xs:string?, $arg2 as xs:string?, $collation as xs:string) as xs:boolean
///
/// Checks if a string ends with a suffix.
pub fn ends_with<N: DomNavigator>(
    _context: &mut DynamicContext<'_, N>,
    mut args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if args.len() < 2 || args.len() > 3 {
        return Err(XPathError::wrong_number_of_arguments("ends-with", 2, args.len()));
    }

    let source = atomize_to_string(args.remove(0))?;
    let suffix = atomize_to_string(args.remove(0))?;
    // Collation argument is ignored for now

    let result = string_ops::ends_with(&source, &suffix);
    Ok(XPathValue::boolean(result))
}

/// fn:substring-before($arg1 as xs:string?, $arg2 as xs:string?) as xs:string
/// fn:substring-before($arg1 as xs:string?, $arg2 as xs:string?, $collation as xs:string) as xs:string
///
/// Returns the substring before the first occurrence of the pattern.
pub fn substring_before<N: DomNavigator>(
    _context: &mut DynamicContext<'_, N>,
    mut args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if args.len() < 2 || args.len() > 3 {
        return Err(XPathError::wrong_number_of_arguments("substring-before", 2, args.len()));
    }

    let source = atomize_to_string(args.remove(0))?;
    let pattern = atomize_to_string(args.remove(0))?;
    // Collation argument is ignored for now

    let result = string_ops::substring_before(&source, &pattern);
    Ok(XPathValue::string(result))
}

/// fn:substring-after($arg1 as xs:string?, $arg2 as xs:string?) as xs:string
/// fn:substring-after($arg1 as xs:string?, $arg2 as xs:string?, $collation as xs:string) as xs:string
///
/// Returns the substring after the first occurrence of the pattern.
pub fn substring_after<N: DomNavigator>(
    _context: &mut DynamicContext<'_, N>,
    mut args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if args.len() < 2 || args.len() > 3 {
        return Err(XPathError::wrong_number_of_arguments("substring-after", 2, args.len()));
    }

    let source = atomize_to_string(args.remove(0))?;
    let pattern = atomize_to_string(args.remove(0))?;
    // Collation argument is ignored for now

    let result = string_ops::substring_after(&source, &pattern);
    Ok(XPathValue::string(result))
}

/// fn:string-to-codepoints($arg as xs:string?) as xs:integer*
///
/// Converts a string to a sequence of codepoints.
pub fn string_to_codepoints<N: DomNavigator>(
    _context: &mut DynamicContext<'_, N>,
    mut args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if args.len() != 1 {
        return Err(XPathError::wrong_number_of_arguments("string-to-codepoints", 1, args.len()));
    }

    let source = atomize_to_string_opt(args.remove(0))?;

    match source {
        None => Ok(XPathValue::empty()),
        Some(ref s) if s.is_empty() => Ok(XPathValue::empty()),
        Some(s) => {
            let codepoints = string_ops::string_to_codepoints(&s);
            let items: Vec<XmlItem<N>> = codepoints
                .into_iter()
                .map(|cp| XmlItem::Atomic(XmlValue::integer(cp.into())))
                .collect();
            Ok(XPathValue::from_sequence(items))
        }
    }
}

/// fn:codepoints-to-string($arg as xs:integer*) as xs:string
///
/// Converts a sequence of codepoints to a string.
pub fn codepoints_to_string<N: DomNavigator>(
    _context: &mut DynamicContext<'_, N>,
    mut args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if args.len() != 1 {
        return Err(XPathError::wrong_number_of_arguments("codepoints-to-string", 1, args.len()));
    }

    let sequence = args.remove(0);
    let items = sequence.into_vec();

    if items.is_empty() {
        return Ok(XPathValue::string(""));
    }

    let mut codepoints = Vec::with_capacity(items.len());
    for item in items {
        match item {
            XmlItem::Atomic(v) => {
                if let Some(i) = v.as_integer() {
                    let cp: u32 = i.try_into().map_err(|_| XPathError::FOCH0001 {
                        codepoint: i.to_string(),
                    })?;
                    codepoints.push(cp);
                } else {
                    return Err(XPathError::XPTY0004 {
                        expected: "xs:integer".to_string(),
                        found: format!("{:?}", v.type_code),
                    });
                }
            }
            XmlItem::Node(_) => {
                return Err(XPathError::XPTY0004 {
                    expected: "xs:integer".to_string(),
                    found: "node()".to_string(),
                });
            }
        }
    }

    match string_ops::codepoints_to_string(&codepoints) {
        Some(s) => Ok(XPathValue::string(s)),
        None => Err(XPathError::FOCH0001 {
            codepoint: "invalid".to_string(),
        }),
    }
}

/// fn:compare($comparand1 as xs:string?, $comparand2 as xs:string?) as xs:integer?
/// fn:compare($comparand1 as xs:string?, $comparand2 as xs:string?, $collation as xs:string) as xs:integer?
///
/// Compares two strings.
pub fn compare<N: DomNavigator>(
    _context: &mut DynamicContext<'_, N>,
    mut args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if args.len() < 2 || args.len() > 3 {
        return Err(XPathError::wrong_number_of_arguments("compare", 2, args.len()));
    }

    let s1 = atomize_to_string_opt(args.remove(0))?;
    let s2 = atomize_to_string_opt(args.remove(0))?;
    // Collation argument is ignored for now

    match (s1, s2) {
        (Some(a), Some(b)) => {
            let result = string_ops::compare(&a, &b);
            Ok(XPathValue::integer(result as i64))
        }
        _ => Ok(XPathValue::empty()),
    }
}

/// fn:codepoint-equal($comparand1 as xs:string?, $comparand2 as xs:string?) as xs:boolean?
///
/// Compares two strings by codepoint.
pub fn codepoint_equal<N: DomNavigator>(
    _context: &mut DynamicContext<'_, N>,
    mut args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if args.len() != 2 {
        return Err(XPathError::wrong_number_of_arguments("codepoint-equal", 2, args.len()));
    }

    let s1 = atomize_to_string_opt(args.remove(0))?;
    let s2 = atomize_to_string_opt(args.remove(0))?;

    match (s1, s2) {
        (Some(a), Some(b)) => {
            let result = string_ops::codepoint_equal(&a, &b);
            Ok(XPathValue::boolean(result))
        }
        _ => Ok(XPathValue::empty()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::namespace::table::NameTable;
    use crate::xpath::context::XPathContext;
    use crate::xpath::RoXmlNavigator;

    fn make_context() -> (NameTable, DynamicContext<'static, RoXmlNavigator<'static>>) {
        let table = Box::leak(Box::new(NameTable::new()));
        let static_ctx = Box::leak(Box::new(XPathContext::new(table)));
        let dyn_ctx = DynamicContext::new(static_ctx, 0);
        (NameTable::new(), dyn_ctx)
    }

    #[test]
    fn test_concat() {
        let (_, mut ctx) = make_context();
        let args = vec![
            XPathValue::string("Hello"),
            XPathValue::string(", "),
            XPathValue::string("World!"),
        ];
        let result = concat(&mut ctx, args).unwrap();
        match result {
            XPathValue::Item(XmlItem::Atomic(v)) => {
                assert_eq!(v.as_string().unwrap(), "Hello, World!");
            }
            _ => panic!("Expected string"),
        }
    }

    #[test]
    fn test_string_join() {
        let (_, mut ctx) = make_context();
        let seq = XPathValue::from_sequence(vec![
            XmlItem::Atomic(XmlValue::string("a")),
            XmlItem::Atomic(XmlValue::string("b")),
            XmlItem::Atomic(XmlValue::string("c")),
        ]);
        let args = vec![seq, XPathValue::string("-")];
        let result = string_join(&mut ctx, args).unwrap();
        match result {
            XPathValue::Item(XmlItem::Atomic(v)) => {
                assert_eq!(v.as_string().unwrap(), "a-b-c");
            }
            _ => panic!("Expected string"),
        }
    }

    #[test]
    fn test_substring() {
        let (_, mut ctx) = make_context();
        let args = vec![
            XPathValue::string("hello"),
            XPathValue::double(2.0),
            XPathValue::double(3.0),
        ];
        let result = substring(&mut ctx, args).unwrap();
        match result {
            XPathValue::Item(XmlItem::Atomic(v)) => {
                assert_eq!(v.as_string().unwrap(), "ell");
            }
            _ => panic!("Expected string"),
        }
    }

    #[test]
    fn test_string_length() {
        let (_, mut ctx) = make_context();
        let args = vec![XPathValue::string("hello")];
        let result = string_length(&mut ctx, args).unwrap();
        match result {
            XPathValue::Item(XmlItem::Atomic(v)) => {
                assert_eq!(*v.as_integer().unwrap(), 5.into());
            }
            _ => panic!("Expected integer"),
        }
    }

    #[test]
    fn test_upper_lower_case() {
        let (_, mut ctx) = make_context();

        let args = vec![XPathValue::string("Hello")];
        let result = upper_case(&mut ctx, args).unwrap();
        match result {
            XPathValue::Item(XmlItem::Atomic(v)) => {
                assert_eq!(v.as_string().unwrap(), "HELLO");
            }
            _ => panic!("Expected string"),
        }

        let args = vec![XPathValue::string("Hello")];
        let result = lower_case(&mut ctx, args).unwrap();
        match result {
            XPathValue::Item(XmlItem::Atomic(v)) => {
                assert_eq!(v.as_string().unwrap(), "hello");
            }
            _ => panic!("Expected string"),
        }
    }

    #[test]
    fn test_contains() {
        let (_, mut ctx) = make_context();
        let args = vec![XPathValue::string("hello world"), XPathValue::string("world")];
        let result = contains(&mut ctx, args).unwrap();
        match result {
            XPathValue::Item(XmlItem::Atomic(v)) => {
                assert!(v.as_boolean().unwrap());
            }
            _ => panic!("Expected boolean"),
        }
    }

    #[test]
    fn test_starts_ends_with() {
        let (_, mut ctx) = make_context();

        let args = vec![XPathValue::string("hello"), XPathValue::string("he")];
        let result = starts_with(&mut ctx, args).unwrap();
        match result {
            XPathValue::Item(XmlItem::Atomic(v)) => {
                assert!(v.as_boolean().unwrap());
            }
            _ => panic!("Expected boolean"),
        }

        let args = vec![XPathValue::string("hello"), XPathValue::string("lo")];
        let result = ends_with(&mut ctx, args).unwrap();
        match result {
            XPathValue::Item(XmlItem::Atomic(v)) => {
                assert!(v.as_boolean().unwrap());
            }
            _ => panic!("Expected boolean"),
        }
    }

    #[test]
    fn test_encode_for_uri() {
        let (_, mut ctx) = make_context();
        let args = vec![XPathValue::string("hello world")];
        let result = encode_for_uri(&mut ctx, args).unwrap();
        match result {
            XPathValue::Item(XmlItem::Atomic(v)) => {
                assert_eq!(v.as_string().unwrap(), "hello%20world");
            }
            _ => panic!("Expected string"),
        }
    }

    #[test]
    fn test_string_to_codepoints() {
        let (_, mut ctx) = make_context();
        let args = vec![XPathValue::string("ABC")];
        let result = string_to_codepoints(&mut ctx, args).unwrap();
        let items = result.into_vec();
        assert_eq!(items.len(), 3);
        match &items[0] {
            XmlItem::Atomic(v) => assert_eq!(*v.as_integer().unwrap(), 65.into()),
            _ => panic!("Expected integer"),
        }
    }

    #[test]
    fn test_codepoints_to_string() {
        let (_, mut ctx) = make_context();
        let seq = XPathValue::from_sequence(vec![
            XmlItem::Atomic(XmlValue::integer(65.into())),
            XmlItem::Atomic(XmlValue::integer(66.into())),
            XmlItem::Atomic(XmlValue::integer(67.into())),
        ]);
        let args = vec![seq];
        let result = codepoints_to_string(&mut ctx, args).unwrap();
        match result {
            XPathValue::Item(XmlItem::Atomic(v)) => {
                assert_eq!(v.as_string().unwrap(), "ABC");
            }
            _ => panic!("Expected string"),
        }
    }

    #[test]
    fn test_compare() {
        let (_, mut ctx) = make_context();
        let args = vec![XPathValue::string("abc"), XPathValue::string("abd")];
        let result = compare(&mut ctx, args).unwrap();
        match result {
            XPathValue::Item(XmlItem::Atomic(v)) => {
                assert_eq!(*v.as_integer().unwrap(), (-1).into());
            }
            _ => panic!("Expected integer"),
        }
    }

    #[test]
    fn test_codepoint_equal() {
        let (_, mut ctx) = make_context();
        let args = vec![XPathValue::string("abc"), XPathValue::string("abc")];
        let result = codepoint_equal(&mut ctx, args).unwrap();
        match result {
            XPathValue::Item(XmlItem::Atomic(v)) => {
                assert!(v.as_boolean().unwrap());
            }
            _ => panic!("Expected boolean"),
        }

        let args = vec![XPathValue::string("abc"), XPathValue::string("ABC")];
        let result = codepoint_equal(&mut ctx, args).unwrap();
        match result {
            XPathValue::Item(XmlItem::Atomic(v)) => {
                assert!(!v.as_boolean().unwrap());
            }
            _ => panic!("Expected boolean"),
        }
    }
}
