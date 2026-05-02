//! Schema-aware document builder.
//!
//! [`build_typed_document`] constructs a [`BufferDocument`] while interleaving
//! validation calls so that each element and attribute node carries a
//! [`NodeSchemaBinding`] (type key, declaration keys, content type).

use std::io::BufRead;

use bumpalo::Bump;

use crate::namespace::table::XML_NAMESPACE;
use crate::parser::location::SourceSpan;
use crate::schema::SchemaSet;
use crate::validation::errors::ValidationError;
use crate::validation::info::{SchemaInfo, ValidationFlags};
use crate::validation::quick_xml_driver::{
    drive_quick_xml_with, AttributeView, DriveWithError, ElementStartView, EndElementInfo,
    EndOfAttributesView, TextKind, ValidationEventHandler,
};
use crate::validation::validator::{ValidationSink, ValidationWarning};
use crate::validation::SchemaValidator;

use super::buffer::BufferDocument;
use super::builder::BufferDocumentBuilder;
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

// ── Handler ───────────────────────────────────────────────────────────

/// Handler that mirrors validator events into a [`BufferDocumentBuilder`].
///
/// Holds the per-element bookkeeping the design doc separates out: the
/// element-ref stack (so text/end events can correlate to the open element),
/// the single-slot scratch for the current attribute being processed, the
/// per-element queue of CTA-deferred attribute refs, and the source-span
/// tracking pieces.
struct TypedBuilderHandler<'b, 'a> {
    builder: &'b mut BufferDocumentBuilder<'a>,
    track: bool,
    pending_start_offset: Option<usize>,
    /// Currently open elements' refs. Pushed at `before_element`, popped at
    /// `after_end_element`.
    elem_ref_stack: Vec<u32>,
    /// (elem_ref, start_byte) for each currently open element when
    /// `track` is on. Popped at `on_element_end_offset` to set the span.
    pending_spans: Vec<(u32, usize)>,
    /// Single-slot scratch for the attribute currently between
    /// `before_attribute` and `after_attribute`.
    current_attr_ref: Option<u32>,
    /// Per-element queue of attribute refs whose binding is deferred until
    /// CTA reselection. Drained in `after_end_of_attributes`.
    #[cfg(feature = "xsd11")]
    deferred_attr_refs: Vec<u32>,
    /// Element-decl key for the currently open element (used so we can
    /// preserve `element_decl` when refreshing the binding after CTA).
    element_decl_stack: Vec<Option<crate::ids::ElementKey>>,
}

impl<'b, 'a> TypedBuilderHandler<'b, 'a> {
    fn new(builder: &'b mut BufferDocumentBuilder<'a>) -> Self {
        let track = builder.track_source_locations();
        Self {
            builder,
            track,
            pending_start_offset: None,
            elem_ref_stack: Vec::new(),
            pending_spans: Vec::new(),
            current_attr_ref: None,
            #[cfg(feature = "xsd11")]
            deferred_attr_refs: Vec::new(),
            element_decl_stack: Vec::new(),
        }
    }
}

impl<'b, 'a> ValidationEventHandler for TypedBuilderHandler<'b, 'a> {
    type Error = BufferDocumentError;

    fn on_element_start_offset(&mut self, byte_pos: usize) -> Result<(), Self::Error> {
        if self.track {
            self.pending_start_offset = Some(byte_pos);
        }
        Ok(())
    }

    fn before_element(&mut self, view: ElementStartView<'_>) -> Result<(), Self::Error> {
        let elem_ref = self.builder.start_element(
            view.local_name,
            view.namespace_uri,
            view.prefix,
            view.namespace_decls,
        )?;
        self.elem_ref_stack.push(elem_ref);
        if self.track {
            if let Some(start) = self.pending_start_offset.take() {
                self.pending_spans.push((elem_ref, start));
            }
        }
        Ok(())
    }

