//! XSD constraining facets
//!
//! This module implements the XSD facet system for constraining simple types.
//! Facets can restrict length, numeric range, pattern matching, enumeration, and whitespace.
//!
//! ## XSD Facet Categories
//!
//! - **Length facets**: length, minLength, maxLength (for string, binary, list types)
//! - **Numeric precision facets**: totalDigits, fractionDigits (for decimal types)
//! - **Bound facets**: minInclusive, maxInclusive, minExclusive, maxExclusive
//! - **String facets**: pattern, enumeration, whitespace
//! - **XSD 1.1 facets**: explicitTimezone, assertion
//!
//! ## Facet Inheritance
//!
//! When deriving a simple type by restriction:
//! - Derived facets must be more restrictive than base facets
//! - Fixed facets cannot be overridden with different values
//! - Patterns are cumulative (ANDed together)
//! - Enumerations must be subsets of base enumerations

use crate::error::{FacetError, FacetResult};
use crate::namespace::context::NamespaceContextSnapshot;
use crate::parser::location::SourceRef;
#[cfg(feature = "xsd11")]
use crate::regex_convert::rewrite_xsd10_category_escapes;
use crate::regex_convert::validate_xml_pattern_syntax;
#[cfg(not(feature = "xsd11"))]
use crate::regex_convert::{convert_xml_pattern, ConvertOptions};
use crate::schema::model::XsdVersion;
#[cfg(not(feature = "xsd11"))]
use regex::Regex;
use std::collections::HashSet;

#[cfg(feature = "xsd11")]
use std::sync::Arc;

use super::XmlTypeCode;

/// Fixed vs default facet values
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FacetFixed {
    /// Value can be further restricted
    #[default]
    Default,
    /// Value cannot be changed by derived types
    Fixed,
}

/// Whitespace handling mode
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WhitespaceMode {
    /// Preserve all whitespace
    Preserve,
    /// Replace tabs/newlines with spaces
    Replace,
    /// Collapse consecutive whitespace to single space, trim
    #[default]
    Collapse,
}

/// Length facet (exact length constraint)
#[derive(Debug, Clone)]
pub struct LengthFacet {
    pub value: u64,
    pub fixed: FacetFixed,
    pub source: Option<SourceRef>,
}

/// MinLength facet
#[derive(Debug, Clone)]
pub struct MinLengthFacet {
    pub value: u64,
    pub fixed: FacetFixed,
    pub source: Option<SourceRef>,
}

/// MaxLength facet
#[derive(Debug, Clone)]
pub struct MaxLengthFacet {
    pub value: u64,
    pub fixed: FacetFixed,
    pub source: Option<SourceRef>,
}

/// Pattern facet (regex constraint)
#[derive(Debug, Clone)]
pub struct PatternFacet {
    /// The pattern string (XSD regex syntax)
    pub value: String,
    /// Compiled regex for efficient matching
    #[cfg(not(feature = "xsd11"))]
    compiled: Option<Regex>,
    #[cfg(feature = "xsd11")]
    compiled: Option<Arc<regexml::Regex>>,
    pub source: Option<SourceRef>,
}

impl PatternFacet {
    /// Create a new pattern facet from an XSD pattern string.
    ///
    /// The pattern is validated and compiled using the appropriate backend
    /// (XSD 1.0: `regex` via `convert_xml_pattern`; XSD 1.1: `regexml` after
    /// a `\p{X}` rewrite if `xsd_version == V1_0`). Returns an error if the
    /// pattern is invalid.
    ///
    /// `xsd_version` controls the `\p{X}` category escape semantics: under
    /// `V1_0` recognized general-category names are expanded to Unicode 3.0
    /// ranges; `V1_1` passes them through to the backend unchanged.
    pub fn new(
        value: String,
        source: Option<SourceRef>,
        xsd_version: XsdVersion,
    ) -> FacetResult<Self> {
        let mut facet = Self::new_unchecked(value, source);
        facet.compile(xsd_version)?;
        Ok(facet)
    }

    /// Create a pattern facet without compiling (for deferred compilation)
    pub fn new_unchecked(value: String, source: Option<SourceRef>) -> Self {
        Self {
            value,
            compiled: None,
            source,
        }
    }

    /// Compile the pattern if not already compiled
    #[cfg(not(feature = "xsd11"))]
    pub fn compile(&mut self, xsd_version: XsdVersion) -> FacetResult<()> {
        if self.compiled.is_none() {
            if xsd_version == XsdVersion::V1_0 {
                validate_xml_pattern_syntax(&self.value).map_err(|message| {
                    FacetError::InvalidPattern {
                        pattern: self.value.clone(),
                        message,
                    }
                })?;
            }
            let opts = match xsd_version {
                XsdVersion::V1_0 => ConvertOptions::xsd_v1_0(),
                XsdVersion::V1_1 => ConvertOptions::xsd(),
            };
            let rust_pattern = convert_xml_pattern(&self.value, opts);
            let compiled = Regex::new(&rust_pattern).map_err(|e| FacetError::InvalidPattern {
                pattern: self.value.clone(),
                message: e.to_string(),
            })?;
            self.compiled = Some(compiled);
        }
        Ok(())
    }

    /// Compile the pattern if not already compiled
    #[cfg(feature = "xsd11")]
    pub fn compile(&mut self, xsd_version: XsdVersion) -> FacetResult<()> {
        if self.compiled.is_none() {
            if xsd_version == XsdVersion::V1_0 {
                validate_xml_pattern_syntax(&self.value).map_err(|message| {
                    FacetError::InvalidPattern {
                        pattern: self.value.clone(),
                        message,
                    }
                })?;
            }
            // Validate against XSD regex rules first. Intentional two-pass:
            // xsd() rejects XPath-only constructs (e.g. `^$`, backrefs, `(?:...)`)
            // that xpath() would otherwise accept. For XSD 1.1, an unrecognized
            // `\p{IsX}` block name is treated as matching every character (W3C
            // bug 13670 / XSD 1.1 Datatypes §G.4.2.3); the rewrite happens here
            // because regexml 0.2 does not yet honour `allow_unknown_block_names`.
            let xsd_validated: std::borrow::Cow<'_, str> = match xsd_version {
                XsdVersion::V1_0 => {
                    regexml::Regex::xsd(&self.value, "").map_err(|e| {
                        FacetError::InvalidPattern {
                            pattern: self.value.clone(),
                            message: format!("{:?}", e),
                        }
                    })?;
                    std::borrow::Cow::Borrowed(&self.value)
                }
                XsdVersion::V1_1 => validate_xsd11_pattern_with_block_fallback(&self.value)?,
            };
            // Under XSD 1.0 the \p{X} rewrite produces a new String; under 1.1
            // we use the (possibly block-rewritten) validated value.
            let pinned: std::borrow::Cow<'_, str> = match xsd_version {
                XsdVersion::V1_0 => {
                    std::borrow::Cow::Owned(rewrite_xsd10_category_escapes(&self.value))
                }
                XsdVersion::V1_1 => xsd_validated,
            };
            // Compile with explicit anchoring for full-string matching
            let anchored = format!("^(?:{})$", pinned);
            let compiled =
                regexml::Regex::xpath(&anchored, "").map_err(|e| FacetError::InvalidPattern {
                    pattern: self.value.clone(),
                    message: format!("{:?}", e),
                })?;
            self.compiled = Some(Arc::new(compiled));
        }
        Ok(())
    }

    /// Test if a value matches this pattern
    #[cfg(not(feature = "xsd11"))]
    pub fn matches(&self, value: &str) -> bool {
        match &self.compiled {
            Some(regex) => regex.is_match(value),
            None => {
                // Defensive fallback: compile on-the-fly using XSD 1.1 defaults.
                // Reached only if a facet was never compiled via `compile_patterns`.
                if let Ok(rust_pattern) = std::panic::catch_unwind(|| {
                    convert_xml_pattern(&self.value, ConvertOptions::xsd())
                }) {
                    if let Ok(regex) = Regex::new(&rust_pattern) {
                        return regex.is_match(value);
                    }
                }
                false
            }
        }
    }

    /// Test if a value matches this pattern
    #[cfg(feature = "xsd11")]
    pub fn matches(&self, value: &str) -> bool {
        match &self.compiled {
            Some(regex) => regex.is_match(value),
            None => {
                // Defensive fallback: validate and compile on-the-fly with XSD 1.1
                // defaults. Reached only if a facet was never compiled via
                // `compile_patterns`.
                if let Ok(rewritten) = validate_xsd11_pattern_with_block_fallback(&self.value) {
                    let anchored = format!("^(?:{})$", rewritten);
                    if let Ok(regex) = regexml::Regex::xpath(&anchored, "") {
                        return regex.is_match(value);
                    }
                }
                false
            }
        }
    }
}

/// Validate an XSD 1.1 pattern with regexml's strict XSD parser, rewriting any
/// unknown `\p{IsX}` / `\P{IsX}` block names to a match-everything expression
/// per W3C bug 13670 / XSD 1.1 Datatypes §G.4.2.3 (unrecognized block names are
/// allowed and match every character). Returns the (possibly rewritten) pattern
/// or a `FacetError` if a non-block-name error remains after up to 16 rewrites.
#[cfg(feature = "xsd11")]
fn validate_xsd11_pattern_with_block_fallback(
    value: &str,
) -> FacetResult<std::borrow::Cow<'_, str>> {
    let mut current: std::borrow::Cow<'_, str> = std::borrow::Cow::Borrowed(value);
    for _ in 0..16 {
        let err = match regexml::Regex::xsd(&current, "") {
            Ok(_) => return Ok(current),
            Err(e) => format!("{:?}", e),
        };
        const PREFIX: &str = "Unknown Unicode block: ";
        let Some(start) = err.find(PREFIX) else {
            return Err(FacetError::InvalidPattern {
                pattern: value.to_string(),
                message: err,
            });
        };
        let after = &err[start + PREFIX.len()..];
        let end = after
            .find(|c: char| !c.is_alphanumeric() && c != '-' && c != '_' && c != ' ')
            .unwrap_or(after.len());
        let block = after[..end].trim();
        if block.is_empty() {
            return Err(FacetError::InvalidPattern {
                pattern: value.to_string(),
                message: err,
            });
        }
        match rewrite_pattern_isblock_token(&current, block) {
            Some(rewritten) => current = std::borrow::Cow::Owned(rewritten),
            None => {
                return Err(FacetError::InvalidPattern {
                    pattern: value.to_string(),
                    message: err,
                });
            }
        }
    }
    // Loop bound exceeded; surface the final error if any.
    if let Err(e) = regexml::Regex::xsd(&current, "") {
        return Err(FacetError::InvalidPattern {
            pattern: value.to_string(),
            message: format!("{:?}", e),
        });
    }
    Ok(current)
}

