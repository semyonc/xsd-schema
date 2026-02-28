//! Core `SchemaValidator` — push-based, DOM-independent instance validation
//!
//! Callers push XML events (element start, attribute, text, element end) and
//! receive `SchemaInfo` decisions back. Errors and warnings are reported to
//! a `ValidationSink`.

use std::collections::{HashMap, HashSet};

use crate::arenas::{ComplexTypeDefData, ResolvedAttributeUse};
use crate::compiler::{compile_content_model_matcher, build_substitution_group_map, SubstitutionGroupMap};
use crate::ids::{AttributeGroupKey, ElementKey, IdentityConstraintKey, NameId, TypeKey, AttributeKey};
use crate::namespace::context::NamespaceContextSnapshot;
use crate::namespace::qname::{parse_qname_with_snapshot, QNameError};
use crate::namespace::table::well_known;
use crate::parser::frames::{AttributeUseKind, AttributeUseResult, IdentityKind, ProcessContents, WildcardNamespace, WildcardResult};
use crate::parser::location::SourceLocation;
use crate::schema::model::DerivationSet;
use crate::schema::SchemaSet;
use crate::types::XmlTypeCode;
use crate::types::value::XmlValue;

use super::content::ContentValidatorState;
use crate::types::complex::ProcessContents as TypesProcessContents;
use super::context::{ElementValidationState, ValidatorState};
use super::errors::{self, ValidationError};
use super::identity::{CompiledIdentityConstraint, ConstraintStruct, KeyTable};
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
// GroupAttribute — flat representation of an attribute from an attribute group
// ---------------------------------------------------------------------------

/// An attribute use collected from a resolved attribute group.
struct GroupAttribute {
    name: NameId,
    namespace: Option<NameId>,
    use_kind: AttributeUseKind,
    type_key: Option<TypeKey>,
    attr_key: Option<AttributeKey>,
    fixed_value: Option<String>,
    default_value: Option<String>,
}

// ---------------------------------------------------------------------------
// AttributeLookup — three-state result from find_attribute_in_type
// ---------------------------------------------------------------------------

/// Result of looking up an attribute in a complex type's attribute list.
enum AttributeLookup {
    /// Found a matching attribute declaration
    Found(Option<AttributeKey>, Option<TypeKey>, Option<String>),
    /// The attribute is explicitly prohibited
    Prohibited,
    /// No matching attribute found
    NotFound,
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
    /// Pre-compiled identity constraints (lazy cache; None = compilation failed)
    compiled_constraints: HashMap<IdentityConstraintKey, Option<CompiledIdentityConstraint>>,
    /// Active constraint state instances
    active_constraints: Vec<ConstraintStruct>,
    /// Collected ID values (for cvc-id.2 duplicate check and cvc-id.1 IDREF validation)
    id_values: HashSet<String>,
    /// Pending IDREF values: (value, location, element_path)
    pending_idrefs: Vec<(String, Option<SourceLocation>, String)>,
    /// Per-element scope stack of key/unique tables. Each entry corresponds to
    /// a validation stack frame. Tables propagate upward on element close,
    /// modelling the XSD "node table propagates to parent" semantics.
    ic_scope_tables: Vec<Option<HashMap<IdentityConstraintKey, KeyTable>>>,
}

