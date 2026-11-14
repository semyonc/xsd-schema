//! T2 and T8 (pattern level) for the validation plan (feature `codegen`).
//!
//! The corpora live outside the repository, so this binary does nothing
//! without arguments:
//!
//! ```text
//! cargo test --release --features xsd11,codegen --test plan_differential -- \
//!     --mutations --workspace /path/to/XmlPad-Windows
//! cargo test --release --features xsd11,codegen --test plan_differential -- \
//!     --patterns --workspace /path/to/XmlPad-Windows
//! ```
//!
//! **T2 (`--mutations`).** NF-e 4.00, FatturaPA 1.2.3 and UBL 2.1 (Invoice,
//! Order, CreditNote): every valid document listed below must be
//! answered `Valid`, as shipped and by the delegating tier, and no mutant of
//! the mutated ones may be answered `Valid`, as shipped or delegating, unless
//! the dynamic validator accepts it.
//!
//! **T8, pattern level (`--patterns`, needs `xsd11`).** Every pattern facet
//! of the W3C schemas, GAEB, the WSDL corpus and UBL, as a one-pattern step
//! under both XSD versions: wherever the plan's table accepts a value inside
//! the step's trusted alphabet, regexml must accept it too; and the table
//! must never accept what the `regex` engine refuses on the same
//! translation (the tripwire). Values: the enumerations and other literals
//! of the same schema, generated strings the table accepts (walks of the
//! DFA, with their characters moved to neighbouring code points, dropped and
//! repeated) and fixed boundary strings. Then a fixed list of hand-written
//! patterns at the corners of the two engines' syntax, alone over every
//! string up to length 2 of a 50-character alphabet and random longer ones,
//! and in random two-pattern steps and two-step chains (a value must then be
//! accepted by every step).
//!
//! **T3 (`--fuzz [--count N]`).** `N` seeded byte-level mutants of each
//! mutated document above (bytes replaced, inserted, deleted, duplicated and
//! swapped, half of them inside character data): no `Valid` the dynamic
//! validator refuses.
//!
//! **Value checks (`--values [--count N]`, no corpus).** Random simple
//! types, `N` schemas per XSD version: one or two restriction steps over a
//! built-in datatype of every family the plan checks, with random facets
//! whose bounds and enumerations are values on the boundaries of the family's
//! lexical space; the type as element content, attribute value, element and
//! attribute `fixed` and `default`, simple content (an extension and a
//! restriction of it with facets of its own) and repeated (`xs:ID`
//! duplicates). Every value of the family's list in every position: no
//! `Valid` may contradict the runtime.
//!
//! **Derived types and attribute uses (`--derived [--count N]`, no corpus).**
//! Random schemas, `N` per XSD version: a base complex type and up to two
//! levels of extension and restriction, with their own attribute uses (every
//! use kind and value constraint, local and global declarations) and two
//! attribute groups; 864 documents each over three attributes, a group
//! attribute and the content. No `Valid` may contradict the runtime.
//!
//! Exit code 1 on any failure.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use xsd_schema::plan::{self, PlanData, Verdict};
use xsd_schema::validation::{SchemaValidator, ValidationFlags};
use xsd_schema::{SchemaSet, SchemaSetBuilder};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut failed = false;
    if args.iter().any(|a| a == "--derived") {
        let count = args
            .iter()
            .position(|a| a == "--count")
            .and_then(|i| args.get(i + 1))
            .and_then(|n| n.parse().ok())
            .unwrap_or(3000);
        failed |= derived::run(count);
    }
    if args.iter().any(|a| a == "--values") {
        let count = args
            .iter()
            .position(|a| a == "--count")
            .and_then(|i| args.get(i + 1))
            .and_then(|n| n.parse().ok())
            .unwrap_or(3000);
        failed |= values::run(count);
    }
    let workspace = args
        .iter()
        .position(|a| a == "--workspace")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from);
    let Some(ws) = workspace else {
        if failed {
            std::process::exit(1);
        }
        println!("plan_differential: no --workspace given; no corpus to run");
        return;
    };
    let only = args
        .iter()
        .position(|a| a == "--only")
        .and_then(|i| args.get(i + 1))
        .cloned();
    if args.iter().any(|a| a == "--mutations") {
        failed |= mutations(&ws, only.as_deref());
    }
    if args.iter().any(|a| a == "--fuzz") {
        let count = args
            .iter()
            .position(|a| a == "--count")
            .and_then(|i| args.get(i + 1))
            .and_then(|n| n.parse().ok())
            .unwrap_or(20_000);
        failed |= fuzz::run(&ws, count);
    }
    if args.iter().any(|a| a == "--patterns") {
        #[cfg(feature = "xsd11")]
        {
            failed |= patterns::run(&ws);
        }
        #[cfg(not(feature = "xsd11"))]
        {
            eprintln!("--patterns needs the xsd11 feature (regexml is the judge)");
            failed = true;
        }
    }
    if failed {
        std::process::exit(1);
    }
}

// ---------------------------------------------------------------------------
// T2
// ---------------------------------------------------------------------------

struct Case {
    name: &'static str,
    xsd: &'static str,
    docs: &'static [&'static str],
    mutate: &'static [&'static str],
}

const CASES: &[Case] = &[
    Case {
        name: "NF-e 4.00",
        xsd: "wsdl-corpus/tier2/br-nfe/schemas/v4_0/nfe_v4.00.xsd",
        docs: &[
            "validation-bench/codegen-probe/instances/nfe_10.xml",
            "validation-bench/codegen-probe/instances/nfe_990.xml",
            "validation-bench/codegen-probe/instances/nfe_10_prefixed.xml",
            "validation-bench/codegen-probe/instances/nfe_990_prefixed.xml",
        ],
        mutate: &[
            "validation-bench/codegen-probe/instances/nfe_10.xml",
            "validation-bench/codegen-probe/instances/nfe_10_prefixed.xml",
        ],
    },
    Case {
        name: "FatturaPA 1.2.3",
        xsd: "validation-bench/codegen-probe/instances/fatturapa/Schema_VFPR12_local.xsd",
        docs: &[
            "validation-bench/codegen-probe/instances/fattura_10.xml",
            "validation-bench/codegen-probe/instances/fattura_1000.xml",
        ],
        mutate: &["validation-bench/codegen-probe/instances/fattura_10.xml"],
    },
    Case {
        name: "UBL 2.1 Invoice",
        xsd: "ubl/os-UBL-2.1/xsd/maindoc/UBL-Invoice-2.1.xsd",
        docs: &[
            "ubl/os-UBL-2.1/xml/UBL-Invoice-2.1-Example-Trivial.xml",
            "ubl/os-UBL-2.1/xml/UBL-Invoice-2.1-Example.xml",
            "validation-bench/codegen-probe/instances/ubl/invoice_1000.xml",
        ],
        mutate: &["ubl/os-UBL-2.1/xml/UBL-Invoice-2.1-Example.xml"],
    },
    Case {
        name: "UBL 2.1 Order",
        xsd: "ubl/os-UBL-2.1/xsd/maindoc/UBL-Order-2.1.xsd",
        docs: &["ubl/os-UBL-2.1/xml/UBL-Order-2.1-Example.xml"],
        mutate: &["ubl/os-UBL-2.1/xml/UBL-Order-2.1-Example.xml"],
    },
    Case {
        name: "UBL 2.1 CreditNote",
        xsd: "ubl/os-UBL-2.1/xsd/maindoc/UBL-CreditNote-2.1.xsd",
        docs: &["ubl/os-UBL-2.1/xml/UBL-CreditNote-2.1-Example.xml"],
        mutate: &[],
    },
];

fn load(path: &Path) -> Result<SchemaSet, String> {
    let mut b = SchemaSetBuilder::new();
    b.try_add(&path.to_string_lossy())
        .map_err(|e| e.to_string())?;
    b.compile()
        .map(|c| c.into_schema_set())
        .map_err(|e| e.to_string())
}

fn mutations(ws: &Path, only: Option<&str>) -> bool {
    let mut failed = false;
    let mut total_mutants = 0usize;
    for case in CASES {
        if only.is_some_and(|o| !case.name.contains(o)) {
            continue;
        }
        println!("=== {}", case.name);
        let t = Instant::now();
        let set = match load(&ws.join(case.xsd)) {
            Ok(s) => s,
            Err(e) => {
                println!("  SCHEMA REFUSED: {e}");
                failed = true;
                continue;
            }
        };
        let compile_ms = t.elapsed().as_secs_f64() * 1e3;
        let t = Instant::now();
        let (data, report) = plan::build(&set);
        let build_ms = t.elapsed().as_secs_f64() * 1e3;
        let mut uncovered = report.uncovered.clone();
        uncovered.sort();
        uncovered.dedup();
        println!(
            "  schema {compile_ms:.0} ms, plan {build_ms:.0} ms: {} names, {} types, {} children, {} states, {} transitions, {} checks, {} pattern tables, {} refused, {} uncovered",
            data.names.len(),
            data.types.len(),
            data.children.len(),
            data.states.len(),
            data.transitions.len(),
            data.checks.len(),
            data.patterns.len(),
            report.patterns_refused.len(),
            uncovered.len()
        );
        for u in uncovered.iter().take(8) {
            println!("    uncovered: {u}");
        }
        for r in report.patterns_refused.iter().take(5) {
            println!("    pattern refused: {r}");
        }
        let validator = SchemaValidator::new(&set, ValidationFlags::default());
        for doc in case.docs {
            let bytes = std::fs::read(ws.join(doc)).expect("document");
            let dynamic = plan::dynamic_valid(&validator, &bytes);
            let (shipped, delegating) = verdicts(&set, &data, &bytes);
            println!(
                "  {doc}: dynamic {}, shipped {shipped:?}, delegating {delegating:?}",
                if dynamic { "valid" } else { "NOT VALID" }
            );
            if dynamic && delegating != Verdict::Valid {
                println!("    FAIL: a valid document of the corpus is not answered Valid by the delegating tier");
                failed = true;
            }
            if dynamic && shipped != Verdict::Valid {
                println!(
                    "    FAIL: a valid document of the corpus is not answered Valid as shipped"
                );
                failed = true;
            }
            // Coverage by check kind: the value checks the document runs.
            let mut counting = Counting(HashMap::new());
            let mut scratch = plan::Scratch::new();
            plan::check_with(data.as_ref(), &bytes, &mut scratch, &mut counting);
            let mut by: HashMap<String, usize> = HashMap::new();
            for (id, n) in counting.0 {
                *by.entry(check_label(&data, id)).or_default() += n;
            }
            let mut by: Vec<_> = by.into_iter().collect();
            by.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            let total: usize = by.iter().map(|x| x.1).sum();
            let rows: Vec<String> = by.iter().map(|(k, n)| format!("{k} {n}")).collect();
            println!("    values checked {total}: {}", rows.join(", "));
            if !dynamic {
                failed = true;
            }
        }
        for doc in case.mutate {
            let text = std::fs::read_to_string(ws.join(doc)).expect("document");
            let r = differential(&set, &data, &validator, &text);
            total_mutants += r.total;
            println!(
                "  {doc}: {} mutants | dynamic valid {} | shipped Valid {} | delegating Valid {} | UNSOUND shipped {} delegating {}",
                r.total, r.dynamic_valid, r.shipped_valid, r.delegating_valid, r.unsound_shipped, r.unsound_delegating
            );
            let mut lost: Vec<_> = r.lost.iter().collect();
            lost.sort_by(|a, b| b.1.cmp(a.1));
            let shown: Vec<String> = lost
                .iter()
                .take(12)
                .map(|(k, n)| format!("{k} {n}"))
                .collect();
            println!(
                "    dynamic valid, delegating not Valid, by mutation: {}",
                shown.join(", ")
            );
            for e in &r.examples {
                println!("    UNSOUND {e}");
            }
            failed |= r.unsound_shipped + r.unsound_delegating > 0;
        }
    }
    println!("T2: {total_mutants} mutants in all");
    failed
}