/// Rewrite every `\p{Is<block>}` / `\P{Is<block>}` token in `pattern` to a
/// match-everything expression. Uses `[\s\S]` at atom position and `\s\S`
/// inside a character class so the rewritten token is structurally valid in
/// either context. Returns `None` if no rewrite happened.
#[cfg(feature = "xsd11")]
fn rewrite_pattern_isblock_token(pattern: &str, block_name: &str) -> Option<String> {
    let inner_p = format!("p{{Is{}}}", block_name);
    let inner_cap = format!("P{{Is{}}}", block_name);
    let token_len = 1 + inner_p.len();
    if !pattern.contains(&format!("\\{}", inner_p))
        && !pattern.contains(&format!("\\{}", inner_cap))
    {
        return None;
    }
    let bytes = pattern.as_bytes();
    let mut result = String::with_capacity(pattern.len());
    let mut i = 0;
    let mut in_class = false;
    let mut found = false;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + token_len <= bytes.len() {
            // Tokens are pure ASCII, so byte-slice comparison is safe here.
            let candidate = &pattern[i + 1..i + token_len];
            if candidate == inner_p || candidate == inner_cap {
                if in_class {
                    result.push_str("\\s\\S");
                } else {
                    result.push_str("[\\s\\S]");
                }
                i += token_len;
                found = true;
                continue;
            }
        }
        let c = bytes[i];
        if c == b'\\' && i + 1 < bytes.len() {
            let next_len = pattern[i + 1..]
                .chars()
                .next()
                .map(|ch| ch.len_utf8())
                .unwrap_or(1);
            result.push_str(&pattern[i..i + 1 + next_len]);
            i += 1 + next_len;
            continue;
        }
        if c == b'[' {
            in_class = true;
            result.push('[');
            i += 1;
            continue;
        }
        if c == b']' {
            in_class = false;
            result.push(']');
            i += 1;
            continue;
        }
        let next_len = pattern[i..]
            .chars()
            .next()
            .map(|ch| ch.len_utf8())
            .unwrap_or(1);
        result.push_str(&pattern[i..i + next_len]);
        i += next_len;
    }
    if found {
        Some(result)
    } else {
        None
    }
}

/// Enumeration facet (allowed values)
#[derive(Debug, Clone)]
pub struct EnumerationFacet {
    /// Set of allowed values (as strings)
    pub values: HashSet<String>,
    pub source: Option<SourceRef>,
}

/// Whitespace facet
#[derive(Debug, Clone)]
pub struct WhitespaceFacet {
    pub value: WhitespaceMode,
    pub fixed: FacetFixed,
    pub source: Option<SourceRef>,
}

/// MinInclusive facet (value >= bound)
#[derive(Debug, Clone)]
pub struct MinInclusiveFacet {
    /// The bound as a string (type-specific interpretation during validation)
    pub value: String,
    pub fixed: FacetFixed,
    pub source: Option<SourceRef>,
}

/// MaxInclusive facet (value <= bound)
#[derive(Debug, Clone)]
pub struct MaxInclusiveFacet {
    pub value: String,
    pub fixed: FacetFixed,
    pub source: Option<SourceRef>,
}

/// MinExclusive facet (value > bound)
#[derive(Debug, Clone)]
pub struct MinExclusiveFacet {
    pub value: String,
    pub fixed: FacetFixed,
    pub source: Option<SourceRef>,
}

/// MaxExclusive facet (value < bound)
#[derive(Debug, Clone)]
pub struct MaxExclusiveFacet {
    pub value: String,
    pub fixed: FacetFixed,
    pub source: Option<SourceRef>,
}

/// TotalDigits facet (for decimal types)
#[derive(Debug, Clone)]
pub struct TotalDigitsFacet {
    pub value: u32,
    pub fixed: FacetFixed,
    pub source: Option<SourceRef>,
}

/// FractionDigits facet (decimal places)
#[derive(Debug, Clone)]
pub struct FractionDigitsFacet {
    pub value: u32,
    pub fixed: FacetFixed,
    pub source: Option<SourceRef>,
}

/// XSD 1.1: Assertion facet (XPath constraint on simple type values)
#[derive(Debug, Clone)]
pub struct AssertionFacet {
    /// XPath 2.0 test expression
    pub test: String,
    /// Raw xpathDefaultNamespace attribute (resolved at evaluation time)
    pub xpath_default_namespace: Option<String>,
    /// Namespace bindings snapshot at parse time (for prefix resolution in XPath)
    pub ns_snapshot: NamespaceContextSnapshot,
    pub source: Option<SourceRef>,
}

/// XSD 1.1: ExplicitTimezone facet
/// TODO: XSD 1.1 - Implement explicitTimezone constraint
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExplicitTimezone {
    Required,
    Prohibited,
    Optional,
}

/// XSD 1.1: ExplicitTimezone facet data
#[derive(Debug, Clone)]
pub struct ExplicitTimezoneFacet {
    pub value: ExplicitTimezone,
    pub fixed: FacetFixed,
    pub source: Option<SourceRef>,
}

/// Complete set of facets for a simple type
///
/// A FacetSet collects all constraining facets that apply to a simple type.
/// Facets are accumulated during type derivation.
#[derive(Debug, Clone, Default)]
pub struct FacetSet {
    // String length facets
    pub length: Option<LengthFacet>,
    pub min_length: Option<MinLengthFacet>,
    pub max_length: Option<MaxLengthFacet>,

    // Pattern facets (multiple patterns are ANDed)
    pub patterns: Vec<PatternFacet>,

    // Enumeration (allowed values). The `Option` is only the presence flag;
    // multi-valued semantics live inside `EnumerationFacet::values` (HashSet),
    // so enumeration is exempt from st-props-correct.1 "no duplicate facet" (§3.16.2).
    pub enumeration: Option<EnumerationFacet>,

    // Whitespace handling
    pub whitespace: Option<WhitespaceFacet>,

    // Numeric range facets
    pub min_inclusive: Option<MinInclusiveFacet>,
    pub max_inclusive: Option<MaxInclusiveFacet>,
    pub min_exclusive: Option<MinExclusiveFacet>,
    pub max_exclusive: Option<MaxExclusiveFacet>,

    // Decimal precision facets
    pub total_digits: Option<TotalDigitsFacet>,
    pub fraction_digits: Option<FractionDigitsFacet>,

    // XSD 1.1 facets
    // TODO: XSD 1.1 - These are parsed but not enforced in 1.0 mode
    pub assertions: Vec<AssertionFacet>,
    pub explicit_timezone: Option<ExplicitTimezoneFacet>,
}

impl FacetSet {
    /// Create a new empty facet set
    pub fn new() -> Self {
        Self::default()
    }

    /// Check if the facet set is empty (no facets defined)
    pub fn is_empty(&self) -> bool {
        self.length.is_none()
            && self.min_length.is_none()
            && self.max_length.is_none()
            && self.patterns.is_empty()
            && self.enumeration.is_none()
            && self.whitespace.is_none()
            && self.min_inclusive.is_none()
            && self.max_inclusive.is_none()
            && self.min_exclusive.is_none()
            && self.max_exclusive.is_none()
            && self.total_digits.is_none()
            && self.fraction_digits.is_none()
            && self.assertions.is_empty()
            && self.explicit_timezone.is_none()
    }

    /// Set length facet
    pub fn set_length(&mut self, value: u64, fixed: FacetFixed, source: Option<SourceRef>) {
        self.length = Some(LengthFacet {
            value,
            fixed,
            source,
        });
    }

    /// Set minLength facet
    pub fn set_min_length(&mut self, value: u64, fixed: FacetFixed, source: Option<SourceRef>) {
        self.min_length = Some(MinLengthFacet {
            value,
            fixed,
            source,
        });
    }

    /// Set maxLength facet
    pub fn set_max_length(&mut self, value: u64, fixed: FacetFixed, source: Option<SourceRef>) {
        self.max_length = Some(MaxLengthFacet {
            value,
            fixed,
            source,
        });
    }

    /// Add a pattern facet (compiles the pattern)
    pub fn add_pattern(
        &mut self,
        value: String,
        source: Option<SourceRef>,
        xsd_version: XsdVersion,
    ) -> FacetResult<()> {
        let pattern = PatternFacet::new(value, source, xsd_version)?;
        self.patterns.push(pattern);
        Ok(())
    }

    /// Add a pattern facet without compiling (for deferred validation)
    pub fn add_pattern_unchecked(&mut self, value: String, source: Option<SourceRef>) {
        self.patterns
            .push(PatternFacet::new_unchecked(value, source));
    }

    /// Compile all uncompiled patterns. Returns the first error encountered.
    ///
    /// `xsd_version` selects the Unicode-category semantics for `\p{X}`: V1_0
    /// pins to Unicode 3.0; V1_1 passes through to the backend.
    pub fn compile_patterns(&mut self, xsd_version: XsdVersion) -> FacetResult<()> {
        for pattern in &mut self.patterns {
            pattern.compile(xsd_version)?;
        }
        Ok(())
    }

