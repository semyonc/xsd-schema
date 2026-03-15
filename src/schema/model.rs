//! Schema model - SchemaSet, SchemaDocument, NamespaceTable
//!
//! This module contains the core schema organization structures:
//! - `SchemaSet` - Complete schema collection with all documents and components
//! - `SchemaDocument` - Individual schema document (root or included/imported)
//! - `NamespaceTable` - Per-namespace component lookup

use std::collections::HashMap;
use bitflags::bitflags;

use crate::ids::*;
use crate::namespace::NameTable;
use crate::namespace::table::well_known;
use crate::parser::location::{SourceMapStorage, SourceRef};
use crate::namespace::QualifiedName;
use crate::schema::annotation::Annotation;
use crate::schema::composition::{
    CompositionEdge, ComponentIdentity, DocumentComponentIndex, EffectiveComponent,
};
use crate::schema::wildcard::ElementWildcard;
use crate::arenas::SchemaArenas;
use crate::types::{BuiltinTypes, XmlTypeCode};

/// XSD version mode
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum XsdVersion {
    #[default]
    V1_0,
    V1_1,
}

bitflags! {
    /// Derivation control flags (for final, block attributes)
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct DerivationSet: u8 {
        const EXTENSION = 0x01;
        const RESTRICTION = 0x02;
        const LIST = 0x04;
        const UNION = 0x08;
        const SUBSTITUTION = 0x10;

        /// All derivation methods blocked
        const ALL = Self::EXTENSION.bits() | Self::RESTRICTION.bits() |
                   Self::LIST.bits() | Self::UNION.bits() | Self::SUBSTITUTION.bits();
    }
}

impl DerivationSet {
    /// Create a DerivationSet with only EXTENSION
    pub fn extension() -> Self {
        Self::EXTENSION
    }

    /// Create a DerivationSet with only RESTRICTION
    pub fn restriction() -> Self {
        Self::RESTRICTION
    }

    /// Check if extension is blocked/final
    pub fn contains_extension(&self) -> bool {
        self.contains(Self::EXTENSION)
    }

    /// Check if restriction is blocked/final
    pub fn contains_restriction(&self) -> bool {
        self.contains(Self::RESTRICTION)
    }

    /// Check if list derivation is blocked/final
    pub fn contains_list(&self) -> bool {
        self.contains(Self::LIST)
    }

    /// Check if union derivation is blocked/final
    pub fn contains_union(&self) -> bool {
        self.contains(Self::UNION)
    }

    /// Check if substitution is blocked
    pub fn contains_substitution(&self) -> bool {
        self.contains(Self::SUBSTITUTION)
    }
}

/// Form choice for element/attribute form defaults
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FormChoice {
    #[default]
    Unqualified,
    Qualified,
}

/// Complete schema set (possibly from multiple documents)
///
/// This is the main entry point for working with XSD schemas.
/// It owns all schema components and provides namespace-based lookup.
#[derive(Debug)]
pub struct SchemaSet {
    /// String interning table for names and namespace URIs
    pub name_table: NameTable,

    /// Centralized source map storage for all documents
    pub source_maps: SourceMapStorage,

    /// All parsed schema documents
    pub documents: Vec<SchemaDocument>,

    /// Per-namespace component tables (keyed by NameId; None = no namespace)
    pub namespaces: HashMap<Option<NameId>, NamespaceTable>,

    /// XSD version mode (1.0 or 1.1)
    pub xsd_version: XsdVersion,

    /// Arena storage for all components
    pub arenas: SchemaArenas,

    /// Loaded schema locations (for cycle detection)
    pub loaded_locations: HashMap<String, DocumentId>,

    /// Composition graph edges recorded during directive resolution
    pub composition_edges: Vec<CompositionEdge>,

    /// Effective component map with provenance (populated by composition phase).
    /// Keyed by `ComponentIdentity` so redefine/override *replaces* the entry
    /// instead of appending, producing the final visible component set.
    pub effective_components: HashMap<ComponentIdentity, EffectiveComponent>,

    /// Built-in type registry with well-known type IDs
    builtin_types: Option<BuiltinTypes>,
}

impl SchemaSet {
    /// Create a new empty schema set
    pub fn new() -> Self {
        Self::with_version(XsdVersion::V1_0)
    }

    /// Create a new schema set configured for XSD 1.0.
    pub fn xsd10() -> Self {
        Self::with_version(XsdVersion::V1_0)
    }

