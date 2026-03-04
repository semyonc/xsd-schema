//! Schema-aware document builder.
//!
//! [`build_typed_document`] constructs a [`BufferDocument`] while interleaving
//! validation calls so that each element and attribute node carries a
//! [`NodeSchemaBinding`] (type key, declaration keys, content type).

use std::collections::HashMap;
use std::io::BufRead;

use bumpalo::Bump;
use quick_xml::events::Event;
use quick_xml::Reader;

use crate::namespace::context::NamespaceContextSnapshot;
use crate::namespace::table::{XML_NAMESPACE, XSI_NAMESPACE};
use crate::parser::location::SourceSpan;
use crate::schema::SchemaSet;
use crate::xpath::string_ops::is_xml_whitespace_str;
use crate::validation::errors::ValidationError;
use crate::validation::info::ValidationFlags;
use crate::validation::validator::{ValidationSink, ValidationWarning};
use crate::validation::{SchemaValidator, ValidationRuntime};

use super::buffer::BufferDocument;
use super::builder::{parse_pi_content, split_prefix_local, BufferDocumentBuilder};
use super::error::BufferDocumentError;
use super::type_remap::NodeSchemaBinding;
use super::BufferDocumentOptions;

// ── SilentValidationSink ──────────────────────────────────────────────

/// A [`ValidationSink`] that silently discards all errors and warnings.
///
/// Used by [`build_typed_document`] when the caller only needs schema
/// bindings on nodes and does not care about validation diagnostics.
pub struct SilentValidationSink;

impl ValidationSink for SilentValidationSink {
    fn on_error(&mut self, _error: ValidationError) {}
    fn on_warning(&mut self, _warning: ValidationWarning) {}
}

// ── build_typed_document ──────────────────────────────────────────────

