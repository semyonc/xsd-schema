use xsd_schema::xpath::roxmltree::RoXmlNavigator;
use xsd_schema::xpath::tree_comparer::TreeComparer;
use xsd_schema::xpath::functions::XPathValue;
use xsd_schema::xpath::iterator::XmlItem;
use xsd_schema::xpath::{DomNavigator, DomNodeType};

/// Special test names that force XML comparison.
const FORCE_XML_COMPARE: &[&str] = &["ReturnExpr010"];

/// Special test names that force is_single to false.
const FORCE_NOT_SINGLE: &[&str] = &["CondExpr012", "NodeTest006"];

/// Special test names treated as exceptions (no wrapping).
const IS_EXCEPTION: &[&str] = &[
    "fn-union-node-args-005",
    "fn-union-node-args-006",
    "fn-union-node-args-007",
    "fn-union-node-args-009",
    "fn-union-node-args-010",
    "fn-union-node-args-011",
];

/// Serialize a DomNavigator node to XML string.
fn serialize_node_to_xml<N: DomNavigator>(nav: &N, out: &mut String) {
    match nav.node_type() {
        DomNodeType::Element => {
            let prefix = nav.prefix();
            let local = nav.local_name();

            // Opening tag
            if prefix.is_empty() {
                out.push('<');
                out.push_str(local);
            } else {
                out.push('<');
                out.push_str(prefix);
                out.push(':');
                out.push_str(local);
            }

            // Namespace declarations (serialize from namespace axis)
            let mut ns_nav = nav.clone();
            if ns_nav.move_to_first_namespace(xsd_schema::xpath::NamespaceAxisScope::Local) {
                loop {
                    let ns_prefix = ns_nav.local_name().to_string();
                    let ns_uri = ns_nav.value();
                    if ns_prefix.is_empty() {
                        out.push_str(&format!(" xmlns=\"{}\"", escape_xml_attr(&ns_uri)));
                    } else {
                        out.push_str(&format!(
                            " xmlns:{}=\"{}\"",
                            ns_prefix,
                            escape_xml_attr(&ns_uri)
                        ));
                    }
                    if !ns_nav.move_to_next_namespace(
                        xsd_schema::xpath::NamespaceAxisScope::Local,
                    ) {
                        break;
                    }
                }
            }

            // Attributes
            let mut attr_nav = nav.clone();
            if attr_nav.move_to_first_attribute() {
                loop {
                    let attr_prefix = attr_nav.prefix().to_string();
                    let attr_local = attr_nav.local_name().to_string();
                    let attr_value = attr_nav.value();
                    if attr_prefix.is_empty() {
                        out.push_str(&format!(
                            " {}=\"{}\"",
                            attr_local,
                            escape_xml_attr(&attr_value)
                        ));
                    } else {
                        out.push_str(&format!(
                            " {}:{}=\"{}\"",
                            attr_prefix,
                            attr_local,
                            escape_xml_attr(&attr_value)
                        ));
                    }
                    if !attr_nav.move_to_next_attribute() {
                        break;
                    }
                }
            }

            // Children
            let mut child_nav = nav.clone();
            if child_nav.move_to_first_child() {
                out.push('>');
                loop {
                    serialize_node_to_xml(&child_nav, out);
                    if !child_nav.move_to_next_sibling() {
                        break;
                    }
                }
                // Closing tag
                if prefix.is_empty() {
                    out.push_str(&format!("</{}>", local));
                } else {
                    out.push_str(&format!("</{}:{}>", prefix, local));
                }
            } else {
                    out.push_str("/>");
            }
        }
        DomNodeType::Text | DomNodeType::Whitespace | DomNodeType::SignificantWhitespace => {
            out.push_str(&escape_xml_text(&nav.value()));
        }
        DomNodeType::Comment => {
            out.push_str(&format!("<!--{}-->", nav.value()));
        }
        DomNodeType::ProcessingInstruction => {
            let value = nav.value();
            if value.is_empty() {
                out.push_str(&format!("<?{}?>", nav.local_name()));
            } else {
                out.push_str(&format!("<?{} {}?>", nav.local_name(), value));
            }
        }
        DomNodeType::Attribute => {
            let prefix = nav.prefix();
            let local = nav.local_name();
            let value = nav.value();
            if prefix.is_empty() {
                out.push_str(&format!("{}=\"{}\"", local, escape_xml_attr(&value)));
            } else {
                out.push_str(&format!(
                    "{}:{}=\"{}\"",
                    prefix,
                    local,
                    escape_xml_attr(&value)
                ));
            }
        }
        DomNodeType::Root => {
            // Serialize children of document root
            let mut child_nav = nav.clone();
            if child_nav.move_to_first_child() {
                loop {
                    serialize_node_to_xml(&child_nav, out);
                    if !child_nav.move_to_next_sibling() {
                        break;
                    }
                }
            }
        }
        _ => {}
    }
}

