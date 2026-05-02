//! Reusable driver that wires a [`quick_xml::Reader`] event stream into a
//! [`ValidationRuntime`].
//!
//! Two layers are provided:
//!
//! * [`drive_quick_xml`] / [`drive_quick_xml_in`] — turn-key, all you want is
//!   for the runtime's [`ValidationSink`] to receive every diagnostic. The
//!   helper calls [`ValidationRuntime::end_validation`] for you.
//! * [`drive_quick_xml_with`] / [`drive_quick_xml_with_in`] — callback-driven,
//!   for callers that need to interleave work between validator events
//!   (typed-document construction, source-span tracking, etc). The caller is
//!   responsible for `end_validation` on this path; see the
//!   [`ValidationEventHandler`] trait for hook ordering.
//!
//! DTD-related events ([`Event::DocType`], [`Event::Decl`]) are silently
//! dropped. Comments and processing instructions flow through the layer-2
//! hooks; layer 1 ignores them via [`NoopHandler`].
//!
//! Namespace scoping (xmlns push/pop), `xsi:type`/`xsi:nil` discovery, and
//! [`NamespaceContextSnapshot`] construction are handled internally so the
//! caller does not have to.

use std::collections::HashMap;
use std::convert::Infallible;
use std::io::BufRead;

use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
use thiserror::Error;

use crate::namespace::context::NamespaceContextSnapshot;
use crate::namespace::table::{XML_NAMESPACE, XSI_NAMESPACE};
use crate::schema::SchemaSet;
use crate::validation::errors::ValidationError;
use crate::validation::info::{SchemaInfo, SchemaValidity};
use crate::validation::runtime::ValidationRuntime;
use crate::validation::validator::ValidationSink;

// ── Public types ──────────────────────────────────────────────────────────

/// Final outcome of a successful drive call.
///
/// Validation diagnostics are reported through the runtime's
/// [`ValidationSink`], not through this struct.
#[derive(Debug, Clone)]
pub struct DriveOutcome {
    /// Validity of the root element after `validate_end_element`.
    /// `None` if the stream contained no elements.
    pub root_validity: Option<SchemaValidity>,
    /// Maximum element depth observed.
    pub max_depth: usize,
}

/// Errors raised by the layer-1 helpers.
#[derive(Debug, Error)]
pub enum DriveError {
    #[error("xml parse error: {0}")]
    Parse(#[from] quick_xml::Error),
    #[error("utf-8 error: {0}")]
    Utf8(#[from] std::str::Utf8Error),
    #[error("unbound prefix '{0}'")]
    UnboundPrefix(String),
    /// Stream ended with `depth > 0` (open elements not closed).
    #[error("unexpected eof: {depth} element(s) still open")]
    UnexpectedEof { depth: usize },
    /// `runtime.end_validation()` returned `Err` after the stream was driven.
    #[error("end_validation failed: {0}")]
    Validation(ValidationError),
}

/// Errors raised by the layer-2 helpers.
#[derive(Debug, Error)]
pub enum DriveWithError<E> {
    #[error("xml parse error: {0}")]
    Parse(quick_xml::Error),
    #[error("utf-8 error: {0}")]
    Utf8(std::str::Utf8Error),
    #[error("unbound prefix '{0}'")]
    UnboundPrefix(String),
    #[error("unexpected eof: {depth} element(s) still open")]
    UnexpectedEof { depth: usize },
    /// A handler hook returned an error.
    #[error("hook error")]
    Hook(E),
}

/// View of an element-start (or empty) event passed to handler hooks.
///
/// Borrowed slices live until the next `read_event_into` call, so hooks must
/// not retain references past their return.
#[derive(Clone, Copy)]
pub struct ElementStartView<'a> {
    pub local_name: &'a str,
    pub namespace_uri: &'a str,
    pub prefix: &'a str,
    /// Lexical value of `xsi:type`, if present.
    pub xsi_type: Option<&'a str>,
    /// Lexical value of `xsi:nil`, if present.
    pub xsi_nil: Option<&'a str>,
    /// Pre-built snapshot used for QName resolution inside the runtime.
    pub ns_context: &'a NamespaceContextSnapshot,
    /// xmlns declarations on THIS element (prefix, uri). Empty prefix is the
    /// default-namespace declaration.
    pub namespace_decls: &'a [(&'a str, &'a str)],
    /// `true` if this came from `Event::Empty`.
    pub is_empty: bool,
}

/// View of a non-xmlns attribute.
#[derive(Clone, Copy)]
pub struct AttributeView<'a> {
    pub local_name: &'a str,
    pub namespace_uri: &'a str,
    pub prefix: &'a str,
    /// Already unescaped (entity references resolved by quick-xml).
    pub value: &'a str,
}

