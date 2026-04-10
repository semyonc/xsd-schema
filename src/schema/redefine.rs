//! xs:redefine processing
//!
//! Redefine allows extending/restricting types and groups from an included schema.
//! The redefining component must:
//! - Have the same name as the original
//! - Reference itself as the base type (for type redefinitions)
//! - Reference itself within the group (for group redefinitions)
//!
//! # XSD 1.0 Constraints
//!
//! For simpleType/complexType redefinitions:
//! - The redefinition must derive from the original type by restriction or extension
//! - The base type reference must use the same name as the type being redefined (self-reference)
//!
//! For group/attributeGroup redefinitions:
//! - The redefinition must contain exactly one reference to the original group
//! - This reference is replaced with the original group's content

use crate::error::{SchemaError, SchemaResult};
use crate::ids::*;
use crate::schema::composition::{
    ComponentIdentity, ComponentKey, ComponentKind, record_provenance, redefined_action,
};
use crate::schema::model::RedefineDirective;
use crate::schema::SchemaSet;

/// Apply a redefine directive to the schema set.
///
/// This replaces the original components with the redefined versions,
/// after validating the redefinition constraints.
///
/// Uses document-scoped lookup via `resolved_doc_id` when available,
/// falling back to global namespace table lookup for backward compatibility.
pub fn apply_redefine(
    schema_set: &mut SchemaSet,
    redefine: &RedefineDirective,
) -> SchemaResult<()> {
    let target_doc_id = redefine.resolved_doc_id;
    // The redefining document is the one that contains the xs:redefine element
    let redefining_doc_id = redefine.source.as_ref().map(|s| s.doc_id);

    for simple_key in &redefine.simple_types {
        apply_simple_type_redefine(schema_set, *simple_key, target_doc_id, redefining_doc_id)?;
    }

    for complex_key in &redefine.complex_types {
        apply_complex_type_redefine(schema_set, *complex_key, target_doc_id, redefining_doc_id)?;
    }

    for group_key in &redefine.groups {
        apply_model_group_redefine(schema_set, *group_key, target_doc_id, redefining_doc_id)?;
    }

    for attr_group_key in &redefine.attribute_groups {
        apply_attribute_group_redefine(schema_set, *attr_group_key, target_doc_id, redefining_doc_id)?;
    }

    Ok(())
}

/// Apply a simple type redefinition.
fn apply_simple_type_redefine(
    schema_set: &mut SchemaSet,
    new_key: SimpleTypeKey,
    target_doc_id: Option<DocumentId>,
    redefining_doc_id: Option<DocumentId>,
) -> SchemaResult<()> {
    let new_type = schema_set
        .arenas
        .simple_types
        .get(new_key)
        .ok_or_else(|| SchemaError::internal("Redefine: new simple type not found"))?;

    let name = new_type.name.ok_or_else(|| {
        SchemaError::structural(
            "src-redefine",
            "Redefined simple type must have a name",
            None,
        )
    })?;
    let namespace = new_type.target_namespace;

    // Kind-specific, document-scoped lookup; global fallback only when
    // resolved_doc_id is None (pre-loaded schemas without resolution).
    let original_key = match target_doc_id {
        Some(id) => schema_set
            .documents
            .get(id as usize)
            .and_then(|doc| doc.component_index.lookup_simple_type(namespace, name))
            .map(TypeKey::Simple),
        None => schema_set.lookup_type(namespace, name),
    }
    .ok_or_else(|| {
        SchemaError::structural(
            "src-redefine",
            format!(
                "Original simple type '{}' not found for redefinition in {}",
                schema_set.name_table.resolve(name),
                target_doc_id
                    .and_then(|id| schema_set.documents.get(id as usize))
                    .map(|d| d.base_uri.as_str())
                    .unwrap_or("schema"),
            ),
            None,
        )
    })?;

    validate_self_derivation_simple(schema_set, new_key, name, namespace)?;

    // Store original type key for base-type redirection during resolution
    if let TypeKey::Simple(orig_key) = original_key {
        if let Some(st) = schema_set.arenas.simple_types.get_mut(new_key) {
            st.redefine_original = Some(orig_key);
        }
    }

    let ns_table = schema_set.get_or_create_namespace(namespace);
    ns_table.register_type(name, TypeKey::Simple(new_key));

    // Make the redefined type visible in the redefining document's
    // component_index so chained redefines can find it.
    if let Some(doc_id) = redefining_doc_id {
        if let Some(doc) = schema_set.documents.get_mut(doc_id as usize) {
            doc.component_index.insert(
                ComponentIdentity { kind: ComponentKind::SimpleType, name, namespace },
                ComponentKey::Type(TypeKey::Simple(new_key)),
            );
        }
    }

    record_provenance(
        &mut schema_set.effective_components,
        ComponentKey::Type(TypeKey::Simple(new_key)),
        ComponentKind::SimpleType, namespace, name, redefining_doc_id,
        redefined_action(redefining_doc_id, ComponentKind::SimpleType, name, namespace, target_doc_id),
    );

    Ok(())
}

