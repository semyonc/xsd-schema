//! Tracked XML reader
//!
//! Reads a schema document through the crate's well-formedness layer, with
//! byte position tracking for source mapping.

use quick_xml::events::Event;
use std::io::BufRead;

use crate::error::{SchemaError, SchemaResult};
use crate::parser::location::SourceSpan;
use crate::xml::reader::{NextEvent, SliceWfReader, WfError, WfReader};
use crate::xml::ParseLimits;

/// XML event with source span
///
/// What kind of event it is ([`is_start`](Self::is_start) and the other
/// predicates) and where it is in the input ([`span`](Self::span)); the
/// event's content is the crate's own (the schema-document parser reads it).
#[derive(Debug)]
pub struct TrackedEvent<'a> {
    /// The XML event (quick-xml's, so not public: quick-xml's types are not
    /// part of this crate's API)
    pub(crate) event: Event<'a>,
    /// Byte span in source
    pub span: SourceSpan,
}

impl<'a> TrackedEvent<'a> {
    /// Create a new tracked event
    pub(crate) fn new(event: Event<'a>, span: SourceSpan) -> Self {
        Self { event, span }
    }

    /// Check if this is a start element event
    pub fn is_start(&self) -> bool {
        matches!(self.event, Event::Start(_))
    }

    /// Check if this is an empty element event
    pub fn is_empty(&self) -> bool {
        matches!(self.event, Event::Empty(_))
    }

    /// Check if this is an end element event
    pub fn is_end(&self) -> bool {
        matches!(self.event, Event::End(_))
    }

    /// Check if this is a text event
    pub fn is_text(&self) -> bool {
        matches!(self.event, Event::Text(_))
    }

    /// Check if this is an EOF event
    pub fn is_eof(&self) -> bool {
        matches!(self.event, Event::Eof)
    }
}

/// Tracked XML reader that wraps quick-xml with position tracking
///
/// This reader provides byte spans for all XML events, enabling accurate
/// source location tracking for error messages.
///
/// The text is read through the crate's well-formedness layer
/// ([`crate::xml`]): a document that is not well-formed is an
/// [`SchemaError::XmlError`] naming the rule and the byte position (also
/// available from [`error_position`](Self::error_position)) — input that ends
/// inside an element included. The rules are XML 1.1's for a document
/// declared `version="1.1"` ([`xml_version`](Self::xml_version)). White space
/// before character data and markup is skipped and text is trimmed at both
/// ends, as before. Spans are byte offsets into the input, a leading BOM
/// included.
///
/// Pass the same buffer to every [`read_event`](Self::read_event): the case
/// of a document type declaration's keyword is checked on its markup, still
/// in the buffer, at the next read (a caller that clears the buffer in
/// between skips that one check).
pub struct TrackedReader<R> {
    inner: Tracked<WfReader<R>>,
}

impl<'a> TrackedReader<&'a [u8]> {
    /// Create a new reader from a byte slice
    ///
    /// The events borrow `buf` ([`read_event`](Self::read_event)), so this
    /// reader copies each one from the slice into the buffer, as for any
    /// other source.
    pub fn from_bytes(bytes: &'a [u8]) -> Self {
        TrackedReader::from_reader(bytes)
    }
}

impl<R: BufRead> TrackedReader<R> {
    /// Create a new reader from a BufRead source
    pub fn from_reader(reader: R) -> Self {
        Self {
            inner: Tracked::new(WfReader::new(reader, &ParseLimits::none(), true)),
        }
    }

    /// Read the next XML event with its source span
    ///
    /// Offsets are input offsets, a leading byte-order mark counted. The span
    /// starts where the event starts: the `<` of a start or empty-element
    /// tag, an XML declaration, comment, processing instruction or CDATA
    /// section, the first character of a text run (after the white space
    /// `trim_text` skips), the `&` of a reference. An end tag's or a
    /// document type declaration's span starts where the previous event
    /// ended — before white space the read skipped — as their length does
    /// not tell where their `<` is.
    pub fn read_event<'b>(&mut self, buf: &'b mut Vec<u8>) -> SchemaResult<TrackedEvent<'b>> {
        self.inner.read_event(buf)
    }

    /// Get the current buffer position
    pub fn buffer_position(&self) -> usize {
        self.inner.buffer_position()
    }

    /// The start of the last event returned: its span's `start`
    pub fn last_position(&self) -> usize {
        self.inner.last_position
    }

    /// The input offset (a leading byte-order mark included) of the
    /// violation behind the last well-formedness error
    /// [`read_event`](Self::read_event) returned, if any.
    pub fn error_position(&self) -> Option<usize> {
        self.inner.error_position
    }

    /// The version of XML whose rules the document is read with (known once
    /// its XML declaration has been read).
    pub fn xml_version(&self) -> crate::xml::XmlVersion {
        self.inner.xml_version()
    }
}

