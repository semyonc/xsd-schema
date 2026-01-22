//! Validation error types for instance validation
//!
//! This module provides spec-aligned error codes for XML instance validation
//! against XSD schemas. Error codes follow the XSD specification anchors:
//!
//! - `cvc-*` - Instance validation constraints (e.g., `cvc-elt`, `cvc-type`)
//! - `cos-*` - Component constraints (e.g., `cos-valid-default`)
//! - `src-*` - Schema representation constraints (e.g., `src-element`)

use crate::error::FacetError;
use crate::parser::location::SourceLocation;
use crate::types::validators::ValidationError as TypeValidationError;

/// Instance validation error with spec-aligned constraint code
#[derive(Debug, Clone)]
pub struct ValidationError {
    /// Spec constraint code (cvc-*, cos-*, src-*)
    pub constraint: &'static str,
    /// Human-readable error message
    pub message: String,
    /// Source location in the instance document
    pub location: Option<SourceLocation>,
    /// XPath-like path to the element (e.g., "/root/child[1]")
    pub element_path: Option<String>,
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.constraint, self.message)?;
        if let Some(loc) = &self.location {
            write!(f, " at {}", loc)?;
        }
        if let Some(path) = &self.element_path {
            write!(f, " ({})", path)?;
        }
        Ok(())
    }
}

impl std::error::Error for ValidationError {}

/// Result type for instance validation operations
pub type ValidationResult<T> = Result<T, ValidationError>;

/// Create a validation error with the given constraint code and message
pub fn error(
    constraint: &'static str,
    message: impl Into<String>,
    location: Option<SourceLocation>,
) -> ValidationError {
    ValidationError {
        constraint,
        message: message.into(),
        location,
        element_path: None,
    }
}

/// Create a validation error with element path information
pub fn error_with_path(
    constraint: &'static str,
    message: impl Into<String>,
    location: Option<SourceLocation>,
    element_path: impl Into<String>,
) -> ValidationError {
    ValidationError {
        constraint,
        message: message.into(),
        location,
        element_path: Some(element_path.into()),
    }
}

/// Convert a type validation error to an instance validation error
///
/// Use this when a `types::validators::ValidationError` needs to be reported
/// with a specific cvc-* constraint code.
pub fn from_value_error(
    constraint: &'static str,
    err: TypeValidationError,
    location: Option<SourceLocation>,
) -> ValidationError {
    ValidationError {
        constraint,
        message: err.to_string(),
        location,
        element_path: None,
    }
}

/// Convert a facet error to an instance validation error
///
/// Use this when a `FacetError` needs to be reported with a specific cvc-* code.
/// Consider using `facet_constraint_code()` to get the appropriate code.
pub fn from_facet_error(
    constraint: &'static str,
    err: FacetError,
    location: Option<SourceLocation>,
) -> ValidationError {
    ValidationError {
        constraint,
        message: err.to_string(),
        location,
        element_path: None,
    }
}