impl<'a, S: ValidationSink> SchemaValidator<'a, S> {
    /// Create a new `SchemaValidator`
    pub fn new(schema_set: &'a SchemaSet, sink: S, flags: ValidationFlags) -> Self {
        let subst_groups = build_substitution_group_map(schema_set);
        SchemaValidator {
            schema_set,
            subst_groups: Some(subst_groups),
            sink,
            flags,
            validation_stack: Vec::new(),
            current_state: ValidatorState::None,
            current_location: None,
            element_path: String::new(),
            compiled_constraints: HashMap::new(),
            active_constraints: Vec::new(),
            id_values: HashSet::new(),
            pending_idrefs: Vec::new(),
            ic_scope_tables: Vec::new(),
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
        let mut nil_error: Option<String> = None;
        if let Some(parent) = self.validation_stack.last_mut() {
            if parent.is_nil {
                let parent_name = self
                    .schema_set
                    .name_table
                    .resolve(parent.local_name)
                    .to_string();
                nil_error = Some(format!(
                    "Element '{}' is nilled (xsi:nil='true') but has child element content",
                    parent_name,
                ));
            } else {
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
        }
        if let Some(msg) = nil_error {
            self.report_error("cvc-elt.3.2.1", msg);
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
        let process_contents = match_info
            .and_then(|i| i.process_contents)
            .map(|pc| match pc {
                TypesProcessContents::Strict => ContentProcessing::Strict,
                TypesProcessContents::Lax => ContentProcessing::Lax,
                TypesProcessContents::Skip => ContentProcessing::Skip,
            })
            .unwrap_or_else(|| {
                self.validation_stack
                    .last()
                    .map(|p| p.process_contents)
                    .unwrap_or(ContentProcessing::Strict)
            });

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
                self.advance_constraints_start_element(local_name, namespace, None);
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
                    self.advance_constraints_start_element(local_name, namespace, None);
                    return SchemaInfo::empty();
                }
                ContentProcessing::Lax => {
                    // Lax: skip if not found
                    let mut ev_state = ElementValidationState::new(local_name, namespace);
                    ev_state.process_contents = ContentProcessing::Lax;
                    ev_state.content_state = ContentValidatorState::Simple;
                    ev_state.validity = SchemaValidity::NotKnown;
                    self.push_element(ev_state);
                    self.advance_constraints_start_element(local_name, namespace, None);
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
                    self.advance_constraints_start_element(local_name, namespace, None);
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
        self.advance_constraints_start_element(local_name, namespace, Some(elem_key));

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
                // Still run post-processing so IC attribute fields and
                // ID/IDREF collection are not skipped.
                self.current_state = ValidatorState::Attribute;
                let result = SchemaInfo::empty();
                self.post_process_attribute(local_name, namespace, value, &result);
                return result;
            }
        };

        let ct_data = &self.schema_set.arenas.complex_types[ct_key];

        // 3. Find attribute in type's attribute list
        let found = self.find_attribute_in_type(ct_data, local_name, namespace);

        match found {
            AttributeLookup::Found(attr_key, attr_type, fixed_value) => {
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
                let result = SchemaInfo {
                    element_decl: None,
                    attribute_decl: attr_key,
                    schema_type: attr_type,
                    member_type,
                    validity: attr_validity,
                    is_default: false,
                    is_nil: false,
                    content_type: None,
                    typed_value,
                };
                self.post_process_attribute(local_name, namespace, value, &result);
                result
            }
            AttributeLookup::Prohibited => {
                let attr_name = self.schema_set.name_table.resolve(local_name);
                self.report_error(
                    "cvc-complex-type.3.2.2",
                    format!("Attribute '{}' is prohibited", attr_name),
                );
                if let Some(s) = self.validation_stack.last_mut() {
                    s.validity = SchemaValidity::Invalid;
                }
                self.current_state = ValidatorState::Attribute;
                SchemaInfo::invalid()
            }
            AttributeLookup::NotFound => {
                // 4. Check attribute wildcard (including from attribute groups)
                let effective_wildcard = self.find_effective_wildcard(ct_data);
                if let Some(ref wildcard) = effective_wildcard {
                    let target_ns = ct_data.target_namespace;
                    if self.wildcard_allows_namespace(wildcard, namespace, target_ns) {
                        self.current_state = ValidatorState::Attribute;
                        let result = match wildcard.process_contents {
                            ProcessContents::Skip => SchemaInfo::empty(),
                            ProcessContents::Strict => {
                                self.validate_wildcard_attribute_strict(
                                    local_name, namespace, value,
                                )
                            }
                            ProcessContents::Lax => {
                                self.validate_wildcard_attribute_lax(
                                    local_name, namespace, value,
                                )
                            }
                        };
                        self.post_process_attribute(local_name, namespace, value, &result);
                        return result;
                    }
                    // wildcard present but namespace not allowed — fall through to error
                }

                // Not found and no matching wildcard
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

        // 3. Identity constraint processing (field values + scope exit + keyref cross-ref)
        self.process_constraints_end_element(&ev_state.text_content, ev_state.typed_value.as_ref());

        // 3b. Pop scope table and propagate key/unique tables upward to parent
        if let Some(Some(scope_map)) = self.ic_scope_tables.pop() {
            if let Some(parent_slot) = self.ic_scope_tables.last_mut() {
                let parent_map = parent_slot.get_or_insert_with(HashMap::new);
                for (ic_key, key_table) in scope_map {
                    parent_map
                        .entry(ic_key)
                        .and_modify(|existing| {
                            existing.sequences.extend(key_table.sequences.clone())
                        })
                        .or_insert(key_table);
                }
            }
        }

        // 4. ID/IDREF collection from element text content
        if let Some(ref tv) = ev_state.typed_value {
            self.collect_id_idref(tv, &ev_state.text_content);
        }

        // 5. Update element path
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
    /// Checks that the validation stack is empty and performs IDREF validation.
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

        // IDREF validation (cvc-id.1): check all pending IDREFs resolve
        for (idref_value, location, element_path) in &self.pending_idrefs {
            if !self.id_values.contains(idref_value) {
                self.sink.on_error(errors::error_with_path(
                    "cvc-id.1",
                    format!(
                        "IDREF '{}' does not match any ID in the document",
                        idref_value
                    ),
                    location.clone(),
                    element_path,
                ));
            }
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
            ContentValidatorState::Nfa { nfa, active_states, .. } => {
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
                    if state.can_accept(model, i) {
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
            #[cfg(feature = "xsd11")]
            ContentValidatorState::AllGroupExtension {
                model, state, extension_nfa, phase,
            } => {
                use super::content::AllGroupExtPhase;

                let mut result = Vec::new();
                match phase {
                    AllGroupExtPhase::AllGroup => {
                        // Include acceptable all-group particles
                        for (i, particle) in model.particles.iter().enumerate() {
                            if state.can_accept(model, i) {
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
                        // If all-group is satisfied, also include extension NFA elements
                        if state.is_satisfied(model) {
                            let initial = crate::compiler::epsilon_closure(
                                extension_nfa,
                                std::iter::once(extension_nfa.start_state),
                            );
                            for state_id in initial {
                                if let Some(nfa_state) = extension_nfa.get_state(state_id) {
                                    if let Some(crate::compiler::NfaTerm::Element {
                                        ref name,
                                        ref namespace,
                                        ref element_key,
                                        ..
                                    }) = nfa_state.term
                                    {
                                        result.push(ExpectedElement {
                                            local_name: *name,
                                            namespace: *namespace,
                                            element_key: *element_key,
                                        });
                                    }
                                }
                            }
                        }
                    }
                    AllGroupExtPhase::Nfa(active_states) => {
                        let closure = crate::compiler::epsilon_closure(
                            extension_nfa,
                            active_states.iter().copied(),
                        );
                        for state_id in closure {
                            if let Some(nfa_state) = extension_nfa.get_state(state_id) {
                                if let Some(crate::compiler::NfaTerm::Element {
                                    ref name,
                                    ref namespace,
                                    ref element_key,
                                    ..
                                }) = nfa_state.term
                                {
                                    result.push(ExpectedElement {
                                        local_name: *name,
                                        namespace: *namespace,
                                        element_key: *element_key,
                                    });
                                }
                            }
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
            let resolved = ct_data.resolved_attributes.get(i);
            let (attr_name, attr_ns) = self.resolve_attr_use_name_ns(attr_use, resolved);
            let attr_key = resolved.and_then(|r| r.resolved_ref);

            result.push(ExpectedAttribute {
                local_name: attr_name,
                namespace: attr_ns,
                attribute_key: attr_key,
                required: use_kind == AttributeUseKind::Required,
            });
        }

        // Include attributes from attribute groups
        for ga in self.collect_group_attributes(ct_data) {
            if ga.use_kind == AttributeUseKind::Prohibited {
                continue;
            }
            result.push(ExpectedAttribute {
                local_name: ga.name,
                namespace: ga.namespace,
                attribute_key: ga.attr_key,
                required: ga.use_kind == AttributeUseKind::Required,
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

            let resolved = ct_data.resolved_attributes.get(i);
            let (attr_name, attr_ns) = self.resolve_attr_use_name_ns(attr_use, resolved);
            let attr_key = resolved.and_then(|r| r.resolved_ref);

            // Skip if already provided
            if ev_state.seen_attributes.contains(&(attr_ns, attr_name)) {
                continue;
            }

            // Check for default value — first on the use, then on the global decl
            let default = attr_use.attribute.default_value.as_deref().or_else(|| {
                attr_key
                    .and_then(|k| self.schema_set.arenas.attributes.get(k))
                    .and_then(|d| d.default_value.as_deref())
            });
            if let Some(value) = default {
                if let Some(attr_key) = attr_key {
                    result.push(DefaultAttribute {
                        local_name: attr_name,
                        namespace: attr_ns,
                        attribute_key: attr_key,
                        value: value.to_string(),
                    });
                }
            }
        }

        // Include defaults from attribute groups
        for ga in self.collect_group_attributes(ct_data) {
            if ga.use_kind == AttributeUseKind::Prohibited {
                continue;
            }
            if ev_state.seen_attributes.contains(&(ga.namespace, ga.name)) {
                continue;
            }
            if let Some(value) = ga.default_value {
                if let Some(attr_key) = ga.attr_key {
                    result.push(DefaultAttribute {
                        local_name: ga.name,
                        namespace: ga.namespace,
                        attribute_key: attr_key,
                        value,
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
        self.ic_scope_tables.push(None);

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

    // -----------------------------------------------------------------------
    // Identity constraint helpers
    // -----------------------------------------------------------------------

    /// Resolve a keyref's `refer` target to a concrete `IdentityConstraintKey`.
    ///
    /// Scans the identity constraint arena for a Key or Unique constraint
    /// whose name and target namespace match the given values.
    fn resolve_refer_key(
        &self,
        refer_local_name: NameId,
        refer_ns: Option<NameId>,
    ) -> Option<IdentityConstraintKey> {
        for (key, ic_data) in &self.schema_set.arenas.identity_constraints {
            if ic_data.kind == IdentityKind::Keyref {
                continue;
            }
            if ic_data.name != refer_local_name {
                continue;
            }
            let ic_target_ns = ic_data
                .source
                .as_ref()
                .and_then(|s| self.schema_set.documents.get(s.doc_id as usize))
                .and_then(|d| d.target_namespace);
            if ic_target_ns == refer_ns {
                return Some(key);
            }
        }
        None
    }

    /// Lazily compile an identity constraint and cache it.
    /// Returns `true` if compilation succeeded (constraint is usable).
    fn ensure_compiled(&mut self, ic_key: IdentityConstraintKey) -> bool {
        if let Some(cached) = self.compiled_constraints.get(&ic_key) {
            return cached.is_some();
        }
        let ic_data = &self.schema_set.arenas.identity_constraints[ic_key];
        let doc = ic_data
            .source
            .as_ref()
            .and_then(|s| self.schema_set.documents.get(s.doc_id as usize));
        let schema_xpath_default_ns = doc.and_then(|d| d.xpath_default_namespace);
        let target_namespace = doc.and_then(|d| d.target_namespace);
        let ic_name = ic_data.name;
        match CompiledIdentityConstraint::compile(
            ic_data,
            ic_key,
            &self.schema_set.name_table,
            schema_xpath_default_ns,
            target_namespace,
            self.schema_set.xsd_version,
        ) {
            Ok(mut compiled) => {
                // Resolve refer_key for keyref constraints
                if compiled.kind == IdentityKind::Keyref {
                    if let Some(refer) = &compiled.refer {
                        let refer_ns = refer.namespace.or(compiled.target_namespace);
                        compiled.refer_key =
                            self.resolve_refer_key(refer.local_name, refer_ns);
                        if compiled.refer_key.is_none() {
                            let name = self.schema_set.name_table.resolve(ic_name);
                            let refer_display = match refer_ns {
                                Some(ns) => format!(
                                    "{{{}}}{}",
                                    self.schema_set.name_table.resolve(ns),
                                    self.schema_set.name_table.resolve(refer.local_name)
                                ),
                                None => self
                                    .schema_set
                                    .name_table
                                    .resolve(refer.local_name)
                                    .to_string(),
                            };
                            self.sink.on_warning(ValidationWarning {
                                code: "cvc-identity-constraint",
                                message: format!(
                                    "Keyref '{}': could not resolve refer target '{}'",
                                    name, refer_display
                                ),
                                location: None,
                            });
                        }
                    }
                }
                self.compiled_constraints.insert(ic_key, Some(compiled));
                true
            }
            Err(e) => {
                let name = self.schema_set.name_table.resolve(ic_name);
                self.sink.on_warning(ValidationWarning {
                    code: "cvc-identity-constraint",
                    message: format!(
                        "Identity constraint '{}': XPath compilation failed: {}",
                        name, e
                    ),
                    location: None,
                });
                self.compiled_constraints.insert(ic_key, None);
                false
            }
        }
    }

    /// Advance existing constraints for a start element, then activate new
    /// constraints from the element declaration (if any).
    fn advance_constraints_start_element(
        &mut self,
        local_name: NameId,
        namespace: Option<NameId>,
        element_key: Option<ElementKey>,
    ) {
        let ns = namespace.unwrap_or(NameId(0));

        // 1. Advance existing active constraints
        for cs in &mut self.active_constraints {
            cs.start_element(local_name, ns);
        }

        // 2. Activate new constraints from element declaration
        if let Some(ek) = element_key {
            let ic_keys: Vec<IdentityConstraintKey> =
                self.schema_set.arenas.elements[ek].identity_constraints.clone();
            for ic_key in ic_keys {
                if self.ensure_compiled(ic_key) {
                    let compiled = self.compiled_constraints[&ic_key].as_ref().unwrap();
                    let mut cs = ConstraintStruct::new(compiled);
                    cs.activate();
                    self.active_constraints.push(cs);
                }
            }
        }
    }

    /// Process identity constraints at element end: advance fields/selectors,
    /// deactivate finished constraints, and perform scope-local keyref
    /// cross-reference.
    fn process_constraints_end_element(
        &mut self,
        text_content: &str,
        typed_value: Option<&XmlValue>,
    ) {
        let name_table = &self.schema_set.name_table;
        let element_path = self.element_path.clone();
        let location = self.current_location.clone();

        // 1. Advance all constraints (field value collection + key sequence finalization)
        let mut ic_errors = Vec::new();
        for cs in &mut self.active_constraints {
            let errs = cs.end_element_with_text(
                text_content,
                typed_value,
                name_table,
                &element_path,
                location.clone(),
            );
            ic_errors.extend(errs);
        }
        for err in ic_errors {
            self.sink.on_error(err);
        }

        // 2. Collect deactivated constraints (constraints whose scope element just closed)
        let mut deactivated: Vec<ConstraintStruct> = Vec::new();
        let mut i = 0;
        while i < self.active_constraints.len() {
            if !self.active_constraints[i].is_active() {
                deactivated.push(self.active_constraints.swap_remove(i));
            } else {
                i += 1;
            }
        }

        // 3. Scope-local keyref cross-reference using ic_scope_tables
        if !deactivated.is_empty() {
            let mut scope_keyrefs: Vec<(KeyTable, Option<IdentityConstraintKey>)> = Vec::new();

            for cs in deactivated {
                if cs.key_table.kind == IdentityKind::Keyref {
                    // Extract the resolved refer_key from the compiled constraint
                    let refer_key = self
                        .compiled_constraints
                        .get(&cs.ic_key)
                        .and_then(|opt| opt.as_ref())
                        .and_then(|compiled| compiled.refer_key);
                    scope_keyrefs.push((cs.key_table, refer_key));
                } else {
                    // Insert key/unique table into current scope
                    let scope_slot = self.ic_scope_tables.last_mut();
                    if let Some(slot) = scope_slot {
                        let scope_map = slot.get_or_insert_with(HashMap::new);
                        let ic_key = cs.key_table.ic_key;
                        scope_map
                            .entry(ic_key)
                            .and_modify(|existing| {
                                existing
                                    .sequences
                                    .extend(cs.key_table.sequences.clone())
                            })
                            .or_insert(cs.key_table);
                    }
                }
            }

            // Cross-reference each keyref against key/unique tables in current scope.
            // The scope map already contains child-propagated tables.
            let name_table = &self.schema_set.name_table;
            for (keyref_table, refer_key) in &scope_keyrefs {
                let target = refer_key.and_then(|rk| {
                    self.ic_scope_tables
                        .last()
                        .and_then(|slot| slot.as_ref())
                        .and_then(|map| map.get(&rk))
                });
                match target {
                    Some(target_table) => {
                        let errs = keyref_table.check_keyref_against(target_table, name_table);
                        for err in errs {
                            self.sink.on_error(err);
                        }
                    }
                    None => {
                        let keyref_name = name_table.resolve(keyref_table.constraint_name);
                        let refer_display = self
                            .compiled_constraints
                            .get(&keyref_table.ic_key)
                            .and_then(|opt| opt.as_ref())
                            .and_then(|compiled| compiled.refer.as_ref().map(|refer| {
                                let refer_ns = refer.namespace.or(compiled.target_namespace);
                                match refer_ns {
                                    Some(ns) => format!(
                                        "{{{}}}{}",
                                        name_table.resolve(ns),
                                        name_table.resolve(refer.local_name)
                                    ),
                                    None => name_table.resolve(refer.local_name).to_string(),
                                }
                            }))
                            .unwrap_or_else(|| "<unknown>".to_string());
                        self.sink.on_error(errors::error(
                            "cvc-identity-constraint.4.3",
                            format!(
                                "Keyref '{}' references unknown constraint '{}'",
                                keyref_name, refer_display
                            ),
                            location.clone(),
                        ));
                    }
                }
            }
        }
    }

    /// Detect ID/IDREF types and collect values for finalization.
    fn collect_id_idref(&mut self, typed_value: &XmlValue, value_str: &str) {
        match typed_value.type_code {
            XmlTypeCode::Id => {
                if !self.id_values.insert(value_str.to_string()) {
                    self.report_error(
                        "cvc-id.2",
                        format!("Duplicate ID value '{}'", value_str),
                    );
                }
            }
            XmlTypeCode::IdRef => {
                self.pending_idrefs.push((
                    value_str.to_string(),
                    self.current_location.clone(),
                    self.element_path.clone(),
                ));
            }
            XmlTypeCode::IdRefs => {
                // Prefer extracting from validated list items
                if let crate::types::value::XmlValueKind::List { items, .. } = &typed_value.value {
                    for item in items {
                        self.pending_idrefs.push((
                            item.to_string(),
                            self.current_location.clone(),
                            self.element_path.clone(),
                        ));
                    }
                } else {
                    // Fallback: split lexical text
                    for token in value_str.split_whitespace() {
                        self.pending_idrefs.push((
                            token.to_string(),
                            self.current_location.clone(),
                            self.element_path.clone(),
                        ));
                    }
                }
            }
            _ => {}
        }
    }

    /// Post-process a validated attribute for identity constraint field matching
    /// and ID/IDREF collection.
    fn post_process_attribute(
        &mut self,
        local_name: NameId,
        namespace: Option<NameId>,
        value: &str,
        result: &SchemaInfo,
    ) {
        let ns = namespace.unwrap_or(NameId(0));

        // Identity constraint: check field attribute matches
        for cs in &mut self.active_constraints {
            let matches = cs.matching_fields(local_name, ns);
            for field_idx in matches {
                cs.set_field_value(field_idx, value.to_string(), result.typed_value.clone());
            }
        }

        // ID/IDREF collection
        if let Some(ref tv) = result.typed_value {
            self.collect_id_idref(tv, value);
        }
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
        // Parse and validate the QName using shared parsing logic
        let qname = match parse_qname_with_snapshot(
            xsi_type_str,
            ns_context,
            &self.schema_set.name_table,
            true,
        ) {
            Ok(qn) => qn,
            Err(e) => {
                let msg = match e {
                    QNameError::UndefinedPrefix(p) => {
                        format!("Undeclared prefix '{}' in xsi:type value '{}'", p, xsi_type_str)
                    }
                    _ => format!("Invalid xsi:type value '{}': {}", xsi_type_str, e),
                };
                self.report_error("cvc-elt.4.1", msg);
                return None;
            }
        };

        // Look up the type
        let resolved = self
            .schema_set
            .lookup_type(qname.namespace_uri, qname.local_name)
            .or_else(|| {
                self.schema_set
                    .get_built_in_type_by_qname(qname.namespace_uri, qname.local_name)
            });

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
                    format!(
                        "Type '{}' specified in xsi:type is not declared",
                        xsi_type_str
                    ),
                );
                None
            }
        }
    }

    /// Validate an attribute matched by a wildcard with processContents="strict".
    ///
    /// A global attribute declaration must exist; its value is validated against
    /// the declared type.
    fn validate_wildcard_attribute_strict(
        &mut self,
        local_name: NameId,
        namespace: Option<NameId>,
        value: &str,
    ) -> SchemaInfo {
        match self.schema_set.lookup_attribute(namespace, local_name) {
            Some(attr_key) => {
                let attr_data = self.schema_set.arenas.attributes.get(attr_key);
                let attr_type = attr_data.and_then(|d| d.resolved_type);
                let fixed = attr_data.and_then(|d| d.fixed_value.clone());

                if let Some(fixed_val) = fixed {
                    if value != fixed_val {
                        let attr_name = self.schema_set.name_table.resolve(local_name);
                        self.report_error(
                            "cvc-attribute.4",
                            format!(
                                "Attribute '{}' has fixed value '{}' but got '{}'",
                                attr_name, fixed_val, value
                            ),
                        );
                        if let Some(s) = self.validation_stack.last_mut() {
                            s.validity = SchemaValidity::Invalid;
                        }
                    }
                }

                let mut member_type = None;
                let mut typed_value = None;
                let mut attr_validity = SchemaValidity::Valid;
                if let Some(type_key) = attr_type {
                    match super::simple::validate_simple_type(
                        value,
                        type_key,
                        self.schema_set,
                    ) {
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

                SchemaInfo {
                    element_decl: None,
                    attribute_decl: Some(attr_key),
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
                let attr_name = self.schema_set.name_table.resolve(local_name);
                self.report_error(
                    "cvc-assess-attr.1.2",
                    format!(
                        "No global attribute declaration for '{}' (wildcard processContents=\"strict\")",
                        attr_name
                    ),
                );
                if let Some(s) = self.validation_stack.last_mut() {
                    s.validity = SchemaValidity::Invalid;
                }
                SchemaInfo::invalid()
            }
        }
    }

    /// Validate an attribute matched by a wildcard with processContents="lax".
    ///
    /// If a global attribute declaration exists, validate; otherwise skip.
    fn validate_wildcard_attribute_lax(
        &mut self,
        local_name: NameId,
        namespace: Option<NameId>,
        value: &str,
    ) -> SchemaInfo {
        match self.schema_set.lookup_attribute(namespace, local_name) {
            Some(attr_key) => {
                // Found a global declaration — validate like strict
                let attr_data = self.schema_set.arenas.attributes.get(attr_key);
                let attr_type = attr_data.and_then(|d| d.resolved_type);
                let fixed = attr_data.and_then(|d| d.fixed_value.clone());

                if let Some(fixed_val) = fixed {
                    if value != fixed_val {
                        let attr_name = self.schema_set.name_table.resolve(local_name);
                        self.report_error(
                            "cvc-attribute.4",
                            format!(
                                "Attribute '{}' has fixed value '{}' but got '{}'",
                                attr_name, fixed_val, value
                            ),
                        );
                        if let Some(s) = self.validation_stack.last_mut() {
                            s.validity = SchemaValidity::Invalid;
                        }
                    }
                }

                let mut member_type = None;
                let mut typed_value = None;
                let mut attr_validity = SchemaValidity::Valid;
                if let Some(type_key) = attr_type {
                    match super::simple::validate_simple_type(
                        value,
                        type_key,
                        self.schema_set,
                    ) {
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

                SchemaInfo {
                    element_decl: None,
                    attribute_decl: Some(attr_key),
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
                // No global declaration — lax means skip
                SchemaInfo::empty()
            }
        }
    }

    /// Check whether a wildcard allows a given namespace.
    fn wildcard_allows_namespace(
        &self,
        wildcard: &WildcardResult,
        namespace: Option<NameId>,
        target_namespace: Option<NameId>,
    ) -> bool {
        match &wildcard.namespace {
            WildcardNamespace::Any => true,
            WildcardNamespace::Other => namespace != target_namespace,
            WildcardNamespace::TargetNamespace => namespace == target_namespace,
            WildcardNamespace::Local => namespace.is_none(),
            WildcardNamespace::List(ns_list) => ns_list.contains(&namespace),
        }
    }

    /// Collect all attribute uses from resolved attribute groups (recursively).
    fn collect_group_attributes(
        &self,
        ct_data: &ComplexTypeDefData,
    ) -> Vec<GroupAttribute> {
        let mut result = Vec::new();
        let mut visited = HashSet::new();
        for &group_key in &ct_data.resolved_attribute_groups {
            self.collect_group_attributes_recursive(group_key, &mut result, &mut visited);
        }
        result
    }

    fn collect_group_attributes_recursive(
        &self,
        group_key: AttributeGroupKey,
        result: &mut Vec<GroupAttribute>,
        visited: &mut HashSet<AttributeGroupKey>,
    ) {
        if !visited.insert(group_key) {
            return; // prevent infinite recursion on circular refs
        }
        let group_data = match self.schema_set.arenas.get_attribute_group(group_key) {
            Some(g) => g,
            None => return,
        };
        for (i, attr_use) in group_data.attributes.iter().enumerate() {
            let resolved = group_data.resolved_attributes.get(i);
            let attr_key = resolved.and_then(|r| r.resolved_ref);
            let attr_type = resolved.and_then(|r| r.resolved_type);
            let (name, namespace) = self.resolve_attr_use_name_ns(attr_use, resolved);
            let fixed_value = attr_use.attribute.fixed_value.clone().or_else(|| {
                attr_key
                    .and_then(|k| self.schema_set.arenas.attributes.get(k))
                    .and_then(|d| d.fixed_value.clone())
            });
            let default_value = attr_use.attribute.default_value.clone().or_else(|| {
                attr_key
                    .and_then(|k| self.schema_set.arenas.attributes.get(k))
                    .and_then(|d| d.default_value.clone())
            });
            result.push(GroupAttribute {
                name,
                namespace,
                use_kind: attr_use.use_kind,
                type_key: attr_type,
                attr_key,
                fixed_value,
                default_value,
            });
        }
        for &nested_key in &group_data.resolved_attribute_groups {
            self.collect_group_attributes_recursive(nested_key, result, visited);
        }
    }

    /// Resolve the effective name and namespace for an attribute use.
    ///
    /// For inline attributes, returns the name directly from the use.
    /// For `<xs:attribute ref="..."/>`, resolves through the global declaration.
    fn resolve_attr_use_name_ns(
        &self,
        attr_use: &AttributeUseResult,
        resolved: Option<&ResolvedAttributeUse>,
    ) -> (NameId, Option<NameId>) {
        if let Some(name) = attr_use.attribute.name {
            return (name, attr_use.attribute.target_namespace);
        }
        if let Some(attr_key) = resolved.and_then(|r| r.resolved_ref) {
            if let Some(decl) = self.schema_set.arenas.attributes.get(attr_key) {
                if let Some(name) = decl.name {
                    return (name, decl.target_namespace);
                }
            }
        }
        (well_known::EMPTY, None)
    }

    /// Find the effective attribute wildcard for a complex type.
    ///
    /// Checks the type's own `attribute_wildcard` first, then walks
    /// referenced attribute groups recursively.
    fn find_effective_wildcard(
        &self,
        ct_data: &ComplexTypeDefData,
    ) -> Option<WildcardResult> {
        if ct_data.attribute_wildcard.is_some() {
            return ct_data.attribute_wildcard.clone();
        }
        let mut visited = HashSet::new();
        self.find_group_wildcard_recursive(&ct_data.resolved_attribute_groups, &mut visited)
    }

    fn find_group_wildcard_recursive(
        &self,
        group_keys: &[AttributeGroupKey],
        visited: &mut HashSet<AttributeGroupKey>,
    ) -> Option<WildcardResult> {
        for &gk in group_keys {
            if !visited.insert(gk) {
                continue;
            }
            if let Some(group_data) = self.schema_set.arenas.get_attribute_group(gk) {
                if let Some(ref wc) = group_data.attribute_wildcard {
                    return Some(wc.clone());
                }
                let result = self.find_group_wildcard_recursive(
                    &group_data.resolved_attribute_groups,
                    visited,
                );
                if result.is_some() {
                    return result;
                }
            }
        }
        None
    }

    /// Find an attribute declaration in a complex type's attribute list
    fn find_attribute_in_type(
        &self,
        ct_data: &ComplexTypeDefData,
        local_name: NameId,
        namespace: Option<NameId>,
    ) -> AttributeLookup {
        for (i, attr_use) in ct_data.attributes.iter().enumerate() {
            let resolved = ct_data.resolved_attributes.get(i);
            let (attr_name, attr_ns) = self.resolve_attr_use_name_ns(attr_use, resolved);

            if attr_name == local_name && attr_ns == namespace {
                if attr_use.use_kind == AttributeUseKind::Prohibited {
                    return AttributeLookup::Prohibited;
                }

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

                return AttributeLookup::Found(attr_key, attr_type, fixed);
            }
        }

        // Search attribute groups
        for ga in self.collect_group_attributes(ct_data) {
            if ga.name == local_name && ga.namespace == namespace {
                if ga.use_kind == AttributeUseKind::Prohibited {
                    return AttributeLookup::Prohibited;
                }
                return AttributeLookup::Found(ga.attr_key, ga.type_key, ga.fixed_value);
            }
        }

        AttributeLookup::NotFound
    }

    /// Check that all required attributes are present
    fn check_required_attributes(
        &mut self,
        ct_data: &ComplexTypeDefData,
        seen: &HashSet<(Option<NameId>, NameId)>,
    ) -> bool {
        let mut has_missing = false;
        for (i, attr_use) in ct_data.attributes.iter().enumerate() {
            if attr_use.use_kind != AttributeUseKind::Required {
                continue;
            }

            let resolved = ct_data.resolved_attributes.get(i);
            let (attr_name, attr_ns) = self.resolve_attr_use_name_ns(attr_use, resolved);

            if !seen.contains(&(attr_ns, attr_name)) {
                let name_str = self.schema_set.name_table.resolve(attr_name);
                self.report_error(
                    "cvc-complex-type.4",
                    format!("Required attribute '{}' is missing", name_str),
                );
                has_missing = true;
            }
        }

        // Check required attributes from attribute groups
        for ga in self.collect_group_attributes(ct_data) {
            if ga.use_kind != AttributeUseKind::Required {
                continue;
            }
            if !seen.contains(&(ga.namespace, ga.name)) {
                let name_str = self.schema_set.name_table.resolve(ga.name);
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
    fn test_local_element_with_inline_type() {
        // Local element with inline <xs:simpleType> — verify that the inline
        // type is resolved and facets are enforced.
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

        // Verify schema internals: inline type is assembled and propagated
        let root_name = schema_set.name_table.get("root")
            .expect("name 'root' not interned");
        let root_key = schema_set.lookup_element(None, root_name)
            .expect("root element not found");
        let root_type = schema_set.arenas.elements[root_key].resolved_type
            .expect("root element has no resolved_type");
        let ct_key = match root_type {
            crate::ids::TypeKey::Complex(k) => k,
            _ => panic!("root type is not complex"),
        };
        let ct = &schema_set.arenas.complex_types[ct_key];
        assert!(
            !ct.resolved_content_particle_types.is_empty(),
            "resolved_content_particle_types is empty"
        );
        assert!(
            ct.resolved_content_particle_types[0].is_some(),
            "resolved_content_particle_types[0] is None"
        );

        // Valid value (within maxLength=10)
        {
            let sink = TestSink::new();
            let mut v = SchemaValidator::new(&schema_set, sink, ValidationFlags::default());
            let ns = empty_ns_context();

            v.validate_element("root", "", None, None, &ns);
            v.validate_end_of_attributes();

            let info = v.validate_element("code", "", None, None, &ns);
            assert_eq!(info.validity, SchemaValidity::Valid);
            assert!(info.schema_type.is_some(), "inline type not resolved");
            assert_eq!(info.content_type, Some(ContentType::TextOnly));

            v.validate_end_of_attributes();
            v.validate_text("ABC");
            v.validate_end_element();

            v.validate_end_element();
            assert!(v.end_validation().is_ok());
            assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
        }

        // Invalid value (exceeds maxLength=10) — facet must be enforced
        {
            let sink = TestSink::new();
            let mut v = SchemaValidator::new(&schema_set, sink, ValidationFlags::default());
            let ns = empty_ns_context();

            v.validate_element("root", "", None, None, &ns);
            v.validate_end_of_attributes();

            v.validate_element("code", "", None, None, &ns);
            v.validate_end_of_attributes();
            v.validate_text("this text exceeds maxLength of 10");
            v.validate_end_element();

            v.validate_end_element();
            v.end_validation().ok();

            assert!(
                !v.sink.errors.is_empty(),
                "expected facet error for text exceeding maxLength=10"
            );
        }
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

    // -----------------------------------------------------------------------
    // Attribute group tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_attribute_group_basic() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:attributeGroup name="myAttrs">
                    <xs:attribute name="color" type="xs:string"/>
                    <xs:attribute name="size" type="xs:integer"/>
                </xs:attributeGroup>
                <xs:element name="root">
                    <xs:complexType>
                        <xs:attributeGroup ref="myAttrs"/>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"#,
        );

        let sink = TestSink::new();
        let mut v = SchemaValidator::new(&schema_set, sink, ValidationFlags::default());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);
        let info = v.validate_attribute("color", "", "red");
        assert_eq!(info.validity, SchemaValidity::Valid);

        let info = v.validate_attribute("size", "", "42");
        assert_eq!(info.validity, SchemaValidity::Valid);

        v.validate_end_of_attributes();
        v.validate_end_element();
        assert!(v.end_validation().is_ok());
        assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
    }

    #[test]
    fn test_attribute_group_nested() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:attributeGroup name="inner">
                    <xs:attribute name="depth" type="xs:integer"/>
                </xs:attributeGroup>
                <xs:attributeGroup name="outer">
                    <xs:attribute name="width" type="xs:string"/>
                    <xs:attributeGroup ref="inner"/>
                </xs:attributeGroup>
                <xs:element name="root">
                    <xs:complexType>
                        <xs:attributeGroup ref="outer"/>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"#,
        );

        let sink = TestSink::new();
        let mut v = SchemaValidator::new(&schema_set, sink, ValidationFlags::default());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);
        let info = v.validate_attribute("width", "", "100px");
        assert_eq!(info.validity, SchemaValidity::Valid);

        // "depth" comes from the nested inner group
        let info = v.validate_attribute("depth", "", "5");
        assert_eq!(info.validity, SchemaValidity::Valid);

        v.validate_end_of_attributes();
        v.validate_end_element();
        assert!(v.end_validation().is_ok());
        assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
    }

    #[test]
    fn test_attribute_group_required_missing() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:attributeGroup name="myAttrs">
                    <xs:attribute name="id" type="xs:string" use="required"/>
                </xs:attributeGroup>
                <xs:element name="root">
                    <xs:complexType>
                        <xs:attributeGroup ref="myAttrs"/>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"#,
        );

        let sink = TestSink::new();
        let mut v = SchemaValidator::new(&schema_set, sink, ValidationFlags::default());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);
        // Do NOT supply the required "id" attribute
        v.validate_end_of_attributes();
        v.validate_end_element();
        assert!(v.end_validation().is_ok());

        assert!(
            v.sink.errors.iter().any(|e| e.constraint == "cvc-complex-type.4"),
            "expected cvc-complex-type.4 for missing required attribute from group, errors: {:?}",
            v.sink.errors
        );
    }

    // -----------------------------------------------------------------------
    // Wildcard tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_wildcard_namespace_other_rejects_same_ns() {
        // anyAttribute namespace="##other" should reject attributes in the same
        // (target) namespace.
        let schema_set = load_schema(
            r###"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
                        targetNamespace="http://example.com/ns"
                        xmlns:tns="http://example.com/ns">
                <xs:element name="root">
                    <xs:complexType>
                        <xs:anyAttribute namespace="##other" processContents="skip"/>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"###,
        );

        let sink = TestSink::new();
        let mut v = SchemaValidator::new(&schema_set, sink, ValidationFlags::default());
        let tns_id = schema_set.name_table.add("http://example.com/ns");
        let tns_prefix = schema_set.name_table.add("tns");
        let ns = NamespaceContextSnapshot {
            default_ns: Some(tns_id),
            bindings: vec![(tns_prefix, tns_id)],
        };

        v.validate_element("root", "http://example.com/ns", None, None, &ns);

        // Attribute in a *different* namespace should be accepted (skip → NotKnown)
        let info = v.validate_attribute("foreign", "http://other.com/ns", "val");
        assert_ne!(info.validity, SchemaValidity::Invalid);

        // Attribute in the *same* (target) namespace should be rejected
        let info = v.validate_attribute("local", "http://example.com/ns", "val");
        assert_eq!(info.validity, SchemaValidity::Invalid);
        assert!(
            v.sink.errors.iter().any(|e| e.constraint == "cvc-complex-type.3.2.2"),
            "expected cvc-complex-type.3.2.2, errors: {:?}",
            v.sink.errors
        );

        v.validate_end_of_attributes();
        v.validate_end_element();
        assert!(v.end_validation().is_ok());
    }

    #[test]
    fn test_wildcard_process_contents_strict() {
        // processContents="strict" with a global attribute declaration
        let schema_set = load_schema(
            r###"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:attribute name="globalAttr" type="xs:integer"/>
                <xs:element name="root">
                    <xs:complexType>
                        <xs:anyAttribute namespace="##any" processContents="strict"/>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"###,
        );

        let sink = TestSink::new();
        let mut v = SchemaValidator::new(&schema_set, sink, ValidationFlags::default());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);

        // Valid global attribute with correct value
        let info = v.validate_attribute("globalAttr", "", "42");
        assert_eq!(info.validity, SchemaValidity::Valid);
        assert!(info.attribute_decl.is_some());

        v.validate_end_of_attributes();
        v.validate_end_element();
        assert!(v.end_validation().is_ok());
        assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
    }

    #[test]
    fn test_wildcard_process_contents_strict_unknown() {
        // processContents="strict" with an unknown attribute -> error
        let schema_set = load_schema(
            r###"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root">
                    <xs:complexType>
                        <xs:anyAttribute namespace="##any" processContents="strict"/>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"###,
        );

        let sink = TestSink::new();
        let mut v = SchemaValidator::new(&schema_set, sink, ValidationFlags::default());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);

        let info = v.validate_attribute("unknownAttr", "", "anything");
        assert_eq!(info.validity, SchemaValidity::Invalid);
        assert!(
            v.sink.errors.iter().any(|e| e.constraint == "cvc-assess-attr.1.2"),
            "expected cvc-assess-attr.1.2 for strict wildcard with unknown attr, errors: {:?}",
            v.sink.errors
        );

        v.validate_end_of_attributes();
        v.validate_end_element();
        assert!(v.end_validation().is_ok());
    }

    #[test]
    fn test_wildcard_process_contents_lax() {
        // processContents="lax" with an unknown attribute -> no error
        let schema_set = load_schema(
            r###"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root">
                    <xs:complexType>
                        <xs:anyAttribute namespace="##any" processContents="lax"/>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"###,
        );

        let sink = TestSink::new();
        let mut v = SchemaValidator::new(&schema_set, sink, ValidationFlags::default());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);

        // Unknown attr with lax → accepted (NotKnown, no error)
        let info = v.validate_attribute("whatever", "", "anything");
        assert_ne!(info.validity, SchemaValidity::Invalid);

        v.validate_end_of_attributes();
        v.validate_end_element();
        assert!(v.end_validation().is_ok());
        assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
    }

    #[test]
    fn test_wildcard_process_contents_skip() {
        // processContents="skip" should accept anything without validation
        let schema_set = load_schema(
            r###"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:attribute name="globalAttr" type="xs:integer"/>
                <xs:element name="root">
                    <xs:complexType>
                        <xs:anyAttribute namespace="##any" processContents="skip"/>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"###,
        );

        let sink = TestSink::new();
        let mut v = SchemaValidator::new(&schema_set, sink, ValidationFlags::default());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);

        // Even an invalid value for a known global attr should pass with skip (NotKnown)
        let info = v.validate_attribute("globalAttr", "", "not_an_integer");
        assert_ne!(info.validity, SchemaValidity::Invalid);

        // Unknown attributes also accepted (NotKnown)
        let info = v.validate_attribute("madeUp", "", "anything");
        assert_ne!(info.validity, SchemaValidity::Invalid);

        v.validate_end_of_attributes();
        v.validate_end_element();
        assert!(v.end_validation().is_ok());
        assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
    }

    // -----------------------------------------------------------------------
    // Issue fix tests: attribute ref, prohibited, group wildcard, defaults
    // -----------------------------------------------------------------------

    #[test]
    fn test_attribute_ref_basic() {
        // Issue 1: <xs:attribute ref="globalAttr"/> should match and validate
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:attribute name="globalAttr" type="xs:integer"/>
                <xs:element name="root">
                    <xs:complexType>
                        <xs:simpleContent>
                            <xs:extension base="xs:string">
                                <xs:attribute ref="globalAttr"/>
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
        let info = v.validate_attribute("globalAttr", "", "42");
        assert_eq!(
            info.validity,
            SchemaValidity::Valid,
            "attribute ref should match by resolved name; errors: {:?}",
            v.sink.errors
        );
        assert!(info.attribute_decl.is_some(), "should resolve attribute decl key");

        v.validate_end_of_attributes();
        v.validate_text("hello");
        v.validate_end_element();
        assert!(v.end_validation().is_ok());
        assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
    }

    #[test]
    fn test_attribute_ref_required_missing() {
        // Issue 1: required attribute ref should be checked properly
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:attribute name="reqAttr" type="xs:string"/>
                <xs:element name="root">
                    <xs:complexType>
                        <xs:simpleContent>
                            <xs:extension base="xs:string">
                                <xs:attribute ref="reqAttr" use="required"/>
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
        // Don't provide the required attribute
        v.validate_end_of_attributes();
        v.validate_text("hello");
        v.validate_end_element();
        v.end_validation().ok();

        assert!(
            v.sink.errors.iter().any(|e| e.constraint == "cvc-complex-type.4"),
            "expected cvc-complex-type.4 for missing required ref attribute, errors: {:?}",
            v.sink.errors
        );
    }

    #[test]
    fn test_prohibited_attribute_despite_wildcard() {
        // Issue 2: use="prohibited" should NOT fall through to anyAttribute
        let schema_set = load_schema(
            r###"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:attribute name="blocked" type="xs:string"/>
                <xs:element name="root">
                    <xs:complexType>
                        <xs:simpleContent>
                            <xs:extension base="xs:string">
                                <xs:attribute ref="blocked" use="prohibited"/>
                                <xs:anyAttribute namespace="##any" processContents="skip"/>
                            </xs:extension>
                        </xs:simpleContent>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"###,
        );

        let sink = TestSink::new();
        let mut v = SchemaValidator::new(&schema_set, sink, ValidationFlags::default());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);
        let info = v.validate_attribute("blocked", "", "value");
        assert_eq!(
            info.validity,
            SchemaValidity::Invalid,
            "prohibited attribute must be rejected even when anyAttribute is present"
        );
        assert!(
            v.sink.errors.iter().any(|e| e.constraint == "cvc-complex-type.3.2.2"
                && e.message.contains("prohibited")),
            "expected 'prohibited' error, errors: {:?}",
            v.sink.errors
        );

        v.validate_end_of_attributes();
        v.validate_text("hello");
        v.validate_end_element();
        assert!(v.end_validation().is_ok());
    }

    #[test]
    fn test_group_wildcard_honored() {
        // Issue 3: anyAttribute inside attributeGroup should be honored
        let schema_set = load_schema(
            r###"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:attributeGroup name="flexAttrs">
                    <xs:attribute name="known" type="xs:string"/>
                    <xs:anyAttribute namespace="##any" processContents="skip"/>
                </xs:attributeGroup>
                <xs:element name="root">
                    <xs:complexType>
                        <xs:attributeGroup ref="flexAttrs"/>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"###,
        );

        let sink = TestSink::new();
        let mut v = SchemaValidator::new(&schema_set, sink, ValidationFlags::default());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);

        // Known attribute from the group
        let info = v.validate_attribute("known", "", "hello");
        assert_eq!(info.validity, SchemaValidity::Valid);

        // Unknown attribute should be accepted via the group's anyAttribute
        let info = v.validate_attribute("extra", "", "anything");
        assert_ne!(
            info.validity,
            SchemaValidity::Invalid,
            "group wildcard should accept unknown attributes; errors: {:?}",
            v.sink.errors
        );

        v.validate_end_of_attributes();
        v.validate_end_element();
        assert!(v.end_validation().is_ok());
        assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
    }

    #[test]
    fn test_default_from_global_declaration() {
        // Issue 4: default value from global attribute decl should be exposed
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:attribute name="lang" type="xs:string" default="en"/>
                <xs:element name="root">
                    <xs:complexType>
                        <xs:simpleContent>
                            <xs:extension base="xs:string">
                                <xs:attribute ref="lang"/>
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
        // Do NOT provide the "lang" attribute — it should appear as a default
        v.validate_end_of_attributes();

        let defaults = v.get_default_attributes();
        assert!(
            defaults.iter().any(|d| {
                let name = schema_set.name_table.resolve(d.local_name);
                name == "lang" && d.value == "en"
            }),
            "expected default attribute lang='en', got: {:?}",
            defaults
                .iter()
                .map(|d| (schema_set.name_table.resolve(d.local_name), &d.value))
                .collect::<Vec<_>>()
        );

        v.validate_text("hello");
        v.validate_end_element();
        assert!(v.end_validation().is_ok());
        assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
    }

    #[test]
    fn test_default_from_global_declaration_in_group() {
        // Issue 4: default from global decl via attributeGroup ref
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:attribute name="lang" type="xs:string" default="en"/>
                <xs:attributeGroup name="grp">
                    <xs:attribute ref="lang"/>
                </xs:attributeGroup>
                <xs:element name="root">
                    <xs:complexType>
                        <xs:attributeGroup ref="grp"/>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"#,
        );

        let sink = TestSink::new();
        let mut v = SchemaValidator::new(&schema_set, sink, ValidationFlags::default());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);
        v.validate_end_of_attributes();

        let defaults = v.get_default_attributes();
        assert!(
            defaults.iter().any(|d| {
                let name = schema_set.name_table.resolve(d.local_name);
                name == "lang" && d.value == "en"
            }),
            "expected default attribute lang='en' from group, got: {:?}",
            defaults
                .iter()
                .map(|d| (schema_set.name_table.resolve(d.local_name), &d.value))
                .collect::<Vec<_>>()
        );

        v.validate_end_element();
        assert!(v.end_validation().is_ok());
        assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
    }

    // ── Mixed content tests ─────────────────────────────────────────────

    #[test]
    fn test_mixed_content_text_allowed() {
        // A mixed complex type with a sequence of child elements.
        // Text between child elements should be valid.
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root">
                    <xs:complexType mixed="true">
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

        let info = v.validate_element("root", "", None, None, &ns);
        assert_eq!(info.content_type, Some(ContentType::Mixed));
        v.validate_end_of_attributes();

        // Text before first child
        v.validate_text("hello ");

        // Child <a>
        v.validate_element("a", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_text("val_a");
        v.validate_end_element();

        // Text between children
        v.validate_text(" middle ");

        // Child <b>
        v.validate_element("b", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_text("val_b");
        v.validate_end_element();

        // Text after last child
        v.validate_text(" world");

        v.validate_end_element();
        assert!(v.end_validation().is_ok());
        assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
    }

    #[test]
    fn test_mixed_content_text_only_incomplete_model() {
        // A mixed complex type with required children in a sequence.
        // Pushing only text (no child elements) → content model incomplete error.
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root">
                    <xs:complexType mixed="true">
                        <xs:sequence>
                            <xs:element name="a" type="xs:string"/>
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

        // Only text, no child elements
        v.validate_text("just text");

        v.validate_end_element();
        v.end_validation().ok();

        // Content model is incomplete because required child <a> was never provided
        assert!(
            v.sink.errors.iter().any(|e| e.constraint == "cvc-complex-type.2.4"),
            "expected content model incomplete error, got: {:?}",
            v.sink.errors
        );
    }

    #[test]
    fn test_mixed_content_whitespace_accumulated() {
        // A mixed complex type should accumulate whitespace (not discard it
        // like element-only content does). We push whitespace between
        // required children to verify it is accepted without error.
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root">
                    <xs:complexType mixed="true">
                        <xs:sequence>
                            <xs:element name="a" type="xs:string"/>
                        </xs:sequence>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"#,
        );

        let sink = TestSink::new();
        let mut v = SchemaValidator::new(&schema_set, sink, ValidationFlags::default());
        let ns = empty_ns_context();

        let info = v.validate_element("root", "", None, None, &ns);
        assert_eq!(info.content_type, Some(ContentType::Mixed));
        v.validate_end_of_attributes();

        // Whitespace before the child — accumulated in mixed, discarded in element-only
        v.validate_whitespace("   ");

        v.validate_element("a", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_text("val");
        v.validate_end_element();

        // Whitespace after the child
        v.validate_whitespace("  \n  ");

        v.validate_end_element();
        assert!(v.end_validation().is_ok());
        assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
    }

    #[test]
    fn test_element_only_rejects_non_whitespace_text() {
        // A non-mixed complex type with a sequence. Pushing non-whitespace
        // text should produce a cvc-complex-type.2.3 error.
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="a" type="xs:string"/>
                        </xs:sequence>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"#,
        );

        let sink = TestSink::new();
        let mut v = SchemaValidator::new(&schema_set, sink, ValidationFlags::default());
        let ns = empty_ns_context();

        let info = v.validate_element("root", "", None, None, &ns);
        assert_eq!(info.content_type, Some(ContentType::ElementOnly));
        v.validate_end_of_attributes();

        // Non-whitespace text in element-only content
        v.validate_text("not allowed here");

        v.validate_element("a", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_text("val");
        v.validate_end_element();

        v.validate_end_element();
        v.end_validation().ok();

        assert!(
            v.sink.errors.iter().any(|e| e.constraint == "cvc-complex-type.2.3"),
            "expected element-only text error, got: {:?}",
            v.sink.errors
        );
    }

    #[test]
    fn test_mixed_content_wrong_child_order() {
        // A mixed complex type with xs:sequence(a, b). Children in wrong
        // order should still produce a content model error — mixed allows
        // text but still enforces child element order.
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root">
                    <xs:complexType mixed="true">
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

        v.validate_text("some text ");

        // Wrong order: b before a
        v.validate_element("b", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_end_element();

        v.validate_text(" more text ");

        v.validate_element("a", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_end_element();

        v.validate_end_element();
        v.end_validation().ok();

        assert!(
            v.sink.errors.iter().any(|e| e.constraint == "cvc-complex-type.2.4"),
            "expected content model error for wrong child order, got: {:?}",
            v.sink.errors
        );
    }

    #[test]
    fn test_mixed_content_model_complete() {
        // A mixed complex type where all required children are provided.
        // Text is interleaved; content model should be complete → valid.
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root">
                    <xs:complexType mixed="true">
                        <xs:sequence>
                            <xs:element name="a" type="xs:string"/>
                        </xs:sequence>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"#,
        );

        let sink = TestSink::new();
        let mut v = SchemaValidator::new(&schema_set, sink, ValidationFlags::default());
        let ns = empty_ns_context();

        let info = v.validate_element("root", "", None, None, &ns);
        assert_eq!(info.content_type, Some(ContentType::Mixed));
        v.validate_end_of_attributes();

        // Text before required child
        v.validate_text("prefix ");

        // Provide the required child
        v.validate_element("a", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_text("child value");
        v.validate_end_element();

        // Text after child — content model should be complete
        v.validate_text(" suffix");

        let end_info = v.validate_end_element();
        assert_eq!(end_info.validity, SchemaValidity::Valid);

        assert!(v.end_validation().is_ok());
        assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
    }

    #[test]
    fn test_minoccurs_zero_element_in_sequence() {
        // An element with minOccurs="0" inside a sequence.
        // Omitting the optional element should produce no errors.
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="a" type="xs:string" minOccurs="0"/>
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
        // Do NOT push child <a> — it is optional
        let end_info = v.validate_end_element();
        assert_eq!(end_info.validity, SchemaValidity::Valid);

        assert!(v.end_validation().is_ok());
        assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
    }

    #[test]
    fn test_maxoccurs_unbounded_element_in_sequence() {
        // An element with maxOccurs="unbounded" inside a sequence.
        // Pushing multiple children should produce no errors.
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="a" type="xs:string" maxOccurs="unbounded"/>
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

        // Push three <a> children — all should be accepted
        for _ in 0..3 {
            v.validate_element("a", "", None, None, &ns);
            v.validate_end_of_attributes();
            v.validate_text("value");
            v.validate_end_element();
        }

        let end_info = v.validate_end_element();
        assert_eq!(end_info.validity, SchemaValidity::Valid);

        assert!(v.end_validation().is_ok());
        assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
    }

    #[test]
    fn test_mixed_content_optional_children_text_only() {
        // Mixed complex type where all children are optional.
        // Pushing only text (no child elements) should be valid.
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root">
                    <xs:complexType mixed="true">
                        <xs:sequence>
                            <xs:element name="a" type="xs:string" minOccurs="0"/>
                            <xs:element name="b" type="xs:string" minOccurs="0"/>
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

        // Only text, no child elements
        v.validate_text("just text content");

        let end_info = v.validate_end_element();
        assert_eq!(end_info.validity, SchemaValidity::Valid);

        assert!(v.end_validation().is_ok());
        assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
    }

    #[test]
    fn test_nil_element_rejects_child_elements() {
        // cvc-elt.3.2.1: A nilled element must not have child element content
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="parent" nillable="true">
                                <xs:complexType>
                                    <xs:sequence>
                                        <xs:element name="child" type="xs:string"/>
                                    </xs:sequence>
                                </xs:complexType>
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

        // Open "parent" with xsi:nil="true"
        let info = v.validate_element("parent", "", None, Some("true"), &ns);
        assert!(info.is_nil);
        v.validate_end_of_attributes();

        // Try to add a child element — should trigger cvc-elt.3.2.1
        v.validate_element("child", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_end_element();

        v.validate_end_element(); // close parent
        v.validate_end_element(); // close root
        v.end_validation().ok();

        assert!(
            v.sink
                .errors
                .iter()
                .any(|e| e.constraint == "cvc-elt.3.2.1"),
            "expected cvc-elt.3.2.1 error for child element in nilled parent, got: {:?}",
            v.sink.errors
        );
    }

    #[test]
    fn test_nil_element_allows_attributes_only() {
        // A nilled element with only attributes (no child elements, no text) is valid
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="item" nillable="true">
                                <xs:complexType>
                                    <xs:sequence>
                                        <xs:element name="child" type="xs:string"/>
                                    </xs:sequence>
                                    <xs:attribute name="id" type="xs:string"/>
                                </xs:complexType>
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

        let info = v.validate_element("item", "", None, Some("true"), &ns);
        assert!(info.is_nil);
        // Attribute on nilled element is valid
        v.validate_attribute("id", "", "123");
        v.validate_end_of_attributes();

        // No child elements, no text — just close
        v.validate_end_element(); // close item
        v.validate_end_element(); // close root
        v.end_validation().ok();

        assert!(
            v.sink.errors.is_empty(),
            "nilled element with attributes only should be valid, got: {:?}",
            v.sink.errors
        );
    }

    // -----------------------------------------------------------------------
    // Identity constraint regression tests
    // -----------------------------------------------------------------------

    /// Test 1: Simple key constraint — duplicate detection (cvc-identity-constraint.4.2.2)
    #[test]
    fn test_ic_key_duplicate() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="item" maxOccurs="unbounded">
                                <xs:complexType>
                                    <xs:attribute name="id" type="xs:string" use="required"/>
                                </xs:complexType>
                            </xs:element>
                        </xs:sequence>
                    </xs:complexType>
                    <xs:key name="itemKey">
                        <xs:selector xpath="./item"/>
                        <xs:field xpath="@id"/>
                    </xs:key>
                </xs:element>
            </xs:schema>"#,
        );

        let sink = TestSink::new();
        let mut v = SchemaValidator::new(&schema_set, sink, ValidationFlags::default());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);
        v.validate_end_of_attributes();

        // First item: @id="A"
        v.validate_element("item", "", None, None, &ns);
        v.validate_attribute("id", "", "A");
        v.validate_end_of_attributes();
        v.validate_end_element();

        // Second item: @id="A" — duplicate
        v.validate_element("item", "", None, None, &ns);
        v.validate_attribute("id", "", "A");
        v.validate_end_of_attributes();
        v.validate_end_element();

        v.validate_end_element(); // </root>
        v.end_validation().ok();

        assert!(
            v.sink.errors.iter().any(|e| e.constraint == "cvc-identity-constraint.4.2.2"),
            "Expected duplicate key error, got: {:?}",
            v.sink.errors
        );
    }

    /// Test 2: Unique constraint — incomplete allowed, duplicates rejected
    #[test]
    fn test_ic_unique_incomplete_ok_duplicate_rejected() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="item" maxOccurs="unbounded">
                                <xs:complexType>
                                    <xs:attribute name="id" type="xs:string"/>
                                </xs:complexType>
                            </xs:element>
                        </xs:sequence>
                    </xs:complexType>
                    <xs:unique name="itemUnique">
                        <xs:selector xpath="./item"/>
                        <xs:field xpath="@id"/>
                    </xs:unique>
                </xs:element>
            </xs:schema>"#,
        );

        let sink = TestSink::new();
        let mut v = SchemaValidator::new(&schema_set, sink, ValidationFlags::default());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);
        v.validate_end_of_attributes();

        // Item without @id (incomplete key sequence — ok for unique)
        v.validate_element("item", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_end_element();

        // Item with @id="X"
        v.validate_element("item", "", None, None, &ns);
        v.validate_attribute("id", "", "X");
        v.validate_end_of_attributes();
        v.validate_end_element();

        // Item with @id="X" — duplicate
        v.validate_element("item", "", None, None, &ns);
        v.validate_attribute("id", "", "X");
        v.validate_end_of_attributes();
        v.validate_end_element();

        v.validate_end_element(); // </root>
        v.end_validation().ok();

        let dup_errors: Vec<_> = v.sink.errors.iter()
            .filter(|e| e.constraint == "cvc-identity-constraint.4.2.2")
            .collect();
        assert_eq!(dup_errors.len(), 1, "Expected exactly 1 duplicate error, got: {:?}", dup_errors);
    }

    /// Test 3: Keyref cross-reference — matching + missing (cvc-identity-constraint.4.3)
    #[test]
    fn test_ic_keyref_matching_and_missing() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="item" maxOccurs="unbounded">
                                <xs:complexType>
                                    <xs:attribute name="id" type="xs:string" use="required"/>
                                </xs:complexType>
                            </xs:element>
                            <xs:element name="ref" maxOccurs="unbounded">
                                <xs:complexType>
                                    <xs:attribute name="ref" type="xs:string" use="required"/>
                                </xs:complexType>
                            </xs:element>
                        </xs:sequence>
                    </xs:complexType>
                    <xs:key name="itemKey">
                        <xs:selector xpath="./item"/>
                        <xs:field xpath="@id"/>
                    </xs:key>
                    <xs:keyref name="itemRef" refer="itemKey">
                        <xs:selector xpath="./ref"/>
                        <xs:field xpath="@ref"/>
                    </xs:keyref>
                </xs:element>
            </xs:schema>"#,
        );

        let sink = TestSink::new();
        let mut v = SchemaValidator::new(&schema_set, sink, ValidationFlags::default());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);
        v.validate_end_of_attributes();

        // Item with @id="A"
        v.validate_element("item", "", None, None, &ns);
        v.validate_attribute("id", "", "A");
        v.validate_end_of_attributes();
        v.validate_end_element();

        // Ref with @ref="A" — matches
        v.validate_element("ref", "", None, None, &ns);
        v.validate_attribute("ref", "", "A");
        v.validate_end_of_attributes();
        v.validate_end_element();

        // Ref with @ref="B" — no match
        v.validate_element("ref", "", None, None, &ns);
        v.validate_attribute("ref", "", "B");
        v.validate_end_of_attributes();
        v.validate_end_element();

        v.validate_end_element(); // </root>
        v.end_validation().ok();

        let keyref_errors: Vec<_> = v.sink.errors.iter()
            .filter(|e| e.constraint == "cvc-identity-constraint.4.3")
            .collect();
        assert_eq!(keyref_errors.len(), 1, "Expected 1 keyref error for missing 'B', got: {:?}", keyref_errors);
    }

    /// Test 4: Element field value — field matches element text content
    #[test]
    fn test_ic_element_field_value() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="item" maxOccurs="unbounded">
                                <xs:complexType>
                                    <xs:sequence>
                                        <xs:element name="code" type="xs:string"/>
                                    </xs:sequence>
                                </xs:complexType>
                            </xs:element>
                        </xs:sequence>
                    </xs:complexType>
                    <xs:key name="codeKey">
                        <xs:selector xpath="./item"/>
                        <xs:field xpath="code"/>
                    </xs:key>
                </xs:element>
            </xs:schema>"#,
        );

        let sink = TestSink::new();
        let mut v = SchemaValidator::new(&schema_set, sink, ValidationFlags::default());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);
        v.validate_end_of_attributes();

        // First item with code="X"
        v.validate_element("item", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_element("code", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_text("X");
        v.validate_end_element(); // </code>
        v.validate_end_element(); // </item>

        // Second item with code="X" — duplicate
        v.validate_element("item", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_element("code", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_text("X");
        v.validate_end_element(); // </code>
        v.validate_end_element(); // </item>

        v.validate_end_element(); // </root>
        v.end_validation().ok();

        assert!(
            v.sink.errors.iter().any(|e| e.constraint == "cvc-identity-constraint.4.2.2"),
            "Expected duplicate key error for element field, got: {:?}",
            v.sink.errors
        );
    }

    /// Test 5: Attribute field value — field matches @attr
    #[test]
    fn test_ic_attribute_field_value() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="item" maxOccurs="unbounded">
                                <xs:complexType>
                                    <xs:attribute name="val" type="xs:string" use="required"/>
                                </xs:complexType>
                            </xs:element>
                        </xs:sequence>
                    </xs:complexType>
                    <xs:unique name="valUnique">
                        <xs:selector xpath="./item"/>
                        <xs:field xpath="@val"/>
                    </xs:unique>
                </xs:element>
            </xs:schema>"#,
        );

        let sink = TestSink::new();
        let mut v = SchemaValidator::new(&schema_set, sink, ValidationFlags::default());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);
        v.validate_end_of_attributes();

        // Two items with different values — should be fine
        v.validate_element("item", "", None, None, &ns);
        v.validate_attribute("val", "", "alpha");
        v.validate_end_of_attributes();
        v.validate_end_element();

        v.validate_element("item", "", None, None, &ns);
        v.validate_attribute("val", "", "beta");
        v.validate_end_of_attributes();
        v.validate_end_element();

        v.validate_end_element();
        v.end_validation().ok();

        assert!(
            v.sink.errors.is_empty(),
            "Expected no errors for unique values, got: {:?}",
            v.sink.errors
        );
    }

    /// Test 7: ID duplicate detection (cvc-id.2)
    #[test]
    fn test_id_duplicate() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="item" maxOccurs="unbounded">
                                <xs:complexType>
                                    <xs:attribute name="id" type="xs:ID" use="required"/>
                                </xs:complexType>
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

        // First item: @id="a1"
        v.validate_element("item", "", None, None, &ns);
        v.validate_attribute("id", "", "a1");
        v.validate_end_of_attributes();
        v.validate_end_element();

        // Second item: @id="a1" — duplicate ID
        v.validate_element("item", "", None, None, &ns);
        v.validate_attribute("id", "", "a1");
        v.validate_end_of_attributes();
        v.validate_end_element();

        v.validate_end_element();
        v.end_validation().ok();

        assert!(
            v.sink.errors.iter().any(|e| e.constraint == "cvc-id.2"),
            "Expected duplicate ID error, got: {:?}",
            v.sink.errors
        );
    }

    /// Test 8: IDREF validation — valid + missing reference (cvc-id.1)
    #[test]
    fn test_idref_valid_and_missing() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="item" maxOccurs="unbounded">
                                <xs:complexType>
                                    <xs:attribute name="id" type="xs:ID" use="required"/>
                                </xs:complexType>
                            </xs:element>
                            <xs:element name="link" maxOccurs="unbounded">
                                <xs:complexType>
                                    <xs:attribute name="ref" type="xs:IDREF" use="required"/>
                                </xs:complexType>
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

        // Define ID
        v.validate_element("item", "", None, None, &ns);
        v.validate_attribute("id", "", "x1");
        v.validate_end_of_attributes();
        v.validate_end_element();

        // Valid IDREF
        v.validate_element("link", "", None, None, &ns);
        v.validate_attribute("ref", "", "x1");
        v.validate_end_of_attributes();
        v.validate_end_element();

        // Missing IDREF
        v.validate_element("link", "", None, None, &ns);
        v.validate_attribute("ref", "", "missing");
        v.validate_end_of_attributes();
        v.validate_end_element();

        v.validate_end_element();
        v.end_validation().ok();

        let idref_errors: Vec<_> = v.sink.errors.iter()
            .filter(|e| e.constraint == "cvc-id.1")
            .collect();
        assert_eq!(idref_errors.len(), 1, "Expected 1 IDREF error for 'missing', got: {:?}", idref_errors);
    }

    /// Test 9: Nested selector matches (.//item with nested items)
    #[test]
    fn test_ic_nested_selector() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="item" maxOccurs="unbounded">
                                <xs:complexType>
                                    <xs:sequence>
                                        <xs:element name="item" minOccurs="0" maxOccurs="unbounded">
                                            <xs:complexType>
                                                <xs:attribute name="id" type="xs:string" use="required"/>
                                            </xs:complexType>
                                        </xs:element>
                                    </xs:sequence>
                                    <xs:attribute name="id" type="xs:string" use="required"/>
                                </xs:complexType>
                            </xs:element>
                        </xs:sequence>
                    </xs:complexType>
                    <xs:unique name="allItems">
                        <xs:selector xpath=".//item"/>
                        <xs:field xpath="@id"/>
                    </xs:unique>
                </xs:element>
            </xs:schema>"#,
        );

