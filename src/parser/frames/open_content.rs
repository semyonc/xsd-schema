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
        let annotation = merge_foreign_attributes(
            self.annotation,
            self.foreign_attributes,
            self.source.clone(),
        );
        Ok(FrameResult::OpenContent(OpenContentResult {
            mode: self.mode,
            wildcard: self.wildcard,
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
        let annotation = merge_foreign_attributes(
            self.annotation,
            self.foreign_attributes,
            self.source.clone(),
        );
        Ok(FrameResult::DefaultOpenContent(DefaultOpenContentResult {
            mode: self.mode,
            applies_to_empty: self.applies_to_empty,
            wildcard: self.wildcard,
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

