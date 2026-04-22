//! Main XSD parser event loop
//!
//! This module provides the main parser that processes XSD documents using
//! a frame-based state machine. Each XSD element type is handled by a
//! corresponding frame that validates structure and collects content.
//!
//! # Architecture
//!
//! The parser uses:
//! - `TrackedReader` for XML parsing with byte position tracking
//! - `NamespaceContext` for scoped namespace management
//! - Frame stack for nested element handling
//! - `create_frame` factory for frame instantiation
//!
//! # Example
//!
//! ```
//! use xsd_schema::parser::parse::parse_schema;
//! use xsd_schema::SchemaSet;
//!
//! let mut schema_set = SchemaSet::new();
//! let xsd = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
//!     <xs:element name="root" type="xs:string"/>
//! </xs:schema>"#;
//!
//! let doc_id = parse_schema(xsd.as_bytes(), "test.xsd", &mut schema_set)
//!     .expect("parse failed");
//! assert_eq!(doc_id, 0);
//! ```

use std::collections::HashSet;

use quick_xml::events::Event;

use crate::error::{SchemaError, SchemaResult};
use crate::ids::{DocumentId, NameId};
use crate::namespace::{NamespaceContext, NameTable, XS_NAMESPACE, is_ncname};
use crate::parser::attrs::{parse_attributes, categorize_attributes, AttributeMap};
use crate::parser::assemble::assemble_schema;
use crate::parser::frames::{
    create_frame, create_frame_recovering, xsd_names, Frame, FrameResult, SchemaFrameResult,
    SkipFrame,
};
use crate::parser::location::{SourceLocation, SourceMap, SourceRef, SourceSpan};
use crate::parser::reader::{split_qname, TrackedReader};
use crate::parser::structure::{
    ValidationContext, validate_xsd_version_element, validate_xsd_version_attribute,
    validate_element_structure, validate_attribute_structure,
    validate_simple_type_structure, validate_complex_type_structure,
    validate_extension_structure,
    validate_key_unique_structure, validate_keyref_structure,
    validate_group_structure, validate_attribute_group_structure,
    validate_notation_structure, validate_include_structure, validate_redefine_structure,
};
use crate::schema::annotation::ForeignAttribute;
use crate::schema::model::XsdVersion;
use crate::SchemaSet;

/// Parser configuration options
#[derive(Debug, Clone)]
pub struct ParserConfig {
    /// Whether to recover from errors and continue parsing
    pub error_recovery: bool,
    /// Whether to collect foreign attributes
    pub collect_foreign_attributes: bool,
    /// Maximum nesting depth (0 = unlimited)
    pub max_depth: usize,
    /// XSD version mode (1.0 or 1.1).
    /// Derived from `SchemaSet.xsd_version` in `parse_schema_with_config`.
    pub(crate) xsd_version: XsdVersion,
}

impl Default for ParserConfig {
    fn default() -> Self {
        Self {
            error_recovery: true,
            collect_foreign_attributes: true,
            max_depth: 0,
            xsd_version: XsdVersion::V1_0,
        }
    }
}

/// Parser state during schema parsing
struct ParserState<'a, 'b, 'c> {
    /// Namespace context for prefix resolution
    ns_context: NamespaceContext<'a>,
    /// Stack of parser frames
    frame_stack: Vec<Box<dyn Frame>>,
    /// Current document ID
    doc_id: DocumentId,
    /// Errors collected during parsing
    errors: Vec<SchemaError>,
    /// Parser configuration
    config: &'b ParserConfig,
    /// XSD namespace ID (cached)
    xsd_ns_id: Option<NameId>,
    /// Source map for location resolution
    source_map: &'c SourceMap,
    /// Completed root schema result (set when root frame finishes)
    root_schema: Option<SchemaFrameResult>,
    /// Collected xs:ID values for document-level uniqueness checking
    id_values: HashSet<String>,
    /// True when the root xs:schema element has a vc: version condition that
    /// excludes this document; all children of xs:schema are then skipped.
    vc_schema_excluded: bool,
}

impl<'a, 'b, 'c> ParserState<'a, 'b, 'c> {
    fn new(
        name_table: &'a mut NameTable,
        doc_id: DocumentId,
        config: &'b ParserConfig,
        source_map: &'c SourceMap,
    ) -> Self {
        let ns_context = NamespaceContext::new(name_table);
        Self {
            ns_context,
            frame_stack: Vec::new(),
            doc_id,
            errors: Vec::new(),
            config,
            xsd_ns_id: None,
            source_map,
            root_schema: None,
            id_values: HashSet::new(),
            vc_schema_excluded: false,
        }
    }

    /// Get the XSD namespace ID, caching it for efficiency
    fn get_xsd_ns_id(&mut self) -> Option<NameId> {
        if self.xsd_ns_id.is_none() {
            self.xsd_ns_id = self.ns_context.name_table().get(XS_NAMESPACE);
        }
        self.xsd_ns_id
    }

    /// Check if a namespace URI is the XSD namespace
    fn is_in_xsd_namespace(&mut self, namespace: Option<NameId>) -> bool {
        match (namespace, self.get_xsd_ns_id()) {
            (Some(ns), Some(xsd_ns)) => ns == xsd_ns,
            (None, _) => false, // Unqualified elements are not XSD elements
            _ => false,
        }
    }

    /// Push a namespace scope
    fn push_scope(&mut self) {
        self.ns_context.push_scope();
    }

