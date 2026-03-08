//! Per-element validation state and validator state machine
//!
//! `ElementValidationState` holds all per-element context that is pushed/popped
//! as the validator enters/exits elements. `ValidatorState` enforces the correct
//! sequence of push-API calls.

use std::collections::HashSet;

use crate::ids::{ElementKey, NameId, TypeKey};
use crate::types::value::XmlValue;

use super::content::ContentValidatorState;
use super::info::{ContentProcessing, ContentType, SchemaValidity};

/// Per-element state pushed onto the validation stack
///
/// Each time `validate_element` is called, a new `ElementValidationState` is
/// created and pushed. It is popped on `validate_end_element`.
#[derive(Debug, Clone)]
pub struct ElementValidationState {
    /// Local name of the element
    pub local_name: NameId,
    /// Namespace URI of the element (None for no-namespace)
    pub namespace: Option<NameId>,
    /// Resolved element declaration, if found
    pub element_decl: Option<ElementKey>,
    /// Resolved schema type (simple or complex)
    pub schema_type: Option<TypeKey>,
    /// Content model state for this element's type
    pub content_state: ContentValidatorState,
    /// Content type classification (Empty, TextOnly, ElementOnly, Mixed)
    pub content_type: Option<ContentType>,
    /// Whether xsi:nil="true" was specified
    pub is_nil: bool,
    /// Whether the element value came from a default declaration
    pub is_default: bool,
    /// For union types: the actual member type that matched the value
    pub member_type: Option<TypeKey>,
    /// The parsed typed value from simple-type validation
    pub typed_value: Option<XmlValue>,
    /// Current validity status
    pub validity: SchemaValidity,
    /// How to process wildcard-matched content
    pub process_contents: ContentProcessing,
    /// Set of (namespace, local_name) pairs for attributes already seen
    pub seen_attributes: HashSet<(Option<NameId>, NameId)>,
    /// Accumulated text content for the element
    pub text_content: String,
    /// Whether any text nodes have been seen
    pub has_text: bool,
    /// Whether any child element nodes have been seen
    pub has_element_children: bool,
    /// Whether this element owns an assertion buffer frame (XSD 1.1)
    #[cfg(feature = "xsd11")]
    pub owns_assertion_buffer: bool,
    /// Whether this element has type alternatives (XSD 1.1)
    #[cfg(feature = "xsd11")]
    pub has_type_alternatives: bool,
    /// Collected attributes for type alternative XPath evaluation (XSD 1.1)
    #[cfg(feature = "xsd11")]
    pub collected_attributes: Vec<(Option<NameId>, NameId, String)>,
    /// Node ref of this element in the assertion fragment document (XSD 1.1).
    /// Saved during `detect_assertions_on_element` for CTA re-detection.
    #[cfg(feature = "xsd11")]
    pub assertion_element_ref: Option<u32>,
}

impl ElementValidationState {
    /// Create a new element validation state with defaults
    pub fn new(local_name: NameId, namespace: Option<NameId>) -> Self {
        ElementValidationState {
            local_name,
            namespace,
            element_decl: None,
            schema_type: None,
            content_state: ContentValidatorState::Empty,
            content_type: None,
            is_nil: false,
            is_default: false,
            member_type: None,
            typed_value: None,
            validity: SchemaValidity::NotKnown,
            process_contents: ContentProcessing::Strict,
            seen_attributes: HashSet::new(),
            text_content: String::new(),
            has_text: false,
            has_element_children: false,
            #[cfg(feature = "xsd11")]
            owns_assertion_buffer: false,
            #[cfg(feature = "xsd11")]
            has_type_alternatives: false,
            #[cfg(feature = "xsd11")]
            collected_attributes: Vec::new(),
            #[cfg(feature = "xsd11")]
            assertion_element_ref: None,
        }
    }
}

/// State machine for the validator's call sequence
///
/// Enforces that push-API methods are called in the correct order.
/// The valid transitions are:
///
/// ```text
/// None → Start → Element → Attribute* → EndOfAttributes → (Text|Whitespace)* → EndElement → ... → Finish
///                                                          ↑                      |
///                                                          └── (Element cycle) ────┘
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidatorState {
    /// Initial state, no validation has started
    None,
    /// `validate_element` has been called for the root element
    Start,
    /// Inside an element (after `validate_element`)
    Element,
    /// Processing attributes (after `validate_attribute`)
    Attribute,
    /// After `validate_end_of_attributes`
    EndOfAttributes,
    /// After `validate_text`
    Text,
    /// After `validate_whitespace`
    Whitespace,
    /// After `validate_end_element`
    EndElement,
    /// After `end_validation` — no further calls allowed
    Finish,
}

