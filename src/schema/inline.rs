//! Inline type assembly pass
//!
//! This module implements Phase 3 inline type resolution as specified in
//! XSD_INLINE_TYPE_RESOLUTION_DESIGN.md. It assembles inline type definitions
//! (TypeRefResult::Inline and inline_type fields) into arena TypeKey values.
//!
//! # Overview
//!
//! Inline types are anonymous type definitions that appear directly within
//! element declarations, attribute declarations, or as base/item/member types
//! in type derivation. These need to be allocated in the type arenas before
//! the reference resolution phase can complete.
//!
//! # Work Queue Approach
//!
//! To avoid borrow conflicts and handle nested inline types, we use a two-pass
//! work queue approach:
//!
//! 1. **Scan Pass**: Collect all inline types into `InlineTypeJob` records.
//!    Each job records the owner component and the inline type AST (cloned).
//!
//! 2. **Assembly Pass**: For each job, assemble the inline type into the
//!    arena and update the owner's resolved_* field with the TypeKey.

use crate::arenas::{
    ComplexTypeDefData, ResolvedAttributeUse, ResolvedParticleTerm, SimpleTypeDefData,
};
use crate::error::SchemaResult;
use crate::ids::*;
use crate::parser::frames::{
    ComplexContentResult, ParticleTerm, TypeFrameResult, TypeRefResult,
};
use crate::schema::SchemaSet;

/// Statistics from the inline type assembly pass
#[derive(Debug, Default)]
pub struct InlineAssemblyStats {
    /// Number of element inline types assembled
    pub element_inline_types: usize,
    /// Number of attribute inline types assembled
    pub attribute_inline_types: usize,
    /// Number of simple type base/item/member inline types assembled
    pub simple_type_inline_derivations: usize,
    /// Number of complex type base type inline types assembled
    pub complex_type_inline_derivations: usize,
    /// Number of inline types in model group particles assembled
    pub model_group_inline_types: usize,
    /// Number of inline types in attribute groups assembled
    pub attribute_group_inline_types: usize,
    /// Number of inline types in complex type attributes assembled
    pub complex_type_attribute_inline_types: usize,
    /// Total inline types assembled
    pub total_inline_types: usize,
}

/// Role of an inline type within its owner
#[derive(Debug, Clone, Copy)]
enum InlineRole {
    /// Element's type (inline_type field)
    ElementType,
    /// Attribute's type (inline_type field)
    AttributeType,
    /// Simple type restriction base (base_type: Inline)
    SimpleTypeBase,
    /// List type item type (item_type: Inline)
    ListItemType,
    /// Union member type (member_types[index]: Inline)
    UnionMemberType(usize),
    /// Complex type derivation base (base_type: Inline)
    ComplexTypeBase,
    /// Attribute use inline type within complex type (attributes[index])
    ComplexTypeAttribute(usize),
    /// Particle inline type in model group (particles[index])
    ModelGroupParticle(usize),
    /// Attribute use inline type in attribute group (attributes[index])
    AttributeGroupAttribute(usize),
}

/// Owner of an inline type
#[derive(Debug, Clone, Copy)]
enum InlineOwner {
    Element(ElementKey),
    Attribute(AttributeKey),
    SimpleType(SimpleTypeKey),
    ComplexType(ComplexTypeKey),
    ModelGroup(ModelGroupKey),
    AttributeGroup(AttributeGroupKey),
}

/// A job representing an inline type to be assembled
struct InlineTypeJob {
    owner: InlineOwner,
    role: InlineRole,
    type_frame: TypeFrameResult,
    target_namespace: Option<NameId>,
}

/// Assemble all inline types in the schema set
///
/// This function walks all components, collects inline type definitions,
/// assembles them into the arenas, and updates the resolved_* fields.
///
/// This should be called after top-level component assembly but before
/// reference resolution.
pub fn assemble_inline_types(schema_set: &mut SchemaSet) -> SchemaResult<InlineAssemblyStats> {
    let mut stats = InlineAssemblyStats::default();

    // Collect all inline type jobs
    let mut jobs = collect_inline_type_jobs(schema_set);

    // Process jobs iteratively (nested inline types may add more jobs)
    while !jobs.is_empty() {
        let current_jobs: Vec<_> = jobs.drain(..).collect();

        for job in current_jobs {
            let type_key = assemble_inline_type(schema_set, &job.type_frame, job.target_namespace)?;
            update_owner(schema_set, &job, type_key, &mut stats)?;

            // Check if the newly assembled type has nested inline types
            collect_nested_inline_types(schema_set, type_key, job.target_namespace, &mut jobs);
        }
    }

    Ok(stats)
}

