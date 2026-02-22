//! NFA compilation functions
//!
//! This module implements the core compilation logic for transforming
//! XSD content model particles into NFAs.

use crate::arenas::{ComplexTypeDefData, ModelGroupData};
use crate::parser::frames::DerivationMethod;
use crate::ids::{ElementKey, NameId, TypeKey};
use crate::parser::frames::{
    Compositor, ComplexContentResult, ElementFrameResult, ModelGroupDefResult, OpenContentMode,
    OpenContentResult, ParticleResult, ParticleTerm, ProcessContents, QNameRef, TypeRefResult,
    WildcardNamespace, WildcardResult,
};
use crate::parser::location::SourceRef;
use crate::schema::model::{DefaultOpenContent, OpenContentMode as SchemaOpenContentMode, XsdVersion};
use crate::schema::wildcard::{ElementWildcard, NamespaceConstraint as SchemaNamespaceConstraint};
use crate::schema::SchemaSet;
#[cfg(test)]
use crate::schema::FormChoice;
use crate::types::complex::{
    NamespaceConstraint, OpenContent, OpenContentMode as TypesOpenContentMode,
    ProcessContents as TypesProcessContents, WildcardRef,
};

use super::all_group::{AllGroupModel, AllParticle, OpenContentWildcard, OpenContentMode as AllGroupOpenContentMode};
use super::error::{NfaCompileError, NfaCompileResult};
use super::fragment::{fragment_to_table, FragmentBuilder, NfaFragment};
use super::nfa::{NfaTable, NfaTerm};
use super::ContentModelMatcher;
use super::particle::{apply_occurs, MaxOccurs};

/// Maximum recursion depth for compiling nested groups
const MAX_RECURSION_DEPTH: usize = 100;

/// Context for NFA compilation
///
/// Provides access to the schema set for resolving references during compilation.
pub struct CompileContext<'a> {
    /// Reference to the schema set for resolving references
    pub schema_set: &'a SchemaSet,
    /// Target namespace for the content model being compiled
    pub target_namespace: Option<NameId>,
    /// Fragment builder for allocating states
    builder: FragmentBuilder,
    /// Current recursion depth
    depth: usize,
    /// Resolved types from resolved_particles (set when compiling a model group)
    resolved_particle_types: Vec<Option<TypeKey>>,
    /// Current particle index within the model group being compiled
    current_particle_idx: Option<usize>,
    /// Flat depth-first element counter for content particle compilation.
    /// When Some, overrides per-level `current_particle_idx` for type resolution.
    content_flat_idx: Option<usize>,
    /// Resolved element keys for local elements in content particles (flat depth-first order)
    resolved_particle_elements: Vec<Option<ElementKey>>,
}

impl<'a> CompileContext<'a> {
    /// Create a new compilation context
    pub fn new(schema_set: &'a SchemaSet, target_namespace: Option<NameId>) -> Self {
        Self {
            schema_set,
            target_namespace,
            builder: FragmentBuilder::new(),
            depth: 0,
            resolved_particle_types: Vec::new(),
            current_particle_idx: None,
            content_flat_idx: None,
            resolved_particle_elements: Vec::new(),
        }
    }

    /// Compile a particle to an NFA table
    ///
    /// This is the main entry point for compiling a content model particle.
    pub fn compile_particle(&mut self, particle: &ParticleResult) -> NfaCompileResult<NfaTable> {
        self.check_recursion(particle.source.as_ref())?;
        self.depth += 1;

        let fragment = self.compile_particle_to_fragment(particle)?;
        let table = fragment_to_table(fragment);

        self.depth -= 1;
        Ok(table)
    }

    /// Compile a model group to an NFA table
    ///
    /// Used for compiling named groups (xs:group).
    pub fn compile_model_group(
        &mut self,
        group: &ModelGroupDefResult,
    ) -> NfaCompileResult<NfaTable> {
        self.check_recursion(group.source.as_ref())?;
        self.depth += 1;

        let fragment = self.compile_model_group_to_fragment(group)?;
        let table = fragment_to_table(fragment);

        self.depth -= 1;
        Ok(table)
    }

    /// Compile a particle to a fragment (internal use)
    fn compile_particle_to_fragment(
        &mut self,
        particle: &ParticleResult,
    ) -> NfaCompileResult<NfaFragment> {
        // Validate occurrence constraints
        if let Some(max) = particle.max_occurs {
            if particle.min_occurs > max {
                return Err(NfaCompileError::invalid_occurrence(
                    particle.min_occurs,
                    max,
                    particle.source.clone(),
                ));
            }
        }

        // Compile the term
        let term_fragment = self.compile_term(&particle.term, particle.source.as_ref())?;

        // Apply occurrence constraints
        let fragment = self.apply_occurrences(
            term_fragment,
            particle.min_occurs,
            particle.max_occurs,
        );

        Ok(fragment)
    }

    /// Compile a particle term to a fragment
    fn compile_term(
        &mut self,
        term: &ParticleTerm,
        source: Option<&SourceRef>,
    ) -> NfaCompileResult<NfaFragment> {
        match term {
            ParticleTerm::Element(elem) => self.compile_element(elem, source),
            ParticleTerm::Group(group) => self.compile_model_group_to_fragment(group),
            ParticleTerm::Any(wildcard) => self.compile_wildcard(wildcard, source),
        }
    }

    /// Build an NfaTerm for an element declaration, resolving name, namespace,
    /// element_key, and type information.
    ///
    /// This is the shared logic used by both `compile_element()` (NFA path)
    /// and `compile_all_group_model()` (AllGroup path).
    fn build_element_term(
        &mut self,
        elem: &ElementFrameResult,
        source: Option<&SourceRef>,
    ) -> NfaCompileResult<NfaTerm> {
        // Grab and increment flat element index (if compiling content particles)
        let current_flat_idx = if let Some(flat_idx) = self.content_flat_idx {
            self.content_flat_idx = Some(flat_idx + 1);
            Some(flat_idx)
        } else {
            None
        };

        // Determine element name and namespace
        let (name, namespace, element_key) = if let Some(ref_name) = &elem.ref_name {
            // Element reference - look up in schema set
            let key = self
                .schema_set
                .lookup_element(ref_name.namespace, ref_name.local_name);
            (ref_name.local_name, ref_name.namespace, key)
        } else if let Some(name) = elem.name {
            // Local element declaration
            let source_ref = source.or(elem.source.as_ref());
            let namespace = self.effective_element_namespace(elem, source_ref);
            // Look up local element key from resolved_particle_elements
            let local_key = current_flat_idx
                .and_then(|idx| self.resolved_particle_elements.get(idx).copied().flatten())
                .or_else(|| {
                    self.current_particle_idx
                        .and_then(|idx| self.resolved_particle_elements.get(idx).copied().flatten())
                });
            (name, namespace, local_key)
        } else {
            return Err(NfaCompileError::unresolved_element(
                "anonymous element without name or ref".to_string(),
                source.cloned(),
            ));
        };

        // For local elements (element_key is None), resolve type
        let resolved_type = if element_key.is_none() {
            // First: check context for resolved type
            let type_from_context = if let Some(flat_idx) = current_flat_idx {
                self.resolved_particle_types.get(flat_idx).copied().flatten()
            } else {
                self.current_particle_idx
                    .and_then(|idx| self.resolved_particle_types.get(idx).copied().flatten())
            };
            // Then: try QName resolution
            type_from_context.or_else(|| self.resolve_element_type_ref(elem))
        } else {
            None // Elements with key get type from element declaration via arena
        };

        Ok(NfaTerm::element_with_type(name, namespace, element_key, resolved_type))
    }