    /// Add an enumeration value
    pub fn add_enumeration(&mut self, value: String, source: Option<SourceRef>) {
        let enumeration = self.enumeration.get_or_insert_with(|| EnumerationFacet {
            values: HashSet::new(),
            source: source.clone(),
        });
        enumeration.values.insert(value);
    }

    /// Set whitespace facet
    pub fn set_whitespace(
        &mut self,
        value: WhitespaceMode,
        fixed: FacetFixed,
        source: Option<SourceRef>,
    ) {
        self.whitespace = Some(WhitespaceFacet {
            value,
            fixed,
            source,
        });
    }

    /// Set minInclusive facet
    pub fn set_min_inclusive(
        &mut self,
        value: String,
        fixed: FacetFixed,
        source: Option<SourceRef>,
    ) {
        self.min_inclusive = Some(MinInclusiveFacet {
            value,
            fixed,
            source,
        });
    }

    /// Set maxInclusive facet
    pub fn set_max_inclusive(
        &mut self,
        value: String,
        fixed: FacetFixed,
        source: Option<SourceRef>,
    ) {
        self.max_inclusive = Some(MaxInclusiveFacet {
            value,
            fixed,
            source,
        });
    }

    /// Set minExclusive facet
    pub fn set_min_exclusive(
        &mut self,
        value: String,
        fixed: FacetFixed,
        source: Option<SourceRef>,
    ) {
        self.min_exclusive = Some(MinExclusiveFacet {
            value,
            fixed,
            source,
        });
    }

    /// Set maxExclusive facet
    pub fn set_max_exclusive(
        &mut self,
        value: String,
        fixed: FacetFixed,
        source: Option<SourceRef>,
    ) {
        self.max_exclusive = Some(MaxExclusiveFacet {
            value,
            fixed,
            source,
        });
    }

    /// Set totalDigits facet
    pub fn set_total_digits(&mut self, value: u32, fixed: FacetFixed, source: Option<SourceRef>) {
        self.total_digits = Some(TotalDigitsFacet {
            value,
            fixed,
            source,
        });
    }

    /// Set fractionDigits facet
    pub fn set_fraction_digits(
        &mut self,
        value: u32,
        fixed: FacetFixed,
        source: Option<SourceRef>,
    ) {
        self.fraction_digits = Some(FractionDigitsFacet {
            value,
            fixed,
            source,
        });
    }

    /// Add an assertion facet (XSD 1.1)
    pub fn add_assertion(
        &mut self,
        test: String,
        xpath_default_namespace: Option<String>,
        ns_snapshot: NamespaceContextSnapshot,
        source: Option<SourceRef>,
    ) {
        self.assertions.push(AssertionFacet {
            test,
            xpath_default_namespace,
            ns_snapshot,
            source,
        });
    }

    /// Set explicitTimezone facet (XSD 1.1)
    pub fn set_explicit_timezone(
        &mut self,
        value: ExplicitTimezone,
        fixed: FacetFixed,
        source: Option<SourceRef>,
    ) {
        self.explicit_timezone = Some(ExplicitTimezoneFacet {
            value,
            fixed,
            source,
        });
    }

    /// Merge facets from a base type (for type derivation by restriction)
    ///
    /// Inherited facets are only set if not already defined in this facet set.
    /// The `fixed` attribute is preserved from the base type.
    ///
    /// Note: This method does not validate that derived facets are more restrictive.
    /// Use `merge_with_base()` for full validation.
    pub fn inherit_from(&mut self, base: &FacetSet) {
        // String length facets
        if self.length.is_none() {
            self.length = base.length.clone();
        }
        if self.min_length.is_none() {
            self.min_length = base.min_length.clone();
        }
        if self.max_length.is_none() {
            self.max_length = base.max_length.clone();
        }

        // Patterns are cumulative (ANDed together)
        for pattern in &base.patterns {
            if !self.patterns.iter().any(|p| p.value == pattern.value) {
                self.patterns.push(pattern.clone());
            }
        }

        // Whitespace
        if self.whitespace.is_none() {
            self.whitespace = base.whitespace.clone();
        }

        // Numeric bounds
        if self.min_inclusive.is_none() {
            self.min_inclusive = base.min_inclusive.clone();
        }
        if self.max_inclusive.is_none() {
            self.max_inclusive = base.max_inclusive.clone();
        }
        if self.min_exclusive.is_none() {
            self.min_exclusive = base.min_exclusive.clone();
        }
        if self.max_exclusive.is_none() {
            self.max_exclusive = base.max_exclusive.clone();
        }

        // Decimal precision
        if self.total_digits.is_none() {
            self.total_digits = base.total_digits.clone();
        }
        if self.fraction_digits.is_none() {
            self.fraction_digits = base.fraction_digits.clone();
        }

        // XSD 1.1 assertions are cumulative
        for assertion in &base.assertions {
            self.assertions.push(assertion.clone());
        }

        if self.explicit_timezone.is_none() {
            self.explicit_timezone = base.explicit_timezone.clone();
        }
    }

