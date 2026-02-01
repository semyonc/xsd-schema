//! XPath 2.0 function registry and dispatch.
//!
//! This module provides:
//! - `FunctionId` - Enum identifying all built-in XPath functions
//! - `XPathValue` - Result type for function evaluation
//! - Helper functions for argument atomization and conversion
//! - `eval_function` - Main dispatch function
//!
//! ## Architecture
//!
//! Functions are identified by `FunctionId` which allows non-generic registry
//! lookup at bind time. Function dispatch uses a match on `FunctionId` to call
//! the appropriate implementation.

pub mod signature;
pub mod registry;
pub mod string;
pub mod numeric;
pub mod sequence;
pub mod aggregate;
pub mod node;
pub mod datetime;
pub mod qname;
pub mod uri;
pub mod regex;
pub mod special;

pub use signature::{FunctionArity, FunctionSignature, FN_NAMESPACE, FN_2010_NAMESPACE};
pub use registry::{FunctionRegistry, FunctionEntry, FunctionKey, FUNCTION_REGISTRY};

use num_bigint::BigInt;

use crate::types::value::XmlValue;
use crate::xpath::error::XPathError;
use crate::xpath::iterator::XmlItem;
use crate::xpath::atomize;
use crate::xpath::DomNavigator;

use super::context::DynamicContext;

