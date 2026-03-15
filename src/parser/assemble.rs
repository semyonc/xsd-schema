//! Assemble schema components from parser frame results.
//!
//! Converts `SchemaFrameResult` into `SchemaDocument` and registers top-level
//! components in the schema set arenas and namespace tables.

use std::collections::HashSet;

use crate::arenas::{
    AttributeDeclData, AttributeGroupData, ComplexTypeDefData, ElementDeclData,
    IdentityConstraintData, ModelGroupData, NotationData, SimpleTypeDefData,
};
use crate::error::{SchemaError, SchemaResult};
use crate::ids::{
    AttributeGroupKey, AttributeKey, DocumentId, ElementKey, IdentityConstraintKey, ModelGroupKey,
    NameId, NotationKey, TypeKey,
};
use crate::parser::frames::{
    AttributeFrameResult, AttributeGroupDefResult, ComplexContentResult, ComplexTypeResult,
    DirectiveResult, FrameResult, GroupFrameResult, ModelGroupDefResult, NotationResult,
    OverrideResult, RedefineComponent, SchemaFrameResult, SimpleTypeResult, TypeFrameResult,
};
use crate::parser::location::SourceRef;
use crate::namespace::QualifiedName;
use crate::schema::composition::{
    ComponentIdentity, ComponentKey, ComponentKind, DocumentComponentIndex,
};
use crate::schema::model::{
    FormChoice, ImportDirective, IncludeDirective, OverrideComponent, OverrideDirective,
    RedefineDirective, SchemaDocument,
};
use crate::schema::wildcard::{ElementWildcard, NamespaceConstraint, ProcessContents as SchemaProcessContents};
use crate::SchemaSet;

/// Result type for convert_directives function
type DirectivesResult = (
    Vec<IncludeDirective>,
    Vec<ImportDirective>,
    Vec<RedefineDirective>,
    Vec<OverrideDirective>,
);

pub struct SchemaAssembler<'a> {
    schema_set: &'a mut SchemaSet,
    target_namespace: Option<NameId>,
    block_default: crate::schema::model::DerivationSet,
    final_default: crate::schema::model::DerivationSet,
    /// Identity constraint names seen in this document (for uniqueness checking)
    /// XSD constraint: Identity Constraint Name Uniqueness - names must be unique per schema document
    identity_constraint_names: HashSet<NameId>,
    /// Per-document component index being built during assembly
    doc_components: DocumentComponentIndex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GroupKeyResult {
    Model(ModelGroupKey),
    Attribute(AttributeGroupKey),
}

impl<'a> SchemaAssembler<'a> {
    pub fn new(
        schema_set: &'a mut SchemaSet,
        target_namespace: Option<NameId>,
        block_default: crate::schema::model::DerivationSet,
        final_default: crate::schema::model::DerivationSet,
    ) -> Self {
        Self {
            schema_set,
            target_namespace,
            block_default,
            final_default,
            identity_constraint_names: HashSet::new(),
            doc_components: DocumentComponentIndex::new(),
        }
    }

    pub fn assemble(
        &mut self,
        schema: SchemaFrameResult,
        doc_id: DocumentId,
        base_uri: &str,
    ) -> SchemaResult<SchemaDocument> {
        let mut doc = build_schema_document(&schema, doc_id, base_uri, &mut self.schema_set.name_table)?;

        let directives = schema.directives;
        let components = schema.components;
        let (includes, imports, redefines, overrides) = convert_directives(directives, self)?;
        doc.includes = includes;
        doc.imports = imports;
        doc.redefines = redefines;
        doc.overrides = overrides;

        for component in components {
            self.assemble_component(component)?;
        }

        doc.component_index = std::mem::take(&mut self.doc_components);
        Ok(doc)
    }