    /// Create a new schema set configured for XSD 1.1.
    pub fn xsd11() -> Self {
        Self::with_version(XsdVersion::V1_1)
    }

    /// Create a new schema set with specified version
    pub fn with_version(version: XsdVersion) -> Self {
        let mut set = Self {
            name_table: NameTable::new(),
            source_maps: SourceMapStorage::new(),
            documents: Vec::new(),
            namespaces: HashMap::new(),
            xsd_version: version,
            arenas: SchemaArenas::new(),
            loaded_locations: HashMap::new(),
            composition_edges: Vec::new(),
            effective_components: HashMap::new(),
            builtin_types: None,
        };

        // Initialize built-in types
        let builtin_types = BuiltinTypes::new(&mut set);
        set.builtin_types = Some(builtin_types);

        set
    }

    /// Check if a schema location has already been loaded
    pub fn is_loaded(&self, location: &str) -> bool {
        self.loaded_locations.contains_key(location)
    }

    /// Mark a schema location as loaded
    pub fn mark_loaded(&mut self, location: String, doc_id: DocumentId) {
        self.loaded_locations.insert(location, doc_id);
    }

    /// Get or create namespace table for a namespace
    pub fn get_or_create_namespace(&mut self, ns: Option<NameId>) -> &mut NamespaceTable {
        self.namespaces.entry(ns).or_default()
    }

    /// Look up a type by namespace and name
    pub fn lookup_type(&self, ns: Option<NameId>, name: NameId) -> Option<TypeKey> {
        self.namespaces.get(&ns)?.types.get(&name).copied()
    }

    /// Look up an element by namespace and name
    pub fn lookup_element(&self, ns: Option<NameId>, name: NameId) -> Option<ElementKey> {
        self.namespaces.get(&ns)?.elements.get(&name).copied()
    }

    /// Look up an attribute by namespace and name
    pub fn lookup_attribute(&self, ns: Option<NameId>, name: NameId) -> Option<AttributeKey> {
        self.namespaces.get(&ns)?.attributes.get(&name).copied()
    }

    /// Look up a model group by namespace and name
    pub fn lookup_model_group(&self, ns: Option<NameId>, name: NameId) -> Option<ModelGroupKey> {
        self.namespaces.get(&ns)?.model_groups.get(&name).copied()
    }

    /// Look up an attribute group by namespace and name
    pub fn lookup_attribute_group(&self, ns: Option<NameId>, name: NameId) -> Option<AttributeGroupKey> {
        self.namespaces.get(&ns)?.attribute_groups.get(&name).copied()
    }

    /// Look up a notation by namespace and name
    pub fn lookup_notation(&self, ns: Option<NameId>, name: NameId) -> Option<NotationKey> {
        self.namespaces.get(&ns)?.notations.get(&name).copied()
    }

    // ========================================================================
    // Built-in type access
    // ========================================================================

    /// Get the built-in types registry.
    ///
    /// This provides access to well-known type IDs for all 47+ built-in XSD types.
    pub fn builtin_types(&self) -> &BuiltinTypes {
        self.builtin_types
            .as_ref()
            .expect("BuiltinTypes should always be initialized")
    }

    /// Get a built-in simple type by QName (namespace + local name).
    ///
    /// This only looks up built-in types in the XS namespace.
    /// For user-defined types, use `lookup_type` instead.
    ///
    /// # Arguments
    /// * `namespace` - The namespace URI (should be XS namespace for built-in types)
    /// * `local_name` - The local name of the type
    ///
    /// # Returns
    /// The `SimpleTypeKey` for the built-in type, or `None` if not found.
    pub fn get_built_in_simple_type_by_qname(
        &self,
        namespace: Option<NameId>,
        local_name: NameId,
    ) -> Option<SimpleTypeKey> {
        // Built-in types are only in the XS namespace
        if namespace != Some(well_known::XS_NAMESPACE) {
            return None;
        }
        self.builtin_types().get_by_local_name(local_name)
    }

    /// Get a built-in type by QName (namespace + local name).
    ///
    /// This includes the built-in complex type `xs:anyType` and all built-in simple types.
    pub fn get_built_in_type_by_qname(
        &self,
        namespace: Option<NameId>,
        local_name: NameId,
    ) -> Option<TypeKey> {
        if namespace != Some(well_known::XS_NAMESPACE) {
            return None;
        }

        if let Some(any_type_name) = self.name_table.get("anyType") {
            if local_name == any_type_name {
                return Some(TypeKey::Complex(self.builtin_types().any_type));
            }
        }

        self.get_built_in_simple_type_by_qname(namespace, local_name)
            .map(TypeKey::Simple)
    }

