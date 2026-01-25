//! XPath 2.0 URI functions.
//!
//! This module implements:
//! - fn:resolve-uri($relative, $base?) - resolve a relative URI against a base URI
//! - fn:static-base-uri() - return the static base URI from context

use crate::xpath::context::DynamicContext;
use crate::xpath::error::XPathError;
use crate::xpath::DomNavigator;

use super::{atomize_to_string_opt, XPathValue};
use crate::xpath::iterator::XmlItem;
use crate::types::value::{XmlValue, XmlValueKind, XmlAtomicValue};
use crate::types::XmlTypeCode;

/// fn:resolve-uri($relative as xs:string?, $base as xs:string?) as xs:anyURI?
///
/// Resolves a relative URI against a base URI, returning the resolved URI.
///
/// Behavior:
/// - If $relative is empty sequence, returns empty sequence
/// - If $relative is empty string, resolves it against base (returns base URI)
/// - 1-arg form: uses static base URI from context (FONS0005 if not defined)
/// - 2-arg form with empty base: requires base to be absolute, FORG0009 if not
/// - FORG0009 if the URI resolution fails
pub fn resolve_uri<N: DomNavigator>(
    context: &mut DynamicContext<'_, N>,
    mut args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if args.is_empty() || args.len() > 2 {
        return Err(XPathError::wrong_number_of_arguments("resolve-uri", 2, args.len()));
    }

    let is_one_arg_form = args.len() == 1;

    // Get the relative URI (first argument)
    let relative_arg = args.remove(0);
    let relative = atomize_to_string_opt(relative_arg)?;

    // If relative is empty sequence, return empty sequence
    let relative = match relative {
        None => return Ok(XPathValue::Empty),
        Some(s) => s, // Empty string is valid and resolves to base
    };

    // Get base URI (second argument or context base_uri)
    let base = if !args.is_empty() {
        // 2-arg form
        let base_arg = args.remove(0);
        atomize_to_string_opt(base_arg)?
    } else {
        // 1-arg form: use static base URI
        context.base_uri.clone()
    };

    // Validate base URI
    let base = match base {
        None if is_one_arg_form => {
            // 1-arg form requires base URI to be defined
            return Err(XPathError::base_uri_not_defined());
        }
        None => {
            // 2-arg form with empty sequence base: if relative is absolute, return it
            if is_absolute_uri(&relative) {
                return Ok(make_any_uri(&relative));
            }
            // Otherwise, error - can't resolve relative against empty
            return Err(XPathError::uri_resolution_error(&relative));
        }
        Some(b) if b.is_empty() => {
            // Empty string base
            if is_one_arg_form {
                return Err(XPathError::base_uri_not_defined());
            }
            // 2-arg with empty string: if relative is absolute, return it
            if is_absolute_uri(&relative) {
                return Ok(make_any_uri(&relative));
            }
            // Can't resolve relative against empty string
            return Err(XPathError::uri_resolution_error(&relative));
        }
        Some(b) => b,
    };

    // Resolve the URI
    let resolved = resolve_uri_reference(&relative, &base)
        .map_err(|_| XPathError::uri_resolution_error(&relative))?;

    Ok(make_any_uri(&resolved))
}

/// fn:static-base-uri() as xs:anyURI?
///
/// Returns the base URI from the static context.
/// Returns empty sequence if no base URI is defined.
pub fn static_base_uri<N: DomNavigator>(
    context: &mut DynamicContext<'_, N>,
    args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if !args.is_empty() {
        return Err(XPathError::wrong_number_of_arguments("static-base-uri", 0, args.len()));
    }

    match &context.base_uri {
        Some(uri) if !uri.is_empty() => Ok(make_any_uri(uri)),
        _ => Ok(XPathValue::Empty),
    }
}

/// Create an xs:anyURI value from a string.
fn make_any_uri<N: DomNavigator>(uri: &str) -> XPathValue<N> {
    let value = XmlValue::new(
        XmlTypeCode::AnyUri,
        XmlValueKind::Atomic(XmlAtomicValue::AnyUri(uri.to_string())),
    );
    XPathValue::Item(XmlItem::Atomic(value))
}

