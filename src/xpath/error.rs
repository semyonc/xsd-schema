//! XPath error types.
//!
//! This module defines XPath 2.0 specification error codes as per the W3C XPath 2.0
//! specification. Error codes follow the pattern:
//! - XPST: Static errors detected during parsing/analysis
//! - XPDY: Dynamic errors detected during evaluation
//! - XPTY: Type errors
//! - XQTY: XQuery type errors
//! - FORG: Function and operators errors (general)
//! - FOAR: Arithmetic errors
//! - FOCA: Casting errors
//! - FONS: Namespace errors
//! - FODT: Date/time errors

use thiserror::Error;

/// XPath-specific error type with W3C specification error codes.
#[derive(Debug, Clone, Error)]
pub enum XPathError {
    // ========================================================================
    // Static Errors (XPST)
    // ========================================================================
    /// XPST0003: Syntax error in expression.
    #[error("[XPST0003] Syntax error: {message}")]
    XPST0003 { message: String },

    /// XPST0008: QName is not defined (undefined variable or type).
    #[error("[XPST0008] QName '{qname}' is not defined")]
    XPST0008 { qname: String },

    /// XPST0051: Unknown atomic type in sequence type.
    #[error("[XPST0051] The type name '{type_name}' is not defined as an atomic type")]
    XPST0051 { type_name: String },

    /// XPST0017: Function not found.
    #[error("[XPST0017] Function '{name}/{arity}' not found in namespace '{namespace}'")]
    XPST0017 {
        name: String,
        arity: usize,
        namespace: String,
    },

    /// XPST0081: Prefix cannot be expanded to namespace URI.
    #[error("[XPST0081] Prefix '{prefix}' cannot be expanded to a namespace URI")]
    XPST0081 { prefix: String },

    // ========================================================================
    // Dynamic Errors (XPDY)
    // ========================================================================
    /// XPDY0002: Context item is undefined.
    #[error("[XPDY0002] The context item is undefined")]
    XPDY0002,

    /// XPDY0050: More than one item where singleton expected.
    #[error("[XPDY0050] More than one item in sequence where single item expected")]
    XPDY0050,

    // ========================================================================
    // Type Errors (XPTY)
    // ========================================================================
    /// XPTY0004: Type mismatch.
    #[error("[XPTY0004] Type mismatch: expected '{expected}', found '{found}'")]
    XPTY0004 { expected: String, found: String },

    /// XPTY0004 variant: Only string literals can be cast to certain types.
    #[error("[XPTY0004] Only string literals can be cast to type '{target_type}'")]
    XPTY0004Cast { target_type: String },

    /// XPTY0018: Path expression result contains both nodes and atomic values.
    #[error("[XPTY0018] Path expression result contains both nodes and atomic values")]
    XPTY0018,

    /// XPTY0019: Step result must not be atomic value in path expression.
    #[error("[XPTY0019] Step result in path expression must not be an atomic value")]
    XPTY0019,

    // ========================================================================
    // XQuery Type Errors (XQTY)
    // ========================================================================
    /// XQTY0030: Validate expression argument must be single document or element.
    #[error("[XQTY0030] Validate expression argument must be exactly one document or element node")]
    XQTY0030,

    // ========================================================================
    // Function Errors - General (FORG)
    // ========================================================================
    /// FORG0001: Invalid value for cast/constructor.
    #[error("[FORG0001] Invalid value '{value}' for cast to type '{target_type}'")]
    FORG0001 { value: String, target_type: String },

    /// FORG0003: fn:zero-or-one called with sequence > 1 item.
    #[error("[FORG0003] fn:zero-or-one called with sequence containing more than one item")]
    FORG0003,

    /// FORG0004: fn:one-or-more called with empty sequence.
    #[error("[FORG0004] fn:one-or-more called with empty sequence")]
    FORG0004,

    /// FORG0005: fn:exactly-one called with wrong cardinality.
    #[error("[FORG0005] fn:exactly-one called with sequence containing zero or more than one item")]
    FORG0005,