/// What a [`TrackedReader`] is, over either source of the well-formedness
/// layer: the public reader over a buffered one, the schema-document parser
/// over the whole document as a slice (its events borrow the input).
pub(crate) struct Tracked<S> {
    /// The underlying quick-xml reader, behind the well-formedness layer
    reader: S,
    /// Start of the last event returned (see [`read_event`](Self::read_event))
    last_position: usize,
    /// Input offset of the last well-formedness error
    error_position: Option<usize>,
}

impl<'a> Tracked<SliceWfReader<'a>> {
    /// A reader over the whole document `bytes`, with `limits` (the
    /// schema-document parser's: only `max_depth`, from
    /// `ParserConfig::max_depth`; [`SchemaError::LimitExceeded`]). Otherwise
    /// as [`TrackedReader::from_reader`] over the same bytes: the same events,
    /// spans and errors.
    pub(crate) fn from_slice(bytes: &'a [u8], limits: &ParseLimits) -> Self {
        Tracked::new(SliceWfReader::from_slice(bytes, limits, true))
    }
}

impl<B: BufRead, W> Tracked<WfReader<B, W>> {
    fn new(reader: WfReader<B, W>) -> Self {
        // quick-xml skips the white space before text and markup (the reader
        // is made so); the end of a text event is trimmed in `read_event`.
        Self {
            reader,
            last_position: 0,
            error_position: None,
        }
    }

    /// See [`TrackedReader::read_event`].
    pub(crate) fn read_event<'i, 'b>(
        &mut self,
        buf: &'b mut Vec<u8>,
    ) -> SchemaResult<TrackedEvent<'b>>
    where
        WfReader<B, W>: NextEvent<'i>,
        'i: 'b,
    {
        let mut event = match self.reader.next_event(buf) {
            Ok(event) => event,
            Err(WfError::Io(e)) => {
                return Err(SchemaError::XmlError {
                    message: format!("I/O error: {e}"),
                    location: None, // Will be filled in by caller with proper source mapping
                });
            }
            Err(WfError::NotWellFormed(e)) => {
                self.error_position = Some(e.position() as usize);
                return Err(SchemaError::xml(e.to_string(), None));
            }
            Err(WfError::Limit(e)) => {
                self.error_position = Some(e.position() as usize);
                // Only the schema-document parser sets limits on this reader
                // (`Tracked::from_slice`), and only the depth limit, from
                // `ParserConfig::max_depth`.
                if e.limit() == crate::xml::ParseLimit::Depth {
                    return Err(SchemaError::limit(
                        crate::error::SchemaLimit::ElementDepth,
                        e.max(),
                        e.found(),
                        format!("element depth {}", e.found()),
                        None,
                    ));
                }
                return Err(SchemaError::xml(e.to_string(), None));
            }
        };
        // `trim_text`: white space at the end of a text run (the start was
        // skipped by the reader; a run of white space only never comes).
        if let Event::Text(t) = &mut event {
            t.inplace_trim_end();
        }

        let end = self.reader.buffer_position() as usize;
        // The layer records where it found the event (this reader never
        // enables `expand_empty_elements`, which would report `<a/>` as a
        // Start).
        let start = self.reader.event_start() as usize;
        self.last_position = start;
        let span = SourceSpan { start, end };

        Ok(TrackedEvent::new(event, span))
    }

    /// See [`TrackedReader::buffer_position`].
    pub(crate) fn buffer_position(&self) -> usize {
        self.reader.buffer_position() as usize
    }

    /// See [`TrackedReader::error_position`].
    pub(crate) fn error_position(&self) -> Option<usize> {
        self.error_position
    }