        let sink = TestSink::new();
        let mut v = SchemaValidator::new(&schema_set, sink, ValidationFlags::default());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);
        v.validate_end_of_attributes();

        // Outer item @id="1"
        v.validate_element("item", "", None, None, &ns);
        v.validate_attribute("id", "", "1");
        v.validate_end_of_attributes();

        // Inner item @id="2" (nested)
        v.validate_element("item", "", None, None, &ns);
        v.validate_attribute("id", "", "2");
        v.validate_end_of_attributes();
        v.validate_end_element(); // </inner item>

        v.validate_end_element(); // </outer item>

        v.validate_end_element(); // </root>
        v.end_validation().ok();

        assert!(
            v.sink.errors.is_empty(),
            "Expected no errors for unique nested items, got: {:?}",
            v.sink.errors
        );
    }

    /// Test 10: Keyref + key on same element, scope-local resolution
    #[test]
    fn test_ic_keyref_key_same_scope() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="dept" maxOccurs="unbounded">
                                <xs:complexType>
                                    <xs:attribute name="id" type="xs:string" use="required"/>
                                </xs:complexType>
                            </xs:element>
                            <xs:element name="emp" maxOccurs="unbounded">
                                <xs:complexType>
                                    <xs:attribute name="dept" type="xs:string" use="required"/>
                                </xs:complexType>
                            </xs:element>
                        </xs:sequence>
                    </xs:complexType>
                    <xs:key name="deptKey">
                        <xs:selector xpath="./dept"/>
                        <xs:field xpath="@id"/>
                    </xs:key>
                    <xs:keyref name="empDeptRef" refer="deptKey">
                        <xs:selector xpath="./emp"/>
                        <xs:field xpath="@dept"/>
                    </xs:keyref>
                </xs:element>
            </xs:schema>"#,
        );

        let sink = TestSink::new();
        let mut v = SchemaValidator::new(&schema_set, sink, ValidationFlags::default());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);
        v.validate_end_of_attributes();

        // Departments
        v.validate_element("dept", "", None, None, &ns);
        v.validate_attribute("id", "", "sales");
        v.validate_end_of_attributes();
        v.validate_end_element();

        v.validate_element("dept", "", None, None, &ns);
        v.validate_attribute("id", "", "eng");
        v.validate_end_of_attributes();
        v.validate_end_element();

        // Employee referencing existing dept — valid
        v.validate_element("emp", "", None, None, &ns);
        v.validate_attribute("dept", "", "sales");
        v.validate_end_of_attributes();
        v.validate_end_element();

        // Employee referencing non-existing dept — invalid
        v.validate_element("emp", "", None, None, &ns);
        v.validate_attribute("dept", "", "hr");
        v.validate_end_of_attributes();
        v.validate_end_element();

        v.validate_end_element(); // </root>
        v.end_validation().ok();

        let keyref_errors: Vec<_> = v.sink.errors.iter()
            .filter(|e| e.constraint == "cvc-identity-constraint.4.3")
            .collect();
        assert_eq!(keyref_errors.len(), 1, "Expected 1 keyref error for 'hr', got: {:?}", keyref_errors);
    }

    /// Test: Key constraint with no duplicates — valid document
    #[test]
    fn test_ic_key_no_duplicates_valid() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="item" maxOccurs="unbounded">
                                <xs:complexType>
                                    <xs:attribute name="id" type="xs:string" use="required"/>
                                </xs:complexType>
                            </xs:element>
                        </xs:sequence>
                    </xs:complexType>
                    <xs:key name="pk">
                        <xs:selector xpath="./item"/>
                        <xs:field xpath="@id"/>
                    </xs:key>
                </xs:element>
            </xs:schema>"#,
        );

        let sink = TestSink::new();
        let mut v = SchemaValidator::new(&schema_set, sink, ValidationFlags::default());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);
        v.validate_end_of_attributes();

        v.validate_element("item", "", None, None, &ns);
        v.validate_attribute("id", "", "A");
        v.validate_end_of_attributes();
        v.validate_end_element();

        v.validate_element("item", "", None, None, &ns);
        v.validate_attribute("id", "", "B");
        v.validate_end_of_attributes();
        v.validate_end_element();

        v.validate_end_element();
        v.end_validation().ok();

        assert!(
            v.sink.errors.is_empty(),
            "Expected no errors for unique keys, got: {:?}",
            v.sink.errors
        );
    }

}