/// Build a [`BufferDocument`] with schema bindings on every element and attribute.
///
/// This function mirrors [`BufferDocumentBuilder::build`] but interleaves
/// [`SchemaValidator`] calls so that the resulting document carries
/// [`NodeSchemaBinding`] entries for typed-value and schema-type queries.
///
/// Validation errors are silently discarded via [`SilentValidationSink`].
/// If you need to collect validation errors, use the lower-level push API
/// with a custom sink instead.
pub fn build_typed_document<'a, R: BufRead>(
    reader: R,
    arena: &'a Bump,
    schema_set: &'a SchemaSet,
    options: BufferDocumentOptions,
) -> Result<BufferDocument<'a>, BufferDocumentError> {
    let mut builder = BufferDocumentBuilder::new(
        arena,
        &schema_set.name_table,
        Some(schema_set),
        options,
    )?;

    let validator = SchemaValidator::new(schema_set, ValidationFlags::default());
    let mut runtime = validator.start_run(SilentValidationSink);

    let mut xml_reader = Reader::from_reader(reader);
    xml_reader.trim_text(false);

    // Transient prefix → URI mapping for namespace resolution
    let mut prefix_map: HashMap<Box<[u8]>, Vec<String>> = HashMap::new();
    prefix_map
        .entry(b"xml".to_vec().into_boxed_slice())
        .or_default()
        .push(XML_NAMESPACE.to_string());
    prefix_map
        .entry(b"".to_vec().into_boxed_slice())
        .or_default()
        .push(String::new());

    // Per-element declared prefixes for cleanup on close
    let mut scope_decls: Vec<Vec<Box<[u8]>>> = Vec::new();

    // Element ref stack for text/end correlation
    let mut element_ref_stack: Vec<u32> = Vec::new();

    let track = builder.track_source_locations();
    // Pending spans: (elem_ref, start_position) — mirrors builder.rs:pending_spans
    let mut pending_spans: Vec<(u32, usize)> = Vec::new();

    let mut buf = Vec::with_capacity(1024);

    loop {
        let event_start = if track {
            xml_reader.buffer_position()
        } else {
            0
        };

        match xml_reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                let elem_ref = handle_start_or_empty(
                    &mut builder,
                    &mut runtime,
                    e,
                    false,
                    &mut prefix_map,
                    &mut scope_decls,
                    schema_set,
                )?;
                element_ref_stack.push(elem_ref);
                if track {
                    pending_spans.push((elem_ref, event_start));
                }
            }
            Ok(Event::Empty(ref e)) => {
                let elem_ref = handle_start_or_empty(
                    &mut builder,
                    &mut runtime,
                    e,
                    true,
                    &mut prefix_map,
                    &mut scope_decls,
                    schema_set,
                )?;
                if track {
                    builder.set_source_span(
                        elem_ref,
                        SourceSpan::new(event_start, xml_reader.buffer_position()),
                    );
                }
                // Empty elements don't push to element_ref_stack — they
                // complete inline (handle_start_or_empty pops scope and
                // calls end_element + validate_end_element).
            }
            Ok(Event::End(_)) => {
                if track {
                    if let Some((elem_ref, start)) = pending_spans.pop() {
                        builder.set_source_span(
                            elem_ref,
                            SourceSpan::new(start, xml_reader.buffer_position()),
                        );
                    }
                }
                // Pop namespace scope
                if let Some(decls) = scope_decls.pop() {
                    for prefix_key in &decls {
                        if let Some(stack) = prefix_map.get_mut(prefix_key.as_ref()) {
                            stack.pop();
                        }
                    }
                }
                builder.end_element()?;
                runtime.validate_end_element();
                element_ref_stack.pop();
            }
            Ok(Event::Text(ref e)) => {
                if !element_ref_stack.is_empty() {
                    let text = e.unescape()?;
                    builder.text(&text);
                    if is_xml_whitespace_str(&text) {
                        runtime.validate_whitespace(&text);
                    } else {
                        runtime.validate_text(&text);
                    }
                }
            }
            Ok(Event::CData(ref e)) => {
                if !element_ref_stack.is_empty() {
                    let text = std::str::from_utf8(e)?;
                    builder.text(text);
                    if is_xml_whitespace_str(text) {
                        runtime.validate_whitespace(text);
                    } else {
                        runtime.validate_text(text);
                    }
                }
            }
            Ok(Event::Comment(ref e)) => {
                let text = std::str::from_utf8(e)?;
                builder.comment(text)?;
            }
            Ok(Event::PI(ref e)) => {
                let raw = std::str::from_utf8(e)?;
                let (target, data) = parse_pi_content(raw);
                builder.processing_instruction(target, data)?;
            }
            Ok(Event::Decl(_) | Event::DocType(_)) => {}
            Ok(Event::Eof) => break,
            Err(e) => return Err(e.into()),
        }
        buf.clear();
    }

    let _ = runtime.end_validation();
    builder.finalize()
}

// ── handle_start_or_empty ─────────────────────────────────────────────