/// XPath function identifiers.
///
/// Each variant corresponds to a built-in XPath 2.0 function.
/// This enum allows bind-time function resolution without generic type parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum FunctionId {
    // ========== String Functions ==========
    /// fn:concat($arg1, $arg2, ...)
    Concat = 1,
    /// fn:string-join($arg1, $arg2)
    StringJoin,
    /// fn:substring($sourceString, $start, $length?)
    Substring,
    /// fn:string-length($arg?)
    StringLength,
    /// fn:normalize-space($arg?)
    NormalizeSpace,
    /// fn:normalize-unicode($arg, $normalizationForm?)
    NormalizeUnicode,
    /// fn:upper-case($arg)
    UpperCase,
    /// fn:lower-case($arg)
    LowerCase,
    /// fn:translate($arg, $mapString, $transString)
    Translate,
    /// fn:encode-for-uri($uri-part)
    EncodeForUri,
    /// fn:iri-to-uri($iri)
    IriToUri,
    /// fn:escape-html-uri($uri)
    EscapeHtmlUri,
    /// fn:contains($arg1, $arg2, $collation?)
    Contains,
    /// fn:starts-with($arg1, $arg2, $collation?)
    StartsWith,
    /// fn:ends-with($arg1, $arg2, $collation?)
    EndsWith,
    /// fn:substring-before($arg1, $arg2, $collation?)
    SubstringBefore,
    /// fn:substring-after($arg1, $arg2, $collation?)
    SubstringAfter,
    /// fn:string-to-codepoints($arg)
    StringToCodepoints,
    /// fn:codepoints-to-string($arg)
    CodepointsToString,
    /// fn:compare($comparand1, $comparand2, $collation?)
    Compare,
    /// fn:codepoint-equal($comparand1, $comparand2)
    CodepointEqual,

    // ========== Numeric Functions ==========
    /// fn:abs($arg)
    Abs = 100,
    /// fn:ceiling($arg)
    Ceiling,
    /// fn:floor($arg)
    Floor,
    /// fn:round($arg)
    Round,
    /// fn:round-half-to-even($arg, $precision?)
    RoundHalfToEven,

    // ========== Sequence Functions ==========
    /// fn:empty($arg)
    Empty = 200,
    /// fn:exists($arg)
    Exists,
    /// fn:reverse($arg)
    Reverse,
    /// fn:index-of($seq, $search, $collation?)
    IndexOf,
    /// fn:remove($target, $position)
    Remove,
    /// fn:insert-before($target, $position, $inserts)
    InsertBefore,
    /// fn:subsequence($sourceSeq, $startingLoc, $length?)
    Subsequence,
    /// fn:unordered($sourceSeq)
    Unordered,
    /// fn:zero-or-one($arg)
    ZeroOrOne,
    /// fn:one-or-more($arg)
    OneOrMore,
    /// fn:exactly-one($arg)
    ExactlyOne,
    /// fn:distinct-values($arg, $collation?)
    DistinctValues,
    /// fn:deep-equal($parameter1, $parameter2, $collation?)
    DeepEqual,
    /// fn:count($arg)
    Count,

    // ========== Aggregate Functions ==========
    /// fn:sum($arg, $zero?)
    Sum = 300,
    /// fn:avg($arg)
    Avg,
    /// fn:min($arg, $collation?)
    Min,
    /// fn:max($arg, $collation?)
    Max,

    // ========== Node Functions ==========
    /// fn:name($arg?)
    Name = 400,
    /// fn:local-name($arg?)
    LocalName,
    /// fn:namespace-uri($arg?)
    NamespaceUri,
    /// fn:node-name($arg?)
    NodeName,
    /// fn:nilled($arg)
    Nilled,
    /// fn:base-uri($arg?)
    BaseUri,
    /// fn:document-uri($arg)
    DocumentUri,
    /// fn:lang($testlang, $node?)
    Lang,
    /// fn:root($arg?)
    Root,

    // ========== DateTime Functions ==========
    /// fn:dateTime($arg1, $arg2)
    DateTime = 500,
    /// fn:current-dateTime()
    CurrentDateTime,
    /// fn:current-date()
    CurrentDate,
    /// fn:current-time()
    CurrentTime,
    /// fn:implicit-timezone()
    ImplicitTimezone,
    /// fn:years-from-duration($arg)
    YearsFromDuration,
    /// fn:months-from-duration($arg)
    MonthsFromDuration,
    /// fn:days-from-duration($arg)
    DaysFromDuration,
    /// fn:hours-from-duration($arg)
    HoursFromDuration,
    /// fn:minutes-from-duration($arg)
    MinutesFromDuration,
    /// fn:seconds-from-duration($arg)
    SecondsFromDuration,
    /// fn:year-from-dateTime($arg)
    YearFromDateTime,
    /// fn:month-from-dateTime($arg)
    MonthFromDateTime,
    /// fn:day-from-dateTime($arg)
    DayFromDateTime,
    /// fn:hours-from-dateTime($arg)
    HoursFromDateTime,
    /// fn:minutes-from-dateTime($arg)
    MinutesFromDateTime,
    /// fn:seconds-from-dateTime($arg)
    SecondsFromDateTime,
    /// fn:timezone-from-dateTime($arg)
    TimezoneFromDateTime,
    /// fn:year-from-date($arg)
    YearFromDate,
    /// fn:month-from-date($arg)
    MonthFromDate,
    /// fn:day-from-date($arg)
    DayFromDate,
    /// fn:timezone-from-date($arg)
    TimezoneFromDate,
    /// fn:hours-from-time($arg)
    HoursFromTime,
    /// fn:minutes-from-time($arg)
    MinutesFromTime,
    /// fn:seconds-from-time($arg)
    SecondsFromTime,
    /// fn:timezone-from-time($arg)
    TimezoneFromTime,
    /// fn:adjust-dateTime-to-timezone($arg, $timezone?)
    AdjustDateTimeToTimezone,
    /// fn:adjust-date-to-timezone($arg, $timezone?)
    AdjustDateToTimezone,
    /// fn:adjust-time-to-timezone($arg, $timezone?)
    AdjustTimeToTimezone,

    // ========== QName Functions ==========
    /// fn:resolve-QName($qname, $element)
    ResolveQName = 600,
    /// fn:QName($paramURI, $paramLocal)
    QName,
    /// fn:prefix-from-QName($arg)
    PrefixFromQName,
    /// fn:local-name-from-QName($arg)
    LocalNameFromQName,
    /// fn:namespace-uri-from-QName($arg)
    NamespaceUriFromQName,
    /// fn:namespace-uri-for-prefix($prefix, $element)
    NamespaceUriForPrefix,
    /// fn:in-scope-prefixes($element)
    InScopePrefixes,

    // ========== URI Functions ==========
    /// fn:resolve-uri($relative, $base?)
    ResolveUri = 700,
    /// fn:static-base-uri()
    StaticBaseUri,

    // ========== Regex Functions ==========
    /// fn:matches($input, $pattern, $flags?)
    Matches = 800,
    /// fn:replace($input, $pattern, $replacement, $flags?)
    Replace,
    /// fn:tokenize($input, $pattern, $flags?)
    Tokenize,

    // ========== Special/Context Functions ==========
    /// fn:position()
    Position = 900,
    /// fn:last()
    Last,
    /// fn:trace($value, $label?)
    Trace,
    /// fn:data($arg)
    Data,
    /// fn:default-collation()
    DefaultCollation,

    // ========== Boolean Functions ==========
    /// fn:true()
    True = 1000,
    /// fn:false()
    False,
    /// fn:not($arg)
    Not,
    /// fn:boolean($arg)
    Boolean,

    // ========== Conversion Functions ==========
    /// fn:string($arg?)
    String = 1100,
    /// fn:number($arg?)
    Number,
}

