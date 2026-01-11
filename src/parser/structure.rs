//! Structural validation rules for XSD elements
//!
//! This module provides validation functions for XSD structural constraints
//! that cannot be expressed in the frame's `allows()` method. These include:
//!
//! - **Mutually exclusive attributes**: e.g., `name` XOR `ref` on elements
//! - **Dependent attributes**: e.g., `keyref` requires `refer`
//! - **Prohibited combinations**: e.g., top-level element cannot have `minOccurs`
//! - **XSD version gates**: e.g., `xs:assert` requires XSD 1.1
//!
//! # XSD Structural Constraints
//!
//! Per W3C XSD 1.0 specification:
//!
//! | Element | Constraint |
//! |---------|------------|
//! | element (top) | Must have `name`, must not have `ref`/`minOccurs`/`maxOccurs` |
//! | element (local) | Must have exactly one of `name` or `ref` |
//! | attribute (top) | Must have `name`, must not have `ref`/`use` |
//! | attribute (local) | Must have exactly one of `name` or `ref` |
//! | simpleType | `name` required at top level, prohibited in local context |
//! | complexType | `name` required at top level, prohibited in local context |
//! | restriction | Must have `base` XOR inline type |
//! | extension | Must have `base` attribute |
//! | key/unique | Must have `name` attribute |
//! | keyref | Must have `name` and `refer` attributes |
//! | list | Must have `itemType` XOR inline simpleType |
//! | union | Must have `memberTypes` XOR inline simpleTypes |

use crate::error::{SchemaError, SchemaResult};
use crate::namespace::NameTable;
use crate::parser::attrs::AttributeMap;
use crate::parser::location::SourceRef;
use crate::schema::XsdVersion;

/// Validation context for structural checks
#[derive(Debug, Clone)]
pub struct ValidationContext {
    /// XSD version mode (1.0 or 1.1)
    pub xsd_version: XsdVersion,
    /// Whether this is a top-level (global) declaration
    pub is_top_level: bool,
    /// Source reference for error reporting
    pub source: Option<SourceRef>,
}

impl Default for ValidationContext {
    fn default() -> Self {
        Self {
            xsd_version: XsdVersion::V1_0,
            is_top_level: false,
            source: None,
        }
    }
}

impl ValidationContext {
    /// Create a new validation context
    pub fn new(xsd_version: XsdVersion, is_top_level: bool) -> Self {
        Self {
            xsd_version,
            is_top_level,
            source: None,
        }
    }

    /// Create a context with source reference
    pub fn with_source(mut self, source: Option<SourceRef>) -> Self {
        self.source = source;
        self
    }
}

// ============================================================================
// Element Declaration Validation
// ============================================================================