    /// FORG0006: Invalid argument type for function.
    #[error("[FORG0006] Function '{function}' called with invalid argument type '{arg_type}'")]
    FORG0006 { function: String, arg_type: String },

    // ========================================================================
    // Arithmetic Errors (FOAR)
    // ========================================================================
    /// FOAR0001: Division by zero.
    #[error("[FOAR0001] Division by zero")]
    FOAR0001,

    /// FOAR0002: Numeric overflow or underflow.
    #[error("[FOAR0002] Numeric operation overflow/underflow")]
    FOAR0002,

    // ========================================================================
    // Casting Errors (FOCA)
    // ========================================================================
    /// FOCA0002: QName has null namespace but non-empty prefix.
    #[error("[FOCA0002] QName '{qname}' has null namespace but non-empty prefix")]
    FOCA0002 { qname: String },

    /// FOCA0005: NaN supplied as float/double value.
    #[error("[FOCA0005] NaN supplied as float/double value")]
    FOCA0005,

    // ========================================================================
    // Date/Time Errors (FODT)
    // ========================================================================
    /// FODT0001: Overflow/underflow in date/time operation.
    #[error("[FODT0001] Overflow/underflow in date/time operation")]
    FODT0001,

    /// FODT0002: Overflow/underflow in duration operation.
    #[error("[FODT0002] Overflow/underflow in duration operation")]
    FODT0002,

    /// FODT0003: Invalid timezone value.
    #[error("[FODT0003] Invalid timezone value: {value}")]
    FODT0003 { value: String },

    // ========================================================================
    // Operator Errors
    // ========================================================================
    /// Binary operator not defined for argument types.
    #[error("Operator '{operator}' is not defined for arguments of type '{left_type}' and '{right_type}'")]
    BinaryOperatorNotDefined {
        operator: String,
        left_type: String,
        right_type: String,
    },

    /// Unary operator not defined for argument type.
    #[error("Operator '{operator}' is not defined for argument of type '{arg_type}'")]
    UnaryOperatorNotDefined { operator: String, arg_type: String },

    // ========================================================================
    // Internal/General Errors
    // ========================================================================
    /// Internal error for unexpected failures.
    #[error("XPath error: {0}")]
    Internal(String),
}

impl XPathError {
    // ========================================================================
    // Convenience Constructors
    // ========================================================================

    /// Create a new internal XPath error.
    pub fn internal(message: impl Into<String>) -> Self {
        XPathError::Internal(message.into())
    }

    /// Create XPST0003 syntax error.
    pub fn syntax_error(message: impl Into<String>) -> Self {
        XPathError::XPST0003 {
            message: message.into(),
        }
    }

    /// Create XPST0008 undefined QName error.
    pub fn undefined_qname(qname: impl Into<String>) -> Self {
        XPathError::XPST0008 {
            qname: qname.into(),
        }
    }

    /// Create XPST0051 unknown type error.
    pub fn unknown_type(type_name: impl Into<String>) -> Self {
        XPathError::XPST0051 {
            type_name: type_name.into(),
        }
    }

    /// Create XPST0017 function not found error.
    pub fn function_not_found(
        name: impl Into<String>,
        arity: usize,
        namespace: impl Into<String>,
    ) -> Self {
        XPathError::XPST0017 {
            name: name.into(),
            arity,
            namespace: namespace.into(),
        }
    }

    /// Create XPST0081 undefined prefix error.
    pub fn undefined_prefix(prefix: impl Into<String>) -> Self {
        XPathError::XPST0081 {
            prefix: prefix.into(),
        }
    }

    /// Create XPDY0002 context undefined error.
    pub fn context_undefined() -> Self {
        XPathError::XPDY0002
    }

    /// Create XPDY0050 more than one item error.
    pub fn more_than_one_item() -> Self {
        XPathError::XPDY0050
    }

    /// Create XPTY0004 type mismatch error.
    pub fn type_mismatch(expected: impl Into<String>, found: impl Into<String>) -> Self {
        XPathError::XPTY0004 {
            expected: expected.into(),
            found: found.into(),
        }
    }

    /// Create XPTY0004 cast-only-from-string error.
    pub fn cast_requires_string_literal(target_type: impl Into<String>) -> Self {
        XPathError::XPTY0004Cast {
            target_type: target_type.into(),
        }
    }

