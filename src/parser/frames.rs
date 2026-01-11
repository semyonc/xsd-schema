//! Parser frames for XSD element processing
//!
//! Each XSD element type has a corresponding frame that:
//! - Validates allowed child elements
//! - Collects and validates attributes
//! - Builds schema components
//! - Handles phase transitions
//!
//! The parser uses a stack of frames to track nested elements.

use crate::error::{SchemaError, SchemaResult};
use crate::ids::NameId;
use crate::namespace::NameTable;
use crate::parser::location::SourceRef;
use crate::parser::attrs::{parse_boolean, parse_form, parse_occurs, parse_use, AttributeMap};
use crate::schema::annotation::{
    Annotation, AnnotationItem, AppInfoElement, DocumentationElement, ForeignAttribute, XmlFragment,
};
use crate::namespace::context::NamespaceContextSnapshot;
use crate::schema::model::DerivationSet;
use crate::types::facets::{FacetSet, ExplicitTimezone};

/// XSD element local names (for matching)
pub mod xsd_names {
    pub const SCHEMA: &str = "schema";
    pub const INCLUDE: &str = "include";
    pub const IMPORT: &str = "import";
    pub const REDEFINE: &str = "redefine";
    pub const OVERRIDE: &str = "override";
    pub const ANNOTATION: &str = "annotation";
    pub const APPINFO: &str = "appinfo";
    pub const DOCUMENTATION: &str = "documentation";
    pub const SIMPLE_TYPE: &str = "simpleType";
    pub const COMPLEX_TYPE: &str = "complexType";
    pub const ELEMENT: &str = "element";
    pub const ATTRIBUTE: &str = "attribute";
    pub const GROUP: &str = "group";
    pub const ATTRIBUTE_GROUP: &str = "attributeGroup";
    pub const NOTATION: &str = "notation";
    pub const RESTRICTION: &str = "restriction";
    pub const EXTENSION: &str = "extension";
    pub const LIST: &str = "list";
    pub const UNION: &str = "union";
    pub const SIMPLE_CONTENT: &str = "simpleContent";
    pub const COMPLEX_CONTENT: &str = "complexContent";
    pub const SEQUENCE: &str = "sequence";
    pub const CHOICE: &str = "choice";
    pub const ALL: &str = "all";
    pub const ANY: &str = "any";
    pub const ANY_ATTRIBUTE: &str = "anyAttribute";
    pub const KEY: &str = "key";
    pub const KEYREF: &str = "keyref";
    pub const UNIQUE: &str = "unique";
    pub const SELECTOR: &str = "selector";
    pub const FIELD: &str = "field";
    pub const ENUMERATION: &str = "enumeration";
    pub const PATTERN: &str = "pattern";
    pub const MIN_INCLUSIVE: &str = "minInclusive";
    pub const MAX_INCLUSIVE: &str = "maxInclusive";
    pub const MIN_EXCLUSIVE: &str = "minExclusive";
    pub const MAX_EXCLUSIVE: &str = "maxExclusive";
    pub const MIN_LENGTH: &str = "minLength";
    pub const MAX_LENGTH: &str = "maxLength";
    pub const LENGTH: &str = "length";
    pub const TOTAL_DIGITS: &str = "totalDigits";
    pub const FRACTION_DIGITS: &str = "fractionDigits";
    pub const WHITE_SPACE: &str = "whiteSpace";
    // XSD 1.1
    pub const ASSERT: &str = "assert";
    pub const ASSERTION: &str = "assertion";
    pub const ALTERNATIVE: &str = "alternative";
    pub const OPEN_CONTENT: &str = "openContent";
    pub const DEFAULT_OPEN_CONTENT: &str = "defaultOpenContent";
    pub const EXPLICIT_TIMEZONE: &str = "explicitTimezone";
}

/// Result of finishing a frame
#[derive(Debug)]
pub enum FrameResult {
    /// Schema document completed
    Schema(SchemaFrameResult),
    /// Type definition completed
    Type(TypeFrameResult),
    /// Element declaration completed
    Element(ElementFrameResult),
    /// Attribute declaration completed
    Attribute(AttributeFrameResult),
    /// Group definition completed
    Group(GroupFrameResult),
    /// Notation declaration completed
    Notation(NotationResult),
    /// Assert completed (XSD 1.1)
    Assert(AssertResult),
    /// Alternative completed (XSD 1.1)
    Alternative(AlternativeResult),
    /// Open content completed (XSD 1.1)
    OpenContent(OpenContentResult),
    /// Default open content completed (XSD 1.1)
    DefaultOpenContent(DefaultOpenContentResult),
    /// Annotation completed
    Annotation(Annotation),
    /// AppInfo element completed
    AppInfo(AppInfoElement),
    /// Documentation element completed
    Documentation(DocumentationElement),
    /// Facet completed
    Facet(FacetResult),
    /// Restriction completed
    Restriction(RestrictionResult),
    /// Extension completed
    Extension(ExtensionResult),
    /// Simple content completed
    SimpleContent(SimpleContentDefResult),
    /// Complex content completed
    ComplexContent(ComplexContentDefResult),
    /// Identity constraint completed
    Identity(IdentityResult),
    /// Selector completed
    Selector(SelectorResult),
    /// Field completed
    Field(FieldResult),
    /// Wildcard completed
    Wildcard(WildcardResult),
    /// Composition directive completed
    Directive(DirectiveResult),
    /// Content particle completed
    Particle(ParticleResult),
    /// Skip frame (error recovery)
    Skip,
    /// Nothing to return (for internal frames)
    None,
}

/// Schema document result
#[derive(Debug)]
pub struct SchemaFrameResult {
    pub target_namespace: Option<NameId>,
    pub element_form_default: Option<String>,
    pub attribute_form_default: Option<String>,
    pub block_default: DerivationSet,
    pub default_attributes: Option<QNameRef>,
    pub xpath_default_namespace: Option<String>,
    pub final_default: DerivationSet,
    pub version: Option<String>,
    pub default_open_content: Option<DefaultOpenContentResult>,
    pub xml_lang: Option<String>,
    pub id: Option<String>,
    pub source: Option<SourceRef>,
    pub annotations: Vec<Annotation>,
    pub directives: Vec<DirectiveResult>,
    pub components: Vec<FrameResult>,
}

/// Type definition result
#[derive(Debug, Clone)]
pub enum TypeFrameResult {
    Simple(SimpleTypeResult),
    Complex(ComplexTypeResult),
}

/// Simple type result
#[derive(Debug, Clone)]
pub struct SimpleTypeResult {
    pub name: Option<NameId>,
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
}

/// Simple type variety
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimpleTypeVariety {
    Atomic,
    List,
    Union,
}

/// Complex type result
#[derive(Debug, Clone)]
pub struct ComplexTypeResult {
    pub name: Option<NameId>,
    pub base_type: Option<TypeRefResult>,
    pub derivation_method: Option<DerivationMethod>,
    pub content: ComplexContentResult,
    pub attributes: Vec<AttributeUseResult>,
    pub attribute_groups: Vec<QNameRef>,
    pub attribute_wildcard: Option<WildcardResult>,
    pub mixed: bool,
    pub is_abstract: bool,
    pub final_derivation: DerivationSet,
    pub block: DerivationSet,
    pub default_attributes_apply: bool,
    pub id: Option<String>,
    pub annotation: Option<Annotation>,
    pub source: Option<SourceRef>,
}

/// Derivation method
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DerivationMethod {
    Restriction,
    Extension,
}

/// Complex content result
#[derive(Debug, Clone)]
pub enum ComplexContentResult {
    Empty,
    Simple(SimpleContentDefResult),
    Complex(ComplexContentDefResult),
}

/// Simple content definition result
#[derive(Debug, Clone)]
pub struct SimpleContentDefResult {
    pub base_type: Option<TypeRefResult>,
    pub derivation: DerivationMethod,
    pub facets: FacetSet,
    pub attributes: Vec<AttributeUseResult>,
    pub attribute_groups: Vec<QNameRef>,
    pub attribute_wildcard: Option<WildcardResult>,
    pub assertions: Vec<AssertResult>,
    pub id: Option<String>,
    pub derivation_id: Option<String>,
    pub source: Option<SourceRef>,
}

/// Complex content definition result
#[derive(Debug, Clone)]
pub struct ComplexContentDefResult {
    pub particle: Option<ParticleResult>,
    pub derivation: DerivationMethod,
    pub mixed: bool,
    pub base_type: Option<TypeRefResult>,
    pub open_content: Option<OpenContentResult>,
    pub attributes: Vec<AttributeUseResult>,
    pub attribute_groups: Vec<QNameRef>,
    pub attribute_wildcard: Option<WildcardResult>,
    pub assertions: Vec<AssertResult>,
    pub id: Option<String>,
    pub derivation_id: Option<String>,
    pub source: Option<SourceRef>,
}

/// Element declaration result
#[derive(Debug, Clone)]
pub struct ElementFrameResult {
    pub name: Option<NameId>,
    pub ref_name: Option<QNameRef>,
    pub target_namespace: Option<NameId>,
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
}

/// Attribute declaration result
#[derive(Debug, Clone)]
pub struct AttributeFrameResult {
    pub name: Option<NameId>,
    pub ref_name: Option<QNameRef>,
    pub target_namespace: Option<NameId>,
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
}

/// Attribute use result (attribute within complex type)
#[derive(Debug, Clone)]
pub struct AttributeUseResult {
    pub attribute: AttributeFrameResult,
    pub use_kind: AttributeUseKind,
}

/// Attribute use kind
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AttributeUseKind {
    #[default]
    Optional,
    Required,
    Prohibited,
}

/// Group definition result
#[derive(Debug)]
pub enum GroupFrameResult {
    Model(ModelGroupDefResult),
    Attribute(AttributeGroupDefResult),
}

/// Model group definition result
#[derive(Debug, Clone)]
pub struct ModelGroupDefResult {
    pub name: Option<NameId>,
    pub ref_name: Option<QNameRef>,
    pub compositor: Option<Compositor>,
    pub particles: Vec<ParticleResult>,
    pub min_occurs: u32,
    pub max_occurs: Option<u32>,
    pub id: Option<String>,
    pub annotation: Option<Annotation>,
    pub source: Option<SourceRef>,
}

/// Attribute group definition result
#[derive(Debug, Clone)]
pub struct AttributeGroupDefResult {
    pub name: Option<NameId>,
    pub ref_name: Option<QNameRef>,
    pub attributes: Vec<AttributeUseResult>,
    pub attribute_groups: Vec<QNameRef>,
    pub attribute_wildcard: Option<WildcardResult>,
    pub id: Option<String>,
    pub annotation: Option<Annotation>,
    pub source: Option<SourceRef>,
}

/// Compositor type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compositor {
    Sequence,
    Choice,
    All,
}

/// Particle result
#[derive(Debug, Clone)]
pub struct ParticleResult {
    pub term: ParticleTerm,
    pub min_occurs: u32,
    pub max_occurs: Option<u32>,
    pub source: Option<SourceRef>,
}

/// Particle term
#[derive(Debug, Clone)]
pub enum ParticleTerm {
    Element(ElementFrameResult),
    Group(ModelGroupDefResult),
    Any(WildcardResult),
}

/// Type reference result
#[derive(Debug, Clone)]
pub enum TypeRefResult {
    QName(QNameRef),
    Inline(Box<TypeFrameResult>),
}

/// QName reference (unresolved)
#[derive(Debug, Clone)]
pub struct QNameRef {
    pub prefix: Option<NameId>,
    pub local_name: NameId,
    pub namespace: Option<NameId>,
}

/// Facet result
#[derive(Debug, Clone)]
pub struct FacetResult {
    pub kind: FacetKind,
    pub value: String,
    pub fixed: bool,
    pub annotation: Option<Annotation>,
    pub source: Option<SourceRef>,
}

/// Restriction result
#[derive(Debug, Clone)]
pub struct RestrictionResult {
    pub base_type: Option<TypeRefResult>,
    pub inline_type: Option<SimpleTypeResult>,
    pub facets: FacetSet,
    pub particle: Option<ParticleResult>,
    pub open_content: Option<OpenContentResult>,
    pub attributes: Vec<AttributeUseResult>,
    pub attribute_groups: Vec<QNameRef>,
    pub attribute_wildcard: Option<WildcardResult>,
    pub assertions: Vec<AssertResult>,
    pub id: Option<String>,
    pub annotation: Option<Annotation>,
    pub source: Option<SourceRef>,
}

/// Extension result
#[derive(Debug, Clone)]
pub struct ExtensionResult {
    pub base_type: Option<TypeRefResult>,
    pub particle: Option<ParticleResult>,
    pub open_content: Option<OpenContentResult>,
    pub attributes: Vec<AttributeUseResult>,
    pub attribute_groups: Vec<QNameRef>,
    pub attribute_wildcard: Option<WildcardResult>,
    pub assertions: Vec<AssertResult>,
    pub id: Option<String>,
    pub annotation: Option<Annotation>,
    pub source: Option<SourceRef>,
}

/// Facet kind
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FacetKind {
    Enumeration,
    Pattern,
    MinInclusive,
    MaxInclusive,
    MinExclusive,
    MaxExclusive,
    MinLength,
    MaxLength,
    Length,
    TotalDigits,
    FractionDigits,
    WhiteSpace,
    // XSD 1.1 facets
    Assertion,
    ExplicitTimezone,
}

/// Selector result
#[derive(Debug, Clone)]
pub struct SelectorResult {
    pub xpath: String,
    pub xpath_default_namespace: Option<String>,
    pub id: Option<String>,
    pub annotation: Option<Annotation>,
    pub source: Option<SourceRef>,
}

/// Notation declaration result
#[derive(Debug, Clone)]
pub struct NotationResult {
    pub name: Option<NameId>,
    pub public: Option<String>,
    pub system: Option<String>,
    pub id: Option<String>,
    pub annotation: Option<Annotation>,
    pub source: Option<SourceRef>,
}

/// Field result
#[derive(Debug, Clone)]
pub struct FieldResult {
    pub xpath: String,
    pub xpath_default_namespace: Option<String>,
    pub id: Option<String>,
    pub annotation: Option<Annotation>,
    pub source: Option<SourceRef>,
}

/// Assert result (XSD 1.1)
#[derive(Debug, Clone)]
pub struct AssertResult {
    pub test: String,
    pub xpath_default_namespace: Option<String>,
    pub id: Option<String>,
    pub annotation: Option<Annotation>,
    pub source: Option<SourceRef>,
}

/// Alternative result (XSD 1.1)
#[derive(Debug, Clone)]
pub struct AlternativeResult {
    pub test: Option<String>,
    pub type_ref: Option<TypeRefResult>,
    pub inline_type: Option<Box<TypeFrameResult>>,
    pub xpath_default_namespace: Option<String>,
    pub id: Option<String>,
    pub annotation: Option<Annotation>,
    pub source: Option<SourceRef>,
}

/// Open content result (XSD 1.1)
#[derive(Debug, Clone)]
pub struct OpenContentResult {
    pub mode: OpenContentMode,
    pub wildcard: Option<WildcardResult>,
    pub id: Option<String>,
    pub annotation: Option<Annotation>,
    pub source: Option<SourceRef>,
}

/// Default open content result (XSD 1.1)
#[derive(Debug, Clone)]
pub struct DefaultOpenContentResult {
    pub mode: OpenContentMode,
    pub applies_to_empty: bool,
    pub wildcard: Option<WildcardResult>,
    pub id: Option<String>,
    pub annotation: Option<Annotation>,
    pub source: Option<SourceRef>,
}

/// Identity constraint result
#[derive(Debug, Clone)]
pub struct IdentityResult {
    pub kind: IdentityKind,
    pub name: NameId,
    pub ref_name: Option<QNameRef>,
    pub refer: Option<QNameRef>,
    pub selector: SelectorResult,
    pub fields: Vec<FieldResult>,
    pub id: Option<String>,
    pub annotation: Option<Annotation>,
    pub source: Option<SourceRef>,
}

/// Identity constraint kind
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityKind {
    Key,
    Keyref,
    Unique,
}

/// Wildcard result
#[derive(Debug, Clone)]
pub struct WildcardResult {
    pub namespace: WildcardNamespace,
    pub process_contents: ProcessContents,
    pub not_namespace: Option<String>,
    pub not_qname: Option<String>,
    pub id: Option<String>,
    pub annotation: Option<Annotation>,
    pub source: Option<SourceRef>,
}

/// Wildcard namespace constraint
#[derive(Debug, Clone)]
pub enum WildcardNamespace {
    Any,
    Other,
    TargetNamespace,
    Local,
    List(Vec<Option<NameId>>),
}

/// Process contents mode
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProcessContents {
    #[default]
    Strict,
    Lax,
    Skip,
}

/// Open content mode (XSD 1.1)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenContentMode {
    None,
    Interleave,
    Suffix,
}

/// Directive result (include/import/redefine/override)
#[derive(Debug)]
pub enum DirectiveResult {
    Include(IncludeResult),
    Import(ImportResult),
    Redefine(RedefineResult),
    Override(OverrideResult),
}

/// Include directive result
#[derive(Debug)]
pub struct IncludeResult {
    pub schema_location: String,
    pub id: Option<String>,
    pub annotation: Option<Annotation>,
    pub source: Option<SourceRef>,
}

/// Import directive result
#[derive(Debug)]
pub struct ImportResult {
    pub namespace: Option<String>,
    pub schema_location: Option<String>,
    pub id: Option<String>,
    pub annotation: Option<Annotation>,
    pub source: Option<SourceRef>,
}

/// Redefine directive result
#[derive(Debug)]
pub struct RedefineResult {
    pub schema_location: String,
    pub id: Option<String>,
    pub annotation: Option<Annotation>,
    pub components: Vec<RedefineComponent>,
    pub source: Option<SourceRef>,
}

/// Redefine component
#[derive(Debug)]
pub enum RedefineComponent {
    SimpleType(SimpleTypeResult),
    ComplexType(ComplexTypeResult),
    Group(ModelGroupDefResult),
    AttributeGroup(AttributeGroupDefResult),
}

/// Override directive result (XSD 1.1)
#[derive(Debug)]
pub struct OverrideResult {
    pub schema_location: String,
    pub id: Option<String>,
    pub annotation: Option<Annotation>,
    pub source: Option<SourceRef>,
    // Overridden components (schemaTop)
    pub simple_types: Vec<SimpleTypeResult>,
    pub complex_types: Vec<ComplexTypeResult>,
    pub elements: Vec<ElementFrameResult>,
    pub attributes: Vec<AttributeFrameResult>,
    pub groups: Vec<GroupFrameResult>,
    pub attribute_groups: Vec<GroupFrameResult>,
    pub notations: Vec<NotationResult>,
}

// ============================================================================
// Frame trait and implementation
// ============================================================================

/// Parser frame trait for handling XSD elements
pub trait Frame {
    /// Check if a child element is allowed in the current phase
    fn allows(&self, local_name: &str, name_table: &NameTable) -> bool;

    /// Check if an attribute is allowed on this element
    fn allows_attribute(&self, local_name: &str, name_table: &NameTable) -> bool;

    /// Validate attributes for this element
    fn validate_attributes(
        &self,
        attrs: &AttributeMap,
        name_table: &NameTable,
    ) -> SchemaResult<()> {
        for name_id in attrs.names() {
            let local_name = match name_table.try_resolve(name_id) {
                Some(name) => name,
                None => continue,
            };

            if !self.allows_attribute(local_name, name_table) {
                return Err(SchemaError::structural(
                    "sch-attributes",
                    format!("Attribute '{}' is not allowed here", local_name),
                    None,
                ));
            }
        }

        Ok(())
    }

    /// Called when a child element is pushed
    fn on_child_start(&mut self, local_name: &str, name_table: &NameTable);

    /// Attach a completed child frame result
    fn attach(&mut self, child: FrameResult) -> SchemaResult<()>;

    /// Finish processing and return result
    fn finish(self: Box<Self>) -> SchemaResult<FrameResult>;

