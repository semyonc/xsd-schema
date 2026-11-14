//! The plan's value checks: white space, the lexical forms of the built-in
//! datatypes and their facets, with no `SchemaSet` and no allocation.
//!
//! Every check under-approximates what the dynamic path does with default
//! flags (`validate_atomic_type`): it accepts only forms it is certain that
//! path accepts and answers `Unknown` for the rest. Where the dynamic path
//! has a pure lexical function the check calls it (the name predicates, the
//! decimal and float lexical forms, the integer gate, the `hex` and `base64`
//! decoders the validators use, the time line's day numbers). Where it has
//! none that runs without allocating (dates, times, durations), the check
//! reads a strict common form of the lexical space:
//!
//! - years of exactly four digits, `0001` to `9999` (no sign, no year zero,
//!   so the XSD 1.0 and 1.1 readings of the year agree);
//! - two-digit months, days, hours, minutes and seconds in range, the day in
//!   the Gregorian calendar, `24:00:00` with zero fraction only;
//! - a time zone `Z` or `±hh:mm` up to `14:00`;
//! - duration fields of at most nine digits (seconds: eighteen whole digits).
//!
//! Range facets on dates and times compare positions on the time line only
//! when the value and the bound agree on having a time zone.

use std::cmp::Ordering;

use super::interp::{in_alphabet, table_accepts, UnknownReason};
use super::model::*;
use crate::types::validators::{
    is_valid_language, is_valid_name, is_valid_ncname, is_valid_nmtoken,
    is_valid_xsd_decimal_lexical, parse_double, parse_float,
};
use crate::validation::simple::integer_lexical_value;

/// Whether `b` is XML white space (#x20, #x9, #xA, #xD), the only white
/// space `whiteSpace` normalizes.
#[inline]
pub(crate) fn is_xml_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r')
}

/// `value` normalized as `ws` says (Datatypes §4.3.6), byte for byte as the
/// dynamic path's `normalize_whitespace` normalizes it: borrowed when nothing
/// changes, else written into `buf`.
pub(crate) fn normalize<'a>(value: &'a str, ws: Ws, buf: &'a mut String) -> &'a str {
    match ws {
        Ws::Preserve => value,
        Ws::Replace => {
            if !value.bytes().any(|b| matches!(b, b'\t' | b'\n' | b'\r')) {
                return value;
            }
            buf.clear();
            for c in value.chars() {
                buf.push(if matches!(c, '\t' | '\n' | '\r') {
                    ' '
                } else {
                    c
                });
            }
            buf
        }
        Ws::Collapse => {
            let b = value.as_bytes();
            // One pass: a value with no #x9, #xA, #xD, no space at either end
            // and no two spaces in a row is its own collapsed form.
            let mut clean = b.first() != Some(&b' ') && b.last() != Some(&b' ');
            let mut prev_space = false;
            for &c in b {
                match c {
                    b'\t' | b'\n' | b'\r' => {
                        clean = false;
                        break;
                    }
                    b' ' if prev_space => {
                        clean = false;
                        break;
                    }
                    b' ' => prev_space = true,
                    _ => prev_space = false,
                }
            }
            if clean {
                return value;
            }
            let Some(start) = b.iter().position(|&c| !is_xml_space(c)) else {
                return "";
            };
            let end = b
                .iter()
                .rposition(|&c| !is_xml_space(c))
                .map_or(start, |e| e + 1);
            let mid = &value[start..end];
            buf.clear();
            for (i, word) in mid
                .split(|c: char| c.is_ascii() && is_xml_space(c as u8))
                .filter(|w| !w.is_empty())
                .enumerate()
            {
                if i > 0 {
                    buf.push(' ');
                }
                buf.push_str(word);
            }
            buf
        }
    }
}

/// A value read in its lexical space, as far as the facets need it.
enum Read<'v> {
    /// No value the facets compare (strings, names, booleans, …).
    Plain,
    /// An octet count (binary).
    Octets(usize),
    /// A decimal `mant / 10^scale` with its `totalDigits` and
    /// `fractionDigits`.
    Number {
        mant: i128,
        scale: u32,
        total: u32,
        fraction: u32,
    },
    /// A position on the time line (whole seconds, fraction digits) and
    /// whether the value has a time zone.
    Moment {
        whole: i128,
        frac: &'v str,
        tz: bool,
    },
    /// A value whose only facet-relevant property is its time zone (the
    /// `g*` types).
    Zoned(bool),
}