/// Payload for [`ValidationEventHandler::after_end_of_attributes`].
pub struct EndOfAttributesView<'a> {
    pub info: &'a SchemaInfo,
    /// Drained `take_deferred_attribute_results()` payload, in original
    /// attribute encounter order. Empty whenever no CTA reselection occurred.
    #[cfg(feature = "xsd11")]
    pub deferred_attribute_results: &'a [SchemaInfo],
}

/// What the runtime returned from `validate_end_element`.
#[derive(Debug, Clone, Copy)]
pub struct EndElementInfo {
    pub validity: SchemaValidity,
}

/// How a text/CDATA event was dispatched into the runtime.
#[derive(Debug, Clone, Copy)]
pub enum TextKind {
    /// `Event::Text` whose unescaped content is all whitespace —
    /// forwarded to `runtime.validate_whitespace`.
    Whitespace,
    /// `Event::Text` with non-whitespace content —
    /// forwarded to `runtime.validate_text`.
    Character,
    /// `Event::CData` — always forwarded to `runtime.validate_text`.
    CData,
}

// ── Handler trait ─────────────────────────────────────────────────────────

/// Handler invoked at each validator-event boundary.
///
/// Every method has a default implementation that does nothing, so a
/// handler that only cares about (say) end-of-element fires exactly that
/// one method.
///
/// Hook ordering for one element:
/// 1. `on_element_start_offset`
/// 2. `before_element`
/// 3. (internal) `runtime.validate_element`
/// 4. `after_element`
/// 5. For each non-xmlns attribute, in document order:
///    1. `before_attribute`
///    2. (internal) `runtime.validate_attribute`
///    3. `after_attribute`
/// 6. (internal) `runtime.validate_end_of_attributes`
/// 7. (internal, xsd11) `runtime.take_deferred_attribute_results`
/// 8. `after_end_of_attributes`
/// 9. Body events: `on_text`, `on_comment`, `on_processing_instruction`.
/// 10. On the closing event:
///     1. (internal) `runtime.validate_end_element`
///     2. `after_end_element`
///     3. `on_element_end_offset`
pub trait ValidationEventHandler {
    /// Caller's hook-error type. Reported through
    /// [`DriveWithError::Hook`].
    type Error;

    fn before_element(&mut self, _view: ElementStartView<'_>) -> Result<(), Self::Error> {
        Ok(())
    }

    fn after_element(
        &mut self,
        _view: ElementStartView<'_>,
        _info: &SchemaInfo,
    ) -> Result<(), Self::Error> {
        Ok(())
    }

    fn before_attribute(&mut self, _view: AttributeView<'_>) -> Result<(), Self::Error> {
        Ok(())
    }

    fn after_attribute(
        &mut self,
        _view: AttributeView<'_>,
        _info: &SchemaInfo,
    ) -> Result<(), Self::Error> {
        Ok(())
    }

    fn after_end_of_attributes(
        &mut self,
        _view: EndOfAttributesView<'_>,
    ) -> Result<(), Self::Error> {
        Ok(())
    }

    /// `depth` is the depth at which the element existed (1 = root close).
    fn after_end_element(
        &mut self,
        _info: &EndElementInfo,
        _depth: usize,
    ) -> Result<(), Self::Error> {
        Ok(())
    }

    fn on_text(&mut self, _kind: TextKind, _text: &str) -> Result<(), Self::Error> {
        Ok(())
    }

    fn on_comment(&mut self, _text: &str) -> Result<(), Self::Error> {
        Ok(())
    }

    fn on_processing_instruction(
        &mut self,
        _target: &str,
        _data: &str,
    ) -> Result<(), Self::Error> {
        Ok(())
    }

    /// Buffer offset of the `<` for the element about to be reported to
    /// `before_element`. Default impl is a no-op; override only when
    /// building a span-aware DOM.
    fn on_element_start_offset(&mut self, _byte_pos: usize) -> Result<(), Self::Error> {
        Ok(())
    }