    /// Get the source location
    fn source(&self) -> Option<&SourceRef>;

    /// Set foreign attributes
    fn set_foreign_attributes(&mut self, attrs: Vec<ForeignAttribute>);

    /// Returns true if this is a SkipFrame (for error recovery depth tracking)
    fn is_skip_frame(&self) -> bool {
        false
    }

    /// Called when leaving a child element in a skip frame
    /// Returns true if the skip frame is complete (depth reached 0)
    fn on_child_end(&mut self) -> bool {
        false
    }

    /// Check if this frame accepts text content
    fn accepts_text(&self) -> bool {
        false
    }

    /// Handle text content (for annotation content like appinfo/documentation)
    fn on_text(&mut self, _text: &str) {}

    /// Handle CDATA content (for annotation content like appinfo/documentation)
    fn on_cdata(&mut self, _cdata: &str) {}

    /// Set namespace context snapshot (for annotation content)
    fn set_namespaces(&mut self, _namespaces: NamespaceContextSnapshot) {}
}

// ============================================================================
// Schema Frame
// ============================================================================

/// Parsing phase for schema element
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SchemaPhase {
    /// Expecting composition elements (include, import, redefine, override)
    Composition,
    /// Expecting annotation or components
    Components,
}

/// Frame for xs:schema element
pub struct SchemaFrame {
    phase: SchemaPhase,
    target_namespace: Option<NameId>,
    element_form_default: Option<String>,
    attribute_form_default: Option<String>,
    block_default: DerivationSet,
    default_attributes: Option<QNameRef>,
    xpath_default_namespace: Option<String>,
    final_default: DerivationSet,
    version: Option<String>,
    default_open_content: Option<DefaultOpenContentResult>,
    xml_lang: Option<String>,
    id: Option<String>,
    source: Option<SourceRef>,
    annotations: Vec<Annotation>,
    directives: Vec<DirectiveResult>,
    components: Vec<FrameResult>,
    foreign_attributes: Vec<ForeignAttribute>,
    xml_namespace_id: Option<NameId>,
    xml_lang_id: Option<NameId>,
}

impl SchemaFrame {
    pub fn new(
        attrs: &AttributeMap,
        name_table: &NameTable,
        source: Option<SourceRef>,
    ) -> SchemaResult<Self> {
        let target_namespace = attrs
            .get_value_by_name(name_table, "targetNamespace")
            .map(|s| name_table.get(s).unwrap_or_else(|| {
                // This shouldn't happen in normal parsing flow
                NameId(0)
            }));

        validate_attr_value(attrs, name_table, "elementFormDefault", parse_form)?;
        let element_form_default = attrs
            .get_value_by_name(name_table, "elementFormDefault")
            .map(String::from);

        validate_attr_value(attrs, name_table, "attributeFormDefault", parse_form)?;
        let attribute_form_default = attrs
            .get_value_by_name(name_table, "attributeFormDefault")
            .map(String::from);

        let block_default = parse_derivation_set(
            attrs.get_value_by_name(name_table, "blockDefault"),
        )?;

        let default_attributes = attrs
            .get_value_by_name(name_table, "defaultAttributes")
            .map(|s| parse_qname_ref(s, name_table))
            .transpose()?;

        let xpath_default_namespace = attrs
            .get_value_by_name(name_table, "xpathDefaultNamespace")
            .map(String::from);

        let final_default = parse_derivation_set(
            attrs.get_value_by_name(name_table, "finalDefault"),
        )?;

        let version = attrs
            .get_value_by_name(name_table, "version")
            .map(String::from);

        let id = attrs
            .get_value_by_name(name_table, "id")
            .map(String::from);

        Ok(Self {
            phase: SchemaPhase::Composition,
            target_namespace,
            element_form_default,
            attribute_form_default,
            block_default,
            default_attributes,
            xpath_default_namespace,
            final_default,
            version,
            default_open_content: None,
            xml_lang: None,
            id,
            source,
            annotations: Vec::new(),
            directives: Vec::new(),
            components: Vec::new(),
            foreign_attributes: Vec::new(),
            xml_namespace_id: name_table.get(crate::namespace::XML_NAMESPACE),
            xml_lang_id: name_table.get("lang"),
        })
    }
}

impl Frame for SchemaFrame {
    fn allows(&self, local_name: &str, _name_table: &NameTable) -> bool {
        match self.phase {
            SchemaPhase::Composition => matches!(
                local_name,
                xsd_names::INCLUDE
                    | xsd_names::IMPORT
                    | xsd_names::REDEFINE
                    | xsd_names::OVERRIDE
                    | xsd_names::ANNOTATION
                    | xsd_names::SIMPLE_TYPE
                    | xsd_names::COMPLEX_TYPE
                    | xsd_names::ELEMENT
                    | xsd_names::ATTRIBUTE
                    | xsd_names::GROUP
                    | xsd_names::ATTRIBUTE_GROUP
                    | xsd_names::NOTATION
                    | xsd_names::DEFAULT_OPEN_CONTENT
            ),
            SchemaPhase::Components => matches!(
                local_name,
                xsd_names::ANNOTATION
                    | xsd_names::SIMPLE_TYPE
                    | xsd_names::COMPLEX_TYPE
                    | xsd_names::ELEMENT
                    | xsd_names::ATTRIBUTE
                    | xsd_names::GROUP
                    | xsd_names::ATTRIBUTE_GROUP
                    | xsd_names::NOTATION
            ),
        }
    }

    fn allows_attribute(&self, local_name: &str, _name_table: &NameTable) -> bool {
        matches!(
            local_name,
            "targetNamespace"
                | "elementFormDefault"
                | "attributeFormDefault"
                | "blockDefault"
                | "defaultAttributes"
                | "xpathDefaultNamespace"
                | "finalDefault"
                | "version"
                | "id"
        )
    }

    fn on_child_start(&mut self, local_name: &str, _name_table: &NameTable) {
        // Transition from Composition to Components when we see a component
        if self.phase == SchemaPhase::Composition {
            match local_name {
                xsd_names::SIMPLE_TYPE
                | xsd_names::COMPLEX_TYPE
                | xsd_names::ELEMENT
                | xsd_names::ATTRIBUTE
                | xsd_names::GROUP
                | xsd_names::ATTRIBUTE_GROUP
                | xsd_names::NOTATION => {
                    self.phase = SchemaPhase::Components;
                }
                _ => {}
            }
        }
    }

    fn attach(&mut self, child: FrameResult) -> SchemaResult<()> {
        match child {
            FrameResult::Annotation(ann) => {
                self.annotations.push(ann);
            }
            FrameResult::Directive(dir) => {
                self.directives.push(dir);
            }
            FrameResult::Type(_)
            | FrameResult::Element(_)
            | FrameResult::Attribute(_)
            | FrameResult::Group(_)
            | FrameResult::Notation(_) => {
                self.components.push(child);
            }
            FrameResult::DefaultOpenContent(doc) => {
                self.default_open_content = Some(doc);
            }
            FrameResult::Skip => {}
            _ => {}
        }
        Ok(())
    }

    fn finish(self: Box<Self>) -> SchemaResult<FrameResult> {
        Ok(FrameResult::Schema(SchemaFrameResult {
            target_namespace: self.target_namespace,
            element_form_default: self.element_form_default,
            attribute_form_default: self.attribute_form_default,
            block_default: self.block_default,
            default_attributes: self.default_attributes,
            xpath_default_namespace: self.xpath_default_namespace,
            final_default: self.final_default,
            version: self.version,
            default_open_content: self.default_open_content,
            xml_lang: self.xml_lang,
            id: self.id,
            source: self.source,
            annotations: self.annotations,
            directives: self.directives,
            components: self.components,
        }))
    }

    fn source(&self) -> Option<&SourceRef> {
        self.source.as_ref()
    }

    fn set_foreign_attributes(&mut self, attrs: Vec<ForeignAttribute>) {
        if let (Some(xml_ns), Some(lang_id)) = (self.xml_namespace_id, self.xml_lang_id) {
            if let Some(attr) = attrs
                .iter()
                .find(|attr| attr.namespace == Some(xml_ns) && attr.local_name == lang_id)
            {
                self.xml_lang = Some(attr.value.clone());
            }
        }
        self.foreign_attributes = attrs;
    }
}

// ============================================================================
// Simple Type Frame
// ============================================================================

/// Parsing phase for simpleType
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SimpleTypePhase {
    /// Expecting annotation
    Annotation,
    /// Expecting restriction, list, or union
    Derivation,
    /// Done
    Done,
}

/// Frame for xs:simpleType element
pub struct SimpleTypeFrame {
    phase: SimpleTypePhase,
    name: Option<NameId>,
    final_derivation: DerivationSet,
    id: Option<String>,
    derivation_id: Option<String>,
    variety: Option<SimpleTypeVariety>,
    base_type: Option<TypeRefResult>,
    item_type: Option<TypeRefResult>,
    member_types: Vec<TypeRefResult>,
    facets: FacetSet,
    annotation: Option<Annotation>,
    source: Option<SourceRef>,
    foreign_attributes: Vec<ForeignAttribute>,
}

impl SimpleTypeFrame {
    pub fn new(
        attrs: &AttributeMap,
        name_table: &NameTable,
        source: Option<SourceRef>,
    ) -> SchemaResult<Self> {
        let name = attrs
            .get_value_by_name(name_table, "name")
            .and_then(|s| name_table.get(s));

        let final_derivation = parse_derivation_set(
            attrs.get_value_by_name(name_table, "final"),
        )?;

        let id = attrs
            .get_value_by_name(name_table, "id")
            .map(String::from);

        Ok(Self {
            phase: SimpleTypePhase::Annotation,
            name,
            final_derivation,
            id,
            derivation_id: None,
            variety: None,
            base_type: None,
            item_type: None,
            member_types: Vec::new(),
            facets: FacetSet::new(),
            annotation: None,
            source,
            foreign_attributes: Vec::new(),
        })
    }
}

impl Frame for SimpleTypeFrame {
    fn allows(&self, local_name: &str, _name_table: &NameTable) -> bool {
        match self.phase {
            SimpleTypePhase::Annotation => matches!(
                local_name,
                xsd_names::ANNOTATION | xsd_names::RESTRICTION | xsd_names::LIST | xsd_names::UNION
            ),
            SimpleTypePhase::Derivation => matches!(
                local_name,
                xsd_names::RESTRICTION | xsd_names::LIST | xsd_names::UNION
            ),
            SimpleTypePhase::Done => false,
        }
    }

    fn allows_attribute(&self, local_name: &str, _name_table: &NameTable) -> bool {
        matches!(local_name, "name" | "final" | "id")
    }

    fn on_child_start(&mut self, local_name: &str, _name_table: &NameTable) {
        match local_name {
            xsd_names::ANNOTATION => {
                self.phase = SimpleTypePhase::Derivation;
            }
            xsd_names::RESTRICTION | xsd_names::LIST | xsd_names::UNION => {
                self.phase = SimpleTypePhase::Done;
            }
            _ => {}
        }
    }

    fn attach(&mut self, child: FrameResult) -> SchemaResult<()> {
        match child {
            FrameResult::Annotation(ann) => {
                self.annotation = Some(ann);
            }
            FrameResult::Type(TypeFrameResult::Simple(st)) => {
                // Inline type from restriction/list/union
                self.variety = Some(st.variety);
                self.base_type = st.base_type;
                self.item_type = st.item_type;
                self.member_types = st.member_types;
                self.facets = st.facets;
                self.derivation_id = st.derivation_id;
            }
            FrameResult::Restriction(res) => {
                let base = if let Some(inline) = res.inline_type {
                    Some(TypeRefResult::Inline(Box::new(TypeFrameResult::Simple(inline))))
                } else {
                    res.base_type
                };
                self.variety = Some(SimpleTypeVariety::Atomic);
                self.base_type = base;
                self.facets = res.facets;
                self.derivation_id = res.id;
            }
            FrameResult::Skip => {}
            _ => {}
        }
        Ok(())
    }

    fn finish(self: Box<Self>) -> SchemaResult<FrameResult> {
        Ok(FrameResult::Type(TypeFrameResult::Simple(SimpleTypeResult {
            name: self.name,
            variety: self.variety.unwrap_or(SimpleTypeVariety::Atomic),
            base_type: self.base_type,
            item_type: self.item_type,
            member_types: self.member_types,
            facets: self.facets,
            final_derivation: self.final_derivation,
            id: self.id,
            derivation_id: self.derivation_id,
            annotation: self.annotation,
            source: self.source,
        })))
    }

    fn source(&self) -> Option<&SourceRef> {
        self.source.as_ref()
    }

    fn set_foreign_attributes(&mut self, attrs: Vec<ForeignAttribute>) {
        self.foreign_attributes = attrs;
    }
}

// ============================================================================
// Restriction Frame
// ============================================================================

/// Frame for xs:restriction
pub struct RestrictionFrame {
    base_type: Option<TypeRefResult>,
    facets: FacetSet,
    particle: Option<ParticleResult>,
    open_content: Option<OpenContentResult>,
    attributes: Vec<AttributeUseResult>,
    attribute_groups: Vec<QNameRef>,
    attribute_wildcard: Option<WildcardResult>,
    assertions: Vec<AssertResult>,
    id: Option<String>,
    annotation: Option<Annotation>,
    inline_type: Option<SimpleTypeResult>,
    source: Option<SourceRef>,
    foreign_attributes: Vec<ForeignAttribute>,
}

impl RestrictionFrame {
    pub fn new(
        attrs: &AttributeMap,
        name_table: &NameTable,
        source: Option<SourceRef>,
    ) -> SchemaResult<Self> {
        let base_type = attrs
            .get_value_by_name(name_table, "base")
            .map(|s| parse_qname_ref(s, name_table))
            .transpose()?
            .map(TypeRefResult::QName);

        let id = attrs
            .get_value_by_name(name_table, "id")
            .map(String::from);

        Ok(Self {
            base_type,
            facets: FacetSet::new(),
            particle: None,
            open_content: None,
            attributes: Vec::new(),
            attribute_groups: Vec::new(),
            attribute_wildcard: None,
            assertions: Vec::new(),
            id,
            annotation: None,
            inline_type: None,
            source,
            foreign_attributes: Vec::new(),
        })
    }
}

impl Frame for RestrictionFrame {
    fn allows(&self, local_name: &str, _name_table: &NameTable) -> bool {
        matches!(
            local_name,
            xsd_names::ANNOTATION
                | xsd_names::SIMPLE_TYPE
                | xsd_names::ENUMERATION
                | xsd_names::PATTERN
                | xsd_names::MIN_INCLUSIVE
                | xsd_names::MAX_INCLUSIVE
                | xsd_names::MIN_EXCLUSIVE
                | xsd_names::MAX_EXCLUSIVE
                | xsd_names::MIN_LENGTH
                | xsd_names::MAX_LENGTH
                | xsd_names::LENGTH
                | xsd_names::TOTAL_DIGITS
                | xsd_names::FRACTION_DIGITS
                | xsd_names::WHITE_SPACE
                | xsd_names::SEQUENCE
                | xsd_names::CHOICE
                | xsd_names::ALL
                | xsd_names::GROUP
                | xsd_names::ATTRIBUTE
                | xsd_names::ATTRIBUTE_GROUP
                | xsd_names::ANY_ATTRIBUTE
                | xsd_names::OPEN_CONTENT
                | xsd_names::ASSERT
        )
    }

    fn allows_attribute(&self, local_name: &str, _name_table: &NameTable) -> bool {
        matches!(local_name, "base" | "id")
    }

    fn on_child_start(&mut self, _local_name: &str, _name_table: &NameTable) {}

    fn attach(&mut self, child: FrameResult) -> SchemaResult<()> {
        match child {
            FrameResult::Annotation(ann) => {
                self.annotation = Some(ann);
            }
            FrameResult::Type(TypeFrameResult::Simple(st)) => {
                self.inline_type = Some(st);
            }
            FrameResult::Facet(facet) => {
                apply_facet(&mut self.facets, facet)?;
            }
            FrameResult::Particle(particle) => {
                self.particle = Some(particle);
            }
            FrameResult::OpenContent(open_content) => {
                self.open_content = Some(open_content);
            }
            FrameResult::Attribute(attr) => {
                self.attributes.push(AttributeUseResult {
                    attribute: attr,
                    use_kind: AttributeUseKind::Optional,
                });
            }
            FrameResult::Group(GroupFrameResult::Attribute(ag)) => {
                if let Some(ref_name) = ag.ref_name {
                    self.attribute_groups.push(ref_name);
                }
            }
            FrameResult::Group(GroupFrameResult::Model(mg)) => {
                let min_occurs = mg.min_occurs;
                let max_occurs = mg.max_occurs;
                self.particle = Some(ParticleResult {
                    term: ParticleTerm::Group(mg),
                    min_occurs,
                    max_occurs,
                    source: None,
                });
            }
            FrameResult::Wildcard(wc) => {
                self.attribute_wildcard = Some(wc);
            }
            FrameResult::Assert(assertion) => {
                self.assertions.push(assertion);
            }
            FrameResult::Skip => {}
            _ => {}
        }
        Ok(())
    }

    fn finish(self: Box<Self>) -> SchemaResult<FrameResult> {
        Ok(FrameResult::Restriction(RestrictionResult {
            base_type: self.base_type,
            inline_type: self.inline_type,
            facets: self.facets,
            particle: self.particle,
            open_content: self.open_content,
            attributes: self.attributes,
            attribute_groups: self.attribute_groups,
            attribute_wildcard: self.attribute_wildcard,
            assertions: self.assertions,
            id: self.id,
            annotation: self.annotation,
            source: self.source,
        }))
    }

    fn source(&self) -> Option<&SourceRef> {
        self.source.as_ref()
    }

    fn set_foreign_attributes(&mut self, attrs: Vec<ForeignAttribute>) {
        self.foreign_attributes = attrs;
    }
}

// ============================================================================
// Extension Frame
// ============================================================================

/// Frame for xs:extension
pub struct ExtensionFrame {
    base_type: Option<TypeRefResult>,
    particle: Option<ParticleResult>,
    open_content: Option<OpenContentResult>,
    attributes: Vec<AttributeUseResult>,
    attribute_groups: Vec<QNameRef>,
    attribute_wildcard: Option<WildcardResult>,
    assertions: Vec<AssertResult>,
    id: Option<String>,
    annotation: Option<Annotation>,
    source: Option<SourceRef>,
    foreign_attributes: Vec<ForeignAttribute>,
}

impl ExtensionFrame {
    pub fn new(
        attrs: &AttributeMap,
        name_table: &NameTable,
        source: Option<SourceRef>,
    ) -> SchemaResult<Self> {
        let base_type = attrs
            .get_value_by_name(name_table, "base")
            .map(|s| parse_qname_ref(s, name_table))
            .transpose()?
            .map(TypeRefResult::QName);

        let id = attrs
            .get_value_by_name(name_table, "id")
            .map(String::from);

        Ok(Self {
            base_type,
            particle: None,
            open_content: None,
            attributes: Vec::new(),
            attribute_groups: Vec::new(),
            attribute_wildcard: None,
            assertions: Vec::new(),
            id,
            annotation: None,
            source,
            foreign_attributes: Vec::new(),
        })
    }
}

impl Frame for ExtensionFrame {
    fn allows(&self, local_name: &str, _name_table: &NameTable) -> bool {
        matches!(
            local_name,
            xsd_names::ANNOTATION
                | xsd_names::OPEN_CONTENT
                | xsd_names::SEQUENCE
                | xsd_names::CHOICE
                | xsd_names::ALL
                | xsd_names::GROUP
                | xsd_names::ATTRIBUTE
                | xsd_names::ATTRIBUTE_GROUP
                | xsd_names::ANY_ATTRIBUTE
                | xsd_names::ASSERT
        )
    }

    fn allows_attribute(&self, local_name: &str, _name_table: &NameTable) -> bool {
        matches!(local_name, "base" | "id")
    }

