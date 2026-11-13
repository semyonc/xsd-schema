//! Pattern steps as DFA tables (feature `codegen`, generation time only).
//!
//! A step's patterns are translated with `regex_convert` exactly as the
//! pattern facets of a build without `xsd11` translate them before
//! `Regex::new` (`PatternFacet::compile`), joined as one alternation, and
//! determinized by `regex-automata` over UTF-8 bytes. The DFA is then
//! re-emitted as the crate's own table (a byte-class map, a `state × class`
//! transition array with the dead state 0, an accept flag per state at the
//! end of input), so the interpreter needs no engine.
//!
//! The table accepts a value when the whole value is matched by one of the
//! translated patterns: anchored at the start, every match kind kept
//! (`MatchKind::All`, so no alternative is pruned), and accepted only if a
//! match ends at the end of input. That is a full match of a translated
//! pattern, which implies `Regex::is_match` on it, so the table never accepts
//! what the `regex` engine refuses.

use regex_automata::dfa::{dense, Automaton, StartKind};
use regex_automata::util::primitives::StateID;
use regex_automata::{Anchored, Input, MatchKind};

use crate::regex_convert::{try_convert_xml_pattern, validate_xml_pattern_syntax, ConvertOptions};
use crate::schema::model::XsdVersion;

/// A determinization or table over this size leaves the step without a
/// table.
pub const PATTERN_SIZE_LIMIT: usize = 32 << 20;
/// A DFA with more states leaves the step without a table.
pub const PATTERN_STATE_LIMIT: usize = 1 << 16;

/// A pattern DFA re-emitted as plain tables.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmittedTable {
    /// Byte → class.
    pub classes: [u8; 256],
    /// Number of classes.
    pub stride: usize,
    /// `trans[state * stride + class]`; state 0 is dead.
    pub trans: Vec<u32>,
    /// Accept at end of input, per state.
    pub accept: Vec<bool>,
    /// The start state.
    pub start: u32,
}

impl EmittedTable {
    /// Whether the table accepts `value` (the interpreter's loop, for tests).
    pub fn accepts(&self, value: &[u8]) -> bool {
        let mut s = self.start as usize;
        for &b in value {
            s = self.trans[s * self.stride + self.classes[b as usize] as usize] as usize;
            if s == 0 {
                return false;
            }
        }
        self.accept[s]
    }
}

/// The text `PatternFacet::compile` hands to `Regex::new` in a build without
/// `xsd11` (strict mode), or why it refuses the pattern.
pub fn translation(pattern: &str, version: XsdVersion) -> Result<String, String> {
    if version == XsdVersion::V1_0 {
        validate_xml_pattern_syntax(pattern)?;
    }
    let opts = match version {
        XsdVersion::V1_0 => ConvertOptions::xsd_v1_0(),
        XsdVersion::V1_1 => ConvertOptions::xsd(),
    };
    try_convert_xml_pattern(pattern, opts)
}

/// The joined expression of a step: one translated pattern, or their
/// alternation.
pub fn step_expression(patterns: &[&str], version: XsdVersion) -> Result<String, String> {
    let mut parts = Vec::with_capacity(patterns.len());
    for p in patterns {
        parts.push(translation(p, version)?);
    }
    Ok(match parts.as_slice() {
        [one] => one.clone(),
        _ => parts
            .iter()
            .map(|p| format!("(?:{p})"))
            .collect::<Vec<_>>()
            .join("|"),
    })
}

/// Build the table of a step's patterns, or say why there is none.
pub fn build_table(patterns: &[&str], version: XsdVersion) -> Result<EmittedTable, String> {
    let expr = step_expression(patterns, version)?;
    build_table_from_expression(&expr)
}