    /// Buffer offset just past the `>` of the closing tag. Default impl is a
    /// no-op; override only when building a span-aware DOM.
    fn on_element_end_offset(&mut self, _byte_pos: usize) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// Zero-sized handler whose every method is the trait default.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopHandler;

impl ValidationEventHandler for NoopHandler {
    type Error = Infallible;
}

// ── Layer 1: turn-key ─────────────────────────────────────────────────────

/// Drive a quick-xml stream into `runtime`, then call
/// `runtime.end_validation()`.
///
/// Validation diagnostics arrive through the sink the runtime was built with.
/// DTD events are silently dropped. Comments and PIs are dropped.
pub fn drive_quick_xml<R, S>(
    reader: R,
    runtime: &mut ValidationRuntime<'_, S>,
    schema_set: &SchemaSet,
) -> Result<DriveOutcome, DriveError>
where
    R: BufRead,
    S: ValidationSink,
{
    let mut buf = Vec::new();
    drive_quick_xml_in(reader, runtime, schema_set, &mut buf)
}

/// [`drive_quick_xml`] variant that reuses a caller-supplied buffer.
pub fn drive_quick_xml_in<R, S>(
    reader: R,
    runtime: &mut ValidationRuntime<'_, S>,
    schema_set: &SchemaSet,
    buf: &mut Vec<u8>,
) -> Result<DriveOutcome, DriveError>
where
    R: BufRead,
    S: ValidationSink,
{
    let mut handler = NoopHandler;
    let outcome =
        drive_quick_xml_with_in(reader, runtime, schema_set, &mut handler, buf).map_err(
            |e| match e {
                DriveWithError::Parse(e) => DriveError::Parse(e),
                DriveWithError::Utf8(e) => DriveError::Utf8(e),
                DriveWithError::UnboundPrefix(p) => DriveError::UnboundPrefix(p),
                DriveWithError::UnexpectedEof { depth } => DriveError::UnexpectedEof { depth },
                DriveWithError::Hook(_) => unreachable!("NoopHandler is infallible"),
            },
        )?;
    runtime.end_validation().map_err(DriveError::Validation)?;
    Ok(outcome)
}

// ── Layer 2: handler-driven ───────────────────────────────────────────────

/// Drive a quick-xml stream into `runtime`, invoking `handler` at each
/// validator-event boundary.
///
/// **Does NOT call `runtime.end_validation()`.** The caller must do so after
/// any post-stream state collection (e.g. `runtime.schema_location_hints()`).
pub fn drive_quick_xml_with<R, S, H>(
    reader: R,
    runtime: &mut ValidationRuntime<'_, S>,
    schema_set: &SchemaSet,
    handler: &mut H,
) -> Result<DriveOutcome, DriveWithError<H::Error>>
where
    R: BufRead,
    S: ValidationSink,
    H: ValidationEventHandler,
{
    let mut buf = Vec::new();
    drive_quick_xml_with_in(reader, runtime, schema_set, handler, &mut buf)
}

/// [`drive_quick_xml_with`] variant that reuses a caller-supplied buffer.
pub fn drive_quick_xml_with_in<R, S, H>(
    reader: R,
    runtime: &mut ValidationRuntime<'_, S>,
    schema_set: &SchemaSet,
    handler: &mut H,
    buf: &mut Vec<u8>,
) -> Result<DriveOutcome, DriveWithError<H::Error>>
where
    R: BufRead,
    S: ValidationSink,
    H: ValidationEventHandler,
{
    let mut xml_reader = Reader::from_reader(reader);
    xml_reader.trim_text(false);

    // prefix bytes -> stack of URI strings (top-of-stack = current binding)
    let mut prefix_map: HashMap<Vec<u8>, Vec<String>> = HashMap::new();
    // Always-in-scope xml prefix (XML Namespaces §3).
    prefix_map
        .entry(b"xml".to_vec())
        .or_default()
        .push(XML_NAMESPACE.to_string());
    // Default-namespace seed; explicit declarations overwrite the top-of-stack.
    prefix_map.entry(Vec::new()).or_default().push(String::new());

    // Per-element list of prefixes that need popping at end-of-element.
    let mut scope_stack: Vec<Vec<Vec<u8>>> = Vec::new();

    let mut depth: usize = 0;
    let mut max_depth: usize = 0;
    let mut root_validity: Option<SchemaValidity> = None;

    buf.clear();

    loop {
        let event_start = xml_reader.buffer_position();
        match xml_reader.read_event_into(buf) {
            Ok(Event::Start(ref e)) => {
                handle_start_or_empty(
                    e,
                    false,
                    event_start,
                    &mut xml_reader,
                    runtime,
                    schema_set,
                    handler,
                    &mut prefix_map,
                    &mut scope_stack,
                    &mut depth,
                    &mut max_depth,
                    &mut root_validity,
                )?;
            }
            Ok(Event::Empty(ref e)) => {
                handle_start_or_empty(
                    e,
                    true,
                    event_start,
                    &mut xml_reader,
                    runtime,
                    schema_set,
                    handler,
                    &mut prefix_map,
                    &mut scope_stack,
                    &mut depth,
                    &mut max_depth,
                    &mut root_validity,
                )?;
            }
            Ok(Event::End(_)) => {
                let end_info = runtime.validate_end_element();
                let end = EndElementInfo {
                    validity: end_info.validity,
                };
                handler
                    .after_end_element(&end, depth)
                    .map_err(DriveWithError::Hook)?;
                let end_pos = xml_reader.buffer_position();
                handler
                    .on_element_end_offset(end_pos)
                    .map_err(DriveWithError::Hook)?;
                if depth == 1 {
                    root_validity = Some(end_info.validity);
                }
                pop_xmlns_scope(&mut prefix_map, &mut scope_stack);
                depth = depth.saturating_sub(1);
            }
            Ok(Event::Text(ref e)) if depth > 0 => {
                let text = e.unescape().map_err(DriveWithError::Parse)?;
                if text.chars().all(|c| c.is_whitespace()) {
                    runtime.validate_whitespace(&text);
                    handler
                        .on_text(TextKind::Whitespace, &text)
                        .map_err(DriveWithError::Hook)?;
                } else {
                    runtime.validate_text(&text);
                    handler
                        .on_text(TextKind::Character, &text)
                        .map_err(DriveWithError::Hook)?;
                }
            }
            Ok(Event::CData(ref e)) if depth > 0 => {
                let s = std::str::from_utf8(e.as_ref()).map_err(DriveWithError::Utf8)?;
                runtime.validate_text(s);
                handler
                    .on_text(TextKind::CData, s)
                    .map_err(DriveWithError::Hook)?;
            }
            Ok(Event::Text(_) | Event::CData(_)) => {
                // Outside any element — significant for neither validator nor handler.
            }
            Ok(Event::Comment(ref e)) => {
                let s = std::str::from_utf8(e.as_ref()).map_err(DriveWithError::Utf8)?;
                handler.on_comment(s).map_err(DriveWithError::Hook)?;
            }
            Ok(Event::PI(ref e)) => {
                let raw = std::str::from_utf8(e.as_ref()).map_err(DriveWithError::Utf8)?;
                let (target, data) = parse_pi_content(raw);
                handler
                    .on_processing_instruction(target, data)
                    .map_err(DriveWithError::Hook)?;
            }
            Ok(Event::Decl(_) | Event::DocType(_)) => {}
            Ok(Event::Eof) => {
                if depth != 0 {
                    return Err(DriveWithError::UnexpectedEof { depth });
                }
                break;
            }
            Err(e) => return Err(DriveWithError::Parse(e)),
        }
        buf.clear();
    }

    Ok(DriveOutcome {
        root_validity,
        max_depth,
    })
}

// ── Internals ─────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn handle_start_or_empty<R, S, H>(
    e: &BytesStart<'_>,
    is_empty: bool,
    event_start: usize,
    xml_reader: &mut Reader<R>,
    runtime: &mut ValidationRuntime<'_, S>,
    schema_set: &SchemaSet,
    handler: &mut H,
    prefix_map: &mut HashMap<Vec<u8>, Vec<String>>,
    scope_stack: &mut Vec<Vec<Vec<u8>>>,
    depth: &mut usize,
    max_depth: &mut usize,
    root_validity: &mut Option<SchemaValidity>,
) -> Result<(), DriveWithError<H::Error>>
where
    R: BufRead,
    S: ValidationSink,
    H: ValidationEventHandler,
{
    *depth += 1;
    if *depth > *max_depth {
        *max_depth = *depth;
    }

    // 1. Push xmlns scope, collect declarations as (prefix_string, uri_string).
    let ns_decls_owned = match push_xmlns_scope(e, prefix_map, scope_stack) {
        Ok(v) => v,
        Err(err) => {
            // Scope was not pushed, so just unwind depth.
            *depth -= 1;
            return Err(err);
        }
    };
    let ns_decls: Vec<(&str, &str)> = ns_decls_owned
        .iter()
        .map(|(p, u)| (p.as_str(), u.as_str()))
        .collect();

    // 2. Resolve element name + scan xsi:type / xsi:nil.
    let (local_name, namespace_uri, elem_prefix) = match resolve_element_qname(e, prefix_map) {
        Ok(v) => v,
        Err(err) => {
            pop_xmlns_scope(prefix_map, scope_stack);
            *depth -= 1;
            return Err(err);
        }
    };
    let (xsi_type, xsi_nil) = match scan_xsi_attributes(e, prefix_map) {
        Ok(v) => v,
        Err(err) => {
            pop_xmlns_scope(prefix_map, scope_stack);
            *depth -= 1;
            return Err(err);
        }
    };

    let ns_ctx = build_ns_context(prefix_map, schema_set);

    let view = ElementStartView {
        local_name: &local_name,
        namespace_uri: &namespace_uri,
        prefix: &elem_prefix,
        xsi_type: xsi_type.as_deref(),
        xsi_nil: xsi_nil.as_deref(),
        ns_context: &ns_ctx,
        namespace_decls: &ns_decls,
        is_empty,
    };

    // 3. Spans + before_element.
    if let Err(err) = handler.on_element_start_offset(event_start) {
        pop_xmlns_scope(prefix_map, scope_stack);
        *depth -= 1;
        return Err(DriveWithError::Hook(err));
    }
    if let Err(err) = handler.before_element(view) {
        pop_xmlns_scope(prefix_map, scope_stack);
        *depth -= 1;
        return Err(DriveWithError::Hook(err));
    }

    // 4. Element validation + after_element.
    let info = runtime.validate_element(
        &local_name,
        &namespace_uri,
        xsi_type.as_deref(),
        xsi_nil.as_deref(),
        &ns_ctx,
    );
    handler
        .after_element(view, &info)
        .map_err(DriveWithError::Hook)?;

    // 5. Attributes.
    for attr_result in e.attributes() {
        let attr = attr_result.map_err(|err| DriveWithError::Parse(err.into()))?;
        let key = attr.key.as_ref();
        if key == b"xmlns" || key.starts_with(b"xmlns:") {
            continue;
        }
        let (prefix_bytes, local_bytes) = split_prefix_local(key);
        let attr_local = std::str::from_utf8(local_bytes).map_err(DriveWithError::Utf8)?;
        let attr_prefix = std::str::from_utf8(prefix_bytes).map_err(DriveWithError::Utf8)?;
        let attr_ns = if prefix_bytes.is_empty() {
            String::new()
        } else {
            match prefix_map
                .get(prefix_bytes)
                .and_then(|stack| stack.last())
            {
                Some(uri) => uri.clone(),
                None => return Err(DriveWithError::UnboundPrefix(attr_prefix.to_string())),
            }
        };
        let value = attr
            .unescape_value()
            .map_err(DriveWithError::Parse)?;

        let av = AttributeView {
            local_name: attr_local,
            namespace_uri: &attr_ns,
            prefix: attr_prefix,
            value: value.as_ref(),
        };
        handler.before_attribute(av).map_err(DriveWithError::Hook)?;
        let attr_info = runtime.validate_attribute(attr_local, &attr_ns, value.as_ref());
        handler
            .after_attribute(av, &attr_info)
            .map_err(DriveWithError::Hook)?;
    }

    // 6. End-of-attributes.
    let eoa_info = runtime.validate_end_of_attributes();
    #[cfg(feature = "xsd11")]
    let deferred = runtime.take_deferred_attribute_results();
    let eoa_view = EndOfAttributesView {
        info: &eoa_info,
        #[cfg(feature = "xsd11")]
        deferred_attribute_results: &deferred,
    };
    handler
        .after_end_of_attributes(eoa_view)
        .map_err(DriveWithError::Hook)?;

    // 7. For empty elements, close inline.
    if is_empty {
        let end_info = runtime.validate_end_element();
        let end = EndElementInfo {
            validity: end_info.validity,
        };
        handler
            .after_end_element(&end, *depth)
            .map_err(DriveWithError::Hook)?;
        let end_pos = xml_reader.buffer_position();
        handler
            .on_element_end_offset(end_pos)
            .map_err(DriveWithError::Hook)?;
        if *depth == 1 {
            *root_validity = Some(end_info.validity);
        }
        pop_xmlns_scope(prefix_map, scope_stack);
        *depth -= 1;
    }

    Ok(())
}