/// Resolve a relative URI reference against a base URI.
///
/// This implements a simplified RFC 3986 URI resolution algorithm.
fn resolve_uri_reference(relative: &str, base: &str) -> Result<String, ()> {
    // If relative is already absolute (has scheme), return as-is
    if is_absolute_uri(relative) {
        return Ok(relative.to_string());
    }

    // Parse base URI
    let (base_scheme, base_authority, base_path, _base_query) = parse_uri_components(base)?;

    // If relative starts with //, it's a network-path reference
    if relative.starts_with("//") {
        return Ok(format!("{}{}", base_scheme.unwrap_or_default(), relative));
    }

    // If relative starts with /, it's an absolute-path reference
    if relative.starts_with('/') {
        let resolved_path = remove_dot_segments(relative);
        return Ok(format!(
            "{}{}{}",
            base_scheme.unwrap_or_default(),
            base_authority.map(|a| format!("//{}", a)).unwrap_or_default(),
            resolved_path
        ));
    }

    // If relative is empty, return base with optional query/fragment from relative
    if relative.is_empty() {
        return Ok(base.to_string());
    }

    // If relative starts with ?, it's a query reference
    if relative.starts_with('?') {
        let (base_without_query, _) = base.split_once('?').unwrap_or((base, ""));
        let (base_without_fragment, _) = base_without_query.split_once('#').unwrap_or((base_without_query, ""));
        return Ok(format!("{}{}", base_without_fragment, relative));
    }

    // If relative starts with #, it's a fragment reference
    if relative.starts_with('#') {
        let (base_without_fragment, _) = base.split_once('#').unwrap_or((base, ""));
        return Ok(format!("{}{}", base_without_fragment, relative));
    }

    // Otherwise, merge paths
    let merged_path = merge_paths(
        base_authority.is_some(),
        base_path.unwrap_or(""),
        relative,
    );
    let resolved_path = remove_dot_segments(&merged_path);

    Ok(format!(
        "{}{}{}",
        base_scheme.unwrap_or_default(),
        base_authority.map(|a| format!("//{}", a)).unwrap_or_default(),
        resolved_path
    ))
}

/// Check if a URI is absolute (has a scheme).
fn is_absolute_uri(uri: &str) -> bool {
    // A scheme is a letter followed by letters, digits, +, -, or .
    // followed by :
    if let Some(colon_pos) = uri.find(':') {
        if colon_pos > 0 {
            let scheme = &uri[..colon_pos];
            let mut chars = scheme.chars();
            if let Some(first) = chars.next() {
                if first.is_ascii_alphabetic() {
                    return chars.all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.');
                }
            }
        }
    }
    false
}

/// URI components tuple: (scheme, authority, path, query)
type UriComponents<'a> = (Option<String>, Option<&'a str>, Option<&'a str>, Option<&'a str>);

/// Parse URI into components (scheme, authority, path, query).
fn parse_uri_components(uri: &str) -> Result<UriComponents<'_>, ()> {
    let mut rest = uri;
    let mut scheme = None;
    let mut authority = None;

    // Extract scheme
    if let Some(colon_pos) = rest.find(':') {
        let potential_scheme = &rest[..colon_pos];
        if is_valid_scheme(potential_scheme) {
            scheme = Some(format!("{}:", potential_scheme));
            rest = &rest[colon_pos + 1..];
        }
    }

    // Extract authority
    if rest.starts_with("//") {
        rest = &rest[2..];
        let auth_end = rest.find('/').or_else(|| rest.find('?')).or_else(|| rest.find('#')).unwrap_or(rest.len());
        authority = Some(&rest[..auth_end]);
        rest = &rest[auth_end..];
    }

    // Extract query and fragment
    let (path_and_query, _fragment) = rest.split_once('#').unwrap_or((rest, ""));
    let (path, query) = path_and_query.split_once('?').map(|(p, q)| (Some(p), Some(q))).unwrap_or((Some(path_and_query), None));

    Ok((scheme, authority, path, query))
}

/// Check if a string is a valid URI scheme.
fn is_valid_scheme(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let mut chars = s.chars();
    if let Some(first) = chars.next() {
        if !first.is_ascii_alphabetic() {
            return false;
        }
        chars.all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.')
    } else {
        false
    }
}

/// Merge paths according to RFC 3986.
fn merge_paths(has_authority: bool, base_path: &str, relative: &str) -> String {
    if has_authority && base_path.is_empty() {
        format!("/{}", relative)
    } else {
        // Remove everything after the last / in base_path
        let last_slash = base_path.rfind('/').map(|i| i + 1).unwrap_or(0);
        format!("{}{}", &base_path[..last_slash], relative)
    }
}

