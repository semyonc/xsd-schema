//! XSD 1.0 Schema Parser and Validator
//!
//! This crate provides a complete XSD 1.0 schema parser with structural validation,
//! namespace management, and W3C conformance testing. It follows the design specifications
//! in the XSD_*.md documentation files.
//!
//! # Entry Points
//!
//! ## Single Schema (Recommended)
//!
//! Use [`load_and_process_schema`] for complete processing of a single schema.
//! XSD version is set on `SchemaSet` — the parser derives it automatically.
//!
//! ```
//! use xsd_schema::{SchemaSet, load_and_process_schema};
//!
//! // Use SchemaSet::new() for XSD 1.0, SchemaSet::xsd11() for XSD 1.1
//! let mut schema_set = SchemaSet::new();
//! let xml = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
//!     <xs:element name="root" type="xs:string"/>
//! </xs:schema>"#;
//!
//! let stats = load_and_process_schema(xml.as_bytes(), "schema.xsd", &mut schema_set, None)
//!     .expect("failed to load schema");
//! assert_eq!(stats.doc_id, 0);
//! ```
//!
//! ## Multiple Related Schemas
//!
//! For loading multiple schema files, use the two-phase approach:
//!
//! ```
//! use xsd_schema::{SchemaSet, parse_schema_only, process_loaded_schemas};
//!
//! let mut schema_set = SchemaSet::new();
//!
//! let schemas = [
//!     (r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
//!                   targetNamespace="urn:schema1">
//!         <xs:element name="item1" type="xs:string"/>
//!     </xs:schema>"#, "schema1.xsd"),
//!     (r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
//!                   targetNamespace="urn:schema2">
//!         <xs:element name="item2" type="xs:int"/>
//!     </xs:schema>"#, "schema2.xsd"),
//! ];
//!
//! // Phase 1: Parse all schemas
//! for (xml, uri) in schemas {
//!     parse_schema_only(xml.as_bytes(), uri, &mut schema_set).expect("parse failed");
//! }
//!
//! // Phase 2: Process all schemas together
//! // (redefine/override application, inline assembly, reference resolution)
//! // Note: all participating schemas — including redefine/override targets —
//! // must be parsed before calling this function.
//! let (inline_stats, resolution_stats) = process_loaded_schemas(&mut schema_set)
//!     .expect("processing failed");
//! ```
//!
//! ## Advanced: Low-Level Parser
//!
//! For custom pipelines, the low-level parser is available at [`parser::parse_schema`].
//! This only performs Phase 1 (parsing + assembly) - subsequent phases must be run manually.
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
pub mod arenas;
pub mod error;
pub mod ids;

// Parser infrastructure
pub mod parser;

// Namespace management
pub mod namespace;

// Schema component model
pub mod schema;
pub mod types;

// DOM navigation (always available)
pub mod navigator;

// XPath 2.0 engine (only with xsd11 feature)
#[cfg(feature = "xsd11")]
pub mod xpath;

// Page-based XML document buffer (only with xsd11 feature)
#[cfg(feature = "xsd11")]
pub mod document;

// Pipeline orchestration
pub mod pipeline;

// Embedded assets
pub mod embedded;

// Builder pattern API
pub mod builder;

// Regex pattern conversion (shared between XSD and XPath)
pub mod regex_convert;
pub(crate) mod regex_xsd_unicode;

// NFA compiler for content models
pub mod compiler;

// Instance validation
pub mod validation;

// Re-export primary types
pub use error::{FacetError, FacetResult, SchemaError, SchemaResult};
pub use ids::*;
pub use schema::{SchemaDocument, SchemaSet};

// Re-export resolution and inline assembly
pub use schema::{
    assemble_inline_types, resolve_all_references, InlineAssemblyStats, ResolutionStats,
};

// Re-export XSD version
pub use schema::model::XsdVersion;

// Re-export regex compatibility mode
pub use schema::model::RegexCompat;

// Re-export type system enums
pub use types::{BuiltinTypes, PrimitiveTypeCode, ValueKind, XmlTypeCode};

// Re-export facet types
pub use types::{
    facet_applicable, facet_applicable_for_type, normalize_whitespace, FacetApplicability,
    FacetFixed, FacetKind, FacetSet, WhitespaceMode,
};

// Re-export navigator types (always available)
pub use navigator::{
    DomNavigator, DomNodeType, NamespaceAxisScope, NavigatorError, RoXmlNavigator, XmlNodeOrder,
};

// Re-export XPath types (only with xsd11 feature)
#[cfg(feature = "xsd11")]
pub use xpath::{
    BufferedNodeIterator, EmptyIterator, EvalValue, ExternalVar, RangeIterator, TreeComparer,
    TypedEvaluator, VecNodeIterator, XPathContext, XPathEvaluator, XPathExpr, XmlItem, XmlItemRef,
    XmlNodeIterator,
};

// Re-export pipeline functions
pub use pipeline::{
    load_and_process_schema, load_schema, parse_schema_only, process_loaded_schemas,
    DirectiveStats, PipelineConfig, PipelineStats,
};

// Re-export async pipeline functions
#[cfg(feature = "async")]
pub use pipeline::{load_and_process_schema_async, load_schema_async};

// Re-export builder types
pub use builder::{CompilationStats, CompiledSchemaSet, SchemaSetBuilder};

// Re-export resolver types
pub use parser::resolver::{
    decode_xml_bytes, decode_xml_to_utf8_bytes, EmbeddedLoader, FileSystemLoader, LoaderChain,
    ResolverConfig, SchemaCatalog, SchemaLoader, SchemaResolver,
};

// Re-export async loader trait
#[cfg(feature = "async")]
pub use parser::resolver::AsyncSchemaLoader;

// Re-export embedded assets
pub use embedded::{
    get_embedded_schema, has_embedded_schema, XLINK_NAMESPACE, XLINK_XSD, XML_NAMESPACE, XML_XSD,
};

// Re-export compiler types
pub use compiler::{
    compile_model_group, compile_particle, fragment_to_table, CompileContext, FragmentBuilder,
    NfaCompileError, NfaCompileResult, NfaFragment, NfaState, NfaTable, NfaTerm, NfaTransition,
    StateId, TransitionKind,
};

// Re-export instance validation types
// Note: ValidationError here is distinct from types::validators::ValidationError
pub use validation::{
    error as validation_error, error_with_path as validation_error_with_path,
    facet_constraint_code, from_facet_error, from_value_error, value_error_constraint_code,
    ValidationError as InstanceValidationError, ValidationResult as InstanceValidationResult,
};

// Re-export hint-driven schema loading
pub use validation::hint_loader::{enrich_schema_set, load_hints_into_builder, HintLoadResult};
pub use validation::info::{NoNamespaceSchemaLocationHint, SchemaLocationHint};