/// Apply a complex type redefinition.
fn apply_complex_type_redefine(
    schema_set: &mut SchemaSet,
    new_key: ComplexTypeKey,
    target_doc_id: Option<DocumentId>,
    redefining_doc_id: Option<DocumentId>,
) -> SchemaResult<()> {
    let new_type = schema_set
        .arenas
        .complex_types
        .get(new_key)
        .ok_or_else(|| SchemaError::internal("Redefine: new complex type not found"))?;

    let name = new_type.name.ok_or_else(|| {
        SchemaError::structural(
            "src-redefine",
            "Redefined complex type must have a name",
            None,
        )
    })?;
    let namespace = new_type.target_namespace;

    let original_key = match target_doc_id {
        Some(id) => schema_set
            .documents
            .get(id as usize)
            .and_then(|doc| doc.component_index.lookup_complex_type(namespace, name))
            .map(TypeKey::Complex),
        None => schema_set.lookup_type(namespace, name),
    }
    .ok_or_else(|| {
        SchemaError::structural(
            "src-redefine",
            format!(
                "Original complex type '{}' not found for redefinition in {}",
                schema_set.name_table.resolve(name),
                target_doc_id
                    .and_then(|id| schema_set.documents.get(id as usize))
                    .map(|d| d.base_uri.as_str())
                    .unwrap_or("schema"),
            ),
            None,
        )
    })?;

    validate_self_derivation_complex(schema_set, new_key, name, namespace)?;

    // Store original type key for base-type redirection during resolution
    if let TypeKey::Complex(orig_key) = original_key {
        if let Some(ct) = schema_set.arenas.complex_types.get_mut(new_key) {
            ct.redefine_original = Some(orig_key);
        }
    }

    let ns_table = schema_set.get_or_create_namespace(namespace);
    ns_table.register_type(name, TypeKey::Complex(new_key));

    if let Some(doc_id) = redefining_doc_id {
        if let Some(doc) = schema_set.documents.get_mut(doc_id as usize) {
            doc.component_index.insert(
                ComponentIdentity { kind: ComponentKind::ComplexType, name, namespace },
                ComponentKey::Type(TypeKey::Complex(new_key)),
            );
        }
    }

    record_provenance(
        &mut schema_set.effective_components,
        ComponentKey::Type(TypeKey::Complex(new_key)),
        ComponentKind::ComplexType, namespace, name, redefining_doc_id,
        redefined_action(redefining_doc_id, ComponentKind::ComplexType, name, namespace, target_doc_id),
    );

    Ok(())
}

/// Apply a model group redefinition.
fn apply_model_group_redefine(
    schema_set: &mut SchemaSet,
    new_key: ModelGroupKey,
    target_doc_id: Option<DocumentId>,
    redefining_doc_id: Option<DocumentId>,
) -> SchemaResult<()> {
    let new_group = schema_set
        .arenas
        .model_groups
        .get(new_key)
        .ok_or_else(|| SchemaError::internal("Redefine: new model group not found"))?;

    let name = new_group.name.ok_or_else(|| {
        SchemaError::structural(
            "src-redefine",
            "Redefined model group must have a name",
            None,
        )
    })?;
    let namespace = new_group.target_namespace;

    let original_key = match target_doc_id {
        Some(id) => schema_set
            .documents
            .get(id as usize)
            .and_then(|doc| doc.component_index.lookup_model_group(namespace, name)),
        None => schema_set.lookup_model_group(namespace, name),
    }
    .ok_or_else(|| {
        SchemaError::structural(
            "src-redefine",
            format!(
                "Original group '{}' not found for redefinition in {}",
                schema_set.name_table.resolve(name),
                target_doc_id
                    .and_then(|id| schema_set.documents.get(id as usize))
                    .map(|d| d.base_uri.as_str())
                    .unwrap_or("schema"),
            ),
            None,
        )
    })?;

    let has_self_ref = validate_self_reference_group(schema_set, new_key, name)?;

    // Store original key so self-references can be redirected during resolution.
    // When the redefine has zero self-references, flag it for the deferred
    // §src-redefine 6.2.2 restriction check in `validate_all_derivations`.
    if let Some(group) = schema_set.arenas.model_groups.get_mut(new_key) {
        group.redefine_original = Some(original_key);
        group.redefine_requires_restriction_check = !has_self_ref;
    }

    let ns_table = schema_set.get_or_create_namespace(namespace);
    ns_table.register_model_group(name, new_key);

    if let Some(doc_id) = redefining_doc_id {
        if let Some(doc) = schema_set.documents.get_mut(doc_id as usize) {
            doc.component_index.insert(
                ComponentIdentity { kind: ComponentKind::ModelGroup, name, namespace },
                ComponentKey::ModelGroup(new_key),
            );
        }
    }

    record_provenance(
        &mut schema_set.effective_components,
        ComponentKey::ModelGroup(new_key),
        ComponentKind::ModelGroup, namespace, name, redefining_doc_id,
        redefined_action(redefining_doc_id, ComponentKind::ModelGroup, name, namespace, target_doc_id),
    );

    Ok(())
}