/// Collect all inline type jobs from schema components
fn collect_inline_type_jobs(schema_set: &SchemaSet) -> Vec<InlineTypeJob> {
    let mut jobs = Vec::new();

    // Scan elements
    for (key, elem) in schema_set.arenas.elements.iter() {
        let target_ns = elem.target_namespace;
        if let Some(inline_type) = &elem.inline_type {
            jobs.push(InlineTypeJob {
                owner: InlineOwner::Element(key),
                role: InlineRole::ElementType,
                type_frame: (**inline_type).clone(),
                target_namespace: target_ns,
            });
        }
    }

    // Scan attributes
    for (key, attr) in schema_set.arenas.attributes.iter() {
        let target_ns = attr.target_namespace;
        if let Some(inline_type) = &attr.inline_type {
            jobs.push(InlineTypeJob {
                owner: InlineOwner::Attribute(key),
                role: InlineRole::AttributeType,
                type_frame: TypeFrameResult::Simple((**inline_type).clone()),
                target_namespace: target_ns,
            });
        }
    }

    // Scan simple types
    for (key, simple) in schema_set.arenas.simple_types.iter() {
        let target_ns = simple.target_namespace;

        // Base type inline
        if let Some(TypeRefResult::Inline(inline_type)) = &simple.base_type {
            jobs.push(InlineTypeJob {
                owner: InlineOwner::SimpleType(key),
                role: InlineRole::SimpleTypeBase,
                type_frame: (**inline_type).clone(),
                target_namespace: target_ns,
            });
        }

        // Item type inline (for list types)
        if let Some(TypeRefResult::Inline(inline_type)) = &simple.item_type {
            jobs.push(InlineTypeJob {
                owner: InlineOwner::SimpleType(key),
                role: InlineRole::ListItemType,
                type_frame: (**inline_type).clone(),
                target_namespace: target_ns,
            });
        }

        // Member types inline (for union types)
        for (idx, member) in simple.member_types.iter().enumerate() {
            if let TypeRefResult::Inline(inline_type) = member {
                jobs.push(InlineTypeJob {
                    owner: InlineOwner::SimpleType(key),
                    role: InlineRole::UnionMemberType(idx),
                    type_frame: (**inline_type).clone(),
                    target_namespace: target_ns,
                });
            }
        }
    }

    // Scan complex types
    for (key, complex) in schema_set.arenas.complex_types.iter() {
        let target_ns = complex.target_namespace;

        // Base type inline
        if let Some(TypeRefResult::Inline(inline_type)) = &complex.base_type {
            jobs.push(InlineTypeJob {
                owner: InlineOwner::ComplexType(key),
                role: InlineRole::ComplexTypeBase,
                type_frame: (**inline_type).clone(),
                target_namespace: target_ns,
            });
        }

        // Attribute uses with inline types
        for (idx, attr_use) in complex.attributes.iter().enumerate() {
            if let Some(inline_type) = &attr_use.attribute.inline_type {
                jobs.push(InlineTypeJob {
                    owner: InlineOwner::ComplexType(key),
                    role: InlineRole::ComplexTypeAttribute(idx),
                    type_frame: TypeFrameResult::Simple((**inline_type).clone()),
                    target_namespace: target_ns,
                });
            }
        }

        // Content particles (in complex content)
        collect_content_inline_types(&complex.content, key, target_ns, &mut jobs);
    }

    // Scan model groups
    for (key, group) in schema_set.arenas.model_groups.iter() {
        let target_ns = group.target_namespace;

        for (idx, particle) in group.particles.iter().enumerate() {
            if let ParticleTerm::Element(elem) = &particle.term {
                if let Some(inline_type) = &elem.inline_type {
                    jobs.push(InlineTypeJob {
                        owner: InlineOwner::ModelGroup(key),
                        role: InlineRole::ModelGroupParticle(idx),
                        type_frame: (**inline_type).clone(),
                        target_namespace: target_ns,
                    });
                }
            }
        }
    }

    // Scan attribute groups
    for (key, group) in schema_set.arenas.attribute_groups.iter() {
        let target_ns = group.target_namespace;

        for (idx, attr_use) in group.attributes.iter().enumerate() {
            if let Some(inline_type) = &attr_use.attribute.inline_type {
                jobs.push(InlineTypeJob {
                    owner: InlineOwner::AttributeGroup(key),
                    role: InlineRole::AttributeGroupAttribute(idx),
                    type_frame: TypeFrameResult::Simple((**inline_type).clone()),
                    target_namespace: target_ns,
                });
            }
        }
    }

    jobs
}

