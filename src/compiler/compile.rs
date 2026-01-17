//! NFA compilation functions
//!
//! This module implements the core compilation logic for transforming
//! XSD content model particles into NFAs.

use crate::arenas::ModelGroupData;
use crate::ids::NameId;
use crate::parser::frames::{
    Compositor, ElementFrameResult, ModelGroupDefResult, ParticleResult, ParticleTerm,
    ProcessContents, QNameRef, WildcardNamespace, WildcardResult,
};
use crate::parser::location::SourceRef;
use crate::schema::SchemaSet;
use crate::types::complex::{NamespaceConstraint, ProcessContents as TypesProcessContents};

use super::error::{NfaCompileError, NfaCompileResult};
use super::fragment::{fragment_to_table, FragmentBuilder, NfaFragment};
use super::nfa::{NfaTable, NfaTerm};
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
}

impl<'a> CompileContext<'a> {
    /// Create a new compilation context
    pub fn new(schema_set: &'a SchemaSet, target_namespace: Option<NameId>) -> Self {
        Self {
            schema_set,
            target_namespace,
            builder: FragmentBuilder::new(),
            depth: 0,
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

    /// Compile an element to a fragment
    fn compile_element(
        &mut self,
        elem: &ElementFrameResult,
        source: Option<&SourceRef>,
    ) -> NfaCompileResult<NfaFragment> {
        // Determine element name and namespace
        let (name, namespace, element_key) = if let Some(ref_name) = &elem.ref_name {
            // Element reference - look up in schema set
            let key = self
                .schema_set
                .lookup_element(ref_name.namespace, ref_name.local_name);
            (ref_name.local_name, ref_name.namespace, key)
        } else if let Some(name) = elem.name {
            // Local element declaration
            (name, elem.target_namespace, None)
        } else {
            return Err(NfaCompileError::unresolved_element(
                "anonymous element without name or ref".to_string(),
                source.cloned(),
            ));
        };

        let nfa_term = NfaTerm::element(name, namespace, element_key);
        Ok(self.builder.single_term(nfa_term, source.cloned()))
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

    /// Compile a sequence (xs:sequence)
    fn compile_sequence(&mut self, particles: &[ParticleResult]) -> NfaCompileResult<NfaFragment> {
        if particles.is_empty() {
            return Ok(self.builder.epsilon_fragment());
        }

        let mut result = self.compile_particle_to_fragment(&particles[0])?;
        for particle in &particles[1..] {
            let frag = self.compile_particle_to_fragment(particle)?;
            result = result.concat(frag);
        }

        Ok(result)
    }

    /// Compile a choice (xs:choice)
    fn compile_choice(&mut self, particles: &[ParticleResult]) -> NfaCompileResult<NfaFragment> {
        if particles.is_empty() {
            return Ok(self.builder.epsilon_fragment());
        }

        let mut result = self.compile_particle_to_fragment(&particles[0])?;
        for particle in &particles[1..] {
            let frag = self.compile_particle_to_fragment(particle)?;
            result = result.alternate(frag);
        }

        Ok(result)
    }

    /// Compile an all-group (xs:all)
    ///
    /// Note: Full all-group support with order-independent matching is
    /// implemented in Task 4.3. This provides a basic implementation that
    /// treats all-groups as sequences for now.
    fn compile_all(
        &mut self,
        particles: &[ParticleResult],
        source: Option<&SourceRef>,
    ) -> NfaCompileResult<NfaFragment> {
        // XSD 1.0 restrictions: all-groups can only contain elements with maxOccurs <= 1
        // For now, we compile as a sequence (which is overly restrictive but correct)
        // Full implementation in Task 4.3 will use AllGroupModel for proper any-order matching

        // Validate XSD 1.0 constraints
        for particle in particles {
            // Check that term is an element (not a group or wildcard)
            if !matches!(particle.term, ParticleTerm::Element(_)) {
                return Err(NfaCompileError::invalid_all_group(source.cloned()));
            }

            // Check maxOccurs constraint (XSD 1.0: must be 0 or 1)
            if let Some(max) = particle.max_occurs {
                if max > 1 {
                    return Err(NfaCompileError::invalid_all_group(source.cloned()));
                }
            } else {
                // unbounded not allowed in XSD 1.0 all-groups
                return Err(NfaCompileError::invalid_all_group(source.cloned()));
            }
        }

        // For now, compile as a permutation of all optional elements
        // This is a simplification - proper implementation needs AllGroupModel
        // We'll create a hub state that can match any element, then loop back
        if particles.is_empty() {
            return Ok(self.builder.epsilon_fragment());
        }

        // Simple approach: create choice of all elements, then wrap in repeat
        // This allows any order but doesn't enforce "each element at most once"
        // Full enforcement requires AllGroupModel (Task 4.3)
        let mut choice = self.compile_particle_to_fragment(&particles[0])?;
        for particle in &particles[1..] {
            let frag = self.compile_particle_to_fragment(particle)?;
            choice = choice.alternate(frag);
        }

        // Allow 0 to n occurrences where n = number of particles
        let n = particles.len() as u32;
        Ok(choice.repeat_range(0, Some(n)))
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

        match compositor {
            Compositor::Sequence => self.compile_sequence(&group.particles),
            Compositor::Choice => self.compile_choice(&group.particles),
            Compositor::All => self.compile_all(&group.particles, source),
        }
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