/// Apply an attribute group redefinition.
fn apply_attribute_group_redefine(
    schema_set: &mut SchemaSet,
    new_key: AttributeGroupKey,
    target_doc_id: Option<DocumentId>,
    redefining_doc_id: Option<DocumentId>,
) -> SchemaResult<()> {
    let new_group = schema_set
        .arenas
        .attribute_groups
        .get(new_key)
        .ok_or_else(|| SchemaError::internal("Redefine: new attribute group not found"))?;

    let name = new_group.name.ok_or_else(|| {
        SchemaError::structural(
            "src-redefine",
            "Redefined attribute group must have a name",
            None,
        )
    })?;
    let namespace = new_group.target_namespace;

    let original_key = match target_doc_id {
        Some(id) => schema_set
            .documents
            .get(id as usize)
            .and_then(|doc| doc.component_index.lookup_attribute_group(namespace, name)),
        None => schema_set.lookup_attribute_group(namespace, name),
    }
    .ok_or_else(|| {
        SchemaError::structural(
            "src-redefine",
            format!(
                "Original attribute group '{}' not found for redefinition in {}",
                schema_set.name_table.resolve(name),
                target_doc_id
                    .and_then(|id| schema_set.documents.get(id as usize))
                    .map(|d| d.base_uri.as_str())
                    .unwrap_or("schema"),
            ),
            None,
        )
    })?;

    let has_self_ref = validate_self_reference_attribute_group(schema_set, new_key, name)?;

    // Store original key so self-references can be redirected during resolution.
    // When the redefine has zero self-references, flag it for the deferred
    // §src-redefine 7.2.2 restriction check in `validate_all_derivations`.
    if let Some(group) = schema_set.arenas.attribute_groups.get_mut(new_key) {
        group.redefine_original = Some(original_key);
        group.redefine_requires_restriction_check = !has_self_ref;
    }

    let ns_table = schema_set.get_or_create_namespace(namespace);
    ns_table.register_attribute_group(name, new_key);

    if let Some(doc_id) = redefining_doc_id {
        if let Some(doc) = schema_set.documents.get_mut(doc_id as usize) {
            doc.component_index.insert(
                ComponentIdentity { kind: ComponentKind::AttributeGroup, name, namespace },
                ComponentKey::AttributeGroup(new_key),
            );
        }
    }

    record_provenance(
        &mut schema_set.effective_components,
        ComponentKey::AttributeGroup(new_key),
        ComponentKind::AttributeGroup, namespace, name, redefining_doc_id,
        redefined_action(redefining_doc_id, ComponentKind::AttributeGroup, name, namespace, target_doc_id),
    );

    Ok(())
}

/// Validate that a simple type redefines itself (self-derivation constraint).
fn validate_self_derivation_simple(
    schema_set: &SchemaSet,
    type_key: SimpleTypeKey,
    expected_name: NameId,
    expected_namespace: Option<NameId>,
) -> SchemaResult<()> {
    use crate::parser::frames::TypeRefResult;

    let type_def = schema_set
        .arenas
        .simple_types
        .get(type_key)
        .ok_or_else(|| SchemaError::internal("Type not found"))?;

    // Check that base_type references the same name and namespace (self-reference).
    // Unprefixed QNames (namespace == None) are accepted — they resolve to the
    // target namespace during the reference-resolution phase.
    if let Some(TypeRefResult::QName(ref qname)) = type_def.base_type {
        if qname.local_name != expected_name {
            return Err(SchemaError::structural(
                "src-redefine",
                "Redefined simple type must derive from the original type (self-reference)",
                type_def
                    .source
                    .as_ref()
                    .and_then(|s| schema_set.source_maps.locate(s)),
            ));
        }
        // If the reference is explicitly namespace-qualified, it must match
        if let Some(ref_ns) = qname.namespace {
            if Some(ref_ns) != expected_namespace {
                return Err(SchemaError::structural(
                    "src-redefine",
                    "Redefined simple type base references a different namespace than the original",
                    type_def
                        .source
                        .as_ref()
                        .and_then(|s| schema_set.source_maps.locate(s)),
                ));
            }
        }
    } else {
        return Err(SchemaError::structural(
            "src-redefine",
            "Redefined simple type must have a base type reference",
            type_def
                .source
                .as_ref()
                .and_then(|s| schema_set.source_maps.locate(s)),
        ));
    }

    Ok(())
}

