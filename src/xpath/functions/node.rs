//! XPath 2.0 node functions.
//!
//! This module implements node functions from the XPath 2.0 specification:
//! - fn:name
//! - fn:local-name
//! - fn:namespace-uri
//! - fn:node-name
//! - fn:nilled
//! - fn:base-uri
//! - fn:document-uri
//! - fn:lang
//! - fn:root
//! - fn:id

use crate::ids::NameId;
use crate::namespace::qname::QualifiedName;
use crate::namespace::table::NameTable;
use crate::types::value::{XmlAtomicValue, XmlValue, XmlValueKind};
use crate::types::XmlTypeCode;
use crate::xpath::context::DynamicContext;
use crate::xpath::error::XPathError;
use crate::xpath::iterator::XmlItem;
use crate::xpath::{DomNavigator, DomNodeType};

use super::{atomize_to_string_opt, materialize, XPathValue};

// ============================================================================
// fn:name($arg as node()?) as xs:string
// ============================================================================

/// Implements fn:name - returns the qualified name of a node.
///
/// If no argument, uses context item.
/// Returns empty string for nodes without names.
pub fn name<N: DomNavigator>(
    context: &mut DynamicContext<'_, N>,
    args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if args.len() > 1 {
        return Err(XPathError::wrong_number_of_arguments("name", 1, args.len()));
    }

    let node = get_node_arg(context, args)?;

    match node {
        None => Ok(XPathValue::string("")),
        Some(nav) => Ok(XPathValue::string(nav.name().to_string())),
    }
}

// ============================================================================
// fn:local-name($arg as node()?) as xs:string
// ============================================================================

/// Implements fn:local-name - returns the local name of a node.
///
/// If no argument, uses context item.
/// Returns empty string for nodes without names.
pub fn local_name<N: DomNavigator>(
    context: &mut DynamicContext<'_, N>,
    args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if args.len() > 1 {
        return Err(XPathError::wrong_number_of_arguments(
            "local-name",
            1,
            args.len(),
        ));
    }

    let node = get_node_arg(context, args)?;

    match node {
        None => Ok(XPathValue::string("")),
        Some(nav) => Ok(XPathValue::string(nav.local_name().to_string())),
    }
}

// ============================================================================
// fn:namespace-uri($arg as node()?) as xs:anyURI
// ============================================================================

/// Implements fn:namespace-uri - returns the namespace URI of a node.
///
/// If no argument, uses context item.
/// Returns empty anyURI for nodes without namespace.
pub fn namespace_uri<N: DomNavigator>(
    context: &mut DynamicContext<'_, N>,
    args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if args.len() > 1 {
        return Err(XPathError::wrong_number_of_arguments(
            "namespace-uri",
            1,
            args.len(),
        ));
    }

    let node = get_node_arg(context, args)?;

    match node {
        None => Ok(XPathValue::from_atomic(any_uri(""))),
        Some(nav) => Ok(XPathValue::from_atomic(any_uri(nav.namespace_uri()))),
    }
}

// ============================================================================
// fn:node-name($arg as node()?) as xs:QName?
// ============================================================================