/// Validate element declaration structural constraints
///
/// Top-level elements:
/// - Must have `name` attribute
/// - Must NOT have `ref`, `minOccurs`, `maxOccurs`, or `form` attributes
///
/// Local elements:
/// - Must have exactly one of `name` OR `ref`
/// - If `ref` is present, type/default/fixed/nillable/block/final are prohibited
pub fn validate_element_structure(
    attrs: &AttributeMap,
    name_table: &NameTable,
    ctx: &ValidationContext,
) -> SchemaResult<()> {
    let has_name = attrs.get_value_by_name(name_table, "name").is_some();
    let has_ref = attrs.get_value_by_name(name_table, "ref").is_some();

    if ctx.is_top_level {
        // Top-level element validation
        if !has_name {
            return Err(SchemaError::structural(
                "sch-element-name-required",
                "Top-level element declaration must have 'name' attribute",
                None,
            ));
        }

        if has_ref {
            return Err(SchemaError::structural(
                "sch-element-top-ref",
                "Top-level element declaration cannot have 'ref' attribute",
                None,
            ));
        }

        // Prohibited attributes for top-level
        for prohibited in &["minOccurs", "maxOccurs", "form"] {
            if attrs.get_value_by_name(name_table, prohibited).is_some() {
                return Err(SchemaError::structural(
                    "sch-element-top-prohibited",
                    format!(
                        "Top-level element declaration cannot have '{}' attribute",
                        prohibited
                    ),
                    None,
                ));
            }
        }
    } else {
        // Local element validation
        if has_name && has_ref {
            return Err(SchemaError::structural(
                "sch-element-name-ref",
                "Local element cannot have both 'name' and 'ref' attributes",
                None,
            ));
        }

        if !has_name && !has_ref {
            return Err(SchemaError::structural(
                "sch-element-name-or-ref",
                "Local element must have either 'name' or 'ref' attribute",
                None,
            ));
        }

        // If ref is present, certain attributes are prohibited
        if has_ref {
            let ref_prohibited = [
                "type", "default", "fixed", "nillable", "block", "final",
            ];
            for prohibited in &ref_prohibited {
                if attrs.get_value_by_name(name_table, prohibited).is_some() {
                    return Err(SchemaError::structural(
                        "sch-element-ref-prohibited",
                        format!(
                            "Element reference cannot have '{}' attribute",
                            prohibited
                        ),
                        None,
                    ));
                }
            }
        }
    }

    // Validate default XOR fixed
    let has_default = attrs.get_value_by_name(name_table, "default").is_some();
    let has_fixed = attrs.get_value_by_name(name_table, "fixed").is_some();
    if has_default && has_fixed {
        return Err(SchemaError::structural(
            "sch-element-default-fixed",
            "Element cannot have both 'default' and 'fixed' attributes",
            None,
        ));
    }

    Ok(())
}

// ============================================================================
// Attribute Declaration Validation
// ============================================================================

/// Validate attribute declaration structural constraints
///
/// Top-level attributes:
/// - Must have `name` attribute
/// - Must NOT have `ref`, `use`, or `form` attributes
///
/// Local attributes:
/// - Must have exactly one of `name` OR `ref`
/// - If `ref` is present, type/default/fixed/form are prohibited
pub fn validate_attribute_structure(
    attrs: &AttributeMap,
    name_table: &NameTable,
    ctx: &ValidationContext,
) -> SchemaResult<()> {
    let has_name = attrs.get_value_by_name(name_table, "name").is_some();
    let has_ref = attrs.get_value_by_name(name_table, "ref").is_some();

    if ctx.is_top_level {
        // Top-level attribute validation
        if !has_name {
            return Err(SchemaError::structural(
                "sch-attribute-name-required",
                "Top-level attribute declaration must have 'name' attribute",
                None,
            ));
        }

        if has_ref {
            return Err(SchemaError::structural(
                "sch-attribute-top-ref",
                "Top-level attribute declaration cannot have 'ref' attribute",
                None,
            ));
        }

        // Prohibited attributes for top-level
        for prohibited in &["use", "form"] {
            if attrs.get_value_by_name(name_table, prohibited).is_some() {
                return Err(SchemaError::structural(
                    "sch-attribute-top-prohibited",
                    format!(
                        "Top-level attribute declaration cannot have '{}' attribute",
                        prohibited
                    ),
                    None,
                ));
            }
        }
    } else {
        // Local attribute validation
        if has_name && has_ref {
            return Err(SchemaError::structural(
                "sch-attribute-name-ref",
                "Local attribute cannot have both 'name' and 'ref' attributes",
                None,
            ));
        }

        if !has_name && !has_ref {
            return Err(SchemaError::structural(
                "sch-attribute-name-or-ref",
                "Local attribute must have either 'name' or 'ref' attribute",
                None,
            ));
        }

        // If ref is present, certain attributes are prohibited
        if has_ref {
            let ref_prohibited = ["type", "default", "fixed", "form"];
            for prohibited in &ref_prohibited {
                if attrs.get_value_by_name(name_table, prohibited).is_some() {
                    return Err(SchemaError::structural(
                        "sch-attribute-ref-prohibited",
                        format!(
                            "Attribute reference cannot have '{}' attribute",
                            prohibited
                        ),
                        None,
                    ));
                }
            }
        }
    }

    // Validate default XOR fixed
    let has_default = attrs.get_value_by_name(name_table, "default").is_some();
    let has_fixed = attrs.get_value_by_name(name_table, "fixed").is_some();
    if has_default && has_fixed {
        return Err(SchemaError::structural(
            "sch-attribute-default-fixed",
            "Attribute cannot have both 'default' and 'fixed' attributes",
            None,
        ));
    }

    // Validate use="prohibited" conflicts
    if let Some(use_val) = attrs.get_value_by_name(name_table, "use") {
        if use_val == "prohibited" {
            if has_default {
                return Err(SchemaError::structural(
                    "sch-attribute-prohibited-default",
                    "Prohibited attribute cannot have 'default' attribute",
                    None,
                ));
            }
            if has_fixed {
                return Err(SchemaError::structural(
                    "sch-attribute-prohibited-fixed",
                    "Prohibited attribute cannot have 'fixed' attribute",
                    None,
                ));
            }
        }
    }

    Ok(())
}