/// The shipped tier, counting the value checks it runs.
struct Counting(HashMap<u32, usize>);

impl plan::ValueTier for Counting {
    fn delegating(&self) -> bool {
        false
    }
    fn value(&mut self, _: u32, _: &str) -> bool {
        false
    }
    fn note(&mut self, check: u32) {
        *self.0.entry(check).or_default() += 1;
    }
}

/// A check's kind and lexical space, as a table row label.
fn check_label(data: &PlanData, id: u32) -> String {
    use plan::model::CheckKind;
    match data.checks.get(id as usize) {
        Some(c) if c.kind == CheckKind::Value => {
            let chained = if c.next != plan::model::NONE {
                " (simple content layers)"
            } else {
                ""
            };
            format!("{:?}{chained}", c.lex)
        }
        Some(c) => format!("{:?}", c.kind),
        None => "?".into(),
    }
}

fn verdicts(set: &SchemaSet, data: &PlanData, bytes: &[u8]) -> (Verdict, Verdict) {
    let mut scratch = plan::Scratch::new();
    let shipped = plan::check(data.as_ref(), bytes, &mut scratch);
    let mut tier = plan::Delegating::new(set, data);
    let delegating = plan::check_with(data.as_ref(), bytes, &mut scratch, &mut tier);
    (shipped, delegating)
}

#[derive(Default)]
struct DiffResult {
    total: usize,
    dynamic_valid: usize,
    shipped_valid: usize,
    delegating_valid: usize,
    unsound_shipped: usize,
    unsound_delegating: usize,
    lost: HashMap<String, usize>,
    examples: Vec<String>,
}

fn differential(
    set: &SchemaSet,
    data: &PlanData,
    v: &SchemaValidator<'_>,
    doc: &str,
) -> DiffResult {
    let mut r = DiffResult::default();
    let mut scratch = plan::Scratch::new();
    mutate(doc, |kind, m| {
        r.total += 1;
        let bytes = m.as_bytes();
        let shipped = plan::check(data.as_ref(), bytes, &mut scratch);
        let mut tier = plan::Delegating::new(set, data);
        let delegating = plan::check_with(data.as_ref(), bytes, &mut scratch, &mut tier);
        let dynamic = plan::dynamic_valid(v, bytes);
        if dynamic {
            r.dynamic_valid += 1;
            if delegating != Verdict::Valid {
                *r.lost.entry(kind.to_string()).or_default() += 1;
            }
        }
        if shipped == Verdict::Valid {
            r.shipped_valid += 1;
        }
        if delegating == Verdict::Valid {
            r.delegating_valid += 1;
        }
        for (run, verdict, n) in [
            ("shipped", shipped, &mut r.unsound_shipped),
            ("delegating", delegating, &mut r.unsound_delegating),
        ] {
            if verdict == Verdict::Valid && !dynamic {
                *n += 1;
                if r.examples.len() < 12 {
                    let at = m
                        .bytes()
                        .zip(doc.bytes())
                        .position(|(a, b)| a != b)
                        .unwrap_or(0);
                    let lo = at.saturating_sub(40);
                    let hi = (at + 60).min(m.len());
                    let snippet = String::from_utf8_lossy(&m.as_bytes()[lo..hi]).into_owned();
                    r.examples.push(format!("[{run}] {kind}: …{snippet}…"));
                }
            }
        }
    });
    r
}

// A scanner for the documents above (elements, attributes, text runs) and the
// mutations made from them: structure, names, namespaces, attributes, values,
// references, the prolog, `xsi:` attributes, and characters just outside the
// trusted alphabets.

struct El {
    name: String,
    start: usize,
    st_end: usize,
    end_start: usize,
    end: usize,
    parent: Option<usize>,
    attrs: Vec<(usize, usize, usize, usize)>,
    text: Option<(usize, usize)>,
}

fn is_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r')
}

fn scan_doc(d: &str) -> Vec<El> {
    let b = d.as_bytes();
    let mut els: Vec<El> = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    let mut i = 0;
    while let Some(off) = b[i..].iter().position(|&c| c == b'<') {
        let lt = i + off;
        if off > 0 && !b[i..lt].iter().all(|&x| is_ws(x)) {
            if let Some(&top) = stack.last() {
                els[top].text = Some((i, lt));
            }
        }
        if b[lt + 1] == b'?' || b[lt + 1] == b'!' {
            let close: &[u8] = if b[lt + 1] == b'?' {
                b"?>"
            } else if b[lt + 2] == b'-' {
                b"-->"
            } else {
                b">"
            };
            let at = b[lt..]
                .windows(close.len())
                .position(|w| w == close)
                .unwrap();
            i = lt + at + close.len();
            continue;
        }
        if b[lt + 1] == b'/' {
            let gt = lt + b[lt..].iter().position(|&c| c == b'>').unwrap();
            let top = stack.pop().unwrap();
            els[top].end_start = lt;
            els[top].end = gt + 1;
            i = gt + 1;
            continue;
        }
        let mut j = lt + 1;
        while !matches!(b[j], b' ' | b'\t' | b'\n' | b'\r' | b'>' | b'/') {
            j += 1;
        }
        let name = d[lt + 1..j].to_string();
        let mut attrs = Vec::new();
        loop {
            while is_ws(b[j]) {
                j += 1;
            }
            if b[j] == b'>' || b[j] == b'/' {
                break;
            }
            let ks = j;
            while b[j] != b'=' {
                j += 1;
            }
            let ke = j;
            j += 1;
            while is_ws(b[j]) {
                j += 1;
            }
            let q = b[j];
            let vs = j + 1;
            let ve = vs + b[vs..].iter().position(|&c| c == q).unwrap();
            attrs.push((ks, ke, vs, ve));
            j = ve + 1;
        }
        let self_close = b[j] == b'/';
        let st_end = if self_close { j + 2 } else { j + 1 };
        let idx = els.len();
        els.push(El {
            name,
            start: lt,
            st_end,
            end_start: st_end,
            end: st_end,
            parent: stack.last().copied(),
            attrs,
            text: None,
        });
        if !self_close {
            stack.push(idx);
        }
        i = st_end;
    }
    els
}

fn splice(d: &str, a: usize, b: usize, with: &str) -> String {
    let mut s = String::with_capacity(d.len() + with.len());
    s.push_str(&d[..a]);
    s.push_str(with);
    s.push_str(&d[b..]);
    s
}

/// After the XML declaration, if any: where a document type declaration or a
/// comment before the root goes.
fn prolog_end(d: &str) -> usize {
    if d.starts_with("<?xml") {
        d.find("?>").map_or(0, |i| i + 2)
    } else {
        0
    }
}

const XSI: &str = "http://www.w3.org/2001/XMLSchema-instance";

