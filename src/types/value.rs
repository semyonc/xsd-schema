//! XSD value types for typed atomic values
//!
//! This module provides the `XmlValue` type for representing typed XSD values,
//! integrating with the `xsd-types` crate for atomic value parsing and formatting.
//!
//! ## Design
//!
//! - `XmlValue` is a typed container for XSD values
//! - `XmlAtomicValue` holds the actual parsed value
//! - QName and NOTATION values use `QualifiedName` (namespace-resolved)
//! - List values store sequences of atomic values with a known item type

use std::fmt;

use num_bigint::BigInt;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;

use crate::ids::SimpleTypeKey;
use crate::namespace::qname::QualifiedName;
use super::{XmlTypeCode, PrimitiveTypeCode};

/// A typed XSD value with type information.
///
/// This is the primary value type for XPath2/XQuery operations.
/// It carries both the value and its type information.
#[derive(Debug, Clone, PartialEq)]
pub struct XmlValue {
    /// The type code identifying the value's type
    pub type_code: XmlTypeCode,
    /// Optional reference to a schema-defined type
    pub schema_type: Option<SimpleTypeKey>,
    /// The actual value
    pub value: XmlValueKind,
}

impl XmlValue {
    /// Create a new XmlValue with the given type code and value
    pub fn new(type_code: XmlTypeCode, value: XmlValueKind) -> Self {
        Self {
            type_code,
            schema_type: None,
            value,
        }
    }

    /// Create a new XmlValue with schema type reference
    pub fn with_schema_type(type_code: XmlTypeCode, schema_type: SimpleTypeKey, value: XmlValueKind) -> Self {
        Self {
            type_code,
            schema_type: Some(schema_type),
            value,
        }
    }

    /// Create an untyped atomic value
    pub fn untyped(s: impl Into<String>) -> Self {
        Self {
            type_code: XmlTypeCode::UntypedAtomic,
            schema_type: None,
            value: XmlValueKind::UntypedAtomic(s.into()),
        }
    }

    /// Create a string value
    pub fn string(s: impl Into<String>) -> Self {
        Self {
            type_code: XmlTypeCode::String,
            schema_type: None,
            value: XmlValueKind::Atomic(XmlAtomicValue::String(s.into())),
        }
    }

    /// Create a boolean value
    pub fn boolean(b: bool) -> Self {
        Self {
            type_code: XmlTypeCode::Boolean,
            schema_type: None,
            value: XmlValueKind::Atomic(XmlAtomicValue::Boolean(b)),
        }
    }

    /// Create a decimal value
    pub fn decimal(d: Decimal) -> Self {
        Self {
            type_code: XmlTypeCode::Decimal,
            schema_type: None,
            value: XmlValueKind::Atomic(XmlAtomicValue::Decimal(d)),
        }
    }

    /// Create an integer value
    pub fn integer(i: BigInt) -> Self {
        Self {
            type_code: XmlTypeCode::Integer,
            schema_type: None,
            value: XmlValueKind::Atomic(XmlAtomicValue::Integer(i)),
        }
    }

    /// Create a float value
    pub fn float(f: f32) -> Self {
        Self {
            type_code: XmlTypeCode::Float,
            schema_type: None,
            value: XmlValueKind::Atomic(XmlAtomicValue::Float(f)),
        }
    }

    /// Create a double value
    pub fn double(d: f64) -> Self {
        Self {
            type_code: XmlTypeCode::Double,
            schema_type: None,
            value: XmlValueKind::Atomic(XmlAtomicValue::Double(d)),
        }
    }

    /// Check if this is an atomic value
    pub fn is_atomic(&self) -> bool {
        matches!(self.value, XmlValueKind::Atomic(_) | XmlValueKind::UntypedAtomic(_))
    }

    /// Check if this is a list value
    pub fn is_list(&self) -> bool {
        matches!(self.value, XmlValueKind::List { .. })
    }

    /// Check if this is a union value
    pub fn is_union(&self) -> bool {
        matches!(self.value, XmlValueKind::Union(_))
    }

    /// Check if this is an untyped atomic value
    pub fn is_untyped(&self) -> bool {
        matches!(self.value, XmlValueKind::UntypedAtomic(_))
    }

    /// Get the primitive type code for this value
    pub fn primitive_type(&self) -> Option<PrimitiveTypeCode> {
        PrimitiveTypeCode::from_type_code(self.type_code)
    }

    /// Get the string value (canonical representation)
    pub fn to_string_value(&self) -> String {
        match &self.value {
            XmlValueKind::Atomic(atom) => atom.to_string(),
            XmlValueKind::List { items, .. } => {
                items.iter()
                    .map(|v| v.to_string())
                    .collect::<Vec<_>>()
                    .join(" ")
            }
            XmlValueKind::Union(inner) => inner.to_string_value(),
            XmlValueKind::UntypedAtomic(s) => s.clone(),
        }
    }

    /// Try to get as boolean
    pub fn as_boolean(&self) -> Option<bool> {
        match &self.value {
            XmlValueKind::Atomic(XmlAtomicValue::Boolean(b)) => Some(*b),
            _ => None,
        }
    }