    /// Get the built-in `xs:anyType` key.
    pub fn any_type_key(&self) -> ComplexTypeKey {
        self.builtin_types().any_type
    }

    /// Check if the given type key refers to `xs:anyType`.
    pub fn is_any_type(&self, type_key: TypeKey) -> bool {
        matches!(type_key, TypeKey::Complex(key) if key == self.builtin_types().any_type)
    }

    /// Get a built-in simple type by its XmlTypeCode.
    ///
    /// # Returns
    /// The `SimpleTypeKey` for the built-in type, or `None` if not a simple type code.
    pub fn get_built_in_simple_type_by_code(&self, code: XmlTypeCode) -> Option<SimpleTypeKey> {
        self.builtin_types().get_by_type_code(code)
    }

    /// Get the XmlTypeCode for a simple type.
    ///
    /// Returns `None` if the type is not a built-in type.
    pub fn get_type_code(&self, type_id: SimpleTypeKey) -> Option<XmlTypeCode> {
        self.builtin_types().get_type_code(type_id)
    }

    /// Check if `derived` derives from `base` (transitively).
    ///
    /// For built-in types, this uses the standard XSD derivation hierarchy.
    /// For user-defined types, this walks the base type chain using resolved references.
    ///
    /// # Returns
    /// - `true` if `derived == base`
    /// - `true` if `derived` has `base` somewhere in its derivation chain
    /// - `false` otherwise
    pub fn derives_from(&self, derived: SimpleTypeKey, base: SimpleTypeKey) -> bool {
        // Same type derives from itself
        if derived == base {
            return true;
        }

        // First, check if both are built-in types and use the built-in derivation
        let builtin = self.builtin_types();
        if builtin.is_builtin(derived) && builtin.is_builtin(base) {
            return builtin.derives_from(derived, base);
        }

        // For user-defined types (or mixed), walk the resolved base type chain
        let mut current = derived;
        let mut visited = std::collections::HashSet::new();

        while visited.insert(current) {
            // Get the simple type data
            if let Some(type_def) = self.arenas.simple_types.get(current) {
                // Check the resolved base type
                if let Some(crate::ids::TypeKey::Simple(simple_base)) = type_def.resolved_base_type {
                    if simple_base == base {
                        return true;
                    }
                    current = simple_base;
                    continue;
                }
            }

            // If no resolved base type, try built-in derivation as fallback
            if builtin.is_builtin(current) {
                if let Some(parent) = builtin.get_base_type(current) {
                    if parent == base {
                        return true;
                    }
                    current = parent;
                    continue;
                }
            }

            // No more base types to traverse
            break;
        }

        false
    }

    // ========================================================================
    // Type derivation checking (analog of C# XmlSchemaType.IsDerivedFrom)
    // ========================================================================

    /// Check if `derived` is derived from `base`, optionally filtering by derivation method.
    ///
    /// This mirrors C#'s `XmlSchemaType.IsDerivedFrom(derivedType, baseType, method)`.
    ///
    /// # Arguments
    /// * `derived` - The potentially derived type
    /// * `base` - The potential base type
    /// * `exclude_methods` - Derivation methods to exclude from the check.
    ///   Use `DerivationSet::empty()` to allow any method (like C#'s Empty).
    ///
    /// # Returns
    /// - `true` if `derived == base`
    /// - `true` if `derived` derives from `base` via a non-excluded derivation method
    /// - `false` otherwise
    pub fn is_type_derived_from(
        &self,
        derived: TypeKey,
        base: TypeKey,
        exclude_methods: DerivationSet,
    ) -> bool {
        // Same type derives from itself
        if derived == base {
            return true;
        }

        if self.is_any_type(base) {
            return true;
        }

        match (derived, base) {
            // Case 1: Both are simple types
            (TypeKey::Simple(d), TypeKey::Simple(b)) => {
                self.is_simple_type_derived_from(d, b, exclude_methods)
            }

            // Case 2: Both are complex types
            (TypeKey::Complex(d), TypeKey::Complex(b)) => {
                self.is_complex_type_derived_from(d, b, exclude_methods)
            }

            // Case 3: Simple derives from Complex
            // All simple types derive from anyType (via anySimpleType).
            (TypeKey::Simple(_), TypeKey::Complex(_)) => {
                false
            }

            // Case 4: Complex derives from Simple
            // Complex types with simpleContent can derive from simple types
            (TypeKey::Complex(d), TypeKey::Simple(b)) => {
                self.is_complex_derived_from_simple(d, b, exclude_methods)
            }
        }
    }