    /// Pop a namespace scope
    fn pop_scope(&mut self) {
        self.ns_context.pop_scope();
    }

    /// Get current frame
    fn current_frame(&self) -> Option<&dyn Frame> {
        self.frame_stack.last().map(|b| b.as_ref())
    }

    /// Get current frame mutably
    fn current_frame_mut(&mut self) -> Option<&mut Box<dyn Frame>> {
        self.frame_stack.last_mut()
    }

    /// Add an error
    fn add_error(&mut self, error: SchemaError) {
        self.errors.push(error);
    }

    /// In error-recovery mode, collect the error and continue; otherwise fail.
    fn recover_or_fail(&mut self, error: SchemaError) -> SchemaResult<()> {
        if self.config.error_recovery {
            self.add_error(error);
            Ok(())
        } else {
            Err(error)
        }
    }

    /// Create a source reference for the given span
    fn source_ref(&self, span: SourceSpan) -> SourceRef {
        SourceRef::new(self.doc_id, span)
    }

    /// Create validation context for structural checks.
    /// Elements are top-level if their parent frame reports `children_are_top_level`
    /// (schema, redefine, and override frames).
    fn validation_context(&self, source: Option<SourceRef>) -> ValidationContext {
        let is_top_level = self
            .frame_stack
            .last()
            .map(|f| f.children_are_top_level())
            .unwrap_or(false);
        ValidationContext {
            xsd_version: self.config.xsd_version,
            is_top_level,
            source,
        }
    }
}

/// Parse an XSD schema document
///
/// This is the main entry point for parsing XSD documents.
///
/// # Arguments
///
/// * `xml` - Raw XML bytes of the schema document
/// * `base_uri` - Base URI for this document (for error messages and include resolution)
/// * `schema_set` - Schema set to add the parsed document to
///
/// # Returns
///
/// The document ID of the parsed schema, or an error if parsing failed.
pub fn parse_schema(
    xml: &[u8],
    base_uri: &str,
    schema_set: &mut SchemaSet,
) -> SchemaResult<DocumentId> {
    let config = ParserConfig::default();
    parse_schema_with_config(xml, base_uri, schema_set, &config)
}

/// Parse an XSD schema document with custom configuration.
///
/// The XSD version is always derived from `schema_set.xsd_version`,
/// regardless of what `config.xsd_version` contains.
pub fn parse_schema_with_config(
    xml: &[u8],
    base_uri: &str,
    schema_set: &mut SchemaSet,
    config: &ParserConfig,
) -> SchemaResult<DocumentId> {
    parse_schema_with_chameleon(xml, base_uri, schema_set, config, None)
}

/// Parse an XSD schema document with chameleon namespace support.
///
/// If `chameleon_namespace` is `Some` and the parsed document has no
/// `targetNamespace`, the chameleon namespace is adopted per §4.2.3
/// clause 2.3 (chameleon include pre-processing).
pub fn parse_schema_with_chameleon(
    xml: &[u8],
    base_uri: &str,
    schema_set: &mut SchemaSet,
    config: &ParserConfig,
    chameleon_namespace: Option<NameId>,
) -> SchemaResult<DocumentId> {
    // Override parser version from the single source of truth
    let mut config = config.clone();
    config.xsd_version = schema_set.xsd_version;

    // Create source map - keep local reference for location resolution during parsing
    let source_text = String::from_utf8_lossy(xml).into_owned();
    let source_map = SourceMap::new(base_uri.to_string(), source_text);

    // Pre-assign document ID (will be used when we add the source map later)
    let doc_id = schema_set.source_maps.len() as DocumentId;

    // Create parser state with reference to source_map
    let mut state = ParserState::new(&mut schema_set.name_table, doc_id, &config, &source_map);

    // Create XML reader
    let mut reader = TrackedReader::from_bytes(xml);
    let mut buf = Vec::new();

    // Track if we've seen the root schema element
    let mut seen_root = false;

    // Main event loop
    loop {
        buf.clear();
        let tracked_event = reader.read_event(&mut buf)?;
        let span = tracked_event.span;

        match tracked_event.event {
            Event::Start(ref e) => {
                handle_start_element(&mut state, e, span, &mut seen_root)?;
            }
            Event::Empty(ref e) => {
                // Empty elements are treated as Start + End
                handle_start_element(&mut state, e, span, &mut seen_root)?;
                handle_end_element(&mut state, span)?;
            }
            Event::End(_) => {
                handle_end_element(&mut state, span)?;
            }
            Event::Text(ref e) => {
                handle_text(&mut state, e, span)?;
            }
            Event::CData(ref e) => {
                handle_cdata(&mut state, e, span)?;
            }
            Event::Comment(_) => {
                // Ignore comments
            }
            Event::PI(_) => {
                // Ignore processing instructions
            }
            Event::Decl(_) => {
                // Ignore XML declaration
            }
            Event::DocType(_) => {
                // Ignore DOCTYPE
            }
            Event::Eof => break,
        }
    }

    // Check for incomplete parsing
    if !state.frame_stack.is_empty() {
        return Err(SchemaError::structural(
            "src-resolve",
            "Schema document ended with unclosed elements",
            None,
        ));
    }

    // Store any collected parsing errors on the schema set so they can be
    // surfaced later (e.g. when process_loaded_schemas runs).
    let parsing_errors = std::mem::take(&mut state.errors);

    let mut root_schema = state.root_schema.take().ok_or_else(|| {
        SchemaError::internal("No schema result produced during parsing")
    })?;
    drop(state);

    schema_set.parsing_errors.extend(parsing_errors);

    // Record the declared targetNamespace before chameleon adoption.
    let declared_target_namespace = root_schema.target_namespace;

    // Chameleon pre-processing (§4.2.3 clause 2.3): if the parsed document
    // has no targetNamespace and the includer specifies one, adopt it.
    if root_schema.target_namespace.is_none() {
        if let Some(ns) = chameleon_namespace {
            root_schema.target_namespace = Some(ns);
        }
    }

    // Add the source map to storage now that parsing is complete
    // Note: We ensured doc_id matches the position where this will be added
    let added_id = schema_set.source_maps.add(source_map);
    debug_assert_eq!(doc_id, added_id, "Document ID mismatch");

    let mut doc = assemble_schema(schema_set, doc_id, base_uri, root_schema)?;
    doc.declared_target_namespace = declared_target_namespace;
    schema_set.documents.push(doc);

    Ok(doc_id)
}