    fn on_child_start(&mut self, _local_name: &str, _name_table: &NameTable) {}

    fn attach(&mut self, child: FrameResult) -> SchemaResult<()> {
        match child {
            FrameResult::Annotation(ann) => {
                self.annotation = Some(ann);
            }
            FrameResult::Particle(particle) => {
                self.particle = Some(particle);
            }
            FrameResult::OpenContent(open_content) => {
                self.open_content = Some(open_content);
            }
            FrameResult::Attribute(attr) => {
                self.attributes.push(AttributeUseResult {
                    attribute: attr,
                    use_kind: AttributeUseKind::Optional,
                });
            }
            FrameResult::Group(GroupFrameResult::Attribute(ag)) => {
                if let Some(ref_name) = ag.ref_name {
                    self.attribute_groups.push(ref_name);
                }
            }
            FrameResult::Group(GroupFrameResult::Model(mg)) => {
                let min_occurs = mg.min_occurs;
                let max_occurs = mg.max_occurs;
                self.particle = Some(ParticleResult {
                    term: ParticleTerm::Group(mg),
                    min_occurs,
                    max_occurs,
                    source: None,
                });
            }
            FrameResult::Wildcard(wc) => {
                self.attribute_wildcard = Some(wc);
            }
            FrameResult::Assert(assertion) => {
                self.assertions.push(assertion);
            }
            FrameResult::Skip => {}
            _ => {}
        }
        Ok(())
    }

    fn finish(self: Box<Self>) -> SchemaResult<FrameResult> {
        Ok(FrameResult::Extension(ExtensionResult {
            base_type: self.base_type,
            particle: self.particle,
            open_content: self.open_content,
            attributes: self.attributes,
            attribute_groups: self.attribute_groups,
            attribute_wildcard: self.attribute_wildcard,
            assertions: self.assertions,
            id: self.id,
            annotation: self.annotation,
            source: self.source,
        }))
    }

    fn source(&self) -> Option<&SourceRef> {
        self.source.as_ref()
    }

    fn set_foreign_attributes(&mut self, attrs: Vec<ForeignAttribute>) {
        self.foreign_attributes = attrs;
    }
}

// ============================================================================
// List Frame
// ============================================================================

/// Frame for xs:list within simpleType
pub struct ListFrame {
    item_type: Option<TypeRefResult>,
    id: Option<String>,
    annotation: Option<Annotation>,
    inline_type: Option<SimpleTypeResult>,
    source: Option<SourceRef>,
    foreign_attributes: Vec<ForeignAttribute>,
}

impl ListFrame {
    pub fn new(
        attrs: &AttributeMap,
        name_table: &NameTable,
        source: Option<SourceRef>,
    ) -> SchemaResult<Self> {
        let item_type = attrs
            .get_value_by_name(name_table, "itemType")
            .map(|s| parse_qname_ref(s, name_table))
            .transpose()?
            .map(TypeRefResult::QName);

        let id = attrs
            .get_value_by_name(name_table, "id")
            .map(String::from);

        Ok(Self {
            item_type,
            id,
            annotation: None,
            inline_type: None,
            source,
            foreign_attributes: Vec::new(),
        })
    }
}

impl Frame for ListFrame {
    fn allows(&self, local_name: &str, _name_table: &NameTable) -> bool {
        matches!(local_name, xsd_names::ANNOTATION | xsd_names::SIMPLE_TYPE)
    }

    fn allows_attribute(&self, local_name: &str, _name_table: &NameTable) -> bool {
        matches!(local_name, "itemType" | "id")
    }

    fn on_child_start(&mut self, _local_name: &str, _name_table: &NameTable) {}

    fn attach(&mut self, child: FrameResult) -> SchemaResult<()> {
        match child {
            FrameResult::Annotation(ann) => {
                self.annotation = Some(ann);
            }
            FrameResult::Type(TypeFrameResult::Simple(st)) => {
                self.inline_type = Some(st);
            }
            FrameResult::Skip => {}
            _ => {}
        }
        Ok(())
    }

    fn finish(self: Box<Self>) -> SchemaResult<FrameResult> {
        let item = if let Some(inline) = self.inline_type {
            Some(TypeRefResult::Inline(Box::new(TypeFrameResult::Simple(inline))))
        } else {
            self.item_type
        };

        Ok(FrameResult::Type(TypeFrameResult::Simple(SimpleTypeResult {
            name: None,
            variety: SimpleTypeVariety::List,
            base_type: None,
            item_type: item,
            member_types: Vec::new(),
            facets: FacetSet::new(),
            final_derivation: DerivationSet::empty(),
            id: None,
            derivation_id: self.id,
            annotation: self.annotation,
            source: self.source,
        })))
    }

    fn source(&self) -> Option<&SourceRef> {
        self.source.as_ref()
    }

    fn set_foreign_attributes(&mut self, attrs: Vec<ForeignAttribute>) {
        self.foreign_attributes = attrs;
    }
}

// ============================================================================
// Union Frame
// ============================================================================

/// Frame for xs:union within simpleType
pub struct UnionFrame {
    member_types: Vec<TypeRefResult>,
    id: Option<String>,
    annotation: Option<Annotation>,
    source: Option<SourceRef>,
    foreign_attributes: Vec<ForeignAttribute>,
}

impl UnionFrame {
    pub fn new(
        attrs: &AttributeMap,
        name_table: &NameTable,
        source: Option<SourceRef>,
    ) -> SchemaResult<Self> {
        let member_types = if let Some(s) = attrs.get_value_by_name(name_table, "memberTypes") {
            parse_qname_list(s, name_table)?
                .into_iter()
                .map(TypeRefResult::QName)
                .collect()
        } else {
            Vec::new()
        };

        let id = attrs
            .get_value_by_name(name_table, "id")
            .map(String::from);

        Ok(Self {
            member_types,
            id,
            annotation: None,
            source,
            foreign_attributes: Vec::new(),
        })
    }
}

impl Frame for UnionFrame {
    fn allows(&self, local_name: &str, _name_table: &NameTable) -> bool {
        matches!(local_name, xsd_names::ANNOTATION | xsd_names::SIMPLE_TYPE)
    }

    fn allows_attribute(&self, local_name: &str, _name_table: &NameTable) -> bool {
        matches!(local_name, "memberTypes" | "id")
    }

    fn on_child_start(&mut self, _local_name: &str, _name_table: &NameTable) {}

    fn attach(&mut self, child: FrameResult) -> SchemaResult<()> {
        match child {
            FrameResult::Annotation(ann) => {
                self.annotation = Some(ann);
            }
            FrameResult::Type(TypeFrameResult::Simple(st)) => {
                self.member_types.push(TypeRefResult::Inline(Box::new(
                    TypeFrameResult::Simple(st),
                )));
            }
            FrameResult::Skip => {}
            _ => {}
        }
        Ok(())
    }

    fn finish(self: Box<Self>) -> SchemaResult<FrameResult> {
        Ok(FrameResult::Type(TypeFrameResult::Simple(SimpleTypeResult {
            name: None,
            variety: SimpleTypeVariety::Union,
            base_type: None,
            item_type: None,
            member_types: self.member_types,
            facets: FacetSet::new(),
            final_derivation: DerivationSet::empty(),
            id: None,
            derivation_id: self.id,
            annotation: self.annotation,
            source: self.source,
        })))
    }

    fn source(&self) -> Option<&SourceRef> {
        self.source.as_ref()
    }

    fn set_foreign_attributes(&mut self, attrs: Vec<ForeignAttribute>) {
        self.foreign_attributes = attrs;
    }
}

// ============================================================================
// Simple/Complex Content Frames
// ============================================================================

/// Frame for xs:simpleContent
pub struct SimpleContentFrame {
    id: Option<String>,
    base_type: Option<TypeRefResult>,
    derivation: Option<DerivationMethod>,
    facets: FacetSet,
    attributes: Vec<AttributeUseResult>,
    attribute_groups: Vec<QNameRef>,
    attribute_wildcard: Option<WildcardResult>,
    assertions: Vec<AssertResult>,
    derivation_id: Option<String>,
    annotation: Option<Annotation>,
    source: Option<SourceRef>,
    foreign_attributes: Vec<ForeignAttribute>,
}

impl SimpleContentFrame {
    pub fn new(
        attrs: &AttributeMap,
        name_table: &NameTable,
        source: Option<SourceRef>,
    ) -> SchemaResult<Self> {
        let id = attrs
            .get_value_by_name(name_table, "id")
            .map(String::from);

        Ok(Self {
            id,
            base_type: None,
            derivation: None,
            facets: FacetSet::new(),
            attributes: Vec::new(),
            attribute_groups: Vec::new(),
            attribute_wildcard: None,
            assertions: Vec::new(),
            derivation_id: None,
            annotation: None,
            source,
            foreign_attributes: Vec::new(),
        })
    }
}

impl Frame for SimpleContentFrame {
    fn allows(&self, local_name: &str, _name_table: &NameTable) -> bool {
        matches!(
            local_name,
            xsd_names::ANNOTATION | xsd_names::RESTRICTION | xsd_names::EXTENSION
        )
    }

    fn allows_attribute(&self, local_name: &str, _name_table: &NameTable) -> bool {
        matches!(local_name, "id")
    }

    fn on_child_start(&mut self, _local_name: &str, _name_table: &NameTable) {}

    fn attach(&mut self, child: FrameResult) -> SchemaResult<()> {
        match child {
            FrameResult::Annotation(ann) => {
                self.annotation = Some(ann);
            }
            FrameResult::Restriction(res) => {
                let base = if let Some(inline) = res.inline_type {
                    Some(TypeRefResult::Inline(Box::new(TypeFrameResult::Simple(inline))))
                } else {
                    res.base_type
                };
                self.base_type = base;
                self.derivation = Some(DerivationMethod::Restriction);
                self.facets = res.facets;
                self.attributes = res.attributes;
                self.attribute_groups = res.attribute_groups;
                self.attribute_wildcard = res.attribute_wildcard;
                self.assertions = res.assertions;
                self.derivation_id = res.id;
            }
            FrameResult::Extension(res) => {
                self.base_type = res.base_type;
                self.derivation = Some(DerivationMethod::Extension);
                self.attributes = res.attributes;
                self.attribute_groups = res.attribute_groups;
                self.attribute_wildcard = res.attribute_wildcard;
                self.assertions = res.assertions;
                self.derivation_id = res.id;
            }
            FrameResult::Skip => {}
            _ => {}
        }
        Ok(())
    }

    fn finish(self: Box<Self>) -> SchemaResult<FrameResult> {
        let base_type = self.base_type.ok_or_else(|| SchemaError::structural(
            "sch-simple-content",
            "xs:simpleContent requires a base type",
            None,
        ))?;

        Ok(FrameResult::SimpleContent(SimpleContentDefResult {
            base_type: Some(base_type),
            derivation: self.derivation.unwrap_or(DerivationMethod::Restriction),
            facets: self.facets,
            attributes: self.attributes,
            attribute_groups: self.attribute_groups,
            attribute_wildcard: self.attribute_wildcard,
            assertions: self.assertions,
            id: self.id,
            derivation_id: self.derivation_id,
            source: self.source,
        }))
    }

    fn source(&self) -> Option<&SourceRef> {
        self.source.as_ref()
    }

    fn set_foreign_attributes(&mut self, attrs: Vec<ForeignAttribute>) {
        self.foreign_attributes = attrs;
    }
}

/// Frame for xs:complexContent
pub struct ComplexContentFrame {
    id: Option<String>,
    mixed: bool,
    base_type: Option<TypeRefResult>,
    derivation: Option<DerivationMethod>,
    particle: Option<ParticleResult>,
    open_content: Option<OpenContentResult>,
    attributes: Vec<AttributeUseResult>,
    attribute_groups: Vec<QNameRef>,
    attribute_wildcard: Option<WildcardResult>,
    assertions: Vec<AssertResult>,
    derivation_id: Option<String>,
    annotation: Option<Annotation>,
    source: Option<SourceRef>,
    foreign_attributes: Vec<ForeignAttribute>,
}

impl ComplexContentFrame {
    pub fn new(
        attrs: &AttributeMap,
        name_table: &NameTable,
        source: Option<SourceRef>,
    ) -> SchemaResult<Self> {
        let id = attrs
            .get_value_by_name(name_table, "id")
            .map(String::from);

        let mixed = parse_bool_attr_default(attrs, name_table, "mixed", false)?;

        Ok(Self {
            id,
            mixed,
            base_type: None,
            derivation: None,
            particle: None,
            open_content: None,
            attributes: Vec::new(),
            attribute_groups: Vec::new(),
            attribute_wildcard: None,
            assertions: Vec::new(),
            derivation_id: None,
            annotation: None,
            source,
            foreign_attributes: Vec::new(),
        })
    }
}

impl Frame for ComplexContentFrame {
    fn allows(&self, local_name: &str, _name_table: &NameTable) -> bool {
        matches!(
            local_name,
            xsd_names::ANNOTATION | xsd_names::RESTRICTION | xsd_names::EXTENSION
        )
    }

    fn allows_attribute(&self, local_name: &str, _name_table: &NameTable) -> bool {
        matches!(local_name, "id" | "mixed")
    }

    fn on_child_start(&mut self, _local_name: &str, _name_table: &NameTable) {}

    fn attach(&mut self, child: FrameResult) -> SchemaResult<()> {
        match child {
            FrameResult::Annotation(ann) => {
                self.annotation = Some(ann);
            }
            FrameResult::Restriction(res) => {
                self.base_type = res.base_type;
                self.derivation = Some(DerivationMethod::Restriction);
                self.particle = res.particle;
                self.open_content = res.open_content;
                self.attributes = res.attributes;
                self.attribute_groups = res.attribute_groups;
                self.attribute_wildcard = res.attribute_wildcard;
                self.assertions = res.assertions;
                self.derivation_id = res.id;
            }
            FrameResult::Extension(res) => {
                self.base_type = res.base_type;
                self.derivation = Some(DerivationMethod::Extension);
                self.particle = res.particle;
                self.open_content = res.open_content;
                self.attributes = res.attributes;
                self.attribute_groups = res.attribute_groups;
                self.attribute_wildcard = res.attribute_wildcard;
                self.assertions = res.assertions;
                self.derivation_id = res.id;
            }
            FrameResult::Skip => {}
            _ => {}
        }
        Ok(())
    }

    fn finish(self: Box<Self>) -> SchemaResult<FrameResult> {
        Ok(FrameResult::ComplexContent(ComplexContentDefResult {
            particle: self.particle,
            derivation: self.derivation.unwrap_or(DerivationMethod::Restriction),
            mixed: self.mixed,
            base_type: self.base_type,
            open_content: self.open_content,
            attributes: self.attributes,
            attribute_groups: self.attribute_groups,
            attribute_wildcard: self.attribute_wildcard,
            assertions: self.assertions,
            id: self.id,
            derivation_id: self.derivation_id,
            source: self.source,
        }))
    }

    fn source(&self) -> Option<&SourceRef> {
        self.source.as_ref()
    }

    fn set_foreign_attributes(&mut self, attrs: Vec<ForeignAttribute>) {
        self.foreign_attributes = attrs;
    }
}

// ============================================================================
// Complex Type Frame
// ============================================================================

/// Parsing phase for complexType
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ComplexTypePhase {
    Annotation,
    Content,
    Attributes,
    Done,
}

/// Frame for xs:complexType element
pub struct ComplexTypeFrame {
    phase: ComplexTypePhase,
    name: Option<NameId>,
    base_type: Option<TypeRefResult>,
    derivation_method: Option<DerivationMethod>,
    mixed: bool,
    is_abstract: bool,
    final_derivation: DerivationSet,
    block: DerivationSet,
    default_attributes_apply: bool,
    id: Option<String>,
    content: ComplexContentResult,
    open_content: Option<OpenContentResult>,
    attributes: Vec<AttributeUseResult>,
    attribute_groups: Vec<QNameRef>,
    attribute_wildcard: Option<WildcardResult>,
    assertions: Vec<AssertResult>,
    annotation: Option<Annotation>,
    source: Option<SourceRef>,
    foreign_attributes: Vec<ForeignAttribute>,
}

impl ComplexTypeFrame {
    pub fn new(
        attrs: &AttributeMap,
        name_table: &NameTable,
        source: Option<SourceRef>,
    ) -> SchemaResult<Self> {
        let name = attrs
            .get_value_by_name(name_table, "name")
            .and_then(|s| name_table.get(s));

        let mixed = parse_bool_attr_default(attrs, name_table, "mixed", false)?;

        let is_abstract = parse_bool_attr_default(attrs, name_table, "abstract", false)?;

        let final_derivation = parse_derivation_set(
            attrs.get_value_by_name(name_table, "final"),
        )?;

        let block = parse_derivation_set(
            attrs.get_value_by_name(name_table, "block"),
        )?;

        let default_attributes_apply =
            parse_bool_attr_default(attrs, name_table, "defaultAttributesApply", true)?;

        let id = attrs
            .get_value_by_name(name_table, "id")
            .map(String::from);

        Ok(Self {
            phase: ComplexTypePhase::Annotation,
            name,
            base_type: None,
            derivation_method: None,
            mixed,
            is_abstract,
            final_derivation,
            block,
            default_attributes_apply,
            id,
            content: ComplexContentResult::Empty,
            open_content: None,
            attributes: Vec::new(),
            attribute_groups: Vec::new(),
            attribute_wildcard: None,
            assertions: Vec::new(),
            annotation: None,
            source,
            foreign_attributes: Vec::new(),
        })
    }
}

impl Frame for ComplexTypeFrame {
    fn allows(&self, local_name: &str, _name_table: &NameTable) -> bool {
        match self.phase {
            ComplexTypePhase::Annotation => matches!(
                local_name,
                xsd_names::ANNOTATION
                    | xsd_names::SIMPLE_CONTENT
                    | xsd_names::COMPLEX_CONTENT
                    | xsd_names::OPEN_CONTENT
                    | xsd_names::SEQUENCE
                    | xsd_names::CHOICE
                    | xsd_names::ALL
                    | xsd_names::GROUP
                    | xsd_names::ATTRIBUTE
                    | xsd_names::ATTRIBUTE_GROUP
                    | xsd_names::ANY_ATTRIBUTE
                    | xsd_names::ASSERT
            ),
            ComplexTypePhase::Content => matches!(
                local_name,
                xsd_names::SIMPLE_CONTENT
                    | xsd_names::COMPLEX_CONTENT
                    | xsd_names::OPEN_CONTENT
                    | xsd_names::SEQUENCE
                    | xsd_names::CHOICE
                    | xsd_names::ALL
                    | xsd_names::GROUP
                    | xsd_names::ATTRIBUTE
                    | xsd_names::ATTRIBUTE_GROUP
                    | xsd_names::ANY_ATTRIBUTE
                    | xsd_names::ASSERT
            ),
            ComplexTypePhase::Attributes => matches!(
                local_name,
                xsd_names::ATTRIBUTE
                    | xsd_names::ATTRIBUTE_GROUP
                    | xsd_names::ANY_ATTRIBUTE
                    | xsd_names::ASSERT
            ),
            ComplexTypePhase::Done => false,
        }
    }

    fn allows_attribute(&self, local_name: &str, _name_table: &NameTable) -> bool {
        matches!(
            local_name,
            "name"
                | "mixed"
                | "abstract"
                | "final"
                | "block"
                | "defaultAttributesApply"
                | "id"
        )
    }

    fn on_child_start(&mut self, local_name: &str, _name_table: &NameTable) {
        match local_name {
            xsd_names::ANNOTATION => {
                self.phase = ComplexTypePhase::Content;
            }
            xsd_names::SIMPLE_CONTENT | xsd_names::COMPLEX_CONTENT => {
                self.phase = ComplexTypePhase::Done;
            }
            xsd_names::SEQUENCE | xsd_names::CHOICE | xsd_names::ALL | xsd_names::GROUP => {
                self.phase = ComplexTypePhase::Attributes;
            }
            xsd_names::ATTRIBUTE | xsd_names::ATTRIBUTE_GROUP => {
                self.phase = ComplexTypePhase::Attributes;
            }
            xsd_names::ANY_ATTRIBUTE => {
                self.phase = ComplexTypePhase::Done;
            }
            _ => {}
        }
    }

