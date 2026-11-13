//! T8, atom level: for every atom kind and both XSD versions, the
//! membership of every XML character in the translation's class (the `regex`
//! engine of a build without `xsd11`) against regexml's. A character where
//! they differ must lie outside the atom kind's checked-in trusted alphabet.
//!
//! Runs every character of the XML `Char` production (values hold no
//! other) in a release build or with `PLAN_T8_FULL=1`; a debug build without
//! it checks ASCII, every 97th character and the edges of the alphabets.

use super::alphabet::{classify, step_alphabet, AtomKind, CATEGORY_NAMES};
use super::pattern::translation;
use crate::schema::model::{RegexCompat, XsdVersion};
use crate::types::facets::PatternFacet;

fn xml_chars(full: bool) -> Vec<char> {
    let ranges = [
        (0x9u32, 0xAu32),
        (0xD, 0xD),
        (0x20, 0xD7FF),
        (0xE000, 0xFFFD),
        (0x10000, 0x10FFFF),
    ];
    let mut out = Vec::new();
    for (lo, hi) in ranges {
        for c in lo..=hi {
            let edge = c < 0x100
                || c % 97 == 0
                || matches!(
                    c,
                    0x85 | 0xA0 | 0x1680 | 0x2000
                        ..=0x200A | 0x2028 | 0x2029 | 0x202F | 0x205F | 0x3000
                )
                || c == lo
                || c == hi;
            if full || edge {
                out.extend(char::from_u32(c));
            }
        }
    }
    out
}

/// The atoms checked, each as a pattern of one atom (a class of one escape
/// counts as that escape inside a class).
fn atoms(version: XsdVersion) -> Vec<String> {
    let mut v: Vec<String> = [
        ".",
        "\\s",
        "\\S",
        "[\\s]",
        "[^\\s]",
        "[\\S]",
        "\\i",
        "\\I",
        "\\c",
        "\\C",
        "[\\i]",
        "[^\\c]",
        "\\d",
        "\\D",
        "\\w",
        "\\W",
        "[\\d]",
        "[\\D]",
        "[\\w]",
        "[\\W]",
        "[^\\w]",
        "[^\\d]",
        "[a-zA-Z0-9]",
        "[^a-c]",
        "[\\-\\[\\]\\^]",
        "[Ā-ſ]",
        "[𐀀-𐃿]",
        "[^𐀀-𐃿]",
        "\\p{IsBasicLatin}",
        "\\P{IsBasicLatin}",
        "[\\P{IsBasicLatin}]",
        "\\p{IsLatin-1Supplement}",
        "\\p{IsPrivateUse}",
        "[\\P{IsPrivateUse}]",
        "\\p{IsHighSurrogates}",
        "\\P{IsHighSurrogates}",
        "\\p{IsCJKUnifiedIdeographs}",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    v.push(match version {
        XsdVersion::V1_0 => "\\p{IsGreek}".into(),
        XsdVersion::V1_1 => "\\p{IsGreekandCoptic}".into(),
    });
    for name in CATEGORY_NAMES {
        v.push(format!("\\p{{{name}}}"));
        v.push(format!("\\P{{{name}}}"));
        v.push(format!("[\\p{{{name}}}]"));
        v.push(format!("[\\P{{{name}}}]"));
    }
    v
}

struct Outcome {
    pattern: String,
    kinds: Vec<AtomKind>,
    checked: usize,
    differ: usize,
    differ_inside: Vec<char>,
}

fn sweep(pattern: &str, version: XsdVersion, chars: &[char]) -> Result<Outcome, String> {
    let kinds = classify(pattern, version).map_err(|e| format!("{pattern}: {e:?}"))?;
    let alphabet = step_alphabet(&[pattern], version).map_err(|e| format!("{e:?}"))?;
    let text = translation(pattern, version)?;
    let engine2 = regex::Regex::new(&text).map_err(|e| format!("{pattern}: {e}"))?;
    let engine1 = PatternFacet::new(pattern.to_string(), None, version, RegexCompat::Strict)
        .map_err(|e| format!("{pattern}: {e}"))?;
    let mut out = Outcome {
        pattern: pattern.to_string(),
        kinds,
        checked: 0,
        differ: 0,
        differ_inside: Vec::new(),
    };
    let mut buf = [0u8; 4];
    for &c in chars {
        let s = c.encode_utf8(&mut buf);
        out.checked += 1;
        if engine2.is_match(s) != engine1.matches(s) {
            out.differ += 1;
            if alphabet.contains(c) {
                out.differ_inside.push(c);
            }
        }
    }
    Ok(out)
}

#[test]
fn atom_membership_agrees_on_the_trusted_alphabets() {
    let full = !cfg!(debug_assertions) || std::env::var_os("PLAN_T8_FULL").is_some();
    let chars = xml_chars(full);
    let mut failures = Vec::new();
    let mut lines = Vec::new();
    for version in [XsdVersion::V1_0, XsdVersion::V1_1] {
        for p in atoms(version) {
            match sweep(&p, version, &chars) {
                Ok(o) => {
                    lines.push(format!(
                        "{version:?} /{}/ {:?}: {} checked, {} differ, {} inside the alphabet",
                        o.pattern,
                        o.kinds,
                        o.checked,
                        o.differ,
                        o.differ_inside.len()
                    ));
                    if !o.differ_inside.is_empty() {
                        let shown: Vec<String> = o
                            .differ_inside
                            .iter()
                            .take(12)
                            .map(|c| format!("U+{:04X}", *c as u32))
                            .collect();
                        failures.push(format!("{version:?} /{}/: {}", o.pattern, shown.join(" ")));
                    }
                }
                Err(e) => failures.push(format!("{version:?}: {e}")),
            }
        }
    }
    if let Some(path) = std::env::var_os("PLAN_T8_REPORT") {
        std::fs::write(path, lines.join("\n") + "\n").expect("report");
    }
    assert!(
        failures.is_empty(),
        "disagreements inside a trusted alphabet:\n{}",
        failures.join("\n")
    );
}

/// The block table of the translation equals regexml's: checked over every
/// block by `regex_xsd_blocks::tests::blocks_agree_with_regexml`; here, the
/// two engines agree on a block's first and last character and the
/// characters just outside it.
#[test]
fn block_edges_agree() {
    for version in [XsdVersion::V1_0, XsdVersion::V1_1] {
        for (name, lo, hi) in [
            ("BasicLatin", 0u32, 0x7Fu32),
            ("Latin-1Supplement", 0x80, 0xFF),
            ("CJKUnifiedIdeographs", 0x4E00, 0x9FFF),
        ] {
            let p = format!("\\p{{Is{name}}}");
            let chars: Vec<char> = [lo.saturating_sub(1), lo, hi, hi + 1]
                .iter()
                .filter_map(|&c| char::from_u32(c))
                .collect();
            let o = sweep(&p, version, &chars).unwrap();
            assert_eq!(o.differ, 0, "{version:?} {p}");
        }
    }
}