/// Serialize XPath result items to an XML string.
fn serialize_xpath_result<N: DomNavigator>(
    items: &[XmlItem<N>],
    xml_compare: bool,
    is_single: bool,
    is_excpt: bool,
) -> String {
    let mut out = String::new();
    let wrap = !((xml_compare && is_single) || is_excpt);

    if wrap {
        out.push_str("<?xml version='1.0'?>");
        out.push_str("<root>");
    }

    let mut string_flag = false;
    for item in items {
        match item {
            XmlItem::Node(nav) => {
                if nav.node_type() == DomNodeType::Attribute {
                    // Standalone attribute
                    let prefix = nav.prefix();
                    let local = nav.local_name();
                    let value = nav.value();
                    if prefix.is_empty() {
                        out.push_str(&format!("{}=\"{}\"", local, escape_xml_attr(&value)));
                    } else {
                        out.push_str(&format!(
                            "{}:{}=\"{}\"",
                            prefix,
                            local,
                            escape_xml_attr(&value)
                        ));
                    }
                } else {
                    serialize_node_to_xml(nav, &mut out);
                }
                string_flag = false;
            }
            XmlItem::Atomic(val) => {
                if string_flag {
                    out.push(' ');
                }
                out.push_str(&val.to_string_value());
                string_flag = true;
            }
        }
    }

    if wrap {
        out.push_str("</root>");
    }

    out
}

/// Compare XPath result against an expected output file.
///
/// Port of C# CompareResult (Form1.cs:827-920).
/// Takes ownership of result since we need to convert to Vec.
pub fn compare_result<'a>(
    test_name: &str,
    result: XPathValue<RoXmlNavigator<'a>>,
    expected_path: &str,
    xml_compare: bool,
) -> Result<bool, String> {
    let xml_compare = xml_compare || FORCE_XML_COMPARE.contains(&test_name);
    let is_excpt = IS_EXCEPTION.contains(&test_name);

    let is_single = if FORCE_NOT_SINGLE.contains(&test_name) {
        false
    } else {
        result.is_single()
    };

    // Serialize result to XML
    let items = result.into_vec();
    let result_xml = serialize_xpath_result(&items, xml_compare, is_single, is_excpt);

    // Load expected file
    let expected_content = std::fs::read_to_string(expected_path)
        .map_err(|e| format!("Failed to read expected file {}: {}", expected_path, e))?;

    let expected_xml = if xml_compare {
        expected_content
    } else {
        format!(
            "<?xml version='1.0'?><root>{}</root>",
            expected_content
        )
    };

    // Parse both and compare with TreeComparer
    let doc1 = roxmltree::Document::parse(&expected_xml)
        .map_err(|e| format!("Failed to parse expected XML: {} (content: {})", e, truncate(&expected_xml, 200)))?;
    let doc2 = roxmltree::Document::parse(&result_xml)
        .map_err(|e| format!("Failed to parse result XML: {} (content: {})", e, truncate(&result_xml, 200)))?;

    let nav1 = RoXmlNavigator::new(&doc1);
    let nav2 = RoXmlNavigator::new(&doc2);

    let comparer = TreeComparer::with_ignore_whitespace(true);
    Ok(comparer.deep_equal(&nav1, &nav2))
}

fn escape_xml_text(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn escape_xml_attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn truncate(s: &str, max_len: usize) -> &str {
    if s.len() <= max_len {
        s
    } else {
        &s[..max_len]
    }
}