    fn attach(&mut self, child: FrameResult) -> SchemaResult<()> {
        match child {
            FrameResult::Annotation(ann) => {
                self.annotation = Some(ann);
            }
            FrameResult::OpenContent(open_content) => {
                self.open_content = Some(open_content);
            }
            FrameResult::SimpleContent(mut sc) => {
                self.base_type = sc
                    .base_type
                    .as_ref()
                    .and_then(|bt| match bt {
                        TypeRefResult::QName(qname) => {
                            Some(TypeRefResult::QName(qname.clone()))
                        }
                        _ => None,
                    });
                self.derivation_method = Some(sc.derivation);
                self.attributes = std::mem::take(&mut sc.attributes);
                self.attribute_groups = std::mem::take(&mut sc.attribute_groups);
                self.attribute_wildcard = sc.attribute_wildcard.take();
                self.content = ComplexContentResult::Simple(sc);
            }
            FrameResult::ComplexContent(mut cc) => {
                self.base_type = cc
                    .base_type
                    .as_ref()
                    .and_then(|bt| match bt {
                        TypeRefResult::QName(qname) => {
                            Some(TypeRefResult::QName(qname.clone()))
                        }
                        _ => None,
                    });
                self.derivation_method = Some(cc.derivation);
                self.attributes = std::mem::take(&mut cc.attributes);
                self.attribute_groups = std::mem::take(&mut cc.attribute_groups);
                self.attribute_wildcard = cc.attribute_wildcard.take();
                self.mixed = cc.mixed;
                self.content = ComplexContentResult::Complex(cc);
            }
            FrameResult::Particle(particle) => {
                self.content = ComplexContentResult::Complex(ComplexContentDefResult {
                    particle: Some(particle),
                    derivation: DerivationMethod::Restriction,
                    mixed: self.mixed,
                    base_type: None,
                    open_content: None,
                    attributes: Vec::new(),
                    attribute_groups: Vec::new(),
                    attribute_wildcard: None,
                    assertions: Vec::new(),
                    id: None,
                    derivation_id: None,
                    source: None,
                });
            }
            FrameResult::Attribute(attr) => {
                self.attributes.push(AttributeUseResult {
                    attribute: attr,
                    use_kind: AttributeUseKind::Optional,
                });
            }
            FrameResult::Group(GroupFrameResult::Attribute(ag)) => {
                if let Some(ref_name) = ag.ref_name {
                    self.attribute_groups.push(ref_name);
                }
            }
            FrameResult::Wildcard(wc) => {
                self.attribute_wildcard = Some(wc);
            }
            FrameResult::Assert(assertion) => {
                self.assertions.push(assertion);
            }
            FrameResult::Skip => {}
            _ => {}
        }
        Ok(())
    }

    fn finish(self: Box<Self>) -> SchemaResult<FrameResult> {
        let mut content = self.content;
        match &mut content {
            ComplexContentResult::Empty => {
                if self.open_content.is_some() || !self.assertions.is_empty() {
                    content = ComplexContentResult::Complex(ComplexContentDefResult {
                        particle: None,
                        derivation: DerivationMethod::Restriction,
                        mixed: self.mixed,
                        base_type: None,
                        open_content: self.open_content,
                        attributes: Vec::new(),
                        attribute_groups: Vec::new(),
                        attribute_wildcard: None,
                        assertions: self.assertions,
                        id: None,
                        derivation_id: None,
                        source: None,
                    });
                }
            }
            ComplexContentResult::Complex(cc) => {
                if cc.open_content.is_none() {
                    cc.open_content = self.open_content;
                }
                if !self.assertions.is_empty() {
                    cc.assertions.extend(self.assertions);
                }
            }
            ComplexContentResult::Simple(_) => {}
        }

        Ok(FrameResult::Type(TypeFrameResult::Complex(ComplexTypeResult {
            name: self.name,
            base_type: self.base_type,
            derivation_method: self.derivation_method,
            content,
            attributes: self.attributes,
            attribute_groups: self.attribute_groups,
            attribute_wildcard: self.attribute_wildcard,
            mixed: self.mixed,
            is_abstract: self.is_abstract,
            final_derivation: self.final_derivation,
            block: self.block,
            default_attributes_apply: self.default_attributes_apply,
            id: self.id,
            annotation: self.annotation,
            source: self.source,
        })))
    }

    fn source(&self) -> Option<&SourceRef> {
        self.source.as_ref()
    }

    fn set_foreign_attributes(&mut self, attrs: Vec<ForeignAttribute>) {
        self.foreign_attributes = attrs;
    }
}

// ============================================================================
// Element Frame
// ============================================================================

/// Parsing phase for element
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
enum ElementPhase {
    Annotation,
    Type,
    Identity,
    Done,
}

/// Frame for xs:element
pub struct ElementFrame {
    phase: ElementPhase,
    name: Option<NameId>,
    ref_name: Option<QNameRef>,
    target_namespace: Option<NameId>,
    type_ref: Option<TypeRefResult>,
    inline_type: Option<Box<TypeFrameResult>>,
    substitution_group: Vec<QNameRef>,
    default_value: Option<String>,
    fixed_value: Option<String>,
    nillable: bool,
    is_abstract: bool,
    min_occurs: u32,
    max_occurs: Option<u32>,
    block: DerivationSet,
    final_derivation: DerivationSet,
    form: Option<String>,
    id: Option<String>,
    alternatives: Vec<AlternativeResult>,
    identity_constraints: Vec<IdentityResult>,
    annotation: Option<Annotation>,
    source: Option<SourceRef>,
    foreign_attributes: Vec<ForeignAttribute>,
}

impl ElementFrame {
    pub fn new(
        attrs: &AttributeMap,
        name_table: &NameTable,
        source: Option<SourceRef>,
    ) -> SchemaResult<Self> {
        let name = attrs
            .get_value_by_name(name_table, "name")
            .and_then(|s| name_table.get(s));

        let ref_name = attrs
            .get_value_by_name(name_table, "ref")
            .map(|s| parse_qname_ref(s, name_table))
            .transpose()?;

        let target_namespace = attrs
            .get_value_by_name(name_table, "targetNamespace")
            .map(|s| name_table.get(s).unwrap_or_else(|| NameId(0)));

        let type_ref = attrs
            .get_value_by_name(name_table, "type")
            .map(|s| parse_qname_ref(s, name_table))
            .transpose()?
            .map(TypeRefResult::QName);

        let substitution_group = attrs
            .get_value_by_name(name_table, "substitutionGroup")
            .map(|s| parse_qname_list(s, name_table))
            .transpose()?
            .unwrap_or_default();

        let default_value = attrs
            .get_value_by_name(name_table, "default")
            .map(String::from);

        let fixed_value = attrs
            .get_value_by_name(name_table, "fixed")
            .map(String::from);

        let nillable = parse_bool_attr_default(attrs, name_table, "nillable", false)?;

        let is_abstract = parse_bool_attr_default(attrs, name_table, "abstract", false)?;

        let min_occurs = parse_min_occurs_attr(attrs, name_table, "minOccurs")?;

        let max_occurs = parse_max_occurs_attr(attrs, name_table, "maxOccurs")?;

        let block = parse_derivation_set(
            attrs.get_value_by_name(name_table, "block"),
        )?;

        let final_derivation = parse_derivation_set(
            attrs.get_value_by_name(name_table, "final"),
        )?;

        let form = attrs
            .get_value_by_name(name_table, "form")
            .map(String::from);

        let id = attrs
            .get_value_by_name(name_table, "id")
            .map(String::from);

        Ok(Self {
            phase: ElementPhase::Annotation,
            name,
            ref_name,
            target_namespace,
            type_ref,
            inline_type: None,
            substitution_group,
            default_value,
            fixed_value,
            nillable,
            is_abstract,
            min_occurs,
            max_occurs,
            block,
            final_derivation,
            form,
            id,
            alternatives: Vec::new(),
            identity_constraints: Vec::new(),
            annotation: None,
            source,
            foreign_attributes: Vec::new(),
        })
    }
}

impl Frame for ElementFrame {
    fn allows(&self, local_name: &str, _name_table: &NameTable) -> bool {
        match self.phase {
            ElementPhase::Annotation => matches!(
                local_name,
                xsd_names::ANNOTATION
                    | xsd_names::SIMPLE_TYPE
                    | xsd_names::COMPLEX_TYPE
                    | xsd_names::ALTERNATIVE
                    | xsd_names::KEY
                    | xsd_names::KEYREF
                    | xsd_names::UNIQUE
            ),
            ElementPhase::Type => matches!(
                local_name,
                xsd_names::SIMPLE_TYPE
                    | xsd_names::COMPLEX_TYPE
                    | xsd_names::ALTERNATIVE
                    | xsd_names::KEY
                    | xsd_names::KEYREF
                    | xsd_names::UNIQUE
            ),
            ElementPhase::Identity => {
                matches!(
                    local_name,
                    xsd_names::ALTERNATIVE | xsd_names::KEY | xsd_names::KEYREF | xsd_names::UNIQUE
                )
            }
            ElementPhase::Done => false,
        }
    }

    fn allows_attribute(&self, local_name: &str, _name_table: &NameTable) -> bool {
        matches!(
            local_name,
            "name"
                | "ref"
                | "targetNamespace"
                | "type"
                | "substitutionGroup"
                | "default"
                | "fixed"
                | "nillable"
                | "abstract"
                | "minOccurs"
                | "maxOccurs"
                | "block"
                | "final"
                | "form"
                | "id"
        )
    }

    fn on_child_start(&mut self, local_name: &str, _name_table: &NameTable) {
        match local_name {
            xsd_names::ANNOTATION => {
                self.phase = ElementPhase::Type;
            }
            xsd_names::SIMPLE_TYPE | xsd_names::COMPLEX_TYPE => {
                self.phase = ElementPhase::Identity;
            }
            xsd_names::ALTERNATIVE => {
                self.phase = ElementPhase::Identity;
            }
            xsd_names::KEY | xsd_names::KEYREF | xsd_names::UNIQUE => {
                self.phase = ElementPhase::Identity;
            }
            _ => {}
        }
    }

    fn attach(&mut self, child: FrameResult) -> SchemaResult<()> {
        match child {
            FrameResult::Annotation(ann) => {
                self.annotation = Some(ann);
            }
            FrameResult::Type(t) => {
                self.inline_type = Some(Box::new(t));
            }
            FrameResult::Alternative(alt) => {
                self.alternatives.push(alt);
            }
            FrameResult::Identity(ic) => {
                self.identity_constraints.push(ic);
            }
            FrameResult::Skip => {}
            _ => {}
        }
        Ok(())
    }

    fn finish(self: Box<Self>) -> SchemaResult<FrameResult> {
        Ok(FrameResult::Element(ElementFrameResult {
            name: self.name,
            ref_name: self.ref_name,
            target_namespace: self.target_namespace,
            type_ref: self.type_ref,
            inline_type: self.inline_type,
            substitution_group: self.substitution_group,
            default_value: self.default_value,
            fixed_value: self.fixed_value,
            nillable: self.nillable,
            is_abstract: self.is_abstract,
            min_occurs: self.min_occurs,
            max_occurs: self.max_occurs,
            block: self.block,
            final_derivation: self.final_derivation,
            form: self.form,
            id: self.id,
            alternatives: self.alternatives,
            identity_constraints: self.identity_constraints,
            annotation: self.annotation,
            source: self.source,
        }))
    }

    fn source(&self) -> Option<&SourceRef> {
        self.source.as_ref()
    }

    fn set_foreign_attributes(&mut self, attrs: Vec<ForeignAttribute>) {
        self.foreign_attributes = attrs;
    }
}

// ============================================================================
// Attribute Frame
// ============================================================================

/// Frame for xs:attribute
pub struct AttributeFrame {
    name: Option<NameId>,
    ref_name: Option<QNameRef>,
    target_namespace: Option<NameId>,
    type_ref: Option<TypeRefResult>,
    inline_type: Option<Box<SimpleTypeResult>>,
    default_value: Option<String>,
    fixed_value: Option<String>,
    use_kind: Option<String>,
    form: Option<String>,
    inheritable: bool,
    id: Option<String>,
    annotation: Option<Annotation>,
    source: Option<SourceRef>,
    foreign_attributes: Vec<ForeignAttribute>,
}

impl AttributeFrame {
    pub fn new(
        attrs: &AttributeMap,
        name_table: &NameTable,
        source: Option<SourceRef>,
    ) -> SchemaResult<Self> {
        let name = attrs
            .get_value_by_name(name_table, "name")
            .and_then(|s| name_table.get(s));

        let ref_name = attrs
            .get_value_by_name(name_table, "ref")
            .map(|s| parse_qname_ref(s, name_table))
            .transpose()?;

        let target_namespace = attrs
            .get_value_by_name(name_table, "targetNamespace")
            .map(|s| name_table.get(s).unwrap_or_else(|| NameId(0)));

        let type_ref = attrs
            .get_value_by_name(name_table, "type")
            .map(|s| parse_qname_ref(s, name_table))
            .transpose()?
            .map(TypeRefResult::QName);

        let default_value = attrs
            .get_value_by_name(name_table, "default")
            .map(String::from);

        let fixed_value = attrs
            .get_value_by_name(name_table, "fixed")
            .map(String::from);

        validate_attr_value(attrs, name_table, "use", parse_use)?;
        let use_kind = attrs
            .get_value_by_name(name_table, "use")
            .map(String::from);

        validate_attr_value(attrs, name_table, "form", parse_form)?;
        let form = attrs
            .get_value_by_name(name_table, "form")
            .map(String::from);

        let inheritable = parse_bool_attr_default(attrs, name_table, "inheritable", false)?;

        let id = attrs
            .get_value_by_name(name_table, "id")
            .map(String::from);

        Ok(Self {
            name,
            ref_name,
            target_namespace,
            type_ref,
            inline_type: None,
            default_value,
            fixed_value,
            use_kind,
            form,
            inheritable,
            id,
            annotation: None,
            source,
            foreign_attributes: Vec::new(),
        })
    }
}

impl Frame for AttributeFrame {
    fn allows(&self, local_name: &str, _name_table: &NameTable) -> bool {
        matches!(local_name, xsd_names::ANNOTATION | xsd_names::SIMPLE_TYPE)
    }

    fn allows_attribute(&self, local_name: &str, _name_table: &NameTable) -> bool {
        matches!(
            local_name,
            "name"
                | "ref"
                | "targetNamespace"
                | "type"
                | "default"
                | "fixed"
                | "use"
                | "form"
                | "inheritable"
                | "id"
        )
    }

    fn on_child_start(&mut self, _local_name: &str, _name_table: &NameTable) {}

    fn attach(&mut self, child: FrameResult) -> SchemaResult<()> {
        match child {
            FrameResult::Annotation(ann) => {
                self.annotation = Some(ann);
            }
            FrameResult::Type(TypeFrameResult::Simple(st)) => {
                self.inline_type = Some(Box::new(st));
            }
            FrameResult::Skip => {}
            _ => {}
        }
        Ok(())
    }

    fn finish(self: Box<Self>) -> SchemaResult<FrameResult> {
        Ok(FrameResult::Attribute(AttributeFrameResult {
            name: self.name,
            ref_name: self.ref_name,
            target_namespace: self.target_namespace,
            type_ref: self.type_ref,
            inline_type: self.inline_type,
            default_value: self.default_value,
            fixed_value: self.fixed_value,
            use_kind: self.use_kind,
            form: self.form,
            inheritable: self.inheritable,
            id: self.id,
            annotation: self.annotation,
            source: self.source,
        }))
    }

    fn source(&self) -> Option<&SourceRef> {
        self.source.as_ref()
    }

    fn set_foreign_attributes(&mut self, attrs: Vec<ForeignAttribute>) {
        self.foreign_attributes = attrs;
    }
}

// ============================================================================
// Alternative Frame (XSD 1.1)
// ============================================================================

/// Frame for xs:alternative
pub struct AlternativeFrame {
    test: Option<String>,
    type_ref: Option<TypeRefResult>,
    inline_type: Option<Box<TypeFrameResult>>,
    xpath_default_namespace: Option<String>,
    id: Option<String>,
    annotation: Option<Annotation>,
    source: Option<SourceRef>,
    foreign_attributes: Vec<ForeignAttribute>,
}

impl AlternativeFrame {
    pub fn new(
        attrs: &AttributeMap,
        name_table: &NameTable,
        source: Option<SourceRef>,
    ) -> SchemaResult<Self> {
        let test = attrs
            .get_value_by_name(name_table, "test")
            .map(String::from);

        let type_ref = attrs
            .get_value_by_name(name_table, "type")
            .map(|s| parse_qname_ref(s, name_table))
            .transpose()?
            .map(TypeRefResult::QName);

        let xpath_default_namespace = attrs
            .get_value_by_name(name_table, "xpathDefaultNamespace")
            .map(String::from);

        let id = attrs
            .get_value_by_name(name_table, "id")
            .map(String::from);

        Ok(Self {
            test,
            type_ref,
            inline_type: None,
            xpath_default_namespace,
            id,
            annotation: None,
            source,
            foreign_attributes: Vec::new(),
        })
    }
}

impl Frame for AlternativeFrame {
    fn allows(&self, local_name: &str, _name_table: &NameTable) -> bool {
        matches!(
            local_name,
            xsd_names::ANNOTATION | xsd_names::SIMPLE_TYPE | xsd_names::COMPLEX_TYPE
        )
    }

    fn allows_attribute(&self, local_name: &str, _name_table: &NameTable) -> bool {
        matches!(
            local_name,
            "test" | "type" | "xpathDefaultNamespace" | "id"
        )
    }

    fn on_child_start(&mut self, _local_name: &str, _name_table: &NameTable) {}

    fn attach(&mut self, child: FrameResult) -> SchemaResult<()> {
        match child {
            FrameResult::Annotation(ann) => {
                self.annotation = Some(ann);
            }
            FrameResult::Type(t) => {
                self.inline_type = Some(Box::new(t));
            }
            FrameResult::Skip => {}
            _ => {}
        }
        Ok(())
    }

    fn finish(self: Box<Self>) -> SchemaResult<FrameResult> {
        Ok(FrameResult::Alternative(AlternativeResult {
            test: self.test,
            type_ref: self.type_ref,
            inline_type: self.inline_type,
            xpath_default_namespace: self.xpath_default_namespace,
            id: self.id,
            annotation: self.annotation,
            source: self.source,
        }))
    }

    fn source(&self) -> Option<&SourceRef> {
        self.source.as_ref()
    }

    fn set_foreign_attributes(&mut self, attrs: Vec<ForeignAttribute>) {
        self.foreign_attributes = attrs;
    }
}

// ============================================================================
// Assert Frame (XSD 1.1)
// ============================================================================

/// Frame for xs:assert
pub struct AssertFrame {
    test: String,
    xpath_default_namespace: Option<String>,
    id: Option<String>,
    annotation: Option<Annotation>,
    source: Option<SourceRef>,
    foreign_attributes: Vec<ForeignAttribute>,
}

impl AssertFrame {
    pub fn new(
        attrs: &AttributeMap,
        name_table: &NameTable,
        source: Option<SourceRef>,
    ) -> SchemaResult<Self> {
        let test = attrs
            .get_value_by_name(name_table, "test")
            .map(String::from)
            .unwrap_or_default();

        let xpath_default_namespace = attrs
            .get_value_by_name(name_table, "xpathDefaultNamespace")
            .map(String::from);

        let id = attrs
            .get_value_by_name(name_table, "id")
            .map(String::from);

        Ok(Self {
            test,
            xpath_default_namespace,
            id,
            annotation: None,
            source,
            foreign_attributes: Vec::new(),
        })
    }
}