/// Run a `Value` check on `value`: the normalized value when it passes,
/// `Unknown` otherwise. The value is unique in nothing here; the caller
/// keeps `xs:ID` values.
pub(crate) fn check_value<'a>(
    plan: PlanRef<'_>,
    c: &CheckPlan,
    value: &'a str,
    buf: &'a mut String,
) -> Result<&'a str, UnknownReason> {
    use UnknownReason::Value as Bad;
    let v = normalize(value, c.ws, buf);
    let read = match c.lex {
        Lex::String => Read::Plain,
        Lex::Name => ok(is_valid_name(v), Read::Plain)?,
        Lex::NCName => ok(is_valid_ncname(v), Read::Plain)?,
        Lex::NmToken => ok(is_valid_nmtoken(v), Read::Plain)?,
        Lex::Language => ok(is_valid_language(v), Read::Plain)?,
        Lex::Boolean => ok(matches!(v, "true" | "false" | "1" | "0"), Read::Plain)?,
        Lex::Float => ok(
            parse_float(v).is_ok() && !(c.flags & CHECK_XSD10 != 0 && v == "+INF"),
            Read::Plain,
        )?,
        Lex::Double => ok(
            parse_double(v).is_ok() && !(c.flags & CHECK_XSD10 != 0 && v == "+INF"),
            Read::Plain,
        )?,
        Lex::Integer => {
            let i = integer_lexical_value(v).ok_or(Bad)?;
            let b = plan
                .bounds
                .get(c.bounds as usize)
                .ok_or(UnknownReason::Construct)?;
            if i < b.int_lo || i > b.int_hi {
                return Err(Bad);
            }
            Read::Number {
                mant: i,
                scale: 0,
                total: digit_count(i.unsigned_abs()),
                fraction: 0,
            }
        }
        Lex::Decimal => {
            if !is_valid_xsd_decimal_lexical(v) {
                return Err(Bad);
            }
            let (mant, scale, total, fraction) = read_decimal(v).ok_or(Bad)?;
            Read::Number {
                mant,
                scale,
                total,
                fraction,
            }
        }
        Lex::DateTime | Lex::Date | Lex::Time => {
            let (whole, frac, tz) = read_moment(v, c.lex).ok_or(Bad)?;
            Read::Moment { whole, frac, tz }
        }
        Lex::GYearMonth | Lex::GYear | Lex::GMonthDay | Lex::GDay | Lex::GMonth => {
            Read::Zoned(read_gregorian(v, c.lex).ok_or(Bad)?)
        }
        Lex::Duration | Lex::YearMonthDuration | Lex::DayTimeDuration => {
            ok(read_duration(v, c.lex), Read::Plain)?
        }
        Lex::HexBinary => Read::Octets(hex_octets(v).ok_or(Bad)?),
        Lex::Base64Binary => Read::Octets(base64_octets(v).ok_or(Bad)?),
    };

    // explicitTimezone.
    if c.flags & (CHECK_TZ_REQUIRED | CHECK_TZ_PROHIBITED) != 0 {
        let tz = match read {
            Read::Moment { tz, .. } | Read::Zoned(tz) => tz,
            _ => return Err(UnknownReason::Construct),
        };
        if (c.flags & CHECK_TZ_REQUIRED != 0 && !tz) || (c.flags & CHECK_TZ_PROHIBITED != 0 && tz) {
            return Err(Bad);
        }
    }

    // Lengths: octets for binary, characters for the string-like families.
    if c.min_len != 0 || c.max_len != NONE {
        let n = match (c.lex, &read) {
            (_, Read::Octets(n)) => *n as u64,
            (Lex::String | Lex::Name | Lex::NCName | Lex::NmToken | Lex::Language, _) => {
                v.chars().count() as u64
            }
            _ => return Err(UnknownReason::Construct),
        };
        if n < u64::from(c.min_len) || (c.max_len != NONE && n > u64::from(c.max_len)) {
            return Err(Bad);
        }
    }

    // Enumeration: the normalized value against the literals as written. A
    // literal equal to the value denotes the value in every datatype here.
    if c.enum_start != NONE {
        let lits = plan
            .enum_literals
            .get(c.enum_start as usize..c.enum_end as usize)
            .ok_or(UnknownReason::Construct)?;
        let found = lits
            .binary_search_by(|&l| {
                let span = plan.literals.get(l as usize).copied();
                span.map_or(&[][..], |sp| plan.span(sp)).cmp(v.as_bytes())
            })
            .is_ok();
        if !found {
            return Err(Bad);
        }
    }

    // Patterns, on the normalized value. Where the dynamic path matches the
    // value as written, only a value that normalization leaves alone is
    // decided here.
    if c.pat_start != c.pat_end {
        if c.flags & CHECK_RAW_PATTERNS != 0 && v.len() != value.len() {
            return Err(Bad);
        }
        let steps = plan
            .pattern_steps
            .get(c.pat_start as usize..c.pat_end as usize)
            .ok_or(UnknownReason::Construct)?;
        for step in steps {
            let table = plan.patterns.get(step.table as usize).ok_or(Bad)?;
            let alphabet = plan.alphabets.get(step.alphabet as usize).ok_or(Bad)?;
            if !in_alphabet(plan, alphabet, v) || !table_accepts(plan, table, v.as_bytes()) {
                return Err(Bad);
            }
        }
    }

    // Range and digit facets.
    if c.bounds != NONE {
        let b = plan
            .bounds
            .get(c.bounds as usize)
            .ok_or(UnknownReason::Construct)?;
        match read {
            Read::Number {
                mant,
                scale,
                total,
                fraction,
            } => {
                if (b.total_digits != NONE && total > b.total_digits)
                    || (b.fraction_digits != NONE && fraction > b.fraction_digits)
                {
                    return Err(Bad);
                }
                let order = |bound: &Bound| decimal_cmp(mant, scale, bound.mant, bound.scale);
                in_range(b, order)?;
            }
            Read::Moment { whole, frac, tz } => {
                let any = b.min_incl.set || b.min_excl.set || b.max_incl.set || b.max_excl.set;
                if any && tz != b.zoned {
                    return Err(Bad);
                }
                let order = |bound: &Bound| {
                    Some(moment_cmp(whole, frac, bound.mant, plan.span(bound.frac)))
                };
                in_range(b, order)?;
            }
            _ => return Err(UnknownReason::Construct),
        }
    }
    Ok(v)
}