/// XPath value representing a function result.
///
/// This enum represents the result of evaluating an XPath expression or function.
/// It can be empty, a single item, or a sequence of items.
#[derive(Debug, Clone)]
pub enum XPathValue<N: DomNavigator> {
    /// Empty sequence
    Empty,
    /// Single item (node or atomic value)
    Item(XmlItem<N>),
    /// Sequence of items (materialized)
    Sequence(Vec<XmlItem<N>>),
}

impl<N: DomNavigator> XPathValue<N> {
    /// Create an empty value
    pub fn empty() -> Self {
        Self::Empty
    }

    /// Create a value from a single item
    pub fn from_item(item: XmlItem<N>) -> Self {
        Self::Item(item)
    }

    /// Create a value from an atomic XmlValue
    pub fn from_atomic(value: XmlValue) -> Self {
        Self::Item(XmlItem::Atomic(value))
    }

    /// Create a value from a node
    pub fn from_node(node: N) -> Self {
        Self::Item(XmlItem::Node(node))
    }

    /// Create a value from a sequence of items
    pub fn from_sequence(items: Vec<XmlItem<N>>) -> Self {
        match items.len() {
            0 => Self::Empty,
            1 => Self::Item(items.into_iter().next().unwrap()),
            _ => Self::Sequence(items),
        }
    }

    /// Create a boolean value
    pub fn boolean(b: bool) -> Self {
        Self::from_atomic(XmlValue::boolean(b))
    }

    /// Create a string value
    pub fn string(s: impl Into<String>) -> Self {
        Self::from_atomic(XmlValue::string(s))
    }

    /// Create an integer value
    pub fn integer(i: impl Into<num_bigint::BigInt>) -> Self {
        Self::from_atomic(XmlValue::integer(i.into()))
    }

    /// Create a double value
    pub fn double(d: f64) -> Self {
        Self::from_atomic(XmlValue::double(d))
    }

    /// Check if this value is empty
    pub fn is_empty(&self) -> bool {
        matches!(self, Self::Empty)
    }

    /// Get the count of items
    pub fn len(&self) -> usize {
        match self {
            Self::Empty => 0,
            Self::Item(_) => 1,
            Self::Sequence(items) => items.len(),
        }
    }

    /// Check if this is a single item
    pub fn is_single(&self) -> bool {
        matches!(self, Self::Item(_))
    }

    /// Convert to a Vec of items
    pub fn into_vec(self) -> Vec<XmlItem<N>> {
        match self {
            Self::Empty => Vec::new(),
            Self::Item(item) => vec![item],
            Self::Sequence(items) => items,
        }
    }

    /// Get a reference to items as a slice
    pub fn as_slice(&self) -> &[XmlItem<N>] {
        match self {
            Self::Empty => &[],
            Self::Item(_) => {
                // Can't return a slice to a single owned item safely
                // This is a limitation - callers should use into_vec() for this case
                &[]
            }
            Self::Sequence(items) => items,
        }
    }

    /// Try to get the first item
    pub fn first(&self) -> Option<&XmlItem<N>> {
        match self {
            Self::Empty => None,
            Self::Item(item) => Some(item),
            Self::Sequence(items) => items.first(),
        }
    }

