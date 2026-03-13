//! `ValidationRuntime` — mutable per-run validation state.
//!
//! Created by [`SchemaValidator::start_run()`]. Holds the validation stack,
//! identity constraint tables, sink, and all other per-run mutable state.
//! Method bodies are moved verbatim from the former monolithic `SchemaValidator`.

use std::collections::{HashMap, HashSet};
use std::marker::PhantomData;

#[cfg(feature = "xsd11")]
use bumpalo::Bump;
use crate::arenas::{ComplexTypeDefData, ResolvedAttributeUse};
use crate::compiler::{compile_content_model_matcher, SubstitutionGroupMap};
use crate::ids::{AttributeGroupKey, ComplexTypeKey, ElementKey, IdentityConstraintKey, NameId, TypeKey, AttributeKey};
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

#[cfg(feature = "xsd11")]
use super::assertions::{
    AssertionBufferFrame, evaluate_complex_type_assertions, has_inherited_assertions,
};
#[cfg(feature = "xsd11")]
use super::validator::AssertionSource;
use super::validator::{ValidationSink, ValidationWarning};

#[cfg(feature = "xsd11")]
use crate::document::builder::BufferDocumentBuilder;
#[cfg(feature = "xsd11")]
use crate::document::BufferDocumentOptions;

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
// XsiTypeOutcome — three-state result from resolve_xsi_type
// ---------------------------------------------------------------------------

/// Result of resolving an `xsi:type` attribute value.
enum XsiTypeOutcome {
    /// Successfully resolved and derivation-valid — use this type.
    Applied(TypeKey),
    /// QName invalid or type not found (cvc-elt.4.1).
    Unresolved,
    /// Type found but does not validly derive from declared type (cvc-elt.4.2).
    InvalidDerivation,
}

// ---------------------------------------------------------------------------
// ValidationRuntime
// ---------------------------------------------------------------------------

/// Mutable per-run validation state.
///
/// Created by [`super::validator::SchemaValidator::start_run()`].
/// Holds the validation stack, identity constraint tables, sink, and all
/// other per-run mutable state. The struct is explicitly `!Send + !Sync`.
pub struct ValidationRuntime<'a, S: ValidationSink> {
    /// The compiled schema set to validate against (borrowed from SchemaValidator)
    pub(crate) schema_set: &'a SchemaSet,
    /// Pre-built substitution group map (borrowed from SchemaValidator)
    pub(crate) subst_groups: &'a Option<SubstitutionGroupMap>,
    /// Validation flags controlling behaviour (copied from SchemaValidator)
    pub(crate) flags: ValidationFlags,
    /// Sink for errors and warnings
    pub sink: S,
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
    /// Per-element scope stack of key/unique tables
    ic_scope_tables: Vec<Option<HashMap<IdentityConstraintKey, KeyTable>>>,
    /// Which assertion evaluation path is active (XSD 1.1 only)
    #[cfg(feature = "xsd11")]
    pub(crate) assertion_source: AssertionSource,
    /// Active fragment document builder (XSD 1.1).
    /// SAFETY: borrows from fragment_arena via lifetime extension. Must be
    /// dropped (.take()) before fragment_arena is reset/dropped.
    /// Declared before fragment_arena to ensure correct drop order.
    #[cfg(feature = "xsd11")]
    fragment_builder: Option<BufferDocumentBuilder<'a>>,
    /// Heap-stable bump arena for assertion fragments (XSD 1.1 only).
    #[cfg(feature = "xsd11")]
    fragment_arena: Option<Box<Bump>>,
    /// Stack of assertion buffer frames (XSD 1.1).
    #[cfg(feature = "xsd11")]
    assertion_buffer_stack: Vec<AssertionBufferFrame>,
    /// Deferred assertion frames from nested asserted elements (XSD 1.1).
    #[cfg(feature = "xsd11")]
    pending_assertion_frames: Vec<AssertionBufferFrame>,
    /// Deferred attribute PSVI results from CTA processing (XSD 1.1).
    #[cfg(feature = "xsd11")]
    deferred_attribute_results: Vec<SchemaInfo>,
    /// `!Send + !Sync` marker
    _not_thread_safe: PhantomData<*const ()>,
}

