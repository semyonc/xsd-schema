#![cfg(feature = "xsd11")]
//! The implicit timezone of the XPath dynamic context.
//!
//! F&O 1.0 §10.4: "If either operand to a comparison function on date or time
//! values does not have an (explicit) timezone then, for the purpose of the
//! operation, an implicit timezone, provided by the dynamic context …, is
//! assumed to be present as part of the value. This creates a total order for
//! all date and time values." The same holds for the subtraction of two
//! date/time values (§10.8.1–§10.8.3), the one-argument
//! `fn:adjust-*-to-timezone` functions (§10.7) and `fn:implicit-timezone`
//! (§16.6); `fn:current-dateTime` (§16.3) "Returns the current dateTime (with
//! timezone) from the dynamic context", and is ·stable·.
//!
//! A dynamic context without an implicit timezone uses UTC, whatever the
//! `TZ` of the process.
//!
//! The tests below set a context timezone of +09:45, which neither UTC nor any
//! zone in use today has, so that a comparison reading the default (or the
//! host's offset) instead of the context fails on any machine;
//! `the_fallback_is_utc_under_any_process_time_zone` runs the fallback under
//! `TZ=UTC` and two other zones.

use xsd_schema::namespace::context::NamespaceContextSnapshot;
use xsd_schema::namespace::table::NameTable;
use xsd_schema::types::value::TimezoneOffset;
use xsd_schema::xpath::{RoXmlNavigator, XPathContext, XPathExpr};
use xsd_schema::XmlItem;

const XS: &str = "http://www.w3.org/2001/XMLSchema";

/// A context timezone (minutes) that is not UTC, nor the offset of any zone
/// in use today: +09:45.
fn context_minutes() -> i16 {
    9 * 60 + 45
}

/// `±hh:mm` (or `Z`) for an offset in minutes.
fn lexical(minutes: i16) -> String {
    TimezoneOffset(minutes).to_string()
}

/// `PTnHmM` / `-PTnHmM` / `PT0S`: the `xs:dayTimeDuration` of an offset.
fn duration(minutes: i16) -> String {
    if minutes == 0 {
        return "PT0S".to_string();
    }
    let sign = if minutes < 0 { "-" } else { "" };
    let (h, m) = (minutes.unsigned_abs() / 60, minutes.unsigned_abs() % 60);
    match (h, m) {
        (h, 0) => format!("{sign}PT{h}H"),
        (0, m) => format!("{sign}PT{m}M"),
        (h, m) => format!("{sign}PT{h}H{m}M"),
    }
}

fn names_with_xs(names: &NameTable) -> NamespaceContextSnapshot {
    let mut namespaces = NamespaceContextSnapshot::default();
    namespaces.bindings.push((names.add("xs"), names.add(XS)));
    namespaces
}