/// Collect xmlns / xmlns:* declarations on `e` into `prefix_map`. Returns
/// the (prefix, uri) declarations so they can be exposed to handlers.
fn push_xmlns_scope<E>(
    e: &BytesStart<'_>,
    prefix_map: &mut HashMap<Vec<u8>, Vec<String>>,
    scope_stack: &mut Vec<Vec<Vec<u8>>>,
) -> Result<Vec<(String, String)>, DriveWithError<E>> {
    let mut declared: Vec<Vec<u8>> = Vec::new();
    let mut decls_owned: Vec<(String, String)> = Vec::new();

    for attr_result in e.attributes() {
        let attr = attr_result.map_err(|err| DriveWithError::Parse(err.into()))?;
        let key = attr.key.as_ref();
        let (prefix_bytes, prefix_str) = if key == b"xmlns" {
            (Vec::new(), String::new())
        } else if let Some(rest) = key.strip_prefix(b"xmlns:") {
            let prefix_str = std::str::from_utf8(rest)
                .map_err(DriveWithError::Utf8)?
                .to_string();
            (rest.to_vec(), prefix_str)
        } else {
            continue;
        };
        let value = attr.unescape_value().map_err(DriveWithError::Parse)?;
        let uri = value.into_owned();
        prefix_map
            .entry(prefix_bytes.clone())
            .or_default()
            .push(uri.clone());
        declared.push(prefix_bytes);
        decls_owned.push((prefix_str, uri));
    }

    scope_stack.push(declared);
    Ok(decls_owned)
}