/// Validate element-specific structural constraints
///
/// Dispatches to the appropriate validation function based on element name.
/// This enforces constraints like name/ref exclusivity, required attributes, etc.
fn validate_element_attributes(
    local_name: &str,
    attrs: &AttributeMap,
    name_table: &NameTable,
    ctx: &ValidationContext,
) -> SchemaResult<()> {
    match local_name {
        xsd_names::ELEMENT => validate_element_structure(attrs, name_table, ctx),
        xsd_names::ATTRIBUTE => validate_attribute_structure(attrs, name_table, ctx),
        xsd_names::SIMPLE_TYPE => validate_simple_type_structure(attrs, name_table, ctx),
        xsd_names::COMPLEX_TYPE => validate_complex_type_structure(attrs, name_table, ctx),
        xsd_names::GROUP => validate_group_structure(attrs, name_table, ctx),
        xsd_names::ATTRIBUTE_GROUP => validate_attribute_group_structure(attrs, name_table, ctx),
        xsd_names::NOTATION => validate_notation_structure(attrs, name_table, ctx),
        xsd_names::INCLUDE => validate_include_structure(attrs, name_table),
        xsd_names::REDEFINE => validate_redefine_structure(attrs, name_table),
        xsd_names::KEY | xsd_names::UNIQUE => validate_key_unique_structure(attrs, name_table),
        xsd_names::KEYREF => validate_keyref_structure(attrs, name_table),
        xsd_names::EXTENSION => validate_extension_structure(attrs, name_table),
        // Note: restriction and list/union validation requires child info (has_inline_type),
        // so they're validated at frame finish time, not here
        _ => Ok(()),
    }
}

fn intern_attribute_values(
    local_name: &str,
    attrs: &AttributeMap,
    name_table: &mut NameTable,
) {
    fn add_if_present(attrs: &AttributeMap, name_table: &mut NameTable, attr: &str) {
        if let Some(value) = attrs.get_value_by_name(name_table, attr) {
            name_table.add(value);
        }
    }

    match local_name {
        xsd_names::SCHEMA => {
            add_if_present(attrs, name_table, "targetNamespace");
            add_if_present(attrs, name_table, "defaultAttributes");
        }
        xsd_names::SIMPLE_TYPE | xsd_names::COMPLEX_TYPE => {
            add_if_present(attrs, name_table, "name");
        }
        xsd_names::ELEMENT | xsd_names::ATTRIBUTE => {
            add_if_present(attrs, name_table, "name");
            add_if_present(attrs, name_table, "targetNamespace");
        }
        xsd_names::GROUP | xsd_names::ATTRIBUTE_GROUP | xsd_names::NOTATION => {
            add_if_present(attrs, name_table, "name");
        }
        xsd_names::KEY | xsd_names::KEYREF | xsd_names::UNIQUE => {
            add_if_present(attrs, name_table, "name");
        }
        _ => {}
    }
}

