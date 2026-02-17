//! Validation output types returned to callers after each validation event
//!
//! These types represent the schema information associated with validated XML nodes.
//! `SchemaInfo` is the primary output, returned from each `validate_*` method.

use bitflags::bitflags;

use crate::ids::{AttributeKey, ElementKey, NameId, TypeKey};
use crate::types::value::XmlValue;

/// Validity status of a validated node
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SchemaValidity {
    /// Validity has not been determined
    #[default]
    NotKnown,
    /// The node is valid according to the schema
    Valid,
    /// The node is invalid according to the schema
    Invalid,
}

/// Content type of a complex type, used to determine what children are allowed
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ContentType {
    /// No child elements or text content allowed
    #[default]
    Empty,
    /// Text content only (simple content), possibly with attributes
    TextOnly,
    /// Child elements only, no interleaved text
    ElementOnly,
    /// Child elements with interleaved text allowed
    Mixed,
}

/// Stable node identity for cross-phase correlation (e.g., linking Phase 1 results to Phase 2 DOM nodes)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeIdentity(pub u64);

bitflags! {
    /// Flags controlling validation behavior
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ValidationFlags: u32 {
        /// Report warnings in addition to errors
        const REPORT_WARNINGS = 0x0001;
        /// Process identity constraints (key, unique, keyref)
        const PROCESS_IDENTITY_CONSTRAINTS = 0x0002;
        /// Allow xml:* attributes without explicit declaration
        const ALLOW_XML_ATTRIBUTES = 0x0004;
        /// Strict mode: treat all warnings as errors
        const STRICT_MODE = 0x0008;
    }
}

impl Default for ValidationFlags {
    fn default() -> Self {
        ValidationFlags::REPORT_WARNINGS | ValidationFlags::ALLOW_XML_ATTRIBUTES
    }
}

/// Content processing mode for wildcard-matched elements/attributes
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ContentProcessing {
    /// Must be validated against the schema; error if no declaration found
    #[default]
    Strict,
    /// Validate if declaration found; skip if not
    Lax,
    /// Do not validate content
    Skip,
}

/// Schema information returned after validating a node
///
/// Contains the resolved schema components and validation status for an element or attribute.
#[derive(Debug, Clone)]
pub struct SchemaInfo {
    /// The element declaration, if one was found
    pub element_decl: Option<ElementKey>,
    /// The attribute declaration, if one was found
    pub attribute_decl: Option<AttributeKey>,
    /// The resolved schema type (simple or complex)
    pub schema_type: Option<TypeKey>,
    /// For union types: the actual member type that matched the value
    pub member_type: Option<TypeKey>,
    /// Validity status
    pub validity: SchemaValidity,
    /// Whether the value was supplied by a default declaration
    pub is_default: bool,
    /// Whether the element was declared nil via xsi:nil="true"
    pub is_nil: bool,
    /// Content type of the element (Empty, TextOnly, ElementOnly, Mixed)
    pub content_type: Option<ContentType>,
    /// The parsed typed value from simple-type validation
    pub typed_value: Option<XmlValue>,
}

impl SchemaInfo {
    /// Create a SchemaInfo with all fields set to None/default
    pub fn empty() -> Self {
        SchemaInfo {
            element_decl: None,
            attribute_decl: None,
            schema_type: None,
            member_type: None,
            validity: SchemaValidity::NotKnown,
            is_default: false,
            is_nil: false,
            content_type: None,
            typed_value: None,
        }
    }

    /// Create a SchemaInfo indicating a valid element
    pub fn valid_element(element_decl: ElementKey, schema_type: TypeKey, content_type: ContentType) -> Self {
        SchemaInfo {
            element_decl: Some(element_decl),
            attribute_decl: None,
            schema_type: Some(schema_type),
            member_type: None,
            validity: SchemaValidity::Valid,
            is_default: false,
            is_nil: false,
            content_type: Some(content_type),
            typed_value: None,
        }
    }

