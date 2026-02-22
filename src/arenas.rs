//! Arena storage for schema components
//!
//! All schema components are stored in arenas to avoid reference cycles.
//! Each component type has its own SlotMap with typed keys for type safety.
//!
//! Uses slotmap for O(1) insertion, lookup, and removal with generation tracking.

use slotmap::SlotMap;

use crate::ids::*;
use crate::parser::frames::{
    AlternativeResult, AttributeUseResult, ComplexContentResult, Compositor,
    DerivationMethod, IdentityResult, OpenContentResult, ParticleResult, QNameRef,
    SimpleTypeResult, SimpleTypeVariety, TypeFrameResult, TypeRefResult, WildcardResult,
};
#[cfg(feature = "xsd11")]
use crate::parser::frames::AssertResult;
use crate::parser::location::SourceRef;
use crate::schema::annotation::Annotation;
use crate::schema::model::DerivationSet;
use crate::types::facets::FacetSet;

// Forward declarations for types that will be defined later
// These are placeholders until we define the actual types

/// Placeholder for SimpleTypeDef (defined in types/simple.rs)
#[derive(Debug)]
pub struct SimpleTypeDefData {
    pub name: Option<NameId>,
    pub target_namespace: Option<NameId>,
    pub variety: SimpleTypeVariety,
    pub base_type: Option<TypeRefResult>,
    pub item_type: Option<TypeRefResult>,
    pub member_types: Vec<TypeRefResult>,
    pub facets: FacetSet,
    pub final_derivation: DerivationSet,
    pub id: Option<String>,
    pub derivation_id: Option<String>,
    pub annotation: Option<Annotation>,
    pub source: Option<SourceRef>,

    // Resolved references (populated after reference resolution phase)
    /// Resolved base type key (for restriction derivation)
    pub resolved_base_type: Option<TypeKey>,
    /// Resolved item type key (for list types)
    pub resolved_item_type: Option<TypeKey>,
    /// Resolved member type keys (for union types)
    pub resolved_member_types: Vec<TypeKey>,
}

/// Resolved attribute use - stores resolved keys for attribute use references
#[derive(Debug, Clone)]
pub struct ResolvedAttributeUse {
    /// Resolved type key (from type_ref or inline type)
    pub resolved_type: Option<TypeKey>,
    /// Resolved attribute reference (for attribute refs)
    pub resolved_ref: Option<AttributeKey>,
}

/// Placeholder for ComplexTypeDef (defined in types/complex.rs)
#[derive(Debug)]
pub struct ComplexTypeDefData {
    pub name: Option<NameId>,
    pub target_namespace: Option<NameId>,
    pub base_type: Option<TypeRefResult>,
    pub derivation_method: Option<DerivationMethod>,
    pub content: ComplexContentResult,
    pub open_content: Option<OpenContentResult>,
    pub attributes: Vec<AttributeUseResult>,
    pub attribute_groups: Vec<QNameRef>,
    pub attribute_wildcard: Option<WildcardResult>,
    pub mixed: bool,
    pub is_abstract: bool,
    pub final_derivation: DerivationSet,
    pub block: DerivationSet,
    pub default_attributes_apply: bool,
    pub id: Option<String>,
    #[cfg(feature = "xsd11")]
    pub assertions: Vec<AssertResult>,
    #[cfg(feature = "xsd11")]
    pub xpath_default_namespace: Option<String>,
    pub annotation: Option<Annotation>,
    pub source: Option<SourceRef>,

    // Resolved references (populated after reference resolution phase)
    /// Resolved base type key (for extension/restriction derivation)
    pub resolved_base_type: Option<TypeKey>,
    /// Resolved attribute group keys
    pub resolved_attribute_groups: Vec<AttributeGroupKey>,
    /// Resolved attribute uses (parallel to attributes vec)
    pub resolved_attributes: Vec<ResolvedAttributeUse>,
    /// Resolved inline types for content particle elements (flat depth-first element order)
    pub resolved_content_particle_types: Vec<Option<TypeKey>>,
    /// Resolved element keys for local elements in content particles (flat depth-first element order)
    pub resolved_content_particle_elements: Vec<Option<ElementKey>>,
}