    /// Try to get as string
    pub fn as_string(&self) -> Option<&str> {
        match &self.value {
            XmlValueKind::Atomic(XmlAtomicValue::String(s)) => Some(s),
            XmlValueKind::UntypedAtomic(s) => Some(s),
            _ => None,
        }
    }

    /// Try to get as decimal
    pub fn as_decimal(&self) -> Option<Decimal> {
        match &self.value {
            XmlValueKind::Atomic(XmlAtomicValue::Decimal(d)) => Some(*d),
            XmlValueKind::Atomic(XmlAtomicValue::Integer(i)) => {
                // Try to convert BigInt to Decimal
                i.to_string().parse().ok()
            }
            _ => None,
        }
    }

    /// Try to get as integer
    pub fn as_integer(&self) -> Option<&BigInt> {
        match &self.value {
            XmlValueKind::Atomic(XmlAtomicValue::Integer(i)) => Some(i),
            _ => None,
        }
    }

    /// Try to get as double
    pub fn as_double(&self) -> Option<f64> {
        match &self.value {
            XmlValueKind::Atomic(XmlAtomicValue::Double(d)) => Some(*d),
            XmlValueKind::Atomic(XmlAtomicValue::Float(f)) => Some(*f as f64),
            XmlValueKind::Atomic(XmlAtomicValue::Decimal(d)) => d.to_string().parse().ok(),
            XmlValueKind::Atomic(XmlAtomicValue::Integer(i)) => i.to_string().parse().ok(),
            _ => None,
        }
    }

    /// Try to get as QName
    pub fn as_qname(&self) -> Option<&QualifiedName> {
        match &self.value {
            XmlValueKind::Atomic(XmlAtomicValue::QName(qn)) => Some(qn),
            _ => None,
        }
    }

    /// Convert this `XmlValue` to an `XPathValue` for use as `$value` in assertion evaluation.
    ///
    /// - **Atomic/UntypedAtomic** → single `XPathValue::Item`
    /// - **List** → `XPathValue::Sequence` of atomic items, each with `item_schema_type`
    /// - **Union** → recursively converts the inner value
    ///
    /// The `item_schema_type` parameter is needed because `XmlValueKind::List` stores bare
    /// `XmlAtomicValue` items without per-item `schema_type`. Callers pass it from the
    /// list type's `resolved_item_type`.
    #[cfg(feature = "xsd11")]
    pub fn to_xpath_value<N: crate::xpath::DomNavigator>(
        &self,
        item_schema_type: Option<SimpleTypeKey>,
    ) -> crate::xpath::XPathValue<N> {
        use crate::xpath::iterator::XmlItem;
        use crate::xpath::XPathValue;

        match &self.value {
            XmlValueKind::Atomic(_) | XmlValueKind::UntypedAtomic(_) => {
                XPathValue::from_atomic(self.clone())
            }
            XmlValueKind::List { item_type, items } => {
                let xml_items: Vec<XmlItem<N>> = items
                    .iter()
                    .map(|atom| {
                        let val = XmlValue {
                            type_code: atom.type_code(),
                            schema_type: item_schema_type,
                            value: XmlValueKind::Atomic(atom.clone()),
                        };
                        XmlItem::Atomic(val)
                    })
                    .collect();
                let _ = item_type; // item_type already embedded in each atom's type_code
                XPathValue::from_sequence(xml_items)
            }
            XmlValueKind::Union(inner) => inner.to_xpath_value(item_schema_type),
        }
    }
}

/// Value kind discriminant for XmlValue
#[derive(Debug, Clone, PartialEq)]
pub enum XmlValueKind {
    /// A single atomic value
    Atomic(XmlAtomicValue),
    /// A list of atomic values (e.g., NMTOKENS)
    List {
        /// The type code of list items
        item_type: XmlTypeCode,
        /// The list items
        items: Vec<XmlAtomicValue>,
    },
    /// A union value (actual type determined at runtime)
    Union(Box<XmlValue>),
    /// An untyped atomic value (raw string)
    UntypedAtomic(String),
}

/// Atomic XSD value types
///
/// This enum holds the actual parsed values for atomic XSD types.
/// For complex types like date/time, we use structured representations.
#[derive(Debug, Clone, PartialEq)]
pub enum XmlAtomicValue {
    // String types
    /// xs:string and derived types
    String(String),

    // Boolean type
    /// xs:boolean
    Boolean(bool),

    // Numeric types
    /// xs:decimal
    Decimal(Decimal),
    /// xs:integer and derived integer types
    Integer(BigInt),
    /// xs:float
    Float(f32),
    /// xs:double
    Double(f64),

    // Date/time types
    /// xs:dateTime
    DateTime(DateTimeValue),
    /// xs:date
    Date(DateValue),
    /// xs:time
    Time(TimeValue),
    /// xs:duration
    Duration(DurationValue),
    /// xs:gYearMonth
    GYearMonth(GYearMonthValue),
    /// xs:gYear
    GYear(GYearValue),
    /// xs:gMonthDay
    GMonthDay(GMonthDayValue),
    /// xs:gDay
    GDay(GDayValue),
    /// xs:gMonth
    GMonth(GMonthValue),
    /// xs:yearMonthDuration (XSD 1.1)
    YearMonthDuration(YearMonthDurationValue),
    /// xs:dayTimeDuration (XSD 1.1)
    DayTimeDuration(DayTimeDurationValue),

