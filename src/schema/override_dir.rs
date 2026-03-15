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
//!
//! When `resolved_doc_id` is available, override only replaces components
//! that actually exist in the target document's component index. Components
//! not found in the target document are silently skipped per §4.2.5.

use std::collections::HashSet;

use crate::error::{SchemaError, SchemaResult};
use crate::ids::*;
use crate::schema::composition::{
    CompositionEdgeKind, ComponentKey, ComponentKind,
    record_provenance, overridden_action,
};
use crate::schema::model::{OverrideComponent, OverrideDirective};
use crate::schema::SchemaSet;

/// Compute the target set of an override directive per XSD 1.1 §4.2.5.
///
/// The target set is the transitive closure of include + override edges
/// starting from the directly referenced document. Import and redefine
/// edges are excluded.
pub fn compute_target_set(schema_set: &SchemaSet, start_doc: DocumentId) -> HashSet<DocumentId> {
    let mut target_set = HashSet::new();
    let mut queue = vec![start_doc];

    while let Some(doc_id) = queue.pop() {
        if !target_set.insert(doc_id) {
            continue; // already visited
        }
        // Follow Include and Override edges from this document
        for edge in &schema_set.composition_edges {
            if edge.source_doc == doc_id {
                match edge.kind {
                    CompositionEdgeKind::Include | CompositionEdgeKind::Override => {
                        if let Some(target) = edge.target_doc {
                            if !target_set.contains(&target) {
                                queue.push(target);
                            }
                        }
                    }
                    _ => {} // Import and Redefine excluded from target set
                }
            }
        }
    }

    target_set
}

/// Apply an override directive to the schema set.
///
/// Computes the target set (transitive closure of include + override edges
/// from the directly referenced document per §4.2.5) and applies
/// replacements to all documents in the target set. Components not found
/// in any target set document are silently ignored.
pub fn apply_override(
    schema_set: &mut SchemaSet,
    override_dir: &OverrideDirective,
) -> SchemaResult<()> {
    let target_doc_id = override_dir.resolved_doc_id;
    let overriding_doc_id = override_dir.source.as_ref().map(|s| s.doc_id);

    // Compute the target set: transitive closure of include + override
    // edges from the directly referenced document.
    let target_set = match target_doc_id {
        Some(id) => compute_target_set(schema_set, id),
        None => HashSet::new(), // no target doc → unconditional replacement
    };

    for component in &override_dir.components {
        match component {
            OverrideComponent::SimpleType(key) => {
                override_simple_type(schema_set, *key, target_doc_id, &target_set, overriding_doc_id)?;
            }
            OverrideComponent::ComplexType(key) => {
                override_complex_type(schema_set, *key, target_doc_id, &target_set, overriding_doc_id)?;
            }
            OverrideComponent::Group(key) => {
                override_model_group(schema_set, *key, target_doc_id, &target_set, overriding_doc_id)?;
            }
            OverrideComponent::AttributeGroup(key) => {
                override_attribute_group(schema_set, *key, target_doc_id, &target_set, overriding_doc_id)?;
            }
            OverrideComponent::Element(key) => {
                override_element(schema_set, *key, target_doc_id, &target_set, overriding_doc_id)?;
            }
            OverrideComponent::Attribute(key) => {
                override_attribute(schema_set, *key, target_doc_id, &target_set, overriding_doc_id)?;
            }
            OverrideComponent::Notation(key) => {
                override_notation(schema_set, *key, target_doc_id, &target_set, overriding_doc_id)?;
            }
        }
    }
    Ok(())
}


/// Check whether a component of the given kind exists in any document in
/// the target set. Returns `true` when the target set is empty (fallback:
/// unconditional replacement for pre-loaded schemas without resolution).
fn target_set_has_component<T>(
    schema_set: &SchemaSet,
    target_set: &HashSet<DocumentId>,
    lookup: impl Fn(&crate::schema::composition::DocumentComponentIndex) -> Option<T>,
) -> bool {
    if target_set.is_empty() {
        return true; // no target set → unconditional replacement
    }
    target_set.iter().any(|&doc_id| {
        schema_set
            .documents
            .get(doc_id as usize)
            .and_then(|doc| lookup(&doc.component_index))
            .is_some()
    })
}