    /// Create FORG0001 invalid cast value error.
    pub fn invalid_cast_value(value: impl Into<String>, target_type: impl Into<String>) -> Self {
        XPathError::FORG0001 {
            value: value.into(),
            target_type: target_type.into(),
        }
    }

    /// Create FORG0006 invalid argument type error.
    pub fn invalid_argument_type(function: impl Into<String>, arg_type: impl Into<String>) -> Self {
        XPathError::FORG0006 {
            function: function.into(),
            arg_type: arg_type.into(),
        }
    }

    /// Create binary operator not defined error.
    pub fn binary_operator_not_defined(
        operator: impl Into<String>,
        left_type: impl Into<String>,
        right_type: impl Into<String>,
    ) -> Self {
        XPathError::BinaryOperatorNotDefined {
            operator: operator.into(),
            left_type: left_type.into(),
            right_type: right_type.into(),
        }
    }

    /// Create unary operator not defined error.
    pub fn unary_operator_not_defined(
        operator: impl Into<String>,
        arg_type: impl Into<String>,
    ) -> Self {
        XPathError::UnaryOperatorNotDefined {
            operator: operator.into(),
            arg_type: arg_type.into(),
        }
    }

    /// Get the error code (e.g., "XPTY0004") if this is a spec-defined error.
    pub fn error_code(&self) -> Option<&'static str> {
        match self {
            XPathError::XPST0003 { .. } => Some("XPST0003"),
            XPathError::XPST0008 { .. } => Some("XPST0008"),
            XPathError::XPST0051 { .. } => Some("XPST0051"),
            XPathError::XPST0017 { .. } => Some("XPST0017"),
            XPathError::XPST0081 { .. } => Some("XPST0081"),
            XPathError::XPDY0002 => Some("XPDY0002"),
            XPathError::XPDY0050 => Some("XPDY0050"),
            XPathError::XPTY0004 { .. } => Some("XPTY0004"),
            XPathError::XPTY0004Cast { .. } => Some("XPTY0004"),
            XPathError::XPTY0018 => Some("XPTY0018"),
            XPathError::XPTY0019 => Some("XPTY0019"),
            XPathError::XQTY0030 => Some("XQTY0030"),
            XPathError::FORG0001 { .. } => Some("FORG0001"),
            XPathError::FORG0003 => Some("FORG0003"),
            XPathError::FORG0004 => Some("FORG0004"),
            XPathError::FORG0005 => Some("FORG0005"),
            XPathError::FORG0006 { .. } => Some("FORG0006"),
            XPathError::FOAR0001 => Some("FOAR0001"),
            XPathError::FOAR0002 => Some("FOAR0002"),
            XPathError::FOCA0002 { .. } => Some("FOCA0002"),
            XPathError::FOCA0005 => Some("FOCA0005"),
            XPathError::FODT0001 => Some("FODT0001"),
            XPathError::FODT0002 => Some("FODT0002"),
            XPathError::FODT0003 { .. } => Some("FODT0003"),
            XPathError::BinaryOperatorNotDefined { .. } => None,
            XPathError::UnaryOperatorNotDefined { .. } => None,
            XPathError::Internal(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_codes() {
        assert_eq!(XPathError::context_undefined().error_code(), Some("XPDY0002"));
        assert_eq!(XPathError::more_than_one_item().error_code(), Some("XPDY0050"));
        assert_eq!(
            XPathError::type_mismatch("xs:integer", "xs:string").error_code(),
            Some("XPTY0004")
        );
    }

    #[test]
    fn test_error_display() {
        let err = XPathError::type_mismatch("xs:integer", "xs:string");
        assert!(err.to_string().contains("XPTY0004"));
        assert!(err.to_string().contains("xs:integer"));
        assert!(err.to_string().contains("xs:string"));
    }

    #[test]
    fn test_internal_error() {
        let err = XPathError::internal("something went wrong");
        assert!(err.to_string().contains("something went wrong"));
        assert_eq!(err.error_code(), None);
    }
}