    /// Check if simple type `derived` is derived from simple type `base` with method filtering.
    fn is_simple_type_derived_from(
        &self,
        derived: SimpleTypeKey,
        base: SimpleTypeKey,
        exclude_methods: DerivationSet,
    ) -> bool {
        use crate::parser::frames::SimpleTypeVariety;

        // Same type
        if derived == base {
            return true;
        }

        let builtin = self.builtin_types();
        let mut current = derived;
        let mut visited = std::collections::HashSet::new();

        while visited.insert(current) {
            // Get type definition
            if let Some(type_def) = self.arenas.simple_types.get(current) {
                // Determine derivation method based on variety
                let method_flag = match type_def.variety {
                    SimpleTypeVariety::Atomic => DerivationSet::RESTRICTION,
                    SimpleTypeVariety::List => DerivationSet::LIST,
                    SimpleTypeVariety::Union => DerivationSet::UNION,
                };

                // If this derivation method is excluded, stop traversal
                if exclude_methods.contains(method_flag) {
                    return false;
                }

                // Check resolved base type
                if let Some(TypeKey::Simple(simple_base)) = type_def.resolved_base_type {
                    if simple_base == base {
                        return true;
                    }
                    current = simple_base;
                    continue;
                }
            }

            // Fallback to built-in derivation
            if builtin.is_builtin(current) {
                // For built-in types, derivation is always by restriction
                if exclude_methods.contains(DerivationSet::RESTRICTION) {
                    return false;
                }
                if let Some(parent) = builtin.get_base_type(current) {
                    if parent == base {
                        return true;
                    }
                    current = parent;
                    continue;
                }
            }

            break;
        }

        false
    }

    /// Check if complex type `derived` is derived from complex type `base` with method filtering.
    fn is_complex_type_derived_from(
        &self,
        derived: ComplexTypeKey,
        base: ComplexTypeKey,
        exclude_methods: DerivationSet,
    ) -> bool {
        use crate::parser::frames::DerivationMethod;

        if derived == base {
            return true;
        }

        let mut current = derived;
        let mut visited = std::collections::HashSet::new();

        while visited.insert(current) {
            if let Some(type_def) = self.arenas.complex_types.get(current) {
                // Determine derivation method flag
                let method_flag = match type_def.derivation_method {
                    Some(DerivationMethod::Extension) => DerivationSet::EXTENSION,
                    Some(DerivationMethod::Restriction) => DerivationSet::RESTRICTION,
                    None => DerivationSet::empty(), // Implicit restriction from anyType
                };

                // If this derivation method is excluded, stop traversal
                if !method_flag.is_empty() && exclude_methods.contains(method_flag) {
                    return false;
                }

                // Check resolved base type
                if let Some(TypeKey::Complex(complex_base)) = type_def.resolved_base_type {
                    if complex_base == base {
                        return true;
                    }
                    current = complex_base;
                    continue;
                }
            }

            break;
        }

        false
    }

    /// Check if complex type `derived` (with simpleContent) derives from simple type `base`.
    fn is_complex_derived_from_simple(
        &self,
        derived: ComplexTypeKey,
        base: SimpleTypeKey,
        exclude_methods: DerivationSet,
    ) -> bool {
        use crate::parser::frames::DerivationMethod;

        if let Some(type_def) = self.arenas.complex_types.get(derived) {
            // Check derivation method
            let method_flag = match type_def.derivation_method {
                Some(DerivationMethod::Extension) => DerivationSet::EXTENSION,
                Some(DerivationMethod::Restriction) => DerivationSet::RESTRICTION,
                None => DerivationSet::empty(),
            };

            if !method_flag.is_empty() && exclude_methods.contains(method_flag) {
                return false;
            }

            // Check if base type is the target simple type
            if let Some(TypeKey::Simple(simple_base)) = type_def.resolved_base_type {
                if simple_base == base {
                    return true;
                }
                // Continue checking up the simple type chain
                return self.is_simple_type_derived_from(simple_base, base, exclude_methods);
            }
        }

        false
    }