/// Build the table of a `regex` expression.
pub fn build_table_from_expression(expr: &str) -> Result<EmittedTable, String> {
    let dfa = dense::Builder::new()
        .configure(
            dense::Config::new()
                .start_kind(StartKind::Anchored)
                .match_kind(MatchKind::All)
                .minimize(false)
                .dfa_size_limit(Some(PATTERN_SIZE_LIMIT))
                .determinize_size_limit(Some(PATTERN_SIZE_LIMIT)),
        )
        .build(expr)
        .map_err(|e| format!("DFA build: {e}"))?;
    let start = dfa
        .start_state_forward(&Input::new("").anchored(Anchored::Yes))
        .map_err(|e| format!("start state: {e}"))?;

    let byte_classes = dfa.byte_classes();
    let mut classes = [0u8; 256];
    let mut stride = 0usize;
    for b in 0..=255u8 {
        let c = byte_classes.get(b);
        classes[b as usize] = c;
        stride = stride.max(c as usize + 1);
    }
    // One byte per class to step the DFA with.
    let mut representative = vec![0u8; stride];
    let mut seen = vec![false; stride];
    for b in 0..=255u8 {
        let c = classes[b as usize] as usize;
        if !seen[c] {
            seen[c] = true;
            representative[c] = b;
        }
    }

    // Renumber the reachable states from 1; 0 is the dead state.
    let mut index = std::collections::HashMap::new();
    let mut order: Vec<StateID> = vec![start];
    index.insert(start, 1u32);
    let mut trans: Vec<u32> = vec![0; stride]; // the dead state's row
    let mut accept: Vec<bool> = vec![false];
    let mut i = 0;
    while i < order.len() {
        if order.len() > PATTERN_STATE_LIMIT {
            return Err(format!("DFA over {PATTERN_STATE_LIMIT} states"));
        }
        let s = order[i];
        accept.push(dfa.is_match_state(dfa.next_eoi_state(s)));
        for &rep in &representative {
            let n = dfa.next_state(s, rep);
            if dfa.is_quit_state(n) {
                return Err("DFA with quit states".to_string());
            }
            let id = if dfa.is_dead_state(n) {
                0
            } else {
                *index.entry(n).or_insert_with(|| {
                    order.push(n);
                    order.len() as u32
                })
            };
            trans.push(id);
        }
        i += 1;
    }
    if trans.len() * 4 > PATTERN_SIZE_LIMIT {
        return Err("table over the size limit".to_string());
    }
    Ok(EmittedTable {
        classes,
        stride,
        trans,
        accept,
        start: 1,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(p: &[&str], v: XsdVersion) -> EmittedTable {
        build_table(p, v).unwrap()
    }

    #[test]
    fn full_match_only() {
        let t = table(&["a|b"], XsdVersion::V1_0);
        assert!(t.accepts(b"a") && t.accepts(b"b"));
        assert!(!t.accepts(b"ab") && !t.accepts(b"ax") && !t.accepts(b"xb") && !t.accepts(b""));
        let t = table(&["[0-9]{2}", "x+"], XsdVersion::V1_0);
        assert!(t.accepts(b"12") && t.accepts(b"xxx"));
        assert!(!t.accepts(b"1") && !t.accepts(b"12x") && !t.accepts(b"x1"));
    }

    #[test]
    fn unicode_over_bytes() {
        let t = table(&["\\p{IsLatin-1Supplement}+"], XsdVersion::V1_0);
        assert!(t.accepts("éü".as_bytes()));
        assert!(!t.accepts(b"e") && !t.accepts("ā".as_bytes()));
        let t = table(&["[a-z-[aeiou]]+"], XsdVersion::V1_1);
        assert!(t.accepts(b"bcd") && !t.accepts(b"bad"));
    }

    #[test]
    fn agrees_with_the_regex_crate() {
        // The table's language is the full-match language of the text the
        // default build gives `Regex::new`.
        let cases: &[(&str, &[&str])] = &[
            (
                "[A-Z]{2}\\d{3}",
                &["AB123", "AB12", "ab123", "AB1234", "AB١٢٣"],
            ),
            ("\\s*x\\S", &[" xa", "\txx", "x", "\u{A0}xa", "xa "]),
            ("(\\i\\c*)?", &["", "a1", "1a", ":-.", "é"]),
            (".{2,3}", &["ab", "abcd", "a\nb", "a\rb", "ébc"]),
        ];
        for (p, values) in cases {
            for v in [XsdVersion::V1_0, XsdVersion::V1_1] {
                let text = translation(p, v).unwrap();
                let re = regex::Regex::new(&text).unwrap();
                let t = table(&[p], v);
                for value in *values {
                    if t.accepts(value.as_bytes()) {
                        assert!(re.is_match(value), "{p} {v:?} {value:?}");
                    }
                    assert_eq!(
                        t.accepts(value.as_bytes()),
                        re.is_match(value),
                        "{p} {v:?} {value:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn refused_translations_have_no_table() {
        assert!(build_table(&["[]"], XsdVersion::V1_1).is_err());
        assert!(build_table(&["\\p{IsNoSuchBlock}"], XsdVersion::V1_0).is_err());
        assert!(build_table(&["[a-c-1]"], XsdVersion::V1_0).is_err());
    }
}