/// Implements fn:node-name - returns the QName of a node.
///
/// Element/Attribute: returns QName with prefix, local, namespace
/// ProcessingInstruction/Namespace: returns QName with target/prefix
/// Others: returns empty sequence
pub fn node_name<N: DomNavigator>(
    context: &mut DynamicContext<'_, N>,
    args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if args.len() > 1 {
        return Err(XPathError::wrong_number_of_arguments(
            "node-name",
            1,
            args.len(),
        ));
    }

    let node = get_node_arg(context, args)?;

    match node {
        None => Ok(XPathValue::Empty),
        Some(nav) => {
            let names = context.static_context.names;
            match nav.node_type() {
                DomNodeType::Element | DomNodeType::Attribute => {
                    let local_name = get_or_empty_id(names, nav.local_name());
                    let namespace_uri = get_opt_id(names, nav.namespace_uri());
                    let prefix = get_opt_id(names, nav.prefix());
                    let qname = QualifiedName::new(namespace_uri, local_name, prefix);
                    Ok(XPathValue::from_atomic(XmlValue::new(
                        XmlTypeCode::QName,
                        XmlValueKind::Atomic(XmlAtomicValue::QName(qname)),
                    )))
                }
                DomNodeType::ProcessingInstruction => {
                    // PI has target as name, no namespace
                    let local_name = get_or_empty_id(names, nav.name());
                    let qname = QualifiedName::new(None, local_name, None);
                    Ok(XPathValue::from_atomic(XmlValue::new(
                        XmlTypeCode::QName,
                        XmlValueKind::Atomic(XmlAtomicValue::QName(qname)),
                    )))
                }
                DomNodeType::Namespace => {
                    // Namespace node: prefix is the name
                    let local_name = get_or_empty_id(names, nav.local_name());
                    let qname = QualifiedName::new(None, local_name, None);
                    Ok(XPathValue::from_atomic(XmlValue::new(
                        XmlTypeCode::QName,
                        XmlValueKind::Atomic(XmlAtomicValue::QName(qname)),
                    )))
                }
                // Text, Comment, Document nodes have no name
                _ => Ok(XPathValue::Empty),
            }
        }
    }
}

// ============================================================================
// fn:nilled($arg as node()) as xs:boolean?
// ============================================================================

/// Implements fn:nilled - returns whether an element is nilled (xsi:nil).
///
/// Returns Empty for non-element nodes.
/// Returns boolean for element nodes (false if no xsi:nil or schema info).
pub fn nilled<N: DomNavigator>(
    context: &mut DynamicContext<'_, N>,
    args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if args.len() > 1 {
        return Err(XPathError::wrong_number_of_arguments(
            "nilled",
            1,
            args.len(),
        ));
    }

    let node = get_node_arg_required(context, args)?;

    match node.node_type() {
        DomNodeType::Element => {
            // Check for xsi:nil attribute
            let mut nav = node.clone();
            if nav.move_to_first_attribute() {
                loop {
                    if nav.local_name() == "nil"
                        && nav.namespace_uri() == "http://www.w3.org/2001/XMLSchema-instance"
                    {
                        let value = nav.value();
                        let is_nilled = value == "true" || value == "1";
                        return Ok(XPathValue::boolean(is_nilled));
                    }
                    if !nav.move_to_next_attribute() {
                        break;
                    }
                }
            }
            // No xsi:nil attribute found
            Ok(XPathValue::boolean(false))
        }
        _ => Ok(XPathValue::Empty),
    }
}

// ============================================================================
// fn:base-uri($arg as node()?) as xs:anyURI?
// ============================================================================

/// Implements fn:base-uri - returns the base URI of a node.
///
/// Walks ancestor chain for xml:base attributes and resolves against static base URI.
/// Returns Empty if base URI is not available.
pub fn base_uri<N: DomNavigator>(
    context: &mut DynamicContext<'_, N>,
    args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if args.len() > 1 {
        return Err(XPathError::wrong_number_of_arguments(
            "base-uri",
            1,
            args.len(),
        ));
    }

    let node = get_node_arg(context, args)?;

    match node {
        None => Ok(XPathValue::Empty),
        Some(nav) => {
            let uri = compute_base_uri(&nav, context.base_uri.as_deref());
            match uri {
                Some(u) if !u.is_empty() => Ok(XPathValue::from_atomic(any_uri(u))),
                _ => Ok(XPathValue::Empty),
            }
        }
    }
}

// ============================================================================
// fn:document-uri($arg as node()) as xs:anyURI?
// ============================================================================

