//! Schema component model
//!
//! This module contains the schema object model including elements, types, groups, etc.
//!
//! ## Module Structure
//!
//! - `model` - SchemaSet, SchemaDocument, NamespaceTable
//! - `decl` - Element and attribute declarations
//! - `group` - Model groups and attribute groups
//! - `wildcard` - Wildcard specifications
//! - `annotation` - Annotations and documentation
//! - `resolver` - Reference resolution (QName → component ID)
//! - `inline` - Inline type assembly (TypeRefResult::Inline → TypeKey)
//! - `dependencies` - Dependency graph for type compilation order
//! - `derivation` - Type derivation validation
//! - `redefine` - xs:redefine processing
//! - `override_dir` - xs:override processing (XSD 1.1)

pub mod model;
pub mod decl;
pub mod group;
pub mod wildcard;
pub mod annotation;
pub mod composition;
pub mod resolver;
pub mod inline;
pub mod dependencies;
pub mod derivation;
pub mod redefine;
#[cfg(feature = "xsd11")]
pub mod override_dir;

// Re-exports from model
pub use model::{
    SchemaSet, SchemaDocument, NamespaceTable,
    XsdVersion, DerivationSet, FormChoice,
    IncludeDirective, ImportDirective, RedefineDirective, OverrideDirective,
    DefaultOpenContent, OpenContentMode,
};

// Re-exports from decl
pub use decl::{
    ElementDecl, AttributeDecl, NotationDecl,
    DeclarationScope, ValueConstraint, TypeReference, FormKind,
};

// Re-exports from group
pub use group::{
    ModelGroupDef, AttributeGroupDef,
    ModelGroupRef, AttributeGroupRef, Occurrence,
};

// Re-exports from wildcard
pub use wildcard::{
    ElementWildcard,
    NamespaceConstraint, ProcessContents,
};

// Re-exports from composition
pub use composition::{
    CompositionEdge, CompositionEdgeKind,
    ComponentKind, ComponentIdentity, ComponentOrigin,
    ComponentKey, DocumentComponentIndex,
    CompositionAction, EffectiveComponent,
};

// Re-exports from annotation
pub use annotation::{
    Annotation, AnnotationItem, AppInfoElement, DocumentationElement,
    XmlFragment, ForeignAttribute,
};

// Re-exports from resolver
pub use resolver::{
    ReferenceResolver, resolve_all_references,
    ResolvedReferences, ResolutionStats,
};

// Re-exports from inline
pub use inline::{
    allocate_content_particle_elements, allocate_model_group_particle_elements,
    assemble_inline_types, InlineAssemblyStats,
};

// Re-exports from dependencies
pub use dependencies::{
    DependencyGraph, DependencyStats, build_dependency_graph,
};

// Re-exports from derivation
pub use derivation::{
    validate_all_derivations, validate_attribute_id_constraints,
    DerivationStats as DerivationValidationStats,
};

// Re-exports from redefine
pub use redefine::apply_redefine;

// Re-exports from override_dir
#[cfg(feature = "xsd11")]
pub use override_dir::apply_override;

use crate::error::{SchemaError, SchemaResult};

/// Compile all deferred pattern facets across every simple type in the schema set.
///
/// Patterns added via `add_pattern_unchecked` during parsing are not compiled until
/// this function runs. Any invalid XSD regex pattern causes a structural error,
/// making the schema invalid.
///
/// Must be called after reference resolution so that all types are fully assembled.
pub fn compile_all_patterns(schema_set: &mut SchemaSet) -> SchemaResult<()> {
    let keys: Vec<_> = schema_set.arenas.simple_types.keys().collect();
    for key in keys {
        let type_def = &mut schema_set.arenas.simple_types[key];
        if let Err(facet_err) = type_def.facets.compile_patterns() {
            let source = type_def.source.clone();
            let location = source.as_ref().and_then(|src| {
                schema_set.source_maps.locate(src)
            });
            return Err(SchemaError::structural(
                "pattern-valid",
                facet_err.to_string(),
                location,
            ));
        }
    }
    Ok(())
}

/// Apply all redefine and override directives collected from loaded documents,
/// then build effective component provenance records.
///
/// This must be called after all participating schemas (including redefine/override
/// targets) have been parsed and loaded into the schema set, but before inline
/// assembly and reference resolution.
///
/// ## Phases
///
/// 1. **Collect** — gather all declared components from every document's
///    `component_index` and detect composition-time duplicates (`sch-props-correct.2`).
/// 2. **Apply** — run redefine/override directives, which mutate namespace tables
///    and record `Redefined`/`Overridden` provenance.
/// 3. **Store** — save the effective component list on `SchemaSet` for later
///    diagnostic and provenance queries.
pub fn apply_redefine_override(schema_set: &mut SchemaSet) -> SchemaResult<()> {
    // Phase 1: collect declared components from all documents
    collect_declared_components(schema_set);

    // Phase 2: apply redefines (records Redefined provenance).
    //
    // Redefines must run so that inner/leaf redefines execute before outer
    // ones that depend on their results. Document order alone is not a
    // reliable proxy: breadth-first loading assigns *lower* doc_ids to outer
    // documents, while pre-loaded (`parse_schema_only` + `process_loaded_schemas`)
    // callers typically push dependencies first, giving them *lower* doc_ids.
    // Sort by dependency depth instead: depth(doc) = 0 when the doc redefines
    // nothing, else 1 + max depth of its redefine targets. Processing
    // ascending by depth guarantees each target doc's own redefines are
    // applied before any redefine that uses that target.
    let redefines = topologically_ordered_redefines(schema_set);
    for redefine in redefines {
        apply_redefine(schema_set, &redefine)?;
    }

    // Phase 2b: apply overrides (records Overridden provenance)
    #[cfg(feature = "xsd11")]
    {
        let overrides: Vec<_> = schema_set
            .documents
            .iter()
            .flat_map(|doc| doc.overrides.iter().cloned())
            .collect();
        for override_dir in overrides {
            apply_override(schema_set, &override_dir)?;
        }
    }

    Ok(())
}