    /// Merge base type facets with derived type facets, validating derivation rules.
    ///
    /// This method enforces XSD derivation by restriction rules:
    /// - Fixed facets cannot be overridden with different values
    /// - Derived facets must be more restrictive than base facets
    /// - Patterns are cumulative (ANDed together)
    /// - Enumerations must be subsets of base enumerations
    ///
    /// Returns a new FacetSet combining base and derived facets, or an error
    /// if the derivation rules are violated.
    pub fn merge_with_base(&self, base: &FacetSet) -> FacetResult<FacetSet> {
        // XSD Datatypes Part 2 §4.3.1.4 / §4.3.2.4 / §4.3.3.4 same-step rule:
        // It is an error for both `length` and `minLength` (or `length` and
        // `maxLength`) to be members of {facets} in the same derivation step.
        // `self` represents this step's locally declared facets before the
        // base merge, so this is the correct moment to detect the conflict.
        if self.length.is_some() && self.min_length.is_some() {
            return Err(FacetError::conflicting(
                "length and minLength cannot both appear in the same restriction step",
            ));
        }
        if self.length.is_some() && self.max_length.is_some() {
            return Err(FacetError::conflicting(
                "length and maxLength cannot both appear in the same restriction step",
            ));
        }

        let mut result = self.clone();

        // === Length facets ===
        // Validate and merge length facet
        if let Some(ref base_length) = base.length {
            match &result.length {
                Some(derived) => {
                    // Fixed length cannot be changed
                    if base_length.fixed == FacetFixed::Fixed && derived.value != base_length.value
                    {
                        return Err(FacetError::fixed_violation(
                            "length",
                            base_length.value.to_string(),
                            derived.value.to_string(),
                        ));
                    }
                }
                None => {
                    result.length = Some(base_length.clone());
                }
            }
        }

        // Validate and merge minLength facet
        if let Some(ref base_min) = base.min_length {
            match &result.min_length {
                Some(derived) => {
                    if base_min.fixed == FacetFixed::Fixed && derived.value != base_min.value {
                        return Err(FacetError::fixed_violation(
                            "minLength",
                            base_min.value.to_string(),
                            derived.value.to_string(),
                        ));
                    }
                    // Derived minLength must be >= base minLength
                    if derived.value < base_min.value {
                        return Err(FacetError::derivation(format!(
                            "minLength {} is less restrictive than base minLength {}",
                            derived.value, base_min.value
                        )));
                    }
                }
                None => {
                    result.min_length = Some(base_min.clone());
                }
            }
        }

        // Validate and merge maxLength facet
        if let Some(ref base_max) = base.max_length {
            match &result.max_length {
                Some(derived) => {
                    if base_max.fixed == FacetFixed::Fixed && derived.value != base_max.value {
                        return Err(FacetError::fixed_violation(
                            "maxLength",
                            base_max.value.to_string(),
                            derived.value.to_string(),
                        ));
                    }
                    // Derived maxLength must be <= base maxLength
                    if derived.value > base_max.value {
                        return Err(FacetError::derivation(format!(
                            "maxLength {} is less restrictive than base maxLength {}",
                            derived.value, base_max.value
                        )));
                    }
                }
                None => {
                    result.max_length = Some(base_max.clone());
                }
            }
        }

        // === Patterns ===
        // Patterns are cumulative (ANDed) - add base patterns that aren't already present
        for base_pattern in &base.patterns {
            if !result
                .patterns
                .iter()
                .any(|p| p.value == base_pattern.value)
            {
                result.patterns.push(base_pattern.clone());
            }
        }

        // === Enumeration ===
        // If base has enumeration, derived must be a subset (or not specify enumeration)
        if let Some(ref base_enum) = base.enumeration {
            match &result.enumeration {
                Some(derived_enum) => {
                    // Check that derived values are subset of base values
                    for value in &derived_enum.values {
                        if !base_enum.values.contains(value) {
                            return Err(FacetError::derivation(format!(
                                "enumeration value '{}' is not in base enumeration",
                                value
                            )));
                        }
                    }
                }
                None => {
                    // Inherit base enumeration
                    result.enumeration = Some(base_enum.clone());
                }
            }
        }

        // === Whitespace ===
        if let Some(ref base_ws) = base.whitespace {
            match &result.whitespace {
                Some(derived) => {
                    if base_ws.fixed == FacetFixed::Fixed && derived.value != base_ws.value {
                        return Err(FacetError::fixed_violation(
                            "whiteSpace",
                            format!("{:?}", base_ws.value),
                            format!("{:?}", derived.value),
                        ));
                    }
                    // Whitespace can only become more restrictive:
                    // preserve -> replace -> collapse
                    if !is_whitespace_more_restrictive(derived.value, base_ws.value) {
                        return Err(FacetError::derivation(format!(
                            "whiteSpace {:?} is less restrictive than base {:?}",
                            derived.value, base_ws.value
                        )));
                    }
                }
                None => {
                    result.whitespace = Some(base_ws.clone());
                }
            }
        }

        // === Numeric bounds ===
        // Note: Full numeric comparison would require parsing the values
        // For now, we check fixed constraints and inherit missing values.
        //
        // A derived type may switch between Inclusive and Exclusive for the same bound
        // (e.g., base has minInclusive, derived has minExclusive).  Per cos-st-restricts,
        // only the derived facet applies, so we must NOT inherit the base facet when the
        // derived type already supplies the complementary one.
        if let Some(ref base_facet) = base.min_inclusive {
            if let Some(ref derived) = result.min_inclusive {
                if base_facet.fixed == FacetFixed::Fixed && derived.value != base_facet.value {
                    return Err(FacetError::fixed_violation(
                        "minInclusive",
                        &base_facet.value,
                        &derived.value,
                    ));
                }
            } else if result.min_exclusive.is_none() {
                // Only inherit if derived hasn't replaced it with minExclusive
                result.min_inclusive = Some(base_facet.clone());
            }
        }

        if let Some(ref base_facet) = base.max_inclusive {
            if let Some(ref derived) = result.max_inclusive {
                if base_facet.fixed == FacetFixed::Fixed && derived.value != base_facet.value {
                    return Err(FacetError::fixed_violation(
                        "maxInclusive",
                        &base_facet.value,
                        &derived.value,
                    ));
                }
            } else if result.max_exclusive.is_none() {
                // Only inherit if derived hasn't replaced it with maxExclusive
                result.max_inclusive = Some(base_facet.clone());
            }
        }

        if let Some(ref base_facet) = base.min_exclusive {
            if let Some(ref derived) = result.min_exclusive {
                if base_facet.fixed == FacetFixed::Fixed && derived.value != base_facet.value {
                    return Err(FacetError::fixed_violation(
                        "minExclusive",
                        &base_facet.value,
                        &derived.value,
                    ));
                }
            } else if result.min_inclusive.is_none() {
                // Only inherit if derived hasn't replaced it with minInclusive
                result.min_exclusive = Some(base_facet.clone());
            }
        }

        if let Some(ref base_facet) = base.max_exclusive {
            if let Some(ref derived) = result.max_exclusive {
                if base_facet.fixed == FacetFixed::Fixed && derived.value != base_facet.value {
                    return Err(FacetError::fixed_violation(
                        "maxExclusive",
                        &base_facet.value,
                        &derived.value,
                    ));
                }
            } else if result.max_inclusive.is_none() {
                // Only inherit if derived hasn't replaced it with maxInclusive
                result.max_exclusive = Some(base_facet.clone());
            }
        }

        // === Digit facets ===
        if let Some(ref base_td) = base.total_digits {
            match &result.total_digits {
                Some(derived) => {
                    if base_td.fixed == FacetFixed::Fixed && derived.value != base_td.value {
                        return Err(FacetError::fixed_violation(
                            "totalDigits",
                            base_td.value.to_string(),
                            derived.value.to_string(),
                        ));
                    }
                    // Derived totalDigits must be <= base totalDigits
                    if derived.value > base_td.value {
                        return Err(FacetError::derivation(format!(
                            "totalDigits {} is less restrictive than base totalDigits {}",
                            derived.value, base_td.value
                        )));
                    }
                }
                None => {
                    result.total_digits = Some(base_td.clone());
                }
            }
        }

        if let Some(ref base_fd) = base.fraction_digits {
            match &result.fraction_digits {
                Some(derived) => {
                    if base_fd.fixed == FacetFixed::Fixed && derived.value != base_fd.value {
                        return Err(FacetError::fixed_violation(
                            "fractionDigits",
                            base_fd.value.to_string(),
                            derived.value.to_string(),
                        ));
                    }
                    // Derived fractionDigits must be <= base fractionDigits
                    if derived.value > base_fd.value {
                        return Err(FacetError::derivation(format!(
                            "fractionDigits {} is less restrictive than base fractionDigits {}",
                            derived.value, base_fd.value
                        )));
                    }
                }
                None => {
                    result.fraction_digits = Some(base_fd.clone());
                }
            }
        }

        // === XSD 1.1 facets ===
        // Assertions are cumulative
        for assertion in &base.assertions {
            result.assertions.push(assertion.clone());
        }

        // ExplicitTimezone — §4.3.16 Valid explicitTimezone Restrictions:
        //   base=optional   → derived ∈ {optional, required, prohibited}
        //   base=required   → derived ∈ {required}
        //   base=prohibited → derived ∈ {prohibited}
        // This restriction is independent of {fixed}; fixed adds only a
        // stronger value-equality requirement on top.
        if let Some(ref base_etz) = base.explicit_timezone {
            if let Some(ref derived) = result.explicit_timezone {
                let restriction_ok = match base_etz.value {
                    ExplicitTimezone::Optional => true,
                    ExplicitTimezone::Required => derived.value == ExplicitTimezone::Required,
                    ExplicitTimezone::Prohibited => derived.value == ExplicitTimezone::Prohibited,
                };
                if !restriction_ok {
                    return Err(FacetError::derivation(format!(
                        "explicitTimezone {:?} is not a valid restriction of base {:?}",
                        derived.value, base_etz.value
                    )));
                }
                if base_etz.fixed == FacetFixed::Fixed && derived.value != base_etz.value {
                    return Err(FacetError::fixed_violation(
                        "explicitTimezone",
                        format!("{:?}", base_etz.value),
                        format!("{:?}", derived.value),
                    ));
                }
            } else {
                result.explicit_timezone = Some(base_etz.clone());
            }
        }

        // === Validate conflicting facets ===
        result.validate_consistency()?;

        Ok(result)
    }

    /// Validate internal consistency of facets
    fn validate_consistency(&self) -> FacetResult<()> {
        // Check minLength <= maxLength
        if let (Some(min), Some(max)) = (&self.min_length, &self.max_length) {
            if min.value > max.value {
                return Err(FacetError::conflicting(format!(
                    "minLength {} is greater than maxLength {}",
                    min.value, max.value
                )));
            }
        }

        // Check length conflicts with minLength/maxLength
        if let Some(len) = &self.length {
            if let Some(min) = &self.min_length {
                if len.value < min.value {
                    return Err(FacetError::conflicting(format!(
                        "length {} is less than minLength {}",
                        len.value, min.value
                    )));
                }
            }
            if let Some(max) = &self.max_length {
                if len.value > max.value {
                    return Err(FacetError::conflicting(format!(
                        "length {} is greater than maxLength {}",
                        len.value, max.value
                    )));
                }
            }
        }

        // Check minInclusive <= maxInclusive (string comparison, approximate)
        // Note: Full validation would require parsing the numeric values
        if self.min_inclusive.is_some() && self.min_exclusive.is_some() {
            return Err(FacetError::conflicting(
                "cannot have both minInclusive and minExclusive",
            ));
        }
        if self.max_inclusive.is_some() && self.max_exclusive.is_some() {
            return Err(FacetError::conflicting(
                "cannot have both maxInclusive and maxExclusive",
            ));
        }

        // Check fractionDigits <= totalDigits
        if let (Some(fd), Some(td)) = (&self.fraction_digits, &self.total_digits) {
            if fd.value > td.value {
                return Err(FacetError::conflicting(format!(
                    "fractionDigits {} is greater than totalDigits {}",
                    fd.value, td.value
                )));
            }
        }

        // Check numeric bound consistency (minInclusive vs maxInclusive, etc.)
        // Uses decimal parsing for numeric comparison
        if let (Some(min_incl), Some(max_incl)) = (&self.min_inclusive, &self.max_inclusive) {
            if let Some(cmp) = compare_decimal_strings(&min_incl.value, &max_incl.value) {
                if cmp == std::cmp::Ordering::Greater {
                    return Err(FacetError::conflicting(format!(
                        "minInclusive '{}' is greater than maxInclusive '{}'",
                        min_incl.value, max_incl.value
                    )));
                }
            }
        }
        if let (Some(min_excl), Some(max_excl)) = (&self.min_exclusive, &self.max_exclusive) {
            if let Some(cmp) = compare_decimal_strings(&min_excl.value, &max_excl.value) {
                if cmp != std::cmp::Ordering::Less {
                    return Err(FacetError::conflicting(format!(
                        "minExclusive '{}' must be less than maxExclusive '{}'",
                        min_excl.value, max_excl.value
                    )));
                }
            }
        }
        if let (Some(min_incl), Some(max_excl)) = (&self.min_inclusive, &self.max_exclusive) {
            if let Some(cmp) = compare_decimal_strings(&min_incl.value, &max_excl.value) {
                if cmp != std::cmp::Ordering::Less {
                    return Err(FacetError::conflicting(format!(
                        "minInclusive '{}' must be less than maxExclusive '{}'",
                        min_incl.value, max_excl.value
                    )));
                }
            }
        }
        if let (Some(min_excl), Some(max_incl)) = (&self.min_exclusive, &self.max_inclusive) {
            if let Some(cmp) = compare_decimal_strings(&min_excl.value, &max_incl.value) {
                if cmp != std::cmp::Ordering::Less {
                    return Err(FacetError::conflicting(format!(
                        "minExclusive '{}' must be less than maxInclusive '{}'",
                        min_excl.value, max_incl.value
                    )));
                }
            }
        }

        Ok(())
    }

    /// Validate a string value against all applicable facets
    ///
    /// This validates length, pattern, enumeration, and whitespace facets.
    /// Numeric bounds and digit facets require parsed values and are not
    /// validated by this method.
    pub fn validate_string(&self, value: &str) -> FacetResult<()> {
        // Apply whitespace normalization for length calculation
        let normalized = match &self.whitespace {
            Some(ws) => normalize_whitespace(value, ws.value),
            None => value.to_string(),
        };
        let check_value = &normalized;

        // Check length facet
        if let Some(ref length) = self.length {
            let len = check_value.chars().count() as u64;
            if len != length.value {
                return Err(FacetError::length(format!(
                    "value length {} does not equal required length {}",
                    len, length.value
                )));
            }
        }

        // Check minLength facet
        if let Some(ref min_length) = self.min_length {
            let len = check_value.chars().count() as u64;
            if len < min_length.value {
                return Err(FacetError::MinLengthViolation {
                    actual: len,
                    min: min_length.value,
                });
            }
        }

        // Check maxLength facet
        if let Some(ref max_length) = self.max_length {
            let len = check_value.chars().count() as u64;
            if len > max_length.value {
                return Err(FacetError::MaxLengthViolation {
                    actual: len,
                    max: max_length.value,
                });
            }
        }

        // Check all patterns (all must match)
        for pattern in &self.patterns {
            if !pattern.matches(check_value) {
                return Err(FacetError::pattern(check_value, &pattern.value));
            }
        }

        // Check enumeration
        if let Some(ref enumeration) = self.enumeration {
            if !enumeration.values.contains(check_value) {
                return Err(FacetError::enumeration(check_value));
            }
        }

        Ok(())
    }