/// Placeholder for ElementDecl (defined in schema/decl.rs)
#[derive(Debug)]
pub struct ElementDeclData {
    pub name: Option<NameId>,
    pub target_namespace: Option<NameId>,
    pub ref_name: Option<QNameRef>,
    pub type_ref: Option<TypeRefResult>,
    pub inline_type: Option<Box<TypeFrameResult>>,
    pub substitution_group: Vec<QNameRef>,
    pub default_value: Option<String>,
    pub fixed_value: Option<String>,
    pub nillable: bool,
    pub is_abstract: bool,
    pub min_occurs: u32,
    pub max_occurs: Option<u32>,
    pub block: DerivationSet,
    pub final_derivation: DerivationSet,
    pub form: Option<String>,
    pub id: Option<String>,
    pub alternatives: Vec<AlternativeResult>,
    pub identity_constraints: Vec<IdentityResult>,
    pub annotation: Option<Annotation>,
    pub source: Option<SourceRef>,

    // Resolved references (populated after reference resolution phase)
    /// Resolved type key (from type_ref or inline_type)
    pub resolved_type: Option<TypeKey>,
    /// Resolved element reference (for element refs)
    pub resolved_ref: Option<ElementKey>,
    /// Resolved substitution group head elements
    pub resolved_substitution_groups: Vec<ElementKey>,
}

/// Placeholder for AttributeDecl (defined in schema/decl.rs)
#[derive(Debug)]
pub struct AttributeDeclData {
    pub name: Option<NameId>,
    pub target_namespace: Option<NameId>,
    pub ref_name: Option<QNameRef>,
    pub type_ref: Option<TypeRefResult>,
    pub inline_type: Option<Box<SimpleTypeResult>>,
    pub default_value: Option<String>,
    pub fixed_value: Option<String>,
    pub use_kind: Option<String>,
    pub form: Option<String>,
    pub inheritable: bool,
    pub id: Option<String>,
    pub annotation: Option<Annotation>,
    pub source: Option<SourceRef>,

    // Resolved references (populated after reference resolution phase)
    /// Resolved type key (from type_ref or inline_type)
    pub resolved_type: Option<TypeKey>,
    /// Resolved attribute reference (for attribute refs)
    pub resolved_ref: Option<AttributeKey>,
}

/// Placeholder for AttributeGroup (defined in schema/group.rs)
#[derive(Debug)]
pub struct AttributeGroupData {
    pub name: Option<NameId>,
    pub target_namespace: Option<NameId>,
    pub ref_name: Option<QNameRef>,
    pub attributes: Vec<AttributeUseResult>,
    pub attribute_groups: Vec<QNameRef>,
    pub attribute_wildcard: Option<WildcardResult>,
    pub id: Option<String>,
    pub annotation: Option<Annotation>,
    pub source: Option<SourceRef>,

    // Resolved references (populated after reference resolution phase)
    /// Resolved attribute group reference (for attributeGroup refs)
    pub resolved_ref: Option<AttributeGroupKey>,
    /// Resolved nested attribute group keys
    pub resolved_attribute_groups: Vec<AttributeGroupKey>,
    /// Resolved attribute uses (parallel to attributes vec)
    pub resolved_attributes: Vec<ResolvedAttributeUse>,
}

/// Resolved particle term - stores resolved keys for particle references
#[derive(Debug, Clone)]
pub enum ResolvedParticleTerm {
    /// Element with resolved type and ref
    Element {
        resolved_type: Option<TypeKey>,
        resolved_ref: Option<ElementKey>,
    },
    /// Group with resolved ref
    Group {
        resolved_ref: Option<ModelGroupKey>,
    },
    /// Wildcard (no resolution needed)
    Any,
}

/// Placeholder for ModelGroup (defined in schema/group.rs)
#[derive(Debug)]
pub struct ModelGroupData {
    pub name: Option<NameId>,
    pub target_namespace: Option<NameId>,
    pub ref_name: Option<QNameRef>,
    pub compositor: Option<Compositor>,
    pub particles: Vec<ParticleResult>,
    pub min_occurs: u32,
    pub max_occurs: Option<u32>,
    pub id: Option<String>,
    pub annotation: Option<Annotation>,
    pub source: Option<SourceRef>,

    // Resolved references (populated after reference resolution phase)
    /// Resolved model group reference (for group refs)
    pub resolved_ref: Option<ModelGroupKey>,
    /// Resolved particle terms (parallel to particles vec)
    pub resolved_particles: Vec<ResolvedParticleTerm>,
    /// Flat depth-first indexed resolved types for all particles (including nested inline groups)
    pub resolved_particle_types: Vec<Option<TypeKey>>,
    /// Flat depth-first indexed resolved element keys for all particles (including nested inline groups)
    pub resolved_particle_elements: Vec<Option<ElementKey>>,
}

/// Placeholder for Notation (defined in schema/decl.rs)
#[derive(Debug)]
pub struct NotationData {
    pub name: NameId,
    pub target_namespace: Option<NameId>,
    pub public: Option<String>,
    pub system: Option<String>,
    pub id: Option<String>,
    pub annotation: Option<Annotation>,
    pub source: Option<SourceRef>,
}

