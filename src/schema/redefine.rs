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
    ComponentKey, ComponentKind, record_provenance, redefined_action,
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
    let _original_key = match target_doc_id {
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
                "Original simple type '{}' not found for redefinition",
                schema_set.name_table.resolve(name)
            ),
            None,
        )
    })?;

    validate_self_derivation_simple(schema_set, new_key, name)?;

    let ns_table = schema_set.get_or_create_namespace(namespace);
    ns_table.register_type(name, TypeKey::Simple(new_key));

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

    let _original_key = match target_doc_id {
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
                "Original complex type '{}' not found for redefinition",
                schema_set.name_table.resolve(name)
            ),
            None,
        )
    })?;

    validate_self_derivation_complex(schema_set, new_key, name)?;

    let ns_table = schema_set.get_or_create_namespace(namespace);
    ns_table.register_type(name, TypeKey::Complex(new_key));

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

    let _original_key = match target_doc_id {
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
                "Original group '{}' not found for redefinition",
                schema_set.name_table.resolve(name)
            ),
            None,
        )
    })?;

    validate_self_reference_group(schema_set, new_key, name)?;

    let ns_table = schema_set.get_or_create_namespace(namespace);
    ns_table.register_model_group(name, new_key);

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

    let _original_key = match target_doc_id {
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
                "Original attribute group '{}' not found for redefinition",
                schema_set.name_table.resolve(name)
            ),
            None,
        )
    })?;

    validate_self_reference_attribute_group(schema_set, new_key, name)?;

    let ns_table = schema_set.get_or_create_namespace(namespace);
    ns_table.register_attribute_group(name, new_key);

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
) -> SchemaResult<()> {
    use crate::parser::frames::TypeRefResult;

    let type_def = schema_set
        .arenas
        .simple_types
        .get(type_key)
        .ok_or_else(|| SchemaError::internal("Type not found"))?;

    // Check that base_type references the same name (self-reference)
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
) -> SchemaResult<()> {
    use crate::parser::frames::TypeRefResult;

    let type_def = schema_set
        .arenas
        .complex_types
        .get(type_key)
        .ok_or_else(|| SchemaError::internal("Type not found"))?;

    // Check that base_type references the same name (self-reference)
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

/// Validate that a model group contains exactly one self-reference.
fn validate_self_reference_group(
    schema_set: &SchemaSet,
    group_key: ModelGroupKey,
    expected_name: NameId,
) -> SchemaResult<()> {
    let group = schema_set
        .arenas
        .model_groups
        .get(group_key)
        .ok_or_else(|| SchemaError::internal("Group not found"))?;

    let mut self_refs = 0;
    for particle in &group.particles {
        if let crate::parser::frames::ParticleTerm::Group(ref grp) = particle.term {
            if let Some(ref ref_name) = grp.ref_name {
                if ref_name.local_name == expected_name {
                    self_refs += 1;
                }
            }
        }
    }

    if self_refs != 1 {
        return Err(SchemaError::structural(
            "src-redefine",
            format!(
                "Redefined group must contain exactly one self-reference (found {})",
                self_refs
            ),
            group
                .source
                .as_ref()
                .and_then(|s| schema_set.source_maps.locate(s)),
        ));
    }

    Ok(())
}

/// Validate that an attribute group contains exactly one self-reference.
fn validate_self_reference_attribute_group(
    schema_set: &SchemaSet,
    group_key: AttributeGroupKey,
    expected_name: NameId,
) -> SchemaResult<()> {
    let group = schema_set
        .arenas
        .attribute_groups
        .get(group_key)
        .ok_or_else(|| SchemaError::internal("Attribute group not found"))?;

    let mut self_refs = 0;
    for attr_group_ref in &group.attribute_groups {
        if attr_group_ref.local_name == expected_name {
            self_refs += 1;
        }
    }

    if self_refs != 1 {
        return Err(SchemaError::structural(
            "src-redefine",
            format!(
                "Redefined attribute group must contain exactly one self-reference (found {})",
                self_refs
            ),
            group
                .source
                .as_ref()
                .and_then(|s| schema_set.source_maps.locate(s)),
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    // Note: Integration tests should be in the pipeline or builder module
    // as they require full schema parsing and assembly
}
