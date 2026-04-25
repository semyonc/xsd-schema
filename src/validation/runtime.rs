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
use crate::ids::{AttributeGroupKey, AttributeKey, ComplexTypeKey, ElementKey, IdentityConstraintKey, NameId, NotationKey, TypeKey};
use crate::namespace::context::NamespaceContextSnapshot;
use crate::namespace::qname::{parse_qname_with_snapshot, QNameError};
use crate::namespace::table::well_known;
use crate::parser::frames::{AttributeUseKind, AttributeUseResult, IdentityKind, ProcessContents};
use crate::parser::location::SourceLocation;
use crate::schema::model::DerivationSet;
use crate::schema::resolver::format_resolved_qname;
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
    NoNamespaceSchemaLocationHint, SchemaInfo, SchemaLocationHint, SchemaValidity, TypeSource,
    ValidationAttempted, ValidationFlags,
};
#[cfg(feature = "xsd11")]
use super::info::{AssertionOutcome, InheritedAttribute};

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
    #[cfg(feature = "xsd11")]
    inheritable: bool,
}

// ---------------------------------------------------------------------------
// AttributeLookup — three-state result from find_attribute_in_type
// ---------------------------------------------------------------------------

/// Result of looking up an attribute in a complex type's attribute list.
enum AttributeLookup {
    /// Found a matching attribute declaration
    /// (attr_key, type_key, fixed_value, inheritable)
    Found(Option<AttributeKey>, Option<TypeKey>, Option<String>, bool),
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
    /// Collected ID values mapped to the owner element serial.
    /// XSD 1.1 §3.17.5.2: same ID on the same owner element is allowed.
    id_values: HashMap<String, u64>,
    /// Monotonically increasing element serial counter for ID binding.
    next_element_serial: u64,
    /// Pending IDREF values: (value, location, element_path)
    pending_idrefs: Vec<(String, Option<SourceLocation>, String)>,
    /// Declared unparsed entity names from the document's DTD.
    /// When set, ENTITY/ENTITIES values are checked against this set (§3.16.4).
    unparsed_entities: Option<HashSet<String>>,
    /// Per-element scope stack of key/unique tables
    ic_scope_tables: Vec<Option<HashMap<IdentityConstraintKey, KeyTable>>>,
    /// Keyrefs whose refer target was not yet available when they deactivated.
    /// Carried upward and retried after each scope propagation.
    deferred_keyrefs: Vec<(KeyTable, Option<IdentityConstraintKey>)>,
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
    /// Final identity constraint tables after root element close (PSVI exposure).
    final_ic_tables: Option<HashMap<IdentityConstraintKey, KeyTable>>,
    /// Accumulated `xsi:schemaLocation` hints with base URI context.
    schema_location_hints: Vec<SchemaLocationHint>,
    /// Accumulated `xsi:noNamespaceSchemaLocation` hints with base URI context.
    no_namespace_schema_location_hints: Vec<NoNamespaceSchemaLocationHint>,
    /// Base URI of the instance document (set by caller for relative URI resolution).
    instance_base_uri: String,
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
            id_values: HashMap::new(),
            next_element_serial: 0,
            pending_idrefs: Vec::new(),
            unparsed_entities: None,
            ic_scope_tables: Vec::new(),
            deferred_keyrefs: Vec::new(),
            final_ic_tables: None,
            schema_location_hints: Vec::new(),
            no_namespace_schema_location_hints: Vec::new(),
            instance_base_uri: String::new(),
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

    /// Returns the final identity constraint tables after validation completes.
    ///
    /// Only available after `end_validation()` succeeds. Contains key/unique/keyref
    /// tables accumulated during the root element's validation scope.
    pub fn identity_constraint_tables(&self) -> Option<&HashMap<IdentityConstraintKey, KeyTable>> {
        self.final_ic_tables.as_ref()
    }

    /// Set the declared unparsed entity names from the document's DTD.
    ///
    /// When set, ENTITY/ENTITIES values are validated against this set per
    /// §3.16.4 String Valid clause 3: "Every ENTITY value in V is a declared
    /// entity name."
    pub fn set_unparsed_entities(&mut self, entities: HashSet<String>) {
        self.unparsed_entities = Some(entities);
    }

    /// Set the base URI of the instance document being validated.
    ///
    /// This base URI is attached to every schema-location hint collected
    /// during validation so that relative URIs can be resolved correctly
    /// when schemas are loaded later.
    pub fn set_instance_base_uri(&mut self, base_uri: impl Into<String>) {
        self.instance_base_uri = base_uri.into();
    }

    /// Returns accumulated `xsi:schemaLocation` hints.
    ///
    /// Each hint contains a namespace/location pair plus the instance base
    /// URI for resolving relative locations. Complete pairs from every
    /// `xsi:schemaLocation` attribute are included, even from attributes
    /// that failed even-token-count enforcement (the complete pairs are
    /// still valid hints). Any trailing unpaired token is ignored.
    pub fn schema_location_hints(&self) -> &[SchemaLocationHint] {
        &self.schema_location_hints
    }