    /// Compile an element to a fragment
    fn compile_element(
        &mut self,
        elem: &ElementFrameResult,
        source: Option<&SourceRef>,
    ) -> NfaCompileResult<NfaFragment> {
        let nfa_term = self.build_element_term(elem, source)?;
        Ok(self.builder.single_term(nfa_term, source.cloned()))
    }

    /// Resolve a local element's QName type reference to a TypeKey
    fn resolve_element_type_ref(&self, elem: &ElementFrameResult) -> Option<TypeKey> {
        match &elem.type_ref {
            Some(TypeRefResult::QName(qname)) => {
                self.schema_set
                    .lookup_type(qname.namespace, qname.local_name)
                    .or_else(|| {
                        self.schema_set
                            .get_built_in_type_by_qname(qname.namespace, qname.local_name)
                    })
            }
            _ => None, // Inline types not resolved at compile time
        }
    }

    /// Compile a wildcard to a fragment
    fn compile_wildcard(
        &mut self,
        wildcard: &WildcardResult,
        source: Option<&SourceRef>,
    ) -> NfaCompileResult<NfaFragment> {
        let namespace_constraint = self.convert_wildcard_namespace(&wildcard.namespace);
        let process_contents = self.convert_process_contents(wildcard.process_contents);

        let nfa_term = NfaTerm::wildcard(namespace_constraint, process_contents);
        Ok(self.builder.single_term(nfa_term, source.cloned()))
    }

    /// Compile a model group definition to a fragment
    fn compile_model_group_to_fragment(
        &mut self,
        group: &ModelGroupDefResult,
    ) -> NfaCompileResult<NfaFragment> {
        // If this is a group reference, resolve and compile the referenced group
        if let Some(ref_name) = &group.ref_name {
            return self.compile_group_ref(ref_name, group.source.as_ref());
        }

        // Get the compositor, default to sequence
        let compositor = group.compositor.unwrap_or(Compositor::Sequence);

        // Handle empty particle list
        if group.particles.is_empty() {
            return Ok(self.builder.epsilon_fragment());
        }

        // Compile based on compositor type
        match compositor {
            Compositor::Sequence => self.compile_sequence(&group.particles),
            Compositor::Choice => self.compile_choice(&group.particles),
            Compositor::All => self.compile_all(&group.particles, group.source.as_ref()),
        }
    }

    /// Compile a particle with a tracked index for resolved type lookup
    fn compile_particle_with_index(
        &mut self,
        particle: &ParticleResult,
        particle_idx: usize,
    ) -> NfaCompileResult<NfaFragment> {
        if self.content_flat_idx.is_some() {
            // When using flat element counter, skip per-level positional indexing
            return self.compile_particle_to_fragment(particle);
        }
        let saved_idx = self.current_particle_idx;
        self.current_particle_idx = Some(particle_idx);
        let result = self.compile_particle_to_fragment(particle);
        self.current_particle_idx = saved_idx;
        result
    }

    /// Compile a sequence (xs:sequence)
    fn compile_sequence(&mut self, particles: &[ParticleResult]) -> NfaCompileResult<NfaFragment> {
        if particles.is_empty() {
            return Ok(self.builder.epsilon_fragment());
        }

        let mut result = self.compile_particle_with_index(&particles[0], 0)?;
        for (i, particle) in particles[1..].iter().enumerate() {
            let frag = self.compile_particle_with_index(particle, i + 1)?;
            result = result.concat(frag);
        }

        Ok(result)
    }

    /// Compile a choice (xs:choice)
    fn compile_choice(&mut self, particles: &[ParticleResult]) -> NfaCompileResult<NfaFragment> {
        if particles.is_empty() {
            return Ok(self.builder.epsilon_fragment());
        }

        let mut result = self.compile_particle_with_index(&particles[0], 0)?;
        for (i, particle) in particles[1..].iter().enumerate() {
            let frag = self.compile_particle_with_index(particle, i + 1)?;
            result = result.alternate(frag);
        }

        Ok(result)
    }

    /// Compile an all-group (xs:all) as NFA — used as fallback for nested
    /// all-groups that cannot use `AllGroupModel` (e.g. inside a sequence
    /// or choice). Top-level all-groups (both inline and named refs) use
    /// `compile_all_group_model()` instead.
    fn compile_all(
        &mut self,
        particles: &[ParticleResult],
        source: Option<&SourceRef>,
    ) -> NfaCompileResult<NfaFragment> {
        // Validate XSD 1.0 constraints
        for particle in particles {
            if !matches!(particle.term, ParticleTerm::Element(_)) {
                return Err(NfaCompileError::invalid_all_group(source.cloned()));
            }

            if let Some(max) = particle.max_occurs {
                if max > 1 {
                    return Err(NfaCompileError::invalid_all_group(source.cloned()));
                }
            } else {
                return Err(NfaCompileError::invalid_all_group(source.cloned()));
            }
        }

        if particles.is_empty() {
            return Ok(self.builder.epsilon_fragment());
        }

        // Choice-of-all with bounded repeat: allows any order but doesn't
        // enforce "each element at most once". Named-group all-groups land
        // here; inline top-level all-groups use AllGroupModel instead.
        let mut choice = self.compile_particle_with_index(&particles[0], 0)?;
        for (i, particle) in particles[1..].iter().enumerate() {
            let frag = self.compile_particle_with_index(particle, i + 1)?;
            choice = choice.alternate(frag);
        }

        let n = particles.len() as u32;
        Ok(choice.repeat_range(0, Some(n)))
    }

    /// Compile an all-group's particles into an `AllGroupModel`.
    ///
    /// Each particle is resolved to an `AllParticle` with its `NfaTerm`,
    /// min/max occurs, and source location. Only element and wildcard
    /// particles are supported; group references inside all-groups are
    /// rejected (XSD 1.0 forbids them; XSD 1.1 named-group support deferred).
    fn compile_all_group_model(
        &mut self,
        particles: &[ParticleResult],
        source: Option<&SourceRef>,
    ) -> NfaCompileResult<AllGroupModel> {
        let mut all_particles = Vec::with_capacity(particles.len());

        for particle in particles {
            let term = match &particle.term {
                ParticleTerm::Element(elem) => {
                    self.build_element_term(elem, particle.source.as_ref().or(source))?
                }
                ParticleTerm::Any(wildcard) => {
                    let ns = self.convert_wildcard_namespace(&wildcard.namespace);
                    let pc = self.convert_process_contents(wildcard.process_contents);
                    NfaTerm::wildcard(ns, pc)
                }
                ParticleTerm::Group(_) => {
                    // Group references inside all-groups are not yet supported.
                    // XSD 1.0 forbids them; XSD 1.1 support is deferred.
                    return Err(NfaCompileError::invalid_all_group(source.cloned()));
                }
            };

            let max_occurs = MaxOccurs::from_option(particle.max_occurs);
            all_particles.push(AllParticle::new(
                term,
                particle.min_occurs,
                max_occurs,
                particle.source.clone().or_else(|| source.cloned()),
            ));
        }

        Ok(AllGroupModel::new(all_particles))
    }

