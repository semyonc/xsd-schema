//! §3.8 / §3.9.6 particle normalization: the `NormalizedParticle` model
//! and the transformations (pointless-particle removal, same-compositor
//! flattening, single-child-group collapsing) applied before a particle
//! restriction check.

use crate::error::{SchemaError, SchemaResult};
use crate::ids::{ElementKey, NameId, TypeKey};
use crate::parser::frames::{
    ComplexContentResult, Compositor, DerivationMethod, ElementFrameResult, ModelGroupDefResult,
    ParticleResult, ParticleTerm, WildcardResult,
};
use crate::parser::location::SourceRef;
use crate::schema::model::DerivationSet;
use crate::schema::SchemaSet;

#[derive(Debug, Clone)]
pub(super) struct NormalizedParticle {
    pub(super) term: NormalizedParticleTerm,
    pub(super) min_occurs: u32,
    pub(super) max_occurs: Option<u32>,
    pub(super) source: Option<SourceRef>,
    /// Compositor of the single-child group that `collapse_single_child_groups`
    /// folded away to produce this particle, if any.  §3.9.6's rules are
    /// structural, so the restriction check has to be able to put that group
    /// back; see the re-expansion step in `particle_restricts`.
    pub(super) collapsed_from: Option<Compositor>,
}

#[derive(Debug, Clone)]
pub(super) enum NormalizedParticleTerm {
    Element(NormalizedElement),
    Wildcard(Box<NormalizedWildcard>),
    Group(NormalizedGroup),
}

#[derive(Debug, Clone)]
pub(super) struct NormalizedElement {
    pub(super) name: NameId,
    pub(super) namespace: Option<NameId>,
    pub(super) type_key: TypeKey,
    pub(super) element_key: Option<ElementKey>,
    pub(super) block: DerivationSet,
    pub(super) nillable: bool,
    pub(super) fixed_value: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) struct NormalizedWildcard {
    pub(super) wildcard: WildcardResult,
    pub(super) target_namespace: Option<NameId>,
}

#[derive(Debug, Clone)]
pub(super) struct NormalizedGroup {
    pub(super) compositor: Compositor,
    pub(super) particles: Vec<NormalizedParticle>,
}

struct ParticleNormalizer<'a> {
    schema_set: &'a SchemaSet,
    target_namespace: Option<NameId>,
    resolved_types: &'a [Option<TypeKey>],
    flat_index: usize,
    depth: usize,
}

const MAX_PARTICLE_RESTRICTION_DEPTH: usize = 100;

impl<'a> ParticleNormalizer<'a> {
    fn new(
        schema_set: &'a SchemaSet,
        target_namespace: Option<NameId>,
        resolved_types: &'a [Option<TypeKey>],
    ) -> Self {
        Self {
            schema_set,
            target_namespace,
            resolved_types,
            flat_index: 0,
            depth: 0,
        }
    }

    fn normalize_particle(
        &mut self,
        particle: &ParticleResult,
    ) -> SchemaResult<NormalizedParticle> {
        if self.depth >= MAX_PARTICLE_RESTRICTION_DEPTH {
            return Err(SchemaError::internal(
                "particle restriction normalization exceeded recursion limit",
            ));
        }

        self.depth += 1;
        let term = match &particle.term {
            ParticleTerm::Element(elem) => {
                let source = particle.source.as_ref().or(elem.source.as_ref());
                NormalizedParticleTerm::Element(self.normalize_element(elem, source)?)
            }
            ParticleTerm::Any(wildcard) => {
                NormalizedParticleTerm::Wildcard(Box::new(NormalizedWildcard {
                    wildcard: wildcard.clone(),
                    target_namespace: self.target_namespace,
                }))
            }
            ParticleTerm::Group(group) => {
                NormalizedParticleTerm::Group(self.normalize_group(group)?)
            }
        };
        self.depth -= 1;

        Ok(collapse_single_child_groups(NormalizedParticle {
            term,
            min_occurs: particle.min_occurs,
            max_occurs: particle.max_occurs,
            source: particle.source.clone(),
            collapsed_from: None,
        }))
    }