    /// Validate only pattern and enumeration facets on a string value.
    /// Used for list types where length facets are checked separately as item count.
    pub fn validate_string_patterns_enums(&self, value: &str) -> FacetResult<()> {
        let normalized = match &self.whitespace {
            Some(ws) => normalize_whitespace(value, ws.value),
            None => value.to_string(),
        };
        let check_value = &normalized;

        for pattern in &self.patterns {
            if !pattern.matches(check_value) {
                return Err(FacetError::pattern(check_value, &pattern.value));
            }
        }

        if let Some(ref enumeration) = self.enumeration {
            if !enumeration.values.contains(check_value) {
                return Err(FacetError::enumeration(check_value));
            }
        }

        Ok(())
    }

    /// Validate only pattern facets (no enumeration, no length).
    /// Used when enumeration must be checked in value space rather than lexically.
    pub fn validate_patterns_only(&self, value: &str) -> FacetResult<()> {
        let normalized = match &self.whitespace {
            Some(ws) => normalize_whitespace(value, ws.value),
            None => value.to_string(),
        };
        for pattern in &self.patterns {
            if !pattern.matches(&normalized) {
                return Err(FacetError::pattern(&normalized, &pattern.value));
            }
        }
        Ok(())
    }

    /// Validate enumeration in value space using a caller-supplied match predicate.
    /// `is_match(enum_str)` returns true if the instance value equals the given
    /// enumeration lexical value. `display` is used in the error message on failure.
    pub fn validate_enum_value_space(
        &self,
        is_match: impl Fn(&str) -> bool,
        display: &str,
    ) -> FacetResult<()> {
        if let Some(ref enumeration) = self.enumeration {
            if !enumeration.values.iter().any(|s| is_match(s)) {
                return Err(FacetError::enumeration(display));
            }
        }
        Ok(())
    }

    /// Validate a decimal value against numeric facets
    pub fn validate_decimal(&self, value: &rust_decimal::Decimal) -> FacetResult<()> {
        // Check totalDigits
        if let Some(ref td) = self.total_digits {
            let total = count_total_digits(value);
            if total > td.value {
                return Err(FacetError::TotalDigitsViolation {
                    actual: total,
                    max: td.value,
                });
            }
        }

        // Check fractionDigits
        if let Some(ref fd) = self.fraction_digits {
            let frac = count_fraction_digits(value);
            if frac > fd.value {
                return Err(FacetError::FractionDigitsViolation {
                    actual: frac,
                    max: fd.value,
                });
            }
        }

        // Check numeric bounds
        if let Some(ref min) = self.min_inclusive {
            if let Ok(bound) = rust_decimal::Decimal::from_str_exact(&min.value) {
                if *value < bound {
                    return Err(FacetError::MinInclusiveViolation {
                        value: value.to_string(),
                        min: min.value.clone(),
                    });
                }
            }
        }

        if let Some(ref max) = self.max_inclusive {
            if let Ok(bound) = rust_decimal::Decimal::from_str_exact(&max.value) {
                if *value > bound {
                    return Err(FacetError::MaxInclusiveViolation {
                        value: value.to_string(),
                        max: max.value.clone(),
                    });
                }
            }
        }

        if let Some(ref min) = self.min_exclusive {
            if let Ok(bound) = rust_decimal::Decimal::from_str_exact(&min.value) {
                if *value <= bound {
                    return Err(FacetError::MinExclusiveViolation {
                        value: value.to_string(),
                        min: min.value.clone(),
                    });
                }
            }
        }

        if let Some(ref max) = self.max_exclusive {
            if let Ok(bound) = rust_decimal::Decimal::from_str_exact(&max.value) {
                if *value >= bound {
                    return Err(FacetError::MaxExclusiveViolation {
                        value: value.to_string(),
                        max: max.value.clone(),
                    });
                }
            }
        }

        Ok(())
    }

    /// Validate a float value against numeric bounds facets
    pub fn validate_float(&self, value: f32) -> FacetResult<()> {
        // NaN doesn't compare normally, so skip bounds checking for NaN
        if value.is_nan() {
            return Ok(());
        }

        // Check numeric bounds
        if let Some(ref min) = self.min_inclusive {
            if let Ok(bound) = min.value.parse::<f32>() {
                if !bound.is_nan() && value < bound {
                    return Err(FacetError::MinInclusiveViolation {
                        value: format_float_for_error(value),
                        min: min.value.clone(),
                    });
                }
            }
        }

        if let Some(ref max) = self.max_inclusive {
            if let Ok(bound) = max.value.parse::<f32>() {
                if !bound.is_nan() && value > bound {
                    return Err(FacetError::MaxInclusiveViolation {
                        value: format_float_for_error(value),
                        max: max.value.clone(),
                    });
                }
            }
        }

        if let Some(ref min) = self.min_exclusive {
            if let Ok(bound) = min.value.parse::<f32>() {
                if !bound.is_nan() && value <= bound {
                    return Err(FacetError::MinExclusiveViolation {
                        value: format_float_for_error(value),
                        min: min.value.clone(),
                    });
                }
            }
        }

        if let Some(ref max) = self.max_exclusive {
            if let Ok(bound) = max.value.parse::<f32>() {
                if !bound.is_nan() && value >= bound {
                    return Err(FacetError::MaxExclusiveViolation {
                        value: format_float_for_error(value),
                        max: max.value.clone(),
                    });
                }
            }
        }

        Ok(())
    }

    /// Validate a double value against numeric bounds facets
    pub fn validate_double(&self, value: f64) -> FacetResult<()> {
        // NaN doesn't compare normally, so skip bounds checking for NaN
        if value.is_nan() {
            return Ok(());
        }

        // Check numeric bounds
        if let Some(ref min) = self.min_inclusive {
            if let Ok(bound) = min.value.parse::<f64>() {
                if !bound.is_nan() && value < bound {
                    return Err(FacetError::MinInclusiveViolation {
                        value: format_double_for_error(value),
                        min: min.value.clone(),
                    });
                }
            }
        }

        if let Some(ref max) = self.max_inclusive {
            if let Ok(bound) = max.value.parse::<f64>() {
                if !bound.is_nan() && value > bound {
                    return Err(FacetError::MaxInclusiveViolation {
                        value: format_double_for_error(value),
                        max: max.value.clone(),
                    });
                }
            }
        }

        if let Some(ref min) = self.min_exclusive {
            if let Ok(bound) = min.value.parse::<f64>() {
                if !bound.is_nan() && value <= bound {
                    return Err(FacetError::MinExclusiveViolation {
                        value: format_double_for_error(value),
                        min: min.value.clone(),
                    });
                }
            }
        }

        if let Some(ref max) = self.max_exclusive {
            if let Ok(bound) = max.value.parse::<f64>() {
                if !bound.is_nan() && value >= bound {
                    return Err(FacetError::MaxExclusiveViolation {
                        value: format_double_for_error(value),
                        max: max.value.clone(),
                    });
                }
            }
        }

        Ok(())
    }

    /// Validate explicitTimezone constraint (XSD 1.1)
    ///
    /// # Arguments
    /// * `has_timezone` - Whether the value has a timezone specified
    pub fn validate_explicit_timezone(&self, has_timezone: bool) -> FacetResult<()> {
        if let Some(ref etz) = self.explicit_timezone {
            match etz.value {
                ExplicitTimezone::Required if !has_timezone => {
                    return Err(FacetError::ExplicitTimezoneViolation {
                        message: "timezone is required but not present".to_string(),
                    });
                }
                ExplicitTimezone::Prohibited if has_timezone => {
                    return Err(FacetError::ExplicitTimezoneViolation {
                        message: "timezone is prohibited but present".to_string(),
                    });
                }
                ExplicitTimezone::Optional
                | ExplicitTimezone::Required
                | ExplicitTimezone::Prohibited => {
                    // Valid
                }
            }
        }
        Ok(())
    }

    /// Validate a binary value (hex or base64) against length facets
    pub fn validate_binary_length(&self, byte_count: u64) -> FacetResult<()> {
        // For binary types, length is measured in octets
        if let Some(ref length) = self.length {
            if byte_count != length.value {
                return Err(FacetError::length(format!(
                    "binary length {} does not equal required length {}",
                    byte_count, length.value
                )));
            }
        }

        if let Some(ref min_length) = self.min_length {
            if byte_count < min_length.value {
                return Err(FacetError::MinLengthViolation {
                    actual: byte_count,
                    min: min_length.value,
                });
            }
        }

        if let Some(ref max_length) = self.max_length {
            if byte_count > max_length.value {
                return Err(FacetError::MaxLengthViolation {
                    actual: byte_count,
                    max: max_length.value,
                });
            }
        }

        Ok(())
    }

    /// Validate a list value against length facets (item count)
    pub fn validate_list_length(&self, item_count: u64) -> FacetResult<()> {
        // For list types, length is measured in number of items
        if let Some(ref length) = self.length {
            if item_count != length.value {
                return Err(FacetError::length(format!(
                    "list length {} does not equal required length {}",
                    item_count, length.value
                )));
            }
        }

        if let Some(ref min_length) = self.min_length {
            if item_count < min_length.value {
                return Err(FacetError::MinLengthViolation {
                    actual: item_count,
                    min: min_length.value,
                });
            }
        }

        if let Some(ref max_length) = self.max_length {
            if item_count > max_length.value {
                return Err(FacetError::MaxLengthViolation {
                    actual: item_count,
                    max: max_length.value,
                });
            }
        }

        Ok(())
    }
}

