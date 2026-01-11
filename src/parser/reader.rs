//! Tracked XML reader
//!
//! Wraps quick-xml Reader with byte position tracking for source mapping.

use quick_xml::events::Event;
use quick_xml::Reader;
use std::io::BufRead;

use crate::parser::location::SourceSpan;
use crate::error::{SchemaError, SchemaResult};

/// XML event with source span
#[derive(Debug)]
pub struct TrackedEvent<'a> {
    /// The XML event
    pub event: Event<'a>,
    /// Byte span in source
    pub span: SourceSpan,
}

impl<'a> TrackedEvent<'a> {
    /// Create a new tracked event
    pub fn new(event: Event<'a>, span: SourceSpan) -> Self {
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
pub struct TrackedReader<R> {
    /// The underlying quick-xml reader
    reader: Reader<R>,
    /// Current buffer position (before event)
    last_position: usize,
}

impl<'a> TrackedReader<&'a [u8]> {
    /// Create a new reader from a byte slice
    pub fn from_bytes(bytes: &'a [u8]) -> Self {
        let mut reader = Reader::from_reader(bytes);
        reader.trim_text(true);

        Self {
            reader,
            last_position: 0,
        }
    }
}

impl<R: BufRead> TrackedReader<R> {
    /// Create a new reader from a BufRead source
    pub fn from_reader(reader: R) -> Self {
        let mut xml_reader = Reader::from_reader(reader);
        xml_reader.trim_text(true);

        Self {
            reader: xml_reader,
            last_position: 0,
        }
    }

    /// Read the next XML event with its source span
    pub fn read_event<'b>(&mut self, buf: &'b mut Vec<u8>) -> SchemaResult<TrackedEvent<'b>> {
        let start = self.reader.buffer_position();
        self.last_position = start;

        let event = self.reader.read_event_into(buf).map_err(|e| {
            SchemaError::XmlError {
                message: e.to_string(),
                location: None, // Will be filled in by caller with proper source mapping
            }
        })?;

        let end = self.reader.buffer_position();
        let span = SourceSpan { start, end };

        Ok(TrackedEvent::new(event, span))
    }

    /// Get the current buffer position
    pub fn buffer_position(&self) -> usize {
        self.reader.buffer_position()
    }

    /// Get the last event's start position
    pub fn last_position(&self) -> usize {
        self.last_position
    }

    /// Get a reference to the underlying reader for decoding
    pub fn inner(&self) -> &Reader<R> {
        &self.reader
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

    #[test]
    fn test_split_qname() {
        assert_eq!(split_qname(b"localName"), (&b"localName"[..], None));
        assert_eq!(
            split_qname(b"xs:element"),
            (&b"element"[..], Some(&b"xs"[..]))
        );
        assert_eq!(
            split_qname(b"xsi:nil"),
            (&b"nil"[..], Some(&b"xsi"[..]))
        );
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