    fn normalize_element(
        &mut self,
        elem: &ElementFrameResult,
        source: Option<&SourceRef>,
    ) -> SchemaResult<NormalizedElement> {
        if let Some(ref_name) = &elem.ref_name {
            let elem_key = self
                .schema_set
                .lookup_element(ref_name.namespace, ref_name.local_name);
            let (type_key, block, nillable, fixed_value) = elem_key
                .and_then(|key| self.schema_set.arenas.elements.get(key))
                .map(|decl| {
                    let (eff_block, _) =
                        crate::compiler::substitution::effective_element_constraints(
                            self.schema_set,
                            decl,
                        );
                    let tk = decl
                        .resolved_type
                        .unwrap_or_else(|| TypeKey::Complex(self.schema_set.any_type_key()));
                    (tk, eff_block, decl.nillable, decl.fixed_value.clone())
                })
                .unwrap_or_else(|| {
                    (
                        TypeKey::Complex(self.schema_set.any_type_key()),
                        DerivationSet::empty(),
                        false,
                        None,
                    )
                });
            return Ok(NormalizedElement {
                name: ref_name.local_name,
                namespace: ref_name.namespace,
                type_key,
                element_key: elem_key,
                block,
                nillable,
                fixed_value,
            });
        }

        let name = elem
            .name
            .ok_or_else(|| SchemaError::internal("element particle missing name and ref"))?;
        let index = self.flat_index;
        self.flat_index += 1;

        let namespace = self.schema_set.effective_local_element_namespace(
            elem.target_namespace,
            elem.form.as_deref(),
            source,
            self.target_namespace,
        );
        let type_key = self
            .resolved_types
            .get(index)
            .copied()
            .flatten()
            .or_else(|| resolve_element_type_ref(self.schema_set, elem))
            .unwrap_or_else(|| TypeKey::Complex(self.schema_set.any_type_key()));

        // Compute effective block for local element
        // block=None means absent → inherit blockDefault; Some(b) means explicit (including "").
        let block = match elem.block {
            Some(b) => b,
            None => source
                .and_then(|s| {
                    let doc_id = s.schema_defaults_doc.unwrap_or(s.doc_id);
                    self.schema_set
                        .documents
                        .get(doc_id as usize)
                        .map(|d| d.block_default)
                })
                .unwrap_or_default(),
        };

        Ok(NormalizedElement {
            name,
            namespace,
            type_key,
            element_key: None,
            block,
            nillable: elem.nillable,
            fixed_value: elem.fixed_value.clone(),
        })
    }

    fn normalize_group(&mut self, group: &ModelGroupDefResult) -> SchemaResult<NormalizedGroup> {
        if let Some(ref_name) = &group.ref_name {
            let group_key = self
                .schema_set
                .lookup_model_group(ref_name.namespace, ref_name.local_name)
                .ok_or_else(|| SchemaError::internal("model group reference was not resolved"))?;
            let group_data = self
                .schema_set
                .arenas
                .get_model_group(group_key)
                .ok_or_else(|| SchemaError::internal("resolved model group not found"))?;
            let compositor = group_data
                .compositor
                .ok_or_else(|| SchemaError::internal("resolved model group missing compositor"))?;
            let mut nested = ParticleNormalizer::new(
                self.schema_set,
                group_data.target_namespace,
                &group_data.resolved_particle_types,
            );
            nested.depth = self.depth;
            let particles = group_data
                .particles
                .iter()
                .map(|particle| nested.normalize_particle(particle))
                .collect::<SchemaResult<Vec<_>>>()?;
            return Ok(NormalizedGroup {
                compositor,
                particles,
            });
        }

        let compositor = group
            .compositor
            .ok_or_else(|| SchemaError::internal("inline model group missing compositor"))?;
        let particles = group
            .particles
            .iter()
            .map(|particle| self.normalize_particle(particle))
            .collect::<SchemaResult<Vec<_>>>()?;
        Ok(NormalizedGroup {
            compositor,
            particles,
        })
    }
}

fn resolve_element_type_ref(schema_set: &SchemaSet, elem: &ElementFrameResult) -> Option<TypeKey> {
    match &elem.type_ref {
        Some(crate::parser::frames::TypeRefResult::QName(qname)) => schema_set
            .lookup_type(qname.namespace, qname.local_name)
            .or_else(|| schema_set.get_built_in_type_by_qname(qname.namespace, qname.local_name)),
        _ => None,
    }
}

