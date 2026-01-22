//! xs:override processing (XSD 1.1)
//!
//! Override is simpler than redefine - it simply replaces components
//! without requiring self-derivation/self-reference constraints.
//!
//! # XSD 1.1 Semantics
//!
//! The xs:override element allows any schema top-level component to be
//! replaced with a new definition. Unlike xs:redefine:
//! - No self-derivation constraint for types
//! - No self-reference constraint for groups
//! - Supports all schemaTop elements (types, elements, attributes, groups, notations)
//!
//! The overriding component completely replaces the original.

use crate::error::{SchemaError, SchemaResult};
use crate::ids::*;
use crate::schema::model::{OverrideComponent, OverrideDirective};
use crate::schema::SchemaSet;

/// Apply an override directive to the schema set.
///
/// This replaces the original components with the overriding versions.
/// Unlike redefine, override does not require self-derivation.
pub fn apply_override(
    schema_set: &mut SchemaSet,
    override_dir: &OverrideDirective,
) -> SchemaResult<()> {
    for component in &override_dir.components {
        match component {
            OverrideComponent::SimpleType(key) => {
                override_simple_type(schema_set, *key)?;
            }
            OverrideComponent::ComplexType(key) => {
                override_complex_type(schema_set, *key)?;
            }
            OverrideComponent::Group(key) => {
                override_model_group(schema_set, *key)?;
            }
            OverrideComponent::AttributeGroup(key) => {
                override_attribute_group(schema_set, *key)?;
            }
            OverrideComponent::Element(key) => {
                override_element(schema_set, *key)?;
            }
            OverrideComponent::Attribute(key) => {
                override_attribute(schema_set, *key)?;
            }
            OverrideComponent::Notation(key) => {
                override_notation(schema_set, *key)?;
            }
        }
    }
    Ok(())
}

/// Override a simple type.
fn override_simple_type(schema_set: &mut SchemaSet, new_key: SimpleTypeKey) -> SchemaResult<()> {
    let new_type = schema_set
        .arenas
        .simple_types
        .get(new_key)
        .ok_or_else(|| SchemaError::internal("Override: simple type not found"))?;

    let name = new_type.name.ok_or_else(|| {
        SchemaError::structural(
            "src-override",
            "Overriding simple type must have a name",
            None,
        )
    })?;
    let namespace = new_type.target_namespace;

    // Replace in namespace table (no validation of original required for override)
    let ns_table = schema_set.get_or_create_namespace(namespace);
    ns_table.register_type(name, TypeKey::Simple(new_key));

    Ok(())
}

/// Override a complex type.
fn override_complex_type(
    schema_set: &mut SchemaSet,
    new_key: ComplexTypeKey,
) -> SchemaResult<()> {
    let new_type = schema_set
        .arenas
        .complex_types
        .get(new_key)
        .ok_or_else(|| SchemaError::internal("Override: complex type not found"))?;

    let name = new_type.name.ok_or_else(|| {
        SchemaError::structural(
            "src-override",
            "Overriding complex type must have a name",
            None,
        )
    })?;
    let namespace = new_type.target_namespace;

    let ns_table = schema_set.get_or_create_namespace(namespace);
    ns_table.register_type(name, TypeKey::Complex(new_key));

    Ok(())
}

/// Override a model group.
fn override_model_group(schema_set: &mut SchemaSet, new_key: ModelGroupKey) -> SchemaResult<()> {
    let new_group = schema_set
        .arenas
        .model_groups
        .get(new_key)
        .ok_or_else(|| SchemaError::internal("Override: model group not found"))?;

    let name = new_group.name.ok_or_else(|| {
        SchemaError::structural(
            "src-override",
            "Overriding model group must have a name",
            None,
        )
    })?;
    let namespace = new_group.target_namespace;

    let ns_table = schema_set.get_or_create_namespace(namespace);
    ns_table.register_model_group(name, new_key);

    Ok(())
}

/// Override an attribute group.
fn override_attribute_group(
    schema_set: &mut SchemaSet,
    new_key: AttributeGroupKey,
) -> SchemaResult<()> {
    let new_group = schema_set
        .arenas
        .attribute_groups
        .get(new_key)
        .ok_or_else(|| SchemaError::internal("Override: attribute group not found"))?;

    let name = new_group.name.ok_or_else(|| {
        SchemaError::structural(
            "src-override",
            "Overriding attribute group must have a name",
            None,
        )
    })?;
    let namespace = new_group.target_namespace;

    let ns_table = schema_set.get_or_create_namespace(namespace);
    ns_table.register_attribute_group(name, new_key);

    Ok(())
}

/// Override an element.
fn override_element(schema_set: &mut SchemaSet, new_key: ElementKey) -> SchemaResult<()> {
    let new_elem = schema_set
        .arenas
        .elements
        .get(new_key)
        .ok_or_else(|| SchemaError::internal("Override: element not found"))?;

    let name = new_elem.name.ok_or_else(|| {
        SchemaError::structural(
            "src-override",
            "Overriding element must have a name",
            None,
        )
    })?;
    let namespace = new_elem.target_namespace;

    let ns_table = schema_set.get_or_create_namespace(namespace);
    ns_table.register_element(name, new_key);

    Ok(())
}

/// Override an attribute.
fn override_attribute(schema_set: &mut SchemaSet, new_key: AttributeKey) -> SchemaResult<()> {
    let new_attr = schema_set
        .arenas
        .attributes
        .get(new_key)
        .ok_or_else(|| SchemaError::internal("Override: attribute not found"))?;

    let name = new_attr.name.ok_or_else(|| {
        SchemaError::structural(
            "src-override",
            "Overriding attribute must have a name",
            None,
        )
    })?;
    let namespace = new_attr.target_namespace;

    let ns_table = schema_set.get_or_create_namespace(namespace);
    ns_table.register_attribute(name, new_key);

    Ok(())
}

/// Override a notation.
fn override_notation(schema_set: &mut SchemaSet, new_key: NotationKey) -> SchemaResult<()> {
    let new_notation = schema_set
        .arenas
        .notations
        .get(new_key)
        .ok_or_else(|| SchemaError::internal("Override: notation not found"))?;

    let name = new_notation.name;
    let namespace = new_notation.target_namespace;

    let ns_table = schema_set.get_or_create_namespace(namespace);
    ns_table.register_notation(name, new_key);

    Ok(())
}

#[cfg(test)]
mod tests {
    // Note: Integration tests should be in the pipeline or builder module
    // as they require full schema parsing and assembly
}
