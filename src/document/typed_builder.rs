//! Schema-aware document builder.
//!
//! A typed build reads an XML document once, validating it and building a
//! [`BufferDocument`] in the same pass, so that each element and attribute
//! node carries a [`NodeSchemaBinding`] (type key, declaration keys, content
//! type) from the validation:
//!
//! * [`build_typed_document_with`] — the composable build: the caller chooses
//!   the [`SchemaValidator`] (for example with XSD 1.1 assertions), supplies
//!   the [`ValidationSink`] that receives the diagnostics, and can run its own
//!   [`ValidationEventHandler`] alongside the builder in the same pass.
//! * [`TypedBuilderHandler`] — the building block underneath: the event
//!   handler that mirrors validator events into a [`BufferDocumentBuilder`],
//!   composed with an inner handler, for callers that drive
//!   [`drive_quick_xml_with`] themselves.
//! * [`build_typed_document`] — the turn-key build: default flags, no
//!   diagnostics ([`SilentValidationSink`]), no handler.

use std::io::BufRead;

use bumpalo::Bump;

use crate::parser::location::SourceSpan;
use crate::schema::SchemaSet;
use crate::validation::errors::ValidationError;
use crate::validation::info::{SchemaInfo, ValidationFlags};
use crate::validation::quick_xml_driver::{
    drive_quick_xml_with, AttributeView, DriveOutcome, DriveWithError, ElementStartView,
    EndElementInfo, EndOfAttributesView, NoopHandler, TextKind, ValidationEventHandler,
};
use crate::validation::runtime::ValidationRuntime;
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

// ── TypedBuildError ───────────────────────────────────────────────────

/// Why a typed build stopped: the document could not be read or built, the
/// caller's [`ValidationEventHandler`] returned an error, or the runtime was
/// for another schema set.
///
/// Validation diagnostics are not errors of the build: they go to the
/// runtime's [`ValidationSink`], and the build completes.
///
/// `Display` shows the document error, or `handler error: ` and the
/// handler's error. It is a [`std::error::Error`] when `E` is one; its
/// `source()` is then the handler's error (for `Document`, the document
/// error's own source).
#[derive(Debug)]
#[non_exhaustive]
pub enum TypedBuildError<E> {
    /// The XML could not be read, or the document could not be built (a
    /// parse error — input that ends inside an element is quick-xml's
    /// missing-end-tag error —, an unbound prefix, a duplicate `xml:id`, a
    /// node-count limit).
    Document(BufferDocumentError),
    /// The caller's handler returned this error; the build stopped there.
    Handler(E),
    /// The runtime does not validate against the schema set the document is
    /// built for (`build_typed_document_with`'s `schema_set`): its bindings
    /// would point into another set's components. Nothing was read.
    SchemaSetMismatch,
}

impl<E> From<BufferDocumentError> for TypedBuildError<E> {
    fn from(e: BufferDocumentError) -> Self {
        TypedBuildError::Document(e)
    }
}

impl<E: std::fmt::Display> std::fmt::Display for TypedBuildError<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TypedBuildError::Document(e) => write!(f, "{e}"),
            TypedBuildError::Handler(e) => write!(f, "handler error: {e}"),
            TypedBuildError::SchemaSetMismatch => {
                f.write_str("the validation runtime is for another schema set than the document's")
            }
        }
    }
}

impl<E: std::error::Error + 'static> std::error::Error for TypedBuildError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            TypedBuildError::Document(e) => e.source(),
            TypedBuildError::Handler(e) => Some(e),
            TypedBuildError::SchemaSetMismatch => None,
        }
    }
}

// ── Handler ───────────────────────────────────────────────────────────