/// Check if a normalized particle is an empty group (all children removed as pointless).
fn is_empty_group(particle: &NormalizedParticle) -> bool {
    matches!(&particle.term, NormalizedParticleTerm::Group(group) if group.particles.is_empty())
}

/// Top-level particle with maxOccurs=0 or fully pruned group — treated as empty content per §3.8.
pub(super) fn is_effectively_empty(particle: &NormalizedParticle) -> bool {
    particle.max_occurs == Some(0) || is_empty_group(particle)
}

pub(super) fn complex_content_particle(content: &ComplexContentResult) -> Option<&ParticleResult> {
    match content {
        ComplexContentResult::Complex(def) => def.particle.as_ref(),
        ComplexContentResult::Empty | ComplexContentResult::Simple(_) => None,
    }
}

/// Walk up the extension chain to find the effective content particle.
/// Empty extensions inherit their base type's content model.
/// Returns (type_def_owning_particle, particle) so the normalizer uses the
/// correct target_namespace and resolved_content_particle_types.
pub(super) fn effective_base_content_particle<'a>(
    schema_set: &'a SchemaSet,
    base: &'a crate::arenas::ComplexTypeDefData,
) -> (
    &'a crate::arenas::ComplexTypeDefData,
    Option<&'a ParticleResult>,
) {
    let mut current = base;
    let mut depth = 0;
    loop {
        if let Some(particle) = complex_content_particle(&current.content) {
            return (current, Some(particle));
        }
        // If this type has no content and was derived by extension, check its base
        if current.derivation_method != Some(DerivationMethod::Extension) {
            return (current, None);
        }
        let Some(TypeKey::Complex(base_key)) = current.resolved_base_type else {
            return (current, None);
        };
        let Some(base_type) = schema_set.arenas.complex_types.get(base_key) else {
            return (current, None);
        };
        depth += 1;
        if depth > 50 {
            return (current, None); // safety limit
        }
        current = base_type;
    }
}

/// The base type's *effective* content particle, already normalized.
///
/// §3.4.2.3 (complex content extension): when a type is derived by extension
/// and both the base and the extension contribute a non-empty particle, the
/// resulting {content type} particle is `sequence(base-particle,
/// own-particle)` — not just the extension's own contribution.  A restriction
/// of such a type has to be checked against the whole model, otherwise every
/// element the base contributed looks like an element the restriction invented.
///
/// `effective_base_content_particle` only covers the degenerate case of an
/// extension that adds nothing; this walks the whole extension chain and
/// re-assembles the model.
pub(super) fn normalized_effective_base_particle(
    schema_set: &SchemaSet,
    base: &crate::arenas::ComplexTypeDefData,
    depth: usize,
) -> SchemaResult<Option<NormalizedParticle>> {
    let own = match complex_content_particle(&base.content) {
        Some(particle) => Some(normalize_type_particle(schema_set, base, particle)?),
        None => None,
    };

    if depth > 50 || base.derivation_method != Some(DerivationMethod::Extension) {
        return Ok(own);
    }
    let Some(TypeKey::Complex(base_key)) = base.resolved_base_type else {
        return Ok(own);
    };
    let Some(base_type) = schema_set.arenas.complex_types.get(base_key) else {
        return Ok(own);
    };
    let inherited = normalized_effective_base_particle(schema_set, base_type, depth + 1)?;

    Ok(match (inherited, own) {
        (None, own) => own,
        (inherited, None) => inherited,
        (Some(inherited), Some(own)) => {
            if contributes_only_the_empty_sequence(&inherited) {
                Some(own)
            } else if contributes_only_the_empty_sequence(&own) {
                Some(inherited)
            } else {
                Some(NormalizedParticle {
                    term: NormalizedParticleTerm::Group(NormalizedGroup {
                        compositor: Compositor::Sequence,
                        particles: vec![inherited, own],
                    }),
                    min_occurs: 1,
                    max_occurs: Some(1),
                    source: None,
                    collapsed_from: None,
                })
            }
        }
    })
}

pub(super) fn normalize_type_particle(
    schema_set: &SchemaSet,
    type_def: &crate::arenas::ComplexTypeDefData,
    particle: &ParticleResult,
) -> SchemaResult<NormalizedParticle> {
    let mut normalizer = ParticleNormalizer::new(
        schema_set,
        type_def.target_namespace,
        &type_def.resolved_content_particle_types,
    );
    let particle = normalizer.normalize_particle(particle)?;
    let particle = remove_pointless_particles(particle);
    // XSD 1.1: skip flattening to preserve structural grouping needed for
    // intensional restriction (e.g. a single-branch choice whose collapsed
    // sequence must match against a multi-branch choice in the base).
    if !schema_set.is_xsd11() {
        let particle = flatten_same_compositor_groups(particle);
        return Ok(particle);
    }
    Ok(particle)
}