impl Frame for AssertFrame {
    fn allows(&self, local_name: &str, _name_table: &NameTable) -> bool {
        matches!(local_name, xsd_names::ANNOTATION)
    }

    fn allows_attribute(&self, local_name: &str, _name_table: &NameTable) -> bool {
        matches!(local_name, "test" | "xpathDefaultNamespace" | "id")
    }

    fn on_child_start(&mut self, _local_name: &str, _name_table: &NameTable) {}

    fn attach(&mut self, child: FrameResult) -> SchemaResult<()> {
        if let FrameResult::Annotation(ann) = child {
            self.annotation = Some(ann);
        }
        Ok(())
    }

    fn finish(self: Box<Self>) -> SchemaResult<FrameResult> {
        Ok(FrameResult::Assert(AssertResult {
            test: self.test,
            xpath_default_namespace: self.xpath_default_namespace,
            id: self.id,
            annotation: self.annotation,
            source: self.source,
        }))
    }

    fn source(&self) -> Option<&SourceRef> {
        self.source.as_ref()
    }

    fn set_foreign_attributes(&mut self, attrs: Vec<ForeignAttribute>) {
        self.foreign_attributes = attrs;
    }
}

// ============================================================================
// Notation Frame
// ============================================================================

/// Frame for xs:notation
pub struct NotationFrame {
    name: Option<NameId>,
    public: Option<String>,
    system: Option<String>,
    id: Option<String>,
    annotation: Option<Annotation>,
    source: Option<SourceRef>,
    foreign_attributes: Vec<ForeignAttribute>,
}

impl NotationFrame {
    pub fn new(
        attrs: &AttributeMap,
        name_table: &NameTable,
        source: Option<SourceRef>,
    ) -> SchemaResult<Self> {
        let name = attrs
            .get_value_by_name(name_table, "name")
            .and_then(|s| name_table.get(s));

        let public = attrs
            .get_value_by_name(name_table, "public")
            .map(String::from);

        let system = attrs
            .get_value_by_name(name_table, "system")
            .map(String::from);

        let id = attrs
            .get_value_by_name(name_table, "id")
            .map(String::from);

        Ok(Self {
            name,
            public,
            system,
            id,
            annotation: None,
            source,
            foreign_attributes: Vec::new(),
        })
    }
}

impl Frame for NotationFrame {
    fn allows(&self, local_name: &str, _name_table: &NameTable) -> bool {
        matches!(local_name, xsd_names::ANNOTATION)
    }

    fn allows_attribute(&self, local_name: &str, _name_table: &NameTable) -> bool {
        matches!(local_name, "name" | "public" | "system" | "id")
    }

    fn on_child_start(&mut self, _local_name: &str, _name_table: &NameTable) {}

    fn attach(&mut self, child: FrameResult) -> SchemaResult<()> {
        if let FrameResult::Annotation(ann) = child {
            self.annotation = Some(ann);
        }
        Ok(())
    }

    fn finish(self: Box<Self>) -> SchemaResult<FrameResult> {
        Ok(FrameResult::Notation(NotationResult {
            name: self.name,
            public: self.public,
            system: self.system,
            id: self.id,
            annotation: self.annotation,
            source: self.source,
        }))
    }

    fn source(&self) -> Option<&SourceRef> {
        self.source.as_ref()
    }

    fn set_foreign_attributes(&mut self, attrs: Vec<ForeignAttribute>) {
        self.foreign_attributes = attrs;
    }
}

// ============================================================================
// Open Content Frames (XSD 1.1)
// ============================================================================

/// Frame for xs:openContent
pub struct OpenContentFrame {
    mode: OpenContentMode,
    wildcard: Option<WildcardResult>,
    id: Option<String>,
    annotation: Option<Annotation>,
    source: Option<SourceRef>,
    foreign_attributes: Vec<ForeignAttribute>,
}

impl OpenContentFrame {
    pub fn new(
        attrs: &AttributeMap,
        name_table: &NameTable,
        source: Option<SourceRef>,
    ) -> SchemaResult<Self> {
        let mode = parse_open_content_mode_attr(attrs, name_table, "mode")?;

        let id = attrs
            .get_value_by_name(name_table, "id")
            .map(String::from);

        Ok(Self {
            mode,
            wildcard: None,
            id,
            annotation: None,
            source,
            foreign_attributes: Vec::new(),
        })
    }
}

impl Frame for OpenContentFrame {
    fn allows(&self, local_name: &str, _name_table: &NameTable) -> bool {
        matches!(local_name, xsd_names::ANNOTATION | xsd_names::ANY)
    }

    fn allows_attribute(&self, local_name: &str, _name_table: &NameTable) -> bool {
        matches!(local_name, "id" | "mode")
    }

    fn on_child_start(&mut self, _local_name: &str, _name_table: &NameTable) {}

    fn attach(&mut self, child: FrameResult) -> SchemaResult<()> {
        match child {
            FrameResult::Annotation(ann) => {
                self.annotation = Some(ann);
            }
            FrameResult::Particle(particle) => {
                if let ParticleTerm::Any(wc) = particle.term {
                    self.wildcard = Some(wc);
                }
            }
            FrameResult::Skip => {}
            _ => {}
        }
        Ok(())
    }

    fn finish(self: Box<Self>) -> SchemaResult<FrameResult> {
        Ok(FrameResult::OpenContent(OpenContentResult {
            mode: self.mode,
            wildcard: self.wildcard,
            id: self.id,
            annotation: self.annotation,
            source: self.source,
        }))
    }

    fn source(&self) -> Option<&SourceRef> {
        self.source.as_ref()
    }

    fn set_foreign_attributes(&mut self, attrs: Vec<ForeignAttribute>) {
        self.foreign_attributes = attrs;
    }
}

/// Frame for xs:defaultOpenContent
pub struct DefaultOpenContentFrame {
    mode: OpenContentMode,
    applies_to_empty: bool,
    wildcard: Option<WildcardResult>,
    id: Option<String>,
    annotation: Option<Annotation>,
    source: Option<SourceRef>,
    foreign_attributes: Vec<ForeignAttribute>,
}

impl DefaultOpenContentFrame {
    pub fn new(
        attrs: &AttributeMap,
        name_table: &NameTable,
        source: Option<SourceRef>,
    ) -> SchemaResult<Self> {
        let mode = parse_open_content_mode_attr(attrs, name_table, "mode")?;

        let applies_to_empty =
            parse_bool_attr_default(attrs, name_table, "appliesToEmpty", false)?;

        let id = attrs
            .get_value_by_name(name_table, "id")
            .map(String::from);

        Ok(Self {
            mode,
            applies_to_empty,
            wildcard: None,
            id,
            annotation: None,
            source,
            foreign_attributes: Vec::new(),
        })
    }
}

impl Frame for DefaultOpenContentFrame {
    fn allows(&self, local_name: &str, _name_table: &NameTable) -> bool {
        matches!(local_name, xsd_names::ANNOTATION | xsd_names::ANY)
    }

    fn allows_attribute(&self, local_name: &str, _name_table: &NameTable) -> bool {
        matches!(local_name, "id" | "mode" | "appliesToEmpty")
    }

    fn on_child_start(&mut self, _local_name: &str, _name_table: &NameTable) {}

    fn attach(&mut self, child: FrameResult) -> SchemaResult<()> {
        match child {
            FrameResult::Annotation(ann) => {
                self.annotation = Some(ann);
            }
            FrameResult::Particle(particle) => {
                if let ParticleTerm::Any(wc) = particle.term {
                    self.wildcard = Some(wc);
                }
            }
            FrameResult::Skip => {}
            _ => {}
        }
        Ok(())
    }

    fn finish(self: Box<Self>) -> SchemaResult<FrameResult> {
        Ok(FrameResult::DefaultOpenContent(DefaultOpenContentResult {
            mode: self.mode,
            applies_to_empty: self.applies_to_empty,
            wildcard: self.wildcard,
            id: self.id,
            annotation: self.annotation,
            source: self.source,
        }))
    }

    fn source(&self) -> Option<&SourceRef> {
        self.source.as_ref()
    }

    fn set_foreign_attributes(&mut self, attrs: Vec<ForeignAttribute>) {
        self.foreign_attributes = attrs;
    }
}

// ============================================================================
// Model Group Frame (sequence, choice, all)
// ============================================================================

/// Frame for xs:sequence, xs:choice, xs:all
pub struct ModelGroupFrame {
    compositor: Compositor,
    min_occurs: u32,
    max_occurs: Option<u32>,
    id: Option<String>,
    particles: Vec<ParticleResult>,
    annotation: Option<Annotation>,
    source: Option<SourceRef>,
    foreign_attributes: Vec<ForeignAttribute>,
}

impl ModelGroupFrame {
    pub fn new(
        compositor: Compositor,
        attrs: &AttributeMap,
        name_table: &NameTable,
        source: Option<SourceRef>,
    ) -> SchemaResult<Self> {
        let min_occurs = parse_min_occurs_attr(attrs, name_table, "minOccurs")?;

        let max_occurs = parse_max_occurs_attr(attrs, name_table, "maxOccurs")?;

        let id = attrs
            .get_value_by_name(name_table, "id")
            .map(String::from);

        Ok(Self {
            compositor,
            min_occurs,
            max_occurs,
            id,
            particles: Vec::new(),
            annotation: None,
            source,
            foreign_attributes: Vec::new(),
        })
    }
}

impl Frame for ModelGroupFrame {
    fn allows(&self, local_name: &str, _name_table: &NameTable) -> bool {
        match self.compositor {
            Compositor::All => matches!(
                local_name,
                xsd_names::ANNOTATION | xsd_names::ELEMENT | xsd_names::ANY | xsd_names::GROUP
            ),
            Compositor::Sequence | Compositor::Choice => matches!(
                local_name,
                xsd_names::ANNOTATION
                    | xsd_names::ELEMENT
                    | xsd_names::GROUP
                    | xsd_names::SEQUENCE
                    | xsd_names::CHOICE
                    | xsd_names::ANY
            ),
        }
    }

    fn allows_attribute(&self, local_name: &str, _name_table: &NameTable) -> bool {
        matches!(local_name, "minOccurs" | "maxOccurs" | "id")
    }

    fn on_child_start(&mut self, _local_name: &str, _name_table: &NameTable) {}

    fn attach(&mut self, child: FrameResult) -> SchemaResult<()> {
        match child {
            FrameResult::Annotation(ann) => {
                self.annotation = Some(ann);
            }
            FrameResult::Element(elem) => {
                self.particles.push(ParticleResult {
                    term: ParticleTerm::Element(elem),
                    min_occurs: 1,
                    max_occurs: Some(1),
                    source: None,
                });
            }
            FrameResult::Particle(particle) => {
                self.particles.push(particle);
            }
            FrameResult::Wildcard(wc) => {
                self.particles.push(ParticleResult {
                    term: ParticleTerm::Any(wc),
                    min_occurs: 1,
                    max_occurs: Some(1),
                    source: None,
                });
            }
            FrameResult::Group(GroupFrameResult::Model(mg)) => {
                self.particles.push(ParticleResult {
                    term: ParticleTerm::Group(mg),
                    min_occurs: 1,
                    max_occurs: Some(1),
                    source: None,
                });
            }
            FrameResult::Skip => {}
            _ => {}
        }
        Ok(())
    }

    fn finish(self: Box<Self>) -> SchemaResult<FrameResult> {
        Ok(FrameResult::Particle(ParticleResult {
            term: ParticleTerm::Group(ModelGroupDefResult {
                name: None,
                ref_name: None,
                compositor: Some(self.compositor),
                particles: self.particles,
                min_occurs: self.min_occurs,
                max_occurs: self.max_occurs,
                id: self.id,
                annotation: self.annotation,
                source: self.source.clone(),
            }),
            min_occurs: self.min_occurs,
            max_occurs: self.max_occurs,
            source: self.source,
        }))
    }

    fn source(&self) -> Option<&SourceRef> {
        self.source.as_ref()
    }

    fn set_foreign_attributes(&mut self, attrs: Vec<ForeignAttribute>) {
        self.foreign_attributes = attrs;
    }
}

// ============================================================================
// Group Frame (named model group)
// ============================================================================

/// Frame for xs:group (named or reference)
pub struct GroupFrame {
    name: Option<NameId>,
    ref_name: Option<QNameRef>,
    min_occurs: u32,
    max_occurs: Option<u32>,
    id: Option<String>,
    compositor: Option<Compositor>,
    particles: Vec<ParticleResult>,
    annotation: Option<Annotation>,
    source: Option<SourceRef>,
    foreign_attributes: Vec<ForeignAttribute>,
}

impl GroupFrame {
    pub fn new(
        attrs: &AttributeMap,
        name_table: &NameTable,
        source: Option<SourceRef>,
    ) -> SchemaResult<Self> {
        let name = attrs
            .get_value_by_name(name_table, "name")
            .and_then(|s| name_table.get(s));

        let ref_name = attrs
            .get_value_by_name(name_table, "ref")
            .map(|s| parse_qname_ref(s, name_table))
            .transpose()?;

        let min_occurs = parse_min_occurs_attr(attrs, name_table, "minOccurs")?;

        let max_occurs = parse_max_occurs_attr(attrs, name_table, "maxOccurs")?;

        let id = attrs
            .get_value_by_name(name_table, "id")
            .map(String::from);

        Ok(Self {
            name,
            ref_name,
            min_occurs,
            max_occurs,
            id,
            compositor: None,
            particles: Vec::new(),
            annotation: None,
            source,
            foreign_attributes: Vec::new(),
        })
    }
}

impl Frame for GroupFrame {
    fn allows(&self, local_name: &str, _name_table: &NameTable) -> bool {
        matches!(
            local_name,
            xsd_names::ANNOTATION | xsd_names::SEQUENCE | xsd_names::CHOICE | xsd_names::ALL
        )
    }

    fn allows_attribute(&self, local_name: &str, _name_table: &NameTable) -> bool {
        matches!(
            local_name,
            "name" | "ref" | "minOccurs" | "maxOccurs" | "id"
        )
    }

    fn on_child_start(&mut self, _local_name: &str, _name_table: &NameTable) {}

    fn attach(&mut self, child: FrameResult) -> SchemaResult<()> {
        match child {
            FrameResult::Annotation(ann) => {
                self.annotation = Some(ann);
            }
            FrameResult::Particle(ParticleResult {
                term: ParticleTerm::Group(mg),
                ..
            }) => {
                self.compositor = mg.compositor;
                self.particles = mg.particles;
            }
            FrameResult::Skip => {}
            _ => {}
        }
        Ok(())
    }

    fn finish(self: Box<Self>) -> SchemaResult<FrameResult> {
        Ok(FrameResult::Group(GroupFrameResult::Model(ModelGroupDefResult {
            name: self.name,
            ref_name: self.ref_name,
            compositor: self.compositor,
            particles: self.particles,
            min_occurs: self.min_occurs,
            max_occurs: self.max_occurs,
            id: self.id,
            annotation: self.annotation,
            source: self.source,
        })))
    }

    fn source(&self) -> Option<&SourceRef> {
        self.source.as_ref()
    }

    fn set_foreign_attributes(&mut self, attrs: Vec<ForeignAttribute>) {
        self.foreign_attributes = attrs;
    }
}

// ============================================================================
// Attribute Group Frame
// ============================================================================

/// Frame for xs:attributeGroup
pub struct AttributeGroupFrame {
    name: Option<NameId>,
    ref_name: Option<QNameRef>,
    id: Option<String>,
    attributes: Vec<AttributeUseResult>,
    attribute_groups: Vec<QNameRef>,
    attribute_wildcard: Option<WildcardResult>,
    annotation: Option<Annotation>,
    source: Option<SourceRef>,
    foreign_attributes: Vec<ForeignAttribute>,
}

impl AttributeGroupFrame {
    pub fn new(
        attrs: &AttributeMap,
        name_table: &NameTable,
        source: Option<SourceRef>,
    ) -> SchemaResult<Self> {
        let name = attrs
            .get_value_by_name(name_table, "name")
            .and_then(|s| name_table.get(s));

        let ref_name = attrs
            .get_value_by_name(name_table, "ref")
            .map(|s| parse_qname_ref(s, name_table))
            .transpose()?;

        let id = attrs
            .get_value_by_name(name_table, "id")
            .map(String::from);

        Ok(Self {
            name,
            ref_name,
            id,
            attributes: Vec::new(),
            attribute_groups: Vec::new(),
            attribute_wildcard: None,
            annotation: None,
            source,
            foreign_attributes: Vec::new(),
        })
    }
}

impl Frame for AttributeGroupFrame {
    fn allows(&self, local_name: &str, _name_table: &NameTable) -> bool {
        matches!(
            local_name,
            xsd_names::ANNOTATION
                | xsd_names::ATTRIBUTE
                | xsd_names::ATTRIBUTE_GROUP
                | xsd_names::ANY_ATTRIBUTE
        )
    }

    fn allows_attribute(&self, local_name: &str, _name_table: &NameTable) -> bool {
        matches!(local_name, "name" | "ref" | "id")
    }

    fn on_child_start(&mut self, _local_name: &str, _name_table: &NameTable) {}

    fn attach(&mut self, child: FrameResult) -> SchemaResult<()> {
        match child {
            FrameResult::Annotation(ann) => {
                self.annotation = Some(ann);
            }
            FrameResult::Attribute(attr) => {
                self.attributes.push(AttributeUseResult {
                    attribute: attr,
                    use_kind: AttributeUseKind::Optional,
                });
            }
            FrameResult::Group(GroupFrameResult::Attribute(ag)) => {
                if let Some(ref_name) = ag.ref_name {
                    self.attribute_groups.push(ref_name);
                }
            }
            FrameResult::Wildcard(wc) => {
                self.attribute_wildcard = Some(wc);
            }
            FrameResult::Skip => {}
            _ => {}
        }
        Ok(())
    }

    fn finish(self: Box<Self>) -> SchemaResult<FrameResult> {
        Ok(FrameResult::Group(GroupFrameResult::Attribute(AttributeGroupDefResult {
            name: self.name,
            ref_name: self.ref_name,
            attributes: self.attributes,
            attribute_groups: self.attribute_groups,
            attribute_wildcard: self.attribute_wildcard,
            id: self.id,
            annotation: self.annotation,
            source: self.source,
        })))
    }

    fn source(&self) -> Option<&SourceRef> {
        self.source.as_ref()
    }

    fn set_foreign_attributes(&mut self, attrs: Vec<ForeignAttribute>) {
        self.foreign_attributes = attrs;
    }
}

// ============================================================================
// Wildcard Frames (any, anyAttribute)
// ============================================================================

/// Frame for xs:any
pub struct AnyFrame {
    namespace: WildcardNamespace,
    process_contents: ProcessContents,
    not_namespace: Option<String>,
    not_qname: Option<String>,
    min_occurs: u32,
    max_occurs: Option<u32>,
    id: Option<String>,
    annotation: Option<Annotation>,
    source: Option<SourceRef>,
    foreign_attributes: Vec<ForeignAttribute>,
}

impl AnyFrame {
    pub fn new(
        attrs: &AttributeMap,
        name_table: &NameTable,
        source: Option<SourceRef>,
    ) -> SchemaResult<Self> {
        let namespace = parse_namespace_constraint(
            attrs.get_value_by_name(name_table, "namespace"),
            name_table,
        )?;

        let process_contents =
            parse_process_contents_attr(attrs, name_table, "processContents")?;

        let not_namespace = attrs
            .get_value_by_name(name_table, "notNamespace")
            .map(String::from);

        let not_qname = attrs
            .get_value_by_name(name_table, "notQName")
            .map(String::from);

        let min_occurs = parse_min_occurs_attr(attrs, name_table, "minOccurs")?;

        let max_occurs = parse_max_occurs_attr(attrs, name_table, "maxOccurs")?;

        let id = attrs
            .get_value_by_name(name_table, "id")
            .map(String::from);

        Ok(Self {
            namespace,
            process_contents,
            not_namespace,
            not_qname,
            min_occurs,
            max_occurs,
            id,
            annotation: None,
            source,
            foreign_attributes: Vec::new(),
        })
    }
}