/// The typed-document builder as a [`ValidationEventHandler`]: it mirrors
/// the validator events of one pass into a [`BufferDocumentBuilder`], and
/// forwards every event to an inner handler `H` (by default the no-op
/// [`NoopHandler`]).
///
/// Every hook first does the builder's work, then calls the same hook of the
/// inner handler with the same arguments — so the inner handler sees each
/// element's [`SchemaInfo`] after the node has its binding, and an error it
/// returns stops the pass as [`TypedBuildError::Handler`]. Builder failures
/// come back as [`TypedBuildError::Document`].
///
/// Drive it with [`drive_quick_xml_with`] over a runtime from the caller's
/// [`SchemaValidator`], then [`finalize`](BufferDocumentBuilder::finalize)
/// the builder — or let [`build_typed_document_with`] do both. The builder
/// must have been created for the same [`SchemaSet`] as the validator
/// (`BufferDocumentBuilder::new(arena, &set.name_table, Some(&set), options)`).
///
/// Internally it keeps the per-element bookkeeping of the build: the
/// element-ref stack (so text/end events correlate to the open element), the
/// slot for the attribute being processed, the per-element queue of
/// attributes whose binding waits for type-alternative selection, and the
/// source-span tracking.
pub struct TypedBuilderHandler<'b, 'a, H = NoopHandler> {
    builder: &'b mut BufferDocumentBuilder<'a>,
    inner: H,
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
    deferred_attr_refs: Vec<u32>,
    /// Element-decl key for the currently open element (used so we can
    /// preserve `element_decl` when refreshing the binding after CTA).
    element_decl_stack: Vec<Option<crate::ids::ElementKey>>,
}

impl<'b, 'a> TypedBuilderHandler<'b, 'a, NoopHandler> {
    /// A handler that builds into `builder` and forwards nothing.
    pub fn new(builder: &'b mut BufferDocumentBuilder<'a>) -> Self {
        Self::with_handler(builder, NoopHandler)
    }
}

impl<'b, 'a, H> TypedBuilderHandler<'b, 'a, H> {
    /// A handler that builds into `builder` and forwards every event to
    /// `inner`, in the same pass.
    pub fn with_handler(builder: &'b mut BufferDocumentBuilder<'a>, inner: H) -> Self {
        let track = builder.track_source_locations();
        Self {
            builder,
            inner,
            track,
            pending_start_offset: None,
            elem_ref_stack: Vec::new(),
            pending_spans: Vec::new(),
            current_attr_ref: None,
            deferred_attr_refs: Vec::new(),
            element_decl_stack: Vec::new(),
        }
    }

    /// The inner handler.
    pub fn handler(&self) -> &H {
        &self.inner
    }

    /// The inner handler, mutably.
    pub fn handler_mut(&mut self) -> &mut H {
        &mut self.inner
    }

    /// Give the inner handler back (the builder stays borrowed until the
    /// handler is dropped; finalize it afterwards).
    pub fn into_handler(self) -> H {
        self.inner
    }

    // The builder's part of each event.

    fn build_element_start_offset(&mut self, byte_pos: usize) {
        if self.track {
            self.pending_start_offset = Some(byte_pos);
        }
    }

