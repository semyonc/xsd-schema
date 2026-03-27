//! Validation output types returned to callers after each validation event
//!
//! These types represent the schema information associated with validated XML nodes.
//! `SchemaInfo` is the primary output, returned from each `validate_*` method.

use bitflags::bitflags;

use crate::ids::{AttributeKey, ElementKey, NameId, NotationKey, TypeKey};
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

/// How much validation was attempted on a node (PSVI `[validation attempted]`)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ValidationAttempted {
    /// No validation was attempted
    #[default]
    None,
    /// Some but not all descendants were validated
    Partial,
    /// Full validation was performed on this node and all descendants
    Full,
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

/// How the final `schema_type` was determined
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeSource {
    /// From the element/attribute declaration's resolved_type
    Declaration,
    /// Overridden by xsi:type attribute
    XsiType,
    /// Selected by Conditional Type Assignment (XSD 1.1)
    #[cfg(feature = "xsd11")]
    TypeAlternative,
}

/// Complex-type assertion evaluation outcome (XSD 1.1)
///
/// Covers only the buffered complex-type assertion path. Simple-type
/// assertion facet failures are reflected in `validity: Invalid` with
/// `cvc-assertion` constraint through the error sink.
#[cfg(feature = "xsd11")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssertionOutcome {
    /// All assertions evaluated and passed
    Passed,
    /// One or more assertions failed (includes compile/eval/EBV errors)
    Failed,
    /// Assertions exist but were not evaluated (PROCESS_ASSERTIONS not set,
    /// or evaluation deferred to an outer asserted element)
    NotEvaluated,
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
        /// Enable XSD 1.1 assertion processing (fragment buffering)
        #[cfg(feature = "xsd11")]
        const PROCESS_ASSERTIONS = 0x0010;
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
    /// How much validation was attempted (PSVI `[validation attempted]`)
    pub validation_attempted: ValidationAttempted,
    /// Whether the value was supplied by a default declaration
    pub is_default: bool,
    /// Whether the element was declared nil via xsi:nil="true"
    pub is_nil: bool,
    /// Content type of the element (Empty, TextOnly, ElementOnly, Mixed)
    pub content_type: Option<ContentType>,
    /// The parsed typed value from simple-type validation
    pub typed_value: Option<XmlValue>,
    /// The whitespace-normalized value (PSVI `[schema normalized value]`)
    pub normalized_value: Option<String>,
    /// Constraint codes from validation errors on this node (PSVI `[schema error code]`)
    pub schema_error_codes: Vec<&'static str>,
    /// Notation declaration resolved from a NOTATION-typed attribute (PSVI `[notation]`).
    /// Only meaningful on element-end SchemaInfo; always `None` for attributes.
    pub notation: Option<NotationKey>,
    /// Whether this attribute was deferred due to CTA (type alternatives)
    pub deferred_by_cta: bool,
    /// How the `schema_type` was determined (declaration, xsi:type, or CTA)
    pub type_source: Option<TypeSource>,
    /// Whether CTA evaluation selected a type (even if it matches the declared type)
    #[cfg(feature = "xsd11")]
    pub cta_selected: bool,
    /// Complex-type assertion outcome (XSD 1.1, end-element SchemaInfo only)
    #[cfg(feature = "xsd11")]
    pub assertion_outcome: Option<AssertionOutcome>,
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
            validation_attempted: ValidationAttempted::None,
            is_default: false,
            is_nil: false,
            content_type: None,
            typed_value: None,
            normalized_value: None,
            schema_error_codes: Vec::new(),
            notation: None,
            deferred_by_cta: false,
            type_source: None,
            #[cfg(feature = "xsd11")]
            cta_selected: false,
            #[cfg(feature = "xsd11")]
            assertion_outcome: None,
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
            validation_attempted: ValidationAttempted::Full,
            is_default: false,
            is_nil: false,
            content_type: Some(content_type),
            typed_value: None,
            normalized_value: None,
            schema_error_codes: Vec::new(),
            notation: None,
            deferred_by_cta: false,
            type_source: Some(TypeSource::Declaration),
            #[cfg(feature = "xsd11")]
            cta_selected: false,
            #[cfg(feature = "xsd11")]
            assertion_outcome: None,
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
            validation_attempted: ValidationAttempted::Full,
            is_default: false,
            is_nil: false,
            content_type: None,
            typed_value: None,
            normalized_value: None,
            schema_error_codes: Vec::new(),
            notation: None,
            deferred_by_cta: false,
            type_source: Some(TypeSource::Declaration),
            #[cfg(feature = "xsd11")]
            cta_selected: false,
            #[cfg(feature = "xsd11")]
            assertion_outcome: None,
        }
    }

    /// Create a SchemaInfo with Invalid validity
    pub fn invalid() -> Self {
        SchemaInfo {
            validity: SchemaValidity::Invalid,
            ..SchemaInfo::empty()
        }
    }

    /// Returns `true` if the resolved schema type is a simple type.
    pub fn is_simple_type(&self) -> bool {
        matches!(self.schema_type, Some(TypeKey::Simple(_)))
    }

    /// Returns `true` if the resolved schema type is a complex type.
    pub fn is_complex_type(&self) -> bool {
        matches!(self.schema_type, Some(TypeKey::Complex(_)))
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

/// An inherited attribute from an ancestor element (XSD 1.1 §3.3.5.6).
///
/// Represents an entry in the PSVI `[inherited attributes]` property.
#[cfg(feature = "xsd11")]
#[derive(Debug, Clone)]
pub struct InheritedAttribute {
    /// Local name of the attribute
    pub local_name: NameId,
    /// Namespace of the attribute
    pub namespace: Option<NameId>,
    /// The governing attribute declaration key, if known
    pub attribute_key: Option<AttributeKey>,
    /// The inherited attribute value
    pub value: String,
}

/// A schema location hint extracted from `xsi:schemaLocation`.
///
/// Pairs a namespace URI with a schema location URI plus the base URI
/// of the instance document element where the hint was found (needed to
/// resolve relative location URIs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaLocationHint {
    /// The namespace URI (first token of each pair in `xsi:schemaLocation`).
    pub namespace: String,
    /// The schema location URI (second token of each pair).
    pub location: String,
    /// Base URI of the instance document at the point where this hint was
    /// found. Empty if no base URI was set on the runtime.
    pub base_uri: String,
}