    // Binary types
    /// xs:hexBinary
    HexBinary(Vec<u8>),
    /// xs:base64Binary
    Base64Binary(Vec<u8>),

    // URI type
    /// xs:anyURI
    AnyUri(String),

    // QName types (namespace-resolved)
    /// xs:QName
    QName(QualifiedName),
    /// xs:NOTATION
    Notation(QualifiedName),
}

impl XmlAtomicValue {
    /// Get the type code for this atomic value
    pub fn type_code(&self) -> XmlTypeCode {
        match self {
            Self::String(_) => XmlTypeCode::String,
            Self::Boolean(_) => XmlTypeCode::Boolean,
            Self::Decimal(_) => XmlTypeCode::Decimal,
            Self::Integer(_) => XmlTypeCode::Integer,
            Self::Float(_) => XmlTypeCode::Float,
            Self::Double(_) => XmlTypeCode::Double,
            Self::DateTime(_) => XmlTypeCode::DateTime,
            Self::Date(_) => XmlTypeCode::Date,
            Self::Time(_) => XmlTypeCode::Time,
            Self::Duration(_) => XmlTypeCode::Duration,
            Self::GYearMonth(_) => XmlTypeCode::GYearMonth,
            Self::GYear(_) => XmlTypeCode::GYear,
            Self::GMonthDay(_) => XmlTypeCode::GMonthDay,
            Self::GDay(_) => XmlTypeCode::GDay,
            Self::GMonth(_) => XmlTypeCode::GMonth,
            Self::YearMonthDuration(_) => XmlTypeCode::YearMonthDuration,
            Self::DayTimeDuration(_) => XmlTypeCode::DayTimeDuration,
            Self::HexBinary(_) => XmlTypeCode::HexBinary,
            Self::Base64Binary(_) => XmlTypeCode::Base64Binary,
            Self::AnyUri(_) => XmlTypeCode::AnyUri,
            Self::QName(_) => XmlTypeCode::QName,
            Self::Notation(_) => XmlTypeCode::Notation,
        }
    }

    /// Get the primitive type code for this atomic value
    pub fn primitive_type(&self) -> PrimitiveTypeCode {
        match self {
            Self::String(_) => PrimitiveTypeCode::String,
            Self::Boolean(_) => PrimitiveTypeCode::Boolean,
            Self::Decimal(_) | Self::Integer(_) => PrimitiveTypeCode::Decimal,
            Self::Float(_) => PrimitiveTypeCode::Float,
            Self::Double(_) => PrimitiveTypeCode::Double,
            Self::DateTime(_) => PrimitiveTypeCode::DateTime,
            Self::Date(_) => PrimitiveTypeCode::Date,
            Self::Time(_) => PrimitiveTypeCode::Time,
            Self::Duration(_) | Self::YearMonthDuration(_) | Self::DayTimeDuration(_) => PrimitiveTypeCode::Duration,
            Self::GYearMonth(_) => PrimitiveTypeCode::GYearMonth,
            Self::GYear(_) => PrimitiveTypeCode::GYear,
            Self::GMonthDay(_) => PrimitiveTypeCode::GMonthDay,
            Self::GDay(_) => PrimitiveTypeCode::GDay,
            Self::GMonth(_) => PrimitiveTypeCode::GMonth,
            Self::HexBinary(_) => PrimitiveTypeCode::HexBinary,
            Self::Base64Binary(_) => PrimitiveTypeCode::Base64Binary,
            Self::AnyUri(_) => PrimitiveTypeCode::AnyUri,
            Self::QName(_) => PrimitiveTypeCode::QName,
            Self::Notation(_) => PrimitiveTypeCode::Notation,
        }
    }
}

impl fmt::Display for XmlAtomicValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::String(s) => write!(f, "{}", s),
            Self::Boolean(b) => write!(f, "{}", if *b { "true" } else { "false" }),
            Self::Decimal(d) => {
                // XSD canonical form: no trailing zeros for integers
                if d.fract().is_zero() {
                    write!(f, "{}", d.trunc())
                } else {
                    write!(f, "{}", d.normalize())
                }
            }
            Self::Integer(i) => write!(f, "{}", i),
            Self::Float(v) => format_float(*v, f),
            Self::Double(v) => format_double(*v, f),
            Self::DateTime(dt) => write!(f, "{}", dt),
            Self::Date(d) => write!(f, "{}", d),
            Self::Time(t) => write!(f, "{}", t),
            Self::Duration(d) => write!(f, "{}", d),
            Self::GYearMonth(v) => write!(f, "{}", v),
            Self::GYear(v) => write!(f, "{}", v),
            Self::GMonthDay(v) => write!(f, "{}", v),
            Self::GDay(v) => write!(f, "{}", v),
            Self::GMonth(v) => write!(f, "{}", v),
            Self::YearMonthDuration(v) => write!(f, "{}", v),
            Self::DayTimeDuration(v) => write!(f, "{}", v),
            Self::HexBinary(bytes) => {
                write!(f, "{}", hex::encode_upper(bytes))
            }
            Self::Base64Binary(bytes) => {
                use base64::Engine;
                write!(f, "{}", base64::engine::general_purpose::STANDARD.encode(bytes))
            }
            Self::AnyUri(uri) => write!(f, "{}", uri),
            Self::QName(qn) => {
                // Display with prefix if available
                write!(f, "QName({:?}:{})", qn.namespace_uri, qn.local_name.0)
            }
            Self::Notation(n) => {
                write!(f, "NOTATION({:?}:{})", n.namespace_uri, n.local_name.0)
            }
        }
    }
}