    fn build_before_element(
        &mut self,
        view: ElementStartView<'_>,
    ) -> Result<(), BufferDocumentError> {
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

    fn build_after_element(&mut self, info: &SchemaInfo) -> Result<(), BufferDocumentError> {
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

    fn build_before_attribute(
        &mut self,
        view: AttributeView<'_>,
    ) -> Result<(), BufferDocumentError> {
        let attr_ref =
            self.builder
                .attribute(view.local_name, view.namespace_uri, view.prefix, view.value)?;
        // This driver also reads a real XML document, so a duplicate `xml:id`
        // is reported here exactly as the plain text parser reports it.
        if let Some(taken) = self.builder.dropped_duplicate_id() {
            return Err(BufferDocumentError::DuplicateId(taken.into_string()));
        }
        self.current_attr_ref = Some(attr_ref);
        Ok(())
    }

    fn build_after_attribute(&mut self, info: &SchemaInfo) -> Result<(), BufferDocumentError> {
        let attr_ref = self
            .current_attr_ref
            .take()
            .expect("after_attribute without matching before_attribute");
        // An attribute of an element with type alternatives is bound once, in
        // `after_end_of_attributes`, with the type the selected alternative
        // gives it: whatever this event carries is not final.
        if info.deferred_by_cta {
            self.deferred_attr_refs.push(attr_ref);
            return Ok(());
        }
        // Binding the attribute is also what files it in the id index when
        // its typed value is an `xs:ID` (`set_node_binding`); an `xml:id` was
        // filed by `attribute()` already.
        if let Some(tk) = info.schema_type {
            let binding = NodeSchemaBinding {
                type_key: tk,
                element_decl: None,
                attribute_decl: info.attribute_decl,
                content_type: None,
            };
            self.builder.set_node_binding(attr_ref, binding)?;
        }
        Ok(())
    }

    fn build_after_end_of_attributes(
        &mut self,
        view: &EndOfAttributesView<'_>,
    ) -> Result<(), BufferDocumentError> {
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

        let deferred_refs = std::mem::take(&mut self.deferred_attr_refs);
        if deferred_refs.len() != view.deferred_attribute_results.len() {
            return Err(BufferDocumentError::InternalError(
                "deferred attribute count mismatch".into(),
            ));
        }
        // The final bindings of the attributes `after_attribute` deferred —
        // and, through `set_node_binding`, their entries in the id index.
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
        Ok(())
    }

    fn build_after_end_element(&mut self) -> Result<(), BufferDocumentError> {
        self.builder.end_element()?;
        self.elem_ref_stack.pop();
        self.element_decl_stack.pop();
        Ok(())
    }

    fn build_element_end_offset(&mut self, byte_pos: usize) {
        if self.track {
            if let Some((elem_ref, start)) = self.pending_spans.pop() {
                self.builder
                    .set_source_span(elem_ref, SourceSpan::new(start, byte_pos));
            }
        }
    }

    fn build_text(&mut self, text: &str) {
        if !self.elem_ref_stack.is_empty() {
            self.builder.text(text);
        }
    }
}

impl<H: ValidationEventHandler> ValidationEventHandler for TypedBuilderHandler<'_, '_, H> {
    type Error = TypedBuildError<H::Error>;

    fn on_element_start_offset(&mut self, byte_pos: usize) -> Result<(), Self::Error> {
        self.build_element_start_offset(byte_pos);
        self.inner
            .on_element_start_offset(byte_pos)
            .map_err(TypedBuildError::Handler)
    }

    fn before_element(&mut self, view: ElementStartView<'_>) -> Result<(), Self::Error> {
        self.build_before_element(view)?;
        self.inner
            .before_element(view)
            .map_err(TypedBuildError::Handler)
    }

    fn after_element(
        &mut self,
        view: ElementStartView<'_>,
        info: &SchemaInfo,
    ) -> Result<(), Self::Error> {
        self.build_after_element(info)?;
        self.inner
            .after_element(view, info)
            .map_err(TypedBuildError::Handler)
    }

    fn before_attribute(&mut self, view: AttributeView<'_>) -> Result<(), Self::Error> {
        self.build_before_attribute(view)?;
        self.inner
            .before_attribute(view)
            .map_err(TypedBuildError::Handler)
    }

    fn after_attribute(
        &mut self,
        view: AttributeView<'_>,
        info: &SchemaInfo,
    ) -> Result<(), Self::Error> {
        self.build_after_attribute(info)?;
        self.inner
            .after_attribute(view, info)
            .map_err(TypedBuildError::Handler)
    }

    /// The inner handler decides; the attribute is built either way (a
    /// skipped attribute stays untyped).
    fn skip_attribute(&mut self, view: AttributeView<'_>) -> Result<bool, Self::Error> {
        self.inner
            .skip_attribute(view)
            .map_err(TypedBuildError::Handler)
    }

    fn after_end_of_attributes(
        &mut self,
        view: EndOfAttributesView<'_>,
    ) -> Result<(), Self::Error> {
        self.build_after_end_of_attributes(&view)?;
        self.inner
            .after_end_of_attributes(view)
            .map_err(TypedBuildError::Handler)
    }

    fn after_end_element(
        &mut self,
        info: &EndElementInfo,
        depth: usize,
    ) -> Result<(), Self::Error> {
        self.build_after_end_element()?;
        self.inner
            .after_end_element(info, depth)
            .map_err(TypedBuildError::Handler)
    }

    fn on_element_end_offset(&mut self, byte_pos: usize) -> Result<(), Self::Error> {
        self.build_element_end_offset(byte_pos);
        self.inner
            .on_element_end_offset(byte_pos)
            .map_err(TypedBuildError::Handler)
    }

    fn on_text(&mut self, kind: TextKind, text: &str) -> Result<(), Self::Error> {
        self.build_text(text);
        self.inner
            .on_text(kind, text)
            .map_err(TypedBuildError::Handler)
    }

    fn on_comment(&mut self, text: &str) -> Result<(), Self::Error> {
        self.builder.comment(text)?;
        self.inner
            .on_comment(text)
            .map_err(TypedBuildError::Handler)
    }

    fn on_processing_instruction(&mut self, target: &str, data: &str) -> Result<(), Self::Error> {
        self.builder.processing_instruction(target, data)?;
        self.inner
            .on_processing_instruction(target, data)
            .map_err(TypedBuildError::Handler)
    }
}

/// Lends a caller's handler to a [`TypedBuilderHandler`] for one pass.
struct LentHandler<'h, H: ?Sized>(&'h mut H);

impl<H: ValidationEventHandler + ?Sized> ValidationEventHandler for LentHandler<'_, H> {
    type Error = H::Error;

