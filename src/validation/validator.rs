//! Core `SchemaValidator` — push-based, DOM-independent instance validation
//!
//! Callers push XML events (element start, attribute, text, element end) and
//! receive `SchemaInfo` decisions back. Errors and warnings are reported to
//! a `ValidationSink`.

use std::collections::HashSet;

use crate::arenas::ComplexTypeDefData;
use crate::compiler::{compile_content_model_matcher, SubstitutionGroupMap};
use crate::ids::{NameId, TypeKey, AttributeKey};
use crate::namespace::context::NamespaceContextSnapshot;
use crate::namespace::table::well_known;
use crate::parser::frames::AttributeUseKind;
use crate::parser::location::SourceLocation;
use crate::schema::model::DerivationSet;
use crate::schema::SchemaSet;

use super::content::ContentValidatorState;
use super::context::{ElementValidationState, ValidatorState};
use super::errors::{self, ValidationError};
use super::info::{
    ContentProcessing, ContentType, DefaultAttribute, ExpectedAttribute, ExpectedElement,
    SchemaInfo, SchemaValidity, ValidationFlags,
};

// ---------------------------------------------------------------------------
// ValidationSink trait
// ---------------------------------------------------------------------------

/// Sink for validation errors and warnings
///
/// Implement this trait to receive validation messages from `SchemaValidator`.
pub trait ValidationSink {
    /// Report a validation error
    fn on_error(&mut self, error: ValidationError);
    /// Report a validation warning
    fn on_warning(&mut self, warning: ValidationWarning);
}

/// A validation warning (non-fatal)
#[derive(Debug, Clone)]
pub struct ValidationWarning {
    /// Warning code
    pub code: &'static str,
    /// Human-readable message
    pub message: String,
    /// Source location in the instance document
    pub location: Option<SourceLocation>,
}

impl std::fmt::Display for ValidationWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)?;
        if let Some(loc) = &self.location {
            write!(f, " at {}", loc)?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Built-in sinks
// ---------------------------------------------------------------------------

/// Collects errors into a `Vec<ValidationError>` and warnings into a `Vec<ValidationWarning>`
pub struct CollectingValidationSink<'a> {
    pub errors: &'a mut Vec<ValidationError>,
    pub warnings: &'a mut Vec<ValidationWarning>,
}

impl<'a> ValidationSink for CollectingValidationSink<'a> {
    fn on_error(&mut self, error: ValidationError) {
        self.errors.push(error);
    }
    fn on_warning(&mut self, warning: ValidationWarning) {
        self.warnings.push(warning);
    }
}

/// Collects errors only; discards warnings
pub struct ErrorOnlySink<'a> {
    pub errors: &'a mut Vec<ValidationError>,
}

impl<'a> ValidationSink for ErrorOnlySink<'a> {
    fn on_error(&mut self, error: ValidationError) {
        self.errors.push(error);
    }
    fn on_warning(&mut self, _warning: ValidationWarning) {
        // Discarded
    }
}

// ---------------------------------------------------------------------------
// SchemaValidator
// ---------------------------------------------------------------------------

/// Push-based schema validator
///
/// Callers push XML events and receive `SchemaInfo` back. The validator
/// maintains an internal stack of `ElementValidationState` entries and
/// a state machine (`ValidatorState`) enforcing the correct call order.
pub struct SchemaValidator<'a, S: ValidationSink> {
    /// The compiled schema set to validate against
    schema_set: &'a SchemaSet,
    /// Pre-built substitution group map (if any)
    subst_groups: Option<SubstitutionGroupMap>,
    /// Sink for errors and warnings
    sink: S,
    /// Validation flags controlling behaviour
    flags: ValidationFlags,
    /// Stack of per-element validation states
    validation_stack: Vec<ElementValidationState>,
    /// Current state machine state
    current_state: ValidatorState,
    /// Current source location (updated by caller)
    current_location: Option<SourceLocation>,
    /// XPath-like element path (e.g., "/root/child[1]")
    element_path: String,
}

impl<'a, S: ValidationSink> SchemaValidator<'a, S> {
    /// Create a new `SchemaValidator`
    pub fn new(schema_set: &'a SchemaSet, sink: S, flags: ValidationFlags) -> Self {
        SchemaValidator {
            schema_set,
            subst_groups: None,
            sink,
            flags,
            validation_stack: Vec::new(),
            current_state: ValidatorState::None,
            current_location: None,
            element_path: String::new(),
        }
    }

    /// Create a new `SchemaValidator` with pre-built substitution groups
    pub fn with_substitution_groups(
        schema_set: &'a SchemaSet,
        sink: S,
        flags: ValidationFlags,
        subst_groups: SubstitutionGroupMap,
    ) -> Self {
        SchemaValidator {
            subst_groups: Some(subst_groups),
            ..Self::new(schema_set, sink, flags)
        }
    }

    /// Set the current source location for error reporting
    pub fn set_location(&mut self, location: SourceLocation) {
        self.current_location = Some(location);
    }

    /// Clear the current source location
    pub fn clear_location(&mut self) {
        self.current_location = None;
    }

    // -----------------------------------------------------------------------
    // Push API
    // -----------------------------------------------------------------------

    /// Validate an element start event (string-based lookup)
    ///
    /// `local_name` and `namespace_uri` identify the element.
    /// `xsi_type` is the value of `xsi:type` (if present), as a raw QName string.
    /// `xsi_nil` is the value of `xsi:nil` (if present).
    /// `ns_context` is used to resolve the xsi:type QName prefix.
    pub fn validate_element(
        &mut self,
        local_name: &str,
        namespace_uri: &str,
        xsi_type: Option<&str>,
        xsi_nil: Option<&str>,
        ns_context: &NamespaceContextSnapshot,
    ) -> SchemaInfo {
        let name_id = self.schema_set.name_table.add(local_name);
        let ns_id = if namespace_uri.is_empty() {
            None
        } else {
            Some(self.schema_set.name_table.add(namespace_uri))
        };
        self.validate_element_by_id(name_id, ns_id, xsi_type, xsi_nil, ns_context)
    }

