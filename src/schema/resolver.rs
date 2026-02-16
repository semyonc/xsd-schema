//! Reference resolution for QName → component ID conversion
//!
//! This module resolves QName string references to component keys (IDs).
//! It is run after all schemas are parsed and assembled, but before
//! type derivation validation or content model compilation.
//!
//! # Resolution Process
//!
//! 1. Check built-in types first (for XS namespace types)
//! 2. Look up in namespace tables for user-defined types
//! 3. Return error with source location if not found
//!
//! # Supported References
//!
//! - Type references (QName → TypeKey)
//! - Element references (QName → ElementKey)
//! - Attribute references (QName → AttributeKey)
//! - Model group references (QName → ModelGroupKey)
//! - Attribute group references (QName → AttributeGroupKey)
//! - Notation references (QName → NotationKey)

use crate::error::{SchemaError, SchemaResult};
use crate::ids::*;
use crate::parser::frames::{QNameRef, TypeRefResult};
use crate::parser::location::SourceRef;
use crate::schema::SchemaSet;

/// Reference resolver for QName → component ID resolution
///
/// This struct holds a reference to the schema set and provides
/// methods to resolve different types of QName references.
pub struct ReferenceResolver<'a> {
    schema_set: &'a SchemaSet,
}

impl<'a> ReferenceResolver<'a> {
    /// Create a new reference resolver for the given schema set
    pub fn new(schema_set: &'a SchemaSet) -> Self {
        Self { schema_set }
    }

    /// Resolve a type reference (QName → TypeKey)
    ///
    /// Checks built-in types first, then user-defined types.
    /// The namespace should already be resolved during parsing via NamespaceContextSnapshot.
    pub fn resolve_type_ref(
        &self,
        qname: &QNameRef,
        source: Option<&SourceRef>,
    ) -> SchemaResult<TypeKey> {
        // Use the namespace resolved during parsing
        let namespace = qname.namespace;

        // 1. Check built-in types first (XS namespace)
        if let Some(type_key) = self
            .schema_set
            .get_built_in_type_by_qname(namespace, qname.local_name)
        {
            return Ok(type_key);
        }

        // 2. Look up in namespace table
        if let Some(type_key) = self
            .schema_set
            .lookup_type(namespace, qname.local_name)
        {
            return Ok(type_key);
        }

        // 3. Not found - error
        let location = source.and_then(|s| self.schema_set.source_maps.locate(s));
        let name_str = self.format_qname(qname);
        Err(SchemaError::structural(
            "src-resolve",
            format!("Type '{}' not found", name_str),
            location,
        ))
    }

    /// Resolve a TypeRefResult to a TypeKey
    ///
    /// Handles both QName references and inline types.
    /// For inline types, the type must already be allocated in the arena.
    pub fn resolve_type_ref_result(
        &self,
        type_ref: &TypeRefResult,
        source: Option<&SourceRef>,
    ) -> SchemaResult<Option<TypeKey>> {
        match type_ref {
            TypeRefResult::QName(qname) => Ok(Some(self.resolve_type_ref(qname, source)?)),
            TypeRefResult::Inline(_) => {
                // Inline types are handled during assembly and should already
                // have their keys stored elsewhere. Return None to indicate
                // the caller should use the inline type key.
                Ok(None)
            }
        }
    }

    /// Resolve an element reference (QName → ElementKey)
    pub fn resolve_element_ref(
        &self,
        qname: &QNameRef,
        source: Option<&SourceRef>,
    ) -> SchemaResult<ElementKey> {
        if let Some(key) = self
            .schema_set
            .lookup_element(qname.namespace, qname.local_name)
        {
            return Ok(key);
        }

        let location = source.and_then(|s| self.schema_set.source_maps.locate(s));
        let name_str = self.format_qname(qname);
        Err(SchemaError::structural(
            "src-resolve",
            format!("Element '{}' not found", name_str),
            location,
        ))
    }

    /// Resolve an attribute reference (QName → AttributeKey)
    pub fn resolve_attribute_ref(
        &self,
        qname: &QNameRef,
        source: Option<&SourceRef>,
    ) -> SchemaResult<AttributeKey> {
        if let Some(key) = self
            .schema_set
            .lookup_attribute(qname.namespace, qname.local_name)
        {
            return Ok(key);
        }

        let location = source.and_then(|s| self.schema_set.source_maps.locate(s));
        let name_str = self.format_qname(qname);
        Err(SchemaError::structural(
            "src-resolve",
            format!("Attribute '{}' not found", name_str),
            location,
        ))
    }

    /// Resolve a model group reference (QName → ModelGroupKey)
    pub fn resolve_group_ref(
        &self,
        qname: &QNameRef,
        source: Option<&SourceRef>,
    ) -> SchemaResult<ModelGroupKey> {
        if let Some(key) = self
            .schema_set
            .lookup_model_group(qname.namespace, qname.local_name)
        {
            return Ok(key);
        }

        let location = source.and_then(|s| self.schema_set.source_maps.locate(s));
        let name_str = self.format_qname(qname);
        Err(SchemaError::structural(
            "src-resolve",
            format!("Group '{}' not found", name_str),
            location,
        ))
    }