    fn before_element(&mut self, view: ElementStartView<'_>) -> Result<(), H::Error> {
        self.0.before_element(view)
    }
    fn after_element(
        &mut self,
        view: ElementStartView<'_>,
        info: &SchemaInfo,
    ) -> Result<(), H::Error> {
        self.0.after_element(view, info)
    }
    fn before_attribute(&mut self, view: AttributeView<'_>) -> Result<(), H::Error> {
        self.0.before_attribute(view)
    }
    fn after_attribute(
        &mut self,
        view: AttributeView<'_>,
        info: &SchemaInfo,
    ) -> Result<(), H::Error> {
        self.0.after_attribute(view, info)
    }
    fn skip_attribute(&mut self, view: AttributeView<'_>) -> Result<bool, H::Error> {
        self.0.skip_attribute(view)
    }
    fn after_end_of_attributes(&mut self, view: EndOfAttributesView<'_>) -> Result<(), H::Error> {
        self.0.after_end_of_attributes(view)
    }
    fn after_end_element(&mut self, info: &EndElementInfo, depth: usize) -> Result<(), H::Error> {
        self.0.after_end_element(info, depth)
    }
    fn on_text(&mut self, kind: TextKind, text: &str) -> Result<(), H::Error> {
        self.0.on_text(kind, text)
    }
    fn on_comment(&mut self, text: &str) -> Result<(), H::Error> {
        self.0.on_comment(text)
    }
    fn on_processing_instruction(&mut self, target: &str, data: &str) -> Result<(), H::Error> {
        self.0.on_processing_instruction(target, data)
    }
    fn on_element_start_offset(&mut self, byte_pos: usize) -> Result<(), H::Error> {
        self.0.on_element_start_offset(byte_pos)
    }
    fn on_element_end_offset(&mut self, byte_pos: usize) -> Result<(), H::Error> {
        self.0.on_element_end_offset(byte_pos)
    }
}

// ── build_typed_document_with ─────────────────────────────────────────

/// Options of a [`build_typed_document_with`] pass.
///
/// `#[non_exhaustive]`, so options can be added later: start from
/// [`TypedBuildOptions::default`] and set what you need.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct TypedBuildOptions {
    /// Options of the document built (its kind, source-span tracking).
    pub document: BufferDocumentOptions,
}

impl TypedBuildOptions {
    /// These options with `document` as the document options.
    pub fn with_document_options(mut self, document: BufferDocumentOptions) -> Self {
        self.document = document;
        self
    }
}

/// What a [`build_typed_document_with`] pass produced.
#[non_exhaustive]
pub struct TypedBuild<'a> {
    /// The document, with a [`NodeSchemaBinding`] on every element and
    /// attribute the validation typed.
    pub document: BufferDocument<'a>,
    /// The drive outcome: the root element's `[validity]` and the depth.
    pub outcome: DriveOutcome,
}