    /// Validate an element start event (NameId fast-path)
    pub fn validate_element_by_id(
        &mut self,
        local_name: NameId,
        namespace: Option<NameId>,
        xsi_type: Option<&str>,
        xsi_nil: Option<&str>,
        ns_context: &NamespaceContextSnapshot,
    ) -> SchemaInfo {
        // 1. State machine check
        if !self.current_state.can_start_element() {
            self.report_error(
                "cvc-complex-type",
                format!(
                    "Element start not allowed in current state {:?}",
                    self.current_state
                ),
            );
            return SchemaInfo::invalid();
        }

        // 2. If not root: advance parent's content model
        let mut match_info: Option<super::content::ElementMatchInfo> = None;
        let mut content_model_accepted = false;
        let mut content_model_error = None;
        if let Some(parent) = self.validation_stack.last_mut() {
            parent.has_element_children = true;
            match parent.content_state.advance_element(
                local_name,
                namespace,
                parent.namespace, // parent's target_namespace for wildcard matching
                self.schema_set.xsd_version,
                self.subst_groups.as_ref(),
            ) {
                Some(info) => {
                    match_info = Some(info);
                    content_model_accepted = true;
                }
                None => {
                    let elem_name = self.schema_set.name_table.resolve(local_name);
                    content_model_error = Some(format!(
                        "Element '{}' is not allowed at this position in the content model",
                        elem_name,
                    ));
                }
            }
        }
        if let Some(msg) = content_model_error {
            self.report_error("cvc-complex-type.2.4", msg);
        }

        // 3. Look up element declaration: prefer content model match, fall back to global
        let matched_elem_key = match_info.and_then(|i| i.element_key);
        let matched_type = match_info.and_then(|i| i.resolved_type);

        // If the content model provided a resolved type for a local element,
        // don't fall back to a global element with the same QName (it may have
        // a different type).
        let element_key = if matched_type.is_some() {
            matched_elem_key
        } else {
            matched_elem_key
                .or_else(|| self.schema_set.lookup_element(namespace, local_name))
        };

        // 4. Handle missing element
        let process_contents = self
            .validation_stack
            .last()
            .map(|p| p.process_contents)
            .unwrap_or(ContentProcessing::Strict);

        if element_key.is_none() {
            if content_model_accepted {
                // Content model accepted this element (local element in content model)
                // but no global declaration exists. The element is structurally valid.
                let is_nil = matches!(xsi_nil, Some("true") | Some("1"));
                let mut ev_state = ElementValidationState::new(local_name, namespace);
                ev_state.validity = SchemaValidity::Valid;
                ev_state.process_contents = process_contents;
                ev_state.is_nil = is_nil;

                if let Some(mut type_key) = matched_type {
                    // xsi:type override for local elements with resolved type
                    if let Some(xsi_type_str) = xsi_type {
                        if let Some(overridden) =
                            self.resolve_xsi_type(xsi_type_str, Some(type_key), ns_context)
                        {
                            type_key = overridden;
                        }
                    }
                    // Local element with resolved type — initialize content model
                    let (content_state, content_type) = self.init_content_model(Some(type_key));
                    ev_state.schema_type = Some(type_key);
                    ev_state.content_state = content_state;
                    ev_state.content_type = Some(content_type);
                } else {
                    // No type info (inline type or unresolved) — check xsi:type, then fallback
                    if let Some(xsi_type_str) = xsi_type {
                        if let Some(overridden) =
                            self.resolve_xsi_type(xsi_type_str, None, ns_context)
                        {
                            let (content_state, content_type) =
                                self.init_content_model(Some(overridden));
                            ev_state.schema_type = Some(overridden);
                            ev_state.content_state = content_state;
                            ev_state.content_type = Some(content_type);
                        } else {
                            ev_state.content_state = ContentValidatorState::Simple;
                            ev_state.content_type = Some(ContentType::TextOnly);
                        }
                    } else {
                        ev_state.content_state = ContentValidatorState::Simple;
                        ev_state.content_type = Some(ContentType::TextOnly);
                    }
                }

                let schema_type = ev_state.schema_type;
                let content_type = ev_state.content_type;
                self.push_element(ev_state);
                return SchemaInfo {
                    element_decl: None,
                    attribute_decl: None,
                    schema_type,
                    member_type: None,
                    validity: SchemaValidity::Valid,
                    is_default: false,
                    is_nil,
                    content_type,
                    typed_value: None,
                };
            }

            match process_contents {
                ContentProcessing::Skip => {
                    // Skip validation entirely
                    let mut ev_state = ElementValidationState::new(local_name, namespace);
                    ev_state.process_contents = ContentProcessing::Skip;
                    ev_state.content_state = ContentValidatorState::Simple; // accept anything
                    ev_state.validity = SchemaValidity::NotKnown;
                    self.push_element(ev_state);
                    return SchemaInfo::empty();
                }
                ContentProcessing::Lax => {
                    // Lax: skip if not found
                    let mut ev_state = ElementValidationState::new(local_name, namespace);
                    ev_state.process_contents = ContentProcessing::Lax;
                    ev_state.content_state = ContentValidatorState::Simple;
                    ev_state.validity = SchemaValidity::NotKnown;
                    self.push_element(ev_state);
                    return SchemaInfo::empty();
                }
                ContentProcessing::Strict => {
                    let elem_name = self.schema_set.name_table.resolve(local_name);
                    self.report_error(
                        "cvc-elt.1",
                        format!("Element '{}' is not declared", elem_name),
                    );
                    let mut ev_state = ElementValidationState::new(local_name, namespace);
                    ev_state.validity = SchemaValidity::Invalid;
                    self.push_element(ev_state);
                    return SchemaInfo::invalid();
                }
            }
        }

        let elem_key = element_key.unwrap();
        let elem_data = &self.schema_set.arenas.elements[elem_key];

        // Check abstract
        if elem_data.is_abstract {
            let elem_name = self.schema_set.name_table.resolve(local_name);
            self.report_error(
                "cvc-elt.2",
                format!("Element '{}' is abstract and cannot appear in instances", elem_name),
            );
        }

        // 5. Resolve type from element declaration
        let mut type_key = elem_data.resolved_type;

        // 6. xsi:type override
        if let Some(xsi_type_str) = xsi_type {
            if let Some(overridden) =
                self.resolve_xsi_type(xsi_type_str, type_key, ns_context)
            {
                type_key = Some(overridden);
            }
        }

        // 7. xsi:nil
        let is_nil = if let Some(nil_str) = xsi_nil {
            if nil_str == "true" || nil_str == "1" {
                if !elem_data.nillable {
                    let elem_name = self.schema_set.name_table.resolve(local_name);
                    self.report_error(
                        "cvc-elt.3.1",
                        format!(
                            "Element '{}' is not nillable but xsi:nil='true' was specified",
                            elem_name,
                        ),
                    );
                }
                true
            } else {
                false
            }
        } else {
            false
        };

        // 8. Initialize content model and determine ContentType
        let (content_state, content_type) = self.init_content_model(type_key);

        // 9. Push ElementValidationState
        let mut ev_state = ElementValidationState::new(local_name, namespace);
        ev_state.element_decl = Some(elem_key);
        ev_state.schema_type = type_key;
        ev_state.content_state = content_state;
        ev_state.content_type = Some(content_type);
        ev_state.is_nil = is_nil;
        ev_state.validity = SchemaValidity::Valid;
        ev_state.process_contents = process_contents;
        self.push_element(ev_state);

        // 10. Return SchemaInfo
        SchemaInfo {
            element_decl: Some(elem_key),
            attribute_decl: None,
            schema_type: type_key,
            member_type: None,
            validity: SchemaValidity::Valid,
            is_default: false,
            is_nil,
            content_type: Some(content_type),
            typed_value: None,
        }
    }