/// Implements fn:document-uri - returns the document URI of a document node.
///
/// Only returns a value for Root nodes with a non-empty base URI.
pub fn document_uri<N: DomNavigator>(
    context: &mut DynamicContext<'_, N>,
    args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if args.len() > 1 {
        return Err(XPathError::wrong_number_of_arguments(
            "document-uri",
            1,
            args.len(),
        ));
    }

    let node = get_node_arg_required(context, args)?;

    match node.node_type() {
        DomNodeType::Root => {
            let uri = node.base_uri();
            if uri.is_empty() {
                Ok(XPathValue::Empty)
            } else {
                Ok(XPathValue::from_atomic(any_uri(uri)))
            }
        }
        _ => Ok(XPathValue::Empty),
    }
}

// ============================================================================
// fn:lang($testlang as xs:string, $node as node()?) as xs:boolean
// ============================================================================

/// Implements fn:lang - tests whether a node's language matches.
///
/// Walks up ancestors to find xml:lang attribute.
/// Matching is case-insensitive with subtag support ("en" matches "en-US").
pub fn lang<N: DomNavigator>(
    context: &mut DynamicContext<'_, N>,
    mut args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if args.is_empty() || args.len() > 2 {
        return Err(XPathError::wrong_number_of_arguments("lang", 1, args.len()));
    }

    let test_lang = atomize_to_string_opt(args.remove(0))?.unwrap_or_default();

    let node = if args.is_empty() {
        // Use context item
        match &context.context_item {
            Some(XmlItem::Node(n)) => n.clone(),
            Some(XmlItem::Atomic(_)) => {
                return Err(XPathError::XPTY0004 {
                    expected: "node()".to_string(),
                    found: "atomic value".to_string(),
                });
            }
            None => {
                return Err(XPathError::XPDY0002 {
                    message: "Context item is absent".to_string(),
                });
            }
        }
    } else {
        let node_arg = args.remove(0);
        let items = materialize(node_arg);
        if items.is_empty() {
            return Ok(XPathValue::boolean(false));
        }
        match &items[0] {
            XmlItem::Node(n) => n.clone(),
            XmlItem::Atomic(_) => {
                return Err(XPathError::XPTY0004 {
                    expected: "node()".to_string(),
                    found: "atomic value".to_string(),
                });
            }
        }
    };

    // Find xml:lang in ancestors
    let node_lang = find_xml_lang(&node);

    let result = match node_lang {
        Some(lang) => lang_matches(&lang, &test_lang),
        None => false,
    };

    Ok(XPathValue::boolean(result))
}

// ============================================================================
// fn:root($arg as node()?) as node()?
// ============================================================================

/// Implements fn:root - returns the root of the tree containing the node.
///
/// If no argument, uses context item.
pub fn root<N: DomNavigator>(
    context: &mut DynamicContext<'_, N>,
    args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if args.len() > 1 {
        return Err(XPathError::wrong_number_of_arguments("root", 1, args.len()));
    }

    let node = get_node_arg(context, args)?;

    match node {
        None => Ok(XPathValue::Empty),
        Some(mut nav) => {
            nav.move_to_root();
            Ok(XPathValue::from_node(nav))
        }
    }
}

// ============================================================================
// fn:id($arg as xs:string*, $node as node()) as element()*
// ============================================================================