#[inline]
fn ok<T>(cond: bool, t: T) -> Result<T, UnknownReason> {
    if cond {
        Ok(t)
    } else {
        Err(UnknownReason::Value)
    }
}

/// The four range facets, given the order of the value to a bound (`None`:
/// not decidable here).
fn in_range(b: &Bounds, order: impl Fn(&Bound) -> Option<Ordering>) -> Result<(), UnknownReason> {
    use Ordering::*;
    let holds = |bound: &Bound, admitted: &[Ordering]| {
        !bound.set || order(bound).is_some_and(|o| admitted.contains(&o))
    };
    if holds(&b.min_incl, &[Greater, Equal])
        && holds(&b.min_excl, &[Greater])
        && holds(&b.max_incl, &[Less, Equal])
        && holds(&b.max_excl, &[Less])
    {
        Ok(())
    } else {
        Err(UnknownReason::Value)
    }
}

/// The number of decimal digits of `n`, at least one (`totalDigits` of an
/// integer value: Datatypes 1.1 §4.3.11.3).
pub(crate) fn digit_count(mut n: u128) -> u32 {
    let mut count = 1;
    while n >= 10 {
        n /= 10;
        count += 1;
    }
    count
}

/// The most significant digits a decimal read here may have: well inside
/// `i128`, so that scaling two of them to one scale rarely overflows (and an
/// overflow answers `Unknown`).
pub(crate) const DECIMAL_DIGITS: u32 = 36;