    /// Compute the effective namespace for a local element declaration per XSD spec.
    ///
    /// Rules: explicit targetNamespace > form attribute > elementFormDefault > Unqualified.
    /// Qualified → document target namespace; Unqualified → None.
    pub fn effective_local_element_namespace(
        &self,
        elem_target_namespace: Option<NameId>,
        elem_form: Option<&str>,
        source: Option<&SourceRef>,
        fallback_namespace: Option<NameId>,
    ) -> Option<NameId> {
        self.effective_local_namespace(
            elem_target_namespace,
            elem_form,
            source,
            fallback_namespace,
            |d| d.element_form_default,
        )
    }

    /// Compute the effective namespace for a local attribute declaration per XSD spec.
    ///
    /// Rules: explicit targetNamespace > form attribute > attributeFormDefault > Unqualified.
    /// Qualified → document target namespace; Unqualified → None.
    pub fn effective_local_attribute_namespace(
        &self,
        attr_target_namespace: Option<NameId>,
        attr_form: Option<&str>,
        source: Option<&SourceRef>,
        fallback_namespace: Option<NameId>,
    ) -> Option<NameId> {
        self.effective_local_namespace(
            attr_target_namespace,
            attr_form,
            source,
            fallback_namespace,
            |d| d.attribute_form_default,
        )
    }

    fn effective_local_namespace(
        &self,
        explicit_target_namespace: Option<NameId>,
        form: Option<&str>,
        source: Option<&SourceRef>,
        fallback_namespace: Option<NameId>,
        form_default: impl Fn(&SchemaDocument) -> FormChoice,
    ) -> Option<NameId> {
        if explicit_target_namespace.is_some() {
            return explicit_target_namespace;
        }
        let doc = source.and_then(|s| self.documents.get(s.doc_id as usize));
        let default_form = doc.map(&form_default).unwrap_or(FormChoice::Unqualified);
        let target_namespace = doc
            .map(|d| d.target_namespace)
            .unwrap_or(fallback_namespace);
        let resolved_form = match form {
            Some("qualified") => FormChoice::Qualified,
            Some("unqualified") => FormChoice::Unqualified,
            _ => default_form,
        };
        match resolved_form {
            FormChoice::Qualified => target_namespace,
            FormChoice::Unqualified => None,
        }
    }
}

impl Default for SchemaSet {
    fn default() -> Self {
        Self::new()
    }
}

/// A single schema document (root or included/imported)
///
/// Represents one XSD file with its components and directives.
#[derive(Debug)]
pub struct SchemaDocument {
    /// Document ID for source map reference
    pub id: DocumentId,

    /// Base URI (location) of this document
    pub base_uri: String,

    /// Target namespace (None = chameleon or no namespace)
    pub target_namespace: Option<NameId>,

    /// Schema-level attributes
    pub version: Option<String>,
    pub element_form_default: FormChoice,
    pub attribute_form_default: FormChoice,
    pub block_default: DerivationSet,
    pub final_default: DerivationSet,
    pub schema_id: Option<String>,
    pub xml_lang: Option<String>,

    /// XSD 1.1: Default attributes group reference
    pub default_attributes: Option<QualifiedName>,

    /// XSD 1.1: Default namespace for XPath
    pub xpath_default_namespace: Option<NameId>,

    /// Composition directives (in document order)
    pub includes: Vec<IncludeDirective>,
    pub imports: Vec<ImportDirective>,
    pub redefines: Vec<RedefineDirective>,
    pub overrides: Vec<OverrideDirective>, // XSD 1.1

    /// XSD 1.1: Default open content
    pub default_open_content: Option<DefaultOpenContent>,

    /// Schema-level annotations
    pub annotations: Vec<Annotation>,

    /// Per-document index of top-level components declared in this document.
    /// Populated during assembly; used for document-scoped lookup in
    /// `apply_redefine()` and `apply_override()`.
    pub component_index: DocumentComponentIndex,

    /// Source reference for error reporting
    pub source: Option<SourceRef>,
}