    /// Compile a group reference
    fn compile_group_ref(
        &mut self,
        ref_name: &QNameRef,
        source: Option<&SourceRef>,
    ) -> NfaCompileResult<NfaFragment> {
        // Look up the referenced group
        let group_key = self
            .schema_set
            .lookup_model_group(ref_name.namespace, ref_name.local_name)
            .ok_or_else(|| {
                let name = format!(
                    "{}:{}",
                    ref_name
                        .namespace
                        .map(|n| format!("{:?}", n))
                        .unwrap_or_default(),
                    ref_name.local_name.0
                );
                NfaCompileError::unresolved_group(name, source.cloned())
            })?;

        // Get the group data from arenas
        let group_data = self
            .schema_set
            .arenas
            .get_model_group(group_key)
            .ok_or_else(|| {
                NfaCompileError::unresolved_group(
                    format!("group key {:?}", group_key),
                    source.cloned(),
                )
            })?;

        // Convert ModelGroupData particles to fragments
        self.compile_model_group_data(group_data, source)
    }

    /// Compile from ModelGroupData (arena storage format)
    fn compile_model_group_data(
        &mut self,
        group: &ModelGroupData,
        source: Option<&SourceRef>,
    ) -> NfaCompileResult<NfaFragment> {
        let compositor = group.compositor.unwrap_or(Compositor::Sequence);

        if group.particles.is_empty() {
            return Ok(self.builder.epsilon_fragment());
        }

        // Save context and set up flat indexing for the named group
        let saved_flat_idx = self.content_flat_idx.take();
        let saved_particle_elements = std::mem::take(&mut self.resolved_particle_elements);
        let saved_types = std::mem::take(&mut self.resolved_particle_types);
        let saved_idx = self.current_particle_idx;

        // Use flat-indexed fields from ModelGroupData
        self.resolved_particle_types = group.resolved_particle_types.clone();
        self.resolved_particle_elements = group.resolved_particle_elements.clone();
        // Enable flat indexing within the group
        self.content_flat_idx = Some(0);

        let result = match compositor {
            Compositor::Sequence => self.compile_sequence(&group.particles),
            Compositor::Choice => self.compile_choice(&group.particles),
            Compositor::All => self.compile_all(&group.particles, source),
        };

        // Restore previous context
        self.content_flat_idx = saved_flat_idx;
        self.resolved_particle_elements = saved_particle_elements;
        self.resolved_particle_types = saved_types;
        self.current_particle_idx = saved_idx;
        result
    }

    /// Apply occurrence constraints to a fragment
    ///
    /// Uses threshold optimization: maxOccurs values > MAX_OCCURS_LIMIT
    /// are treated as unbounded to avoid NFA state explosion.
    fn apply_occurrences(
        &mut self,
        fragment: NfaFragment,
        min: u32,
        max: Option<u32>,
    ) -> NfaFragment {
        let max_occurs = MaxOccurs::from_option(max);
        apply_occurs(fragment, min, max_occurs)
    }

    /// Check recursion depth
    fn check_recursion(&self, source: Option<&SourceRef>) -> NfaCompileResult<()> {
        if self.depth >= MAX_RECURSION_DEPTH {
            return Err(NfaCompileError::recursion_exceeded(source.cloned()));
        }
        Ok(())
    }

    /// Convert WildcardNamespace to NamespaceConstraint
    fn convert_wildcard_namespace(&self, ns: &WildcardNamespace) -> NamespaceConstraint {
        match ns {
            WildcardNamespace::Any => NamespaceConstraint::Any,
            WildcardNamespace::Other => NamespaceConstraint::Other,
            WildcardNamespace::TargetNamespace => NamespaceConstraint::TargetNamespace,
            WildcardNamespace::Local => NamespaceConstraint::Local,
            WildcardNamespace::List(list) => NamespaceConstraint::List(list.clone()),
        }
    }

    /// Convert parser ProcessContents to types ProcessContents
    fn convert_process_contents(&self, pc: ProcessContents) -> TypesProcessContents {
        match pc {
            ProcessContents::Strict => TypesProcessContents::Strict,
            ProcessContents::Lax => TypesProcessContents::Lax,
            ProcessContents::Skip => TypesProcessContents::Skip,
        }
    }

    fn effective_element_namespace(
        &self,
        elem: &ElementFrameResult,
        source: Option<&SourceRef>,
    ) -> Option<NameId> {
        self.schema_set.effective_local_element_namespace(
            elem.target_namespace,
            elem.form.as_deref(),
            source,
            self.target_namespace,
        )
    }
}

/// Compile a particle to an NFA table (convenience function)
pub fn compile_particle(
    schema_set: &SchemaSet,
    particle: &ParticleResult,
    target_namespace: Option<NameId>,
) -> NfaCompileResult<NfaTable> {
    let mut ctx = CompileContext::new(schema_set, target_namespace);
    ctx.compile_particle(particle)
}

/// Compile a model group to an NFA table (convenience function)
pub fn compile_model_group(
    schema_set: &SchemaSet,
    group: &ModelGroupDefResult,
    target_namespace: Option<NameId>,
) -> NfaCompileResult<NfaTable> {
    let mut ctx = CompileContext::new(schema_set, target_namespace);
    ctx.compile_model_group(group)
}

/// Detect an inline `xs:all` at the top level of a particle.
///
/// Returns the all-group's particles and source if the particle's term is a
/// group with `compositor == All` and no `ref_name` (i.e., an inline definition,
/// not a named model group reference).
fn is_top_level_all_group(particle: &ParticleResult) -> Option<(&[ParticleResult], Option<&SourceRef>)> {
    if let ParticleTerm::Group(group) = &particle.term {
        if group.compositor == Some(Compositor::All) && group.ref_name.is_none() {
            return Some((&group.particles, group.source.as_ref()));
        }
    }
    None
}

/// Detect a named model group reference at the top level that resolves to
/// an `xs:all` group.
///
/// Returns the resolved [`ModelGroupData`] when the particle's term is a
/// group with a `ref_name` and the referenced definition has
/// `compositor == All`.
fn resolve_top_level_all_group_ref<'a>(
    particle: &ParticleResult,
    schema_set: &'a SchemaSet,
) -> Option<&'a ModelGroupData> {
    if let ParticleTerm::Group(group) = &particle.term {
        let ref_name = group.ref_name.as_ref()?;
        let group_key = schema_set.lookup_model_group(ref_name.namespace, ref_name.local_name)?;
        let group_data = schema_set.arenas.get_model_group(group_key)?;
        if group_data.compositor == Some(Compositor::All) {
            return Some(group_data);
        }
    }
    None
}

/// Compile the base type's all-group model for an extension type.
///
/// If the extension's resolved base type is a complex type whose content model
/// compiles to an `AllGroup`, returns the `AllGroupModel`. Otherwise returns `None`.
#[cfg(feature = "xsd11")]
fn compile_base_all_group(
    schema_set: &SchemaSet,
    type_def: &ComplexTypeDefData,
) -> NfaCompileResult<Option<AllGroupModel>> {
    let base_ct_key = match type_def.resolved_base_type {
        Some(TypeKey::Complex(key)) => key,
        _ => return Ok(None),
    };
    let base_type_def = &schema_set.arenas.complex_types[base_ct_key];
    let base_matcher = compile_content_model_matcher(schema_set, base_type_def)?;
    match base_matcher {
        ContentModelMatcher::AllGroup(model) => Ok(Some(model)),
        _ => Ok(None),
    }
}

