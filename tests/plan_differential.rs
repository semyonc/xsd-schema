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
//! Order, CreditNote): every valid document of the probe's corpus must be
//! answered `Valid` by the delegating tier, and no mutant of the mutated ones
//! may be answered `Valid`, as shipped or delegating, unless the dynamic
//! validator accepts it.
//!
//! **T8, pattern level (`--patterns`, needs `xsd11`).** Every pattern facet
//! of the W3C schemas, GAEB, the WSDL corpus and UBL, as a one-pattern step
//! under both XSD versions: wherever the plan's table accepts a value inside
//! the step's trusted alphabet, regexml must accept it too; and the table
//! must never accept what the `regex` engine refuses on the same
//! translation (the tripwire). Values: the enumerations and other literals
//! of the same schema, generated strings the table accepts (walks of the
//! DFA, with their characters moved to neighbouring code points, dropped and
//! repeated) and fixed boundary strings.
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
    let workspace = args
        .iter()
        .position(|a| a == "--workspace")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from);
    let Some(ws) = workspace else {
        println!("plan_differential: no --workspace given; nothing to run");
        return;
    };
    let only = args
        .iter()
        .position(|a| a == "--only")
        .and_then(|i| args.get(i + 1))
        .cloned();
    let mut failed = false;
    if args.iter().any(|a| a == "--mutations") {
        failed |= mutations(&ws, only.as_deref());
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

// The probe's document scanner and mutation generator
// (`validation-bench/codegen-probe/src/bin/real.rs`), with the operators the
// design adds (§8 of the plan's design notes).

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
}
