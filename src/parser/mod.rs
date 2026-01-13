//! XSD document parsing
//!
//! This module contains the XSD parser implementation using quick-xml
//! with location tracking for accurate error reporting.
//!
//! ## Module Structure
//!
//! - `location` - Source location tracking (spans, line/column mapping)
//! - `reader` - Tracked XML reader wrapping quick-xml
//! - `attrs` - Attribute parsing and validation
//! - `frames` - Parser state machine frames
//! - `parse` - Main parser event loop
//! - `structure` - Structural validation rules
//! - `resolver` - Schema resolution (include/import/redefine)

pub mod location;
pub mod reader;
pub mod attrs;
pub mod frames;
pub mod parse;
pub mod structure;
pub mod resolver;
pub mod assemble;

// Re-exports from location
pub use location::{SourceLocation, SourceMap, SourceRef, SourceSpan, SourceRetention};

// Re-exports from reader
pub use reader::{TrackedReader, TrackedEvent, ReaderConfig, split_qname};

// Re-exports from attrs
pub use attrs::{
    ParsedAttribute, AttributeMap,
    parse_attributes, categorize_attributes,
    parse_boolean, parse_occurs, parse_use, parse_process_contents, parse_form,
};

// Re-exports from parse
//
// Note: `parse_schema` and `parse_schema_with_config` are low-level APIs that only perform
// Phase 1 (parsing + assembly). For typical use cases, prefer:
// - `crate::load_and_process_schema` for single schemas with full processing
// - `crate::parse_schema_only` + `crate::process_loaded_schemas` for multiple schemas
pub use parse::{parse_schema, parse_schema_with_config, ParserConfig};

// Re-exports from assemble
pub use assemble::{assemble_schema, parse_form_choice};

// Re-exports from structure
pub use structure::{
    ValidationContext,
    validate_element_structure, validate_attribute_structure,
    validate_simple_type_structure, validate_complex_type_structure,
    validate_restriction_structure, validate_extension_structure,
    validate_list_structure, validate_union_structure,
    validate_key_unique_structure, validate_keyref_structure,
    validate_group_structure, validate_attribute_group_structure,
    validate_xsd_version_element, validate_xsd_version_attribute,
    validate_notation_structure,
    validate_include_structure, validate_import_structure, validate_redefine_structure,
};

// Re-exports from resolver
pub use resolver::{
    SchemaResolver, ResolverConfig, SchemaCatalog, CatalogEntry,
    ResolutionResult, resolve_all_directives,
    // Loader trait and implementations
    SchemaLoader, FileSystemLoader, EmbeddedLoader, LoaderChain,
};