/// Placeholder for IdentityConstraint
#[derive(Debug)]
pub struct IdentityConstraintData {
    pub name: NameId,
    // TODO: Add full fields
}

/// Arena storage for all schema component types
///
/// Components are stored in type-specific SlotMaps and accessed via typed keys.
/// This approach avoids reference cycles and provides O(1) access with generation tracking.
#[derive(Debug, Default)]
pub struct SchemaArenas {
    /// Simple type definitions
    pub simple_types: SlotMap<SimpleTypeKey, SimpleTypeDefData>,
    /// Complex type definitions
    pub complex_types: SlotMap<ComplexTypeKey, ComplexTypeDefData>,
    /// Element declarations
    pub elements: SlotMap<ElementKey, ElementDeclData>,
    /// Attribute declarations
    pub attributes: SlotMap<AttributeKey, AttributeDeclData>,
    /// Attribute groups
    pub attribute_groups: SlotMap<AttributeGroupKey, AttributeGroupData>,
    /// Named model groups
    pub model_groups: SlotMap<ModelGroupKey, ModelGroupData>,
    /// Notations
    pub notations: SlotMap<NotationKey, NotationData>,
    /// Identity constraints
    pub identity_constraints: SlotMap<IdentityConstraintKey, IdentityConstraintData>,
}

impl SchemaArenas {
    /// Create new empty arenas
    pub fn new() -> Self {
        Self::default()
    }

    // Simple types
    pub fn alloc_simple_type(&mut self, data: SimpleTypeDefData) -> SimpleTypeKey {
        self.simple_types.insert(data)
    }

    pub fn get_simple_type(&self, key: SimpleTypeKey) -> Option<&SimpleTypeDefData> {
        self.simple_types.get(key)
    }

    pub fn get_simple_type_mut(&mut self, key: SimpleTypeKey) -> Option<&mut SimpleTypeDefData> {
        self.simple_types.get_mut(key)
    }

    // Complex types
    pub fn alloc_complex_type(&mut self, data: ComplexTypeDefData) -> ComplexTypeKey {
        self.complex_types.insert(data)
    }

    pub fn get_complex_type(&self, key: ComplexTypeKey) -> Option<&ComplexTypeDefData> {
        self.complex_types.get(key)
    }

    pub fn get_complex_type_mut(&mut self, key: ComplexTypeKey) -> Option<&mut ComplexTypeDefData> {
        self.complex_types.get_mut(key)
    }

    // Elements
    pub fn alloc_element(&mut self, data: ElementDeclData) -> ElementKey {
        self.elements.insert(data)
    }

    pub fn get_element(&self, key: ElementKey) -> Option<&ElementDeclData> {
        self.elements.get(key)
    }

    pub fn get_element_mut(&mut self, key: ElementKey) -> Option<&mut ElementDeclData> {
        self.elements.get_mut(key)
    }

    // Attributes
    pub fn alloc_attribute(&mut self, data: AttributeDeclData) -> AttributeKey {
        self.attributes.insert(data)
    }

    pub fn get_attribute(&self, key: AttributeKey) -> Option<&AttributeDeclData> {
        self.attributes.get(key)
    }

    pub fn get_attribute_mut(&mut self, key: AttributeKey) -> Option<&mut AttributeDeclData> {
        self.attributes.get_mut(key)
    }

    // Attribute groups
    pub fn alloc_attribute_group(&mut self, data: AttributeGroupData) -> AttributeGroupKey {
        self.attribute_groups.insert(data)
    }

    pub fn get_attribute_group(&self, key: AttributeGroupKey) -> Option<&AttributeGroupData> {
        self.attribute_groups.get(key)
    }

    pub fn get_attribute_group_mut(&mut self, key: AttributeGroupKey) -> Option<&mut AttributeGroupData> {
        self.attribute_groups.get_mut(key)
    }

    // Model groups
    pub fn alloc_model_group(&mut self, data: ModelGroupData) -> ModelGroupKey {
        self.model_groups.insert(data)
    }

    pub fn get_model_group(&self, key: ModelGroupKey) -> Option<&ModelGroupData> {
        self.model_groups.get(key)
    }

    pub fn get_model_group_mut(&mut self, key: ModelGroupKey) -> Option<&mut ModelGroupData> {
        self.model_groups.get_mut(key)
    }

    // Notations
    pub fn alloc_notation(&mut self, data: NotationData) -> NotationKey {
        self.notations.insert(data)
    }