/// Collect inline types from complex content (recursive helper)
fn collect_content_inline_types(
    content: &ComplexContentResult,
    owner_key: ComplexTypeKey,
    target_ns: Option<NameId>,
    jobs: &mut Vec<InlineTypeJob>,
) {
    match content {
        ComplexContentResult::Complex(complex_content) => {
            // Check base type inline
            if let Some(TypeRefResult::Inline(inline_type)) = &complex_content.base_type {
                jobs.push(InlineTypeJob {
                    owner: InlineOwner::ComplexType(owner_key),
                    role: InlineRole::ComplexTypeBase,
                    type_frame: (**inline_type).clone(),
                    target_namespace: target_ns,
                });
            }

            // Check attributes in complex content
            for (idx, attr_use) in complex_content.attributes.iter().enumerate() {
                if let Some(inline_type) = &attr_use.attribute.inline_type {
                    jobs.push(InlineTypeJob {
                        owner: InlineOwner::ComplexType(owner_key),
                        role: InlineRole::ComplexTypeAttribute(idx),
                        type_frame: TypeFrameResult::Simple((**inline_type).clone()),
                        target_namespace: target_ns,
                    });
                }
            }
        }
        ComplexContentResult::Simple(simple_content) => {
            // Check base type inline
            if let Some(TypeRefResult::Inline(inline_type)) = &simple_content.base_type {
                jobs.push(InlineTypeJob {
                    owner: InlineOwner::ComplexType(owner_key),
                    role: InlineRole::ComplexTypeBase,
                    type_frame: (**inline_type).clone(),
                    target_namespace: target_ns,
                });
            }

            // Check attributes in simple content
            for (idx, attr_use) in simple_content.attributes.iter().enumerate() {
                if let Some(inline_type) = &attr_use.attribute.inline_type {
                    jobs.push(InlineTypeJob {
                        owner: InlineOwner::ComplexType(owner_key),
                        role: InlineRole::ComplexTypeAttribute(idx),
                        type_frame: TypeFrameResult::Simple((**inline_type).clone()),
                        target_namespace: target_ns,
                    });
                }
            }
        }
        ComplexContentResult::Empty => {}
    }
}

/// Assemble an inline type into the arena
///
/// Returns the TypeKey for the newly allocated type.
/// Anonymous types are not registered in namespace tables.
fn assemble_inline_type(
    schema_set: &mut SchemaSet,
    type_frame: &TypeFrameResult,
    target_namespace: Option<NameId>,
) -> SchemaResult<TypeKey> {
    match type_frame {
        TypeFrameResult::Simple(simple) => {
            let data = SimpleTypeDefData {
                name: simple.name, // May be None for anonymous types
                target_namespace,
                variety: simple.variety,
                base_type: simple.base_type.clone(),
                item_type: simple.item_type.clone(),
                member_types: simple.member_types.clone(),
                facets: simple.facets.clone(),
                final_derivation: simple.final_derivation,
                id: simple.id.clone(),
                derivation_id: simple.derivation_id.clone(),
                annotation: simple.annotation.clone(),
                source: simple.source.clone(),
                // Resolved references (to be populated later)
                resolved_base_type: None,
                resolved_item_type: None,
                resolved_member_types: Vec::new(),
            };
            let key = schema_set.arenas.alloc_simple_type(data);
            Ok(TypeKey::Simple(key))
        }
        TypeFrameResult::Complex(complex) => {
            let data = ComplexTypeDefData {
                name: complex.name, // May be None for anonymous types
                target_namespace,
                base_type: complex.base_type.clone(),
                derivation_method: complex.derivation_method,
                content: complex.content.clone(),
                attributes: complex.attributes.clone(),
                attribute_groups: complex.attribute_groups.clone(),
                attribute_wildcard: complex.attribute_wildcard.clone(),
                mixed: complex.mixed,
                is_abstract: complex.is_abstract,
                final_derivation: complex.final_derivation,
                block: complex.block,
                default_attributes_apply: complex.default_attributes_apply,
                id: complex.id.clone(),
                annotation: complex.annotation.clone(),
                source: complex.source.clone(),
                // Resolved references (to be populated later)
                resolved_base_type: None,
                resolved_attribute_groups: Vec::new(),
                resolved_attributes: Vec::new(),
            };
            let key = schema_set.arenas.alloc_complex_type(data);
            Ok(TypeKey::Complex(key))
        }
    }
}

