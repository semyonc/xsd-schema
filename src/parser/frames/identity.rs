// ============================================================================
// Identity Constraint Frames
// ============================================================================

/// Frame for xs:selector
pub struct SelectorFrame {
    xpath: String,
    xpath_default_namespace: Option<String>,
    ns_snapshot: NamespaceContextSnapshot,
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
        ns_snapshot: NamespaceContextSnapshot,
    ) -> SchemaResult<Self> {
        let xpath = attrs
            .get_value_by_name(name_table, "xpath")
            .map(String::from)
            .unwrap_or_default();

        #[cfg(feature = "xsd11")]
        let xpath_default_namespace = attrs
            .get_value_by_name(name_table, "xpathDefaultNamespace")
            .map(String::from);
        #[cfg(not(feature = "xsd11"))]
        let xpath_default_namespace: Option<String> = None;

        let id = attrs
            .get_value_by_name(name_table, "id")
            .map(String::from);

        Ok(Self {
            xpath,
            xpath_default_namespace,
            ns_snapshot,
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
        #[cfg(feature = "xsd11")]
        if local_name == "xpathDefaultNamespace" {
            return true;
        }
        matches!(local_name, "xpath" | "id")
    }

    fn on_child_start(&mut self, _local_name: &str, _name_table: &NameTable) {}

    fn attach(&mut self, child: FrameResult) -> SchemaResult<()> {
        if let FrameResult::Annotation(ann) = child {
            self.annotation = Some(ann);
        }
        Ok(())
    }

    fn finish(self: Box<Self>) -> SchemaResult<FrameResult> {
        let annotation = merge_foreign_attributes(
            self.annotation,
            self.foreign_attributes,
            self.source.clone(),
        );
        Ok(FrameResult::Selector(SelectorResult {
            xpath: self.xpath,
            xpath_default_namespace: self.xpath_default_namespace,
            ns_snapshot: self.ns_snapshot,
            id: self.id,
            annotation,
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
    ns_snapshot: NamespaceContextSnapshot,
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
        ns_snapshot: NamespaceContextSnapshot,
    ) -> SchemaResult<Self> {
        let xpath = attrs
            .get_value_by_name(name_table, "xpath")
            .map(String::from)
            .unwrap_or_default();

        #[cfg(feature = "xsd11")]
        let xpath_default_namespace = attrs
            .get_value_by_name(name_table, "xpathDefaultNamespace")
            .map(String::from);
        #[cfg(not(feature = "xsd11"))]
        let xpath_default_namespace: Option<String> = None;

        let id = attrs
            .get_value_by_name(name_table, "id")
            .map(String::from);

        Ok(Self {
            xpath,
            xpath_default_namespace,
            ns_snapshot,
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
        #[cfg(feature = "xsd11")]
        if local_name == "xpathDefaultNamespace" {
            return true;
        }
        matches!(local_name, "xpath" | "id")
    }

    fn on_child_start(&mut self, _local_name: &str, _name_table: &NameTable) {}

    fn attach(&mut self, child: FrameResult) -> SchemaResult<()> {
        if let FrameResult::Annotation(ann) = child {
            self.annotation = Some(ann);
        }
        Ok(())
    }

    fn finish(self: Box<Self>) -> SchemaResult<FrameResult> {
        let annotation = merge_foreign_attributes(
            self.annotation,
            self.foreign_attributes,
            self.source.clone(),
        );
        Ok(FrameResult::Field(FieldResult {
            xpath: self.xpath,
            xpath_default_namespace: self.xpath_default_namespace,
            ns_snapshot: self.ns_snapshot,
            id: self.id,
            annotation,
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
        ns_snapshot: &NamespaceContextSnapshot,
    ) -> SchemaResult<Self> {
        let name = attrs
            .get_value_by_name(name_table, "name")
            .and_then(|s| name_table.get(s));

        let ref_name = attrs
            .get_value_by_name(name_table, "ref")
            .map(|s| parse_qname_ref(s, name_table, ns_snapshot))
            .transpose()?;

        let refer = if kind == IdentityKind::Keyref {
            attrs
                .get_value_by_name(name_table, "refer")
                .map(|s| parse_qname_ref(s, name_table, ns_snapshot))
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
                "src-identity-constraint",
                "Identity constraint requires 'name' attribute",
                None,
            )
        })?;

        let selector = self.selector.ok_or_else(|| {
            SchemaError::structural(
                "src-identity-constraint",
                "Identity constraint requires a selector",
                None,
            )
        })?;

        if self.fields.is_empty() {
            return Err(SchemaError::structural(
                "src-identity-constraint",
                "Identity constraint requires at least one field",
                None,
            ));
        }

        let annotation = merge_foreign_attributes(
            self.annotation,
            self.foreign_attributes,
            self.source.clone(),
        );
        Ok(FrameResult::Identity(IdentityResult {
            kind: self.kind,
            name,
            ref_name: self.ref_name,
            refer: self.refer,
            selector,
            fields: self.fields,
            id: self.id,
            annotation,
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