/// Normalize a top-level named model group into a `NormalizedParticle` for
/// §src-redefine 6.2.2 restriction comparisons.
///
/// **Chain-of-redefine caveat**: when called on an original whose own
/// particles include `group-ref`s, those refs are resolved via
/// `schema_set.lookup_model_group` inside [`ParticleNormalizer::normalize_group`]
/// — which returns the *currently bound* version. For a chain
/// `orig → v1 → v2`, v1's inner group-refs resolve to whatever the
/// current namespace binding is, not to what v1 saw at creation time. This
/// is a pre-existing limitation shared with the complex-type restriction
/// path; it is not fixed here.
pub(super) fn normalize_model_group_as_particle(
    schema_set: &SchemaSet,
    group_data: &crate::arenas::ModelGroupData,
) -> SchemaResult<NormalizedParticle> {
    let compositor = group_data
        .compositor
        .ok_or_else(|| SchemaError::internal("redefined named model group missing compositor"))?;

    let mut normalizer = ParticleNormalizer::new(
        schema_set,
        group_data.target_namespace,
        &group_data.resolved_particle_types,
    );
    let particles = group_data
        .particles
        .iter()
        .map(|particle| normalizer.normalize_particle(particle))
        .collect::<SchemaResult<Vec<_>>>()?;

    let wrapper = NormalizedParticle {
        term: NormalizedParticleTerm::Group(NormalizedGroup {
            compositor,
            particles,
        }),
        min_occurs: group_data.min_occurs,
        max_occurs: group_data.max_occurs,
        source: group_data.source.clone(),
        collapsed_from: None,
    };

    // `normalize_particle` collapses every child; the outer wrapper we
    // built by hand above is *not* collapsed and must be so explicitly so
    // a single-element named group ends up shaped identically to the same
    // content inside a complex type.
    let particle = collapse_single_child_groups(wrapper);
    let particle = remove_pointless_particles(particle);
    // XSD 1.1: skip flattening (same rationale as `normalize_type_particle`).
    if !schema_set.is_xsd11() {
        let particle = flatten_same_compositor_groups(particle);
        return Ok(particle);
    }
    Ok(particle)
}

/// Remove "pointless" particles per Section 3.8 normalization:
/// - Particles with maxOccurs=0 are effectively absent
/// - Groups with no remaining children after removal are also pointless
fn remove_pointless_particles(mut particle: NormalizedParticle) -> NormalizedParticle {
    if let NormalizedParticleTerm::Group(group) = &mut particle.term {
        group.particles = group
            .particles
            .drain(..)
            .map(remove_pointless_particles)
            .filter(|p| p.max_occurs != Some(0) && !is_pointless_empty_group(p))
            .collect();
    }
    particle
}

/// True when a particle accepts the empty sequence and nothing else, so it can
/// be dropped when assembling `sequence(inherited, own)` for an extension.
///
/// Deliberately narrower than [`is_effectively_empty`]: a *required* empty
/// choice accepts no sequence at all, so dropping it would turn an
/// unsatisfiable content type into a satisfiable one and let a restriction of
/// it be checked against the inherited half alone.
fn contributes_only_the_empty_sequence(particle: &NormalizedParticle) -> bool {
    particle.max_occurs == Some(0) || is_pointless_empty_group(particle)
}

/// A child group particle that contributes nothing to the content model.
///
/// `sequence`/`all` with no particles accepts only the empty sequence however
/// often it is repeated.  An empty `choice` accepts nothing at all, so it is
/// only pointless when it is optional — `<choice minOccurs="1"/>` makes the
/// content model unsatisfiable and must be kept.
fn is_pointless_empty_group(particle: &NormalizedParticle) -> bool {
    match &particle.term {
        NormalizedParticleTerm::Group(group) if group.particles.is_empty() => {
            match group.compositor {
                Compositor::Sequence | Compositor::All => true,
                Compositor::Choice => particle.min_occurs == 0,
            }
        }
        _ => false,
    }
}