/// Implements fn:id - selects elements by their ID attribute value.
///
/// If 1 arg: uses context item as reference node.
/// If 2 args: second arg is the reference node.
///
/// The reference node determines which document tree to search.
/// Each string argument is tokenized by whitespace and each token
/// is looked up via `find_element_by_id`. Results are deduplicated
/// and returned in document order.
///
/// Without DTD/schema ID declarations, the default `find_element_by_id`
/// returns `None`, so this returns an empty sequence.
pub fn id<N: DomNavigator>(
    context: &mut DynamicContext<'_, N>,
    args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if args.is_empty() || args.len() > 2 {
        return Err(XPathError::wrong_number_of_arguments("id", 1, args.len()));
    }

    let mut args = args;

    // Get the reference node (arg 2 or context item)
    let ref_node = if args.len() == 2 {
        let node_arg = args.remove(1);
        let items = materialize(node_arg);
        if items.len() != 1 {
            return Err(XPathError::XPTY0004 {
                expected: "node()".to_string(),
                found: if items.is_empty() {
                    "empty-sequence()".to_string()
                } else {
                    format!("sequence of {} items", items.len())
                },
            });
        }
        match items.into_iter().next().unwrap() {
            XmlItem::Node(n) => n,
            XmlItem::Atomic(_) => {
                return Err(XPathError::XPTY0004 {
                    expected: "node()".to_string(),
                    found: "atomic value".to_string(),
                });
            }
        }
    } else {
        match &context.context_item {
            Some(XmlItem::Node(n)) => n.clone(),
            Some(XmlItem::Atomic(_)) => {
                return Err(XPathError::XPTY0004 {
                    expected: "node()".to_string(),
                    found: "atomic value".to_string(),
                });
            }
            None => {
                return Err(XPathError::XPDY0002 {
                    message: "Context item is absent".to_string(),
                });
            }
        }
    };

    // Navigate reference node to document root
    let mut root_nav = ref_node;
    root_nav.move_to_root();

    // Collect all ID tokens from the first argument
    let id_arg = args.into_iter().next().unwrap();
    let tokens = collect_id_tokens(id_arg);

    // Look up each token, dedup by position
    let mut result_nodes: Vec<N> = Vec::new();
    for token in &tokens {
        if let Some(found) = root_nav.find_element_by_id(token)? {
            // Deduplicate: only add if not already present
            let already_present = result_nodes.iter().any(|n| n.is_same_position(&found));
            if !already_present {
                result_nodes.push(found);
            }
        }
    }

    // Sort in document order
    result_nodes.sort_by(|a, b| {
        use crate::xpath::node_ops::compare_document_order;
        compare_document_order(a, b)
    });

    // Convert to XPathValue
    let items: Vec<XmlItem<N>> = result_nodes.into_iter().map(XmlItem::Node).collect();
    Ok(XPathValue::from_sequence(items))
}

/// Collect whitespace-tokenized ID strings from an XPathValue argument.
///
/// Per the spec, each string value in the argument is split on whitespace
/// and each resulting token is an IDREF to look up.
fn collect_id_tokens<N: DomNavigator>(value: XPathValue<N>) -> Vec<String> {
    let mut tokens = Vec::new();
    match value {
        XPathValue::Empty => {}
        XPathValue::Item(item) => {
            let s = item_string_value(item);
            for token in s.split_whitespace() {
                tokens.push(token.to_string());
            }
        }
        XPathValue::Sequence(items) => {
            for item in items {
                let s = item_string_value(item);
                for token in s.split_whitespace() {
                    tokens.push(token.to_string());
                }
            }
        }
    }
    tokens
}

