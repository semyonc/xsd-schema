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
                    "ct-props-correct",
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
                    "ct-props-correct",
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
            "ct-props-correct",
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

#[cfg(feature = "xsd11")]
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

#[cfg(feature = "xsd11")]
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
                    "sch-props-correct",
                    format!("Invalid derivation method: '{}'", token),
                    None,
                ));
            }
        }
    }

    Ok(set)
}

/// Parse a QName reference with namespace resolution
///
/// Resolves the prefix to a namespace URI using the provided namespace context snapshot.
fn parse_qname_ref(
    value: &str,
    name_table: &NameTable,
    ns_snapshot: &NamespaceContextSnapshot,
) -> SchemaResult<QNameRef> {
    let (local, prefix) = if let Some(pos) = value.find(':') {
        let prefix = &value[..pos];
        let local = &value[pos + 1..];
        (local, Some(prefix))
    } else {
        (value, None)
    };

    // Validate QName lexical form: both prefix and local part must be non-empty
    // NCNames (no colons allowed in either part).
    if local.is_empty() || local.contains(':') {
        return Err(SchemaError::structural(
            "src-resolve",
            format!("Invalid QName: '{}'", value),
            None,
        ));
    }
    if let Some(p) = prefix {
        if p.is_empty() {
            return Err(SchemaError::structural(
                "src-resolve",
                format!("Invalid QName: '{}'", value),
                None,
            ));
        }
    }

    // Use add() (intern-or-get) rather than get() so that forward-referenced
    // names are interned immediately. Resolution of whether the name actually
    // exists happens later in the reference-resolution phase.
    let local_name = name_table.add(local);

    let prefix_id = prefix.and_then(|p| name_table.get(p));

    // Resolve namespace immediately using the snapshot
    let namespace = if let Some(pid) = prefix_id {
        ns_snapshot.resolve_prefix(pid)
    } else {
        // For unprefixed QNames in XSD attribute values (type, ref, base, etc.),
        // use the default namespace if one is declared. Per XML namespace rules,
        // an unprefixed QName resolves using the default namespace in scope.
        ns_snapshot.default_namespace()
    };

    Ok(QNameRef {
        prefix: prefix_id,
        local_name,
        namespace,
    })
}

/// Parse a list of QName references with namespace resolution
fn parse_qname_list(
    value: &str,
    name_table: &NameTable,
    ns_snapshot: &NamespaceContextSnapshot,
) -> SchemaResult<Vec<QNameRef>> {
    value
        .split_whitespace()
        .map(|s| parse_qname_ref(s, name_table, ns_snapshot))
        .collect()
}

/// Parse namespace constraint for wildcards
fn parse_namespace_constraint(
    value: Option<&str>,
    name_table: &NameTable,
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
            let namespaces: Vec<NamespaceToken> = value
                .split_whitespace()
                .map(|s| match s {
                    "##targetNamespace" => NamespaceToken::TargetNamespace,
                    "##local" => NamespaceToken::Local,
                    _ => NamespaceToken::Uri(name_table.add(s)),
                })
                .collect();
            Ok(WildcardNamespace::List(namespaces))
        }
    }
}

/// Parse notNamespace attribute (XSD 1.1)
#[cfg(feature = "xsd11")]
pub(crate) fn parse_not_namespace(
    value: Option<&str>,
    name_table: &NameTable,
) -> Vec<NamespaceToken> {
    let Some(value) = value else {
        return Vec::new();
    };
    value
        .split_whitespace()
        .map(|s| match s {
            "##targetNamespace" => NamespaceToken::TargetNamespace,
            "##local" => NamespaceToken::Local,
            _ => NamespaceToken::Uri(name_table.add(s)),
        })
        .collect()
}

/// Parse notQName attribute (XSD 1.1)
#[cfg(feature = "xsd11")]
pub(crate) fn parse_not_qname(
    value: Option<&str>,
    name_table: &NameTable,
    ns_snapshot: &NamespaceContextSnapshot,
    is_element_wildcard: bool,
) -> SchemaResult<Vec<NotQNameItem>> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let mut items = Vec::new();
    for s in value.split_whitespace() {
        match s {
            "##defined" => items.push(NotQNameItem::Defined),
            "##definedSibling" => {
                if !is_element_wildcard {
                    return Err(SchemaError::structural(
                        "src-wildcard",
                        "##definedSibling is not allowed on xs:anyAttribute".to_string(),
                        None,
                    ));
                }
                items.push(NotQNameItem::DefinedSibling);
            }
            _ => {
                // Parse as QName
                let (local, prefix) = if let Some(pos) = s.find(':') {
                    (&s[pos + 1..], Some(&s[..pos]))
                } else {
                    (s, None)
                };
                let local_name = name_table.add(local);
                let namespace = if let Some(p) = prefix {
                    let prefix_id = name_table.add(p);
                    ns_snapshot.resolve_prefix(prefix_id)
                } else {
                    // Unprefixed: use default namespace if declared
                    ns_snapshot.default_namespace()
                };
                items.push(NotQNameItem::QName { namespace, local_name });
            }
        }
    }
    Ok(items)
}

/// Parse an XSD nonNegativeInteger/positiveInteger facet value.
///
/// Applies whitespace collapse (the schema-for-schemas types these attributes
/// as `nonNegativeInteger`/`positiveInteger`) and strips an optional leading `+`.
fn parse_nonneg_integer<T: std::str::FromStr>(value: &str, facet_name: &str) -> SchemaResult<T> {
    let trimmed = value.trim();
    let text = trimmed.strip_prefix('+').unwrap_or(trimmed);
    text.parse().map_err(|_| {
        SchemaError::structural(
            "st-props-correct",
            format!("Invalid {} value '{}': must be a nonNegativeInteger", facet_name, value),
            None,
        )
    })
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
            facets.set_min_length(parse_nonneg_integer(&facet.value, "minLength")?, fixed, facet.source);
        }
        FacetKind::MaxLength => {
            facets.set_max_length(parse_nonneg_integer(&facet.value, "maxLength")?, fixed, facet.source);
        }
        FacetKind::Length => {
            facets.set_length(parse_nonneg_integer(&facet.value, "length")?, fixed, facet.source);
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
            let v: u32 = parse_nonneg_integer(&facet.value, "totalDigits")?;
            if v == 0 {
                return Err(SchemaError::structural(
                    "st-props-correct",
                    "Invalid totalDigits value '0': must be a positiveInteger (> 0)",
                    None,
                ));
            }
            facets.set_total_digits(v, fixed, facet.source);
        }
        FacetKind::FractionDigits => {
            facets.set_fraction_digits(parse_nonneg_integer(&facet.value, "fractionDigits")?, fixed, facet.source);
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
            facets.add_assertion(
                facet.value,
                facet.xpath_default_namespace,
                facet.ns_snapshot.unwrap_or_default(),
                facet.source,
            );
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