impl SchemaDocument {
    /// Create a new schema document
    pub fn new(id: DocumentId, base_uri: String) -> Self {
        Self {
            id,
            base_uri,
            target_namespace: None,
            version: None,
            element_form_default: FormChoice::default(),
            attribute_form_default: FormChoice::default(),
            block_default: DerivationSet::empty(),
            final_default: DerivationSet::empty(),
            schema_id: None,
            xml_lang: None,
            default_attributes: None,
            xpath_default_namespace: None,
            includes: Vec::new(),
            imports: Vec::new(),
            redefines: Vec::new(),
            overrides: Vec::new(),
            default_open_content: None,
            annotations: Vec::new(),
            component_index: DocumentComponentIndex::new(),
            source: None,
        }
    }
}

/// Per-namespace component lookup tables
///
/// Each namespace has its own table mapping local names to component keys.
/// Uses NameId as keys for fast equality checks.
#[derive(Debug, Default)]
pub struct NamespaceTable {
    /// Type definitions (simple and complex)
    pub types: HashMap<NameId, TypeKey>,
    /// Element declarations
    pub elements: HashMap<NameId, ElementKey>,
    /// Attribute declarations
    pub attributes: HashMap<NameId, AttributeKey>,
    /// Attribute groups
    pub attribute_groups: HashMap<NameId, AttributeGroupKey>,
    /// Named model groups
    pub model_groups: HashMap<NameId, ModelGroupKey>,
    /// Notations
    pub notations: HashMap<NameId, NotationKey>,
    /// Identity constraints (global, for XSD 1.1 refs)
    pub identity_constraints: HashMap<NameId, IdentityConstraintKey>,
}

impl NamespaceTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a type in this namespace
    pub fn register_type(&mut self, name: NameId, key: TypeKey) -> Option<TypeKey> {
        self.types.insert(name, key)
    }

    /// Register an element in this namespace
    pub fn register_element(&mut self, name: NameId, key: ElementKey) -> Option<ElementKey> {
        self.elements.insert(name, key)
    }

    /// Register an attribute in this namespace
    pub fn register_attribute(&mut self, name: NameId, key: AttributeKey) -> Option<AttributeKey> {
        self.attributes.insert(name, key)
    }

    /// Register a model group in this namespace
    pub fn register_model_group(&mut self, name: NameId, key: ModelGroupKey) -> Option<ModelGroupKey> {
        self.model_groups.insert(name, key)
    }

    /// Register an attribute group in this namespace
    pub fn register_attribute_group(&mut self, name: NameId, key: AttributeGroupKey) -> Option<AttributeGroupKey> {
        self.attribute_groups.insert(name, key)
    }

    /// Register a notation in this namespace
    pub fn register_notation(&mut self, name: NameId, key: NotationKey) -> Option<NotationKey> {
        self.notations.insert(name, key)
    }
}

// Schema composition directives

/// xs:include directive
#[derive(Debug, Clone)]
pub struct IncludeDirective {
    pub source: Option<SourceRef>,
    pub schema_location: String,
    pub resolved_doc_id: Option<DocumentId>,
}

/// xs:import directive
#[derive(Debug, Clone)]
pub struct ImportDirective {
    pub source: Option<SourceRef>,
    pub namespace: Option<String>,
    pub schema_location: Option<String>,
    pub resolved_doc_id: Option<DocumentId>,
}

/// xs:redefine directive (deprecated in XSD 1.1)
#[derive(Debug, Clone)]
pub struct RedefineDirective {
    pub source: Option<SourceRef>,
    pub schema_location: String,
    pub resolved_doc_id: Option<DocumentId>,
    pub simple_types: Vec<SimpleTypeKey>,
    pub complex_types: Vec<ComplexTypeKey>,
    pub groups: Vec<ModelGroupKey>,
    pub attribute_groups: Vec<AttributeGroupKey>,
}

/// xs:override directive (XSD 1.1)
#[derive(Debug, Clone)]
pub struct OverrideDirective {
    pub source: Option<SourceRef>,
    pub schema_location: String,
    pub resolved_doc_id: Option<DocumentId>,
    pub components: Vec<OverrideComponent>,
}

/// Component that can appear in xs:override
#[derive(Debug, Clone)]
pub enum OverrideComponent {
    SimpleType(SimpleTypeKey),
    ComplexType(ComplexTypeKey),
    Group(ModelGroupKey),
    AttributeGroup(AttributeGroupKey),
    Element(ElementKey),
    Attribute(AttributeKey),
    Notation(NotationKey),
}

/// Default open content at schema level (XSD 1.1)
#[derive(Debug, Clone)]
pub struct DefaultOpenContent {
    pub source: Option<SourceRef>,
    pub applies_to_empty: bool,
    pub mode: OpenContentMode,
    pub wildcard: Option<ElementWildcard>,
}