fn mutate(d: &str, mut f: impl FnMut(&str, &str)) {
    let els = scan_doc(d);
    let bad_values = [
        "",
        " ",
        "x",
        "0",
        "1",
        "-1",
        "1.5",
        "1.50",
        "00",
        "99999999999999999999999",
        "2026-02-30",
        "2026-11-06",
        "2026-11-06T10:00:00-03:00",
        "true",
        "é",
        "a b",
        " a",
        "a  b",
        "a\tb",
        "a\nb",
        "&amp;",
        "&#65;",
        "&#x1F600;",
        "<![CDATA[x]]>",
        "]]>",
        "&undefined;",
        "\u{7}",
        // Added: references to white space, line ends, characters just
        // outside the trusted alphabets.
        "a&#x9;b",
        "a&#xA;b",
        "a&#xD;b",
        "a\r\nb",
        "&#xD;",
        "a\u{A0}b",
        "a&#xA0;b",
        "a\u{2003}b",
        "a\u{85}b",
        "x_",
        "x$",
        "ä",
        "\u{1D400}",
    ];
    let long = "A".repeat(3000);
    let root_end = els.first().map(|e| e.start + 1 + e.name.len()).unwrap_or(0);
    // Prolog and whole-document operators.
    let p = prolog_end(d);
    let root_name = els.first().map(|e| e.name.clone()).unwrap_or_default();
    f(
        "doctype",
        &splice(d, p, p, &format!("<!DOCTYPE {root_name}>")),
    );
    f(
        "doctype internal subset entity",
        &splice(
            d,
            p,
            p,
            &format!("<!DOCTYPE {root_name} [<!ENTITY e \"x\">]>"),
        ),
    );
    if let Some(e) = els.get(1) {
        f(
            "doctype ATTLIST default",
            &splice(
                d,
                p,
                p,
                &format!(
                    "<!DOCTYPE {root_name} [<!ATTLIST {} zz CDATA \"v\">]>",
                    e.name
                ),
            ),
        );
    }
    let body = &d[p..];
    f(
        "xml declaration latin-1",
        &format!("<?xml version=\"1.0\" encoding=\"ISO-8859-1\"?>{body}"),
    );
    f(
        "xml declaration utf-16",
        &format!("<?xml version=\"1.0\" encoding=\"UTF-16\"?>{body}"),
    );
    f(
        "xml declaration 1.1",
        &format!("<?xml version=\"1.1\"?>{body}"),
    );
    f(
        "xml declaration utf-8",
        &format!("<?xml version=\"1.0\" encoding=\"utf-8\"?>{body}"),
    );
    f("byte order mark", &format!("\u{FEFF}{d}"));
    f("comment before root", &splice(d, p, p, "<!--c-->"));
    for loc in [
        "urn:x a.xsd",
        "urn:x a.xsd urn:y b.xsd",
        "odd",
        "",
        "urn:x a.xsd extra",
        "urn:x %zz",
    ] {
        f(
            "xsi:schemaLocation on the root",
            &splice(
                d,
                root_end,
                root_end,
                &format!(" xmlns:xsi=\"{XSI}\" xsi:schemaLocation=\"{loc}\""),
            ),
        );
    }
    f(
        "xsi:noNamespaceSchemaLocation on the root",
        &splice(
            d,
            root_end,
            root_end,
            &format!(" xmlns:xsi=\"{XSI}\" xsi:noNamespaceSchemaLocation=\"a.xsd\""),
        ),
    );
    for (i, e) in els.iter().enumerate() {
        f("element delete", &splice(d, e.start, e.end, ""));
        let whole = &d[e.start..e.end];
        f("element duplicate", &splice(d, e.end, e.end, whole));
        if let Some(next) = els[i + 1..].iter().find(|n| n.parent == e.parent) {
            let sib = &d[next.start..next.end];
            f(
                "element swap",
                &format!(
                    "{}{}{}{}{}",
                    &d[..e.start],
                    sib,
                    &d[e.end..next.start],
                    whole,
                    &d[next.end..]
                ),
            );
        }
        let rename = |to: &str| {
            let end_part = if e.end_start != e.end {
                format!("</{to}>")
            } else {
                String::new()
            };
            let close = if e.end_start != e.end {
                d[e.st_end..e.end_start].to_string()
            } else {
                String::new()
            };
            format!(
                "{}<{to}{}{}{}{}",
                &d[..e.start],
                &d[e.start + 1 + e.name.len()..e.st_end],
                close,
                end_part,
                &d[e.end..]
            )
        };
        f("element rename", &rename(&format!("{}X", e.name)));
        if e.end_start != e.end {
            f(
                "element end tag mismatch",
                &splice(d, e.end_start, e.end, &format!("</{}X>", e.name)),
            );
        }
        let name_end = e.start + 1 + e.name.len();
        f(
            "attribute add unknown",
            &splice(d, name_end, name_end, " foo=\"1\""),
        );
        f(
            "attribute add xsi:nil",
            &splice(
                d,
                name_end,
                name_end,
                &format!(" xmlns:xsi=\"{XSI}\" xsi:nil=\"true\""),
            ),
        );
        f(
            "attribute add xsi:type",
            &splice(
                d,
                name_end,
                name_end,
                &format!(" xmlns:xsi=\"{XSI}\" xsi:type=\"nope\""),
            ),
        );
        f(
            "attribute add xml:lang",
            &splice(d, name_end, name_end, " xml:lang=\"en\""),
        );
        f(
            "namespace default changed",
            &splice(d, name_end, name_end, " xmlns=\"urn:other\""),
        );
        f(
            "namespace default removed",
            &splice(d, name_end, name_end, " xmlns=\"\""),
        );
        if let Some((prefix, _)) = e.name.split_once(':') {
            f(
                "prefix redeclared",
                &splice(
                    d,
                    name_end,
                    name_end,
                    &format!(" xmlns:{prefix}=\"urn:other\""),
                ),
            );
        }
        if i > 0 {
            f(
                "schemaLocation on an inner element",
                &splice(
                    d,
                    name_end,
                    name_end,
                    &format!(" xmlns:xsi=\"{XSI}\" xsi:schemaLocation=\"urn:x a.xsd\""),
                ),
            );
        }
        if e.end_start != e.end {
            f("insert text", &splice(d, e.st_end, e.st_end, "x"));
            f("insert whitespace", &splice(d, e.st_end, e.st_end, " "));
            f(
                "insert reference to white space",
                &splice(d, e.st_end, e.st_end, "&#x20;"),
            );
            f("insert comment", &splice(d, e.st_end, e.st_end, "<!--c-->"));
            f("insert PI", &splice(d, e.st_end, e.st_end, "<?pi x?>"));
            f("insert child", &splice(d, e.st_end, e.st_end, "<zzz/>"));
            f(
                "insert same-name child",
                &splice(d, e.st_end, e.st_end, &format!("<{0}></{0}>", e.name)),
            );
        }
        for &(ks, ke, vs, ve) in &e.attrs {
            let key = &d[ks..ke];
            if key.starts_with("xmlns") {
                f("namespace uri changed", &splice(d, vs, ve, "urn:other"));
                if key == "xmlns" {
                    f("namespace undeclared", &splice(d, vs, ve, ""));
                }
                continue;
            }
            f(
                "attribute delete",
                &splice(d, ks.saturating_sub(1), ve + 1, ""),
            );
            f(
                "attribute dup",
                &splice(d, ve + 1, ve + 1, &format!(" {}=\"{}\"", key, &d[vs..ve])),
            );
            let v = &d[vs..ve];
            for nv in [
                "", "x", "1", "4.00", "  4.00", "é", "a&amp;b", "a b", "a&#x9;b", "a&#xA;b",
                "a&#xD;b", "a\tb", "a\nb", " a ", "a\u{A0}b", "x_", "ä",
            ] {
                f("attribute value", &splice(d, vs, ve, nv));
            }
            f(
                "attribute value suffix",
                &splice(d, vs, ve, &format!("{v}x")),
            );
            f("attribute value tab", &splice(d, vs, ve, &format!("{v}\t")));
            // The same value on another element's attribute of the same
            // name: duplicate IDs and keys.
            if let Some(other) = els
                .iter()
                .filter(|o| o.start != e.start)
                .flat_map(|o| o.attrs.iter())
                .find(|&&(a, b, _, _)| &d[a..b] == key)
            {
                let ov = &d[other.2..other.3];
                if ov != v {
                    f("attribute value of a sibling", &splice(d, vs, ve, ov));
                }
            }
        }
        if let Some((a, b)) = e.text {
            let t = &d[a..b];
            for v in &bad_values {
                f("value", &splice(d, a, b, v));
            }
            f("value long", &splice(d, a, b, &long));
            f("value suffix x", &splice(d, a, b, &format!("{t}x")));
            f("value suffix space", &splice(d, a, b, &format!("{t} ")));
            f("value prefix space", &splice(d, a, b, &format!(" {t}")));
            f("value tab inside", &splice(d, a, b, &format!("{t}\t{t}")));
            f("value upper", &splice(d, a, b, &t.to_uppercase()));
            f("value lower", &splice(d, a, b, &t.to_lowercase()));
            if let Some((idx, _)) = t.char_indices().last() {
                f("value truncated", &splice(d, a, b, &t[..idx]));
            }
            if t.len() > 1 {
                f(
                    "value split by a comment",
                    &splice(d, a + 1, a + 1, "<!---->"),
                );
            }
        }
    }
    f("trailing text", &format!("{d}x"));
    f("trailing element", &format!("{d}<zzz/>"));
    f("truncated", &d[..d.len() / 2]);
}

// ---------------------------------------------------------------------------
// T8, pattern level
// ---------------------------------------------------------------------------

#[cfg(feature = "xsd11")]
mod patterns {
    use super::*;
    use std::collections::BTreeMap;
    use xsd_schema::plan::alphabet::step_alphabet;
    use xsd_schema::plan::pattern::{build_table, translation, EmittedTable};
    use xsd_schema::types::facets::PatternFacet;
    use xsd_schema::{RegexCompat, XsdVersion};

    const XS: &str = "http://www.w3.org/2001/XMLSchema";