    // ========================================================================
    // Atomic Value Extraction Methods
    // ========================================================================

    /// Try to extract a string from a single atomic item.
    ///
    /// Returns `None` if:
    /// - The value is empty
    /// - The value is a sequence
    /// - The item is a node (not atomic)
    /// - The atomic value is not a string type
    pub fn as_str(&self) -> Option<String> {
        match self {
            Self::Item(XmlItem::Atomic(v)) => v.as_string().map(|s| s.to_string()),
            _ => None,
        }
    }

    /// Try to extract a boolean from a single atomic item.
    ///
    /// Returns `None` if the value is not a single atomic boolean.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Item(XmlItem::Atomic(v)) => v.as_boolean(),
            _ => None,
        }
    }

    /// Try to extract a double from a single atomic item.
    ///
    /// Returns `None` if the value is not a single atomic numeric value.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Item(XmlItem::Atomic(v)) => v.as_double(),
            _ => None,
        }
    }

    /// Try to extract an integer from a single atomic item.
    ///
    /// Returns `None` if the value is not a single atomic integer.
    pub fn as_integer(&self) -> Option<num_bigint::BigInt> {
        match self {
            Self::Item(XmlItem::Atomic(v)) => v.as_integer().cloned(),
            _ => None,
        }
    }
}

// ============================================================================
// Helper Functions for Argument Processing
// ============================================================================

/// Atomize a value and convert to string.
///
/// This handles:
/// - Empty value -> empty string
/// - Single item -> atomized string value
/// - Sequence -> error (XPTY0004)
pub fn atomize_to_string<N: DomNavigator>(value: XPathValue<N>) -> Result<String, XPathError> {
    match value {
        XPathValue::Empty => Ok(String::new()),
        XPathValue::Item(item) => item_to_string(item),
        XPathValue::Sequence(items) => {
            if items.len() == 1 {
                item_to_string(items.into_iter().next().unwrap())
            } else {
                Err(XPathError::more_than_one_item())
            }
        }
    }
}

/// Atomize a value and convert to required string.
///
/// Returns error if the value is empty or contains more than one item.
pub fn atomize_to_string_required<N: DomNavigator>(value: XPathValue<N>) -> Result<String, XPathError> {
    match value {
        XPathValue::Empty => Err(XPathError::XPTY0004 {
            expected: "xs:string".to_string(),
            found: "empty-sequence()".to_string(),
        }),
        other => atomize_to_string(other),
    }
}

/// Atomize a value and convert to optional string.
///
/// Returns None for empty sequences.
pub fn atomize_to_string_opt<N: DomNavigator>(value: XPathValue<N>) -> Result<Option<String>, XPathError> {
    match value {
        XPathValue::Empty => Ok(None),
        other => atomize_to_string(other).map(Some),
    }
}

/// Convert an XmlItem to string
fn item_to_string<N: DomNavigator>(item: XmlItem<N>) -> Result<String, XPathError> {
    match item {
        XmlItem::Atomic(value) => Ok(atomize::string_value(&value)),
        XmlItem::Node(nav) => Ok(nav.value()),
    }
}

/// Atomize a value and convert to double.
///
/// This handles:
/// - Empty value -> NaN
/// - Single item -> atomized double value
/// - Sequence -> error (XPTY0004)
pub fn atomize_to_double<N: DomNavigator>(value: XPathValue<N>) -> Result<f64, XPathError> {
    match value {
        XPathValue::Empty => Ok(f64::NAN),
        XPathValue::Item(item) => item_to_double(item),
        XPathValue::Sequence(items) => {
            if items.len() == 1 {
                item_to_double(items.into_iter().next().unwrap())
            } else {
                Err(XPathError::more_than_one_item())
            }
        }
    }
}

/// Convert an XmlItem to double
fn item_to_double<N: DomNavigator>(item: XmlItem<N>) -> Result<f64, XPathError> {
    match item {
        XmlItem::Atomic(value) => Ok(atomize::to_number(&value)),
        XmlItem::Node(nav) => {
            let s = nav.value();
            Ok(s.trim().parse().unwrap_or(f64::NAN))
        }
    }
}