    pub fn get_notation(&self, key: NotationKey) -> Option<&NotationData> {
        self.notations.get(key)
    }

    pub fn get_notation_mut(&mut self, key: NotationKey) -> Option<&mut NotationData> {
        self.notations.get_mut(key)
    }

    // Identity constraints
    pub fn alloc_identity_constraint(&mut self, data: IdentityConstraintData) -> IdentityConstraintKey {
        self.identity_constraints.insert(data)
    }

    pub fn get_identity_constraint(&self, key: IdentityConstraintKey) -> Option<&IdentityConstraintData> {
        self.identity_constraints.get(key)
    }

    pub fn get_identity_constraint_mut(&mut self, key: IdentityConstraintKey) -> Option<&mut IdentityConstraintData> {
        self.identity_constraints.get_mut(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn simple_type_data(name: NameId) -> SimpleTypeDefData {
        SimpleTypeDefData {
            name: Some(name),
            target_namespace: None,
            variety: SimpleTypeVariety::Atomic,
            base_type: None,
            item_type: None,
            member_types: Vec::new(),
            facets: FacetSet::new(),
            final_derivation: DerivationSet::empty(),
            id: None,
            derivation_id: None,
            annotation: None,
            source: None,
            // Resolved references
            resolved_base_type: None,
            resolved_item_type: None,
            resolved_member_types: Vec::new(),
        }
    }

    fn element_data(name: NameId, target_namespace: Option<NameId>) -> ElementDeclData {
        ElementDeclData {
            name: Some(name),
            target_namespace,
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
            source: None,
            // Resolved references
            resolved_type: None,
            resolved_ref: None,
            resolved_substitution_groups: Vec::new(),
        }
    }

    #[test]
    fn test_alloc_and_get_simple_type() {
        let mut arenas = SchemaArenas::new();
        let data = simple_type_data(NameId(1));
        let key = arenas.alloc_simple_type(data);
        let retrieved = arenas.get_simple_type(key).unwrap();
        assert_eq!(retrieved.name, Some(NameId(1)));
    }

    #[test]
    fn test_alloc_and_get_element() {
        let mut arenas = SchemaArenas::new();
        let data = element_data(NameId(2), Some(NameId(3)));
        let key = arenas.alloc_element(data);
        let retrieved = arenas.get_element(key).unwrap();
        assert_eq!(retrieved.name, Some(NameId(2)));
        assert_eq!(retrieved.target_namespace, Some(NameId(3)));
    }

    #[test]
    fn test_multiple_allocations() {
        let mut arenas = SchemaArenas::new();

        let key1 = arenas.alloc_simple_type(simple_type_data(NameId(1)));
        let key2 = arenas.alloc_simple_type(simple_type_data(NameId(2)));

        assert_ne!(key1, key2);
        assert_eq!(arenas.get_simple_type(key1).unwrap().name, Some(NameId(1)));
        assert_eq!(arenas.get_simple_type(key2).unwrap().name, Some(NameId(2)));
    }

    #[test]
    fn test_mutable_access() {
        let mut arenas = SchemaArenas::new();
        let key = arenas.alloc_simple_type(simple_type_data(NameId(1)));

        // Modify through mutable reference
        if let Some(data) = arenas.get_simple_type_mut(key) {
            data.name = Some(NameId(99));
        }

        assert_eq!(arenas.get_simple_type(key).unwrap().name, Some(NameId(99)));
    }

    #[test]
    fn test_slotmap_iteration() {
        let mut arenas = SchemaArenas::new();
        arenas.alloc_simple_type(simple_type_data(NameId(10)));
        arenas.alloc_simple_type(simple_type_data(NameId(20)));
        arenas.alloc_simple_type(simple_type_data(NameId(30)));

        let names: Vec<_> = arenas.simple_types.values()
            .filter_map(|d| d.name)
            .map(|n| n.0)
            .collect();
        assert_eq!(names.len(), 3);
        assert!(names.contains(&10));
        assert!(names.contains(&20));
        assert!(names.contains(&30));
    }

    #[test]
    fn test_key_with_values() {
        let mut arenas = SchemaArenas::new();
        let key1 = arenas.alloc_element(element_data(NameId(1), None));
        let key2 = arenas.alloc_element(element_data(NameId(2), None));

        // Iterate with keys
        let pairs: Vec<_> = arenas.elements.iter()
            .map(|(k, v)| (k, v.name.unwrap().0))
            .collect();

        assert_eq!(pairs.len(), 2);
        assert!(pairs.iter().any(|(k, _)| *k == key1));
        assert!(pairs.iter().any(|(k, _)| *k == key2));
    }
}
