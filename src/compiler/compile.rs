//! NFA compilation functions
//!
//! This module implements the core compilation logic for transforming
//! XSD content model particles into NFAs.

use crate::arenas::{ComplexTypeDefData, ModelGroupData};
use crate::ids::ModelGroupKey;
use crate::parser::frames::DerivationMethod;
use crate::ids::{ElementKey, NameId, TypeKey};
use crate::parser::frames::{
    Compositor, ComplexContentResult, ElementFrameResult, ModelGroupDefResult, NamespaceToken,
    NotQNameItem, OpenContentResult, ParticleResult, ParticleTerm,
    ProcessContents, QNameRef, TypeRefResult, WildcardNamespace, WildcardResult,
};
use crate::parser::location::SourceRef;
use crate::schema::model::{DefaultOpenContent, XsdVersion};
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
    /// Fragment builder for constructing NFA fragments
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
    /// When compiling a redefining group, stores (group_name, group_ns, original_key)
    /// so self-referencing QName refs are redirected to the original group.
    redefine_redirect: Option<(NameId, Option<NameId>, ModelGroupKey)>,
    /// When true, occurrence bounds are capped for UPA checking.
    /// Produces counter-free NFAs suitable for epsilon-closure-based UPA analysis.
    upa_mode: bool,
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
            redefine_redirect: None,
            upa_mode: false,
        }
    }

    /// Create a compilation context with UPA occurrence-bound capping enabled.
    pub fn new_for_upa(schema_set: &'a SchemaSet, target_namespace: Option<NameId>) -> Self {
        Self {
            upa_mode: true,
            ..Self::new(schema_set, target_namespace)
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
            // An unresolved ref with an explicit namespace is an error
            // (§4.2.4: imports are not transitive). No-namespace refs
            // stay lenient — chameleon-adopted target components leave
            // such refs pointing at empty-namespace names that no
            // longer exist, but the owning complex type is replaced by
            // an override before any instance reaches it.
            let key = self
                .schema_set
                .lookup_element(ref_name.namespace, ref_name.local_name);
            if key.is_none() && ref_name.namespace.is_some() {
                let name_str = crate::schema::resolver::format_resolved_qname(
                    &self.schema_set.name_table,
                    ref_name.namespace,
                    ref_name.local_name,
                );
                return Err(NfaCompileError::unresolved_element(
                    name_str,
                    elem.source.clone().or_else(|| source.cloned()),
                ));
            }
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
                    if !self.schema_set.is_xsd11() {
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

        // Resolve the group ref (redefine-aware)
        let group_key = self
            .resolve_model_group_key(ref_name)
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
        // Check recursion depth to detect circular group references
        self.check_recursion(source)?;
        self.depth += 1;

        // Look up the referenced group (redefine-aware)
        let group_key = self
            .resolve_model_group_key(ref_name)
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
        let result = self.compile_model_group_data(group_data, source);
        self.depth -= 1;
        result
    }

    /// Resolve a model group reference QName to a key, redirecting self-references
    /// in redefining groups to the original group to avoid infinite recursion.
    fn resolve_model_group_key(&self, ref_name: &QNameRef) -> Option<ModelGroupKey> {
        if let Some((name, ns, original_key)) = self.redefine_redirect {
            if ref_name.local_name == name && ref_name.namespace == ns {
                return Some(original_key);
            }
        }
        self.schema_set
            .lookup_model_group(ref_name.namespace, ref_name.local_name)
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

        // Set up redefine redirect if this is a redefining group
        let saved_redirect = self.redefine_redirect.take();
        if let (Some(original_key), Some(name)) = (group.redefine_original, group.name) {
            self.redefine_redirect = Some((name, group.target_namespace, original_key));
        }

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
        self.redefine_redirect = saved_redirect;
        self.content_flat_idx = saved_flat_idx;
        self.resolved_particle_elements = saved_particle_elements;
        self.resolved_particle_types = saved_types;
        self.current_particle_idx = saved_idx;
        result
    }

    /// Apply occurrence constraints to a fragment
    ///
    /// Small maxOccurs are unrolled; large values use counted NFA transitions;
    /// very large values (> MAX_COUNTED_OCCURS) fall back to unbounded.
    ///
    /// When `upa_mode` is true, bounds are capped to small values first,
    /// producing a counter-free NFA suitable for UPA analysis.
    fn apply_occurrences(
        &mut self,
        fragment: NfaFragment,
        min: u32,
        max: Option<u32>,
    ) -> NfaFragment {
        let (eff_min, eff_max) = if self.upa_mode {
            cap_for_upa(min, max)
        } else {
            (min, max)
        };
        let max_occurs = MaxOccurs::from_option(eff_max);
        apply_occurs(fragment, eff_min, max_occurs)
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
                        // §3.10.6.1 ##definedSibling also excludes substitution-
                        // group members of any sibling element declaration.
                        // Local elements cannot be substitution heads (members
                        // must be globally declared per §3.3.3), so this only
                        // applies to refs.
                        if let Some(head_key) = self
                            .schema_set
                            .lookup_element(ref_name.namespace, ref_name.local_name)
                        {
                            collect_substitution_members(
                                self.schema_set,
                                head_key,
                                &mut result,
                            );
                        }
                    } else if let Some(name) = elem.name {
                        // Local element — resolve namespace through form/elementFormDefault
                        let source = p.source.as_ref().or(elem.source.as_ref());
                        let ns = self.effective_element_namespace(elem, source);
                        result.push((ns, name));
                    }
                }
                ParticleTerm::Group(group) => {
                    if let Some(ref_name) = &group.ref_name {
                        if let Some(key) = self.resolve_model_group_key(ref_name) {
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

/// Push QNames of every element that may substitute for `head_key` into `out`,
/// honoring the head's `block`/`final` restrictions and walking transitively
/// through nested substitution chains. Used for `##definedSibling` expansion.
fn collect_substitution_members(
    schema_set: &SchemaSet,
    head_key: ElementKey,
    out: &mut Vec<(Option<NameId>, NameId)>,
) {
    // Resolve through ref to get the canonical head decl.
    let head_key = schema_set
        .arenas
        .elements
        .get(head_key)
        .and_then(|e| e.resolved_ref)
        .unwrap_or(head_key);
    let mut visited = std::collections::HashSet::new();
    let mut stack = vec![head_key];
    while let Some(current) = stack.pop() {
        if !visited.insert(current) {
            continue;
        }
        for (member_key, member) in schema_set.arenas.elements.iter() {
            if member_key == current {
                continue;
            }
            if member.resolved_substitution_groups.contains(&current) {
                if let Some(name) = member.name {
                    let entry = (member.target_namespace, name);
                    if !out.contains(&entry) {
                        out.push(entry);
                    }
                }
                stack.push(member_key);
            }
        }
    }
}

/// Cap occurrence bounds for UPA checking.
///
/// UPA ambiguity is structural: it arises at iteration boundaries.
/// `maxOccurs=2` is sufficient to expose any boundary ambiguity.
/// This follows the approach used by Xerces-J, Saxon, and .NET.
///
/// See Sperberg-McQueen (2005): for determinism testing, `F{n,m}` can be
/// replaced with `F{min(n,1), min(m,2)}` without affecting the result.
#[cfg_attr(test, allow(dead_code))]
pub(super) fn cap_for_upa(min: u32, max: Option<u32>) -> (u32, Option<u32>) {
    match (min, max) {
        // Dead particle: maxOccurs=0 means the particle is absent
        (0, Some(0)) => (0, Some(0)),
        // Already simple: no capping needed
        (m, Some(1)) if m <= 1 => (m, Some(1)),
        // Exact repeat (min == max > 1): cap to {2, 2}
        (m, Some(mx)) if m == mx && m > 1 => (2, Some(2)),
        // Optional with repetition: preserve optionality + iteration boundary
        (0, _) => (0, Some(2)),
        // Required with repetition: cap to {1, 2}
        (_, _) => (min.min(1), Some(2)),
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
pub(crate) fn is_top_level_all_group(particle: &ParticleResult) -> Option<(&[ParticleResult], Option<&SourceRef>)> {
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
pub(crate) fn resolve_top_level_all_group_ref<'a>(
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

/// Validate the outer occurrence constraints on a particle whose term is an all-group.
///
/// XSD 1.0 (cos-all-limited.2): minOccurs must be 0 or 1, maxOccurs must be 1.
/// XSD 1.1: more relaxed, but minOccurs > maxOccurs is always invalid.
pub(crate) fn validate_outer_all_group_occurs(
    particle: &ParticleResult,
    xsd_version: XsdVersion,
) -> NfaCompileResult<()> {
    let min = particle.min_occurs;
    let max = particle.max_occurs; // Option<u32>, None means unbounded

    // XSD 1.0 specific constraints (check first for more specific error messages)
    if xsd_version == XsdVersion::V1_0 {
        if min > 1 {
            return Err(NfaCompileError::InvalidAllGroupOccurs {
                reason: format!(
                    "cos-all-limited.2: minOccurs must be 0 or 1 for xs:all group, found {}",
                    min
                ),
                location: particle.source.clone(),
            });
        }
        match max {
            Some(1) => {} // OK
            Some(n) => {
                return Err(NfaCompileError::InvalidAllGroupOccurs {
                    reason: format!(
                        "cos-all-limited.2: maxOccurs must be 1 for xs:all group, found {}",
                        n
                    ),
                    location: particle.source.clone(),
                });
            }
            None => {
                return Err(NfaCompileError::InvalidAllGroupOccurs {
                    reason: "cos-all-limited.2: maxOccurs='unbounded' not allowed for xs:all group"
                        .to_string(),
                    location: particle.source.clone(),
                });
            }
        }
    }

    // Universal: minOccurs > maxOccurs is always invalid
    if let Some(max_val) = max {
        if min > max_val {
            return Err(NfaCompileError::InvalidOccurrence {
                min,
                max: max_val,
                location: particle.source.clone(),
            });
        }
    }

    Ok(())
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
    let builder = FragmentBuilder::new();
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
    compile_content_model_matcher_impl(schema_set, type_def, false)
}

/// Compile a content model with capped occurrence bounds for UPA checking.
///
/// All `maxOccurs` values are reduced to at most 2 before NFA construction,
/// producing a counter-free NFA suitable for epsilon-closure-based UPA analysis.
/// This is the standard approach used by Xerces-J, Saxon, and .NET.
pub fn compile_content_model_for_upa(
    schema_set: &SchemaSet,
    type_def: &ComplexTypeDefData,
) -> NfaCompileResult<ContentModelMatcher> {
    compile_content_model_matcher_impl(schema_set, type_def, true)
}

fn compile_content_model_matcher_impl(
    schema_set: &SchemaSet,
    type_def: &ComplexTypeDefData,
    upa_mode: bool,
) -> NfaCompileResult<ContentModelMatcher> {
    let target_namespace = type_def.target_namespace;
    let mut ctx = if upa_mode {
        CompileContext::new_for_upa(schema_set, target_namespace)
    } else {
        CompileContext::new(schema_set, target_namespace)
    };
    let is_extension = matches!(type_def.derivation_method, Some(DerivationMethod::Extension));

    // Try the all-group path for non-extension types with an inline xs:all
    if !is_extension {
        if let ComplexContentResult::Complex(def) = &type_def.content {
            if let Some(particle) = &def.particle {
                if let Some((all_particles, all_source)) = is_top_level_all_group(particle) {
                    validate_outer_all_group_occurs(particle, schema_set.xsd_version)?;
                    ctx.resolved_particle_types =
                        type_def.resolved_content_particle_types.to_vec();
                    ctx.resolved_particle_elements =
                        type_def.resolved_content_particle_elements.to_vec();
                    ctx.content_flat_idx = Some(0);
                    let mut model = ctx.compile_all_group_model(all_particles, all_source)?;
                    if particle.min_occurs == 0 {
                        model.outer_optional = true;
                    }
                    let base_matcher = ContentModelMatcher::AllGroup(model);

                    let open_content = resolve_open_content(
                        schema_set,
                        &type_def.content,
                        type_def.open_content.as_ref(),
                        type_def.source.as_ref(),
                    );

                    return Ok(attach_open_content(schema_set, base_matcher, open_content));
                }

                // Named group ref resolving to all-group
                if let Some(group_data) = resolve_top_level_all_group_ref(particle, schema_set) {
                    validate_outer_all_group_occurs(particle, schema_set.xsd_version)?;
                    ctx.resolved_particle_types = group_data.resolved_particle_types.clone();
                    ctx.resolved_particle_elements = group_data.resolved_particle_elements.clone();
                    ctx.content_flat_idx = Some(0);
                    // Set up redefine redirect so self-references inside the
                    // all-group resolve to the original group, not back to
                    // the redefining group.
                    if let (Some(original_key), Some(name)) =
                        (group_data.redefine_original, group_data.name)
                    {
                        ctx.redefine_redirect =
                            Some((name, group_data.target_namespace, original_key));
                    }
                    let mut model = ctx.compile_all_group_model(
                        &group_data.particles,
                        group_data.source.as_ref(),
                    )?;
                    if particle.min_occurs == 0 {
                        model.outer_optional = true;
                    }
                    let base_matcher = ContentModelMatcher::AllGroup(model);

                    let open_content = resolve_open_content(
                        schema_set,
                        &type_def.content,
                        type_def.open_content.as_ref(),
                        type_def.source.as_ref(),
                    );

                    return Ok(attach_open_content(schema_set, base_matcher, open_content));
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
                    return Ok(attach_open_content(schema_set, matcher, open_content));
                }
                Some(particle) => {
                    // Check if extension's own particle is an inline all-group
                    if let Some((ext_particles, ext_source)) = is_top_level_all_group(particle) {
                        // §3.4.2.3 / cos-ct-extends: when both base and extension
                        // are all-groups, their outer {min occurs} must match.
                        let base_outer_optional = base_all_model.outer_optional;
                        let ext_outer_optional = particle.min_occurs == 0;
                        if base_outer_optional != ext_outer_optional {
                            return Err(NfaCompileError::InvalidAllGroupOccurs {
                                reason: format!(
                                    "cos-ct-extends: when extending an xs:all base with an xs:all, \
                                     the outer minOccurs must match (base minOccurs={}, \
                                     extension minOccurs={})",
                                    if base_outer_optional { 0 } else { 1 },
                                    particle.min_occurs,
                                ),
                                location: particle.source.clone().or_else(|| ext_source.cloned()),
                            });
                        }

                        // Reject extending an empty xs:all — there is no
                        // base content to extend and the resulting type would
                        // collapse to the extension's own all-group, which is
                        // not a true extension. (W3C bug 6202; Saxon allows
                        // this but conformance tests treat it as invalid.)
                        if base_all_model.particles.is_empty() {
                            return Err(NfaCompileError::InvalidAllGroupContent {
                                location: particle.source.clone().or_else(|| ext_source.cloned()),
                            });
                        }

                        let mut ctx = if upa_mode {
                            CompileContext::new_for_upa(schema_set, type_def.target_namespace)
                        } else {
                            CompileContext::new(schema_set, type_def.target_namespace)
                        };
                        ctx.resolved_particle_types =
                            type_def.resolved_content_particle_types.to_vec();
                        ctx.resolved_particle_elements =
                            type_def.resolved_content_particle_elements.to_vec();
                        ctx.content_flat_idx = Some(0);
                        let ext_model = ctx.compile_all_group_model(ext_particles, ext_source)?;

                        let merged_outer_optional = base_outer_optional && ext_outer_optional;
                        let mut merged_particles = base_all_model.particles;
                        merged_particles.extend(ext_model.particles);
                        let mut merged = AllGroupModel::new(merged_particles);
                        merged.outer_optional = merged_outer_optional;
                        let matcher = ContentModelMatcher::AllGroup(merged);
                        return Ok(attach_open_content(schema_set, matcher, open_content));
                    }

                    // Extension is sequence/choice/group-ref — invalid per
                    // cos-all-limited.1.2: an xs:all may only appear at the top
                    // of a content model. Wrapping the base's all-group inside
                    // a sequence(base, extension) violates that constraint.
                    return Err(NfaCompileError::InvalidAllGroupContent {
                        location: particle.source.clone(),
                    });
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

    // For extensions, prepend the base type's content model. Capture base's effective
    // OC for §3.4.2.3 clause 6 inheritance/union on XSD 1.1.
    #[cfg(feature = "xsd11")]
    let mut inherited_oc: Option<OpenContent> = None;
    #[cfg(feature = "xsd11")]
    let mut base_target_ns: Option<NameId> = None;

    let base_nfa = if is_extension {
        if let Some(TypeKey::Complex(base_ct_key)) = type_def.resolved_base_type {
            let base_type_def = &schema_set.arenas.complex_types[base_ct_key];
            #[cfg(feature = "xsd11")]
            { base_target_ns = base_type_def.target_namespace; }
            let base_matcher = compile_content_model_matcher_impl(schema_set, base_type_def, upa_mode)?;
            match base_matcher {
                ContentModelMatcher::Nfa(nfa) => Some(nfa),
                ContentModelMatcher::WithOpenContent { nfa, mode, wildcard } => {
                    #[cfg(feature = "xsd11")]
                    { inherited_oc = Some(OpenContent { mode, wildcard, source: None }); }
                    #[cfg(not(feature = "xsd11"))]
                    let _ = (mode, wildcard);
                    Some(nfa)
                }
                ContentModelMatcher::AllGroup(ref model) => {
                    if own_nfa.is_none() {
                        // Extension adds only attributes — base AllGroup already carries its OC;
                        // attach_open_content(AllGroup, None) preserves it.
                        let open_content = resolve_open_content(
                            schema_set,
                            &type_def.content,
                            type_def.open_content.as_ref(),
                            type_def.source.as_ref(),
                        );
                        return Ok(attach_open_content(schema_set, base_matcher, open_content));
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

    // §3.4.2.3 clause 6 (inherit + union) for XSD 1.1 extensions; simple resolve otherwise.
    #[cfg(feature = "xsd11")]
    let open_content = if schema_set.is_xsd11() && is_extension {
        effective_open_content_for_extension(
            schema_set,
            type_def,
            base_target_ns,
            inherited_oc.as_ref(),
        )
    } else {
        resolve_open_content(
            schema_set,
            &type_def.content,
            type_def.open_content.as_ref(),
            type_def.source.as_ref(),
        )
    };
    #[cfg(not(feature = "xsd11"))]
    let open_content = resolve_open_content(
        schema_set,
        &type_def.content,
        type_def.open_content.as_ref(),
        type_def.source.as_ref(),
    );

    Ok(attach_open_content(schema_set, base_matcher, open_content))
}

/// §3.4.2.3 clauses 5–6: effective open content for an XSD 1.1 extension type.
#[cfg(feature = "xsd11")]
fn effective_open_content_for_extension(
    schema_set: &SchemaSet,
    type_def: &ComplexTypeDefData,
    base_target_ns: Option<NameId>,
    inherited: Option<&OpenContent>,
) -> Option<OpenContent> {
    let own_oc = resolve_open_content(
        schema_set,
        &type_def.content,
        type_def.open_content.as_ref(),
        type_def.source.as_ref(),
    );
    match own_oc {
        // Clause 6.1: own OC absent or mode="none" → inherit base OC.
        None => inherited.cloned(),
        // Clause 6.2: union wildcards with base OC.
        Some(own) => {
            let Some(base_oc) = inherited else {
                return Some(own);
            };
            let derived_target_ns = type_def.target_namespace;
            let unioned_wildcard = match (own.wildcard.as_ref(), base_oc.wildcard.as_ref()) {
                (Some(own_wc), Some(base_wc)) => {
                    Some(wildcard_ref_union(base_wc, base_target_ns, own_wc, derived_target_ns))
                }
                (Some(own_wc), None) => Some(own_wc.clone()),
                (None, Some(base_wc)) => Some(base_wc.clone()),
                (None, None) => None,
            };
            Some(OpenContent { mode: own.mode, wildcard: unioned_wildcard, source: own.source })
        }
    }
}

/// §3.10.6.3 cos-aw-union on `WildcardRef`.
#[cfg(feature = "xsd11")]
fn wildcard_ref_union(
    base: &WildcardRef,
    base_target_ns: Option<NameId>,
    derived: &WildcardRef,
    derived_target_ns: Option<NameId>,
) -> WildcardRef {
    let c1 = expand_ns_constraint(&base.namespace_constraint, base_target_ns);
    let c2 = expand_ns_constraint(&derived.namespace_constraint, derived_target_ns);
    let union_ns = namespace_constraint_union(c1, c2);

    let process_contents =
        less_restrictive_process_contents(base.process_contents, derived.process_contents);

    // notQName union: exclusion requires both sides to exclude.
    let not_qnames: Vec<_> = base
        .not_qnames
        .iter()
        .filter(|q| derived.not_qnames.contains(q))
        .cloned()
        .collect();

    WildcardRef {
        namespace_constraint: union_ns,
        process_contents,
        not_qnames,
        has_defined_sibling: false,
        source: derived.source.clone(),
    }
}

/// Expand token-form namespace constraints (Other/TargetNamespace/Local) to explicit sets.
#[cfg(feature = "xsd11")]
fn expand_ns_constraint(nc: &NamespaceConstraint, target_ns: Option<NameId>) -> NamespaceConstraint {
    match nc {
        NamespaceConstraint::Other => NamespaceConstraint::Not(vec![target_ns, None]),
        NamespaceConstraint::TargetNamespace => NamespaceConstraint::List(vec![target_ns]),
        NamespaceConstraint::Local => NamespaceConstraint::List(vec![None]),
        other => other.clone(),
    }
}

/// §3.10.6.3 set union. Callers must pre-expand token forms via `expand_ns_constraint`.
#[cfg(feature = "xsd11")]
fn namespace_constraint_union(c1: NamespaceConstraint, c2: NamespaceConstraint) -> NamespaceConstraint {
    match (c1, c2) {
        // Any ∪ X = Any
        (NamespaceConstraint::Any, _) | (_, NamespaceConstraint::Any) => NamespaceConstraint::Any,
        // Not(E1) ∪ Not(E2) = Not(E1 ∩ E2)
        (NamespaceConstraint::Not(e1), NamespaceConstraint::Not(e2)) => {
            let intersection: Vec<_> = e1.iter().filter(|x| e2.contains(x)).cloned().collect();
            if intersection.is_empty() {
                NamespaceConstraint::Any
            } else {
                NamespaceConstraint::Not(intersection)
            }
        }
        // Not(E) ∪ Pos(S) = Not(E \ S)  [and symmetric]
        (NamespaceConstraint::Not(e), NamespaceConstraint::List(s))
        | (NamespaceConstraint::List(s), NamespaceConstraint::Not(e)) => {
            let diff: Vec<_> = e.into_iter().filter(|x| !s.contains(x)).collect();
            if diff.is_empty() {
                NamespaceConstraint::Any
            } else {
                NamespaceConstraint::Not(diff)
            }
        }
        // Pos(S1) ∪ Pos(S2) = Pos(S1 ∪ S2)
        (NamespaceConstraint::List(mut a), NamespaceConstraint::List(b)) => {
            for x in b {
                if !a.contains(&x) {
                    a.push(x);
                }
            }
            NamespaceConstraint::List(a)
        }
        // Token forms should be pre-expanded; widen to Any defensively.
        (NamespaceConstraint::Other | NamespaceConstraint::TargetNamespace | NamespaceConstraint::Local, _)
        | (_, NamespaceConstraint::Other | NamespaceConstraint::TargetNamespace | NamespaceConstraint::Local) => {
            NamespaceConstraint::Any
        }
    }
}

/// Return the less-restrictive of two processContents values (skip > lax > strict).
#[cfg(feature = "xsd11")]
fn less_restrictive_process_contents(
    a: TypesProcessContents,
    b: TypesProcessContents,
) -> TypesProcessContents {
    match (a, b) {
        (TypesProcessContents::Skip, _) | (_, TypesProcessContents::Skip) => TypesProcessContents::Skip,
        (TypesProcessContents::Lax, _) | (_, TypesProcessContents::Lax) => TypesProcessContents::Lax,
        _ => TypesProcessContents::Strict,
    }
}

fn empty_nfa() -> NfaTable {
    let builder = FragmentBuilder::new();
    fragment_to_table(builder.epsilon_fragment())
}

fn attach_open_content(
    schema_set: &SchemaSet,
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
                    w.not_qnames.extend(collect_nfa_element_qnames(schema_set, &nfa));
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
                    wildcard_ref.not_qnames.extend(collect_all_group_element_qnames(schema_set, &model));
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
                    wildcard_ref.not_qnames.extend(collect_all_group_element_qnames(schema_set, &base_model));
                    wildcard_ref.not_qnames.extend(collect_nfa_element_qnames(schema_set, &extension_nfa));
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
    if !schema_set.is_xsd11() {
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

    // Use defaults_doc() so components that were moved into an xs:override
    // read the overridden schema document's <xs:defaultOpenContent> — per
    // §4.2.5 and the saxon open043 test ("For types defined within xs:override,
    // the relevant defaultOpenContent is the one in the overridden schema
    // document").  For non-override components defaults_doc() == doc_id so
    // normal parsing is unchanged.
    let doc = source.and_then(|s| schema_set.documents.get(s.defaults_doc() as usize));
    let default = doc.and_then(|d| d.default_open_content.as_ref())?;

    if !default.applies_to_empty && content.is_empty() {
        return None;
    }

    open_content_from_default(default, schema_set)
}

fn open_content_from_result(
    result: &OpenContentResult,
    schema_set: &SchemaSet,
    target_namespace: Option<NameId>,
) -> Option<OpenContent> {
    let mode: TypesOpenContentMode = result.mode.into();
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
    let mode: TypesOpenContentMode = default.mode.into();
    if matches!(mode, TypesOpenContentMode::None) {
        return None;
    }

    Some(OpenContent {
        mode,
        wildcard: default.wildcard.as_ref().map(|w| wildcard_ref_from_default(w, schema_set)),
        source: default.source.clone(),
    })
}

/// Expand all globally declared element QNames from the schema set.
fn expand_defined_element_qnames(schema_set: &SchemaSet) -> Vec<(Option<NameId>, NameId)> {
    schema_set.namespaces.iter()
        .flat_map(|(ns, table)| {
            table.elements.keys().map(move |name| (*ns, *name))
        })
        .collect()
}

/// Collect all element QNames from an NFA content model (for ##definedSibling
/// expansion). Includes substitution-group members of each declared element.
fn collect_nfa_element_qnames(
    schema_set: &SchemaSet,
    nfa: &NfaTable,
) -> Vec<(Option<NameId>, NameId)> {
    let mut result = Vec::new();
    for state in &nfa.states {
        if let Some(NfaTerm::Element { namespace, name, element_key, .. }) = &state.term {
            let qname = (*namespace, *name);
            if !result.contains(&qname) {
                result.push(qname);
            }
            if let Some(head_key) = element_key {
                collect_substitution_members(schema_set, *head_key, &mut result);
            }
        }
    }
    result
}

/// Collect all element QNames from an all-group model (for ##definedSibling
/// expansion). Includes substitution-group members of each declared element.
fn collect_all_group_element_qnames(
    schema_set: &SchemaSet,
    model: &AllGroupModel,
) -> Vec<(Option<NameId>, NameId)> {
    let mut result = Vec::new();
    for particle in &model.particles {
        if let NfaTerm::Element { namespace, name, element_key, .. } = &particle.term {
            let qname = (*namespace, *name);
            if !result.contains(&qname) {
                result.push(qname);
            }
            if let Some(head_key) = element_key {
                collect_substitution_members(schema_set, *head_key, &mut result);
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

    WildcardRef {
        namespace_constraint,
        process_contents: wildcard.process_contents,
        not_qnames,
        has_defined_sibling,
        source: wildcard.source.clone(),
    }
}

#[cfg(test)]
#[path = "compile_tests.rs"]
mod tests;