    /// Validate an attribute (string-based lookup)
    pub fn validate_attribute(
        &mut self,
        local_name: &str,
        namespace_uri: &str,
        value: &str,
    ) -> SchemaInfo {
        let name_id = self.schema_set.name_table.add(local_name);
        let ns_id = if namespace_uri.is_empty() {
            None
        } else {
            Some(self.schema_set.name_table.add(namespace_uri))
        };
        self.validate_attribute_by_id(name_id, ns_id, value)
    }

    /// Validate an attribute (NameId fast-path)
    pub fn validate_attribute_by_id(
        &mut self,
        local_name: NameId,
        namespace: Option<NameId>,
        value: &str,
    ) -> SchemaInfo {
        // 1. State machine check
        if !self.current_state.can_validate_attribute() {
            self.report_error(
                "cvc-complex-type",
                format!(
                    "Attribute validation not allowed in current state {:?}",
                    self.current_state
                ),
            );
            return SchemaInfo::invalid();
        }

        let ev_state = match self.validation_stack.last_mut() {
            Some(s) => s,
            None => {
                self.report_error("cvc-complex-type", "No element context for attribute");
                return SchemaInfo::invalid();
            }
        };

        // Skip xsi:* attributes — they are processed by validate_element
        if namespace == Some(well_known::XSI_NAMESPACE) {
            self.current_state = ValidatorState::Attribute;
            return SchemaInfo::empty();
        }

        // Skip xml:* attributes when ALLOW_XML_ATTRIBUTES is set
        if namespace == Some(well_known::XML_NAMESPACE)
            && self.flags.contains(ValidationFlags::ALLOW_XML_ATTRIBUTES)
        {
            self.current_state = ValidatorState::Attribute;
            return SchemaInfo::empty();
        }

        // 2. Duplicate check
        let attr_pair = (namespace, local_name);
        if !ev_state.seen_attributes.insert(attr_pair) {
            let attr_name = self.schema_set.name_table.resolve(local_name);
            self.report_error(
                "cvc-complex-type.3",
                format!("Duplicate attribute '{}'", attr_name),
            );
            if let Some(s) = self.validation_stack.last_mut() {
                s.validity = SchemaValidity::Invalid;
            }
            self.current_state = ValidatorState::Attribute;
            return SchemaInfo::invalid();
        }

        // If the element has no type info, skip detailed attribute validation
        let type_key = ev_state.schema_type;
        let ct_key = match type_key {
            Some(TypeKey::Complex(ct)) => ct,
            _ => {
                // Simple type or no type: no attributes expected (except xsi:*)
                // For Skip/Lax process contents, just accept
                if ev_state.process_contents != ContentProcessing::Strict {
                    self.current_state = ValidatorState::Attribute;
                    return SchemaInfo::empty();
                }
                self.current_state = ValidatorState::Attribute;
                return SchemaInfo::empty();
            }
        };

        let ct_data = &self.schema_set.arenas.complex_types[ct_key];

        // 3. Find attribute in type's attribute list
        let found = self.find_attribute_in_type(ct_data, local_name, namespace);

        match found {
            Some((attr_key, attr_type, fixed_value)) => {
                // 6. Check fixed value
                if let Some(fixed) = fixed_value {
                    if value != fixed {
                        let attr_name = self.schema_set.name_table.resolve(local_name);
                        self.report_error(
                            "cvc-attribute.4",
                            format!(
                                "Attribute '{}' has fixed value '{}' but got '{}'",
                                attr_name, fixed, value
                            ),
                        );
                        if let Some(s) = self.validation_stack.last_mut() {
                            s.validity = SchemaValidity::Invalid;
                        }
                    }
                }

                // Validate attribute value against its simple type
                let mut member_type = None;
                let mut typed_value = None;
                let mut attr_validity = SchemaValidity::Valid;
                if let Some(type_key) = attr_type {
                    match super::simple::validate_simple_type(value, type_key, self.schema_set) {
                        Ok(result) => {
                            member_type = result.member_type;
                            typed_value = Some(result.typed_value);
                        }
                        Err(err) => {
                            let err = match &self.current_location {
                                Some(loc) => err.with_location(loc.clone()),
                                None => err,
                            };
                            let err = if self.element_path.is_empty() {
                                err
                            } else {
                                err.with_path(self.element_path.clone())
                            };
                            self.sink.on_error(err);
                            attr_validity = SchemaValidity::Invalid;
                            if let Some(s) = self.validation_stack.last_mut() {
                                s.validity = SchemaValidity::Invalid;
                            }
                        }
                    }
                }

                self.current_state = ValidatorState::Attribute;
                SchemaInfo {
                    element_decl: None,
                    attribute_decl: attr_key,
                    schema_type: attr_type,
                    member_type,
                    validity: attr_validity,
                    is_default: false,
                    is_nil: false,
                    content_type: None,
                    typed_value,
                }
            }
            None => {
                // 4. Check attribute wildcard
                if ct_data.attribute_wildcard.is_some() {
                    // Wildcard allows this attribute
                    self.current_state = ValidatorState::Attribute;
                    return SchemaInfo::empty();
                }

                // Not found and no wildcard
                let attr_name = self.schema_set.name_table.resolve(local_name);
                self.report_error(
                    "cvc-complex-type.3.2.2",
                    format!(
                        "Attribute '{}' is not allowed for this element",
                        attr_name
                    ),
                );
                if let Some(s) = self.validation_stack.last_mut() {
                    s.validity = SchemaValidity::Invalid;
                }
                self.current_state = ValidatorState::Attribute;
                SchemaInfo::invalid()
            }
        }
    }

    /// Signal end of attributes; checks for missing required attributes
    pub fn validate_end_of_attributes(&mut self) -> SchemaInfo {
        if !self.current_state.can_end_attributes() {
            self.report_error(
                "cvc-complex-type",
                format!(
                    "End-of-attributes not allowed in current state {:?}",
                    self.current_state
                ),
            );
            return SchemaInfo::invalid();
        }

        // Extract what we need before calling check_required_attributes
        let (schema_type, seen_attributes) = match self.validation_stack.last() {
            Some(s) => (s.schema_type, s.seen_attributes.clone()),
            None => {
                self.current_state = ValidatorState::EndOfAttributes;
                return SchemaInfo::empty();
            }
        };

        // Check required attributes
        if let Some(TypeKey::Complex(ct_key)) = schema_type {
            let ct_data = &self.schema_set.arenas.complex_types[ct_key];
            if self.check_required_attributes(ct_data, &seen_attributes) {
                if let Some(ev_state) = self.validation_stack.last_mut() {
                    ev_state.validity = SchemaValidity::Invalid;
                }
            }
        }

        self.current_state = ValidatorState::EndOfAttributes;
        SchemaInfo::empty()
    }