    /// Resolve an attribute group reference (QName → AttributeGroupKey)
    pub fn resolve_attribute_group_ref(
        &self,
        qname: &QNameRef,
        source: Option<&SourceRef>,
    ) -> SchemaResult<AttributeGroupKey> {
        if let Some(key) = self
            .schema_set
            .lookup_attribute_group(qname.namespace, qname.local_name)
        {
            return Ok(key);
        }

        let location = source.and_then(|s| self.schema_set.source_maps.locate(s));
        let name_str = self.format_qname(qname);
        Err(SchemaError::structural(
            "src-resolve",
            format!("Attribute group '{}' not found", name_str),
            location,
        ))
    }

    /// Resolve a notation reference (QName → NotationKey)
    pub fn resolve_notation_ref(
        &self,
        qname: &QNameRef,
        source: Option<&SourceRef>,
    ) -> SchemaResult<NotationKey> {
        if let Some(key) = self
            .schema_set
            .lookup_notation(qname.namespace, qname.local_name)
        {
            return Ok(key);
        }

        let location = source.and_then(|s| self.schema_set.source_maps.locate(s));
        let name_str = self.format_qname(qname);
        Err(SchemaError::structural(
            "src-resolve",
            format!("Notation '{}' not found", name_str),
            location,
        ))
    }

    /// Format a QName for error messages
    fn format_qname(&self, qname: &QNameRef) -> String {
        let ns = qname
            .namespace
            .map(|id| self.schema_set.name_table.resolve(id))
            .unwrap_or_default();
        let local = self.schema_set.name_table.resolve(qname.local_name);
        if ns.is_empty() {
            local
        } else {
            format!("{{{}}}{}", ns, local)
        }
    }
}

/// Collected resolution results for a component
#[derive(Debug, Default)]
pub struct ResolvedReferences {
    /// Resolved type reference for elements/attributes
    pub resolved_type: Option<TypeKey>,
    /// Resolved element reference (for element refs)
    pub resolved_ref: Option<ElementKey>,
    /// Resolved substitution group heads
    pub resolved_substitution_groups: Vec<ElementKey>,
    /// Resolved attribute reference (for attribute refs)
    pub resolved_attr_ref: Option<AttributeKey>,
    /// Resolved base type for type definitions
    pub resolved_base_type: Option<TypeKey>,
    /// Resolved item type for list types
    pub resolved_item_type: Option<TypeKey>,
    /// Resolved member types for union types
    pub resolved_member_types: Vec<TypeKey>,
    /// Resolved attribute group references
    pub resolved_attribute_groups: Vec<AttributeGroupKey>,
    /// Resolved model group reference
    pub resolved_group_ref: Option<ModelGroupKey>,
}

/// Statistics from the resolution pass
#[derive(Debug, Default)]
pub struct ResolutionStats {
    /// Number of type references resolved
    pub types_resolved: usize,
    /// Number of element references resolved
    pub elements_resolved: usize,
    /// Number of attribute references resolved
    pub attributes_resolved: usize,
    /// Number of group references resolved
    pub groups_resolved: usize,
    /// Number of attribute group references resolved
    pub attribute_groups_resolved: usize,
    /// Number of notation references resolved
    pub notations_resolved: usize,
    /// Total errors encountered
    pub errors: usize,
}

/// Resolve all references in a schema set
///
/// This function walks all components and resolves QName references
/// to component keys. It should be called after all schemas are
/// parsed and assembled, but before type derivation validation.
///
/// # Errors
///
/// Returns an error if any reference cannot be resolved.
/// The error will contain the source location of the unresolved reference.
pub fn resolve_all_references(schema_set: &mut SchemaSet) -> SchemaResult<ResolutionStats> {
    let mut stats = ResolutionStats::default();
    let mut errors: Vec<SchemaError> = Vec::new();

    // Create resolver (borrows schema_set immutably for lookups)
    // We need to collect keys first, then iterate with mutable access

    // Collect all keys first to avoid borrowing issues
    let element_keys: Vec<ElementKey> = schema_set.arenas.elements.keys().collect();
    let attribute_keys: Vec<AttributeKey> = schema_set.arenas.attributes.keys().collect();
    let simple_type_keys: Vec<SimpleTypeKey> = schema_set.arenas.simple_types.keys().collect();
    let complex_type_keys: Vec<ComplexTypeKey> = schema_set.arenas.complex_types.keys().collect();
    let model_group_keys: Vec<ModelGroupKey> = schema_set.arenas.model_groups.keys().collect();
    let attribute_group_keys: Vec<AttributeGroupKey> =
        schema_set.arenas.attribute_groups.keys().collect();

    // Resolve element references
    for key in element_keys {
        if let Err(e) = resolve_element_references(schema_set, key, &mut stats) {
            errors.push(e);
            stats.errors += 1;
        }
    }

    // Resolve attribute references
    for key in attribute_keys {
        if let Err(e) = resolve_attribute_references(schema_set, key, &mut stats) {
            errors.push(e);
            stats.errors += 1;
        }
    }

    // Resolve simple type references
    for key in simple_type_keys {
        if let Err(e) = resolve_simple_type_references(schema_set, key, &mut stats) {
            errors.push(e);
            stats.errors += 1;
        }
    }

    // Resolve complex type references
    for key in complex_type_keys {
        if let Err(e) = resolve_complex_type_references(schema_set, key, &mut stats) {
            errors.push(e);
            stats.errors += 1;
        }
    }

    // Resolve model group references
    for key in model_group_keys {
        if let Err(e) = resolve_model_group_references(schema_set, key, &mut stats) {
            errors.push(e);
            stats.errors += 1;
        }
    }

    // Resolve attribute group references
    for key in attribute_group_keys {
        if let Err(e) = resolve_attribute_group_references(schema_set, key, &mut stats) {
            errors.push(e);
            stats.errors += 1;
        }
    }

    // Resolve notation references
    // Note: Notation references can appear in:
    // 1. The NOTATION facet in simple type restrictions
    // 2. Element declarations (rare)
    // Currently the data model doesn't store unresolved notation QNames,
    // but we iterate notations here for completeness and future support.
    let notation_keys: Vec<NotationKey> = schema_set.arenas.notations.keys().collect();
    for key in notation_keys {
        if let Err(e) = resolve_notation_references(schema_set, key, &mut stats) {
            errors.push(e);
            stats.errors += 1;
        }
    }

    // If there were errors, return the first one
    if let Some(first_error) = errors.into_iter().next() {
        return Err(first_error);
    }

    Ok(stats)
}