fn join(value: xsd_schema::xpath::XPathValue<RoXmlNavigator<'static>>) -> String {
    value
        .into_vec()
        .into_iter()
        .map(|item| match item {
            XmlItem::Atomic(a) => a.to_string_value(),
            XmlItem::Node(_) => "<node>".to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Evaluate `expr` with the static context's implicit timezone `tz` (`None`:
/// the default, UTC); the result's items joined by spaces, or the error code.
fn eval(tz: Option<i16>, expr: &str) -> Result<String, String> {
    let names = NameTable::new();
    let mut ctx = XPathContext::new(&names).with_namespaces(names_with_xs(&names));
    if let Some(minutes) = tz {
        ctx = ctx.with_implicit_timezone(TimezoneOffset(minutes));
    }
    let compiled = XPathExpr::compile(expr, &ctx).map_err(|e| format!("compile {e:?}"))?;
    compiled
        .evaluator(&ctx)
        .run::<RoXmlNavigator<'static>>()
        .map(join)
        .map_err(|e| e.error_code().unwrap_or("?").to_string())
}

/// [`eval`] with the implicit timezone set on the *dynamic* context only, as a
/// host does that builds the dynamic context itself.
fn eval_dynamic(tz: i16, expr: &str) -> Result<String, String> {
    let names = NameTable::new();
    let ctx = XPathContext::new(&names).with_namespaces(names_with_xs(&names));
    let compiled = XPathExpr::compile(expr, &ctx).map_err(|e| format!("compile {e:?}"))?;
    compiled
        .evaluator(&ctx)
        .run_with::<RoXmlNavigator<'static>, _>(|ev| {
            ev.context().implicit_timezone = Some(TimezoneOffset(tz));
        })
        .map(join)
        .map_err(|e| e.error_code().unwrap_or("?").to_string())
}

fn ok(tz: Option<i16>, expr: &str) -> String {
    eval(tz, expr).unwrap_or_else(|e| panic!("{expr}: {e}"))
}

/// Rows of `(expression, expected)` with `{tz}` replaced by the context
/// timezone's lexical form and `{dur}` by its duration.
fn check_rows(tz: i16, rows: &[(&str, &str)], dynamic: bool) {
    let mut failures = Vec::new();
    for (expr, expected) in rows {
        let expr = expr
            .replace("{tz}", &lexical(tz))
            .replace("{dur}", &duration(tz));
        let expected = expected
            .replace("{tz}", &lexical(tz))
            .replace("{dur}", &duration(tz));
        let got = if dynamic {
            eval_dynamic(tz, &expr)
        } else {
            eval(Some(tz), &expr)
        };
        if got.as_deref() != Ok(expected.as_str()) {
            failures.push(format!("{expr}\n    expected {expected}, got {got:?}"));
        }
    }
    assert!(
        failures.is_empty(),
        "context timezone {}:\n{}",
        lexical(tz),
        failures.join("\n")
    );
}

/// Comparisons, subtraction and the timezone functions of a context timezone
/// `{tz}` that is not UTC.
const CONTEXT_ROWS: &[(&str, &str)] = &[
    // §10.4.6 – §10.4.14: a value without a timezone is read in `{tz}`.
    ("xs:dateTime('2000-01-01T00:00:00') eq xs:dateTime('2000-01-01T00:00:00{tz}')", "true"),
    ("xs:dateTime('2000-01-01T00:00:00') eq xs:dateTime('2000-01-01T00:00:00Z')", "false"),
    ("xs:dateTime('2000-01-01T00:00:00') ne xs:dateTime('2000-01-01T00:00:00{tz}')", "false"),
    ("xs:date('2000-01-01') eq xs:date('2000-01-01{tz}')", "true"),
    ("xs:date('2000-01-01') eq xs:date('2000-01-01Z')", "false"),
    ("xs:time('12:00:00') eq xs:time('12:00:00{tz}')", "true"),
    ("xs:time('12:00:00') eq xs:time('12:00:00Z')", "false"),
    ("xs:dateTime('2000-01-01T00:00:00') le xs:dateTime('2000-01-01T00:00:00{tz}')", "true"),
    ("xs:dateTime('2000-01-01T00:00:00') ge xs:dateTime('2000-01-01T00:00:00{tz}')", "true"),
    ("xs:dateTime('2000-01-01T00:00:00') lt xs:dateTime('2000-01-01T00:00:00.001{tz}')", "true"),
    ("xs:dateTime('2000-01-01T00:00:00.001') gt xs:dateTime('2000-01-01T00:00:00{tz}')", "true"),
    ("xs:date('2000-01-01') lt xs:date('2000-01-02{tz}')", "true"),
    ("xs:time('12:00:00.5') gt xs:time('12:00:00{tz}')", "true"),
    // General comparisons, pairwise and through the index (more than 64 pairs).
    ("xs:dateTime('2000-01-01T00:00:00') = xs:dateTime('2000-01-01T00:00:00{tz}')", "true"),
    (
        "(for $i in 1 to 100 return xs:dateTime('2000-01-01T00:00:00')) = (for $i in 1 to 100 return xs:dateTime('2000-01-01T00:00:00{tz}'))",
        "true",
    ),
    (
        "(for $i in 1 to 100 return xs:date('2000-01-01')) = (for $i in 1 to 100 return xs:date('2000-01-01{tz}'))",
        "true",
    ),
    (
        "(for $i in 1 to 100 return xs:time('12:00:00')) = (for $i in 1 to 100 return xs:time('12:00:00{tz}'))",
        "true",
    ),
    (
        "(for $i in 1 to 100 return xs:dateTime('2000-01-01T00:00:00')) = (for $i in 1 to 100 return xs:dateTime('2000-01-01T00:00:00Z'))",
        "false",
    ),
    ("xs:dateTime('2000-01-01T00:00:00') < xs:dateTime('2000-01-01T00:00:00.001{tz}')", "true"),
    // Functions that compare.
    (
        "count(distinct-values((xs:dateTime('2000-01-01T00:00:00'), xs:dateTime('2000-01-01T00:00:00{tz}'))))",
        "1",
    ),
    (
        "index-of((xs:dateTime('2000-01-01T00:00:00Z'), xs:dateTime('2000-01-01T00:00:00{tz}')), xs:dateTime('2000-01-01T00:00:00'))",
        "2",
    ),
    ("deep-equal(xs:date('2000-01-01'), xs:date('2000-01-01{tz}'))", "true"),
    (
        "string(max((xs:time('12:00:00'), xs:time('12:00:00.001{tz}'))))",
        "12:00:00.001{tz}",
    ),
    ("string(min((xs:time('12:00:00'), xs:time('12:00:00.001{tz}'))))", "12:00:00"),
    // §10.8.1 – §10.8.3: subtraction.
    ("xs:dateTime('2000-01-01T00:00:00') - xs:dateTime('2000-01-01T00:00:00{tz}')", "PT0S"),
    ("xs:date('2000-01-02') - xs:date('2000-01-01{tz}')", "P1D"),
    ("xs:time('12:00:00') - xs:time('12:00:00{tz}')", "PT0S"),
    // §10.7, §16.6.
    ("implicit-timezone()", "{dur}"),
    (
        "adjust-dateTime-to-timezone(xs:dateTime('2000-01-01T00:00:00')) eq xs:dateTime('2000-01-01T00:00:00{tz}')",
        "true",
    ),
    ("timezone-from-dateTime(adjust-dateTime-to-timezone(xs:dateTime('2000-01-01T00:00:00Z')))", "{dur}"),
    ("timezone-from-date(adjust-date-to-timezone(xs:date('2000-01-01')))", "{dur}"),
    ("timezone-from-time(adjust-time-to-timezone(xs:time('12:00:00Z')))", "{dur}"),
    // §16.3 – §16.5: the current dateTime carries the implicit timezone.
    ("timezone-from-dateTime(current-dateTime())", "{dur}"),
    ("timezone-from-date(current-date())", "{dur}"),
    ("timezone-from-time(current-time())", "{dur}"),
    ("current-dateTime() eq current-dateTime()", "true"),
    ("current-date() eq xs:date(current-dateTime())", "true"),
    ("current-time() eq xs:time(current-dateTime())", "true"),
];

#[test]
fn comparisons_read_the_context_implicit_timezone() {
    check_rows(context_minutes(), CONTEXT_ROWS, false);
}

/// The same rows with the timezone set on the dynamic context after it was
/// built — `DynamicContext::implicit_timezone`, not the static context's copy.
#[test]
fn comparisons_read_the_dynamic_context_implicit_timezone() {
    check_rows(context_minutes(), CONTEXT_ROWS, true);
}

/// Also at the ends of the range of timezones, ±14:00.
#[test]
fn comparisons_read_extreme_context_timezones() {
    for tz in [14 * 60, -14 * 60] {
        check_rows(tz, &CONTEXT_ROWS[..12], false);
    }
}

/// Seconds since 1970-01-01T00:00:00Z of `current-dateTime()`.
fn epoch_seconds(eval: impl Fn(&str) -> Result<String, String>) -> f64 {
    let s = eval(
        "(current-dateTime() - xs:dateTime('1970-01-01T00:00:00Z')) div xs:dayTimeDuration('PT1S')",
    )
    .expect("current-dateTime() evaluates");
    s.parse().unwrap_or_else(|_| panic!("a number: {s}"))
}

fn now_epoch_seconds() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("after 1970")
        .as_secs_f64()
}