/// Remove dot segments from a path (RFC 3986 section 5.2.4).
fn remove_dot_segments(path: &str) -> String {
    let mut input = path.to_string();
    let mut output = Vec::new();

    while !input.is_empty() {
        // A: If the input buffer begins with a prefix of "../" or "./"
        if input.starts_with("../") {
            input = input[3..].to_string();
        } else if input.starts_with("./") {
            input = input[2..].to_string();
        }
        // B: If the input buffer begins with a prefix of "/./" or "/."
        else if input.starts_with("/./") {
            input = format!("/{}", &input[3..]);
        } else if input == "/." {
            input = "/".to_string();
        }
        // C: If the input buffer begins with a prefix of "/../" or "/.."
        else if input.starts_with("/../") {
            input = format!("/{}", &input[4..]);
            output.pop();
        } else if input == "/.." {
            input = "/".to_string();
            output.pop();
        }
        // D: if the input buffer consists only of "." or ".."
        else if input == "." || input == ".." {
            input.clear();
        }
        // E: move the first path segment (including initial "/" if any) to output
        else {
            let start = if input.starts_with('/') { 1 } else { 0 };
            let end = input[start..].find('/').map(|i| i + start).unwrap_or(input.len());
            output.push(input[..end].to_string());
            input = input[end..].to_string();
        }
    }

    output.join("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::namespace::table::NameTable;
    use crate::xpath::context::XPathContext;
    use crate::xpath::RoXmlNavigator;

    fn create_context<'a>(names: &'a NameTable, base_uri: Option<&str>) -> DynamicContext<'a, RoXmlNavigator<'a>> {
        let mut static_ctx = XPathContext::new(names);
        if let Some(uri) = base_uri {
            static_ctx = static_ctx.with_base_uri(uri);
        }
        let static_ctx = Box::leak(Box::new(static_ctx));
        DynamicContext::new(static_ctx, 0)
    }

    #[test]
    fn test_static_base_uri_defined() {
        let names = NameTable::new();
        let mut ctx = create_context(&names, Some("http://example.com/base/"));

        let result = static_base_uri(&mut ctx, vec![]).unwrap();
        match result {
            XPathValue::Item(XmlItem::Atomic(value)) => {
                assert_eq!(value.to_string_value(), "http://example.com/base/");
            }
            _ => panic!("Expected anyURI"),
        }
    }

    #[test]
    fn test_static_base_uri_empty() {
        let names = NameTable::new();
        let mut ctx = create_context(&names, None);

        let result = static_base_uri(&mut ctx, vec![]).unwrap();
        assert!(matches!(result, XPathValue::Empty));
    }

    #[test]
    fn test_resolve_uri_absolute() {
        let names = NameTable::new();
        let mut ctx = create_context(&names, Some("http://example.com/base/"));

        let result = resolve_uri(
            &mut ctx,
            vec![
                XPathValue::string("http://other.com/path"),
                XPathValue::string("http://example.com/ignored"),
            ],
        ).unwrap();

        match result {
            XPathValue::Item(XmlItem::Atomic(value)) => {
                assert_eq!(value.to_string_value(), "http://other.com/path");
            }
            _ => panic!("Expected anyURI"),
        }
    }

    #[test]
    fn test_resolve_uri_relative() {
        let names = NameTable::new();
        let mut ctx = create_context(&names, Some("http://example.com/base/"));

        let result = resolve_uri(
            &mut ctx,
            vec![
                XPathValue::string("path/file.xml"),
                XPathValue::string("http://example.com/base/"),
            ],
        ).unwrap();

        match result {
            XPathValue::Item(XmlItem::Atomic(value)) => {
                assert_eq!(value.to_string_value(), "http://example.com/base/path/file.xml");
            }
            _ => panic!("Expected anyURI"),
        }
    }

    #[test]
    fn test_resolve_uri_empty_relative() {
        let names = NameTable::new();
        let mut ctx = create_context(&names, Some("http://example.com/base/"));

        let result = resolve_uri(
            &mut ctx,
            vec![XPathValue::Empty],
        ).unwrap();

        assert!(matches!(result, XPathValue::Empty));
    }

    #[test]
    fn test_resolve_uri_dotdot() {
        let names = NameTable::new();
        let mut ctx = create_context(&names, None);

        let result = resolve_uri(
            &mut ctx,
            vec![
                XPathValue::string("../other/file.xml"),
                XPathValue::string("http://example.com/base/subdir/"),
            ],
        ).unwrap();

        match result {
            XPathValue::Item(XmlItem::Atomic(value)) => {
                assert_eq!(value.to_string_value(), "http://example.com/base/other/file.xml");
            }
            _ => panic!("Expected anyURI"),
        }
    }

    #[test]
    fn test_resolve_uri_no_base() {
        let names = NameTable::new();
        let mut ctx = create_context(&names, None);

        // 1-arg form with no base URI should fail
        let result = resolve_uri(
            &mut ctx,
            vec![XPathValue::string("path/file.xml")],
        );

        assert!(result.is_err());
        if let Err(XPathError::FONS0005) = result {
            // Expected
        } else {
            panic!("Expected FONS0005 error");
        }
    }

    #[test]
    fn test_is_absolute_uri() {
        assert!(is_absolute_uri("http://example.com"));
        assert!(is_absolute_uri("https://example.com/path"));
        assert!(is_absolute_uri("file:///path/to/file"));
        assert!(is_absolute_uri("urn:isbn:0451450523"));
        assert!(!is_absolute_uri("/path/to/file"));
        assert!(!is_absolute_uri("path/to/file"));
        assert!(!is_absolute_uri("../relative"));
    }

    #[test]
    fn test_remove_dot_segments() {
        assert_eq!(remove_dot_segments("/a/b/c/./../../g"), "/a/g");
        assert_eq!(remove_dot_segments("mid/content=5/../6"), "mid/6");
        assert_eq!(remove_dot_segments("/../../../g"), "/g");
        assert_eq!(remove_dot_segments("./g"), "g");
        assert_eq!(remove_dot_segments("../../../g"), "g");
    }
}