// ============================================================================
// Type Definition Validation
// ============================================================================

/// Validate simple type definition structure
///
/// - Top-level: `name` required
/// - Local (inline): `name` prohibited
/// - Must have exactly one of: restriction, list, or union child
pub fn validate_simple_type_structure(
    attrs: &AttributeMap,
    name_table: &NameTable,
    ctx: &ValidationContext,
) -> SchemaResult<()> {
    let has_name = attrs.get_value_by_name(name_table, "name").is_some();

    if ctx.is_top_level && !has_name {
        return Err(SchemaError::structural(
            "sch-simpletype-name-required",
            "Top-level simpleType must have 'name' attribute",
            None,
        ));
    }

    if !ctx.is_top_level && has_name {
        return Err(SchemaError::structural(
            "sch-simpletype-name-prohibited",
            "Inline simpleType cannot have 'name' attribute",
            None,
        ));
    }

    Ok(())
}

/// Validate complex type definition structure
///
/// - Top-level: `name` required
/// - Local (inline): `name` prohibited
pub fn validate_complex_type_structure(
    attrs: &AttributeMap,
    name_table: &NameTable,
    ctx: &ValidationContext,
) -> SchemaResult<()> {
    let has_name = attrs.get_value_by_name(name_table, "name").is_some();

    if ctx.is_top_level && !has_name {
        return Err(SchemaError::structural(
            "sch-complextype-name-required",
            "Top-level complexType must have 'name' attribute",
            None,
        ));
    }

    if !ctx.is_top_level && has_name {
        return Err(SchemaError::structural(
            "sch-complextype-name-prohibited",
            "Inline complexType cannot have 'name' attribute",
            None,
        ));
    }

    Ok(())
}

// ============================================================================
// Derivation Validation
// ============================================================================

/// Validate restriction element structure
///
/// - Must have `base` attribute XOR inline type definition
pub fn validate_restriction_structure(
    attrs: &AttributeMap,
    name_table: &NameTable,
    has_inline_type: bool,
) -> SchemaResult<()> {
    let has_base = attrs.get_value_by_name(name_table, "base").is_some();

    if has_base && has_inline_type {
        return Err(SchemaError::structural(
            "sch-restriction-base-type",
            "Restriction cannot have both 'base' attribute and inline type",
            None,
        ));
    }

    // Note: In simple type restriction, base is required unless inline type exists
    // This validation may need to be context-specific

    Ok(())
}

/// Validate extension element structure
///
/// - Must have `base` attribute
pub fn validate_extension_structure(
    attrs: &AttributeMap,
    name_table: &NameTable,
) -> SchemaResult<()> {
    let has_base = attrs.get_value_by_name(name_table, "base").is_some();

    if !has_base {
        return Err(SchemaError::structural(
            "sch-extension-base-required",
            "Extension must have 'base' attribute",
            None,
        ));
    }

    Ok(())
}

// ============================================================================
// List and Union Validation
// ============================================================================