/// Atomize a value to a single XmlValue.
///
/// Returns error if the value is empty or contains more than one item.
pub fn atomize_to_single<N: DomNavigator>(value: XPathValue<N>) -> Result<XmlValue, XPathError> {
    match value {
        XPathValue::Empty => Err(XPathError::XPTY0004 {
            expected: "item()".to_string(),
            found: "empty-sequence()".to_string(),
        }),
        XPathValue::Item(item) => item_to_atomic(item),
        XPathValue::Sequence(items) => {
            if items.len() == 1 {
                item_to_atomic(items.into_iter().next().unwrap())
            } else {
                Err(XPathError::more_than_one_item())
            }
        }
    }
}

/// Atomize a value to an optional XmlValue.
pub fn atomize_to_single_opt<N: DomNavigator>(value: XPathValue<N>) -> Result<Option<XmlValue>, XPathError> {
    match value {
        XPathValue::Empty => Ok(None),
        other => atomize_to_single(other).map(Some),
    }
}

/// Convert an XmlItem to an atomic XmlValue
fn item_to_atomic<N: DomNavigator>(item: XmlItem<N>) -> Result<XmlValue, XPathError> {
    match item {
        XmlItem::Atomic(value) => atomize::atomize(&value),
        XmlItem::Node(nav) => Ok(nav.atomized_value()),
    }
}

/// Atomize all items in a value to a sequence of XmlValues.
pub fn atomize_sequence<N: DomNavigator>(value: XPathValue<N>) -> Result<Vec<XmlValue>, XPathError> {
    match value {
        XPathValue::Empty => Ok(Vec::new()),
        XPathValue::Item(item) => {
            let atomic = item_to_atomic(item)?;
            Ok(vec![atomic])
        }
        XPathValue::Sequence(items) => {
            items.into_iter()
                .map(item_to_atomic)
                .collect()
        }
    }
}

/// Materialize a value to a Vec of XmlItems.
pub fn materialize<N: DomNavigator>(value: XPathValue<N>) -> Vec<XmlItem<N>> {
    value.into_vec()
}

// ============================================================================
// Function Dispatch
// ============================================================================