/// A decimal lexical form (already checked by `is_valid_xsd_decimal_lexical`)
/// as `(mantissa, scale, totalDigits, fractionDigits)`: leading zeros and
/// trailing fraction zeros carry nothing (Datatypes 1.1 §3.3.3: "the number
/// 2.0 is not distinct from the number 2.00"); `totalDigits` is the larger of
/// the significant digits and the scale, at least one (§4.3.11.3).
pub(crate) fn read_decimal(v: &str) -> Option<(i128, u32, u32, u32)> {
    let (neg, rest) = match v.as_bytes().first() {
        Some(b'-') => (true, &v[1..]),
        Some(b'+') => (false, &v[1..]),
        _ => (false, v),
    };
    let (int, frac) = rest.split_once('.').unwrap_or((rest, ""));
    let int = int.trim_start_matches('0');
    let frac = frac.trim_end_matches('0');
    let mut mant: i128 = 0;
    let mut digits = 0u32;
    for b in int.bytes().chain(frac.bytes()) {
        let d = i128::from(b.checked_sub(b'0').filter(|d| *d <= 9)?);
        if digits == 0 && d == 0 {
            continue;
        }
        digits += 1;
        if digits > DECIMAL_DIGITS {
            return None;
        }
        mant = mant * 10 + d;
    }
    let scale = u32::try_from(frac.len()).ok()?;
    let total = digits.max(scale).max(1);
    Some((if neg { -mant } else { mant }, scale, total, scale))
}

/// The order of `a / 10^sa` and `b / 10^sb`, exactly (`None` when scaling
/// one of them to the other's scale leaves `i128`).
pub(crate) fn decimal_cmp(a: i128, sa: u32, b: i128, sb: u32) -> Option<Ordering> {
    match sa.cmp(&sb) {
        Ordering::Equal => Some(a.cmp(&b)),
        Ordering::Less => Some(a.checked_mul(10i128.checked_pow(sb - sa)?)?.cmp(&b)),
        Ordering::Greater => Some(a.cmp(&b.checked_mul(10i128.checked_pow(sa - sb)?)?)),
    }
}

/// The order of two positions on the time line: whole seconds, then the
/// fraction digits (a missing digit is a zero).
pub(crate) fn moment_cmp(aw: i128, af: &str, bw: i128, bf: &[u8]) -> Ordering {
    aw.cmp(&bw).then_with(|| {
        let (a, b) = (af.as_bytes(), bf);
        for i in 0..a.len().max(b.len()) {
            let x = a.get(i).copied().unwrap_or(b'0');
            let y = b.get(i).copied().unwrap_or(b'0');
            match x.cmp(&y) {
                Ordering::Equal => {}
                o => return o,
            }
        }
        Ordering::Equal
    })
}

/// `n` ASCII digits at the start of `s` as a number, the rest of `s`.
fn digits(s: &[u8], n: usize) -> Option<(u32, &[u8])> {
    if s.len() < n {
        return None;
    }
    let mut v = 0u32;
    for &b in &s[..n] {
        if !b.is_ascii_digit() {
            return None;
        }
        v = v * 10 + u32::from(b - b'0');
    }
    Some((v, &s[n..]))
}

/// A time zone suffix, as the dynamic path's `split_timezone` reads it on
/// the forms read here: `Z`, or `±hh:mm` up to `14:00` (the six last bytes);
/// anything else leaves the whole value as the body. The body and the offset
/// in minutes.
fn split_zone(v: &[u8]) -> (&[u8], Option<i32>) {
    if let Some(body) = v.strip_suffix(b"Z") {
        return (body, Some(0));
    }
    let n = v.len();
    if n > 6 && matches!(v[n - 6], b'+' | b'-') {
        let offset = || {
            let (h, rest) = digits(&v[n - 5..], 2)?;
            let rest = rest.strip_prefix(b":")?;
            let (m, rest) = digits(rest, 2)?;
            if !rest.is_empty() || h > 14 || m > 59 || (h == 14 && m != 0) {
                return None;
            }
            Some((h * 60 + m) as i32)
        };
        if let Some(off) = offset() {
            return (&v[..n - 6], Some(if v[n - 6] == b'-' { -off } else { off }));
        }
    }
    (v, None)
}

fn days_in_month(year: u32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400) => {
            29
        }
        2 => 28,
        _ => 0,
    }
}