impl ValidatorState {
    /// Check if `validate_element` can be called in this state
    pub fn can_start_element(&self) -> bool {
        matches!(
            self,
            ValidatorState::None
                | ValidatorState::Start
                | ValidatorState::EndOfAttributes
                | ValidatorState::Text
                | ValidatorState::Whitespace
                | ValidatorState::EndElement
        )
    }

    /// Check if `validate_attribute` can be called in this state
    pub fn can_validate_attribute(&self) -> bool {
        matches!(self, ValidatorState::Element | ValidatorState::Attribute)
    }

    /// Check if `validate_end_of_attributes` can be called in this state
    pub fn can_end_attributes(&self) -> bool {
        matches!(
            self,
            ValidatorState::Element | ValidatorState::Attribute
        )
    }

    /// Check if `validate_text` / `validate_whitespace` can be called in this state
    pub fn can_validate_text(&self) -> bool {
        matches!(
            self,
            ValidatorState::EndOfAttributes
                | ValidatorState::Text
                | ValidatorState::Whitespace
                | ValidatorState::EndElement
        )
    }

    /// Check if `validate_end_element` can be called in this state
    pub fn can_end_element(&self) -> bool {
        matches!(
            self,
            ValidatorState::EndOfAttributes
                | ValidatorState::Text
                | ValidatorState::Whitespace
                | ValidatorState::EndElement
        )
    }

    /// Check if `end_validation` can be called in this state
    pub fn can_finish(&self) -> bool {
        matches!(self, ValidatorState::EndElement | ValidatorState::None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_element_validation_state_defaults() {
        let state = ElementValidationState::new(NameId(1), None);
        assert_eq!(state.local_name, NameId(1));
        assert!(state.namespace.is_none());
        assert!(state.element_decl.is_none());
        assert!(state.schema_type.is_none());
        assert!(state.content_type.is_none());
        assert!(!state.is_nil);
        assert!(!state.is_default);
        assert_eq!(state.validity, SchemaValidity::NotKnown);
        assert_eq!(state.process_contents, ContentProcessing::Strict);
        assert!(state.seen_attributes.is_empty());
        assert!(state.text_content.is_empty());
        assert!(!state.has_text);
        assert!(!state.has_element_children);
    }

    #[test]
    fn test_element_validation_state_with_namespace() {
        let state = ElementValidationState::new(NameId(5), Some(NameId(10)));
        assert_eq!(state.local_name, NameId(5));
        assert_eq!(state.namespace, Some(NameId(10)));
    }

    #[test]
    fn test_seen_attributes_dedup() {
        let mut state = ElementValidationState::new(NameId(1), None);
        let attr = (None, NameId(100));
        assert!(state.seen_attributes.insert(attr));
        // Second insert returns false — duplicate
        assert!(!state.seen_attributes.insert(attr));
        assert_eq!(state.seen_attributes.len(), 1);
    }

    #[test]
    fn test_validator_state_transitions() {
        // None -> can start element
        assert!(ValidatorState::None.can_start_element());
        assert!(!ValidatorState::None.can_validate_attribute());
        assert!(ValidatorState::None.can_finish());

        // Element -> can validate attribute, can end attributes
        assert!(ValidatorState::Element.can_validate_attribute());
        assert!(ValidatorState::Element.can_end_attributes());
        assert!(!ValidatorState::Element.can_validate_text());
        assert!(!ValidatorState::Element.can_end_element());

        // Attribute -> can continue attributes, can end attributes
        assert!(ValidatorState::Attribute.can_validate_attribute());
        assert!(ValidatorState::Attribute.can_end_attributes());

        // EndOfAttributes -> can have text, children, or end
        assert!(ValidatorState::EndOfAttributes.can_validate_text());
        assert!(ValidatorState::EndOfAttributes.can_start_element());
        assert!(ValidatorState::EndOfAttributes.can_end_element());

        // Text -> can have more text, children, or end
        assert!(ValidatorState::Text.can_validate_text());
        assert!(ValidatorState::Text.can_start_element());
        assert!(ValidatorState::Text.can_end_element());

        // EndElement -> can start sibling or end
        assert!(ValidatorState::EndElement.can_start_element());
        assert!(ValidatorState::EndElement.can_end_element());
        assert!(ValidatorState::EndElement.can_finish());

        // Finish -> nothing allowed
        assert!(!ValidatorState::Finish.can_start_element());
        assert!(!ValidatorState::Finish.can_validate_attribute());
        assert!(!ValidatorState::Finish.can_validate_text());
        assert!(!ValidatorState::Finish.can_end_element());
        assert!(!ValidatorState::Finish.can_finish());
    }
}