/// Flatten nested groups with unit occurs and the same compositor into
/// their parent (Section 3.8 particle normalization).
/// E.g. sequence(sequence{1,1}(a, b), c) → sequence(a, b, c).
fn flatten_same_compositor_groups(mut particle: NormalizedParticle) -> NormalizedParticle {
    if let NormalizedParticleTerm::Group(group) = &mut particle.term {
        // First recurse into children
        group.particles = group
            .particles
            .drain(..)
            .map(flatten_same_compositor_groups)
            .collect();
        // Then flatten children that are same-compositor groups with unit occurs
        let parent_compositor = group.compositor;
        let mut flattened = Vec::with_capacity(group.particles.len());
        for child in group.particles.drain(..) {
            if let NormalizedParticleTerm::Group(ref child_group) = child.term {
                if child_group.compositor == parent_compositor
                    && occurs_is_unit(child.min_occurs, child.max_occurs)
                {
                    flattened.extend(child_group.particles.iter().cloned());
                    continue;
                }
            }
            flattened.push(child);
        }
        group.particles = flattened;
    }
    particle
}

pub(super) fn collapse_single_child_groups(mut particle: NormalizedParticle) -> NormalizedParticle {
    if let NormalizedParticleTerm::Group(group) = &mut particle.term {
        group.particles = group
            .particles
            .drain(..)
            .map(collapse_single_child_groups)
            .collect();
    }

    loop {
        let child = match &particle.term {
            NormalizedParticleTerm::Group(group)
                if group.particles.len() == 1
                    && can_collapse_single_child_group(
                        group.compositor,
                        particle.min_occurs,
                        particle.max_occurs,
                        &group.particles[0],
                    ) =>
            {
                Some(group.particles[0].clone())
            }
            _ => None,
        };
        let Some(child) = child else {
            return particle;
        };
        let (min_occurs, max_occurs) = multiply_occurs(
            particle.min_occurs,
            particle.max_occurs,
            child.min_occurs,
            child.max_occurs,
        );
        let compositor = match &particle.term {
            NormalizedParticleTerm::Group(group) => Some(group.compositor),
            _ => None,
        };
        particle = NormalizedParticle {
            term: child.term,
            min_occurs,
            max_occurs,
            source: particle.source.clone().or(child.source),
            collapsed_from: compositor,
        };
    }
}

fn can_collapse_single_child_group(
    compositor: Compositor,
    group_min_occurs: u32,
    group_max_occurs: Option<u32>,
    child: &NormalizedParticle,
) -> bool {
    if compositor == Compositor::Choice {
        return true;
    }

    occurs_is_unit(group_min_occurs, group_max_occurs) || child.max_occurs == Some(1)
}

pub(super) fn occurs_is_unit(min_occurs: u32, max_occurs: Option<u32>) -> bool {
    min_occurs == 1 && max_occurs == Some(1)
}

pub(super) fn multiply_occurs(
    left_min: u32,
    left_max: Option<u32>,
    right_min: u32,
    right_max: Option<u32>,
) -> (u32, Option<u32>) {
    let min_occurs = left_min.saturating_mul(right_min);
    let max_occurs = match (left_max, right_max) {
        (Some(left), Some(right)) => Some(left.saturating_mul(right)),
        (Some(0), None) | (None, Some(0)) => Some(0),
        _ => None,
    };
    (min_occurs, max_occurs)
}

/// XSD 1.1: fold a single-child sequence/all group by multiplying occurs.
/// sequence{M,N}(e{m,n}) ≡ e{M*m, N*n}
pub(super) fn fold_single_child_group(particle: &NormalizedParticle) -> Option<NormalizedParticle> {
    if let NormalizedParticleTerm::Group(group) = &particle.term {
        if group.particles.len() == 1
            && matches!(group.compositor, Compositor::Sequence | Compositor::All)
        {
            let child = &group.particles[0];
            let (min_occurs, max_occurs) = multiply_occurs(
                particle.min_occurs,
                particle.max_occurs,
                child.min_occurs,
                child.max_occurs,
            );
            return Some(NormalizedParticle {
                term: child.term.clone(),
                min_occurs,
                max_occurs,
                source: particle.source.clone().or(child.source.clone()),
                collapsed_from: None,
            });
        }
    }
    None
}