/// Format a scientific notation string to ensure the mantissa has a decimal point.
///
/// Rust's `{:E}` may produce `1E7` or `-1E7`; XPath 2.0 requires `1.0E7` or `-1.0E7`.
fn fix_scientific_notation(s: &str) -> String {
    // Find the position of 'E'
    if let Some(e_pos) = s.find('E') {
        let mantissa = &s[..e_pos];
        let exponent = &s[e_pos..]; // includes 'E'
        if !mantissa.contains('.') {
            format!("{}.0{}", mantissa, exponent)
        } else {
            s.to_string()
        }
    } else {
        s.to_string()
    }
}

/// Format float according to XSD canonical representation
fn format_float(v: f32, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    if v.is_nan() {
        write!(f, "NaN")
    } else if v.is_infinite() {
        if v.is_sign_positive() {
            write!(f, "INF")
        } else {
            write!(f, "-INF")
        }
    } else if v == 0.0 {
        // Per XPath 2.0: negative zero serializes as "-0", positive zero as "0"
        if v.is_sign_negative() {
            write!(f, "-0")
        } else {
            write!(f, "0")
        }
    } else if v.abs() >= 1e-6 && v.abs() < 1e6 {
        // Use regular notation for values in this range
        write!(f, "{}", v)
    } else {
        // Use scientific notation with guaranteed decimal point in mantissa
        let s = format!("{:E}", v);
        write!(f, "{}", fix_scientific_notation(&s))
    }
}

/// Format double according to XSD canonical representation
fn format_double(v: f64, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    if v.is_nan() {
        write!(f, "NaN")
    } else if v.is_infinite() {
        if v.is_sign_positive() {
            write!(f, "INF")
        } else {
            write!(f, "-INF")
        }
    } else if v == 0.0 {
        // Per XPath 2.0: negative zero serializes as "-0", positive zero as "0"
        if v.is_sign_negative() {
            write!(f, "-0")
        } else {
            write!(f, "0")
        }
    } else if v.abs() >= 1e-6 && v.abs() < 1e6 {
        // Use regular notation for values in this range
        write!(f, "{}", v)
    } else {
        // Use scientific notation with guaranteed decimal point in mantissa
        let s = format!("{:E}", v);
        write!(f, "{}", fix_scientific_notation(&s))
    }
}

// ============================================================================
// Date/Time Value Types
// ============================================================================

/// Format a year value according to XPath 2.0 rules.
///
/// Negative years must be formatted as sign + 4-digit year (e.g., -12 → "-0012").
/// Positive years use standard 4-digit zero-padded format.
fn format_year(year: i32, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    if year < 0 {
        write!(f, "-{:04}", -year)
    } else {
        write!(f, "{:04}", year)
    }
}

/// xs:dateTime value
#[derive(Debug, Clone, PartialEq)]
pub struct DateTimeValue {
    pub year: i32,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: Decimal,
    pub timezone: Option<TimezoneOffset>,
}

impl DateTimeValue {
    /// Convert to total seconds from a reference epoch for comparison.
    /// Uses implicit UTC for timezone-unaware values (XSD 1.0 facet semantics).
    fn to_comparable_instant(&self) -> Decimal {
        let tz_minutes = self.timezone.map_or(0i64, |tz| tz.0 as i64);
        let days = date_to_days(self.year, self.month as i32, self.day as i32);
        Decimal::from(days) * Decimal::from(86400)
            + Decimal::from(self.hour as i64) * Decimal::from(3600)
            + Decimal::from(self.minute as i64) * Decimal::from(60)
            + self.second
            - Decimal::from(tz_minutes) * Decimal::from(60)
    }
}

impl PartialOrd for DateTimeValue {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        self.to_comparable_instant().partial_cmp(&other.to_comparable_instant())
    }
}

impl fmt::Display for DateTimeValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        format_year(self.year, f)?;
        write!(f, "-{:02}-{:02}T{:02}:{:02}:",
            self.month, self.day,
            self.hour, self.minute)?;
        format_seconds(self.second, f)?;
        if let Some(tz) = &self.timezone {
            write!(f, "{}", tz)?;
        }
        Ok(())
    }
}