/// Convert an `AllGroupModel` into an NFA table for concatenation with
/// extension content. Each particle becomes a choice alternative wrapped
/// in repeat(0, max_occurs).
fn all_group_to_nfa(model: &AllGroupModel) -> NfaTable {
    let mut builder = FragmentBuilder::new();
    if model.particles.is_empty() {
        return fragment_to_table(builder.epsilon_fragment());
    }

    let fragments: Vec<NfaFragment> = model
        .particles
        .iter()
        .map(|p| {
            let frag = builder.single_term(p.term.clone(), p.source.clone());
            let max = match p.max_occurs {
                MaxOccurs::Bounded(n) => Some(n),
                MaxOccurs::Unbounded => None,
            };
            apply_occurs(frag, p.min_occurs, MaxOccurs::from_option(max))
        })
        .collect();

    let mut choice = fragments.into_iter().reduce(|a, b| a.alternate(b)).unwrap();
    let n = model.particles.len() as u32;
    choice = choice.repeat_range(0, Some(n));

    fragment_to_table(choice)
}

/// Compile a complex type's content model into a matcher, applying open content defaults.
pub fn compile_content_model_matcher(
    schema_set: &SchemaSet,
    type_def: &ComplexTypeDefData,
) -> NfaCompileResult<ContentModelMatcher> {
    let target_namespace = type_def.target_namespace;
    let mut ctx = CompileContext::new(schema_set, target_namespace);
    let is_extension = matches!(type_def.derivation_method, Some(DerivationMethod::Extension));

    // Try the all-group path for non-extension types with an inline xs:all
    if !is_extension {
        if let ComplexContentResult::Complex(def) = &type_def.content {
            if let Some(particle) = &def.particle {
                if let Some((all_particles, all_source)) = is_top_level_all_group(particle) {
                    ctx.resolved_particle_types =
                        type_def.resolved_content_particle_types.to_vec();
                    ctx.resolved_particle_elements =
                        type_def.resolved_content_particle_elements.to_vec();
                    ctx.content_flat_idx = Some(0);
                    let model = ctx.compile_all_group_model(all_particles, all_source)?;
                    let base_matcher = ContentModelMatcher::AllGroup(model);

                    let open_content = resolve_open_content(
                        schema_set,
                        &type_def.content,
                        type_def.open_content.as_ref(),
                        type_def.source.as_ref(),
                    );

                    return Ok(attach_open_content(base_matcher, open_content));
                }

                // Named group ref resolving to all-group
                if let Some(group_data) = resolve_top_level_all_group_ref(particle, schema_set) {
                    ctx.resolved_particle_types = group_data.resolved_particle_types.clone();
                    ctx.resolved_particle_elements = group_data.resolved_particle_elements.clone();
                    ctx.content_flat_idx = Some(0);
                    let model = ctx.compile_all_group_model(
                        &group_data.particles,
                        group_data.source.as_ref(),
                    )?;
                    let base_matcher = ContentModelMatcher::AllGroup(model);

                    let open_content = resolve_open_content(
                        schema_set,
                        &type_def.content,
                        type_def.open_content.as_ref(),
                        type_def.source.as_ref(),
                    );

                    return Ok(attach_open_content(base_matcher, open_content));
                }
            }
        }
    }

    // XSD 1.1: Extension from an all-group base type — produce AllGroup or
    // AllGroupExtension instead of the lossy NFA conversion.
    #[cfg(feature = "xsd11")]
    if is_extension {
        if let Some(base_all_model) = compile_base_all_group(schema_set, type_def)? {
            let open_content = resolve_open_content(
                schema_set,
                &type_def.content,
                type_def.open_content.as_ref(),
                type_def.source.as_ref(),
            );

            // Determine what the extension adds
            let own_particle = match &type_def.content {
                ComplexContentResult::Complex(def) => def.particle.as_ref(),
                _ => None,
            };

            match own_particle {
                None => {
                    // Extension adds only attributes — return base AllGroup directly
                    let matcher = ContentModelMatcher::AllGroup(base_all_model);
                    return Ok(attach_open_content(matcher, open_content));
                }
                Some(particle) => {
                    // Check if extension's own particle is an inline all-group
                    if let Some((ext_particles, ext_source)) = is_top_level_all_group(particle) {
                        // Merge: base all-group + extension all-group → single AllGroup
                        let mut ctx = CompileContext::new(schema_set, type_def.target_namespace);
                        ctx.resolved_particle_types =
                            type_def.resolved_content_particle_types.to_vec();
                        ctx.resolved_particle_elements =
                            type_def.resolved_content_particle_elements.to_vec();
                        ctx.content_flat_idx = Some(0);
                        let ext_model = ctx.compile_all_group_model(ext_particles, ext_source)?;

                        let mut merged_particles = base_all_model.particles;
                        merged_particles.extend(ext_model.particles);
                        let merged = AllGroupModel::new(merged_particles);
                        let matcher = ContentModelMatcher::AllGroup(merged);
                        return Ok(attach_open_content(matcher, open_content));
                    }

                    // Extension is sequence/choice — compile as NFA, return composite
                    let mut ctx = CompileContext::new(schema_set, type_def.target_namespace);
                    ctx.resolved_particle_types =
                        type_def.resolved_content_particle_types.to_vec();
                    ctx.resolved_particle_elements =
                        type_def.resolved_content_particle_elements.to_vec();
                    ctx.content_flat_idx = Some(0);
                    let ext_nfa = ctx.compile_particle(particle)?;

                    let matcher = ContentModelMatcher::AllGroupExtension {
                        base_model: base_all_model,
                        extension_nfa: ext_nfa,
                    };
                    return Ok(attach_open_content(matcher, open_content));
                }
            }
        }
    }

    // Standard NFA path (sequences, choices, named group refs, extensions)
    let own_nfa = match &type_def.content {
        ComplexContentResult::Complex(def) => match &def.particle {
            Some(particle) => {
                ctx.resolved_particle_types =
                    type_def.resolved_content_particle_types.to_vec();
                ctx.resolved_particle_elements =
                    type_def.resolved_content_particle_elements.to_vec();
                ctx.content_flat_idx = Some(0);
                Some(ctx.compile_particle(particle)?)
            }
            None => None,
        },
        ComplexContentResult::Empty | ComplexContentResult::Simple(_) => None,
    };

    // For extensions, prepend the base type's content model
    let base_nfa = if is_extension {
        if let Some(TypeKey::Complex(base_ct_key)) = type_def.resolved_base_type {
            let base_type_def = &schema_set.arenas.complex_types[base_ct_key];
            let base_matcher = compile_content_model_matcher(schema_set, base_type_def)?;
            match base_matcher {
                ContentModelMatcher::Nfa(nfa) => Some(nfa),
                ContentModelMatcher::WithOpenContent { nfa, .. } => Some(nfa),
                ContentModelMatcher::AllGroup(ref model) => {
                    if own_nfa.is_none() {
                        // Extension adds only attributes — return base AllGroup directly
                        let open_content = resolve_open_content(
                            schema_set,
                            &type_def.content,
                            type_def.open_content.as_ref(),
                            type_def.source.as_ref(),
                        );
                        return Ok(attach_open_content(base_matcher, open_content));
                    }
                    // Extension has own particles — convert AllGroup to NFA for concat
                    // (XSD 1.0 path; XSD 1.1 is handled above via compile_base_all_group)
                    Some(all_group_to_nfa(model))
                }
                #[cfg(feature = "xsd11")]
                ContentModelMatcher::AllGroupExtension { .. } => {
                    // Should not occur: base type should not produce AllGroupExtension
                    unreachable!("base type produced AllGroupExtension")
                }
            }
        } else {
            None
        }
    } else {
        None
    };

    let effective_nfa = match (base_nfa, own_nfa) {
        (Some(base), Some(own)) => base.concat(own),
        (Some(base), None) => base,
        (None, Some(own)) => own,
        (None, None) => empty_nfa(),
    };

    let base_matcher = ContentModelMatcher::Nfa(effective_nfa);

    let open_content = resolve_open_content(
        schema_set,
        &type_def.content,
        type_def.open_content.as_ref(),
        type_def.source.as_ref(),
    );

    Ok(attach_open_content(base_matcher, open_content))
}