/// Validate list element structure
///
/// - Must have `itemType` attribute XOR inline simpleType child
pub fn validate_list_structure(
    attrs: &AttributeMap,
    name_table: &NameTable,
    has_inline_type: bool,
) -> SchemaResult<()> {
    let has_item_type = attrs.get_value_by_name(name_table, "itemType").is_some();

    if has_item_type && has_inline_type {
        return Err(SchemaError::structural(
            "sch-list-itemtype-type",
            "List cannot have both 'itemType' attribute and inline simpleType",
            None,
        ));
    }

    if !has_item_type && !has_inline_type {
        return Err(SchemaError::structural(
            "sch-list-itemtype-required",
            "List must have either 'itemType' attribute or inline simpleType",
            None,
        ));
    }

    Ok(())
}

/// Validate union element structure
///
/// - Must have `memberTypes` attribute and/or inline simpleType children
pub fn validate_union_structure(
    attrs: &AttributeMap,
    name_table: &NameTable,
    has_inline_types: bool,
) -> SchemaResult<()> {
    let has_member_types = attrs.get_value_by_name(name_table, "memberTypes").is_some();

    if !has_member_types && !has_inline_types {
        return Err(SchemaError::structural(
            "sch-union-members-required",
            "Union must have 'memberTypes' attribute or inline simpleType children",
            None,
        ));
    }

    Ok(())
}

// ============================================================================
// Identity Constraint Validation
// ============================================================================

/// Validate key/unique element structure
///
/// - Must have `name` attribute
/// - Child requirements (selector/field) are validated when the frame finishes
pub fn validate_key_unique_structure(
    attrs: &AttributeMap,
    name_table: &NameTable,
) -> SchemaResult<()> {
    let has_name = attrs.get_value_by_name(name_table, "name").is_some();

    if !has_name {
        return Err(SchemaError::structural(
            "sch-identity-name-required",
            "Identity constraint (key/unique) must have 'name' attribute",
            None,
        ));
    }

    Ok(())
}

/// Validate keyref element structure
///
/// - Must have `name` attribute
/// - Must have `refer` attribute
/// - Child requirements (selector/field) are validated when the frame finishes
pub fn validate_keyref_structure(
    attrs: &AttributeMap,
    name_table: &NameTable,
) -> SchemaResult<()> {
    let has_name = attrs.get_value_by_name(name_table, "name").is_some();
    let has_refer = attrs.get_value_by_name(name_table, "refer").is_some();

    if !has_name {
        return Err(SchemaError::structural(
            "sch-keyref-name-required",
            "Keyref must have 'name' attribute",
            None,
        ));
    }

    if !has_refer {
        return Err(SchemaError::structural(
            "sch-keyref-refer-required",
            "Keyref must have 'refer' attribute",
            None,
        ));
    }

    Ok(())
}

// ============================================================================
// Group Validation
// ============================================================================

/// Validate model group (group) element structure
///
/// - Top-level: `name` required
/// - Reference: `ref` required, `name` prohibited
pub fn validate_group_structure(
    attrs: &AttributeMap,
    name_table: &NameTable,
    ctx: &ValidationContext,
) -> SchemaResult<()> {
    let has_name = attrs.get_value_by_name(name_table, "name").is_some();
    let has_ref = attrs.get_value_by_name(name_table, "ref").is_some();

    if ctx.is_top_level {
        if !has_name {
            return Err(SchemaError::structural(
                "sch-group-name-required",
                "Top-level group must have 'name' attribute",
                None,
            ));
        }
        if has_ref {
            return Err(SchemaError::structural(
                "sch-group-top-ref",
                "Top-level group cannot have 'ref' attribute",
                None,
            ));
        }
    } else {
        if has_name && has_ref {
            return Err(SchemaError::structural(
                "sch-group-name-ref",
                "Group cannot have both 'name' and 'ref' attributes",
                None,
            ));
        }
        if !has_name && !has_ref {
            return Err(SchemaError::structural(
                "sch-group-name-or-ref",
                "Group must have either 'name' or 'ref' attribute",
                None,
            ));
        }
    }

    Ok(())
}