/// Check if derived whitespace mode is more restrictive than base
fn is_whitespace_more_restrictive(derived: WhitespaceMode, base: WhitespaceMode) -> bool {
    use WhitespaceMode::*;
    match (base, derived) {
        // Same is always OK
        (Preserve, Preserve) | (Replace, Replace) | (Collapse, Collapse) => true,
        // preserve -> replace -> collapse is more restrictive
        (Preserve, Replace) | (Preserve, Collapse) | (Replace, Collapse) => true,
        // Going the other way is less restrictive
        _ => false,
    }
}

/// Compare two strings as decimal/integer values.
/// Returns None if either string cannot be parsed as a number.
fn compare_decimal_strings(a: &str, b: &str) -> Option<std::cmp::Ordering> {
    // Try parsing as f64 for general numeric comparison
    let a_val: f64 = a.trim().parse().ok()?;
    let b_val: f64 = b.trim().parse().ok()?;
    a_val.partial_cmp(&b_val)
}

/// Apply whitespace normalization to a string
pub fn normalize_whitespace(s: &str, mode: WhitespaceMode) -> String {
    match mode {
        WhitespaceMode::Preserve => s.to_string(),
        WhitespaceMode::Replace => {
            // Replace tab, CR, LF with space
            s.chars()
                .map(|c| match c {
                    '\t' | '\r' | '\n' => ' ',
                    _ => c,
                })
                .collect()
        }
        WhitespaceMode::Collapse => {
            // Replace, then collapse consecutive spaces, then trim
            let replaced: String = s
                .chars()
                .map(|c| match c {
                    '\t' | '\r' | '\n' => ' ',
                    _ => c,
                })
                .collect();

            let mut result = String::with_capacity(replaced.len());
            let mut prev_space = true; // Start true to trim leading spaces

            for c in replaced.chars() {
                if c == ' ' {
                    if !prev_space {
                        result.push(' ');
                        prev_space = true;
                    }
                } else {
                    result.push(c);
                    prev_space = false;
                }
            }

            // Trim trailing space
            if result.ends_with(' ') {
                result.pop();
            }

            result
        }
    }
}

/// Count total significant digits in a decimal value
fn count_total_digits(value: &rust_decimal::Decimal) -> u32 {
    // Get absolute value and remove trailing zeros
    let s = value.abs().normalize().to_string();
    // Count digits, excluding decimal point and leading zeros after decimal
    s.chars().filter(|c| c.is_ascii_digit()).count() as u32
}

/// Count fraction digits in a decimal value
fn count_fraction_digits(value: &rust_decimal::Decimal) -> u32 {
    let s = value.normalize().to_string();
    match s.find('.') {
        Some(pos) => (s.len() - pos - 1) as u32,
        None => 0,
    }
}

/// Format a float value for error messages (XSD canonical form)
fn format_float_for_error(v: f32) -> String {
    if v.is_nan() {
        "NaN".to_string()
    } else if v.is_infinite() {
        if v.is_sign_positive() {
            "INF".to_string()
        } else {
            "-INF".to_string()
        }
    } else {
        v.to_string()
    }
}

/// Format a double value for error messages (XSD canonical form)
fn format_double_for_error(v: f64) -> String {
    if v.is_nan() {
        "NaN".to_string()
    } else if v.is_infinite() {
        if v.is_sign_positive() {
            "INF".to_string()
        } else {
            "-INF".to_string()
        }
    } else {
        v.to_string()
    }
}

/// Facet applicability for built-in types
///
/// Defines which facets can be applied to which primitive types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FacetApplicability {
    /// Facet is not applicable to this type
    NotApplicable,
    /// Facet is applicable to this type
    Applicable,
    /// Facet is required for this type (e.g., whitespace for string)
    Required,
}

/// Facet kind enumeration for checking applicability
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FacetKind {
    Length,
    MinLength,
    MaxLength,
    Pattern,
    Enumeration,
    Whitespace,
    MinInclusive,
    MaxInclusive,
    MinExclusive,
    MaxExclusive,
    TotalDigits,
    FractionDigits,
    /// XSD 1.1
    ExplicitTimezone,
    /// XSD 1.1
    Assertion,
}

impl FacetKind {
    /// Parse facet kind from name
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "length" => Some(Self::Length),
            "minLength" => Some(Self::MinLength),
            "maxLength" => Some(Self::MaxLength),
            "pattern" => Some(Self::Pattern),
            "enumeration" => Some(Self::Enumeration),
            "whiteSpace" => Some(Self::Whitespace),
            "minInclusive" => Some(Self::MinInclusive),
            "maxInclusive" => Some(Self::MaxInclusive),
            "minExclusive" => Some(Self::MinExclusive),
            "maxExclusive" => Some(Self::MaxExclusive),
            "totalDigits" => Some(Self::TotalDigits),
            "fractionDigits" => Some(Self::FractionDigits),
            "explicitTimezone" => Some(Self::ExplicitTimezone),
            "assertion" => Some(Self::Assertion),
            _ => None,
        }
    }

    /// Get the name of this facet kind
    pub fn name(&self) -> &'static str {
        match self {
            Self::Length => "length",
            Self::MinLength => "minLength",
            Self::MaxLength => "maxLength",
            Self::Pattern => "pattern",
            Self::Enumeration => "enumeration",
            Self::Whitespace => "whiteSpace",
            Self::MinInclusive => "minInclusive",
            Self::MaxInclusive => "maxInclusive",
            Self::MinExclusive => "minExclusive",
            Self::MaxExclusive => "maxExclusive",
            Self::TotalDigits => "totalDigits",
            Self::FractionDigits => "fractionDigits",
            Self::ExplicitTimezone => "explicitTimezone",
            Self::Assertion => "assertion",
        }
    }
}

/// Check if a facet is applicable to a type (using XmlTypeCode)
pub fn facet_applicable_for_type(facet: FacetKind, type_code: XmlTypeCode) -> FacetApplicability {
    use FacetApplicability::*;
    use FacetKind::*;
    use XmlTypeCode::*;

    match facet {
        // Length facets apply to string, binary, list, and URI types
        Length | MinLength | MaxLength => match type_code {
            String | NormalizedString | Token | Language | NmToken | Name | NCName | Id | IdRef
            | Entity | HexBinary | Base64Binary | AnyUri | QName | Notation | NmTokens | IdRefs
            | Entities => Applicable,
            _ => NotApplicable,
        },

        // Pattern and enumeration apply to all atomic types
        Pattern | Enumeration => {
            if type_code.is_atomic() || type_code == AnySimpleType || type_code == AnyAtomicType {
                Applicable
            } else {
                NotApplicable
            }
        }

        // Whitespace is required for string, applicable to all string-derived types
        Whitespace => match type_code {
            String => Required,
            NormalizedString | Token | Language | NmToken | Name | NCName | Id | IdRef | Entity => {
                Applicable
            }
            // All other atomic types can have whitespace
            _ if type_code.is_atomic() => Applicable,
            _ => NotApplicable,
        },

        // Bound facets apply to ordered types (numeric, date/time)
        MinInclusive | MaxInclusive | MinExclusive | MaxExclusive => match type_code {
            // Decimal hierarchy
            Decimal | Integer | NonPositiveInteger | NegativeInteger | NonNegativeInteger
            | PositiveInteger | Long | Int | Short | Byte | UnsignedLong | UnsignedInt
            | UnsignedShort | UnsignedByte => Applicable,
            // Float/Double
            Float | Double => Applicable,
            // Date/time types (all have total ordering)
            Duration | DateTime | Time | Date | GYearMonth | GYear | GMonthDay | GDay | GMonth
            | YearMonthDuration | DayTimeDuration | DateTimeStamp => Applicable,
            _ => NotApplicable,
        },

        // Digit facets apply only to decimal types
        TotalDigits => match type_code {
            Decimal | Integer | NonPositiveInteger | NegativeInteger | NonNegativeInteger
            | PositiveInteger | Long | Int | Short | Byte | UnsignedLong | UnsignedInt
            | UnsignedShort | UnsignedByte => Applicable,
            _ => NotApplicable,
        },

        FractionDigits => match type_code {
            Decimal => Applicable,
            // Integer types have fractionDigits implicitly 0
            Integer | NonPositiveInteger | NegativeInteger | NonNegativeInteger
            | PositiveInteger | Long | Int | Short | Byte | UnsignedLong | UnsignedInt
            | UnsignedShort | UnsignedByte => Applicable,
            _ => NotApplicable,
        },

        // XSD 1.1: explicitTimezone applies to date/time types with optional timezone
        ExplicitTimezone => match type_code {
            DateTime | Time | Date | GYearMonth | GYear | GMonthDay | GDay | GMonth
            | DateTimeStamp => Applicable,
            _ => NotApplicable,
        },

        // XSD 1.1: assertion applies to all types
        Assertion => Applicable,
    }
}