    /// See [`TrackedReader::xml_version`].
    pub(crate) fn xml_version(&self) -> crate::xml::XmlVersion {
        self.reader.xml_version()
    }
}

/// Extract local name and prefix from a qualified name
pub fn split_qname(qname: &[u8]) -> (&[u8], Option<&[u8]>) {
    match qname.iter().position(|&b| b == b':') {
        Some(pos) => (&qname[pos + 1..], Some(&qname[..pos])),
        None => (qname, None),
    }
}

/// Configuration for XML parsing
#[derive(Debug, Clone)]
pub struct ReaderConfig {
    /// Trim whitespace in text nodes
    pub trim_text: bool,
    /// Check for duplicate attributes
    pub check_duplicates: bool,
}

impl Default for ReaderConfig {
    fn default() -> Self {
        Self {
            trim_text: true,
            check_duplicates: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tracked_reader_basic() {
        let xml = b"<root><child/></root>";
        let mut reader = TrackedReader::from_bytes(xml);
        let mut buf = Vec::new();

        // First event: Start element <root>
        let event = reader.read_event(&mut buf).unwrap();
        assert!(event.is_start());

        // Second event: Empty element <child/>
        buf.clear();
        let event = reader.read_event(&mut buf).unwrap();
        assert!(event.is_empty());

        // Third event: End element </root>
        buf.clear();
        let event = reader.read_event(&mut buf).unwrap();
        assert!(event.is_end());

        // Fourth event: EOF
        buf.clear();
        let event = reader.read_event(&mut buf).unwrap();
        assert!(event.is_eof());
    }

    #[test]
    fn test_tracked_reader_spans() {
        let xml = b"<root>text</root>";
        let mut reader = TrackedReader::from_bytes(xml);
        let mut buf = Vec::new();

        // Start element span
        let event = reader.read_event(&mut buf).unwrap();
        assert!(event.span.start == 0);

        // Text span
        buf.clear();
        let event = reader.read_event(&mut buf).unwrap();
        assert!(event.is_text());
        assert!(event.span.start > 0);
    }

    /// An element's span starts at its `<`, not at the end of whatever event
    /// preceded it — the whitespace between the two is consumed by the same
    /// read.
    #[test]
    fn test_element_span_starts_at_open_angle_bracket() {
        let xml = b"<root>\n   <a x=\"1\"/>\n   <b>t</b>\n</root>";
        let mut reader = TrackedReader::from_bytes(xml);
        let mut buf = Vec::new();

        let event = reader.read_event(&mut buf).unwrap();
        assert!(event.is_start());
        assert_eq!((event.span.start, event.span.end), (0, 6), "<root>");

        buf.clear();
        let event = reader.read_event(&mut buf).unwrap();
        assert!(event.is_empty());
        let start = xml.windows(2).position(|w| w == b"<a").unwrap();
        assert_eq!(
            event.span.start, start,
            "empty element must start at its '<', not after </root>'s predecessor"
        );
        assert_eq!(&xml[event.span.start..event.span.end], b"<a x=\"1\"/>");

        buf.clear();
        let event = reader.read_event(&mut buf).unwrap();
        assert!(event.is_start());
        assert_eq!(&xml[event.span.start..event.span.end], b"<b>");
    }

    #[test]
    fn test_split_qname() {
        assert_eq!(split_qname(b"localName"), (&b"localName"[..], None));
        assert_eq!(
            split_qname(b"xs:element"),
            (&b"element"[..], Some(&b"xs"[..]))
        );
        assert_eq!(split_qname(b"xsi:nil"), (&b"nil"[..], Some(&b"xsi"[..])));
    }

    #[test]
    fn test_tracked_event_type_checks() {
        let xml = b"<root/>";
        let mut reader = TrackedReader::from_bytes(xml);
        let mut buf = Vec::new();

        let event = reader.read_event(&mut buf).unwrap();
        assert!(event.is_empty());
        assert!(!event.is_start());
        assert!(!event.is_end());
        assert!(!event.is_text());
        assert!(!event.is_eof());
    }

    #[test]
    fn test_reader_config_default() {
        let config = ReaderConfig::default();
        assert!(config.trim_text);
        assert!(config.check_duplicates);
    }
}