/// §16.3: the current instant, expressed in the context's timezone — not the
/// wall-clock time of another zone labelled with the context's offset.
#[test]
fn current_date_time_is_the_current_instant_in_the_context_timezone() {
    let tz = context_minutes();
    for (label, seconds) in [
        ("static", epoch_seconds(|e| eval(Some(tz), e))),
        ("dynamic", epoch_seconds(|e| eval_dynamic(tz, e))),
        ("default", epoch_seconds(|e| eval(None, e))),
    ] {
        let skew = (seconds - now_epoch_seconds()).abs();
        assert!(
            skew < 60.0,
            "{label}: current-dateTime() is {skew} s away from now (context {})",
            lexical(tz)
        );
    }
}

/// Without a context timezone the implicit timezone is UTC: the timezone
/// `current-dateTime()` carries, and the one comparisons, subtraction and the
/// one-argument `fn:adjust-*-to-timezone` use.
fn assert_the_fallback_is_utc() {
    assert_eq!(ok(None, "implicit-timezone()"), "PT0S");
    assert_eq!(
        ok(None, "timezone-from-dateTime(current-dateTime())"),
        "PT0S"
    );
    for expr in [
        "xs:dateTime('2000-01-01T00:00:00') eq xs:dateTime('2000-01-01T00:00:00Z')",
        // A date in summer: no daylight-saving offset of any process zone.
        "xs:dateTime('2000-06-01T00:00:00') eq xs:dateTime('2000-06-01T00:00:00Z')",
        "xs:time('12:00:00') eq xs:time('12:00:00Z')",
        "xs:date('2000-01-01') eq xs:date('2000-01-01Z')",
        "xs:gDay('---05') eq xs:gDay('---05Z')",
        "xs:dateTime('2000-01-01T00:00:00') - xs:dateTime('2000-01-01T00:00:00Z') eq xs:dayTimeDuration('PT0S')",
        "string(adjust-dateTime-to-timezone(xs:dateTime('2000-01-01T00:00:00+05:00'))) eq '1999-12-31T19:00:00Z'",
        "timezone-from-dateTime(current-dateTime()) eq implicit-timezone()",
        "not(xs:dateTime('2000-01-01T00:00:00') eq xs:dateTime('2000-01-01T00:00:00+05:00'))",
        "not(xs:dateTime('2000-01-01T00:00:00') eq xs:dateTime('2000-01-01T00:00:00-03:00'))",
    ] {
        assert_eq!(ok(None, expr), "true", "{expr}");
    }
    // A context timezone still wins over the default.
    let other = 300;
    let expr = format!(
        "xs:dateTime('2000-01-01T00:00:00') eq xs:dateTime('2000-01-01T00:00:00{}')",
        lexical(other)
    );
    assert_eq!(ok(Some(other), &expr), "true");
    assert_eq!(ok(Some(other), "implicit-timezone()"), duration(other));
}