/// Override a simple type.
fn override_simple_type(
    schema_set: &mut SchemaSet,
    new_key: SimpleTypeKey,
    target_doc_id: Option<DocumentId>,
    target_set: &HashSet<DocumentId>,
    overriding_doc_id: Option<DocumentId>,
) -> SchemaResult<()> {
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

    if !target_set_has_component(schema_set, target_set, |idx| {
        idx.lookup_simple_type(namespace, name)
    }) {
        return Ok(()); // silently skip unmatched override per §4.2.5
    }

    let ns_table = schema_set.get_or_create_namespace(namespace);
    ns_table.register_type(name, TypeKey::Simple(new_key));
    record_provenance(
        &mut schema_set.effective_components,
        ComponentKey::Type(TypeKey::Simple(new_key)),
        ComponentKind::SimpleType, namespace, name, overriding_doc_id,
        overridden_action(overriding_doc_id, ComponentKind::SimpleType, name, namespace, target_doc_id),
    );

    Ok(())
}

/// Override a complex type.
fn override_complex_type(
    schema_set: &mut SchemaSet,
    new_key: ComplexTypeKey,
    target_doc_id: Option<DocumentId>,
    target_set: &HashSet<DocumentId>,
    overriding_doc_id: Option<DocumentId>,
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

    if !target_set_has_component(schema_set, target_set, |idx| {
        idx.lookup_complex_type(namespace, name)
    }) {
        return Ok(());
    }

    let ns_table = schema_set.get_or_create_namespace(namespace);
    ns_table.register_type(name, TypeKey::Complex(new_key));
    record_provenance(
        &mut schema_set.effective_components,
        ComponentKey::Type(TypeKey::Complex(new_key)),
        ComponentKind::ComplexType, namespace, name, overriding_doc_id,
        overridden_action(overriding_doc_id, ComponentKind::ComplexType, name, namespace, target_doc_id),
    );

    Ok(())
}

/// Override a model group.
fn override_model_group(
    schema_set: &mut SchemaSet,
    new_key: ModelGroupKey,
    target_doc_id: Option<DocumentId>,
    target_set: &HashSet<DocumentId>,
    overriding_doc_id: Option<DocumentId>,
) -> SchemaResult<()> {
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

    if !target_set_has_component(schema_set, target_set, |idx| {
        idx.lookup_model_group(namespace, name)
    }) {
        return Ok(());
    }

    let ns_table = schema_set.get_or_create_namespace(namespace);
    ns_table.register_model_group(name, new_key);
    record_provenance(
        &mut schema_set.effective_components,
        ComponentKey::ModelGroup(new_key),
        ComponentKind::ModelGroup, namespace, name, overriding_doc_id,
        overridden_action(overriding_doc_id, ComponentKind::ModelGroup, name, namespace, target_doc_id),
    );

    Ok(())
}

/// Override an attribute group.
fn override_attribute_group(
    schema_set: &mut SchemaSet,
    new_key: AttributeGroupKey,
    target_doc_id: Option<DocumentId>,
    target_set: &HashSet<DocumentId>,
    overriding_doc_id: Option<DocumentId>,
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

    if !target_set_has_component(schema_set, target_set, |idx| {
        idx.lookup_attribute_group(namespace, name)
    }) {
        return Ok(());
    }

    let ns_table = schema_set.get_or_create_namespace(namespace);
    ns_table.register_attribute_group(name, new_key);
    record_provenance(
        &mut schema_set.effective_components,
        ComponentKey::AttributeGroup(new_key),
        ComponentKind::AttributeGroup, namespace, name, overriding_doc_id,
        overridden_action(overriding_doc_id, ComponentKind::AttributeGroup, name, namespace, target_doc_id),
    );

    Ok(())
}

/// Override an element.
fn override_element(
    schema_set: &mut SchemaSet,
    new_key: ElementKey,
    target_doc_id: Option<DocumentId>,
    target_set: &HashSet<DocumentId>,
    overriding_doc_id: Option<DocumentId>,
) -> SchemaResult<()> {
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

    if !target_set_has_component(schema_set, target_set, |idx| {
        idx.lookup_element(namespace, name)
    }) {
        return Ok(());
    }

    let ns_table = schema_set.get_or_create_namespace(namespace);
    ns_table.register_element(name, new_key);
    record_provenance(
        &mut schema_set.effective_components,
        ComponentKey::Element(new_key),
        ComponentKind::Element, namespace, name, overriding_doc_id,
        overridden_action(overriding_doc_id, ComponentKind::Element, name, namespace, target_doc_id),
    );

    Ok(())
}

