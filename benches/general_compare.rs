//! Benchmark for the general comparison `A = B` (XPath 2.0 §3.5.2).
//!
//! Run the full sweep with
//!
//! ```text
//! cargo bench --bench general_compare --features xsd11
//! ```
//!
//! Without `--bench` (for instance under `cargo test --all-targets`) it does a
//! small smoke pass instead, so that the target is still type-checked and
//! exercised without spending minutes on it.
//!
//! The sweep covers the four shapes that matter:
//!
//! * `a` — two disjoint sequences of `xs:string`: the worst case, no true pair,
//!   so nothing can exit early;
//! * `b` — two disjoint sequences of `xs:untypedAtomic` taken from attribute
//!   nodes of a real document, evaluated through the public XPath API;
//! * `c` — two disjoint sequences of `xs:integer`;
//! * `d` — sequences whose only true pair is the very last one.
//!
//! plus a small-operand micro pass (1×1, 3×3, 10×10) that must not regress.

use std::time::{Duration, Instant};

use xsd_schema::namespace::table::NameTable;
use xsd_schema::types::value::XmlValue;
use xsd_schema::xpath::api::XPathExpr;
use xsd_schema::xpath::iterator::VecNodeIterator;
use xsd_schema::xpath::operators::general_eq_iter;
use xsd_schema::xpath::{RoXmlNavigator, XPathContext, XmlItem};

type Nav = RoXmlNavigator<'static>;

/// Stop growing a shape once a single evaluation costs more than this, so that
/// the quadratic baseline can be measured at the sizes where it still returns.
const GIVE_UP_AFTER: Duration = Duration::from_secs(5);

fn sequence(values: Vec<XmlValue>) -> VecNodeIterator<Nav> {
    VecNodeIterator::new(values.into_iter().map(XmlItem::Atomic).collect())
}

fn strings(prefix: &str, count: usize) -> Vec<XmlValue> {
    (0..count)
        .map(|i| XmlValue::string(format!("{prefix}-{i:07}")))
        .collect()
}

fn integers(offset: i64, count: usize) -> Vec<XmlValue> {
    (0..count)
        .map(|i| XmlValue::integer(num_bigint::BigInt::from(offset + i as i64)))
        .collect()
}

/// Time one closure, repeating it until at least `floor` has elapsed, and
/// report the mean duration of a single call.
fn time<F: FnMut()>(floor: Duration, mut body: F) -> Duration {
    // One warm-up run, which also tells us whether a single call is already
    // expensive enough that repeating it is pointless.
    let start = Instant::now();
    body();
    let first = start.elapsed();
    if first >= floor {
        return first;
    }
    let mut runs = 1u32;
    let start = Instant::now();
    while start.elapsed() < floor {
        body();
        runs += 1;
    }
    start.elapsed() / runs
}

fn row(label: &str, size: usize, elapsed: Duration) {
    println!("{label:<34} n = {size:>7}   {:>12.3?}", elapsed);
}

fn skipped(label: &str, size: usize) {
    println!("{label:<34} n = {size:>7}   {:>12}", "not run");
}

// ---------------------------------------------------------------------------
// Atomic-sequence shapes
// ---------------------------------------------------------------------------

