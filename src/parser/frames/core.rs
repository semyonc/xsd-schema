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
                    "ct-props-correct",
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