/// Validate a document and build it, typed, in one pass over `reader`, with
/// the caller's validator, sink and event handler.
///
/// * `runtime` comes from the caller's [`SchemaValidator`] for `schema_set`
///   (`validator.start_run(sink)`): the validator decides the flags — for
///   example `ValidationFlags::PROCESS_ASSERTIONS` to evaluate XSD 1.1
///   assertions — and the sink receives every error and warning, each with
///   its subject and positional path.
/// * `handler` sees every event of the pass, after the builder has handled
///   it (see [`TypedBuilderHandler`]); pass [`NoopHandler`] for none. An
///   error it returns stops the build as [`TypedBuildError::Handler`].
/// * `schema_set` must be the set the runtime's validator was made for
///   ([`TypedBuildError::SchemaSetMismatch`] otherwise); the document borrows
///   it (and `arena`) for `'a`.
/// * `handler` may be a trait object (`&mut dyn ValidationEventHandler<Error
///   = E>`).
/// * `options` carry the document options ([`TypedBuildOptions`]).
///
/// Like [`drive_quick_xml_with`], this does **not** call
/// [`ValidationRuntime::end_validation`]: call it afterwards — it reports
/// the checks that need the whole document (an IDREF without its ID,
/// `cvc-id.1`) to the sink, and returns an operational failure, if any. The
/// runtime also still answers
/// [`schema_location_hints`](ValidationRuntime::schema_location_hints) and
/// the identity-constraint tables.
///
/// Validation diagnostics never stop the build: an invalid document is
/// built and typed as far as the validation typed it.
///
/// # Example
///
/// Validate and type a document in one pass, and get the errors:
///
/// ```
/// use bumpalo::Bump;
/// use xsd_schema::document::{build_typed_document_with, TypedBuildOptions};
/// use xsd_schema::navigator::{DomNavigator, TypedValue};
/// use xsd_schema::validation::{
///     CollectingValidationSink, NoopHandler, SchemaValidator, SchemaValidity, ValidationFlags,
/// };
/// use xsd_schema::SchemaSetBuilder;
///
/// let xsd = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
///   <xs:element name="order">
///     <xs:complexType><xs:sequence>
///       <xs:element name="qty" type="xs:positiveInteger" maxOccurs="unbounded"/>
///     </xs:sequence></xs:complexType>
///   </xs:element>
/// </xs:schema>"#;
/// let schema_set = SchemaSetBuilder::new()
///     .add_source(xsd, "file:///order.xsd")?
///     .compile()?
///     .into_schema_set();
/// let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
///
/// let arena = Bump::new();
/// let (mut errors, mut warnings) = (Vec::new(), Vec::new());
/// let mut runtime = validator.start_run(CollectingValidationSink {
///     errors: &mut errors,
///     warnings: &mut warnings,
/// });
/// let built = build_typed_document_with(
///     "<order><qty>2</qty><qty>0</qty></order>".as_bytes(),
///     &arena,
///     &schema_set,
///     &mut runtime,
///     &mut NoopHandler,
///     TypedBuildOptions::default(),
/// )?;
/// runtime.end_validation()?; // the end-of-document checks report here
/// drop(runtime); // releases the sink's borrow of `errors`
///
/// // The errors, each with the item it is about…
/// assert_eq!(built.outcome.root_validity, Some(SchemaValidity::Invalid));
/// assert_eq!(errors.len(), 1);
/// assert_eq!(
///     errors[0].instance_path().unwrap().to_string(),
///     "/Q{}order[1]/Q{}qty[2]"
/// );
///
/// // …and the typed document, from the same pass.
/// let mut nav = built.document.create_navigator();
/// assert!(nav.move_to_first_child() && nav.move_to_first_child()); // order/qty[1]
/// assert!(matches!(nav.typed_value(), TypedValue::Value(_)));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn build_typed_document_with<'a, R, S, H>(
    reader: R,
    arena: &'a Bump,
    schema_set: &'a SchemaSet,
    runtime: &mut ValidationRuntime<'_, S>,
    handler: &mut H,
    options: TypedBuildOptions,
) -> Result<TypedBuild<'a>, TypedBuildError<H::Error>>
where
    R: BufRead,
    S: ValidationSink,
    H: ValidationEventHandler + ?Sized,
{
    if !std::ptr::eq(runtime.schema_set, schema_set) {
        return Err(TypedBuildError::SchemaSetMismatch);
    }
    let mut builder = BufferDocumentBuilder::new(
        arena,
        &schema_set.name_table,
        Some(schema_set),
        options.document,
    )?;
    let driven = {
        let mut typed = TypedBuilderHandler::with_handler(&mut builder, LentHandler(handler));
        drive_quick_xml_with(reader, runtime, schema_set, &mut typed)
    };
    let outcome = driven.map_err(|e| match e {
        DriveWithError::Parse(e) => TypedBuildError::Document(BufferDocumentError::Parse(e)),
        DriveWithError::Utf8(e) => TypedBuildError::Document(BufferDocumentError::Utf8(e)),
        DriveWithError::UnboundPrefix(p) => {
            TypedBuildError::Document(BufferDocumentError::UnboundPrefix(p))
        }
        // The XML error quick-xml itself gives for an element whose end tag
        // never comes (it reports a plain end of input to an event loop).
        DriveWithError::UnexpectedEof { .. } => {
            TypedBuildError::Document(BufferDocumentError::Parse(quick_xml::Error::IllFormed(
                quick_xml::errors::IllFormedError::MissingEndTag(
                    runtime.innermost_open_element_name().unwrap_or_default(),
                ),
            )))
        }
        DriveWithError::Hook(e) => e,
    })?;
    Ok(TypedBuild {
        document: builder.finalize()?,
        outcome,
    })
}