    /// Every `.xsd` under `dir`.
    fn schemas(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
        entries.sort();
        for p in entries {
            if p.is_dir() {
                schemas(&p, out);
            } else if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("xsd")) {
                out.push(p);
            }
        }
    }

    /// The pattern facets of a schema document, and its other literals
    /// (enumerations, fixed and default values).
    fn facets(path: &Path) -> (Vec<String>, Vec<String>) {
        let Ok(bytes) = std::fs::read(path) else {
            return (vec![], vec![]);
        };
        let Ok(text) = String::from_utf8(bytes) else {
            return (vec![], vec![]);
        };
        let Ok(doc) = roxmltree::Document::parse_with_options(
            &text,
            roxmltree::ParsingOptions {
                allow_dtd: true,
                ..Default::default()
            },
        ) else {
            return (vec![], vec![]);
        };
        let (mut pats, mut lits) = (Vec::new(), Vec::new());
        for n in doc
            .descendants()
            .filter(|n| n.is_element() && n.tag_name().namespace() == Some(XS))
        {
            let value = n.attribute("value");
            match (n.tag_name().name(), value) {
                ("pattern", Some(v)) => pats.push(v.to_string()),
                ("enumeration", Some(v)) => lits.push(v.to_string()),
                _ => {}
            }
            for a in ["fixed", "default"] {
                if let Some(v) = n.attribute(a) {
                    lits.push(v.to_string());
                }
            }
        }
        (pats, lits)
    }

    /// Strings the table accepts: walks of the DFA from the start state,
    /// stopping at accepting states, choosing low, high or middle bytes of a
    /// class.
    fn walks(t: &EmittedTable, seed: u64, n: usize) -> Vec<String> {
        let mut rng = seed | 1;
        let mut next = || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        let mut members: Vec<Vec<u8>> = vec![Vec::new(); t.stride];
        for b in 0..=255u8 {
            members[t.classes[b as usize] as usize].push(b);
        }
        let mut out = Vec::new();
        for _ in 0..n * 4 {
            if out.len() >= n {
                break;
            }
            let mut s = t.start as usize;
            let mut bytes = Vec::new();
            for _ in 0..64 {
                if t.accept[s] && next() % 3 == 0 {
                    break;
                }
                let live: Vec<usize> = (0..t.stride)
                    .filter(|&c| t.trans[s * t.stride + c] != 0)
                    .collect();
                if live.is_empty() {
                    break;
                }
                let c = live[(next() % live.len() as u64) as usize];
                let m = &members[c];
                let b = match next() % 3 {
                    0 => m[0],
                    1 => m[m.len() - 1],
                    _ => m[(next() % m.len() as u64) as usize],
                };
                bytes.push(b);
                s = t.trans[s * t.stride + c] as usize;
            }
            if t.accept[s] {
                if let Ok(v) = String::from_utf8(bytes) {
                    out.push(v);
                }
            }
        }
        out
    }

    /// Neighbours of an accepted string: each character moved one code
    /// point down and up, dropped, and repeated.
    fn neighbours(v: &str) -> Vec<String> {
        let chars: Vec<char> = v.chars().collect();
        let mut out = Vec::new();
        for i in 0..chars.len().min(8) {
            for d in [-1i64, 1] {
                if let Some(c) = char::from_u32((chars[i] as i64 + d).clamp(0, 0x10FFFF) as u32) {
                    let mut w = chars.clone();
                    w[i] = c;
                    out.push(w.into_iter().collect());
                }
            }
            let mut w = chars.clone();
            w.remove(i);
            out.push(w.into_iter().collect());
            let mut w = chars.clone();
            w.insert(i, chars[i]);
            out.push(w.into_iter().collect());
        }
        out
    }

    const FIXED: &[&str] = &[
        "",
        " ",
        "a",
        "A",
        "0",
        "9",
        "_",
        "-",
        ".",
        ":",
        "$",
        "^",
        "\r",
        "\n",
        "\t",
        "\u{A0}",
        "\u{85}",
        "\u{2003}",
        "é",
        "ä",
        "ß",
        "Ā",
        "\u{3000}",
        "\u{1D400}",
        "\u{10FFFF}",
        "\u{E000}",
        "a b",
        "ab",
        "abc",
        "12",
        "123",
        "2026-11-11",
    ];

    pub fn run(ws: &Path) -> bool {
        let mut files = Vec::new();
        for d in ["xsdtests", "gaeb/xsd", "wsdl-corpus", "ubl/os-UBL-2.1/xsd"] {
            schemas(&ws.join(d), &mut files);
        }
        let mut seen: HashMap<(String, bool), ()> = HashMap::new();
        let mut stats: BTreeMap<&str, usize> = BTreeMap::new();
        let mut failures = Vec::new();
        let t = Instant::now();
        for file in &files {
            let (pats, lits) = facets(file);
            for p in &pats {
                for version in [XsdVersion::V1_0, XsdVersion::V1_1] {
                    let key = (p.clone(), version == XsdVersion::V1_1);
                    if seen.insert(key, ()).is_some() {
                        continue;
                    }
                    *stats.entry("patterns").or_default() += 1;
                    let Ok(engine1) =
                        PatternFacet::new(p.clone(), None, version, RegexCompat::Strict)
                    else {
                        *stats.entry("refused by regexml").or_default() += 1;
                        continue;
                    };
                    let Ok(table) = build_table(&[p], version) else {
                        *stats.entry("no table").or_default() += 1;
                        continue;
                    };
                    let Ok(alphabet) = step_alphabet(&[p], version) else {
                        *stats.entry("no alphabet").or_default() += 1;
                        continue;
                    };
                    let engine2 = translation(p, version)
                        .ok()
                        .and_then(|t| regex::Regex::new(&t).ok());
                    *stats.entry("checked").or_default() += 1;
                    let mut values: Vec<String> = lits.iter().take(400).cloned().collect();
                    values.extend(FIXED.iter().map(|s| s.to_string()));
                    let walked = walks(&table, p.len() as u64 * 7919 + 17, 24);
                    for w in &walked {
                        values.extend(neighbours(w));
                    }
                    values.extend(walked);
                    for v in &values {
                        *stats.entry("values").or_default() += 1;
                        let accepted = table.accepts(v.as_bytes());
                        if accepted {
                            if let Some(re) = &engine2 {
                                if !re.is_match(v) {
                                    failures.push(format!("TRIPWIRE {version:?} /{p}/ table accepts {v:?}, regex refuses"));
                                }
                            }
                        }
                        let inside = v.chars().all(|c| alphabet.contains(c));
                        if accepted && inside {
                            *stats.entry("claims").or_default() += 1;
                            if !engine1.matches(v) {
                                failures.push(format!(
                                    "UNSOUND {version:?} /{p}/ {v:?} (in {})",
                                    file.strip_prefix(ws).unwrap_or(file).display()
                                ));
                            }
                        }
                    }
                }
            }
        }
        corner_patterns(&mut stats, &mut failures);
        println!(
            "T8 pattern level: {} schema files, {:.1} s",
            files.len(),
            t.elapsed().as_secs_f64()
        );
        for (k, n) in &stats {
            println!("  {k}: {n}");
        }
        for f in failures.iter().take(40) {
            println!("  {f}");
        }
        println!("  failures: {}", failures.len());
        !failures.is_empty()
    }

    /// Hand-written patterns at the corners of the two engines' syntax
    /// (class subtraction, ASCII-class and set-operator look-alikes,
    /// category, block and name escapes, anchors, quantifiers), and the
    /// characters around them.
    const CORNER_PATTERNS: &[&str] = &[
        "[a-z-[:alpha:]]",
        "[a-z-[:^alpha:]]",
        "[a-z-[:alpha:]]*",
        "[\\w-[_]]",
        "\\w+",
        "\\W",
        "[^\\W]",
        "[\\W]",
        "\\d+",
        "\\D",
        "[\\D]",
        "[^\\D]",
        "\\p{L}",
        "[\\P{L}]",
        "\\P{L}",
        "[^\\p{L}]",
        "\\p{Nd}",
        "\\p{Lu}\\p{Ll}*",
        "[\\p{Lu}-[A]]",
        "\\p{Cn}",
        "[^\\p{Cn}]",
        "\\p{Co}",
        "\\P{Co}",
        "[\\P{Co}]",
        "\\p{Zs}",
        "\\p{Sc}",
        "\\p{Lt}",
        "\\p{Mn}",
        "\\p{S}",
        "\\p{P}",
        "[\\p{P}\\p{S}]",
        "\\p{C}",
        "\\P{C}",
        ".",
        ".*",
        "..",
        "[.]",
        "\\.",
        "a.b",
        ".\\r",
        "[^\\n]",
        "[^\\n\\r]",
        "\\s",
        "[^\\s]",
        "\\S+",
        "[\\s\\S]",
        "(\\s|x)",
        "\\s{2}",
        "[ \\t]",
        "\\r",
        "[\\r\\n]",
        "\\S",
        "[\\S]",
        "\\s*",
        "\\i\\c*",
        "[\\i-[:]]",
        "[\\c-[\\d]]",
        "\\I",
        "\\C",
        "[\\I]",
        "[^\\i]",
        "\\c+",
        "[\\c]",
        "[^\\c]",
        "[\\C-[\\s]]",
        "a{2,3}",
        "(a|b)*c",
        "[a\\-z]",
        "[a-]",
        "[-a]",
        "[\\^a]",
        "[a^]",
        "a\\|b",
        "a?b+",
        "[^a]",
        "[^a-z-[q]]",
        "[a-c-e]",
        "[\\-\\^\\]]",
        "\\{",
        "[A-z]",
        "\u{e9}",
        "[\u{e0}-\u{ff}]",
        "a|",
        "|a",
        "()",
        "(a)",
        "a{0}",
        "a{1,}",
        "[a-z-[b-d-[c]]]",
        "[^a-z-[^b]]",
        "[a-z-[^b-d]]",
        "\\p{IsBasicLatin}",
        "[\\p{IsBasicLatin}-[a]]",
        "\\P{IsGreek}",
        "[\\P{IsPrivateUse}]",
        "\\P{IsPrivateUse}",
        "\\p{IsPrivateUse}",
        "[\\P{IsBasicLatin}]",
        "[^\\p{IsBasicLatin}]",
        "\\p{IsLatin-1Supplement}",
        "[\\w\\W]",
        "[\\d\\D]",
        "[\\d-[1]]",
        "[\\w-[a]]",
        "[\\W-[ ]]",
        "[^\\w]",
        "[^\\d]",
        "[^\\W-[a]]",
        "[a&]",
        "a&b",
        "[a&&[b]]",
        "[a~~b]",
        "[a--b]",
        "^a",
        "a$",
        "[$]",
        "[\\^$]",
        "\\$",
        "[+\\-]?[0-9]+",
        "[A-Z]{2}[0-9]{3}",
        "[^:]*",
        "([a-z])+",
        "(ab|a)(c|bc)",
        "a*a",
        "(a*)*b",
        "(a|aa)+",
        "[\u{10000}-\u{10ffff}]",
        "[^\u{10000}-\u{10ffff}]",
        "\u{10000}",
        "[\u{e000}-\u{f8ff}]",
        "\\p{Ll}",
        "\\p{Lo}",
        "\\p{N}",
        "\\p{No}",
        "\\p{Z}",
        "[\\p{L}-[\\p{Lu}]]",
        "[\\P{L}-[\\P{Lu}]]",
        "\\w\\s\\d",
        "[\\w\\s]",
        "[^\\w\\s]",
        "[\\i\\d]",
        "\\n",
        "\\t",
        "[\\n\\t ]",
        "\\p{IsCombiningDiacriticalMarks}",
        "\\p{IsGeneralPunctuation}",
        "[\\p{IsGeneralPunctuation}\\s]",
        "\\p{IsArabic}",
        "[\\p{IsArabic}\\d]",
        "\\p{IsSupplementaryPrivateUseArea-A}",
        "[\\P{IsSupplementaryPrivateUseArea-A}]",
        "\\p{IsHighSurrogates}",
        "[\\P{IsHighSurrogates}]",
    ];

    const CORNER_ALPHABET: &[char] = &[
        'a',
        'b',
        'c',
        'z',
        'A',
        'q',
        '_',
        ':',
        '-',
        '.',
        '^',
        '$',
        '1',
        '\u{663}',
        '\u{e9}',
        '\u{1c5}',
        '\u{a0}',
        '\u{2003}',
        '\u{85}',
        '\t',
        '\n',
        '\r',
        ' ',
        '\u{e000}',
        '\u{f0000}',
        '\u{10000}',
        '\u{300}',
        '\u{20ac}',
        '+',
        '~',
        '|',
        '`',
        '<',
        '=',
        '&',
        '\u{2028}',
        '\u{3a9}',
        '\u{378}',
        '\u{d7ff}',
        '\u{fffd}',
        ']',
        '[',
        '\\',
        '\u{2165}',
        '\u{b2}',
        '\u{2e2f}',
        '\u{1885}',
        '\u{ff10}',
        '\u{10ffff}',
        '\u{100000}',
    ];

    /// One step of a check: its patterns, table, alphabet and both engines.
    struct Step {
        patterns: Vec<String>,
        table: EmittedTable,
        alphabet: xsd_schema::plan::alphabet::ComposedAlphabet,
        regexml: Vec<PatternFacet>,
        regex: Vec<Option<regex::Regex>>,
    }

    fn step(patterns: &[&str], version: XsdVersion) -> Option<Step> {
        let mut regexml = Vec::new();
        for p in patterns {
            regexml
                .push(PatternFacet::new(p.to_string(), None, version, RegexCompat::Strict).ok()?);
        }
        Some(Step {
            patterns: patterns.iter().map(|p| p.to_string()).collect(),
            table: build_table(patterns, version).ok()?,
            alphabet: step_alphabet(patterns, version).ok()?,
            regexml,
            regex: patterns
                .iter()
                .map(|p| {
                    translation(p, version)
                        .ok()
                        .and_then(|t| regex::Regex::new(&t).ok())
                })
                .collect(),
        })
    }

    fn xorshift(rng: &mut u64) -> u64 {
        *rng ^= *rng << 13;
        *rng ^= *rng >> 7;
        *rng ^= *rng << 17;
        *rng
    }

    /// Every string up to length `exhaustive` over `alphabet`, and `random`
    /// longer ones.
    fn corner_values(
        rng: &mut u64,
        alphabet: &[char],
        exhaustive: usize,
        random: usize,
    ) -> Vec<String> {
        let mut out = vec![String::new()];
        let mut level = vec![String::new()];
        for _ in 0..exhaustive {
            let mut longer = Vec::new();
            for s in &level {
                for &c in alphabet {
                    let mut q = s.clone();
                    q.push(c);
                    longer.push(q);
                }
            }
            out.extend(longer.iter().cloned());
            level = longer;
        }
        for _ in 0..random {
            let len = exhaustive + 1 + (xorshift(rng) % 4) as usize;
            out.push(
                (0..len)
                    .map(|_| alphabet[(xorshift(rng) % alphabet.len() as u64) as usize])
                    .collect(),
            );
        }
        out
    }

    /// Judge a chain of steps on `values`: where every step's table accepts
    /// a value inside its alphabet, regexml must accept it for every step
    /// (one of the step's patterns); and a table must never accept what the
    /// `regex` engine refuses on every translation of its step.
    fn judge(
        steps: &[Step],
        values: &[String],
        label: &str,
        stats: &mut BTreeMap<&'static str, usize>,
        failures: &mut Vec<String>,
    ) {
        for v in values {
            *stats.entry("corner values").or_default() += 1;
            let mut claim = true;
            for s in steps {
                let accepted = s.table.accepts(v.as_bytes());
                let translated = s.regex.iter().all(Option::is_some);
                if accepted && translated && s.regex.iter().flatten().all(|r| !r.is_match(v)) {
                    failures.push(format!(
                        "TRIPWIRE {label} {:?}: table accepts {v:?}, regex refuses",
                        s.patterns
                    ));
                }
                claim &= accepted && v.chars().all(|c| s.alphabet.contains(c));
            }
            if claim {
                *stats.entry("corner claims").or_default() += 1;
                for s in steps {
                    if !s.regexml.iter().any(|r| r.matches(v)) {
                        failures.push(format!("UNSOUND {label} {:?} {v:?}", s.patterns));
                    }
                }
            }
        }
    }

    /// The corner patterns, each as a one-pattern step over every string up
    /// to length 2 of the corner alphabet and 2,000 longer ones; then random
    /// two-pattern steps and two-step chains of them over every string of
    /// length 1 and 600 longer ones; under both XSD versions.
    fn corner_patterns(stats: &mut BTreeMap<&'static str, usize>, failures: &mut Vec<String>) {
        let mut rng = 0x9E37_79B9_7F4A_7C15u64;
        for version in [XsdVersion::V1_0, XsdVersion::V1_1] {
            let label = format!("{version:?}");
            let values = corner_values(&mut rng, CORNER_ALPHABET, 2, 2000);
            let mut usable: Vec<&str> = Vec::new();
            for p in CORNER_PATTERNS {
                *stats.entry("corner patterns").or_default() += 1;
                match step(&[p], version) {
                    Some(s) => {
                        usable.push(p);
                        judge(&[s], &values, &label, stats, failures);
                    }
                    None => {
                        *stats
                            .entry(
                                "corner patterns without a table, an alphabet or a regexml program",
                            )
                            .or_default() += 1
                    }
                }
            }
            let short = corner_values(&mut rng, CORNER_ALPHABET, 1, 600);
            for _ in 0..400 {
                let mut pick = || usable[(xorshift(&mut rng) % usable.len() as u64) as usize];
                let (a, b, c, d) = (pick(), pick(), pick(), pick());
                if let Some(s) = step(&[a, b], version) {
                    *stats.entry("corner two-pattern steps").or_default() += 1;
                    judge(&[s], &short, &label, stats, failures);
                }
                if let (Some(s1), Some(s2)) = (step(&[c], version), step(&[d], version)) {
                    *stats.entry("corner two-step chains").or_default() += 1;
                    judge(&[s1, s2], &short, &label, stats, failures);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Derived types and attribute uses
// ---------------------------------------------------------------------------

mod derived {
    use super::*;

    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
        fn pick<'a, T>(&mut self, v: &'a [T]) -> &'a T {
            &v[self.below(v.len())]
        }
    }

    /// An attribute use: a local `a`/`b`/`c`, or (`g`) a reference to one of
    /// the global attributes, with a random type, use and value constraint.
    fn attr_use(r: &mut Rng, name: &str) -> String {
        let ty = *r.pick(&["xs:string", "xs:string", "xs:token", "K", "xs:int"]);
        let lit = if ty == "xs:int" {
            *r.pick(&["5", "7"])
        } else {
            *r.pick(&["x", "y"])
        };
        let vc = match r.below(6) {
            0 => format!(" default=\"{lit}\""),
            1 | 2 => format!(" fixed=\"{lit}\""),
            _ => String::new(),
        };
        let usek = match r.below(8) {
            0 | 1 => " use=\"required\"",
            5..=7 => " use=\"prohibited\"",
            _ => "",
        };
        let vc = if usek.contains("required") && vc.contains("default") {
            String::new()
        } else {
            vc
        };
        if name == "g" {
            let g = *r.pick(&["ga", "gf", "gd"]);
            let vc = if g == "ga" {
                vc.replace('5', "x").replace('7', "y")
            } else {
                String::new()
            };
            let usek = if g == "gd" && usek.contains("required") {
                ""
            } else {
                usek
            };
            return format!("<xs:attribute ref=\"{g}\"{usek}{vc}/>");
        }
        format!("<xs:attribute name=\"{name}\" type=\"{ty}\"{usek}{vc}/>")
    }

    fn attr_set(r: &mut Rng, groups: &[&str]) -> String {
        let mut s = String::new();
        for n in ["a", "b", "c", "g"] {
            if r.below(100) < 45 {
                s.push_str(&attr_use(r, n));
            }
        }
        if !groups.is_empty() && r.below(3) == 0 {
            s.push_str(&format!("<xs:attributeGroup ref=\"{}\"/>", r.pick(groups)));
        }
        s
    }

    fn schema(r: &mut Rng) -> String {
        let mut s = String::from(
            "<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\">\
             <xs:simpleType name=\"K\"><xs:restriction base=\"xs:token\"><xs:enumeration value=\"x\"/>\
             <xs:enumeration value=\"y\"/></xs:restriction></xs:simpleType>\
             <xs:attribute name=\"ga\" type=\"xs:string\"/><xs:attribute name=\"gf\" type=\"xs:string\" fixed=\"x\"/>\
             <xs:attribute name=\"gd\" type=\"xs:string\" default=\"x\"/>",
        );
        s.push_str(&format!(
            "<xs:attributeGroup name=\"G1\">{}</xs:attributeGroup>",
            attr_set(r, &[])
        ));
        s.push_str(&format!(
            "<xs:attributeGroup name=\"G2\">{}</xs:attributeGroup>",
            attr_set(r, &["G1"])
        ));
        let groups = ["G1", "G2"];
        let optional = |n: &str| {
            format!("<xs:sequence><xs:element name=\"{n}\" type=\"xs:string\" minOccurs=\"0\"/></xs:sequence>")
        };
        let content0 = if r.below(2) == 0 {
            String::new()
        } else {
            optional("e")
        };
        s.push_str(&format!(
            "<xs:complexType name=\"T0\">{content0}{}</xs:complexType>",
            attr_set(r, &groups)
        ));
        let mut prev = "T0";
        let mut has_content = !content0.is_empty();
        for (name, added) in [("T1", "f"), ("T2", "h")] {
            let how = if r.below(2) == 0 {
                "extension"
            } else {
                "restriction"
            };
            let content = if how == "restriction" && has_content {
                [optional("e"), "<xs:sequence/>".to_string(), String::new()][r.below(3)].clone()
            } else if how == "extension" && r.below(3) == 0 {
                has_content = true;
                optional(added)
            } else {
                String::new()
            };
            s.push_str(&format!(
                "<xs:complexType name=\"{name}\"><xs:complexContent><xs:{how} base=\"{prev}\">{content}{}</xs:{how}></xs:complexContent></xs:complexType>",
                attr_set(r, &groups)
            ));
            prev = name;
            if r.below(4) == 0 {
                break;
            }
        }
        s.push_str(&format!(
            "<xs:element name=\"r\" type=\"{prev}\"/></xs:schema>"
        ));
        s
    }

    fn load(xsd: &str, v11: bool) -> Option<SchemaSet> {
        let b = if v11 {
            SchemaSetBuilder::xsd11()
        } else {
            SchemaSetBuilder::new()
        };
        let b = b.add_bytes(xsd.as_bytes(), "derived.xsd").ok()?;
        b.compile().ok().map(|c| c.into_schema_set())
    }

    pub fn run(count: usize) -> bool {
        let mut failed = false;
        for (v11, seed) in [
            (false, 0x9E37_79B9_7F4A_7C15u64),
            (true, 0x9E37_79B9_7F4A_7C40),
        ] {
            let t = Instant::now();
            let mut r = Rng(seed);
            let (
                mut schemas,
                mut refused,
                mut docs,
                mut dynamic_valid,
                mut shipped_valid,
                mut delegating_valid,
                mut unsound,
            ) = (0usize, 0usize, 0usize, 0usize, 0usize, 0usize, 0usize);
            let values: [Option<&str>; 6] = [
                None,
                Some("x"),
                Some("y"),
                Some("wrong"),
                Some("5"),
                Some(" x "),
            ];
            let bodies = [
                "/>",
                "/>",
                "><e>t</e></r>",
                "><e/><f/></r>",
                "><f>t</f></r>",
                "><e/><f/><h/></r>",
                "><h/></r>",
            ];
            while schemas < count {
                let xsd = schema(&mut r);
                let Some(set) = load(&xsd, v11) else {
                    refused += 1;
                    continue;
                };
                schemas += 1;
                let (data, _) = plan::build(&set);
                let validator = SchemaValidator::new(&set, ValidationFlags::default());
                let mut scratch = plan::Scratch::new();
                for a in values {
                    for b in values {
                        for c in values {
                            for g in [Some("ga"), Some("gf"), Some("gd"), None] {
                                let mut d = String::from("<r");
                                for (n, v) in [("a", a), ("b", b), ("c", c)] {
                                    if let Some(v) = v {
                                        d.push_str(&format!(" {n}=\"{v}\""));
                                    }
                                }
                                if let Some(g) = g {
                                    let gv = *r.pick(&["x", "y", "wrong"]);
                                    d.push_str(&format!(" {g}=\"{gv}\""));
                                }
                                d.push_str(r.pick::<&str>(&bodies));
                                docs += 1;
                                let bytes = d.as_bytes();
                                let dynamic = plan::dynamic_valid(&validator, bytes);
                                let shipped = plan::check(data.as_ref(), bytes, &mut scratch);
                                let mut tier = plan::Delegating::new(&set, &data);
                                let delegating =
                                    plan::check_with(data.as_ref(), bytes, &mut scratch, &mut tier);
                                dynamic_valid += usize::from(dynamic);
                                shipped_valid += usize::from(shipped == Verdict::Valid);
                                delegating_valid += usize::from(delegating == Verdict::Valid);
                                for (run, v) in [("shipped", shipped), ("delegating", delegating)] {
                                    if v == Verdict::Valid && !dynamic {
                                        unsound += 1;
                                        if unsound <= 20 {
                                            println!("  UNSOUND [{run}] {d}\n    schema: {xsd}");
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            println!(
                "derived types, XSD {}: {schemas} schemas ({refused} refused), {docs} documents, dynamically valid {dynamic_valid}, shipped Valid {shipped_valid}, delegating Valid {delegating_valid}, UNSOUND {unsound}, {:.1} s",
                if v11 { "1.1" } else { "1.0" },
                t.elapsed().as_secs_f64()
            );
            failed |= unsound > 0;
        }
        failed
    }
}

// ---------------------------------------------------------------------------
// Value checks: random derived types against values on their boundaries
// ---------------------------------------------------------------------------

mod values {
    use super::*;

    pub struct Rng(pub u64);

    impl Rng {
        pub fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        pub fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
        pub fn pick<'a, T>(&mut self, v: &'a [T]) -> &'a T {
            &v[self.below(v.len())]
        }
        fn chance(&mut self, percent: usize) -> bool {
            self.below(100) < percent
        }
    }

    /// A family of built-in datatypes: the bases a type may restrict, the
    /// facets it takes, and values on the boundaries of its lexical space.
    struct Family {
        bases: &'static [&'static str],
        /// Range facets take their bounds from `values`.
        range: bool,
        digits: bool,
        length: bool,
        whitespace: bool,
        patterns: &'static [&'static str],
        values: &'static [&'static str],
    }

    const INTEGERS: &[&str] = &[
        "0",
        "-0",
        "+0",
        "00",
        "1",
        "-1",
        "+1",
        "007",
        "-007",
        "2",
        "5",
        "9",
        "10",
        "99",
        "100",
        "127",
        "128",
        "-128",
        "-129",
        "255",
        "256",
        "32767",
        "32768",
        "-32768",
        "-32769",
        "65535",
        "65536",
        "2147483647",
        "2147483648",
        "-2147483648",
        "-2147483649",
        "4294967295",
        "4294967296",
        "9223372036854775807",
        "9223372036854775808",
        "-9223372036854775808",
        "-9223372036854775809",
        "18446744073709551615",
        "18446744073709551616",
        "79228162514264337593543950335",
        "79228162514264337593543950336",
        "170141183460469231731687303715884105727",
        "170141183460469231731687303715884105728",
        "-170141183460469231731687303715884105728",
        "-170141183460469231731687303715884105729",
        "999999999999999999999999999999999999999",
        "12345",
        "-12345",
        "1000",
        "0001000",
        "1.0",
        "1.",
        ".0",
        "1e3",
        "1E3",
        "",
        " ",
        "+",
        "-",
        "--1",
        "+-1",
        "1_000",
        "1,0",
        "0x10",
        "١",
        "1 2",
        " 5",
        "5 ",
        "\n 42\t",
        "\u{a0}5",
        "INF",
        "NaN",
    ];
    const DECIMALS: &[&str] = &[
        "0",
        "-0",
        "+0",
        "0.0",
        "-0.0",
        "+0.000",
        ".0",
        "0.",
        ".5",
        "5.",
        "+.5",
        "-.5",
        "1.5",
        "1.50",
        "-1.50",
        "001.500",
        "0.05",
        "0.050",
        "100",
        "100.0",
        "99.99",
        "99.995",
        "100.00",
        "100.01",
        "-100",
        "12345.6789",
        "0.000000000000000000000000000001",
        "79228162514264337593543950335",
        "79228162514264337593543950335.5",
        "123456789012345678901234567890123456",
        "1234567890123456789012345678901234567",
        "12345678901234567890123456789012345678",
        "0.123456789012345678901234567890123456",
        "9.99",
        "10",
        "10.0",
        "-9.99",
        "1",
        "-1",
        "2",
        "1e5",
        "1E-2",
        "INF",
        "NaN",
        "",
        ".",
        "-",
        "+.",
        "1..2",
        "1.2.3",
        "1,5",
        " 1.5 ",
        "\t2.5\n",
        "1 .5",
    ];
    const BOOLEANS: &[&str] = &[
        "true", "false", "1", "0", " true ", "\nfalse", "TRUE", "True", "yes", "", "01", "t",
    ];
    const FLOATS: &[&str] = &[
        "0",
        "-0",
        "+0",
        "1",
        "1.5",
        "-1.5",
        ".5",
        "5.",
        "1e10",
        "1E+10",
        "1e-10",
        "1.5E-3",
        "+1.5e3",
        "1e45",
        "1e-45",
        "1e-46",
        "3.4028235e38",
        "3.4028236e38",
        "3.5e38",
        "1e309",
        "1e999",
        "1e-999",
        "INF",
        "-INF",
        "+INF",
        "NaN",
        "-NaN",
        "+NaN",
        "nan",
        "inf",
        "Infinity",
        "",
        ".",
        "e5",
        "1e",
        "1.e5",
        "0x1p3",
        " 1.5 ",
        "\t-INF\n",
        "1 5",
        "1,5",
        "1d",
    ];
    const DATETIMES: &[&str] = &[
        "2000-01-01T00:00:00",
        "2000-01-01T00:00:00Z",
        "2000-01-01T00:00:00+00:00",
        "2000-01-01T00:00:00-00:00",
        "2000-01-01T00:00:00+14:00",
        "2000-01-01T00:00:00-14:00",
        "2000-01-01T00:00:00+14:01",
        "2000-01-01T00:00:00+13:59",
        "2000-01-01T00:00:00+15:00",
        "2000-01-01T00:00:00+05",
        "2000-01-01T00:00:00+0500",
        "2000-01-01T12:30:45.5",
        "2000-01-01T12:30:45.50",
        "2000-01-01T12:30:45.123456789012345678901234567890",
        "2000-01-01T12:30:45.",
        "2000-01-01T24:00:00",
        "2000-01-01T24:00:00Z",
        "2000-12-31T24:00:00",
        "2000-01-01T24:00:00.000",
        "2000-01-01T24:00:01",
        "2000-01-01T24:01:00",
        "2000-01-01T23:59:59",
        "2000-01-01T23:59:60",
        "2000-01-01T23:60:00",
        "2000-01-01T25:00:00",
        "2000-02-29T00:00:00",
        "1900-02-29T00:00:00",
        "2100-02-29T00:00:00",
        "2004-02-29T10:00:00",
        "1999-12-31T23:59:59",
        "1999-12-31T23:59:59Z",
        "2000-01-02T00:00:00",
        "1999-12-31T10:00:00-14:00",
        "2000-01-01T09:59:59+14:00",
        "0001-01-01T00:00:00",
        "0000-01-01T00:00:00",
        "-0001-01-01T00:00:00",
        "9999-12-31T23:59:59",
        "10000-01-01T00:00:00",
        "02000-01-01T00:00:00",
        "2000-1-01T00:00:00",
        "2000-01-1T00:00:00",
        "2000-01-01T0:00:00",
        "2000-01-01 00:00:00",
        "2000-01-01T00:00",
        "2000-01-01",
        "2000-13-01T00:00:00",
        "2000-00-01T00:00:00",
        "2000-04-31T00:00:00",
        "",
        "T",
        " 2000-01-01T00:00:00 ",
        "\n2000-01-01T00:00:00Z\t",
        "2000-01-01T00:00:00 Z",
        "2000-01-01T00:00:00z",
        "+2000-01-01T00:00:00",
    ];
    const DATES: &[&str] = &[
        "2000-01-01",
        "2000-01-01Z",
        "2000-01-01+14:00",
        "2000-01-01-14:00",
        "2000-01-01+14:01",
        "2000-01-01-00:00",
        "1999-12-31",
        "1999-12-31Z",
        "2000-01-02",
        "2000-02-29",
        "1900-02-29",
        "2100-02-28",
        "2004-02-29",
        "1970-01-01",
        "1969-12-31",
        "0001-01-01",
        "0000-01-01",
        "-0001-01-01",
        "9999-12-31",
        "10000-01-01",
        "2000-1-1",
        "2000-01-32",
        "2000-04-31",
        "2000-13-01",
        "2000-01",
        "",
        " 1970-01-01 ",
        "2000-01-01T00:00:00",
        "2000-01-01+05",
        "2000-01-01Z ",
        "2000-01-01+13:00",
        "2000-01-01-11:00",
    ];
    const TIMES: &[&str] = &[
        "00:00:00",
        "00:00:00Z",
        "12:00:00",
        "12:00:00.5",
        "12:00:00.500",
        "23:59:59",
        "23:59:59.999999999999999999999999999999",
        "24:00:00",
        "24:00:00Z",
        "24:00:00.0",
        "24:00:01",
        "23:59:60",
        "23:60:00",
        "25:00:00",
        "12:00:00+14:00",
        "12:00:00-14:00",
        "12:00:00+14:30",
        "12:00:00+05:30",
        "1:00:00",
        "12:0:00",
        "12:00",
        "12:00:00.",
        "",
        " 12:00:00 ",
        "12:00:00 Z",
        "11:59:59",
        "12:00:01",
    ];
    const GREGORIAN: &[&str] = &[
        "2000",
        "2000Z",
        "2000+14:00",
        "2000-14:00",
        "0000",
        "-0001",
        "0001",
        "9999",
        "10000",
        "02000",
        "200",
        "2000-01",
        "2000-12",
        "2000-13",
        "2000-00",
        "2000-01Z",
        "--01",
        "--12",
        "--13",
        "--00",
        "--01Z",
        "--01--",
        "--02-29",
        "--02-30",
        "--04-31",
        "--12-31",
        "--01-01Z",
        "---01",
        "---31",
        "---32",
        "---00",
        "---15Z",
        "---15-14:00",
        "",
        " 2000 ",
        "--1",
    ];
    const DURATIONS: &[&str] = &[
        "P1Y",
        "P1M",
        "P1D",
        "PT1H",
        "PT1M",
        "PT1S",
        "PT1.5S",
        "PT0S",
        "P0D",
        "-P1D",
        "-PT1S",
        "P1Y2M3DT4H5M6.7S",
        "P1Y2M",
        "P3DT4H",
        "PT4H5M",
        "P",
        "PT",
        "-P",
        "P1YT",
        "P1.5Y",
        "PT1.S",
        "PT.5S",
        "P1M1Y",
        "P1D1M",
        "PT1S1M",
        "P1Y1Y",
        "P999999999Y",
        "P4294967295Y",
        "P4294967296Y",
        "P0000000001Y",
        "PT999999999999999999S",
        "PT9999999999999999999S",
        "PT99999999999999999999999999999S",
        "P1W",
        "1Y",
        "",
        " P1Y ",
        "P1Y ",
        "P-1Y",
        "+P1Y",
        "pT1S",
        "P1y",
    ];
    const HEX: &[&str] = &[
        "",
        "00",
        "0a",
        "0A",
        "ff",
        "FF",
        "00ff",
        "0",
        "abc",
        "0g",
        "00 11",
        " 00ff ",
        "0x00",
        "deadbeef",
        "DEADBEEF",
        "0102030405060708090a0b0c0d0e0f10",
    ];
    const BASE64: &[&str] = &[
        "",
        "QQ==",
        "QUI=",
        "QUJD",
        "QUJDRA==",
        "QUJDREU=",
        "QUJDREVG",
        "QQ",
        "QUI",
        "QUJ",
        "QR==",
        "QUJ=",
        "Q===",
        "=QQ=",
        "QQ==QQ==",
        "QUJD RA==",
        "QUJD\nRA==",
        " QUJD ",
        "QUJD  RA==",
        "Q U J D",
        "QUJD!",
        "QUJD-_==",
        "+/+/",
        "////",
        "AAAA",
        "A===",
    ];
    const URIS: &[&str] = &[
        "http://example.com/a",
        "",
        "a",
        "#f",
        "urn:x:y",
        " http://a/b ",
        "a b",
        "%zz",
        "ä",
        "http://a/b?c=d&e",
        "\\\\x",
        "1:a",
        "::",
    ];
    const NAMES: &[&str] = &[
        "a", "ab", "a1", "a-b", "a.b", "_a", "a:b", ":a", "1a", "-a", ".a", "ä", "aä", "a b", "",
        " a ", "\na\t", "a\u{b7}", "\u{300}a", "xml", "a\u{37e}", "日本",
    ];
    const LANGUAGES: &[&str] = &[
        "en",
        "en-US",
        "e",
        "abcdefgh",
        "abcdefghi",
        "en-",
        "-en",
        "en-12345678",
        "en-123456789",
        "x-a",
        "1en",
        "en_US",
        "",
        " en ",
        "en-US-x-y",
        "EN",
    ];
    const STRINGS: &[&str] = &[
        "",
        "a",
        "ab",
        "abc",
        "abcd",
        "a b",
        "a  b",
        " a",
        "a ",
        " a b ",
        "a\tb",
        "a\nb",
        "a\r\nb",
        "\t",
        "  ",
        "x",
        "xyz",
        "é",
        "日本語",
        "a\u{a0}b",
        "A1",
        "1",
        "-",
    ];

    const FAMILIES: &[Family] = &[
        Family {
            bases: &[
                "xs:integer",
                "xs:long",
                "xs:int",
                "xs:short",
                "xs:byte",
                "xs:nonNegativeInteger",
                "xs:positiveInteger",
                "xs:nonPositiveInteger",
                "xs:negativeInteger",
                "xs:unsignedLong",
                "xs:unsignedInt",
                "xs:unsignedShort",
                "xs:unsignedByte",
            ],
            range: true,
            digits: true,
            length: false,
            whitespace: false,
            patterns: &[
                "\\d{1,3}",
                "-?[0-9]+",
                "[0-9]*5",
                "[^0]*",
                "[1-9][0-9]*|0",
                "\\d*",
            ],
            values: INTEGERS,
        },
        Family {
            bases: &["xs:decimal"],
            range: true,
            digits: true,
            length: false,
            whitespace: false,
            patterns: &[
                "-?\\d+\\.\\d{2}",
                "[0-9.]+",
                "\\d*\\.?\\d*",
                "-?[0-9]{1,3}(\\.[0-9]+)?",
            ],
            values: DECIMALS,
        },
        Family {
            bases: &["xs:boolean"],
            range: false,
            digits: false,
            length: false,
            whitespace: false,
            patterns: &["true|false", "[01]", "\\w+"],
            values: BOOLEANS,
        },
        Family {
            bases: &["xs:float", "xs:double"],
            range: false,
            digits: false,
            length: false,
            whitespace: false,
            patterns: &["[0-9.]+", "-?\\d+(\\.\\d+)?", ".*E.*|[^E]*"],
            values: FLOATS,
        },
        Family {
            bases: &["xs:dateTime"],
            range: true,
            digits: false,
            length: false,
            whitespace: false,
            patterns: &[".*Z", "\\d{4}-.*", "[^+]*"],
            values: DATETIMES,
        },
        Family {
            bases: &["xs:date"],
            range: true,
            digits: false,
            length: false,
            whitespace: false,
            patterns: &["\\d{4}-\\d{2}-\\d{2}", "2.*"],
            values: DATES,
        },
        Family {
            bases: &["xs:time"],
            range: true,
            digits: false,
            length: false,
            whitespace: false,
            patterns: &["\\d{2}:\\d{2}:\\d{2}", "1.*"],
            values: TIMES,
        },
        Family {
            bases: &["xs:gYear"],
            range: false,
            digits: false,
            length: false,
            whitespace: false,
            patterns: &["[0-9-]+", ".*Z"],
            values: GREGORIAN,
        },
        Family {
            bases: &["xs:gYearMonth"],
            range: false,
            digits: false,
            length: false,
            whitespace: false,
            patterns: &["[0-9-]+", ".*Z"],
            values: GREGORIAN,
        },
        Family {
            bases: &["xs:gMonthDay", "xs:gDay", "xs:gMonth"],
            range: false,
            digits: false,
            length: false,
            whitespace: false,
            patterns: &["[0-9-]+", ".*Z"],
            values: GREGORIAN,
        },
        Family {
            bases: &["xs:duration"],
            range: false,
            digits: false,
            length: false,
            whitespace: false,
            patterns: &["P.*", "-?P\\d+Y"],
            values: DURATIONS,
        },
        Family {
            bases: &["xs:hexBinary"],
            range: false,
            digits: false,
            length: true,
            whitespace: false,
            patterns: &["[0-9a-f]*", "(..)+"],
            values: HEX,
        },
        Family {
            bases: &["xs:base64Binary"],
            range: false,
            digits: false,
            length: true,
            whitespace: false,
            patterns: &["[A-Z]+=*", ".{4}"],
            values: BASE64,
        },
        Family {
            bases: &["xs:anyURI"],
            range: false,
            digits: false,
            length: true,
            whitespace: false,
            patterns: &["http:.*", "[a-z]*"],
            values: URIS,
        },
        Family {
            bases: &["xs:Name", "xs:NCName", "xs:NMTOKEN", "xs:ID"],
            range: false,
            digits: false,
            length: true,
            whitespace: false,
            patterns: &["[a-z]+", "\\i\\c*", "a.*"],
            values: NAMES,
        },
        Family {
            bases: &["xs:language"],
            range: false,
            digits: false,
            length: true,
            whitespace: false,
            patterns: &["[a-z]{2}", "en.*"],
            values: LANGUAGES,
        },
        Family {
            bases: &["xs:string", "xs:normalizedString", "xs:token"],
            range: false,
            digits: false,
            length: true,
            whitespace: true,
            patterns: &["a.*", "[a-z ]*", "\\s*a\\s*b", ".{2,3}", "\\S+", "[^\\t]*"],
            values: STRINGS,
        },
    ];

    fn esc(v: &str) -> String {
        let mut out = String::new();
        for c in v.chars() {
            match c {
                '&' => out.push_str("&amp;"),
                '<' => out.push_str("&lt;"),
                '>' => out.push_str("&gt;"),
                '"' => out.push_str("&quot;"),
                '\'' => out.push_str("&apos;"),
                '\t' => out.push_str("&#x9;"),
                '\n' => out.push_str("&#xA;"),
                '\r' => out.push_str("&#xD;"),
                c => out.push(c),
            }
        }
        out
    }

    /// One restriction step's facets, bounds taken from the family's values;
    /// the bound literals go to `bounds`. Now and then none at all.
    fn facets(r: &mut Rng, f: &Family, base: &str, bounds: &mut Vec<String>) -> String {
        let mut s = String::new();
        if r.chance(20) {
            return s;
        }
        if f.range {
            for facet in [
                "minInclusive",
                "maxInclusive",
                "minExclusive",
                "maxExclusive",
            ] {
                if r.chance(22) {
                    let b = *r.pick(f.values);
                    bounds.push(b.to_string());
                    s.push_str(&format!("<xs:{facet} value=\"{}\"/>", esc(b)));
                }
            }
        }
        if f.digits {
            if r.chance(20) {
                let n = *r.pick(&[1, 2, 3, 4, 5, 9, 18, 28, 29, 36, 38, 39]);
                s.push_str(&format!("<xs:totalDigits value=\"{n}\"/>"));
            }
            if base == "xs:decimal" && r.chance(20) {
                let n = *r.pick(&[0, 1, 2, 3, 30]);
                s.push_str(&format!("<xs:fractionDigits value=\"{n}\"/>"));
            }
        }
        if r.chance(4) {
            // An assertion (XSD 1.1): the plan checks no XPath.
            s.push_str("<xs:assertion test=\"true()\"/>");
        }
        if f.length {
            if r.chance(5) {
                // A bound past what the plan's lengths hold.
                s.push_str("<xs:maxLength value=\"4294967295\"/>");
                return s;
            }
            match r.below(8) {
                0 => s.push_str(&format!("<xs:length value=\"{}\"/>", r.below(5))),
                1 => s.push_str(&format!("<xs:minLength value=\"{}\"/>", r.below(4))),
                2 => s.push_str(&format!("<xs:maxLength value=\"{}\"/>", r.below(5))),
                3 => s.push_str(&format!(
                    "<xs:minLength value=\"{}\"/><xs:maxLength value=\"{}\"/>",
                    r.below(2),
                    2 + r.below(3)
                )),
                _ => {}
            }
        }
        if f.whitespace && r.chance(30) {
            let ws = *r.pick(&["preserve", "replace", "collapse"]);
            s.push_str(&format!("<xs:whiteSpace value=\"{ws}\"/>"));
        }
        if r.chance(25) {
            let n = 1 + r.below(2);
            for _ in 0..n {
                s.push_str(&format!(
                    "<xs:pattern value=\"{}\"/>",
                    esc(r.pick::<&str>(f.patterns))
                ));
            }
        }
        if r.chance(20) {
            for _ in 0..1 + r.below(3) {
                s.push_str(&format!(
                    "<xs:enumeration value=\"{}\"/>",
                    esc(r.pick::<&str>(f.values))
                ));
            }
        }
        s
    }

    /// A schema: a simple type `T` (one or two restriction steps over a
    /// base of `f`), the same as element content, attribute value, element
    /// and attribute value constraints, and as simple content (an extension,
    /// and a restriction of it with facets of its own).
    fn schema(r: &mut Rng, f: &Family, constraints: bool, v11: bool) -> (String, Vec<String>) {
        let base = *r.pick(f.bases);
        let mut bounds = Vec::new();
        let mut s = String::from("<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\">");
        s.push_str(&format!(
            "<xs:simpleType name=\"T1\"><xs:restriction base=\"{base}\">{}</xs:restriction></xs:simpleType>",
            facets(r, f, base, &mut bounds)
        ));
        let t = if r.chance(40) {
            s.push_str(&format!(
                "<xs:simpleType name=\"T2\"><xs:restriction base=\"T1\">{}</xs:restriction></xs:simpleType>",
                facets(r, f, base, &mut bounds)
            ));
            "T2"
        } else {
            "T1"
        };
        // Value constraints, now and then (an `xs:ID` takes none in 1.0, and
        // a literal its type refuses makes the schema invalid).
        let vc = |kind: &str, r: &mut Rng| {
            if !constraints || !r.chance(60) {
                String::new()
            } else {
                format!(" {kind}=\"{}\"", esc(r.pick::<&str>(f.values)))
            }
        };
        let (fixed_e, default_e) = (vc("fixed", r), vc("default", r));
        let (fixed_a, default_a) = (vc("fixed", r), vc("default", r));
        s.push_str(&format!(
            "<xs:complexType name=\"C\"><xs:simpleContent><xs:extension base=\"{t}\">\
             <xs:attribute name=\"a\" type=\"{t}\"/></xs:extension></xs:simpleContent></xs:complexType>\
             <xs:complexType name=\"R\"><xs:simpleContent><xs:restriction base=\"C\">{}</xs:restriction>\
             </xs:simpleContent></xs:complexType>",
            facets(r, f, base, &mut bounds)
        ));
        // XSD 1.0 allows one attribute of type ID per element.
        let more = if base == "xs:ID" && !v11 {
            String::new()
        } else {
            format!(
                "<xs:attribute name=\"af\" type=\"{t}\"{fixed_a}/><xs:attribute name=\"ad\" type=\"{t}\"{default_a}/>"
            )
        };
        s.push_str(&format!(
            "<xs:element name=\"r\"><xs:complexType><xs:choice>\
             <xs:element name=\"v\" type=\"{t}\"/>\
             <xs:element name=\"w\" maxOccurs=\"2\"><xs:complexType><xs:attribute name=\"a\" type=\"{t}\"/>{more}\
             </xs:complexType></xs:element>\
             <xs:element name=\"f\" type=\"{t}\"{fixed_e}/>\
             <xs:element name=\"d\" type=\"{t}\"{default_e}/>\
             <xs:element name=\"c\" type=\"C\"/>\
             <xs:element name=\"s\" type=\"R\"/>\
             <xs:element name=\"l\" maxOccurs=\"3\" type=\"{t}\"/>\
             </xs:choice></xs:complexType></xs:element></xs:schema>"
        ));
        (s, bounds)
    }

    /// Values next to a bound: the bound itself in other spellings, and its
    /// neighbours one unit away at its own scale and one digit finer
    /// (numbers), or one second away and with other time zones (dates and
    /// times).
    fn neighbours(lit: &str) -> Vec<String> {
        let mut out = vec![lit.to_string(), format!(" {lit} ")];
        let t = lit.trim();
        // Numbers.
        let (neg, body) = match t.as_bytes().first() {
            Some(b'-') => (true, &t[1..]),
            Some(b'+') => (false, &t[1..]),
            _ => (false, t),
        };
        let (int, frac) = body.split_once('.').unwrap_or((body, ""));
        if !body.is_empty()
            && int.bytes().all(|b| b.is_ascii_digit())
            && frac.bytes().all(|b| b.is_ascii_digit())
            && int.len() + frac.len() <= 36
            && !(int.is_empty() && frac.is_empty())
        {
            let scale = frac.len() as u32;
            let digits = format!("{int}{frac}");
            let mant: i128 = digits.parse().unwrap_or(0) * if neg { -1 } else { 1 };
            let fmt = |m: i128, sc: u32| {
                let sign = if m < 0 { "-" } else { "" };
                let a = m.unsigned_abs().to_string();
                if sc == 0 {
                    format!("{sign}{a}")
                } else {
                    let a = format!("{a:0>width$}", width = sc as usize + 1);
                    let (i, f) = a.split_at(a.len() - sc as usize);
                    format!("{sign}{i}.{f}")
                }
            };
            for (m, sc) in [
                (mant - 1, scale),
                (mant + 1, scale),
                (mant * 10 - 1, scale + 1),
                (mant * 10 + 1, scale + 1),
                (mant * 10, scale + 1),
                (-mant, scale),
            ] {
                out.push(fmt(m, sc));
            }
            out.push(format!("0{t}"));
            out.push(format!("+{}", t.trim_start_matches(['+', '-'])));
            return out;
        }
        // Dates and times: the seconds or the day one away, other zones.
        let zones = ["", "Z", "+14:00", "-14:00", "+01:00", "-00:00"];
        let (core, _) = match t.rfind(['Z', '+']) {
            Some(i) if t[i..].len() <= 6 => (&t[..i], &t[i..]),
            _ => match t.len().checked_sub(6) {
                Some(i) if t.as_bytes()[i] == b'-' && t[i..].contains(':') => (&t[..i], &t[i..]),
                _ => (t, ""),
            },
        };
        let mut cores = vec![core.to_string()];
        let b = core.as_bytes();
        // The last two digits of the core (seconds, or a day): one away.
        if b.len() >= 2 && b[b.len() - 1].is_ascii_digit() && b[b.len() - 2].is_ascii_digit() {
            let n: u32 = core[core.len() - 2..].parse().unwrap_or(0);
            for m in [n.wrapping_sub(1), n + 1] {
                if m < 100 {
                    cores.push(format!("{}{m:02}", &core[..core.len() - 2]));
                }
            }
        }
        if core.contains(':') {
            cores.push(format!("{core}.5"));
            cores.push(format!("{core}.000001"));
        }
        for c in &cores {
            for z in zones {
                out.push(format!("{c}{z}"));
            }
        }
        out
    }

    fn load(xsd: &str, v11: bool) -> Option<SchemaSet> {
        let b = if v11 {
            SchemaSetBuilder::xsd11()
        } else {
            SchemaSetBuilder::new()
        };
        let b = b.add_bytes(xsd.as_bytes(), "values.xsd").ok()?;
        b.compile().ok().map(|c| c.into_schema_set())
    }

    /// The documents for one value: every position of the schema.
    fn documents(v: &str, other: &str) -> Vec<String> {
        let e = esc(v);
        let o = esc(other);
        // Content as written (only the markup characters escaped), so that
        // literal tabs and line ends reach the value too.
        let raw: String = v
            .chars()
            .map(|c| match c {
                '&' => "&amp;".to_string(),
                '<' => "&lt;".to_string(),
                c => c.to_string(),
            })
            .collect();
        vec![
            format!("<r><v>{raw}</v></r>"),
            format!("<r><v>{e}</v></r>"),
            format!("<r><w a=\"{e}\"/></r>"),
            format!("<r><w af=\"{e}\" ad=\"{e}\"/></r>"),
            "<r><w/></r>".to_string(),
            "<r><w/><w/></r>".to_string(),
            format!("<r><f>{raw}</f></r>"),
            "<r><f/></r>".to_string(),
            format!("<r><d>{raw}</d></r>"),
            "<r><d/></r>".to_string(),
            format!("<r><c a=\"{o}\">{raw}</c></r>"),
            format!("<r><s>{raw}</s></r>"),
            format!("<r><s a=\"{e}\">{e}</s></r>"),
            format!("<r><l>{e}</l><l>{o}</l></r>"),
            format!("<r><l>{e}</l><l>{e}</l></r>"),
            format!("<r><w a=\"{e}\"/><w a=\"{e}\"/></r>"),
            format!("<r><w a=\"{e}\"/><w a=\"{o}\"/></r>"),
            format!("<r><w a=\"{}\"/></r>", raw.replace('"', "&quot;")),
        ]
    }

    pub fn run(count: usize) -> bool {
        let mut failed = false;
        for (v11, seed) in [
            (false, 0xD1B5_4A32_D192_ED03u64),
            (true, 0xD1B5_4A32_D192_ED31),
        ] {
            let t = Instant::now();
            let mut r = Rng(seed);
            let (mut schemas, mut refused, mut docs, mut dynamic_valid) = (0usize, 0, 0, 0);
            let (mut shipped_valid, mut delegating_valid, mut unsound) = (0usize, 0, 0);
            let mut by_family: Vec<(usize, usize, usize)> = vec![(0, 0, 0); FAMILIES.len()];
            while schemas < count {
                let fi = r.below(FAMILIES.len());
                let f = &FAMILIES[fi];
                // A schema refused with value constraints (a literal its type
                // refuses) is tried again without them.
                let saved = r.0;
                let (mut xsd, mut bounds) = schema(&mut r, f, true, v11);
                let mut set = load(&xsd, v11);
                if set.is_none() {
                    refused += 1;
                    r.0 = saved;
                    (xsd, bounds) = schema(&mut r, f, false, v11);
                    set = load(&xsd, v11);
                }
                let Some(set) = set else {
                    refused += 1;
                    continue;
                };
                schemas += 1;
                let (data, _) = plan::build(&set);
                let validator = SchemaValidator::new(&set, ValidationFlags::default());
                let mut scratch = plan::Scratch::new();
                let mut vals: Vec<String> = f.values.iter().map(|v| v.to_string()).collect();
                for b in &bounds {
                    vals.extend(neighbours(b));
                }
                vals.sort();
                vals.dedup();
                for v in &vals {
                    let other = *r.pick(f.values);
                    for d in documents(v, other) {
                        docs += 1;
                        let bytes = d.as_bytes();
                        let dynamic = plan::dynamic_valid(&validator, bytes);
                        let shipped = plan::check(data.as_ref(), bytes, &mut scratch);
                        let mut tier = plan::Delegating::new(&set, &data);
                        let delegating =
                            plan::check_with(data.as_ref(), bytes, &mut scratch, &mut tier);
                        dynamic_valid += usize::from(dynamic);
                        shipped_valid += usize::from(shipped == Verdict::Valid);
                        delegating_valid += usize::from(delegating == Verdict::Valid);
                        let fam = &mut by_family[fi];
                        fam.0 += usize::from(dynamic);
                        fam.1 += usize::from(dynamic && shipped == Verdict::Valid);
                        for (run, verdict) in [("shipped", shipped), ("delegating", delegating)] {
                            if verdict == Verdict::Valid && !dynamic {
                                unsound += 1;
                                fam.2 += 1;
                                if unsound <= 25 {
                                    println!("  UNSOUND [{run}] {d:?}\n    schema: {xsd}");
                                }
                            }
                        }
                    }
                }
            }
            println!(
                "value checks, XSD {}: {schemas} schemas ({refused} refused), {docs} documents, dynamically valid {dynamic_valid}, shipped Valid {shipped_valid}, delegating Valid {delegating_valid}, UNSOUND {unsound}, {:.1} s",
                if v11 { "1.1" } else { "1.0" },
                t.elapsed().as_secs_f64()
            );
            for (f, (dv, sv, u)) in FAMILIES.iter().zip(&by_family) {
                println!(
                    "    {:<24} dynamically valid {dv:>6}, shipped Valid {sv:>6}, UNSOUND {u}",
                    f.bases[0]
                );
            }
            failed |= unsound > 0;
        }
        failed
    }
}

// ---------------------------------------------------------------------------
// T3: seeded byte-level fuzz over the corpus documents
// ---------------------------------------------------------------------------

mod fuzz {
    use super::*;
    use values::Rng;

    /// Bytes a fuzzed byte is drawn from: markup, white space, digits,
    /// letters, signs, and bytes that are not UTF-8 on their own.
    const BYTES: &[u8] =
        b"<>&;\"'=/:!?- \t\r\n0123456789.+eEZTabcxyzAQ#%_\x00\x7f\x80\xa0\xc3\xa9\xe2\xff";

    /// Positions inside character data (between `>` and `<`), where the
    /// values are.
    fn text_positions(d: &[u8]) -> Vec<usize> {
        let mut out = Vec::new();
        let mut inside = false;
        for (i, &b) in d.iter().enumerate() {
            match b {
                b'>' => inside = true,
                b'<' => inside = false,
                _ if inside => out.push(i),
                _ => {}
            }
        }
        out
    }

    fn mutate(r: &mut Rng, d: &[u8], text: &[usize]) -> Vec<u8> {
        let mut m = d.to_vec();
        for _ in 0..1 + r.below(3) {
            if m.is_empty() {
                break;
            }
            let at = if r.below(2) == 0 && !text.is_empty() {
                text[r.below(text.len())].min(m.len() - 1)
            } else {
                r.below(m.len())
            };
            match r.below(7) {
                0 => m[at] = *r.pick(BYTES),
                1 => m.insert(at, *r.pick(BYTES)),
                2 => {
                    let n = (1 + r.below(4)).min(m.len() - at);
                    m.drain(at..at + n);
                }
                3 => {
                    let n = (1 + r.below(64)).min(m.len() - at);
                    let dup: Vec<u8> = m[at..at + n].to_vec();
                    m.splice(at..at, dup);
                }
                4 if at + 1 < m.len() => m.swap(at, at + 1),
                5 if m[at].is_ascii_digit() => m[at] = b'0' + r.below(10) as u8,
                _ => m[at] = *r.pick(BYTES),
            }
        }
        m
    }

    pub fn run(ws: &Path, per_document: usize) -> bool {
        let mut failed = false;
        let mut r = Rng(0xF00D_5EED_1234_ABCD);
        let (mut total, mut dynamic_valid, mut shipped_valid, mut delegating_valid, mut unsound) =
            (0usize, 0, 0, 0, 0);
        let t = Instant::now();
        for case in CASES {
            if case.mutate.is_empty() {
                continue;
            }
            let set = match load(&ws.join(case.xsd)) {
                Ok(s) => s,
                Err(e) => {
                    println!("  SCHEMA REFUSED: {e}");
                    failed = true;
                    continue;
                }
            };
            let (data, _) = plan::build(&set);
            let validator = SchemaValidator::new(&set, ValidationFlags::default());
            let mut scratch = plan::Scratch::new();
            for doc in case.mutate {
                let bytes = std::fs::read(ws.join(doc)).expect("document");
                let text = text_positions(&bytes);
                for _ in 0..per_document {
                    let m = mutate(&mut r, &bytes, &text);
                    total += 1;
                    let dynamic = plan::dynamic_valid(&validator, &m);
                    let shipped = plan::check(data.as_ref(), &m, &mut scratch);
                    let mut tier = plan::Delegating::new(&set, &data);
                    let delegating = plan::check_with(data.as_ref(), &m, &mut scratch, &mut tier);
                    dynamic_valid += usize::from(dynamic);
                    shipped_valid += usize::from(shipped == Verdict::Valid);
                    delegating_valid += usize::from(delegating == Verdict::Valid);
                    for (run, v) in [("shipped", shipped), ("delegating", delegating)] {
                        if v == Verdict::Valid && !dynamic {
                            unsound += 1;
                            if unsound <= 10 {
                                let at =
                                    m.iter().zip(&bytes).position(|(a, b)| a != b).unwrap_or(0);
                                let lo = at.saturating_sub(60);
                                let hi = (at + 60).min(m.len());
                                println!(
                                    "  UNSOUND [{run}] {doc} at {at}: …{}…",
                                    String::from_utf8_lossy(&m[lo..hi])
                                );
                            }
                        }
                    }
                }
            }
        }
        println!(
            "T3: {total} mutants, dynamically valid {dynamic_valid}, shipped Valid {shipped_valid}, delegating Valid {delegating_valid}, UNSOUND {unsound}, {:.1} s",
            t.elapsed().as_secs_f64()
        );
        failed | (unsound > 0)
    }
}
