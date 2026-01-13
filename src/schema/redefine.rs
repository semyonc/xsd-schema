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
use crate::schema::model::RedefineDirective;
use crate::schema::SchemaSet;

/// Apply a redefine directive to the schema set.
///
/// This replaces the original components with the redefined versions,
/// after validating the redefinition constraints.
pub fn apply_redefine(
    schema_set: &mut SchemaSet,
    redefine: &RedefineDirective,
) -> SchemaResult<()> {
    // Process simple type redefinitions
    for simple_key in &redefine.simple_types {
        apply_simple_type_redefine(schema_set, *simple_key)?;
    }

    // Process complex type redefinitions
    for complex_key in &redefine.complex_types {
        apply_complex_type_redefine(schema_set, *complex_key)?;
    }

    // Process model group redefinitions
    for group_key in &redefine.groups {
        apply_model_group_redefine(schema_set, *group_key)?;
    }

    // Process attribute group redefinitions
    for attr_group_key in &redefine.attribute_groups {
        apply_attribute_group_redefine(schema_set, *attr_group_key)?;
    }

    Ok(())
}

/// Apply a simple type redefinition.
fn apply_simple_type_redefine(
    schema_set: &mut SchemaSet,
    new_key: SimpleTypeKey,
) -> SchemaResult<()> {
    let new_type = schema_set
        .arenas
        .simple_types
        .get(new_key)
        .ok_or_else(|| SchemaError::internal("Redefine: new simple type not found"))?;

    let name = new_type.name.ok_or_else(|| {
        SchemaError::structural(
            "sch-redefine",
            "Redefined simple type must have a name",
            None,
        )
    })?;
    let namespace = new_type.target_namespace;

    // Find the original type
    let original_key = schema_set.lookup_type(namespace, name).ok_or_else(|| {
        SchemaError::structural(
            "sch-redefine",
            format!(
                "Original type '{}' not found for redefinition",
                schema_set.name_table.resolve(name)
            ),
            None,
        )
    })?;

    // Validate: the new type must derive from the original (self-reference)
    validate_self_derivation_simple(schema_set, new_key, name)?;

    // Replace in namespace table
    let ns_table = schema_set.get_or_create_namespace(namespace);
    let _ = original_key; // Suppress unused warning - we've validated it exists
    ns_table.register_type(name, TypeKey::Simple(new_key));

    Ok(())
}

/// Apply a complex type redefinition.
fn apply_complex_type_redefine(
    schema_set: &mut SchemaSet,
    new_key: ComplexTypeKey,
) -> SchemaResult<()> {
    let new_type = schema_set
        .arenas
        .complex_types
        .get(new_key)
        .ok_or_else(|| SchemaError::internal("Redefine: new complex type not found"))?;

    let name = new_type.name.ok_or_else(|| {
        SchemaError::structural(
            "sch-redefine",
            "Redefined complex type must have a name",
            None,
        )
    })?;
    let namespace = new_type.target_namespace;

    // Find and validate original exists
    let _original_key = schema_set.lookup_type(namespace, name).ok_or_else(|| {
        SchemaError::structural(
            "sch-redefine",
            format!(
                "Original type '{}' not found for redefinition",
                schema_set.name_table.resolve(name)
            ),
            None,
        )
    })?;

    // Validate: the new type must derive from the original (self-reference)
    validate_self_derivation_complex(schema_set, new_key, name)?;

    // Replace in namespace table
    let ns_table = schema_set.get_or_create_namespace(namespace);
    ns_table.register_type(name, TypeKey::Complex(new_key));

    Ok(())
}

/// Apply a model group redefinition.
fn apply_model_group_redefine(
    schema_set: &mut SchemaSet,
    new_key: ModelGroupKey,
) -> SchemaResult<()> {
    let new_group = schema_set
        .arenas
        .model_groups
        .get(new_key)
        .ok_or_else(|| SchemaError::internal("Redefine: new model group not found"))?;

    let name = new_group.name.ok_or_else(|| {
        SchemaError::structural(
            "sch-redefine",
            "Redefined model group must have a name",
            None,
        )
    })?;
    let namespace = new_group.target_namespace;

    // Validate original exists
    let _original_key = schema_set
        .lookup_model_group(namespace, name)
        .ok_or_else(|| {
            SchemaError::structural(
                "sch-redefine",
                format!(
                    "Original group '{}' not found for redefinition",
                    schema_set.name_table.resolve(name)
                ),
                None,
            )
        })?;

    // Validate: group must contain exactly one reference to itself
    validate_self_reference_group(schema_set, new_key, name)?;

    // Replace in namespace table
    let ns_table = schema_set.get_or_create_namespace(namespace);
    ns_table.register_model_group(name, new_key);

    Ok(())
}

/// Apply an attribute group redefinition.
fn apply_attribute_group_redefine(
    schema_set: &mut SchemaSet,
    new_key: AttributeGroupKey,
) -> SchemaResult<()> {
    let new_group = schema_set
        .arenas
        .attribute_groups
        .get(new_key)
        .ok_or_else(|| SchemaError::internal("Redefine: new attribute group not found"))?;

    let name = new_group.name.ok_or_else(|| {
        SchemaError::structural(
            "sch-redefine",
            "Redefined attribute group must have a name",
            None,
        )
    })?;
    let namespace = new_group.target_namespace;

    // Validate original exists
    let _original_key = schema_set
        .lookup_attribute_group(namespace, name)
        .ok_or_else(|| {
            SchemaError::structural(
                "sch-redefine",
                format!(
                    "Original attribute group '{}' not found for redefinition",
                    schema_set.name_table.resolve(name)
                ),
                None,
            )
        })?;

    // Validate: attribute group must contain exactly one reference to itself
    validate_self_reference_attribute_group(schema_set, new_key, name)?;

    // Replace in namespace table
    let ns_table = schema_set.get_or_create_namespace(namespace);
    ns_table.register_attribute_group(name, new_key);

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
                "sch-redefine",
                "Redefined simple type must derive from the original type (self-reference)",
                type_def
                    .source
                    .as_ref()
                    .and_then(|s| schema_set.source_maps.locate(s)),
            ));
        }
    } else {
        return Err(SchemaError::structural(
            "sch-redefine",
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
                "sch-redefine",
                "Redefined complex type must derive from the original type (self-reference)",
                type_def
                    .source
                    .as_ref()
                    .and_then(|s| schema_set.source_maps.locate(s)),
            ));
        }
    } else {
        return Err(SchemaError::structural(
            "sch-redefine",
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
            "sch-redefine",
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
            "sch-redefine",
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