/// Validate attribute group element structure
///
/// - Top-level: `name` required
/// - Reference: `ref` required, `name` prohibited
pub fn validate_attribute_group_structure(
    attrs: &AttributeMap,
    name_table: &NameTable,
    ctx: &ValidationContext,
) -> SchemaResult<()> {
    let has_name = attrs.get_value_by_name(name_table, "name").is_some();
    let has_ref = attrs.get_value_by_name(name_table, "ref").is_some();

    if ctx.is_top_level {
        if !has_name {
            return Err(SchemaError::structural(
                "sch-attrgroup-name-required",
                "Top-level attributeGroup must have 'name' attribute",
                None,
            ));
        }
        if has_ref {
            return Err(SchemaError::structural(
                "sch-attrgroup-top-ref",
                "Top-level attributeGroup cannot have 'ref' attribute",
                None,
            ));
        }
    } else {
        if has_name && has_ref {
            return Err(SchemaError::structural(
                "sch-attrgroup-name-ref",
                "AttributeGroup cannot have both 'name' and 'ref' attributes",
                None,
            ));
        }
        if !has_name && !has_ref {
            return Err(SchemaError::structural(
                "sch-attrgroup-name-or-ref",
                "AttributeGroup must have either 'name' or 'ref' attribute",
                None,
            ));
        }
    }

    Ok(())
}

// ============================================================================
// XSD 1.1 Feature Gates
// ============================================================================

/// XSD 1.1 element names that are not allowed in XSD 1.0 mode
pub const XSD_1_1_ELEMENTS: &[&str] = &[
    "assert",
    "assertion",
    "alternative",
    "openContent",
    "defaultOpenContent",
    "override",
    "explicitTimezone",
];

/// XSD 1.1 attribute names that are not allowed in XSD 1.0 mode
pub const XSD_1_1_ATTRIBUTES: &[&str] = &[
    "targetNamespace",      // on element/attribute (local)
    "notNamespace",         // on any/anyAttribute
    "notQName",             // on any/anyAttribute
    "inheritable",          // on attribute
    "defaultAttributesApply", // on complexType
    "xpathDefaultNamespace", // on schema/type definitions
];

/// Validate that an element is allowed in the current XSD version
pub fn validate_xsd_version_element(
    element_name: &str,
    ctx: &ValidationContext,
) -> SchemaResult<()> {
    if ctx.xsd_version == XsdVersion::V1_0 {
        if XSD_1_1_ELEMENTS.contains(&element_name) {
            return Err(SchemaError::feature(
                format!(
                    "Element '{}' requires XSD 1.1 but schema is in XSD 1.0 mode",
                    element_name
                ),
                None,
            ));
        }
    }
    Ok(())
}

/// Validate that an attribute is allowed in the current XSD version
pub fn validate_xsd_version_attribute(
    attr_name: &str,
    element_name: &str,
    ctx: &ValidationContext,
) -> SchemaResult<()> {
    if ctx.xsd_version == XsdVersion::V1_0 {
        // Some XSD 1.1 attributes are context-specific
        let is_xsd_1_1_attr = match (element_name, attr_name) {
            ("element", "targetNamespace") => true,
            ("attribute", "targetNamespace") => true,
            ("attribute", "inheritable") => true,
            ("complexType", "defaultAttributesApply") => true,
            ("any", "notNamespace") | ("any", "notQName") => true,
            ("anyAttribute", "notNamespace") | ("anyAttribute", "notQName") => true,
            ("schema", "xpathDefaultNamespace") => true,
            _ => XSD_1_1_ATTRIBUTES.contains(&attr_name),
        };

        if is_xsd_1_1_attr {
            return Err(SchemaError::feature(
                format!(
                    "Attribute '{}' on '{}' requires XSD 1.1 but schema is in XSD 1.0 mode",
                    attr_name, element_name
                ),
                None,
            ));
        }
    }
    Ok(())
}

// ============================================================================
// Notation Validation
// ============================================================================