/// Check if a facet is applicable to a built-in type (by name)
///
/// This is a convenience wrapper around `facet_applicable_for_type` that
/// takes string names for compatibility.
pub fn facet_applicable(type_name: &str, facet_name: &str) -> FacetApplicability {
    let facet = match FacetKind::from_name(facet_name) {
        Some(f) => f,
        None => return FacetApplicability::NotApplicable,
    };

    let type_code = match XmlTypeCode::from_local_name(type_name) {
        Some(tc) => tc,
        None => return FacetApplicability::NotApplicable,
    };

    facet_applicable_for_type(facet, type_code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;
    use std::str::FromStr;

    // =========================================================================
    // Basic FacetSet tests
    // =========================================================================

    #[test]
    fn test_facet_set_empty() {
        let facets = FacetSet::new();
        assert!(facets.is_empty());
    }

    #[test]
    fn test_facet_set_length() {
        let mut facets = FacetSet::new();
        facets.set_length(10, FacetFixed::Default, None);

        assert!(!facets.is_empty());
        assert_eq!(facets.length.as_ref().unwrap().value, 10);
    }

    #[test]
    fn test_facet_set_patterns() {
        let mut facets = FacetSet::new();
        facets
            .add_pattern("[a-z]+".to_string(), None, XsdVersion::V1_1)
            .unwrap();
        facets
            .add_pattern("[0-9]+".to_string(), None, XsdVersion::V1_1)
            .unwrap();

        assert_eq!(facets.patterns.len(), 2);
    }

    #[test]
    fn test_facet_set_enumeration() {
        let mut facets = FacetSet::new();
        facets.add_enumeration("red".to_string(), None);
        facets.add_enumeration("green".to_string(), None);
        facets.add_enumeration("blue".to_string(), None);

        let enum_facet = facets.enumeration.as_ref().unwrap();
        assert_eq!(enum_facet.values.len(), 3);
        assert!(enum_facet.values.contains("red"));
    }

    #[test]
    fn test_facet_inheritance() {
        let mut base = FacetSet::new();
        base.set_min_length(5, FacetFixed::Fixed, None);
        base.set_max_length(100, FacetFixed::Default, None);
        base.add_pattern("[a-z]+".to_string(), None, XsdVersion::V1_1)
            .unwrap();

        let mut derived = FacetSet::new();
        derived.set_max_length(50, FacetFixed::Default, None); // Override

        derived.inherit_from(&base);

        // minLength inherited
        assert_eq!(derived.min_length.as_ref().unwrap().value, 5);
        // maxLength not inherited (was overridden)
        assert_eq!(derived.max_length.as_ref().unwrap().value, 50);
        // Pattern inherited
        assert_eq!(derived.patterns.len(), 1);
    }

    // =========================================================================
    // Facet applicability tests
    // =========================================================================

    #[test]
    fn test_facet_applicability() {
        use FacetApplicability::*;

        // Length facets apply to string types
        assert_eq!(facet_applicable("string", "length"), Applicable);
        assert_eq!(facet_applicable("decimal", "length"), NotApplicable);

        // Numeric facets apply to numeric types
        assert_eq!(facet_applicable("decimal", "minInclusive"), Applicable);
        assert_eq!(facet_applicable("string", "minInclusive"), NotApplicable);

        // Pattern and enumeration apply to all
        assert_eq!(facet_applicable("string", "pattern"), Applicable);
        assert_eq!(facet_applicable("decimal", "pattern"), Applicable);

        // Whitespace is required for string
        assert_eq!(facet_applicable("string", "whiteSpace"), Required);
    }

    #[test]
    fn test_facet_applicability_with_type_code() {
        use FacetApplicability::*;
        use FacetKind::*;
        use XmlTypeCode::*;

        // Length facets
        assert_eq!(facet_applicable_for_type(Length, String), Applicable);
        assert_eq!(facet_applicable_for_type(Length, HexBinary), Applicable);
        assert_eq!(facet_applicable_for_type(Length, Decimal), NotApplicable);

        // Digit facets
        assert_eq!(facet_applicable_for_type(TotalDigits, Decimal), Applicable);
        assert_eq!(facet_applicable_for_type(TotalDigits, Integer), Applicable);
        assert_eq!(facet_applicable_for_type(TotalDigits, Float), NotApplicable);

        // Date/time facets
        assert_eq!(
            facet_applicable_for_type(ExplicitTimezone, DateTime),
            Applicable
        );
        assert_eq!(
            facet_applicable_for_type(ExplicitTimezone, String),
            NotApplicable
        );
    }

    // =========================================================================
    // Pattern tests
    // =========================================================================

    #[test]
    fn test_pattern_matching() {
        let pattern = PatternFacet::new("[a-z]+".to_string(), None, XsdVersion::V1_1).unwrap();
        assert!(pattern.matches("hello"));
        assert!(!pattern.matches("HELLO"));
        assert!(!pattern.matches("hello123"));
    }

    #[test]
    fn test_pattern_xsd_anchoring() {
        // XSD patterns are implicitly anchored
        let pattern = PatternFacet::new("abc".to_string(), None, XsdVersion::V1_1).unwrap();
        assert!(pattern.matches("abc"));
        assert!(!pattern.matches("xabc"));
        assert!(!pattern.matches("abcx"));
    }

    #[test]
    fn test_pattern_xsd_name_chars() {
        // Test \i (initial name char) and \c (name char)
        let pattern = PatternFacet::new(r"\i\c*".to_string(), None, XsdVersion::V1_1).unwrap();
        assert!(pattern.matches("foo"));
        assert!(pattern.matches("_bar"));
        assert!(pattern.matches("x123"));
        assert!(!pattern.matches("123"));
    }

    #[test]
    fn test_invalid_pattern() {
        let result = PatternFacet::new("[invalid".to_string(), None, XsdVersion::V1_1);
        assert!(result.is_err());
    }

    // =========================================================================
    // Whitespace normalization tests
    // =========================================================================

    #[test]
    fn test_whitespace_preserve() {
        let result = normalize_whitespace("  hello\t\nworld  ", WhitespaceMode::Preserve);
        assert_eq!(result, "  hello\t\nworld  ");
    }

    #[test]
    fn test_whitespace_replace() {
        let result = normalize_whitespace("  hello\t\nworld  ", WhitespaceMode::Replace);
        assert_eq!(result, "  hello  world  ");
    }

    #[test]
    fn test_whitespace_collapse() {
        let result = normalize_whitespace("  hello\t\nworld  ", WhitespaceMode::Collapse);
        assert_eq!(result, "hello world");
    }

    #[test]
    fn test_whitespace_collapse_multiple_spaces() {
        let result = normalize_whitespace("a     b", WhitespaceMode::Collapse);
        assert_eq!(result, "a b");
    }

    // =========================================================================
    // String validation tests
    // =========================================================================

    #[test]
    fn test_validate_string_length() {
        let mut facets = FacetSet::new();
        facets.set_length(5, FacetFixed::Default, None);

        assert!(facets.validate_string("hello").is_ok());
        assert!(facets.validate_string("hi").is_err());
        assert!(facets.validate_string("toolong").is_err());
    }

    #[test]
    fn test_validate_string_min_max_length() {
        let mut facets = FacetSet::new();
        facets.set_min_length(3, FacetFixed::Default, None);
        facets.set_max_length(10, FacetFixed::Default, None);

        assert!(facets.validate_string("hello").is_ok());
        assert!(facets.validate_string("hi").is_err());
        assert!(facets.validate_string("this is way too long").is_err());
    }

    #[test]
    fn test_validate_string_pattern() {
        let mut facets = FacetSet::new();
        facets
            .add_pattern("[a-z]+".to_string(), None, XsdVersion::V1_1)
            .unwrap();

        assert!(facets.validate_string("hello").is_ok());
        assert!(facets.validate_string("HELLO").is_err());
    }

    #[test]
    fn test_validate_string_enumeration() {
        let mut facets = FacetSet::new();
        facets.add_enumeration("red".to_string(), None);
        facets.add_enumeration("green".to_string(), None);
        facets.add_enumeration("blue".to_string(), None);

        assert!(facets.validate_string("red").is_ok());
        assert!(facets.validate_string("yellow").is_err());
    }

    // =========================================================================
    // Decimal validation tests
    // =========================================================================

    #[test]
    fn test_validate_decimal_total_digits() {
        let mut facets = FacetSet::new();
        facets.set_total_digits(5, FacetFixed::Default, None);

        let val = Decimal::from_str("12345").unwrap();
        assert!(facets.validate_decimal(&val).is_ok());

        let val = Decimal::from_str("123456").unwrap();
        assert!(facets.validate_decimal(&val).is_err());
    }

    #[test]
    fn test_validate_decimal_fraction_digits() {
        let mut facets = FacetSet::new();
        facets.set_fraction_digits(2, FacetFixed::Default, None);

        let val = Decimal::from_str("123.45").unwrap();
        assert!(facets.validate_decimal(&val).is_ok());

        let val = Decimal::from_str("123.456").unwrap();
        assert!(facets.validate_decimal(&val).is_err());
    }

    #[test]
    fn test_validate_decimal_bounds() {
        let mut facets = FacetSet::new();
        facets.set_min_inclusive("0".to_string(), FacetFixed::Default, None);
        facets.set_max_inclusive("100".to_string(), FacetFixed::Default, None);

        let val = Decimal::from_str("50").unwrap();
        assert!(facets.validate_decimal(&val).is_ok());

        let val = Decimal::from_str("-1").unwrap();
        assert!(facets.validate_decimal(&val).is_err());

        let val = Decimal::from_str("101").unwrap();
        assert!(facets.validate_decimal(&val).is_err());
    }

    #[test]
    fn test_validate_decimal_exclusive_bounds() {
        let mut facets = FacetSet::new();
        facets.set_min_exclusive("0".to_string(), FacetFixed::Default, None);
        facets.set_max_exclusive("100".to_string(), FacetFixed::Default, None);

        let val = Decimal::from_str("0").unwrap();
        assert!(facets.validate_decimal(&val).is_err()); // 0 is not > 0

        let val = Decimal::from_str("100").unwrap();
        assert!(facets.validate_decimal(&val).is_err()); // 100 is not < 100

        let val = Decimal::from_str("50").unwrap();
        assert!(facets.validate_decimal(&val).is_ok());
    }

    // =========================================================================
    // Binary/List length validation tests
    // =========================================================================

    #[test]
    fn test_validate_binary_length() {
        let mut facets = FacetSet::new();
        facets.set_length(4, FacetFixed::Default, None);

        assert!(facets.validate_binary_length(4).is_ok());
        assert!(facets.validate_binary_length(3).is_err());
        assert!(facets.validate_binary_length(5).is_err());
    }

    #[test]
    fn test_validate_list_length() {
        let mut facets = FacetSet::new();
        facets.set_min_length(1, FacetFixed::Default, None);
        facets.set_max_length(5, FacetFixed::Default, None);

        assert!(facets.validate_list_length(3).is_ok());
        assert!(facets.validate_list_length(0).is_err());
        assert!(facets.validate_list_length(10).is_err());
    }

    // =========================================================================
    // merge_with_base tests
    // =========================================================================

    #[test]
    fn test_merge_with_base_inherits_facets() {
        let mut base = FacetSet::new();
        base.set_min_length(5, FacetFixed::Default, None);
        base.set_max_length(100, FacetFixed::Default, None);

        let derived = FacetSet::new();
        let merged = derived.merge_with_base(&base).unwrap();

        assert_eq!(merged.min_length.as_ref().unwrap().value, 5);
        assert_eq!(merged.max_length.as_ref().unwrap().value, 100);
    }

    #[test]
    fn test_merge_with_base_allows_more_restrictive() {
        let mut base = FacetSet::new();
        base.set_min_length(5, FacetFixed::Default, None);
        base.set_max_length(100, FacetFixed::Default, None);

        let mut derived = FacetSet::new();
        derived.set_min_length(10, FacetFixed::Default, None); // More restrictive
        derived.set_max_length(50, FacetFixed::Default, None); // More restrictive

        let merged = derived.merge_with_base(&base).unwrap();
        assert_eq!(merged.min_length.as_ref().unwrap().value, 10);
        assert_eq!(merged.max_length.as_ref().unwrap().value, 50);
    }

    #[test]
    fn test_merge_with_base_rejects_less_restrictive_min_length() {
        let mut base = FacetSet::new();
        base.set_min_length(10, FacetFixed::Default, None);

        let mut derived = FacetSet::new();
        derived.set_min_length(5, FacetFixed::Default, None); // Less restrictive

        let result = derived.merge_with_base(&base);
        assert!(result.is_err());
    }

    #[test]
    fn test_merge_with_base_rejects_less_restrictive_max_length() {
        let mut base = FacetSet::new();
        base.set_max_length(50, FacetFixed::Default, None);

        let mut derived = FacetSet::new();
        derived.set_max_length(100, FacetFixed::Default, None); // Less restrictive

        let result = derived.merge_with_base(&base);
        assert!(result.is_err());
    }

    #[test]
    fn test_merge_with_base_fixed_facet_same_value_ok() {
        let mut base = FacetSet::new();
        base.set_length(10, FacetFixed::Fixed, None);

        let mut derived = FacetSet::new();
        derived.set_length(10, FacetFixed::Default, None); // Same value

        let result = derived.merge_with_base(&base);
        assert!(result.is_ok());
    }

    #[test]
    fn test_merge_with_base_fixed_facet_different_value_error() {
        let mut base = FacetSet::new();
        base.set_length(10, FacetFixed::Fixed, None);

        let mut derived = FacetSet::new();
        derived.set_length(20, FacetFixed::Default, None); // Different value

        let result = derived.merge_with_base(&base);
        assert!(result.is_err());
        if let Err(FacetError::FixedFacetViolation { facet_name, .. }) = result {
            assert_eq!(facet_name, "length");
        } else {
            panic!("Expected FixedFacetViolation error");
        }
    }

    #[test]
    fn test_merge_with_base_patterns_cumulative() {
        let mut base = FacetSet::new();
        base.add_pattern("[a-z]+".to_string(), None, XsdVersion::V1_1)
            .unwrap();

        let mut derived = FacetSet::new();
        derived
            .add_pattern("[0-9]+".to_string(), None, XsdVersion::V1_1)
            .unwrap();

        let merged = derived.merge_with_base(&base).unwrap();
        assert_eq!(merged.patterns.len(), 2);
    }

    #[test]
    fn test_merge_with_base_enumeration_subset() {
        let mut base = FacetSet::new();
        base.add_enumeration("red".to_string(), None);
        base.add_enumeration("green".to_string(), None);
        base.add_enumeration("blue".to_string(), None);

        let mut derived = FacetSet::new();
        derived.add_enumeration("red".to_string(), None);
        derived.add_enumeration("blue".to_string(), None);

        let merged = derived.merge_with_base(&base);
        assert!(merged.is_ok());
    }

    #[test]
    fn test_merge_with_base_enumeration_not_subset_error() {
        let mut base = FacetSet::new();
        base.add_enumeration("red".to_string(), None);
        base.add_enumeration("green".to_string(), None);

        let mut derived = FacetSet::new();
        derived.add_enumeration("yellow".to_string(), None); // Not in base

        let result = derived.merge_with_base(&base);
        assert!(result.is_err());
    }

    #[test]
    fn test_merge_with_base_whitespace_more_restrictive() {
        let mut base = FacetSet::new();
        base.set_whitespace(WhitespaceMode::Preserve, FacetFixed::Default, None);

        let mut derived = FacetSet::new();
        derived.set_whitespace(WhitespaceMode::Collapse, FacetFixed::Default, None);

        let result = derived.merge_with_base(&base);
        assert!(result.is_ok());
    }

    #[test]
    fn test_merge_with_base_whitespace_less_restrictive_error() {
        let mut base = FacetSet::new();
        base.set_whitespace(WhitespaceMode::Collapse, FacetFixed::Default, None);

        let mut derived = FacetSet::new();
        derived.set_whitespace(WhitespaceMode::Preserve, FacetFixed::Default, None);

        let result = derived.merge_with_base(&base);
        assert!(result.is_err());
    }

    #[test]
    fn test_merge_with_base_digit_facets() {
        let mut base = FacetSet::new();
        base.set_total_digits(10, FacetFixed::Default, None);
        base.set_fraction_digits(5, FacetFixed::Default, None);

        let mut derived = FacetSet::new();
        derived.set_total_digits(5, FacetFixed::Default, None); // More restrictive
        derived.set_fraction_digits(2, FacetFixed::Default, None); // More restrictive

        let result = derived.merge_with_base(&base);
        assert!(result.is_ok());
    }

    #[test]
    fn test_merge_with_base_digit_facets_less_restrictive_error() {
        let mut base = FacetSet::new();
        base.set_total_digits(5, FacetFixed::Default, None);

        let mut derived = FacetSet::new();
        derived.set_total_digits(10, FacetFixed::Default, None); // Less restrictive

        let result = derived.merge_with_base(&base);
        assert!(result.is_err());
    }

    // =========================================================================
    // Consistency validation tests
    // =========================================================================

    #[test]
    fn test_consistency_min_greater_than_max_length() {
        let mut base = FacetSet::new();
        base.set_min_length(10, FacetFixed::Default, None);
        base.set_max_length(5, FacetFixed::Default, None);

        let result = base.merge_with_base(&FacetSet::new());
        assert!(result.is_err());
    }

    #[test]
    fn test_consistency_both_inclusive_and_exclusive() {
        let mut base = FacetSet::new();
        base.set_min_inclusive("0".to_string(), FacetFixed::Default, None);
        base.set_min_exclusive("0".to_string(), FacetFixed::Default, None);

        let result = base.merge_with_base(&FacetSet::new());
        assert!(result.is_err());
    }

    #[test]
    fn test_consistency_fraction_greater_than_total() {
        let mut base = FacetSet::new();
        base.set_total_digits(3, FacetFixed::Default, None);
        base.set_fraction_digits(5, FacetFixed::Default, None);

        let result = base.merge_with_base(&FacetSet::new());
        assert!(result.is_err());
    }

    // =========================================================================
    // FacetKind tests
    // =========================================================================

    #[test]
    fn test_facet_kind_from_name() {
        assert_eq!(FacetKind::from_name("length"), Some(FacetKind::Length));
        assert_eq!(
            FacetKind::from_name("minLength"),
            Some(FacetKind::MinLength)
        );
        assert_eq!(FacetKind::from_name("pattern"), Some(FacetKind::Pattern));
        assert_eq!(FacetKind::from_name("unknown"), None);
    }

    #[test]
    fn test_facet_kind_name_roundtrip() {
        let kinds = [
            FacetKind::Length,
            FacetKind::MinLength,
            FacetKind::MaxLength,
            FacetKind::Pattern,
            FacetKind::Enumeration,
            FacetKind::Whitespace,
            FacetKind::MinInclusive,
            FacetKind::MaxInclusive,
            FacetKind::MinExclusive,
            FacetKind::MaxExclusive,
            FacetKind::TotalDigits,
            FacetKind::FractionDigits,
            FacetKind::ExplicitTimezone,
            FacetKind::Assertion,
        ];

        for kind in kinds {
            let name = kind.name();
            assert_eq!(FacetKind::from_name(name), Some(kind));
        }
    }

    // =========================================================================
    // XSD pattern to Rust conversion tests (XSD 1.0 path only)
    // =========================================================================

    #[cfg(not(feature = "xsd11"))]
    #[test]
    fn test_xsd_pattern_anchoring() {
        let rust = convert_xml_pattern("abc", ConvertOptions::xsd());
        assert!(rust.starts_with('^'));
        assert!(rust.ends_with('$'));
    }

    #[cfg(not(feature = "xsd11"))]
    #[test]
    fn test_xsd_pattern_initial_name_char() {
        let rust = convert_xml_pattern(r"\i", ConvertOptions::xsd());
        assert!(rust.contains("[A-Za-z_:]"));
    }

    #[cfg(not(feature = "xsd11"))]
    #[test]
    fn test_xsd_pattern_name_char() {
        let rust = convert_xml_pattern(r"\c", ConvertOptions::xsd());
        // The hyphen is escaped in the character class
        assert!(rust.contains(r"[A-Za-z0-9._:\-]"));
    }

    #[cfg(not(feature = "xsd11"))]
    #[test]
    fn test_xsd_pattern_standard_escapes() {
        let rust = convert_xml_pattern(r"\d+\s*", ConvertOptions::xsd());
        assert!(rust.contains(r"\d"));
        assert!(rust.contains(r"\s"));
    }
}
