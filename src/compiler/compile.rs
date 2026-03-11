//! NFA compilation functions
//!
//! This module implements the core compilation logic for transforming
//! XSD content model particles into NFAs.

use crate::arenas::{ComplexTypeDefData, ModelGroupData};
use crate::parser::frames::DerivationMethod;
use crate::ids::{ElementKey, NameId, TypeKey};
use crate::parser::frames::{
    Compositor, ComplexContentResult, ElementFrameResult, ModelGroupDefResult, NamespaceToken,
    NotQNameItem, OpenContentMode, OpenContentResult, ParticleResult, ParticleTerm,
    ProcessContents, QNameRef, TypeRefResult, WildcardNamespace, WildcardResult,
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
    /// Current sibling element QNames for ##definedSibling expansion in wildcard compilation.
    /// Set before compiling particles in a model group (sequence/choice/all).
    current_sibling_elements: Vec<(Option<NameId>, NameId)>,
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
            current_sibling_elements: Vec::new(),
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
        let mut namespace_constraint = self.convert_wildcard_namespace(&wildcard.namespace);
        let process_contents = self.convert_process_contents(wildcard.process_contents);

        // Override with notNamespace if present
        if let Some(not_ns) = self.convert_not_namespace(&wildcard.not_namespace) {
            namespace_constraint = not_ns;
        }

        // Expand notQName items — use current_sibling_elements for ##definedSibling
        let not_qnames = self.expand_not_qnames(&wildcard.not_qname);

        let nfa_term = NfaTerm::wildcard_with_not_qnames(namespace_constraint, process_contents, not_qnames);
        Ok(self.builder.single_term(nfa_term, source.cloned()))
    }

    /// Expand NotQNameItems into concrete (namespace, local_name) pairs.
    /// Uses `self.current_sibling_elements` for ##definedSibling expansion.
    fn expand_not_qnames(
        &self,
        items: &[NotQNameItem],
    ) -> Vec<(Option<NameId>, NameId)> {
        let mut result = Vec::new();
        for item in items {
            match item {
                NotQNameItem::QName { namespace, local_name } => {
                    result.push((*namespace, *local_name));
                }
                NotQNameItem::Defined => {
                    result.extend(expand_defined_element_qnames(self.schema_set));
                }
                NotQNameItem::DefinedSibling => {
                    result.extend_from_slice(&self.current_sibling_elements);
                }
            }
        }
        result
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

        // Set sibling elements for ##definedSibling expansion in wildcards
        let new_siblings = self.collect_sibling_element_qnames(particles);
        let saved_siblings = std::mem::replace(
            &mut self.current_sibling_elements,
            new_siblings,
        );

        let mut result = self.compile_particle_with_index(&particles[0], 0)?;
        for (i, particle) in particles[1..].iter().enumerate() {
            let frag = self.compile_particle_with_index(particle, i + 1)?;
            result = result.concat(frag);
        }

        self.current_sibling_elements = saved_siblings;
        Ok(result)
    }

    /// Compile a choice (xs:choice)
    fn compile_choice(&mut self, particles: &[ParticleResult]) -> NfaCompileResult<NfaFragment> {
        if particles.is_empty() {
            return Ok(self.builder.epsilon_fragment());
        }

        // Set sibling elements for ##definedSibling expansion in wildcards
        let new_siblings = self.collect_sibling_element_qnames(particles);
        let saved_siblings = std::mem::replace(
            &mut self.current_sibling_elements,
            new_siblings,
        );

        let mut result = self.compile_particle_with_index(&particles[0], 0)?;
        for (i, particle) in particles[1..].iter().enumerate() {
            let frag = self.compile_particle_with_index(particle, i + 1)?;
            result = result.alternate(frag);
        }

        self.current_sibling_elements = saved_siblings;
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
    /// min/max occurs, and source location. Element and wildcard particles
    /// are supported directly. In XSD 1.1, group references inside all-groups
    /// are flattened per cos-all-limited constraints (the referenced group must
    /// be an all-group with minOccurs=maxOccurs=1). XSD 1.0 rejects group refs.
    fn compile_all_group_model(
        &mut self,
        particles: &[ParticleResult],
        source: Option<&SourceRef>,
    ) -> NfaCompileResult<AllGroupModel> {
        // Set sibling elements for ##definedSibling expansion in wildcards
        let new_siblings = self.collect_sibling_element_qnames(particles);
        let saved_siblings = std::mem::replace(
            &mut self.current_sibling_elements,
            new_siblings,
        );

        let mut all_particles = Vec::with_capacity(particles.len());

        for particle in particles {
            let term = match &particle.term {
                ParticleTerm::Element(elem) => {
                    self.build_element_term(elem, particle.source.as_ref().or(source))?
                }
                ParticleTerm::Any(wildcard) => {
                    let mut ns = self.convert_wildcard_namespace(&wildcard.namespace);
                    let pc = self.convert_process_contents(wildcard.process_contents);
                    // Override with notNamespace if present
                    if let Some(not_ns) = self.convert_not_namespace(&wildcard.not_namespace) {
                        ns = not_ns;
                    }
                    let not_qnames = self.expand_not_qnames(&wildcard.not_qname);
                    NfaTerm::wildcard_with_not_qnames(ns, pc, not_qnames)
                }
                #[cfg(feature = "xsd11")]
                ParticleTerm::Group(group) => {
                    // XSD 1.0 forbids group refs inside xs:all even in an xsd11 build
                    if self.schema_set.xsd_version != XsdVersion::V1_1 {
                        return Err(NfaCompileError::invalid_all_group(
                            particle.source.clone().or_else(|| source.cloned()),
                        ));
                    }
                    // cos-all-limited 1.3: minOccurs = maxOccurs = 1
                    if particle.min_occurs != 1 || particle.max_occurs != Some(1) {
                        return Err(NfaCompileError::InvalidAllGroupOccurs {
                            reason: "cos-all-limited.1.3: group reference inside xs:all \
                                     must have minOccurs = maxOccurs = 1"
                                .into(),
                            location: particle.source.clone().or_else(|| source.cloned()),
                        });
                    }
                    // Must be a group reference, not an inline group.
                    if group.ref_name.is_none() {
                        return Err(NfaCompileError::invalid_all_group(
                            particle.source.clone().or_else(|| source.cloned()),
                        ));
                    }
                    // Flatten: resolve group ref, verify compositor, inline particles
                    self.flatten_all_group_ref_into(
                        group,
                        particle.source.as_ref().or(source),
                        &mut all_particles,
                    )?;
                    continue; // particles already added, skip the push below
                }
                #[cfg(not(feature = "xsd11"))]
                ParticleTerm::Group(_) => {
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

        self.current_sibling_elements = saved_siblings;
        Ok(AllGroupModel::new(all_particles))
    }

    /// Flatten a group reference inside an xs:all group into the parent's
    /// `all_particles` vector.
    ///
    /// **Preconditions** (checked by caller):
    /// - `group.ref_name` is `Some` (this is a group reference, not inline)
    /// - The containing particle has `minOccurs = maxOccurs = 1`
    ///
    /// This method resolves the group ref, verifies the referenced group's
    /// compositor is `All` (cos-all-limited rule 2), then recursively compiles
    /// each inner particle into the parent's `all_particles`.
    #[cfg(feature = "xsd11")]
    fn flatten_all_group_ref_into(
        &mut self,
        group: &ModelGroupDefResult,
        source: Option<&SourceRef>,
        all_particles: &mut Vec<AllParticle>,
    ) -> NfaCompileResult<()> {
        // Recursion guard
        self.check_recursion(source)?;
        self.depth += 1;

        let ref_name = group.ref_name.as_ref().expect("caller checked ref_name is Some");

        // Resolve the group ref
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

        // cos-all-limited rule 2: compositor must be All
        let compositor = group_data.compositor.unwrap_or(Compositor::Sequence);
        if compositor != Compositor::All {
            self.depth -= 1;
            return Err(NfaCompileError::InvalidAllGroupContent {
                location: source.cloned(),
            });
        }

        // Save context and set up flat indexing for the referenced group
        let saved_flat_idx = self.content_flat_idx.take();
        let saved_particle_elements = std::mem::take(&mut self.resolved_particle_elements);
        let saved_types = std::mem::take(&mut self.resolved_particle_types);
        let saved_idx = self.current_particle_idx;

        self.resolved_particle_types = group_data.resolved_particle_types.clone();
        self.resolved_particle_elements = group_data.resolved_particle_elements.clone();
        self.content_flat_idx = Some(0);

        // Compile each inner particle
        let result = self.flatten_all_group_particles(
            &group_data.particles,
            source,
            all_particles,
        );

        // Restore context
        self.content_flat_idx = saved_flat_idx;
        self.resolved_particle_elements = saved_particle_elements;
        self.resolved_particle_types = saved_types;
        self.current_particle_idx = saved_idx;
        self.depth -= 1;

        result
    }

    /// Compile inner particles of a referenced all-group into the parent's
    /// `all_particles` vector. Called by `flatten_all_group_ref_into`.
    #[cfg(feature = "xsd11")]
    fn flatten_all_group_particles(
        &mut self,
        particles: &[ParticleResult],
        source: Option<&SourceRef>,
        all_particles: &mut Vec<AllParticle>,
    ) -> NfaCompileResult<()> {
        for particle in particles {
            let term = match &particle.term {
                ParticleTerm::Element(elem) => {
                    self.build_element_term(elem, particle.source.as_ref().or(source))?
                }
                ParticleTerm::Any(wildcard) => {
                    let mut ns = self.convert_wildcard_namespace(&wildcard.namespace);
                    let pc = self.convert_process_contents(wildcard.process_contents);
                    if let Some(not_ns) = self.convert_not_namespace(&wildcard.not_namespace) {
                        ns = not_ns;
                    }
                    let not_qnames = self.expand_not_qnames(&wildcard.not_qname);
                    NfaTerm::wildcard_with_not_qnames(ns, pc, not_qnames)
                }
                ParticleTerm::Group(inner_group) => {
                    // Nested group ref inside the referenced all-group — recurse
                    // cos-all-limited 1.3: minOccurs = maxOccurs = 1
                    if particle.min_occurs != 1 || particle.max_occurs != Some(1) {
                        return Err(NfaCompileError::InvalidAllGroupOccurs {
                            reason: "cos-all-limited.1.3: group reference inside xs:all \
                                     must have minOccurs = maxOccurs = 1"
                                .into(),
                            location: particle.source.clone().or_else(|| source.cloned()),
                        });
                    }
                    if inner_group.ref_name.is_none() {
                        return Err(NfaCompileError::invalid_all_group(
                            particle.source.clone().or_else(|| source.cloned()),
                        ));
                    }
                    self.flatten_all_group_ref_into(
                        inner_group,
                        particle.source.as_ref().or(source),
                        all_particles,
                    )?;
                    continue;
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
        Ok(())
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
            WildcardNamespace::List(list) => {
                NamespaceConstraint::List(
                    list.iter().map(|t| t.resolve(self.target_namespace)).collect()
                )
            }
        }
    }

    /// Convert WildcardResult's not_namespace to NamespaceConstraint::Not if non-empty.
    /// Returns None if not_namespace is empty (no override).
    fn convert_not_namespace(&self, not_namespace: &[NamespaceToken]) -> Option<NamespaceConstraint> {
        if not_namespace.is_empty() {
            return None;
        }
        let excluded: Vec<Option<NameId>> = not_namespace.iter()
            .map(|t| t.resolve(self.target_namespace))
            .collect();
        Some(NamespaceConstraint::Not(excluded))
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

    /// Collect sibling element QNames from a particle list, using proper
    /// namespace resolution (element refs, form attribute, elementFormDefault).
    /// Used for ##definedSibling expansion. Recurses into group refs to
    /// include elements from referenced groups.
    fn collect_sibling_element_qnames(&self, particles: &[ParticleResult]) -> Vec<(Option<NameId>, NameId)> {
        self.collect_sibling_element_qnames_inner(particles, 0)
    }

    fn collect_sibling_element_qnames_inner(
        &self,
        particles: &[ParticleResult],
        depth: usize,
    ) -> Vec<(Option<NameId>, NameId)> {
        // Defense-in-depth: bail on unreasonably deep nesting
        if depth >= MAX_RECURSION_DEPTH {
            return Vec::new();
        }
        let mut result = Vec::new();
        for p in particles {
            match &p.term {
                ParticleTerm::Element(elem) => {
                    if let Some(ref_name) = &elem.ref_name {
                        // Element reference — use the ref's resolved QName
                        result.push((ref_name.namespace, ref_name.local_name));
                    } else if let Some(name) = elem.name {
                        // Local element — resolve namespace through form/elementFormDefault
                        let source = p.source.as_ref().or(elem.source.as_ref());
                        let ns = self.effective_element_namespace(elem, source);
                        result.push((ns, name));
                    }
                }
                ParticleTerm::Group(group) => {
                    if let Some(ref_name) = &group.ref_name {
                        if let Some(key) = self.schema_set.lookup_model_group(
                            ref_name.namespace,
                            ref_name.local_name,
                        ) {
                            if let Some(data) = self.schema_set.arenas.get_model_group(key) {
                                result.extend(
                                    self.collect_sibling_element_qnames_inner(
                                        &data.particles,
                                        depth + 1,
                                    ),
                                );
                            }
                        }
                    }
                }
                ParticleTerm::Any(_) => {}
            }
        }
        result
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
        ContentModelMatcher::Nfa(nfa) => {
            let wildcard = open_content.wildcard.map(|mut w| {
                if w.has_defined_sibling {
                    w.not_qnames.extend(collect_nfa_element_qnames(&nfa));
                    w.has_defined_sibling = false;
                }
                w
            });
            ContentModelMatcher::WithOpenContent {
                nfa,
                mode: open_content.mode,
                wildcard,
            }
        }
        ContentModelMatcher::AllGroup(mut model) => {
            if let Some(mut wildcard_ref) = open_content.wildcard {
                if wildcard_ref.has_defined_sibling {
                    wildcard_ref.not_qnames.extend(collect_all_group_element_qnames(&model));
                    wildcard_ref.has_defined_sibling = false;
                }
                let mode = match open_content.mode {
                    TypesOpenContentMode::Interleave => AllGroupOpenContentMode::Interleave,
                    TypesOpenContentMode::Suffix => AllGroupOpenContentMode::Suffix,
                    TypesOpenContentMode::None => AllGroupOpenContentMode::None,
                };
                model.open_content = Some(OpenContentWildcard {
                    namespace_constraint: wildcard_ref.namespace_constraint,
                    process_contents: wildcard_ref.process_contents,
                    mode,
                    not_qnames: wildcard_ref.not_qnames,
                });
            }
            ContentModelMatcher::AllGroup(model)
        }
        #[cfg(feature = "xsd11")]
        ContentModelMatcher::AllGroupExtension { mut base_model, extension_nfa } => {
            if let Some(mut wildcard_ref) = open_content.wildcard {
                if wildcard_ref.has_defined_sibling {
                    // Collect siblings from both the base all-group and extension NFA
                    wildcard_ref.not_qnames.extend(collect_all_group_element_qnames(&base_model));
                    wildcard_ref.not_qnames.extend(collect_nfa_element_qnames(&extension_nfa));
                    wildcard_ref.has_defined_sibling = false;
                }
                let mode = match open_content.mode {
                    TypesOpenContentMode::Interleave => AllGroupOpenContentMode::Interleave,
                    TypesOpenContentMode::Suffix => AllGroupOpenContentMode::Suffix,
                    TypesOpenContentMode::None => AllGroupOpenContentMode::None,
                };
                base_model.open_content = Some(OpenContentWildcard {
                    namespace_constraint: wildcard_ref.namespace_constraint,
                    process_contents: wildcard_ref.process_contents,
                    mode,
                    not_qnames: wildcard_ref.not_qnames,
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
        let target_namespace = source
            .and_then(|s| schema_set.documents.get(s.doc_id as usize))
            .and_then(|d| d.target_namespace);
        return open_content_from_result(explicit, schema_set, target_namespace);
    }

    if !matches!(content, ComplexContentResult::Complex(_) | ComplexContentResult::Empty) {
        return None;
    }

    let doc = source.and_then(|s| schema_set.documents.get(s.doc_id as usize));
    let default = doc.and_then(|d| d.default_open_content.as_ref())?;

    if !default.applies_to_empty && content_is_empty(content) {
        return None;
    }

    open_content_from_default(default, schema_set)
}

fn content_is_empty(content: &ComplexContentResult) -> bool {
    match content {
        ComplexContentResult::Empty => true,
        ComplexContentResult::Complex(def) => def.particle.is_none(),
        ComplexContentResult::Simple(_) => false,
    }
}

fn open_content_from_result(
    result: &OpenContentResult,
    schema_set: &SchemaSet,
    target_namespace: Option<NameId>,
) -> Option<OpenContent> {
    let mode = convert_open_content_mode(result.mode);
    if matches!(mode, TypesOpenContentMode::None) {
        return None;
    }

    Some(OpenContent {
        mode,
        wildcard: result.wildcard.as_ref().map(|w| wildcard_ref_from_result(w, schema_set, target_namespace)),
        source: result.source.clone(),
    })
}

fn open_content_from_default(
    default: &DefaultOpenContent,
    schema_set: &SchemaSet,
) -> Option<OpenContent> {
    let mode = convert_schema_open_content_mode(default.mode);
    if matches!(mode, TypesOpenContentMode::None) {
        return None;
    }

    Some(OpenContent {
        mode,
        wildcard: default.wildcard.as_ref().map(|w| wildcard_ref_from_default(w, schema_set)),
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

/// Expand all globally declared element QNames from the schema set.
fn expand_defined_element_qnames(schema_set: &SchemaSet) -> Vec<(Option<NameId>, NameId)> {
    schema_set.namespaces.iter()
        .flat_map(|(ns, table)| {
            table.elements.keys().map(move |name| (*ns, *name))
        })
        .collect()
}

/// Collect all element QNames from an NFA content model (for ##definedSibling expansion).
fn collect_nfa_element_qnames(nfa: &NfaTable) -> Vec<(Option<NameId>, NameId)> {
    let mut result = Vec::new();
    for state in &nfa.states {
        if let Some(NfaTerm::Element { namespace, name, .. }) = &state.term {
            let qname = (*namespace, *name);
            if !result.contains(&qname) {
                result.push(qname);
            }
        }
    }
    result
}

/// Collect all element QNames from an all-group model (for ##definedSibling expansion).
fn collect_all_group_element_qnames(model: &AllGroupModel) -> Vec<(Option<NameId>, NameId)> {
    let mut result = Vec::new();
    for particle in &model.particles {
        if let NfaTerm::Element { namespace, name, .. } = &particle.term {
            let qname = (*namespace, *name);
            if !result.contains(&qname) {
                result.push(qname);
            }
        }
    }
    result
}

fn wildcard_ref_from_result(
    wildcard: &WildcardResult,
    schema_set: &SchemaSet,
    target_namespace: Option<NameId>,
) -> WildcardRef {
    let mut namespace_constraint = match &wildcard.namespace {
        WildcardNamespace::Any => NamespaceConstraint::Any,
        WildcardNamespace::Other => NamespaceConstraint::Other,
        WildcardNamespace::TargetNamespace => NamespaceConstraint::TargetNamespace,
        WildcardNamespace::Local => NamespaceConstraint::Local,
        WildcardNamespace::List(list) => {
            NamespaceConstraint::List(
                list.iter().map(|t| t.resolve(target_namespace)).collect()
            )
        }
    };

    // Override with notNamespace if present
    if !wildcard.not_namespace.is_empty() {
        let excluded: Vec<Option<NameId>> = wildcard.not_namespace.iter()
            .map(|t| t.resolve(target_namespace))
            .collect();
        namespace_constraint = NamespaceConstraint::Not(excluded);
    }

    // Expand notQName — resolve ##defined to concrete QNames using schema_set
    let mut not_qnames: Vec<(Option<NameId>, NameId)> = Vec::new();
    let mut has_defined_sibling = false;
    for item in &wildcard.not_qname {
        match item {
            NotQNameItem::QName { namespace, local_name } => {
                not_qnames.push((*namespace, *local_name));
            }
            NotQNameItem::Defined => {
                not_qnames.extend(expand_defined_element_qnames(schema_set));
            }
            NotQNameItem::DefinedSibling => {
                // Defer: sibling context not yet available for open content wildcards.
                // Resolved in attach_open_content when siblings are known.
                has_defined_sibling = true;
            }
        }
    }

    let process_contents = match wildcard.process_contents {
        ProcessContents::Strict => TypesProcessContents::Strict,
        ProcessContents::Lax => TypesProcessContents::Lax,
        ProcessContents::Skip => TypesProcessContents::Skip,
    };

    WildcardRef {
        namespace_constraint,
        process_contents,
        not_qnames,
        has_defined_sibling,
        source: wildcard.source.clone(),
    }
}

fn wildcard_ref_from_default(
    wildcard: &ElementWildcard,
    schema_set: &SchemaSet,
) -> WildcardRef {
    let namespace_constraint = match &wildcard.namespace_constraint {
        SchemaNamespaceConstraint::Any => NamespaceConstraint::Any,
        SchemaNamespaceConstraint::Other => NamespaceConstraint::Other,
        SchemaNamespaceConstraint::Enumeration(list) => NamespaceConstraint::List(list.clone()),
        SchemaNamespaceConstraint::Not(excluded) => NamespaceConstraint::Not(excluded.clone()),
    };

    // Expand not_qnames from ElementWildcard — resolve ##defined using schema_set
    let mut not_qnames: Vec<(Option<NameId>, NameId)> = Vec::new();
    let mut has_defined_sibling = false;
    for item in &wildcard.not_qnames {
        match item {
            crate::schema::wildcard::QNameDisallowed::QName { namespace, local_name } => {
                not_qnames.push((*namespace, *local_name));
            }
            crate::schema::wildcard::QNameDisallowed::Defined => {
                not_qnames.extend(expand_defined_element_qnames(schema_set));
            }
            crate::schema::wildcard::QNameDisallowed::DefinedSibling => {
                // Defer: sibling context not yet available for open content wildcards.
                // Resolved in attach_open_content when siblings are known.
                has_defined_sibling = true;
            }
        }
    }

    let process_contents = match wildcard.process_contents {
        crate::schema::wildcard::ProcessContents::Strict => TypesProcessContents::Strict,
        crate::schema::wildcard::ProcessContents::Lax => TypesProcessContents::Lax,
        crate::schema::wildcard::ProcessContents::Skip => TypesProcessContents::Skip,
    };

    WildcardRef {
        namespace_constraint,
        process_contents,
        not_qnames,
        has_defined_sibling,
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
                not_qnames: Vec::new(),
                has_defined_sibling: false,
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

    // ========================================================================
    // End-to-end wildcard conversion tests
    // ========================================================================

    #[test]
    fn test_wildcard_ref_from_result_resolves_target_namespace_in_list() {
        let schema_set = SchemaSet::with_version(XsdVersion::V1_1);
        let target_ns = schema_set.name_table.add("http://target.example.com");
        let other_ns = schema_set.name_table.add("http://other.example.com");

        use NamespaceToken;

        let wildcard = WildcardResult {
            namespace: WildcardNamespace::List(vec![
                NamespaceToken::TargetNamespace,
                NamespaceToken::Uri(other_ns),
                NamespaceToken::Local,
            ]),
            process_contents: ProcessContents::Lax,
            not_namespace: Vec::new(),
            not_qname: Vec::new(),
            id: None,
            annotation: None,
            source: None,
        };

        let wref = wildcard_ref_from_result(&wildcard, &schema_set, Some(target_ns));
        match &wref.namespace_constraint {
            NamespaceConstraint::List(list) => {
                assert_eq!(list.len(), 3);
                assert_eq!(list[0], Some(target_ns), "##targetNamespace should resolve to target_ns");
                assert_eq!(list[1], Some(other_ns));
                assert_eq!(list[2], None, "##local should resolve to None");
            }
            other => panic!("expected List, got {:?}", other),
        }
    }

    #[test]
    fn test_wildcard_ref_from_result_resolves_target_namespace_in_not_namespace() {
        let schema_set = SchemaSet::with_version(XsdVersion::V1_1);
        let target_ns = schema_set.name_table.add("http://target.example.com");

        use NamespaceToken;

        let wildcard = WildcardResult {
            namespace: WildcardNamespace::Any,
            process_contents: ProcessContents::Lax,
            not_namespace: vec![NamespaceToken::TargetNamespace],
            not_qname: Vec::new(),
            id: None,
            annotation: None,
            source: None,
        };

        let wref = wildcard_ref_from_result(&wildcard, &schema_set, Some(target_ns));
        match &wref.namespace_constraint {
            NamespaceConstraint::Not(excluded) => {
                assert_eq!(excluded.len(), 1);
                assert_eq!(excluded[0], Some(target_ns), "##targetNamespace in notNamespace should resolve to target_ns");
            }
            other => panic!("expected Not, got {:?}", other),
        }
    }

    #[test]
    fn test_wildcard_ref_from_result_expands_defined() {
        let mut schema_set = SchemaSet::with_version(XsdVersion::V1_1);
        let ns = schema_set.name_table.add("http://example.com");
        let elem_name = schema_set.name_table.add("foo");

        // Register a globally declared element in the schema
        schema_set.namespaces.entry(Some(ns)).or_default().elements.insert(elem_name, Default::default());

        use NotQNameItem;

        let wildcard = WildcardResult {
            namespace: WildcardNamespace::Any,
            process_contents: ProcessContents::Lax,
            not_namespace: Vec::new(),
            not_qname: vec![NotQNameItem::Defined],
            id: None,
            annotation: None,
            source: None,
        };

        let wref = wildcard_ref_from_result(&wildcard, &schema_set, None);
        assert!(
            wref.not_qnames.contains(&(Some(ns), elem_name)),
            "##defined should expand to include globally declared element (ns, foo)"
        );
    }

    #[test]
    fn test_wildcard_ref_from_default_expands_defined() {
        use crate::schema::wildcard::QNameDisallowed;

        let mut schema_set = SchemaSet::with_version(XsdVersion::V1_1);
        let ns = schema_set.name_table.add("http://example.com");
        let elem_name = schema_set.name_table.add("bar");

        // Register a globally declared element
        schema_set.namespaces.entry(Some(ns)).or_default().elements.insert(elem_name, Default::default());

        let mut wildcard = ElementWildcard::any_lax();
        wildcard.not_qnames = vec![QNameDisallowed::Defined];

        let wref = wildcard_ref_from_default(&wildcard, &schema_set);
        assert!(
            wref.not_qnames.contains(&(Some(ns), elem_name)),
            "##defined in default open content should expand to include globally declared element"
        );
    }

    #[test]
    fn test_open_content_from_result_e2e_with_not_namespace() {
        // End-to-end: compile a type with explicit open content using notNamespace=##targetNamespace
        let mut schema_set = SchemaSet::with_version(XsdVersion::V1_1);
        let target_ns = schema_set.name_table.add("http://target.example.com");

        let doc_id = schema_set.documents.len() as u32;
        let mut doc = SchemaDocument::new(doc_id, "test.xsd".to_string());
        doc.target_namespace = Some(target_ns);
        schema_set.documents.push(doc);

        use NamespaceToken;

        let oc_result = OpenContentResult {
            mode: OpenContentMode::Interleave,
            wildcard: Some(WildcardResult {
                namespace: WildcardNamespace::Any,
                process_contents: ProcessContents::Lax,
                not_namespace: vec![NamespaceToken::TargetNamespace],
                not_qname: Vec::new(),
                id: None,
                annotation: None,
                source: None,
            }),
            id: None,
            annotation: None,
            source: None,
        };

        let oc = open_content_from_result(&oc_result, &schema_set, Some(target_ns));
        assert!(oc.is_some());
        let oc = oc.unwrap();
        let wildcard = oc.wildcard.unwrap();
        match &wildcard.namespace_constraint {
            NamespaceConstraint::Not(excluded) => {
                assert_eq!(excluded, &vec![Some(target_ns)]);
            }
            other => panic!("expected Not constraint, got {:?}", other),
        }
    }

    #[test]
    fn test_default_open_content_e2e_with_defined() {
        // End-to-end: compile a type using default open content with ##defined notQName
        use crate::schema::wildcard::QNameDisallowed;

        let mut schema_set = SchemaSet::with_version(XsdVersion::V1_1);
        let ns = schema_set.name_table.add("http://example.com");
        let elem_name = schema_set.name_table.add("globalElem");

        // Register a globally declared element
        schema_set.namespaces.entry(Some(ns)).or_default().elements.insert(elem_name, Default::default());

        let doc_id = schema_set.documents.len() as u32;
        let mut doc = SchemaDocument::new(doc_id, "test.xsd".to_string());
        let mut wc = ElementWildcard::any_lax();
        wc.not_qnames = vec![QNameDisallowed::Defined];
        doc.default_open_content = Some(DefaultOpenContent {
            source: None,
            applies_to_empty: true,
            mode: SchemaOpenContentMode::Interleave,
            wildcard: Some(wc),
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
            ContentModelMatcher::WithOpenContent { wildcard, .. } => {
                let wref = wildcard.expect("wildcard should be present");
                assert!(
                    wref.not_qnames.contains(&(Some(ns), elem_name)),
                    "##defined should expand to include globally declared element through full compilation path"
                );
            }
            _ => panic!("expected open content wrapper"),
        }
    }

    #[test]
    fn test_collect_sibling_element_qnames_with_ref() {
        // Verify that collect_sibling_element_qnames handles element refs properly
        let schema_set = SchemaSet::with_version(XsdVersion::V1_1);
        let ref_name = schema_set.name_table.add("refElem");
        let ref_ns = schema_set.name_table.add("http://ref.example.com");
        let local_name = schema_set.name_table.add("localElem");

        let ctx = CompileContext::new(&schema_set, None);

        let particles = vec![
            // Element ref
            ParticleResult {
                term: ParticleTerm::Element(ElementFrameResult {
                    name: None,
                    ref_name: Some(QNameRef {
                        prefix: None,
                        local_name: ref_name,
                        namespace: Some(ref_ns),
                    }),
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
                min_occurs: 1,
                max_occurs: Some(1),
                source: None,
            },
            // Local element
            make_element_particle(local_name, 1, Some(1)),
        ];

        let siblings = ctx.collect_sibling_element_qnames(&particles);
        assert_eq!(siblings.len(), 2);
        assert!(
            siblings.contains(&(Some(ref_ns), ref_name)),
            "should include element ref with resolved namespace"
        );
        assert!(
            siblings.contains(&(None, local_name)),
            "should include local element with proper namespace"
        );
    }

    #[test]
    fn test_defined_sibling_expansion_in_sequence() {
        // Verify that ##definedSibling expands to sibling elements in a sequence
        use NotQNameItem;

        let schema_set = SchemaSet::with_version(XsdVersion::V1_1);
        let elem_a = schema_set.name_table.add("a");
        let elem_b = schema_set.name_table.add("b");

        // Build sequence: <a/> <xs:any notQName="##definedSibling"/>
        let wildcard_particle = ParticleResult {
            term: ParticleTerm::Any(WildcardResult {
                namespace: WildcardNamespace::Any,
                process_contents: ProcessContents::Lax,
                not_namespace: Vec::new(),
                not_qname: vec![NotQNameItem::DefinedSibling],
                id: None,
                annotation: None,
                source: None,
            }),
            min_occurs: 0,
            max_occurs: None,
            source: None,
        };

        let sequence = make_sequence_particle(vec![
            make_element_particle(elem_a, 1, Some(1)),
            make_element_particle(elem_b, 1, Some(1)),
            wildcard_particle,
        ]);

        let nfa = compile_particle(&schema_set, &sequence, None).unwrap();

        // The wildcard in the NFA should have not_qnames excluding siblings a and b
        let mut found_wildcard = false;
        for state in &nfa.states {
            if let Some(NfaTerm::Wildcard { not_qnames, .. }) = &state.term {
                found_wildcard = true;
                assert!(
                    not_qnames.contains(&(None, elem_a)),
                    "##definedSibling should exclude sibling element 'a'"
                );
                assert!(
                    not_qnames.contains(&(None, elem_b)),
                    "##definedSibling should exclude sibling element 'b'"
                );
            }
        }
        assert!(found_wildcard, "NFA should contain a wildcard state");
    }

    #[test]
    fn test_defined_sibling_open_content_nfa() {
        // ##definedSibling in open content wildcard should expand to sibling elements
        // from the NFA content model when attached via attach_open_content
        use NotQNameItem;

        let schema_set = SchemaSet::with_version(XsdVersion::V1_1);
        let elem_a = schema_set.name_table.add("a");
        let elem_b = schema_set.name_table.add("b");

        // Build a sequence: <a/> <b/>
        let sequence = make_sequence_particle(vec![
            make_element_particle(elem_a, 1, Some(1)),
            make_element_particle(elem_b, 1, Some(1)),
        ]);
        let nfa = compile_particle(&schema_set, &sequence, None).unwrap();
        let matcher = ContentModelMatcher::Nfa(nfa);

        // Build open content with ##definedSibling
        let oc_result = OpenContentResult {
            mode: OpenContentMode::Interleave,
            wildcard: Some(WildcardResult {
                namespace: WildcardNamespace::Any,
                process_contents: ProcessContents::Lax,
                not_namespace: Vec::new(),
                not_qname: vec![NotQNameItem::DefinedSibling],
                id: None,
                annotation: None,
                source: None,
            }),
            id: None,
            annotation: None,
            source: None,
        };
        let oc = open_content_from_result(&oc_result, &schema_set, None).unwrap();

        // has_defined_sibling should be set
        assert!(oc.wildcard.as_ref().unwrap().has_defined_sibling);

        let result = attach_open_content(matcher, Some(oc));
        match result {
            ContentModelMatcher::WithOpenContent { wildcard, .. } => {
                let wref = wildcard.expect("wildcard should be present");
                assert!(!wref.has_defined_sibling, "has_defined_sibling should be resolved");
                assert!(
                    wref.not_qnames.contains(&(None, elem_a)),
                    "##definedSibling should exclude sibling element 'a'"
                );
                assert!(
                    wref.not_qnames.contains(&(None, elem_b)),
                    "##definedSibling should exclude sibling element 'b'"
                );
            }
            _ => panic!("expected WithOpenContent"),
        }
    }

    #[test]
    fn test_defined_sibling_open_content_all_group() {
        // ##definedSibling in open content wildcard should expand to sibling elements
        // from AllGroup content model when attached via attach_open_content
        use NotQNameItem;

        let schema_set = SchemaSet::with_version(XsdVersion::V1_1);
        let elem_x = schema_set.name_table.add("x");
        let elem_y = schema_set.name_table.add("y");

        // Build all-group with elements x and y
        let model = AllGroupModel::new(vec![
            AllParticle::new(
                NfaTerm::element(elem_x, None, None),
                1,
                MaxOccurs::Bounded(1),
                None,
            ),
            AllParticle::new(
                NfaTerm::element(elem_y, None, None),
                1,
                MaxOccurs::Bounded(1),
                None,
            ),
        ]);
        let matcher = ContentModelMatcher::AllGroup(model);

        // Build open content with ##definedSibling
        let oc_result = OpenContentResult {
            mode: OpenContentMode::Suffix,
            wildcard: Some(WildcardResult {
                namespace: WildcardNamespace::Any,
                process_contents: ProcessContents::Lax,
                not_namespace: Vec::new(),
                not_qname: vec![NotQNameItem::DefinedSibling],
                id: None,
                annotation: None,
                source: None,
            }),
            id: None,
            annotation: None,
            source: None,
        };
        let oc = open_content_from_result(&oc_result, &schema_set, None).unwrap();

        let result = attach_open_content(matcher, Some(oc));
        match result {
            ContentModelMatcher::AllGroup(model) => {
                let oc_wc = model.open_content.expect("open content should be present");
                assert!(
                    oc_wc.not_qnames.contains(&(None, elem_x)),
                    "##definedSibling should exclude sibling element 'x'"
                );
                assert!(
                    oc_wc.not_qnames.contains(&(None, elem_y)),
                    "##definedSibling should exclude sibling element 'y'"
                );
            }
            _ => panic!("expected AllGroup"),
        }
    }

    // ========================================================================
    // Group refs inside xs:all — cos-all-limited flattening tests
    // ========================================================================

    /// Helper: register a named model group in the schema and return its key.
    /// The group is stored in arenas and registered in namespace lookup.
    fn register_model_group(
        schema_set: &mut SchemaSet,
        name: NameId,
        ns: Option<NameId>,
        compositor: Compositor,
        particles: Vec<ParticleResult>,
    ) -> crate::ids::ModelGroupKey {
        let data = ModelGroupData {
            name: Some(name),
            target_namespace: ns,
            ref_name: None,
            compositor: Some(compositor),
            particles,
            min_occurs: 1,
            max_occurs: Some(1),
            id: None,
            annotation: None,
            source: None,
            resolved_ref: None,
            resolved_particles: vec![],
            resolved_particle_types: vec![],
            resolved_particle_elements: vec![],
        };
        let key = schema_set.arenas.alloc_model_group(data);
        schema_set
            .namespaces
            .entry(ns)
            .or_default()
            .model_groups
            .insert(name, key);
        key
    }

    /// Helper: make a group reference particle (xs:group ref="...").
    fn make_group_ref_particle(
        ref_ns: Option<NameId>,
        ref_local: NameId,
        min: u32,
        max: Option<u32>,
    ) -> ParticleResult {
        ParticleResult {
            term: ParticleTerm::Group(ModelGroupDefResult {
                name: None,
                ref_name: Some(QNameRef {
                    prefix: None,
                    local_name: ref_local,
                    namespace: ref_ns,
                }),
                compositor: None,
                particles: vec![],
                min_occurs: 1,
                max_occurs: Some(1),
                id: None,
                annotation: None,
                source: None,
            }),
            min_occurs: min,
            max_occurs: max,
            source: None,
        }
    }

    /// Helper: make an inline all-group particle with a group ref inside it,
    /// suitable for use as a complex type's top-level particle.
    #[cfg(feature = "xsd11")]
    fn make_all_with_group_ref(
        direct_particles: Vec<ParticleResult>,
        group_ref_particles: Vec<ParticleResult>,
    ) -> ParticleResult {
        let mut all_children = direct_particles;
        all_children.extend(group_ref_particles);
        make_all_particle(all_children)
    }

    /// Helper: build a complex type def with an all-group particle and compile it.
    #[cfg(feature = "xsd11")]
    fn compile_all_type(
        schema_set: &SchemaSet,
        all_particle: ParticleResult,
    ) -> NfaCompileResult<ContentModelMatcher> {
        use crate::parser::frames::ComplexContentDefResult;
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
        compile_content_model_matcher(schema_set, &type_def)
    }

    // --- Valid schemas (XSD 1.1) ---

    #[cfg(feature = "xsd11")]
    #[test]
    fn test_group_ref_to_all_inside_all() {
        // Test 1: G = all(a, b), parent all(group-ref-G, c) → flattened to all(a, b, c)
        let mut schema_set = SchemaSet::xsd11();
        let a = schema_set.name_table.add("a");
        let b = schema_set.name_table.add("b");
        let c = schema_set.name_table.add("c");
        let g_name = schema_set.name_table.add("G");

        register_model_group(
            &mut schema_set,
            g_name,
            None,
            Compositor::All,
            vec![
                make_element_particle(a, 1, Some(1)),
                make_element_particle(b, 1, Some(1)),
            ],
        );

        let all_particle = make_all_with_group_ref(
            vec![make_element_particle(c, 1, Some(1))],
            vec![make_group_ref_particle(None, g_name, 1, Some(1))],
        );

        let matcher = compile_all_type(&schema_set, all_particle).unwrap();
        match &matcher {
            ContentModelMatcher::AllGroup(model) => {
                assert_eq!(model.particle_count(), 3, "should flatten to 3 particles: a, b, c");
            }
            other => panic!("expected AllGroup, got {:?}", other),
        }
    }

    #[cfg(feature = "xsd11")]
    #[test]
    fn test_nested_group_refs_in_all() {
        // Test 2: G2 = all(b, c), G1 = all(a, group-ref-G2), parent all(group-ref-G1, d)
        // → flattened to all(a, b, c, d)
        let mut schema_set = SchemaSet::xsd11();
        let a = schema_set.name_table.add("a");
        let b = schema_set.name_table.add("b");
        let c = schema_set.name_table.add("c");
        let d = schema_set.name_table.add("d");
        let g1_name = schema_set.name_table.add("G1");
        let g2_name = schema_set.name_table.add("G2");

        register_model_group(
            &mut schema_set,
            g2_name,
            None,
            Compositor::All,
            vec![
                make_element_particle(b, 1, Some(1)),
                make_element_particle(c, 1, Some(1)),
            ],
        );

        register_model_group(
            &mut schema_set,
            g1_name,
            None,
            Compositor::All,
            vec![
                make_element_particle(a, 1, Some(1)),
                make_group_ref_particle(None, g2_name, 1, Some(1)),
            ],
        );

        let all_particle = make_all_with_group_ref(
            vec![make_element_particle(d, 1, Some(1))],
            vec![make_group_ref_particle(None, g1_name, 1, Some(1))],
        );

        let matcher = compile_all_type(&schema_set, all_particle).unwrap();
        match &matcher {
            ContentModelMatcher::AllGroup(model) => {
                assert_eq!(model.particle_count(), 4, "should flatten to 4 particles: a, b, c, d");
            }
            other => panic!("expected AllGroup, got {:?}", other),
        }
    }

    #[cfg(feature = "xsd11")]
    #[test]
    fn test_group_ref_with_optional_inner_particles() {
        // Test 3: G = all(a[1..1], b[0..1]), parent all(group-ref-G, c)
        let mut schema_set = SchemaSet::xsd11();
        let a = schema_set.name_table.add("a");
        let b = schema_set.name_table.add("b");
        let c = schema_set.name_table.add("c");
        let g_name = schema_set.name_table.add("G");

        register_model_group(
            &mut schema_set,
            g_name,
            None,
            Compositor::All,
            vec![
                make_element_particle(a, 1, Some(1)),
                make_element_particle(b, 0, Some(1)), // optional
            ],
        );

        let all_particle = make_all_with_group_ref(
            vec![make_element_particle(c, 1, Some(1))],
            vec![make_group_ref_particle(None, g_name, 1, Some(1))],
        );

        let matcher = compile_all_type(&schema_set, all_particle).unwrap();
        match &matcher {
            ContentModelMatcher::AllGroup(model) => {
                assert_eq!(model.particle_count(), 3);
                // Check that inner particle b kept its optional nature
                let optional_count = model.particles.iter().filter(|p| p.is_optional()).count();
                assert_eq!(optional_count, 1, "b should remain optional after flattening");
            }
            other => panic!("expected AllGroup, got {:?}", other),
        }
    }

    #[cfg(feature = "xsd11")]
    #[test]
    fn test_group_ref_alongside_wildcard() {
        // Test 4: all(group-ref-G, xs:any)
        let mut schema_set = SchemaSet::xsd11();
        let a = schema_set.name_table.add("a");
        let g_name = schema_set.name_table.add("G");

        register_model_group(
            &mut schema_set,
            g_name,
            None,
            Compositor::All,
            vec![make_element_particle(a, 1, Some(1))],
        );

        let wildcard_particle = ParticleResult {
            term: ParticleTerm::Any(WildcardResult {
                namespace: WildcardNamespace::Any,
                process_contents: ProcessContents::Lax,
                not_namespace: Vec::new(),
                not_qname: Vec::new(),
                id: None,
                annotation: None,
                source: None,
            }),
            min_occurs: 0,
            max_occurs: Some(1),
            source: None,
        };

        let all_particle = make_all_with_group_ref(
            vec![wildcard_particle],
            vec![make_group_ref_particle(None, g_name, 1, Some(1))],
        );

        let matcher = compile_all_type(&schema_set, all_particle).unwrap();
        match &matcher {
            ContentModelMatcher::AllGroup(model) => {
                assert_eq!(model.particle_count(), 2, "wildcard + flattened element a");
                // One should be element, one should be wildcard
                let has_wildcard = model.particles.iter().any(|p| {
                    matches!(p.term, NfaTerm::Wildcard { .. })
                });
                let has_element = model.particles.iter().any(|p| {
                    matches!(p.term, NfaTerm::Element { .. })
                });
                assert!(has_wildcard, "should have wildcard particle");
                assert!(has_element, "should have element particle from group ref");
            }
            other => panic!("expected AllGroup, got {:?}", other),
        }
    }

    #[cfg(feature = "xsd11")]
    #[test]
    fn test_defined_sibling_includes_group_ref_elements() {
        // Test 7: ##definedSibling wildcard excludes elements from group refs
        use NotQNameItem;

        let mut schema_set = SchemaSet::with_version(XsdVersion::V1_1);
        let a = schema_set.name_table.add("a");
        let b = schema_set.name_table.add("b");
        let g_name = schema_set.name_table.add("G");

        register_model_group(
            &mut schema_set,
            g_name,
            None,
            Compositor::All,
            vec![make_element_particle(b, 1, Some(1))],
        );

        let wildcard_particle = ParticleResult {
            term: ParticleTerm::Any(WildcardResult {
                namespace: WildcardNamespace::Any,
                process_contents: ProcessContents::Lax,
                not_namespace: Vec::new(),
                not_qname: vec![NotQNameItem::DefinedSibling],
                id: None,
                annotation: None,
                source: None,
            }),
            min_occurs: 0,
            max_occurs: Some(1),
            source: None,
        };

        let all_particle = make_all_with_group_ref(
            vec![
                make_element_particle(a, 1, Some(1)),
                wildcard_particle,
            ],
            vec![make_group_ref_particle(None, g_name, 1, Some(1))],
        );

        let matcher = compile_all_type(&schema_set, all_particle).unwrap();
        match &matcher {
            ContentModelMatcher::AllGroup(model) => {
                // Find the wildcard particle and verify not_qnames
                let wc = model.particles.iter().find(|p| {
                    matches!(p.term, NfaTerm::Wildcard { .. })
                }).expect("should have wildcard particle");
                if let NfaTerm::Wildcard { not_qnames, .. } = &wc.term {
                    assert!(
                        not_qnames.contains(&(None, a)),
                        "##definedSibling should exclude element 'a'"
                    );
                    assert!(
                        not_qnames.contains(&(None, b)),
                        "##definedSibling should exclude element 'b' from group ref"
                    );
                }
            }
            other => panic!("expected AllGroup, got {:?}", other),
        }
    }

    // --- Invalid schemas (compile errors) ---

    #[cfg(feature = "xsd11")]
    #[test]
    fn test_group_ref_in_all_min_occurs_zero_error() {
        // Test 8: Group ref with minOccurs=0 → cos-all-limited.1.3 error
        let mut schema_set = SchemaSet::xsd11();
        let a = schema_set.name_table.add("a");
        let g_name = schema_set.name_table.add("G");

        register_model_group(
            &mut schema_set,
            g_name,
            None,
            Compositor::All,
            vec![make_element_particle(a, 1, Some(1))],
        );

        let all_particle = make_all_particle(vec![
            make_group_ref_particle(None, g_name, 0, Some(1)), // minOccurs=0 — invalid
        ]);

        let result = compile_all_type(&schema_set, all_particle);
        assert!(
            matches!(result, Err(NfaCompileError::InvalidAllGroupOccurs { .. })),
            "minOccurs=0 should produce InvalidAllGroupOccurs error"
        );
    }

    #[cfg(feature = "xsd11")]
    #[test]
    fn test_group_ref_in_all_max_occurs_two_error() {
        // Test 9: Group ref with maxOccurs=2 → cos-all-limited.1.3 error
        let mut schema_set = SchemaSet::xsd11();
        let a = schema_set.name_table.add("a");
        let g_name = schema_set.name_table.add("G");

        register_model_group(
            &mut schema_set,
            g_name,
            None,
            Compositor::All,
            vec![make_element_particle(a, 1, Some(1))],
        );

        let all_particle = make_all_particle(vec![
            make_group_ref_particle(None, g_name, 1, Some(2)), // maxOccurs=2 — invalid
        ]);

        let result = compile_all_type(&schema_set, all_particle);
        assert!(
            matches!(result, Err(NfaCompileError::InvalidAllGroupOccurs { .. })),
            "maxOccurs=2 should produce InvalidAllGroupOccurs error"
        );
    }

    #[cfg(feature = "xsd11")]
    #[test]
    fn test_group_ref_to_sequence_in_all_error() {
        // Test 10: Group ref to sequence → cos-all-limited.2 error
        let mut schema_set = SchemaSet::xsd11();
        let a = schema_set.name_table.add("a");
        let g_name = schema_set.name_table.add("G");

        register_model_group(
            &mut schema_set,
            g_name,
            None,
            Compositor::Sequence, // not All — invalid
            vec![make_element_particle(a, 1, Some(1))],
        );

        let all_particle = make_all_particle(vec![
            make_group_ref_particle(None, g_name, 1, Some(1)),
        ]);

        let result = compile_all_type(&schema_set, all_particle);
        assert!(
            matches!(result, Err(NfaCompileError::InvalidAllGroupContent { .. })),
            "sequence group ref should produce InvalidAllGroupContent error"
        );
    }

    #[cfg(feature = "xsd11")]
    #[test]
    fn test_group_ref_to_choice_in_all_error() {
        // Test 11: Group ref to choice → cos-all-limited.2 error
        let mut schema_set = SchemaSet::xsd11();
        let a = schema_set.name_table.add("a");
        let g_name = schema_set.name_table.add("G");

        register_model_group(
            &mut schema_set,
            g_name,
            None,
            Compositor::Choice, // not All — invalid
            vec![make_element_particle(a, 1, Some(1))],
        );

        let all_particle = make_all_particle(vec![
            make_group_ref_particle(None, g_name, 1, Some(1)),
        ]);

        let result = compile_all_type(&schema_set, all_particle);
        assert!(
            matches!(result, Err(NfaCompileError::InvalidAllGroupContent { .. })),
            "choice group ref should produce InvalidAllGroupContent error"
        );
    }

    #[test]
    fn test_group_ref_in_all_xsd10_error() {
        // Test 12: XSD 1.0 schema set with group ref in all → InvalidAllGroupContent error
        // SchemaSet::new() creates XSD 1.0 — must reject group refs regardless of
        // whether the crate is built with the xsd11 feature.
        let mut schema_set = SchemaSet::new(); // XSD 1.0
        let a = schema_set.name_table.add("a");
        let g_name = schema_set.name_table.add("G");

        register_model_group(
            &mut schema_set,
            g_name,
            None,
            Compositor::All,
            vec![make_element_particle(a, 1, Some(1))],
        );

        // Directly test compile_all_group_model with a group ref particle
        let particles = vec![make_group_ref_particle(None, g_name, 1, Some(1))];
        let mut ctx = CompileContext::new(&schema_set, None);
        ctx.content_flat_idx = Some(0);

        let result = ctx.compile_all_group_model(&particles, None);
        assert!(
            matches!(result, Err(NfaCompileError::InvalidAllGroupContent { .. })),
            "XSD 1.0 schema should reject group refs in xs:all even in xsd11 build"
        );
    }

    #[cfg(feature = "xsd11")]
    #[test]
    fn test_inline_group_in_all_error() {
        // Test 13: Inline group (no ref_name) inside xs:all → InvalidAllGroupContent error
        let schema_set = SchemaSet::xsd11();

        // Create an inline group particle (compositor=All but no ref_name)
        let inline_group = ParticleResult {
            term: ParticleTerm::Group(ModelGroupDefResult {
                name: None,
                ref_name: None, // inline, not a reference
                compositor: Some(Compositor::All),
                particles: vec![make_element_particle(NameId(1), 1, Some(1))],
                min_occurs: 1,
                max_occurs: Some(1),
                id: None,
                annotation: None,
                source: None,
            }),
            min_occurs: 1,
            max_occurs: Some(1),
            source: None,
        };

        let particles = vec![inline_group];
        let mut ctx = CompileContext::new(&schema_set, None);
        ctx.content_flat_idx = Some(0);
        let result = ctx.compile_all_group_model(&particles, None);
        assert!(
            matches!(result, Err(NfaCompileError::InvalidAllGroupContent { .. })),
            "inline group (no ref_name) should be rejected"
        );
    }
}