impl Frame for AnyFrame {
    fn allows(&self, local_name: &str, _name_table: &NameTable) -> bool {
        matches!(local_name, xsd_names::ANNOTATION)
    }

    fn allows_attribute(&self, local_name: &str, _name_table: &NameTable) -> bool {
        matches!(
            local_name,
            "namespace"
                | "processContents"
                | "notNamespace"
                | "notQName"
                | "minOccurs"
                | "maxOccurs"
                | "id"
        )
    }

    fn on_child_start(&mut self, _local_name: &str, _name_table: &NameTable) {}

    fn attach(&mut self, child: FrameResult) -> SchemaResult<()> {
        if let FrameResult::Annotation(ann) = child {
            self.annotation = Some(ann);
        }
        Ok(())
    }

    fn finish(self: Box<Self>) -> SchemaResult<FrameResult> {
        Ok(FrameResult::Particle(ParticleResult {
            term: ParticleTerm::Any(WildcardResult {
                namespace: self.namespace,
                process_contents: self.process_contents,
                not_namespace: self.not_namespace,
                not_qname: self.not_qname,
                id: self.id,
                annotation: self.annotation,
                source: self.source.clone(),
            }),
            min_occurs: self.min_occurs,
            max_occurs: self.max_occurs,
            source: self.source,
        }))
    }

    fn source(&self) -> Option<&SourceRef> {
        self.source.as_ref()
    }

    fn set_foreign_attributes(&mut self, attrs: Vec<ForeignAttribute>) {
        self.foreign_attributes = attrs;
    }
}

/// Frame for xs:anyAttribute
pub struct AnyAttributeFrame {
    namespace: WildcardNamespace,
    process_contents: ProcessContents,
    not_namespace: Option<String>,
    not_qname: Option<String>,
    id: Option<String>,
    annotation: Option<Annotation>,
    source: Option<SourceRef>,
    foreign_attributes: Vec<ForeignAttribute>,
}

impl AnyAttributeFrame {
    pub fn new(
        attrs: &AttributeMap,
        name_table: &NameTable,
        source: Option<SourceRef>,
    ) -> SchemaResult<Self> {
        let namespace = parse_namespace_constraint(
            attrs.get_value_by_name(name_table, "namespace"),
            name_table,
        )?;

        let process_contents =
            parse_process_contents_attr(attrs, name_table, "processContents")?;

        let not_namespace = attrs
            .get_value_by_name(name_table, "notNamespace")
            .map(String::from);

        let not_qname = attrs
            .get_value_by_name(name_table, "notQName")
            .map(String::from);

        let id = attrs
            .get_value_by_name(name_table, "id")
            .map(String::from);

        Ok(Self {
            namespace,
            process_contents,
            not_namespace,
            not_qname,
            id,
            annotation: None,
            source,
            foreign_attributes: Vec::new(),
        })
    }
}

impl Frame for AnyAttributeFrame {
    fn allows(&self, local_name: &str, _name_table: &NameTable) -> bool {
        matches!(local_name, xsd_names::ANNOTATION)
    }

    fn allows_attribute(&self, local_name: &str, _name_table: &NameTable) -> bool {
        matches!(
            local_name,
            "namespace" | "processContents" | "notNamespace" | "notQName" | "id"
        )
    }

    fn on_child_start(&mut self, _local_name: &str, _name_table: &NameTable) {}

    fn attach(&mut self, child: FrameResult) -> SchemaResult<()> {
        if let FrameResult::Annotation(ann) = child {
            self.annotation = Some(ann);
        }
        Ok(())
    }

    fn finish(self: Box<Self>) -> SchemaResult<FrameResult> {
        Ok(FrameResult::Wildcard(WildcardResult {
            namespace: self.namespace,
            process_contents: self.process_contents,
            not_namespace: self.not_namespace,
            not_qname: self.not_qname,
            id: self.id,
            annotation: self.annotation,
            source: self.source,
        }))
    }

    fn source(&self) -> Option<&SourceRef> {
        self.source.as_ref()
    }

    fn set_foreign_attributes(&mut self, attrs: Vec<ForeignAttribute>) {
        self.foreign_attributes = attrs;
    }
}

// ============================================================================
// Annotation Frame
// ============================================================================

/// Frame for xs:annotation
pub struct AnnotationFrame {
    id: Option<String>,
    items: Vec<AnnotationItem>,
    source: Option<SourceRef>,
    foreign_attributes: Vec<ForeignAttribute>,
}

impl AnnotationFrame {
    pub fn new(
        attrs: &AttributeMap,
        name_table: &NameTable,
        source: Option<SourceRef>,
    ) -> SchemaResult<Self> {
        let id = attrs
            .get_value_by_name(name_table, "id")
            .map(String::from);

        Ok(Self {
            id,
            items: Vec::new(),
            source,
            foreign_attributes: Vec::new(),
        })
    }
}

impl Frame for AnnotationFrame {
    fn allows(&self, local_name: &str, _name_table: &NameTable) -> bool {
        matches!(local_name, xsd_names::APPINFO | xsd_names::DOCUMENTATION)
    }

    fn allows_attribute(&self, local_name: &str, _name_table: &NameTable) -> bool {
        matches!(local_name, "id")
    }

    fn on_child_start(&mut self, _local_name: &str, _name_table: &NameTable) {}

    fn attach(&mut self, child: FrameResult) -> SchemaResult<()> {
        match child {
            FrameResult::AppInfo(appinfo) => {
                self.items.push(AnnotationItem::AppInfo(appinfo));
            }
            FrameResult::Documentation(doc) => {
                self.items.push(AnnotationItem::Documentation(doc));
            }
            _ => {
                // Ignore other content in annotations
            }
        }
        Ok(())
    }

    fn finish(self: Box<Self>) -> SchemaResult<FrameResult> {
        Ok(FrameResult::Annotation(Annotation {
            id: self.id,
            items: self.items,
            source: self.source,
            attributes: self.foreign_attributes,
        }))
    }

    fn source(&self) -> Option<&SourceRef> {
        self.source.as_ref()
    }

    fn set_foreign_attributes(&mut self, attrs: Vec<ForeignAttribute>) {
        self.foreign_attributes = attrs;
    }
}

// ============================================================================
// AppInfo Frame
// ============================================================================

/// Frame for xs:appinfo element
pub struct AppinfoFrame {
    source_attr: Option<String>,
    content: String,
    start_span: Option<SourceRef>,
    namespaces: NamespaceContextSnapshot,
    foreign_attributes: Vec<ForeignAttribute>,
}

impl AppinfoFrame {
    pub fn new(
        attrs: &AttributeMap,
        name_table: &NameTable,
        source: Option<SourceRef>,
    ) -> SchemaResult<Self> {
        let source_attr = attrs
            .get_value_by_name(name_table, "source")
            .map(String::from);

        Ok(Self {
            source_attr,
            content: String::new(),
            start_span: source,
            namespaces: NamespaceContextSnapshot::default(),
            foreign_attributes: Vec::new(),
        })
    }
}

impl Frame for AppinfoFrame {
    fn allows(&self, _local_name: &str, _name_table: &NameTable) -> bool {
        // Appinfo allows any content (mixed content)
        true
    }

    fn allows_attribute(&self, local_name: &str, _name_table: &NameTable) -> bool {
        // Only 'source' attribute is standard, but allow foreign attrs
        local_name == "source"
    }

    fn on_child_start(&mut self, _local_name: &str, _name_table: &NameTable) {
        // Content is captured as raw XML, not parsed
    }

    fn attach(&mut self, _child: FrameResult) -> SchemaResult<()> {
        // Any children are captured as part of mixed content
        Ok(())
    }

    fn finish(self: Box<Self>) -> SchemaResult<FrameResult> {
        // Create XmlFragment from span
        let fragment = if let Some(ref source) = self.start_span {
            XmlFragment::new(source.doc_id, source.span)
        } else {
            XmlFragment::new(0, crate::parser::location::SourceSpan::new(0, 0))
        };

        let mut appinfo = AppInfoElement::new(fragment, self.namespaces);
        appinfo.source = self.source_attr;
        appinfo.attributes = self.foreign_attributes;
        appinfo.source_ref = self.start_span;

        Ok(FrameResult::AppInfo(appinfo))
    }

    fn source(&self) -> Option<&SourceRef> {
        self.start_span.as_ref()
    }

    fn set_foreign_attributes(&mut self, attrs: Vec<ForeignAttribute>) {
        self.foreign_attributes = attrs;
    }

    fn accepts_text(&self) -> bool {
        true
    }

    fn on_text(&mut self, text: &str) {
        self.content.push_str(text);
    }

    fn on_cdata(&mut self, cdata: &str) {
        self.content.push_str(cdata);
    }

    fn set_namespaces(&mut self, namespaces: NamespaceContextSnapshot) {
        self.namespaces = namespaces;
    }
}

// ============================================================================
// Documentation Frame
// ============================================================================

/// Frame for xs:documentation element
pub struct DocumentationFrame {
    source_attr: Option<String>,
    lang: Option<String>,
    content: String,
    start_span: Option<SourceRef>,
    namespaces: NamespaceContextSnapshot,
    foreign_attributes: Vec<ForeignAttribute>,
}

impl DocumentationFrame {
    pub fn new(
        attrs: &AttributeMap,
        name_table: &NameTable,
        source: Option<SourceRef>,
    ) -> SchemaResult<Self> {
        let source_attr = attrs
            .get_value_by_name(name_table, "source")
            .map(String::from);
        let lang = attrs
            .get_value_by_name(name_table, "lang")
            .map(String::from);

        Ok(Self {
            source_attr,
            lang,
            content: String::new(),
            start_span: source,
            namespaces: NamespaceContextSnapshot::default(),
            foreign_attributes: Vec::new(),
        })
    }
}

impl Frame for DocumentationFrame {
    fn allows(&self, _local_name: &str, _name_table: &NameTable) -> bool {
        // Documentation allows any content (mixed content)
        true
    }

    fn allows_attribute(&self, local_name: &str, _name_table: &NameTable) -> bool {
        // 'source' and xml:lang are standard attributes
        matches!(local_name, "source" | "lang")
    }

    fn on_child_start(&mut self, _local_name: &str, _name_table: &NameTable) {
        // Content is captured as raw XML, not parsed
    }

    fn attach(&mut self, _child: FrameResult) -> SchemaResult<()> {
        // Any children are captured as part of mixed content
        Ok(())
    }

    fn finish(self: Box<Self>) -> SchemaResult<FrameResult> {
        // Create XmlFragment from span
        let fragment = if let Some(ref source) = self.start_span {
            XmlFragment::new(source.doc_id, source.span)
        } else {
            XmlFragment::new(0, crate::parser::location::SourceSpan::new(0, 0))
        };

        let mut doc = DocumentationElement::new(fragment, self.namespaces);
        doc.source = self.source_attr;
        doc.lang = self.lang;
        doc.attributes = self.foreign_attributes;
        doc.source_ref = self.start_span;

        Ok(FrameResult::Documentation(doc))
    }

    fn source(&self) -> Option<&SourceRef> {
        self.start_span.as_ref()
    }

    fn set_foreign_attributes(&mut self, attrs: Vec<ForeignAttribute>) {
        self.foreign_attributes = attrs;
    }

    fn accepts_text(&self) -> bool {
        true
    }

    fn on_text(&mut self, text: &str) {
        self.content.push_str(text);
    }

    fn on_cdata(&mut self, cdata: &str) {
        self.content.push_str(cdata);
    }

    fn set_namespaces(&mut self, namespaces: NamespaceContextSnapshot) {
        self.namespaces = namespaces;
    }
}

// ============================================================================
// Facet Frame
// ============================================================================

/// Frame for facet elements (enumeration, pattern, etc.)
pub struct FacetFrame {
    kind: FacetKind,
    value: String,
    fixed: bool,
    #[allow(dead_code)]
    id: Option<String>,
    annotation: Option<Annotation>,
    source: Option<SourceRef>,
    foreign_attributes: Vec<ForeignAttribute>,
}

impl FacetFrame {
    pub fn new(
        kind: FacetKind,
        attrs: &AttributeMap,
        name_table: &NameTable,
        source: Option<SourceRef>,
    ) -> SchemaResult<Self> {
        let value = attrs
            .get_value_by_name(name_table, "value")
            .map(String::from)
            .unwrap_or_default();

        let fixed = parse_bool_attr_default(attrs, name_table, "fixed", false)?;

        let id = attrs
            .get_value_by_name(name_table, "id")
            .map(String::from);

        Ok(Self {
            kind,
            value,
            fixed,
            id,
            annotation: None,
            source,
            foreign_attributes: Vec::new(),
        })
    }
}

impl Frame for FacetFrame {
    fn allows(&self, local_name: &str, _name_table: &NameTable) -> bool {
        matches!(local_name, xsd_names::ANNOTATION)
    }

    fn allows_attribute(&self, local_name: &str, _name_table: &NameTable) -> bool {
        matches!(local_name, "value" | "fixed" | "id")
    }

    fn on_child_start(&mut self, _local_name: &str, _name_table: &NameTable) {}

    fn attach(&mut self, child: FrameResult) -> SchemaResult<()> {
        if let FrameResult::Annotation(ann) = child {
            self.annotation = Some(ann);
        }
        Ok(())
    }

    fn finish(self: Box<Self>) -> SchemaResult<FrameResult> {
        Ok(FrameResult::Facet(FacetResult {
            kind: self.kind,
            value: self.value,
            fixed: self.fixed,
            annotation: self.annotation,
            source: self.source,
        }))
    }

    fn source(&self) -> Option<&SourceRef> {
        self.source.as_ref()
    }

    fn set_foreign_attributes(&mut self, attrs: Vec<ForeignAttribute>) {
        self.foreign_attributes = attrs;
    }
}

// ============================================================================
// Identity Constraint Frames
// ============================================================================

/// Frame for xs:selector
pub struct SelectorFrame {
    xpath: String,
    xpath_default_namespace: Option<String>,
    id: Option<String>,
    annotation: Option<Annotation>,
    source: Option<SourceRef>,
    foreign_attributes: Vec<ForeignAttribute>,
}

impl SelectorFrame {
    pub fn new(
        attrs: &AttributeMap,
        name_table: &NameTable,
        source: Option<SourceRef>,
    ) -> SchemaResult<Self> {
        let xpath = attrs
            .get_value_by_name(name_table, "xpath")
            .map(String::from)
            .unwrap_or_default();

        let xpath_default_namespace = attrs
            .get_value_by_name(name_table, "xpathDefaultNamespace")
            .map(String::from);

        let id = attrs
            .get_value_by_name(name_table, "id")
            .map(String::from);

        Ok(Self {
            xpath,
            xpath_default_namespace,
            id,
            annotation: None,
            source,
            foreign_attributes: Vec::new(),
        })
    }
}

impl Frame for SelectorFrame {
    fn allows(&self, local_name: &str, _name_table: &NameTable) -> bool {
        matches!(local_name, xsd_names::ANNOTATION)
    }

    fn allows_attribute(&self, local_name: &str, _name_table: &NameTable) -> bool {
        matches!(local_name, "xpath" | "xpathDefaultNamespace" | "id")
    }

    fn on_child_start(&mut self, _local_name: &str, _name_table: &NameTable) {}

    fn attach(&mut self, child: FrameResult) -> SchemaResult<()> {
        if let FrameResult::Annotation(ann) = child {
            self.annotation = Some(ann);
        }
        Ok(())
    }

    fn finish(self: Box<Self>) -> SchemaResult<FrameResult> {
        Ok(FrameResult::Selector(SelectorResult {
            xpath: self.xpath,
            xpath_default_namespace: self.xpath_default_namespace,
            id: self.id,
            annotation: self.annotation,
            source: self.source,
        }))
    }

    fn source(&self) -> Option<&SourceRef> {
        self.source.as_ref()
    }

    fn set_foreign_attributes(&mut self, attrs: Vec<ForeignAttribute>) {
        self.foreign_attributes = attrs;
    }
}

/// Frame for xs:field
pub struct FieldFrame {
    xpath: String,
    xpath_default_namespace: Option<String>,
    id: Option<String>,
    annotation: Option<Annotation>,
    source: Option<SourceRef>,
    foreign_attributes: Vec<ForeignAttribute>,
}

impl FieldFrame {
    pub fn new(
        attrs: &AttributeMap,
        name_table: &NameTable,
        source: Option<SourceRef>,
    ) -> SchemaResult<Self> {
        let xpath = attrs
            .get_value_by_name(name_table, "xpath")
            .map(String::from)
            .unwrap_or_default();

        let xpath_default_namespace = attrs
            .get_value_by_name(name_table, "xpathDefaultNamespace")
            .map(String::from);

        let id = attrs
            .get_value_by_name(name_table, "id")
            .map(String::from);

        Ok(Self {
            xpath,
            xpath_default_namespace,
            id,
            annotation: None,
            source,
            foreign_attributes: Vec::new(),
        })
    }
}

impl Frame for FieldFrame {
    fn allows(&self, local_name: &str, _name_table: &NameTable) -> bool {
        matches!(local_name, xsd_names::ANNOTATION)
    }

    fn allows_attribute(&self, local_name: &str, _name_table: &NameTable) -> bool {
        matches!(local_name, "xpath" | "xpathDefaultNamespace" | "id")
    }

    fn on_child_start(&mut self, _local_name: &str, _name_table: &NameTable) {}

    fn attach(&mut self, child: FrameResult) -> SchemaResult<()> {
        if let FrameResult::Annotation(ann) = child {
            self.annotation = Some(ann);
        }
        Ok(())
    }

    fn finish(self: Box<Self>) -> SchemaResult<FrameResult> {
        Ok(FrameResult::Field(FieldResult {
            xpath: self.xpath,
            xpath_default_namespace: self.xpath_default_namespace,
            id: self.id,
            annotation: self.annotation,
            source: self.source,
        }))
    }

    fn source(&self) -> Option<&SourceRef> {
        self.source.as_ref()
    }

    fn set_foreign_attributes(&mut self, attrs: Vec<ForeignAttribute>) {
        self.foreign_attributes = attrs;
    }
}

/// Frame for xs:key, xs:keyref, xs:unique
pub struct IdentityFrame {
    kind: IdentityKind,
    name: Option<NameId>,
    ref_name: Option<QNameRef>,
    refer: Option<QNameRef>,
    id: Option<String>,
    selector: Option<SelectorResult>,
    fields: Vec<FieldResult>,
    annotation: Option<Annotation>,
    source: Option<SourceRef>,
    foreign_attributes: Vec<ForeignAttribute>,
}

impl IdentityFrame {
    pub fn new(
        kind: IdentityKind,
        attrs: &AttributeMap,
        name_table: &NameTable,
        source: Option<SourceRef>,
    ) -> SchemaResult<Self> {
        let name = attrs
            .get_value_by_name(name_table, "name")
            .and_then(|s| name_table.get(s));

        let ref_name = attrs
            .get_value_by_name(name_table, "ref")
            .map(|s| parse_qname_ref(s, name_table))
            .transpose()?;

        let refer = if kind == IdentityKind::Keyref {
            attrs
                .get_value_by_name(name_table, "refer")
                .map(|s| parse_qname_ref(s, name_table))
                .transpose()?
        } else {
            None
        };

        let id = attrs
            .get_value_by_name(name_table, "id")
            .map(String::from);

        Ok(Self {
            kind,
            name,
            ref_name,
            refer,
            id,
            selector: None,
            fields: Vec::new(),
            annotation: None,
            source,
            foreign_attributes: Vec::new(),
        })
    }
}

impl Frame for IdentityFrame {
    fn allows(&self, local_name: &str, _name_table: &NameTable) -> bool {
        matches!(
            local_name,
            xsd_names::ANNOTATION | xsd_names::SELECTOR | xsd_names::FIELD
        )
    }

