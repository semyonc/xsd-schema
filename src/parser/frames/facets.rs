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
        let annotation = merge_foreign_attributes(
            self.annotation,
            self.foreign_attributes,
            self.source.clone(),
        );
        Ok(FrameResult::Facet(FacetResult {
            kind: self.kind,
            value: self.value,
            fixed: self.fixed,
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