/// Get the string value of an XmlItem for ID tokenization.
fn item_string_value<N: DomNavigator>(item: XmlItem<N>) -> String {
    match item {
        XmlItem::Node(nav) => nav.value(),
        XmlItem::Atomic(v) => crate::xpath::atomize::string_value(&v),
    }
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Create an anyURI value.
fn any_uri(s: impl Into<String>) -> XmlValue {
    XmlValue::new(
        XmlTypeCode::AnyUri,
        XmlValueKind::Atomic(XmlAtomicValue::AnyUri(s.into())),
    )
}

/// Get a NameId from a string, using NameId(0) (empty string) if string is empty.
///
/// This is used for required names (like local-name) where we need a NameId.
/// The string is interned using `add()` which always succeeds.
fn get_or_empty_id(names: &NameTable, s: &str) -> NameId {
    if s.is_empty() {
        NameId(0)
    } else {
        names.add(s)
    }
}

/// Get an optional NameId from a string.
///
/// Returns None if the string is empty, Some(NameId) otherwise.
/// The string is interned using `add()` which always succeeds.
fn get_opt_id(names: &NameTable, s: &str) -> Option<NameId> {
    if s.is_empty() {
        None
    } else {
        Some(names.add(s))
    }
}

/// Get a node argument, using context item if no argument provided.
/// Returns None for empty sequence.
fn get_node_arg<N: DomNavigator>(
    context: &DynamicContext<'_, N>,
    args: Vec<XPathValue<N>>,
) -> Result<Option<N>, XPathError> {
    if args.is_empty() {
        // Use context item
        match &context.context_item {
            Some(XmlItem::Node(n)) => Ok(Some(n.clone())),
            Some(XmlItem::Atomic(_)) => {
                // Non-node context item returns empty for these functions
                Ok(None)
            }
            None => Err(XPathError::XPDY0002 {
                message: "Context item is absent".to_string(),
            }),
        }
    } else {
        let items = materialize(args.into_iter().next().unwrap());
        if items.is_empty() {
            return Ok(None);
        }
        match &items[0] {
            XmlItem::Node(n) => Ok(Some(n.clone())),
            XmlItem::Atomic(_) => {
                // Non-node returns empty for these functions
                Ok(None)
            }
        }
    }
}

/// Get a required node argument.
fn get_node_arg_required<N: DomNavigator>(
    context: &DynamicContext<'_, N>,
    args: Vec<XPathValue<N>>,
) -> Result<N, XPathError> {
    if args.is_empty() {
        // Use context item
        match &context.context_item {
            Some(XmlItem::Node(n)) => Ok(n.clone()),
            Some(XmlItem::Atomic(_)) => Err(XPathError::XPTY0004 {
                expected: "node()".to_string(),
                found: "atomic value".to_string(),
            }),
            None => Err(XPathError::XPDY0002 {
                message: "Context item is absent".to_string(),
            }),
        }
    } else {
        let items = materialize(args.into_iter().next().unwrap());
        if items.is_empty() {
            return Err(XPathError::XPTY0004 {
                expected: "node()".to_string(),
                found: "empty-sequence()".to_string(),
            });
        }
        match &items[0] {
            XmlItem::Node(n) => Ok(n.clone()),
            XmlItem::Atomic(_) => Err(XPathError::XPTY0004 {
                expected: "node()".to_string(),
                found: "atomic value".to_string(),
            }),
        }
    }
}

/// Compute the base URI of a node by walking ancestor chain.
///
/// Per XPath 2.0:
/// 1. Walk ancestor chain collecting xml:base attributes
/// 2. Get document base URI at root
/// 3. Resolve chain against static base URI
fn compute_base_uri<N: DomNavigator>(node: &N, static_base_uri: Option<&str>) -> Option<String> {
    let mut xml_bases: Vec<String> = Vec::new();
    let mut nav = node.clone();

    // For text, comment, PI nodes, start from parent
    match nav.node_type() {
        DomNodeType::Text
        | DomNodeType::Whitespace
        | DomNodeType::SignificantWhitespace
        | DomNodeType::Comment
        | DomNodeType::ProcessingInstruction => {
            if !nav.move_to_parent() {
                return None;
            }
        }
        _ => {}
    }

    // Walk up ancestor chain, collecting xml:base attributes
    loop {
        if nav.node_type() == DomNodeType::Element {
            if let Some(xml_base) = get_xml_base_attr(&nav) {
                xml_bases.push(xml_base);
            }
        }

        if nav.node_type() == DomNodeType::Root {
            // At root - get document base URI
            let doc_base = nav.base_uri();
            if !doc_base.is_empty() {
                xml_bases.push(doc_base.to_string());
            }
            break;
        }

        if !nav.move_to_parent() {
            break;
        }
    }

    // Start with static base URI (if any)
    let mut base = static_base_uri.map(|s| s.to_string());

    // Resolve xml:base chain from root to node (reverse order)
    for uri in xml_bases.into_iter().rev() {
        base = Some(resolve_uri(&uri, base.as_deref()));
    }

    base
}

/// Get xml:base attribute value from an element node.
fn get_xml_base_attr<N: DomNavigator>(nav: &N) -> Option<String> {
    let mut attr_nav = nav.clone();
    if attr_nav.move_to_first_attribute() {
        loop {
            if attr_nav.local_name() == "base"
                && attr_nav.namespace_uri() == "http://www.w3.org/XML/1998/namespace"
            {
                return Some(attr_nav.value());
            }
            if !attr_nav.move_to_next_attribute() {
                break;
            }
        }
    }
    None
}

/// Resolve a URI reference against a base URI.
///
/// Simple implementation that handles:
/// - Absolute URIs (returned as-is)
/// - Relative paths resolved against base
fn resolve_uri(uri: &str, base: Option<&str>) -> String {
    // If URI is absolute (has scheme), return as-is
    if uri.contains("://") || uri.starts_with("file:") {
        return uri.to_string();
    }

    match base {
        None => uri.to_string(),
        Some(base_uri) => {
            if uri.is_empty() {
                return base_uri.to_string();
            }

            // Simple resolution: append relative to base directory
            if uri.starts_with('/') {
                // Absolute path - find scheme://host and append
                if let Some(scheme_end) = base_uri.find("://") {
                    if let Some(path_start) = base_uri[scheme_end + 3..].find('/') {
                        let host_end = scheme_end + 3 + path_start;
                        return format!("{}{}", &base_uri[..host_end], uri);
                    }
                }
                uri.to_string()
            } else {
                // Relative path - append to base directory
                if let Some(last_slash) = base_uri.rfind('/') {
                    format!("{}/{}", &base_uri[..last_slash], uri)
                } else {
                    uri.to_string()
                }
            }
        }
    }
}

/// Find xml:lang attribute by walking up ancestors.
fn find_xml_lang<N: DomNavigator>(node: &N) -> Option<String> {
    let mut nav = node.clone();

    loop {
        // Check attributes on this element
        if nav.node_type() == DomNodeType::Element {
            let mut attr_nav = nav.clone();
            if attr_nav.move_to_first_attribute() {
                loop {
                    if attr_nav.local_name() == "lang"
                        && attr_nav.namespace_uri() == "http://www.w3.org/XML/1998/namespace"
                    {
                        return Some(attr_nav.value());
                    }
                    if !attr_nav.move_to_next_attribute() {
                        break;
                    }
                }
            }
        }

        // Move to parent
        if !nav.move_to_parent() {
            break;
        }
    }

    None
}

/// Check if a language tag matches the test language.
///
/// Case-insensitive comparison with subtag support.
/// "en" matches "en", "en-US", "en-GB", etc.
fn lang_matches(lang: &str, test_lang: &str) -> bool {
    let lang_lower = lang.to_lowercase();
    let test_lower = test_lang.to_lowercase();

    if lang_lower == test_lower {
        return true;
    }

    // Check if lang starts with test_lang followed by '-'
    if lang_lower.starts_with(&test_lower) {
        let remainder = &lang_lower[test_lower.len()..];
        if remainder.starts_with('-') {
            return true;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lang_matches_exact() {
        assert!(lang_matches("en", "en"));
        assert!(lang_matches("EN", "en"));
        assert!(lang_matches("en", "EN"));
    }

    #[test]
    fn test_lang_matches_subtag() {
        assert!(lang_matches("en-US", "en"));
        assert!(lang_matches("en-GB", "en"));
        assert!(lang_matches("zh-Hans-CN", "zh"));
    }

    #[test]
    fn test_lang_matches_no_match() {
        assert!(!lang_matches("de", "en"));
        assert!(!lang_matches("english", "en"));
        assert!(!lang_matches("en", "en-US"));
    }

    #[test]
    fn test_any_uri_creation() {
        let uri = any_uri("http://example.com");
        assert_eq!(uri.type_code, XmlTypeCode::AnyUri);
    }

    #[test]
    fn test_lang_matches_empty_testlang() {
        // Empty test_lang should not match anything
        assert!(!lang_matches("en", ""));
        assert!(!lang_matches("en-US", ""));
    }

    #[test]
    fn test_resolve_uri_absolute() {
        // Absolute URI returned as-is
        assert_eq!(
            resolve_uri("http://example.com/path", Some("http://other.com/")),
            "http://example.com/path"
        );
    }

    #[test]
    fn test_resolve_uri_relative() {
        // Relative path appended to base directory
        assert_eq!(
            resolve_uri("file.xml", Some("http://example.com/dir/base.xml")),
            "http://example.com/dir/file.xml"
        );
    }

    #[test]
    fn test_resolve_uri_absolute_path() {
        // Absolute path resolved against host
        assert_eq!(
            resolve_uri("/absolute/path.xml", Some("http://example.com/dir/base.xml")),
            "http://example.com/absolute/path.xml"
        );
    }

    #[test]
    fn test_resolve_uri_no_base() {
        // No base returns URI as-is
        assert_eq!(resolve_uri("relative.xml", None), "relative.xml");
    }

    #[test]
    fn test_resolve_uri_empty() {
        // Empty URI returns base
        assert_eq!(
            resolve_uri("", Some("http://example.com/base.xml")),
            "http://example.com/base.xml"
        );
    }

    // ========================================================================
    // fn:id tests
    // ========================================================================

    #[test]
    fn test_collect_id_tokens_single_string() {
        use crate::xpath::RoXmlNavigator;
        let value: super::super::XPathValue<RoXmlNavigator<'static>> =
            super::super::XPathValue::string("abc");
        let tokens = collect_id_tokens(value);
        assert_eq!(tokens, vec!["abc"]);
    }

    #[test]
    fn test_collect_id_tokens_multi_whitespace() {
        use crate::xpath::RoXmlNavigator;
        let value: super::super::XPathValue<RoXmlNavigator<'static>> =
            super::super::XPathValue::string("  foo   bar  baz  ");
        let tokens = collect_id_tokens(value);
        assert_eq!(tokens, vec!["foo", "bar", "baz"]);
    }

    #[test]
    fn test_collect_id_tokens_empty() {
        use crate::xpath::RoXmlNavigator;
        let value: super::super::XPathValue<RoXmlNavigator<'static>> =
            super::super::XPathValue::Empty;
        let tokens = collect_id_tokens(value);
        assert!(tokens.is_empty());
    }

    #[test]
    fn test_collect_id_tokens_sequence() {
        use crate::xpath::RoXmlNavigator;
        use crate::xpath::iterator::XmlItem;
        use crate::types::value::XmlValue;

        let items = vec![
            XmlItem::Atomic(XmlValue::string("a1 a2")),
            XmlItem::Atomic(XmlValue::string("b1")),
        ];
        let value: super::super::XPathValue<RoXmlNavigator<'static>> =
            super::super::XPathValue::from_sequence(items);
        let tokens = collect_id_tokens(value);
        assert_eq!(tokens, vec!["a1", "a2", "b1"]);
    }

    #[test]
    fn test_fn_id_empty_without_dtd() {
        // RoXmlNavigator inherits default find_element_by_id (returns None),
        // so fn:id should return empty sequence, not an error.
        use crate::namespace::table::NameTable;
        use crate::xpath::context::{DynamicContext, XPathContext};
        use crate::xpath::RoXmlNavigator;

        let doc = roxmltree::Document::parse("<root><a id='x'/></root>").unwrap();
        let nav = RoXmlNavigator::new(&doc);

        let names = NameTable::new();
        let static_ctx = XPathContext::new(&names);
        let mut dyn_ctx = DynamicContext::new(&static_ctx, 0);
        dyn_ctx.context_item = Some(XmlItem::Node(nav));

        let args = vec![super::super::XPathValue::string("x")];
        let result = id(&mut dyn_ctx, args).unwrap();

        // Should be empty (no DTD/schema means no ID declarations)
        assert!(
            matches!(result, super::super::XPathValue::Empty),
            "Expected empty sequence from fn:id without DTD"
        );
    }
}