fn empty_nfa() -> NfaTable {
    let mut builder = FragmentBuilder::new();
    fragment_to_table(builder.epsilon_fragment())
}

fn attach_open_content(
    matcher: ContentModelMatcher,
    open_content: Option<OpenContent>,
) -> ContentModelMatcher {
    let open_content = match open_content {
        Some(open_content) => open_content,
        None => return matcher,
    };

    match matcher {
        ContentModelMatcher::Nfa(nfa) => ContentModelMatcher::WithOpenContent {
            nfa,
            mode: open_content.mode,
            wildcard: open_content.wildcard,
        },
        ContentModelMatcher::AllGroup(mut model) => {
            if let Some(wildcard_ref) = open_content.wildcard {
                let mode = match open_content.mode {
                    TypesOpenContentMode::Interleave => AllGroupOpenContentMode::Interleave,
                    TypesOpenContentMode::Suffix => AllGroupOpenContentMode::Suffix,
                    TypesOpenContentMode::None => AllGroupOpenContentMode::None,
                };
                model.open_content = Some(OpenContentWildcard {
                    namespace_constraint: wildcard_ref.namespace_constraint,
                    process_contents: wildcard_ref.process_contents,
                    mode,
                });
            }
            ContentModelMatcher::AllGroup(model)
        }
        #[cfg(feature = "xsd11")]
        ContentModelMatcher::AllGroupExtension { mut base_model, extension_nfa } => {
            if let Some(wildcard_ref) = open_content.wildcard {
                let mode = match open_content.mode {
                    TypesOpenContentMode::Interleave => AllGroupOpenContentMode::Interleave,
                    TypesOpenContentMode::Suffix => AllGroupOpenContentMode::Suffix,
                    TypesOpenContentMode::None => AllGroupOpenContentMode::None,
                };
                base_model.open_content = Some(OpenContentWildcard {
                    namespace_constraint: wildcard_ref.namespace_constraint,
                    process_contents: wildcard_ref.process_contents,
                    mode,
                });
            }
            ContentModelMatcher::AllGroupExtension { base_model, extension_nfa }
        }
        other => other,
    }
}

fn resolve_open_content(
    schema_set: &SchemaSet,
    content: &ComplexContentResult,
    explicit: Option<&OpenContentResult>,
    source: Option<&SourceRef>,
) -> Option<OpenContent> {
    if schema_set.xsd_version != XsdVersion::V1_1 {
        return None;
    }

    if let Some(explicit) = explicit {
        return open_content_from_result(explicit);
    }

    if !matches!(content, ComplexContentResult::Complex(_) | ComplexContentResult::Empty) {
        return None;
    }

    let doc = source.and_then(|s| schema_set.documents.get(s.doc_id as usize));
    let default = doc.and_then(|d| d.default_open_content.as_ref())?;

    if !default.applies_to_empty && content_is_empty(content) {
        return None;
    }

    open_content_from_default(default)
}

fn content_is_empty(content: &ComplexContentResult) -> bool {
    match content {
        ComplexContentResult::Empty => true,
        ComplexContentResult::Complex(def) => def.particle.is_none(),
        ComplexContentResult::Simple(_) => false,
    }
}

fn open_content_from_result(result: &OpenContentResult) -> Option<OpenContent> {
    let mode = convert_open_content_mode(result.mode);
    if matches!(mode, TypesOpenContentMode::None) {
        return None;
    }

    Some(OpenContent {
        mode,
        wildcard: result.wildcard.as_ref().map(wildcard_ref_from_result),
        source: result.source.clone(),
    })
}

fn open_content_from_default(default: &DefaultOpenContent) -> Option<OpenContent> {
    let mode = convert_schema_open_content_mode(default.mode);
    if matches!(mode, TypesOpenContentMode::None) {
        return None;
    }

    Some(OpenContent {
        mode,
        wildcard: default.wildcard.as_ref().map(wildcard_ref_from_default),
        source: default.source.clone(),
    })
}

fn convert_open_content_mode(mode: OpenContentMode) -> TypesOpenContentMode {
    match mode {
        OpenContentMode::None => TypesOpenContentMode::None,
        OpenContentMode::Interleave => TypesOpenContentMode::Interleave,
        OpenContentMode::Suffix => TypesOpenContentMode::Suffix,
    }
}

fn convert_schema_open_content_mode(mode: SchemaOpenContentMode) -> TypesOpenContentMode {
    match mode {
        SchemaOpenContentMode::None => TypesOpenContentMode::None,
        SchemaOpenContentMode::Interleave => TypesOpenContentMode::Interleave,
        SchemaOpenContentMode::Suffix => TypesOpenContentMode::Suffix,
    }
}

fn wildcard_ref_from_result(wildcard: &WildcardResult) -> WildcardRef {
    let namespace_constraint = match &wildcard.namespace {
        WildcardNamespace::Any => NamespaceConstraint::Any,
        WildcardNamespace::Other => NamespaceConstraint::Other,
        WildcardNamespace::TargetNamespace => NamespaceConstraint::TargetNamespace,
        WildcardNamespace::Local => NamespaceConstraint::Local,
        WildcardNamespace::List(list) => NamespaceConstraint::List(list.clone()),
    };

    let process_contents = match wildcard.process_contents {
        ProcessContents::Strict => TypesProcessContents::Strict,
        ProcessContents::Lax => TypesProcessContents::Lax,
        ProcessContents::Skip => TypesProcessContents::Skip,
    };

    WildcardRef {
        namespace_constraint,
        process_contents,
        source: wildcard.source.clone(),
    }
}