impl<'a, S: ValidationSink> ValidationRuntime<'a, S> {
    /// Create a new `ValidationRuntime` (called by `SchemaValidator::start_run()`).
    pub(crate) fn new(
        schema_set: &'a SchemaSet,
        subst_groups: &'a Option<SubstitutionGroupMap>,
        flags: ValidationFlags,
        sink: S,
        #[cfg(feature = "xsd11")]
        assertion_source: AssertionSource,
    ) -> Self {
        ValidationRuntime {
            schema_set,
            subst_groups,
            flags,
            sink,
            validation_stack: Vec::new(),
            current_state: ValidatorState::None,
            current_location: None,
            element_path: String::new(),
            compiled_constraints: HashMap::new(),
            active_constraints: Vec::new(),
            id_values: HashSet::new(),
            pending_idrefs: Vec::new(),
            ic_scope_tables: Vec::new(),
            #[cfg(feature = "xsd11")]
            assertion_source,
            #[cfg(feature = "xsd11")]
            fragment_builder: None,
            #[cfg(feature = "xsd11")]
            fragment_arena: None,
            #[cfg(feature = "xsd11")]
            assertion_buffer_stack: Vec::new(),
            #[cfg(feature = "xsd11")]
            pending_assertion_frames: Vec::new(),
            #[cfg(feature = "xsd11")]
            deferred_attribute_results: Vec::new(),
            _not_thread_safe: PhantomData,
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

    /// Returns a reference to the fragment arena, if it has been allocated.
    #[cfg(feature = "xsd11")]
    #[cfg(test)]
    fn fragment_arena(&self) -> Option<&Bump> {
        self.fragment_arena.as_deref()
    }

    /// Returns a mutable reference to the fragment arena, allocating it on
    /// first use.
    ///
    /// # Panics (debug)
    /// Panics if `fragment_builder` is active, which would mean the arena
    /// is borrowed and mutation would invalidate the builder's reference.
    #[cfg(feature = "xsd11")]
    #[cfg(test)]
    fn fragment_arena_mut(&mut self) -> &mut Bump {
        debug_assert!(
            self.fragment_builder.is_none(),
            "fragment_arena_mut() called while fragment_builder is active — \
             would invalidate the builder's borrow"
        );
        self.fragment_arena.get_or_insert_with(|| Box::new(Bump::new()))
    }

    // -----------------------------------------------------------------------
    // Assertion buffering helpers (XSD 1.1)
    // -----------------------------------------------------------------------

    /// Returns `true` if assertion buffering is currently active.
    #[cfg(feature = "xsd11")]
    #[inline]
    fn is_buffering_assertions(&self) -> bool {
        !self.assertion_buffer_stack.is_empty()
    }

    /// Creates the arena + builder when the outermost asserted element is
    /// encountered. Returns `false` if builder creation fails.
    #[cfg(feature = "xsd11")]
    fn begin_assertion_buffering(&mut self) -> bool {
        let arena_box = self.fragment_arena.get_or_insert_with(|| Box::new(Bump::new()));
        // SAFETY: Box<Bump> is heap-allocated — stable address across struct moves.
        // The builder will be dropped (via .take()) before the arena is reset or dropped.
        // Field declaration order guarantees fragment_builder drops before fragment_arena.
        let arena_ref: &'a Bump = unsafe { &*(&**arena_box as *const Bump) };
        let names_ref: &'a crate::namespace::table::NameTable = &self.schema_set.name_table;
        match BufferDocumentBuilder::new(
            arena_ref,
            names_ref,
            None,
            BufferDocumentOptions::fragment(),
        ) {
            Ok(builder) => {
                self.fragment_builder = Some(builder);
                true
            }
            Err(e) => {
                self.report_error(
                    "cvc-assertion",
                    format!("Failed to create assertion fragment builder: {}", e),
                );
                false
            }
        }
    }

    /// Abort assertion buffering due to a builder error. Drops the builder,
    /// clears all assertion state, and resets the arena. Called when a
    /// forwarding operation (attribute/end_element) fails, to prevent a
    /// desynchronized fragment from producing wrong assertion results.
    #[cfg(feature = "xsd11")]
    fn abort_assertion_buffering(&mut self, error_msg: String) {
        self.report_error("cvc-assertion", error_msg);
        // Drop builder before resetting arena (maintains safety invariant)
        self.fragment_builder.take();
        self.assertion_buffer_stack.clear();
        self.pending_assertion_frames.clear();
        if let Some(arena) = self.fragment_arena.as_mut() {
            arena.reset();
        }
    }

    /// Called after every `push_element()`. Detects whether the element's
    /// complex type has assertions. If so, starts or extends assertion
    /// buffering. Also forwards `start_element` to the builder for all
    /// children within an active buffered scope.
    #[cfg(feature = "xsd11")]
    fn detect_assertions_on_element(
        &mut self,
        type_key: Option<TypeKey>,
        local_name: NameId,
        namespace: Option<NameId>,
    ) {
        if self.assertion_source != AssertionSource::FragmentBuffer {
            return;
        }
        let has_assertions = match type_key {
            Some(TypeKey::Complex(ct_key))
                if has_inherited_assertions(ct_key, &self.schema_set.arenas) =>
            {
                Some(ct_key)
            }
            _ => None,
        };

        let force_start = self
            .validation_stack
            .last()
            .is_some_and(|ev| ev.has_type_alternatives);

        if !self.is_buffering_assertions() && has_assertions.is_none() && !force_start {
            return; // nothing to do
        }

        // Start buffering if this is the outermost asserted element
        if !self.is_buffering_assertions() && !self.begin_assertion_buffering() {
            return; // builder creation failed — error already reported
        }

        // Forward start_element to builder (all children in scope)
        let local = self.schema_set.name_table.resolve(local_name);
        let ns = namespace
            .map(|id| self.schema_set.name_table.resolve(id).to_string())
            .unwrap_or_default();
        let element_ref = match self.fragment_builder.as_mut() {
            Some(builder) => match builder.start_element(&local, &ns, "", &[]) {
                Ok(r) => r,
                Err(e) => {
                    self.report_error(
                        "cvc-assertion",
                        format!("Assertion fragment buffer error: {}", e),
                    );
                    return;
                }
            },
            None => return, // builder was not created (error already reported)
        };

        // Save element_ref for potential CTA re-detection
        if let Some(ev) = self.validation_stack.last_mut() {
            ev.assertion_element_ref = Some(element_ref);
        }

        // Push assertion frame if this element has assertions
        if let Some(ct_key) = has_assertions {
            self.assertion_buffer_stack.push(AssertionBufferFrame {
                element_ref,
                complex_type_key: ct_key,
                element_path: String::new(), // populated at end-element
                location: None,              // populated at end-element
            });
            if let Some(ev) = self.validation_stack.last_mut() {
                ev.owns_assertion_buffer = true;
            }
        }
    }

    /// Re-detect assertions after CTA switched the type, without
    /// re-emitting `start_element` on the fragment builder. Pops any
    /// stale assertion frame for the old type and pushes a new one
    /// for the new type if it carries inherited assertions.
    #[cfg(feature = "xsd11")]
    fn redetect_assertions_after_cta(&mut self, new_type: Option<TypeKey>) {
        if self.assertion_source != AssertionSource::FragmentBuffer {
            return;
        }

        // Pop old assertion frame, saving its element_ref
        let old_element_ref = if let Some(ev) = self.validation_stack.last_mut() {
            if ev.owns_assertion_buffer {
                let frame = self.assertion_buffer_stack.pop();
                ev.owns_assertion_buffer = false;
                frame.map(|f| f.element_ref)
            } else {
                None
            }
        } else {
            return;
        };

        // Check if new type has assertions
        let new_ct_key = match new_type {
            Some(TypeKey::Complex(ct_key))
                if has_inherited_assertions(ct_key, &self.schema_set.arenas) =>
            {
                ct_key
            }
            _ => {
                // New type has no assertions. If no parent is buffering,
                // tear down the builder to avoid a dangling fragment_builder
                // at end_validation. This covers both the case where we
                // popped an old frame (old_element_ref is Some) and the
                // force_start case where no frame was ever pushed
                // (old_element_ref is None but fragment_builder exists).
                if self.assertion_buffer_stack.is_empty() && self.fragment_builder.is_some() {
                    self.fragment_builder.take();
                    self.pending_assertion_frames.clear();
                    if let Some(arena) = self.fragment_arena.as_mut() {
                        arena.reset();
                    }
                }
                return;
            }
        };

        // Get element_ref: prefer the old frame's ref, fall back to saved ref
        let element_ref = old_element_ref.or_else(|| {
            self.validation_stack
                .last()
                .and_then(|ev| ev.assertion_element_ref)
        });

        let Some(element_ref) = element_ref else {
            return;
        };

        // Compute BEFORE pushing: replay needed when no own frame existed AND
        // no parent was buffering (attrs weren't forwarded during attr phase).
        let need_replay = old_element_ref.is_none() && !self.is_buffering_assertions();

        self.assertion_buffer_stack.push(AssertionBufferFrame {
            element_ref,
            complex_type_key: new_ct_key,
            element_path: String::new(),
            location: None,
        });
        if let Some(ev) = self.validation_stack.last_mut() {
            ev.owns_assertion_buffer = true;
        }

        if need_replay {
            let collected: Vec<_> = match self.validation_stack.last() {
                Some(ev) => ev.collected_attributes.clone(),
                None => return,
            };
            for (ns, name, value) in &collected {
                let local = self.schema_set.name_table.resolve(*name);
                let ns_str = ns
                    .map(|id| self.schema_set.name_table.resolve(id).to_string())
                    .unwrap_or_default();
                let result = self
                    .fragment_builder
                    .as_mut()
                    .map(|b| b.attribute(&local, &ns_str, "", value));
                if let Some(Err(e)) = result {
                    self.abort_assertion_buffering(format!(
                        "Assertion fragment buffer error (attribute replay): {}",
                        e
                    ));
                    return;
                }
            }
        }
    }

    /// Report assertion errors for the outermost frame (current element).
    #[cfg(feature = "xsd11")]
    fn report_assertion_errors(
        &mut self,
        assertion_errors: Vec<ValidationError>,
        ev_state: &mut ElementValidationState,
    ) {
        for err in assertion_errors {
            let err = if !self.element_path.is_empty() {
                err.with_path(self.element_path.clone())
            } else {
                err
            };
            let err = match &self.current_location {
                Some(loc) => err.with_location(loc.clone()),
                None => err,
            };
            self.sink.on_error(err);
            ev_state.validity = SchemaValidity::Invalid;
        }
    }

    /// Report assertion errors for deferred (nested) frames using their stored
    /// path and location instead of the current runtime state.
    ///
    /// Note: deferred errors are reported to the sink but do **not** affect the
    /// outer (current) element's `SchemaValidity`. This is intentional per XSD
    /// 1.1 §3.13.4.1: each element's validity is determined by its own type's
    /// assertions, not by those of descendant elements.
    #[cfg(feature = "xsd11")]
    fn report_assertion_errors_deferred(
        &mut self,
        assertion_errors: Vec<ValidationError>,
        element_path: &str,
        location: &Option<SourceLocation>,
    ) {
        for err in assertion_errors {
            let err = if !element_path.is_empty() {
                err.with_path(element_path.to_string())
            } else {
                err
            };
            let err = match location {
                Some(loc) => err.with_location(loc.clone()),
                None => err,
            };
            self.sink.on_error(err);
        }
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

        // 1b. Root element: verify PROCESS_ASSERTIONS ↔ AssertionSource consistency
        #[cfg(feature = "xsd11")]
        if self.validation_stack.is_empty() {
            let has_flag = self.flags.contains(ValidationFlags::PROCESS_ASSERTIONS);
            let is_fragment = self.assertion_source == AssertionSource::FragmentBuffer;
            assert!(
                has_flag == is_fragment,
                "PROCESS_ASSERTIONS flag and AssertionSource are inconsistent: \
                 flag={has_flag}, source={:?}. Call set_assertion_source() before validation.",
                self.assertion_source,
            );
        }

        // 2. If not root: advance parent's content model
        let mut match_info: Option<super::content::ElementMatchInfo> = None;
        let mut content_model_accepted = false;
        let mut content_model_error = None;
        let mut nil_error: Option<String> = None;
        if let Some(parent) = self.validation_stack.last_mut() {
            if parent.process_contents == ContentProcessing::Skip {
                // Skipped element: don't validate content model, push as skip, return
                parent.has_element_children = true;
                let mut ev_state = ElementValidationState::new(local_name, namespace);
                ev_state.process_contents = ContentProcessing::Skip;
                ev_state.content_state = ContentValidatorState::Simple;
                ev_state.validity = SchemaValidity::NotKnown;
                self.push_element(ev_state);
                self.advance_constraints_start_element(local_name, namespace, None);
                #[cfg(feature = "xsd11")]
                self.detect_assertions_on_element(None, local_name, namespace);
                return SchemaInfo::empty();
            } else if parent.is_nil {
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
                // Content model accepted this element (wildcard in content model)
                // but no global declaration exists.
                let is_nil = matches!(xsi_nil, Some("true") | Some("1"));
                let mut ev_state = ElementValidationState::new(local_name, namespace);
                ev_state.validity = SchemaValidity::Valid;
                ev_state.process_contents = process_contents;
                ev_state.is_nil = is_nil;

                if let Some(mut type_key) = matched_type {
                    // PATH B1: xsi:type override for local elements with resolved type
                    if let Some(xsi_type_str) = xsi_type {
                        match self.resolve_xsi_type(xsi_type_str, Some(type_key), ns_context) {
                            XsiTypeOutcome::Applied(overridden) => {
                                type_key = overridden;
                            }
                            XsiTypeOutcome::Unresolved | XsiTypeOutcome::InvalidDerivation => {
                                ev_state.validity = SchemaValidity::Invalid;
                                // keep original type_key
                            }
                        }
                    }
                    // Local element with resolved type — initialize content model
                    let (content_state, content_type) = self.init_content_model(Some(type_key));
                    ev_state.schema_type = Some(type_key);
                    ev_state.content_state = content_state;
                    ev_state.content_type = Some(content_type);
                } else {
                    // PATH B2/B3: No declaration and no matched type.
                    // Try xsi:type first — it can supply a governing type even
                    // without a declaration.
                    if let Some(xsi_type_str) = xsi_type {
                        match self.resolve_xsi_type(xsi_type_str, None, ns_context) {
                            XsiTypeOutcome::Applied(overridden) => {
                                let (content_state, content_type) =
                                    self.init_content_model(Some(overridden));
                                ev_state.schema_type = Some(overridden);
                                ev_state.content_state = content_state;
                                ev_state.content_type = Some(content_type);
                            }
                            XsiTypeOutcome::Unresolved | XsiTypeOutcome::InvalidDerivation => {
                                // No governing type — lax assessment
                                ev_state.validity = SchemaValidity::Invalid;
                                let (content_state, content_type) =
                                    self.lax_assessment_content_model();
                                ev_state.content_state = content_state;
                                ev_state.content_type = Some(content_type);
                                // schema_type stays None (no governing type)
                            }
                        }
                    } else {
                        // PATH B3: No governing declaration/type — lax assessment via xs:anyType
                        let (content_state, content_type) = self.lax_assessment_content_model();
                        ev_state.content_state = content_state;
                        ev_state.content_type = Some(content_type);
                        // schema_type stays None
                    }
                    // Strict wildcard with no global declaration and no governing
                    // type from xsi:type → cvc-elt.1.  Checked AFTER xsi:type so
                    // that a valid xsi:type can still supply assessment.
                    if process_contents == ContentProcessing::Strict
                        && ev_state.schema_type.is_none()
                    {
                        let elem_name = self.schema_set.name_table.resolve(local_name);
                        self.report_error(
                            "cvc-elt.1",
                            format!("Element '{}' is not declared", elem_name),
                        );
                        ev_state.validity = SchemaValidity::Invalid;
                    }
                }

                let schema_type = ev_state.schema_type;
                let content_type = ev_state.content_type;
                let validity = ev_state.validity;
                self.push_element(ev_state);
                self.advance_constraints_start_element(local_name, namespace, None);
                #[cfg(feature = "xsd11")]
                self.detect_assertions_on_element(schema_type, local_name, namespace);
                return SchemaInfo {
                    element_decl: None,
                    attribute_decl: None,
                    schema_type,
                    member_type: None,
                    validity,
                    is_default: false,
                    is_nil,
                    content_type,
                    typed_value: None,
                    deferred_by_cta: false,
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
                    #[cfg(feature = "xsd11")]
                    self.detect_assertions_on_element(None, local_name, namespace);
                    return SchemaInfo::empty();
                }
                ContentProcessing::Lax => {
                    // Lax: no declaration found — lax assessment via xs:anyType
                    let mut ev_state = ElementValidationState::new(local_name, namespace);
                    ev_state.process_contents = ContentProcessing::Lax;
                    let (content_state, content_type) = self.lax_assessment_content_model();
                    ev_state.content_state = content_state;
                    ev_state.content_type = Some(content_type);
                    // schema_type stays None — no governing type
                    ev_state.validity = SchemaValidity::NotKnown;
                    self.push_element(ev_state);
                    self.advance_constraints_start_element(local_name, namespace, None);
                    #[cfg(feature = "xsd11")]
                    self.detect_assertions_on_element(None, local_name, namespace);
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
                    // Lax assessment for content (same PSVI as lax when no declaration found)
                    let (content_state, content_type) = self.lax_assessment_content_model();
                    ev_state.content_state = content_state;
                    ev_state.content_type = Some(content_type);
                    // schema_type stays None
                    self.push_element(ev_state);
                    self.advance_constraints_start_element(local_name, namespace, None);
                    #[cfg(feature = "xsd11")]
                    self.detect_assertions_on_element(None, local_name, namespace);
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
        let mut xsi_type_invalid = false;
        if let Some(xsi_type_str) = xsi_type {
            match self.resolve_xsi_type(xsi_type_str, type_key, ns_context) {
                XsiTypeOutcome::Applied(overridden) => {
                    type_key = Some(overridden);
                }
                XsiTypeOutcome::Unresolved | XsiTypeOutcome::InvalidDerivation => {
                    // Error already reported; keep declared type, mark invalid
                    xsi_type_invalid = true;
                }
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
        ev_state.validity = if xsi_type_invalid {
            SchemaValidity::Invalid
        } else {
            SchemaValidity::Valid
        };
        ev_state.process_contents = process_contents;
        #[cfg(feature = "xsd11")]
        {
            ev_state.has_type_alternatives = !self.schema_set.arenas.elements[elem_key]
                .alternatives.is_empty();
        }
        self.push_element(ev_state);
        self.advance_constraints_start_element(local_name, namespace, Some(elem_key));

        // 9b. Assertion detection hook (XSD 1.1)
        #[cfg(feature = "xsd11")]
        self.detect_assertions_on_element(type_key, local_name, namespace);

        // 10. Return SchemaInfo
        let validity = if xsi_type_invalid {
            SchemaValidity::Invalid
        } else {
            SchemaValidity::Valid
        };
        SchemaInfo {
            element_decl: Some(elem_key),
            attribute_decl: None,
            schema_type: type_key,
            member_type: None,
            validity,
            is_default: false,
            is_nil,
            content_type: Some(content_type),
            typed_value: None,
            deferred_by_cta: false,
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

        // Forward attribute to assertion fragment builder (XSD 1.1)
        // Done before ev_state borrow to avoid borrow conflict.
        #[cfg(feature = "xsd11")]
        if self.is_buffering_assertions() {
            let local = self.schema_set.name_table.resolve(local_name);
            let ns = namespace
                .map(|id| self.schema_set.name_table.resolve(id).to_string())
                .unwrap_or_default();
            let result = self
                .fragment_builder
                .as_mut()
                .map(|b| b.attribute(&local, &ns, "", value));
            if let Some(Err(e)) = result {
                self.abort_assertion_buffering(format!(
                    "Assertion fragment buffer error (attribute): {}",
                    e
                ));
            }
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

        // When type alternatives are active, defer type-dependent attribute
        // validation until after CTA selection in validate_end_of_attributes().
        // Only collect the attribute data and perform type-independent checks.
        #[cfg(feature = "xsd11")]
        if ev_state.has_type_alternatives {
            ev_state.collected_attributes.push((namespace, local_name, value.to_string()));
            self.current_state = ValidatorState::Attribute;
            // Post-process without type info (IC field matching still works;
            // ID/IDREF will be handled during deferred validation).
            return SchemaInfo { deferred_by_cta: true, ..SchemaInfo::empty() };
        }

        // Determine effective type for attribute validation.
        // When schema_type is None (no governing type) and the element is under
        // lax assessment, use xs:anyType which has anyAttribute processContents=lax.
        let type_key = ev_state.schema_type;
        let process_contents = ev_state.process_contents;
        let ct_key = match type_key {
            Some(TypeKey::Complex(ct)) => ct,
            None if process_contents != ContentProcessing::Skip => {
                // Lax assessment: validate attributes against xs:anyType
                // (anyAttribute processContents=lax accepts any attribute)
                self.schema_set.any_type_key()
            }
            _ => {
                // Simple type or skip: no attributes expected (except xsi:*)
                // Still run post-processing so IC attribute fields and
                // ID/IDREF collection are not skipped.
                self.current_state = ValidatorState::Attribute;
                let result = SchemaInfo::empty();
                self.post_process_attribute(local_name, namespace, value, &result);
                return result;
            }
        };

        self.current_state = ValidatorState::Attribute;
        self.validate_attribute_against_type(ct_key, local_name, namespace, value)
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

        let schema_type = match self.validation_stack.last() {
            Some(s) => s.schema_type,
            None => {
                self.current_state = ValidatorState::EndOfAttributes;
                return SchemaInfo::empty();
            }
        };

        // Evaluate type alternatives (XSD 1.1)
        #[cfg(feature = "xsd11")]
        let has_type_alternatives = self
            .validation_stack
            .last()
            .is_some_and(|s| s.has_type_alternatives);

        #[cfg(feature = "xsd11")]
        let (schema_type, cta_switched) = if has_type_alternatives {
            let mut st = schema_type;
            let mut switched = false;
            if let Some(ev_state) = self.validation_stack.last() {
                if let Some(elem_key) = ev_state.element_decl {
                    let new_type = super::alternatives::evaluate_type_alternatives(
                        elem_key,
                        ev_state.local_name,
                        ev_state.namespace,
                        &ev_state.collected_attributes,
                        self.schema_set,
                    );
                    if let Some(new_type_key) = new_type {
                        if Some(new_type_key) != st {
                            let (content_state, content_type) =
                                self.init_content_model(Some(new_type_key));
                            st = Some(new_type_key);
                            switched = true;
                            if let Some(ev) = self.validation_stack.last_mut() {
                                ev.schema_type = Some(new_type_key);
                                ev.content_state = content_state;
                                ev.content_type = Some(content_type);
                            }
                        }
                    }
                }
            }
            (st, switched)
        } else {
            (schema_type, false)
        };

        #[cfg(not(feature = "xsd11"))]
        let cta_switched = false;

        // When attributes were deferred for CTA, always validate them
        // against the (possibly unchanged) type.
        #[cfg(feature = "xsd11")]
        if has_type_alternatives {
            // Re-detect assertions BEFORE draining collected_attributes,
            // so redetect can replay them into the fragment builder.
            if cta_switched {
                self.redetect_assertions_after_cta(schema_type);
            }
            self.validate_deferred_attributes(schema_type);
        }

        // Check required attributes (clone seen_attributes to avoid borrow conflict)
        if let Some(TypeKey::Complex(ct_key)) = schema_type {
            let seen_attributes = match self.validation_stack.last() {
                Some(s) => s.seen_attributes.clone(),
                None => HashSet::new(),
            };
            let ct_data = &self.schema_set.arenas.complex_types[ct_key];
            if self.check_required_attributes(ct_data, &seen_attributes) {
                self.mark_current_invalid();
            }
        }

        // Forward to assertion fragment builder (XSD 1.1)
        #[cfg(feature = "xsd11")]
        if self.is_buffering_assertions() {
            if let Some(builder) = self.fragment_builder.as_mut() {
                builder.end_of_attributes();
            }
        }

        self.current_state = ValidatorState::EndOfAttributes;

        // When CTA switched the type, return updated SchemaInfo so callers
        // (e.g. typed_builder) can update element bindings. Preserve prior
        // invalidity (e.g. from a bad xsi:type).
        if cta_switched {
            let ev = self.validation_stack.last();
            let content_type = ev.and_then(|s| s.content_type);
            let validity = ev
                .map(|s| s.validity)
                .unwrap_or(SchemaValidity::NotKnown);
            SchemaInfo {
                schema_type,
                content_type,
                validity,
                ..SchemaInfo::empty()
            }
        } else {
            SchemaInfo::empty()
        }
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

        // Forward text to assertion fragment builder (XSD 1.1)
        #[cfg(feature = "xsd11")]
        if self.is_buffering_assertions() {
            if let Some(builder) = self.fragment_builder.as_mut() {
                builder.text(text);
            }
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

        // Forward whitespace to assertion fragment builder (XSD 1.1)
        #[cfg(feature = "xsd11")]
        if self.is_buffering_assertions() {
            if let Some(builder) = self.fragment_builder.as_mut() {
                builder.text(text);
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

        // 2b. Assertion evaluation hook (XSD 1.1)
        #[cfg(feature = "xsd11")]
        'assertion_eval: {
            debug_assert!(
                self.flags.contains(ValidationFlags::PROCESS_ASSERTIONS)
                    == (self.assertion_source == AssertionSource::FragmentBuffer),
                "PROCESS_ASSERTIONS / AssertionSource invariant violated at end-element"
            );

            if !self.is_buffering_assertions() {
                // Clean up orphan fragment_builder left by force_start when
                // the CTA element's type had no assertions and cta_switched
                // was false (so redetect_assertions_after_cta was never called).
                if self.fragment_builder.is_some() {
                    self.fragment_builder.take();
                    self.pending_assertion_frames.clear();
                    if let Some(arena) = self.fragment_arena.as_mut() {
                        arena.reset();
                    }
                }
                break 'assertion_eval;
            }

            // Forward end_element to builder
            if let Some(builder) = self.fragment_builder.as_mut() {
                if let Err(e) = builder.end_element() {
                    self.abort_assertion_buffering(format!(
                        "Assertion fragment buffer error (end_element): {}",
                        e
                    ));
                    ev_state.validity = SchemaValidity::Invalid;
                    break 'assertion_eval;
                }
            }

            if ev_state.owns_assertion_buffer {
                // Pop the assertion frame for this element
                let frame = match self.assertion_buffer_stack.pop() {
                    Some(f) => f,
                    None => {
                        // Should not happen, but don't panic in validation
                        self.abort_assertion_buffering(
                            "Internal: assertion buffer stack underflow".into(),
                        );
                        ev_state.validity = SchemaValidity::Invalid;
                        break 'assertion_eval;
                    }
                };

                if self.assertion_buffer_stack.is_empty() {
                    // Outermost asserted element closes — finalize and evaluate
                    let builder = match self.fragment_builder.take() {
                        Some(b) => b,
                        None => {
                            // builder was already taken (should not happen)
                            self.pending_assertion_frames.clear();
                            if let Some(arena) = self.fragment_arena.as_mut() {
                                arena.reset();
                            }
                            break 'assertion_eval;
                        }
                    };
                    match builder.finalize() {
                        Ok(doc) => {
                            // Evaluate nested (deferred) frames first
                            let pending =
                                std::mem::take(&mut self.pending_assertion_frames);
                            for pf in &pending {
                                let errs = evaluate_complex_type_assertions(
                                    &doc,
                                    pf.element_ref,
                                    pf.complex_type_key,
                                    self.schema_set,
                                );
                                self.report_assertion_errors_deferred(
                                    errs,
                                    &pf.element_path,
                                    &pf.location,
                                );
                            }
                            // Evaluate outermost frame (current element_path is valid)
                            let errs = evaluate_complex_type_assertions(
                                &doc,
                                frame.element_ref,
                                frame.complex_type_key,
                                self.schema_set,
                            );
                            self.report_assertion_errors(errs, &mut ev_state);
                        }
                        Err(e) => {
                            // Clear stale deferred frames — they reference
                            // nodes in the failed document and must not leak
                            // into the next buffered subtree.
                            self.pending_assertion_frames.clear();
                            self.report_error(
                                "cvc-assertion",
                                format!(
                                    "Failed to finalize assertion fragment: {}",
                                    e
                                ),
                            );
                            ev_state.validity = SchemaValidity::Invalid;
                        }
                    }
                    // Reset arena for reuse
                    if let Some(arena) = self.fragment_arena.as_mut() {
                        arena.reset();
                    }
                } else {
                    // Nested asserted element — defer to outermost close
                    let mut deferred = frame;
                    deferred.element_path = self.element_path.clone();
                    deferred.location = self.current_location.clone();
                    self.pending_assertion_frames.push(deferred);
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
            deferred_by_cta: false,
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

        #[cfg(feature = "xsd11")]
        {
            debug_assert!(
                self.assertion_buffer_stack.is_empty(),
                "assertion_buffer_stack not empty at end_validation"
            );
            debug_assert!(
                self.fragment_builder.is_none(),
                "fragment_builder not None at end_validation"
            );
            debug_assert!(
                self.pending_assertion_frames.is_empty(),
                "pending_assertion_frames not empty at end_validation"
            );
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
            let (attr_name, attr_ns) =
                self.resolve_attr_use_name_ns(attr_use, resolved, ct_data.target_namespace);
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
            let (attr_name, attr_ns) =
                self.resolve_attr_use_name_ns(attr_use, resolved, ct_data.target_namespace);
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

    /// Enrich an existing `ValidationError` with location/path and report it.
    fn report_validation_error(&mut self, err: ValidationError) {
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
    }

    /// Mark the current element as invalid.
    fn mark_current_invalid(&mut self) {
        if let Some(s) = self.validation_stack.last_mut() {
            s.validity = SchemaValidity::Invalid;
        }
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
    ///
    /// Uses the normalized value from `typed_value` (not raw `value_str`)
    /// for ID and IDREF tracking, so whitespace-collapsed values match
    /// consistently across ID, IDREF, and IDREFS.
    ///
    /// For IDREF list values (both built-in xs:IDREFS and user-defined
    /// `<xs:list itemType="xs:IDREF">`), each token is tracked individually.
    fn collect_id_idref(&mut self, typed_value: &XmlValue, value_str: &str) {
        match typed_value.type_code {
            XmlTypeCode::Id => {
                let normalized = typed_value.to_string_value();
                if self.id_values.contains(&normalized) {
                    self.report_error(
                        "cvc-id.2",
                        format!("Duplicate ID value '{}'", normalized),
                    );
                } else {
                    self.id_values.insert(normalized);
                }
            }
            XmlTypeCode::IdRef | XmlTypeCode::IdRefs => {
                // Both built-in xs:IDREFS and user-defined <xs:list itemType="xs:IDREF">
                // produce XmlValueKind::List — decompose into individual IDREF tokens.
                if let crate::types::value::XmlValueKind::List { items, .. } = &typed_value.value {
                    for item in items {
                        self.pending_idrefs.push((
                            item.to_string(),
                            self.current_location.clone(),
                            self.element_path.clone(),
                        ));
                    }
                } else if typed_value.type_code == XmlTypeCode::IdRefs {
                    // Fallback for IdRefs without parsed list: split lexical text
                    for token in value_str.split_whitespace() {
                        self.pending_idrefs.push((
                            token.to_string(),
                            self.current_location.clone(),
                            self.element_path.clone(),
                        ));
                    }
                } else {
                    // Single IdRef value
                    self.pending_idrefs.push((
                        typed_value.to_string_value(),
                        self.current_location.clone(),
                        self.element_path.clone(),
                    ));
                }
            }
            _ => {}
        }
    }

    /// Validate a single attribute against a complex type's attribute
    /// declarations, wildcards, and fixed-value constraints. Shared by
    /// `validate_attribute` and `validate_deferred_attributes`.
    fn validate_attribute_against_type(
        &mut self,
        ct_key: ComplexTypeKey,
        local_name: NameId,
        namespace: Option<NameId>,
        value: &str,
    ) -> SchemaInfo {
        let ct_data = &self.schema_set.arenas.complex_types[ct_key];
        let found = self.find_attribute_in_type(ct_data, local_name, namespace);

        match found {
            AttributeLookup::Found(attr_key, attr_type, fixed_value) => {
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
                        self.mark_current_invalid();
                    }
                }

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
                            self.report_validation_error(err);
                            attr_validity = SchemaValidity::Invalid;
                            self.mark_current_invalid();
                        }
                    }
                }

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
                    deferred_by_cta: false,
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
                self.mark_current_invalid();
                SchemaInfo::invalid()
            }
            AttributeLookup::NotFound => {
                let ct_data = &self.schema_set.arenas.complex_types[ct_key];
                let effective_wildcard = self.find_effective_wildcard(ct_data);
                if let Some(ref wildcard) = effective_wildcard {
                    let target_ns = ct_data.target_namespace;
                    if self.wildcard_allows_attribute(wildcard, namespace, local_name, target_ns) {
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
                }

                let attr_name = self.schema_set.name_table.resolve(local_name);
                self.report_error(
                    "cvc-complex-type.3.2.2",
                    format!(
                        "Attribute '{}' is not allowed for this element",
                        attr_name
                    ),
                );
                self.mark_current_invalid();
                SchemaInfo::invalid()
            }
        }
    }

    /// Re-validate attributes that were deferred during CTA evaluation.
    ///
    /// Called from `validate_end_of_attributes()` after the type alternative
    /// has been selected.
    #[cfg(feature = "xsd11")]
    fn validate_deferred_attributes(
        &mut self,
        schema_type: Option<TypeKey>,
    ) {
        self.deferred_attribute_results.clear();

        let collected = match self.validation_stack.last_mut() {
            Some(ev) => std::mem::take(&mut ev.collected_attributes),
            None => return,
        };

        let ct_key = match schema_type {
            Some(TypeKey::Complex(k)) => k,
            _ => {
                // Non-complex type (e.g. simple type selected by CTA):
                // no attribute declarations exist, but we must produce one
                // result per collected attribute to keep the 1:1 invariant
                // with the typed builder's deferred_attr_refs.
                self.deferred_attribute_results
                    .resize_with(collected.len(), SchemaInfo::empty);
                return;
            }
        };

        for (namespace, local_name, value) in &collected {
            let info = self.validate_attribute_against_type(ct_key, *local_name, *namespace, value);
            self.deferred_attribute_results.push(info);
        }
    }

    /// Drain deferred attribute validation results collected during CTA processing.
    ///
    /// Returns the `SchemaInfo` results in the same order as the attributes were
    /// originally encountered. The internal buffer is emptied.
    #[cfg(feature = "xsd11")]
    pub fn take_deferred_attribute_results(&mut self) -> Vec<SchemaInfo> {
        std::mem::take(&mut self.deferred_attribute_results)
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

    /// Resolve an xsi:type QName string to an [`XsiTypeOutcome`].
    fn resolve_xsi_type(
        &mut self,
        xsi_type_str: &str,
        declared_type: Option<TypeKey>,
        ns_context: &NamespaceContextSnapshot,
    ) -> XsiTypeOutcome {
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
                return XsiTypeOutcome::Unresolved;
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
                        return XsiTypeOutcome::InvalidDerivation;
                    }
                }
                XsiTypeOutcome::Applied(type_key)
            }
            None => {
                self.report_error(
                    "cvc-elt.4.1",
                    format!(
                        "Type '{}' specified in xsi:type is not declared",
                        xsi_type_str
                    ),
                );
                XsiTypeOutcome::Unresolved
            }
        }
    }

    /// Returns xs:anyType's content model for lax assessment.
    ///
    /// The caller keeps `schema_type = None` (no governing type) — only the
    /// content model (Mixed + `(xs:any processContents=lax)*`) is used.
    fn lax_assessment_content_model(&self) -> (ContentValidatorState, ContentType) {
        let any_type_key = TypeKey::Complex(self.schema_set.any_type_key());
        self.init_content_model(Some(any_type_key))
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
                    deferred_by_cta: false,
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
                    deferred_by_cta: false,
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
        // Positive namespace check
        let ns_ok = match &wildcard.namespace {
            WildcardNamespace::Any => true,
            WildcardNamespace::Other => namespace != target_namespace,
            WildcardNamespace::TargetNamespace => namespace == target_namespace,
            WildcardNamespace::Local => namespace.is_none(),
            WildcardNamespace::List(ns_list) => {
                ns_list.iter().any(|t| t.resolve(target_namespace) == namespace)
            }
        };
        if !ns_ok {
            return false;
        }
        // Check notNamespace exclusions
        for token in &wildcard.not_namespace {
            let excluded_ns = token.resolve(target_namespace);
            if namespace == excluded_ns {
                return false;
            }
        }
        true
    }

    /// Check whether a wildcard allows a given attribute (namespace + notQName).
    fn wildcard_allows_attribute(
        &self,
        wildcard: &WildcardResult,
        namespace: Option<NameId>,
        name: NameId,
        target_namespace: Option<NameId>,
    ) -> bool {
        if !self.wildcard_allows_namespace(wildcard, namespace, target_namespace) {
            return false;
        }
        // Check notQName exclusions
        for item in &wildcard.not_qname {
            match item {
                crate::parser::frames::NotQNameItem::QName { namespace: qns, local_name } => {
                    if *qns == namespace && *local_name == name {
                        return false;
                    }
                }
                crate::parser::frames::NotQNameItem::Defined => {
                    // Reject if this attribute is globally declared
                    if self.schema_set.lookup_attribute(namespace, name).is_some() {
                        return false;
                    }
                }
                crate::parser::frames::NotQNameItem::DefinedSibling => {
                    // Should never appear on attribute wildcards (rejected at parse time)
                }
            }
        }
        true
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
            let (name, namespace) =
                self.resolve_attr_use_name_ns(attr_use, resolved, group_data.target_namespace);
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
    ///
    /// `fallback_namespace` is used when the attribute's source document is
    /// unavailable (e.g. synthesized attributes); callers should pass the
    /// containing type's or group's target namespace.
    fn resolve_attr_use_name_ns(
        &self,
        attr_use: &AttributeUseResult,
        resolved: Option<&ResolvedAttributeUse>,
        fallback_namespace: Option<NameId>,
    ) -> (NameId, Option<NameId>) {
        if let Some(name) = attr_use.attribute.name {
            if attr_use.attribute.ref_name.is_none() {
                // Inline local attribute: apply form / attributeFormDefault
                let ns = self.schema_set.effective_local_attribute_namespace(
                    attr_use.attribute.target_namespace,
                    attr_use.attribute.form.as_deref(),
                    attr_use.attribute.source.as_ref(),
                    fallback_namespace,
                );
                return (name, ns);
            }
            // ref with name — fall through to ref resolution
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
        // Check the content's attribute wildcard (e.g. xs:anyType stores it
        // inside ComplexContentDefResult, not at the top-level).
        if let crate::parser::frames::ComplexContentResult::Complex(ref def) = ct_data.content {
            if def.attribute_wildcard.is_some() {
                return def.attribute_wildcard.clone();
            }
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
            let (attr_name, attr_ns) =
                self.resolve_attr_use_name_ns(attr_use, resolved, ct_data.target_namespace);

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
            let (attr_name, attr_ns) =
                self.resolve_attr_use_name_ns(attr_use, resolved, ct_data.target_namespace);

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
    use super::super::validator::SchemaValidator;
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

    #[cfg(feature = "xsd11")]
    fn load_schema_xsd11(xsd: &str) -> SchemaSet {
        let mut schema_set = SchemaSet::xsd11();
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

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
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

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
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

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
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

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
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

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
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

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
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

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
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

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());

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

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
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

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
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

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
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

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
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

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
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

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
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
            let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
            let mut v = validator.start_run(TestSink::new());
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
            let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
            let mut v = validator.start_run(TestSink::new());
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

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
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

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
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

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
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

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
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

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
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

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
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

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
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

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
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

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
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

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
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

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
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

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
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

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
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

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
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

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
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

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
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

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
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

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
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

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
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

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
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

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
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

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
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

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
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

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
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

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
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

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
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

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
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

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
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

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
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

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
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

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
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

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
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

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
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

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
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

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
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

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
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

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
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

    #[cfg(feature = "xsd11")]
    mod assertion_runtime_tests {
        use super::*;

        #[test]
        fn test_disabled_mode_no_overhead() {
            let schema_set = load_schema(
                r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                    <xs:element name="root" type="xs:string"/>
                </xs:schema>"#,
            );
            let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
            let mut v = validator.start_run(TestSink::new());
            assert_eq!(v.assertion_source, AssertionSource::Disabled);

            let ns = empty_ns_context();
            let info = v.validate_element("root", "", None, None, &ns);
            assert_eq!(info.validity, SchemaValidity::Valid);
            v.validate_end_of_attributes();
            v.validate_text("hello");
            let end_info = v.validate_end_element();
            assert_eq!(end_info.validity, SchemaValidity::Valid);
            v.end_validation().ok();

            assert!(
                v.sink.errors.is_empty(),
                "Expected no errors in Disabled mode, got: {:?}",
                v.sink.errors
            );
        }

        #[test]
        fn test_new_strips_process_assertions_flag() {
            // SchemaValidator::new() silently strips PROCESS_ASSERTIONS,
            // preventing the flag/source mismatch that would panic at runtime.
            let schema_set = load_schema(
                r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                    <xs:element name="root" type="xs:string"/>
                </xs:schema>"#,
            );
            let flags = ValidationFlags::default() | ValidationFlags::PROCESS_ASSERTIONS;
            let validator = SchemaValidator::new(&schema_set, flags);
            assert!(!validator.flags.contains(ValidationFlags::PROCESS_ASSERTIONS));
            // Validation proceeds without panic
            let mut v = validator.start_run(TestSink::new());
            let ns = empty_ns_context();
            let info = v.validate_element("root", "", None, None, &ns);
            assert_eq!(info.validity, SchemaValidity::Valid);
        }

        #[test]
        fn test_main_document_full_roundtrip() {
            let schema_set = load_schema(
                r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                    <xs:element name="root" type="xs:string"/>
                </xs:schema>"#,
            );
            let mut validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
            validator.set_assertion_source(AssertionSource::MainDocument);
            let mut v = validator.start_run(TestSink::new());
            assert_eq!(v.assertion_source, AssertionSource::MainDocument);

            let ns = empty_ns_context();
            let info = v.validate_element("root", "", None, None, &ns);
            assert_eq!(info.validity, SchemaValidity::Valid);
            v.validate_end_of_attributes();
            v.validate_text("hello");
            let end_info = v.validate_end_element();
            assert_eq!(end_info.validity, SchemaValidity::Valid);
            v.end_validation().ok();

            assert!(
                v.sink.errors.is_empty(),
                "Expected no errors in MainDocument mode, got: {:?}",
                v.sink.errors
            );
        }

        // ── Complex-type assertion behavior tests ───────────────────────

        /// Helper: validate a full element lifecycle via fragment buffer mode.
        fn validate_with_fragment_buffer(
            xsd: &str,
            element: &str,
            attrs: &[(&str, &str)],
            text: Option<&str>,
        ) -> Vec<ValidationError> {
            let schema_set = load_schema_xsd11(xsd);
            let validator = SchemaValidator::new_fragment_buffer(
                &schema_set,
                ValidationFlags::default(),
            );
            let mut v = validator.start_run(TestSink::new());
            let ns = empty_ns_context();
            v.validate_element(element, "", None, None, &ns);
            for &(name, value) in attrs {
                v.validate_attribute(name, "", value);
            }
            v.validate_end_of_attributes();
            if let Some(t) = text {
                v.validate_text(t);
            }
            v.validate_end_element();
            v.end_validation().ok();
            v.sink.errors
        }

        #[test]
        fn test_assertion_pass() {
            // xs:assert on inline complexType — assertion passes
            let errors = validate_with_fragment_buffer(
                r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                    <xs:element name="item">
                        <xs:complexType>
                            <xs:attribute name="val" type="xs:integer"/>
                            <xs:assert test="@val >= 0"/>
                        </xs:complexType>
                    </xs:element>
                </xs:schema>"#,
                "item",
                &[("val", "25")],
                None,
            );
            assert!(
                errors.is_empty(),
                "Expected no assertion errors, got: {:?}",
                errors
            );
        }

        #[test]
        fn test_assertion_fail() {
            // xs:assert on inline complexType — assertion fails
            let errors = validate_with_fragment_buffer(
                r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                    <xs:element name="item">
                        <xs:complexType>
                            <xs:attribute name="val" type="xs:integer"/>
                            <xs:assert test="@val >= 0"/>
                        </xs:complexType>
                    </xs:element>
                </xs:schema>"#,
                "item",
                &[("val", "-5")],
                None,
            );
            let has_assertion_error = errors
                .iter()
                .any(|e| e.constraint == "cvc-assertion");
            assert!(
                has_assertion_error,
                "Expected cvc-assertion error for negative @val, got: {:?}",
                errors
            );
        }

        #[test]
        fn test_assertion_multiple_one_fails() {
            // Two assertions on same type: first passes, second fails
            let errors = validate_with_fragment_buffer(
                r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                    <xs:element name="item">
                        <xs:complexType>
                            <xs:attribute name="val" type="xs:integer"/>
                            <xs:assert test="@val >= 0"/>
                            <xs:assert test="@val &lt; 100"/>
                        </xs:complexType>
                    </xs:element>
                </xs:schema>"#,
                "item",
                &[("val", "150")],
                None,
            );
            // Value 150 passes "@val >= 0" but fails "@val < 100"
            let assertion_errors: Vec<_> = errors
                .iter()
                .filter(|e| e.constraint == "cvc-assertion")
                .collect();
            assert_eq!(
                assertion_errors.len(),
                1,
                "Expected exactly 1 assertion failure, got: {:?}",
                assertion_errors
            );
        }

        #[test]
        fn test_no_assertion_type_no_buffering_overhead() {
            // A type without assertions should not trigger buffering at all,
            // even in FragmentBuffer mode.
            let errors = validate_with_fragment_buffer(
                r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                    <xs:element name="plain" type="xs:string"/>
                </xs:schema>"#,
                "plain",
                &[],
                Some("hello"),
            );
            assert!(
                errors.is_empty(),
                "No assertion type should produce no errors, got: {:?}",
                errors
            );
        }

        #[test]
        fn test_assertion_attribute_check() {
            // Assertion checking string-length of attribute
            let errors = validate_with_fragment_buffer(
                r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                    <xs:element name="item">
                        <xs:complexType>
                            <xs:attribute name="code" type="xs:string" use="required"/>
                            <xs:assert test="string-length(@code) > 0"/>
                        </xs:complexType>
                    </xs:element>
                </xs:schema>"#,
                "item",
                &[("code", "ABC")],
                None,
            );
            assert!(
                errors.is_empty(),
                "Assertion on non-empty @code should pass, got: {:?}",
                errors
            );
        }

        #[test]
        fn test_assertion_on_element_content() {
            // Assertion using element-only content with child elements
            let schema_set = load_schema_xsd11(
                r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                    <xs:element name="order">
                        <xs:complexType>
                            <xs:sequence>
                                <xs:element name="qty" type="xs:integer"/>
                            </xs:sequence>
                            <xs:assert test="qty > 0"/>
                        </xs:complexType>
                    </xs:element>
                </xs:schema>"#,
            );
            let validator = SchemaValidator::new_fragment_buffer(
                &schema_set,
                ValidationFlags::default(),
            );
            let mut v = validator.start_run(TestSink::new());
            let ns = empty_ns_context();

            // <order><qty>5</qty></order>
            v.validate_element("order", "", None, None, &ns);
            v.validate_end_of_attributes();
            v.validate_element("qty", "", None, None, &ns);
            v.validate_end_of_attributes();
            v.validate_text("5");
            v.validate_end_element(); // </qty>
            v.validate_end_element(); // </order>
            v.end_validation().ok();

            let assertion_errors: Vec<_> = v
                .sink
                .errors
                .iter()
                .filter(|e| e.constraint == "cvc-assertion")
                .collect();
            assert!(
                assertion_errors.is_empty(),
                "qty=5 should pass qty > 0 assertion, got: {:?}",
                assertion_errors
            );
        }

        // ── Assertion on element content — failure ──────────────────────

        #[test]
        fn test_assertion_on_element_content_fail() {
            // qty=0 violates qty > 0
            let schema_set = load_schema_xsd11(
                r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                    <xs:element name="order">
                        <xs:complexType>
                            <xs:sequence>
                                <xs:element name="qty" type="xs:integer"/>
                            </xs:sequence>
                            <xs:assert test="qty > 0"/>
                        </xs:complexType>
                    </xs:element>
                </xs:schema>"#,
            );
            let validator = SchemaValidator::new_fragment_buffer(
                &schema_set,
                ValidationFlags::default(),
            );
            let mut v = validator.start_run(TestSink::new());
            let ns = empty_ns_context();

            // <order><qty>0</qty></order>
            v.validate_element("order", "", None, None, &ns);
            v.validate_end_of_attributes();
            v.validate_element("qty", "", None, None, &ns);
            v.validate_end_of_attributes();
            v.validate_text("0");
            v.validate_end_element();
            v.validate_end_element();
            v.end_validation().ok();

            let assertion_errors: Vec<_> = v
                .sink
                .errors
                .iter()
                .filter(|e| e.constraint == "cvc-assertion")
                .collect();
            assert_eq!(
                assertion_errors.len(),
                1,
                "qty=0 should fail qty > 0, got: {:?}",
                v.sink.errors
            );
        }

        // ── Inherited assertions: base assertion evaluated on derived type ──

        #[test]
        fn test_inherited_assertion_pass() {
            // Base type has assertion @val >= 0; derived type restricts further.
            // Value 50 satisfies both base (@val >= 0) and derived (@val < 100).
            let schema_set = load_schema_xsd11(
                r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                    <xs:complexType name="baseType">
                        <xs:attribute name="val" type="xs:integer"/>
                        <xs:assert test="@val >= 0"/>
                    </xs:complexType>
                    <xs:complexType name="derivedType">
                        <xs:complexContent>
                            <xs:restriction base="baseType">
                                <xs:attribute name="val" type="xs:integer"/>
                                <xs:assert test="@val &lt; 100"/>
                            </xs:restriction>
                        </xs:complexContent>
                    </xs:complexType>
                    <xs:element name="item" type="derivedType"/>
                </xs:schema>"#,
            );
            let validator = SchemaValidator::new_fragment_buffer(
                &schema_set,
                ValidationFlags::default(),
            );
            let mut v = validator.start_run(TestSink::new());
            let ns = empty_ns_context();

            v.validate_element("item", "", None, None, &ns);
            v.validate_attribute("val", "", "50");
            v.validate_end_of_attributes();
            v.validate_end_element();
            v.end_validation().ok();

            let assertion_errors: Vec<_> = v
                .sink
                .errors
                .iter()
                .filter(|e| e.constraint == "cvc-assertion")
                .collect();
            assert!(
                assertion_errors.is_empty(),
                "val=50 should satisfy both base and derived assertions, got: {:?}",
                assertion_errors
            );
        }

        #[test]
        fn test_inherited_assertion_base_fails() {
            // Value -5 fails the base assertion @val >= 0
            let schema_set = load_schema_xsd11(
                r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                    <xs:complexType name="baseType">
                        <xs:attribute name="val" type="xs:integer"/>
                        <xs:assert test="@val >= 0"/>
                    </xs:complexType>
                    <xs:complexType name="derivedType">
                        <xs:complexContent>
                            <xs:restriction base="baseType">
                                <xs:attribute name="val" type="xs:integer"/>
                                <xs:assert test="@val &lt; 100"/>
                            </xs:restriction>
                        </xs:complexContent>
                    </xs:complexType>
                    <xs:element name="item" type="derivedType"/>
                </xs:schema>"#,
            );
            let validator = SchemaValidator::new_fragment_buffer(
                &schema_set,
                ValidationFlags::default(),
            );
            let mut v = validator.start_run(TestSink::new());
            let ns = empty_ns_context();

            v.validate_element("item", "", None, None, &ns);
            v.validate_attribute("val", "", "-5");
            v.validate_end_of_attributes();
            v.validate_end_element();
            v.end_validation().ok();

            let assertion_errors: Vec<_> = v
                .sink
                .errors
                .iter()
                .filter(|e| e.constraint == "cvc-assertion")
                .collect();
            assert!(
                !assertion_errors.is_empty(),
                "val=-5 should fail inherited @val >= 0 assertion"
            );
        }

        #[test]
        fn test_inherited_assertion_derived_fails() {
            // Value 200 passes base (@val >= 0) but fails derived (@val < 100)
            let schema_set = load_schema_xsd11(
                r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                    <xs:complexType name="baseType">
                        <xs:attribute name="val" type="xs:integer"/>
                        <xs:assert test="@val >= 0"/>
                    </xs:complexType>
                    <xs:complexType name="derivedType">
                        <xs:complexContent>
                            <xs:restriction base="baseType">
                                <xs:attribute name="val" type="xs:integer"/>
                                <xs:assert test="@val &lt; 100"/>
                            </xs:restriction>
                        </xs:complexContent>
                    </xs:complexType>
                    <xs:element name="item" type="derivedType"/>
                </xs:schema>"#,
            );
            let validator = SchemaValidator::new_fragment_buffer(
                &schema_set,
                ValidationFlags::default(),
            );
            let mut v = validator.start_run(TestSink::new());
            let ns = empty_ns_context();

            v.validate_element("item", "", None, None, &ns);
            v.validate_attribute("val", "", "200");
            v.validate_end_of_attributes();
            v.validate_end_element();
            v.end_validation().ok();

            let assertion_errors: Vec<_> = v
                .sink
                .errors
                .iter()
                .filter(|e| e.constraint == "cvc-assertion")
                .collect();
            assert_eq!(
                assertion_errors.len(),
                1,
                "val=200 should fail only derived @val < 100, got: {:?}",
                assertion_errors
            );
        }

        #[test]
        fn test_inherited_assertion_both_fail() {
            // Value -200 fails both base (@val >= 0) and derived (@val < 100)
            // (well, -200 < 100 passes, so use @val > 10 for derived instead)
            let schema_set = load_schema_xsd11(
                r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                    <xs:complexType name="baseType">
                        <xs:attribute name="val" type="xs:integer"/>
                        <xs:assert test="@val >= 0"/>
                    </xs:complexType>
                    <xs:complexType name="derivedType">
                        <xs:complexContent>
                            <xs:restriction base="baseType">
                                <xs:attribute name="val" type="xs:integer"/>
                                <xs:assert test="@val > 10"/>
                            </xs:restriction>
                        </xs:complexContent>
                    </xs:complexType>
                    <xs:element name="item" type="derivedType"/>
                </xs:schema>"#,
            );
            let validator = SchemaValidator::new_fragment_buffer(
                &schema_set,
                ValidationFlags::default(),
            );
            let mut v = validator.start_run(TestSink::new());
            let ns = empty_ns_context();

            // val=-5: fails base (>= 0) and fails derived (> 10)
            v.validate_element("item", "", None, None, &ns);
            v.validate_attribute("val", "", "-5");
            v.validate_end_of_attributes();
            v.validate_end_element();
            v.end_validation().ok();

            let assertion_errors: Vec<_> = v
                .sink
                .errors
                .iter()
                .filter(|e| e.constraint == "cvc-assertion")
                .collect();
            assert_eq!(
                assertion_errors.len(),
                2,
                "val=-5 should fail both inherited assertions, got: {:?}",
                assertion_errors
            );
        }

        // ── Nested element with its own assertions ──────────────────────

        #[test]
        fn test_nested_element_assertions() {
            // Parent and child both have assertions; both should be evaluated
            let schema_set = load_schema_xsd11(
                r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                    <xs:element name="parent">
                        <xs:complexType>
                            <xs:sequence>
                                <xs:element name="child">
                                    <xs:complexType>
                                        <xs:attribute name="x" type="xs:integer"/>
                                        <xs:assert test="@x > 0"/>
                                    </xs:complexType>
                                </xs:element>
                            </xs:sequence>
                            <xs:attribute name="total" type="xs:integer"/>
                            <xs:assert test="@total >= 0"/>
                        </xs:complexType>
                    </xs:element>
                </xs:schema>"#,
            );
            let validator = SchemaValidator::new_fragment_buffer(
                &schema_set,
                ValidationFlags::default(),
            );
            let mut v = validator.start_run(TestSink::new());
            let ns = empty_ns_context();

            // <parent total="10"><child x="5"/></parent> — both pass
            v.validate_element("parent", "", None, None, &ns);
            v.validate_attribute("total", "", "10");
            v.validate_end_of_attributes();

            v.validate_element("child", "", None, None, &ns);
            v.validate_attribute("x", "", "5");
            v.validate_end_of_attributes();
            v.validate_end_element(); // </child>

            v.validate_end_element(); // </parent>
            v.end_validation().ok();

            let assertion_errors: Vec<_> = v
                .sink
                .errors
                .iter()
                .filter(|e| e.constraint == "cvc-assertion")
                .collect();
            assert!(
                assertion_errors.is_empty(),
                "Both assertions should pass, got: {:?}",
                assertion_errors
            );
        }

        #[test]
        fn test_nested_element_child_assertion_fails() {
            // Parent assertion passes, child assertion fails
            let schema_set = load_schema_xsd11(
                r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                    <xs:element name="parent">
                        <xs:complexType>
                            <xs:sequence>
                                <xs:element name="child">
                                    <xs:complexType>
                                        <xs:attribute name="x" type="xs:integer"/>
                                        <xs:assert test="@x > 0"/>
                                    </xs:complexType>
                                </xs:element>
                            </xs:sequence>
                            <xs:attribute name="total" type="xs:integer"/>
                            <xs:assert test="@total >= 0"/>
                        </xs:complexType>
                    </xs:element>
                </xs:schema>"#,
            );
            let validator = SchemaValidator::new_fragment_buffer(
                &schema_set,
                ValidationFlags::default(),
            );
            let mut v = validator.start_run(TestSink::new());
            let ns = empty_ns_context();

            // <parent total="10"><child x="-1"/></parent>
            v.validate_element("parent", "", None, None, &ns);
            v.validate_attribute("total", "", "10");
            v.validate_end_of_attributes();

            v.validate_element("child", "", None, None, &ns);
            v.validate_attribute("x", "", "-1");
            v.validate_end_of_attributes();
            v.validate_end_element(); // </child>

            v.validate_end_element(); // </parent>
            v.end_validation().ok();

            let assertion_errors: Vec<_> = v
                .sink
                .errors
                .iter()
                .filter(|e| e.constraint == "cvc-assertion")
                .collect();
            assert_eq!(
                assertion_errors.len(),
                1,
                "Only child assertion should fail, got: {:?}",
                assertion_errors
            );
        }

        // ── Named complex type with assertions ──────────────────────────

        #[test]
        fn test_named_type_assertion_pass() {
            // Global element references named type with assertion
            let errors = validate_with_fragment_buffer(
                r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                    <xs:complexType name="positiveType">
                        <xs:attribute name="val" type="xs:integer"/>
                        <xs:assert test="@val > 0"/>
                    </xs:complexType>
                    <xs:element name="item" type="positiveType"/>
                </xs:schema>"#,
                "item",
                &[("val", "42")],
                None,
            );
            assert!(
                errors.is_empty(),
                "Named type assertion should pass for val=42, got: {:?}",
                errors
            );
        }

        #[test]
        fn test_named_type_assertion_fail() {
            let errors = validate_with_fragment_buffer(
                r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                    <xs:complexType name="positiveType">
                        <xs:attribute name="val" type="xs:integer"/>
                        <xs:assert test="@val > 0"/>
                    </xs:complexType>
                    <xs:element name="item" type="positiveType"/>
                </xs:schema>"#,
                "item",
                &[("val", "-1")],
                None,
            );
            let has_assertion_error = errors
                .iter()
                .any(|e| e.constraint == "cvc-assertion");
            assert!(
                has_assertion_error,
                "Named type assertion should fail for val=-1, got: {:?}",
                errors
            );
        }

        // ── Assertion with child element content on named type ──────────

        #[test]
        fn test_named_type_child_element_assertion() {
            // Named type with sequence + assertion referencing child element
            let schema_set = load_schema_xsd11(
                r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                    <xs:complexType name="orderType">
                        <xs:sequence>
                            <xs:element name="qty" type="xs:integer"/>
                            <xs:element name="price" type="xs:decimal"/>
                        </xs:sequence>
                        <xs:assert test="qty > 0 and price > 0"/>
                    </xs:complexType>
                    <xs:element name="order" type="orderType"/>
                </xs:schema>"#,
            );
            let validator = SchemaValidator::new_fragment_buffer(
                &schema_set,
                ValidationFlags::default(),
            );
            let mut v = validator.start_run(TestSink::new());
            let ns = empty_ns_context();

            // <order><qty>3</qty><price>9.99</price></order>
            v.validate_element("order", "", None, None, &ns);
            v.validate_end_of_attributes();
            v.validate_element("qty", "", None, None, &ns);
            v.validate_end_of_attributes();
            v.validate_text("3");
            v.validate_end_element();
            v.validate_element("price", "", None, None, &ns);
            v.validate_end_of_attributes();
            v.validate_text("9.99");
            v.validate_end_element();
            v.validate_end_element();
            v.end_validation().ok();

            let assertion_errors: Vec<_> = v
                .sink
                .errors
                .iter()
                .filter(|e| e.constraint == "cvc-assertion")
                .collect();
            assert!(
                assertion_errors.is_empty(),
                "qty=3, price=9.99 should pass assertion, got: {:?}",
                assertion_errors
            );
        }

        #[test]
        fn test_named_type_child_element_assertion_fail() {
            let schema_set = load_schema_xsd11(
                r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                    <xs:complexType name="orderType">
                        <xs:sequence>
                            <xs:element name="qty" type="xs:integer"/>
                            <xs:element name="price" type="xs:decimal"/>
                        </xs:sequence>
                        <xs:assert test="qty > 0 and price > 0"/>
                    </xs:complexType>
                    <xs:element name="order" type="orderType"/>
                </xs:schema>"#,
            );
            let validator = SchemaValidator::new_fragment_buffer(
                &schema_set,
                ValidationFlags::default(),
            );
            let mut v = validator.start_run(TestSink::new());
            let ns = empty_ns_context();

            // <order><qty>0</qty><price>9.99</price></order> — qty=0 fails
            v.validate_element("order", "", None, None, &ns);
            v.validate_end_of_attributes();
            v.validate_element("qty", "", None, None, &ns);
            v.validate_end_of_attributes();
            v.validate_text("0");
            v.validate_end_element();
            v.validate_element("price", "", None, None, &ns);
            v.validate_end_of_attributes();
            v.validate_text("9.99");
            v.validate_end_element();
            v.validate_end_element();
            v.end_validation().ok();

            let assertion_errors: Vec<_> = v
                .sink
                .errors
                .iter()
                .filter(|e| e.constraint == "cvc-assertion")
                .collect();
            assert_eq!(
                assertion_errors.len(),
                1,
                "qty=0 should fail 'qty > 0 and price > 0', got: {:?}",
                assertion_errors
            );
        }

        // ── xpathDefaultNamespace on assertion ──────────────────────────

        #[test]
        fn test_assertion_xpath_default_namespace() {
            // Schema with target namespace; assertion uses
            // xpathDefaultNamespace="##targetNamespace" so unqualified
            // element steps match the target namespace.
            let schema_set = load_schema_xsd11(
                r###"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
                            targetNamespace="http://example.com/ns"
                            xmlns:tns="http://example.com/ns"
                            elementFormDefault="qualified">
                    <xs:element name="order">
                        <xs:complexType>
                            <xs:sequence>
                                <xs:element name="qty" type="xs:integer"/>
                            </xs:sequence>
                            <xs:assert test="qty > 0"
                                       xpathDefaultNamespace="##targetNamespace"/>
                        </xs:complexType>
                    </xs:element>
                </xs:schema>"###,
            );
            let validator = SchemaValidator::new_fragment_buffer(
                &schema_set,
                ValidationFlags::default(),
            );
            let mut v = validator.start_run(TestSink::new());
            let ns = empty_ns_context();
            let tns = "http://example.com/ns";

            // <tns:order xmlns:tns="..."><tns:qty>5</tns:qty></tns:order>
            v.validate_element("order", tns, None, None, &ns);
            v.validate_end_of_attributes();
            v.validate_element("qty", tns, None, None, &ns);
            v.validate_end_of_attributes();
            v.validate_text("5");
            v.validate_end_element();
            v.validate_end_element();
            v.end_validation().ok();

            let assertion_errors: Vec<_> = v
                .sink
                .errors
                .iter()
                .filter(|e| e.constraint == "cvc-assertion")
                .collect();
            assert!(
                assertion_errors.is_empty(),
                "xpathDefaultNamespace=##targetNamespace should allow unqualified 'qty' to match, got: {:?}",
                assertion_errors
            );
        }

        // ── Extension-derived type inherits base assertions ─────────────

        #[test]
        fn test_extension_inherits_base_assertion() {
            let schema_set = load_schema_xsd11(
                r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                    <xs:complexType name="baseType">
                        <xs:sequence>
                            <xs:element name="name" type="xs:string"/>
                        </xs:sequence>
                        <xs:assert test="string-length(name) > 0"/>
                    </xs:complexType>
                    <xs:complexType name="extType">
                        <xs:complexContent>
                            <xs:extension base="baseType">
                                <xs:sequence>
                                    <xs:element name="extra" type="xs:string"/>
                                </xs:sequence>
                            </xs:extension>
                        </xs:complexContent>
                    </xs:complexType>
                    <xs:element name="item" type="extType"/>
                </xs:schema>"#,
            );
            let validator = SchemaValidator::new_fragment_buffer(
                &schema_set,
                ValidationFlags::default(),
            );
            let mut v = validator.start_run(TestSink::new());
            let ns = empty_ns_context();

            // <item><name>hello</name><extra>world</extra></item>
            v.validate_element("item", "", None, None, &ns);
            v.validate_end_of_attributes();
            v.validate_element("name", "", None, None, &ns);
            v.validate_end_of_attributes();
            v.validate_text("hello");
            v.validate_end_element();
            v.validate_element("extra", "", None, None, &ns);
            v.validate_end_of_attributes();
            v.validate_text("world");
            v.validate_end_element();
            v.validate_end_element();
            v.end_validation().ok();

            let assertion_errors: Vec<_> = v
                .sink
                .errors
                .iter()
                .filter(|e| e.constraint == "cvc-assertion")
                .collect();
            assert!(
                assertion_errors.is_empty(),
                "Extension type should inherit and pass base assertion, got: {:?}",
                assertion_errors
            );
        }
    }

    // ── Fragment arena lifecycle tests ────────────────────────────────

    #[cfg(feature = "xsd11")]
    mod fragment_arena_tests {
        use super::*;

        #[test]
        fn fragment_arena_lifecycle() {
            let schema_set = load_schema(
                r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                    <xs:element name="root" type="xs:string"/>
                </xs:schema>"#,
            );
            let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
            let mut v = validator.start_run(TestSink::new());

            // Initially None
            assert!(v.fragment_arena().is_none());

            // Lazy allocation via fragment_arena_mut()
            let _arena = v.fragment_arena_mut();

            // Now Some
            assert!(v.fragment_arena().is_some());
        }

        #[test]
        fn fragment_arena_allocate_and_drop() {
            let schema_set = load_schema(
                r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                    <xs:element name="root" type="xs:string"/>
                </xs:schema>"#,
            );
            let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
            let mut v = validator.start_run(TestSink::new());

            // Allocate something into the arena
            let arena = v.fragment_arena_mut();
            let _s = arena.alloc_str("hello fragment");

            // Drop validator — arena drops cleanly (Miri-safe)
            drop(v);
        }
    }

    /// Test: global element with named complex type reference (type="itemType")
    #[test]
    fn test_global_element_with_named_complex_type_ref() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:complexType name="itemType">
                    <xs:sequence>
                        <xs:element name="name" type="xs:string"/>
                        <xs:element name="value" type="xs:integer"/>
                    </xs:sequence>
                </xs:complexType>
                <xs:element name="item" type="itemType"/>
            </xs:schema>"#,
        );

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
        let ns = empty_ns_context();

        // Open root element "item" (global, type="itemType")
        let info = v.validate_element("item", "", None, None, &ns);
        assert_eq!(info.validity, SchemaValidity::Valid, "item should be valid");
        assert!(info.schema_type.is_some(), "item should have a schema type");

        v.validate_end_of_attributes();

        // Child "name"
        let name_info = v.validate_element("name", "", None, None, &ns);
        assert_eq!(name_info.validity, SchemaValidity::Valid, "name should be valid");
        v.validate_end_of_attributes();
        v.validate_text("Widget");
        v.validate_end_element();

        // Child "value"
        let value_info = v.validate_element("value", "", None, None, &ns);
        assert_eq!(value_info.validity, SchemaValidity::Valid, "value should be valid");
        v.validate_end_of_attributes();
        v.validate_text("42");
        v.validate_end_element();

        // Close root
        v.validate_end_element();
        assert!(v.end_validation().is_ok());
        assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
    }

    #[cfg(feature = "xsd11")]
    mod type_alternatives_tests {
        use super::*;

        /// Helper: run a full validation pass and return the collected errors.
        fn validate_errors(schema_set: &SchemaSet, run: impl FnOnce(&mut ValidationRuntime<'_, TestSink>)) -> Vec<ValidationError> {
            let validator = SchemaValidator::new(schema_set, ValidationFlags::default());
            let mut v = validator.start_run(TestSink::new());
            run(&mut v);
            v.end_validation().ok();
            v.sink.errors
        }

        const ALTERNATIVES_SCHEMA: &str = r#"
            <xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:complexType name="intContent">
                    <xs:sequence>
                        <xs:element name="val" type="xs:integer"/>
                    </xs:sequence>
                    <xs:attribute name="kind" type="xs:string"/>
                </xs:complexType>
                <xs:complexType name="strContent">
                    <xs:sequence>
                        <xs:element name="val" type="xs:string"/>
                    </xs:sequence>
                    <xs:attribute name="kind" type="xs:string"/>
                </xs:complexType>
                <xs:element name="data">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="val" type="xs:string"/>
                        </xs:sequence>
                        <xs:attribute name="kind" type="xs:string"/>
                    </xs:complexType>
                    <xs:alternative test="@kind='int'" type="intContent"/>
                    <xs:alternative test="@kind='str'" type="strContent"/>
                </xs:element>
            </xs:schema>"#;

        #[test]
        fn test_alternative_selects_int_type() {
            let schema_set = load_schema_xsd11(ALTERNATIVES_SCHEMA);
            let ns = empty_ns_context();
            let errors = validate_errors(&schema_set, |v| {
                v.validate_element("data", "", None, None, &ns);
                v.validate_attribute("kind", "", "int");
                v.validate_end_of_attributes();

                v.validate_element("val", "", None, None, &ns);
                v.validate_end_of_attributes();
                v.validate_text("42");
                v.validate_end_element();

                v.validate_end_element();
            });
            assert!(errors.is_empty(), "Expected no errors, got: {:?}", errors);
        }

        #[test]
        fn test_alternative_selects_str_type() {
            let schema_set = load_schema_xsd11(ALTERNATIVES_SCHEMA);
            let ns = empty_ns_context();
            let errors = validate_errors(&schema_set, |v| {
                v.validate_element("data", "", None, None, &ns);
                v.validate_attribute("kind", "", "str");
                v.validate_end_of_attributes();

                v.validate_element("val", "", None, None, &ns);
                v.validate_end_of_attributes();
                v.validate_text("hello");
                v.validate_end_element();

                v.validate_end_element();
            });
            assert!(errors.is_empty(), "Expected no errors, got: {:?}", errors);
        }

        #[test]
        fn test_alternative_int_rejects_non_integer() {
            let schema_set = load_schema_xsd11(ALTERNATIVES_SCHEMA);
            let ns = empty_ns_context();
            let errors = validate_errors(&schema_set, |v| {
                v.validate_element("data", "", None, None, &ns);
                v.validate_attribute("kind", "", "int");
                v.validate_end_of_attributes();

                v.validate_element("val", "", None, None, &ns);
                v.validate_end_of_attributes();
                // "hello" is not a valid integer
                v.validate_text("hello");
                v.validate_end_element();

                v.validate_end_element();
            });
            assert!(!errors.is_empty(), "Expected validation error for non-integer value");
        }

        #[test]
        fn test_no_matching_alternative_uses_declared_type() {
            let schema_set = load_schema_xsd11(ALTERNATIVES_SCHEMA);
            let ns = empty_ns_context();
            // kind='other' doesn't match any alternative — use element's declared type
            let errors = validate_errors(&schema_set, |v| {
                v.validate_element("data", "", None, None, &ns);
                v.validate_attribute("kind", "", "other");
                v.validate_end_of_attributes();

                // Declared type has <val> as xs:string
                v.validate_element("val", "", None, None, &ns);
                v.validate_end_of_attributes();
                v.validate_text("anything");
                v.validate_end_element();

                v.validate_end_element();
            });
            assert!(errors.is_empty(), "Expected no errors with declared type, got: {:?}", errors);
        }

        #[test]
        fn test_alternative_with_default_fallback() {
            let schema_set = load_schema_xsd11(
                r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                    <xs:complexType name="specialType">
                        <xs:sequence>
                            <xs:element name="s" type="xs:integer"/>
                        </xs:sequence>
                        <xs:attribute name="mode" type="xs:string"/>
                    </xs:complexType>
                    <xs:complexType name="defaultType">
                        <xs:sequence>
                            <xs:element name="d" type="xs:string"/>
                        </xs:sequence>
                        <xs:attribute name="mode" type="xs:string"/>
                    </xs:complexType>
                    <xs:element name="item">
                        <xs:complexType>
                            <xs:sequence>
                                <xs:element name="x" type="xs:string"/>
                            </xs:sequence>
                            <xs:attribute name="mode" type="xs:string"/>
                        </xs:complexType>
                        <xs:alternative test="@mode='special'" type="specialType"/>
                        <xs:alternative type="defaultType"/>
                    </xs:element>
                </xs:schema>"#,
            );
            let ns = empty_ns_context();

            // mode='special' -> specialType (expects integer child)
            let errors_special = validate_errors(&schema_set, |v| {
                v.validate_element("item", "", None, None, &ns);
                v.validate_attribute("mode", "", "special");
                v.validate_end_of_attributes();
                v.validate_element("s", "", None, None, &ns);
                v.validate_end_of_attributes();
                v.validate_text("42");
                v.validate_end_element();
                v.validate_end_element();
            });
            assert!(errors_special.is_empty(), "Expected no errors for special mode, got: {:?}", errors_special);

            // mode='other' -> defaultType (expects string child "d")
            let errors_default = validate_errors(&schema_set, |v| {
                v.validate_element("item", "", None, None, &ns);
                v.validate_attribute("mode", "", "other");
                v.validate_end_of_attributes();
                v.validate_element("d", "", None, None, &ns);
                v.validate_end_of_attributes();
                v.validate_text("hello");
                v.validate_end_element();
                v.validate_end_element();
            });
            assert!(errors_default.is_empty(), "Expected no errors for default mode, got: {:?}", errors_default);
        }

        #[test]
        fn test_alternative_wrong_child_for_selected_type() {
            let schema_set = load_schema_xsd11(
                r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                    <xs:complexType name="typeA">
                        <xs:sequence>
                            <xs:element name="a" type="xs:string"/>
                        </xs:sequence>
                        <xs:attribute name="kind" type="xs:string"/>
                    </xs:complexType>
                    <xs:element name="root">
                        <xs:complexType>
                            <xs:sequence>
                                <xs:element name="x" type="xs:string"/>
                            </xs:sequence>
                            <xs:attribute name="kind" type="xs:string"/>
                        </xs:complexType>
                        <xs:alternative test="@kind='A'" type="typeA"/>
                    </xs:element>
                </xs:schema>"#,
            );
            let ns = empty_ns_context();

            // kind='A' selects typeA which expects child "a", but we provide "x"
            let errors = validate_errors(&schema_set, |v| {
                v.validate_element("root", "", None, None, &ns);
                v.validate_attribute("kind", "", "A");
                v.validate_end_of_attributes();
                v.validate_element("x", "", None, None, &ns);
                v.validate_end_of_attributes();
                v.validate_text("hello");
                v.validate_end_element();
                v.validate_end_element();
            });
            assert!(!errors.is_empty(), "Expected content model error for wrong child element");
        }

        #[test]
        fn test_alternative_no_attribute_no_match() {
            // When no attributes are present, XPath test @kind='A' should be false
            let schema_set = load_schema_xsd11(ALTERNATIVES_SCHEMA);
            let ns = empty_ns_context();
            let errors = validate_errors(&schema_set, |v| {
                v.validate_element("data", "", None, None, &ns);
                // No kind attribute
                v.validate_end_of_attributes();
                v.validate_element("val", "", None, None, &ns);
                v.validate_end_of_attributes();
                v.validate_text("anything");
                v.validate_end_element();
                v.validate_end_element();
            });
            // Falls through to declared type (xs:string child), should be valid
            assert!(errors.is_empty(), "Expected no errors, got: {:?}", errors);
        }

        #[test]
        fn test_alternative_schema_info_reflects_selected_type() {
            let schema_set = load_schema_xsd11(ALTERNATIVES_SCHEMA);
            let ns = empty_ns_context();
            let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
            let mut v = validator.start_run(TestSink::new());

            v.validate_element("data", "", None, None, &ns);
            v.validate_attribute("kind", "", "int");
            let eoa_info = v.validate_end_of_attributes();
            // CTA switched the type — SchemaInfo should carry the new type
            assert!(
                eoa_info.schema_type.is_some(),
                "validate_end_of_attributes() should return updated type after CTA switch"
            );

            v.validate_element("val", "", None, None, &ns);
            v.validate_end_of_attributes();
            v.validate_text("123");
            v.validate_end_element();
            v.validate_end_element();
            v.end_validation().ok();
            assert!(v.sink.errors.is_empty(), "errors: {:?}", v.sink.errors);
        }

        // Issue 1: Attribute validation deferred until after CTA selection
        #[test]
        fn test_deferred_attr_validation_rejects_prohibited_attr() {
            // The selected type does not declare "extra" — should be rejected
            let schema_set = load_schema_xsd11(
                r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                    <xs:complexType name="strict">
                        <xs:sequence>
                            <xs:element name="v" type="xs:string"/>
                        </xs:sequence>
                        <xs:attribute name="kind" type="xs:string"/>
                    </xs:complexType>
                    <xs:element name="root">
                        <xs:complexType>
                            <xs:sequence>
                                <xs:element name="v" type="xs:string"/>
                            </xs:sequence>
                            <xs:attribute name="kind" type="xs:string"/>
                            <xs:attribute name="extra" type="xs:string"/>
                        </xs:complexType>
                        <xs:alternative test="@kind='strict'" type="strict"/>
                    </xs:element>
                </xs:schema>"#,
            );
            let ns = empty_ns_context();
            let errors = validate_errors(&schema_set, |v| {
                v.validate_element("root", "", None, None, &ns);
                // "extra" is declared on element's own type, but not on "strict"
                v.validate_attribute("kind", "", "strict");
                v.validate_attribute("extra", "", "foo");
                v.validate_end_of_attributes();
                v.validate_element("v", "", None, None, &ns);
                v.validate_end_of_attributes();
                v.validate_text("hello");
                v.validate_end_element();
                v.validate_end_element();
            });
            // "extra" should be rejected because CTA selected "strict" type
            assert!(
                errors.iter().any(|e| e.message.contains("extra")),
                "Expected error for undeclared 'extra' attribute in selected type, got: {:?}",
                errors
            );
        }

        #[test]
        fn test_deferred_attr_validation_checks_fixed_value() {
            // The selected type has a fixed value for an attribute
            let schema_set = load_schema_xsd11(
                r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                    <xs:complexType name="fixed">
                        <xs:sequence>
                            <xs:element name="v" type="xs:string"/>
                        </xs:sequence>
                        <xs:attribute name="kind" type="xs:string"/>
                        <xs:attribute name="code" type="xs:string" fixed="ABC"/>
                    </xs:complexType>
                    <xs:element name="root">
                        <xs:complexType>
                            <xs:sequence>
                                <xs:element name="v" type="xs:string"/>
                            </xs:sequence>
                            <xs:attribute name="kind" type="xs:string"/>
                            <xs:attribute name="code" type="xs:string"/>
                        </xs:complexType>
                        <xs:alternative test="@kind='fixed'" type="fixed"/>
                    </xs:element>
                </xs:schema>"#,
            );
            let ns = empty_ns_context();

            // Wrong fixed value
            let errors = validate_errors(&schema_set, |v| {
                v.validate_element("root", "", None, None, &ns);
                v.validate_attribute("kind", "", "fixed");
                v.validate_attribute("code", "", "XYZ");
                v.validate_end_of_attributes();
                v.validate_element("v", "", None, None, &ns);
                v.validate_end_of_attributes();
                v.validate_text("hello");
                v.validate_end_element();
                v.validate_end_element();
            });
            assert!(
                errors.iter().any(|e| e.constraint == "cvc-attribute.4"),
                "Expected cvc-attribute.4 error for fixed value mismatch, got: {:?}",
                errors
            );

            // Correct fixed value
            let errors_ok = validate_errors(&schema_set, |v| {
                v.validate_element("root", "", None, None, &ns);
                v.validate_attribute("kind", "", "fixed");
                v.validate_attribute("code", "", "ABC");
                v.validate_end_of_attributes();
                v.validate_element("v", "", None, None, &ns);
                v.validate_end_of_attributes();
                v.validate_text("hello");
                v.validate_end_element();
                v.validate_end_element();
            });
            assert!(errors_ok.is_empty(), "Expected no errors, got: {:?}", errors_ok);
        }

        #[test]
        fn test_deferred_attr_validates_type_against_selected() {
            // The selected type declares attr as xs:integer — value "abc" should fail
            let schema_set = load_schema_xsd11(
                r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                    <xs:complexType name="numType">
                        <xs:sequence>
                            <xs:element name="v" type="xs:string"/>
                        </xs:sequence>
                        <xs:attribute name="kind" type="xs:string"/>
                        <xs:attribute name="val" type="xs:integer"/>
                    </xs:complexType>
                    <xs:element name="root">
                        <xs:complexType>
                            <xs:sequence>
                                <xs:element name="v" type="xs:string"/>
                            </xs:sequence>
                            <xs:attribute name="kind" type="xs:string"/>
                            <xs:attribute name="val" type="xs:string"/>
                        </xs:complexType>
                        <xs:alternative test="@kind='num'" type="numType"/>
                    </xs:element>
                </xs:schema>"#,
            );
            let ns = empty_ns_context();

            // "abc" is valid xs:string (declared type) but not xs:integer (selected type)
            let errors = validate_errors(&schema_set, |v| {
                v.validate_element("root", "", None, None, &ns);
                v.validate_attribute("kind", "", "num");
                v.validate_attribute("val", "", "abc");
                v.validate_end_of_attributes();
                v.validate_element("v", "", None, None, &ns);
                v.validate_end_of_attributes();
                v.validate_text("hello");
                v.validate_end_element();
                v.validate_end_element();
            });
            assert!(
                !errors.is_empty(),
                "Expected type error for 'abc' against xs:integer in selected type"
            );

            // "42" should be valid
            let errors_ok = validate_errors(&schema_set, |v| {
                v.validate_element("root", "", None, None, &ns);
                v.validate_attribute("kind", "", "num");
                v.validate_attribute("val", "", "42");
                v.validate_end_of_attributes();
                v.validate_element("v", "", None, None, &ns);
                v.validate_end_of_attributes();
                v.validate_text("hello");
                v.validate_end_element();
                v.validate_end_element();
            });
            assert!(errors_ok.is_empty(), "Expected no errors, got: {:?}", errors_ok);
        }

        // Regression: when CTA evaluates but selects the same type (or no
        // match), deferred attributes must still be validated.
        #[test]
        fn test_cta_no_switch_still_validates_attributes() {
            // Schema where element "data" has alternatives but we'll supply
            // kind='other' which matches neither, so no CTA switch occurs.
            // The default fallback selects the declared type.
            // The attribute "unknown" is not declared and should be reported.
            let schema_set = load_schema_xsd11(ALTERNATIVES_SCHEMA);
            let ns = empty_ns_context();

            let errors = validate_errors(&schema_set, |v| {
                v.validate_element("data", "", None, None, &ns);
                v.validate_attribute("kind", "", "other"); // no alternative matches
                v.validate_attribute("unknown", "", "val"); // undeclared attribute
                v.validate_end_of_attributes();
                v.validate_end_element();
            });
            assert!(
                errors.iter().any(|e| e.constraint == "cvc-complex-type.3.2.2"),
                "Undeclared attribute 'unknown' should be reported even when CTA \
                 doesn't switch type, got: {:?}",
                errors
            );
        }

        // Issue 3: validate_end_of_attributes returns empty SchemaInfo when no CTA
        #[test]
        fn test_no_cta_returns_empty_schema_info() {
            let schema_set = load_schema_xsd11(
                r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                    <xs:element name="root" type="xs:string"/>
                </xs:schema>"#,
            );
            let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
            let mut v = validator.start_run(TestSink::new());
            let ns = empty_ns_context();
            v.validate_element("root", "", None, None, &ns);
            let eoa_info = v.validate_end_of_attributes();
            // No CTA — schema_type should be None (empty SchemaInfo)
            assert!(
                eoa_info.schema_type.is_none(),
                "No CTA switch should return empty SchemaInfo, got type: {:?}",
                eoa_info.schema_type
            );
        }

        /// Helper: run a full validation pass with PROCESS_ASSERTIONS enabled
        /// (fragment buffer mode) and return the collected errors.
        fn validate_errors_with_assertions(
            schema_set: &SchemaSet,
            run: impl FnOnce(&mut ValidationRuntime<'_, TestSink>),
        ) -> Vec<ValidationError> {
            let validator = SchemaValidator::new_fragment_buffer(
                schema_set,
                ValidationFlags::default(),
            );
            let mut v = validator.start_run(TestSink::new());
            run(&mut v);
            v.end_validation().ok();
            v.sink.errors
        }

        // ── CTA + assertion interaction tests ───────────────────────────

        #[test]
        fn test_cta_non_asserted_to_asserted() {
            // Default type has NO assertions; CTA-selected type has xs:assert.
            // Assertion should fire and see the attributes.
            let schema_set = load_schema_xsd11(
                r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                    <xs:complexType name="assertedType">
                        <xs:sequence>
                            <xs:element name="v" type="xs:string"/>
                        </xs:sequence>
                        <xs:attribute name="kind" type="xs:string"/>
                        <xs:attribute name="val" type="xs:integer"/>
                        <xs:assert test="@val > 0"/>
                    </xs:complexType>
                    <xs:element name="item">
                        <xs:complexType>
                            <xs:sequence>
                                <xs:element name="v" type="xs:string"/>
                            </xs:sequence>
                            <xs:attribute name="kind" type="xs:string"/>
                            <xs:attribute name="val" type="xs:integer"/>
                        </xs:complexType>
                        <xs:alternative test="@kind='checked'" type="assertedType"/>
                    </xs:element>
                </xs:schema>"#,
            );
            let ns = empty_ns_context();

            // val=-1 violates @val > 0 on the CTA-selected type
            let errors = validate_errors_with_assertions(&schema_set, |v| {
                v.validate_element("item", "", None, None, &ns);
                v.validate_attribute("kind", "", "checked");
                v.validate_attribute("val", "", "-1");
                v.validate_end_of_attributes();
                v.validate_element("v", "", None, None, &ns);
                v.validate_end_of_attributes();
                v.validate_text("hello");
                v.validate_end_element();
                v.validate_end_element();
            });
            assert!(
                errors.iter().any(|e| e.constraint == "cvc-assertion"),
                "Expected assertion error for @val > 0 with val=-1, got: {:?}",
                errors
            );

            // val=5 satisfies @val > 0
            let errors_ok = validate_errors_with_assertions(&schema_set, |v| {
                v.validate_element("item", "", None, None, &ns);
                v.validate_attribute("kind", "", "checked");
                v.validate_attribute("val", "", "5");
                v.validate_end_of_attributes();
                v.validate_element("v", "", None, None, &ns);
                v.validate_end_of_attributes();
                v.validate_text("hello");
                v.validate_end_element();
                v.validate_end_element();
            });
            assert!(
                errors_ok.iter().all(|e| e.constraint != "cvc-assertion"),
                "Expected no assertion errors for @val > 0 with val=5, got: {:?}",
                errors_ok
            );
        }

        #[test]
        fn test_cta_asserted_to_non_asserted() {
            // Default type has xs:assert; CTA-selected type has none.
            // The old assertion should NOT be evaluated.
            let schema_set = load_schema_xsd11(
                r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                    <xs:complexType name="plainType">
                        <xs:sequence>
                            <xs:element name="v" type="xs:string"/>
                        </xs:sequence>
                        <xs:attribute name="kind" type="xs:string"/>
                        <xs:attribute name="val" type="xs:integer"/>
                    </xs:complexType>
                    <xs:element name="item">
                        <xs:complexType>
                            <xs:sequence>
                                <xs:element name="v" type="xs:string"/>
                            </xs:sequence>
                            <xs:attribute name="kind" type="xs:string"/>
                            <xs:attribute name="val" type="xs:integer"/>
                            <xs:assert test="@val > 100"/>
                        </xs:complexType>
                        <xs:alternative test="@kind='plain'" type="plainType"/>
                    </xs:element>
                </xs:schema>"#,
            );
            let ns = empty_ns_context();

            // val=1 would fail @val > 100 on the default type, but CTA selects
            // plainType which has no assertions — no assertion error expected.
            let errors = validate_errors_with_assertions(&schema_set, |v| {
                v.validate_element("item", "", None, None, &ns);
                v.validate_attribute("kind", "", "plain");
                v.validate_attribute("val", "", "1");
                v.validate_end_of_attributes();
                v.validate_element("v", "", None, None, &ns);
                v.validate_end_of_attributes();
                v.validate_text("hello");
                v.validate_end_element();
                v.validate_end_element();
            });
            assert!(
                errors.iter().all(|e| e.constraint != "cvc-assertion"),
                "Expected NO assertion errors (CTA selected non-asserted type), got: {:?}",
                errors
            );
        }

        #[test]
        fn test_cta_asserted_to_asserted() {
            // Both default type and CTA-selected type have assertions.
            // Only the selected type's assertion should run.
            let schema_set = load_schema_xsd11(
                r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                    <xs:complexType name="strictType">
                        <xs:sequence>
                            <xs:element name="v" type="xs:string"/>
                        </xs:sequence>
                        <xs:attribute name="kind" type="xs:string"/>
                        <xs:attribute name="val" type="xs:integer"/>
                        <xs:assert test="@val > 10"/>
                    </xs:complexType>
                    <xs:element name="item">
                        <xs:complexType>
                            <xs:sequence>
                                <xs:element name="v" type="xs:string"/>
                            </xs:sequence>
                            <xs:attribute name="kind" type="xs:string"/>
                            <xs:attribute name="val" type="xs:integer"/>
                            <xs:assert test="@val > 0"/>
                        </xs:complexType>
                        <xs:alternative test="@kind='strict'" type="strictType"/>
                    </xs:element>
                </xs:schema>"#,
            );
            let ns = empty_ns_context();

            // val=5 passes default @val > 0 but fails strict @val > 10
            let errors = validate_errors_with_assertions(&schema_set, |v| {
                v.validate_element("item", "", None, None, &ns);
                v.validate_attribute("kind", "", "strict");
                v.validate_attribute("val", "", "5");
                v.validate_end_of_attributes();
                v.validate_element("v", "", None, None, &ns);
                v.validate_end_of_attributes();
                v.validate_text("hello");
                v.validate_end_element();
                v.validate_end_element();
            });
            assert!(
                errors.iter().any(|e| e.constraint == "cvc-assertion"),
                "Expected assertion error from strict @val > 10 with val=5, got: {:?}",
                errors
            );

            // val=20 passes strict @val > 10
            let errors_ok = validate_errors_with_assertions(&schema_set, |v| {
                v.validate_element("item", "", None, None, &ns);
                v.validate_attribute("kind", "", "strict");
                v.validate_attribute("val", "", "20");
                v.validate_end_of_attributes();
                v.validate_element("v", "", None, None, &ns);
                v.validate_end_of_attributes();
                v.validate_text("hello");
                v.validate_end_element();
                v.validate_end_element();
            });
            assert!(
                errors_ok.iter().all(|e| e.constraint != "cvc-assertion"),
                "Expected no assertion errors for @val > 10 with val=20, got: {:?}",
                errors_ok
            );
        }
    }

    // -----------------------------------------------------------------------
    // Schema-level defaultAttributes tests (XSD 1.1)
    // -----------------------------------------------------------------------

    #[test]
    #[cfg(feature = "xsd11")]
    fn test_default_attributes_applied() {
        let schema_set = load_schema_xsd11(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
                         defaultAttributes="commonAttrs">
                <xs:attributeGroup name="commonAttrs">
                    <xs:attribute name="lang" type="xs:string"/>
                </xs:attributeGroup>
                <xs:element name="root">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="a" type="xs:string"/>
                        </xs:sequence>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"#,
        );

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);
        v.validate_attribute("lang", "", "en");
        v.validate_end_of_attributes();
        v.validate_element("a", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_text("hello");
        v.validate_end_element();
        v.validate_end_element();
        v.end_validation().ok();

        assert!(
            v.sink.errors.is_empty(),
            "Default attribute group attribute 'lang' should be accepted, got: {:?}",
            v.sink.errors
        );
    }

    #[test]
    #[cfg(feature = "xsd11")]
    fn test_default_attributes_opt_out() {
        let schema_set = load_schema_xsd11(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
                         defaultAttributes="commonAttrs">
                <xs:attributeGroup name="commonAttrs">
                    <xs:attribute name="lang" type="xs:string"/>
                </xs:attributeGroup>
                <xs:element name="root">
                    <xs:complexType defaultAttributesApply="false">
                        <xs:sequence>
                            <xs:element name="a" type="xs:string"/>
                        </xs:sequence>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"#,
        );

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);
        v.validate_attribute("lang", "", "en");
        v.validate_end_of_attributes();
        v.validate_element("a", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_text("hello");
        v.validate_end_element();
        v.validate_end_element();
        v.end_validation().ok();

        // 'lang' should be rejected because the type opted out
        assert!(
            v.sink.errors.iter().any(|e| e.constraint.starts_with("cvc-complex-type.3")),
            "Attribute 'lang' should be rejected when defaultAttributesApply=false, got: {:?}",
            v.sink.errors
        );
    }

    #[test]
    #[cfg(feature = "xsd11")]
    fn test_default_attributes_contributes_defaults() {
        let schema_set = load_schema_xsd11(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
                         defaultAttributes="commonAttrs">
                <xs:attribute name="lang" type="xs:string" default="en"/>
                <xs:attributeGroup name="commonAttrs">
                    <xs:attribute ref="lang"/>
                </xs:attributeGroup>
                <xs:element name="root">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="a" type="xs:string"/>
                        </xs:sequence>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"#,
        );

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);
        v.validate_end_of_attributes();

        // get_default_attributes should include 'lang' with value "en"
        let defaults = v.get_default_attributes();
        assert!(
            defaults.iter().any(|d| {
                let name = schema_set.name_table.resolve(d.local_name);
                name == "lang" && d.value == "en"
            }),
            "Default attributes should include 'lang' with value 'en', got: {:?}",
            defaults.iter().map(|d| (schema_set.name_table.resolve(d.local_name), &d.value)).collect::<Vec<_>>()
        );

        v.validate_element("a", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_text("hello");
        v.validate_end_element();
        v.validate_end_element();
        v.end_validation().ok();
    }

    #[test]
    #[cfg(feature = "xsd11")]
    fn test_default_attributes_required() {
        let schema_set = load_schema_xsd11(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
                         defaultAttributes="commonAttrs">
                <xs:attributeGroup name="commonAttrs">
                    <xs:attribute name="lang" type="xs:string" use="required"/>
                </xs:attributeGroup>
                <xs:element name="root">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="a" type="xs:string"/>
                        </xs:sequence>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"#,
        );

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);
        // Don't provide 'lang' attribute
        v.validate_end_of_attributes();
        v.validate_element("a", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_text("hello");
        v.validate_end_element();
        v.validate_end_element();
        v.end_validation().ok();

        assert!(
            v.sink.errors.iter().any(|e| e.constraint == "cvc-complex-type.4"),
            "Required attribute from default group should cause cvc-complex-type.4 error, got: {:?}",
            v.sink.errors
        );
    }

    #[test]
    #[cfg(feature = "xsd11")]
    fn test_default_attributes_any_attribute() {
        let schema_set = load_schema_xsd11(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
                         defaultAttributes="commonAttrs">
                <xs:attributeGroup name="commonAttrs">
                    <xs:anyAttribute processContents="lax"/>
                </xs:attributeGroup>
                <xs:element name="root">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="a" type="xs:string"/>
                        </xs:sequence>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"#,
        );

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);
        v.validate_attribute("unknown", "", "value");
        v.validate_end_of_attributes();
        v.validate_element("a", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_text("hello");
        v.validate_end_element();
        v.validate_end_element();
        v.end_validation().ok();

        assert!(
            v.sink.errors.is_empty(),
            "anyAttribute in default group should allow unknown attributes, got: {:?}",
            v.sink.errors
        );
    }

    // -----------------------------------------------------------------------
    // Attribute form / attributeFormDefault tests
    // -----------------------------------------------------------------------

    /// Build a namespace context for `http://example.com/ns` with `tns` prefix.
    fn tns_ns_context(schema_set: &SchemaSet) -> NamespaceContextSnapshot {
        let tns_id = schema_set.name_table.add("http://example.com/ns");
        let tns_prefix = schema_set.name_table.add("tns");
        NamespaceContextSnapshot {
            default_ns: Some(tns_id),
            bindings: vec![(tns_prefix, tns_id)],
        }
    }

    /// Validate a single attribute on `<root>` and assert accept/reject.
    ///
    /// `accept_ns` is the attribute namespace that should be accepted.
    /// `reject_ns` is the attribute namespace that should be rejected.
    fn assert_attribute_form(
        schema_set: &SchemaSet,
        accept_ns: &str,
        reject_ns: &str,
        accept_msg: &str,
        reject_msg: &str,
    ) {
        let validator = SchemaValidator::new(schema_set, ValidationFlags::default());
        let ns = tns_ns_context(schema_set);

        // --- Accept case
        let mut v = validator.start_run(TestSink::new());
        v.validate_element("root", "http://example.com/ns", None, None, &ns);
        let info = v.validate_attribute("id", accept_ns, "val");
        assert_ne!(info.validity, SchemaValidity::Invalid, "{accept_msg}, errors: {:?}", v.sink.errors);
        v.validate_end_of_attributes();
        v.validate_end_element();
        v.end_validation().ok();
        assert!(v.sink.errors.is_empty(), "expected no errors, got: {:?}", v.sink.errors);

        // --- Reject case
        let mut v2 = validator.start_run(TestSink::new());
        v2.validate_element("root", "http://example.com/ns", None, None, &ns);
        let info = v2.validate_attribute("id", reject_ns, "val");
        assert_eq!(info.validity, SchemaValidity::Invalid, "{reject_msg}");
        v2.validate_end_of_attributes();
        v2.validate_end_element();
        v2.end_validation().ok();
        assert!(
            v2.sink.errors.iter().any(|e| e.constraint == "cvc-complex-type.3.2.2"),
            "expected cvc-complex-type.3.2.2, got: {:?}", v2.sink.errors
        );
    }

    const TNS: &str = "http://example.com/ns";

    #[test]
    fn test_attribute_form_default_qualified() {
        let schema_set = load_schema(
            r###"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
                         targetNamespace="http://example.com/ns"
                         attributeFormDefault="qualified"
                         xmlns:tns="http://example.com/ns">
                <xs:element name="root">
                    <xs:complexType>
                        <xs:attribute name="id" type="xs:string"/>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"###,
        );
        assert_attribute_form(
            &schema_set, TNS, "",
            "qualified attribute should be valid",
            "unqualified attribute should be rejected when attributeFormDefault=qualified",
        );
    }

    #[test]
    fn test_attribute_form_qualified_explicit() {
        let schema_set = load_schema(
            r###"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
                         targetNamespace="http://example.com/ns"
                         xmlns:tns="http://example.com/ns">
                <xs:element name="root">
                    <xs:complexType>
                        <xs:attribute name="id" type="xs:string" form="qualified"/>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"###,
        );
        assert_attribute_form(
            &schema_set, TNS, "",
            "form=qualified attribute should be valid",
            "unqualified attribute should be rejected when form=qualified",
        );
    }

    #[test]
    fn test_attribute_form_unqualified_explicit() {
        let schema_set = load_schema(
            r###"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
                         targetNamespace="http://example.com/ns"
                         attributeFormDefault="qualified"
                         xmlns:tns="http://example.com/ns">
                <xs:element name="root">
                    <xs:complexType>
                        <xs:attribute name="id" type="xs:string" form="unqualified"/>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"###,
        );
        assert_attribute_form(
            &schema_set, "", TNS,
            "form=unqualified attribute should be valid",
            "qualified attribute should be rejected when form=unqualified",
        );
    }

    #[test]
    fn test_attribute_form_default_unqualified() {
        let schema_set = load_schema(
            r###"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
                         targetNamespace="http://example.com/ns"
                         xmlns:tns="http://example.com/ns">
                <xs:element name="root">
                    <xs:complexType>
                        <xs:attribute name="id" type="xs:string"/>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"###,
        );
        assert_attribute_form(
            &schema_set, "", TNS,
            "default unqualified attribute should be valid",
            "qualified attribute should be rejected when default is unqualified",
        );
    }

    #[test]
    fn test_attribute_group_form_qualified() {
        let schema_set = load_schema(
            r###"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
                         targetNamespace="http://example.com/ns"
                         attributeFormDefault="qualified"
                         xmlns:tns="http://example.com/ns">
                <xs:attributeGroup name="myAttrs">
                    <xs:attribute name="id" type="xs:string"/>
                </xs:attributeGroup>
                <xs:element name="root">
                    <xs:complexType>
                        <xs:attributeGroup ref="tns:myAttrs"/>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"###,
        );
        assert_attribute_form(
            &schema_set, TNS, "",
            "qualified attribute from group should be valid",
            "unqualified attribute should be rejected for qualified group attribute",
        );
    }

    #[cfg(feature = "xsd11")]
    #[test]
    fn test_attribute_explicit_target_namespace() {
        let schema_set = load_schema_xsd11(
            r###"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
                         targetNamespace="http://example.com/ns"
                         xmlns:tns="http://example.com/ns"
                         xmlns:other="http://other.com/ns">
                <xs:element name="root">
                    <xs:complexType>
                        <xs:attribute name="id" type="xs:string"
                                      targetNamespace="http://other.com/ns"/>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"###,
        );
        assert_attribute_form(
            &schema_set, "http://other.com/ns", TNS,
            "explicit targetNamespace attribute should be valid",
            "attribute with wrong namespace should be rejected",
        );
    }

    // -----------------------------------------------------------------------
    // ID / IDREF / IDREFS correctness proof tests
    // -----------------------------------------------------------------------

    /// Helper schema for ID/IDREF attribute tests.
    fn id_idref_attr_schema() -> crate::schema::SchemaSet {
        load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="item" maxOccurs="unbounded">
                                <xs:complexType>
                                    <xs:attribute name="id" type="xs:ID" use="required"/>
                                </xs:complexType>
                            </xs:element>
                            <xs:element name="link" minOccurs="0" maxOccurs="unbounded">
                                <xs:complexType>
                                    <xs:attribute name="ref" type="xs:IDREF" use="required"/>
                                </xs:complexType>
                            </xs:element>
                            <xs:element name="multi" minOccurs="0" maxOccurs="unbounded">
                                <xs:complexType>
                                    <xs:attribute name="refs" type="xs:IDREFS" use="required"/>
                                </xs:complexType>
                            </xs:element>
                        </xs:sequence>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"#,
        )
    }

    /// IDREF valid forward reference — reference appears before the ID definition.
    #[test]
    fn test_idref_forward_reference() {
        // Use xs:choice so link can appear before item
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root">
                    <xs:complexType>
                        <xs:choice maxOccurs="unbounded">
                            <xs:element name="item">
                                <xs:complexType>
                                    <xs:attribute name="id" type="xs:ID" use="required"/>
                                </xs:complexType>
                            </xs:element>
                            <xs:element name="link">
                                <xs:complexType>
                                    <xs:attribute name="ref" type="xs:IDREF" use="required"/>
                                </xs:complexType>
                            </xs:element>
                        </xs:choice>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"#,
        );
        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);
        v.validate_end_of_attributes();

        // Forward reference: link before item
        v.validate_element("link", "", None, None, &ns);
        v.validate_attribute("ref", "", "future");
        v.validate_end_of_attributes();
        v.validate_end_element();

        // Now define the ID
        v.validate_element("item", "", None, None, &ns);
        v.validate_attribute("id", "", "future");
        v.validate_end_of_attributes();
        v.validate_end_element();

        v.validate_end_element();
        v.end_validation().ok();

        assert!(
            v.sink.errors.is_empty(),
            "Forward IDREF reference should be valid, got: {:?}",
            v.sink.errors
        );
    }

    /// IDREFS with all tokens valid — no errors expected.
    #[test]
    fn test_idrefs_all_valid() {
        let schema_set = id_idref_attr_schema();
        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);
        v.validate_end_of_attributes();

        v.validate_element("item", "", None, None, &ns);
        v.validate_attribute("id", "", "a1");
        v.validate_end_of_attributes();
        v.validate_end_element();

        v.validate_element("item", "", None, None, &ns);
        v.validate_attribute("id", "", "a2");
        v.validate_end_of_attributes();
        v.validate_end_element();

        v.validate_element("multi", "", None, None, &ns);
        v.validate_attribute("refs", "", "a1 a2");
        v.validate_end_of_attributes();
        v.validate_end_element();

        v.validate_end_element();
        v.end_validation().ok();

        assert!(
            v.sink.errors.is_empty(),
            "IDREFS with all valid tokens should succeed, got: {:?}",
            v.sink.errors
        );
    }

    /// IDREFS with one missing token and one valid token.
    #[test]
    fn test_idrefs_one_missing_one_valid() {
        let schema_set = id_idref_attr_schema();
        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);
        v.validate_end_of_attributes();

        v.validate_element("item", "", None, None, &ns);
        v.validate_attribute("id", "", "exists");
        v.validate_end_of_attributes();
        v.validate_end_element();

        v.validate_element("multi", "", None, None, &ns);
        v.validate_attribute("refs", "", "exists ghost");
        v.validate_end_of_attributes();
        v.validate_end_element();

        v.validate_end_element();
        v.end_validation().ok();

        let idref_errors: Vec<_> = v.sink.errors.iter()
            .filter(|e| e.constraint == "cvc-id.1")
            .collect();
        assert_eq!(
            idref_errors.len(), 1,
            "Expected 1 IDREF error for 'ghost', got: {:?}",
            idref_errors
        );
    }

    /// IDREFS with multiple missing tokens.
    #[test]
    fn test_idrefs_multiple_missing() {
        let schema_set = id_idref_attr_schema();
        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);
        v.validate_end_of_attributes();

        v.validate_element("item", "", None, None, &ns);
        v.validate_attribute("id", "", "only");
        v.validate_end_of_attributes();
        v.validate_end_element();

        v.validate_element("multi", "", None, None, &ns);
        v.validate_attribute("refs", "", "nope1 nope2 nope3");
        v.validate_end_of_attributes();
        v.validate_end_element();

        v.validate_end_element();
        v.end_validation().ok();

        let idref_errors: Vec<_> = v.sink.errors.iter()
            .filter(|e| e.constraint == "cvc-id.1")
            .collect();
        assert_eq!(
            idref_errors.len(), 3,
            "Expected 3 IDREF errors for nope1/nope2/nope3, got: {:?}",
            idref_errors
        );
    }

    /// IDREFS empty after whitespace collapse is a lexical error.
    #[test]
    fn test_idrefs_empty_after_collapse() {
        let schema_set = id_idref_attr_schema();
        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);
        v.validate_end_of_attributes();

        v.validate_element("multi", "", None, None, &ns);
        v.validate_attribute("refs", "", "   ");
        v.validate_end_of_attributes();
        v.validate_end_element();

        v.validate_end_element();
        v.end_validation().ok();

        // Should have a validation error (lexical), no cvc-id.1 errors
        assert!(
            !v.sink.errors.is_empty(),
            "IDREFS with only whitespace should produce an error"
        );
        let idref_errors: Vec<_> = v.sink.errors.iter()
            .filter(|e| e.constraint == "cvc-id.1")
            .collect();
        assert!(
            idref_errors.is_empty(),
            "Empty IDREFS should not produce cvc-id.1 errors (lexical rejection), got: {:?}",
            idref_errors
        );
    }

    /// ID lexical rejection for invalid NCName.
    #[test]
    fn test_id_invalid_ncname() {
        let schema_set = id_idref_attr_schema();
        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);
        v.validate_end_of_attributes();

        // "1bad" starts with digit — not a valid NCName
        v.validate_element("item", "", None, None, &ns);
        v.validate_attribute("id", "", "1bad");
        v.validate_end_of_attributes();
        v.validate_end_element();

        v.validate_end_element();
        v.end_validation().ok();

        assert!(
            !v.sink.errors.is_empty(),
            "Invalid NCName for ID should produce an error"
        );
        // Should NOT appear in ID table (no duplicate detection)
        let id_dup_errors: Vec<_> = v.sink.errors.iter()
            .filter(|e| e.constraint == "cvc-id.2")
            .collect();
        assert!(
            id_dup_errors.is_empty(),
            "Invalid NCName should not produce cvc-id.2, got: {:?}",
            id_dup_errors
        );
    }

    /// IDREF lexical rejection for invalid NCName.
    #[test]
    fn test_idref_invalid_ncname() {
        let schema_set = id_idref_attr_schema();
        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);
        v.validate_end_of_attributes();

        v.validate_element("link", "", None, None, &ns);
        v.validate_attribute("ref", "", "bad:name");
        v.validate_end_of_attributes();
        v.validate_end_element();

        v.validate_end_element();
        v.end_validation().ok();

        assert!(
            !v.sink.errors.is_empty(),
            "Invalid NCName for IDREF should produce an error"
        );
        // The invalid value should NOT end up in pending_idrefs
        let idref_errors: Vec<_> = v.sink.errors.iter()
            .filter(|e| e.constraint == "cvc-id.1")
            .collect();
        assert!(
            idref_errors.is_empty(),
            "Invalid IDREF should not produce cvc-id.1 (no runtime tracking), got: {:?}",
            idref_errors
        );
    }

    /// IDREFS lexical rejection when one token is invalid NCName.
    #[test]
    fn test_idrefs_one_invalid_ncname_token() {
        let schema_set = id_idref_attr_schema();
        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);
        v.validate_end_of_attributes();

        // Second token "2bad" is invalid NCName
        v.validate_element("multi", "", None, None, &ns);
        v.validate_attribute("refs", "", "good 2bad");
        v.validate_end_of_attributes();
        v.validate_end_element();

        v.validate_end_element();
        v.end_validation().ok();

        assert!(
            !v.sink.errors.is_empty(),
            "IDREFS with one invalid token should produce an error"
        );
        // No tokens should be tracked (lexical validation rejects entire value)
        let idref_errors: Vec<_> = v.sink.errors.iter()
            .filter(|e| e.constraint == "cvc-id.1")
            .collect();
        assert!(
            idref_errors.is_empty(),
            "Invalid IDREFS should not produce cvc-id.1 errors, got: {:?}",
            idref_errors
        );
    }

    /// Element text typed as xs:ID participates in duplicate detection.
    #[test]
    fn test_element_text_id_duplicate() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="id" type="xs:ID" maxOccurs="unbounded"/>
                        </xs:sequence>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"#,
        );
        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);
        v.validate_end_of_attributes();

        v.validate_element("id", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_text("alpha");
        v.validate_end_element();

        v.validate_element("id", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_text("alpha"); // duplicate
        v.validate_end_element();

        v.validate_end_element();
        v.end_validation().ok();

        assert!(
            v.sink.errors.iter().any(|e| e.constraint == "cvc-id.2"),
            "Duplicate ID in element text should raise cvc-id.2, got: {:?}",
            v.sink.errors
        );
    }

    /// Element text typed as xs:IDREF participates in end-of-document resolution.
    #[test]
    fn test_element_text_idref_resolution() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="id" type="xs:ID" maxOccurs="unbounded"/>
                            <xs:element name="ref" type="xs:IDREF" maxOccurs="unbounded"/>
                        </xs:sequence>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"#,
        );
        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);
        v.validate_end_of_attributes();

        v.validate_element("id", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_text("x1");
        v.validate_end_element();

        // Valid reference
        v.validate_element("ref", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_text("x1");
        v.validate_end_element();

        // Missing reference
        v.validate_element("ref", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_text("missing");
        v.validate_end_element();

        v.validate_end_element();
        v.end_validation().ok();

        let idref_errors: Vec<_> = v.sink.errors.iter()
            .filter(|e| e.constraint == "cvc-id.1")
            .collect();
        assert_eq!(
            idref_errors.len(), 1,
            "Expected 1 cvc-id.1 error for element-text IDREF 'missing', got: {:?}",
            idref_errors
        );
    }

    /// Derived type from xs:ID still contributes to duplicate detection.
    #[test]
    fn test_derived_id_duplicate_detection() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:simpleType name="myID">
                    <xs:restriction base="xs:ID">
                        <xs:maxLength value="20"/>
                    </xs:restriction>
                </xs:simpleType>
                <xs:element name="root">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="item" maxOccurs="unbounded">
                                <xs:complexType>
                                    <xs:attribute name="id" type="myID" use="required"/>
                                </xs:complexType>
                            </xs:element>
                        </xs:sequence>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"#,
        );
        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);
        v.validate_end_of_attributes();

        v.validate_element("item", "", None, None, &ns);
        v.validate_attribute("id", "", "dup");
        v.validate_end_of_attributes();
        v.validate_end_element();

        v.validate_element("item", "", None, None, &ns);
        v.validate_attribute("id", "", "dup"); // duplicate
        v.validate_end_of_attributes();
        v.validate_end_element();

        v.validate_end_element();
        v.end_validation().ok();

        assert!(
            v.sink.errors.iter().any(|e| e.constraint == "cvc-id.2"),
            "Derived xs:ID should still detect duplicates, got: {:?}",
            v.sink.errors
        );
    }