/// Update the owner component with the resolved type key
fn update_owner(
    schema_set: &mut SchemaSet,
    job: &InlineTypeJob,
    type_key: TypeKey,
    stats: &mut InlineAssemblyStats,
) -> SchemaResult<()> {
    match job.owner {
        InlineOwner::Element(key) => {
            if let Some(elem) = schema_set.arenas.elements.get_mut(key) {
                elem.resolved_type = Some(type_key);
                stats.element_inline_types += 1;
                stats.total_inline_types += 1;
            }
        }
        InlineOwner::Attribute(key) => {
            if let Some(attr) = schema_set.arenas.attributes.get_mut(key) {
                attr.resolved_type = Some(type_key);
                stats.attribute_inline_types += 1;
                stats.total_inline_types += 1;
            }
        }
        InlineOwner::SimpleType(key) => {
            if let Some(simple) = schema_set.arenas.simple_types.get_mut(key) {
                match job.role {
                    InlineRole::SimpleTypeBase => {
                        simple.resolved_base_type = Some(type_key);
                    }
                    InlineRole::ListItemType => {
                        simple.resolved_item_type = Some(type_key);
                    }
                    InlineRole::UnionMemberType(idx) => {
                        // For union members, we need to track position
                        // Insert at the correct position in resolved_member_types
                        while simple.resolved_member_types.len() <= idx {
                            // Use a placeholder that will be overwritten
                            // This handles sparse indices from inline types
                            simple.resolved_member_types.push(type_key);
                        }
                        simple.resolved_member_types[idx] = type_key;
                    }
                    _ => {}
                }
                stats.simple_type_inline_derivations += 1;
                stats.total_inline_types += 1;
            }
        }
        InlineOwner::ComplexType(key) => {
            if let Some(complex) = schema_set.arenas.complex_types.get_mut(key) {
                match job.role {
                    InlineRole::ComplexTypeBase => {
                        complex.resolved_base_type = Some(type_key);
                        stats.complex_type_inline_derivations += 1;
                    }
                    InlineRole::ComplexTypeAttribute(idx) => {
                        // Ensure resolved_attributes vec is large enough
                        while complex.resolved_attributes.len() <= idx {
                            complex.resolved_attributes.push(ResolvedAttributeUse {
                                resolved_type: None,
                                resolved_ref: None,
                            });
                        }
                        complex.resolved_attributes[idx].resolved_type = Some(type_key);
                        stats.complex_type_attribute_inline_types += 1;
                    }
                    _ => {}
                }
                stats.total_inline_types += 1;
            }
        }
        InlineOwner::ModelGroup(key) => {
            if let Some(group) = schema_set.arenas.model_groups.get_mut(key) {
                if let InlineRole::ModelGroupParticle(idx) = job.role {
                    // Ensure resolved_particles vec is large enough
                    while group.resolved_particles.len() <= idx {
                        group.resolved_particles.push(ResolvedParticleTerm::Any);
                    }
                    group.resolved_particles[idx] = ResolvedParticleTerm::Element {
                        resolved_type: Some(type_key),
                        resolved_ref: None,
                    };
                    stats.model_group_inline_types += 1;
                    stats.total_inline_types += 1;
                }
            }
        }
        InlineOwner::AttributeGroup(key) => {
            if let Some(group) = schema_set.arenas.attribute_groups.get_mut(key) {
                if let InlineRole::AttributeGroupAttribute(idx) = job.role {
                    // Ensure resolved_attributes vec is large enough
                    while group.resolved_attributes.len() <= idx {
                        group.resolved_attributes.push(ResolvedAttributeUse {
                            resolved_type: None,
                            resolved_ref: None,
                        });
                    }
                    group.resolved_attributes[idx].resolved_type = Some(type_key);
                    stats.attribute_group_inline_types += 1;
                    stats.total_inline_types += 1;
                }
            }
        }
    }
    Ok(())
}