    fn after_element(
        &mut self,
        _view: ElementStartView<'_>,
        info: &SchemaInfo,
    ) -> Result<(), Self::Error> {
        let elem_ref = *self
            .elem_ref_stack
            .last()
            .expect("after_element with empty stack");
        self.element_decl_stack.push(info.element_decl);
        if let Some(tk) = info.schema_type {
            let binding = NodeSchemaBinding {
                type_key: tk,
                element_decl: info.element_decl,
                attribute_decl: None,
                content_type: info.content_type,
            };
            self.builder.set_node_binding(elem_ref, binding)?;
        }
        if info.is_nil {
            self.builder.set_nil(elem_ref);
        }
        Ok(())
    }

    fn before_attribute(&mut self, view: AttributeView<'_>) -> Result<(), Self::Error> {
        let attr_ref = self.builder.attribute(
            view.local_name,
            view.namespace_uri,
            view.prefix,
            view.value,
        )?;
        self.current_attr_ref = Some(attr_ref);
        Ok(())
    }

    fn after_attribute(
        &mut self,
        view: AttributeView<'_>,
        info: &SchemaInfo,
    ) -> Result<(), Self::Error> {
        let attr_ref = self
            .current_attr_ref
            .take()
            .expect("after_attribute without matching before_attribute");
        if let Some(tk) = info.schema_type {
            let binding = NodeSchemaBinding {
                type_key: tk,
                element_decl: None,
                attribute_decl: info.attribute_decl,
                content_type: None,
            };
            self.builder.set_node_binding(attr_ref, binding)?;
        }
        if view.local_name == "id" && view.namespace_uri == XML_NAMESPACE {
            let owner_elem = *self
                .elem_ref_stack
                .last()
                .expect("xml:id without an open element");
            self.builder.register_xml_id(view.value, owner_elem)?;
        }
        #[cfg(feature = "xsd11")]
        if info.deferred_by_cta {
            self.deferred_attr_refs.push(attr_ref);
        }
        Ok(())
    }

    fn after_end_of_attributes(
        &mut self,
        view: EndOfAttributesView<'_>,
    ) -> Result<(), Self::Error> {
        self.builder.end_of_attributes();
        let elem_ref = *self
            .elem_ref_stack
            .last()
            .expect("after_end_of_attributes without an open element");
        let element_decl = *self
            .element_decl_stack
            .last()
            .expect("after_end_of_attributes without an open element");

        if let Some(tk) = view.info.schema_type {
            let binding = NodeSchemaBinding {
                type_key: tk,
                element_decl,
                attribute_decl: None,
                content_type: view.info.content_type,
            };
            self.builder.set_node_binding(elem_ref, binding)?;
        }

        #[cfg(feature = "xsd11")]
        {
            let deferred_refs = std::mem::take(&mut self.deferred_attr_refs);
            if deferred_refs.len() != view.deferred_attribute_results.len() {
                return Err(BufferDocumentError::InternalError(
                    "deferred attribute count mismatch".into(),
                ));
            }
            for (attr_ref, attr_info) in deferred_refs
                .iter()
                .zip(view.deferred_attribute_results.iter())
            {
                if let Some(tk) = attr_info.schema_type {
                    let binding = NodeSchemaBinding {
                        type_key: tk,
                        element_decl: None,
                        attribute_decl: attr_info.attribute_decl,
                        content_type: None,
                    };
                    self.builder.set_node_binding(*attr_ref, binding)?;
                }
            }
        }

        Ok(())
    }

    fn after_end_element(
        &mut self,
        _info: &EndElementInfo,
        _depth: usize,
    ) -> Result<(), Self::Error> {
        self.builder.end_element()?;
        self.elem_ref_stack.pop();
        self.element_decl_stack.pop();
        Ok(())
    }

    fn on_element_end_offset(&mut self, byte_pos: usize) -> Result<(), Self::Error> {
        if self.track {
            if let Some((elem_ref, start)) = self.pending_spans.pop() {
                self.builder
                    .set_source_span(elem_ref, SourceSpan::new(start, byte_pos));
            }
        }
        Ok(())
    }

    fn on_text(&mut self, _kind: TextKind, text: &str) -> Result<(), Self::Error> {
        if !self.elem_ref_stack.is_empty() {
            self.builder.text(text);
        }
        Ok(())
    }

    fn on_comment(&mut self, text: &str) -> Result<(), Self::Error> {
        self.builder.comment(text)?;
        Ok(())
    }