/// Handle a start element event
fn handle_start_element(
    state: &mut ParserState,
    element: &quick_xml::events::BytesStart,
    span: SourceSpan,
    seen_root: &mut bool,
) -> SchemaResult<()> {
    // Push namespace scope for this element
    state.push_scope();

    // Parse element name
    let name = element.name();
    let name_bytes = name.as_ref();
    let (local_name_bytes, prefix_bytes) = split_qname(name_bytes);

    let local_name = std::str::from_utf8(local_name_bytes).map_err(|e| {
        SchemaError::xml(
            format!("Invalid UTF-8 in element name: {}", e),
            Some(state.source_ref(span).to_location(state.source_map)),
        )
    })?;

    // First, process namespace declarations from attributes
    for attr_result in element.attributes() {
        let attr = attr_result.map_err(|e| {
            SchemaError::xml(format!("Attribute error: {}", e), None)
        })?;

        let attr_name = attr.key.as_ref();
        let attr_value = attr.unescape_value().map_err(|e| {
            SchemaError::xml(format!("Attribute value error: {}", e), None)
        })?;

        // Check for xmlns declarations
        if attr_name == b"xmlns" {
            // Default namespace
            state.ns_context.add_namespace("", &attr_value);
        } else if attr_name.starts_with(b"xmlns:") {
            // Prefixed namespace
            let prefix = std::str::from_utf8(&attr_name[6..]).unwrap_or("");
            state.ns_context.add_namespace(prefix, &attr_value);
        }
    }

    // Now resolve the element's namespace
    let element_ns = if let Some(prefix) = prefix_bytes {
        let prefix_str = std::str::from_utf8(prefix).unwrap_or("");
        state.ns_context.lookup_namespace(prefix_str)
    } else {
        state.ns_context.default_namespace()
    };

    // Check if this is the root schema element
    if !*seen_root {
        *seen_root = true;

        // Must be xs:schema
        if local_name != xsd_names::SCHEMA || !state.is_in_xsd_namespace(element_ns) {
            return Err(SchemaError::structural(
                "sch-props-correct",
                format!(
                    "Root element must be xs:schema, found '{}'",
                    local_name
                ),
                None,
            ));
        }
    }

    // Parse and categorize attributes
    let source_ref = Some(state.source_ref(span));
    let parsed_attrs = parse_attributes(
        element.attributes(),
        &mut state.ns_context,
        source_ref.clone(),
    )?;
    let (xsd_attrs, foreign_attrs) = categorize_attributes(parsed_attrs, state.ns_context.name_table());
    let attr_map = AttributeMap::new(xsd_attrs);

    // §F (XSD 1.1 Appendix F): conditional inclusion via vc:* attributes.
    let vc_excluded = if foreign_attrs.is_empty() {
        false
    } else {
        let ns_snapshot = state.ns_context.snapshot();
        should_skip_for_vc(&foreign_attrs, state.ns_context.name_table(), &ns_snapshot, state.config.xsd_version)?
    };
    if state.frame_stack.is_empty() {
        if vc_excluded {
            state.vc_schema_excluded = true;
        }
    } else if vc_excluded || state.vc_schema_excluded {
        push_skip_frame(state, source_ref, foreign_attrs)?;
        return Ok(());
    }

    // Check if this is an XSD element (must do before borrowing frame)
    let is_in_xsd_ns = state.is_in_xsd_namespace(element_ns);

    // Check if current frame allows this child and handle skip frames
    let (allows_child, has_frame, in_skip_frame) = {
        if let Some(frame) = state.current_frame() {
            let mut allowed = frame.allows(local_name, state.ns_context.name_table());
            // Reject duplicate annotations: each XSD element allows at most one annotation
            if allowed && local_name == xsd_names::ANNOTATION && frame.has_annotation() {
                allowed = false;
            }
            (
                allowed,
                true,
                frame.is_skip_frame(),
            )
        } else {
            (true, false, false)
        }
    };

    if has_frame {
        // If we're inside a skip frame, absorb all children without creating new frames
        if in_skip_frame {
            // Just notify the skip frame (increments depth) and return
            if let Some(mut frame) = state.frame_stack.pop() {
                frame.on_child_start(local_name, state.ns_context.name_table());
                state.frame_stack.push(frame);
            }
            return Ok(());
        }

        if !is_in_xsd_ns {
            // Non-XSD element - could be in annotation or skip
            // For now, skip it by pushing a skip frame
            push_skip_frame(state, source_ref, foreign_attrs)?;
            return Ok(());
        }

        if !allows_child {
            if state.config.error_recovery {
                // Push a skip frame for error recovery
                state.add_error(SchemaError::structural(
                    "sch-props-correct",
                    format!("Unexpected element '{}' in current context", local_name),
                    None,
                ));
                push_skip_frame(state, source_ref, foreign_attrs)?;
                return Ok(());
            } else {
                return Err(SchemaError::structural(
                    "sch-props-correct",
                    format!("Unexpected element '{}' in current context", local_name),
                    None,
                ));
            }
        }

        // Notify current frame about child start
        // Pop the frame, call method, push it back to avoid borrow issues
        if let Some(mut frame) = state.frame_stack.pop() {
            frame.on_child_start(local_name, state.ns_context.name_table());
            state.frame_stack.push(frame);
        }
    }

    // Validate XSD version compatibility
    let validation_ctx = state.validation_context(source_ref.clone());
    if let Err(e) = validate_xsd_version_element(local_name, &validation_ctx) {
        if state.config.error_recovery {
            state.add_error(e);
            push_skip_frame(state, source_ref, foreign_attrs)?;
            return Ok(());
        } else {
            return Err(e);
        }
    }

    // Perform element-specific structural validation
    if let Err(e) = validate_element_attributes(local_name, &attr_map, state.ns_context.name_table(), &validation_ctx) {
        state.recover_or_fail(e)?;
    }

    // Validate XSD version for individual attributes
    if is_in_xsd_ns {
        for attr_name_id in attr_map.names() {
            let attr_name = state.ns_context.name_table().resolve(attr_name_id);
            if let Err(e) = validate_xsd_version_attribute(&attr_name, local_name, &validation_ctx) {
                state.recover_or_fail(e)?;
            }
        }
    }

    // Validate xs:ID attribute (NCName format + document-level uniqueness).
    // Skip xs:appinfo and xs:documentation — they don't define `id` in the XSD spec.
    if !matches!(local_name, xsd_names::APPINFO | xsd_names::DOCUMENTATION) {
        if let Some(id_val) = attr_map.get_value_by_name(state.ns_context.name_table(), "id") {
            if !is_ncname(id_val) {
                state.recover_or_fail(SchemaError::structural(
                    "s4s-att-invalid-value",
                    format!(
                        "'{}' attribute 'id' has invalid value '{}': not a valid xs:ID",
                        local_name, id_val
                    ),
                    source_ref.as_ref().map(|s| s.to_location(state.source_map)),
                ))?;
            } else if !state.id_values.insert(id_val.to_string()) {
                state.recover_or_fail(SchemaError::structural(
                    "s4s-att-invalid-value",
                    format!(
                        "Duplicate xs:ID value '{}' on element '{}'",
                        id_val, local_name
                    ),
                    source_ref.as_ref().map(|s| s.to_location(state.source_map)),
                ))?;
            }
        }
    }

    // Intern attribute values that are represented as NameId in frame results
    if is_in_xsd_ns {
        intern_attribute_values(local_name, &attr_map, state.ns_context.name_table_mut());
    }

    // Create namespace snapshot for QName resolution during frame construction
    let ns_snapshot = state.ns_context.snapshot();

    // Create the new frame
    let frame = if state.config.error_recovery {
        let mut frame = create_frame_recovering(
            local_name,
            &attr_map,
            state.ns_context.name_table(),
            source_ref.clone(),
            &ns_snapshot,
            &mut state.errors,
        );
        frame.set_foreign_attributes(foreign_attrs);
        // Set namespace context for annotation content frames
        if matches!(local_name, xsd_names::APPINFO | xsd_names::DOCUMENTATION) {
            frame.set_namespaces(ns_snapshot.clone());
        }
        frame
    } else {
        let mut frame = create_frame(
            local_name,
            &attr_map,
            state.ns_context.name_table(),
            source_ref.clone(),
            &ns_snapshot,
        )?;
        frame.set_foreign_attributes(foreign_attrs);
        // Set namespace context for annotation content frames
        if matches!(local_name, xsd_names::APPINFO | xsd_names::DOCUMENTATION) {
            frame.set_namespaces(ns_snapshot.clone());
        }
        frame
    };

    // Push frame onto stack
    state.frame_stack.push(frame);

    Ok(())
}