/// xs:date value
#[derive(Debug, Clone, PartialEq)]
pub struct DateValue {
    pub year: i32,
    pub month: u8,
    pub day: u8,
    pub timezone: Option<TimezoneOffset>,
}

impl DateValue {
    fn to_comparable_instant(&self) -> Decimal {
        let tz_minutes = self.timezone.map_or(0i64, |tz| tz.0 as i64);
        let days = date_to_days(self.year, self.month as i32, self.day as i32);
        Decimal::from(days) * Decimal::from(86400)
            - Decimal::from(tz_minutes) * Decimal::from(60)
    }
}

impl PartialOrd for DateValue {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        self.to_comparable_instant().partial_cmp(&other.to_comparable_instant())
    }
}

impl fmt::Display for DateValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        format_year(self.year, f)?;
        write!(f, "-{:02}-{:02}", self.month, self.day)?;
        if let Some(tz) = &self.timezone {
            write!(f, "{}", tz)?;
        }
        Ok(())
    }
}

/// xs:time value
#[derive(Debug, Clone, PartialEq)]
pub struct TimeValue {
    pub hour: u8,
    pub minute: u8,
    pub second: Decimal,
    pub timezone: Option<TimezoneOffset>,
}

impl TimeValue {
    fn to_comparable_seconds(&self) -> Decimal {
        let tz_minutes = self.timezone.map_or(0i64, |tz| tz.0 as i64);
        Decimal::from(self.hour as i64) * Decimal::from(3600)
            + Decimal::from(self.minute as i64) * Decimal::from(60)
            + self.second
            - Decimal::from(tz_minutes) * Decimal::from(60)
    }
}

impl PartialOrd for TimeValue {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        self.to_comparable_seconds().partial_cmp(&other.to_comparable_seconds())
    }
}

impl fmt::Display for TimeValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:02}:{:02}:", self.hour, self.minute)?;
        format_seconds(self.second, f)?;
        if let Some(tz) = &self.timezone {
            write!(f, "{}", tz)?;
        }
        Ok(())
    }
}

/// xs:duration value
#[derive(Debug, Clone, PartialEq)]
pub struct DurationValue {
    pub negative: bool,
    pub years: u32,
    pub months: u32,
    pub days: u32,
    pub hours: u32,
    pub minutes: u32,
    pub seconds: Decimal,
}

impl DurationValue {
    /// Convert duration to approximate total seconds for comparison.
    /// Uses average Gregorian month length (365.2425 / 12 = 30.436875 days).
    /// This provides a total order consistent with the NIST conformance test
    /// suite expectations. Strict XSD 1.0 §3.2.6.2 uses a partial order via
    /// 4 reference dates, but most practical implementations use a total order.
    fn to_approx_total_seconds(&self) -> Decimal {
        // Average month in seconds: 30.436875 * 86400 = 2629746
        let month_secs = Decimal::from(2629746i64);
        let total_months = Decimal::from(self.years as i64) * Decimal::from(12) + Decimal::from(self.months as i64);
        let day_time_secs = Decimal::from(self.days as i64) * Decimal::from(86400)
            + Decimal::from(self.hours as i64) * Decimal::from(3600)
            + Decimal::from(self.minutes as i64) * Decimal::from(60)
            + self.seconds;
        let total = total_months * month_secs + day_time_secs;
        if self.negative { -total } else { total }
    }
}

impl PartialOrd for DurationValue {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        self.to_approx_total_seconds().partial_cmp(&other.to_approx_total_seconds())
    }
}

impl fmt::Display for DurationValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Normalize year-month part
        let total_months = self.years * 12 + self.months;
        let years = total_months / 12;
        let months = total_months % 12;

        // Normalize day-time part
        let (days, hours, minutes, seconds) =
            normalize_day_time(self.days, self.hours, self.minutes, self.seconds);

        if self.negative {
            write!(f, "-")?;
        }
        write!(f, "P")?;
        if years > 0 {
            write!(f, "{}Y", years)?;
        }
        if months > 0 {
            write!(f, "{}M", months)?;
        }
        if days > 0 {
            write!(f, "{}D", days)?;
        }
        if hours > 0 || minutes > 0 || !seconds.is_zero() {
            write!(f, "T")?;
            if hours > 0 {
                write!(f, "{}H", hours)?;
            }
            if minutes > 0 {
                write!(f, "{}M", minutes)?;
            }
            if !seconds.is_zero() {
                format_duration_seconds(seconds, f)?;
                write!(f, "S")?;
            }
        }
        // Handle zero duration
        if years == 0 && months == 0 && days == 0
            && hours == 0 && minutes == 0 && seconds.is_zero() {
            write!(f, "T0S")?;
        }
        Ok(())
    }
}

/// xs:yearMonthDuration (XSD 1.1)
#[derive(Debug, Clone, PartialEq)]
pub struct YearMonthDurationValue {
    pub negative: bool,
    pub years: u32,
    pub months: u32,
}