/// A no-namespace schema location hint extracted from
/// `xsi:noNamespaceSchemaLocation`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoNamespaceSchemaLocationHint {
    /// The schema location URI.
    pub location: String,
    /// Base URI of the instance document at the point where this hint was
    /// found. Empty if no base URI was set on the runtime.
    pub base_uri: String,
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
        assert!(info.type_source.is_none());
        #[cfg(feature = "xsd11")]
        {
            assert!(!info.cta_selected);
            assert!(info.assertion_outcome.is_none());
        }
    }

    #[test]
    fn test_schema_info_invalid() {
        let info = SchemaInfo::invalid();
        assert_eq!(info.validity, SchemaValidity::Invalid);
        assert!(info.element_decl.is_none());
    }

    #[test]
    fn test_is_simple_type() {
        let info = SchemaInfo::empty();
        assert!(!info.is_simple_type());
        assert!(!info.is_complex_type());

        use slotmap::SlotMap;
        let mut sm: SlotMap<crate::ids::SimpleTypeKey, ()> = SlotMap::with_key();
        let sk = sm.insert(());
        let mut info = SchemaInfo::empty();
        info.schema_type = Some(TypeKey::Simple(sk));
        assert!(info.is_simple_type());
        assert!(!info.is_complex_type());
    }

    #[test]
    fn test_is_complex_type() {
        use slotmap::SlotMap;
        let mut sm: SlotMap<crate::ids::ComplexTypeKey, ()> = SlotMap::with_key();
        let ck = sm.insert(());
        let mut info = SchemaInfo::empty();
        info.schema_type = Some(TypeKey::Complex(ck));
        assert!(info.is_complex_type());
        assert!(!info.is_simple_type());
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

    #[cfg(feature = "xsd11")]
    #[test]
    fn test_process_assertions_flag() {
        let default_flags = ValidationFlags::default();
        assert!(
            !default_flags.contains(ValidationFlags::PROCESS_ASSERTIONS),
            "PROCESS_ASSERTIONS must not be in defaults"
        );
        let with_flag = default_flags | ValidationFlags::PROCESS_ASSERTIONS;
        assert!(with_flag.contains(ValidationFlags::PROCESS_ASSERTIONS));
        // Original defaults still present
        assert!(with_flag.contains(ValidationFlags::REPORT_WARNINGS));
        assert!(with_flag.contains(ValidationFlags::ALLOW_XML_ATTRIBUTES));
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