/// `YYYY-MM-DD` with a four-digit year from 0001: (year, month, day), rest.
fn read_ymd(s: &[u8]) -> Option<(u32, u32, u32, &[u8])> {
    let (y, s) = digits(s, 4)?;
    let s = s.strip_prefix(b"-")?;
    let (m, s) = digits(s, 2)?;
    let s = s.strip_prefix(b"-")?;
    let (d, s) = digits(s, 2)?;
    if y == 0 || !(1..=12).contains(&m) || d == 0 || d > days_in_month(y, m) {
        return None;
    }
    Some((y, m, d, s))
}

/// `hh:mm:ss(.s+)?`: seconds from the start of the day (`24:00:00` is
/// 86,400, the start of the next day) and the fraction digits.
fn read_hms(s: &[u8]) -> Option<(i128, &[u8])> {
    let (h, s) = digits(s, 2)?;
    let s = s.strip_prefix(b":")?;
    let (m, s) = digits(s, 2)?;
    let s = s.strip_prefix(b":")?;
    let (sec, s) = digits(s, 2)?;
    let frac = match s.strip_prefix(b".") {
        Some(f) if !f.is_empty() && f.iter().all(u8::is_ascii_digit) => f,
        Some(_) => return None,
        None if s.is_empty() => s,
        None => return None,
    };
    let midnight = h == 24 && m == 0 && sec == 0 && frac.iter().all(|&b| b == b'0');
    if (h > 23 && !midnight) || m > 59 || sec > 59 {
        return None;
    }
    Some((i128::from(h * 3600 + m * 60 + sec), frac))
}

/// A `dateTime`, `date` or `time` value in the forms read here: its whole
/// seconds on the time line (the time zone applied, a missing one read as
/// `Z`), its fraction digits (no trailing zero) and whether it has a time
/// zone.
pub(crate) fn read_moment(v: &str, lex: Lex) -> Option<(i128, &str, bool)> {
    let (body, tz) = split_zone(v.as_bytes());
    let (days, secs, frac) = match lex {
        Lex::Date => {
            let (y, m, d, rest) = read_ymd(body)?;
            if !rest.is_empty() {
                return None;
            }
            (day_number(y, m, d), 0, &[][..])
        }
        Lex::DateTime => {
            let (y, m, d, rest) = read_ymd(body)?;
            let (secs, frac) = read_hms(rest.strip_prefix(b"T")?)?;
            (day_number(y, m, d), secs, frac)
        }
        Lex::Time => {
            // A time of day has no day to carry into: `24:00:00` is
            // `00:00:00` (Datatypes §3.3.8).
            let (secs, frac) = read_hms(body)?;
            (0, secs % 86_400, frac)
        }
        _ => return None,
    };
    let offset = i128::from(tz.unwrap_or(0));
    let whole = days * 86_400 + secs - offset * 60;
    // The fraction is ASCII digits, a suffix of the body.
    let frac = std::str::from_utf8(frac).ok()?.trim_end_matches('0');
    Some((whole, frac, tz.is_some()))
}

/// The day of `year-month-day` from 1970-01-01, by the time line's own count.
fn day_number(year: u32, month: u32, day: u32) -> i128 {
    // Four-digit years, months and days in range: the casts are exact.
    crate::types::value::day_number(year as i32, month as u8, day as u8)
}

/// A `gYearMonth`, `gYear`, `gMonthDay`, `gDay` or `gMonth` value in the
/// forms read here: whether it has a time zone.
pub(crate) fn read_gregorian(v: &str, lex: Lex) -> Option<bool> {
    let (body, tz) = split_zone(v.as_bytes());
    let done = match lex {
        Lex::GYearMonth => {
            let (y, s) = digits(body, 4)?;
            let s = s.strip_prefix(b"-")?;
            let (m, s) = digits(s, 2)?;
            y != 0 && (1..=12).contains(&m) && s.is_empty()
        }
        Lex::GYear => {
            let (y, s) = digits(body, 4)?;
            y != 0 && s.is_empty()
        }
        Lex::GMonthDay => {
            let s = body.strip_prefix(b"--")?;
            let (m, s) = digits(s, 2)?;
            let s = s.strip_prefix(b"-")?;
            let (d, s) = digits(s, 2)?;
            // Any year: February 29 is a day.
            (1..=12).contains(&m) && d >= 1 && d <= days_in_month(2000, m) && s.is_empty()
        }
        Lex::GDay => {
            let s = body.strip_prefix(b"---")?;
            let (d, s) = digits(s, 2)?;
            (1..=31).contains(&d) && s.is_empty()
        }
        Lex::GMonth => {
            let s = body.strip_prefix(b"--")?;
            let (m, s) = digits(s, 2)?;
            (1..=12).contains(&m) && s.is_empty()
        }
        _ => false,
    };
    done.then_some(tz.is_some())
}