fn pop_xmlns_scope(
    prefix_map: &mut HashMap<Vec<u8>, Vec<String>>,
    scope_stack: &mut Vec<Vec<Vec<u8>>>,
) {
    if let Some(declared) = scope_stack.pop() {
        for prefix in declared {
            if let Some(stack) = prefix_map.get_mut(&prefix) {
                stack.pop();
                if stack.is_empty() {
                    prefix_map.remove(&prefix);
                }
            }
        }
    }
}

fn resolve_element_qname<E>(
    e: &BytesStart<'_>,
    prefix_map: &HashMap<Vec<u8>, Vec<String>>,
) -> Result<(String, String, String), DriveWithError<E>> {
    let name = e.name();
    let (prefix_bytes, local_bytes) = split_prefix_local(name.as_ref());
    let local = std::str::from_utf8(local_bytes)
        .map_err(DriveWithError::Utf8)?
        .to_string();
    let prefix = std::str::from_utf8(prefix_bytes)
        .map_err(DriveWithError::Utf8)?
        .to_string();
    let namespace = if prefix_bytes.is_empty() {
        prefix_map
            .get(prefix_bytes)
            .and_then(|stack| stack.last())
            .cloned()
            .unwrap_or_default()
    } else {
        match prefix_map.get(prefix_bytes).and_then(|stack| stack.last()) {
            Some(uri) => uri.clone(),
            None => return Err(DriveWithError::UnboundPrefix(prefix)),
        }
    };
    Ok((local, namespace, prefix))
}