    fn assemble_component(&mut self, component: FrameResult) -> SchemaResult<()> {
        match component {
            FrameResult::Type(result) => {
                self.assemble_type(result, true)?;
            }
            FrameResult::Element(result) => {
                self.assemble_element(result, true)?;
            }
            FrameResult::Attribute(result) => {
                self.assemble_attribute(result, true)?;
            }
            FrameResult::Group(result) => {
                self.assemble_group(result, true)?;
            }
            FrameResult::Notation(result) => {
                self.assemble_notation(result, true)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn assemble_type(&mut self, result: TypeFrameResult, register: bool) -> SchemaResult<TypeKey> {
        match result {
            TypeFrameResult::Simple(simple) => {
                let SimpleTypeResult {
                    name,
                    variety,
                    base_type,
                    item_type,
                    member_types,
                    facets,
                    final_derivation,
                    id,
                    derivation_id,
                    annotation,
                    source,
                } = *simple;
                let mut final_derivation = final_derivation;
                if final_derivation.is_empty() {
                    final_derivation = self.final_default;
                }
                let source_ref = source.clone();
                let name = name.ok_or_else(|| missing_name("simpleType", source_ref.as_ref(), self.schema_set))?;
                let data = SimpleTypeDefData {
                    name: Some(name),
                    target_namespace: self.target_namespace,
                    variety,
                    base_type,
                    item_type,
                    member_types,
                    facets,
                    final_derivation,
                    id,
                    derivation_id,
                    annotation,
                    source,
                    // Resolved references (populated after reference resolution phase)
                    resolved_base_type: None,
                    resolved_item_type: None,
                    resolved_member_types: Vec::new(),
                };
                let key = self.schema_set.arenas.alloc_simple_type(data);
                let type_key = TypeKey::Simple(key);
                if register {
                    self.register_type(name, type_key, source_ref.as_ref())?;
                }
                Ok(type_key)
            }
            TypeFrameResult::Complex(complex) => {
                let ComplexTypeResult {
                    name,
                    base_type,
                    derivation_method,
                    content,
                    attributes,
                    attribute_groups,
                    attribute_wildcard,
                    mixed,
                    is_abstract,
                    final_derivation,
                    block,
                    default_attributes_apply,
                    id,
                    #[cfg(feature = "xsd11")]
                    xpath_default_namespace,
                    annotation,
                    source,
                } = *complex;
                let mut final_derivation = final_derivation;
                let mut block = block;
                if final_derivation.is_empty() {
                    final_derivation = self.final_default;
                }
                if block.is_empty() {
                    block = self.block_default;
                }
                let open_content = match &content {
                    ComplexContentResult::Complex(def) => def.open_content.clone(),
                    _ => None,
                };
                #[cfg(feature = "xsd11")]
                let assertions = match &content {
                    ComplexContentResult::Simple(sc) => sc.assertions.clone(),
                    ComplexContentResult::Complex(cc) => cc.assertions.clone(),
                    ComplexContentResult::Empty => Vec::new(),
                };
                let source_ref = source.clone();
                let name = name.ok_or_else(|| missing_name("complexType", source_ref.as_ref(), self.schema_set))?;
                let data = ComplexTypeDefData {
                    name: Some(name),
                    target_namespace: self.target_namespace,
                    base_type,
                    derivation_method,
                    content,
                    open_content,
                    attributes,
                    attribute_groups,
                    attribute_wildcard,
                    mixed,
                    is_abstract,
                    final_derivation,
                    block,
                    default_attributes_apply,
                    id,
                    #[cfg(feature = "xsd11")]
                    assertions,
                    #[cfg(feature = "xsd11")]
                    xpath_default_namespace,
                    annotation,
                    source,
                    // Resolved references (populated after reference resolution phase)
                    resolved_base_type: None,
                    resolved_attribute_groups: Vec::new(),
                    resolved_attributes: Vec::new(),
                    resolved_content_particle_types: Vec::new(),
                    resolved_content_particle_elements: Vec::new(),
                };
                let key = self.schema_set.arenas.alloc_complex_type(data);
                let type_key = TypeKey::Complex(key);
                if register {
                    self.register_type(name, type_key, source_ref.as_ref())?;
                }
                Ok(type_key)
            }
        }
    }

    fn assemble_element(&mut self, result: crate::parser::frames::ElementFrameResult, register: bool) -> SchemaResult<ElementKey> {
        let crate::parser::frames::ElementFrameResult {
            name,
            ref_name,
            target_namespace: local_namespace,
            type_ref,
            inline_type,
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
            alternatives,
            identity_constraints,
            annotation,
            source,
        } = result;
        let source_ref = source.clone();
        let name = name.ok_or_else(|| missing_name("element", source_ref.as_ref(), self.schema_set))?;
        let target_namespace = local_namespace.or(self.target_namespace);
        let mut block = block;
        let mut final_derivation = final_derivation;
        if ref_name.is_none() {
            if block.is_empty() {
                block = self.block_default;
            }
            if final_derivation.is_empty() {
                final_derivation = self.final_default;
            }
        }

        // Check identity constraint name uniqueness (per schema document)
        // XSD Constraint: Identity Constraint Name Uniqueness (§3.11.1)
        for ic in &identity_constraints {
            if !self.identity_constraint_names.insert(ic.name) {
                // Name already exists in this document - duplicate error
                let location = ic.source.as_ref().and_then(|s| self.schema_set.source_maps.locate(s));
                let name_str = self.schema_set.name_table.resolve(ic.name);
                return Err(SchemaError::structural(
                    "ic-unique",
                    format!("Duplicate identity constraint name '{}' in schema document", name_str),
                    location,
                ));
            }
        }

        // Allocate identity constraints into the arena
        let identity_constraint_keys: Vec<IdentityConstraintKey> = identity_constraints
            .into_iter()
            .map(|ic| {
                self.schema_set.arenas.alloc_identity_constraint(IdentityConstraintData {
                    kind: ic.kind,
                    name: ic.name,
                    ref_name: ic.ref_name,
                    refer: ic.refer,
                    selector: ic.selector,
                    fields: ic.fields,
                    id: ic.id,
                    annotation: ic.annotation,
                    source: ic.source,
                })
            })
            .collect();

        let data = ElementDeclData {
            name: Some(name),
            target_namespace,
            ref_name,
            type_ref,
            inline_type,
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
            alternatives,
            identity_constraints: identity_constraint_keys,
            annotation,
            source,
            // Resolved references (populated after reference resolution phase)
            resolved_type: None,
            resolved_ref: None,
            resolved_substitution_groups: Vec::new(),
        };
        let key = self.schema_set.arenas.alloc_element(data);
        if register {
            self.register_element(name, key, source_ref.as_ref())?;
        }
        Ok(key)
    }

    fn assemble_attribute(&mut self, result: AttributeFrameResult, register: bool) -> SchemaResult<AttributeKey> {
        let AttributeFrameResult {
            name,
            ref_name,
            target_namespace: local_namespace,
            type_ref,
            inline_type,
            default_value,
            fixed_value,
            use_kind,
            form,
            inheritable,
            id,
            annotation,
            source,
        } = result;
        let source_ref = source.clone();
        let name = name.ok_or_else(|| missing_name("attribute", source_ref.as_ref(), self.schema_set))?;
        let target_namespace = local_namespace.or(self.target_namespace);
        let data = AttributeDeclData {
            name: Some(name),
            target_namespace,
            ref_name,
            type_ref,
            inline_type,
            default_value,
            fixed_value,
            use_kind,
            form,
            inheritable,
            id,
            annotation,
            source,
            // Resolved references (populated after reference resolution phase)
            resolved_type: None,
            resolved_ref: None,
        };
        let key = self.schema_set.arenas.alloc_attribute(data);
        if register {
            self.register_attribute(name, key, source_ref.as_ref())?;
        }
        Ok(key)
    }

    fn assemble_group(
        &mut self,
        result: GroupFrameResult,
        register: bool,
    ) -> SchemaResult<GroupKeyResult> {
        match result {
            GroupFrameResult::Model(group) => {
                let ModelGroupDefResult {
                    name,
                    ref_name,
                    compositor,
                    particles,
                    min_occurs,
                    max_occurs,
                    id,
                    annotation,
                    source,
                } = *group;
                let source_ref = source.clone();
                let name = name.ok_or_else(|| missing_name("group", source_ref.as_ref(), self.schema_set))?;
                let data = ModelGroupData {
                    name: Some(name),
                    target_namespace: self.target_namespace,
                    ref_name,
                    compositor,
                    particles,
                    min_occurs,
                    max_occurs,
                    id,
                    annotation,
                    source,
                    // Resolved references (populated after reference resolution phase)
                    resolved_ref: None,
                    resolved_particles: Vec::new(),
                    resolved_particle_types: Vec::new(),
                    resolved_particle_elements: Vec::new(),
                };
                let key = self.schema_set.arenas.alloc_model_group(data);
                if register {
                    self.register_model_group(name, key, source_ref.as_ref())?;
                }
                Ok(GroupKeyResult::Model(key))
            }
            GroupFrameResult::Attribute(group) => {
                let AttributeGroupDefResult {
                    name,
                    ref_name,
                    attributes,
                    attribute_groups,
                    attribute_wildcard,
                    id,
                    annotation,
                    source,
                } = *group;
                let source_ref = source.clone();
                let name = name.ok_or_else(|| missing_name("attributeGroup", source_ref.as_ref(), self.schema_set))?;
                let data = AttributeGroupData {
                    name: Some(name),
                    target_namespace: self.target_namespace,
                    ref_name,
                    attributes,
                    attribute_groups,
                    attribute_wildcard,
                    id,
                    annotation,
                    source,
                    // Resolved references (populated after reference resolution phase)
                    resolved_ref: None,
                    resolved_attribute_groups: Vec::new(),
                    resolved_attributes: Vec::new(),
                };
                let key = self.schema_set.arenas.alloc_attribute_group(data);
                if register {
                    self.register_attribute_group(name, key, source_ref.as_ref())?;
                }
                Ok(GroupKeyResult::Attribute(key))
            }
        }
    }

    fn assemble_notation(&mut self, result: NotationResult, register: bool) -> SchemaResult<NotationKey> {
        let NotationResult {
            name,
            public,
            system,
            id,
            annotation,
            source,
        } = result;
        let source_ref = source.clone();
        let name = name.ok_or_else(|| missing_name("notation", source_ref.as_ref(), self.schema_set))?;
        let data = NotationData {
            name,
            target_namespace: self.target_namespace,
            public,
            system,
            id,
            annotation,
            source,
        };
        let key = self.schema_set.arenas.alloc_notation(data);
        if register {
            self.register_notation(name, key, source_ref.as_ref())?;
        }
        Ok(key)
    }

    fn register_type(
        &mut self,
        name: NameId,
        key: TypeKey,
        source: Option<&SourceRef>,
    ) -> SchemaResult<()> {
        let location = source.and_then(|s| self.schema_set.source_maps.locate(s));
        let name_str = self.schema_set.name_table.resolve(name).to_string();
        let ns_table = self.schema_set.get_or_create_namespace(self.target_namespace);
        if ns_table.register_type(name, key).is_some() {
            return Err(SchemaError::structural(
                "sch-props-correct",
                format!("Duplicate type declaration '{}'", name_str),
                location,
            ));
        }
        let kind = match key {
            TypeKey::Simple(_) => ComponentKind::SimpleType,
            TypeKey::Complex(_) => ComponentKind::ComplexType,
        };
        self.record_component(kind, name, ComponentKey::Type(key));
        Ok(())
    }

    /// Record a component in the per-document component index.
    fn record_component(&mut self, kind: ComponentKind, name: NameId, key: ComponentKey) {
        self.doc_components.insert(
            ComponentIdentity {
                kind,
                name,
                namespace: self.target_namespace,
            },
            key,
        );
    }

    fn register_element(
        &mut self,
        name: NameId,
        key: ElementKey,
        source: Option<&SourceRef>,
    ) -> SchemaResult<()> {
        let location = source.and_then(|s| self.schema_set.source_maps.locate(s));
        let name_str = self.schema_set.name_table.resolve(name).to_string();
        let ns_table = self.schema_set.get_or_create_namespace(self.target_namespace);
        if ns_table.register_element(name, key).is_some() {
            return Err(SchemaError::structural(
                "sch-props-correct",
                format!("Duplicate element declaration '{}'", name_str),
                location,
            ));
        }
        self.record_component(ComponentKind::Element, name, ComponentKey::Element(key));
        Ok(())
    }

    fn register_attribute(
        &mut self,
        name: NameId,
        key: AttributeKey,
        source: Option<&SourceRef>,
    ) -> SchemaResult<()> {
        let location = source.and_then(|s| self.schema_set.source_maps.locate(s));
        let name_str = self.schema_set.name_table.resolve(name).to_string();
        let ns_table = self.schema_set.get_or_create_namespace(self.target_namespace);
        if ns_table.register_attribute(name, key).is_some() {
            return Err(SchemaError::structural(
                "sch-props-correct",
                format!("Duplicate attribute declaration '{}'", name_str),
                location,
            ));
        }
        self.record_component(ComponentKind::Attribute, name, ComponentKey::Attribute(key));
        Ok(())
    }

    fn register_model_group(
        &mut self,
        name: NameId,
        key: ModelGroupKey,
        source: Option<&SourceRef>,
    ) -> SchemaResult<()> {
        let location = source.and_then(|s| self.schema_set.source_maps.locate(s));
        let name_str = self.schema_set.name_table.resolve(name).to_string();
        let ns_table = self.schema_set.get_or_create_namespace(self.target_namespace);
        if ns_table.register_model_group(name, key).is_some() {
            return Err(SchemaError::structural(
                "sch-props-correct",
                format!("Duplicate group declaration '{}'", name_str),
                location,
            ));
        }
        self.record_component(ComponentKind::ModelGroup, name, ComponentKey::ModelGroup(key));
        Ok(())
    }

    fn register_attribute_group(
        &mut self,
        name: NameId,
        key: AttributeGroupKey,
        source: Option<&SourceRef>,
    ) -> SchemaResult<()> {
        let location = source.and_then(|s| self.schema_set.source_maps.locate(s));
        let name_str = self.schema_set.name_table.resolve(name).to_string();
        let ns_table = self.schema_set.get_or_create_namespace(self.target_namespace);
        if ns_table.register_attribute_group(name, key).is_some() {
            return Err(SchemaError::structural(
                "sch-props-correct",
                format!("Duplicate attribute group declaration '{}'", name_str),
                location,
            ));
        }
        self.record_component(ComponentKind::AttributeGroup, name, ComponentKey::AttributeGroup(key));
        Ok(())
    }

    fn register_notation(
        &mut self,
        name: NameId,
        key: NotationKey,
        source: Option<&SourceRef>,
    ) -> SchemaResult<()> {
        let location = source.and_then(|s| self.schema_set.source_maps.locate(s));
        let name_str = self.schema_set.name_table.resolve(name).to_string();
        let ns_table = self.schema_set.get_or_create_namespace(self.target_namespace);
        if ns_table.register_notation(name, key).is_some() {
            return Err(SchemaError::structural(
                "sch-props-correct",
                format!("Duplicate notation declaration '{}'", name_str),
                location,
            ));
        }
        self.record_component(ComponentKind::Notation, name, ComponentKey::Notation(key));
        Ok(())
    }
}

pub fn assemble_schema(
    schema_set: &mut SchemaSet,
    doc_id: DocumentId,
    base_uri: &str,
    result: SchemaFrameResult,
) -> SchemaResult<SchemaDocument> {
    let target_namespace = result.target_namespace;
    let mut assembler = SchemaAssembler::new(
        schema_set,
        target_namespace,
        result.block_default,
        result.final_default,
    );
    assembler.assemble(result, doc_id, base_uri)
}

pub fn build_schema_document(
    result: &SchemaFrameResult,
    doc_id: DocumentId,
    base_uri: &str,
    name_table: &mut crate::namespace::NameTable,
) -> SchemaResult<SchemaDocument> {
    let mut doc = SchemaDocument::new(doc_id, base_uri.to_string());

    doc.target_namespace = result.target_namespace;
    doc.element_form_default = parse_form_choice(result.element_form_default.as_deref());
    doc.attribute_form_default = parse_form_choice(result.attribute_form_default.as_deref());
    doc.block_default = result.block_default;
    doc.final_default = result.final_default;
    doc.version = result.version.clone();
    doc.source = result.source.clone();
    doc.schema_id = result.id.clone();
    doc.xml_lang = result.xml_lang.clone();
    doc.annotations = result.annotations.clone();

    if let Some(default_attrs) = &result.default_attributes {
        doc.default_attributes = Some(QualifiedName::new(
            default_attrs.namespace,
            default_attrs.local_name,
            default_attrs.prefix,
        ));
    }

    if let Some(xpath_ns) = result.xpath_default_namespace.as_deref() {
        doc.xpath_default_namespace = name_table.get(xpath_ns).or_else(|| Some(name_table.add(xpath_ns)));
    }

    if let Some(doc_result) = &result.default_open_content {
        doc.default_open_content = Some(crate::schema::DefaultOpenContent {
            source: doc_result.source.clone(),
            applies_to_empty: doc_result.applies_to_empty,
            mode: convert_open_content_mode(doc_result.mode),
            wildcard: doc_result
                .wildcard
                .as_ref()
                .map(|wc| convert_element_wildcard(wc, result.target_namespace)),
        });
    }

    Ok(doc)
}

pub fn convert_directives(
    directives: Vec<DirectiveResult>,
    assembler: &mut SchemaAssembler<'_>,
) -> SchemaResult<DirectivesResult> {
    let mut includes = Vec::new();
    let mut imports = Vec::new();
    let mut redefines = Vec::new();
    let mut overrides = Vec::new();

    for directive in directives {
        match directive {
            DirectiveResult::Include(inc) => {
                includes.push(IncludeDirective {
                    source: inc.source.clone(),
                    schema_location: inc.schema_location.clone(),
                    resolved_doc_id: None,
                });
            }
            DirectiveResult::Import(imp) => {
                imports.push(ImportDirective {
                    source: imp.source.clone(),
                    namespace: imp.namespace.clone(),
                    schema_location: imp.schema_location.clone(),
                    resolved_doc_id: None,
                });
            }
            DirectiveResult::Redefine(red) => {
                let mut simple_types = Vec::new();
                let mut complex_types = Vec::new();
                let mut groups = Vec::new();
                let mut attribute_groups = Vec::new();

                for component in red.components {
                    match component {
                        RedefineComponent::SimpleType(st) => {
                            let key = assembler.assemble_type(TypeFrameResult::Simple(st), false)?;
                            if let TypeKey::Simple(simple) = key {
                                simple_types.push(simple);
                            }
                        }
                        RedefineComponent::ComplexType(ct) => {
                            let key = assembler.assemble_type(TypeFrameResult::Complex(ct), false)?;
                            if let TypeKey::Complex(complex) = key {
                                complex_types.push(complex);
                            }
                        }
                        RedefineComponent::Group(group) => {
                            if let GroupKeyResult::Model(key) = assembler
                                .assemble_group(GroupFrameResult::Model(group), false)?
                            {
                                groups.push(key);
                            }
                        }
                        RedefineComponent::AttributeGroup(group) => {
                            if let GroupKeyResult::Attribute(key) = assembler
                                .assemble_group(GroupFrameResult::Attribute(group), false)?
                            {
                                attribute_groups.push(key);
                            }
                        }
                    }
                }

                redefines.push(RedefineDirective {
                    source: red.source.clone(),
                    schema_location: red.schema_location.clone(),
                    resolved_doc_id: None,
                    simple_types,
                    complex_types,
                    groups,
                    attribute_groups,
                });
            }
            DirectiveResult::Override(override_result) => {
                overrides.push(convert_override(override_result, assembler)?);
            }
        }
    }

    Ok((includes, imports, redefines, overrides))
}

fn convert_override(
    override_result: OverrideResult,
    assembler: &mut SchemaAssembler<'_>,
) -> SchemaResult<OverrideDirective> {
    let mut components = Vec::new();

    for st in override_result.simple_types {
        let key = assembler.assemble_type(TypeFrameResult::Simple(Box::new(st)), false)?;
        if let TypeKey::Simple(simple) = key {
            components.push(OverrideComponent::SimpleType(simple));
        }
    }

    for ct in override_result.complex_types {
        let key = assembler.assemble_type(TypeFrameResult::Complex(Box::new(ct)), false)?;
        if let TypeKey::Complex(complex) = key {
            components.push(OverrideComponent::ComplexType(complex));
        }
    }

    for el in override_result.elements {
        let key = assembler.assemble_element(el, false)?;
        components.push(OverrideComponent::Element(key));
    }

    for attr in override_result.attributes {
        let key = assembler.assemble_attribute(attr, false)?;
        components.push(OverrideComponent::Attribute(key));
    }

    for group in override_result.groups {
        match group {
            GroupFrameResult::Model(model) => {
                if let GroupKeyResult::Model(key) = assembler
                    .assemble_group(GroupFrameResult::Model(model), false)?
                {
                    components.push(OverrideComponent::Group(key));
                }
            }
            GroupFrameResult::Attribute(group) => {
                if let GroupKeyResult::Attribute(key) = assembler
                    .assemble_group(GroupFrameResult::Attribute(group), false)?
                {
                    components.push(OverrideComponent::AttributeGroup(key));
                }
            }
        }
    }

    for group in override_result.attribute_groups {
        if let GroupFrameResult::Attribute(group) = group {
            if let GroupKeyResult::Attribute(key) = assembler
                .assemble_group(GroupFrameResult::Attribute(group), false)?
            {
                components.push(OverrideComponent::AttributeGroup(key));
            }
        }
    }

    for notation in override_result.notations {
        let key = assembler.assemble_notation(notation, false)?;
        components.push(OverrideComponent::Notation(key));
    }

    Ok(OverrideDirective {
        source: override_result.source.clone(),
        schema_location: override_result.schema_location.clone(),
        resolved_doc_id: None,
        components,
    })
}

pub fn parse_form_choice(value: Option<&str>) -> FormChoice {
    match value {
        Some("qualified") => FormChoice::Qualified,
        Some("unqualified") | None => FormChoice::Unqualified,
        _ => FormChoice::Unqualified,
    }
}

fn convert_open_content_mode(
    mode: crate::parser::frames::OpenContentMode,
) -> crate::schema::OpenContentMode {
    match mode {
        crate::parser::frames::OpenContentMode::None => crate::schema::OpenContentMode::None,
        crate::parser::frames::OpenContentMode::Interleave => {
            crate::schema::OpenContentMode::Interleave
        }
        crate::parser::frames::OpenContentMode::Suffix => crate::schema::OpenContentMode::Suffix,
    }
}

fn convert_element_wildcard(
    wildcard: &crate::parser::frames::WildcardResult,
    target_namespace: Option<NameId>,
) -> ElementWildcard {
    use crate::schema::wildcard::QNameDisallowed;

    let mut result = ElementWildcard::new();
    result.namespace_constraint = match &wildcard.namespace {
        crate::parser::frames::WildcardNamespace::Any => NamespaceConstraint::Any,
        crate::parser::frames::WildcardNamespace::Other => NamespaceConstraint::Other,
        crate::parser::frames::WildcardNamespace::TargetNamespace => {
            NamespaceConstraint::Enumeration(vec![target_namespace])
        }
        crate::parser::frames::WildcardNamespace::Local => {
            NamespaceConstraint::Enumeration(vec![None])
        }
        crate::parser::frames::WildcardNamespace::List(list) => {
            NamespaceConstraint::Enumeration(
                list.iter().map(|t| t.resolve(target_namespace)).collect()
            )
        }
    };

    // notNamespace → NamespaceConstraint::Not(...)
    if !wildcard.not_namespace.is_empty() {
        let excluded: Vec<Option<NameId>> = wildcard.not_namespace.iter()
            .map(|t| t.resolve(target_namespace))
            .collect();
        result.namespace_constraint = NamespaceConstraint::Not(excluded);
    }

    // notQName → not_qnames
    result.not_qnames = wildcard.not_qname.iter().map(|item| {
        match item {
            crate::parser::frames::NotQNameItem::QName { namespace, local_name } => {
                QNameDisallowed::QName { namespace: *namespace, local_name: *local_name }
            }
            crate::parser::frames::NotQNameItem::Defined => QNameDisallowed::Defined,
            crate::parser::frames::NotQNameItem::DefinedSibling => QNameDisallowed::DefinedSibling,
        }
    }).collect();

    result.process_contents = match wildcard.process_contents {
        crate::parser::frames::ProcessContents::Strict => SchemaProcessContents::Strict,
        crate::parser::frames::ProcessContents::Lax => SchemaProcessContents::Lax,
        crate::parser::frames::ProcessContents::Skip => SchemaProcessContents::Skip,
    };
    result.id = wildcard.id.clone();
    result.source = wildcard.source.clone();
    result
}

fn missing_name(
    kind: &str,
    source: Option<&SourceRef>,
    schema_set: &SchemaSet,
) -> SchemaError {
    let location = source.and_then(|s| schema_set.source_maps.locate(s));
    SchemaError::structural(
        "src-resolve",
        format!("Missing required name for {}", kind),
        location,
    )
}