impl fmt::Display for YearMonthDurationValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Normalize months → years + months
        let total_months = self.years * 12 + self.months;
        let years = total_months / 12;
        let months = total_months % 12;

        // Negative zero is normalized to positive zero
        if self.negative && (years > 0 || months > 0) {
            write!(f, "-")?;
        }
        write!(f, "P")?;
        if years > 0 {
            write!(f, "{}Y", years)?;
        }
        if months > 0 {
            write!(f, "{}M", months)?;
        }
        if years == 0 && months == 0 {
            write!(f, "0M")?;
        }
        Ok(())
    }
}

impl PartialOrd for YearMonthDurationValue {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        let self_months = if self.negative {
            -(self.years as i64 * 12 + self.months as i64)
        } else {
            self.years as i64 * 12 + self.months as i64
        };
        let other_months = if other.negative {
            -(other.years as i64 * 12 + other.months as i64)
        } else {
            other.years as i64 * 12 + other.months as i64
        };
        self_months.partial_cmp(&other_months)
    }
}

/// xs:dayTimeDuration (XSD 1.1)
#[derive(Debug, Clone, PartialEq)]
pub struct DayTimeDurationValue {
    pub negative: bool,
    pub days: u32,
    pub hours: u32,
    pub minutes: u32,
    pub seconds: Decimal,
}

impl fmt::Display for DayTimeDurationValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Normalize seconds → minutes → hours → days
        let (days, hours, minutes, seconds) =
            normalize_day_time(self.days, self.hours, self.minutes, self.seconds);

        // Negative zero is normalized to positive zero
        if self.negative && (days > 0 || hours > 0 || minutes > 0 || !seconds.is_zero()) {
            write!(f, "-")?;
        }
        write!(f, "P")?;
        if days > 0 {
            write!(f, "{}D", days)?;
        }
        if hours > 0 || minutes > 0 || !seconds.is_zero() {
            write!(f, "T")?;
            if hours > 0 {
                write!(f, "{}H", hours)?;
            }
            if minutes > 0 {
                write!(f, "{}M", minutes)?;
            }
            if !seconds.is_zero() {
                format_duration_seconds(seconds, f)?;
                write!(f, "S")?;
            }
        }
        if days == 0 && hours == 0 && minutes == 0 && seconds.is_zero() {
            write!(f, "T0S")?;
        }
        Ok(())
    }
}

impl PartialOrd for DayTimeDurationValue {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        let self_secs = {
            let s = Decimal::from(self.days as i64) * Decimal::from(86400i64)
                + Decimal::from(self.hours as i64) * Decimal::from(3600i64)
                + Decimal::from(self.minutes as i64) * Decimal::from(60i64)
                + self.seconds;
            if self.negative { -s } else { s }
        };
        let other_secs = {
            let s = Decimal::from(other.days as i64) * Decimal::from(86400i64)
                + Decimal::from(other.hours as i64) * Decimal::from(3600i64)
                + Decimal::from(other.minutes as i64) * Decimal::from(60i64)
                + other.seconds;
            if other.negative { -s } else { s }
        };
        self_secs.partial_cmp(&other_secs)
    }
}

/// xs:gYearMonth value
#[derive(Debug, Clone, PartialEq)]
pub struct GYearMonthValue {
    pub year: i32,
    pub month: u8,
    pub timezone: Option<TimezoneOffset>,
}

impl PartialOrd for GYearMonthValue {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        let tz1 = self.timezone.map_or(0i64, |tz| tz.0 as i64);
        let tz2 = other.timezone.map_or(0i64, |tz| tz.0 as i64);
        let d1 = date_to_days(self.year, self.month as i32, 1) * 1440 - tz1;
        let d2 = date_to_days(other.year, other.month as i32, 1) * 1440 - tz2;
        d1.partial_cmp(&d2)
    }
}

impl fmt::Display for GYearMonthValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        format_year(self.year, f)?;
        write!(f, "-{:02}", self.month)?;
        if let Some(tz) = &self.timezone {
            write!(f, "{}", tz)?;
        }
        Ok(())
    }
}

/// xs:gYear value
#[derive(Debug, Clone, PartialEq)]
pub struct GYearValue {
    pub year: i32,
    pub timezone: Option<TimezoneOffset>,
}

impl PartialOrd for GYearValue {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        let tz1 = self.timezone.map_or(0i64, |tz| tz.0 as i64);
        let tz2 = other.timezone.map_or(0i64, |tz| tz.0 as i64);
        let d1 = date_to_days(self.year, 1, 1) * 1440 - tz1;
        let d2 = date_to_days(other.year, 1, 1) * 1440 - tz2;
        d1.partial_cmp(&d2)
    }
}

impl fmt::Display for GYearValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        format_year(self.year, f)?;
        if let Some(tz) = &self.timezone {
            write!(f, "{}", tz)?;
        }
        Ok(())
    }
}

/// xs:gMonthDay value
#[derive(Debug, Clone, PartialEq)]
pub struct GMonthDayValue {
    pub month: u8,
    pub day: u8,
    pub timezone: Option<TimezoneOffset>,
}