/// Return all collected redefine directives ordered so that every redefine's
/// target document has already had its own redefines applied.
///
/// The order is computed from a per-document *dependency depth*:
///   depth(doc) = 0 if doc has no redefines, else
///                1 + max(depth(target) for each redefine target in doc)
///
/// Documents are then stable-sorted by their depth (ascending). Redefines
/// keep their original document order for equal depths, so within a single
/// document the per-redefine order matches source order.
///
/// Cycles cannot occur for well-formed XSD (a redefine cannot form a loop),
/// but the traversal is defensive and treats any cycle as depth 0 rather
/// than looping.
fn topologically_ordered_redefines(
    schema_set: &SchemaSet,
) -> Vec<crate::schema::model::RedefineDirective> {
    use std::collections::{HashMap, HashSet};
    use crate::ids::DocumentId;
    use crate::schema::model::{RedefineDirective, SchemaDocument};

    fn depth(
        doc_id: DocumentId,
        docs: &[SchemaDocument],
        cache: &mut HashMap<DocumentId, usize>,
        visiting: &mut HashSet<DocumentId>,
    ) -> usize {
        if let Some(&d) = cache.get(&doc_id) {
            return d;
        }
        if !visiting.insert(doc_id) {
            // Cycle guard: treat revisited nodes as depth 0.
            return 0;
        }
        let d = docs
            .iter()
            .find(|d| d.id == doc_id)
            .map(|doc| {
                let mut max_dep = 0usize;
                for r in &doc.redefines {
                    if let Some(target) = r.resolved_doc_id {
                        let t = depth(target, docs, cache, visiting) + 1;
                        if t > max_dep {
                            max_dep = t;
                        }
                    }
                }
                max_dep
            })
            .unwrap_or(0);
        visiting.remove(&doc_id);
        cache.insert(doc_id, d);
        d
    }

    let mut cache: HashMap<DocumentId, usize> = HashMap::new();
    for doc in &schema_set.documents {
        depth(doc.id, &schema_set.documents, &mut cache, &mut HashSet::new());
    }

    // Tag each redefine with its source doc's depth, then stable-sort ascending.
    let mut tagged: Vec<(usize, RedefineDirective)> = schema_set
        .documents
        .iter()
        .flat_map(|doc| {
            let d = cache.get(&doc.id).copied().unwrap_or(0);
            doc.redefines.iter().cloned().map(move |r| (d, r))
        })
        .collect();
    tagged.sort_by_key(|(d, _)| *d);
    tagged.into_iter().map(|(_, r)| r).collect()
}

/// Collect all declared components from every document's component index
/// into `schema_set.effective_components` with provenance metadata.
///
/// Components from included documents are marked `Included`; components
/// declared in root documents are marked `Declared`. When the same identity
/// appears from multiple documents (e.g. via include), the last-registered
/// entry wins, matching current namespace-table behavior.
///
/// Note: duplicate-component detection (`sch-props-correct.2`) is handled
/// at parse time by `register_*` in `assemble.rs`. Composition-time
/// detection will be added when namespace tables are rebuilt from
/// effective components (future step).
fn collect_declared_components(schema_set: &mut SchemaSet) {
    use std::collections::HashMap;
    use crate::ids::DocumentId;
    use crate::schema::composition::CompositionEdgeKind;

    // Build set of (target_doc → source_doc) for Include edges.
    let mut included_from: HashMap<DocumentId, DocumentId> = HashMap::new();
    for edge in &schema_set.composition_edges {
        if edge.kind == CompositionEdgeKind::Include {
            if let Some(target) = edge.target_doc {
                included_from.entry(target).or_insert(edge.source_doc);
            }
        }
    }

    let mut effective: HashMap<ComponentIdentity, EffectiveComponent> = HashMap::new();

    for doc in &schema_set.documents {
        for (&identity, &key) in doc.component_index.iter() {
            let origin = ComponentOrigin {
                owner_doc: Some(doc.id),
                identity,
            };
            let action = if let Some(&from_doc) = included_from.get(&doc.id) {
                CompositionAction::Included { from_doc }
            } else {
                CompositionAction::Declared
            };
            effective.insert(identity, EffectiveComponent { key, origin, action });
        }
    }

    schema_set.effective_components = effective;
}