/// A duration in the forms read here: `-?P(nY)?(nM)?(nD)?(T(nH)?(nM)?(n(.n+)?S)?)?`
/// with at least one field, at least one after `T`, at most nine digits per
/// integer field and eighteen whole digits of seconds; `yearMonthDuration`
/// without day and time, `dayTimeDuration` without year and month and with
/// one of them.
pub(crate) fn read_duration(v: &str, lex: Lex) -> bool {
    let s = v.as_bytes();
    let s = s.strip_prefix(b"-").unwrap_or(s);
    let Some(mut s) = s.strip_prefix(b"P") else {
        return false;
    };
    let mut fields = 0u8; // bits: Y M D H Mi S
    let mut in_time = false;
    let mut time_fields = 0;
    // The rank of the last field, so they come in order.
    let mut last = 0u8;
    while !s.is_empty() {
        if s[0] == b'T' {
            if in_time {
                return false;
            }
            in_time = true;
            s = &s[1..];
            continue;
        }
        let n = s.iter().take_while(|b| b.is_ascii_digit()).count();
        if n == 0 {
            return false;
        }
        let mut rest = &s[n..];
        let mut fraction = false;
        if let Some(f) = rest.strip_prefix(b".") {
            let k = f.iter().take_while(|b| b.is_ascii_digit()).count();
            if k == 0 {
                return false;
            }
            fraction = true;
            rest = &f[k..];
        }
        let Some((&d, after)) = rest.split_first() else {
            return false;
        };
        let rank = match (d, in_time) {
            (b'Y', false) => 1,
            (b'M', false) => 2,
            (b'D', false) => 3,
            (b'H', true) => 4,
            (b'M', true) => 5,
            (b'S', true) => 6,
            _ => return false,
        };
        let max = if rank == 6 { 18 } else { 9 };
        if rank <= last || n > max || (fraction && rank != 6) {
            return false;
        }
        last = rank;
        fields |= 1 << (rank - 1);
        if in_time {
            time_fields += 1;
        }
        s = after;
    }
    if fields == 0 || (in_time && time_fields == 0) {
        return false;
    }
    match lex {
        Lex::Duration => true,
        // No day field and no time part.
        Lex::YearMonthDuration => fields & 0b11_1100 == 0 && !in_time,
        // No year or month field, a day field or a time part.
        Lex::DayTimeDuration => fields & 0b11 == 0 && (fields & 0b100 != 0 || in_time),
        _ => false,
    }
}

/// The octet count of a `hexBinary` lexical form, decoded as the validator
/// decodes it (`hex`), in chunks: no allocation.
pub(crate) fn hex_octets(v: &str) -> Option<usize> {
    let b = v.as_bytes();
    if !b.len().is_multiple_of(2) {
        return None;
    }
    let mut out = [0u8; 256];
    for chunk in b.chunks(512) {
        hex::decode_to_slice(chunk, &mut out[..chunk.len() / 2]).ok()?;
    }
    Some(b.len() / 2)
}

/// The octet count of a `base64Binary` lexical form (already collapsed),
/// decoded as the validator decodes it (`base64`'s standard engine: padding
/// required, canonical trailing bits), in chunks of whole quanta: no
/// allocation. A chunk before the last one with padding answers `None`.
pub(crate) fn base64_octets(v: &str) -> Option<usize> {
    use base64::Engine;
    const CHUNK: usize = 1024;
    let b = v.as_bytes();
    let mut out = [0u8; CHUNK];
    let mut total = 0;
    let mut i = 0;
    loop {
        let end = (i + CHUNK).min(b.len());
        let chunk = &b[i..end];
        if end < b.len() && chunk.contains(&b'=') {
            return None;
        }
        total += base64::engine::general_purpose::STANDARD
            .decode_slice(chunk, &mut out)
            .ok()?;
        i = end;
        if i >= b.len() {
            return Some(total);
        }
    }
}
