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
use crate::regex_convert::lenient_ms_preprocess;
#[cfg(feature = "xsd11")]
use crate::regex_convert::rewrite_xsd10_category_escapes;
use crate::regex_convert::validate_xml_pattern_syntax;
#[cfg(not(feature = "xsd11"))]
use crate::regex_convert::{convert_xml_pattern, ConvertOptions};
use crate::schema::model::{RegexCompat, XsdVersion};
#[cfg(not(feature = "xsd11"))]
use regex::Regex;
use std::borrow::Cow;
use std::collections::HashSet;

#[cfg(feature = "xsd11")]
use std::sync::Arc;

use super::XmlTypeCode;

/// Process-global cache of compiled XSD pattern regexes (`xsd11` / `regexml`
/// backend only).
///
/// `regexml` 0.2 rebuilds the Unicode category sets behind `\d`, `\w`, and
/// `\p{…}` from the ICU code-point trie on **every** `Regex::new` — it memoizes
/// nothing — which is the dominant cost when compiling pattern-heavy schemas
/// (see `validation-bench/PERF_PROFILE_CONFORMANCE.md`). The same pattern
/// strings recur constantly: built-in datatype facets are re-compiled for every
/// `SchemaSet`, the conformance harness re-compiles per test, and the two-pass
/// `xsi:schemaLocation` enrichment flow re-compiles a whole schema's patterns.
/// Caching the compiled program turns those repeats into an `Arc` clone.
///
/// ## Design — direct-mapped, like a CPU cache
///
/// A **fixed-size** table of [`SLOTS`] entries. A key hashes to exactly one slot
/// (`index = hash & MASK`); a colliding key simply **overwrites** the resident
/// entry (no probing, no chaining). Properties:
///
/// - **Bounded by construction** — memory is `O(SLOTS)`, never grows, no
///   eviction-policy bookkeeping. This degrades gracefully in a long-running
///   process that sees an unbounded variety of schemas: recent patterns stay
///   resident, cold ones fall out — unlike an unbounded map, which would either
///   grow without limit or freeze at a cap and never cache anything new.
/// - **Verified hits** — each slot stores its full key; a probe is a hit only
///   when the resident key equals the lookup key, so collisions are correctness-
///   safe (they just miss and recompile).
/// - **Clearable** — [`clear_pattern_cache`] empties it (reclaim between
///   workloads, or for deterministic benchmark timings).
///
/// Keyed by everything that determines the compiled program —
/// `(pattern, xsd_version, regex_compat)`. Only **successful** compiles are
/// stored (an invalid pattern must keep reporting its error). *Distinct*
/// patterns still pay full price; removing that needs an upstream `regexml`
/// category memoization, which this cache deliberately does not require.
#[cfg(feature = "xsd11")]
mod pattern_cache {
    use std::sync::{Arc, OnceLock, RwLock};

    use crate::schema::model::{RegexCompat, XsdVersion};

    /// (pattern, version, compat) — everything that affects the result.
    pub(super) type Key = (String, XsdVersion, RegexCompat);

    struct Slot {
        key: Key,
        regex: Arc<regexml::Regex>,
    }

    /// Number of cache lines. Power of two so `index = hash & MASK`. At 4096
    /// the table itself is ~4096 × `size_of::<Option<Slot>>()` ≈ 130 KB plus the
    /// resident pattern strings — far smaller than the working set of *all*
    /// distinct patterns, but large enough that the heavily-repeated ones
    /// (built-ins, two-pass enrichment recompiles, shared test fixtures) rarely
    /// collide.
    const SLOTS: usize = 4096;
    const MASK: u64 = (SLOTS as u64) - 1;

    fn table() -> &'static RwLock<Box<[Option<Slot>]>> {
        static TABLE: OnceLock<RwLock<Box<[Option<Slot>]>>> = OnceLock::new();
        TABLE.get_or_init(|| {
            let slots = (0..SLOTS)
                .map(|_| None)
                .collect::<Vec<_>>()
                .into_boxed_slice();
            RwLock::new(slots)
        })
    }

    /// Slot index for a key. Fixed-seed `ahash` so a given key maps to the same
    /// line for the whole process (the seed need not be secret — the index is
    /// never exposed and collisions are correctness-safe).
    fn index(key: &Key) -> usize {
        static HASHER: OnceLock<ahash::RandomState> = OnceLock::new();
        let state = HASHER.get_or_init(|| {
            ahash::RandomState::with_seeds(
                0xa5a5_5a5a_3c3c_c3c3,
                0x1234_5678_9abc_def0,
                0x9e37_79b9_7f4a_7c15,
                0xc2b2_ae35_5d12_3456,
            )
        });
        (state.hash_one(key) & MASK) as usize
    }

    /// Look up a previously-compiled regex. A hit requires the resident slot to
    /// hold this exact key (guards against hash collisions). The lock is held
    /// only for the probe — never across a compile — so poisoning is effectively
    /// impossible; recover rather than propagate a panic.
    pub(super) fn get(key: &Key) -> Option<Arc<regexml::Regex>> {
        let idx = index(key);
        let guard = table().read().unwrap_or_else(|e| e.into_inner());
        match &guard[idx] {
            Some(slot) if &slot.key == key => Some(Arc::clone(&slot.regex)),
            _ => None,
        }
    }

    /// Record a successfully-compiled regex, replacing whatever shared its line.
    pub(super) fn store(key: Key, regex: Arc<regexml::Regex>) {
        let idx = index(&key);
        let mut guard = table().write().unwrap_or_else(|e| e.into_inner());
        guard[idx] = Some(Slot { key, regex });
    }

    /// Drop every cached entry.
    pub(super) fn clear() {
        let mut guard = table().write().unwrap_or_else(|e| e.into_inner());
        for slot in guard.iter_mut() {
            *slot = None;
        }
    }
}

/// Clear the process-global compiled-pattern regex cache.
///
/// The cache (a small fixed-size, direct-mapped table; see the module-level
/// `pattern_cache`) lives for the life of the process and speeds up recompiling
/// identical `xs:pattern` facets. Call this to reclaim it — e.g. between
/// unrelated workloads in a long-running service, or for deterministic timings
/// in a benchmark. Safe to call concurrently with compilation. Without the
/// `xsd11` feature there is no such cache and this is a no-op.
#[cfg(feature = "xsd11")]
pub fn clear_pattern_cache() {
    pattern_cache::clear();
}

