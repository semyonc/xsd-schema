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
    ElementWildcard, AttributeWildcard,
    NamespaceConstraint, ProcessContents,
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
    validate_all_derivations, DerivationStats as DerivationValidationStats,
};

// Re-exports from redefine
pub use redefine::apply_redefine;

// Re-exports from override_dir
#[cfg(feature = "xsd11")]
pub use override_dir::apply_override;

use crate::error::SchemaResult;

/// Apply all redefine and override directives collected from loaded documents.
///
/// This must be called after all participating schemas (including redefine/override
/// targets) have been parsed and loaded into the schema set, but before inline
/// assembly and reference resolution.
pub fn apply_redefine_override(schema_set: &mut SchemaSet) -> SchemaResult<()> {
    let redefines: Vec<_> = schema_set
        .documents
        .iter()
        .flat_map(|doc| doc.redefines.iter().cloned())
        .collect();
    for redefine in redefines {
        apply_redefine(schema_set, &redefine)?;
    }

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