/// Handle an end element event
fn handle_end_element(state: &mut ParserState, _span: SourceSpan) -> SchemaResult<()> {
    // Check if current frame is a skip frame with pending depth
    {
        if let Some(mut frame) = state.frame_stack.pop() {
            if frame.is_skip_frame() {
                // Call on_child_end to decrement depth
                // Returns true if this is the final end element for the skipped element
                if !frame.on_child_end() {
                    // Still inside nested children, put frame back and just pop scope
                    state.frame_stack.push(frame);
                    state.pop_scope();
                    return Ok(());
                }
            }
            // Put frame back for normal processing
            state.frame_stack.push(frame);
        }
    }

    // Pop the current frame and get its result
    let frame = match state.frame_stack.pop() {
        Some(f) => f,
        None => {
            return Err(SchemaError::internal(
                "End element with no frame on stack",
            ));
        }
    };

    // Save source ref before finish() consumes the frame
    let source_ref = frame.source().cloned();

    let result = match frame.finish() {
        Ok(r) => r,
        Err(e) => {
            // Add source location to error if available
            let e = if let Some(ref src) = source_ref {
                e.with_location(state.source_map.locate(src.span.start))
            } else {
                e
            };
            return Err(e);
        }
    };

    // Pop namespace scope
    state.pop_scope();

    // Attach result to parent frame. Errors from attach() (e.g. st-props-correct
    // duplicate facet from apply_facet) are attributed to the child frame's
    // source location — that is the offending element per §3.16.2.
    if let Some(parent) = state.current_frame_mut() {
        if let Err(e) = parent.attach(result) {
            let e = if let Some(ref src) = source_ref {
                e.with_location(state.source_map.locate(src.span.start))
            } else {
                e
            };
            return Err(e);
        }
    }
    // If no parent, store the root schema result
    else if let FrameResult::Schema(schema_result) = result {
        state.root_schema = Some(schema_result);
    } else {
        return Err(SchemaError::internal(
            "Root frame did not produce a schema result",
        ));
    }

    Ok(())
}

/// Handle a text event
fn handle_text(
    state: &mut ParserState,
    text: &quick_xml::events::BytesText,
    _span: SourceSpan,
) -> SchemaResult<()> {
    let text_content = text.unescape().map_err(|e| {
        SchemaError::xml(format!("Text content error: {}", e), None)
    })?;

    // Pass text to current frame if it accepts text content
    if let Some(mut frame) = state.frame_stack.pop() {
        if frame.accepts_text() {
            frame.on_text(&text_content);
        }
        state.frame_stack.push(frame);
    }

    Ok(())
}

/// Handle a CDATA section
fn handle_cdata(
    state: &mut ParserState,
    cdata: &quick_xml::events::BytesCData,
    _span: SourceSpan,
) -> SchemaResult<()> {
    // CDATA is similar to text, typically in annotations
    if let Some(mut frame) = state.frame_stack.pop() {
        if frame.accepts_text() {
            // Convert CDATA to string
            if let Ok(cdata_str) = std::str::from_utf8(cdata.as_ref()) {
                frame.on_cdata(cdata_str);
            }
        }
        state.frame_stack.push(frame);
    }
    Ok(())
}