/// Validate that a complex type redefines itself (self-derivation constraint).
fn validate_self_derivation_complex(
    schema_set: &SchemaSet,
    type_key: ComplexTypeKey,
    expected_name: NameId,
    expected_namespace: Option<NameId>,
) -> SchemaResult<()> {
    use crate::parser::frames::TypeRefResult;

    let type_def = schema_set
        .arenas
        .complex_types
        .get(type_key)
        .ok_or_else(|| SchemaError::internal("Type not found"))?;

    // Check that base_type references the same name and namespace (self-reference).
    // Unprefixed QNames (namespace == None) are accepted — they resolve to the
    // target namespace during the reference-resolution phase.
    if let Some(TypeRefResult::QName(ref qname)) = type_def.base_type {
        if qname.local_name != expected_name {
            return Err(SchemaError::structural(
                "src-redefine",
                "Redefined complex type must derive from the original type (self-reference)",
                type_def
                    .source
                    .as_ref()
                    .and_then(|s| schema_set.source_maps.locate(s)),
            ));
        }
        // If the reference is explicitly namespace-qualified, it must match
        if let Some(ref_ns) = qname.namespace {
            if Some(ref_ns) != expected_namespace {
                return Err(SchemaError::structural(
                    "src-redefine",
                    "Redefined complex type base references a different namespace than the original",
                    type_def
                        .source
                        .as_ref()
                        .and_then(|s| schema_set.source_maps.locate(s)),
                ));
            }
        }
    } else {
        return Err(SchemaError::structural(
            "src-redefine",
            "Redefined complex type must have a base type reference",
            type_def
                .source
                .as_ref()
                .and_then(|s| schema_set.source_maps.locate(s)),
        ));
    }

    Ok(())
}

/// Validate that a model group's self-reference structure is consistent with
/// §src-redefine clause 6.
///
/// Returns `Ok(true)` when the redefine contains exactly one well-formed
/// self-reference (§6.1), `Ok(false)` when the redefine contains zero
/// self-references and the caller must schedule a deferred §6.2.2
/// restriction check. Returns an error for >1 self-refs or a self-ref with
/// `min/maxOccurs != 1` (violations of §6.1.1 / §6.1.2).
fn validate_self_reference_group(
    schema_set: &SchemaSet,
    group_key: ModelGroupKey,
    expected_name: NameId,
) -> SchemaResult<bool> {
    let group = schema_set
        .arenas
        .model_groups
        .get(group_key)
        .ok_or_else(|| SchemaError::internal("Group not found"))?;

    let mut self_refs = 0u32;
    let mut self_ref_min_occurs = 0u32;
    let mut self_ref_max_occurs: Option<u32> = None;
    for particle in &group.particles {
        if let crate::parser::frames::ParticleTerm::Group(ref grp) = particle.term {
            if let Some(ref ref_name) = grp.ref_name {
                if ref_name.local_name == expected_name {
                    self_refs += 1;
                    self_ref_min_occurs = particle.min_occurs;
                    self_ref_max_occurs = particle.max_occurs;
                }
            }
        }
    }

    match self_refs {
        // Clause 6.2 (src-redefine §6.2): the redefining group has no
        // self-reference. Clause 6.2.1 — that the name resolves in S2 — is
        // already enforced by the caller's component lookup against the
        // target document. Clause 6.2.2 — that the new model is a valid
        // restriction of the original — is enforced later by the deferred
        // `validate_all_redefine_group_restrictions` pass once reference
        // resolution is complete. Return `false` so the caller sets the
        // arena flag that schedules that pass.
        0 => Ok(false),
        // Clause 6.1: self-reference present.
        // Clause 6.1.2: minOccurs and maxOccurs must both be 1.
        1 => {
            if self_ref_min_occurs != 1 || self_ref_max_occurs != Some(1) {
                Err(SchemaError::structural(
                    "src-redefine",
                    format!(
                        "Self-referencing group particle must have \
                         minOccurs=1 and maxOccurs=1, but found \
                         minOccurs={} maxOccurs={}",
                        self_ref_min_occurs,
                        self_ref_max_occurs
                            .map(|v| v.to_string())
                            .unwrap_or_else(|| "unbounded".to_string()),
                    ),
                    group
                        .source
                        .as_ref()
                        .and_then(|s| schema_set.source_maps.locate(s)),
                ))
            } else {
                Ok(true)
            }
        }
        _ => Err(SchemaError::structural(
            "src-redefine",
            format!(
                "Redefined group must contain at most one \
                 self-reference (found {})",
                self_refs,
            ),
            group
                .source
                .as_ref()
                .and_then(|s| schema_set.source_maps.locate(s)),
        )),
    }
}