    /// Create a SchemaInfo indicating a valid attribute
    pub fn valid_attribute(attribute_decl: AttributeKey, schema_type: TypeKey) -> Self {
        SchemaInfo {
            element_decl: None,
            attribute_decl: Some(attribute_decl),
            schema_type: Some(schema_type),
            member_type: None,
            validity: SchemaValidity::Valid,
            is_default: false,
            is_nil: false,
            content_type: None,
            typed_value: None,
        }
    }

    /// Create a SchemaInfo with Invalid validity
    pub fn invalid() -> Self {
        SchemaInfo {
            validity: SchemaValidity::Invalid,
            ..SchemaInfo::empty()
        }
    }
}

/// An expected element in the current content model position
#[derive(Debug, Clone)]
pub struct ExpectedElement {
    /// Local name of the expected element
    pub local_name: NameId,
    /// Namespace of the expected element
    pub namespace: Option<NameId>,
    /// The element declaration key, if available
    pub element_key: Option<ElementKey>,
}

/// An expected attribute for the current element type
#[derive(Debug, Clone)]
pub struct ExpectedAttribute {
    /// Local name of the attribute
    pub local_name: NameId,
    /// Namespace of the attribute
    pub namespace: Option<NameId>,
    /// The attribute declaration key
    pub attribute_key: Option<AttributeKey>,
    /// Whether the attribute is required
    pub required: bool,
}

/// A default attribute that should be added to the element
#[derive(Debug, Clone)]
pub struct DefaultAttribute {
    /// Local name of the attribute
    pub local_name: NameId,
    /// Namespace of the attribute
    pub namespace: Option<NameId>,
    /// The attribute declaration key
    pub attribute_key: AttributeKey,
    /// The default value
    pub value: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_schema_info_empty() {
        let info = SchemaInfo::empty();
        assert_eq!(info.validity, SchemaValidity::NotKnown);
        assert!(info.element_decl.is_none());
        assert!(info.attribute_decl.is_none());
        assert!(info.schema_type.is_none());
        assert!(info.member_type.is_none());
        assert!(!info.is_default);
        assert!(!info.is_nil);
        assert!(info.content_type.is_none());
        assert!(info.typed_value.is_none());
    }

    #[test]
    fn test_schema_info_invalid() {
        let info = SchemaInfo::invalid();
        assert_eq!(info.validity, SchemaValidity::Invalid);
        assert!(info.element_decl.is_none());
    }

    #[test]
    fn test_schema_validity_default() {
        let v = SchemaValidity::default();
        assert_eq!(v, SchemaValidity::NotKnown);
    }

    #[test]
    fn test_content_type_default() {
        let ct = ContentType::default();
        assert_eq!(ct, ContentType::Empty);
    }

    #[test]
    fn test_content_processing_default() {
        let cp = ContentProcessing::default();
        assert_eq!(cp, ContentProcessing::Strict);
    }

    #[test]
    fn test_validation_flags_default() {
        let flags = ValidationFlags::default();
        assert!(flags.contains(ValidationFlags::REPORT_WARNINGS));
        assert!(flags.contains(ValidationFlags::ALLOW_XML_ATTRIBUTES));
        assert!(!flags.contains(ValidationFlags::PROCESS_IDENTITY_CONSTRAINTS));
        assert!(!flags.contains(ValidationFlags::STRICT_MODE));
    }

    #[test]
    fn test_validation_flags_bitops() {
        let flags = ValidationFlags::REPORT_WARNINGS | ValidationFlags::STRICT_MODE;
        assert!(flags.contains(ValidationFlags::REPORT_WARNINGS));
        assert!(flags.contains(ValidationFlags::STRICT_MODE));
        assert!(!flags.contains(ValidationFlags::PROCESS_IDENTITY_CONSTRAINTS));

        let combined = flags | ValidationFlags::PROCESS_IDENTITY_CONSTRAINTS;
        assert!(combined.contains(ValidationFlags::PROCESS_IDENTITY_CONSTRAINTS));
    }

    #[test]
    fn test_node_identity_eq() {
        let a = NodeIdentity(42);
        let b = NodeIdentity(42);
        let c = NodeIdentity(99);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }
}