fn scan_xsi_attributes<E>(
    e: &BytesStart<'_>,
    prefix_map: &HashMap<Vec<u8>, Vec<String>>,
) -> Result<(Option<String>, Option<String>), DriveWithError<E>> {
    let mut xsi_type: Option<String> = None;
    let mut xsi_nil: Option<String> = None;
    for attr_result in e.attributes() {
        let attr = attr_result.map_err(|err| DriveWithError::Parse(err.into()))?;
        let key = attr.key.as_ref();
        if key == b"xmlns" || key.starts_with(b"xmlns:") {
            continue;
        }
        let (prefix_bytes, local_bytes) = split_prefix_local(key);
        if prefix_bytes.is_empty() {
            continue;
        }
        let ns_uri = match prefix_map.get(prefix_bytes).and_then(|s| s.last()) {
            Some(uri) => uri.as_str(),
            None => continue,
        };
        if ns_uri != XSI_NAMESPACE {
            continue;
        }
        let local = std::str::from_utf8(local_bytes).map_err(DriveWithError::Utf8)?;
        let value = attr.unescape_value().map_err(DriveWithError::Parse)?;
        match local {
            "type" => xsi_type = Some(value.into_owned()),
            "nil" => xsi_nil = Some(value.into_owned()),
            _ => {}
        }
    }
    Ok((xsi_type, xsi_nil))
}