fn bench_atomic_shapes(sizes: &[usize], floor: Duration) {
    let names = NameTable::new();
    let context = XPathContext::new(&names);

    let mut run_strings = true;
    let mut run_integers = true;
    let mut run_tail_hit = true;

    for &size in sizes {
        if run_strings {
            let left = sequence(strings("left", size));
            let right = sequence(strings("right", size));
            let elapsed = time(floor, || {
                let hit = general_eq_iter(&context, &left, &right).unwrap();
                assert!(!hit);
            });
            row("(a) disjoint xs:string", size, elapsed);
            run_strings = elapsed < GIVE_UP_AFTER;
        } else {
            skipped("(a) disjoint xs:string", size);
        }

        if run_integers {
            let left = sequence(integers(0, size));
            let right = sequence(integers(1_000_000_000, size));
            let elapsed = time(floor, || {
                let hit = general_eq_iter(&context, &left, &right).unwrap();
                assert!(!hit);
            });
            row("(c) disjoint xs:integer", size, elapsed);
            run_integers = elapsed < GIVE_UP_AFTER;
        } else {
            skipped("(c) disjoint xs:integer", size);
        }

        if run_tail_hit {
            let mut left_values = strings("left", size);
            let mut right_values = strings("right", size);
            // The only true pair is (last of A, last of B).
            if let (Some(l), Some(r)) = (left_values.last_mut(), right_values.last_mut()) {
                *l = XmlValue::string("needle");
                *r = XmlValue::string("needle");
            }
            let left = sequence(left_values);
            let right = sequence(right_values);
            let elapsed = time(floor, || {
                let hit = general_eq_iter(&context, &left, &right).unwrap();
                assert!(hit);
            });
            row("(d) single hit at the very end", size, elapsed);
            run_tail_hit = elapsed < GIVE_UP_AFTER;
        } else {
            skipped("(d) single hit at the very end", size);
        }
    }
}

// ---------------------------------------------------------------------------
// Attribute nodes through the public API
// ---------------------------------------------------------------------------

fn document_text(size: usize) -> String {
    let mut xml = String::with_capacity(size * 40);
    xml.push_str("<d><p>");
    for i in 0..size {
        xml.push_str(&format!("<a v=\"left-{i:07}\"/>"));
    }
    xml.push_str("</p><q>");
    for i in 0..size {
        xml.push_str(&format!("<b v=\"right-{i:07}\"/>"));
    }
    xml.push_str("</q></d>");
    xml
}

fn bench_untyped_attributes(sizes: &[usize], floor: Duration) {
    let names = NameTable::new();
    let context = XPathContext::new(&names);
    let expr = XPathExpr::compile("/d/p/a/@v = /d/q/b/@v", &context).unwrap();

    let mut keep_going = true;
    for &size in sizes {
        if !keep_going {
            skipped("(b) untypedAtomic attributes", size);
            continue;
        }
        let text = document_text(size);
        let doc = roxmltree::Document::parse(&text).unwrap();
        let elapsed = time(floor, || {
            let value = expr
                .evaluator(&context)
                .run_with_node(RoXmlNavigator::new(&doc))
                .unwrap();
            let items = value.into_vec();
            assert_eq!(items.len(), 1);
        });
        row("(b) untypedAtomic attributes", size, elapsed);
        keep_going = elapsed < GIVE_UP_AFTER;
    }
}

// ---------------------------------------------------------------------------
// Small operands
// ---------------------------------------------------------------------------

fn bench_small_operands(floor: Duration) {
    let names = NameTable::new();
    let context = XPathContext::new(&names);

    for size in [1usize, 2, 3, 4, 5, 6, 8, 10, 16] {
        let left = sequence(strings("left", size));
        let right = sequence(strings("right", size));
        let elapsed = time(floor, || {
            let hit = general_eq_iter(&context, &left, &right).unwrap();
            assert!(!hit);
        });
        row("(small) disjoint xs:string", size, elapsed);

        let left = sequence(integers(0, size));
        let right = sequence(integers(1_000_000_000, size));
        let elapsed = time(floor, || {
            let hit = general_eq_iter(&context, &left, &right).unwrap();
            assert!(!hit);
        });
        row("(small) disjoint xs:integer", size, elapsed);
    }
}

fn main() {
    let full = std::env::args().any(|arg| arg == "--bench");
    let (sizes, floor): (&[usize], Duration) = if full {
        (&[100, 1_000, 10_000, 100_000], Duration::from_millis(400))
    } else {
        (&[100], Duration::from_millis(5))
    };

    println!("== general comparison `A = B` ==");
    bench_atomic_shapes(sizes, floor);
    bench_untyped_attributes(sizes, floor);
    bench_small_operands(floor);
}