impl PartialOrd for GMonthDayValue {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        let tz1 = self.timezone.map_or(0i64, |tz| tz.0 as i64);
        let tz2 = other.timezone.map_or(0i64, |tz| tz.0 as i64);
        // Use reference year 2000 for gMonthDay comparison
        let d1 = date_to_days(2000, self.month as i32, self.day as i32) * 1440 - tz1;
        let d2 = date_to_days(2000, other.month as i32, other.day as i32) * 1440 - tz2;
        d1.partial_cmp(&d2)
    }
}

impl fmt::Display for GMonthDayValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "--{:02}-{:02}", self.month, self.day)?;
        if let Some(tz) = &self.timezone {
            write!(f, "{}", tz)?;
        }
        Ok(())
    }
}

/// xs:gDay value
#[derive(Debug, Clone, PartialEq)]
pub struct GDayValue {
    pub day: u8,
    pub timezone: Option<TimezoneOffset>,
}

impl PartialOrd for GDayValue {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        let tz1 = self.timezone.map_or(0i64, |tz| tz.0 as i64);
        let tz2 = other.timezone.map_or(0i64, |tz| tz.0 as i64);
        let d1 = (self.day as i64) * 1440 - tz1;
        let d2 = (other.day as i64) * 1440 - tz2;
        d1.partial_cmp(&d2)
    }
}

impl fmt::Display for GDayValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "---{:02}", self.day)?;
        if let Some(tz) = &self.timezone {
            write!(f, "{}", tz)?;
        }
        Ok(())
    }
}

/// xs:gMonth value
#[derive(Debug, Clone, PartialEq)]
pub struct GMonthValue {
    pub month: u8,
    pub timezone: Option<TimezoneOffset>,
}

impl PartialOrd for GMonthValue {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        let tz1 = self.timezone.map_or(0i64, |tz| tz.0 as i64);
        let tz2 = other.timezone.map_or(0i64, |tz| tz.0 as i64);
        let d1 = (self.month as i64) * 1440 - tz1;
        let d2 = (other.month as i64) * 1440 - tz2;
        d1.partial_cmp(&d2)
    }
}

impl fmt::Display for GMonthValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "--{:02}", self.month)?;
        if let Some(tz) = &self.timezone {
            write!(f, "{}", tz)?;
        }
        Ok(())
    }
}

/// Timezone offset in minutes from UTC
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimezoneOffset(pub i16);

impl TimezoneOffset {
    /// UTC timezone
    pub const UTC: Self = Self(0);

    /// Create a timezone offset from hours and minutes
    pub fn from_hm(hours: i8, minutes: i8) -> Self {
        Self(hours as i16 * 60 + minutes as i16)
    }

    /// Get hours component
    pub fn hours(&self) -> i8 {
        (self.0 / 60) as i8
    }

    /// Get minutes component
    pub fn minutes(&self) -> i8 {
        (self.0 % 60).abs() as i8
    }
}

impl fmt::Display for TimezoneOffset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0 == 0 {
            write!(f, "Z")
        } else {
            let sign = if self.0 > 0 { '+' } else { '-' };
            let hours = (self.0.abs() / 60) as u8;
            let minutes = (self.0.abs() % 60) as u8;
            write!(f, "{}{:02}:{:02}", sign, hours, minutes)
        }
    }
}

/// Normalize day-time duration components.
/// Convert a date to total days from a reference epoch (year 1, month 1, day 1).
/// Used for comparing date/time values.
fn date_to_days(year: i32, month: i32, day: i32) -> i64 {
    // Adjust for months < 3 by treating Jan/Feb as months 13/14 of the previous year
    let (y, m) = if month <= 2 {
        (year as i64 - 1, month as i64 + 12)
    } else {
        (year as i64, month as i64)
    };
    // Use a simplified Julian day calculation
    365 * y + y / 4 - y / 100 + y / 400 + (153 * (m - 3) + 2) / 5 + day as i64 - 307
}

///
/// Carries over whole seconds into minutes, minutes into hours, hours into days.
/// Only the integer part of seconds is carried; the fractional part stays in seconds.
fn normalize_day_time(days: u32, hours: u32, minutes: u32, seconds: Decimal) -> (u32, u32, u32, Decimal) {
    let whole_secs = seconds.trunc();
    let frac_secs = seconds - whole_secs;

    let total_secs: u64 = whole_secs.to_u64().unwrap_or(0);
    let mut mins = minutes as u64 + total_secs / 60;
    let rem_secs = (total_secs % 60) as u32;
    let mut hrs = hours as u64 + mins / 60;
    mins %= 60;
    let d = days as u64 + hrs / 24;
    hrs %= 24;

    let out_seconds = Decimal::from(rem_secs) + frac_secs;
    (d as u32, hrs as u32, mins as u32, out_seconds)
}