fn build_ns_context(
    prefix_map: &HashMap<Vec<u8>, Vec<String>>,
    schema_set: &SchemaSet,
) -> NamespaceContextSnapshot {
    let mut snapshot = NamespaceContextSnapshot::default();

    for (prefix_bytes, stack) in prefix_map {
        let uri = match stack.last() {
            Some(s) => s,
            None => continue,
        };
        if prefix_bytes.is_empty() {
            // Default namespace; skip the empty seed binding.
            if uri.is_empty() {
                continue;
            }
            snapshot.default_ns = Some(schema_set.name_table.add(uri));
        } else if let Ok(prefix_str) = std::str::from_utf8(prefix_bytes) {
            // Skip the always-in-scope xml prefix and any xmlns binding —
            // the runtime treats them as implicit.
            if prefix_str == "xml" || prefix_str == "xmlns" || uri.is_empty() {
                continue;
            }
            let prefix_id = schema_set.name_table.add(prefix_str);
            let uri_id = schema_set.name_table.add(uri);
            snapshot.bindings.push((prefix_id, uri_id));
        }
    }

    snapshot
}

fn split_prefix_local(name: &[u8]) -> (&[u8], &[u8]) {
    match name.iter().position(|&b| b == b':') {
        Some(pos) => (&name[..pos], &name[pos + 1..]),
        None => (b"", name),
    }
}

