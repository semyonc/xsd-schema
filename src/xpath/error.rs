//! XPath error types.

use thiserror::Error;

/// XPath-specific error type.
#[derive(Debug, Clone, Error)]
pub enum XPathError {
    /// Internal error for unexpected operator or type failures.
    #[error("XPath error: {0}")]
    Internal(String),
}

impl XPathError {
    /// Create a new internal XPath error.
    pub fn internal(message: impl Into<String>) -> Self {
        XPathError::Internal(message.into())
    }
}