/// Resolve references in an element declaration
fn resolve_element_references(
    schema_set: &mut SchemaSet,
    key: ElementKey,
    stats: &mut ResolutionStats,
) -> SchemaResult<()> {
    // First pass: extract QName references we need to resolve (only Clone-able data)
    // Also check if resolved_type is already set (from inline type assembly)
    let (type_qname, ref_name, substitution_groups, source, already_resolved_type) = {
        let elem = schema_set
            .arenas
            .elements
            .get(key)
            .ok_or_else(|| SchemaError::internal("Element not found in arena"))?;

        // Extract QName from TypeRefResult if it's a QName reference
        let type_qname = match &elem.type_ref {
            Some(TypeRefResult::QName(qname)) => Some(qname.clone()),
            _ => None,
        };

        (
            type_qname,
            elem.ref_name.clone(),
            elem.substitution_group.clone(),
            elem.source.clone(),
            elem.resolved_type, // Already resolved from inline type assembly
        )
    };

    // Create resolver
    let resolver = ReferenceResolver::new(schema_set);

    // Resolve type reference (if not already resolved from inline type)
    let mut resolved_type = if already_resolved_type.is_some() {
        // Type was already resolved during assembly (inline type)
        already_resolved_type
    } else if let Some(ref qname) = type_qname {
        let type_key = resolver.resolve_type_ref(qname, source.as_ref())?;
        stats.types_resolved += 1;
        Some(type_key)
    } else {
        None
    };
    if resolved_type.is_none() && ref_name.is_none() {
        resolved_type = Some(TypeKey::Complex(schema_set.any_type_key()));
    }

    // Resolve element reference (for <xs:element ref="...">)
    let resolved_ref = if let Some(ref qname) = ref_name {
        let elem_key = resolver.resolve_element_ref(qname, source.as_ref())?;
        stats.elements_resolved += 1;
        Some(elem_key)
    } else {
        None
    };

    // Resolve substitution groups
    let mut resolved_subst_groups = Vec::with_capacity(substitution_groups.len());
    for qname in &substitution_groups {
        let elem_key = resolver.resolve_element_ref(qname, source.as_ref())?;
        stats.elements_resolved += 1;
        resolved_subst_groups.push(elem_key);
    }

    // Store resolved references back
    if let Some(elem) = schema_set.arenas.elements.get_mut(key) {
        elem.resolved_type = resolved_type;
        elem.resolved_ref = resolved_ref;
        elem.resolved_substitution_groups = resolved_subst_groups;
    }

    Ok(())
}

/// Resolve references in an attribute declaration
fn resolve_attribute_references(
    schema_set: &mut SchemaSet,
    key: AttributeKey,
    stats: &mut ResolutionStats,
) -> SchemaResult<()> {
    // First pass: extract QName references we need to resolve
    // Also check if resolved_type is already set (from inline type assembly)
    let (type_qname, ref_name, source, already_resolved_type) = {
        let attr = schema_set
            .arenas
            .attributes
            .get(key)
            .ok_or_else(|| SchemaError::internal("Attribute not found in arena"))?;

        // Extract QName from TypeRefResult if it's a QName reference
        let type_qname = match &attr.type_ref {
            Some(TypeRefResult::QName(qname)) => Some(qname.clone()),
            _ => None,
        };

        (type_qname, attr.ref_name.clone(), attr.source.clone(), attr.resolved_type)
    };

    // Create resolver
    let resolver = ReferenceResolver::new(schema_set);

    // Resolve type reference (if not already resolved from inline type)
    let resolved_type = if already_resolved_type.is_some() {
        // Type was already resolved during assembly (inline type)
        already_resolved_type
    } else if let Some(ref qname) = type_qname {
        let type_key = resolver.resolve_type_ref(qname, source.as_ref())?;
        stats.types_resolved += 1;
        Some(type_key)
    } else {
        None
    };

    // Resolve attribute reference (for <xs:attribute ref="...">)
    let resolved_ref = if let Some(ref qname) = ref_name {
        let attr_key = resolver.resolve_attribute_ref(qname, source.as_ref())?;
        stats.attributes_resolved += 1;
        Some(attr_key)
    } else {
        None
    };

    // Store resolved references back
    if let Some(attr) = schema_set.arenas.attributes.get_mut(key) {
        attr.resolved_type = resolved_type;
        attr.resolved_ref = resolved_ref;
    }

    Ok(())
}