/// XSD 1.1 Appendix F — conditional inclusion via vc:* version attributes.
///
/// Returns `Ok(true)` if the element should be excluded (skipped), `Ok(false)`
/// if it should be included. Returns `Err` if any vc:* version attribute has
/// an invalid decimal value (the schema is then structurally invalid).
fn should_skip_for_vc(
    foreign_attrs: &[ForeignAttribute],
    name_table: &NameTable,
    ns_snapshot: &crate::namespace::NamespaceContextSnapshot,
    xsd_version: XsdVersion,
) -> SchemaResult<bool> {
    const VC_NAMESPACE: &str = "http://www.w3.org/2007/XMLSchema-versioning";
    let Some(vc_ns_id) = name_table.get(VC_NAMESPACE) else {
        return Ok(false);
    };
    let current: f64 = match xsd_version {
        XsdVersion::V1_0 => 1.0,
        XsdVersion::V1_1 => 1.1,
    };
    for attr in foreign_attrs {
        if attr.namespace != Some(vc_ns_id) {
            continue;
        }
        let local = name_table.resolve_ref(attr.local_name);
        let include = match local {
            "minVersion" | "maxVersion" | "minVersionExclusive" | "maxVersionExclusive" => {
                let bound = match attr.value.trim().parse::<f64>() {
                    Ok(v) => v,
                    Err(_) => {
                        if xsd_version == XsdVersion::V1_1 {
                            return Err(err_versioning(format!(
                                "Invalid vc:{} value '{}': must be a valid xs:decimal",
                                local, attr.value.trim()
                            )));
                        }
                        // XSD 1.0: vc: attributes are informational only; ignore invalid values
                        continue;
                    }
                };
                match local {
                    "minVersion" => current >= bound,
                    "maxVersion" => current <= bound,
                    "minVersionExclusive" => current > bound,
                    _ => current < bound,
                }
            }
            "typeAvailable" | "typeUnavailable" | "facetAvailable" | "facetUnavailable" => {
                // In XSD 1.0, these vc: attributes are silently ignored.
                if xsd_version != XsdVersion::V1_1 {
                    continue;
                }
                let is_available_attr = matches!(local, "typeAvailable" | "facetAvailable");
                let is_type_check = matches!(local, "typeAvailable" | "typeUnavailable");
                let mut available_count = 0usize;
                let mut total_count = 0usize;
                for token in attr.value.split_whitespace() {
                    total_count += 1;
                    if vc_token_available(token, local, is_type_check, ns_snapshot, name_table, xsd_version)? {
                        available_count += 1;
                    }
                }
                if total_count == 0 {
                    continue;
                }
                // typeAvailable/facetAvailable: include iff ALL items available.
                // typeUnavailable/facetUnavailable: include iff ANY item is unavailable.
                if is_available_attr {
                    available_count == total_count
                } else {
                    available_count < total_count
                }
            }
            _ => continue,
        };
        if !include {
            return Ok(true);
        }
    }
    Ok(false)
}

fn err_versioning(msg: String) -> SchemaError {
    SchemaError::structural("src-versioning", msg, None)
}

/// Evaluate a single QName token in a vc:typeAvailable / facetAvailable list.
fn vc_token_available(
    token: &str,
    local: &str,
    is_type_check: bool,
    ns_snapshot: &crate::namespace::NamespaceContextSnapshot,
    name_table: &NameTable,
    xsd_version: XsdVersion,
) -> SchemaResult<bool> {
    use crate::namespace::is_ncname;
    let (prefix_str, local_str) = match token.find(':') {
        Some(pos) => (Some(&token[..pos]), &token[pos + 1..]),
        None => (None, token),
    };
    if !is_ncname(local_str) {
        return Err(err_versioning(format!(
            "Invalid QName '{}' in vc:{}: '{}' is not a valid NCName",
            token, local, local_str
        )));
    }
    let ns_id = match prefix_str {
        Some(p) => {
            if !is_ncname(p) {
                return Err(err_versioning(format!(
                    "Invalid QName '{}' in vc:{}: '{}' is not a valid NCName prefix",
                    token, local, p
                )));
            }
            let p_id = name_table.get(p).ok_or_else(|| {
                err_versioning(format!(
                    "Undeclared prefix '{}' in vc:{} value '{}'",
                    p, local, token
                ))
            })?;
            Some(ns_snapshot.resolve_prefix(p_id).ok_or_else(|| {
                err_versioning(format!(
                    "Undeclared prefix '{}' in vc:{} value '{}'",
                    p, local, token
                ))
            })?)
        }
        None => None,
    };
    if ns_id != Some(crate::namespace::well_known::XS_NAMESPACE) {
        return Ok(false);
    }
    Ok(if is_type_check {
        vc_is_xs_type_available(local_str, xsd_version)
    } else {
        vc_is_xs_facet_available(local_str)
    })
}

fn vc_is_xs_type_available(local_name: &str, xsd_version: XsdVersion) -> bool {
    match crate::types::XmlTypeCode::from_local_name(local_name) {
        Some(code) => !code.is_xsd11() || xsd_version == XsdVersion::V1_1,
        None => false,
    }
}

fn vc_is_xs_facet_available(local_name: &str) -> bool {
    matches!(
        local_name,
        "minLength"
            | "maxLength"
            | "length"
            | "pattern"
            | "enumeration"
            | "whiteSpace"
            | "totalDigits"
            | "fractionDigits"
            | "minInclusive"
            | "maxInclusive"
            | "minExclusive"
            | "maxExclusive"
            | "assertion"
            | "explicitTimezone"
    )
}

/// Push a skip frame for error recovery
fn push_skip_frame(
    state: &mut ParserState,
    source: Option<SourceRef>,
    foreign_attrs: Vec<ForeignAttribute>,
) -> SchemaResult<()> {
    let mut frame: Box<dyn Frame> = Box::new(SkipFrame::new(source));
    frame.set_foreign_attributes(foreign_attrs);
    state.frame_stack.push(frame);
    Ok(())
}