/// Validate notation declaration structure
///
/// - Must have `name` attribute
/// - Must have `public` attribute (XSD 1.0) or `public` or `system` (XSD 1.1)
pub fn validate_notation_structure(
    attrs: &AttributeMap,
    name_table: &NameTable,
    ctx: &ValidationContext,
) -> SchemaResult<()> {
    let has_name = attrs.get_value_by_name(name_table, "name").is_some();
    let has_public = attrs.get_value_by_name(name_table, "public").is_some();
    let has_system = attrs.get_value_by_name(name_table, "system").is_some();

    if !has_name {
        return Err(SchemaError::structural(
            "sch-notation-name-required",
            "Notation must have 'name' attribute",
            None,
        ));
    }

    match ctx.xsd_version {
        XsdVersion::V1_0 => {
            if !has_public {
                return Err(SchemaError::structural(
                    "sch-notation-public-required",
                    "Notation must have 'public' attribute in XSD 1.0",
                    None,
                ));
            }
        }
        XsdVersion::V1_1 => {
            if !has_public && !has_system {
                return Err(SchemaError::structural(
                    "sch-notation-id-required",
                    "Notation must have 'public' or 'system' attribute in XSD 1.1",
                    None,
                ));
            }
        }
    }

    Ok(())
}

// ============================================================================
// Include/Import/Redefine Validation
// ============================================================================

/// Validate include directive structure
///
/// - Must have `schemaLocation` attribute
pub fn validate_include_structure(
    attrs: &AttributeMap,
    name_table: &NameTable,
) -> SchemaResult<()> {
    let has_location = attrs.get_value_by_name(name_table, "schemaLocation").is_some();

    if !has_location {
        return Err(SchemaError::structural(
            "sch-include-location-required",
            "Include must have 'schemaLocation' attribute",
            None,
        ));
    }

    Ok(())
}

/// Validate import directive structure
///
/// - `schemaLocation` is optional
/// - `namespace` is optional but recommended
pub fn validate_import_structure(
    _attrs: &AttributeMap,
    _name_table: &NameTable,
) -> SchemaResult<()> {
    // Import has no required attributes
    Ok(())
}