/// Resolve references in a simple type definition
fn resolve_simple_type_references(
    schema_set: &mut SchemaSet,
    key: SimpleTypeKey,
    stats: &mut ResolutionStats,
) -> SchemaResult<()> {
    // First pass: extract QName references we need to resolve
    // Also get already resolved types from assembly (for inline types)
    let (base_qname, item_qname, member_qnames, source, already_resolved_base, already_resolved_item, already_resolved_members) = {
        let type_def = schema_set
            .arenas
            .simple_types
            .get(key)
            .ok_or_else(|| SchemaError::internal("Simple type not found in arena"))?;

        // Extract QName from TypeRefResult if it's a QName reference
        let base_qname = match &type_def.base_type {
            Some(TypeRefResult::QName(qname)) => Some(qname.clone()),
            _ => None,
        };
        let item_qname = match &type_def.item_type {
            Some(TypeRefResult::QName(qname)) => Some(qname.clone()),
            _ => None,
        };
        let member_qnames: Vec<_> = type_def
            .member_types
            .iter()
            .filter_map(|tr| match tr {
                TypeRefResult::QName(qname) => Some(qname.clone()),
                _ => None,
            })
            .collect();

        (
            base_qname,
            item_qname,
            member_qnames,
            type_def.source.clone(),
            type_def.resolved_base_type,
            type_def.resolved_item_type,
            type_def.resolved_member_types.clone(),
        )
    };

    // Create resolver
    let resolver = ReferenceResolver::new(schema_set);

    // Resolve base type reference (for restriction) - if not already resolved
    let resolved_base = if already_resolved_base.is_some() {
        already_resolved_base
    } else if let Some(ref qname) = base_qname {
        let type_key = resolver.resolve_type_ref(qname, source.as_ref())?;
        stats.types_resolved += 1;
        Some(type_key)
    } else {
        None
    };

    // Resolve item type reference (for list) - if not already resolved
    let resolved_item = if already_resolved_item.is_some() {
        already_resolved_item
    } else if let Some(ref qname) = item_qname {
        let type_key = resolver.resolve_type_ref(qname, source.as_ref())?;
        stats.types_resolved += 1;
        Some(type_key)
    } else {
        None
    };

    // Resolve member type references (for union)
    // Start with already resolved members (from inline types), then add QName resolved ones
    let mut resolved_members = already_resolved_members;
    for qname in &member_qnames {
        let type_key = resolver.resolve_type_ref(qname, source.as_ref())?;
        stats.types_resolved += 1;
        resolved_members.push(type_key);
    }

    // Store resolved references back
    if let Some(type_def) = schema_set.arenas.simple_types.get_mut(key) {
        type_def.resolved_base_type = resolved_base;
        type_def.resolved_item_type = resolved_item;
        type_def.resolved_member_types = resolved_members;
    }

    Ok(())
}

/// Resolve references in a complex type definition
fn resolve_complex_type_references(
    schema_set: &mut SchemaSet,
    key: ComplexTypeKey,
    stats: &mut ResolutionStats,
) -> SchemaResult<()> {
    use crate::arenas::ResolvedAttributeUse;

    // First pass: extract QName references we need to resolve
    // Also get already resolved base type from assembly (for inline types)
    let (base_qname, attribute_groups, attribute_uses, source, already_resolved_base) = {
        let type_def = schema_set
            .arenas
            .complex_types
            .get(key)
            .ok_or_else(|| SchemaError::internal("Complex type not found in arena"))?;

        // Extract QName from TypeRefResult if it's a QName reference
        let base_qname = match &type_def.base_type {
            Some(TypeRefResult::QName(qname)) => Some(qname.clone()),
            _ => None,
        };

        // Extract attribute use info for resolution
        let attribute_uses: Vec<_> = type_def.attributes.iter().map(|attr_use| {
            let type_qname = match &attr_use.attribute.type_ref {
                Some(TypeRefResult::QName(qname)) => Some(qname.clone()),
                _ => None,
            };
            (attr_use.attribute.ref_name.clone(), type_qname, attr_use.attribute.source.clone())
        }).collect();

        (
            base_qname,
            type_def.attribute_groups.clone(),
            attribute_uses,
            type_def.source.clone(),
            type_def.resolved_base_type,
        )
    };

    // Create resolver
    let resolver = ReferenceResolver::new(schema_set);

    // Resolve base type reference - if not already resolved
    let resolved_base = if already_resolved_base.is_some() {
        already_resolved_base
    } else if let Some(ref qname) = base_qname {
        let type_key = resolver.resolve_type_ref(qname, source.as_ref())?;
        stats.types_resolved += 1;
        Some(type_key)
    } else {
        None
    };

    // Resolve attribute group references
    let mut resolved_attr_groups = Vec::with_capacity(attribute_groups.len());
    for qname in &attribute_groups {
        let group_key = resolver.resolve_attribute_group_ref(qname, source.as_ref())?;
        stats.attribute_groups_resolved += 1;
        resolved_attr_groups.push(group_key);
    }

    // Resolve attribute use references
    let mut resolved_attrs = Vec::with_capacity(attribute_uses.len());
    for (ref_name, type_qname, attr_source) in &attribute_uses {
        let resolved_type = if let Some(ref qname) = type_qname {
            let type_key = resolver.resolve_type_ref(qname, attr_source.as_ref())?;
            stats.types_resolved += 1;
            Some(type_key)
        } else {
            None
        };
        let resolved_ref = if let Some(ref qname) = ref_name {
            let attr_key = resolver.resolve_attribute_ref(qname, attr_source.as_ref())?;
            stats.attributes_resolved += 1;
            Some(attr_key)
        } else {
            None
        };
        resolved_attrs.push(ResolvedAttributeUse {
            resolved_type,
            resolved_ref,
        });
    }

    // Store resolved references back
    if let Some(type_def) = schema_set.arenas.complex_types.get_mut(key) {
        type_def.resolved_base_type = resolved_base;
        type_def.resolved_attribute_groups = resolved_attr_groups;
        type_def.resolved_attributes = resolved_attrs;
    }

    Ok(())
}