/// Evaluate a function by its ID.
///
/// This is the main dispatch function that routes to the appropriate
/// function implementation based on the FunctionId.
pub fn eval_function<N: DomNavigator>(
    id: FunctionId,
    context: &mut DynamicContext<'_, N>,
    args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    match id {
        // ====================================================================
        // Boolean functions
        // ====================================================================
        FunctionId::True => Ok(XPathValue::boolean(true)),
        FunctionId::False => Ok(XPathValue::boolean(false)),
        FunctionId::Not => eval_not(args),

        // ====================================================================
        // Context/Special functions
        // ====================================================================
        FunctionId::Position => special::position(context, args),
        FunctionId::Last => special::last(context, args),
        FunctionId::Trace => special::trace(context, args),
        FunctionId::Data => special::data(context, args),
        FunctionId::DefaultCollation => special::default_collation(context, args),

        // ====================================================================
        // Sequence functions (basic)
        // ====================================================================
        FunctionId::Empty => eval_empty(args),
        FunctionId::Exists => eval_exists(args),
        FunctionId::Count => eval_count(args),

        // ====================================================================
        // String functions (Phase 2)
        // ====================================================================
        FunctionId::Concat => string::concat(context, args),
        FunctionId::StringJoin => string::string_join(context, args),
        FunctionId::Substring => string::substring(context, args),
        FunctionId::StringLength => string::string_length(context, args),
        FunctionId::NormalizeSpace => string::normalize_space(context, args),
        FunctionId::NormalizeUnicode => string::normalize_unicode(context, args),
        FunctionId::UpperCase => string::upper_case(context, args),
        FunctionId::LowerCase => string::lower_case(context, args),
        FunctionId::Translate => string::translate(context, args),
        FunctionId::EncodeForUri => string::encode_for_uri(context, args),
        FunctionId::IriToUri => string::iri_to_uri(context, args),
        FunctionId::EscapeHtmlUri => string::escape_html_uri(context, args),
        FunctionId::Contains => string::contains(context, args),
        FunctionId::StartsWith => string::starts_with(context, args),
        FunctionId::EndsWith => string::ends_with(context, args),
        FunctionId::SubstringBefore => string::substring_before(context, args),
        FunctionId::SubstringAfter => string::substring_after(context, args),
        FunctionId::StringToCodepoints => string::string_to_codepoints(context, args),
        FunctionId::CodepointsToString => string::codepoints_to_string(context, args),
        FunctionId::Compare => string::compare(context, args),
        FunctionId::CodepointEqual => string::codepoint_equal(context, args),

        // ====================================================================
        // Numeric functions (Phase 3)
        // ====================================================================
        FunctionId::Abs => numeric::abs(context, args),
        FunctionId::Ceiling => numeric::ceiling(context, args),
        FunctionId::Floor => numeric::floor(context, args),
        FunctionId::Round => numeric::round(context, args),
        FunctionId::RoundHalfToEven => numeric::round_half_to_even(context, args),

        // ====================================================================
        // Sequence functions (Phase 3)
        // ====================================================================
        FunctionId::Reverse => sequence::reverse(context, args),
        FunctionId::ZeroOrOne => sequence::zero_or_one(context, args),
        FunctionId::OneOrMore => sequence::one_or_more(context, args),
        FunctionId::ExactlyOne => sequence::exactly_one(context, args),
        FunctionId::DistinctValues => sequence::distinct_values(context, args),
        FunctionId::IndexOf => sequence::index_of(context, args),
        FunctionId::Remove => sequence::remove(context, args),
        FunctionId::InsertBefore => sequence::insert_before(context, args),
        FunctionId::Subsequence => sequence::subsequence(context, args),
        FunctionId::Unordered => sequence::unordered(context, args),
        FunctionId::DeepEqual => sequence::deep_equal(context, args),

        // ====================================================================
        // Aggregate functions (Phase 5)
        // ====================================================================
        FunctionId::Sum => aggregate::sum(context, args),
        FunctionId::Avg => aggregate::avg(context, args),
        FunctionId::Min => aggregate::min(context, args),
        FunctionId::Max => aggregate::max(context, args),

        // ====================================================================
        // Node functions (Phase 5)
        // ====================================================================
        FunctionId::Name => node::name(context, args),
        FunctionId::LocalName => node::local_name(context, args),
        FunctionId::NamespaceUri => node::namespace_uri(context, args),
        FunctionId::NodeName => node::node_name(context, args),
        FunctionId::Nilled => node::nilled(context, args),
        FunctionId::BaseUri => node::base_uri(context, args),
        FunctionId::DocumentUri => node::document_uri(context, args),
        FunctionId::Lang => node::lang(context, args),
        FunctionId::Root => node::root(context, args),

        // ====================================================================
        // DateTime functions (Phase 6)
        // ====================================================================
        FunctionId::DateTime => datetime::create_datetime(context, args),
        FunctionId::CurrentDateTime => datetime::current_datetime(context, args),
        FunctionId::CurrentDate => datetime::current_date(context, args),
        FunctionId::CurrentTime => datetime::current_time(context, args),
        FunctionId::ImplicitTimezone => datetime::implicit_timezone(context, args),
        // Duration component extraction
        FunctionId::YearsFromDuration => datetime::years_from_duration(context, args),
        FunctionId::MonthsFromDuration => datetime::months_from_duration(context, args),
        FunctionId::DaysFromDuration => datetime::days_from_duration(context, args),
        FunctionId::HoursFromDuration => datetime::hours_from_duration(context, args),
        FunctionId::MinutesFromDuration => datetime::minutes_from_duration(context, args),
        FunctionId::SecondsFromDuration => datetime::seconds_from_duration(context, args),
        // DateTime component extraction
        FunctionId::YearFromDateTime => datetime::year_from_datetime(context, args),
        FunctionId::MonthFromDateTime => datetime::month_from_datetime(context, args),
        FunctionId::DayFromDateTime => datetime::day_from_datetime(context, args),
        FunctionId::HoursFromDateTime => datetime::hours_from_datetime(context, args),
        FunctionId::MinutesFromDateTime => datetime::minutes_from_datetime(context, args),
        FunctionId::SecondsFromDateTime => datetime::seconds_from_datetime(context, args),
        FunctionId::TimezoneFromDateTime => datetime::timezone_from_datetime(context, args),
        // Date component extraction
        FunctionId::YearFromDate => datetime::year_from_date(context, args),
        FunctionId::MonthFromDate => datetime::month_from_date(context, args),
        FunctionId::DayFromDate => datetime::day_from_date(context, args),
        FunctionId::TimezoneFromDate => datetime::timezone_from_date(context, args),
        // Time component extraction
        FunctionId::HoursFromTime => datetime::hours_from_time(context, args),
        FunctionId::MinutesFromTime => datetime::minutes_from_time(context, args),
        FunctionId::SecondsFromTime => datetime::seconds_from_time(context, args),
        FunctionId::TimezoneFromTime => datetime::timezone_from_time(context, args),
        // Timezone adjustment
        FunctionId::AdjustDateTimeToTimezone => datetime::adjust_datetime_to_timezone(context, args),
        FunctionId::AdjustDateToTimezone => datetime::adjust_date_to_timezone(context, args),
        FunctionId::AdjustTimeToTimezone => datetime::adjust_time_to_timezone(context, args),

        // ====================================================================
        // QName functions (Phase 7)
        // ====================================================================
        FunctionId::ResolveQName => qname::resolve_qname(context, args),
        FunctionId::QName => qname::qname_constructor(context, args),
        FunctionId::PrefixFromQName => qname::prefix_from_qname(context, args),
        FunctionId::LocalNameFromQName => qname::local_name_from_qname(context, args),
        FunctionId::NamespaceUriFromQName => qname::namespace_uri_from_qname(context, args),
        FunctionId::NamespaceUriForPrefix => qname::namespace_uri_for_prefix(context, args),
        FunctionId::InScopePrefixes => qname::in_scope_prefixes(context, args),

        // ====================================================================
        // URI functions (Phase 7)
        // ====================================================================
        FunctionId::ResolveUri => uri::resolve_uri(context, args),
        FunctionId::StaticBaseUri => uri::static_base_uri(context, args),

        // ====================================================================
        // Regex functions (Phase 7)
        // ====================================================================
        FunctionId::Matches => regex::matches(context, args),
        FunctionId::Replace => regex::replace(context, args),
        FunctionId::Tokenize => regex::tokenize(context, args),

        // All other functions will be implemented in later phases
        _ => Err(XPathError::not_implemented(format!(
            "Function {:?} not yet implemented",
            id
        ))),
    }
}