/// Validate redefine directive structure
///
/// - Must have `schemaLocation` attribute
pub fn validate_redefine_structure(
    attrs: &AttributeMap,
    name_table: &NameTable,
) -> SchemaResult<()> {
    let has_location = attrs.get_value_by_name(name_table, "schemaLocation").is_some();

    if !has_location {
        return Err(SchemaError::structural(
            "sch-redefine-location-required",
            "Redefine must have 'schemaLocation' attribute",
            None,
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::attrs::ParsedAttribute;

    fn make_attr_map(name_table: &mut NameTable, attrs: &[(&str, &str)]) -> AttributeMap {
        let parsed: Vec<ParsedAttribute> = attrs
            .iter()
            .map(|(name, value)| ParsedAttribute {
                namespace: None,
                local_name: name_table.add(name),
                prefix: None,
                value: value.to_string(),
                source: None,
            })
            .collect();
        AttributeMap::new(parsed)
    }

    #[test]
    fn test_element_top_level_valid() {
        let mut name_table = NameTable::new();
        let attrs = make_attr_map(&mut name_table, &[("name", "myElement")]);
        let ctx = ValidationContext::new(XsdVersion::V1_0, true);

        let result = validate_element_structure(&attrs, &name_table, &ctx);
        assert!(result.is_ok());
    }

    #[test]
    fn test_element_top_level_missing_name() {
        let mut name_table = NameTable::new();
        let attrs = make_attr_map(&mut name_table, &[("type", "xs:string")]);
        let ctx = ValidationContext::new(XsdVersion::V1_0, true);

        let result = validate_element_structure(&attrs, &name_table, &ctx);
        assert!(result.is_err());
    }

    #[test]
    fn test_element_top_level_has_ref() {
        let mut name_table = NameTable::new();
        let attrs = make_attr_map(&mut name_table, &[("name", "myElement"), ("ref", "other")]);
        let ctx = ValidationContext::new(XsdVersion::V1_0, true);

        let result = validate_element_structure(&attrs, &name_table, &ctx);
        assert!(result.is_err());
    }

    #[test]
    fn test_element_local_name_and_ref() {
        let mut name_table = NameTable::new();
        let attrs = make_attr_map(&mut name_table, &[("name", "myElement"), ("ref", "other")]);
        let ctx = ValidationContext::new(XsdVersion::V1_0, false);

        let result = validate_element_structure(&attrs, &name_table, &ctx);
        assert!(result.is_err());
    }

    #[test]
    fn test_element_default_and_fixed() {
        let mut name_table = NameTable::new();
        let attrs = make_attr_map(
            &mut name_table,
            &[("name", "myElement"), ("default", "a"), ("fixed", "b")],
        );
        let ctx = ValidationContext::new(XsdVersion::V1_0, true);

        let result = validate_element_structure(&attrs, &name_table, &ctx);
        assert!(result.is_err());
    }

    #[test]
    fn test_attribute_prohibited_with_default() {
        let mut name_table = NameTable::new();
        let attrs = make_attr_map(
            &mut name_table,
            &[("ref", "myAttr"), ("use", "prohibited"), ("default", "x")],
        );
        let ctx = ValidationContext::new(XsdVersion::V1_0, false);

        let result = validate_attribute_structure(&attrs, &name_table, &ctx);
        assert!(result.is_err());
    }

    #[test]
    fn test_xsd_1_1_element_in_1_0_mode() {
        let ctx = ValidationContext::new(XsdVersion::V1_0, false);
        let result = validate_xsd_version_element("assert", &ctx);
        assert!(result.is_err());
    }

    #[test]
    fn test_xsd_1_1_element_in_1_1_mode() {
        let ctx = ValidationContext::new(XsdVersion::V1_1, false);
        let result = validate_xsd_version_element("assert", &ctx);
        assert!(result.is_ok());
    }

    #[test]
    fn test_keyref_requires_refer() {
        let mut name_table = NameTable::new();
        let attrs = make_attr_map(&mut name_table, &[("name", "myKeyRef")]);

        let result = validate_keyref_structure(&attrs, &name_table);
        assert!(result.is_err());
    }

    #[test]
    fn test_keyref_with_refer() {
        let mut name_table = NameTable::new();
        let attrs = make_attr_map(&mut name_table, &[("name", "myKeyRef"), ("refer", "myKey")]);

        let result = validate_keyref_structure(&attrs, &name_table);
        assert!(result.is_ok());
    }

    #[test]
    fn test_list_itemtype_and_inline() {
        let mut name_table = NameTable::new();
        let attrs = make_attr_map(&mut name_table, &[("itemType", "xs:string")]);

        // Has both itemType and inline type - should fail
        let result = validate_list_structure(&attrs, &name_table, true);
        assert!(result.is_err());
    }

    #[test]
    fn test_list_neither_itemtype_nor_inline() {
        let mut name_table = NameTable::new();
        let attrs = make_attr_map(&mut name_table, &[]);

        let result = validate_list_structure(&attrs, &name_table, false);
        assert!(result.is_err());
    }

    #[test]
    fn test_extension_requires_base() {
        let mut name_table = NameTable::new();
        let attrs = make_attr_map(&mut name_table, &[]);

        let result = validate_extension_structure(&attrs, &name_table);
        assert!(result.is_err());
    }

    #[test]
    fn test_notation_requires_public_in_1_0() {
        let mut name_table = NameTable::new();
        let attrs = make_attr_map(&mut name_table, &[("name", "myNotation"), ("system", "foo")]);
        let ctx = ValidationContext::new(XsdVersion::V1_0, true);

        let result = validate_notation_structure(&attrs, &name_table, &ctx);
        assert!(result.is_err());
    }

    #[test]
    fn test_notation_system_ok_in_1_1() {
        let mut name_table = NameTable::new();
        let attrs = make_attr_map(&mut name_table, &[("name", "myNotation"), ("system", "foo")]);
        let ctx = ValidationContext::new(XsdVersion::V1_1, true);

        let result = validate_notation_structure(&attrs, &name_table, &ctx);
        assert!(result.is_ok());
    }
}