fn parse_pi_content(raw: &str) -> (&str, &str) {
    let trimmed = raw.trim();
    match trimmed.find(|c: char| c.is_ascii_whitespace()) {
        Some(pos) => (&trimmed[..pos], trimmed[pos..].trim_start()),
        None => (trimmed, ""),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::load_and_process_schema;
    use crate::validation::{
        CollectingValidationSink, SchemaValidator, ValidationFlags, ValidationWarning,
    };

    fn load_schema(xsd: &str) -> SchemaSet {
        let mut ss = SchemaSet::new();
        load_and_process_schema(xsd.as_bytes(), "test.xsd", &mut ss, None)
            .expect("schema parse");
        ss
    }

    fn run(xsd: &str, instance: &str) -> (DriveOutcome, Vec<String>) {
        let schema_set = load_schema(xsd);
        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut errors = Vec::new();
        let mut warnings: Vec<ValidationWarning> = Vec::new();
        let sink = CollectingValidationSink {
            errors: &mut errors,
            warnings: &mut warnings,
        };
        let mut runtime = validator.start_run(sink);
        let outcome = drive_quick_xml(instance.as_bytes(), &mut runtime, &schema_set)
            .expect("drive failed");
        (outcome, errors.iter().map(|e| e.to_string()).collect())
    }

    #[test]
    fn simple_valid_root() {
        let (outcome, errors) = run(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root" type="xs:string"/>
            </xs:schema>"#,
            "<root>hello</root>",
        );
        assert!(errors.is_empty(), "expected no errors, got {errors:?}");
        assert!(matches!(outcome.root_validity, Some(SchemaValidity::Valid)));
        assert_eq!(outcome.max_depth, 1);
    }

    #[test]
    fn empty_root_element() {
        let (outcome, _errors) = run(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root">
                    <xs:complexType/>
                </xs:element>
            </xs:schema>"#,
            "<root/>",
        );
        assert!(matches!(outcome.root_validity, Some(SchemaValidity::Valid)));
        assert_eq!(outcome.max_depth, 1);
    }

    #[test]
    fn unexpected_eof_open_element() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root" type="xs:string"/>
            </xs:schema>"#,
        );
        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut errors = Vec::new();
        let mut warnings: Vec<ValidationWarning> = Vec::new();
        let sink = CollectingValidationSink {
            errors: &mut errors,
            warnings: &mut warnings,
        };
        let mut runtime = validator.start_run(sink);
        // Truncated stream: open tag with no close.
        let res = drive_quick_xml("<root>".as_bytes(), &mut runtime, &schema_set);
        match res {
            Err(DriveError::UnexpectedEof { depth }) => assert_eq!(depth, 1),
            other => panic!("expected UnexpectedEof, got {other:?}"),
        }
    }

    #[test]
    fn unbound_attribute_prefix_errors() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root">
                    <xs:complexType>
                        <xs:anyAttribute processContents="skip"/>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"#,
        );
        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut errors = Vec::new();
        let mut warnings: Vec<ValidationWarning> = Vec::new();
        let sink = CollectingValidationSink {
            errors: &mut errors,
            warnings: &mut warnings,
        };
        let mut runtime = validator.start_run(sink);
        let res = drive_quick_xml(
            r#"<root nope:x="1"/>"#.as_bytes(),
            &mut runtime,
            &schema_set,
        );
        match res {
            Err(DriveError::UnboundPrefix(p)) => assert_eq!(p, "nope"),
            other => panic!("expected UnboundPrefix, got {other:?}"),
        }
    }

    #[test]
    fn buffer_reuse_across_calls() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root" type="xs:string"/>
            </xs:schema>"#,
        );
        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut buf = Vec::new();
        for _ in 0..2 {
            let mut errors = Vec::new();
            let mut warnings: Vec<ValidationWarning> = Vec::new();
            let sink = CollectingValidationSink {
                errors: &mut errors,
                warnings: &mut warnings,
            };
            let mut runtime = validator.start_run(sink);
            let outcome = drive_quick_xml_in(
                "<root>x</root>".as_bytes(),
                &mut runtime,
                &schema_set,
                &mut buf,
            )
            .expect("drive ok");
            assert!(matches!(outcome.root_validity, Some(SchemaValidity::Valid)));
            assert!(errors.is_empty());
        }
    }

    #[test]
    fn comment_and_pi_forwarded_to_handler() {
        struct Capture {
            comments: Vec<String>,
            pis: Vec<(String, String)>,
        }
        impl ValidationEventHandler for Capture {
            type Error = Infallible;
            fn on_comment(&mut self, text: &str) -> Result<(), Self::Error> {
                self.comments.push(text.to_string());
                Ok(())
            }
            fn on_processing_instruction(
                &mut self,
                target: &str,
                data: &str,
            ) -> Result<(), Self::Error> {
                self.pis.push((target.to_string(), data.to_string()));
                Ok(())
            }
        }

        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root">
                    <xs:complexType/>
                </xs:element>
            </xs:schema>"#,
        );
        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut errors = Vec::new();
        let mut warnings: Vec<ValidationWarning> = Vec::new();
        let sink = CollectingValidationSink {
            errors: &mut errors,
            warnings: &mut warnings,
        };
        let mut runtime = validator.start_run(sink);

        let mut handler = Capture {
            comments: Vec::new(),
            pis: Vec::new(),
        };
        let _ = drive_quick_xml_with(
            "<root><!-- hi --><?pi target data?></root>".as_bytes(),
            &mut runtime,
            &schema_set,
            &mut handler,
        )
        .expect("drive ok");
        assert_eq!(handler.comments, vec![" hi ".to_string()]);
        assert_eq!(handler.pis, vec![("pi".to_string(), "target data".to_string())]);
    }

    #[test]
    fn span_offsets_bracket_each_element() {
        struct Spans {
            spans: Vec<(usize, usize)>,
            stack: Vec<usize>,
        }
        impl ValidationEventHandler for Spans {
            type Error = Infallible;
            fn on_element_start_offset(&mut self, byte_pos: usize) -> Result<(), Self::Error> {
                self.stack.push(byte_pos);
                Ok(())
            }
            fn on_element_end_offset(&mut self, byte_pos: usize) -> Result<(), Self::Error> {
                let start = self.stack.pop().expect("balanced span stack");
                self.spans.push((start, byte_pos));
                Ok(())
            }
        }

        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="b" type="xs:string"/>
                        </xs:sequence>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"#,
        );
        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut errors = Vec::new();
        let mut warnings: Vec<ValidationWarning> = Vec::new();
        let sink = CollectingValidationSink {
            errors: &mut errors,
            warnings: &mut warnings,
        };
        let mut runtime = validator.start_run(sink);

        let mut handler = Spans {
            spans: Vec::new(),
            stack: Vec::new(),
        };
        let xml = "<root><b>hi</b></root>";
        drive_quick_xml_with(xml.as_bytes(), &mut runtime, &schema_set, &mut handler)
            .expect("drive ok");
        // Inner <b> closes before <root>; spans are captured in close order.
        assert_eq!(handler.spans.len(), 2);
        for (start, end) in &handler.spans {
            assert!(end > start);
            assert!(*end <= xml.len());
        }
    }
}