    fn allows_attribute(&self, local_name: &str, _name_table: &NameTable) -> bool {
        matches!(local_name, "name" | "ref" | "refer" | "id")
    }

    fn on_child_start(&mut self, _local_name: &str, _name_table: &NameTable) {}

    fn attach(&mut self, child: FrameResult) -> SchemaResult<()> {
        match child {
            FrameResult::Annotation(ann) => {
                self.annotation = Some(ann);
            }
            FrameResult::Selector(selector) => {
                self.selector = Some(selector);
            }
            FrameResult::Field(field) => {
                self.fields.push(field);
            }
            FrameResult::Skip => {}
            _ => {}
        }
        Ok(())
    }

    fn finish(self: Box<Self>) -> SchemaResult<FrameResult> {
        let name = self.name.ok_or_else(|| {
            SchemaError::structural(
                "sch-identity-constraint",
                "Identity constraint requires 'name' attribute",
                None,
            )
        })?;

        let selector = self.selector.ok_or_else(|| {
            SchemaError::structural(
                "sch-identity-selector",
                "Identity constraint requires a selector",
                None,
            )
        })?;

        if self.fields.is_empty() {
            return Err(SchemaError::structural(
                "sch-identity-field",
                "Identity constraint requires at least one field",
                None,
            ));
        }

        Ok(FrameResult::Identity(IdentityResult {
            kind: self.kind,
            name,
            ref_name: self.ref_name,
            refer: self.refer,
            selector,
            fields: self.fields,
            id: self.id,
            annotation: self.annotation,
            source: self.source,
        }))
    }

    fn source(&self) -> Option<&SourceRef> {
        self.source.as_ref()
    }

    fn set_foreign_attributes(&mut self, attrs: Vec<ForeignAttribute>) {
        self.foreign_attributes = attrs;
    }
}

// ============================================================================
// Include/Import/Redefine Frames
// ============================================================================

/// Frame for xs:include
pub struct IncludeFrame {
    schema_location: Option<String>,
    id: Option<String>,
    annotation: Option<Annotation>,
    source: Option<SourceRef>,
    foreign_attributes: Vec<ForeignAttribute>,
}

impl IncludeFrame {
    pub fn new(
        attrs: &AttributeMap,
        name_table: &NameTable,
        source: Option<SourceRef>,
    ) -> SchemaResult<Self> {
        let schema_location = attrs
            .get_value_by_name(name_table, "schemaLocation")
            .map(String::from);

        let id = attrs
            .get_value_by_name(name_table, "id")
            .map(String::from);

        Ok(Self {
            schema_location,
            id,
            annotation: None,
            source,
            foreign_attributes: Vec::new(),
        })
    }
}

impl Frame for IncludeFrame {
    fn allows(&self, local_name: &str, _name_table: &NameTable) -> bool {
        matches!(local_name, xsd_names::ANNOTATION)
    }

    fn allows_attribute(&self, local_name: &str, _name_table: &NameTable) -> bool {
        matches!(local_name, "schemaLocation" | "id")
    }

    fn on_child_start(&mut self, _local_name: &str, _name_table: &NameTable) {}

    fn attach(&mut self, child: FrameResult) -> SchemaResult<()> {
        if let FrameResult::Annotation(ann) = child {
            self.annotation = Some(ann);
        }
        Ok(())
    }

    fn finish(self: Box<Self>) -> SchemaResult<FrameResult> {
        let schema_location = self.schema_location.ok_or_else(|| SchemaError::structural(
            "sch-include",
            "xs:include requires 'schemaLocation' attribute",
            None,
        ))?;

        Ok(FrameResult::Directive(DirectiveResult::Include(IncludeResult {
            schema_location,
            id: self.id,
            annotation: self.annotation,
            source: self.source,
        })))
    }

    fn source(&self) -> Option<&SourceRef> {
        self.source.as_ref()
    }

    fn set_foreign_attributes(&mut self, attrs: Vec<ForeignAttribute>) {
        self.foreign_attributes = attrs;
    }
}

/// Frame for xs:import
pub struct ImportFrame {
    namespace: Option<String>,
    schema_location: Option<String>,
    id: Option<String>,
    annotation: Option<Annotation>,
    source: Option<SourceRef>,
    foreign_attributes: Vec<ForeignAttribute>,
}

impl ImportFrame {
    pub fn new(
        attrs: &AttributeMap,
        name_table: &NameTable,
        source: Option<SourceRef>,
    ) -> SchemaResult<Self> {
        let namespace = attrs
            .get_value_by_name(name_table, "namespace")
            .map(String::from);

        let schema_location = attrs
            .get_value_by_name(name_table, "schemaLocation")
            .map(String::from);

        let id = attrs
            .get_value_by_name(name_table, "id")
            .map(String::from);

        Ok(Self {
            namespace,
            schema_location,
            id,
            annotation: None,
            source,
            foreign_attributes: Vec::new(),
        })
    }
}

impl Frame for ImportFrame {
    fn allows(&self, local_name: &str, _name_table: &NameTable) -> bool {
        matches!(local_name, xsd_names::ANNOTATION)
    }

    fn allows_attribute(&self, local_name: &str, _name_table: &NameTable) -> bool {
        matches!(local_name, "namespace" | "schemaLocation" | "id")
    }

    fn on_child_start(&mut self, _local_name: &str, _name_table: &NameTable) {}

    fn attach(&mut self, child: FrameResult) -> SchemaResult<()> {
        if let FrameResult::Annotation(ann) = child {
            self.annotation = Some(ann);
        }
        Ok(())
    }

    fn finish(self: Box<Self>) -> SchemaResult<FrameResult> {
        Ok(FrameResult::Directive(DirectiveResult::Import(ImportResult {
            namespace: self.namespace,
            schema_location: self.schema_location,
            id: self.id,
            annotation: self.annotation,
            source: self.source,
        })))
    }

    fn source(&self) -> Option<&SourceRef> {
        self.source.as_ref()
    }

    fn set_foreign_attributes(&mut self, attrs: Vec<ForeignAttribute>) {
        self.foreign_attributes = attrs;
    }
}

/// Frame for xs:override (XSD 1.1)
pub struct OverrideFrame {
    schema_location: Option<String>,
    id: Option<String>,
    annotation: Option<Annotation>,
    source: Option<SourceRef>,
    foreign_attributes: Vec<ForeignAttribute>,
    // Overridden components (schemaTop)
    simple_types: Vec<SimpleTypeResult>,
    complex_types: Vec<ComplexTypeResult>,
    elements: Vec<ElementFrameResult>,
    attributes: Vec<AttributeFrameResult>,
    groups: Vec<GroupFrameResult>,
    attribute_groups: Vec<GroupFrameResult>,
    notations: Vec<NotationResult>,
}

impl OverrideFrame {
    pub fn new(
        attrs: &AttributeMap,
        name_table: &NameTable,
        source: Option<SourceRef>,
    ) -> SchemaResult<Self> {
        let schema_location = attrs
            .get_value_by_name(name_table, "schemaLocation")
            .map(String::from);

        let id = attrs
            .get_value_by_name(name_table, "id")
            .map(String::from);

        Ok(Self {
            schema_location,
            id,
            annotation: None,
            source,
            foreign_attributes: Vec::new(),
            simple_types: Vec::new(),
            complex_types: Vec::new(),
            elements: Vec::new(),
            attributes: Vec::new(),
            groups: Vec::new(),
            attribute_groups: Vec::new(),
            notations: Vec::new(),
        })
    }
}

impl Frame for OverrideFrame {
    fn allows(&self, local_name: &str, _name_table: &NameTable) -> bool {
        // Override can contain annotation and schemaTop elements
        matches!(
            local_name,
            xsd_names::ANNOTATION
                | xsd_names::SIMPLE_TYPE
                | xsd_names::COMPLEX_TYPE
                | xsd_names::ELEMENT
                | xsd_names::ATTRIBUTE
                | xsd_names::GROUP
                | xsd_names::ATTRIBUTE_GROUP
                | xsd_names::NOTATION
        )
    }

    fn allows_attribute(&self, local_name: &str, _name_table: &NameTable) -> bool {
        matches!(local_name, "schemaLocation" | "id")
    }

    fn on_child_start(&mut self, _local_name: &str, _name_table: &NameTable) {}

    fn attach(&mut self, child: FrameResult) -> SchemaResult<()> {
        match child {
            FrameResult::Annotation(ann) => {
                self.annotation = Some(ann);
            }
            FrameResult::Type(TypeFrameResult::Simple(st)) => {
                self.simple_types.push(st);
            }
            FrameResult::Type(TypeFrameResult::Complex(ct)) => {
                self.complex_types.push(ct);
            }
            FrameResult::Element(el) => {
                self.elements.push(el);
            }
            FrameResult::Attribute(attr) => {
                self.attributes.push(attr);
            }
            FrameResult::Group(group) => {
                match &group {
                    GroupFrameResult::Model(_) => self.groups.push(group),
                    GroupFrameResult::Attribute(_) => self.attribute_groups.push(group),
                }
            }
            FrameResult::Notation(notation) => {
                self.notations.push(notation);
            }
            _ => {}
        }
        Ok(())
    }

    fn finish(self: Box<Self>) -> SchemaResult<FrameResult> {
        let schema_location = self.schema_location.ok_or_else(|| SchemaError::structural(
            "sch-override",
            "xs:override requires 'schemaLocation' attribute",
            None,
        ))?;

        Ok(FrameResult::Directive(DirectiveResult::Override(OverrideResult {
            schema_location,
            id: self.id,
            annotation: self.annotation,
            source: self.source,
            simple_types: self.simple_types,
            complex_types: self.complex_types,
            elements: self.elements,
            attributes: self.attributes,
            groups: self.groups,
            attribute_groups: self.attribute_groups,
            notations: self.notations,
        })))
    }

    fn source(&self) -> Option<&SourceRef> {
        self.source.as_ref()
    }

    fn set_foreign_attributes(&mut self, attrs: Vec<ForeignAttribute>) {
        self.foreign_attributes = attrs;
    }
}

/// Frame for xs:redefine
pub struct RedefineFrame {
    schema_location: Option<String>,
    id: Option<String>,
    annotation: Option<Annotation>,
    components: Vec<RedefineComponent>,
    source: Option<SourceRef>,
    foreign_attributes: Vec<ForeignAttribute>,
}

impl RedefineFrame {
    pub fn new(
        attrs: &AttributeMap,
        name_table: &NameTable,
        source: Option<SourceRef>,
    ) -> SchemaResult<Self> {
        let schema_location = attrs
            .get_value_by_name(name_table, "schemaLocation")
            .map(String::from);

        let id = attrs
            .get_value_by_name(name_table, "id")
            .map(String::from);

        Ok(Self {
            schema_location,
            id,
            annotation: None,
            components: Vec::new(),
            source,
            foreign_attributes: Vec::new(),
        })
    }
}

impl Frame for RedefineFrame {
    fn allows(&self, local_name: &str, _name_table: &NameTable) -> bool {
        matches!(
            local_name,
            xsd_names::ANNOTATION
                | xsd_names::SIMPLE_TYPE
                | xsd_names::COMPLEX_TYPE
                | xsd_names::GROUP
                | xsd_names::ATTRIBUTE_GROUP
        )
    }

    fn allows_attribute(&self, local_name: &str, _name_table: &NameTable) -> bool {
        matches!(local_name, "schemaLocation" | "id")
    }

    fn on_child_start(&mut self, _local_name: &str, _name_table: &NameTable) {}

    fn attach(&mut self, child: FrameResult) -> SchemaResult<()> {
        match child {
            FrameResult::Annotation(ann) => {
                self.annotation = Some(ann);
            }
            FrameResult::Type(TypeFrameResult::Simple(st)) => {
                self.components.push(RedefineComponent::SimpleType(st));
            }
            FrameResult::Type(TypeFrameResult::Complex(ct)) => {
                self.components.push(RedefineComponent::ComplexType(ct));
            }
            FrameResult::Group(GroupFrameResult::Model(mg)) => {
                self.components.push(RedefineComponent::Group(mg));
            }
            FrameResult::Group(GroupFrameResult::Attribute(ag)) => {
                self.components.push(RedefineComponent::AttributeGroup(ag));
            }
            FrameResult::Skip => {}
            _ => {}
        }
        Ok(())
    }

    fn finish(self: Box<Self>) -> SchemaResult<FrameResult> {
        let schema_location = self.schema_location.ok_or_else(|| SchemaError::structural(
            "sch-redefine",
            "xs:redefine requires 'schemaLocation' attribute",
            None,
        ))?;

        Ok(FrameResult::Directive(DirectiveResult::Redefine(RedefineResult {
            schema_location,
            id: self.id,
            annotation: self.annotation,
            components: self.components,
            source: self.source,
        })))
    }

    fn source(&self) -> Option<&SourceRef> {
        self.source.as_ref()
    }

    fn set_foreign_attributes(&mut self, attrs: Vec<ForeignAttribute>) {
        self.foreign_attributes = attrs;
    }
}

// ============================================================================
// Skip Frame (Error Recovery)
// ============================================================================

/// Frame for skipping unknown or invalid elements
pub struct SkipFrame {
    depth: u32,
    source: Option<SourceRef>,
}

impl SkipFrame {
    pub fn new(source: Option<SourceRef>) -> Self {
        Self { depth: 0, source }
    }

    /// Increment depth when entering a child
    pub fn enter(&mut self) {
        self.depth += 1;
    }

    /// Decrement depth when leaving a child
    pub fn leave(&mut self) -> bool {
        if self.depth > 0 {
            self.depth -= 1;
            false
        } else {
            true
        }
    }
}

impl Frame for SkipFrame {
    fn allows(&self, _local_name: &str, _name_table: &NameTable) -> bool {
        true // Accept everything when skipping
    }

    fn allows_attribute(&self, _local_name: &str, _name_table: &NameTable) -> bool {
        true
    }

    fn on_child_start(&mut self, _local_name: &str, _name_table: &NameTable) {
        self.enter();
    }

    fn attach(&mut self, _child: FrameResult) -> SchemaResult<()> {
        Ok(())
    }

    fn finish(self: Box<Self>) -> SchemaResult<FrameResult> {
        Ok(FrameResult::Skip)
    }

    fn source(&self) -> Option<&SourceRef> {
        self.source.as_ref()
    }

    fn set_foreign_attributes(&mut self, _attrs: Vec<ForeignAttribute>) {}

    fn is_skip_frame(&self) -> bool {
        true
    }

    fn on_child_end(&mut self) -> bool {
        self.leave()
    }
}

// ============================================================================
// Helper Functions
// ============================================================================

fn parse_optional_attr<T, F>(
    attrs: &AttributeMap,
    name_table: &NameTable,
    name: &str,
    parse: F,
) -> SchemaResult<Option<T>>
where
    F: FnOnce(&str) -> Result<T, String>,
{
    match attrs.get_value_by_name(name_table, name) {
        Some(value) => {
            let parsed = parse(value).map_err(|err| {
                SchemaError::structural(
                    "sch-attributes",
                    format!("Invalid value for attribute '{}': {}", name, err),
                    None,
                )
            })?;
            Ok(Some(parsed))
        }
        None => Ok(None),
    }
}

fn validate_attr_value<T, F>(
    attrs: &AttributeMap,
    name_table: &NameTable,
    name: &str,
    parse: F,
) -> SchemaResult<()>
where
    F: FnOnce(&str) -> Result<T, String>,
{
    parse_optional_attr(attrs, name_table, name, parse).map(|_| ())
}

fn parse_optional_bool_attr(
    attrs: &AttributeMap,
    name_table: &NameTable,
    name: &str,
) -> SchemaResult<Option<bool>> {
    parse_optional_attr(attrs, name_table, name, parse_boolean)
}

fn parse_bool_attr_default(
    attrs: &AttributeMap,
    name_table: &NameTable,
    name: &str,
    default: bool,
) -> SchemaResult<bool> {
    Ok(parse_optional_bool_attr(attrs, name_table, name)?.unwrap_or(default))
}

fn parse_occurs_attr_raw(
    attrs: &AttributeMap,
    name_table: &NameTable,
    name: &str,
) -> SchemaResult<Option<Option<u32>>> {
    match attrs.get_value_by_name(name_table, name) {
        Some(value) => {
            let parsed = parse_occurs(value).map_err(|err| {
                SchemaError::structural(
                    "sch-attributes",
                    format!("Invalid value for attribute '{}': {}", name, err),
                    None,
                )
            })?;
            Ok(Some(parsed))
        }
        None => Ok(None),
    }
}

fn parse_min_occurs_attr(
    attrs: &AttributeMap,
    name_table: &NameTable,
    name: &str,
) -> SchemaResult<u32> {
    match parse_occurs_attr_raw(attrs, name_table, name)? {
        None => Ok(1),
        Some(Some(value)) => Ok(value),
        Some(None) => Err(SchemaError::structural(
            "sch-attributes",
            format!("Invalid value for attribute '{}': 'unbounded'", name),
            None,
        )),
    }
}

fn parse_max_occurs_attr(
    attrs: &AttributeMap,
    name_table: &NameTable,
    name: &str,
) -> SchemaResult<Option<u32>> {
    match parse_occurs_attr_raw(attrs, name_table, name)? {
        None => Ok(Some(1)),
        Some(Some(value)) => Ok(Some(value)),
        Some(None) => Ok(None),
    }
}

fn parse_process_contents_value(value: &str) -> Result<ProcessContents, String> {
    match value {
        "strict" => Ok(ProcessContents::Strict),
        "lax" => Ok(ProcessContents::Lax),
        "skip" => Ok(ProcessContents::Skip),
        _ => Err(format!("Invalid processContents value: '{}'", value)),
    }
}

fn parse_open_content_mode(value: &str) -> Result<OpenContentMode, String> {
    match value {
        "none" => Ok(OpenContentMode::None),
        "interleave" => Ok(OpenContentMode::Interleave),
        "suffix" => Ok(OpenContentMode::Suffix),
        _ => Err(format!("Invalid open content mode: '{}'", value)),
    }
}

fn parse_process_contents_attr(
    attrs: &AttributeMap,
    name_table: &NameTable,
    name: &str,
) -> SchemaResult<ProcessContents> {
    match parse_optional_attr(attrs, name_table, name, parse_process_contents_value)? {
        Some(value) => Ok(value),
        None => Ok(ProcessContents::Strict),
    }
}

fn parse_open_content_mode_attr(
    attrs: &AttributeMap,
    name_table: &NameTable,
    name: &str,
) -> SchemaResult<OpenContentMode> {
    match parse_optional_attr(attrs, name_table, name, parse_open_content_mode)? {
        Some(value) => Ok(value),
        None => Ok(OpenContentMode::Interleave),
    }
}

/// Parse a derivation set from attribute value
fn parse_derivation_set(value: Option<&str>) -> SchemaResult<DerivationSet> {
    let Some(value) = value else {
        return Ok(DerivationSet::empty());
    };

    if value == "#all" {
        return Ok(DerivationSet::ALL);
    }

    let mut set = DerivationSet::empty();
    for token in value.split_whitespace() {
        match token {
            "extension" => set |= DerivationSet::EXTENSION,
            "restriction" => set |= DerivationSet::RESTRICTION,
            "list" => set |= DerivationSet::LIST,
            "union" => set |= DerivationSet::UNION,
            "substitution" => set |= DerivationSet::SUBSTITUTION,
            _ => {
                return Err(SchemaError::structural(
                    "sch-derivation-set",
                    format!("Invalid derivation method: '{}'", token),
                    None,
                ));
            }
        }
    }

    Ok(set)
}