/// Resolve references in a model group definition
fn resolve_model_group_references(
    schema_set: &mut SchemaSet,
    key: ModelGroupKey,
    stats: &mut ResolutionStats,
) -> SchemaResult<()> {
    use crate::arenas::ResolvedParticleTerm;
    use crate::parser::frames::ParticleTerm;

    // Get the model group data to read references
    let group = schema_set
        .arenas
        .model_groups
        .get(key)
        .ok_or_else(|| SchemaError::internal("Model group not found in arena"))?;

    // Clone references we need to resolve
    let ref_name = group.ref_name.clone();
    let source = group.source.clone();
    let particles_clone = group.particles.clone();

    // Read existing resolved_particles BEFORE building new ones,
    // so we can preserve inline-resolved types from Phase 3.
    let existing_resolved: Vec<_> = group.resolved_particles.clone();
    let existing_particle_types = group.resolved_particle_types.clone();

    // Extract particle info for resolution
    let particle_info: Vec<_> = group.particles.iter().map(|p| {
        match &p.term {
            ParticleTerm::Element(elem) => {
                let type_qname = match &elem.type_ref {
                    Some(TypeRefResult::QName(qname)) => Some(qname.clone()),
                    _ => None,
                };
                (0, elem.ref_name.clone(), type_qname, p.source.clone())
            }
            ParticleTerm::Group(grp) => {
                (1, grp.ref_name.clone(), None, p.source.clone())
            }
            ParticleTerm::Any(_) => {
                (2, None, None, p.source.clone())
            }
        }
    }).collect();

    // Create resolver
    let resolver = ReferenceResolver::new(schema_set);

    // Resolve group reference (for <xs:group ref="...">)
    let resolved_ref = if let Some(ref qname) = ref_name {
        let group_key = resolver.resolve_group_ref(qname, source.as_ref())?;
        stats.groups_resolved += 1;
        Some(group_key)
    } else {
        None
    };

    // Resolve particle references
    let mut resolved_particles = Vec::with_capacity(particle_info.len());
    for (i, (kind, elem_or_group_ref, type_qname, particle_source)) in particle_info.iter().enumerate() {
        match kind {
            0 => {
                // Element particle — preserve inline-resolved type from Phase 3
                let already_resolved_type = existing_resolved.get(i).and_then(|rp| {
                    if let ResolvedParticleTerm::Element { resolved_type: Some(key), .. } = rp {
                        Some(*key)
                    } else {
                        None
                    }
                });
                let resolved_type = if let Some(key) = already_resolved_type {
                    Some(key)
                } else if let Some(ref qname) = type_qname {
                    let type_key = resolver.resolve_type_ref(qname, particle_source.as_ref())?;
                    stats.types_resolved += 1;
                    Some(type_key)
                } else {
                    None
                };
                let resolved_elem_ref = if let Some(ref qname) = elem_or_group_ref {
                    let elem_key = resolver.resolve_element_ref(qname, particle_source.as_ref())?;
                    stats.elements_resolved += 1;
                    Some(elem_key)
                } else {
                    None
                };
                resolved_particles.push(ResolvedParticleTerm::Element {
                    resolved_type,
                    resolved_ref: resolved_elem_ref,
                });
            }
            1 => {
                // Group particle
                let resolved_group_ref = if let Some(ref qname) = elem_or_group_ref {
                    let grp_key = resolver.resolve_group_ref(qname, particle_source.as_ref())?;
                    stats.groups_resolved += 1;
                    Some(grp_key)
                } else {
                    None
                };
                resolved_particles.push(ResolvedParticleTerm::Group {
                    resolved_ref: resolved_group_ref,
                });
            }
            _ => {
                // Wildcard
                resolved_particles.push(ResolvedParticleTerm::Any);
            }
        }
    }

    // Build flat-indexed resolved_particle_types (including nested inline groups)
    let mut resolved_particle_types = Vec::new();
    let mut flat_idx = 0;
    resolve_model_group_particle_types_recursive(
        &particles_clone,
        &existing_particle_types,
        &resolver,
        &mut flat_idx,
        &mut resolved_particle_types,
        stats,
    )?;

    // Store resolved references back
    if let Some(group) = schema_set.arenas.model_groups.get_mut(key) {
        group.resolved_ref = resolved_ref;
        group.resolved_particles = resolved_particles;
        group.resolved_particle_types = resolved_particle_types;
    }

    Ok(())
}