    fn on_processing_instruction(
        &mut self,
        target: &str,
        data: &str,
    ) -> Result<(), Self::Error> {
        self.builder.processing_instruction(target, data)?;
        Ok(())
    }
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
    let mut builder =
        BufferDocumentBuilder::new(arena, &schema_set.name_table, Some(schema_set), options)?;

    let validator = SchemaValidator::new(schema_set, ValidationFlags::default());
    let mut runtime = validator.start_run(SilentValidationSink);

    {
        let mut handler = TypedBuilderHandler::new(&mut builder);
        drive_quick_xml_with(reader, &mut runtime, schema_set, &mut handler).map_err(|e| {
            match e {
                DriveWithError::Parse(e) => BufferDocumentError::Parse(e),
                DriveWithError::Utf8(e) => BufferDocumentError::Utf8(e),
                DriveWithError::UnboundPrefix(p) => BufferDocumentError::UnboundPrefix(p),
                DriveWithError::UnexpectedEof { depth } => {
                    BufferDocumentError::InternalError(format!(
                        "unexpected EOF: {} element(s) still open",
                        depth
                    ))
                }
                DriveWithError::Hook(e) => e,
            }
        })?;
    }

    let _ = runtime.end_validation();
    builder.finalize()
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
        let mut schema_set = SchemaSet::xsd11();
        load_and_process_schema(xsd.as_bytes(), "test.xsd", &mut schema_set, None)
            .expect("failed to load schema");
        schema_set
    }

    fn build_doc<'a>(xml: &str, arena: &'a Bump, schema_set: &'a SchemaSet) -> BufferDocument<'a> {
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
        assert!(
            matches!(tv, TypedValue::Value(_)),
            "attribute should have typed value"
        );
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
        assert!(
            matches!(tv, TypedValue::Value(_)),
            "simple-typed element should have typed value"
        );
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
        let doc = build_doc("<root><child>hello</child></root>", &arena, &schema_set);

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
        assert!(matches!(nav.element_type_key(), Some(TypeKey::Complex(_))));
        let binding = nav.schema_binding().unwrap();
        assert_eq!(
            binding.content_type,
            Some(ContentType::TextOnly),
            "simpleContent should have TextOnly content type"
        );
        let tv = nav.typed_value();
        assert!(
            matches!(tv, TypedValue::Value(_)),
            "simpleContent element should have typed_value"
        );
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
        assert!(
            matches!(tv, TypedValue::Value(_)),
            "xsi:type override should produce typed value"
        );
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
        let doc = build_doc("<root><val>10</val></root>", &arena, &schema_set);

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

    // ── Test 11b: fixed value on element → typed_value uses fixed ──

    #[test]
    fn fixed_value_on_empty_element() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root" type="xs:integer" fixed="42"/>
            </xs:schema>"#,
        );
        let arena = Bump::new();
        // Empty element — no text content, should use fixed "42"
        let doc = build_doc("<root/>", &arena, &schema_set);

        let mut nav = doc.create_navigator();
        assert!(nav.move_to_first_child()); // root element
        let tv = nav.typed_value();
        assert!(
            matches!(tv, TypedValue::Value(_)),
            "empty element with fixed value should produce typed value, got: {:?}",
            tv
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

    // ── Test 13: comments and PIs preserved in typed document ────────

    #[test]
    fn comments_and_pis_preserved() {
        use crate::document::node::NodeType;

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
        let xml = "<root><!-- before --><?p1 d1?><child>hi</child><!-- after --></root>";
        let doc = build_typed_document(
            xml.as_bytes(),
            &arena,
            &schema_set,
            BufferDocumentOptions::default(),
        )
        .unwrap();

        let mut comment_count = 0usize;
        let mut pi_count = 0usize;
        for i in 0..doc.nodes.len() {
            match doc.nodes.get(i).node_type() {
                NodeType::Comment => comment_count += 1,
                NodeType::ProcessingInstruction => pi_count += 1,
                _ => {}
            }
        }
        assert_eq!(comment_count, 2, "two comments should be preserved");
        assert_eq!(pi_count, 1, "one PI should be preserved");
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

    // ── CTA deferred attribute binding tests (XSD 1.1) ──────────────

    #[cfg(feature = "xsd11")]
    mod cta_deferred_bindings {
        use super::*;

        /// Schema where CTA switches type based on @kind, and the selected type
        /// declares an attribute with a specific type.
        const CTA_ATTR_SCHEMA: &str = r#"
            <xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:complexType name="intType">
                    <xs:attribute name="kind" type="xs:string"/>
                    <xs:attribute name="val" type="xs:integer"/>
                </xs:complexType>
                <xs:complexType name="strType">
                    <xs:attribute name="kind" type="xs:string"/>
                    <xs:attribute name="val" type="xs:string"/>
                </xs:complexType>
                <xs:element name="data">
                    <xs:complexType>
                        <xs:attribute name="kind" type="xs:string"/>
                        <xs:attribute name="val" type="xs:string"/>
                    </xs:complexType>
                    <xs:alternative test="@kind='int'" type="intType"/>
                    <xs:alternative test="@kind='str'" type="strType"/>
                </xs:element>
            </xs:schema>"#;

        #[test]
        fn cta_selected_type_binds_attribute() {
            let schema_set = load_schema(CTA_ATTR_SCHEMA);
            let arena = Bump::new();
            let doc = build_doc(r#"<data kind="int" val="42"/>"#, &arena, &schema_set);

            let mut nav = doc.create_navigator();
            assert!(nav.move_to_first_child()); // <data>
                                                // Element should have intType binding
            assert!(nav.element_type_key().is_some());

            // Navigate to 'val' attribute — should have xs:integer type from intType
            assert!(nav.move_to_first_attribute());
            // Attributes are in document order: kind, val
            // Move to second attribute 'val'
            if nav.local_name() == "kind" {
                assert!(nav.move_to_next_attribute());
            }
            assert_eq!(nav.local_name(), "val");
            assert!(
                nav.element_type_key().is_some(),
                "deferred CTA attribute 'val' should have a type binding"
            );
        }

        #[test]
        fn cta_default_type_binds_attribute() {
            let schema_set = load_schema(CTA_ATTR_SCHEMA);
            let arena = Bump::new();
            // kind='str' selects strType — val should be xs:string
            let doc = build_doc(r#"<data kind="str" val="hello"/>"#, &arena, &schema_set);

            let mut nav = doc.create_navigator();
            assert!(nav.move_to_first_child()); // <data>
            assert!(nav.element_type_key().is_some());

            assert!(nav.move_to_first_attribute());
            if nav.local_name() == "kind" {
                assert!(nav.move_to_next_attribute());
            }
            assert_eq!(nav.local_name(), "val");
            assert!(
                nav.element_type_key().is_some(),
                "deferred CTA attribute 'val' should have a type binding for strType"
            );
        }

        #[test]
        fn cta_multiple_deferred_attributes_ordering() {
            // Schema where CTA type has multiple typed attributes
            let schema = r#"
                <xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                    <xs:complexType name="fullType">
                        <xs:attribute name="kind" type="xs:string"/>
                        <xs:attribute name="x" type="xs:integer"/>
                        <xs:attribute name="y" type="xs:integer"/>
                    </xs:complexType>
                    <xs:element name="point">
                        <xs:complexType>
                            <xs:attribute name="kind" type="xs:string"/>
                            <xs:attribute name="x" type="xs:string"/>
                            <xs:attribute name="y" type="xs:string"/>
                        </xs:complexType>
                        <xs:alternative test="@kind='full'" type="fullType"/>
                    </xs:element>
                </xs:schema>"#;

            let schema_set = load_schema(schema);
            let arena = Bump::new();
            let doc = build_doc(r#"<point kind="full" x="10" y="20"/>"#, &arena, &schema_set);

            let mut nav = doc.create_navigator();
            assert!(nav.move_to_first_child()); // <point>

            // Check all attributes have bindings
            assert!(nav.move_to_first_attribute());
            let mut bound_count = 0;
            loop {
                if nav.element_type_key().is_some() {
                    bound_count += 1;
                }
                if !nav.move_to_next_attribute() {
                    break;
                }
            }
            assert!(
                bound_count >= 3,
                "all 3 attributes (kind, x, y) should have type bindings, got {}",
                bound_count
            );
        }

        #[test]
        fn cta_nested_elements_no_cross_leakage() {
            // Schema with CTA on two nested elements
            let schema = r#"
                <xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                    <xs:complexType name="outerAlt">
                        <xs:sequence>
                            <xs:element ref="inner"/>
                        </xs:sequence>
                        <xs:attribute name="kind" type="xs:string"/>
                        <xs:attribute name="outerAttr" type="xs:integer"/>
                    </xs:complexType>
                    <xs:complexType name="innerAlt">
                        <xs:attribute name="kind" type="xs:string"/>
                        <xs:attribute name="innerAttr" type="xs:integer"/>
                    </xs:complexType>
                    <xs:element name="inner">
                        <xs:complexType>
                            <xs:attribute name="kind" type="xs:string"/>
                            <xs:attribute name="innerAttr" type="xs:string"/>
                        </xs:complexType>
                        <xs:alternative test="@kind='alt'" type="innerAlt"/>
                    </xs:element>
                    <xs:element name="outer">
                        <xs:complexType>
                            <xs:sequence>
                                <xs:element ref="inner"/>
                            </xs:sequence>
                            <xs:attribute name="kind" type="xs:string"/>
                            <xs:attribute name="outerAttr" type="xs:string"/>
                        </xs:complexType>
                        <xs:alternative test="@kind='alt'" type="outerAlt"/>
                    </xs:element>
                </xs:schema>"#;

            let schema_set = load_schema(schema);
            let arena = Bump::new();
            let doc = build_doc(
                r#"<outer kind="alt" outerAttr="99"><inner kind="alt" innerAttr="42"/></outer>"#,
                &arena,
                &schema_set,
            );

            let mut nav = doc.create_navigator();
            assert!(nav.move_to_first_child()); // <outer>
            assert!(
                nav.element_type_key().is_some(),
                "outer should have type binding"
            );

            // Check outerAttr has binding
            assert!(nav.move_to_first_attribute());
            let mut found_outer_attr = false;
            loop {
                if nav.local_name() == "outerAttr" {
                    assert!(
                        nav.element_type_key().is_some(),
                        "outerAttr should have type binding"
                    );
                    found_outer_attr = true;
                }
                if !nav.move_to_next_attribute() {
                    break;
                }
            }
            assert!(found_outer_attr, "should find outerAttr");

            // Navigate to <inner>
            nav.move_to_parent();
            assert!(nav.move_to_first_child()); // <inner>
            assert!(
                nav.element_type_key().is_some(),
                "inner should have type binding"
            );

            // Check innerAttr has binding
            assert!(nav.move_to_first_attribute());
            let mut found_inner_attr = false;
            loop {
                if nav.local_name() == "innerAttr" {
                    assert!(
                        nav.element_type_key().is_some(),
                        "innerAttr should have type binding"
                    );
                    found_inner_attr = true;
                }
                if !nav.move_to_next_attribute() {
                    break;
                }
            }
            assert!(found_inner_attr, "should find innerAttr");
        }

        #[test]
        fn cta_simple_type_selection_no_panic() {
            // CTA selects a simple type — the element then has no attribute
            // declarations. Deferred attributes should produce empty results
            // (no bindings) rather than triggering an InternalError.
            let schema = r#"
                <xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                    <xs:element name="data">
                        <xs:complexType>
                            <xs:attribute name="kind" type="xs:string"/>
                            <xs:attribute name="val" type="xs:string"/>
                        </xs:complexType>
                        <xs:alternative test="@kind='simple'" type="xs:string"/>
                    </xs:element>
                </xs:schema>"#;

            let schema_set = load_schema(schema);
            let arena = Bump::new();
            // kind='simple' triggers CTA → xs:string (a simple type).
            // Both attributes were deferred; revalidation must not panic.
            let result = build_typed_document(
                r#"<data kind="simple" val="hello"/>"#.as_bytes(),
                &arena,
                &schema_set,
                BufferDocumentOptions::default(),
            );
            assert!(
                result.is_ok(),
                "CTA selecting simple type should not cause InternalError, got: {:?}",
                result.err()
            );
        }
    }
}