/// Override an attribute.
fn override_attribute(
    schema_set: &mut SchemaSet,
    new_key: AttributeKey,
    target_doc_id: Option<DocumentId>,
    target_set: &HashSet<DocumentId>,
    overriding_doc_id: Option<DocumentId>,
) -> SchemaResult<()> {
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

    if !target_set_has_component(schema_set, target_set, |idx| {
        idx.lookup_attribute(namespace, name)
    }) {
        return Ok(());
    }

    let ns_table = schema_set.get_or_create_namespace(namespace);
    ns_table.register_attribute(name, new_key);
    record_provenance(
        &mut schema_set.effective_components,
        ComponentKey::Attribute(new_key),
        ComponentKind::Attribute, namespace, name, overriding_doc_id,
        overridden_action(overriding_doc_id, ComponentKind::Attribute, name, namespace, target_doc_id),
    );

    Ok(())
}

/// Override a notation.
fn override_notation(
    schema_set: &mut SchemaSet,
    new_key: NotationKey,
    target_doc_id: Option<DocumentId>,
    target_set: &HashSet<DocumentId>,
    overriding_doc_id: Option<DocumentId>,
) -> SchemaResult<()> {
    let new_notation = schema_set
        .arenas
        .notations
        .get(new_key)
        .ok_or_else(|| SchemaError::internal("Override: notation not found"))?;

    let name = new_notation.name;
    let namespace = new_notation.target_namespace;

    if !target_set_has_component(schema_set, target_set, |idx| {
        idx.lookup_notation(namespace, name)
    }) {
        return Ok(());
    }

    let ns_table = schema_set.get_or_create_namespace(namespace);
    ns_table.register_notation(name, new_key);
    record_provenance(
        &mut schema_set.effective_components,
        ComponentKey::Notation(new_key),
        ComponentKind::Notation, namespace, name, overriding_doc_id,
        overridden_action(overriding_doc_id, ComponentKind::Notation, name, namespace, target_doc_id),
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse::parse_schema;
    use crate::schema::model::OverrideDirective;

    /// When resolved_doc_id is Some and the target document does NOT
    /// declare the overridden component, the override must be silently
    /// skipped — it must NOT clobber whatever is in the global namespace
    /// table from another document.
    #[test]
    fn test_override_skips_unmatched_component() {
        let tmp = std::env::temp_dir().join("xsd_test_override_skip_unmatched");
        std::fs::create_dir_all(&tmp).unwrap();

        // doc_a.xsd declares MyElem
        let doc_a_xsd = r#"<?xml version="1.0" encoding="UTF-8"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
    <xs:element name="MyElem" type="xs:string"/>
</xs:schema>"#;
        let doc_a_path = tmp.join("ovr_skip_a.xsd");
        std::fs::write(&doc_a_path, doc_a_xsd).unwrap();

        // doc_b.xsd does NOT declare MyElem
        let doc_b_xsd = r#"<?xml version="1.0" encoding="UTF-8"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
    <xs:element name="OtherElem" type="xs:integer"/>
</xs:schema>"#;
        let doc_b_path = tmp.join("ovr_skip_b.xsd");
        std::fs::write(&doc_b_path, doc_b_xsd).unwrap();

        // Parse both documents
        let mut schema_set = SchemaSet::new();
        let _doc_a_id = parse_schema(
            std::fs::read_to_string(&doc_a_path).unwrap().as_bytes(),
            &doc_a_path.to_string_lossy(),
            &mut schema_set,
        )
        .unwrap();
        let doc_b_id = parse_schema(
            std::fs::read_to_string(&doc_b_path).unwrap().as_bytes(),
            &doc_b_path.to_string_lossy(),
            &mut schema_set,
        )
        .unwrap();

        // Capture the original global entry for MyElem (from doc_a)
        let my_elem_name = schema_set.name_table.get("MyElem").unwrap();
        let original_key = schema_set
            .lookup_element(None, my_elem_name)
            .expect("MyElem should be in global namespace table from doc_a");

        // Create a replacement element in the arena
        let replacement_key = schema_set.arenas.alloc_element(
            crate::arenas::ElementDeclData {
                name: Some(my_elem_name),
                target_namespace: None,
                ref_name: None,
                type_ref: None,
                inline_type: None,
                substitution_group: Vec::new(),
                default_value: None,
                fixed_value: None,
                nillable: false,
                is_abstract: false,
                min_occurs: 1,
                max_occurs: Some(1),
                block: crate::schema::model::DerivationSet::empty(),
                final_derivation: crate::schema::model::DerivationSet::empty(),
                form: None,
                id: None,
                alternatives: Vec::new(),
                identity_constraints: Vec::new(),
                annotation: None,
                source: None,
                resolved_type: None,
                resolved_ref: None,
                resolved_substitution_groups: Vec::new(),
            },
        );

        // Override targeting doc_b (which does NOT have MyElem)
        let override_dir = OverrideDirective {
            source: None,
            schema_location: doc_b_path.to_string_lossy().to_string(),
            resolved_doc_id: Some(doc_b_id),
            components: vec![OverrideComponent::Element(replacement_key)],
        };

        // Apply override — should succeed (silently skip unmatched)
        apply_override(&mut schema_set, &override_dir).unwrap();

        // The global namespace table must still have the ORIGINAL key from doc_a,
        // NOT the replacement — the override was skipped.
        let current_key = schema_set
            .lookup_element(None, my_elem_name)
            .expect("MyElem should still be in namespace table");
        assert_eq!(
            current_key, original_key,
            "Override must not clobber global entry when target document lacks the component"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Override with transitive target set: base.xsd includes helper.xsd,
    /// main.xsd overrides from base.xsd. The override should be able to
    /// target components declared in helper.xsd (via the transitive
    /// include + override closure).
    #[test]
    fn test_override_transitive_target_set() {
        use crate::parser::parse::parse_schema;
        use crate::parser::resolver::{resolve_all_directives, SchemaResolver};

        let tmp = std::env::temp_dir().join("xsd_test_override_transitive");
        std::fs::create_dir_all(&tmp).unwrap();

        // helper.xsd declares MyElem
        let helper_xsd = r#"<?xml version="1.0" encoding="UTF-8"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
    <xs:element name="MyElem" type="xs:string"/>
</xs:schema>"#;
        let helper_path = tmp.join("ovr_helper.xsd");
        std::fs::write(&helper_path, helper_xsd).unwrap();

        // base.xsd includes helper.xsd
        let base_xsd = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
    <xs:include schemaLocation="{}"/>
    <xs:element name="BaseElem" type="xs:integer"/>
</xs:schema>"#,
            helper_path.to_string_lossy()
        );
        let base_path = tmp.join("ovr_base.xsd");
        std::fs::write(&base_path, &base_xsd).unwrap();

        // main.xsd overrides from base.xsd, replacing MyElem
        // (which is declared in helper.xsd, not base.xsd directly)
        let main_xsd = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
    <xs:override schemaLocation="{}">
        <xs:element name="MyElem" type="xs:integer"/>
    </xs:override>
</xs:schema>"#,
            base_path.to_string_lossy()
        );

        // Must use XSD 1.1 for xs:override support
        let mut schema_set = SchemaSet::xsd11();
        let main_path = tmp.join("ovr_main.xsd").to_string_lossy().to_string();
        let doc_id = parse_schema(main_xsd.as_bytes(), &main_path, &mut schema_set).unwrap();

        let mut resolver = SchemaResolver::new();
        // Resolve main → loads base → loads helper
        let result = resolve_all_directives(doc_id, &mut resolver, &mut schema_set);
        assert!(result.is_ok(), "Resolution should succeed");

        // Resolve base's directives (loads helper via include)
        for loaded_id in result.loaded.clone() {
            let nested = resolve_all_directives(loaded_id, &mut resolver, &mut schema_set);
            assert!(nested.is_ok());
            for nested_id in nested.loaded {
                let _ = resolve_all_directives(nested_id, &mut resolver, &mut schema_set);
            }
        }

        // Verify the target set includes both base and helper
        let main_doc = &schema_set.documents[doc_id as usize];
        let override_target = main_doc.overrides[0].resolved_doc_id;
        assert!(override_target.is_some(), "Override should have resolved_doc_id");

        let target_set = compute_target_set(&schema_set, override_target.unwrap());
        assert!(
            target_set.len() >= 2,
            "Target set should include base and helper, got {} documents",
            target_set.len()
        );

        // Apply composition — override should replace MyElem even though
        // it's declared in helper.xsd (transitive target)
        crate::schema::apply_redefine_override(&mut schema_set).unwrap();

        // MyElem should still exist in namespace table (replaced, not removed)
        let my_elem_name = schema_set.name_table.get("MyElem").unwrap();
        assert!(
            schema_set.lookup_element(None, my_elem_name).is_some(),
            "MyElem should be in namespace table after transitive override"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