#[test]
fn the_fallback_is_utc() {
    assert_the_fallback_is_utc();
}

/// The fallback under several process time zones: this test runs
/// `utc_fallback_child` in a child process with `TZ` set; the fallback is UTC
/// in each of them.
#[test]
#[cfg(unix)]
fn the_fallback_is_utc_under_any_process_time_zone() {
    let exe = std::env::current_exe().expect("test executable");
    for tz in ["UTC", "Etc/GMT-5", "Etc/GMT+3"] {
        let output = std::process::Command::new(&exe)
            .args([
                "--exact",
                "utc_fallback_child",
                "--ignored",
                "--test-threads=1",
            ])
            .env("TZ", tz)
            .env("XPATH_TZ_CHILD", "1")
            .output()
            .expect("child test runs");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("1 passed"),
            "TZ={tz}:\n{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
#[ignore = "run by the_fallback_is_utc_under_any_process_time_zone with TZ set"]
fn utc_fallback_child() {
    if std::env::var_os("XPATH_TZ_CHILD").is_none() {
        return;
    }
    assert_the_fallback_is_utc();
}

// ---------------------------------------------------------------------------
// C5: `eq` on the gregorian types (F&O 1.0 §10.4.15 – §10.4.19)
// ---------------------------------------------------------------------------

/// The examples of §10.4.15 – §10.4.19, which "Assume that the dynamic
/// context provides an implicit timezone value of -05:00".
#[test]
fn gregorian_equality_follows_the_f_and_o_examples() {
    let rows: &[(&str, &str)] = &[
        // §10.4.15.1
        (
            "xs:gYearMonth('1976-02') eq xs:gYearMonth('1976-03Z')",
            "false",
        ),
        (
            "xs:gYearMonth('1976-03') eq xs:gYearMonth('1976-03Z')",
            "false",
        ),
        // §10.4.16.1
        ("xs:gYear('2005-12:00') eq xs:gYear('2005+12:00')", "false"),
        ("xs:gYear('1976-05:00') eq xs:gYear('1976')", "true"),
        // §10.4.17.1
        (
            "xs:gMonthDay('--12-25-14:00') eq xs:gMonthDay('--12-26+10:00')",
            "true",
        ),
        (
            "xs:gMonthDay('--12-25') eq xs:gMonthDay('--12-26Z')",
            "false",
        ),
        // §10.4.18.1
        (
            "xs:gMonth('--12-14:00') eq xs:gMonth('--12+10:00')",
            "false",
        ),
        ("xs:gMonth('--12') eq xs:gMonth('--12Z')", "false"),
        // §10.4.19.1
        ("xs:gDay('---25-14:00') eq xs:gDay('---25+10:00')", "false"),
        ("xs:gDay('---12') eq xs:gDay('---12Z')", "false"),
        // The implicit timezone -05:00 itself.
        (
            "xs:gYearMonth('1976-03') eq xs:gYearMonth('1976-03-05:00')",
            "true",
        ),
        ("xs:gYear('1976') ne xs:gYear('1976-05:00')", "false"),
        (
            "xs:gMonthDay('--12-25') eq xs:gMonthDay('--12-25-05:00')",
            "true",
        ),
        ("xs:gMonth('--12') eq xs:gMonth('--12-05:00')", "true"),
        ("xs:gDay('---12') eq xs:gDay('---12-05:00')", "true"),
        ("xs:gDay('---12') eq xs:gDay('---13+19:00')", "ERR"),
        // One instant in two offsets.
        ("xs:gDay('---25-14:00') eq xs:gDay('---26+10:00')", "true"),
        ("xs:gYear('2005-14:00') eq xs:gYear('2005+10:00')", "false"),
        (
            "xs:gYearMonth('2005-01-14:00') eq xs:gYearMonth('2005-01-14:00')",
            "true",
        ),
        // Only equality is defined (§10.1).
        ("xs:gDay('---12') lt xs:gDay('---13')", "XPTY0004"),
        ("xs:gYear('2000') gt xs:gYear('1999')", "XPTY0004"),
        ("xs:gYear('2000') eq xs:gYearMonth('2000-01')", "XPTY0004"),
    ];
    let mut failures = Vec::new();
    for (expr, expected) in rows {
        let got = eval(Some(-300), expr);
        let pass = match *expected {
            "ERR" => got.is_err(),
            "XPTY0004" => matches!(&got, Err(code) if code == "XPTY0004"),
            value => got.as_deref() == Ok(value),
        };
        if !pass {
            failures.push(format!("{expr}: expected {expected}, got {got:?}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// `xs:gDay('---05') eq xs:gDay('---05Z')` with the implicit timezone `Z`
/// (the M2f note), and the general comparison of the gregorian types through
/// the index (more than 64 pairs) — which must not miss a pair that `eq`
/// finds.
#[test]
fn gregorian_equality_in_general_comparisons() {
    let tz = context_minutes();
    let rows: &[(&str, &str)] = &[
        ("xs:gDay('---05') eq xs:gDay('---05{tz}')", "true"),
        ("xs:gDay('---05') = xs:gDay('---05{tz}')", "true"),
        (
            "(for $i in 1 to 100 return xs:gDay('---01')) = (for $i in 1 to 100 return xs:gDay('---05'), xs:gDay('---05{tz}'))",
            "false",
        ),
        (
            "xs:gDay('---05') = (for $i in 1 to 100 return xs:gDay('---01'), xs:gDay('---05{tz}'))",
            "true",
        ),
        (
            "xs:gDay('---25-14:00') = (for $i in 1 to 100 return xs:gDay('---01'), xs:gDay('---26+10:00'))",
            "true",
        ),
        (
            "xs:gMonthDay('--12-25-14:00') = (for $i in 1 to 100 return xs:gMonthDay('--01-01'), xs:gMonthDay('--12-26+10:00'))",
            "true",
        ),
        (
            "xs:gYear('2000') = (for $i in 1 to 100 return xs:gYear('1999'), xs:gYear('2000{tz}'))",
            "true",
        ),
        (
            "xs:gYearMonth('2000-01') != (for $i in 1 to 100 return xs:gYearMonth('2000-01{tz}'))",
            "false",
        ),
        (
            "count(distinct-values((xs:gMonth('--05'), xs:gMonth('--05{tz}'), xs:gMonth('--05Z'))))",
            "2",
        ),
        ("index-of((xs:gDay('---05Z'), xs:gDay('---05{tz}')), xs:gDay('---05'))", "2"),
        ("deep-equal(xs:gDay('---05'), xs:gDay('---05{tz}'))", "true"),
    ];
    check_rows(tz, rows, false);
    // With the implicit timezone Z.
    assert_eq!(ok(Some(0), "xs:gDay('---05') eq xs:gDay('---05Z')"), "true");
}
