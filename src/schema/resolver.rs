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
use crate::schema::composition::ComponentKind;
use crate::schema::SchemaSet;
use crate::parser::frames::SimpleTypeVariety;

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

        // 3. Not found - error with provenance note
        let location = source.and_then(|s| self.schema_set.source_maps.locate(s));
        let name_str = self.format_qname(qname);
        // Try both simple and complex type provenance
        let note = {
            let simple_note = self.schema_set.format_provenance_note(
                ComponentKind::SimpleType,
                qname.namespace,
                qname.local_name,
            );
            if simple_note.is_empty() {
                self.schema_set.format_provenance_note(
                    ComponentKind::ComplexType,
                    qname.namespace,
                    qname.local_name,
                )
            } else {
                simple_note
            }
        };
        Err(SchemaError::structural(
            "src-resolve",
            format!("Type '{}' not found{}", name_str, note),
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

    /// Resolve a component reference by looking up in the schema set's namespace
    /// tables, returning a `src-resolve` error if not found.
    ///
    /// When a lookup fails the error message is enriched with provenance
    /// information (e.g. "originally in base.xsd, redefined by main.xsd")
    /// when available.
    fn resolve_ref<K: Copy>(
        &self,
        qname: &QNameRef,
        source: Option<&SourceRef>,
        kind_label: &str,
        component_kind: ComponentKind,
        lookup: impl FnOnce(&SchemaSet, Option<NameId>, NameId) -> Option<K>,
    ) -> SchemaResult<K> {
        if let Some(key) = lookup(self.schema_set, qname.namespace, qname.local_name) {
            return Ok(key);
        }
        let location = source.and_then(|s| self.schema_set.source_maps.locate(s));
        let name_str = self.format_qname(qname);
        let note = self.schema_set.format_provenance_note(
            component_kind,
            qname.namespace,
            qname.local_name,
        );
        Err(SchemaError::structural(
            "src-resolve",
            format!("{} '{}' not found{}", kind_label, name_str, note),
            location,
        ))
    }

    /// Resolve an element reference (QName → ElementKey)
    pub fn resolve_element_ref(
        &self,
        qname: &QNameRef,
        source: Option<&SourceRef>,
    ) -> SchemaResult<ElementKey> {
        self.resolve_ref(qname, source, "Element", ComponentKind::Element, SchemaSet::lookup_element)
    }

    /// Resolve an attribute reference (QName → AttributeKey)
    pub fn resolve_attribute_ref(
        &self,
        qname: &QNameRef,
        source: Option<&SourceRef>,
    ) -> SchemaResult<AttributeKey> {
        self.resolve_ref(qname, source, "Attribute", ComponentKind::Attribute, SchemaSet::lookup_attribute)
    }

    /// Resolve a model group reference (QName → ModelGroupKey)
    pub fn resolve_group_ref(
        &self,
        qname: &QNameRef,
        source: Option<&SourceRef>,
    ) -> SchemaResult<ModelGroupKey> {
        self.resolve_ref(qname, source, "Group", ComponentKind::ModelGroup, SchemaSet::lookup_model_group)
    }

    /// Resolve an attribute group reference (QName → AttributeGroupKey)
    pub fn resolve_attribute_group_ref(
        &self,
        qname: &QNameRef,
        source: Option<&SourceRef>,
    ) -> SchemaResult<AttributeGroupKey> {
        self.resolve_ref(qname, source, "Attribute group", ComponentKind::AttributeGroup, SchemaSet::lookup_attribute_group)
    }

    /// Resolve a notation reference (QName → NotationKey)
    pub fn resolve_notation_ref(
        &self,
        qname: &QNameRef,
        source: Option<&SourceRef>,
    ) -> SchemaResult<NotationKey> {
        self.resolve_ref(qname, source, "Notation", ComponentKind::Notation, SchemaSet::lookup_notation)
    }

    /// Format a QName for error messages
    fn format_qname(&self, qname: &QNameRef) -> String {
        format_resolved_qname(&self.schema_set.name_table, qname.namespace, qname.local_name)
    }
}

/// Format a resolved QName (namespace + local name) for error messages.
pub(crate) fn format_resolved_qname(
    name_table: &crate::namespace::NameTable,
    namespace: Option<crate::ids::NameId>,
    local_name: crate::ids::NameId,
) -> String {
    let local = name_table.resolve(local_name);
    if let Some(ns_id) = namespace {
        let ns = name_table.resolve(ns_id);
        if ns.is_empty() {
            local
        } else {
            format!("{{{}}}{}", ns, local)
        }
    } else {
        local
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
    for key in &element_keys {
        if let Err(e) = resolve_element_references(schema_set, *key, &mut stats) {
            errors.push(e);
            stats.errors += 1;
        }
    }

    // Post-pass: inherit type from substitution group head for elements that have a
    // substitutionGroup but no explicit type (§3.3.2.1 rule 3).  We iterate until
    // stable so that transitive chains (A → B → C where none have an explicit type)
    // are fully resolved.
    if errors.is_empty() {
        loop {
            let mut changed = false;
            for &key in &element_keys {
                let (needs_type, subst_groups) = {
                    let elem = schema_set.arenas.elements.get(key).unwrap();
                    (
                        elem.resolved_type.is_none()
                            && elem.resolved_ref.is_none()
                            && !elem.resolved_substitution_groups.is_empty(),
                        elem.resolved_substitution_groups.clone(),
                    )
                };
                if needs_type {
                    for &head_key in &subst_groups {
                        if let Some(head_type) = schema_set
                            .arenas
                            .elements
                            .get(head_key)
                            .and_then(|h| h.resolved_type)
                        {
                            let elem = schema_set.arenas.elements.get_mut(key).unwrap();
                            assign_element_type(elem, head_type);
                            changed = true;
                            break;
                        }
                    }
                }
            }
            if !changed {
                break;
            }
        }
        // Final anyType fallback for elements that still have no type (e.g., circular
        // substitution group chains or heads that themselves have no type).
        let any_type = TypeKey::Complex(schema_set.any_type_key());
        for &key in &element_keys {
            if let Some(elem) = schema_set.arenas.elements.get_mut(key) {
                if elem.resolved_type.is_none() && elem.resolved_ref.is_none() {
                    assign_element_type(elem, any_type);
                }
            }
        }
    }

    // Resolve XSD 1.1 identity constraint @ref references on top-level elements.
    // This runs after element resolution so ICs from all elements are registered.
    for &key in &element_keys {
        let pending = std::mem::take(&mut schema_set.arenas.elements[key].pending_ic_refs);
        if !pending.is_empty() {
            let target_ns = schema_set.arenas.elements[key].target_namespace;
            for (kind, ref_name, source) in pending {
                match crate::schema::inline::resolve_ic_ref(
                    kind, &ref_name, source.as_ref(), target_ns, schema_set,
                ) {
                    Ok(target_key) => {
                        schema_set.arenas.elements[key].identity_constraints.push(target_key);
                    }
                    Err(e) => {
                        errors.push(e);
                        stats.errors += 1;
                    }
                }
            }
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
    for &key in &complex_type_keys {
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

    // Resolve schema-level defaultAttributes and inject into complex types
    // Step A: Pre-resolve document-level default attribute groups
    let mut doc_default_attr_groups: Vec<Option<AttributeGroupKey>> =
        Vec::with_capacity(schema_set.documents.len());
    for doc in &schema_set.documents {
        if let Some(ref qname) = doc.default_attributes {
            if let Some(key) = schema_set.lookup_attribute_group(
                qname.namespace_uri,
                qname.local_name,
            ) {
                doc_default_attr_groups.push(Some(key));
                stats.attribute_groups_resolved += 1;
            } else {
                // Step B: Error for unresolvable defaultAttributes
                let location = doc.source.as_ref().and_then(|s| schema_set.source_maps.locate(s));
                let name_str = format_resolved_qname(
                    &schema_set.name_table,
                    qname.namespace_uri,
                    qname.local_name,
                );
                errors.push(SchemaError::structural(
                    "src-resolve",
                    format!("Attribute group '{}' not found", name_str),
                    location,
                ));
                stats.errors += 1;
                doc_default_attr_groups.push(None);
            }
        } else {
            doc_default_attr_groups.push(None);
        }
    }

    // Step C: Inject default attribute group into applicable complex types
    for &key in &complex_type_keys {
        let doc_id = {
            let type_def = match schema_set.arenas.complex_types.get(key) {
                Some(td) => td,
                None => continue,
            };
            if !type_def.default_attributes_apply {
                continue;
            }
            match type_def.source.as_ref() {
                // Use defaults_doc() so override children read the
                // overridden document's defaultAttributes per §4.2.5.
                Some(src) => src.defaults_doc(),
                None => continue, // synthesized types have no source
            }
        };
        if let Some(Some(group_key)) = doc_default_attr_groups.get(doc_id as usize) {
            let group_key = *group_key;
            let type_def = schema_set.arenas.complex_types.get_mut(key).unwrap();
            if !type_def.resolved_attribute_groups.contains(&group_key) {
                type_def.resolved_attribute_groups.push(group_key);
            }
        }
    }

    // If there were errors, return the first one
    if let Some(first_error) = errors.into_iter().next() {
        return Err(first_error);
    }

    Ok(stats)
}

/// Set an element's resolved type and propagate to XSD 1.1 type alternatives
/// that have no explicit type (they use the element's declared type as fallback).
fn assign_element_type(elem: &mut crate::arenas::ElementDeclData, type_key: TypeKey) {
    elem.resolved_type = Some(type_key);
    #[cfg(feature = "xsd11")]
    for alt in &mut elem.alternatives {
        if alt.resolved_type.is_none() && alt.type_ref.is_none() {
            alt.resolved_type = Some(type_key);
        }
    }
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
    // Only fall back to anyType when there is no substitution group.
    // Elements with a substitutionGroup but no explicit type inherit the
    // head's type in the post-pass inside resolve_all_references (§3.3.2.1 rule 3).
    if resolved_type.is_none() && ref_name.is_none() && substitution_groups.is_empty() {
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

    // Resolve alternative type references (XSD 1.1)
    #[cfg(feature = "xsd11")]
    let resolved_alt_types = {
        let elem = schema_set.arenas.elements.get(key)
            .ok_or_else(|| SchemaError::internal("Element not found in arena"))?;
        let mut alt_types: Vec<Option<TypeKey>> = Vec::with_capacity(elem.alternatives.len());
        for alt in &elem.alternatives {
            if alt.resolved_type.is_some() {
                // Already resolved (from inline type assembly)
                alt_types.push(alt.resolved_type);
            } else if let Some(TypeRefResult::QName(ref qname)) = alt.type_ref {
                let resolver = ReferenceResolver::new(schema_set);
                let type_key = resolver.resolve_type_ref(qname, source.as_ref())?;
                stats.types_resolved += 1;
                alt_types.push(Some(type_key));
            } else {
                // No type specified — use element's declared type as fallback
                alt_types.push(resolved_type);
            }
        }
        alt_types
    };

    // Store resolved references back
    if let Some(elem) = schema_set.arenas.elements.get_mut(key) {
        elem.resolved_type = resolved_type;
        elem.resolved_ref = resolved_ref;
        elem.resolved_substitution_groups = resolved_subst_groups;

        #[cfg(feature = "xsd11")]
        for (i, alt_type) in resolved_alt_types.into_iter().enumerate() {
            if let Some(alt) = elem.alternatives.get_mut(i) {
                alt.resolved_type = alt_type;
            }
        }
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
    let (base_qname, item_qname, member_qnames, source, already_resolved_base, already_resolved_item, already_resolved_members, redefine_original, type_name, type_ns) = {
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
            type_def.redefine_original,
            type_def.name,
            type_def.target_namespace,
        )
    };

    // Create resolver
    let resolver = ReferenceResolver::new(schema_set);

    // Resolve base type reference (for restriction) - if not already resolved
    // For redefine self-references, redirect to the original type key
    let resolved_base = if already_resolved_base.is_some() {
        already_resolved_base
    } else if let Some(ref qname) = base_qname {
        let is_redefine_self_ref = redefine_original.is_some()
            && Some(qname.local_name) == type_name
            && qname.namespace == type_ns;
        if is_redefine_self_ref {
            stats.types_resolved += 1;
            Some(TypeKey::Simple(redefine_original.unwrap()))
        } else {
            let type_key = resolver.resolve_type_ref(qname, source.as_ref())?;
            stats.types_resolved += 1;
            Some(type_key)
        }
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
    // Per XSD spec, memberTypes attribute members come first, then inline simpleType children
    let mut resolved_members = Vec::new();
    for qname in &member_qnames {
        let type_key = resolver.resolve_type_ref(qname, source.as_ref())?;
        stats.types_resolved += 1;
        resolved_members.push(type_key);
    }
    resolved_members.extend(already_resolved_members);

    // For list and union types without an explicit base, the XSD spec defines the
    // {base type definition} to be anySimpleType (§4.1.2 / §3.16.2.2).
    // Setting resolved_base_type here makes is_simple_type_derived_from work
    // correctly when checking derivation from anySimpleType (e.g. e-props-correct.4).
    let resolved_base = if resolved_base.is_none() {
        let variety = schema_set
            .arenas
            .simple_types
            .get(key)
            .map(|t| t.variety)
            .unwrap_or(SimpleTypeVariety::Atomic);
        if matches!(variety, SimpleTypeVariety::List | SimpleTypeVariety::Union) {
            let any_simple = schema_set.builtin_types().any_simple_type;
            Some(TypeKey::Simple(any_simple))
        } else {
            None
        }
    } else {
        resolved_base
    };

    // Store resolved references back
    if let Some(type_def) = schema_set.arenas.simple_types.get_mut(key) {
        type_def.resolved_base_type = resolved_base;
        type_def.resolved_item_type = resolved_item;
        type_def.resolved_member_types = resolved_members;
    }

    // Inherit variety and structural properties from base type for restriction-derived types.
    // Parser sets variety=Atomic for all restrictions, but restrictions of union/list types
    // must inherit the base type's variety and member types / item type.
    if let Some(TypeKey::Simple(base_sk)) = resolved_base {
        let (base_variety, base_members, base_item) = {
            if let Some(base_def) = schema_set.arenas.simple_types.get(base_sk) {
                (base_def.variety, base_def.resolved_member_types.clone(), base_def.resolved_item_type)
            } else {
                (SimpleTypeVariety::Atomic, Vec::new(), None)
            }
        };
        if let Some(type_def) = schema_set.arenas.simple_types.get_mut(key) {
            if type_def.variety == SimpleTypeVariety::Atomic && base_variety != SimpleTypeVariety::Atomic {
                type_def.variety = base_variety;
            }
            if base_variety == SimpleTypeVariety::Union && type_def.resolved_member_types.is_empty() {
                type_def.resolved_member_types = base_members;
            }
            if base_variety == SimpleTypeVariety::List && type_def.resolved_item_type.is_none() {
                type_def.resolved_item_type = base_item;
            }
        }
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
    let (base_qname, attribute_groups, attribute_uses, source, already_resolved_base, redefine_original, type_name, type_ns, already_resolved_attrs) = {
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

        // Preserve resolved_attributes from inline type assembly (Phase 3)
        let already_resolved_attrs = type_def.resolved_attributes.clone();

        (
            base_qname,
            type_def.attribute_groups.clone(),
            attribute_uses,
            type_def.source.clone(),
            type_def.resolved_base_type,
            type_def.redefine_original,
            type_def.name,
            type_def.target_namespace,
            already_resolved_attrs,
        )
    };

    // Create resolver
    let resolver = ReferenceResolver::new(schema_set);

    // Resolve base type reference - if not already resolved
    // For redefine self-references, redirect to the original type key
    let resolved_base = if already_resolved_base.is_some() {
        already_resolved_base
    } else if let Some(ref qname) = base_qname {
        let is_redefine_self_ref = redefine_original.is_some()
            && Some(qname.local_name) == type_name
            && qname.namespace == type_ns;
        if is_redefine_self_ref {
            stats.types_resolved += 1;
            Some(TypeKey::Complex(redefine_original.unwrap()))
        } else {
            let type_key = resolver.resolve_type_ref(qname, source.as_ref())?;
            stats.types_resolved += 1;
            Some(type_key)
        }
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
    for (i, (ref_name, type_qname, attr_source)) in attribute_uses.iter().enumerate() {
        let resolved_type = if let Some(ref qname) = type_qname {
            let type_key = resolver.resolve_type_ref(qname, attr_source.as_ref())?;
            stats.types_resolved += 1;
            Some(type_key)
        } else {
            // Preserve type from inline assembly (Phase 3) when no QName ref
            already_resolved_attrs.get(i).and_then(|r| r.resolved_type)
        };
        let resolved_ref = if let Some(ref qname) = ref_name {
            let attr_key = resolver.resolve_attribute_ref(qname, attr_source.as_ref())?;
            stats.attributes_resolved += 1;
            Some(attr_key)
        } else {
            // Preserve ref from inline assembly
            already_resolved_attrs.get(i).and_then(|r| r.resolved_ref)
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

    // Capture redefine info for self-reference redirection
    let redefine_original = group.redefine_original;
    let group_name = group.name;
    let group_ns = group.target_namespace;

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
                // Group particle — redirect self-references to the original group
                let resolved_group_ref = if let Some(ref qname) = elem_or_group_ref {
                    let is_self_ref = redefine_original.is_some()
                        && Some(qname.local_name) == group_name
                        && qname.namespace == group_ns;
                    let grp_key = if is_self_ref {
                        redefine_original.unwrap()
                    } else {
                        resolver.resolve_group_ref(qname, particle_source.as_ref())?
                    };
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

    // Capture redefine info for self-reference redirection
    let redefine_original = group.redefine_original;
    let group_name = group.name;
    let group_ns = group.target_namespace;

    // Extract attribute use info for resolution
    let attribute_uses: Vec<_> = group.attributes.iter().map(|attr_use| {
        let type_qname = match &attr_use.attribute.type_ref {
            Some(TypeRefResult::QName(qname)) => Some(qname.clone()),
            _ => None,
        };
        (attr_use.attribute.ref_name.clone(), type_qname, attr_use.attribute.source.clone())
    }).collect();

    // Preserve resolved_attributes from inline type assembly (Phase 3)
    let already_resolved_attrs = group.resolved_attributes.clone();

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

    // Resolve nested attribute group references — redirect self-references to the original
    let mut resolved_nested = Vec::with_capacity(nested_groups.len());
    for qname in &nested_groups {
        let is_self_ref = redefine_original.is_some()
            && Some(qname.local_name) == group_name
            && qname.namespace == group_ns;
        let group_key = if is_self_ref {
            redefine_original.unwrap()
        } else {
            resolver.resolve_attribute_group_ref(qname, source.as_ref())?
        };
        stats.attribute_groups_resolved += 1;
        resolved_nested.push(group_key);
    }

    // Resolve attribute use references
    let mut resolved_attrs = Vec::with_capacity(attribute_uses.len());
    for (i, (ref_name_opt, type_qname, attr_source)) in attribute_uses.iter().enumerate() {
        let resolved_type = if let Some(ref qname) = type_qname {
            let type_key = resolver.resolve_type_ref(qname, attr_source.as_ref())?;
            stats.types_resolved += 1;
            Some(type_key)
        } else {
            // Preserve type from inline assembly (Phase 3) when no QName ref
            already_resolved_attrs.get(i).and_then(|r| r.resolved_type)
        };
        let resolved_attr_ref = if let Some(ref qname) = ref_name_opt {
            let attr_key = resolver.resolve_attribute_ref(qname, attr_source.as_ref())?;
            stats.attributes_resolved += 1;
            Some(attr_key)
        } else {
            // Preserve ref from inline assembly
            already_resolved_attrs.get(i).and_then(|r| r.resolved_ref)
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
            pending_ic_refs: vec![],
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
            pending_ic_refs: vec![],
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
                    block: None,
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
            // Pre-populate with inline-resolved type
            resolved_particles: vec![ResolvedParticleTerm::Element {
                resolved_type: Some(TypeKey::Simple(string_key)),
                resolved_ref: None,
            }],
            resolved_particle_types: vec![Some(TypeKey::Simple(string_key))],
            resolved_particle_elements: Vec::new(),
            redefine_original: None,
            redefine_requires_restriction_check: false,
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

    /// Helper: set up a SchemaSet with a default attribute group and a complex type.
    /// Returns (schema_set, complex_type_key, attribute_group_key).
    fn setup_default_attrs_test(
        default_attributes_apply: bool,
    ) -> (SchemaSet, crate::ids::ComplexTypeKey, crate::ids::AttributeGroupKey) {
        use crate::arenas::{AttributeGroupData, ComplexTypeDefData};
        use crate::namespace::QualifiedName;
        use crate::parser::frames::ComplexContentResult;
        use crate::parser::location::{SourceRef, SourceSpan};
        use crate::schema::model::{DerivationSet, SchemaDocument};

        let mut schema_set = SchemaSet::new();

        let group_name = schema_set.name_table.add("commonAttrs");
        let group_data = AttributeGroupData {
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
        let group_key = schema_set.arenas.alloc_attribute_group(group_data);
        schema_set
            .get_or_create_namespace(None)
            .register_attribute_group(group_name, group_key);

        let doc_id = schema_set.documents.len() as u32;
        let mut doc = SchemaDocument::new(doc_id, "test.xsd".to_string());
        doc.default_attributes = Some(QualifiedName::local(group_name));
        schema_set.documents.push(doc);

        let type_name = schema_set.name_table.add("myType");
        let ct_data = ComplexTypeDefData {
            name: Some(type_name),
            target_namespace: None,
            base_type: None,
            derivation_method: None,
            content: ComplexContentResult::Empty,
            open_content: None,
            attributes: Vec::new(),
            attribute_groups: Vec::new(),
            attribute_wildcard: None,
            mixed: false,
            is_abstract: false,
            final_derivation: DerivationSet::empty(),
            block: DerivationSet::empty(),
            default_attributes_apply,
            id: None,
            #[cfg(feature = "xsd11")]
            assertions: Vec::new(),
            #[cfg(feature = "xsd11")]
            xpath_default_namespace: None,
            annotation: None,
            source: Some(SourceRef::new(doc_id, SourceSpan::new(0, 0))),
            resolved_base_type: None,
            resolved_attribute_groups: Vec::new(),
            resolved_attributes: Vec::new(),
            resolved_content_particle_types: Vec::new(),
            resolved_content_particle_elements: Vec::new(),
            redefine_original: None,
        };
        let ct_key = schema_set.arenas.alloc_complex_type(ct_data);

        (schema_set, ct_key, group_key)
    }

    #[test]
    fn test_resolve_default_attributes_injects_group() {
        let (mut schema_set, ct_key, group_key) = setup_default_attrs_test(true);

        let result = resolve_all_references(&mut schema_set);
        assert!(result.is_ok(), "Resolution should succeed: {:?}", result);

        let ct = schema_set.arenas.complex_types.get(ct_key).unwrap();
        assert!(
            ct.resolved_attribute_groups.contains(&group_key),
            "Default attribute group should be injected into resolved_attribute_groups"
        );
    }

    #[test]
    fn test_resolve_default_attributes_opt_out() {
        let (mut schema_set, ct_key, _group_key) = setup_default_attrs_test(false);

        let result = resolve_all_references(&mut schema_set);
        assert!(result.is_ok(), "Resolution should succeed: {:?}", result);

        let ct = schema_set.arenas.complex_types.get(ct_key).unwrap();
        assert!(
            ct.resolved_attribute_groups.is_empty(),
            "Default attribute group should NOT be injected when defaultAttributesApply=false"
        );
    }
}