// Simple function implementations for Phase 1

fn eval_not<N: DomNavigator>(mut args: Vec<XPathValue<N>>) -> Result<XPathValue<N>, XPathError> {
    if args.len() != 1 {
        return Err(XPathError::wrong_number_of_arguments("not", 1, args.len()));
    }
    let arg = args.remove(0);
    let ebv = effective_boolean_value(&arg)?;
    Ok(XPathValue::boolean(!ebv))
}

fn eval_empty<N: DomNavigator>(mut args: Vec<XPathValue<N>>) -> Result<XPathValue<N>, XPathError> {
    if args.len() != 1 {
        return Err(XPathError::wrong_number_of_arguments("empty", 1, args.len()));
    }
    let arg = args.remove(0);
    Ok(XPathValue::boolean(arg.is_empty()))
}

fn eval_exists<N: DomNavigator>(mut args: Vec<XPathValue<N>>) -> Result<XPathValue<N>, XPathError> {
    if args.len() != 1 {
        return Err(XPathError::wrong_number_of_arguments("exists", 1, args.len()));
    }
    let arg = args.remove(0);
    Ok(XPathValue::boolean(!arg.is_empty()))
}

fn eval_count<N: DomNavigator>(mut args: Vec<XPathValue<N>>) -> Result<XPathValue<N>, XPathError> {
    if args.len() != 1 {
        return Err(XPathError::wrong_number_of_arguments("count", 1, args.len()));
    }
    let arg = args.remove(0);
    Ok(XPathValue::integer(arg.len() as i64))
}

