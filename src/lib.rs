//! XSD 1.0 Schema Parser and Validator
//!
//! This crate provides a complete XSD 1.0 schema parser with structural validation,
//! namespace management, and W3C conformance testing. It follows the design specifications
//! in the XSD_*.md documentation files.
//!
//! # Architecture
//!
//! The parser uses a state machine approach with typed parser frames for each XSD element type.
//! All schema components are stored in arenas with typed IDs to avoid reference cycles.
//!
//! ## Core Modules
//!
//! - `parser` - XSD document parsing with location tracking
//! - `namespace` - String interning and namespace management
//! - `schema` - Schema component model (elements, types, groups)
//! - `types` - Type definitions and facets
//!
//! # Example
//!
//! ```rust
//! use xsd_schema::SchemaSet;
//!
//! // Create an empty schema set
//! let mut schema_set = SchemaSet::new();
//!
//! // Schema parser will be added in later implementation
//! // For now, the schema set can be populated programmatically
//! ```

// Core type definitions
pub mod ids;
pub mod error;
pub mod arenas;

// Parser infrastructure
pub mod parser;

// Namespace management
pub mod namespace;

// Schema component model
pub mod schema;
pub mod types;

// XPath navigation
pub mod xpath;

// Pipeline orchestration
pub mod pipeline;

// Re-export primary types
pub use error::{SchemaError, SchemaResult, FacetError, FacetResult};
pub use ids::*;
pub use schema::{SchemaSet, SchemaDocument};

// Re-export resolution and inline assembly
pub use schema::{
    assemble_inline_types, InlineAssemblyStats,
    resolve_all_references, ResolutionStats,
};

// Re-export XSD version
pub use schema::model::XsdVersion;

// Re-export type system enums
pub use types::{XmlTypeCode, PrimitiveTypeCode, ValueKind, BuiltinTypes};

// Re-export facet types
pub use types::{
    FacetSet, FacetFixed, WhitespaceMode, FacetApplicability, FacetKind,
    facet_applicable, facet_applicable_for_type, normalize_whitespace,
};

// Re-export XPath navigation types
pub use xpath::{DomNavigator, DomNodeType, XmlNodeOrder, NamespaceAxisScope, RoXmlNavigator};

// Re-export pipeline functions
pub use pipeline::{
    load_and_process_schema, load_schema, parse_schema_only, process_loaded_schemas,
    PipelineConfig, PipelineStats, DirectiveStats,
};