// ── build_typed_document ──────────────────────────────────────────────

/// Build a [`BufferDocument`] with schema bindings on every element and attribute.
///
/// This function mirrors [`BufferDocumentBuilder::build`] but interleaves
/// [`SchemaValidator`] calls so that the resulting document carries
/// [`NodeSchemaBinding`] entries for typed-value and schema-type queries.
///
/// The validator is `SchemaValidator::new(schema_set, ValidationFlags::default())`
/// — XSD 1.1 assertions are not evaluated — and validation errors are
/// silently discarded via [`SilentValidationSink`]. To choose the validator,
/// receive the errors or run a handler of your own in the same pass, use
/// [`build_typed_document_with`], on which this function is built.
pub fn build_typed_document<'a, R: BufRead>(
    reader: R,
    arena: &'a Bump,
    schema_set: &'a SchemaSet,
    options: BufferDocumentOptions,
) -> Result<BufferDocument<'a>, BufferDocumentError> {
    let validator = SchemaValidator::new(schema_set, ValidationFlags::default());
    let mut runtime = validator.start_run(SilentValidationSink);
    let built = build_typed_document_with(
        reader,
        arena,
        schema_set,
        &mut runtime,
        &mut NoopHandler,
        TypedBuildOptions::default().with_document_options(options),
    )
    .map_err(|e| match e {
        TypedBuildError::Document(e) => e,
        TypedBuildError::Handler(never) => match never {},
        TypedBuildError::SchemaSetMismatch => {
            unreachable!("the runtime is made from `schema_set` above")
        }
    })?;
    let _ = runtime.end_validation();
    Ok(built.document)
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::TypeKey;
    use crate::navigator::{DomNavigator, DomNodeType, NamespaceAxisScope, TypedValue};
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

    // ── Test 4: typed_value() returns Absent for ElementOnly ──────────

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

    // ── Test 6b: type_annotation() — the full TypeKey ────────────────

    /// `type_annotation()` is the XDM *type-name* property, so unlike
    /// `schema_type()` it must also report complex types — that is what
    /// `element(*, T)` matching with a complex `T` needs.
    #[test]
    fn type_annotation_reports_complex_simple_and_attribute_types() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="leaf" type="xs:integer"/>
                        </xs:sequence>
                        <xs:attribute name="count" type="xs:integer"/>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"#,
        );
        let arena = Bump::new();
        let doc = build_doc(
            r#"<root count="7"><leaf>42</leaf></root>"#,
            &arena,
            &schema_set,
        );

        // Document root: not an element or attribute → no annotation.
        let mut nav = doc.create_navigator();
        assert_eq!(nav.node_type(), DomNodeType::Root);
        assert_eq!(nav.type_annotation(), None);

        // <root>: annotated with a complex type, which `schema_type()`
        // cannot express.
        assert!(nav.move_to_first_child());
        assert!(
            matches!(nav.type_annotation(), Some(TypeKey::Complex(_))),
            "complex-typed element should report TypeKey::Complex, got {:?}",
            nav.type_annotation()
        );
        assert_eq!(
            nav.schema_type(),
            None,
            "a complex annotation has no simple-type projection"
        );

        // @count: a typed attribute; the annotation agrees with
        // `schema_type()`.
        let mut attr = nav.clone();
        assert!(attr.move_to_first_attribute());
        assert_eq!(attr.local_name(), "count");
        match attr.type_annotation() {
            Some(TypeKey::Simple(k)) => assert_eq!(Some(k), attr.schema_type()),
            other => panic!("expected a simple annotation on @count, got {:?}", other),
        }

        // <leaf>: a simple-typed element; same agreement.
        assert!(nav.move_to_first_child());
        assert_eq!(nav.local_name(), "leaf");
        match nav.type_annotation() {
            Some(TypeKey::Simple(k)) => assert_eq!(Some(k), nav.schema_type()),
            other => panic!("expected a simple annotation on <leaf>, got {:?}", other),
        }

        // The text child of <leaf> is not an element or attribute.
        let mut text = nav.clone();
        assert!(text.move_to_first_child());
        assert_eq!(text.node_type(), DomNodeType::Text);
        assert_eq!(text.type_annotation(), None);

        // The namespace cursor keeps `current` on its owning element, but is
        // not itself an annotated node.
        let mut ns = nav.clone();
        assert!(ns.move_to_first_namespace(NamespaceAxisScope::All));
        assert_eq!(ns.node_type(), DomNodeType::Namespace);
        assert_eq!(ns.type_annotation(), None);
    }

    /// A document whose content the schema does not declare carries no
    /// bindings, so every node is untyped.
    #[test]
    fn type_annotation_is_none_for_untyped_document() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="other" type="xs:string"/>
            </xs:schema>"#,
        );
        let arena = Bump::new();
        let doc = build_doc(r#"<root count="7">hello</root>"#, &arena, &schema_set);

        let mut nav = doc.create_navigator();
        assert!(nav.move_to_first_child()); // undeclared <root>
        assert_eq!(nav.type_annotation(), None);
        assert_eq!(nav.typed_value(), TypedValue::Untyped);
        assert!(nav.move_to_first_attribute());
        assert_eq!(nav.type_annotation(), None);
    }

    /// `RoXmlNavigator` is schema-unaware and keeps the trait's default body.
    #[test]
    fn type_annotation_is_none_for_roxml_navigator() {
        let parsed = roxmltree::Document::parse(r#"<root count="7">hello</root>"#).unwrap();
        let mut nav = crate::navigator::RoXmlNavigator::new(&parsed);
        assert_eq!(nav.type_annotation(), None);
        assert!(nav.move_to_first_child());
        assert_eq!(nav.type_annotation(), None);
        assert!(nav.move_to_first_attribute());
        assert_eq!(nav.type_annotation(), None);
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

    // ── `BufferDocument::has_type_annotations` ────────────────────────

    /// Walks the whole tree and reports whether any element or attribute node
    /// carries a type annotation — the O(nodes) answer the O(1) accessor must
    /// agree with exactly.
    fn walk_finds_an_annotation(doc: &BufferDocument<'_>) -> bool {
        fn visit(nav: &crate::document::navigator::BufferDocNavigator<'_>) -> bool {
            if matches!(
                nav.node_type(),
                DomNodeType::Element | DomNodeType::Attribute
            ) && nav.type_annotation().is_some()
            {
                return true;
            }
            if nav.node_type() == DomNodeType::Element {
                let mut attr = nav.clone();
                if attr.move_to_first_attribute() {
                    loop {
                        if attr.type_annotation().is_some() {
                            return true;
                        }
                        if !attr.move_to_next_attribute() {
                            break;
                        }
                    }
                }
            }
            let mut child = nav.clone();
            if child.move_to_first_child() {
                loop {
                    if visit(&child) {
                        return true;
                    }
                    if !child.move_to_next_sibling() {
                        break;
                    }
                }
            }
            false
        }
        visit(&doc.create_navigator())
    }

    fn annotation_schema() -> SchemaSet {
        load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="n" type="xs:integer"/>
                        </xs:sequence>
                        <xs:attribute name="count" type="xs:integer"/>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"#,
        )
    }

    #[test]
    fn has_type_annotations_is_false_for_an_unvalidated_document() {
        let arena = Bump::new();
        let names = crate::namespace::NameTable::new();
        let doc = BufferDocument::from_reader_default(
            r#"<root count="1"><n>2</n><!--c--><?pi t?></root>"#.as_bytes(),
            &arena,
            &names,
        )
        .expect("parses");
        assert!(!doc.has_type_annotations());
        assert!(!walk_finds_an_annotation(&doc));
    }

    #[test]
    fn has_type_annotations_is_true_for_a_typed_document() {
        let schema_set = annotation_schema();
        let arena = Bump::new();
        let doc = build_doc(r#"<root count="1"><n>2</n></root>"#, &arena, &schema_set);
        assert!(doc.has_type_annotations());
        assert!(walk_finds_an_annotation(&doc));
    }

    #[test]
    fn has_type_annotations_follows_the_copys_annotation_mode() {
        use crate::document::{Annotations, CopyOptions};

        let schema_set = annotation_schema();
        let arena = Bump::new();
        let source = build_doc(r#"<root count="1"><n>2</n></root>"#, &arena, &schema_set);
        assert!(source.has_type_annotations());

        let mut node = source.create_navigator();
        assert!(node.move_to_first_child());

        for (annotations, expected) in [(Annotations::Preserve, true), (Annotations::Strip, false)]
        {
            let mut builder = crate::document::BufferDocumentBuilder::new(
                &arena,
                &schema_set.name_table,
                Some(&schema_set),
                BufferDocumentOptions::default(),
            )
            .expect("a builder");
            builder.start_element("out", "", "", &[]).expect("starts");
            builder.end_of_attributes();
            builder
                .copy_subtree(
                    &node,
                    CopyOptions {
                        annotations,
                        ..CopyOptions::default()
                    },
                )
                .expect("copies");
            builder.end_element().expect("ends");
            let copy = builder.finalize().expect("finalizes");
            assert_eq!(
                copy.has_type_annotations(),
                expected,
                "{annotations:?} copy"
            );
            assert_eq!(
                walk_finds_an_annotation(&copy),
                expected,
                "{annotations:?} copy, by walk"
            );
        }
    }

    #[test]
    fn has_type_annotations_agrees_with_a_full_walk() {
        let schema_set = annotation_schema();
        let names = crate::namespace::NameTable::new();
        for xml in [
            "<root/>",
            r#"<root count="1"><n>2</n></root>"#,
            "<root><n>0</n></root>",
            r#"<root count="7"><n>-3</n></root>"#,
        ] {
            // Unvalidated: never annotated.
            let arena = Bump::new();
            let plain = BufferDocument::from_reader_default(xml.as_bytes(), &arena, &names)
                .expect("parses");
            assert_eq!(
                plain.has_type_annotations(),
                walk_finds_an_annotation(&plain),
                "untyped {xml}"
            );
            assert!(!plain.has_type_annotations(), "untyped {xml}");

            // The same markup through the validating builder. `<root/>` is
            // invalid against the schema, so it exercises the path where
            // validation fails and only some nodes get a binding.
            let arena = Bump::new();
            if let Ok(typed) = build_typed_document(
                xml.as_bytes(),
                &arena,
                &schema_set,
                BufferDocumentOptions::default(),
            ) {
                assert_eq!(
                    typed.has_type_annotations(),
                    walk_finds_an_annotation(&typed),
                    "typed {xml}"
                );
            }
        }
    }

    // ── Schema-typed xs:ID attributes are is-id nodes ────────────────

    /// F&O §15.5.2: "E has an attribute node whose is-id property is true and
    /// whose typed value is equal to V". A schema-validated `xs:ID` attribute
    /// has that property, and it is the annotation the validator attaches that
    /// says which attributes do.
    #[test]
    fn a_schema_typed_id_attribute_is_findable() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="item" maxOccurs="unbounded">
                                <xs:complexType>
                                    <xs:attribute name="key" type="xs:ID"/>
                                </xs:complexType>
                            </xs:element>
                        </xs:sequence>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"#,
        );
        let arena = Bump::new();
        let doc = build_doc(
            r#"<root><item key="a"/><item key=" b "/></root>"#,
            &arena,
            &schema_set,
        );

        let found = doc.get_element_by_id("a").expect("the xs:ID is indexed");
        let mut nav = doc.create_navigator_at(found);
        assert_eq!(nav.local_name(), "item");
        assert!(nav.move_to_first_attribute());
        assert_eq!(nav.value(), "a");

        // The key is whitespace-normalized; the attribute value is not.
        let padded = doc.get_element_by_id("b").expect("a padded xs:ID too");
        let mut nav = doc.create_navigator_at(padded);
        assert!(nav.move_to_first_attribute());
        assert_eq!(nav.value(), " b ", "the visible value is untouched");
    }

    /// An attribute of an ordinary type is not an is-id node.
    #[test]
    fn a_plain_attribute_is_not_indexed_as_an_id() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root">
                    <xs:complexType>
                        <xs:attribute name="key" type="xs:NCName"/>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"#,
        );
        let arena = Bump::new();
        let doc = build_doc(r#"<root key="a"/>"#, &arena, &schema_set);
        assert_eq!(doc.get_element_by_id("a"), None);
    }
}