/// Compute the effective boolean value of an XPathValue.
pub fn effective_boolean_value<N: DomNavigator>(value: &XPathValue<N>) -> Result<bool, XPathError> {
    match value {
        XPathValue::Empty => Ok(false),
        XPathValue::Item(item) => item_boolean_value(item),
        XPathValue::Sequence(items) => {
            if items.is_empty() {
                Ok(false)
            } else if let Some(XmlItem::Node(_)) = items.first() {
                // Non-empty sequence starting with a node is true
                Ok(true)
            } else if items.len() == 1 {
                item_boolean_value(&items[0])
            } else {
                // Sequence of multiple atomics is an error
                Err(XPathError::FORG0006 {
                    message: "Effective boolean value not defined for sequence of multiple atomic values".to_string(),
                })
            }
        }
    }
}

fn item_boolean_value<N: DomNavigator>(item: &XmlItem<N>) -> Result<bool, XPathError> {
    match item {
        XmlItem::Node(_) => Ok(true),
        XmlItem::Atomic(value) => {
            match value.as_boolean() {
                Some(b) => Ok(b),
                None => {
                    // For strings, empty is false, non-empty is true
                    if let Some(s) = value.as_string() {
                        Ok(!s.is_empty())
                    } else if let Some(d) = value.as_double() {
                        // For numbers, 0 and NaN are false
                        Ok(!d.is_nan() && d != 0.0)
                    } else if let Some(i) = value.as_integer() {
                        Ok(*i != BigInt::from(0))
                    } else {
                        // Other types - try string conversion
                        let s = value.to_string_value();
                        Ok(!s.is_empty())
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xpath::RoXmlNavigator;

    #[test]
    fn test_xpath_value_empty() {
        let value: XPathValue<RoXmlNavigator<'static>> = XPathValue::empty();
        assert!(value.is_empty());
        assert_eq!(value.len(), 0);
    }

    #[test]
    fn test_xpath_value_single() {
        let value: XPathValue<RoXmlNavigator<'static>> = XPathValue::boolean(true);
        assert!(!value.is_empty());
        assert_eq!(value.len(), 1);
        assert!(value.is_single());
    }

    #[test]
    fn test_xpath_value_from_sequence() {
        let items: Vec<XmlItem<RoXmlNavigator<'static>>> = vec![
            XmlItem::Atomic(XmlValue::integer(1.into())),
            XmlItem::Atomic(XmlValue::integer(2.into())),
        ];
        let value = XPathValue::from_sequence(items);
        assert_eq!(value.len(), 2);
        assert!(!value.is_single());
    }

    #[test]
    fn test_effective_boolean_value_empty() {
        let value: XPathValue<RoXmlNavigator<'static>> = XPathValue::empty();
        assert!(!effective_boolean_value(&value).unwrap());
    }

    #[test]
    fn test_effective_boolean_value_boolean() {
        let value: XPathValue<RoXmlNavigator<'static>> = XPathValue::boolean(true);
        assert!(effective_boolean_value(&value).unwrap());

        let value: XPathValue<RoXmlNavigator<'static>> = XPathValue::boolean(false);
        assert!(!effective_boolean_value(&value).unwrap());
    }

    #[test]
    fn test_effective_boolean_value_string() {
        let value: XPathValue<RoXmlNavigator<'static>> = XPathValue::string("hello");
        assert!(effective_boolean_value(&value).unwrap());

        let value: XPathValue<RoXmlNavigator<'static>> = XPathValue::string("");
        assert!(!effective_boolean_value(&value).unwrap());
    }

    #[test]
    fn test_effective_boolean_value_number() {
        let value: XPathValue<RoXmlNavigator<'static>> = XPathValue::double(1.0);
        assert!(effective_boolean_value(&value).unwrap());

        let value: XPathValue<RoXmlNavigator<'static>> = XPathValue::double(0.0);
        assert!(!effective_boolean_value(&value).unwrap());

        let value: XPathValue<RoXmlNavigator<'static>> = XPathValue::double(f64::NAN);
        assert!(!effective_boolean_value(&value).unwrap());
    }
}