/// Open content mode (XSD 1.1)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OpenContentMode {
    None,
    #[default]
    Interleave,
    Suffix,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arenas::ComplexTypeDefData;
    use crate::parser::frames::ComplexContentResult;

    #[test]
    fn test_schema_set_creation() {
        let set = SchemaSet::new();
        assert_eq!(set.xsd_version, XsdVersion::V1_0);
        assert!(set.documents.is_empty());
        assert!(set.namespaces.is_empty());
    }

    #[test]
    fn test_schema_set_with_version() {
        let set = SchemaSet::with_version(XsdVersion::V1_1);
        assert_eq!(set.xsd_version, XsdVersion::V1_1);
    }

    #[test]
    fn test_schema_set_xsd10() {
        let set = SchemaSet::xsd10();
        assert_eq!(set.xsd_version, XsdVersion::V1_0);
    }

    #[test]
    fn test_schema_set_xsd11() {
        let set = SchemaSet::xsd11();
        assert_eq!(set.xsd_version, XsdVersion::V1_1);
    }

    #[test]
    fn test_namespace_table_registration() {
        use slotmap::SlotMap;
        let mut table = NamespaceTable::new();

        // Create a dummy key for testing
        let mut dummy_map: SlotMap<SimpleTypeKey, ()> = SlotMap::with_key();
        let key1 = dummy_map.insert(());
        let key2 = dummy_map.insert(());

        // Register a type
        let old = table.register_type(NameId(1), TypeKey::Simple(key1));
        assert!(old.is_none());

        // Re-registering returns old value
        let old = table.register_type(NameId(1), TypeKey::Simple(key2));
        assert!(old.is_some());
    }

    #[test]
    fn test_schema_set_load_tracking() {
        let mut set = SchemaSet::new();

        assert!(!set.is_loaded("test.xsd"));
        set.mark_loaded("test.xsd".to_string(), 0);
        assert!(set.is_loaded("test.xsd"));
    }

    #[test]
    fn test_derivation_set_flags() {
        let mut flags = DerivationSet::empty();
        assert!(flags.is_empty());

        flags |= DerivationSet::EXTENSION;
        assert!(flags.contains(DerivationSet::EXTENSION));
        assert!(!flags.contains(DerivationSet::RESTRICTION));

        let all = DerivationSet::ALL;
        assert!(all.contains(DerivationSet::EXTENSION));
        assert!(all.contains(DerivationSet::RESTRICTION));
    }

    #[test]
    fn test_form_choice_default() {
        assert_eq!(FormChoice::default(), FormChoice::Unqualified);
    }

    // ========================================================================
    // Tests for is_type_derived_from (analog of C# XmlSchemaType.IsDerivedFrom)
    // ========================================================================

    #[test]
    fn test_is_type_derived_from_same_type() {
        let set = SchemaSet::new();
        let string_key = set.builtin_types().string;

        // Same type derives from itself
        assert!(set.is_type_derived_from(
            TypeKey::Simple(string_key),
            TypeKey::Simple(string_key),
            DerivationSet::empty()
        ));
    }

    #[test]
    fn test_is_type_derived_from_direct_derivation() {
        let set = SchemaSet::new();
        let builtin = set.builtin_types();

        // xs:normalizedString derives from xs:string
        assert!(set.is_type_derived_from(
            TypeKey::Simple(builtin.normalized_string),
            TypeKey::Simple(builtin.string),
            DerivationSet::empty()
        ));

        // xs:integer derives from xs:decimal
        assert!(set.is_type_derived_from(
            TypeKey::Simple(builtin.integer),
            TypeKey::Simple(builtin.decimal),
            DerivationSet::empty()
        ));
    }

    #[test]
    fn test_is_type_derived_from_transitive() {
        let set = SchemaSet::new();
        let builtin = set.builtin_types();

        // xs:NCName derives from xs:string (NCName < Name < token < normalizedString < string)
        assert!(set.is_type_derived_from(
            TypeKey::Simple(builtin.ncname),
            TypeKey::Simple(builtin.string),
            DerivationSet::empty()
        ));

        // xs:byte derives from xs:decimal (byte < short < int < long < integer < decimal)
        assert!(set.is_type_derived_from(
            TypeKey::Simple(builtin.byte),
            TypeKey::Simple(builtin.decimal),
            DerivationSet::empty()
        ));

        // xs:ID derives from xs:string (ID < NCName < Name < token < normalizedString < string)
        assert!(set.is_type_derived_from(
            TypeKey::Simple(builtin.id),
            TypeKey::Simple(builtin.string),
            DerivationSet::empty()
        ));
    }

    #[test]
    fn test_is_type_derived_from_not_derived() {
        let set = SchemaSet::new();
        let builtin = set.builtin_types();

        // xs:string does NOT derive from xs:integer
        assert!(!set.is_type_derived_from(
            TypeKey::Simple(builtin.string),
            TypeKey::Simple(builtin.integer),
            DerivationSet::empty()
        ));

        // xs:decimal does NOT derive from xs:integer (reverse direction)
        assert!(!set.is_type_derived_from(
            TypeKey::Simple(builtin.decimal),
            TypeKey::Simple(builtin.integer),
            DerivationSet::empty()
        ));

        // xs:date does NOT derive from xs:duration
        assert!(!set.is_type_derived_from(
            TypeKey::Simple(builtin.date),
            TypeKey::Simple(builtin.duration),
            DerivationSet::empty()
        ));
    }

    #[test]
    fn test_is_type_derived_from_any_simple_type() {
        let set = SchemaSet::new();
        let builtin = set.builtin_types();

        // All simple types derive from xs:anySimpleType
        assert!(set.is_type_derived_from(
            TypeKey::Simple(builtin.string),
            TypeKey::Simple(builtin.any_simple_type),
            DerivationSet::empty()
        ));

        assert!(set.is_type_derived_from(
            TypeKey::Simple(builtin.integer),
            TypeKey::Simple(builtin.any_simple_type),
            DerivationSet::empty()
        ));

        assert!(set.is_type_derived_from(
            TypeKey::Simple(builtin.byte),
            TypeKey::Simple(builtin.any_simple_type),
            DerivationSet::empty()
        ));
    }

    #[test]
    fn test_is_type_derived_from_any_type() {
        let mut set = SchemaSet::new();
        let any_type = set.builtin_types().any_type;
        let string_type = set.builtin_types().string;

        assert!(set.is_type_derived_from(
            TypeKey::Simple(string_type),
            TypeKey::Complex(any_type),
            DerivationSet::empty()
        ));

        let ct_key = set.arenas.alloc_complex_type(ComplexTypeDefData {
            name: None,
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
            default_attributes_apply: true,
            id: None,
            #[cfg(feature = "xsd11")]
            assertions: Vec::new(),
            #[cfg(feature = "xsd11")]
            xpath_default_namespace: None,
            annotation: None,
            source: None,
            resolved_base_type: None,
            resolved_attribute_groups: Vec::new(),
            resolved_attributes: Vec::new(),
            resolved_content_particle_types: Vec::new(),
            resolved_content_particle_elements: Vec::new(),
        });

        assert!(set.is_type_derived_from(
            TypeKey::Complex(ct_key),
            TypeKey::Complex(any_type),
            DerivationSet::empty()
        ));
    }

    #[test]
    fn test_is_type_derived_from_exclude_restriction() {
        let set = SchemaSet::new();
        let builtin = set.builtin_types();

        // With RESTRICTION excluded, xs:normalizedString does NOT derive from xs:string
        assert!(!set.is_type_derived_from(
            TypeKey::Simple(builtin.normalized_string),
            TypeKey::Simple(builtin.string),
            DerivationSet::RESTRICTION
        ));

        // Same type still derives from itself even with exclusions
        assert!(set.is_type_derived_from(
            TypeKey::Simple(builtin.string),
            TypeKey::Simple(builtin.string),
            DerivationSet::RESTRICTION
        ));
    }

    #[test]
    fn test_is_type_derived_from_list_types() {
        let set = SchemaSet::new();
        let builtin = set.builtin_types();

        // xs:NMTOKENS is a list type that derives from xs:anySimpleType
        assert!(set.is_type_derived_from(
            TypeKey::Simple(builtin.nmtokens),
            TypeKey::Simple(builtin.any_simple_type),
            DerivationSet::empty()
        ));

        // With LIST excluded, xs:NMTOKENS should not derive from xs:anySimpleType
        assert!(!set.is_type_derived_from(
            TypeKey::Simple(builtin.nmtokens),
            TypeKey::Simple(builtin.any_simple_type),
            DerivationSet::LIST
        ));
    }
}