/// Format seconds with optional fractional part (zero-padded for time-of-day: HH:MM:SS)
fn format_seconds(s: Decimal, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    if s.fract().is_zero() {
        write!(f, "{:02}", s.trunc())
    } else {
        // Format with fractional seconds, trimming trailing zeros
        let formatted = format!("{}", s);
        if let Some(dot_pos) = formatted.find('.') {
            let int_part = &formatted[..dot_pos];
            let frac_part = formatted[dot_pos + 1..].trim_end_matches('0');
            if int_part.len() == 1 {
                write!(f, "0{}.{}", int_part, frac_part)
            } else {
                write!(f, "{}.{}", int_part, frac_part)
            }
        } else {
            write!(f, "{:02}", s)
        }
    }
}

/// Format seconds for duration values (no zero-padding)
fn format_duration_seconds(s: Decimal, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    if s.fract().is_zero() {
        write!(f, "{}", s.trunc())
    } else {
        let formatted = format!("{}", s);
        if let Some(dot_pos) = formatted.find('.') {
            let int_part = &formatted[..dot_pos];
            let frac_part = formatted[dot_pos + 1..].trim_end_matches('0');
            write!(f, "{}.{}", int_part, frac_part)
        } else {
            write!(f, "{}", s)
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_xml_value_string() {
        let v = XmlValue::string("hello");
        assert_eq!(v.type_code, XmlTypeCode::String);
        assert!(v.is_atomic());
        assert_eq!(v.to_string_value(), "hello");
        assert_eq!(v.as_string(), Some("hello"));
    }

    #[test]
    fn test_xml_value_boolean() {
        let v = XmlValue::boolean(true);
        assert_eq!(v.type_code, XmlTypeCode::Boolean);
        assert_eq!(v.as_boolean(), Some(true));
        assert_eq!(v.to_string_value(), "true");

        let v = XmlValue::boolean(false);
        assert_eq!(v.to_string_value(), "false");
    }

    #[test]
    fn test_xml_value_decimal() {
        let v = XmlValue::decimal(Decimal::new(12345, 2));
        assert_eq!(v.type_code, XmlTypeCode::Decimal);
        assert_eq!(v.as_decimal(), Some(Decimal::new(12345, 2)));
    }

    #[test]
    fn test_xml_value_integer() {
        let v = XmlValue::integer(BigInt::from(42));
        assert_eq!(v.type_code, XmlTypeCode::Integer);
        assert_eq!(v.as_integer(), Some(&BigInt::from(42)));
        assert_eq!(v.to_string_value(), "42");
    }

    #[test]
    fn test_xml_value_double() {
        let v = XmlValue::double(2.5);
        assert_eq!(v.type_code, XmlTypeCode::Double);
        assert_eq!(v.as_double(), Some(2.5));
    }

    #[test]
    fn test_xml_value_untyped() {
        let v = XmlValue::untyped("raw text");
        assert_eq!(v.type_code, XmlTypeCode::UntypedAtomic);
        assert!(v.is_untyped());
        assert_eq!(v.as_string(), Some("raw text"));
    }

    #[test]
    fn test_xml_atomic_value_type_code() {
        assert_eq!(XmlAtomicValue::String("test".into()).type_code(), XmlTypeCode::String);
        assert_eq!(XmlAtomicValue::Boolean(true).type_code(), XmlTypeCode::Boolean);
        assert_eq!(XmlAtomicValue::Integer(BigInt::from(1)).type_code(), XmlTypeCode::Integer);
    }

    #[test]
    fn test_timezone_display() {
        assert_eq!(TimezoneOffset::UTC.to_string(), "Z");
        assert_eq!(TimezoneOffset::from_hm(5, 30).to_string(), "+05:30");
        assert_eq!(TimezoneOffset::from_hm(-8, 0).to_string(), "-08:00");
    }

    #[test]
    fn test_date_display() {
        let d = DateValue {
            year: 2024,
            month: 3,
            day: 15,
            timezone: Some(TimezoneOffset::UTC),
        };
        assert_eq!(d.to_string(), "2024-03-15Z");
    }

    #[test]
    fn test_duration_display() {
        let d = DurationValue {
            negative: false,
            years: 1,
            months: 2,
            days: 3,
            hours: 4,
            minutes: 5,
            seconds: Decimal::new(65, 1), // 6.5 seconds
        };
        // Note: format_seconds may zero-pad to 2 digits
        assert!(d.to_string().starts_with("P1Y2M3DT4H5M"));
        assert!(d.to_string().contains("6.5S"));
    }

    #[test]
    fn test_float_special_values() {
        assert_eq!(
            format!("{}", XmlAtomicValue::Float(f32::INFINITY)),
            "INF"
        );
        assert_eq!(
            format!("{}", XmlAtomicValue::Float(f32::NEG_INFINITY)),
            "-INF"
        );
        assert_eq!(
            format!("{}", XmlAtomicValue::Float(f32::NAN)),
            "NaN"
        );
    }

    #[test]
    fn test_hex_binary_display() {
        let v = XmlAtomicValue::HexBinary(vec![0xDE, 0xAD, 0xBE, 0xEF]);
        assert_eq!(format!("{}", v), "DEADBEEF");
    }

    #[test]
    fn test_base64_binary_display() {
        let v = XmlAtomicValue::Base64Binary(b"Hello".to_vec());
        assert_eq!(format!("{}", v), "SGVsbG8=");
    }
}