/// Recursive helper: resolve types for model group particles in depth-first order
fn resolve_model_group_particle_types_recursive(
    particles: &[crate::parser::frames::ParticleResult],
    existing_types: &[Option<TypeKey>],
    resolver: &ReferenceResolver,
    flat_idx: &mut usize,
    resolved_types: &mut Vec<Option<TypeKey>>,
    stats: &mut ResolutionStats,
) -> SchemaResult<()> {
    use crate::parser::frames::ParticleTerm;

    for particle in particles {
        match &particle.term {
            ParticleTerm::Element(elem) => {
                let idx = *flat_idx;
                *flat_idx += 1;
                // Preserve inline-resolved type from Phase 3
                let already_resolved = existing_types.get(idx).copied().flatten();
                let resolved_type = if let Some(key) = already_resolved {
                    Some(key)
                } else {
                    // Try QName resolution
                    match &elem.type_ref {
                        Some(TypeRefResult::QName(qname)) => {
                            let type_key =
                                resolver.resolve_type_ref(qname, particle.source.as_ref())?;
                            stats.types_resolved += 1;
                            Some(type_key)
                        }
                        _ => None,
                    }
                };
                while resolved_types.len() <= idx {
                    resolved_types.push(None);
                }
                resolved_types[idx] = resolved_type;
            }
            ParticleTerm::Group(group_def) if group_def.ref_name.is_none() => {
                resolve_model_group_particle_types_recursive(
                    &group_def.particles,
                    existing_types,
                    resolver,
                    flat_idx,
                    resolved_types,
                    stats,
                )?;
            }
            _ => {} // Skip group refs and wildcards
        }
    }
    Ok(())
}

/// Resolve references in an attribute group definition
fn resolve_attribute_group_references(
    schema_set: &mut SchemaSet,
    key: AttributeGroupKey,
    stats: &mut ResolutionStats,
) -> SchemaResult<()> {
    use crate::arenas::ResolvedAttributeUse;

    // Get the attribute group data to read references
    let group = schema_set
        .arenas
        .attribute_groups
        .get(key)
        .ok_or_else(|| SchemaError::internal("Attribute group not found in arena"))?;

    // Clone references we need to resolve
    let ref_name = group.ref_name.clone();
    let nested_groups = group.attribute_groups.clone();
    let source = group.source.clone();

    // Extract attribute use info for resolution
    let attribute_uses: Vec<_> = group.attributes.iter().map(|attr_use| {
        let type_qname = match &attr_use.attribute.type_ref {
            Some(TypeRefResult::QName(qname)) => Some(qname.clone()),
            _ => None,
        };
        (attr_use.attribute.ref_name.clone(), type_qname, attr_use.attribute.source.clone())
    }).collect();

    // Create resolver
    let resolver = ReferenceResolver::new(schema_set);

    // Resolve group reference (for <xs:attributeGroup ref="...">)
    let resolved_ref = if let Some(ref qname) = ref_name {
        let group_key = resolver.resolve_attribute_group_ref(qname, source.as_ref())?;
        stats.attribute_groups_resolved += 1;
        Some(group_key)
    } else {
        None
    };

    // Resolve nested attribute group references
    let mut resolved_nested = Vec::with_capacity(nested_groups.len());
    for qname in &nested_groups {
        let group_key = resolver.resolve_attribute_group_ref(qname, source.as_ref())?;
        stats.attribute_groups_resolved += 1;
        resolved_nested.push(group_key);
    }

    // Resolve attribute use references
    let mut resolved_attrs = Vec::with_capacity(attribute_uses.len());
    for (ref_name_opt, type_qname, attr_source) in &attribute_uses {
        let resolved_type = if let Some(ref qname) = type_qname {
            let type_key = resolver.resolve_type_ref(qname, attr_source.as_ref())?;
            stats.types_resolved += 1;
            Some(type_key)
        } else {
            None
        };
        let resolved_attr_ref = if let Some(ref qname) = ref_name_opt {
            let attr_key = resolver.resolve_attribute_ref(qname, attr_source.as_ref())?;
            stats.attributes_resolved += 1;
            Some(attr_key)
        } else {
            None
        };
        resolved_attrs.push(ResolvedAttributeUse {
            resolved_type,
            resolved_ref: resolved_attr_ref,
        });
    }

    // Store resolved references back
    if let Some(group) = schema_set.arenas.attribute_groups.get_mut(key) {
        group.resolved_ref = resolved_ref;
        group.resolved_attribute_groups = resolved_nested;
        group.resolved_attributes = resolved_attrs;
    }

    Ok(())
}