/// Map a FacetError variant to its specific cvc-* constraint code
///
/// This function returns the most specific constraint code for each facet type,
/// preferring codes like `cvc-pattern-valid` over generic `cvc-facet-valid`.
///
/// # Mappings
///
/// | FacetError Variant | Constraint Code |
/// |--------------------|-----------------|
/// | LengthViolation | cvc-length-valid |
/// | MinLengthViolation | cvc-minLength-valid |
/// | MaxLengthViolation | cvc-maxLength-valid |
/// | PatternViolation | cvc-pattern-valid |
/// | EnumerationViolation | cvc-enumeration-valid |
/// | MinInclusiveViolation | cvc-minInclusive-valid |
/// | MaxInclusiveViolation | cvc-maxInclusive-valid |
/// | MinExclusiveViolation | cvc-minExclusive-valid |
/// | MaxExclusiveViolation | cvc-maxExclusive-valid |
/// | TotalDigitsViolation | cvc-totalDigits-valid |
/// | FractionDigitsViolation | cvc-fractionDigits-valid |
/// | ExplicitTimezoneViolation | cvc-explicitTimezone-valid |
/// | InvalidPattern | cvc-pattern-valid |
/// | DerivationRestriction | cos-st-restricts |
/// | FixedFacetViolation | cos-st-restricts |
/// | ConflictingFacets | cos-st-restricts |
/// | NotApplicable | cos-applicable-facets |
pub fn facet_constraint_code(err: &FacetError) -> &'static str {
    match err {
        FacetError::LengthViolation { .. } => "cvc-length-valid",
        FacetError::MinLengthViolation { .. } => "cvc-minLength-valid",
        FacetError::MaxLengthViolation { .. } => "cvc-maxLength-valid",
        FacetError::PatternViolation { .. } => "cvc-pattern-valid",
        FacetError::EnumerationViolation { .. } => "cvc-enumeration-valid",
        FacetError::MinInclusiveViolation { .. } => "cvc-minInclusive-valid",
        FacetError::MaxInclusiveViolation { .. } => "cvc-maxInclusive-valid",
        FacetError::MinExclusiveViolation { .. } => "cvc-minExclusive-valid",
        FacetError::MaxExclusiveViolation { .. } => "cvc-maxExclusive-valid",
        FacetError::TotalDigitsViolation { .. } => "cvc-totalDigits-valid",
        FacetError::FractionDigitsViolation { .. } => "cvc-fractionDigits-valid",
        FacetError::ExplicitTimezoneViolation { .. } => "cvc-explicitTimezone-valid",
        FacetError::InvalidPattern { .. } => "cvc-pattern-valid",
        FacetError::DerivationRestriction { .. } => "cos-st-restricts",
        FacetError::FixedFacetViolation { .. } => "cos-st-restricts",
        FacetError::ConflictingFacets { .. } => "cos-st-restricts",
        FacetError::NotApplicable { .. } => "cos-applicable-facets",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_constructor() {
        let err = error("cvc-elt", "Element is invalid", None);
        assert_eq!(err.constraint, "cvc-elt");
        assert_eq!(err.message, "Element is invalid");
        assert!(err.location.is_none());
        assert!(err.element_path.is_none());
    }

    #[test]
    fn test_error_with_path() {
        let err = error_with_path(
            "cvc-complex-type",
            "Missing required element",
            None,
            "/root/child",
        );
        assert_eq!(err.constraint, "cvc-complex-type");
        assert_eq!(err.element_path.as_deref(), Some("/root/child"));
    }

    #[test]
    fn test_error_display() {
        let err = error("cvc-elt", "Invalid element", None);
        assert_eq!(format!("{}", err), "[cvc-elt] Invalid element");

        let err_with_path = error_with_path("cvc-type", "Type mismatch", None, "/root");
        assert_eq!(
            format!("{}", err_with_path),
            "[cvc-type] Type mismatch (/root)"
        );
    }

    #[test]
    fn test_facet_constraint_code_mapping() {
        // Test all facet error variants map to correct codes
        assert_eq!(
            facet_constraint_code(&FacetError::LengthViolation {
                message: "test".to_string()
            }),
            "cvc-length-valid"
        );
        assert_eq!(
            facet_constraint_code(&FacetError::MinLengthViolation { actual: 1, min: 5 }),
            "cvc-minLength-valid"
        );
        assert_eq!(
            facet_constraint_code(&FacetError::MaxLengthViolation {
                actual: 10,
                max: 5
            }),
            "cvc-maxLength-valid"
        );
        assert_eq!(
            facet_constraint_code(&FacetError::PatternViolation {
                value: "abc".to_string(),
                pattern: "[0-9]+".to_string()
            }),
            "cvc-pattern-valid"
        );
        assert_eq!(
            facet_constraint_code(&FacetError::EnumerationViolation {
                value: "x".to_string()
            }),
            "cvc-enumeration-valid"
        );
        assert_eq!(
            facet_constraint_code(&FacetError::MinInclusiveViolation {
                value: "1".to_string(),
                min: "5".to_string()
            }),
            "cvc-minInclusive-valid"
        );
        assert_eq!(
            facet_constraint_code(&FacetError::MaxInclusiveViolation {
                value: "10".to_string(),
                max: "5".to_string()
            }),
            "cvc-maxInclusive-valid"
        );
        assert_eq!(
            facet_constraint_code(&FacetError::MinExclusiveViolation {
                value: "5".to_string(),
                min: "5".to_string()
            }),
            "cvc-minExclusive-valid"
        );
        assert_eq!(
            facet_constraint_code(&FacetError::MaxExclusiveViolation {
                value: "5".to_string(),
                max: "5".to_string()
            }),
            "cvc-maxExclusive-valid"
        );
        assert_eq!(
            facet_constraint_code(&FacetError::TotalDigitsViolation { actual: 10, max: 5 }),
            "cvc-totalDigits-valid"
        );
        assert_eq!(
            facet_constraint_code(&FacetError::FractionDigitsViolation { actual: 5, max: 2 }),
            "cvc-fractionDigits-valid"
        );
        assert_eq!(
            facet_constraint_code(&FacetError::ExplicitTimezoneViolation {
                message: "test".to_string()
            }),
            "cvc-explicitTimezone-valid"
        );
        assert_eq!(
            facet_constraint_code(&FacetError::InvalidPattern {
                pattern: "[".to_string(),
                message: "invalid".to_string()
            }),
            "cvc-pattern-valid"
        );
        assert_eq!(
            facet_constraint_code(&FacetError::DerivationRestriction {
                message: "test".to_string()
            }),
            "cos-st-restricts"
        );
        assert_eq!(
            facet_constraint_code(&FacetError::FixedFacetViolation {
                facet_name: "length".to_string(),
                base_value: "5".to_string(),
                derived_value: "10".to_string()
            }),
            "cos-st-restricts"
        );
        assert_eq!(
            facet_constraint_code(&FacetError::ConflictingFacets {
                message: "test".to_string()
            }),
            "cos-st-restricts"
        );
        assert_eq!(
            facet_constraint_code(&FacetError::NotApplicable {
                facet: "length".to_string(),
                type_name: "integer".to_string()
            }),
            "cos-applicable-facets"
        );
    }

    #[test]
    fn test_from_facet_error() {
        let facet_err = FacetError::MinLengthViolation { actual: 2, min: 5 };
        let code = facet_constraint_code(&facet_err);
        let val_err = from_facet_error(code, facet_err, None);
        assert_eq!(val_err.constraint, "cvc-minLength-valid");
        assert!(val_err.message.contains("minLength"));
    }
}
