//! T1, the plan differential (`--plan-differential`, feature `codegen`).
//!
//! For each instance test whose schema compiles, the plan is built in process
//! from the test's schema set and run on the instance twice: as shipped, and
//! with the delegating value tier. A `Valid` is checked against the dynamic
//! verdict the plan's `Valid` is defined by (`drive_quick_xml_bytes` with
//! default flags: `Ok`, no error, root valid). A `Valid` the dynamic verdict
//! refuses is printed as `PLAN-UNSOUND` and makes the run exit with code 3.
//! Coverage is the share of dynamically valid instances answered `Valid`.
//!
//! The lines printed here start with `PLAN`, so the `FAIL` / `ERROR` lines the
//! gate compares are the same with the mode on and off.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use xsd_schema::plan::{self, Verdict};
use xsd_schema::validation::{SchemaValidator, ValidationFlags};
use xsd_schema::SchemaSet;

/// Whether the mode is on.
pub static ENABLED: AtomicBool = AtomicBool::new(false);

#[derive(Default)]
struct Stats {
    instances: usize,
    plan_panics: usize,
    build_ms: f64,
    dynamic_valid: usize,
    shipped_valid: usize,
    delegating_valid: usize,
    unsound_shipped: usize,
    unsound_delegating: usize,
    driver_disagrees: usize,
    reasons_shipped: BTreeMap<String, usize>,
    reasons_delegating: BTreeMap<String, usize>,
    /// Dynamically valid, not `Valid` delegating, by a rough cause.
    lost_by: BTreeMap<String, usize>,
    /// Dynamically valid, not `Valid` as shipped, by a rough cause.
    lost_by_shipped: BTreeMap<String, usize>,
}

static STATS: Mutex<Option<Stats>> = Mutex::new(None);

fn verdict_label(v: Verdict) -> String {
    match v {
        Verdict::Valid => "Valid".into(),
        Verdict::Unknown(r) => format!("{r:?}"),
    }
}

/// Run the plan on one instance test. `driver_valid` is the conformance
/// driver's own verdict (with its hints, entities and second pass), when it
/// produced one.
pub fn observe(set: &SchemaSet, instance: &Path, name: &str, driver_valid: Option<bool>) {
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    let Ok(raw) = std::fs::read(instance) else {
        return;
    };
    let Ok(content) = xsd_schema::decode_xml_to_utf8_bytes(raw) else {
        return;
    };
    let t = Instant::now();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let (data, report) = plan::build(set);
        if std::env::var_os("PLAN_DEBUG").is_some() {
            eprintln!(
                "PLAN-DEBUG {name}: roots {} types {} names {} uncovered {:?} refused {:?}",
                data.roots.len(),
                data.types.len(),
                data.names.len(),
                report.uncovered,
                report.patterns_refused
            );
        }
        let build_ms = t.elapsed().as_secs_f64() * 1000.0;
        let validator = SchemaValidator::new(set, ValidationFlags::default());
        let dynamic = plan::dynamic_valid(&validator, &content);
        let mut scratch = plan::Scratch::new();
        let shipped = plan::check(data.as_ref(), &content, &mut scratch);
        let mut tier = plan::Delegating::new(set, &data);
        let delegating = plan::check_with(data.as_ref(), &content, &mut scratch, &mut tier);
        (build_ms, dynamic, shipped, delegating)
    }));
    let mut guard = STATS.lock().unwrap_or_else(|e| e.into_inner());
    let stats = guard.get_or_insert_with(Stats::default);
    stats.instances += 1;
    let Ok((build_ms, dynamic, shipped, delegating)) = outcome else {
        stats.plan_panics += 1;
        println!("  PLAN-PANIC - {name}");
        return;
    };
    stats.build_ms += build_ms;
    if dynamic {
        stats.dynamic_valid += 1;
        *stats
            .reasons_shipped
            .entry(verdict_label(shipped))
            .or_default() += 1;
        *stats
            .reasons_delegating
            .entry(verdict_label(delegating))
            .or_default() += 1;
        if delegating != Verdict::Valid {
            *stats
                .lost_by
                .entry(rough_cause(&content, delegating))
                .or_default() += 1;
        }
        if shipped != Verdict::Valid {
            *stats
                .lost_by_shipped
                .entry(rough_cause(&content, shipped))
                .or_default() += 1;
        }
    }
    for (run, v, counter) in [
        ("shipped", shipped, &mut stats.unsound_shipped),
        ("delegating", delegating, &mut stats.unsound_delegating),
    ] {
        if v == Verdict::Valid && !dynamic {
            *counter += 1;
            println!("  PLAN-UNSOUND [{run}] - {name}");
        }
    }
    if shipped == Verdict::Valid {
        stats.shipped_valid += 1;
    }
    if delegating == Verdict::Valid {
        stats.delegating_valid += 1;
    }
    if (shipped == Verdict::Valid || delegating == Verdict::Valid) && driver_valid == Some(false) {
        stats.driver_disagrees += 1;
        println!("  PLAN-DRIVER - {name}: Valid, but the conformance run did not accept it");
    }
}

/// A rough cause of a lost instance, for the coverage report.
fn rough_cause(content: &[u8], v: Verdict) -> String {
    let has = |needle: &[u8]| content.windows(needle.len()).any(|w| w == needle);
    let cause = if has(b"XMLSchema-instance") {
        "xsi attributes"
    } else if has(b"<!DOCTYPE") {
        "doctype"
    } else if has(b"xml:") {
        "xml: attributes"
    } else {
        "other"
    };
    format!("{} / {cause}", verdict_label(v))
}

/// Print the summary; `true` when a false `Valid` was found.
pub fn print_summary() -> bool {
    if !ENABLED.load(Ordering::Relaxed) {
        return false;
    }
    let guard = STATS.lock().unwrap_or_else(|e| e.into_inner());
    let Some(s) = guard.as_ref() else {
        println!("PLAN differential: no instance test ran");
        return false;
    };
    let pct = |n: usize| {
        if s.dynamic_valid == 0 {
            0.0
        } else {
            100.0 * n as f64 / s.dynamic_valid as f64
        }
    };
    println!();
    println!("PLAN differential (T1)");
    println!(
        "PLAN instances {}, dynamically valid {}, plan panics {}, plan build {:.0} ms in all",
        s.instances, s.dynamic_valid, s.plan_panics, s.build_ms
    );
    println!(
        "PLAN as shipped: Valid {} ({:.1} % of the dynamically valid), false Valid {}",
        s.shipped_valid,
        pct(s.shipped_valid),
        s.unsound_shipped
    );
    println!(
        "PLAN delegating: Valid {} ({:.1} % of the dynamically valid), false Valid {}",
        s.delegating_valid,
        pct(s.delegating_valid),
        s.unsound_delegating
    );
    println!(
        "PLAN valid instances by answer, as shipped: {:?}",
        s.reasons_shipped
    );
    println!(
        "PLAN valid instances by answer, delegating: {:?}",
        s.reasons_delegating
    );
    println!(
        "PLAN Valid where the conformance run did not accept: {}",
        s.driver_disagrees
    );
    println!(
        "PLAN valid instances not Valid delegating, by rough cause: {:?}",
        s.lost_by
    );
    println!(
        "PLAN valid instances not Valid as shipped, by rough cause: {:?}",
        s.lost_by_shipped
    );
    s.unsound_shipped + s.unsound_delegating + s.plan_panics > 0
}
