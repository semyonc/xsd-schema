use thiserror::Error;

/// Errors that can occur during `BufferDocument` construction or navigation.
#[derive(Debug, Error)]
pub enum BufferDocumentError {
    #[error("XML parsing error: {0}")]
    Parse(#[from] quick_xml::Error),

    #[error("Invalid UTF-8 in XML: {0}")]
    Utf8(#[from] std::str::Utf8Error),

    #[error("Namespace resolution error: prefix '{0}' not bound")]
    UnboundPrefix(String),

    #[error("Duplicate ID value: '{0}'")]
    DuplicateId(String),

    #[error("Node allocation overflow (exceeds u32::MAX - 1 nodes)")]
    Overflow,

    #[error("end_element called without matching start_element")]
    UnmatchedEndElement,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_unbound_prefix() {
        let err = BufferDocumentError::UnboundPrefix("ns1".into());
        assert_eq!(
            err.to_string(),
            "Namespace resolution error: prefix 'ns1' not bound"
        );
    }

    #[test]
    fn display_duplicate_id() {
        let err = BufferDocumentError::DuplicateId("myId".into());
        assert_eq!(err.to_string(), "Duplicate ID value: 'myId'");
    }

    #[test]
    fn display_overflow() {
        let err = BufferDocumentError::Overflow;
        assert_eq!(
            err.to_string(),
            "Node allocation overflow (exceeds u32::MAX - 1 nodes)"
        );
    }

    #[test]
    fn from_quick_xml_error() {
        let qx_err = quick_xml::Error::TextNotFound;
        let err: BufferDocumentError = qx_err.into();
        assert!(matches!(err, BufferDocumentError::Parse(_)));
        assert!(err.to_string().contains("XML parsing error"));
    }

    #[test]
    fn from_utf8_error() {
        // Create a real Utf8Error by decoding invalid bytes at runtime
        let bad: &[u8] = &[0xFF, 0xFE];
        #[allow(invalid_from_utf8)]
        let utf8_err = std::str::from_utf8(bad).unwrap_err();
        let err: BufferDocumentError = utf8_err.into();
        assert!(matches!(err, BufferDocumentError::Utf8(_)));
        assert!(err.to_string().contains("Invalid UTF-8"));
    }
}