/// Clear the process-global compiled-pattern regex cache. No-op without the
/// `xsd11` feature (the XSD 1.0 backend compiles via the `regex` crate and does
/// not cache).
#[cfg(not(feature = "xsd11"))]
pub fn clear_pattern_cache() {}

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
    ///
    /// `regex_compat` controls grammar leniency: `Strict` enforces XSD
    /// Part 2 §F/§G; `LenientMs` first applies [`lenient_ms_preprocess`]
    /// to drop start/end anchors and `(?#…)` comments common in
    /// .NET-authored schemas.
    pub fn new(
        value: String,
        source: Option<SourceRef>,
        xsd_version: XsdVersion,
        regex_compat: RegexCompat,
    ) -> FacetResult<Self> {
        let mut facet = Self::new_unchecked(value, source);
        facet.compile(xsd_version, regex_compat)?;
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
    pub fn compile(
        &mut self,
        xsd_version: XsdVersion,
        regex_compat: RegexCompat,
    ) -> FacetResult<()> {
        if self.compiled.is_none() {
            let effective: std::borrow::Cow<'_, str> = match regex_compat {
                RegexCompat::Strict => std::borrow::Cow::Borrowed(self.value.as_str()),
                RegexCompat::LenientMs => lenient_ms_preprocess(&self.value),
            };
            // The XSD 1.0 hyphen-rule check is part of the strict §F grammar
            // gate; skip it under LenientMs so the engine alone decides.
            if xsd_version == XsdVersion::V1_0 && regex_compat == RegexCompat::Strict {
                validate_xml_pattern_syntax(&effective).map_err(|message| {
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
            let rust_pattern = convert_xml_pattern(&effective, opts);
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
    pub fn compile(
        &mut self,
        xsd_version: XsdVersion,
        regex_compat: RegexCompat,
    ) -> FacetResult<()> {
        if self.compiled.is_none() {
            // Content-addressed fast path: identical (pattern, version, compat)
            // reuses the already-compiled program instead of rebuilding the ICU
            // category sets via regexml. See [`pattern_cache`].
            let cache_key: pattern_cache::Key = (self.value.clone(), xsd_version, regex_compat);
            if let Some(cached) = pattern_cache::get(&cache_key) {
                self.compiled = Some(cached);
                return Ok(());
            }

            let strict = regex_compat == RegexCompat::Strict;
            // Apply MS dialect preprocess (closed list) before grammar
            // validation when LenientMs is on. Owned to keep one stable
            // backing string across the two-step validate+rewrite below.
            let effective: String = if strict {
                self.value.clone()
            } else {
                lenient_ms_preprocess(&self.value).into_owned()
            };
            // Strict XSD 1.0 hyphen-rule grammar gate. Skipped under
            // LenientMs so the engine alone decides what is well-formed.
            if xsd_version == XsdVersion::V1_0 && strict {
                validate_xml_pattern_syntax(&effective).map_err(|message| {
                    FacetError::InvalidPattern {
                        pattern: self.value.clone(),
                        message,
                    }
                })?;
            }
            // Strict XSD §F/§G grammar gate via regexml `xsd()`. Skipped
            // under LenientMs — the runtime matcher uses regexml `xpath()`
            // (next step), which natively accepts XPath-only constructs
            // like `^`/`$` outside char class, backrefs `\1`, non-capturing
            // `(?:...)`, and reluctant quantifiers `*?` that are valid
            // .NET regex idioms. For XSD 1.1, an unrecognized `\p{IsX}`
            // block name is treated as matching every character (W3C bug
            // 13670 / XSD 1.1 Datatypes §G.4.2.3); the rewrite remains in
            // place under both modes because it is a spec rule, not a
            // grammar choice (regexml 0.2 does not yet honour
            // `allow_unknown_block_names`).
            let xsd_validated: std::borrow::Cow<'_, str> = match xsd_version {
                XsdVersion::V1_0 => {
                    if strict {
                        regexml::Regex::xsd(&effective, "").map_err(|e| {
                            FacetError::InvalidPattern {
                                pattern: self.value.clone(),
                                message: format!("{:?}", e),
                            }
                        })?;
                    }
                    std::borrow::Cow::Borrowed(effective.as_str())
                }
                XsdVersion::V1_1 => validate_xsd11_pattern_with_block_fallback(&effective)?,
            };
            // Under XSD 1.0 the \p{X} rewrite produces a new String; under 1.1
            // we use the (possibly block-rewritten) validated value.
            let pinned: std::borrow::Cow<'_, str> = match xsd_version {
                XsdVersion::V1_0 => {
                    std::borrow::Cow::Owned(rewrite_xsd10_category_escapes(&effective))
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
            let arc = Arc::new(compiled);
            pattern_cache::store(cache_key, Arc::clone(&arc));
            self.compiled = Some(arc);
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
    /// Per-value namespace bindings captured at parse time, for values that
    /// are QNames (NOTATION/QName bases). Parallel to `values`; only
    /// populated when the parser supplies a snapshot.
    pub qname_contexts: Vec<(String, NamespaceContextSnapshot)>,
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

    // Pattern facets grouped by derivation step.
    //
    // Per XSD Datatypes Part 2 §4.3.4 (and the equivalent §A.2 prose),
    // multiple `<xs:pattern>` facets in a single restriction step combine
    // as alternation (logical OR), while patterns inherited from earlier
    // derivation steps further restrict the value (logical AND).
    //
    // Outer Vec = AND across derivation steps; inner Vec = OR within a step.
    pub patterns: Vec<Vec<PatternFacet>>,

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
            && self.patterns.iter().all(|step| step.is_empty())
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

    /// Add a pattern facet (compiles the pattern) at the current
    /// derivation step. Multiple consecutive `add_pattern` calls on the
    /// same FacetSet are treated as alternatives (OR'd) within a single
    /// step; a new step is opened by `inherit_from` / `merge_with_base`.
    pub fn add_pattern(
        &mut self,
        value: String,
        source: Option<SourceRef>,
        xsd_version: XsdVersion,
        regex_compat: RegexCompat,
    ) -> FacetResult<()> {
        let pattern = PatternFacet::new(value, source, xsd_version, regex_compat)?;
        self.push_pattern_to_current_step(pattern);
        Ok(())
    }

    /// Add a pattern facet without compiling (for deferred validation),
    /// at the current derivation step. See `add_pattern` for the OR/AND
    /// semantics across multiple calls.
    pub fn add_pattern_unchecked(&mut self, value: String, source: Option<SourceRef>) {
        self.push_pattern_to_current_step(PatternFacet::new_unchecked(value, source));
    }

    fn push_pattern_to_current_step(&mut self, pattern: PatternFacet) {
        if self.patterns.is_empty() {
            self.patterns.push(Vec::new());
        }
        self.patterns.last_mut().unwrap().push(pattern);
    }

    /// Compile all uncompiled patterns. Returns the first error encountered.
    ///
    /// `xsd_version` selects the Unicode-category semantics for `\p{X}`: V1_0
    /// pins to Unicode 3.0; V1_1 passes through to the backend.
    /// `regex_compat` controls grammar leniency (see [`PatternFacet::compile`]).
    pub fn compile_patterns(
        &mut self,
        xsd_version: XsdVersion,
        regex_compat: RegexCompat,
    ) -> FacetResult<()> {
        for step in &mut self.patterns {
            for pattern in step {
                pattern.compile(xsd_version, regex_compat)?;
            }
        }
        Ok(())
    }

    /// Run the per-step XSD pattern check on `value`: every step must have
    /// at least one alternative that matches (within-step OR, across-step
    /// AND). Returns the first failing step's first pattern as error
    /// context.
    fn check_patterns(&self, value: &str) -> FacetResult<()> {
        for step in &self.patterns {
            if step.is_empty() {
                continue;
            }
            if !step.iter().any(|p| p.matches(value)) {
                return Err(FacetError::pattern(value, &step[0].value));
            }
        }
        Ok(())
    }

    /// Add an enumeration value
    pub fn add_enumeration(&mut self, value: String, source: Option<SourceRef>) {
        self.add_enumeration_with_ns(value, None, source);
    }

    /// Add an enumeration value, capturing the namespace bindings in scope
    /// at the `<xs:enumeration>` element (needed to resolve QName-valued
    /// enumerations against NOTATION/QName bases).
    pub fn add_enumeration_with_ns(
        &mut self,
        value: String,
        ns_snapshot: Option<NamespaceContextSnapshot>,
        source: Option<SourceRef>,
    ) {
        let enumeration = self.enumeration.get_or_insert_with(|| EnumerationFacet {
            values: HashSet::new(),
            qname_contexts: Vec::new(),
            source: source.clone(),
        });
        if let Some(snapshot) = ns_snapshot {
            enumeration.qname_contexts.push((value.clone(), snapshot));
        }
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

        // Each base derivation step is appended as a new outer step on
        // self, preserving the within-step OR / across-step AND structure.
        // (Step-level dedup vs. derived's local step would require value-set
        // equivalence checks; skip it — repeated identical patterns are
        // idempotent under either OR or AND, and recompilation cost is
        // bounded by `compile_patterns` running per FacetSet at most once.)
        for base_step in &base.patterns {
            if !base_step.is_empty() {
                self.patterns.push(base_step.clone());
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
    ///
    /// The facets' datatype is not known here, so a lower bound is checked
    /// against an upper one by comparing the two literals as `f64` numbers
    /// (literals that are not numbers are not compared).
    pub fn merge_with_base(&self, base: &FacetSet) -> FacetResult<FacetSet> {
        self.merge_with_base_ordered(base, BoundOrder::Unknown)
    }

    /// [`FacetSet::merge_with_base`] for facets of a datatype whose order
    /// relation `order` names: the bounds are compared in that value space
    /// (Datatypes 1.1 §4.3.7–§4.3.10 compare bound values "according to the
    /// datatype's order relation").
    pub(crate) fn merge_with_base_ordered(
        &self,
        base: &FacetSet,
        order: BoundOrder,
    ) -> FacetResult<FacetSet> {
        // XSD Datatypes Part 2 §4.3.1.4 "length and minLength or maxLength":
        // both `length` and `minLength` in {facets} is an error UNLESS
        // (1.1) minLength <= length AND (1.2) some restriction ancestor has
        // minLength with the same {value} and no length. The base's merged
        // facet set stands in for "some ancestor": if it carries the same
        // minLength without a length, clause 1.2 holds (e.g. xs:IDREFS whose
        // built-in minLength=1 licenses re-stating minLength=1 next to a new
        // length — W3C bug 6446, msData IDREFS_length006). Symmetrically for
        // maxLength via clauses 2.1/2.2.
        if let (Some(len), Some(min)) = (&self.length, &self.min_length) {
            let permitted = min.value <= len.value
                && base.length.is_none()
                && base
                    .min_length
                    .as_ref()
                    .is_some_and(|b| b.value == min.value);
            if !permitted {
                return Err(FacetError::conflicting(
                    "length and minLength cannot both appear in the same restriction step",
                ));
            }
        }
        if let (Some(len), Some(max)) = (&self.length, &self.max_length) {
            let permitted = len.value <= max.value
                && base.length.is_none()
                && base
                    .max_length
                    .as_ref()
                    .is_some_and(|b| b.value == max.value);
            if !permitted {
                return Err(FacetError::conflicting(
                    "length and maxLength cannot both appear in the same restriction step",
                ));
            }
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
        // Each base derivation step is appended as a new outer step on
        // result. Within-step OR / across-step AND semantics survive the
        // merge per XSD Datatypes §4.3.4.
        for base_step in &base.patterns {
            if !base_step.is_empty() {
                result.patterns.push(base_step.clone());
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
        result.validate_consistency(order)?;

        Ok(result)
    }

    /// Validate internal consistency of facets
    fn validate_consistency(&self, order: BoundOrder) -> FacetResult<()> {
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
            if let Some(cmp) = order.compare(&min_incl.value, &max_incl.value) {
                if cmp == std::cmp::Ordering::Greater {
                    return Err(FacetError::conflicting(format!(
                        "minInclusive '{}' is greater than maxInclusive '{}'",
                        min_incl.value, max_incl.value
                    )));
                }
            }
        }
        if let (Some(min_excl), Some(max_excl)) = (&self.min_exclusive, &self.max_exclusive) {
            if let Some(cmp) = order.compare(&min_excl.value, &max_excl.value) {
                if cmp != std::cmp::Ordering::Less {
                    return Err(FacetError::conflicting(format!(
                        "minExclusive '{}' must be less than maxExclusive '{}'",
                        min_excl.value, max_excl.value
                    )));
                }
            }
        }
        if let (Some(min_incl), Some(max_excl)) = (&self.min_inclusive, &self.max_exclusive) {
            if let Some(cmp) = order.compare(&min_incl.value, &max_excl.value) {
                if cmp != std::cmp::Ordering::Less {
                    return Err(FacetError::conflicting(format!(
                        "minInclusive '{}' must be less than maxExclusive '{}'",
                        min_incl.value, max_excl.value
                    )));
                }
            }
        }
        if let (Some(min_excl), Some(max_incl)) = (&self.min_exclusive, &self.max_inclusive) {
            if let Some(cmp) = order.compare(&min_excl.value, &max_incl.value) {
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
            None => Cow::Borrowed(value),
        };
        let check_value: &str = &normalized;

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

        // Check all pattern steps (each step's alternatives are OR'd; all
        // steps must match).
        self.check_patterns(check_value)?;

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
            None => Cow::Borrowed(value),
        };
        let check_value: &str = &normalized;

        self.check_patterns(check_value)?;

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
            None => Cow::Borrowed(value),
        };
        self.check_patterns(&normalized)
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
    ///
    /// `totalDigits` and `fractionDigits` follow their definitions on the value
    /// (Datatypes 1.1 §4.3.11.3 / §4.3.12.3): the value is `i / 10^n` with
    /// `|i| < 10^totalDigits` and `0 ≤ n ≤ totalDigits`, resp.
    /// `0 ≤ n ≤ fractionDigits` — so `0.05` needs two total digits. Bound
    /// facets compare exactly, including bounds that are themselves beyond
    /// what `rust_decimal::Decimal` holds.
    pub fn validate_decimal(&self, value: &rust_decimal::Decimal) -> FacetResult<()> {
        self.check_decimal_facets(
            count_total_digits(value) as usize,
            count_fraction_digits(value) as usize,
            |bound| compare_decimal_with_bound(value, bound),
            || value.to_string(),
        )
    }

    /// [`FacetSet::validate_decimal`] for a value of any size and precision.
    pub(crate) fn validate_exact_decimal(
        &self,
        value: &super::exact_decimal::ExactDecimal,
    ) -> FacetResult<()> {
        self.check_decimal_facets(
            value.total_digits(),
            value.fraction_digits(),
            |bound| {
                let bound = super::exact_decimal::ExactDecimal::parse(&normalize_whitespace(
                    bound,
                    WhitespaceMode::Collapse,
                ))?;
                Some(value.cmp(&bound))
            },
            || value.to_string(),
        )
    }

    /// The value-based facets of an `xs:decimal`-family typed value — digits
    /// and bounds on the exact value, then `pattern` on the lexical form and
    /// `enumeration` by value (Datatypes 1.1 §4.3.5.4: "A value in a ·value
    /// space· is facet-valid with respect to ·enumeration· if and only if the
    /// value is equal or identical to one of the values specified in
    /// {value}"; §3.3.3: "the number 2.0 is not distinct from the number
    /// 2.00").
    ///
    /// `value` is the typed value a decimal-family validator produced from
    /// `normalized`, its whitespace-collapsed lexical form.
    pub(crate) fn validate_decimal_family(
        &self,
        value: &super::value::XmlValue,
        normalized: &str,
    ) -> FacetResult<()> {
        use super::exact_decimal::ExactDecimal;
        use super::value::{XmlAtomicValue, XmlValueKind};
        match &value.value {
            XmlValueKind::Atomic(XmlAtomicValue::Decimal(d)) => self.validate_decimal(d)?,
            XmlValueKind::Atomic(XmlAtomicValue::Integer(i)) => match integer_as_decimal(i) {
                Some(d) => self.validate_decimal(&d)?,
                None => self.validate_exact_decimal(&ExactDecimal::from_bigint(i))?,
            },
            _ => {
                if let Some(exact) = ExactDecimal::from_value(value) {
                    self.validate_exact_decimal(&exact)?;
                }
            }
        }
        self.validate_patterns_only(normalized)?;
        self.validate_decimal_enumeration(value, normalized)
    }

    /// `enumeration` for an `xs:decimal`-family value, compared by value. A
    /// lexically identical member is accepted without parsing anything.
    fn validate_decimal_enumeration(
        &self,
        value: &super::value::XmlValue,
        normalized: &str,
    ) -> FacetResult<()> {
        use super::exact_decimal::ExactDecimal;
        let Some(ref enumeration) = self.enumeration else {
            return Ok(());
        };
        if enumeration.values.contains(normalized) {
            return Ok(());
        }
        let found = ExactDecimal::from_value(value).is_some_and(|exact| {
            enumeration.values.iter().any(|member| {
                ExactDecimal::parse(&normalize_whitespace(member, WhitespaceMode::Collapse))
                    .is_some_and(|m| m == exact)
            })
        });
        if found {
            Ok(())
        } else {
            Err(FacetError::enumeration(normalized))
        }
    }

    /// The digit and bound facets, given the value's digit counts, a
    /// comparison of the value with a bound literal (`None` when the literal
    /// does not parse — such a bound was rejected when the schema compiled),
    /// and the value's display form for messages.
    fn check_decimal_facets(
        &self,
        total_digits: usize,
        fraction_digits: usize,
        compare_with: impl Fn(&str) -> Option<std::cmp::Ordering>,
        display: impl Fn() -> String,
    ) -> FacetResult<()> {
        use std::cmp::Ordering::{Equal, Greater, Less};
        let saturate = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);

        if let Some(ref td) = self.total_digits {
            if total_digits > td.value as usize {
                return Err(FacetError::TotalDigitsViolation {
                    actual: saturate(total_digits),
                    max: td.value,
                });
            }
        }

        if let Some(ref fd) = self.fraction_digits {
            if fraction_digits > fd.value as usize {
                return Err(FacetError::FractionDigitsViolation {
                    actual: saturate(fraction_digits),
                    max: fd.value,
                });
            }
        }

        if let Some(ref min) = self.min_inclusive {
            if compare_with(&min.value) == Some(Less) {
                return Err(FacetError::MinInclusiveViolation {
                    value: display(),
                    min: min.value.clone(),
                });
            }
        }

        if let Some(ref max) = self.max_inclusive {
            if compare_with(&max.value) == Some(Greater) {
                return Err(FacetError::MaxInclusiveViolation {
                    value: display(),
                    max: max.value.clone(),
                });
            }
        }

        if let Some(ref min) = self.min_exclusive {
            if matches!(compare_with(&min.value), Some(Less | Equal)) {
                return Err(FacetError::MinExclusiveViolation {
                    value: display(),
                    min: min.value.clone(),
                });
            }
        }

        if let Some(ref max) = self.max_exclusive {
            if matches!(compare_with(&max.value), Some(Greater | Equal)) {
                return Err(FacetError::MaxExclusiveViolation {
                    value: display(),
                    max: max.value.clone(),
                });
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

/// The order relation in which [`FacetSet::merge_with_base_ordered`] compares
/// a lower bound with an upper bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BoundOrder {
    /// The datatype is not known: numeric literals compare as `f64`.
    Unknown,
    /// `xs:decimal` and the types derived from it: exactly, at any size and
    /// precision.
    Decimal,
    /// `xs:float`: as the `f32` values the literals denote.
    Float,
    /// `xs:double`: as the `f64` values the literals denote.
    Double,
}

impl BoundOrder {
    /// The order of the datatypes whose primitive is `primitive`; `Unknown`
    /// for the non-numeric primitives (their bounds are compared elsewhere,
    /// on typed values).
    pub(crate) fn for_primitive(primitive: Option<super::PrimitiveTypeCode>) -> Self {
        match primitive {
            Some(super::PrimitiveTypeCode::Decimal) => BoundOrder::Decimal,
            Some(super::PrimitiveTypeCode::Float) => BoundOrder::Float,
            Some(super::PrimitiveTypeCode::Double) => BoundOrder::Double,
            _ => BoundOrder::Unknown,
        }
    }

    /// The order of two bound literals, or `None` when either is not a
    /// literal of the datatype (such a bound is rejected on its own) or the
    /// two are unordered (`NaN`).
    ///
    /// Exact comparison is right only for `xs:decimal`: two `xs:double`
    /// literals that differ beyond double precision, such as `0.1` and
    /// `0.10000000000000000001`, denote the same value.
    fn compare(self, a: &str, b: &str) -> Option<std::cmp::Ordering> {
        use super::exact_decimal::ExactDecimal;
        use super::validators::{parse_double, parse_float};
        let (a, b) = (a.trim(), b.trim());
        match self {
            BoundOrder::Decimal => Some(ExactDecimal::parse(a)?.cmp(&ExactDecimal::parse(b)?)),
            BoundOrder::Float => parse_float(a).ok()?.partial_cmp(&parse_float(b).ok()?),
            BoundOrder::Double => parse_double(a).ok()?.partial_cmp(&parse_double(b).ok()?),
            BoundOrder::Unknown => {
                let a: f64 = a.parse().ok()?;
                let b: f64 = b.parse().ok()?;
                a.partial_cmp(&b)
            }
        }
    }
}

/// Apply whitespace normalization to a string.
///
/// Returns [`Cow::Borrowed`] when the input is already in the target form (the
/// common case for clean instance values) — no allocation — and
/// [`Cow::Owned`] only when a transformation is actually required. The produced
/// string is byte-identical to the previous always-allocating implementation.
pub fn normalize_whitespace(s: &str, mode: WhitespaceMode) -> Cow<'_, str> {
    match mode {
        WhitespaceMode::Preserve => Cow::Borrowed(s),
        WhitespaceMode::Replace => replace_whitespace(s),
        WhitespaceMode::Collapse => collapse_whitespace(s),
    }
}

/// `true` for exactly the four XSD whitespace characters #x20 #x9 #xD #xA.
///
/// Deliberately byte-level and *not* [`char::is_whitespace`] / [`str::trim`] /
/// [`str::split_whitespace`]: those match Unicode whitespace (U+00A0, U+2028, …),
/// which XSD `whiteSpace` must leave untouched. All four are ASCII, so trimming
/// or splitting on them never lands inside a multi-byte UTF-8 sequence.
#[inline]
fn is_xsd_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n')
}

/// `replace` whitespace normalization: each #x9/#xD/#xA becomes #x20 (a #x20 is
/// already itself). Borrows when there is nothing to replace; otherwise allocates
/// once and bulk-copies the unchanged runs around each replaced byte.
fn replace_whitespace(s: &str) -> Cow<'_, str> {
    let bytes = s.as_bytes();
    if !bytes.iter().any(|&b| matches!(b, b'\t' | b'\r' | b'\n')) {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut chunk_start = 0;
    for (i, &b) in bytes.iter().enumerate() {
        if matches!(b, b'\t' | b'\r' | b'\n') {
            out.push_str(&s[chunk_start..i]); // unchanged run before this byte
            out.push(' ');
            chunk_start = i + 1;
        }
    }
    out.push_str(&s[chunk_start..]); // trailing unchanged run
    Cow::Owned(out)
}

/// `collapse` whitespace normalization: `replace`, then squeeze each run of
/// whitespace to a single #x20, then strip leading/trailing whitespace.
///
/// Two no-alloc fast paths cover the common shapes:
/// * all-whitespace (or empty) → the empty string;
/// * a trimmed core that needs no internal rewriting (no #x9/#xD/#xA and no
///   double whitespace) → a borrowed sub-slice, so `" 123 "` / `"\nfoo\t"`
///   borrow instead of allocating.
///
/// Only genuinely dirty input (internal tabs/newlines or multi-space runs)
/// allocates, and then the rewrite copies whole words with `push_str` rather
/// than one `char` at a time.
fn collapse_whitespace(s: &str) -> Cow<'_, str> {
    let bytes = s.as_bytes();
    // Trim XSD whitespace from both ends. The cut points are ASCII bytes, hence
    // always char boundaries.
    let Some(start) = bytes.iter().position(|&b| !is_xsd_ws(b)) else {
        return Cow::Borrowed(""); // all whitespace (or empty) collapses to ""
    };
    let end = bytes.iter().rposition(|&b| !is_xsd_ws(b)).unwrap() + 1;
    let mid = &s[start..end];

    // Borrow the trimmed core unless it needs internal rewriting: any #x9/#xD/#xA
    // (which `replace` would turn into a space) or any run of two+ whitespace
    // (which `collapse` would squeeze). The core has no leading/trailing ws.
    let mut prev_ws = false;
    let mut needs_rewrite = false;
    for &b in mid.as_bytes() {
        match b {
            b'\t' | b'\r' | b'\n' => {
                needs_rewrite = true;
                break;
            }
            b' ' => {
                if prev_ws {
                    needs_rewrite = true;
                    break;
                }
                prev_ws = true;
            }
            _ => prev_ws = false,
        }
    }
    if !needs_rewrite {
        return Cow::Borrowed(mid);
    }

    // Rewrite: emit each non-whitespace word, separated by a single space. `mid`
    // is already trimmed, so there is exactly one space between words and none at
    // the ends — byte-identical to the previous char-by-char implementation.
    let mut out = String::with_capacity(mid.len());
    let mbytes = mid.as_bytes();
    let mut i = 0;
    let mut first_word = true;
    while i < mbytes.len() {
        while i < mbytes.len() && is_xsd_ws(mbytes[i]) {
            i += 1; // skip a whitespace run
        }
        if i >= mbytes.len() {
            break;
        }
        if !first_word {
            out.push(' ');
        }
        first_word = false;
        let word_start = i;
        while i < mbytes.len() && !is_xsd_ws(mbytes[i]) {
            i += 1;
        }
        out.push_str(&mid[word_start..i]);
    }
    Cow::Owned(out)
}

/// The `totalDigits` a decimal value needs (Datatypes 1.1 §4.3.11.3): the
/// value is `i / 10^n` with `|i| < 10^t` and `0 ≤ n ≤ t`. With the value
/// normalized to the mantissa `i` and scale `n` (no trailing zeros), that is
/// the larger of the digit count of `i` and `n` — `0.12345 = 12345 × 10^-5`
/// needs five digits (the leading `0` of the lexical form is not one of them),
/// and `0.05 = 5 × 10^-2` needs two.
fn count_total_digits(value: &rust_decimal::Decimal) -> u32 {
    let normalized = value.normalize();
    let mut m = normalized.mantissa().unsigned_abs();
    let mut count = 0u32;
    while m > 0 {
        count += 1;
        m /= 10;
    }
    count.max(normalized.scale()).max(1)
}

/// The `fractionDigits` a decimal value needs (Datatypes 1.1 §4.3.12.3): the
/// scale of the normalized value.
fn count_fraction_digits(value: &rust_decimal::Decimal) -> u32 {
    value.normalize().scale()
}

/// An `xs:integer` value as a `rust_decimal::Decimal`, when it fits.
fn integer_as_decimal(value: &num_bigint::BigInt) -> Option<rust_decimal::Decimal> {
    match i64::try_from(value) {
        Ok(small) => Some(rust_decimal::Decimal::from(small)),
        Err(_) => value.to_string().parse().ok(),
    }
}

/// The order of a decimal value relative to a bound literal, exactly — also
/// when the bound is beyond what `rust_decimal::Decimal` holds. `None` when
/// the literal is not an `xs:decimal` lexical form.
fn compare_decimal_with_bound(
    value: &rust_decimal::Decimal,
    bound: &str,
) -> Option<std::cmp::Ordering> {
    if let Ok(bound) = rust_decimal::Decimal::from_str_exact(bound) {
        return Some(value.cmp(&bound));
    }
    let bound = super::exact_decimal::ExactDecimal::parse(&normalize_whitespace(
        bound,
        WhitespaceMode::Collapse,
    ))?;
    Some(super::exact_decimal::ExactDecimal::from_decimal(value).cmp(&bound))
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
            .add_pattern(
                "[a-z]+".to_string(),
                None,
                XsdVersion::V1_1,
                RegexCompat::Strict,
            )
            .unwrap();
        facets
            .add_pattern(
                "[0-9]+".to_string(),
                None,
                XsdVersion::V1_1,
                RegexCompat::Strict,
            )
            .unwrap();

        // Two adds within one FacetSet share the same derivation step (OR'd).
        assert_eq!(facets.patterns.len(), 1);
        assert_eq!(facets.patterns[0].len(), 2);
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
        base.add_pattern(
            "[a-z]+".to_string(),
            None,
            XsdVersion::V1_1,
            RegexCompat::Strict,
        )
        .unwrap();

        let mut derived = FacetSet::new();
        derived.set_max_length(50, FacetFixed::Default, None); // Override

        derived.inherit_from(&base);

        // minLength inherited
        assert_eq!(derived.min_length.as_ref().unwrap().value, 5);
        // maxLength not inherited (was overridden)
        assert_eq!(derived.max_length.as_ref().unwrap().value, 50);
        // Base step inherited as a separate step entry.
        assert_eq!(derived.patterns.len(), 1);
        assert_eq!(derived.patterns[0].len(), 1);
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
        let pattern = PatternFacet::new(
            "[a-z]+".to_string(),
            None,
            XsdVersion::V1_1,
            RegexCompat::Strict,
        )
        .unwrap();
        assert!(pattern.matches("hello"));
        assert!(!pattern.matches("HELLO"));
        assert!(!pattern.matches("hello123"));
    }

    #[test]
    fn test_pattern_cache_reuse_and_clear() {
        // Compile the same category-using pattern twice: the second compile
        // should be served from the regex cache (xsd11) yet behave identically.
        let mk = || {
            PatternFacet::new(
                r"\d{3}-\d{4}".to_string(),
                None,
                XsdVersion::V1_1,
                RegexCompat::Strict,
            )
            .unwrap()
        };
        let a = mk();
        let b = mk(); // cache hit under xsd11; full recompile otherwise
        for p in [&a, &b] {
            assert!(p.matches("123-4567"));
            assert!(!p.matches("12-4567"));
            assert!(!p.matches("123-456"));
        }
        // Clearing must not corrupt anything: a fresh compile still works.
        crate::clear_pattern_cache();
        let c = mk();
        assert!(c.matches("123-4567"));
        assert!(!c.matches("xx-yyyy"));
    }

    #[test]
    fn test_pattern_xsd_anchoring() {
        // XSD patterns are implicitly anchored
        let pattern = PatternFacet::new(
            "abc".to_string(),
            None,
            XsdVersion::V1_1,
            RegexCompat::Strict,
        )
        .unwrap();
        assert!(pattern.matches("abc"));
        assert!(!pattern.matches("xabc"));
        assert!(!pattern.matches("abcx"));
    }

    #[test]
    fn test_pattern_xsd_name_chars() {
        // Test \i (initial name char) and \c (name char)
        let pattern = PatternFacet::new(
            r"\i\c*".to_string(),
            None,
            XsdVersion::V1_1,
            RegexCompat::Strict,
        )
        .unwrap();
        assert!(pattern.matches("foo"));
        assert!(pattern.matches("_bar"));
        assert!(pattern.matches("x123"));
        assert!(!pattern.matches("123"));
    }

    #[test]
    fn test_invalid_pattern() {
        let result = PatternFacet::new(
            "[invalid".to_string(),
            None,
            XsdVersion::V1_1,
            RegexCompat::Strict,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_pattern_compiles_independently_of_xpath_dialect_gate() {
        // Pattern facets compile through the regex backend directly and wrap
        // their own value in `^(?:…)$`. The XPath function layer rejects a
        // pattern containing `(?` (its grammar has no such atom), which must
        // not leak into facet compilation. A facet whose own value contains a
        // group with an alternation exercises exactly the wrapper that would
        // be rejected there.
        let pattern = PatternFacet::new(
            "(abc|def)+".to_string(),
            None,
            XsdVersion::V1_1,
            RegexCompat::Strict,
        )
        .unwrap();
        assert!(pattern.matches("abcdef"));
        assert!(!pattern.matches("abcx"));

        // Same under XSD 1.0 and under the lenient grammar gate.
        let pattern = PatternFacet::new(
            "(a|b)*".to_string(),
            None,
            XsdVersion::V1_0,
            RegexCompat::LenientMs,
        )
        .unwrap();
        assert!(pattern.matches("abba"));
        assert!(!pattern.matches("abc"));
    }

    // =========================================================================
    // Whitespace normalization tests
    // =========================================================================

    #[test]
    fn test_whitespace_preserve() {
        let result = normalize_whitespace("  hello\t\nworld  ", WhitespaceMode::Preserve);
        assert_eq!(&*result, "  hello\t\nworld  ");
    }

    #[test]
    fn test_whitespace_replace() {
        let result = normalize_whitespace("  hello\t\nworld  ", WhitespaceMode::Replace);
        assert_eq!(&*result, "  hello  world  ");
    }

    #[test]
    fn test_whitespace_collapse() {
        let result = normalize_whitespace("  hello\t\nworld  ", WhitespaceMode::Collapse);
        assert_eq!(&*result, "hello world");
    }

    #[test]
    fn test_whitespace_collapse_multiple_spaces() {
        let result = normalize_whitespace("a     b", WhitespaceMode::Collapse);
        assert_eq!(&*result, "a b");
    }

    #[test]
    fn test_whitespace_borrows_when_already_normalized() {
        // Preserve always borrows.
        assert!(matches!(
            normalize_whitespace("anything\t\n", WhitespaceMode::Preserve),
            Cow::Borrowed(_)
        ));
        // Replace borrows when there is no tab/CR/LF to replace.
        assert!(matches!(
            normalize_whitespace("no special ws", WhitespaceMode::Replace),
            Cow::Borrowed(_)
        ));
        // Collapse borrows when already collapsed (no edge/double spaces, no \t\r\n).
        assert!(matches!(
            normalize_whitespace("a b c", WhitespaceMode::Collapse),
            Cow::Borrowed(_)
        ));
        // Collapse also borrows the trimmed core when only leading/trailing XSD
        // whitespace differs — the core needs no internal rewriting.
        assert!(matches!(
            normalize_whitespace(" leading", WhitespaceMode::Collapse),
            Cow::Borrowed(_)
        ));
        assert!(matches!(
            normalize_whitespace("trailing ", WhitespaceMode::Collapse),
            Cow::Borrowed(_)
        ));
        assert!(matches!(
            normalize_whitespace("\n a b \t", WhitespaceMode::Collapse),
            Cow::Borrowed(_)
        ));
        // …and these still produce the correct collapsed value.
        assert_eq!(
            &*normalize_whitespace(" leading", WhitespaceMode::Collapse),
            "leading"
        );
        assert_eq!(
            &*normalize_whitespace("\n a b \t", WhitespaceMode::Collapse),
            "a b"
        );
        // Whitespace-only / empty collapse to the empty string, also borrowed.
        assert!(matches!(
            normalize_whitespace("   \t\n", WhitespaceMode::Collapse),
            Cow::Borrowed(_)
        ));
        assert_eq!(
            &*normalize_whitespace("   \t\n", WhitespaceMode::Collapse),
            ""
        );
    }

    #[test]
    fn test_whitespace_owns_when_transformation_needed() {
        // Replace owns when a tab/CR/LF must become a space.
        assert!(matches!(
            normalize_whitespace("has\ttab", WhitespaceMode::Replace),
            Cow::Owned(_)
        ));
        // Collapse owns only when the *core* needs rewriting: an internal
        // double space or an internal \t\r\n. (Leading/trailing-only whitespace
        // now borrows the trimmed core — see the borrow test above.)
        assert!(matches!(
            normalize_whitespace("a  b", WhitespaceMode::Collapse),
            Cow::Owned(_)
        ));
        assert!(matches!(
            normalize_whitespace("tab\there", WhitespaceMode::Collapse),
            Cow::Owned(_)
        ));
        // Owns when there are both edge whitespace and an internal run to squeeze.
        assert!(matches!(
            normalize_whitespace(" a  b ", WhitespaceMode::Collapse),
            Cow::Owned(_)
        ));
        assert_eq!(
            &*normalize_whitespace(" a  b ", WhitespaceMode::Collapse),
            "a b"
        );
    }

    #[test]
    fn test_whitespace_only_xsd_whitespace_not_unicode() {
        // XSD whiteSpace is exactly #x20 #x9 #xD #xA. Unicode whitespace such as
        // NBSP (U+00A0) or LINE SEPARATOR (U+2028) must be left untouched — both
        // as content and at the edges (str::trim / split_whitespace would eat it).
        let nbsp = "\u{00A0}"; // not XSD whitespace

        let collapse_in = format!(" a{nbsp}{nbsp}b ");
        let collapsed = normalize_whitespace(&collapse_in, WhitespaceMode::Collapse);
        assert_eq!(&*collapsed, format!("a{nbsp}{nbsp}b")); // edges trimmed, NBSP kept (incl. the run)
        assert!(matches!(collapsed, Cow::Borrowed(_))); // NBSP is not ws, so the core borrows

        // A leading NBSP is content, so nothing is trimmed and the whole thing borrows.
        let lead_in = format!("{nbsp}x");
        let lead = normalize_whitespace(&lead_in, WhitespaceMode::Collapse);
        assert_eq!(&*lead, format!("{nbsp}x"));
        assert!(matches!(lead, Cow::Borrowed(_)));

        // Replace only maps #x9/#xD/#xA → #x20; NBSP and multi-byte content survive.
        let repl_in = format!("café\tx{nbsp}y");
        let repl = normalize_whitespace(&repl_in, WhitespaceMode::Replace);
        assert_eq!(&*repl, format!("café x{nbsp}y"));
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
            .add_pattern(
                "[a-z]+".to_string(),
                None,
                XsdVersion::V1_1,
                RegexCompat::Strict,
            )
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

    /// Datatypes 1.1 §4.3.11.3: "|i| < 10^t and 0 ≤ n ≤ t" — `0.05` is
    /// `5 / 10^2`, so it needs two total digits, not one.
    #[test]
    fn test_validate_decimal_total_digits_counts_leading_fraction_zeros() {
        let mut facets = FacetSet::new();
        facets.set_total_digits(1, FacetFixed::Default, None);
        assert!(facets
            .validate_decimal(&Decimal::from_str("0.05").unwrap())
            .is_err());
        assert!(facets
            .validate_decimal(&Decimal::from_str("0.5").unwrap())
            .is_ok());
        assert!(facets
            .validate_decimal(&Decimal::from_str("-0.500").unwrap())
            .is_ok());
        assert!(facets.validate_decimal(&Decimal::ZERO).is_ok());
        facets.set_total_digits(2, FacetFixed::Default, None);
        assert!(facets
            .validate_decimal(&Decimal::from_str("0.05").unwrap())
            .is_ok());
    }

    /// Bounds beyond what `Decimal` holds still bind (0.2.2 skipped a bound
    /// that `Decimal::from_str_exact` refused).
    #[test]
    fn test_validate_decimal_with_bounds_beyond_capacity() {
        let huge = "100000000000000000000000000000000000000";
        let mut facets = FacetSet::new();
        facets.set_min_inclusive(huge.into(), FacetFixed::Default, None);
        assert!(facets.validate_decimal(&Decimal::MAX).is_err());

        let mut facets = FacetSet::new();
        facets.set_max_inclusive(huge.into(), FacetFixed::Default, None);
        assert!(facets.validate_decimal(&Decimal::MAX).is_ok());
        facets.set_max_inclusive(format!("-{huge}"), FacetFixed::Default, None);
        assert!(facets.validate_decimal(&Decimal::MIN).is_err());

        // 1 − 10^-32: more fraction digits than `Decimal` holds.
        let below_one = "0.99999999999999999999999999999999";
        let mut facets = FacetSet::new();
        facets.set_min_exclusive(below_one.into(), FacetFixed::Default, None);
        assert!(facets.validate_decimal(&Decimal::ONE).is_ok());
        let mut facets = FacetSet::new();
        facets.set_max_exclusive(below_one.into(), FacetFixed::Default, None);
        assert!(facets.validate_decimal(&Decimal::ONE).is_err());
        // Trailing zeros past 28 places do not change the bound's value.
        let mut facets = FacetSet::new();
        facets.set_max_inclusive(
            "1.000000000000000000000000000000000".into(),
            FacetFixed::Default,
            None,
        );
        assert!(facets.validate_decimal(&Decimal::ONE).is_ok());
    }

    /// Datatypes 1.1 §4.3.7–§4.3.10: a lower bound above an upper bound is
    /// an error, "according to the datatype's order relation" — exact for
    /// `xs:decimal`, in the float or double value space for those types.
    #[test]
    fn test_bound_consistency_in_the_datatype_value_space() {
        let bounds = |min: &str, max: &str| {
            let mut facets = FacetSet::new();
            facets.set_min_inclusive(min.into(), FacetFixed::Default, None);
            facets.set_max_inclusive(max.into(), FacetFixed::Default, None);
            facets
        };
        let base = FacetSet::new();
        let consistent =
            |facets: &FacetSet, order| facets.merge_with_base_ordered(&base, order).is_ok();
        // The same double, spelled two ways.
        let dbl = bounds("0.10000000000000000001", "0.1");
        assert!(consistent(&dbl, BoundOrder::Double));
        assert!(consistent(&dbl, BoundOrder::Unknown));
        assert!(!consistent(&dbl, BoundOrder::Decimal));
        // The same float, but two doubles.
        let flt = bounds("0.1000000001", "0.1");
        assert!(consistent(&flt, BoundOrder::Float));
        assert!(!consistent(&flt, BoundOrder::Double));
        // Decimals beyond double precision.
        let dec = bounds(
            "1.00000000000000000000000000002",
            "1.00000000000000000000000000001",
        );
        assert!(!consistent(&dec, BoundOrder::Decimal));
        assert!(consistent(&dec, BoundOrder::Double));
        // Genuine conflicts in every order; special values in float/double.
        for order in [
            BoundOrder::Decimal,
            BoundOrder::Float,
            BoundOrder::Double,
            BoundOrder::Unknown,
        ] {
            assert!(!consistent(&bounds("0.2", "0.1"), order), "{order:?}");
            assert!(consistent(&bounds("0.1", "0.2"), order), "{order:?}");
        }
        assert!(consistent(&bounds("-INF", "INF"), BoundOrder::Double));
        assert!(!consistent(&bounds("INF", "-INF"), BoundOrder::Float));
        // The public method keeps comparing as f64.
        assert!(dbl.merge_with_base(&base).is_ok());
        assert!(dec.merge_with_base(&base).is_ok());
    }

    #[test]
    fn test_validate_exact_decimal() {
        use crate::types::exact_decimal::ExactDecimal;
        let x = |s: &str| ExactDecimal::parse(s).unwrap();
        let mut facets = FacetSet::new();
        facets.set_fraction_digits(28, FacetFixed::Default, None);
        facets.set_min_exclusive("1".into(), FacetFixed::Default, None);
        assert!(facets
            .validate_exact_decimal(&x("1.0000000000000000000000000001"))
            .is_ok());
        assert!(matches!(
            facets.validate_exact_decimal(&x("1.00000000000000000000000000001")),
            Err(FacetError::FractionDigitsViolation {
                actual: 29,
                max: 28
            })
        ));
        assert!(matches!(
            facets.validate_exact_decimal(&x("1.000")),
            Err(FacetError::MinExclusiveViolation { .. })
        ));
    }

    #[test]
    fn test_validate_decimal_family_enumeration_by_value() {
        use crate::types::exact_decimal::decimal_typed_value;
        let mut facets = FacetSet::new();
        facets.add_enumeration("1.0".to_string(), None);
        facets.add_enumeration("100000000000000000000000000000000000000".to_string(), None);
        let check =
            |lexical: &str| facets.validate_decimal_family(&decimal_typed_value(lexical), lexical);
        assert!(check("1.0").is_ok());
        assert!(check("1.00").is_ok());
        assert!(check("+1").is_ok());
        assert!(check("100000000000000000000000000000000000000.000").is_ok());
        assert!(check("1.1").is_err());
        assert!(check("100000000000000000000000000000000000001").is_err());
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
        base.add_pattern(
            "[a-z]+".to_string(),
            None,
            XsdVersion::V1_1,
            RegexCompat::Strict,
        )
        .unwrap();

        let mut derived = FacetSet::new();
        derived
            .add_pattern(
                "[0-9]+".to_string(),
                None,
                XsdVersion::V1_1,
                RegexCompat::Strict,
            )
            .unwrap();

        let merged = derived.merge_with_base(&base).unwrap();
        // Derived step (one OR'd pattern) AND base step (one OR'd pattern)
        // = two separate AND'd steps.
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