fn wildcard_ref_from_default(wildcard: &ElementWildcard) -> WildcardRef {
    let namespace_constraint = match &wildcard.namespace_constraint {
        SchemaNamespaceConstraint::Any => NamespaceConstraint::Any,
        SchemaNamespaceConstraint::Other => NamespaceConstraint::Other,
        SchemaNamespaceConstraint::Enumeration(list) => NamespaceConstraint::List(list.clone()),
        SchemaNamespaceConstraint::Not(_) => {
            // TODO: Preserve notNamespace constraints once supported in types::complex.
            NamespaceConstraint::Any
        }
    };

    let process_contents = match wildcard.process_contents {
        crate::schema::wildcard::ProcessContents::Strict => TypesProcessContents::Strict,
        crate::schema::wildcard::ProcessContents::Lax => TypesProcessContents::Lax,
        crate::schema::wildcard::ProcessContents::Skip => TypesProcessContents::Skip,
    };

    WildcardRef {
        namespace_constraint,
        process_contents,
        source: wildcard.source.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::location::{SourceRef, SourceSpan};
    use crate::schema::model::{DefaultOpenContent, OpenContentMode as SchemaOpenContentMode, XsdVersion};
    use crate::schema::wildcard::ElementWildcard;
    use crate::schema::SchemaDocument;

    fn make_element_particle(name: NameId, min: u32, max: Option<u32>) -> ParticleResult {
        ParticleResult {
            term: ParticleTerm::Element(ElementFrameResult {
                name: Some(name),
                ref_name: None,
                target_namespace: None,
                type_ref: None,
                inline_type: None,
                substitution_group: vec![],
                default_value: None,
                fixed_value: None,
                nillable: false,
                is_abstract: false,
                min_occurs: 1,
                max_occurs: Some(1),
                block: Default::default(),
                final_derivation: Default::default(),
                form: None,
                id: None,
                alternatives: vec![],
                identity_constraints: vec![],
                annotation: None,
                source: None,
            }),
            min_occurs: min,
            max_occurs: max,
            source: None,
        }
    }

    fn make_sequence_particle(particles: Vec<ParticleResult>) -> ParticleResult {
        ParticleResult {
            term: ParticleTerm::Group(ModelGroupDefResult {
                name: None,
                ref_name: None,
                compositor: Some(Compositor::Sequence),
                particles,
                min_occurs: 1,
                max_occurs: Some(1),
                id: None,
                annotation: None,
                source: None,
            }),
            min_occurs: 1,
            max_occurs: Some(1),
            source: None,
        }
    }

    fn make_choice_particle(particles: Vec<ParticleResult>) -> ParticleResult {
        ParticleResult {
            term: ParticleTerm::Group(ModelGroupDefResult {
                name: None,
                ref_name: None,
                compositor: Some(Compositor::Choice),
                particles,
                min_occurs: 1,
                max_occurs: Some(1),
                id: None,
                annotation: None,
                source: None,
            }),
            min_occurs: 1,
            max_occurs: Some(1),
            source: None,
        }
    }

    fn make_complex_type_data(
        source: Option<SourceRef>,
        content: ComplexContentResult,
    ) -> ComplexTypeDefData {
        ComplexTypeDefData {
            name: None,
            target_namespace: None,
            base_type: None,
            derivation_method: None,
            content,
            open_content: None,
            attributes: Vec::new(),
            attribute_groups: Vec::new(),
            attribute_wildcard: None,
            mixed: false,
            is_abstract: false,
            final_derivation: Default::default(),
            block: Default::default(),
            default_attributes_apply: true,
            id: None,
            #[cfg(feature = "xsd11")]
            assertions: Vec::new(),
            #[cfg(feature = "xsd11")]
            xpath_default_namespace: None,
            annotation: None,
            source,
            resolved_base_type: None,
            resolved_attribute_groups: Vec::new(),
            resolved_attributes: Vec::new(),
            resolved_content_particle_types: Vec::new(),
            resolved_content_particle_elements: Vec::new(),
        }
    }

    #[test]
    fn test_compile_single_element() {
        let schema_set = SchemaSet::new();
        let particle = make_element_particle(NameId(1), 1, Some(1));

        let table = compile_particle(&schema_set, &particle, None).unwrap();

        assert!(table.state_count() >= 2); // At least start and end
    }

    #[test]
    fn test_compile_optional_element() {
        let schema_set = SchemaSet::new();
        let particle = make_element_particle(NameId(1), 0, Some(1));

        let table = compile_particle(&schema_set, &particle, None).unwrap();

        // Optional should have epsilon bypass
        let start = table.get_state(table.start_state).unwrap();
        assert!(start.epsilon_transitions().count() > 0);
    }

    #[test]
    fn test_compile_sequence() {
        let schema_set = SchemaSet::new();
        let particle = make_sequence_particle(vec![
            make_element_particle(NameId(1), 1, Some(1)),
            make_element_particle(NameId(2), 1, Some(1)),
        ]);

        let table = compile_particle(&schema_set, &particle, None).unwrap();

        // Sequence of 2 elements should have multiple states
        assert!(table.state_count() >= 4);
    }

    #[test]
    fn test_compile_choice() {
        let schema_set = SchemaSet::new();
        let particle = make_choice_particle(vec![
            make_element_particle(NameId(1), 1, Some(1)),
            make_element_particle(NameId(2), 1, Some(1)),
        ]);

        let table = compile_particle(&schema_set, &particle, None).unwrap();

        // Choice should have branch states
        assert!(table.state_count() >= 4);
    }

    #[test]
    fn test_default_open_content_applies_to_empty_complex_type() {
        let mut schema_set = SchemaSet::with_version(XsdVersion::V1_1);
        let doc_id = schema_set.documents.len() as u32;
        let mut doc = SchemaDocument::new(doc_id, "test.xsd".to_string());
        doc.default_open_content = Some(DefaultOpenContent {
            source: None,
            applies_to_empty: true,
            mode: SchemaOpenContentMode::Suffix,
            wildcard: Some(ElementWildcard::any_lax()),
        });
        schema_set.documents.push(doc);

        let source = SourceRef::new(doc_id, SourceSpan::new(0, 0));
        let ct_key = schema_set.arenas.alloc_complex_type(make_complex_type_data(
            Some(source),
            ComplexContentResult::Empty,
        ));
        let type_def = schema_set.arenas.complex_types.get(ct_key).unwrap();

        let matcher = compile_content_model_matcher(&schema_set, type_def).unwrap();
        match matcher {
            ContentModelMatcher::WithOpenContent { mode, wildcard, .. } => {
                assert_eq!(mode, TypesOpenContentMode::Suffix);
                assert!(wildcard.is_some());
            }
            _ => panic!("expected open content wrapper"),
        }
    }

    #[test]
    fn test_default_open_content_skipped_when_not_applies_to_empty() {
        let mut schema_set = SchemaSet::with_version(XsdVersion::V1_1);
        let doc_id = schema_set.documents.len() as u32;
        let mut doc = SchemaDocument::new(doc_id, "test.xsd".to_string());
        doc.default_open_content = Some(DefaultOpenContent {
            source: None,
            applies_to_empty: false,
            mode: SchemaOpenContentMode::Interleave,
            wildcard: Some(ElementWildcard::any_lax()),
        });
        schema_set.documents.push(doc);

        let source = SourceRef::new(doc_id, SourceSpan::new(0, 0));
        let ct_key = schema_set.arenas.alloc_complex_type(make_complex_type_data(
            Some(source),
            ComplexContentResult::Empty,
        ));
        let type_def = schema_set.arenas.complex_types.get(ct_key).unwrap();

        let matcher = compile_content_model_matcher(&schema_set, type_def).unwrap();
        assert!(matches!(matcher, ContentModelMatcher::Nfa(_)));
    }

    #[test]
    fn test_invalid_occurrence() {
        let schema_set = SchemaSet::new();
        let particle = ParticleResult {
            term: ParticleTerm::Element(ElementFrameResult {
                name: Some(NameId(1)),
                ref_name: None,
                target_namespace: None,
                type_ref: None,
                inline_type: None,
                substitution_group: vec![],
                default_value: None,
                fixed_value: None,
                nillable: false,
                is_abstract: false,
                min_occurs: 1,
                max_occurs: Some(1),
                block: Default::default(),
                final_derivation: Default::default(),
                form: None,
                id: None,
                alternatives: vec![],
                identity_constraints: vec![],
                annotation: None,
                source: None,
            }),
            min_occurs: 5, // min > max
            max_occurs: Some(3),
            source: None,
        };

        let result = compile_particle(&schema_set, &particle, None);
        assert!(matches!(result, Err(NfaCompileError::InvalidOccurrence { .. })));
    }

    #[test]
    fn test_element_form_default_applies_to_local_element() {
        let mut schema_set = SchemaSet::new();
        let target_ns = schema_set.name_table.add("http://example.com");
        let name = schema_set.name_table.add("local");

        let doc_id = schema_set.documents.len() as u32;
        let mut doc = SchemaDocument::new(doc_id, "test.xsd".to_string());
        doc.target_namespace = Some(target_ns);
        doc.element_form_default = FormChoice::Qualified;
        schema_set.documents.push(doc);

        let source_ref = SourceRef::new(doc_id, SourceSpan::new(0, 0));
        let particle = ParticleResult {
            term: ParticleTerm::Element(ElementFrameResult {
                name: Some(name),
                ref_name: None,
                target_namespace: None,
                type_ref: None,
                inline_type: None,
                substitution_group: vec![],
                default_value: None,
                fixed_value: None,
                nillable: false,
                is_abstract: false,
                min_occurs: 1,
                max_occurs: Some(1),
                block: Default::default(),
                final_derivation: Default::default(),
                form: None,
                id: None,
                alternatives: vec![],
                identity_constraints: vec![],
                annotation: None,
                source: Some(source_ref.clone()),
            }),
            min_occurs: 1,
            max_occurs: Some(1),
            source: Some(source_ref),
        };

        let table = compile_particle(&schema_set, &particle, Some(target_ns)).unwrap();
        let term = table
            .states
            .iter()
            .find_map(|state| state.term.as_ref())
            .expect("expected element term");
        match term {
            NfaTerm::Element { namespace, .. } => {
                assert_eq!(*namespace, Some(target_ns));
            }
            _ => panic!("expected element term"),
        }
    }

    #[test]
    fn test_element_form_override_unqualified() {
        let mut schema_set = SchemaSet::new();
        let target_ns = schema_set.name_table.add("http://example.com");
        let name = schema_set.name_table.add("local");

        let doc_id = schema_set.documents.len() as u32;
        let mut doc = SchemaDocument::new(doc_id, "test.xsd".to_string());
        doc.target_namespace = Some(target_ns);
        doc.element_form_default = FormChoice::Qualified;
        schema_set.documents.push(doc);

        let source_ref = SourceRef::new(doc_id, SourceSpan::new(0, 0));
        let particle = ParticleResult {
            term: ParticleTerm::Element(ElementFrameResult {
                name: Some(name),
                ref_name: None,
                target_namespace: None,
                type_ref: None,
                inline_type: None,
                substitution_group: vec![],
                default_value: None,
                fixed_value: None,
                nillable: false,
                is_abstract: false,
                min_occurs: 1,
                max_occurs: Some(1),
                block: Default::default(),
                final_derivation: Default::default(),
                form: Some("unqualified".to_string()),
                id: None,
                alternatives: vec![],
                identity_constraints: vec![],
                annotation: None,
                source: Some(source_ref.clone()),
            }),
            min_occurs: 1,
            max_occurs: Some(1),
            source: Some(source_ref),
        };

        let table = compile_particle(&schema_set, &particle, Some(target_ns)).unwrap();
        let term = table
            .states
            .iter()
            .find_map(|state| state.term.as_ref())
            .expect("expected element term");
        match term {
            NfaTerm::Element { namespace, .. } => {
                assert_eq!(*namespace, None);
            }
            _ => panic!("expected element term"),
        }
    }

    fn make_all_particle(particles: Vec<ParticleResult>) -> ParticleResult {
        ParticleResult {
            term: ParticleTerm::Group(ModelGroupDefResult {
                name: None,
                ref_name: None,
                compositor: Some(Compositor::All),
                particles,
                min_occurs: 1,
                max_occurs: Some(1),
                id: None,
                annotation: None,
                source: None,
            }),
            min_occurs: 1,
            max_occurs: Some(1),
            source: None,
        }
    }

    fn make_complex_type_with_content(
        content: ComplexContentResult,
    ) -> ComplexTypeDefData {
        make_complex_type_data(None, content)
    }

    #[test]
    fn test_all_group_produces_all_group_matcher() {
        use crate::parser::frames::ComplexContentDefResult;

        let schema_set = SchemaSet::new();
        let all_particle = make_all_particle(vec![
            make_element_particle(NameId(1), 1, Some(1)),
            make_element_particle(NameId(2), 0, Some(1)),
        ]);

        let content = ComplexContentResult::Complex(ComplexContentDefResult {
            particle: Some(all_particle),
            derivation: DerivationMethod::Restriction,
            mixed: false,
            base_type: None,
            open_content: None,
            attributes: vec![],
            attribute_groups: vec![],
            attribute_wildcard: None,
            assertions: vec![],
            id: None,
            derivation_id: None,
            source: None,
        });

        let type_def = make_complex_type_with_content(content);
        let matcher = compile_content_model_matcher(&schema_set, &type_def).unwrap();

        match &matcher {
            ContentModelMatcher::AllGroup(model) => {
                assert_eq!(model.particle_count(), 2);
                // First particle required, second optional
                assert!(!model.particles[0].is_optional());
                assert!(model.particles[1].is_optional());
            }
            other => panic!("expected AllGroup matcher, got {:?}", other),
        }
    }

    #[test]
    fn test_sequence_still_produces_nfa() {
        use crate::parser::frames::ComplexContentDefResult;

        let schema_set = SchemaSet::new();
        let seq_particle = make_sequence_particle(vec![
            make_element_particle(NameId(1), 1, Some(1)),
            make_element_particle(NameId(2), 1, Some(1)),
        ]);

        let content = ComplexContentResult::Complex(ComplexContentDefResult {
            particle: Some(seq_particle),
            derivation: DerivationMethod::Restriction,
            mixed: false,
            base_type: None,
            open_content: None,
            attributes: vec![],
            attribute_groups: vec![],
            attribute_wildcard: None,
            assertions: vec![],
            id: None,
            derivation_id: None,
            source: None,
        });

        let type_def = make_complex_type_with_content(content);
        let matcher = compile_content_model_matcher(&schema_set, &type_def).unwrap();
        assert!(matches!(matcher, ContentModelMatcher::Nfa(_)));
    }

    #[test]
    fn test_extension_from_all_group_base_no_own_particles() {
        use crate::parser::frames::ComplexContentDefResult;

        let mut schema_set = SchemaSet::new();

        // Create base type with all-group
        let base_all = make_all_particle(vec![
            make_element_particle(NameId(1), 1, Some(1)),
            make_element_particle(NameId(2), 1, Some(1)),
        ]);
        let base_content = ComplexContentResult::Complex(ComplexContentDefResult {
            particle: Some(base_all),
            derivation: DerivationMethod::Restriction,
            mixed: false,
            base_type: None,
            open_content: None,
            attributes: vec![],
            attribute_groups: vec![],
            attribute_wildcard: None,
            assertions: vec![],
            id: None,
            derivation_id: None,
            source: None,
        });
        let base_ct = make_complex_type_data(None, base_content);
        let base_key = schema_set.arenas.alloc_complex_type(base_ct);

        // Create extension type with no own particle
        let ext_content = ComplexContentResult::Complex(ComplexContentDefResult {
            particle: None,
            derivation: DerivationMethod::Extension,
            mixed: false,
            base_type: None,
            open_content: None,
            attributes: vec![],
            attribute_groups: vec![],
            attribute_wildcard: None,
            assertions: vec![],
            id: None,
            derivation_id: None,
            source: None,
        });
        let mut ext_type = make_complex_type_data(None, ext_content);
        ext_type.derivation_method = Some(DerivationMethod::Extension);
        ext_type.resolved_base_type = Some(TypeKey::Complex(base_key));

        let matcher = compile_content_model_matcher(&schema_set, &ext_type).unwrap();
        // Extension with no own particles should inherit AllGroup from base
        assert!(matches!(matcher, ContentModelMatcher::AllGroup(_)));
    }

    #[test]
    fn test_extension_from_all_group_base_with_own_particles() {
        use crate::parser::frames::ComplexContentDefResult;

        let mut schema_set = SchemaSet::new();

        // Create base type with all-group
        let base_all = make_all_particle(vec![
            make_element_particle(NameId(1), 1, Some(1)),
        ]);
        let base_content = ComplexContentResult::Complex(ComplexContentDefResult {
            particle: Some(base_all),
            derivation: DerivationMethod::Restriction,
            mixed: false,
            base_type: None,
            open_content: None,
            attributes: vec![],
            attribute_groups: vec![],
            attribute_wildcard: None,
            assertions: vec![],
            id: None,
            derivation_id: None,
            source: None,
        });
        let base_ct = make_complex_type_data(None, base_content);
        let base_key = schema_set.arenas.alloc_complex_type(base_ct);

        // Create extension type with its own sequence particle
        let ext_seq = make_sequence_particle(vec![
            make_element_particle(NameId(3), 1, Some(1)),
        ]);
        let ext_content = ComplexContentResult::Complex(ComplexContentDefResult {
            particle: Some(ext_seq),
            derivation: DerivationMethod::Extension,
            mixed: false,
            base_type: None,
            open_content: None,
            attributes: vec![],
            attribute_groups: vec![],
            attribute_wildcard: None,
            assertions: vec![],
            id: None,
            derivation_id: None,
            source: None,
        });
        let mut ext_type = make_complex_type_data(None, ext_content);
        ext_type.derivation_method = Some(DerivationMethod::Extension);
        ext_type.resolved_base_type = Some(TypeKey::Complex(base_key));

        let matcher = compile_content_model_matcher(&schema_set, &ext_type).unwrap();
        // XSD 1.0: AllGroup converted to NFA, concat with own → Nfa
        // XSD 1.1: AllGroup base + sequence extension → AllGroupExtension
        #[cfg(not(feature = "xsd11"))]
        assert!(matches!(matcher, ContentModelMatcher::Nfa(_)));
        #[cfg(feature = "xsd11")]
        assert!(matches!(matcher, ContentModelMatcher::AllGroupExtension { .. }));
    }

    #[test]
    fn test_attach_open_content_all_group() {
        use crate::types::complex::{
            OpenContent, OpenContentMode as TypesOpenContentMode, WildcardRef,
            NamespaceConstraint, ProcessContents as TypesProcessContents,
        };

        let a_name = NameId(1);
        let model = AllGroupModel::new(vec![
            AllParticle::new(
                NfaTerm::element(a_name, None, None),
                1,
                MaxOccurs::Bounded(1),
                None,
            ),
        ]);
        let matcher = ContentModelMatcher::AllGroup(model);
        let oc = OpenContent {
            mode: TypesOpenContentMode::Interleave,
            wildcard: Some(WildcardRef {
                namespace_constraint: NamespaceConstraint::Any,
                process_contents: TypesProcessContents::Lax,
                source: None,
            }),
            source: None,
        };

        let result = attach_open_content(matcher, Some(oc));
        match result {
            ContentModelMatcher::AllGroup(model) => {
                assert!(model.open_content.is_some(), "open content should be populated");
                let oc = model.open_content.unwrap();
                assert_eq!(oc.mode, crate::compiler::OpenContentMode::Interleave);
            }
            other => panic!("expected AllGroup, got {:?}", other),
        }
    }

    #[cfg(feature = "xsd11")]
    #[test]
    fn test_extension_merged_all_groups() {
        use crate::parser::frames::ComplexContentDefResult;

        let mut schema_set = SchemaSet::new();

        // Base type: all(A, B)
        let base_all = make_all_particle(vec![
            make_element_particle(NameId(1), 1, Some(1)),
            make_element_particle(NameId(2), 1, Some(1)),
        ]);
        let base_content = ComplexContentResult::Complex(ComplexContentDefResult {
            particle: Some(base_all),
            derivation: DerivationMethod::Restriction,
            mixed: false,
            base_type: None,
            open_content: None,
            attributes: vec![],
            attribute_groups: vec![],
            attribute_wildcard: None,
            assertions: vec![],
            id: None,
            derivation_id: None,
            source: None,
        });
        let base_ct = make_complex_type_data(None, base_content);
        let base_key = schema_set.arenas.alloc_complex_type(base_ct);

        // Extension type: all(C, D)
        let ext_all = make_all_particle(vec![
            make_element_particle(NameId(3), 1, Some(1)),
            make_element_particle(NameId(4), 0, Some(1)),
        ]);
        let ext_content = ComplexContentResult::Complex(ComplexContentDefResult {
            particle: Some(ext_all),
            derivation: DerivationMethod::Extension,
            mixed: false,
            base_type: None,
            open_content: None,
            attributes: vec![],
            attribute_groups: vec![],
            attribute_wildcard: None,
            assertions: vec![],
            id: None,
            derivation_id: None,
            source: None,
        });
        let mut ext_type = make_complex_type_data(None, ext_content);
        ext_type.derivation_method = Some(DerivationMethod::Extension);
        ext_type.resolved_base_type = Some(TypeKey::Complex(base_key));

        let matcher = compile_content_model_matcher(&schema_set, &ext_type).unwrap();
        // Two all-groups should merge into a single AllGroup with 4 particles
        match &matcher {
            ContentModelMatcher::AllGroup(model) => {
                assert_eq!(model.particle_count(), 4);
            }
            other => panic!("expected AllGroup, got {:?}", other),
        }
    }

    #[cfg(feature = "xsd11")]
    #[test]
    fn test_extension_all_group_base_with_sequence() {
        use crate::parser::frames::ComplexContentDefResult;

        let mut schema_set = SchemaSet::new();

        // Base type: all(A, B)
        let base_all = make_all_particle(vec![
            make_element_particle(NameId(1), 1, Some(1)),
            make_element_particle(NameId(2), 1, Some(1)),
        ]);
        let base_content = ComplexContentResult::Complex(ComplexContentDefResult {
            particle: Some(base_all),
            derivation: DerivationMethod::Restriction,
            mixed: false,
            base_type: None,
            open_content: None,
            attributes: vec![],
            attribute_groups: vec![],
            attribute_wildcard: None,
            assertions: vec![],
            id: None,
            derivation_id: None,
            source: None,
        });
        let base_ct = make_complex_type_data(None, base_content);
        let base_key = schema_set.arenas.alloc_complex_type(base_ct);

        // Extension type: sequence(C)
        let ext_seq = make_sequence_particle(vec![
            make_element_particle(NameId(3), 1, Some(1)),
        ]);
        let ext_content = ComplexContentResult::Complex(ComplexContentDefResult {
            particle: Some(ext_seq),
            derivation: DerivationMethod::Extension,
            mixed: false,
            base_type: None,
            open_content: None,
            attributes: vec![],
            attribute_groups: vec![],
            attribute_wildcard: None,
            assertions: vec![],
            id: None,
            derivation_id: None,
            source: None,
        });
        let mut ext_type = make_complex_type_data(None, ext_content);
        ext_type.derivation_method = Some(DerivationMethod::Extension);
        ext_type.resolved_base_type = Some(TypeKey::Complex(base_key));

        let matcher = compile_content_model_matcher(&schema_set, &ext_type).unwrap();
        match &matcher {
            ContentModelMatcher::AllGroupExtension { base_model, .. } => {
                assert_eq!(base_model.particle_count(), 2);
            }
            other => panic!("expected AllGroupExtension, got {:?}", other),
        }
    }
}
