//! Substitution group helpers for validation and UPA checks.

use std::collections::{HashMap, HashSet};

use crate::ids::{ElementKey, NameId, TypeKey};
use crate::schema::model::{DerivationSet, SchemaSet};

/// Map from substitution group head to all substitutable element names.
pub type SubstitutionGroupMap = HashMap<ElementKey, HashSet<(NameId, Option<NameId>)>>;

/// Build a substitution group membership map for the schema set.
pub fn build_substitution_group_map(schema_set: &SchemaSet) -> SubstitutionGroupMap {
    let mut member_map: HashMap<ElementKey, Vec<ElementKey>> = HashMap::new();
    for (member_key, elem) in schema_set.arenas.elements.iter() {
        for head_key in &elem.resolved_substitution_groups {
            member_map.entry(*head_key).or_default().push(member_key);
        }
    }

    let mut result = HashMap::new();
    let mut seen_heads = HashSet::new();
    for (head_key, _) in schema_set.arenas.elements.iter() {
        let head_key = resolve_element_key(schema_set, head_key);
        if !seen_heads.insert(head_key) {
            continue;
        }

        let head_elem = match schema_set.arenas.elements.get(head_key) {
            Some(elem) => elem,
            None => continue,
        };
        let mut names = HashSet::new();
        if let Some(name) = head_elem.name {
            if !head_elem.is_abstract {
                names.insert((name, head_elem.target_namespace));
            }
        }

        let (effective_block, effective_final) = effective_element_constraints(schema_set, head_elem);
        if !effective_block.contains_substitution() {
            let head_type = head_elem.resolved_type;
            let exclude =
                derivation_exclusions(schema_set, effective_block, effective_final, head_type);

            let mut stack = member_map.get(&head_key).cloned().unwrap_or_default();
            let mut visited = HashSet::new();
            while let Some(member_key) = stack.pop() {
                if !visited.insert(member_key) {
                    continue;
                }
                if let Some(member) = resolved_element(schema_set, member_key) {
                    if let Some(name) = member.name {
                        if !member.is_abstract
                            && is_substitutable(schema_set, head_type, exclude, member.resolved_type)
                        {
                            names.insert((name, member.target_namespace));
                        }
                    }
                }
                if let Some(nested) = member_map.get(&member_key) {
                    for &next in nested {
                        if !visited.contains(&next) {
                            stack.push(next);
                        }
                    }
                }
            }
        }

        result.insert(head_key, names);
    }

    result
}

fn resolve_element_key(schema_set: &SchemaSet, key: ElementKey) -> ElementKey {
    schema_set
        .arenas
        .elements
        .get(key)
        .and_then(|elem| elem.resolved_ref)
        .unwrap_or(key)
}

fn resolved_element(
    schema_set: &SchemaSet,
    key: ElementKey,
) -> Option<&crate::arenas::ElementDeclData> {
    let key = resolve_element_key(schema_set, key);
    schema_set.arenas.elements.get(key)
}

fn derivation_exclusions(
    schema_set: &SchemaSet,
    effective_block: DerivationSet,
    effective_final: DerivationSet,
    head_type: Option<TypeKey>,
) -> DerivationSet {
    let mut exclude = effective_block & derivation_mask();
    exclude |= effective_final & derivation_mask();
    if let Some(head_type) = head_type {
        exclude |= type_final_derivation(schema_set, head_type) & derivation_mask();
    }
    exclude
}

fn derivation_mask() -> DerivationSet {
    DerivationSet::EXTENSION | DerivationSet::RESTRICTION | DerivationSet::LIST | DerivationSet::UNION
}

fn type_final_derivation(schema_set: &SchemaSet, type_key: TypeKey) -> DerivationSet {
    match type_key {
        TypeKey::Simple(key) => schema_set
            .arenas
            .simple_types
            .get(key)
            .map(|t| t.final_derivation)
            .unwrap_or_default(),
        TypeKey::Complex(key) => schema_set
            .arenas
            .complex_types
            .get(key)
            .map(|t| t.final_derivation)
            .unwrap_or_default(),
    }
}