    /// Validate a text content event
    pub fn validate_text(&mut self, text: &str) {
        if !self.current_state.can_validate_text() {
            self.report_error(
                "cvc-complex-type",
                format!(
                    "Text content not allowed in current state {:?}",
                    self.current_state
                ),
            );
            return;
        }

        // Collect errors first to avoid borrow conflicts
        let mut pending_errors: Vec<(&'static str, String)> = Vec::new();
        let has_non_ws = !text.trim().is_empty();

        if let Some(ev_state) = self.validation_stack.last_mut() {
            // 1. Check content type
            if has_non_ws {
                match ev_state.content_type {
                    Some(ContentType::Empty) => {
                        let elem_name = self
                            .schema_set
                            .name_table
                            .resolve(ev_state.local_name)
                            .to_string();
                        pending_errors.push((
                            "cvc-complex-type.2.1",
                            format!(
                                "Element '{}' has empty content type but text was found",
                                elem_name,
                            ),
                        ));
                    }
                    Some(ContentType::ElementOnly) => {
                        let elem_name = self
                            .schema_set
                            .name_table
                            .resolve(ev_state.local_name)
                            .to_string();
                        pending_errors.push((
                            "cvc-complex-type.2.3",
                            format!(
                                "Element '{}' has element-only content but non-whitespace text was found",
                                elem_name,
                            ),
                        ));
                    }
                    _ => {}
                }

                // 2. Check xsi:nil
                if ev_state.is_nil {
                    let elem_name = self
                        .schema_set
                        .name_table
                        .resolve(ev_state.local_name)
                        .to_string();
                    pending_errors.push((
                        "cvc-elt.3.1",
                        format!(
                            "Element '{}' is nilled but has non-empty text content",
                            elem_name,
                        ),
                    ));
                }
            }

            // 3. Accumulate text
            ev_state.text_content.push_str(text);
            ev_state.has_text = true;
        }

        // Report collected errors
        for (constraint, message) in pending_errors {
            self.report_error(constraint, message);
        }

        self.current_state = ValidatorState::Text;
    }

    /// Validate a whitespace-only text event
    ///
    /// Whitespace is always allowed in element-only content (it is insignificant).
    pub fn validate_whitespace(&mut self, text: &str) {
        if !self.current_state.can_validate_text() {
            self.report_error(
                "cvc-complex-type",
                format!(
                    "Whitespace not allowed in current state {:?}",
                    self.current_state
                ),
            );
            return;
        }

        if let Some(ev_state) = self.validation_stack.last_mut() {
            // Check xsi:nil for non-empty whitespace
            if ev_state.is_nil && !text.is_empty() {
                // Whitespace in nilled element is borderline; for now, accumulate but
                // the final check is done in end_element for non-empty text_content
            }

            // Accumulate (may be needed for TextOnly simple type validation)
            if matches!(
                ev_state.content_type,
                Some(ContentType::TextOnly) | Some(ContentType::Mixed)
            ) {
                ev_state.text_content.push_str(text);
                ev_state.has_text = true;
            }
        }

        self.current_state = ValidatorState::Whitespace;
    }

    /// Validate an element end event
    pub fn validate_end_element(&mut self) -> SchemaInfo {
        if !self.current_state.can_end_element() {
            self.report_error(
                "cvc-complex-type",
                format!(
                    "End element not allowed in current state {:?}",
                    self.current_state
                ),
            );
            return SchemaInfo::invalid();
        }

        let mut ev_state = match self.validation_stack.pop() {
            Some(s) => s,
            None => {
                self.report_error(
                    "cvc-complex-type",
                    "End element called but validation stack is empty",
                );
                return SchemaInfo::invalid();
            }
        };

        // 1. Check content model completion
        if !ev_state.is_nil {
            match ev_state.content_type {
                Some(ContentType::ElementOnly) | Some(ContentType::Mixed) => {
                    if !ev_state.content_state.is_complete() {
                        let elem_name =
                            self.schema_set.name_table.resolve(ev_state.local_name);
                        self.report_error(
                            "cvc-complex-type.2.4",
                            format!(
                                "Element '{}' content model is incomplete: expected more child elements",
                                elem_name,
                            ),
                        );
                        ev_state.validity = SchemaValidity::Invalid;
                    }
                }
                _ => {}
            }
        }

        // 2. For TextOnly: validate text content against simple type
        if ev_state.content_type == Some(ContentType::TextOnly) && !ev_state.is_nil {
            // Handle default value before validation: if the element has no text
            // content and has a default, substitute the default value so that
            // simple-type validation runs against it (not the empty string).
            if let Some(elem_key) = ev_state.element_decl {
                let elem_data = &self.schema_set.arenas.elements[elem_key];
                if !ev_state.has_text && !ev_state.has_element_children {
                    if let Some(default_value) = &elem_data.default_value {
                        ev_state.is_default = true;
                        ev_state.text_content = default_value.clone();
                    }
                }
            }

            if let Some(schema_type) = ev_state.schema_type {
                match super::simple::validate_simple_type(
                    &ev_state.text_content,
                    schema_type,
                    self.schema_set,
                ) {
                    Ok(result) => {
                        ev_state.member_type = result.member_type;
                        ev_state.typed_value = Some(result.typed_value);
                    }
                    Err(err) => {
                        let err = match &self.current_location {
                            Some(loc) => err.with_location(loc.clone()),
                            None => err,
                        };
                        let err = if self.element_path.is_empty() {
                            err
                        } else {
                            err.with_path(self.element_path.clone())
                        };
                        self.sink.on_error(err);
                        ev_state.validity = SchemaValidity::Invalid;
                    }
                }
            }

            // Check fixed value on element
            if let Some(elem_key) = ev_state.element_decl {
                let elem_data = &self.schema_set.arenas.elements[elem_key];
                if let Some(fixed) = &elem_data.fixed_value {
                    if ev_state.text_content != *fixed {
                        let elem_name =
                            self.schema_set.name_table.resolve(ev_state.local_name);
                        self.report_error(
                            "cvc-elt.5.2.2",
                            format!(
                                "Element '{}' has fixed value '{}' but actual value is '{}'",
                                elem_name, fixed, ev_state.text_content,
                            ),
                        );
                        ev_state.validity = SchemaValidity::Invalid;
                    }
                }
            }
        }

        // 3. Update element path
        self.pop_element_path();

        let validity = ev_state.validity;
        self.current_state = ValidatorState::EndElement;

        SchemaInfo {
            element_decl: ev_state.element_decl,
            attribute_decl: None,
            schema_type: ev_state.schema_type,
            member_type: ev_state.member_type,
            validity,
            is_default: ev_state.is_default,
            is_nil: ev_state.is_nil,
            content_type: ev_state.content_type,
            typed_value: ev_state.typed_value,
        }
    }

    /// Finalize validation
    ///
    /// Checks that the validation stack is empty. IDREF/keyref checks
    /// will be added in Task 5.6.
    pub fn end_validation(&mut self) -> Result<(), ValidationError> {
        if !self.current_state.can_finish() {
            return Err(errors::error(
                "cvc-complex-type",
                format!(
                    "end_validation called in invalid state {:?}",
                    self.current_state
                ),
                self.current_location.clone(),
            ));
        }

        if !self.validation_stack.is_empty() {
            return Err(errors::error(
                "cvc-complex-type",
                format!(
                    "Validation ended with {} unclosed elements",
                    self.validation_stack.len()
                ),
                self.current_location.clone(),
            ));
        }

        self.current_state = ValidatorState::Finish;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Query API
    // -----------------------------------------------------------------------

    /// Get elements expected at the current position in the content model
    pub fn get_expected_elements(&self) -> Vec<ExpectedElement> {
        let ev_state = match self.validation_stack.last() {
            Some(s) => s,
            None => return Vec::new(),
        };

        match &ev_state.content_state {
            ContentValidatorState::Nfa { nfa, active_states } => {
                let mut result = Vec::new();
                let closure =
                    crate::compiler::epsilon_closure(nfa, active_states.iter().copied());
                for state_id in closure {
                    if let Some(state) = nfa.get_state(state_id) {
                        if let Some(crate::compiler::NfaTerm::Element {
                            ref name,
                            ref namespace,
                            ref element_key,
                            ..
                        }) = state.term
                        {
                            result.push(ExpectedElement {
                                local_name: *name,
                                namespace: *namespace,
                                element_key: *element_key,
                            });
                        }
                    }
                }
                result
            }
            ContentValidatorState::AllGroup { model, state } => {
                let mut result = Vec::new();
                for (i, particle) in model.particles.iter().enumerate() {
                    if state.can_accept(i) {
                        if let crate::compiler::NfaTerm::Element {
                            ref name,
                            ref namespace,
                            ref element_key,
                            ..
                        } = particle.term
                        {
                            result.push(ExpectedElement {
                                local_name: *name,
                                namespace: *namespace,
                                element_key: *element_key,
                            });
                        }
                    }
                }
                result
            }
            _ => Vec::new(),
        }
    }

    /// Get attributes expected/allowed for the current element
    pub fn get_expected_attributes(&self) -> Vec<ExpectedAttribute> {
        let ev_state = match self.validation_stack.last() {
            Some(s) => s,
            None => return Vec::new(),
        };

        let ct_key = match ev_state.schema_type {
            Some(TypeKey::Complex(ct)) => ct,
            _ => return Vec::new(),
        };

        let ct_data = &self.schema_set.arenas.complex_types[ct_key];
        let mut result = Vec::new();

        for (i, attr_use) in ct_data.attributes.iter().enumerate() {
            let use_kind = attr_use.use_kind;
            if use_kind == AttributeUseKind::Prohibited {
                continue;
            }
            let attr_name = attr_use.attribute.name.unwrap_or(well_known::EMPTY);
            let attr_ns = attr_use.attribute.target_namespace;
            let resolved = ct_data.resolved_attributes.get(i);
            let attr_key = resolved.and_then(|r| r.resolved_ref);

            result.push(ExpectedAttribute {
                local_name: attr_name,
                namespace: attr_ns,
                attribute_key: attr_key,
                required: use_kind == AttributeUseKind::Required,
            });
        }

        result
    }

    /// Get default attributes that should be added to the current element
    pub fn get_default_attributes(&self) -> Vec<DefaultAttribute> {
        let ev_state = match self.validation_stack.last() {
            Some(s) => s,
            None => return Vec::new(),
        };

        let ct_key = match ev_state.schema_type {
            Some(TypeKey::Complex(ct)) => ct,
            _ => return Vec::new(),
        };

        let ct_data = &self.schema_set.arenas.complex_types[ct_key];
        let mut result = Vec::new();

        for (i, attr_use) in ct_data.attributes.iter().enumerate() {
            if attr_use.use_kind == AttributeUseKind::Prohibited {
                continue;
            }

            let attr_name = attr_use.attribute.name.unwrap_or(well_known::EMPTY);
            let attr_ns = attr_use.attribute.target_namespace;

            // Skip if already provided
            if ev_state.seen_attributes.contains(&(attr_ns, attr_name)) {
                continue;
            }

            // Check for default value — first on the use, then on the decl
            let default = attr_use.attribute.default_value.as_deref();
            if let Some(value) = default {
                let resolved = ct_data.resolved_attributes.get(i);
                if let Some(attr_key) = resolved.and_then(|r| r.resolved_ref) {
                    result.push(DefaultAttribute {
                        local_name: attr_name,
                        namespace: attr_ns,
                        attribute_key: attr_key,
                        value: value.to_string(),
                    });
                }
            }
        }

        result
    }

    /// Get the content processing mode for the current element
    pub fn content_processing(&self) -> ContentProcessing {
        self.validation_stack
            .last()
            .map(|s| s.process_contents)
            .unwrap_or(ContentProcessing::Strict)
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// Push a new element onto the validation stack and update the element path
    fn push_element(&mut self, ev_state: ElementValidationState) {
        let local_name = self.schema_set.name_table.resolve(ev_state.local_name);
        if !self.element_path.is_empty() || self.validation_stack.is_empty() {
            self.element_path.push('/');
        }
        self.element_path.push_str(&local_name);

        self.validation_stack.push(ev_state);

        if self.current_state == ValidatorState::None {
            self.current_state = ValidatorState::Start;
        }
        self.current_state = ValidatorState::Element;
    }

    /// Pop the last element from the element path
    fn pop_element_path(&mut self) {
        if let Some(pos) = self.element_path.rfind('/') {
            self.element_path.truncate(pos);
        } else {
            self.element_path.clear();
        }
    }

    /// Report a validation error through the sink
    fn report_error(&mut self, constraint: &'static str, message: impl Into<String>) {
        let err = errors::error(constraint, message, self.current_location.clone());
        let err = if self.element_path.is_empty() {
            err
        } else {
            err.with_path(self.element_path.clone())
        };
        self.sink.on_error(err);
    }

    /// Initialize content model and ContentType from a TypeKey
    fn init_content_model(&self, type_key: Option<TypeKey>) -> (ContentValidatorState, ContentType) {
        match type_key {
            Some(TypeKey::Complex(ct_key)) => {
                let ct_data = &self.schema_set.arenas.complex_types[ct_key];
                let content_type = self.determine_content_type(ct_data);

                let content_state = match content_type {
                    ContentType::Empty => ContentValidatorState::Empty,
                    ContentType::TextOnly => ContentValidatorState::Simple,
                    ContentType::ElementOnly | ContentType::Mixed => {
                        match compile_content_model_matcher(self.schema_set, ct_data) {
                            Ok(matcher) => ContentValidatorState::from_matcher(matcher),
                            Err(_) => {
                                // Compilation error — treat as empty
                                ContentValidatorState::Empty
                            }
                        }
                    }
                };

                (content_state, content_type)
            }
            Some(TypeKey::Simple(_)) => (ContentValidatorState::Simple, ContentType::TextOnly),
            None => (ContentValidatorState::Simple, ContentType::TextOnly),
        }
    }

    /// Determine the ContentType from a ComplexTypeDefData
    fn determine_content_type(&self, ct_data: &ComplexTypeDefData) -> ContentType {
        use crate::parser::frames::ComplexContentResult;
        use crate::parser::frames::DerivationMethod;
        match &ct_data.content {
            ComplexContentResult::Empty => ContentType::Empty,
            ComplexContentResult::Simple(_) => ContentType::TextOnly,
            ComplexContentResult::Complex(def) => {
                if def.particle.is_none() && !ct_data.mixed {
                    // For extensions with no own particle, inherit base type's content type
                    if matches!(ct_data.derivation_method, Some(DerivationMethod::Extension)) {
                        if let Some(TypeKey::Complex(base_ct_key)) = ct_data.resolved_base_type {
                            let base_data = &self.schema_set.arenas.complex_types[base_ct_key];
                            return self.determine_content_type(base_data);
                        }
                    }
                    ContentType::Empty
                } else if ct_data.mixed || def.mixed {
                    ContentType::Mixed
                } else {
                    ContentType::ElementOnly
                }
            }
        }
    }

    /// Resolve an xsi:type QName string to a TypeKey
    fn resolve_xsi_type(
        &mut self,
        xsi_type_str: &str,
        declared_type: Option<TypeKey>,
        ns_context: &NamespaceContextSnapshot,
    ) -> Option<TypeKey> {
        // Parse QName: "prefix:local" or "local"
        let (prefix, local) = match xsi_type_str.find(':') {
            Some(pos) => (&xsi_type_str[..pos], &xsi_type_str[pos + 1..]),
            None => ("", xsi_type_str),
        };

        // Resolve prefix to namespace
        let namespace = if prefix.is_empty() {
            ns_context.default_namespace()
        } else {
            let prefix_id = self.schema_set.name_table.add(prefix);
            match ns_context.resolve_prefix(prefix_id) {
                Some(ns) => Some(ns),
                None => {
                    self.report_error(
                        "cvc-elt.4.1",
                        format!("Undeclared prefix '{}' in xsi:type value '{}'", prefix, xsi_type_str),
                    );
                    return None;
                }
            }
        };

        let local_id = self.schema_set.name_table.add(local);

        // Look up the type
        let resolved = self
            .schema_set
            .lookup_type(namespace, local_id)
            .or_else(|| self.schema_set.get_built_in_type_by_qname(namespace, local_id));

        match resolved {
            Some(type_key) => {
                // Validate derivation: the xsi:type must derive from the declared type
                if let Some(declared) = declared_type {
                    if !self.schema_set.is_type_derived_from(
                        type_key,
                        declared,
                        DerivationSet::empty(),
                    ) {
                        self.report_error(
                            "cvc-elt.4.2",
                            format!(
                                "xsi:type '{}' does not derive from the declared type",
                                xsi_type_str
                            ),
                        );
                    }
                }
                Some(type_key)
            }
            None => {
                self.report_error(
                    "cvc-elt.4.1",
                    format!("Type '{}' specified in xsi:type is not declared", xsi_type_str),
                );
                None
            }
        }
    }

    /// Find an attribute declaration in a complex type's attribute list
    ///
    /// Returns (attribute_key, type_key, fixed_value) if found.
    fn find_attribute_in_type(
        &self,
        ct_data: &ComplexTypeDefData,
        local_name: NameId,
        namespace: Option<NameId>,
    ) -> Option<(Option<AttributeKey>, Option<TypeKey>, Option<String>)> {
        for (i, attr_use) in ct_data.attributes.iter().enumerate() {
            let attr_name = attr_use.attribute.name.unwrap_or(well_known::EMPTY);
            let attr_ns = attr_use.attribute.target_namespace;

            if attr_name == local_name && attr_ns == namespace {
                if attr_use.use_kind == AttributeUseKind::Prohibited {
                    return None; // prohibited means not allowed
                }

                let resolved = ct_data.resolved_attributes.get(i);
                let attr_key = resolved.and_then(|r| r.resolved_ref);
                let attr_type = resolved.and_then(|r| r.resolved_type);

                // Get fixed value from the attribute use or from the attribute declaration
                let fixed = attr_use
                    .attribute
                    .fixed_value
                    .clone()
                    .or_else(|| {
                        attr_key
                            .and_then(|k| self.schema_set.arenas.attributes.get(k))
                            .and_then(|d| d.fixed_value.clone())
                    });

                return Some((attr_key, attr_type, fixed));
            }
        }
        None
    }

    /// Check that all required attributes are present
    fn check_required_attributes(
        &mut self,
        ct_data: &ComplexTypeDefData,
        seen: &HashSet<(Option<NameId>, NameId)>,
    ) -> bool {
        let mut has_missing = false;
        for attr_use in &ct_data.attributes {
            if attr_use.use_kind != AttributeUseKind::Required {
                continue;
            }

            let attr_name = attr_use.attribute.name.unwrap_or(well_known::EMPTY);
            let attr_ns = attr_use.attribute.target_namespace;

            if !seen.contains(&(attr_ns, attr_name)) {
                let name_str = self.schema_set.name_table.resolve(attr_name);
                self.report_error(
                    "cvc-complex-type.4",
                    format!("Required attribute '{}' is missing", name_str),
                );
                has_missing = true;
            }
        }
        has_missing
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::namespace::context::NamespaceContextSnapshot;
    use crate::pipeline::load_and_process_schema;

    /// A simple test sink that collects errors
    struct TestSink {
        errors: Vec<ValidationError>,
        warnings: Vec<ValidationWarning>,
    }

    impl TestSink {
        fn new() -> Self {
            TestSink {
                errors: Vec::new(),
                warnings: Vec::new(),
            }
        }
    }

    impl ValidationSink for TestSink {
        fn on_error(&mut self, error: ValidationError) {
            self.errors.push(error);
        }
        fn on_warning(&mut self, warning: ValidationWarning) {
            self.warnings.push(warning);
        }
    }

    fn empty_ns_context() -> NamespaceContextSnapshot {
        NamespaceContextSnapshot {
            default_ns: None,
            bindings: Vec::new(),
        }
    }

    fn load_schema(xsd: &str) -> SchemaSet {
        let mut schema_set = SchemaSet::new();
        load_and_process_schema(xsd.as_bytes(), "test.xsd", &mut schema_set, None)
            .expect("failed to load schema");
        schema_set
    }

    #[test]
    fn test_simple_element_valid() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root" type="xs:string"/>
            </xs:schema>"#,
        );

        let sink = TestSink::new();
        let mut v = SchemaValidator::new(&schema_set, sink, ValidationFlags::default());
        let ns = empty_ns_context();

        let info = v.validate_element("root", "", None, None, &ns);
        assert_eq!(info.validity, SchemaValidity::Valid);
        assert!(info.element_decl.is_some());
        assert!(info.schema_type.is_some());

        v.validate_end_of_attributes();
        v.validate_text("hello world");

        let end_info = v.validate_end_element();
        assert_eq!(end_info.validity, SchemaValidity::Valid);

        assert!(v.end_validation().is_ok());
        assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
    }

    #[test]
    fn test_unknown_element_strict() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root" type="xs:string"/>
            </xs:schema>"#,
        );

        let sink = TestSink::new();
        let mut v = SchemaValidator::new(&schema_set, sink, ValidationFlags::default());
        let ns = empty_ns_context();

        let info = v.validate_element("unknown", "", None, None, &ns);
        assert_eq!(info.validity, SchemaValidity::Invalid);

        // Should have cvc-elt.1 error
        assert!(v.sink.errors.iter().any(|e| e.constraint == "cvc-elt.1"));

        v.validate_end_of_attributes();
        v.validate_end_element();
        assert!(v.end_validation().is_ok());
    }