/// Validate self-reference constraints for an attribute group redefine.
///
/// Clause 7.1: with self-reference — exactly one is allowed.
/// Clause 7.2: without self-reference — restriction of original (checked
/// later by `validate_all_redefine_attribute_group_restrictions`).
///
/// Returns `Ok(true)` when the redefine contains exactly one self-reference
/// (§7.1), `Ok(false)` when the redefine contains zero self-references and
/// the caller must schedule a deferred §7.2.2 restriction check. Errors
/// for >1 self-refs.
fn validate_self_reference_attribute_group(
    schema_set: &SchemaSet,
    group_key: AttributeGroupKey,
    expected_name: NameId,
) -> SchemaResult<bool> {
    let group = schema_set
        .arenas
        .attribute_groups
        .get(group_key)
        .ok_or_else(|| SchemaError::internal("Attribute group not found"))?;

    let mut self_refs = 0u32;
    for attr_group_ref in &group.attribute_groups {
        if attr_group_ref.local_name == expected_name {
            self_refs += 1;
        }
    }

    match self_refs {
        // Clause 7.2 (src-redefine §7.2): no self-reference. Clause 7.2.1
        // is enforced by the caller's component lookup; clause 7.2.2 (the
        // restriction check) is deferred to
        // `validate_all_redefine_attribute_group_restrictions` which runs
        // after reference resolution. Return `false` so the caller sets
        // the arena flag that schedules that pass.
        0 => Ok(false),
        // Clause 7.1: exactly one self-reference. Valid.
        1 => Ok(true),
        _ => Err(SchemaError::structural(
            "src-redefine",
            format!(
                "Redefined attribute group must contain at most one \
                 self-reference (found {})",
                self_refs,
            ),
            group
                .source
                .as_ref()
                .and_then(|s| schema_set.source_maps.locate(s)),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arenas::{AttributeGroupData, ModelGroupData};
    use crate::parser::frames::{
        Compositor, ElementFrameResult, ModelGroupDefResult, ParticleResult, ParticleTerm, QNameRef,
    };
    use crate::schema::composition::ComponentKind;
    use crate::schema::model::{DerivationSet, SchemaDocument};

    /// Helper: set up a schema set with a base document and a named model group.
    fn setup_model_group_redefine() -> (SchemaSet, ModelGroupKey, ModelGroupKey) {
        let mut schema_set = SchemaSet::new();

        // Create base document
        let base_doc_id = schema_set.documents.len() as u32;
        let base_doc = SchemaDocument::new(base_doc_id, "base.xsd".to_string());
        schema_set.documents.push(base_doc);

        let group_name = schema_set.name_table.add("personGroup");

        // Create original group with element "name"
        let name_elem = schema_set.name_table.add("name");
        let original_data = ModelGroupData {
            name: Some(group_name),
            target_namespace: None,
            ref_name: None,
            compositor: Some(Compositor::Sequence),
            particles: vec![ParticleResult {
                term: ParticleTerm::Element(ElementFrameResult {
                    name: Some(name_elem),
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
                    block: DerivationSet::empty(),
                    final_derivation: DerivationSet::empty(),
                    form: None,
                    id: None,
                    alternatives: vec![],
                    identity_constraints: vec![],
                    identity_constraint_refs: vec![],
                    annotation: None,
                    source: None,
                }),
                min_occurs: 1,
                max_occurs: Some(1),
                source: None,
            }],
            min_occurs: 1,
            max_occurs: Some(1),
            id: None,
            annotation: None,
            source: None,
            resolved_ref: None,
            resolved_particles: Vec::new(),
            resolved_particle_types: Vec::new(),
            resolved_particle_elements: Vec::new(),
            redefine_original: None,
            redefine_requires_restriction_check: false,
        };
        let original_key = schema_set.arenas.alloc_model_group(original_data);
        schema_set
            .get_or_create_namespace(None)
            .register_model_group(group_name, original_key);

        // Create redefining group with self-ref + element "age"
        let age_elem = schema_set.name_table.add("age");
        let new_data = ModelGroupData {
            name: Some(group_name),
            target_namespace: None,
            ref_name: None,
            compositor: Some(Compositor::Sequence),
            particles: vec![
                // Self-reference
                ParticleResult {
                    term: ParticleTerm::Group(ModelGroupDefResult {
                        name: None,
                        ref_name: Some(QNameRef {
                            prefix: None,
                            local_name: group_name,
                            namespace: None,
                        }),
                        compositor: None,
                        particles: vec![],
                        min_occurs: 1,
                        max_occurs: Some(1),
                        id: None,
                        annotation: None,
                        source: None,
                    }),
                    min_occurs: 1,
                    max_occurs: Some(1),
                    source: None,
                },
                // New element
                ParticleResult {
                    term: ParticleTerm::Element(ElementFrameResult {
                        name: Some(age_elem),
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
                        block: DerivationSet::empty(),
                        final_derivation: DerivationSet::empty(),
                        form: None,
                        id: None,
                        alternatives: vec![],
                        identity_constraints: vec![],
                        identity_constraint_refs: vec![],
                        annotation: None,
                        source: None,
                    }),
                    min_occurs: 1,
                    max_occurs: Some(1),
                    source: None,
                },
            ],
            min_occurs: 1,
            max_occurs: Some(1),
            id: None,
            annotation: None,
            source: None,
            resolved_ref: None,
            resolved_particles: Vec::new(),
            resolved_particle_types: Vec::new(),
            resolved_particle_elements: Vec::new(),
            redefine_original: None,
            redefine_requires_restriction_check: false,
        };
        let new_key = schema_set.arenas.alloc_model_group(new_data);

        (schema_set, original_key, new_key)
    }

    #[test]
    fn test_redefine_model_group_self_reference() {
        let (mut schema_set, original_key, new_key) = setup_model_group_redefine();

        // Apply redefine (no target doc — fallback to global lookup)
        let result =
            apply_model_group_redefine(&mut schema_set, new_key, None, None);
        assert!(result.is_ok(), "apply_model_group_redefine failed: {:?}", result.err());

        // Verify redefine_original is set
        let group = schema_set.arenas.model_groups.get(new_key).unwrap();
        assert_eq!(
            group.redefine_original,
            Some(original_key),
            "redefine_original should point to the original group"
        );

        // Now resolve references and verify self-ref redirects to original
        let result = crate::schema::resolver::resolve_all_references(&mut schema_set);
        assert!(result.is_ok(), "resolve_all_references failed: {:?}", result.err());

        let group = schema_set.arenas.model_groups.get(new_key).unwrap();
        // First particle is the group ref (self-reference → should resolve to original)
        match &group.resolved_particles[0] {
            crate::arenas::ResolvedParticleTerm::Group {
                resolved_ref: Some(key),
            } => {
                assert_eq!(
                    *key, original_key,
                    "Self-reference should resolve to the original group, not the new one"
                );
            }
            other => panic!("Expected Group particle with resolved_ref, got {:?}", other),
        }
    }

    #[test]
    fn test_redefine_attribute_group_self_reference() {
        let mut schema_set = SchemaSet::new();

        let group_name = schema_set.name_table.add("commonAttrs");

        // Create original attribute group
        let original_data = AttributeGroupData {
            name: Some(group_name),
            target_namespace: None,
            ref_name: None,
            attributes: Vec::new(),
            attribute_groups: Vec::new(),
            attribute_wildcard: None,
            id: None,
            annotation: None,
            source: None,
            resolved_ref: None,
            resolved_attribute_groups: Vec::new(),
            resolved_attributes: Vec::new(),
            redefine_original: None,
            redefine_requires_restriction_check: false,
        };
        let original_key = schema_set.arenas.alloc_attribute_group(original_data);
        schema_set
            .get_or_create_namespace(None)
            .register_attribute_group(group_name, original_key);

        // Create redefining group with self-reference
        let new_data = AttributeGroupData {
            name: Some(group_name),
            target_namespace: None,
            ref_name: None,
            attributes: Vec::new(),
            attribute_groups: vec![QNameRef {
                prefix: None,
                local_name: group_name,
                namespace: None,
            }],
            attribute_wildcard: None,
            id: None,
            annotation: None,
            source: None,
            resolved_ref: None,
            resolved_attribute_groups: Vec::new(),
            resolved_attributes: Vec::new(),
            redefine_original: None,
            redefine_requires_restriction_check: false,
        };
        let new_key = schema_set.arenas.alloc_attribute_group(new_data);

        // Apply redefine
        let result =
            apply_attribute_group_redefine(&mut schema_set, new_key, None, None);
        assert!(result.is_ok(), "apply_attribute_group_redefine failed: {:?}", result.err());

        // Verify redefine_original
        let group = schema_set.arenas.attribute_groups.get(new_key).unwrap();
        assert_eq!(group.redefine_original, Some(original_key));

        // Resolve references
        let result = crate::schema::resolver::resolve_all_references(&mut schema_set);
        assert!(result.is_ok(), "resolve_all_references failed: {:?}", result.err());

        // Verify self-ref redirected to original
        let group = schema_set.arenas.attribute_groups.get(new_key).unwrap();
        assert_eq!(group.resolved_attribute_groups.len(), 1);
        assert_eq!(
            group.resolved_attribute_groups[0], original_key,
            "Self-reference should resolve to the original attribute group"
        );
    }

    #[test]
    fn test_provenance_note_redefined() {
        let mut schema_set = SchemaSet::new();

        // Create two documents for provenance tracking
        let base_doc_id = schema_set.documents.len() as u32;
        let base_doc = SchemaDocument::new(base_doc_id, "base.xsd".to_string());
        schema_set.documents.push(base_doc);

        let redefining_doc_id = schema_set.documents.len() as u32;
        let redefining_doc = SchemaDocument::new(redefining_doc_id, "main.xsd".to_string());
        schema_set.documents.push(redefining_doc);

        let group_name = schema_set.name_table.add("testGroup");

        // Record provenance as if a redefine occurred
        crate::schema::composition::record_provenance(
            &mut schema_set.effective_components,
            crate::schema::composition::ComponentKey::ModelGroup(
                // dummy key (not used for lookup)
                schema_set.arenas.alloc_model_group(ModelGroupData {
                    name: Some(group_name),
                    target_namespace: None,
                    ref_name: None,
                    compositor: Some(Compositor::Sequence),
                    particles: Vec::new(),
                    min_occurs: 1,
                    max_occurs: Some(1),
                    id: None,
                    annotation: None,
                    source: None,
                    resolved_ref: None,
                    resolved_particles: Vec::new(),
                    resolved_particle_types: Vec::new(),
                    resolved_particle_elements: Vec::new(),
                    redefine_original: None,
                    redefine_requires_restriction_check: false,
                }),
            ),
            ComponentKind::ModelGroup,
            None,
            group_name,
            Some(redefining_doc_id),
            crate::schema::composition::redefined_action(
                Some(redefining_doc_id),
                ComponentKind::ModelGroup,
                group_name,
                None,
                Some(base_doc_id),
            ),
        );

        let note = schema_set.format_provenance_note(ComponentKind::ModelGroup, None, group_name);
        assert!(
            note.contains("base.xsd"),
            "Provenance note should mention the original document: {}",
            note
        );
        assert!(
            note.contains("main.xsd"),
            "Provenance note should mention the redefining document: {}",
            note
        );
        assert!(
            note.contains("redefined"),
            "Provenance note should mention 'redefined': {}",
            note
        );
    }

    #[test]
    fn test_provenance_note_declared() {
        let schema_set = SchemaSet::new();
        let name = schema_set.name_table.get("string").unwrap();

        // No provenance recorded → empty string
        let note = schema_set.format_provenance_note(ComponentKind::SimpleType, None, name);
        assert!(
            note.is_empty(),
            "Provenance note for undeclared component should be empty, got: {}",
            note
        );
    }

    #[test]
    fn test_redefine_model_group_no_self_reference() {
        // Clause 6.2: a redefining group with NO self-reference is valid
        // (pure restriction of the original).
        let (mut schema_set, _original_key, _new_key) = setup_model_group_redefine();

        let group_name = schema_set.name_table.add("personGroup");
        let age_elem = schema_set.name_table.add("age");

        // Create redefining group WITHOUT self-reference (just element "age")
        let new_data = ModelGroupData {
            name: Some(group_name),
            target_namespace: None,
            ref_name: None,
            compositor: Some(Compositor::Sequence),
            particles: vec![ParticleResult {
                term: ParticleTerm::Element(ElementFrameResult {
                    name: Some(age_elem),
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
                    block: DerivationSet::empty(),
                    final_derivation: DerivationSet::empty(),
                    form: None,
                    id: None,
                    alternatives: vec![],
                    identity_constraints: vec![],
                    identity_constraint_refs: vec![],
                    annotation: None,
                    source: None,
                }),
                min_occurs: 1,
                max_occurs: Some(1),
                source: None,
            }],
            min_occurs: 1,
            max_occurs: Some(1),
            id: None,
            annotation: None,
            source: None,
            resolved_ref: None,
            resolved_particles: Vec::new(),
            resolved_particle_types: Vec::new(),
            resolved_particle_elements: Vec::new(),
            redefine_original: None,
            redefine_requires_restriction_check: false,
        };
        let no_selfref_key = schema_set.arenas.alloc_model_group(new_data);

        let result = apply_model_group_redefine(&mut schema_set, no_selfref_key, None, None);
        assert!(
            result.is_ok(),
            "Group redefine without self-reference should succeed (clause 6.2): {:?}",
            result.err()
        );
    }

    #[test]
    fn test_redefine_model_group_self_ref_wrong_min_occurs() {
        // Clause 6.1.2: self-ref with minOccurs=0 must be rejected.
        let (mut schema_set, _original_key, _new_key) = setup_model_group_redefine();

        let group_name = schema_set.name_table.add("personGroup");
        let age_elem = schema_set.name_table.add("age");

        let new_data = ModelGroupData {
            name: Some(group_name),
            target_namespace: None,
            ref_name: None,
            compositor: Some(Compositor::Sequence),
            particles: vec![
                ParticleResult {
                    term: ParticleTerm::Group(ModelGroupDefResult {
                        name: None,
                        ref_name: Some(QNameRef {
                            prefix: None,
                            local_name: group_name,
                            namespace: None,
                        }),
                        compositor: None,
                        particles: vec![],
                        min_occurs: 1,
                        max_occurs: Some(1),
                        id: None,
                        annotation: None,
                        source: None,
                    }),
                    min_occurs: 0, // ← violates clause 6.1.2
                    max_occurs: Some(1),
                    source: None,
                },
                ParticleResult {
                    term: ParticleTerm::Element(ElementFrameResult {
                        name: Some(age_elem),
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
                        block: DerivationSet::empty(),
                        final_derivation: DerivationSet::empty(),
                        form: None,
                        id: None,
                        alternatives: vec![],
                        identity_constraints: vec![],
                        identity_constraint_refs: vec![],
                        annotation: None,
                        source: None,
                    }),
                    min_occurs: 1,
                    max_occurs: Some(1),
                    source: None,
                },
            ],
            min_occurs: 1,
            max_occurs: Some(1),
            id: None,
            annotation: None,
            source: None,
            resolved_ref: None,
            resolved_particles: Vec::new(),
            resolved_particle_types: Vec::new(),
            resolved_particle_elements: Vec::new(),
            redefine_original: None,
            redefine_requires_restriction_check: false,
        };
        let bad_key = schema_set.arenas.alloc_model_group(new_data);

        let result = apply_model_group_redefine(&mut schema_set, bad_key, None, None);
        assert!(
            result.is_err(),
            "Self-ref with minOccurs=0 should fail (clause 6.1.2)"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("minOccurs"),
            "Error should mention minOccurs: {}",
            msg
        );
    }

    #[test]
    fn test_redefine_model_group_self_ref_wrong_max_occurs() {
        // Clause 6.1.2: self-ref with maxOccurs=unbounded must be rejected.
        let (mut schema_set, _original_key, _new_key) = setup_model_group_redefine();

        let group_name = schema_set.name_table.add("personGroup");

        let new_data = ModelGroupData {
            name: Some(group_name),
            target_namespace: None,
            ref_name: None,
            compositor: Some(Compositor::Sequence),
            particles: vec![ParticleResult {
                term: ParticleTerm::Group(ModelGroupDefResult {
                    name: None,
                    ref_name: Some(QNameRef {
                        prefix: None,
                        local_name: group_name,
                        namespace: None,
                    }),
                    compositor: None,
                    particles: vec![],
                    min_occurs: 1,
                    max_occurs: Some(1),
                    id: None,
                    annotation: None,
                    source: None,
                }),
                min_occurs: 1,
                max_occurs: None, // ← unbounded, violates clause 6.1.2
                source: None,
            }],
            min_occurs: 1,
            max_occurs: Some(1),
            id: None,
            annotation: None,
            source: None,
            resolved_ref: None,
            resolved_particles: Vec::new(),
            resolved_particle_types: Vec::new(),
            resolved_particle_elements: Vec::new(),
            redefine_original: None,
            redefine_requires_restriction_check: false,
        };
        let bad_key = schema_set.arenas.alloc_model_group(new_data);

        let result = apply_model_group_redefine(&mut schema_set, bad_key, None, None);
        assert!(
            result.is_err(),
            "Self-ref with maxOccurs=unbounded should fail (clause 6.1.2)"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("maxOccurs"),
            "Error should mention maxOccurs: {}",
            msg
        );
    }

    #[test]
    fn test_redefine_attribute_group_no_self_reference() {
        // Clause 7.2: attribute group redefine with no self-reference is valid.
        let mut schema_set = SchemaSet::new();

        let group_name = schema_set.name_table.add("commonAttrs");

        let original_data = AttributeGroupData {
            name: Some(group_name),
            target_namespace: None,
            ref_name: None,
            attributes: Vec::new(),
            attribute_groups: Vec::new(),
            attribute_wildcard: None,
            id: None,
            annotation: None,
            source: None,
            resolved_ref: None,
            resolved_attribute_groups: Vec::new(),
            resolved_attributes: Vec::new(),
            redefine_original: None,
            redefine_requires_restriction_check: false,
        };
        let _original_key = schema_set.arenas.alloc_attribute_group(original_data);
        schema_set
            .get_or_create_namespace(None)
            .register_attribute_group(group_name, _original_key);

        // Redefining group with NO self-reference
        let new_data = AttributeGroupData {
            name: Some(group_name),
            target_namespace: None,
            ref_name: None,
            attributes: Vec::new(),
            attribute_groups: Vec::new(), // empty — no self-ref
            attribute_wildcard: None,
            id: None,
            annotation: None,
            source: None,
            resolved_ref: None,
            resolved_attribute_groups: Vec::new(),
            resolved_attributes: Vec::new(),
            redefine_original: None,
            redefine_requires_restriction_check: false,
        };
        let new_key = schema_set.arenas.alloc_attribute_group(new_data);

        let result = apply_attribute_group_redefine(&mut schema_set, new_key, None, None);
        assert!(
            result.is_ok(),
            "Attribute group redefine without self-reference should succeed (clause 7.2): {:?}",
            result.err()
        );
    }
}