/// Process an element start (or empty) event with interleaved validation.
fn handle_start_or_empty<S: ValidationSink>(
    builder: &mut BufferDocumentBuilder<'_>,
    runtime: &mut ValidationRuntime<'_, S>,
    e: &quick_xml::events::BytesStart<'_>,
    is_empty: bool,
    prefix_map: &mut HashMap<Box<[u8]>, Vec<String>>,
    scope_decls: &mut Vec<Vec<Box<[u8]>>>,
    schema_set: &SchemaSet,
) -> Result<u32, BufferDocumentError> {
    let mut local_decls: Vec<Box<[u8]>> = Vec::new();
    let mut ns_decls_str: Vec<(String, String)> = Vec::new();

    // ── First pass: collect xmlns declarations ────────────────────────
    for attr_result in e.attributes() {
        let attr = attr_result?;
        let key = attr.key.as_ref();

        if key == b"xmlns" {
            let value = attr.unescape_value()?;
            let uri = value.to_string();
            let prefix_key: Box<[u8]> = b"".to_vec().into_boxed_slice();
            prefix_map
                .entry(prefix_key.clone())
                .or_default()
                .push(uri.clone());
            local_decls.push(prefix_key);
            ns_decls_str.push((String::new(), uri));
        } else if key.starts_with(b"xmlns:") {
            let prefix_bytes = &key[6..];
            let value = attr.unescape_value()?;
            let uri = value.to_string();
            let prefix_key: Box<[u8]> = prefix_bytes.to_vec().into_boxed_slice();
            prefix_map
                .entry(prefix_key.clone())
                .or_default()
                .push(uri.clone());
            local_decls.push(prefix_key);
            let prefix_str =
                std::str::from_utf8(prefix_bytes).map_err(BufferDocumentError::Utf8)?;
            ns_decls_str.push((prefix_str.to_string(), uri));
        }
    }

    scope_decls.push(local_decls);

    // Build ns_declarations slice for start_element
    let ns_decl_refs: Vec<(&str, &str)> = ns_decls_str
        .iter()
        .map(|(p, u)| (p.as_str(), u.as_str()))
        .collect();

    // Resolve element name
    let full_name = e.name();
    let full_name_bytes = full_name.as_ref();
    let (elem_prefix_bytes, elem_local_bytes) = split_prefix_local(full_name_bytes);

    let elem_local =
        std::str::from_utf8(elem_local_bytes).map_err(BufferDocumentError::Utf8)?;
    let elem_prefix_str =
        std::str::from_utf8(elem_prefix_bytes).map_err(BufferDocumentError::Utf8)?;

    // Resolve element namespace
    let elem_ns_uri = match prefix_map.get(elem_prefix_bytes) {
        Some(stack) if !stack.is_empty() => stack.last().unwrap().as_str().to_string(),
        _ if elem_prefix_bytes.is_empty() => String::new(),
        _ => {
            return Err(BufferDocumentError::UnboundPrefix(
                elem_prefix_str.to_string(),
            ))
        }
    };

    // ── Scan attributes for xsi:type and xsi:nil ──────────────────────
    let mut xsi_type: Option<String> = None;
    let mut xsi_nil: Option<String> = None;

    for attr_result in e.attributes() {
        let attr = attr_result?;
        let key = attr.key.as_ref();
        if key == b"xmlns" || key.starts_with(b"xmlns:") {
            continue;
        }
        let (attr_prefix_bytes, attr_local_bytes) = split_prefix_local(key);
        if attr_prefix_bytes.is_empty() {
            continue;
        }
        // Resolve attribute namespace to check for XSI
        if let Some(stack) = prefix_map.get(attr_prefix_bytes) {
            if let Some(ns_uri) = stack.last() {
                if ns_uri == XSI_NAMESPACE {
                    let local = std::str::from_utf8(attr_local_bytes)
                        .map_err(BufferDocumentError::Utf8)?;
                    let value = attr.unescape_value()?;
                    match local {
                        "type" => xsi_type = Some(value.to_string()),
                        "nil" => xsi_nil = Some(value.to_string()),
                        _ => {}
                    }
                }
            }
        }
    }

    // ── Build NamespaceContextSnapshot for xsi:type resolution ────────
    let ns_ctx = build_ns_context(prefix_map, schema_set);

    // ── Push element to builder ───────────────────────────────────────
    let elem_ref = builder.start_element(elem_local, &elem_ns_uri, elem_prefix_str, &ns_decl_refs)?;

    // ── Validate element ──────────────────────────────────────────────
    let info = runtime.validate_element(
        elem_local,
        &elem_ns_uri,
        xsi_type.as_deref(),
        xsi_nil.as_deref(),
        &ns_ctx,
    );

    // Set schema binding on element
    if let Some(tk) = info.schema_type {
        let binding = NodeSchemaBinding {
            type_key: tk,
            element_decl: info.element_decl,
            attribute_decl: None,
            content_type: info.content_type,
        };
        builder.set_node_binding(elem_ref, binding)?;
    }

    // Set nil flag
    if info.is_nil {
        builder.set_nil(elem_ref);
    }

    // ── Second pass: non-xmlns attributes ─────────────────────────────
    for attr_result in e.attributes() {
        let attr = attr_result?;
        let key = attr.key.as_ref();

        if key == b"xmlns" || key.starts_with(b"xmlns:") {
            continue;
        }

        let (attr_prefix_bytes, attr_local_bytes) = split_prefix_local(key);
        let attr_local =
            std::str::from_utf8(attr_local_bytes).map_err(BufferDocumentError::Utf8)?;
        let attr_prefix_str =
            std::str::from_utf8(attr_prefix_bytes).map_err(BufferDocumentError::Utf8)?;

        // Resolve attr namespace
        let attr_ns_uri = if attr_prefix_bytes.is_empty() {
            String::new()
        } else {
            match prefix_map.get(attr_prefix_bytes) {
                Some(stack) if !stack.is_empty() => {
                    stack.last().unwrap().as_str().to_string()
                }
                _ => {
                    return Err(BufferDocumentError::UnboundPrefix(
                        attr_prefix_str.to_string(),
                    ))
                }
            }
        };

        let unescaped = attr.unescape_value()?;
        let attr_ref = builder.attribute(attr_local, &attr_ns_uri, attr_prefix_str, &unescaped)?;

        // Skip validation for xsi:* attributes (handled internally by the validator)
        if attr_ns_uri != XSI_NAMESPACE {
            let attr_info = runtime.validate_attribute(attr_local, &attr_ns_uri, &unescaped);

            if let Some(tk) = attr_info.schema_type {
                let binding = NodeSchemaBinding {
                    type_key: tk,
                    element_decl: None,
                    attribute_decl: attr_info.attribute_decl,
                    content_type: None,
                };
                builder.set_node_binding(attr_ref, binding)?;
            }
        }

        // xml:id detection (mirrors builder.rs:706)
        if attr_local == "id" && attr_ns_uri == XML_NAMESPACE {
            builder.register_xml_id(&unescaped, elem_ref)?;
        }
    }

    builder.end_of_attributes();
    runtime.validate_end_of_attributes();

    // ── Empty element: close immediately ──────────────────────────────
    if is_empty {
        if let Some(decls) = scope_decls.pop() {
            for prefix_key in &decls {
                if let Some(stack) = prefix_map.get_mut(prefix_key.as_ref()) {
                    stack.pop();
                }
            }
        }
        builder.end_element()?;
        runtime.validate_end_element();
    }

    Ok(elem_ref)
}