    #[test]
    fn test_sequence_content_model() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="a" type="xs:string"/>
                            <xs:element name="b" type="xs:string"/>
                        </xs:sequence>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"#,
        );

        let sink = TestSink::new();
        let mut v = SchemaValidator::new(&schema_set, sink, ValidationFlags::default());
        let ns = empty_ns_context();

        // Open root
        v.validate_element("root", "", None, None, &ns);
        v.validate_end_of_attributes();

        // Children in correct order
        v.validate_element("a", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_text("hello");
        v.validate_end_element();

        v.validate_element("b", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_text("world");
        v.validate_end_element();

        // Close root
        v.validate_end_element();
        assert!(v.end_validation().is_ok());
        assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
    }

    #[test]
    fn test_sequence_wrong_order() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="a" type="xs:string"/>
                            <xs:element name="b" type="xs:string"/>
                        </xs:sequence>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"#,
        );

        let sink = TestSink::new();
        let mut v = SchemaValidator::new(&schema_set, sink, ValidationFlags::default());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);
        v.validate_end_of_attributes();

        // Wrong order: b before a
        v.validate_element("b", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_end_element();

        v.validate_element("a", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_end_element();

        v.validate_end_element();
        v.end_validation().ok();

        // Should have content model error
        assert!(
            v.sink.errors.iter().any(|e| e.constraint == "cvc-complex-type.2.4"),
            "errors: {:?}",
            v.sink.errors
        );
    }

    #[test]
    fn test_required_attribute_missing() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root">
                    <xs:complexType>
                        <xs:simpleContent>
                            <xs:extension base="xs:string">
                                <xs:attribute name="id" type="xs:string" use="required"/>
                            </xs:extension>
                        </xs:simpleContent>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"#,
        );

        let sink = TestSink::new();
        let mut v = SchemaValidator::new(&schema_set, sink, ValidationFlags::default());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);
        // Don't provide any attributes
        v.validate_end_of_attributes();
        v.validate_text("hello");
        v.validate_end_element();
        v.end_validation().ok();

        assert!(
            v.sink.errors.iter().any(|e| e.constraint == "cvc-complex-type.4"),
            "expected required attribute error, got: {:?}",
            v.sink.errors
        );
    }

    #[test]
    fn test_duplicate_attribute() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root">
                    <xs:complexType>
                        <xs:simpleContent>
                            <xs:extension base="xs:string">
                                <xs:attribute name="id" type="xs:string"/>
                            </xs:extension>
                        </xs:simpleContent>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"#,
        );

        let sink = TestSink::new();
        let mut v = SchemaValidator::new(&schema_set, sink, ValidationFlags::default());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);
        v.validate_attribute("id", "", "val1");
        v.validate_attribute("id", "", "val2"); // duplicate
        v.validate_end_of_attributes();
        v.validate_text("hello");
        v.validate_end_element();
        v.end_validation().ok();

        assert!(
            v.sink.errors.iter().any(|e| e.constraint == "cvc-complex-type.3"),
            "expected duplicate attribute error, got: {:?}",
            v.sink.errors
        );
    }

    #[test]
    fn test_text_in_empty_content() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root">
                    <xs:complexType/>
                </xs:element>
            </xs:schema>"#,
        );

        let sink = TestSink::new();
        let mut v = SchemaValidator::new(&schema_set, sink, ValidationFlags::default());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_text("not allowed");
        v.validate_end_element();
        v.end_validation().ok();

        assert!(
            v.sink.errors.iter().any(|e| e.constraint == "cvc-complex-type.2.1"),
            "expected empty content error, got: {:?}",
            v.sink.errors
        );
    }

    #[test]
    fn test_state_machine_attribute_before_element() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root" type="xs:string"/>
            </xs:schema>"#,
        );

        let sink = TestSink::new();
        let mut v = SchemaValidator::new(&schema_set, sink, ValidationFlags::default());

        // Try to validate attribute before any element — should error
        let info = v.validate_attribute("id", "", "val");
        assert_eq!(info.validity, SchemaValidity::Invalid);
        assert!(!v.sink.errors.is_empty());
    }

    #[test]
    fn test_xsi_type_override() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root" type="xs:anyType"/>
                <xs:complexType name="myType">
                    <xs:sequence>
                        <xs:element name="child" type="xs:string"/>
                    </xs:sequence>
                </xs:complexType>
            </xs:schema>"#,
        );

        let sink = TestSink::new();
        let mut v = SchemaValidator::new(&schema_set, sink, ValidationFlags::default());
        let ns = empty_ns_context();

        // Use xsi:type to override the element type
        let info = v.validate_element("root", "", Some("myType"), None, &ns);
        assert_eq!(info.validity, SchemaValidity::Valid);
        // The schema_type should be the overridden type, not anyType
        assert!(info.schema_type.is_some());

        v.validate_end_of_attributes();
        v.validate_element("child", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_text("hello");
        v.validate_end_element();
        v.validate_end_element();
        v.end_validation().ok();

        assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
    }

    #[test]
    fn test_xsi_nil_on_nillable_element() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="child" type="xs:string" nillable="true"/>
                        </xs:sequence>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"#,
        );

        let sink = TestSink::new();
        let mut v = SchemaValidator::new(&schema_set, sink, ValidationFlags::default());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);
        v.validate_end_of_attributes();

        let info = v.validate_element("child", "", None, Some("true"), &ns);
        assert!(info.is_nil);
        assert_eq!(info.validity, SchemaValidity::Valid);

        v.validate_end_of_attributes();
        // Empty content is valid for nilled element
        v.validate_end_element();
        v.validate_end_element();
        v.end_validation().ok();

        assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
    }

    #[test]
    fn test_end_validation_with_unclosed_elements() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root" type="xs:string"/>
            </xs:schema>"#,
        );

        let sink = TestSink::new();
        let mut v = SchemaValidator::new(&schema_set, sink, ValidationFlags::default());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);
        v.validate_end_of_attributes();

        // Don't close the element — end_validation should fail
        let result = v.end_validation();
        assert!(result.is_err());
    }

    #[test]
    fn test_local_element_with_complex_type() {
        // Local element with type="addressType" (a named complex type).
        // Verify schema_type is resolved and children are validated.
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:complexType name="addressType">
                    <xs:sequence>
                        <xs:element name="street" type="xs:string"/>
                        <xs:element name="city" type="xs:string"/>
                    </xs:sequence>
                </xs:complexType>
                <xs:element name="root">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="address" type="addressType"/>
                        </xs:sequence>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"#,
        );

        let sink = TestSink::new();
        let mut v = SchemaValidator::new(&schema_set, sink, ValidationFlags::default());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);
        v.validate_end_of_attributes();

        let info = v.validate_element("address", "", None, None, &ns);
        assert_eq!(info.validity, SchemaValidity::Valid);
        assert!(info.schema_type.is_some(), "local element should have resolved type");
        assert!(
            matches!(info.content_type, Some(ContentType::ElementOnly)),
            "addressType has element-only content, got {:?}",
            info.content_type,
        );

        v.validate_end_of_attributes();

        // Children should be validated against the content model
        v.validate_element("street", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_text("123 Main St");
        v.validate_end_element();

        v.validate_element("city", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_text("Springfield");
        v.validate_end_element();

        v.validate_end_element(); // close address
        v.validate_end_element(); // close root
        assert!(v.end_validation().is_ok());
        assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
    }

    #[test]
    fn test_local_element_with_simple_type_resolved() {
        // Local element with type="xs:integer". Verify schema_type is set.
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="count" type="xs:integer"/>
                        </xs:sequence>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"#,
        );

        let sink = TestSink::new();
        let mut v = SchemaValidator::new(&schema_set, sink, ValidationFlags::default());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);
        v.validate_end_of_attributes();

        let info = v.validate_element("count", "", None, None, &ns);
        assert_eq!(info.validity, SchemaValidity::Valid);
        assert!(info.schema_type.is_some(), "local element should have resolved type for xs:integer");
        assert_eq!(info.content_type, Some(ContentType::TextOnly));

        v.validate_end_of_attributes();
        v.validate_text("42");
        v.validate_end_element();

        v.validate_end_element();
        assert!(v.end_validation().is_ok());
        assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
    }

    #[test]
    fn test_local_element_complex_type_rejects_wrong_children() {
        // Local element with type="myType" containing wrong child element.
        // Verify content model error is reported.
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:complexType name="myType">
                    <xs:sequence>
                        <xs:element name="expected" type="xs:string"/>
                    </xs:sequence>
                </xs:complexType>
                <xs:element name="root">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="item" type="myType"/>
                        </xs:sequence>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"#,
        );

        let sink = TestSink::new();
        let mut v = SchemaValidator::new(&schema_set, sink, ValidationFlags::default());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);
        v.validate_end_of_attributes();

        v.validate_element("item", "", None, None, &ns);
        v.validate_end_of_attributes();

        // Wrong child element - should trigger content model error
        v.validate_element("wrong", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_end_element();

        v.validate_end_element(); // close item
        v.validate_end_element(); // close root
        v.end_validation().ok();

        assert!(
            v.sink.errors.iter().any(|e| e.constraint == "cvc-complex-type.2.4"),
            "expected content model error for wrong child, got: {:?}",
            v.sink.errors
        );
    }

    #[test]
    fn test_local_element_with_inline_type_fallback() {
        // Local element with inline <xs:simpleType>. Verify graceful fallback.
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="code">
                                <xs:simpleType>
                                    <xs:restriction base="xs:string">
                                        <xs:maxLength value="10"/>
                                    </xs:restriction>
                                </xs:simpleType>
                            </xs:element>
                        </xs:sequence>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"#,
        );

        let sink = TestSink::new();
        let mut v = SchemaValidator::new(&schema_set, sink, ValidationFlags::default());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);
        v.validate_end_of_attributes();

        let info = v.validate_element("code", "", None, None, &ns);
        assert_eq!(info.validity, SchemaValidity::Valid);
        // Inline type is not resolved at compile time — fallback to TextOnly
        assert_eq!(info.content_type, Some(ContentType::TextOnly));

        v.validate_end_of_attributes();
        v.validate_text("ABC");
        v.validate_end_element();

        v.validate_end_element();
        assert!(v.end_validation().is_ok());
        assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
    }

    #[test]
    fn test_xsi_type_on_local_element() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:complexType name="baseType">
                    <xs:sequence>
                        <xs:element name="name" type="xs:string"/>
                    </xs:sequence>
                </xs:complexType>
                <xs:complexType name="derivedType">
                    <xs:complexContent>
                        <xs:extension base="baseType">
                            <xs:sequence>
                                <xs:element name="extra" type="xs:string"/>
                            </xs:sequence>
                        </xs:extension>
                    </xs:complexContent>
                </xs:complexType>
                <xs:element name="root">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="item" type="baseType"/>
                        </xs:sequence>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"#,
        );

        let sink = TestSink::new();
        let mut v = SchemaValidator::new(&schema_set, sink, ValidationFlags::default());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);
        v.validate_end_of_attributes();

        let info = v.validate_element("item", "", Some("derivedType"), None, &ns);
        assert_eq!(info.validity, SchemaValidity::Valid);
        assert!(info.schema_type.is_some(), "schema_type should reflect overridden type");

        v.validate_end_of_attributes();

        // derivedType = sequence(name, extra)
        v.validate_element("name", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_text("test");
        v.validate_end_element();

        v.validate_element("extra", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_text("additional");
        v.validate_end_element();

        v.validate_end_element(); // close item
        v.validate_end_element(); // close root
        assert!(v.end_validation().is_ok());
        assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
    }

    #[test]
    fn test_group_ref_with_nillable_fixed_default() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:group name="fields">
                    <xs:sequence>
                        <xs:element name="nillableField" type="xs:string" nillable="true"/>
                        <xs:element name="fixedField" type="xs:string" fixed="LOCKED"/>
                        <xs:element name="defaultField" type="xs:string" default="fallback"/>
                    </xs:sequence>
                </xs:group>
                <xs:element name="root">
                    <xs:complexType>
                        <xs:group ref="fields"/>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"#,
        );

        let sink = TestSink::new();
        let mut v = SchemaValidator::new(&schema_set, sink, ValidationFlags::default());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);
        v.validate_end_of_attributes();

        // 1. Nillable from group — xsi:nil="true" should be accepted
        let info = v.validate_element("nillableField", "", None, Some("true"), &ns);
        assert!(info.is_nil, "nillableField should report is_nil=true");
        assert_eq!(info.validity, SchemaValidity::Valid);
        v.validate_end_of_attributes();
        v.validate_end_element();

        // 2. Fixed value mismatch from group — wrong text should produce cvc-elt.5.2.2
        v.validate_element("fixedField", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_text("WRONG");
        let end_info = v.validate_end_element();
        assert!(
            v.sink.errors.iter().any(|e| e.constraint == "cvc-elt.5.2.2"),
            "expected cvc-elt.5.2.2 for fixed value mismatch, errors: {:?}",
            v.sink.errors
        );
        assert_eq!(end_info.validity, SchemaValidity::Invalid);

        // 3. Default value from group — empty content should set is_default
        v.validate_element("defaultField", "", None, None, &ns);
        v.validate_end_of_attributes();
        let end_info = v.validate_end_element();
        assert!(
            end_info.is_default,
            "defaultField with no text should report is_default=true"
        );

        v.validate_end_element(); // close root
        assert!(v.end_validation().is_ok());
        // Only the fixed-value error is expected
        assert_eq!(
            v.sink.errors.len(),
            1,
            "expected exactly 1 error (cvc-elt.5.2.2), got: {:?}",
            v.sink.errors
        );
    }
}