/// Helper extension for SourceRef to convert to SourceLocation
trait SourceRefExt {
    fn to_location(&self, source_map: &SourceMap) -> SourceLocation;
}

impl SourceRefExt for SourceRef {
    fn to_location(&self, source_map: &SourceMap) -> SourceLocation {
        source_map.locate(self.span.start)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::TypeKey;
    use crate::schema::model::FormChoice;

    #[test]
    fn test_parse_minimal_schema() {
        let mut schema_set = SchemaSet::new();
        let xsd = r#"<?xml version="1.0" encoding="UTF-8"?>
            <xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            </xs:schema>"#;

        let result = parse_schema(xsd.as_bytes(), "test.xsd", &mut schema_set);
        assert!(result.is_ok());
    }

    #[test]
    fn test_parse_schema_with_element() {
        let mut schema_set = SchemaSet::new();
        let xsd = r#"<?xml version="1.0" encoding="UTF-8"?>
            <xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root" type="xs:string"/>
            </xs:schema>"#;

        let result = parse_schema(xsd.as_bytes(), "test.xsd", &mut schema_set);
        assert!(result.is_ok());
    }

    #[test]
    fn test_parse_schema_with_complex_type() {
        let mut schema_set = SchemaSet::new();
        let xsd = r#"<?xml version="1.0" encoding="UTF-8"?>
            <xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:complexType name="PersonType">
                    <xs:sequence>
                        <xs:element name="name" type="xs:string"/>
                        <xs:element name="age" type="xs:int"/>
                    </xs:sequence>
                </xs:complexType>
            </xs:schema>"#;

        let result = parse_schema(xsd.as_bytes(), "test.xsd", &mut schema_set);
        assert!(result.is_ok());
    }

    #[test]
    fn test_parse_schema_with_simple_type() {
        let mut schema_set = SchemaSet::new();
        let xsd = r#"<?xml version="1.0" encoding="UTF-8"?>
            <xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:simpleType name="StringList">
                    <xs:list itemType="xs:string"/>
                </xs:simpleType>
            </xs:schema>"#;

        let result = parse_schema(xsd.as_bytes(), "test.xsd", &mut schema_set);
        assert!(result.is_ok());
    }

    #[test]
    fn test_parse_schema_with_target_namespace() {
        let mut schema_set = SchemaSet::new();
        let xsd = r#"<?xml version="1.0" encoding="UTF-8"?>
            <xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
                       targetNamespace="http://example.com/test">
            </xs:schema>"#;

        let result = parse_schema(xsd.as_bytes(), "test.xsd", &mut schema_set);
        assert!(result.is_ok());
    }

    #[test]
    fn test_parse_schema_with_import() {
        let mut schema_set = SchemaSet::new();
        let xsd = r#"<?xml version="1.0" encoding="UTF-8"?>
            <xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:import namespace="http://www.w3.org/XML/1998/namespace"/>
            </xs:schema>"#;

        let result = parse_schema(xsd.as_bytes(), "test.xsd", &mut schema_set);
        assert!(result.is_ok());
    }

    #[cfg(feature = "xsd11")]
    #[test]
    fn test_parse_schema_assembles_arena_fields() {
        use crate::parser::frames::TypeFrameResult;
        use crate::schema::model::OpenContentMode;
        use crate::schema::wildcard::{NamespaceConstraint, ProcessContents};

        let mut schema_set = SchemaSet::xsd11();
        let xsd = r###"<?xml version="1.0" encoding="UTF-8"?>
            <xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
                       defaultAttributes="common">
                <xs:defaultOpenContent mode="suffix">
                    <xs:any namespace="##other" processContents="lax"/>
                </xs:defaultOpenContent>
                <xs:attributeGroup name="common">
                    <xs:attribute name="lang" type="xs:string"/>
                </xs:attributeGroup>
                <xs:element name="head1" type="xs:string"/>
                <xs:element name="head2" type="xs:string"/>
                <xs:element name="root" substitutionGroup="head1 head2">
                    <xs:complexType>
                        <xs:attribute name="code" type="xs:string"/>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"###;

        let doc_id = parse_schema_with_config(
            xsd.as_bytes(),
            "test.xsd",
            &mut schema_set,
            &ParserConfig::default(),
        )
        .unwrap();

        let doc = &schema_set.documents[doc_id as usize];
        let default_attrs = doc.default_attributes.as_ref().expect("defaultAttributes");
        assert_eq!(
            schema_set.name_table.resolve(default_attrs.local_name),
            "common"
        );
        assert!(default_attrs.namespace_uri.is_none());

        let default_open = doc.default_open_content.as_ref().expect("defaultOpenContent");
        assert_eq!(default_open.mode, OpenContentMode::Suffix);
        let wildcard = default_open.wildcard.as_ref().expect("wildcard");
        assert!(matches!(
            wildcard.namespace_constraint,
            NamespaceConstraint::Other
        ));
        assert_eq!(wildcard.process_contents, ProcessContents::Lax);

        let common_id = schema_set.name_table.get("common").unwrap();
        let group_key = schema_set
            .lookup_attribute_group(None, common_id)
            .expect("attributeGroup lookup");
        let group = schema_set.arenas.get_attribute_group(group_key).unwrap();
        assert_eq!(group.attributes.len(), 1);
        let lang_id = group.attributes[0].attribute.name.unwrap();
        assert_eq!(schema_set.name_table.resolve(lang_id), "lang");

        let root_id = schema_set.name_table.get("root").unwrap();
        let root_key = schema_set
            .lookup_element(None, root_id)
            .expect("element lookup");
        let root = schema_set.arenas.get_element(root_key).unwrap();
        assert_eq!(root.substitution_group.len(), 2);
        assert_eq!(
            schema_set.name_table.resolve(root.substitution_group[0].local_name),
            "head1"
        );
        assert_eq!(
            schema_set.name_table.resolve(root.substitution_group[1].local_name),
            "head2"
        );

        let inline = root.inline_type.as_ref().expect("inline type");
        match inline.as_ref() {
            TypeFrameResult::Complex(ct) => {
                assert_eq!(ct.attributes.len(), 1);
                let code_id = ct.attributes[0].attribute.name.unwrap();
                assert_eq!(schema_set.name_table.resolve(code_id), "code");
            }
            _ => panic!("expected inline complex type"),
        }
    }

    #[test]
    fn test_parse_invalid_root() {
        let mut schema_set = SchemaSet::new();
        let xsd = r#"<?xml version="1.0" encoding="UTF-8"?>
            <notSchema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            </notSchema>"#;

        let result = parse_schema(xsd.as_bytes(), "test.xsd", &mut schema_set);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_form_choice() {
        assert_eq!(
            crate::parser::assemble::parse_form_choice(Some("qualified")),
            FormChoice::Qualified
        );
        assert_eq!(
            crate::parser::assemble::parse_form_choice(Some("unqualified")),
            FormChoice::Unqualified
        );
        assert_eq!(
            crate::parser::assemble::parse_form_choice(None),
            FormChoice::Unqualified
        );
    }

    #[test]
    fn test_parser_config_default() {
        let config = ParserConfig::default();
        assert!(config.error_recovery);
        assert!(config.collect_foreign_attributes);
        assert_eq!(config.max_depth, 0);
    }

    #[test]
    fn test_apply_schema_defaults_to_elements_and_types() {
        let mut schema_set = SchemaSet::new();
        let xsd = r#"<?xml version="1.0" encoding="UTF-8"?>
            <xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
                       blockDefault="extension"
                       finalDefault="restriction">
              <xs:element name="head" type="xs:string"/>
              <xs:complexType name="Base"/>
              <xs:simpleType name="Simple">
                <xs:restriction base="xs:string"/>
              </xs:simpleType>
            </xs:schema>"#;

        let result = parse_schema(xsd.as_bytes(), "test.xsd", &mut schema_set);
        assert!(result.is_ok());

        let name_id = schema_set.name_table.get("head").expect("name id for head");
        let ns_table = schema_set
            .namespaces
            .get(&None)
            .expect("default namespace table");
        let elem_key = ns_table.elements.get(&name_id).expect("element key");
        let elem = schema_set
            .arenas
            .elements
            .get(*elem_key)
            .expect("element data");
        assert!(elem.block.contains_extension());
        assert!(elem.final_derivation.contains_restriction());

        let base_id = schema_set.name_table.get("Base").expect("name id for Base");
        let base_key = ns_table.types.get(&base_id).expect("type key for Base");
        match base_key {
            TypeKey::Complex(key) => {
                let base = schema_set
                    .arenas
                    .complex_types
                    .get(*key)
                    .expect("complex type data");
                assert!(base.block.contains_extension());
                assert!(base.final_derivation.contains_restriction());
            }
            _ => panic!("expected complex type for Base"),
        }

        let simple_id = schema_set.name_table.get("Simple").expect("name id for Simple");
        let simple_key = ns_table.types.get(&simple_id).expect("type key for Simple");
        match simple_key {
            TypeKey::Simple(key) => {
                let simple = schema_set
                    .arenas
                    .simple_types
                    .get(*key)
                    .expect("simple type data");
                assert!(simple.final_derivation.contains_restriction());
            }
            _ => panic!("expected simple type for Simple"),
        }
    }

    #[test]
    fn test_duplicate_id_detected() {
        let mut schema_set = SchemaSet::new();
        let xsd = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element id="foo123" name="a" type="xs:string"/>
            <xs:element id="foo123" name="b" type="xs:string"/>
        </xs:schema>"#;
        let result = parse_schema(xsd.as_bytes(), "test.xsd", &mut schema_set);
        assert!(result.is_ok());
        assert!(schema_set.parsing_errors.iter().any(|e| {
            e.to_string().contains("Duplicate xs:ID value 'foo123'")
        }));
    }

    #[test]
    fn test_unique_ids_valid() {
        let mut schema_set = SchemaSet::new();
        let xsd = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element id="id1" name="a" type="xs:string"/>
            <xs:element id="id2" name="b" type="xs:string"/>
        </xs:schema>"#;
        let result = parse_schema(xsd.as_bytes(), "test.xsd", &mut schema_set);
        assert!(result.is_ok());
        assert!(schema_set.parsing_errors.is_empty());
    }

    #[test]
    fn test_invalid_id_format() {
        let mut schema_set = SchemaSet::new();
        let xsd = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element id="123bad" name="a" type="xs:string"/>
        </xs:schema>"#;
        let result = parse_schema(xsd.as_bytes(), "test.xsd", &mut schema_set);
        assert!(result.is_ok());
        assert!(schema_set.parsing_errors.iter().any(|e| {
            e.to_string().contains("not a valid xs:ID")
        }));
    }
}