/// Parse a QName reference
fn parse_qname_ref(value: &str, name_table: &NameTable) -> SchemaResult<QNameRef> {
    let (local, prefix) = if let Some(pos) = value.find(':') {
        let prefix = &value[..pos];
        let local = &value[pos + 1..];
        (local, Some(prefix))
    } else {
        (value, None)
    };

    let local_name = name_table.get(local).ok_or_else(|| SchemaError::structural(
        "sch-qname",
        format!("Unknown local name: '{}'", local),
        None,
    ))?;

    let prefix_id = prefix.and_then(|p| name_table.get(p));

    Ok(QNameRef {
        prefix: prefix_id,
        local_name,
        namespace: None, // To be resolved later
    })
}

/// Parse a list of QName references
fn parse_qname_list(value: &str, name_table: &NameTable) -> SchemaResult<Vec<QNameRef>> {
    value
        .split_whitespace()
        .map(|s| parse_qname_ref(s, name_table))
        .collect()
}

/// Parse namespace constraint for wildcards
fn parse_namespace_constraint(
    value: Option<&str>,
    _name_table: &NameTable,
) -> SchemaResult<WildcardNamespace> {
    let Some(value) = value else {
        return Ok(WildcardNamespace::Any);
    };

    match value {
        "##any" => Ok(WildcardNamespace::Any),
        "##other" => Ok(WildcardNamespace::Other),
        "##targetNamespace" => Ok(WildcardNamespace::TargetNamespace),
        "##local" => Ok(WildcardNamespace::Local),
        _ => {
            // List of namespaces
            let namespaces: Vec<_> = value
                .split_whitespace()
                .map(|s| {
                    if s == "##targetNamespace" || s == "##local" {
                        None
                    } else {
                        // TODO: Intern namespace URIs
                        None
                    }
                })
                .collect();
            Ok(WildcardNamespace::List(namespaces))
        }
    }
}

/// Apply a facet to a facet set
fn apply_facet(facets: &mut FacetSet, facet: FacetResult) -> SchemaResult<()> {
    use crate::types::facets::{FacetFixed, WhitespaceMode};

    let fixed = if facet.fixed {
        FacetFixed::Fixed
    } else {
        FacetFixed::Default
    };

    match facet.kind {
        FacetKind::Enumeration => {
            facets.add_enumeration(facet.value, facet.source);
        }
        FacetKind::Pattern => {
            // Use unchecked to defer pattern compilation to validation phase
            facets.add_pattern_unchecked(facet.value, facet.source);
        }
        FacetKind::MinLength => {
            if let Ok(v) = facet.value.parse() {
                facets.set_min_length(v, fixed, facet.source);
            }
        }
        FacetKind::MaxLength => {
            if let Ok(v) = facet.value.parse() {
                facets.set_max_length(v, fixed, facet.source);
            }
        }
        FacetKind::Length => {
            if let Ok(v) = facet.value.parse() {
                facets.set_length(v, fixed, facet.source);
            }
        }
        FacetKind::MinInclusive => {
            facets.set_min_inclusive(facet.value, fixed, facet.source);
        }
        FacetKind::MaxInclusive => {
            facets.set_max_inclusive(facet.value, fixed, facet.source);
        }
        FacetKind::MinExclusive => {
            facets.set_min_exclusive(facet.value, fixed, facet.source);
        }
        FacetKind::MaxExclusive => {
            facets.set_max_exclusive(facet.value, fixed, facet.source);
        }
        FacetKind::TotalDigits => {
            if let Ok(v) = facet.value.parse() {
                facets.set_total_digits(v, fixed, facet.source);
            }
        }
        FacetKind::FractionDigits => {
            if let Ok(v) = facet.value.parse() {
                facets.set_fraction_digits(v, fixed, facet.source);
            }
        }
        FacetKind::WhiteSpace => {
            let mode = match facet.value.as_str() {
                "preserve" => WhitespaceMode::Preserve,
                "replace" => WhitespaceMode::Replace,
                "collapse" => WhitespaceMode::Collapse,
                _ => WhitespaceMode::Collapse,
            };
            facets.set_whitespace(mode, fixed, facet.source);
        }
        FacetKind::Assertion => {
            // XSD 1.1: assertion facet - the value is the XPath test expression
            facets.add_assertion(facet.value, None, facet.source);
        }
        FacetKind::ExplicitTimezone => {
            // XSD 1.1: explicitTimezone facet
            let mode = match facet.value.as_str() {
                "required" => ExplicitTimezone::Required,
                "prohibited" => ExplicitTimezone::Prohibited,
                "optional" => ExplicitTimezone::Optional,
                _ => ExplicitTimezone::Optional,
            };
            facets.set_explicit_timezone(mode, fixed, facet.source);
        }
    }

    Ok(())
}

// ============================================================================
// Frame Factory
// ============================================================================

/// Create a frame for the given element
pub fn create_frame(
    local_name: &str,
    attrs: &AttributeMap,
    name_table: &NameTable,
    source: Option<SourceRef>,
) -> SchemaResult<Box<dyn Frame>> {
    let frame: Box<dyn Frame> = match local_name {
        xsd_names::SCHEMA => Box::new(SchemaFrame::new(attrs, name_table, source)?),
        xsd_names::SIMPLE_TYPE => Box::new(SimpleTypeFrame::new(attrs, name_table, source)?),
        xsd_names::COMPLEX_TYPE => Box::new(ComplexTypeFrame::new(attrs, name_table, source)?),
        xsd_names::ELEMENT => Box::new(ElementFrame::new(attrs, name_table, source)?),
        xsd_names::ATTRIBUTE => Box::new(AttributeFrame::new(attrs, name_table, source)?),
        xsd_names::GROUP => Box::new(GroupFrame::new(attrs, name_table, source)?),
        xsd_names::ATTRIBUTE_GROUP => Box::new(AttributeGroupFrame::new(attrs, name_table, source)?),
        xsd_names::NOTATION => Box::new(NotationFrame::new(attrs, name_table, source)?),
        xsd_names::SIMPLE_CONTENT => Box::new(SimpleContentFrame::new(attrs, name_table, source)?),
        xsd_names::COMPLEX_CONTENT => Box::new(ComplexContentFrame::new(attrs, name_table, source)?),
        xsd_names::RESTRICTION => Box::new(RestrictionFrame::new(attrs, name_table, source)?),
        xsd_names::EXTENSION => Box::new(ExtensionFrame::new(attrs, name_table, source)?),
        xsd_names::LIST => Box::new(ListFrame::new(attrs, name_table, source)?),
        xsd_names::UNION => Box::new(UnionFrame::new(attrs, name_table, source)?),
        xsd_names::SEQUENCE => {
            Box::new(ModelGroupFrame::new(Compositor::Sequence, attrs, name_table, source)?)
        }
        xsd_names::CHOICE => {
            Box::new(ModelGroupFrame::new(Compositor::Choice, attrs, name_table, source)?)
        }
        xsd_names::ALL => {
            Box::new(ModelGroupFrame::new(Compositor::All, attrs, name_table, source)?)
        }
        xsd_names::ANY => Box::new(AnyFrame::new(attrs, name_table, source)?),
        xsd_names::ANY_ATTRIBUTE => Box::new(AnyAttributeFrame::new(attrs, name_table, source)?),
        xsd_names::ANNOTATION => Box::new(AnnotationFrame::new(attrs, name_table, source)?),
        xsd_names::APPINFO => Box::new(AppinfoFrame::new(attrs, name_table, source)?),
        xsd_names::DOCUMENTATION => Box::new(DocumentationFrame::new(attrs, name_table, source)?),
        xsd_names::INCLUDE => Box::new(IncludeFrame::new(attrs, name_table, source)?),
        xsd_names::IMPORT => Box::new(ImportFrame::new(attrs, name_table, source)?),
        xsd_names::REDEFINE => Box::new(RedefineFrame::new(attrs, name_table, source)?),
        xsd_names::OVERRIDE => Box::new(OverrideFrame::new(attrs, name_table, source)?),
        xsd_names::KEY => Box::new(IdentityFrame::new(IdentityKind::Key, attrs, name_table, source)?),
        xsd_names::KEYREF => Box::new(IdentityFrame::new(IdentityKind::Keyref, attrs, name_table, source)?),
        xsd_names::UNIQUE => Box::new(IdentityFrame::new(IdentityKind::Unique, attrs, name_table, source)?),
        xsd_names::SELECTOR => Box::new(SelectorFrame::new(attrs, name_table, source)?),
        xsd_names::FIELD => Box::new(FieldFrame::new(attrs, name_table, source)?),
        xsd_names::ALTERNATIVE => Box::new(AlternativeFrame::new(attrs, name_table, source)?),
        xsd_names::ASSERT => Box::new(AssertFrame::new(attrs, name_table, source)?),
        xsd_names::ENUMERATION => {
            Box::new(FacetFrame::new(FacetKind::Enumeration, attrs, name_table, source)?)
        }
        xsd_names::PATTERN => Box::new(FacetFrame::new(FacetKind::Pattern, attrs, name_table, source)?),
        xsd_names::MIN_INCLUSIVE => {
            Box::new(FacetFrame::new(FacetKind::MinInclusive, attrs, name_table, source)?)
        }
        xsd_names::MAX_INCLUSIVE => {
            Box::new(FacetFrame::new(FacetKind::MaxInclusive, attrs, name_table, source)?)
        }
        xsd_names::MIN_EXCLUSIVE => {
            Box::new(FacetFrame::new(FacetKind::MinExclusive, attrs, name_table, source)?)
        }
        xsd_names::MAX_EXCLUSIVE => {
            Box::new(FacetFrame::new(FacetKind::MaxExclusive, attrs, name_table, source)?)
        }
        xsd_names::MIN_LENGTH => Box::new(FacetFrame::new(FacetKind::MinLength, attrs, name_table, source)?),
        xsd_names::MAX_LENGTH => Box::new(FacetFrame::new(FacetKind::MaxLength, attrs, name_table, source)?),
        xsd_names::LENGTH => Box::new(FacetFrame::new(FacetKind::Length, attrs, name_table, source)?),
        xsd_names::TOTAL_DIGITS => Box::new(FacetFrame::new(FacetKind::TotalDigits, attrs, name_table, source)?),
        xsd_names::FRACTION_DIGITS => Box::new(FacetFrame::new(FacetKind::FractionDigits, attrs, name_table, source)?),
        xsd_names::WHITE_SPACE => Box::new(FacetFrame::new(FacetKind::WhiteSpace, attrs, name_table, source)?),
        xsd_names::OPEN_CONTENT => Box::new(OpenContentFrame::new(attrs, name_table, source)?),
        xsd_names::DEFAULT_OPEN_CONTENT => {
            Box::new(DefaultOpenContentFrame::new(attrs, name_table, source)?)
        }
        // XSD 1.1 facets
        xsd_names::ASSERTION => {
            Box::new(FacetFrame::new(FacetKind::Assertion, attrs, name_table, source)?)
        }
        xsd_names::EXPLICIT_TIMEZONE => {
            Box::new(FacetFrame::new(FacetKind::ExplicitTimezone, attrs, name_table, source)?)
        }
        _ => {
            // Unknown element - skip it
            Box::new(SkipFrame::new(source))
        }
    };

    frame.validate_attributes(attrs, name_table)?;
    Ok(frame)
}

pub fn create_frame_recovering(
    local_name: &str,
    attrs: &AttributeMap,
    name_table: &NameTable,
    source: Option<SourceRef>,
    errors: &mut Vec<SchemaError>,
) -> Box<dyn Frame> {
    let recovery_source = source.clone();
    match create_frame(local_name, attrs, name_table, source) {
        Ok(frame) => frame,
        Err(err) => {
            errors.push(err);
            Box::new(SkipFrame::new(recovery_source))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::attrs::ParsedAttribute;

    fn make_attr(name_table: &mut NameTable, name: &str, value: &str) -> ParsedAttribute {
        let name_id = name_table.add(name);
        ParsedAttribute {
            namespace: None,
            local_name: name_id,
            prefix: None,
            value: value.to_string(),
            source: None,
        }
    }

    #[test]
    fn test_parse_derivation_set_empty() {
        let set = parse_derivation_set(None).unwrap();
        assert!(set.is_empty());
    }

    #[test]
    fn test_parse_derivation_set_all() {
        let set = parse_derivation_set(Some("#all")).unwrap();
        assert!(set.contains(DerivationSet::EXTENSION));
        assert!(set.contains(DerivationSet::RESTRICTION));
    }

    #[test]
    fn test_parse_derivation_set_list() {
        let set = parse_derivation_set(Some("extension restriction")).unwrap();
        assert!(set.contains(DerivationSet::EXTENSION));
        assert!(set.contains(DerivationSet::RESTRICTION));
        assert!(!set.contains(DerivationSet::LIST));
    }

    #[test]
    fn test_parse_process_contents_attr() {
        let mut name_table = NameTable::new();
        let name_id = name_table.add("processContents");

        let attrs = AttributeMap::new(vec![ParsedAttribute {
            namespace: None,
            local_name: name_id,
            prefix: None,
            value: "lax".to_string(),
            source: None,
        }]);

        assert_eq!(
            parse_process_contents_attr(&attrs, &name_table, "processContents").unwrap(),
            ProcessContents::Lax
        );

        let attrs = AttributeMap::new(Vec::new());
        assert_eq!(
            parse_process_contents_attr(&attrs, &name_table, "processContents").unwrap(),
            ProcessContents::Strict
        );

        let attrs = AttributeMap::new(vec![ParsedAttribute {
            namespace: None,
            local_name: name_id,
            prefix: None,
            value: "invalid".to_string(),
            source: None,
        }]);
        assert!(parse_process_contents_attr(&attrs, &name_table, "processContents").is_err());
    }

    #[test]
    fn test_parse_open_content_mode_attr() {
        let mut name_table = NameTable::new();
        let attrs = AttributeMap::new(vec![make_attr(&mut name_table, "mode", "suffix")]);
        assert_eq!(
            parse_open_content_mode_attr(&attrs, &name_table, "mode").unwrap(),
            OpenContentMode::Suffix
        );

        let attrs = AttributeMap::new(Vec::new());
        assert_eq!(
            parse_open_content_mode_attr(&attrs, &name_table, "mode").unwrap(),
            OpenContentMode::Interleave
        );

        let attrs = AttributeMap::new(vec![make_attr(&mut name_table, "mode", "bad")]);
        assert!(parse_open_content_mode_attr(&attrs, &name_table, "mode").is_err());
    }

    #[test]
    fn test_override_requires_schema_location() {
        let name_table = NameTable::new();
        let attrs = AttributeMap::new(Vec::new());
        let frame = OverrideFrame::new(&attrs, &name_table, None).unwrap();
        assert!(Box::new(frame).finish().is_err());
    }

    #[test]
    fn test_schema_result_retains_annotations() {
        let name_table = NameTable::new();
        let attrs = AttributeMap::new(Vec::new());
        let mut frame = SchemaFrame::new(&attrs, &name_table, None).unwrap();
        frame
            .attach(FrameResult::Annotation(Annotation::new()))
            .unwrap();
        let result = Box::new(frame).finish().unwrap();
        match result {
            FrameResult::Schema(schema) => {
                assert_eq!(schema.annotations.len(), 1);
            }
            _ => panic!("expected schema result"),
        }
    }

    #[test]
    fn test_element_substitution_group_list() {
        let mut name_table = NameTable::new();
        name_table.add("root");
        name_table.add("head1");
        name_table.add("head2");
        let attrs = AttributeMap::new(vec![
            make_attr(&mut name_table, "name", "root"),
            make_attr(&mut name_table, "substitutionGroup", "head1 head2"),
        ]);
        let frame = ElementFrame::new(&attrs, &name_table, None).unwrap();
        let result = Box::new(frame).finish().unwrap();
        match result {
            FrameResult::Element(element) => {
                assert_eq!(element.substitution_group.len(), 2);
                assert_eq!(
                    name_table.resolve(element.substitution_group[0].local_name),
                    "head1"
                );
                assert_eq!(
                    name_table.resolve(element.substitution_group[1].local_name),
                    "head2"
                );
            }
            _ => panic!("expected element result"),
        }
    }

    #[test]
    fn test_identity_requires_selector() {
        let mut name_table = NameTable::new();
        name_table.add("key1");
        let attrs = AttributeMap::new(vec![make_attr(&mut name_table, "name", "key1")]);
        let frame = IdentityFrame::new(IdentityKind::Key, &attrs, &name_table, None).unwrap();
        assert!(Box::new(frame).finish().is_err());
    }

    #[test]
    fn test_identity_requires_field() {
        let mut name_table = NameTable::new();
        name_table.add("key1");
        let attrs = AttributeMap::new(vec![make_attr(&mut name_table, "name", "key1")]);
        let mut frame =
            IdentityFrame::new(IdentityKind::Unique, &attrs, &name_table, None).unwrap();
        frame
            .attach(FrameResult::Selector(SelectorResult {
                xpath: ".//a".to_string(),
                xpath_default_namespace: None,
                id: None,
                annotation: None,
                source: None,
            }))
            .unwrap();
        assert!(Box::new(frame).finish().is_err());
    }

    #[test]
    fn test_parse_bool_attr_default_invalid() {
        let mut name_table = NameTable::new();
        let attrs = AttributeMap::new(vec![make_attr(&mut name_table, "mixed", "maybe")]);
        assert!(parse_bool_attr_default(&attrs, &name_table, "mixed", false).is_err());
    }

    #[test]
    fn test_parse_min_occurs_attr_invalid() {
        let mut name_table = NameTable::new();
        let attrs = AttributeMap::new(vec![make_attr(&mut name_table, "minOccurs", "unbounded")]);
        assert!(parse_min_occurs_attr(&attrs, &name_table, "minOccurs").is_err());

        let attrs = AttributeMap::new(vec![make_attr(&mut name_table, "minOccurs", "nope")]);
        assert!(parse_min_occurs_attr(&attrs, &name_table, "minOccurs").is_err());
    }

    #[test]
    fn test_parse_max_occurs_attr() {
        let mut name_table = NameTable::new();
        let attrs = AttributeMap::new(vec![make_attr(&mut name_table, "maxOccurs", "unbounded")]);
        assert_eq!(
            parse_max_occurs_attr(&attrs, &name_table, "maxOccurs").unwrap(),
            None
        );

        let attrs = AttributeMap::new(vec![make_attr(&mut name_table, "maxOccurs", "bad")]);
        assert!(parse_max_occurs_attr(&attrs, &name_table, "maxOccurs").is_err());
    }

    #[test]
    fn test_validate_attr_value_invalid_form() {
        let mut name_table = NameTable::new();
        let attrs = AttributeMap::new(vec![make_attr(&mut name_table, "form", "bad")]);
        assert!(validate_attr_value(&attrs, &name_table, "form", parse_form).is_err());
    }

    #[test]
    fn test_create_frame_recovering_on_invalid_value() {
        let mut name_table = NameTable::new();
        let attrs = AttributeMap::new(vec![make_attr(&mut name_table, "nillable", "maybe")]);
        let mut errors = Vec::new();

        let frame = create_frame_recovering(
            xsd_names::ELEMENT,
            &attrs,
            &name_table,
            None,
            &mut errors,
        );

        assert_eq!(errors.len(), 1);
        let result = frame.finish().unwrap();
        assert!(matches!(result, FrameResult::Skip));
    }

    #[test]
    fn test_skip_frame_depth() {
        let mut frame = SkipFrame::new(None);
        frame.enter();
        frame.enter();
        assert!(!frame.leave());
        assert!(!frame.leave());
        assert!(frame.leave());
    }

    #[test]
    fn test_facet_kind() {
        assert_eq!(FacetKind::Enumeration, FacetKind::Enumeration);
        assert_ne!(FacetKind::Enumeration, FacetKind::Pattern);
    }

    #[test]
    fn test_compositor() {
        assert_eq!(Compositor::Sequence, Compositor::Sequence);
        assert_ne!(Compositor::Sequence, Compositor::Choice);
    }

    #[test]
    fn test_identity_kind() {
        assert_eq!(IdentityKind::Key, IdentityKind::Key);
        assert_ne!(IdentityKind::Key, IdentityKind::Keyref);
    }
}