    /// Returns accumulated `xsi:noNamespaceSchemaLocation` hints.
    pub fn no_namespace_schema_location_hints(&self) -> &[NoNamespaceSchemaLocationHint] {
        &self.no_namespace_schema_location_hints
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
        // Pass the SchemaSet through so navigator.typed_value() can resolve
        // bindings against the schema arenas. Without it the navigator
        // short-circuits to TypedValue::Untyped (navigator.rs:683-686),
        // and assertion XPath sees every typed attribute/element as
        // xs:untypedAtomic — value-comparison operators then fail with
        // "op:eq is not defined for xs:untypedAtomic and ...".
        match BufferDocumentBuilder::new(
            arena_ref,
            names_ref,
            Some(self.schema_set),
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

    /// Unified message format for assertion-fragment-buffer aborts.
    #[cfg(feature = "xsd11")]
    fn abort_assertion_buffer_op(&mut self, context: &str, e: impl std::fmt::Display) {
        self.abort_assertion_buffering(format!(
            "Assertion fragment buffer error ({}): {}",
            context, e
        ));
    }

    /// Install a schema binding on a buffered fragment node, aborting
    /// the buffer with a labelled error on failure. Returns `false` if
    /// the buffer was aborted, so callers can short-circuit.
    #[cfg(feature = "xsd11")]
    fn install_fragment_binding(
        &mut self,
        node_ref: u32,
        binding: crate::document::type_remap::NodeSchemaBinding,
        context: &str,
    ) -> bool {
        let Some(builder) = self.fragment_builder.as_mut() else {
            return true;
        };
        if let Err(e) = builder.set_node_binding(node_ref, binding) {
            self.abort_assertion_buffer_op(context, e);
            return false;
        }
        true
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
                        format!("Assertion fragment buffer error (start_element): {}", e),
                    );
                    return;
                }
            },
            None => return, // builder was not created (error already reported)
        };

        // Install schema binding for descendant elements so assertion
        // XPath sees their declared type. The asserter element itself
        // is left unbound — per §3.13.4.1 (note around clause 2.3.1.3),
        // it has annotation `anyType`, so `data(.)` and `string(.)`
        // yield xs:untypedAtomic; the typed value is exposed via
        // `$value`. Observable in saxonData/Assert/assert014–017.
        if has_assertions.is_none() {
            if let Some(tk) = type_key {
                let (element_decl, content_type) = self
                    .validation_stack
                    .last()
                    .map(|ev| (ev.element_decl, ev.content_type))
                    .unwrap_or((None, None));
                let binding = crate::document::type_remap::NodeSchemaBinding {
                    type_key: tk,
                    element_decl,
                    attribute_decl: None,
                    content_type,
                };
                if !self.install_fragment_binding(element_ref, binding, "element binding") {
                    return;
                }
            }
        }

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
                    self.abort_assertion_buffer_op("attribute replay", e);
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
            self.report_validation_error_to(err, &mut ev_state.error_codes);
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

    /// Push a skip-wildcard-matched element onto the stack with
    /// `process_contents=Skip`, `validity=NotKnown`, and no content-model
    /// validation. Returns the empty SchemaInfo callers propagate on skip.
    fn push_skipped_element(
        &mut self,
        local_name: NameId,
        namespace: Option<NameId>,
        ns_context: &NamespaceContextSnapshot,
    ) -> SchemaInfo {
        let mut ev_state = ElementValidationState::new(local_name, namespace);
        ev_state.ns_context = Some(ns_context.clone());
        ev_state.process_contents = ContentProcessing::Skip;
        ev_state.content_state = ContentValidatorState::Simple;
        ev_state.validity = SchemaValidity::NotKnown;
        self.push_element(ev_state);
        self.advance_constraints_start_element_skipped(local_name, namespace);
        #[cfg(feature = "xsd11")]
        self.detect_assertions_on_element(None, local_name, namespace);
        SchemaInfo::empty()
    }

    /// XSD 1.1 dynamic EDC (§3.4.6.4 / cvc-complex-type rule 5): when a
    /// wildcard accepts an element and resolves it to a governing type/decl,
    /// that governing binding must be consistent with any locally declared
    /// element binding for the same QName in the same content model. Returns
    /// `Some(reason)` on violation; the caller should mark the child invalid
    /// and emit a `cvc-complex-type.5` error after push.
    #[cfg(feature = "xsd11")]
    fn dynamic_edc_violation_reason(
        &self,
        matched_via_wildcard: bool,
        local_name: NameId,
        namespace: Option<NameId>,
        governing_type: Option<TypeKey>,
        governing_decl: Option<ElementKey>,
    ) -> Option<String> {
        if !matched_via_wildcard || !self.schema_set.is_xsd11() {
            return None;
        }
        let parent_ct = match self
            .validation_stack
            .last()
            .and_then(|p| p.schema_type)
        {
            Some(TypeKey::Complex(ct_key)) => ct_key,
            _ => return None,
        };
        match crate::schema::edc::check_dynamic_edc(
            self.schema_set,
            self.subst_groups.as_ref(),
            parent_ct,
            (namespace, local_name),
            governing_type,
            governing_decl,
        ) {
            crate::schema::edc::EdcOutcome::Mismatch { reason } => Some(reason),
            _ => None,
        }
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
                return self.push_skipped_element(local_name, namespace, ns_context);
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
                // Derive wildcard target namespace from the parent's schema
                // type, not the instance element namespace. For unqualified
                // local elements parent.namespace may be None while the
                // wildcard's ##targetNamespace should resolve to the schema
                // document's target namespace (XSD spec §3.10.4).
                let wildcard_target_ns = match parent.schema_type {
                    Some(TypeKey::Complex(ct_key)) => {
                        self.schema_set.arenas.complex_types[ct_key].target_namespace
                    }
                    _ => parent.namespace,
                };
                match parent.content_state.advance_element(
                    local_name,
                    namespace,
                    wildcard_target_ns,
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

        // Was this element matched via a content-model wildcard (or open content)?
        // Set when the matched particle was a wildcard, regardless of strict/lax/skip.
        // §3.4.6.4 dynamic EDC fires only on wildcard matches.
        #[cfg(feature = "xsd11")]
        let matched_via_wildcard = match_info
            .as_ref()
            .and_then(|i| i.process_contents)
            .is_some();

        // Determine process_contents before element_key lookup: a skip wildcard
        // must suppress the global declaration lookup (§3.10.4 cvc-wildcard).
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

        // If the content model provided a resolved type for a local element,
        // don't fall back to a global element with the same QName (it may have
        // a different type).
        let element_key = if matched_type.is_some() || process_contents == ContentProcessing::Skip {
            matched_elem_key
        } else {
            matched_elem_key
                .or_else(|| self.schema_set.lookup_element(namespace, local_name))
        };

        if element_key.is_none() {
            if content_model_accepted {
                if process_contents == ContentProcessing::Skip {
                    return self.push_skipped_element(local_name, namespace, ns_context);
                }

                // Content model accepted this element (wildcard in content model)
                // but no global declaration exists.
                let is_nil = matches!(xsi_nil, Some("true") | Some("1"));
                let mut ev_state = ElementValidationState::new(local_name, namespace);
                ev_state.ns_context = Some(ns_context.clone());
                ev_state.validity = SchemaValidity::Valid;
                ev_state.process_contents = process_contents;
                ev_state.is_nil = is_nil;

                let mut wildcard_xsi_type_errors = Vec::new();
                if let Some(mut type_key) = matched_type {
                    // PATH B1: xsi:type override for local elements with resolved type
                    let mut b1_type_source = TypeSource::Declaration;
                    if let Some(xsi_type_str) = xsi_type {
                        match self.resolve_xsi_type(xsi_type_str, Some(type_key), DerivationSet::empty(), ns_context, &mut wildcard_xsi_type_errors) {
                            XsiTypeOutcome::Applied(overridden) => {
                                type_key = overridden;
                                b1_type_source = TypeSource::XsiType;
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
                    ev_state.type_source = Some(b1_type_source);
                    ev_state.content_state = content_state;
                    ev_state.content_type = Some(content_type);
                } else {
                    // PATH B2/B3: No declaration and no matched type.
                    // Try xsi:type first — it can supply a governing type even
                    // without a declaration.
                    if let Some(xsi_type_str) = xsi_type {
                        match self.resolve_xsi_type(xsi_type_str, None, DerivationSet::empty(), ns_context, &mut wildcard_xsi_type_errors) {
                            XsiTypeOutcome::Applied(overridden) => {
                                let (content_state, content_type) =
                                    self.init_content_model(Some(overridden));
                                ev_state.schema_type = Some(overridden);
                                ev_state.type_source = Some(TypeSource::XsiType);
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
                    // Error deferred until after push so it lands on the child.
                    if process_contents == ContentProcessing::Strict
                        && ev_state.schema_type.is_none()
                    {
                        ev_state.validity = SchemaValidity::Invalid;
                    }
                }

                ev_state.strictly_assessed = (ev_state.element_decl.is_some() || ev_state.schema_type.is_some())
                    && ev_state.process_contents != ContentProcessing::Skip;
                // §3.4.6.4 dynamic EDC for the no-element-key wildcard branch:
                // even without a governing global declaration, an xsi:type may
                // supply a governing type whose binding must agree with any
                // QName-equal local element declaration in the parent CT.
                #[cfg(feature = "xsd11")]
                let edc_violation_b = self.dynamic_edc_violation_reason(
                    matched_via_wildcard,
                    local_name,
                    namespace,
                    ev_state.schema_type,
                    None,
                );
                #[cfg(feature = "xsd11")]
                if edc_violation_b.is_some() {
                    ev_state.validity = SchemaValidity::Invalid;
                }
                let schema_type = ev_state.schema_type;
                let content_type = ev_state.content_type;
                let validity = ev_state.validity;
                let type_source = ev_state.type_source;
                let needs_undeclared_error = process_contents == ContentProcessing::Strict
                    && schema_type.is_none();
                self.push_element(ev_state);
                #[cfg(feature = "xsd11")]
                if let Some(reason) = edc_violation_b {
                    let elem_name = self.schema_set.name_table.resolve(local_name).to_string();
                    self.report_error(
                        "cvc-complex-type.5",
                        format!(
                            "Element '{}' matched a wildcard but its governing type is \
                             inconsistent with the local element declaration in the \
                             parent's content model: {}",
                            elem_name, reason,
                        ),
                    );
                }
                // Emit deferred xsi:type errors now that the child is on the stack
                self.emit_deferred_xsi_type_errors(wildcard_xsi_type_errors);
                if needs_undeclared_error {
                    let elem_name = self.schema_set.name_table.resolve(local_name);
                    self.report_error(
                        "cvc-elt.1",
                        format!("Element '{}' is not declared", elem_name),
                    );
                }
                self.advance_constraints_start_element(local_name, namespace, None);
                #[cfg(feature = "xsd11")]
                self.detect_assertions_on_element(schema_type, local_name, namespace);
                return SchemaInfo {
                    element_decl: None,
                    attribute_decl: None,
                    schema_type,
                    member_type: None,
                    validity,
                    validation_attempted: ValidationAttempted::None,
                    is_default: false,
                    is_nil,
                    content_type,
                    typed_value: None,
                    normalized_value: None,
                    schema_error_codes: Vec::new(),
                    notation: None,
                    deferred_by_cta: false,
                    type_source,
                    #[cfg(feature = "xsd11")]
                    cta_selected: false,
                    #[cfg(feature = "xsd11")]
                    assertion_outcome: None,
                };
            }

            match process_contents {
                ContentProcessing::Skip => {
                    // Skip validation entirely
                    let mut ev_state = ElementValidationState::new(local_name, namespace);
                ev_state.ns_context = Some(ns_context.clone());
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
                ev_state.ns_context = Some(ns_context.clone());
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
                    let mut ev_state = ElementValidationState::new(local_name, namespace);
                ev_state.ns_context = Some(ns_context.clone());
                    ev_state.validity = SchemaValidity::Invalid;
                    // Lax assessment for content (same PSVI as lax when no declaration found)
                    let (content_state, content_type) = self.lax_assessment_content_model();
                    ev_state.content_state = content_state;
                    ev_state.content_type = Some(content_type);
                    // schema_type stays None
                    self.push_element(ev_state);
                    // Report error AFTER push so code lands on the child, not parent
                    let elem_name = self.schema_set.name_table.resolve(local_name);
                    self.report_error(
                        "cvc-elt.1",
                        format!("Element '{}' is not declared", elem_name),
                    );
                    self.advance_constraints_start_element(local_name, namespace, None);
                    #[cfg(feature = "xsd11")]
                    self.detect_assertions_on_element(None, local_name, namespace);
                    return SchemaInfo::invalid();
                }
            }
        }

        let elem_key = element_key.unwrap();
        let elem_data = &self.schema_set.arenas.elements[elem_key];

        // Check abstract (deferred until after push)
        let is_abstract = elem_data.is_abstract;

        // 5. Resolve type from element declaration
        let mut type_key = elem_data.resolved_type;

        // 6. xsi:type override
        // Errors are deferred and emitted after push so they land on the child element.
        let (effective_block, _) =
            crate::compiler::substitution::effective_element_constraints(self.schema_set, elem_data);
        // Mask to element-relevant bits only (extension, restriction, substitution)
        // to avoid spuriously blocking list/union derivation steps.
        let effective_block = effective_block.element_block_mask();
        let mut xsi_type_deferred_errors = Vec::new();
        let mut xsi_type_invalid = false;
        let mut type_source = TypeSource::Declaration;
        if let Some(xsi_type_str) = xsi_type {
            // Default to anyType when no explicit type is declared (cvc-elt.4.3 still applies)
            let declared_for_xsi = type_key.or(Some(TypeKey::Complex(self.schema_set.any_type_key())));
            match self.resolve_xsi_type(xsi_type_str, declared_for_xsi, effective_block, ns_context, &mut xsi_type_deferred_errors) {
                XsiTypeOutcome::Applied(overridden) => {
                    type_key = Some(overridden);
                    type_source = TypeSource::XsiType;
                }
                XsiTypeOutcome::Unresolved | XsiTypeOutcome::InvalidDerivation => {
                    xsi_type_invalid = true;
                }
            }
        }

        // §3.3.4.4 cvc-type clause 2: if T is a complex type definition, T.{abstract} must be false.
        // Hoist the complex-type fetch once so the cvc-type.2 error path below can
        // reuse the name/target_namespace without re-resolving the arena entry.
        let abstract_ct_info: Option<(Option<NameId>, Option<NameId>)> =
            if !xsi_type_invalid {
                if let Some(TypeKey::Complex(k)) = type_key {
                    self.schema_set.arenas.complex_types.get(k)
                        .filter(|ct| ct.is_abstract)
                        .map(|ct| (ct.name, ct.target_namespace))
                } else {
                    None
                }
            } else {
                None
            };
        let abstract_type_invalid = abstract_ct_info.is_some();

        // 7. xsi:nil
        let is_nil = if let Some(nil_str) = xsi_nil {
            nil_str == "true" || nil_str == "1"
        } else {
            false
        };
        let nillable_violation = is_nil && !elem_data.nillable;

        // 8. Initialize content model and determine ContentType
        let (content_state, content_type) = self.init_content_model(type_key);

        // §3.4.6.4 dynamic EDC: when this element was matched by a wildcard
        // (not directly by an element particle), the resolved governing decl
        // and effective type must be consistent with any QName-equal local
        // element declaration in the parent CT.
        #[cfg(feature = "xsd11")]
        let edc_violation_a = self.dynamic_edc_violation_reason(
            matched_via_wildcard,
            local_name,
            namespace,
            type_key,
            Some(elem_key),
        );

        // 9. Push ElementValidationState
        let mut ev_state = ElementValidationState::new(local_name, namespace);
                ev_state.ns_context = Some(ns_context.clone());
        ev_state.element_decl = Some(elem_key);
        ev_state.schema_type = type_key;
        ev_state.type_source = Some(type_source);
        ev_state.content_state = content_state;
        ev_state.content_type = Some(content_type);
        ev_state.is_nil = is_nil;
        ev_state.validity = if xsi_type_invalid || abstract_type_invalid {
            SchemaValidity::Invalid
        } else {
            SchemaValidity::Valid
        };
        #[cfg(feature = "xsd11")]
        if edc_violation_a.is_some() {
            ev_state.validity = SchemaValidity::Invalid;
        }
        ev_state.process_contents = process_contents;
        // Strictly assessed: has governing declaration or type, and not skipped
        ev_state.strictly_assessed = (ev_state.element_decl.is_some() || ev_state.schema_type.is_some())
            && process_contents != ContentProcessing::Skip;
        #[cfg(feature = "xsd11")]
        {
            ev_state.has_type_alternatives = !self.schema_set.arenas.elements[elem_key]
                .alternatives.is_empty();
        }
        self.push_element(ev_state);
        #[cfg(feature = "xsd11")]
        if let Some(reason) = edc_violation_a.as_ref() {
            let elem_name = self.schema_set.name_table.resolve(local_name).to_string();
            self.report_error(
                "cvc-complex-type.5",
                format!(
                    "Element '{}' matched a wildcard but its governing element declaration \
                     is inconsistent with the local element declaration in the parent's \
                     content model: {}",
                    elem_name, reason,
                ),
            );
        }

        // Emit deferred xsi:type errors now that the child is on the stack
        self.emit_deferred_xsi_type_errors(xsi_type_deferred_errors);

        // Report deferred element-start errors AFTER push so codes land on the child
        if is_abstract {
            let elem_name = self.schema_set.name_table.resolve(local_name);
            self.report_error(
                "cvc-elt.2",
                format!("Element '{}' is abstract and cannot appear in instances", elem_name),
            );
        }
        if let Some((ct_name, ct_ns)) = abstract_ct_info {
            let type_name = crate::schema::derivation::format_type_name(
                self.schema_set, ct_name, ct_ns,
            );
            self.report_error(
                "cvc-type.2",
                format!("Type '{}' is abstract and cannot be used to validate an element", type_name),
            );
        }
        if nillable_violation {
            let elem_name = self.schema_set.name_table.resolve(local_name);
            self.report_error(
                "cvc-elt.3.1",
                format!(
                    "Element '{}' is not nillable but xsi:nil='true' was specified",
                    elem_name,
                ),
            );
        }

        self.advance_constraints_start_element(local_name, namespace, Some(elem_key));

        // 9b. Assertion detection hook (XSD 1.1)
        #[cfg(feature = "xsd11")]
        self.detect_assertions_on_element(type_key, local_name, namespace);

        // 10. Return SchemaInfo
        #[allow(unused_mut)]
        let mut validity = if xsi_type_invalid || abstract_type_invalid {
            SchemaValidity::Invalid
        } else {
            SchemaValidity::Valid
        };
        #[cfg(feature = "xsd11")]
        if edc_violation_a.is_some() {
            validity = SchemaValidity::Invalid;
        }
        SchemaInfo {
            element_decl: Some(elem_key),
            attribute_decl: None,
            schema_type: type_key,
            member_type: None,
            validity,
            validation_attempted: ValidationAttempted::None,
            is_default: false,
            is_nil,
            content_type: Some(content_type),
            typed_value: None,
            normalized_value: None,
            schema_error_codes: Vec::new(),
            notation: None,
            deferred_by_cta: false,
            type_source: Some(type_source),
            #[cfg(feature = "xsd11")]
            cta_selected: false,
            #[cfg(feature = "xsd11")]
            assertion_outcome: None,
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

        // Forward attribute to assertion fragment builder (XSD 1.1).
        // Done before ev_state borrow to avoid borrow conflict.
        // The returned attr_ref is stashed so we can install the schema
        // binding (type/decl/content_type) once validation has resolved
        // the attribute's type — without a binding, navigator.typed_value()
        // returns Untyped and assertion XPath value-comparisons reject the
        // attribute as xs:untypedAtomic (§3.13.4.1, §3.5).
        #[cfg(feature = "xsd11")]
        let fragment_attr_ref: Option<u32> = if self.is_buffering_assertions() {
            let local = self.schema_set.name_table.resolve(local_name);
            let ns = namespace
                .map(|id| self.schema_set.name_table.resolve(id).to_string())
                .unwrap_or_default();
            let attempt = self
                .fragment_builder
                .as_mut()
                .map(|b| b.attribute(&local, &ns, "", value));
            match attempt {
                Some(Ok(r)) => Some(r),
                Some(Err(e)) => {
                    self.abort_assertion_buffer_op("attribute", e);
                    None
                }
                None => None,
            }
        } else {
            None
        };

        // Detect xml:base unconditionally (regardless of ALLOW_XML_ATTRIBUTES)
        // and update the current element's base URI for schema-location hints.
        // xml:base is compositional: relative values resolve against the
        // inherited base URI (RFC 3986 §5).
        // Only apply if this is the first xml:base on this element (skip
        // duplicates so an invalid repeated xml:base doesn't overwrite).
        let mut xml_base_rebase = None;
        if namespace == Some(well_known::XML_NAMESPACE)
            && local_name == self.schema_set.name_table.add("base")
        {
            if let Some(ev) = self.validation_stack.last_mut() {
                if !ev.base_uri_set_by_xml_base {
                    let xml_base = value.trim();
                    let base_uri = resolve_base_uri(xml_base, &ev.base_uri);
                    ev.base_uri = base_uri.clone();
                    ev.base_uri_set_by_xml_base = true;
                    xml_base_rebase = Some((
                        base_uri,
                        ev.schema_location_hint_start,
                        ev.no_namespace_schema_location_hint_start,
                    ));
                }
            }
        }
        if let Some((base_uri, sl_start, nnsl_start)) = xml_base_rebase {
            self.rebase_hint_range(sl_start, nnsl_start, &base_uri);
        }

        let ev_state = match self.validation_stack.last_mut() {
            Some(s) => s,
            None => {
                self.report_error("cvc-complex-type", "No element context for attribute");
                return SchemaInfo::invalid();
            }
        };

        // Validate xsi:* built-in attributes with proper type information
        if namespace == Some(well_known::XSI_NAMESPACE) {
            self.current_state = ValidatorState::Attribute;
            let ec_snapshot = self
                .validation_stack
                .last()
                .map(|ev| ev.error_codes.len())
                .unwrap_or(0);
            let mut result = self.validate_xsi_attribute(local_name, value);
            if let Some(ev) = self.validation_stack.last_mut() {
                // Extract attribute-specific error codes (mirrors normal attribute path)
                if ev.error_codes.len() > ec_snapshot {
                    result.schema_error_codes = ev.error_codes[ec_snapshot..].to_vec();
                    ev.error_codes.truncate(ec_snapshot);
                }
                // Track attribute [validation attempted] on parent element (§3.3.5.1)
                match result.validation_attempted {
                    ValidationAttempted::Full => {
                        ev.any_attr_not_none = true;
                    }
                    ValidationAttempted::None => {
                        ev.any_attr_not_full = true;
                    }
                    ValidationAttempted::Partial => {
                        ev.any_attr_not_full = true;
                        ev.any_attr_not_none = true;
                    }
                }
            }
            // XSD IC field XPaths (e.g. @*) match all attributes including xsi:*.
            // Feed xsi: attributes to IC field matching so that a field selecting
            // @* on an element with both schema and xsi: attributes correctly
            // detects the multi-node condition (cvc-identity-constraint.4.2.1).
            self.post_process_attribute(local_name, namespace, value, &result);
            #[cfg(feature = "xsd11")]
            self.bind_fragment_attribute(fragment_attr_ref, &result);
            return result;
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
            let mut result = SchemaInfo::invalid();
            result.schema_error_codes = vec!["cvc-complex-type.3"];
            return result;
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

        // When the element is skip-processed (matched by a processContents="skip"
        // xs:any wildcard), accept all attributes without schema validation.
        // The element is not assessed, so its attributes cannot generate cvc-complex-type
        // errors regardless of whether the element has a global declaration.
        if process_contents == ContentProcessing::Skip {
            self.current_state = ValidatorState::Attribute;
            let result = SchemaInfo::empty();
            self.post_process_attribute(local_name, namespace, value, &result);
            return result;
        }

        let ct_key = match type_key {
            Some(TypeKey::Complex(ct)) => ct,
            None if process_contents != ContentProcessing::Skip => {
                // Lax assessment: validate attributes against xs:anyType
                // (anyAttribute processContents=lax accepts any attribute)
                self.schema_set.any_type_key()
            }
            _ => {
                // Simple type: no attributes expected (except xsi:*)
                // Still run post-processing so IC attribute fields and
                // ID/IDREF collection are not skipped.
                self.current_state = ValidatorState::Attribute;
                let result = SchemaInfo::empty();
                self.post_process_attribute(local_name, namespace, value, &result);
                return result;
            }
        };

        self.current_state = ValidatorState::Attribute;
        // Snapshot error_codes so we can extract attribute-specific codes
        let ec_snapshot = self.validation_stack.last().map_or(0, |ev| ev.error_codes.len());
        let mut result = self.validate_attribute_against_type(ct_key, local_name, namespace, value);
        // Slice attribute-specific error codes and remove from parent
        if let Some(ev) = self.validation_stack.last_mut() {
            if ev.error_codes.len() > ec_snapshot {
                result.schema_error_codes = ev.error_codes[ec_snapshot..].to_vec();
                ev.error_codes.truncate(ec_snapshot);
            }
            // Track attribute [validation attempted] on parent element
            match result.validation_attempted {
                ValidationAttempted::Full => { ev.any_attr_not_none = true; }
                ValidationAttempted::None => { ev.any_attr_not_full = true; }
                ValidationAttempted::Partial => {
                    ev.any_attr_not_full = true;
                    ev.any_attr_not_none = true;
                }
            }
        }
        #[cfg(feature = "xsd11")]
        self.bind_fragment_attribute(fragment_attr_ref, &result);
        result
    }

    /// Install a schema binding (type/decl) on the buffered fragment
    /// attribute node so navigator.typed_value() returns the declared
    /// atomic type during XSD 1.1 assertion XPath evaluation. No-op
    /// when no fragment buffering is active or no type was resolved.
    #[cfg(feature = "xsd11")]
    fn bind_fragment_attribute(&mut self, attr_ref: Option<u32>, info: &SchemaInfo) {
        let Some(attr_ref) = attr_ref else { return };
        let Some(type_key) = info.schema_type else { return };
        let binding = crate::document::type_remap::NodeSchemaBinding {
            type_key,
            element_decl: None,
            attribute_decl: info.attribute_decl,
            content_type: None,
        };
        let _ = self.install_fragment_binding(attr_ref, binding, "attr binding");
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
        let (schema_type, cta_switched, cta_selected) = if has_type_alternatives {
            let mut st = schema_type;
            let mut switched = false;
            let mut selected = false;
            if let Some(ev_state) = self.validation_stack.last() {
                if let Some(elem_key) = ev_state.element_decl {
                    // §3.12.4 clause 1.1.3: include [inherited attributes]
                    // that do not have the same expanded name as any of
                    // E's [attributes] when building the CTA XDM instance.
                    // Use incoming_inherited (the PSVI view) with the
                    // CTA-specific same-name exclusion.
                    let mut cta_attrs = ev_state.collected_attributes.clone();
                    let explicit_names: HashSet<(Option<NameId>, NameId)> = cta_attrs
                        .iter()
                        .map(|(ns, name, _)| (*ns, *name))
                        .collect();
                    for ((ns, name), val) in &ev_state.incoming_inherited {
                        if !explicit_names.contains(&(*ns, *name)) {
                            cta_attrs.push((*ns, *name, val.value.clone()));
                        }
                    }
                    let new_type = super::alternatives::evaluate_type_alternatives(
                        elem_key,
                        ev_state.local_name,
                        ev_state.namespace,
                        &cta_attrs,
                        self.schema_set,
                    );
                    if let Some(new_type_key) = new_type {
                        selected = true;
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
            // Track CTA selection on ev_state regardless of type change.
            // Preserve XsiType as the governing source when xsi:type was
            // applied — per spec xsi:type takes precedence over CTA.
            if selected {
                if let Some(ev) = self.validation_stack.last_mut() {
                    ev.cta_selected = true;
                    if ev.type_source != Some(TypeSource::XsiType) {
                        ev.type_source = Some(TypeSource::TypeAlternative);
                    }
                }
            }
            (st, switched, selected)
        } else {
            (schema_type, false, false)
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

        // Record inheritable attributes with default values for propagation (XSD 1.1)
        #[cfg(feature = "xsd11")]
        if let Some(TypeKey::Complex(ct_key)) = schema_type {
            self.record_inheritable_defaults(ct_key);
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

        // Process default/fixed attribute values not explicitly provided in the
        // instance. A single pass handles both IC field matching (§3.11.4) and
        // ID/IDREF collection (§3.3.4 cvc-id.2) to avoid iterating twice.
        if let Some(TypeKey::Complex(ct_key)) = schema_type {
            let ct_data = &self.schema_set.arenas.complex_types[ct_key];
            let empty_seen = HashSet::new();
            let seen = self.validation_stack.last()
                .map(|s| &s.seen_attributes)
                .unwrap_or(&empty_seen);
            let has_ic = !self.active_constraints.is_empty();
            let builtin = self.schema_set.builtin_types();
            let id_key = builtin.get_by_type_code(XmlTypeCode::Id);
            let idref_key = builtin.get_by_type_code(XmlTypeCode::IdRef);
            let idrefs_key = builtin.get_by_type_code(XmlTypeCode::IdRefs);
            let entity_key = builtin.get_by_type_code(XmlTypeCode::Entity);
            let entities_key = builtin.get_by_type_code(XmlTypeCode::Entities);
            let mut ic_defaults: Vec<(NameId, NameId, String)> = Vec::new();
            let mut id_defaults: Vec<(String, TypeKey)> = Vec::new();
            for (i, attr_use) in ct_data.attributes.iter().enumerate() {
                if attr_use.use_kind == AttributeUseKind::Prohibited {
                    continue;
                }
                let resolved = ct_data.resolved_attributes.get(i);
                let (attr_name, attr_ns) =
                    self.resolve_attr_use_name_ns(attr_use, resolved, ct_data.target_namespace);
                if seen.contains(&(attr_ns, attr_name)) {
                    continue;
                }
                let attr_key = resolved.and_then(|r| r.resolved_ref);
                let ref_decl = attr_key
                    .and_then(|k| self.schema_set.arenas.attributes.get(k));
                let value = attr_use.attribute.default_value.as_deref()
                    .or(attr_use.attribute.fixed_value.as_deref())
                    .or_else(|| {
                        ref_decl.and_then(|d| d.default_value.as_deref().or(d.fixed_value.as_deref()))
                    });
                let Some(v) = value else { continue };
                if has_ic {
                    ic_defaults.push((attr_name, attr_ns.unwrap_or(NameId(0)), v.to_string()));
                }
                // Check if this attribute's type is ID/IDREF/IDREFS/ENTITY/ENTITIES
                // (need to validate defaults for these types).
                let attr_type = resolved.and_then(|r| r.resolved_type)
                    .or_else(|| ref_decl.and_then(|d| d.resolved_type));
                let needs_default_validation = match attr_type {
                    Some(TypeKey::Simple(sk)) =>
                        id_key == Some(sk) || idref_key == Some(sk) || idrefs_key == Some(sk)
                        || entity_key == Some(sk) || entities_key == Some(sk),
                    _ => false,
                };
                if needs_default_validation {
                    if let Some(tk) = attr_type {
                        id_defaults.push((v.to_string(), tk));
                    }
                }
            }
            // Feed IC field matches (borrow-split: active_constraints borrows &mut self)
            let mut multi_node_defaults: Vec<(NameId, usize)> = Vec::new();
            for (name, ns, value) in ic_defaults {
                for cs in &mut self.active_constraints {
                    let matches = cs.matching_fields(name, ns);
                    for field_idx in matches {
                        let already_matched = cs.set_field_value(field_idx, value.clone(), None);
                        if already_matched {
                            multi_node_defaults.push((cs.key_table.constraint_name, field_idx));
                        }
                    }
                }
            }
            for (constraint_name, field_idx) in multi_node_defaults {
                let cname = self.schema_set.name_table.resolve(constraint_name).to_string();
                self.report_error(
                    "cvc-identity-constraint.4.2.1",
                    format!(
                        "Identity constraint '{}': field {} matches more than one node",
                        cname,
                        field_idx + 1
                    ),
                );
            }
            // Validate and collect ID/IDREF values from absent defaults.
            // Owner is the current element (attributes bind to their element).
            let default_owner = self.validation_stack.last()
                .map(|e| e.element_serial).unwrap_or(0);
            for (value, type_key) in id_defaults {
                if let Ok(result) = super::simple::validate_simple_type(&value, type_key, self.schema_set) {
                    self.collect_id_idref(&result.typed_value, &value, default_owner);
                    self.check_entity_declared(&result.typed_value);
                }
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

        // When CTA switched the type or selected a type, return updated SchemaInfo
        // so callers (e.g. typed_builder) can update element bindings. Preserve prior
        // invalidity (e.g. from a bad xsi:type).
        if cta_switched {
            let ev = self.validation_stack.last();
            let content_type = ev.and_then(|s| s.content_type);
            let validity = ev
                .map(|s| s.validity)
                .unwrap_or(SchemaValidity::NotKnown);
            return SchemaInfo {
                schema_type,
                content_type,
                validity,
                type_source: ev.and_then(|s| s.type_source),
                #[cfg(feature = "xsd11")]
                cta_selected: ev.map(|s| s.cta_selected).unwrap_or(false),
                #[cfg(feature = "xsd11")]
                assertion_outcome: None,
                ..SchemaInfo::empty()
            };
        }
        #[cfg(feature = "xsd11")]
        if cta_selected {
            let ev = self.validation_stack.last();
            let content_type = ev.and_then(|s| s.content_type);
            let validity = ev
                .map(|s| s.validity)
                .unwrap_or(SchemaValidity::NotKnown);
            return SchemaInfo {
                schema_type,
                content_type,
                validity,
                type_source: ev.and_then(|s| s.type_source),
                cta_selected: true,
                assertion_outcome: None,
                ..SchemaInfo::empty()
            };
        }
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
            }

            // 2. Check xsi:nil — per cvc-elt.3.2.1 (XSD 1.1 §3.3.4.3),
            // a nilled element's children must be empty: no element info items
            // and no character info items. For mixed content this includes
            // whitespace, which is otherwise significant (only ignorable in
            // element-only content).
            if ev_state.is_nil
                && (has_non_ws
                    || matches!(ev_state.content_type, Some(ContentType::Mixed)))
                && !text.is_empty()
            {
                let elem_name = self
                    .schema_set
                    .name_table
                    .resolve(ev_state.local_name)
                    .to_string();
                pending_errors.push((
                    "cvc-elt.3.2.1",
                    format!(
                        "Element '{}' is nilled but has non-empty content",
                        elem_name,
                    ),
                ));
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

        let mut nil_violation: Option<String> = None;
        if let Some(ev_state) = self.validation_stack.last_mut() {
            // cvc-elt.3.2.1: a nilled element must have empty content. For
            // mixed and text-only content whitespace is significant, so any
            // text (including whitespace) is invalid. Element-only and empty
            // content treat whitespace as ignorable, so it stays accepted.
            if ev_state.is_nil
                && !text.is_empty()
                && matches!(
                    ev_state.content_type,
                    Some(ContentType::TextOnly) | Some(ContentType::Mixed)
                )
            {
                let elem_name = self
                    .schema_set
                    .name_table
                    .resolve(ev_state.local_name)
                    .to_string();
                nil_violation = Some(format!(
                    "Element '{}' is nilled but has non-empty content",
                    elem_name,
                ));
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

        if let Some(msg) = nil_violation {
            self.report_error("cvc-elt.3.2.1", msg);
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
                        let err = errors::error(
                            "cvc-complex-type.2.4",
                            format!(
                                "Element '{}' content model is incomplete: expected more child elements",
                                elem_name,
                            ),
                            self.current_location.clone(),
                        );
                        self.report_validation_error_to(err, &mut ev_state.error_codes);
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
                    } else if let Some(fixed_value) = &elem_data.fixed_value {
                        // XSD spec §3.3.4.3: "If fixed is specified, then the
                        // element's content must either be empty, in which case
                        // fixed behaves as default, or match fixed."
                        ev_state.text_content = fixed_value.clone();
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
                        ev_state.normalized_value = result.normalized_value;
                    }
                    Err(err) => {
                        self.report_validation_error_to(err, &mut ev_state.error_codes);
                        ev_state.validity = SchemaValidity::Invalid;
                    }
                }
            }

            // Check fixed value on element — cvc-elt.5.2.2.2.2 (§3.3.4.3).
            // Use value-space comparison so that lexically-different but value-equivalent
            // forms (e.g. boolean "1" vs "true", float "1.0" vs "1.000", token whitespace)
            // are treated as matching. Reuses `ev_state.typed_value` parsed above
            // (line ~1778) to avoid a second parse of the same text content.
            if let Some(elem_key) = ev_state.element_decl {
                let elem_data = &self.schema_set.arenas.elements[elem_key];
                if let Some(fixed) = &elem_data.fixed_value {
                    let matches = if let Some(ref typed) = ev_state.typed_value {
                        super::simple::fixed_matches_typed(
                            &ev_state.text_content,
                            typed,
                            fixed,
                            ev_state.schema_type,
                            self.schema_set,
                        )
                    } else {
                        super::simple::fixed_values_equal(
                            &ev_state.text_content,
                            fixed,
                            ev_state.schema_type,
                            self.schema_set,
                        )
                    };
                    if !matches {
                        let elem_name =
                            self.schema_set.name_table.resolve(ev_state.local_name);
                        let err = errors::error(
                            "cvc-elt.5.2.2",
                            format!(
                                "Element '{}' has fixed value '{}' but actual value is '{}'",
                                elem_name, fixed, ev_state.text_content,
                            ),
                            self.current_location.clone(),
                        );
                        self.report_validation_error_to(err, &mut ev_state.error_codes);
                        ev_state.validity = SchemaValidity::Invalid;
                    }
                }
            }
        }

        // 2b. Fixed value check for mixed-content elements — cvc-elt.5.2.2.1 + 5.2.2.2.1 (§3.3.4.3).
        if ev_state.content_type == Some(ContentType::Mixed) && !ev_state.is_nil {
            if let Some(elem_key) = ev_state.element_decl {
                let elem_data = &self.schema_set.arenas.elements[elem_key];
                if let Some(ref fixed) = elem_data.fixed_value {
                    let elem_name = self.schema_set.name_table.resolve(ev_state.local_name);
                    if ev_state.has_element_children {
                        // cvc-elt.5.2.2.1: no element children when fixed value is present
                        let err = errors::error(
                            "cvc-elt.5.2.2.1",
                            format!(
                                "Element '{}' has fixed value '{}' but contains element children",
                                elem_name, fixed,
                            ),
                            self.current_location.clone(),
                        );
                        self.report_validation_error_to(err, &mut ev_state.error_codes);
                        ev_state.validity = SchemaValidity::Invalid;
                    } else if ev_state.has_text && ev_state.text_content != *fixed {
                        // cvc-elt.5.2.2.2.1: initial value (concatenated text) must match fixed
                        let err = errors::error(
                            "cvc-elt.5.2.2.2",
                            format!(
                                "Element '{}' has fixed value '{}' but actual value is '{}'",
                                elem_name, fixed, ev_state.text_content,
                            ),
                            self.current_location.clone(),
                        );
                        self.report_validation_error_to(err, &mut ev_state.error_codes);
                        ev_state.validity = SchemaValidity::Invalid;
                    }
                }
            }
        }

        // 2c. Assertion evaluation hook (XSD 1.1)
        #[cfg(feature = "xsd11")]
        let type_has_assertions = matches!(ev_state.schema_type,
            Some(TypeKey::Complex(ct_key)) if has_inherited_assertions(ct_key, &self.schema_set.arenas));
        #[cfg(feature = "xsd11")]
        let mut assertion_outcome: Option<AssertionOutcome> = None;

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
                if type_has_assertions {
                    assertion_outcome = Some(AssertionOutcome::NotEvaluated);
                }
                break 'assertion_eval;
            }

            // Forward end_element to builder
            if let Some(builder) = self.fragment_builder.as_mut() {
                if let Err(e) = builder.end_element() {
                    self.abort_assertion_buffer_op("end_element", e);
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
                            if errs.is_empty() {
                                assertion_outcome = Some(AssertionOutcome::Passed);
                            } else {
                                assertion_outcome = Some(AssertionOutcome::Failed);
                            }
                            self.report_assertion_errors(errs, &mut ev_state);
                        }
                        Err(e) => {
                            // Clear stale deferred frames — they reference
                            // nodes in the failed document and must not leak
                            // into the next buffered subtree.
                            self.pending_assertion_frames.clear();
                            let err = errors::error(
                                "cvc-assertion",
                                format!(
                                    "Failed to finalize assertion fragment: {}",
                                    e
                                ),
                                self.current_location.clone(),
                            );
                            self.report_validation_error_to(err, &mut ev_state.error_codes);
                            ev_state.validity = SchemaValidity::Invalid;
                            assertion_outcome = Some(AssertionOutcome::Failed);
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
                    assertion_outcome = Some(AssertionOutcome::NotEvaluated);
                }
            }
        }

        // Fallback: if assertion_outcome wasn't set but the type has assertions
        #[cfg(feature = "xsd11")]
        if assertion_outcome.is_none() && type_has_assertions {
            assertion_outcome = Some(AssertionOutcome::NotEvaluated);
        }

        // 3. Identity constraint processing (field values + scope exit + keyref cross-ref)
        let is_complex_content = matches!(
            ev_state.content_type,
            Some(ContentType::ElementOnly) | Some(ContentType::Mixed)
        );
        // Resolve QName/NOTATION typed values for IC comparison (namespace-aware).
        // Use a separate copy to preserve the original PSVI typed_value.
        let ic_typed_value = Self::resolve_ic_qname_value(
            &ev_state.typed_value,
            &ev_state.text_content,
            ev_state.ns_context.as_ref(),
            &self.schema_set.name_table,
        );
        let ic_ref = ic_typed_value.as_ref().or(ev_state.typed_value.as_ref());
        self.process_constraints_end_element(
            &ev_state.text_content,
            ic_ref,
            ev_state.is_nil,
            is_complex_content,
            &mut ev_state.error_codes,
        );

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
            } else {
                // Root element — save for public access via identity_constraint_tables()
                self.final_ic_tables = Some(scope_map);
            }
        }

        // 3c. Retry deferred keyrefs against enriched parent scope
        if !self.deferred_keyrefs.is_empty() {
            let pending = std::mem::take(&mut self.deferred_keyrefs);
            let name_table = &self.schema_set.name_table;
            let scope_empty = self.ic_scope_tables.is_empty();
            let mut still_deferred = Vec::new();
            let mut deferred_errors = Vec::new();
            for (keyref_table, refer_key) in pending {
                let target = refer_key.and_then(|rk| {
                    self.ic_scope_tables
                        .last()
                        .and_then(|slot| slot.as_ref())
                        .and_then(|map| map.get(&rk))
                });
                match target {
                    Some(target_table) => {
                        let errs =
                            keyref_table.check_keyref_against(target_table, name_table);
                        deferred_errors.extend(errs);
                    }
                    None => {
                        if scope_empty {
                            // Root element closed — no more ancestors to try
                            let keyref_name =
                                name_table.resolve(keyref_table.constraint_name);
                            let refer_display = self
                                .compiled_constraints
                                .get(&keyref_table.ic_key)
                                .and_then(|opt| opt.as_ref())
                                .and_then(|compiled| {
                                    compiled.refer.as_ref().map(|refer| {
                                        let refer_ns =
                                            refer.namespace.or(compiled.target_namespace);
                                        format_resolved_qname(
                                            name_table, refer_ns, refer.local_name,
                                        )
                                    })
                                })
                                .unwrap_or_else(|| "<unknown>".to_string());
                            deferred_errors.push(errors::error(
                                "cvc-identity-constraint.4.3",
                                format!(
                                    "Keyref '{}' references unknown constraint '{}'",
                                    keyref_name, refer_display
                                ),
                                None,
                            ));
                        } else {
                            still_deferred.push((keyref_table, refer_key));
                        }
                    }
                }
            }
            self.deferred_keyrefs = still_deferred;
            // Emit deferred errors directly through the sink (not emit_error) because
            // the original keyref element has already been popped from validation_stack.
            // Using emit_error() would misattribute the error to the current ancestor.
            for err in deferred_errors {
                self.sink.on_error(err);
            }
        }

        // 4. ID/IDREF collection from element text content.
        // Owner is the parent element per §3.17.5.2: the binding is to the
        // element that has the ID-typed child in its [children].
        // ev_state is the popped child; the parent is now the stack top.
        if let Some(ref tv) = ev_state.typed_value {
            let parent_serial = self.validation_stack.last()
                .map(|e| e.element_serial)
                .unwrap_or(ev_state.element_serial); // root: no parent, use self
            self.collect_id_idref(tv, &ev_state.text_content, parent_serial);
            self.check_entity_declared(tv);
        }

        // 5. Update element path
        self.pop_element_path();

        let validity = ev_state.validity;
        self.current_state = ValidatorState::EndElement;

        // Compute [validation attempted] per spec §3.3.5.1
        let all_full = !ev_state.any_child_not_full && !ev_state.any_attr_not_full;
        let all_none = !ev_state.any_child_not_none && !ev_state.any_attr_not_none;
        let validation_attempted = if ev_state.strictly_assessed && all_full {
            ValidationAttempted::Full
        } else if !ev_state.strictly_assessed && all_none {
            ValidationAttempted::None
        } else {
            ValidationAttempted::Partial
        };

        // Propagate to parent
        if let Some(parent) = self.validation_stack.last_mut() {
            match validation_attempted {
                ValidationAttempted::Full => { parent.any_child_not_none = true; }
                ValidationAttempted::None => { parent.any_child_not_full = true; }
                ValidationAttempted::Partial => {
                    parent.any_child_not_full = true;
                    parent.any_child_not_none = true;
                }
            }
        }

        SchemaInfo {
            element_decl: ev_state.element_decl,
            attribute_decl: None,
            schema_type: ev_state.schema_type,
            member_type: ev_state.member_type,
            validity,
            validation_attempted,
            is_default: ev_state.is_default,
            is_nil: ev_state.is_nil,
            content_type: ev_state.content_type,
            typed_value: ev_state.typed_value,
            normalized_value: ev_state.normalized_value,
            schema_error_codes: ev_state.error_codes,
            notation: ev_state.notation,
            deferred_by_cta: false,
            type_source: ev_state.type_source,
            #[cfg(feature = "xsd11")]
            cta_selected: ev_state.cta_selected,
            #[cfg(feature = "xsd11")]
            assertion_outcome,
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
            if !self.id_values.contains_key(idref_value) {
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
                active_states.expected_element_terms(nfa)
                    .into_iter()
                    .map(|(name, namespace, element_key)| ExpectedElement {
                        local_name: name,
                        namespace,
                        element_key,
                    })
                    .collect()
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
                            let initial = crate::compiler::ActiveStates::from_nfa(extension_nfa);
                            for (name, namespace, element_key) in initial.expected_element_terms(extension_nfa) {
                                result.push(ExpectedElement {
                                    local_name: name,
                                    namespace,
                                    element_key,
                                });
                            }
                        }
                    }
                    AllGroupExtPhase::Nfa(active_states) => {
                        for (name, namespace, element_key) in active_states.expected_element_terms(extension_nfa) {
                            result.push(ExpectedElement {
                                local_name: name,
                                namespace,
                                element_key,
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

    /// Get the `[inherited attributes]` PSVI property for the current element
    /// (XSD 1.1 §3.3.5.6, structures.html line 5200).
    ///
    /// Returns the frozen `incoming_inherited` snapshot — ancestor-owned
    /// potentially-inherited attributes with nearest-owner shadowing.
    /// This is the spec's `[inherited attributes]` property; it is NOT
    /// filtered by the element's own `[attributes]`.
    ///
    /// Returns empty for skipped elements.
    #[cfg(feature = "xsd11")]
    pub fn get_inherited_attributes(&self) -> Vec<InheritedAttribute> {
        let ev_state = match self.validation_stack.last() {
            Some(s) => s,
            None => return Vec::new(),
        };

        // §3.3.5.6: [inherited attributes] is only defined for non-skipped elements
        if ev_state.process_contents == ContentProcessing::Skip {
            return Vec::new();
        }

        let mut result = Vec::new();
        for ((ns, name), val) in &ev_state.incoming_inherited {
            result.push(InheritedAttribute {
                local_name: *name,
                namespace: *ns,
                attribute_key: val.attribute_key,
                value: val.value.clone(),
            });
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
    fn push_element(&mut self, mut ev_state: ElementValidationState) {
        // Assign a unique serial for ID/IDREF owner-element binding (§3.17.5.2)
        ev_state.element_serial = self.next_element_serial;
        self.next_element_serial += 1;

        let local_name = self.schema_set.name_table.resolve(ev_state.local_name);
        if !self.element_path.is_empty() || self.validation_stack.is_empty() {
            self.element_path.push('/');
        }
        self.element_path.push_str(&local_name);

        // Inherit base URI from parent element, or from the document-level
        // base URI for the root element.
        ev_state.base_uri = match self.validation_stack.last() {
            Some(parent) => parent.base_uri.clone(),
            None => self.instance_base_uri.clone(),
        };
        ev_state.schema_location_hint_start = self.schema_location_hints.len();
        ev_state.no_namespace_schema_location_hint_start =
            self.no_namespace_schema_location_hints.len();

        self.validation_stack.push(ev_state);
        self.ic_scope_tables.push(None);

        // Propagate inherited attributes from parent to child (XSD 1.1).
        // §3.3.5.6: [inherited attributes] only exists when the parent is
        // strictly/laxly assessed and the child is not attributed to a skip
        // wildcard.
        //
        // Parent's outgoing_inherited becomes the child's incoming_inherited
        // (frozen PSVI view) and outgoing_inherited (mutable for this
        // element's own inheritable attrs to shadow).
        #[cfg(feature = "xsd11")]
        {
            let len = self.validation_stack.len();
            if len >= 2 {
                let parent_pc = self.validation_stack[len - 2].process_contents;
                let child_pc = self.validation_stack[len - 1].process_contents;
                if parent_pc != ContentProcessing::Skip
                    && child_pc != ContentProcessing::Skip
                {
                    let from_parent =
                        self.validation_stack[len - 2].outgoing_inherited.clone();
                    self.validation_stack[len - 1].incoming_inherited = from_parent.clone();
                    self.validation_stack[len - 1].outgoing_inherited = from_parent;
                }
            }
        }

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

    /// Emit a validation error: record its constraint code on the current
    /// element's `error_codes` list and dispatch to the sink.
    fn emit_error(&mut self, err: ValidationError) {
        if let Some(ev) = self.validation_stack.last_mut() {
            ev.error_codes.push(err.constraint);
        }
        self.sink.on_error(err);
    }

    /// Emit a validation error recording the code on an explicit target
    /// instead of the stack top. Used during `validate_end_element` where
    /// the element has already been popped.
    fn emit_error_to(&mut self, err: ValidationError, codes: &mut Vec<&'static str>) {
        codes.push(err.constraint);
        self.sink.on_error(err);
    }

    /// Report a validation error through the sink
    fn report_error(&mut self, constraint: &'static str, message: impl Into<String>) {
        let err = errors::error(constraint, message, self.current_location.clone());
        let err = if self.element_path.is_empty() {
            err
        } else {
            err.with_path(self.element_path.clone())
        };
        self.emit_error(err);
    }

    /// Enrich `err` with `current_location` and `element_path` (if either is set).
    /// `with_location` / `with_path` overwrite so this is safe to call even when
    /// the error already carries them.
    fn enrich(&self, err: ValidationError) -> ValidationError {
        let err = match &self.current_location {
            Some(loc) => err.with_location(loc.clone()),
            None => err,
        };
        if self.element_path.is_empty() {
            err
        } else {
            err.with_path(self.element_path.clone())
        }
    }

    /// Enrich `err` with current location/path and report it to an explicit
    /// `codes` target (e.g. during `validate_end_element` after the element
    /// has been popped off the stack).
    fn report_validation_error_to(
        &mut self,
        err: ValidationError,
        codes: &mut Vec<&'static str>,
    ) {
        let err = self.enrich(err);
        self.emit_error_to(err, codes);
    }

    /// Enrich an existing `ValidationError` with location/path and report it.
    fn report_validation_error(&mut self, err: ValidationError) {
        let err = self.enrich(err);
        self.emit_error(err);
    }

    /// Mark the current element as invalid.
    fn mark_current_invalid(&mut self) {
        if let Some(s) = self.validation_stack.last_mut() {
            s.validity = SchemaValidity::Invalid;
        }
    }

    /// Get the effective base URI for the current element (or the document
    /// base URI if no element is on the stack).
    fn current_element_base_uri(&self) -> String {
        self.validation_stack
            .last()
            .map(|ev| ev.base_uri.clone())
            .unwrap_or_else(|| self.instance_base_uri.clone())
    }

    fn rebase_hint_range(&mut self, sl_start: usize, nnsl_start: usize, base_uri: &str) {
        for hint in &mut self.schema_location_hints[sl_start..] {
            hint.base_uri = base_uri.to_string();
        }
        for hint in &mut self.no_namespace_schema_location_hints[nnsl_start..] {
            hint.base_uri = base_uri.to_string();
        }
    }

    // -----------------------------------------------------------------------
    // XSI built-in attribute helpers
    // -----------------------------------------------------------------------

    /// Dispatch validation for an `xsi:*` attribute.
    ///
    /// The four built-in XSI attributes (`type`, `nil`, `schemaLocation`,
    /// `noNamespaceSchemaLocation`) are validated against their spec-defined
    /// types. Unknown `xsi:*` attributes fall through to normal
    /// attribute/wildcard validation.
    fn validate_xsi_attribute(&mut self, local_name: NameId, value: &str) -> SchemaInfo {
        let builtin = self.schema_set.builtin_types();
        if local_name == well_known::XSI_TYPE {
            // Attribute-level: lexical xs:QName validation only.
            // Semantic resolution (cvc-elt.4.1) is handled in validate_element_by_id.
            let type_key = TypeKey::Simple(builtin.qname);
            let attr_key = builtin.xsi_type_attr;
            return self.validate_xsi_simple_value(value, type_key, attr_key);
        }
        if local_name == well_known::XSI_NIL {
            let type_key = TypeKey::Simple(builtin.boolean);
            let attr_key = builtin.xsi_nil_attr;
            return self.validate_xsi_simple_value(value, type_key, attr_key);
        }
        if local_name == well_known::XSI_SCHEMA_LOCATION {
            return self.validate_xsi_schema_location(value);
        }
        if local_name == well_known::XSI_NO_NAMESPACE_SCHEMA_LOCATION {
            return self.validate_xsi_no_ns_schema_location(value);
        }
        // Unknown xsi:* — fall through to normal attribute/wildcard validation
        self.validate_unknown_xsi_attribute(local_name, value)
    }

    /// Unknown `xsi:*` attributes go through normal wildcard validation.
    fn validate_unknown_xsi_attribute(
        &mut self,
        local_name: NameId,
        value: &str,
    ) -> SchemaInfo {
        let ct_key = match self.validation_stack.last() {
            Some(ev) => match ev.schema_type {
                Some(TypeKey::Complex(ct)) => ct,
                _ => return SchemaInfo::empty(),
            },
            None => return SchemaInfo::empty(),
        };
        self.validate_attribute_against_type(
            ct_key,
            local_name,
            Some(well_known::XSI_NAMESPACE),
            value,
        )
    }

    /// Validate a value against a built-in simple type and return attribute `SchemaInfo`.
    fn validate_xsi_simple_value(
        &mut self,
        value: &str,
        type_key: TypeKey,
        attr_key: AttributeKey,
    ) -> SchemaInfo {
        match super::simple::validate_simple_type(value, type_key, self.schema_set) {
            Ok(result) => SchemaInfo {
                element_decl: None,
                attribute_decl: Some(attr_key),
                schema_type: Some(type_key),
                member_type: result.member_type,
                validity: SchemaValidity::Valid,
                validation_attempted: ValidationAttempted::Full,
                is_default: false,
                is_nil: false,
                content_type: None,
                typed_value: Some(result.typed_value),
                normalized_value: result.normalized_value,
                schema_error_codes: Vec::new(),
                notation: None,
                deferred_by_cta: false,
                type_source: None,
                #[cfg(feature = "xsd11")]
                cta_selected: false,
                #[cfg(feature = "xsd11")]
                assertion_outcome: None,
            },
            Err(err) => {
                self.report_validation_error(err);
                SchemaInfo {
                    attribute_decl: Some(attr_key),
                    schema_type: Some(type_key),
                    validity: SchemaValidity::Invalid,
                    validation_attempted: ValidationAttempted::Full,
                    ..SchemaInfo::empty()
                }
            }
        }
    }

    /// Validate `xsi:schemaLocation` as a whitespace-separated list of anyURI
    /// tokens with even token count.
    fn validate_xsi_schema_location(&mut self, value: &str) -> SchemaInfo {
        let builtin = self.schema_set.builtin_types();
        let any_uri_key = TypeKey::Simple(builtin.any_uri);
        let list_type_key = TypeKey::Simple(builtin.xsi_schema_location_type);
        let attr_key = builtin.xsi_schema_location_attr;

        let tokens: Vec<&str> = value.split_whitespace().collect();
        let mut validity = SchemaValidity::Valid;

        // Even token count check (namespace/location pairs)
        if !tokens.len().is_multiple_of(2) {
            self.report_error(
                "cvc-schema-location",
                format!(
                    "xsi:schemaLocation value must contain an even number of URI tokens, \
                     but found {} tokens",
                    tokens.len()
                ),
            );
            validity = SchemaValidity::Invalid;
        }

        // Validate each token as xs:anyURI
        for token in &tokens {
            if let Err(err) =
                super::simple::validate_simple_type(token, any_uri_key, self.schema_set)
            {
                self.report_validation_error(err);
                validity = SchemaValidity::Invalid;
            }
        }

        // Accumulate namespace/location pairs (even from invalid attributes —
        // the complete pairs are still valid hints)
        let base_uri = self.current_element_base_uri();
        for pair in tokens.chunks_exact(2) {
            self.schema_location_hints.push(SchemaLocationHint {
                namespace: pair[0].to_string(),
                location: pair[1].to_string(),
                base_uri: base_uri.clone(),
            });
        }

        SchemaInfo {
            element_decl: None,
            attribute_decl: Some(attr_key),
            schema_type: Some(list_type_key),
            member_type: None,
            validity,
            validation_attempted: ValidationAttempted::Full,
            is_default: false,
            is_nil: false,
            content_type: None,
            typed_value: None,
            normalized_value: if tokens.is_empty() {
                None
            } else {
                Some(tokens.join(" "))
            },
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

    /// Validate `xsi:noNamespaceSchemaLocation` as `xs:anyURI`.
    fn validate_xsi_no_ns_schema_location(&mut self, value: &str) -> SchemaInfo {
        let builtin = self.schema_set.builtin_types();
        let any_uri_key = TypeKey::Simple(builtin.any_uri);
        let attr_key = builtin.xsi_no_namespace_schema_location_attr;
        let result = self.validate_xsi_simple_value(value, any_uri_key, attr_key);

        let trimmed = value.trim();
        if !trimmed.is_empty() {
            self.no_namespace_schema_location_hints
                .push(NoNamespaceSchemaLocationHint {
                    location: trimmed.to_string(),
                    base_uri: self.current_element_base_uri(),
                });
        }

        result
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
                            let refer_display = format_resolved_qname(
                                &self.schema_set.name_table,
                                refer_ns,
                                refer.local_name,
                            );
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
        self.advance_constraints_start_element_inner(local_name, namespace, element_key, false);
    }

    /// Variant of [`advance_constraints_start_element`] used when the element
    /// is inside a wildcard `processContents="skip"` subtree. Bumps depth
    /// tracking on existing IC selectors / fields without admitting new
    /// matches, and never activates element-attached identity constraints:
    /// per XSD 1.1 §3.11.4 (and the W3C wild101..103 fixtures), skipped
    /// content is outside the schema's validation scope.
    fn advance_constraints_start_element_skipped(
        &mut self,
        local_name: NameId,
        namespace: Option<NameId>,
    ) {
        self.advance_constraints_start_element_inner(local_name, namespace, None, true);
    }

    fn advance_constraints_start_element_inner(
        &mut self,
        local_name: NameId,
        namespace: Option<NameId>,
        element_key: Option<ElementKey>,
        skipped: bool,
    ) {
        let ns = namespace.unwrap_or(NameId(0));

        // 1. Advance existing active constraints
        for cs in &mut self.active_constraints {
            if skipped {
                cs.start_element_skipped(local_name, ns);
            } else {
                cs.start_element(local_name, ns);
            }
        }

        // 2. Activate new constraints from element declaration (only when
        //    NOT in skip mode — skipped elements have no element_key).
        if !skipped {
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
    }

    /// Process identity constraints at element end: advance fields/selectors,
    /// deactivate finished constraints, and perform scope-local keyref
    /// cross-reference.
    fn process_constraints_end_element(
        &mut self,
        text_content: &str,
        typed_value: Option<&XmlValue>,
        is_nil: bool,
        is_complex_content: bool,
        error_codes: &mut Vec<&'static str>,
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
                is_nil,
                is_complex_content,
                name_table,
                &element_path,
                location.clone(),
            );
            ic_errors.extend(errs);
        }
        for err in ic_errors {
            self.emit_error_to(err, error_codes);
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
            // If the target isn't in scope yet (ancestor key), defer to parent.
            let name_table = &self.schema_set.name_table;
            for (keyref_table, refer_key) in scope_keyrefs {
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
                            self.emit_error_to(err, error_codes);
                        }
                    }
                    None => {
                        // Target not yet in scope — defer to ancestor element end
                        self.deferred_keyrefs.push((keyref_table, refer_key));
                    }
                }
            }
        }
    }

    /// Check if a type (possibly a union) transitively contains xs:ID or xs:IDREF
    /// as a member type. Used for list(union(ID, ...)) detection.
    fn union_has_id_idref(&self, type_key: TypeKey) -> bool {
        let sk = match type_key {
            TypeKey::Simple(sk) => sk,
            _ => return false,
        };
        // Direct check: is this type itself ID or IDREF?
        if let Some(code) = self.schema_set.get_type_code(sk) {
            if matches!(code, XmlTypeCode::Id | XmlTypeCode::IdRef) {
                return true;
            }
        }
        // Check union members
        if let Some(st) = self.schema_set.arenas.simple_types.get(sk) {
            for &member_key in &st.resolved_member_types {
                if self.union_has_id_idref(member_key) {
                    return true;
                }
            }
        }
        false
    }

    /// Check if an ENTITY/ENTITIES value names declared unparsed entities.
    ///
    /// §3.16.4 String Valid clause 3: "Every ENTITY value in V is a declared
    /// entity name." Only checked when `unparsed_entities` is set.
    fn check_entity_declared(&mut self, typed_value: &XmlValue) {
        use crate::types::value::XmlValueKind;
        let entities = match &self.unparsed_entities {
            Some(e) => e,
            None => return,
        };
        // Collect undeclared names first to avoid borrow conflict with report_error
        let mut undeclared: Vec<String> = Vec::new();

        // Check list values first — handles both built-in xs:ENTITIES and
        // custom <xs:list itemType="xs:ENTITY"> which has type_code=Entity
        // but XmlValueKind::List.
        if let XmlValueKind::List { item_type, items } = &typed_value.value {
            if *item_type == XmlTypeCode::Entity
                || typed_value.type_code == XmlTypeCode::Entities
            {
                for item in items {
                    let name = item.to_string();
                    if !entities.contains(&name) {
                        undeclared.push(name);
                    }
                }
            }
        } else if typed_value.type_code == XmlTypeCode::Entity {
            let name = typed_value.to_string_value();
            if !entities.contains(&name) {
                undeclared.push(name);
            }
        }

        for name in undeclared {
            self.report_error(
                "cvc-datatype-valid.1.2.1",
                format!("ENTITY '{}' is not declared as an unparsed entity", name),
            );
        }
    }

    /// Register an ID value with its owner element serial.
    ///
    /// XSD 1.1 §3.17.5.2: the [binding] is a set of elements. Same ID on the
    /// same owner element is allowed (set size stays 1); same ID on different
    /// elements is cvc-id.2.
    fn register_id_value(&mut self, value: String, owner_serial: u64) {
        match self.id_values.get(&value) {
            Some(&existing_serial) => {
                if !(self.schema_set.is_xsd11() && existing_serial == owner_serial) {
                    self.report_error(
                        "cvc-id.2",
                        format!("Duplicate ID value '{}'", value),
                    );
                }
            }
            None => {
                self.id_values.insert(value, owner_serial);
            }
        }
    }

    /// Detect ID/IDREF types and collect values for finalization.
    ///
    /// Uses the normalized value from `typed_value` (not raw `value_str`)
    /// for ID and IDREF tracking, so whitespace-collapsed values match
    /// consistently across ID, IDREF, and IDREFS.
    ///
    /// `owner_serial` identifies the element that owns the ID binding per
    /// §3.17.5.2: for attributes it is the element carrying the attribute;
    /// for element text content it is the parent element.
    ///
    /// Handles both built-in types and user-defined list/union types
    /// containing ID/IDREF (§3.17.5.2: eligible items include types
    /// "derived or constructed directly or indirectly from" ID/IDREF).
    fn collect_id_idref(&mut self, typed_value: &XmlValue, value_str: &str, owner_serial: u64) {
        use crate::types::value::XmlValueKind;

        // Unwrap union recursively to get the effective value
        let mut effective = typed_value;
        while let XmlValueKind::Union(inner) = &effective.value {
            effective = inner.as_ref();
        }

        // Check list values — handles custom list-of-ID, list-of-IDREF,
        // and list-of-union(ID/IDREF, ...) types.
        if let XmlValueKind::List { item_type, items } = &effective.value {
            // First check: is the list's item type a union containing ID/IDREF?
            // If so, per-item type codes vary; re-validate each token individually
            // to identify which items are ID vs IDREF vs other (§3.17.5.2).
            let union_resolved = effective
                .schema_type
                .and_then(|sk| self.schema_set.arenas.simple_types.get(sk))
                .and_then(|st| st.resolved_item_type)
                .filter(|item_tk| self.union_has_id_idref(*item_tk));
            if let Some(item_type_key) = union_resolved {
                for token in value_str.split_whitespace() {
                    if let Ok(result) = super::simple::validate_simple_type(
                        token, item_type_key, self.schema_set,
                    ) {
                        match result.typed_value.type_code {
                            XmlTypeCode::Id => {
                                self.register_id_value(token.to_string(), owner_serial);
                            }
                            XmlTypeCode::IdRef => {
                                self.pending_idrefs.push((
                                    token.to_string(),
                                    self.current_location.clone(),
                                    self.element_path.clone(),
                                ));
                            }
                            _ => {} // e.g. integer — not ID/IDREF
                        }
                    }
                }
                return;
            }
            // Simple case: all items have the same type (non-union item type)
            match *item_type {
                XmlTypeCode::Id => {
                    for item in items {
                        self.register_id_value(item.to_string(), owner_serial);
                    }
                    return;
                }
                XmlTypeCode::IdRef => {
                    for item in items {
                        self.pending_idrefs.push((
                            item.to_string(),
                            self.current_location.clone(),
                            self.element_path.clone(),
                        ));
                    }
                    return;
                }
                _ => {}
            }
        }

        // Fall back to type_code-based dispatch for atomic values and
        // built-in list types (IdRefs, Entities).
        match effective.type_code {
            XmlTypeCode::Id => {
                let normalized = effective.to_string_value();
                self.register_id_value(normalized, owner_serial);
            }
            XmlTypeCode::IdRef | XmlTypeCode::IdRefs => {
                if let XmlValueKind::List { items, .. } = &effective.value {
                    for item in items {
                        self.pending_idrefs.push((
                            item.to_string(),
                            self.current_location.clone(),
                            self.element_path.clone(),
                        ));
                    }
                } else if effective.type_code == XmlTypeCode::IdRefs {
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
                        effective.to_string_value(),
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
        let found = {
            let ct_data = &self.schema_set.arenas.complex_types[ct_key];
            self.find_attribute_in_type(ct_data, local_name, namespace)
        };
        // cvc-complex-type.3.2.2: clause 3.2.2 (wildcard) is checked
        // independently of 3.2.1 — a Prohibited declaration does not block a
        // matching wildcard (XSD 1.0 behaviour; W3C attZ002, addB034, addB136).
        // Applies identically under XSD 1.1: §3.2.2 mapping drops
        // use="prohibited" from {attribute uses}, so §3.4.4.2 clause 2.1
        // never matches and the fall-through to clause 2.2 (wildcard) is
        // the spec-compliant rescue. The §3.4.4.2 clause-4 Note about
        // "attribute use always takes precedence" is non-normative and
        // addresses only non-prohibited matches.
        //
        // The wildcard is the FULL effective attribute wildcard per
        // §3.6.2.2 (own + groups intersection) chained with §3.4.2.5's
        // extension-union over the base chain — `compute_runtime_attribute_wildcard`
        // returns this canonical form. Cached so the rescued-prohibited
        // path does not recompute when falling through to NotFound.
        let mut wildcard_cache: Option<
            Option<crate::schema::derivation::EffectiveAttributeWildcard>,
        > = None;
        let found = match found {
            AttributeLookup::Prohibited => {
                let wc = crate::schema::derivation::compute_runtime_attribute_wildcard(
                    self.schema_set, ct_key,
                );
                let rescued = match wc.as_ref() {
                    Some(w) => crate::schema::derivation::effective_wildcard_allows_attribute(
                        self.schema_set, w, namespace, local_name,
                    ),
                    None => false,
                };
                wildcard_cache = Some(wc);
                if rescued {
                    AttributeLookup::NotFound
                } else {
                    AttributeLookup::Prohibited
                }
            }
            other => other,
        };

        match found {
            AttributeLookup::Found(attr_key, attr_type, fixed_value, inheritable) => {
                // Parse value once; reused for the fixed-value check and for SchemaInfo.
                let mut member_type = None;
                let mut typed_value = None;
                let mut normalized_value = None;
                let mut attr_validity = SchemaValidity::Valid;
                if let Some(type_key) = attr_type {
                    match super::simple::validate_simple_type(value, type_key, self.schema_set) {
                        Ok(result) => {
                            member_type = result.member_type;
                            typed_value = Some(result.typed_value);
                            normalized_value = result.normalized_value;
                        }
                        Err(err) => {
                            self.report_validation_error(err);
                            attr_validity = SchemaValidity::Invalid;
                            self.mark_current_invalid();
                        }
                    }
                }

                if let Some(fixed) = fixed_value {
                    let matches = if let Some(ref tv) = typed_value {
                        super::simple::fixed_matches_typed(value, tv, &fixed, attr_type, self.schema_set)
                    } else {
                        super::simple::fixed_values_equal(value, &fixed, attr_type, self.schema_set)
                    };
                    if !matches {
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

                // Record inheritable attribute into outgoing map for descendants
                // (XSD 1.1 §3.3.5.6). This shadows any ancestor value with the
                // same expanded name per the nearest-owner rule.
                #[cfg(feature = "xsd11")]
                if inheritable {
                    if let Some(ev) = self.validation_stack.last_mut() {
                        use super::context::InheritedAttributeValue;
                        ev.outgoing_inherited.insert(
                            (namespace, local_name),
                            InheritedAttributeValue {
                                value: value.to_string(),
                                attribute_key: attr_key,
                            },
                        );
                    }
                }
                let _ = inheritable;

                let result = SchemaInfo {
                    element_decl: None,
                    attribute_decl: attr_key,
                    schema_type: attr_type,
                    member_type,
                    validity: attr_validity,
                    validation_attempted: ValidationAttempted::Full,
                    is_default: false,
                    is_nil: false,
                    content_type: None,
                    typed_value,
                    normalized_value,
                    schema_error_codes: Vec::new(),
                    notation: None,
                    deferred_by_cta: false,
                    type_source: Some(TypeSource::Declaration),
                    #[cfg(feature = "xsd11")]
                    cta_selected: false,
                    #[cfg(feature = "xsd11")]
                    assertion_outcome: None,
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
                let effective_wildcard = wildcard_cache.unwrap_or_else(|| {
                    crate::schema::derivation::compute_runtime_attribute_wildcard(
                        self.schema_set, ct_key,
                    )
                });
                if let Some(ref wildcard) = effective_wildcard {
                    if crate::schema::derivation::effective_wildcard_allows_attribute(
                        self.schema_set, wildcard, namespace, local_name,
                    ) {
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
                        // Wildcard-backed inheritance (XSD 1.1 §3.3.5.6 clause 3.2):
                        // If strict/lax resolved a governing declaration with
                        // {inheritable}=true, record for propagation. Skip has no
                        // governing declaration (attribute_decl is None).
                        #[cfg(feature = "xsd11")]
                        if let Some(attr_key) = result.attribute_decl {
                            if let Some(decl) =
                                self.schema_set.arenas.attributes.get(attr_key)
                            {
                                if decl.inheritable {
                                    if let Some(ev) = self.validation_stack.last_mut() {
                                        use super::context::InheritedAttributeValue;
                                        ev.outgoing_inherited.insert(
                                            (namespace, local_name),
                                            InheritedAttributeValue {
                                                value: value.to_string(),
                                                attribute_key: Some(attr_key),
                                            },
                                        );
                                    }
                                }
                            }
                        }
                        if wildcard.process_contents == ProcessContents::Skip {
                            // Skip-processed attributes cannot contribute a typed value to
                            // an IC field (§3.11.4), so the field slot stays absent — xs:key
                            // will report a missing-field violation.  However, the attribute
                            // IS still selected by the field XPath, so it must count toward
                            // multi-node detection (cvc-identity-constraint.4.2.1).
                            let ns = namespace.unwrap_or(NameId(0));
                            let mut multi_node_ic: Vec<(NameId, usize)> = Vec::new();
                            for cs in &mut self.active_constraints {
                                let matches = cs.matching_fields(local_name, ns);
                                for field_idx in matches {
                                    if cs.increment_field_match_count(field_idx) {
                                        multi_node_ic.push((cs.key_table.constraint_name, field_idx));
                                    }
                                }
                            }
                            for (constraint_name, field_idx) in multi_node_ic {
                                let name = self.schema_set.name_table.resolve(constraint_name).to_string();
                                self.report_error(
                                    "cvc-identity-constraint.4.2.1",
                                    format!(
                                        "Identity constraint '{}': field {} matches more than one node",
                                        name, field_idx + 1
                                    ),
                                );
                            }
                        } else {
                            self.post_process_attribute(local_name, namespace, value, &result);
                        }
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
            let ec_snapshot = self.validation_stack.last().map_or(0, |ev| ev.error_codes.len());
            let mut info = self.validate_attribute_against_type(ct_key, *local_name, *namespace, value);
            if let Some(ev) = self.validation_stack.last_mut() {
                if ev.error_codes.len() > ec_snapshot {
                    info.schema_error_codes = ev.error_codes[ec_snapshot..].to_vec();
                    ev.error_codes.truncate(ec_snapshot);
                }
                // Track attribute [validation attempted] on parent element
                match info.validation_attempted {
                    ValidationAttempted::Full => { ev.any_attr_not_none = true; }
                    ValidationAttempted::None => { ev.any_attr_not_full = true; }
                    ValidationAttempted::Partial => {
                        ev.any_attr_not_full = true;
                        ev.any_attr_not_none = true;
                    }
                }
            }
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
        let ns_ctx = self.validation_stack.last().and_then(|ev| ev.ns_context.as_ref());
        let ic_typed_value = Self::resolve_ic_qname_value(
            &result.typed_value, value, ns_ctx, &self.schema_set.name_table,
        ).or_else(|| result.typed_value.clone());
        let mut multi_node_ic: Vec<(NameId, usize)> = Vec::new();
        for cs in &mut self.active_constraints {
            let matches = cs.matching_fields(local_name, ns);
            for field_idx in matches {
                let already_matched =
                    cs.set_field_value(field_idx, value.to_string(), ic_typed_value.clone());
                if already_matched {
                    multi_node_ic.push((cs.key_table.constraint_name, field_idx));
                }
            }
        }
        for (constraint_name, field_idx) in multi_node_ic {
            let name = self.schema_set.name_table.resolve(constraint_name).to_string();
            self.report_error(
                "cvc-identity-constraint.4.2.1",
                format!(
                    "Identity constraint '{}': field {} matches more than one node",
                    name,
                    field_idx + 1
                ),
            );
        }

        // ID/IDREF/ENTITY collection — owner is current element (attribute binding)
        if let Some(ref tv) = result.typed_value {
            let owner = self.validation_stack.last()
                .map(|e| e.element_serial).unwrap_or(0);
            self.collect_id_idref(tv, value, owner);
            self.check_entity_declared(tv);

            // NOTATION tracking (§3.14.5): set [notation] on parent element
            if tv.type_code == XmlTypeCode::Notation {
                // Resolve before borrowing the stack to avoid borrow conflict
                let notation = self.validation_stack.last()
                    .filter(|ev| ev.notation.is_none())
                    .and_then(|ev| ev.ns_context.as_ref())
                    .and_then(|ctx| self.resolve_notation_qname(value, ctx));
                if let (Some(nk), Some(ev)) = (notation, self.validation_stack.last_mut()) {
                    if ev.notation.is_none() {
                        ev.notation = Some(nk);
                    }
                }
            }
        }
    }

    /// Resolve a NOTATION QName value to a NotationKey using the element's
    /// namespace context. Returns `None` if the QName is malformed or the
    /// notation is not declared.
    fn resolve_notation_qname(
        &self,
        value: &str,
        ns_context: &NamespaceContextSnapshot,
    ) -> Option<NotationKey> {
        let qn = parse_qname_with_snapshot(
            value,
            ns_context,
            &self.schema_set.name_table,
            true,
        ).ok()?;
        self.schema_set.lookup_notation(qn.namespace_uri, qn.local_name)
    }

    /// Resolve a QName/NOTATION-typed value for IC purposes.
    ///
    /// QName values from simple type validation are stored as `XmlAtomicValue::String`
    /// because namespace context wasn't available at validation time. For IC field
    /// value comparison, we need namespace-aware QNames. Returns a new resolved
    /// copy (does NOT mutate the original, preserving PSVI integrity).
    fn resolve_ic_qname_value(
        typed_value: &Option<XmlValue>,
        string_value: &str,
        ns_context: Option<&NamespaceContextSnapshot>,
        name_table: &crate::namespace::table::NameTable,
    ) -> Option<XmlValue> {
        use crate::types::value::{XmlAtomicValue, XmlValueKind};
        let val = typed_value.as_ref()?;
        if val.type_code != XmlTypeCode::QName && val.type_code != XmlTypeCode::Notation {
            return None;
        }
        let ctx = ns_context?;
        let qn = parse_qname_with_snapshot(string_value, ctx, name_table, true).ok()?;
        let atom = if val.type_code == XmlTypeCode::Notation {
            XmlAtomicValue::Notation(qn)
        } else {
            XmlAtomicValue::QName(qn)
        };
        Some(XmlValue {
            type_code: val.type_code,
            schema_type: val.schema_type,
            value: XmlValueKind::Atomic(atom),
        })
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
            ComplexContentResult::Empty => {
                if ct_data.mixed {
                    ContentType::Mixed
                } else {
                    ContentType::Empty
                }
            }
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
    ///
    /// Errors are collected in `deferred_errors` instead of being emitted
    /// immediately, so the caller can emit them after the child element is
    /// pushed onto the validation stack (ensuring correct PSVI attribution).
    fn resolve_xsi_type(
        &self,
        xsi_type_str: &str,
        declared_type: Option<TypeKey>,
        block: DerivationSet,
        ns_context: &NamespaceContextSnapshot,
        deferred_errors: &mut Vec<(&'static str, String)>,
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
                deferred_errors.push(("cvc-elt.4.1", msg));
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
                    // cvc-elt.4.2: basic derivation check (no block keywords)
                    if !self.schema_set.is_type_derived_from(
                        type_key,
                        declared,
                        DerivationSet::empty(),
                    ) {
                        deferred_errors.push((
                            "cvc-elt.4.2",
                            format!(
                                "xsi:type '{}' does not derive from the declared type",
                                xsi_type_str
                            ),
                        ));
                        return XsiTypeOutcome::InvalidDerivation;
                    }
                    // cvc-elt.4.3: validly substitutable — combine the element's
                    // {disallowed substitutions} (block) with the declared type's
                    // {prohibited substitutions} into a single exclusion mask.
                    let mut combined_block = block;
                    if let TypeKey::Complex(declared_ct_key) = declared {
                        if let Some(declared_ct) = self.schema_set.arenas.complex_types.get(declared_ct_key) {
                            combined_block |= declared_ct.block.element_block_mask();
                        }
                    }
                    if !combined_block.is_empty()
                        && !self
                            .schema_set
                            .is_type_derived_from(type_key, declared, combined_block)
                    {
                        deferred_errors.push((
                            "cvc-elt.4.3",
                            format!(
                                "xsi:type '{}' is not validly substitutable for the declared type \
                                 (blocked by 'block' attribute)",
                                xsi_type_str
                            ),
                        ));
                        return XsiTypeOutcome::InvalidDerivation;
                    }
                }
                XsiTypeOutcome::Applied(type_key)
            }
            None => {
                deferred_errors.push((
                    "cvc-elt.4.1",
                    format!(
                        "Type '{}' specified in xsi:type is not declared",
                        xsi_type_str
                    ),
                ));
                XsiTypeOutcome::Unresolved
            }
        }
    }

    /// Emit deferred xsi:type errors (collected by [`resolve_xsi_type`]).
    fn emit_deferred_xsi_type_errors(&mut self, errors: Vec<(&'static str, String)>) {
        for (constraint, message) in errors {
            self.report_error(constraint, message);
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

                // Parse value once; reused for the fixed-value check and SchemaInfo.
                let mut member_type = None;
                let mut typed_value = None;
                let mut normalized_value = None;
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
                            normalized_value = result.normalized_value;
                        }
                        Err(err) => {
                            self.report_validation_error(err);
                            attr_validity = SchemaValidity::Invalid;
                            if let Some(s) = self.validation_stack.last_mut() {
                                s.validity = SchemaValidity::Invalid;
                            }
                        }
                    }
                }

                if let Some(fixed_val) = fixed {
                    let matches = if let Some(ref tv) = typed_value {
                        super::simple::fixed_matches_typed(value, tv, &fixed_val, attr_type, self.schema_set)
                    } else {
                        super::simple::fixed_values_equal(value, &fixed_val, attr_type, self.schema_set)
                    };
                    if !matches {
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

                SchemaInfo {
                    element_decl: None,
                    attribute_decl: Some(attr_key),
                    schema_type: attr_type,
                    member_type,
                    validity: attr_validity,
                    validation_attempted: ValidationAttempted::Full,
                    is_default: false,
                    is_nil: false,
                    content_type: None,
                    typed_value,
                    normalized_value,
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

                // Parse value once; reused for the fixed-value check and SchemaInfo.
                let mut member_type = None;
                let mut typed_value = None;
                let mut normalized_value = None;
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
                            normalized_value = result.normalized_value;
                        }
                        Err(err) => {
                            self.report_validation_error(err);
                            attr_validity = SchemaValidity::Invalid;
                            if let Some(s) = self.validation_stack.last_mut() {
                                s.validity = SchemaValidity::Invalid;
                            }
                        }
                    }
                }

                if let Some(fixed_val) = fixed {
                    let matches = if let Some(ref tv) = typed_value {
                        super::simple::fixed_matches_typed(value, tv, &fixed_val, attr_type, self.schema_set)
                    } else {
                        super::simple::fixed_values_equal(value, &fixed_val, attr_type, self.schema_set)
                    };
                    if !matches {
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

                SchemaInfo {
                    element_decl: None,
                    attribute_decl: Some(attr_key),
                    schema_type: attr_type,
                    member_type,
                    validity: attr_validity,
                    validation_attempted: ValidationAttempted::Full,
                    is_default: false,
                    is_nil: false,
                    content_type: None,
                    typed_value,
                    normalized_value,
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
            None => {
                // No global declaration — lax means skip
                SchemaInfo::empty()
            }
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
            // For `<xs:attribute ref="x:foo"/>` uses, the use's own
            // `resolved_type` is `None` — the type lives on the global
            // declaration. Fall back to the resolved type recorded on the
            // global decl, otherwise simple-type validation gets skipped
            // (the W3C `xsd003b` fixture exercises this through a redefined
            // `simpleType` whose enum-restricted facet must be applied to
            // an attribute reached via an attribute group).
            let attr_type = resolved
                .and_then(|r| r.resolved_type)
                .or_else(|| {
                    attr_key
                        .and_then(|k| self.schema_set.arenas.attributes.get(k))
                        .and_then(|d| d.resolved_type)
                });
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
                #[cfg(feature = "xsd11")]
                inheritable: attr_use.attribute.inheritable,
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

    // Effective-attribute-wildcard helpers superseded by
    // `crate::schema::derivation::compute_runtime_attribute_wildcard`,
    // which implements full §3.6.2.2 (own + groups intersection) plus
    // §3.4.2.5 (extension union over the base chain) in canonical form.

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
                let attr_key = resolved.and_then(|r| r.resolved_ref);
                let attr_type = resolved
                    .and_then(|r| r.resolved_type)
                    .or_else(|| {
                        attr_key
                            .and_then(|k| self.schema_set.arenas.attributes.get(k))
                            .and_then(|d| d.resolved_type)
                    });

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

                if attr_use.use_kind == AttributeUseKind::Prohibited {
                    // In XSD 1.0, use="prohibited" combined with fixed=X is a valid
                    // schema construct (constraint au-props-correct.5 was added in XSD 1.1).
                    // The combination means the attribute may appear with the fixed value.
                    // (W3C test attP031: schema valid in 1.0, instance with fixed value valid.)
                    if fixed.is_some() && self.schema_set.is_xsd10() {
                        let inheritable = attr_use.attribute.inheritable;
                        return AttributeLookup::Found(attr_key, attr_type, fixed, inheritable);
                    }
                    return AttributeLookup::Prohibited;
                }

                let inheritable = attr_use.attribute.inheritable;
                return AttributeLookup::Found(attr_key, attr_type, fixed, inheritable);
            }
        }

        // Search attribute groups
        for ga in self.collect_group_attributes(ct_data) {
            if ga.name == local_name && ga.namespace == namespace {
                if ga.use_kind == AttributeUseKind::Prohibited {
                    // A prohibited use inside an attribute group is transparent —
                    // it does NOT propagate the prohibition to the referencing type
                    // (W3C Bugzilla #4043 / TSTF conclusion). Skip it and let the
                    // base-type chain walk below decide.
                    break;
                }
                #[cfg(feature = "xsd11")]
                let inheritable = ga.inheritable;
                #[cfg(not(feature = "xsd11"))]
                let inheritable = false;
                return AttributeLookup::Found(ga.attr_key, ga.type_key, ga.fixed_value, inheritable);
            }
        }

        // Walk base type chain for inherited attributes (XSD spec §3.4.2.4)
        if let Some(TypeKey::Complex(base_ct_key)) = ct_data.resolved_base_type {
            if base_ct_key != self.schema_set.any_type_key() {
                let base_data = &self.schema_set.arenas.complex_types[base_ct_key];
                return self.find_attribute_in_type(base_data, local_name, namespace);
            }
        }

        AttributeLookup::NotFound
    }

    /// Record inheritable attributes with default/fixed values for propagation
    /// to descendant elements (XSD 1.1 §3.3.5.6).
    ///
    /// Scans both direct attribute uses and attribute group uses. Only records
    /// defaults for inheritable attributes that were not explicitly provided.
    #[cfg(feature = "xsd11")]
    fn record_inheritable_defaults(&mut self, ct_key: ComplexTypeKey) {
        use super::context::InheritedAttributeValue;

        let ct_data = &self.schema_set.arenas.complex_types[ct_key];

        // Collect candidates to avoid borrow conflict with validation_stack
        let mut candidates: Vec<(Option<NameId>, NameId, String, Option<AttributeKey>)> =
            Vec::new();

        // 1. Direct attribute uses
        for (i, attr_use) in ct_data.attributes.iter().enumerate() {
            if attr_use.use_kind == AttributeUseKind::Prohibited || !attr_use.attribute.inheritable
            {
                continue;
            }
            let resolved = ct_data.resolved_attributes.get(i);
            let attr_key = resolved.and_then(|r| r.resolved_ref);
            let (name, ns) =
                self.resolve_attr_use_name_ns(attr_use, resolved, ct_data.target_namespace);
            let value = attr_use
                .attribute
                .default_value
                .as_deref()
                .or(attr_use.attribute.fixed_value.as_deref())
                .or_else(|| {
                    attr_key
                        .and_then(|k| self.schema_set.arenas.attributes.get(k))
                        .and_then(|d| {
                            d.default_value
                                .as_deref()
                                .or(d.fixed_value.as_deref())
                        })
                });
            if let Some(val) = value {
                candidates.push((ns, name, val.to_string(), attr_key));
            }
        }

        // 2. Attribute group uses
        for ga in self.collect_group_attributes(ct_data) {
            if ga.use_kind == AttributeUseKind::Prohibited || !ga.inheritable {
                continue;
            }
            let value = ga
                .default_value
                .as_deref()
                .or(ga.fixed_value.as_deref());
            if let Some(val) = value {
                candidates.push((ga.namespace, ga.name, val.to_string(), ga.attr_key));
            }
        }

        // Apply to outgoing_inherited: only for attributes not explicitly
        // provided. Use insert() (not or_insert) so defaulted values from
        // this element shadow ancestor values per nearest-owner rule.
        if let Some(ev) = self.validation_stack.last_mut() {
            for (ns, name, val, attr_key) in candidates {
                if !ev.seen_attributes.contains(&(ns, name)) {
                    ev.outgoing_inherited.insert(
                        (ns, name),
                        InheritedAttributeValue {
                            value: val,
                            attribute_key: attr_key,
                        },
                    );
                }
            }
        }
    }

    /// Check that all required attributes are present.
    ///
    /// Walks the base type chain to find inherited required attributes
    /// (XSD spec §3.4.2.4). Attributes already declared or prohibited in
    /// derived types are skipped via `checked_names`.
    fn check_required_attributes(
        &mut self,
        ct_data: &ComplexTypeDefData,
        seen: &HashSet<(Option<NameId>, NameId)>,
    ) -> bool {
        let mut has_missing = false;
        let mut checked_names: HashSet<(Option<NameId>, NameId)> = HashSet::new();

        for (i, attr_use) in ct_data.attributes.iter().enumerate() {
            let resolved = ct_data.resolved_attributes.get(i);
            let (attr_name, attr_ns) =
                self.resolve_attr_use_name_ns(attr_use, resolved, ct_data.target_namespace);
            checked_names.insert((attr_ns, attr_name));

            if attr_use.use_kind != AttributeUseKind::Required {
                continue;
            }
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
            checked_names.insert((ga.namespace, ga.name));
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

        // Walk base type chain for inherited required attributes
        let any_type = self.schema_set.any_type_key();
        let mut base_type = ct_data.resolved_base_type;
        while let Some(TypeKey::Complex(base_ct_key)) = base_type {
            if base_ct_key == any_type {
                break;
            }
            let base_data = &self.schema_set.arenas.complex_types[base_ct_key];

            for (i, attr_use) in base_data.attributes.iter().enumerate() {
                let resolved = base_data.resolved_attributes.get(i);
                let (attr_name, attr_ns) =
                    self.resolve_attr_use_name_ns(attr_use, resolved, base_data.target_namespace);
                if !checked_names.insert((attr_ns, attr_name)) {
                    continue; // already handled by derived type
                }
                if attr_use.use_kind != AttributeUseKind::Required {
                    continue;
                }
                if !seen.contains(&(attr_ns, attr_name)) {
                    let name_str = self.schema_set.name_table.resolve(attr_name);
                    self.report_error(
                        "cvc-complex-type.4",
                        format!("Required attribute '{}' is missing", name_str),
                    );
                    has_missing = true;
                }
            }

            for ga in self.collect_group_attributes(base_data) {
                if !checked_names.insert((ga.namespace, ga.name)) {
                    continue;
                }
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

            base_type = base_data.resolved_base_type;
        }

        has_missing
    }
}

/// Resolve an `xml:base` value against an inherited base URI.
///
/// If the value is already absolute (contains `://` or starts with `/`
/// or is a Windows absolute path), it replaces the inherited base.
/// Otherwise, it is resolved as a relative URI against the inherited base.
fn resolve_base_uri(xml_base: &str, inherited: &str) -> String {
    if xml_base.is_empty() {
        return inherited.to_string();
    }
    // Check for absolute URI
    if xml_base.contains("://")
        || xml_base.starts_with('/')
        || (xml_base.len() >= 2 && xml_base.as_bytes().get(1) == Some(&b':'))
    {
        return xml_base.to_string();
    }
    if inherited.is_empty() {
        return xml_base.to_string();
    }
    // Resolve relative against inherited base directory.
    // Find the last path separator (handles both / and \ for Windows paths).
    let last_sep = inherited
        .rfind('/')
        .or_else(|| inherited.rfind('\\'));
    let base_dir = match last_sep {
        Some(pos) => &inherited[..=pos],
        None => "",
    };
    if base_dir.is_empty() {
        xml_base.to_string()
    } else {
        format!("{}{}", base_dir, xml_base)
    }
}

#[cfg(test)]
#[path = "runtime_tests.rs"]
mod tests;