// ── Namespace context snapshot builder ────────────────────────────────

/// Build a [`NamespaceContextSnapshot`] from the current `prefix_map`.
fn build_ns_context(
    prefix_map: &HashMap<Box<[u8]>, Vec<String>>,
    schema_set: &SchemaSet,
) -> NamespaceContextSnapshot {
    let mut snapshot = NamespaceContextSnapshot::default();

    for (prefix_bytes, stack) in prefix_map {
        if let Some(uri) = stack.last() {
            if uri.is_empty() && prefix_bytes.is_empty() {
                continue; // default ns = "" → no binding
            }
            let uri_id = schema_set.name_table.add(uri);

            if prefix_bytes.is_empty() {
                snapshot.default_ns = Some(uri_id);
            } else if let Ok(prefix_str) = std::str::from_utf8(prefix_bytes) {
                // Skip well-known xml/xmlns prefixes (always in scope)
                if prefix_str != "xml" && prefix_str != "xmlns" {
                    let prefix_id = schema_set.name_table.add(prefix_str);
                    snapshot.bindings.push((prefix_id, uri_id));
                }
            }
        }
    }

    snapshot
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::TypeKey;
    use crate::navigator::{DomNavigator, TypedValue};
    use crate::pipeline::load_and_process_schema;
    use crate::validation::info::ContentType;

    fn load_schema(xsd: &str) -> SchemaSet {
        let mut schema_set = SchemaSet::new();
        load_and_process_schema(xsd.as_bytes(), "test.xsd", &mut schema_set, None)
            .expect("failed to load schema");
        schema_set
    }

    fn build_doc<'a>(
        xml: &str,
        arena: &'a Bump,
        schema_set: &'a SchemaSet,
    ) -> BufferDocument<'a> {
        build_typed_document(
            xml.as_bytes(),
            arena,
            schema_set,
            BufferDocumentOptions::default(),
        )
        .expect("failed to build typed document")
    }

    // ── Test 1: schema_type() / element_type_key() work ──────────────

    #[test]
    fn typed_document_has_schema_bindings() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root" type="xs:string"/>
            </xs:schema>"#,
        );
        let arena = Bump::new();
        let doc = build_doc("<root>hello</root>", &arena, &schema_set);

        let mut nav = doc.create_navigator();
        assert!(nav.move_to_first_child()); // root element
        assert!(nav.element_type_key().is_some());
        assert!(matches!(nav.element_type_key(), Some(TypeKey::Simple(_))));
    }

    // ── Test 2: typed_value() for xs:integer attribute ────────────────

    #[test]
    fn typed_value_integer_attribute() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root">
                    <xs:complexType>
                        <xs:attribute name="count" type="xs:integer"/>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"#,
        );
        let arena = Bump::new();
        let doc = build_doc(r#"<root count="42"/>"#, &arena, &schema_set);

        let mut nav = doc.create_navigator();
        assert!(nav.move_to_first_child()); // root element
        assert!(nav.move_to_first_attribute());
        assert!(nav.element_type_key().is_some());
        let tv = nav.typed_value();
        assert!(matches!(tv, TypedValue::Value(_)), "attribute should have typed value");
    }

    // ── Test 3: typed_value() for TextOnly element ────────────────────

    #[test]
    fn typed_value_text_only_element() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root" type="xs:integer"/>
            </xs:schema>"#,
        );
        let arena = Bump::new();
        let doc = build_doc("<root>123</root>", &arena, &schema_set);

        let mut nav = doc.create_navigator();
        assert!(nav.move_to_first_child()); // root element
        let tv = nav.typed_value();
        assert!(matches!(tv, TypedValue::Value(_)), "simple-typed element should have typed value");
    }

    // ── Test 4: typed_value() returns Absent for ElementOnly/Mixed ────

    #[test]
    fn typed_value_absent_for_element_only() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="child" type="xs:string"/>
                        </xs:sequence>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"#,
        );
        let arena = Bump::new();
        let doc = build_doc(
            "<root><child>hello</child></root>",
            &arena,
            &schema_set,
        );

        let mut nav = doc.create_navigator();
        assert!(nav.move_to_first_child()); // root element
        assert!(nav.element_type_key().is_some());
        assert_eq!(
            nav.typed_value(),
            TypedValue::Absent,
            "ElementOnly complex type should produce Absent"
        );
    }

    // ── Test 5: complex type with simpleContent → typed_value works ──

    #[test]
    fn typed_value_simple_content() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root">
                    <xs:complexType>
                        <xs:simpleContent>
                            <xs:extension base="xs:integer">
                                <xs:attribute name="unit" type="xs:string"/>
                            </xs:extension>
                        </xs:simpleContent>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"#,
        );
        let arena = Bump::new();
        let doc = build_doc(r#"<root unit="kg">42</root>"#, &arena, &schema_set);

        let mut nav = doc.create_navigator();
        assert!(nav.move_to_first_child()); // root element
        assert!(matches!(
            nav.element_type_key(),
            Some(TypeKey::Complex(_))
        ));
        let binding = nav.schema_binding().unwrap();
        assert_eq!(
            binding.content_type,
            Some(ContentType::TextOnly),
            "simpleContent should have TextOnly content type"
        );
        let tv = nav.typed_value();
        assert!(matches!(tv, TypedValue::Value(_)), "simpleContent element should have typed_value");
    }

    // ── Test 6: untyped document → binding_index=0, typed_value=Untyped

    #[test]
    fn untyped_document_no_bindings() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="other" type="xs:string"/>
            </xs:schema>"#,
        );
        let arena = Bump::new();
        // Element "root" not declared in schema — unresolved
        let doc = build_doc("<root>hello</root>", &arena, &schema_set);

        let mut nav = doc.create_navigator();
        assert!(nav.move_to_first_child()); // root element
        // Unknown element should still build, just no type binding
        assert_eq!(nav.typed_value(), TypedValue::Untyped);
    }

    // ── Test 7: xsi:type override ─────────────────────────────────────

    #[test]
    fn xsi_type_override() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root" type="xs:string"/>
            </xs:schema>"#,
        );
        let arena = Bump::new();
        let doc = build_doc(
            r#"<root xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"
                   xmlns:xs="http://www.w3.org/2001/XMLSchema"
                   xsi:type="xs:integer">42</root>"#,
            &arena,
            &schema_set,
        );

        let mut nav = doc.create_navigator();
        assert!(nav.move_to_first_child()); // root element
        assert!(nav.element_type_key().is_some());
        // The type should be resolved (either xs:integer or the declared type)
        let tv = nav.typed_value();
        assert!(matches!(tv, TypedValue::Value(_)), "xsi:type override should produce typed value");
    }

    // ── Test 8: xsi:nil → IS_NIL, typed_value() returns Nilled ───────

    #[test]
    fn xsi_nil_sets_flag() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root" type="xs:string" nillable="true"/>
            </xs:schema>"#,
        );
        let arena = Bump::new();
        let doc = build_doc(
            r#"<root xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"
                   xsi:nil="true"/>"#,
            &arena,
            &schema_set,
        );

        let mut nav = doc.create_navigator();
        assert!(nav.move_to_first_child()); // root element
        assert_eq!(
            nav.typed_value(),
            TypedValue::Nilled,
            "nil element should return Nilled"
        );
    }

    // ── Test 9: NameTable sharing ────────────────────────────────────

    #[test]
    fn name_table_sharing() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root" type="xs:string"/>
            </xs:schema>"#,
        );
        let arena = Bump::new();
        let doc = build_doc("<root>hello</root>", &arena, &schema_set);

        // Builder and validator share schema_set.name_table
        assert!(std::ptr::eq(doc.names(), &schema_set.name_table));
    }

    // ── Test 10: element_type_key() returns both Simple and Complex ──

    #[test]
    fn element_type_key_simple_and_complex() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="val" type="xs:integer"/>
                        </xs:sequence>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"#,
        );
        let arena = Bump::new();
        let doc = build_doc(
            "<root><val>10</val></root>",
            &arena,
            &schema_set,
        );

        let mut nav = doc.create_navigator();
        assert!(nav.move_to_first_child()); // root element
        assert!(
            matches!(nav.element_type_key(), Some(TypeKey::Complex(_))),
            "root should have Complex type key"
        );

        // Navigate to <val>
        assert!(nav.move_to_first_child());
        assert!(
            matches!(nav.element_type_key(), Some(TypeKey::Simple(_))),
            "val should have Simple type key"
        );
    }

    // ── Test 11: default value on element → typed_value uses default ─

    #[test]
    fn default_value_on_empty_element() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root" type="xs:integer" default="99"/>
            </xs:schema>"#,
        );
        let arena = Bump::new();
        // Empty element — no text content, should use default "99"
        let doc = build_doc("<root/>", &arena, &schema_set);

        let mut nav = doc.create_navigator();
        assert!(nav.move_to_first_child()); // root element
        let tv = nav.typed_value();
        assert!(
            matches!(tv, TypedValue::Value(_)),
            "empty element with default should produce typed value"
        );
    }

    // ── Test 12: source span tracking works ───────────────────────────

    #[test]
    fn source_span_tracking() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root" type="xs:string"/>
            </xs:schema>"#,
        );
        let arena = Bump::new();
        let doc = build_typed_document(
            "<root>hello</root>".as_bytes(),
            &arena,
            &schema_set,
            BufferDocumentOptions::full(), // track_source_locations = true
        )
        .unwrap();

        // Source spans should be populated
        assert!(
            !doc.source_spans.is_empty(),
            "source spans should be recorded with full() options"
        );
    }

    // ── Fragment mode tests ──────────────────────────────────────────────

    #[test]
    fn fragment_set_node_binding_typed_value() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root" type="xs:integer"/>
            </xs:schema>"#,
        );
        let arena = Bump::new();
        let doc = build_typed_document(
            "<root>42</root>".as_bytes(),
            &arena,
            &schema_set,
            BufferDocumentOptions::fragment(),
        )
        .unwrap();

        let mut nav = doc.create_navigator();
        assert!(nav.move_to_first_child()); // root element
        assert!(nav.element_type_key().is_some());
        let tv = nav.typed_value();
        assert!(
            matches!(tv, TypedValue::Value(_)),
            "fragment typed_value should resolve, got: {:?}",
            tv
        );
    }

    #[test]
    fn fragment_schema_set_propagation() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root" type="xs:string"/>
            </xs:schema>"#,
        );
        let arena = Bump::new();
        let doc = build_typed_document(
            "<root>hello</root>".as_bytes(),
            &arena,
            &schema_set,
            BufferDocumentOptions::fragment(),
        )
        .unwrap();

        assert!(
            doc.schema_set().is_some(),
            "fragment document should carry schema_set reference"
        );
    }
}
