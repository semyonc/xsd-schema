//! Embedded schema assets
//!
//! Contains well-known schemas embedded as static binary data for
//! offline use without network access.

/// Embedded xml.xsd schema defining xml:lang, xml:space, xml:base attributes.
/// Source: <http://www.w3.org/2001/xml.xsd>
pub const XML_XSD: &[u8] = include_bytes!("../assets/xml.xsd");

/// Embedded xlink.xsd schema defining XLink attributes (xlink:type, xlink:href, etc.).
/// Source: <http://www.w3.org/XML/2008/06/xlink.xsd>
pub const XLINK_XSD: &[u8] = include_bytes!("../assets/xlink.xsd");

/// Well-known namespace URI for the xml: prefix
pub const XML_NAMESPACE: &str = "http://www.w3.org/XML/1998/namespace";

/// Well-known namespace URI for the xlink: prefix
pub const XLINK_NAMESPACE: &str = "http://www.w3.org/1999/xlink";

/// Get embedded schema by namespace URI
pub fn get_embedded_schema(namespace: &str) -> Option<&'static [u8]> {
    match namespace {
        XML_NAMESPACE => Some(XML_XSD),
        XLINK_NAMESPACE => Some(XLINK_XSD),
        _ => None,
    }
}

/// Check if a namespace has an embedded schema available
pub fn has_embedded_schema(namespace: &str) -> bool {
    matches!(namespace, XML_NAMESPACE | XLINK_NAMESPACE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_xml_xsd_embedded() {
        let content = XML_XSD;
        assert!(!content.is_empty());
        // Verify it's valid XML
        let xml_str = std::str::from_utf8(content).expect("xml.xsd should be valid UTF-8");
        assert!(xml_str.contains("targetNamespace=\"http://www.w3.org/XML/1998/namespace\""));
    }

    #[test]
    fn test_xlink_xsd_embedded() {
        let content = XLINK_XSD;
        assert!(!content.is_empty());
        let xml_str = std::str::from_utf8(content).expect("xlink.xsd should be valid UTF-8");
        assert!(xml_str.contains("targetNamespace=\"http://www.w3.org/1999/xlink\""));
    }

    #[test]
    fn test_get_embedded_schema() {
        assert!(get_embedded_schema(XML_NAMESPACE).is_some());
        assert!(get_embedded_schema(XLINK_NAMESPACE).is_some());
        assert!(get_embedded_schema("http://example.com/unknown").is_none());
    }

    #[test]
    fn test_has_embedded_schema() {
        assert!(has_embedded_schema(XML_NAMESPACE));
        assert!(has_embedded_schema(XLINK_NAMESPACE));
        assert!(!has_embedded_schema("http://example.com/unknown"));
    }
}