/// Collect nested inline types from a newly assembled type
fn collect_nested_inline_types(
    schema_set: &SchemaSet,
    type_key: TypeKey,
    target_namespace: Option<NameId>,
    jobs: &mut Vec<InlineTypeJob>,
) {
    match type_key {
        TypeKey::Simple(key) => {
            if let Some(simple) = schema_set.arenas.simple_types.get(key) {
                // Check for nested inline types in base/item/member
                if let Some(TypeRefResult::Inline(inline_type)) = &simple.base_type {
                    jobs.push(InlineTypeJob {
                        owner: InlineOwner::SimpleType(key),
                        role: InlineRole::SimpleTypeBase,
                        type_frame: (**inline_type).clone(),
                        target_namespace,
                    });
                }
                if let Some(TypeRefResult::Inline(inline_type)) = &simple.item_type {
                    jobs.push(InlineTypeJob {
                        owner: InlineOwner::SimpleType(key),
                        role: InlineRole::ListItemType,
                        type_frame: (**inline_type).clone(),
                        target_namespace,
                    });
                }
                for (idx, member) in simple.member_types.iter().enumerate() {
                    if let TypeRefResult::Inline(inline_type) = member {
                        jobs.push(InlineTypeJob {
                            owner: InlineOwner::SimpleType(key),
                            role: InlineRole::UnionMemberType(idx),
                            type_frame: (**inline_type).clone(),
                            target_namespace,
                        });
                    }
                }
            }
        }
        TypeKey::Complex(key) => {
            if let Some(complex) = schema_set.arenas.complex_types.get(key) {
                // Check for nested inline types in base type
                if let Some(TypeRefResult::Inline(inline_type)) = &complex.base_type {
                    jobs.push(InlineTypeJob {
                        owner: InlineOwner::ComplexType(key),
                        role: InlineRole::ComplexTypeBase,
                        type_frame: (**inline_type).clone(),
                        target_namespace,
                    });
                }

                // Check for inline types in attributes
                for (idx, attr_use) in complex.attributes.iter().enumerate() {
                    if let Some(inline_type) = &attr_use.attribute.inline_type {
                        jobs.push(InlineTypeJob {
                            owner: InlineOwner::ComplexType(key),
                            role: InlineRole::ComplexTypeAttribute(idx),
                            type_frame: TypeFrameResult::Simple((**inline_type).clone()),
                            target_namespace,
                        });
                    }
                }

                // Check content for nested inline types
                collect_content_inline_types(&complex.content, key, target_namespace, jobs);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arenas::ElementDeclData;
    use crate::parser::frames::{SimpleTypeResult, SimpleTypeVariety};
    use crate::schema::model::DerivationSet;
    use crate::types::facets::FacetSet;

    fn create_simple_type_frame(variety: SimpleTypeVariety) -> TypeFrameResult {
        TypeFrameResult::Simple(SimpleTypeResult {
            name: None,
            variety,
            base_type: None,
            item_type: None,
            member_types: Vec::new(),
            facets: FacetSet::new(),
            final_derivation: DerivationSet::empty(),
            id: None,
            derivation_id: None,
            annotation: None,
            source: None,
        })
    }

    #[test]
    fn test_assemble_inline_types_empty_schema() {
        let mut schema_set = SchemaSet::new();
        let result = assemble_inline_types(&mut schema_set);
        assert!(result.is_ok());
        let stats = result.unwrap();
        assert_eq!(stats.total_inline_types, 0);
    }

    #[test]
    fn test_element_with_inline_simple_type() {
        let mut schema_set = SchemaSet::new();

        let elem_name = schema_set.name_table.add("testElement");
        let inline_type = Box::new(create_simple_type_frame(SimpleTypeVariety::Atomic));

        let elem_data = ElementDeclData {
            name: Some(elem_name),
            target_namespace: None,
            ref_name: None,
            type_ref: None,
            inline_type: Some(inline_type),
            substitution_group: Vec::new(),
            default_value: None,
            fixed_value: None,
            nillable: false,
            is_abstract: false,
            min_occurs: 1,
            max_occurs: Some(1),
            block: DerivationSet::empty(),
            final_derivation: DerivationSet::empty(),
            form: None,
            id: None,
            alternatives: Vec::new(),
            identity_constraints: Vec::new(),
            annotation: None,
            source: None,
            resolved_type: None,
            resolved_ref: None,
            resolved_substitution_groups: Vec::new(),
        };

        let elem_key = schema_set.arenas.alloc_element(elem_data);

        // Run inline assembly
        let result = assemble_inline_types(&mut schema_set);
        assert!(result.is_ok());
        let stats = result.unwrap();
        assert_eq!(stats.element_inline_types, 1);
        assert_eq!(stats.total_inline_types, 1);

        // Verify resolved_type is set
        let elem = schema_set.arenas.elements.get(elem_key).unwrap();
        assert!(elem.resolved_type.is_some());
        assert!(matches!(elem.resolved_type, Some(TypeKey::Simple(_))));

        // Verify inline_type is preserved
        assert!(elem.inline_type.is_some());
    }

    #[test]
    fn test_simple_type_with_inline_base() {
        let mut schema_set = SchemaSet::new();

        let type_name = schema_set.name_table.add("testType");
        let inline_base = Box::new(create_simple_type_frame(SimpleTypeVariety::Atomic));

        let type_data = SimpleTypeDefData {
            name: Some(type_name),
            target_namespace: None,
            variety: SimpleTypeVariety::Atomic,
            base_type: Some(TypeRefResult::Inline(inline_base)),
            item_type: None,
            member_types: Vec::new(),
            facets: FacetSet::new(),
            final_derivation: DerivationSet::empty(),
            id: None,
            derivation_id: None,
            annotation: None,
            source: None,
            resolved_base_type: None,
            resolved_item_type: None,
            resolved_member_types: Vec::new(),
        };

        let type_key = schema_set.arenas.alloc_simple_type(type_data);

        // Run inline assembly
        let result = assemble_inline_types(&mut schema_set);
        assert!(result.is_ok());
        let stats = result.unwrap();
        assert_eq!(stats.simple_type_inline_derivations, 1);

        // Verify resolved_base_type is set
        let simple = schema_set.arenas.simple_types.get(type_key).unwrap();
        assert!(simple.resolved_base_type.is_some());

        // Verify original base_type is preserved
        assert!(matches!(simple.base_type, Some(TypeRefResult::Inline(_))));
    }

    #[test]
    fn test_list_type_with_inline_item() {
        let mut schema_set = SchemaSet::new();

        let type_name = schema_set.name_table.add("listType");
        let inline_item = Box::new(create_simple_type_frame(SimpleTypeVariety::Atomic));

        let type_data = SimpleTypeDefData {
            name: Some(type_name),
            target_namespace: None,
            variety: SimpleTypeVariety::List,
            base_type: None,
            item_type: Some(TypeRefResult::Inline(inline_item)),
            member_types: Vec::new(),
            facets: FacetSet::new(),
            final_derivation: DerivationSet::empty(),
            id: None,
            derivation_id: None,
            annotation: None,
            source: None,
            resolved_base_type: None,
            resolved_item_type: None,
            resolved_member_types: Vec::new(),
        };

        let type_key = schema_set.arenas.alloc_simple_type(type_data);

        // Run inline assembly
        let result = assemble_inline_types(&mut schema_set);
        assert!(result.is_ok());

        // Verify resolved_item_type is set
        let simple = schema_set.arenas.simple_types.get(type_key).unwrap();
        assert!(simple.resolved_item_type.is_some());
    }

    #[test]
    fn test_union_type_with_inline_members() {
        let mut schema_set = SchemaSet::new();

        let type_name = schema_set.name_table.add("unionType");
        let inline_member1 = Box::new(create_simple_type_frame(SimpleTypeVariety::Atomic));
        let inline_member2 = Box::new(create_simple_type_frame(SimpleTypeVariety::Atomic));

        let type_data = SimpleTypeDefData {
            name: Some(type_name),
            target_namespace: None,
            variety: SimpleTypeVariety::Union,
            base_type: None,
            item_type: None,
            member_types: vec![
                TypeRefResult::Inline(inline_member1),
                TypeRefResult::Inline(inline_member2),
            ],
            facets: FacetSet::new(),
            final_derivation: DerivationSet::empty(),
            id: None,
            derivation_id: None,
            annotation: None,
            source: None,
            resolved_base_type: None,
            resolved_item_type: None,
            resolved_member_types: Vec::new(),
        };

        let type_key = schema_set.arenas.alloc_simple_type(type_data);

        // Run inline assembly
        let result = assemble_inline_types(&mut schema_set);
        assert!(result.is_ok());
        let stats = result.unwrap();
        assert_eq!(stats.simple_type_inline_derivations, 2);

        // Verify resolved_member_types has both members
        let simple = schema_set.arenas.simple_types.get(type_key).unwrap();
        assert_eq!(simple.resolved_member_types.len(), 2);
    }
}