/// Resolve references in a notation declaration
///
/// Currently notations don't have internal references that need resolution,
/// but this function is provided for completeness and to track notation
/// processing in statistics. In the future, if notation references are
/// added to the data model, resolution logic would go here.
fn resolve_notation_references(
    schema_set: &mut SchemaSet,
    key: NotationKey,
    stats: &mut ResolutionStats,
) -> SchemaResult<()> {
    // Verify the notation exists
    let _notation = schema_set
        .arenas
        .notations
        .get(key)
        .ok_or_else(|| SchemaError::internal("Notation not found in arena"))?;

    // Track notation processing
    stats.notations_resolved += 1;

    // Currently notations don't have unresolved references in the data model.
    // Future: If NOTATION facet references or element notation attributes
    // are stored as QNames, resolve them here.

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::namespace::table::well_known;

    #[test]
    fn test_reference_resolver_creation() {
        let schema_set = SchemaSet::new();
        let _resolver = ReferenceResolver::new(&schema_set);
    }

    #[test]
    fn test_resolve_builtin_type() {
        let schema_set = SchemaSet::new();
        let resolver = ReferenceResolver::new(&schema_set);

        // Create a QNameRef for xs:string
        let string_name = schema_set.name_table.get("string").unwrap();
        let qname = QNameRef {
            prefix: None,
            local_name: string_name,
            namespace: Some(well_known::XS_NAMESPACE),
        };

        let result = resolver.resolve_type_ref(&qname, None);
        assert!(result.is_ok(), "Should resolve xs:string");

        if let Ok(TypeKey::Simple(key)) = result {
            // Verify it's the string type
            let string_key = schema_set.builtin_types().string;
            assert_eq!(key, string_key);
        } else {
            panic!("Expected Simple type key");
        }
    }

    #[test]
    fn test_resolve_builtin_integer() {
        let schema_set = SchemaSet::new();
        let resolver = ReferenceResolver::new(&schema_set);

        // Create a QNameRef for xs:integer
        let integer_name = schema_set.name_table.get("integer").unwrap();
        let qname = QNameRef {
            prefix: None,
            local_name: integer_name,
            namespace: Some(well_known::XS_NAMESPACE),
        };

        let result = resolver.resolve_type_ref(&qname, None);
        assert!(result.is_ok(), "Should resolve xs:integer");
    }

    #[test]
    fn test_resolve_builtin_any_type() {
        let schema_set = SchemaSet::new();
        let resolver = ReferenceResolver::new(&schema_set);

        let any_type_name = schema_set.name_table.get("anyType").unwrap();
        let qname = QNameRef {
            prefix: None,
            local_name: any_type_name,
            namespace: Some(well_known::XS_NAMESPACE),
        };

        let result = resolver.resolve_type_ref(&qname, None);
        assert!(result.is_ok(), "Should resolve xs:anyType");

        if let Ok(TypeKey::Complex(key)) = result {
            assert_eq!(key, schema_set.builtin_types().any_type);
        } else {
            panic!("Expected Complex type key");
        }
    }

    #[test]
    fn test_resolve_unknown_type_error() {
        let schema_set = SchemaSet::new();

        // Add the name before creating the resolver to avoid borrow conflicts
        let unknown_name = schema_set.name_table.add("nonExistentType");

        let resolver = ReferenceResolver::new(&schema_set);
        let qname = QNameRef {
            prefix: None,
            local_name: unknown_name,
            namespace: Some(well_known::XS_NAMESPACE),
        };

        let result = resolver.resolve_type_ref(&qname, None);
        assert!(result.is_err(), "Should fail for unknown type");
    }

    #[test]
    fn test_format_qname_with_namespace() {
        let schema_set = SchemaSet::new();
        let resolver = ReferenceResolver::new(&schema_set);

        // "string" should already exist from built-in types initialization
        let string_name = schema_set.name_table.get("string").unwrap();
        let qname = QNameRef {
            prefix: None,
            local_name: string_name,
            namespace: Some(well_known::XS_NAMESPACE),
        };

        let formatted = resolver.format_qname(&qname);
        assert!(formatted.contains("string"));
        assert!(formatted.contains("XMLSchema"));
    }

    #[test]
    fn test_format_qname_without_namespace() {
        let schema_set = SchemaSet::new();

        // Add the name before creating the resolver to avoid borrow conflicts
        let local_name = schema_set.name_table.add("localType");

        let resolver = ReferenceResolver::new(&schema_set);
        let qname = QNameRef {
            prefix: None,
            local_name,
            namespace: None,
        };

        let formatted = resolver.format_qname(&qname);
        assert_eq!(formatted, "localType");
    }

    #[test]
    fn test_resolution_stats_default() {
        let stats = ResolutionStats::default();
        assert_eq!(stats.types_resolved, 0);
        assert_eq!(stats.elements_resolved, 0);
        assert_eq!(stats.errors, 0);
    }

    #[test]
    fn test_resolve_all_references_empty_schema() {
        let mut schema_set = SchemaSet::new();

        // Should succeed with empty schema (only built-in types)
        let result = resolve_all_references(&mut schema_set);
        assert!(result.is_ok());

        let stats = result.unwrap();
        // Should resolve notations (0 user-defined notations)
        assert_eq!(stats.errors, 0);
    }

    #[test]
    fn test_resolve_user_defined_element_with_builtin_type() {
        use crate::arenas::ElementDeclData;
        use crate::parser::frames::QNameRef;
        use crate::schema::model::DerivationSet;

        let mut schema_set = SchemaSet::new();

        // Get names
        let elem_name = schema_set.name_table.add("myElement");
        let string_name = schema_set.name_table.get("string").unwrap();

        // Create an element with type="xs:string"
        let type_ref = TypeRefResult::QName(QNameRef {
            prefix: None,
            local_name: string_name,
            namespace: Some(well_known::XS_NAMESPACE),
        });

        let elem_data = ElementDeclData {
            name: Some(elem_name),
            target_namespace: None,
            ref_name: None,
            type_ref: Some(type_ref),
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
            resolved_type: None,
            resolved_ref: None,
            resolved_substitution_groups: Vec::new(),
        };

        let elem_key = schema_set.arenas.alloc_element(elem_data);

        // Resolve references
        let result = resolve_all_references(&mut schema_set);
        assert!(result.is_ok(), "Resolution should succeed: {:?}", result);

        // Verify the type was resolved
        let elem = schema_set.arenas.elements.get(elem_key).unwrap();
        assert!(elem.resolved_type.is_some(), "Type should be resolved");

        if let Some(TypeKey::Simple(key)) = elem.resolved_type {
            assert_eq!(key, schema_set.builtin_types().string);
        } else {
            panic!("Expected Simple type key for xs:string");
        }
    }

    #[test]
    fn test_resolve_attribute_with_builtin_type() {
        use crate::arenas::AttributeDeclData;
        use crate::parser::frames::QNameRef;

        let mut schema_set = SchemaSet::new();

        // Get names
        let attr_name = schema_set.name_table.add("myAttribute");
        let integer_name = schema_set.name_table.get("integer").unwrap();

        // Create an attribute with type="xs:integer"
        let type_ref = TypeRefResult::QName(QNameRef {
            prefix: None,
            local_name: integer_name,
            namespace: Some(well_known::XS_NAMESPACE),
        });

        let attr_data = AttributeDeclData {
            name: Some(attr_name),
            target_namespace: None,
            ref_name: None,
            type_ref: Some(type_ref),
            inline_type: None,
            default_value: None,
            fixed_value: None,
            use_kind: None,
            form: None,
            inheritable: false,
            id: None,
            annotation: None,
            source: None,
            resolved_type: None,
            resolved_ref: None,
        };

        let attr_key = schema_set.arenas.alloc_attribute(attr_data);

        // Resolve references
        let result = resolve_all_references(&mut schema_set);
        assert!(result.is_ok());

        // Verify the type was resolved
        let attr = schema_set.arenas.attributes.get(attr_key).unwrap();
        assert!(attr.resolved_type.is_some(), "Type should be resolved");
    }

    #[test]
    fn test_resolve_element_already_resolved_inline_type() {
        use crate::arenas::ElementDeclData;
        use crate::schema::model::DerivationSet;

        let mut schema_set = SchemaSet::new();

        let elem_name = schema_set.name_table.add("myElement");

        // Pre-resolved type (as if from inline type assembly)
        let string_key = schema_set.builtin_types().string;

        let elem_data = ElementDeclData {
            name: Some(elem_name),
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
            source: None,
            // Already resolved (from inline type assembly)
            resolved_type: Some(TypeKey::Simple(string_key)),
            resolved_ref: None,
            resolved_substitution_groups: Vec::new(),
        };

        let elem_key = schema_set.arenas.alloc_element(elem_data);

        // Resolve references - should preserve the pre-resolved type
        let result = resolve_all_references(&mut schema_set);
        assert!(result.is_ok());

        // Verify the pre-resolved type was preserved
        let elem = schema_set.arenas.elements.get(elem_key).unwrap();
        assert!(elem.resolved_type.is_some());
        assert_eq!(elem.resolved_type, Some(TypeKey::Simple(string_key)));
    }

    #[test]
    fn test_resolver_preserves_inline_resolved_type_in_model_group() {
        use crate::arenas::{ModelGroupData, ResolvedParticleTerm};
        use crate::parser::frames::{Compositor, ElementFrameResult, ParticleResult, ParticleTerm};
        use crate::schema::model::DerivationSet;

        let mut schema_set = SchemaSet::new();

        let elem_name = schema_set.name_table.add("detail");
        let group_name = schema_set.name_table.add("myGroup");

        // Pre-resolved type (as if from inline type assembly)
        let string_key = schema_set.builtin_types().string;

        // Create a model group with one element particle
        let group_data = ModelGroupData {
            name: Some(group_name),
            target_namespace: None,
            ref_name: None,
            compositor: Some(Compositor::Sequence),
            particles: vec![ParticleResult {
                term: ParticleTerm::Element(ElementFrameResult {
                    name: Some(elem_name),
                    ref_name: None,
                    target_namespace: None,
                    type_ref: None, // No QName type ref
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
            // Pre-populate with inline-resolved type
            resolved_particles: vec![ResolvedParticleTerm::Element {
                resolved_type: Some(TypeKey::Simple(string_key)),
                resolved_ref: None,
            }],
            resolved_particle_types: vec![Some(TypeKey::Simple(string_key))],
            resolved_particle_elements: Vec::new(),
        };

        let group_key = schema_set.arenas.alloc_model_group(group_data);

        // Resolve references - should preserve the pre-resolved type
        let result = resolve_all_references(&mut schema_set);
        assert!(result.is_ok());

        // Verify the pre-resolved type was preserved
        let group = schema_set.arenas.model_groups.get(group_key).unwrap();
        assert_eq!(group.resolved_particles.len(), 1);
        match &group.resolved_particles[0] {
            ResolvedParticleTerm::Element {
                resolved_type: Some(TypeKey::Simple(key)),
                ..
            } => {
                assert_eq!(*key, string_key, "Inline-resolved type should be preserved");
            }
            other => panic!(
                "Expected Element with pre-resolved type, got {:?}",
                other
            ),
        }
    }
}