fn is_substitutable(
    schema_set: &SchemaSet,
    head_type: Option<TypeKey>,
    exclude: DerivationSet,
    member_type: Option<TypeKey>,
) -> bool {
    let any_type = TypeKey::Complex(schema_set.any_type_key());
    let head_type = head_type.unwrap_or(any_type);
    let member_type = member_type.unwrap_or(any_type);
    schema_set.is_type_derived_from(member_type, head_type, exclude)
}

fn effective_element_constraints(
    schema_set: &SchemaSet,
    element: &crate::arenas::ElementDeclData,
) -> (DerivationSet, DerivationSet) {
    let mut block = element.block;
    let mut final_derivation = element.final_derivation;

    if block.is_empty() || final_derivation.is_empty() {
        if let Some(source) = element.source.as_ref() {
            if let Some(doc) = schema_set.documents.get(source.doc_id as usize) {
                if block.is_empty() {
                    block = doc.block_default;
                }
                if final_derivation.is_empty() {
                    final_derivation = doc.final_default;
                }
            }
        }
    }

    (block, final_derivation)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::location::{SourceRef, SourceSpan};
    use crate::schema::model::SchemaDocument;

    fn element_data(
        name: NameId,
        type_key: TypeKey,
        source: Option<SourceRef>,
    ) -> crate::arenas::ElementDeclData {
        crate::arenas::ElementDeclData {
            name: Some(name),
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
            block: DerivationSet::empty(),
            final_derivation: DerivationSet::empty(),
            form: None,
            id: None,
            alternatives: Vec::new(),
            identity_constraints: Vec::new(),
            annotation: None,
            source,
            resolved_type: Some(type_key),
            resolved_ref: None,
            resolved_substitution_groups: Vec::new(),
        }
    }

    fn with_doc(
        schema_set: &mut SchemaSet,
        block_default: DerivationSet,
        final_default: DerivationSet,
    ) -> SourceRef {
        let doc_id = schema_set.documents.len() as u32;
        let mut doc = SchemaDocument::new(doc_id, "test.xsd".to_string());
        doc.block_default = block_default;
        doc.final_default = final_default;
        schema_set.documents.push(doc);
        SourceRef::new(doc_id, SourceSpan::new(0, 0))
    }

    #[test]
    fn test_substitution_group_type_derivation_allows_member() {
        let mut schema_set = SchemaSet::new();
        let head_name = schema_set.name_table.add("head");
        let member_name = schema_set.name_table.add("member");
        let head_type = TypeKey::Simple(schema_set.builtin_types().decimal);
        let member_type = TypeKey::Simple(schema_set.builtin_types().int);

        let head_key = schema_set
            .arenas
            .alloc_element(element_data(head_name, head_type, None));
        let member_key = schema_set
            .arenas
            .alloc_element(element_data(member_name, member_type, None));
        schema_set
            .arenas
            .elements
            .get_mut(member_key)
            .unwrap()
            .resolved_substitution_groups
            .push(head_key);

        let map = build_substitution_group_map(&schema_set);
        let names = map.get(&head_key).unwrap();
        assert!(names.contains(&(member_name, None)));
    }

    #[test]
    fn test_substitution_group_element_final_blocks_member() {
        let mut schema_set = SchemaSet::new();
        let head_name = schema_set.name_table.add("head");
        let member_name = schema_set.name_table.add("member");
        let head_type = TypeKey::Simple(schema_set.builtin_types().decimal);
        let member_type = TypeKey::Simple(schema_set.builtin_types().int);

        let mut head = element_data(head_name, head_type, None);
        head.final_derivation = DerivationSet::RESTRICTION;
        let head_key = schema_set.arenas.alloc_element(head);
        let member_key = schema_set
            .arenas
            .alloc_element(element_data(member_name, member_type, None));
        schema_set
            .arenas
            .elements
            .get_mut(member_key)
            .unwrap()
            .resolved_substitution_groups
            .push(head_key);

        let map = build_substitution_group_map(&schema_set);
        let names = map.get(&head_key).unwrap();
        assert!(!names.contains(&(member_name, None)));
    }

    #[test]
    fn test_substitution_group_type_final_blocks_member() {
        let mut schema_set = SchemaSet::new();
        let head_name = schema_set.name_table.add("head");
        let member_name = schema_set.name_table.add("member");
        let head_type = schema_set.builtin_types().decimal;
        let member_type = TypeKey::Simple(schema_set.builtin_types().int);

        if let Some(type_def) = schema_set.arenas.simple_types.get_mut(head_type) {
            type_def.final_derivation = DerivationSet::RESTRICTION;
        }

        let head_key = schema_set
            .arenas
            .alloc_element(element_data(head_name, TypeKey::Simple(head_type), None));
        let member_key = schema_set
            .arenas
            .alloc_element(element_data(member_name, member_type, None));
        schema_set
            .arenas
            .elements
            .get_mut(member_key)
            .unwrap()
            .resolved_substitution_groups
            .push(head_key);

        let map = build_substitution_group_map(&schema_set);
        let names = map.get(&head_key).unwrap();
        assert!(!names.contains(&(member_name, None)));
    }

    #[test]
    fn test_substitution_group_block_substitution_keeps_head_only() {
        let mut schema_set = SchemaSet::new();
        let head_name = schema_set.name_table.add("head");
        let member_name = schema_set.name_table.add("member");
        let head_type = TypeKey::Simple(schema_set.builtin_types().decimal);
        let member_type = TypeKey::Simple(schema_set.builtin_types().int);

        let mut head = element_data(head_name, head_type, None);
        head.block = DerivationSet::SUBSTITUTION;
        let head_key = schema_set.arenas.alloc_element(head);
        let member_key = schema_set
            .arenas
            .alloc_element(element_data(member_name, member_type, None));
        schema_set
            .arenas
            .elements
            .get_mut(member_key)
            .unwrap()
            .resolved_substitution_groups
            .push(head_key);

        let map = build_substitution_group_map(&schema_set);
        let names = map.get(&head_key).unwrap();
        assert!(names.contains(&(head_name, None)));
        assert!(!names.contains(&(member_name, None)));
    }

    #[test]
    fn test_substitution_group_block_default_blocks_member() {
        let mut schema_set = SchemaSet::new();
        let head_name = schema_set.name_table.add("head");
        let member_name = schema_set.name_table.add("member");
        let head_type = TypeKey::Simple(schema_set.builtin_types().decimal);
        let member_type = TypeKey::Simple(schema_set.builtin_types().int);
        let source = with_doc(&mut schema_set, DerivationSet::SUBSTITUTION, DerivationSet::empty());

        let head_key = schema_set
            .arenas
            .alloc_element(element_data(head_name, head_type, Some(source.clone())));
        let member_key = schema_set
            .arenas
            .alloc_element(element_data(member_name, member_type, Some(source)));
        schema_set
            .arenas
            .elements
            .get_mut(member_key)
            .unwrap()
            .resolved_substitution_groups
            .push(head_key);

        let map = build_substitution_group_map(&schema_set);
        let names = map.get(&head_key).unwrap();
        assert!(names.contains(&(head_name, None)));
        assert!(!names.contains(&(member_name, None)));
    }

    #[test]
    fn test_substitution_group_final_default_blocks_member() {
        let mut schema_set = SchemaSet::new();
        let head_name = schema_set.name_table.add("head");
        let member_name = schema_set.name_table.add("member");
        let head_type = TypeKey::Simple(schema_set.builtin_types().decimal);
        let member_type = TypeKey::Simple(schema_set.builtin_types().int);
        let source = with_doc(&mut schema_set, DerivationSet::empty(), DerivationSet::RESTRICTION);

        let head_key = schema_set
            .arenas
            .alloc_element(element_data(head_name, head_type, Some(source.clone())));
        let member_key = schema_set
            .arenas
            .alloc_element(element_data(member_name, member_type, Some(source)));
        schema_set
            .arenas
            .elements
            .get_mut(member_key)
            .unwrap()
            .resolved_substitution_groups
            .push(head_key);

        let map = build_substitution_group_map(&schema_set);
        let names = map.get(&head_key).unwrap();
        assert!(names.contains(&(head_name, None)));
        assert!(!names.contains(&(member_name, None)));
    }
}