    /// Derived type from xs:IDREF still contributes to reference tracking.
    #[test]
    fn test_derived_idref_tracking() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:simpleType name="myIDREF">
                    <xs:restriction base="xs:IDREF">
                        <xs:maxLength value="20"/>
                    </xs:restriction>
                </xs:simpleType>
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
                                    <xs:attribute name="ref" type="myIDREF" use="required"/>
                                </xs:complexType>
                            </xs:element>
                        </xs:sequence>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"#,
        );
        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);
        v.validate_end_of_attributes();

        v.validate_element("item", "", None, None, &ns);
        v.validate_attribute("id", "", "ok");
        v.validate_end_of_attributes();
        v.validate_end_element();

        // Valid derived IDREF
        v.validate_element("link", "", None, None, &ns);
        v.validate_attribute("ref", "", "ok");
        v.validate_end_of_attributes();
        v.validate_end_element();

        // Missing derived IDREF
        v.validate_element("link", "", None, None, &ns);
        v.validate_attribute("ref", "", "nope");
        v.validate_end_of_attributes();
        v.validate_end_element();

        v.validate_end_element();
        v.end_validation().ok();

        let idref_errors: Vec<_> = v.sink.errors.iter()
            .filter(|e| e.constraint == "cvc-id.1")
            .collect();
        assert_eq!(
            idref_errors.len(), 1,
            "Derived xs:IDREF should track references, got: {:?}",
            idref_errors
        );
    }

    /// Derived type from xs:IDREFS still tracks each token.
    #[test]
    fn test_derived_idrefs_tracking() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:simpleType name="myIDREFS">
                    <xs:restriction base="xs:IDREFS">
                        <xs:maxLength value="5"/>
                    </xs:restriction>
                </xs:simpleType>
                <xs:element name="root">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="item" maxOccurs="unbounded">
                                <xs:complexType>
                                    <xs:attribute name="id" type="xs:ID" use="required"/>
                                </xs:complexType>
                            </xs:element>
                            <xs:element name="multi" maxOccurs="unbounded">
                                <xs:complexType>
                                    <xs:attribute name="refs" type="myIDREFS" use="required"/>
                                </xs:complexType>
                            </xs:element>
                        </xs:sequence>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"#,
        );
        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);
        v.validate_end_of_attributes();

        v.validate_element("item", "", None, None, &ns);
        v.validate_attribute("id", "", "x");
        v.validate_end_of_attributes();
        v.validate_end_element();

        // "x" valid, "y" missing — derived IDREFS should track each token
        v.validate_element("multi", "", None, None, &ns);
        v.validate_attribute("refs", "", "x y");
        v.validate_end_of_attributes();
        v.validate_end_element();

        v.validate_end_element();
        v.end_validation().ok();

        let idref_errors: Vec<_> = v.sink.errors.iter()
            .filter(|e| e.constraint == "cvc-id.1")
            .collect();
        assert_eq!(
            idref_errors.len(), 1,
            "Derived xs:IDREFS should track each token, got: {:?}",
            idref_errors
        );
    }

    /// Valid repeated IDREF values do not raise duplicate-style errors.
    #[test]
    fn test_repeated_idref_no_false_duplicate() {
        let schema_set = id_idref_attr_schema();
        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);
        v.validate_end_of_attributes();

        v.validate_element("item", "", None, None, &ns);
        v.validate_attribute("id", "", "target");
        v.validate_end_of_attributes();
        v.validate_end_element();

        // Multiple references to the same ID — all valid
        v.validate_element("link", "", None, None, &ns);
        v.validate_attribute("ref", "", "target");
        v.validate_end_of_attributes();
        v.validate_end_element();

        v.validate_element("link", "", None, None, &ns);
        v.validate_attribute("ref", "", "target");
        v.validate_end_of_attributes();
        v.validate_end_element();

        v.validate_element("multi", "", None, None, &ns);
        v.validate_attribute("refs", "", "target target");
        v.validate_end_of_attributes();
        v.validate_end_element();

        v.validate_end_element();
        v.end_validation().ok();

        assert!(
            v.sink.errors.is_empty(),
            "Repeated IDREF to same ID should not error, got: {:?}",
            v.sink.errors
        );
    }

    /// Invalid lexical ID / IDREF values do not poison runtime tracking state.
    #[test]
    fn test_invalid_lexical_does_not_poison_tracking() {
        let schema_set = id_idref_attr_schema();
        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);
        v.validate_end_of_attributes();

        // Invalid ID (not NCName) — should not be tracked
        v.validate_element("item", "", None, None, &ns);
        v.validate_attribute("id", "", "123bad");
        v.validate_end_of_attributes();
        v.validate_end_element();

        // Valid ID
        v.validate_element("item", "", None, None, &ns);
        v.validate_attribute("id", "", "good");
        v.validate_end_of_attributes();
        v.validate_end_element();

        // IDREF to the invalid one — should raise cvc-id.1
        v.validate_element("link", "", None, None, &ns);
        v.validate_attribute("ref", "", "123bad");
        v.validate_end_of_attributes();
        v.validate_end_element();

        // IDREF to the valid one — should be fine
        v.validate_element("link", "", None, None, &ns);
        v.validate_attribute("ref", "", "good");
        v.validate_end_of_attributes();
        v.validate_end_element();

        v.validate_end_element();
        v.end_validation().ok();

        // Should have lexical errors for the invalid ID + IDREF,
        // but the valid ID/IDREF pair should work
        let dup_errors: Vec<_> = v.sink.errors.iter()
            .filter(|e| e.constraint == "cvc-id.2")
            .collect();
        assert!(
            dup_errors.is_empty(),
            "Invalid lexical values should not cause cvc-id.2, got: {:?}",
            dup_errors
        );
        // "good" should resolve, "123bad" IDREF also fails lexically so no cvc-id.1 for it
        let idref_errors: Vec<_> = v.sink.errors.iter()
            .filter(|e| e.constraint == "cvc-id.1")
            .collect();
        assert!(
            idref_errors.is_empty(),
            "Invalid IDREF '123bad' should fail lexically, not produce cvc-id.1, got: {:?}",
            idref_errors
        );
    }

    /// User-defined <xs:list itemType="xs:IDREF"> tracks each token individually.
    ///
    /// This proves that custom IDREF-list types (not just built-in xs:IDREFS)
    /// correctly decompose into per-token tracking, even though
    /// validate_list_type produces type_code==IdRef (the item code).
    #[test]
    fn test_custom_idref_list_tracking() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:simpleType name="myRefList">
                    <xs:list itemType="xs:IDREF"/>
                </xs:simpleType>
                <xs:element name="root">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="item" maxOccurs="unbounded">
                                <xs:complexType>
                                    <xs:attribute name="id" type="xs:ID" use="required"/>
                                </xs:complexType>
                            </xs:element>
                            <xs:element name="refs" minOccurs="0" maxOccurs="unbounded">
                                <xs:complexType>
                                    <xs:attribute name="targets" type="myRefList" use="required"/>
                                </xs:complexType>
                            </xs:element>
                        </xs:sequence>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"#,
        );
        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);
        v.validate_end_of_attributes();

        v.validate_element("item", "", None, None, &ns);
        v.validate_attribute("id", "", "a1");
        v.validate_end_of_attributes();
        v.validate_end_element();

        // "a1" exists, "missing1" and "missing2" do not
        v.validate_element("refs", "", None, None, &ns);
        v.validate_attribute("targets", "", "a1 missing1 missing2");
        v.validate_end_of_attributes();
        v.validate_end_element();

        v.validate_end_element();
        v.end_validation().ok();

        let idref_errors: Vec<_> = v.sink.errors.iter()
            .filter(|e| e.constraint == "cvc-id.1")
            .collect();
        assert_eq!(
            idref_errors.len(), 2,
            "Custom IDREF-list should track each token; expected 2 cvc-id.1 errors for missing1/missing2, got: {:?}",
            idref_errors
        );
    }

    /// Whitespace normalization regression: ID and IDREF with surrounding
    /// whitespace must match after collapse, and IDREFS cross-references
    /// must resolve against the collapsed ID value.
    #[test]
    fn test_whitespace_normalization_id_idref_match() {
        let schema_set = id_idref_attr_schema();
        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);
        v.validate_end_of_attributes();

        // ID with surrounding whitespace — collapsed to "foo"
        v.validate_element("item", "", None, None, &ns);
        v.validate_attribute("id", "", "  foo  ");
        v.validate_end_of_attributes();
        v.validate_end_element();

        // IDREF without whitespace — must match the collapsed ID
        v.validate_element("link", "", None, None, &ns);
        v.validate_attribute("ref", "", "foo");
        v.validate_end_of_attributes();
        v.validate_end_element();

        // IDREF with whitespace — must also match
        v.validate_element("link", "", None, None, &ns);
        v.validate_attribute("ref", "", "  foo  ");
        v.validate_end_of_attributes();
        v.validate_end_element();

        // IDREFS where the token matches the collapsed ID
        v.validate_element("multi", "", None, None, &ns);
        v.validate_attribute("refs", "", "  foo  ");
        v.validate_end_of_attributes();
        v.validate_end_element();

        v.validate_end_element();
        v.end_validation().ok();

        let idref_errors: Vec<_> = v.sink.errors.iter()
            .filter(|e| e.constraint == "cvc-id.1")
            .collect();
        assert!(
            idref_errors.is_empty(),
            "Whitespace-padded ID/IDREF/IDREFS should all resolve after collapse, got: {:?}",
            idref_errors
        );
        let dup_errors: Vec<_> = v.sink.errors.iter()
            .filter(|e| e.constraint == "cvc-id.2")
            .collect();
        assert!(
            dup_errors.is_empty(),
            "Single whitespace-padded ID should not produce duplicates, got: {:?}",
            dup_errors
        );
    }

    /// Whitespace normalization regression for element text content:
    /// ID defined via element text with whitespace must be found by IDREF.
    #[test]
    fn test_whitespace_normalization_element_text() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="id" type="xs:ID" maxOccurs="unbounded"/>
                            <xs:element name="ref" type="xs:IDREF" maxOccurs="unbounded"/>
                        </xs:sequence>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"#,
        );
        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);
        v.validate_end_of_attributes();

        // ID element with whitespace text
        v.validate_element("id", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_text("  bar  ");
        v.validate_end_element();

        // IDREF element referencing collapsed value
        v.validate_element("ref", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_text("bar");
        v.validate_end_element();

        v.validate_end_element();
        v.end_validation().ok();

        let idref_errors: Vec<_> = v.sink.errors.iter()
            .filter(|e| e.constraint == "cvc-id.1")
            .collect();
        assert!(
            idref_errors.is_empty(),
            "Element-text ID '  bar  ' collapsed to 'bar' should match IDREF 'bar', got: {:?}",
            idref_errors
        );
    }

    // -----------------------------------------------------------------------
    // xsi:type validation fallback semantics tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_xsi_type_unresolved_on_global_element() {
        // Global element + unknown xsi:type → Invalid, declared type used
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root" type="xs:string"/>
            </xs:schema>"#,
        );

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
        let ns = empty_ns_context();

        let info = v.validate_element("root", "", Some("noSuchType"), None, &ns);
        assert_eq!(info.validity, SchemaValidity::Invalid);
        // schema_type should be the declared type (xs:string), not None
        assert!(info.schema_type.is_some());

        // Should have cvc-elt.4.1 error
        assert!(
            v.sink.errors.iter().any(|e| e.constraint == "cvc-elt.4.1"),
            "Expected cvc-elt.4.1 error, got: {:?}",
            v.sink.errors
        );

        // Text should still validate against the declared type (xs:string)
        v.validate_end_of_attributes();
        v.validate_text("hello");
        let end_info = v.validate_end_element();
        // End element should not produce additional type errors
        let type_errors: Vec<_> = v.sink.errors.iter()
            .filter(|e| e.constraint != "cvc-elt.4.1")
            .collect();
        assert!(
            type_errors.is_empty(),
            "Expected only cvc-elt.4.1 error, but got additional: {:?}",
            type_errors
        );
        // end_info preserves invalidity from the xsi:type error
        assert_eq!(end_info.validity, SchemaValidity::Invalid);
        v.end_validation().ok();
    }

    #[test]
    fn test_xsi_type_invalid_derivation_on_global_element() {
        // Global element + xsi:type that doesn't derive → Invalid, declared type used
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root" type="xs:string"/>
                <xs:complexType name="unrelatedType">
                    <xs:sequence>
                        <xs:element name="child" type="xs:string"/>
                    </xs:sequence>
                </xs:complexType>
            </xs:schema>"#,
        );

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
        let ns = empty_ns_context();

        let info = v.validate_element("root", "", Some("unrelatedType"), None, &ns);
        assert_eq!(info.validity, SchemaValidity::Invalid);
        // schema_type should be the declared type (xs:string), not unrelatedType
        assert!(info.schema_type.is_some());

        // Should have cvc-elt.4.2 error
        assert!(
            v.sink.errors.iter().any(|e| e.constraint == "cvc-elt.4.2"),
            "Expected cvc-elt.4.2 error, got: {:?}",
            v.sink.errors
        );

        // Assessment uses declared type (xs:string), so text content should be fine
        v.validate_end_of_attributes();
        v.validate_text("hello");
        v.validate_end_element();
        v.end_validation().ok();

        // No additional errors beyond cvc-elt.4.2
        let other_errors: Vec<_> = v.sink.errors.iter()
            .filter(|e| e.constraint != "cvc-elt.4.2")
            .collect();
        assert!(
            other_errors.is_empty(),
            "Expected only cvc-elt.4.2 error, but got additional: {:?}",
            other_errors
        );
    }

    #[test]
    fn test_xsi_type_unresolved_on_local_element_with_type() {
        // Local element with type + unknown xsi:type → Invalid, falls back to matched type
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:element name="item" type="xs:string"/>
                        </xs:sequence>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"#,
        );

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);
        v.validate_end_of_attributes();

        let info = v.validate_element("item", "", Some("noSuchType"), None, &ns);
        assert_eq!(info.validity, SchemaValidity::Invalid);
        // Falls back to matched type (xs:string)
        assert!(info.schema_type.is_some());

        assert!(
            v.sink.errors.iter().any(|e| e.constraint == "cvc-elt.4.1"),
            "Expected cvc-elt.4.1 error, got: {:?}",
            v.sink.errors
        );

        v.validate_end_of_attributes();
        v.validate_text("hello");
        v.validate_end_element();
        v.validate_end_element();
        v.end_validation().ok();
    }

    #[test]
    fn test_xsi_type_unresolved_lax_assessment() {
        // Local element without type + bad xsi:type → Invalid, lax assessment, children accepted
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:any processContents="lax"/>
                        </xs:sequence>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"#,
        );

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);
        v.validate_end_of_attributes();

        // Unknown element matched by lax wildcard, with bad xsi:type
        let info = v.validate_element("unknown", "", Some("noSuchType"), None, &ns);
        // schema_type stays None (no governing type)
        assert!(info.schema_type.is_none());

        v.validate_end_of_attributes();
        // Nested child should be accepted via lax assessment (xs:anyType content model)
        v.validate_element("nested", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_text("hello");
        v.validate_end_element();

        v.validate_end_element(); // close unknown
        v.validate_end_element(); // close root
        v.end_validation().ok();

        // Should have cvc-elt.4.1 for the bad xsi:type, but no content model errors
        assert!(
            v.sink.errors.iter().any(|e| e.constraint == "cvc-elt.4.1"),
            "Expected cvc-elt.4.1 error, got: {:?}",
            v.sink.errors
        );
        let content_errors: Vec<_> = v.sink.errors.iter()
            .filter(|e| e.constraint == "cvc-complex-type.2.4")
            .collect();
        assert!(
            content_errors.is_empty(),
            "Lax assessment should not produce content model errors, got: {:?}",
            content_errors
        );
    }

    #[test]
    fn test_undeclared_element_lax_allows_children() {
        // Lax wildcard + nested children → no errors, xs:anyType content model accepts children
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:any processContents="lax"/>
                        </xs:sequence>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"#,
        );

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);
        v.validate_end_of_attributes();

        // Wildcard matches in content model → content_model_accepted path
        let info = v.validate_element("unknown", "", None, None, &ns);
        // Element accepted by content model, no governing type → schema_type = None
        assert!(info.schema_type.is_none());

        v.validate_end_of_attributes();
        // Nested children should be accepted via xs:anyType content model
        v.validate_element("child1", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_text("text1");
        v.validate_end_element();

        v.validate_element("child2", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_end_element();

        v.validate_end_element(); // close unknown
        v.validate_end_element(); // close root
        v.end_validation().ok();

        assert!(
            v.sink.errors.is_empty(),
            "Lax undeclared element should accept children without errors, got: {:?}",
            v.sink.errors
        );
    }

    #[test]
    fn test_undeclared_element_skip_no_assessment() {
        // Skip wildcard + nested children → no errors, skip bypass prevents content model errors
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:any processContents="skip"/>
                        </xs:sequence>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"#,
        );

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);
        v.validate_end_of_attributes();

        v.validate_element("anything", "", None, None, &ns);
        v.validate_end_of_attributes();

        // Deeply nested children should be accepted (skip bypass)
        v.validate_element("nested1", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_element("nested2", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_text("deep");
        v.validate_end_element(); // close nested2
        v.validate_end_element(); // close nested1

        v.validate_end_element(); // close anything
        v.validate_end_element(); // close root
        v.end_validation().ok();

        assert!(
            v.sink.errors.is_empty(),
            "Skip wildcard should accept all nested content without errors, got: {:?}",
            v.sink.errors
        );
    }

    #[test]
    fn test_strict_undeclared_same_assessment_as_lax() {
        // Strict wildcard: element is matched by wildcard in content model with
        // processContents=strict, but has no global declaration → cvc-elt.1.
        // Children should still be accepted via lax assessment.
        //
        // Use namespace-based wildcard to get strict processContents on
        // an element that is NOT globally declared.
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
                         targetNamespace="http://test">
                <xs:element name="root">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:any namespace="http://other" processContents="strict"/>
                        </xs:sequence>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"#,
        );

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
        let ns = empty_ns_context();

        v.validate_element("root", "http://test", None, None, &ns);
        v.validate_end_of_attributes();

        let info = v.validate_element("unknown", "http://other", None, None, &ns);
        assert_eq!(info.validity, SchemaValidity::Invalid);

        // cvc-elt.1 for undeclared element under strict processing
        assert!(
            v.sink.errors.iter().any(|e| e.constraint == "cvc-elt.1"),
            "Expected cvc-elt.1 error, got: {:?}",
            v.sink.errors
        );

        v.validate_end_of_attributes();
        // Children should still be accepted (lax assessment for content)
        v.validate_element("child", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_text("hello");
        v.validate_end_element();

        v.validate_end_element(); // close unknown
        v.validate_end_element(); // close root
        v.end_validation().ok();

        // No content model errors on the unknown element's children
        let content_errors: Vec<_> = v.sink.errors.iter()
            .filter(|e| e.constraint == "cvc-complex-type.2.4")
            .collect();
        assert!(
            content_errors.is_empty(),
            "Strict undeclared element should use lax assessment for children, got: {:?}",
            content_errors
        );
    }

    #[cfg(feature = "xsd11")]
    #[test]
    fn test_cta_preserves_xsi_type_invalidity() {
        // CTA switch after bad xsi:type → type switches, validity stays Invalid
        let schema_set = load_schema_xsd11(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:complexType name="typeA">
                    <xs:sequence>
                        <xs:element name="a" type="xs:string"/>
                    </xs:sequence>
                </xs:complexType>
                <xs:complexType name="typeB">
                    <xs:sequence>
                        <xs:element name="b" type="xs:string"/>
                    </xs:sequence>
                </xs:complexType>
                <xs:element name="root" type="typeA">
                    <xs:alternative test="@kind = 'B'" type="typeB"/>
                </xs:element>
            </xs:schema>"#,
        );

        let flags = ValidationFlags::default() | ValidationFlags::PROCESS_ASSERTIONS;
        let validator = SchemaValidator::new(&schema_set, flags);
        let mut v = validator.start_run(TestSink::new());
        let ns = empty_ns_context();

        // Bad xsi:type (unrelated to typeA) + CTA trigger attribute
        let info = v.validate_element("root", "", Some("noSuchType"), None, &ns);
        assert_eq!(info.validity, SchemaValidity::Invalid);

        // Supply CTA-triggering attribute
        v.validate_attribute("kind", "", "B");
        let eoa_info = v.validate_end_of_attributes();

        // CTA should switch to typeB, but validity should stay Invalid
        assert_eq!(
            eoa_info.validity, SchemaValidity::Invalid,
            "CTA switch should preserve prior invalidity from bad xsi:type"
        );

        // Validate content against typeB
        v.validate_element("b", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_text("hello");
        v.validate_end_element();

        v.validate_end_element(); // close root
        v.end_validation().ok();

        assert!(
            v.sink.errors.iter().any(|e| e.constraint == "cvc-elt.4.1"),
            "Expected cvc-elt.4.1 for bad xsi:type, got: {:?}",
            v.sink.errors
        );
    }

    // -----------------------------------------------------------------------
    // Reviewer finding regression tests (P1/P2)
    // -----------------------------------------------------------------------

    /// P1(a): Lax-assessment elements must assess attributes against xs:anyType's
    /// anyAttribute wildcard, not skip them entirely.
    #[test]
    fn test_lax_assessment_validates_attributes_against_any_type() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:any processContents="lax"/>
                        </xs:sequence>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"#,
        );

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);
        v.validate_end_of_attributes();

        // Undeclared element matched by lax wildcard → lax assessment, schema_type=None
        let info = v.validate_element("unknown", "", None, None, &ns);
        assert!(info.schema_type.is_none());

        // Attributes should be accepted (xs:anyType's anyAttribute lax wildcard)
        let attr_info = v.validate_attribute("myattr", "", "some-value");
        assert_ne!(
            attr_info.validity,
            SchemaValidity::Invalid,
            "Lax assessment should accept attributes via xs:anyType's anyAttribute wildcard"
        );

        v.validate_end_of_attributes();
        v.validate_end_element();
        v.validate_end_element();
        v.end_validation().ok();

        // No errors about unexpected attributes
        let attr_errors: Vec<_> = v
            .sink
            .errors
            .iter()
            .filter(|e| e.constraint.contains("cvc-complex-type"))
            .collect();
        assert!(
            attr_errors.is_empty(),
            "Lax assessment should not produce attribute errors, got: {:?}",
            attr_errors
        );
    }

    /// P1(b): Descendants of a skip wildcard must remain unassessed even when
    /// globally declared.
    #[test]
    fn test_skip_descendant_globally_declared_not_validated() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:element name="root">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:any processContents="skip"/>
                        </xs:sequence>
                    </xs:complexType>
                </xs:element>
                <xs:element name="known" type="xs:integer"/>
            </xs:schema>"#,
        );

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
        let ns = empty_ns_context();

        v.validate_element("root", "", None, None, &ns);
        v.validate_end_of_attributes();

        // Enter skipped subtree
        v.validate_element("wrapper", "", None, None, &ns);
        v.validate_end_of_attributes();

        // "known" is globally declared as xs:integer, but inside a skip subtree
        // it must remain unassessed — invalid text should NOT produce errors
        v.validate_element("known", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_text("not-an-integer");
        v.validate_end_element();

        v.validate_end_element(); // close wrapper
        v.validate_end_element(); // close root
        v.end_validation().ok();

        assert!(
            v.sink.errors.is_empty(),
            "Globally declared element inside skip subtree should not be validated, got: {:?}",
            v.sink.errors
        );
    }

    /// P2: Strict wildcard with valid xsi:type should use that type for
    /// assessment instead of rejecting with cvc-elt.1.
    #[test]
    fn test_strict_wildcard_xsi_type_supplies_governing_type() {
        let schema_set = load_schema(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
                         targetNamespace="http://test">
                <xs:complexType name="myType">
                    <xs:sequence>
                        <xs:element name="child" type="xs:string"/>
                    </xs:sequence>
                </xs:complexType>
                <xs:element name="root">
                    <xs:complexType>
                        <xs:sequence>
                            <xs:any namespace="http://other" processContents="strict"/>
                        </xs:sequence>
                    </xs:complexType>
                </xs:element>
            </xs:schema>"#,
        );

        let validator = SchemaValidator::new(&schema_set, ValidationFlags::default());
        let mut v = validator.start_run(TestSink::new());
        // Need namespace binding for xsi:type resolution
        let tns_prefix = schema_set.name_table.add("tns");
        let tns_uri = schema_set.name_table.add("http://test");
        let ns = NamespaceContextSnapshot {
            default_ns: None,
            bindings: vec![(tns_prefix, tns_uri)],
        };

        v.validate_element("root", "http://test", None, None, &ns);
        v.validate_end_of_attributes();

        // Element "foo" in http://other is NOT globally declared, matched by
        // strict wildcard. But xsi:type supplies tns:myType as governing type.
        let info = v.validate_element("foo", "http://other", Some("tns:myType"), None, &ns);
        // xsi:type supplied a valid governing type — element should be valid
        assert!(
            info.schema_type.is_some(),
            "xsi:type should supply governing type even without global declaration"
        );

        // No cvc-elt.1 error — xsi:type provided the governing type
        let elt1_errors: Vec<_> = v
            .sink
            .errors
            .iter()
            .filter(|e| e.constraint == "cvc-elt.1")
            .collect();
        assert!(
            elt1_errors.is_empty(),
            "Strict wildcard should not report cvc-elt.1 when xsi:type supplies a type, got: {:?}",
            elt1_errors
        );

        v.validate_end_of_attributes();
        v.validate_element("child", "", None, None, &ns);
        v.validate_end_of_attributes();
        v.validate_text("hello");
        v.validate_end_element();
        v.validate_end_element(); // close foo
        v.validate_end_element(); // close root
        v.end_validation().ok();

        assert!(
            v.sink.errors.is_empty(),
            "No errors expected when xsi:type supplies valid governing type, got: {:?}",
            v.sink.errors
        );
    }
}
