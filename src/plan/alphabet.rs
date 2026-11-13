//! Trusted alphabets of pattern atoms.
//!
//! The plan's pattern tables are built from the `regex_convert` translation
//! of a pattern, which is the language the `regex` crate checks in a build
//! without `xsd11`. A build with `xsd11` checks patterns with regexml, and
//! the two engines do not agree on every atom: whether a string is in a
//! pattern's language depends on its atoms only through the membership of the
//! string's characters, so where the two engines give every atom of a pattern
//! the same membership over an alphabet, they agree on every string over it.
//! Each atom kind has a **trusted alphabet**, the code points on which the
//! engines are known to agree for it; a pattern step's alphabet is the
//! intersection over its atoms, and a value with a character outside it
//! answers `Unknown`.
//!
//! The sets are checked-in constants per atom kind and XSD version. A test
//! under `xsd11` (T8) compares the membership of every code point of each
//! atom kind between the two engines and fails if a disagreement lies inside
//! the kind's alphabet, so an update of either engine that moves a set fails
//! the gate.

use crate::schema::model::XsdVersion;

/// An atom kind of an XSD regular expression, as far as the trusted alphabet
/// goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AtomKind {
    /// Literal characters, escaped single characters, ranges and classes of
    /// them: the same code points on both sides.
    Literal,
    /// A block escape `\p{IsX}` / `\P{IsX}` whose name the block table
    /// knows: written out as code-point ranges on the translation side, the
    /// same table on regexml's.
    Block,
    /// XSD 1.0 `\P{IsPrivateUse}` inside a class: the translation writes out
    /// the 1.0 block (the BMP's private use area), but the regexml rewrite
    /// leaves a negated escape inside a class to regexml, whose block also
    /// holds planes 15 and 16 (found by T8).
    PrivateUse10Negated,
    /// `.`: every character but LF in `regex`, but LF and CR in XSD.
    Dot,
    /// `\s`, `\S`: Unicode `White_Space` in `regex`, four characters in XSD.
    Space,
    /// `\i`, `\I`, `\c`, `\C`: ASCII classes in the translation, the XML
    /// name-character tables in regexml.
    NameChar,
    /// XSD 1.0 category escapes the translation and the regexml rewrite both
    /// write out as the same Unicode 3.0 ranges: `\d`, `\w`, `\W`, `\D`
    /// outside a class, `\p{X}` and `\P{X}` (not `\P{X}` inside a class).
    Category10Expanded,
    /// XSD 1.0 category escapes both sides pass to their engine's own
    /// Unicode data: `\D` and `\P{X}` inside a class, and a category name
    /// the expansion does not know.
    Category10Native,
    /// XSD 1.1 `\d`, `\D`.
    Digit11,
    /// XSD 1.1 `\w`, `\W` (`regex` has its own `\w`).
    Word11,
    /// XSD 1.1 `\p{X}`, `\P{X}` for a general category `X`.
    Category11,
}

/// A checked-in trusted alphabet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrustedAlphabet {
    /// ASCII code points 0–63.
    pub ascii_lo: u64,
    /// ASCII code points 64–127.
    pub ascii_hi: u64,
    /// Whether code points past ASCII are trusted.
    pub non_ascii: bool,
    /// Untrusted code points past ASCII (inclusive ranges, sorted).
    pub excluded: &'static [(u32, u32)],
}

const fn ascii_without(lo: u64, hi: u64, chars: &[u8]) -> (u64, u64) {
    let (mut lo, mut hi) = (lo, hi);
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c < 64 {
            lo &= !(1u64 << c);
        } else {
            hi &= !(1u64 << (c - 64));
        }
        i += 1;
    }
    (lo, hi)
}

const fn alphabet(
    without: &[u8],
    non_ascii: bool,
    excluded: &'static [(u32, u32)],
) -> TrustedAlphabet {
    let (ascii_lo, ascii_hi) = ascii_without(u64::MAX, u64::MAX, without);
    TrustedAlphabet {
        ascii_lo,
        ascii_hi,
        non_ascii,
        excluded,
    }
}

/// Every code point.
pub const EVERYTHING: TrustedAlphabet = alphabet(&[], true, &[]);

/// `.`: everything but CR.
pub const DOT: TrustedAlphabet = alphabet(b"\r", true, &[]);

/// `\s`, `\S`: everything but the `White_Space` characters XSD's `\s` leaves
/// out.
pub const SPACE: TrustedAlphabet = alphabet(
    b"\x0B\x0C",
    true,
    &[
        (0x85, 0x85),
        (0xA0, 0xA0),
        (0x1680, 0x1680),
        (0x2000, 0x200A),
        (0x2028, 0x2029),
        (0x202F, 0x202F),
        (0x205F, 0x205F),
        (0x3000, 0x3000),
    ],
);

/// ASCII only.
pub const ASCII: TrustedAlphabet = alphabet(&[], false, &[]);

/// XSD's `\w` is every character but punctuation, separators and others, so
/// it holds the ASCII symbols and not `_`; `regex`'s holds `_` and no symbol.
const WORD_DIFFERENCES: &[u8] = b"$+<=>^_`|~";

/// XSD 1.1 `\w`, `\W`: ASCII without the characters where the two `\w`
/// differ.
pub const WORD11: TrustedAlphabet = alphabet(WORD_DIFFERENCES, false, &[]);

/// XSD 1.0 native `\D` / `\P{X}` inside a class: ASCII.
pub const CATEGORY10_NATIVE: TrustedAlphabet = ASCII;

/// The BMP: every code point below U+10000.
pub const BMP: TrustedAlphabet = alphabet(&[], true, &[(0x10000, 0x10FFFF)]);

/// The checked-in alphabet of an atom kind.
pub fn trusted(kind: AtomKind) -> TrustedAlphabet {
    match kind {
        AtomKind::Literal | AtomKind::Block | AtomKind::Category10Expanded => EVERYTHING,
        AtomKind::PrivateUse10Negated => BMP,
        AtomKind::Dot => DOT,
        AtomKind::Space => SPACE,
        AtomKind::NameChar => ASCII,
        AtomKind::Category10Native => CATEGORY10_NATIVE,
        AtomKind::Digit11 | AtomKind::Category11 => ASCII,
        AtomKind::Word11 => WORD11,
    }
}

/// An alphabet as the builder composes it: the intersection of atom
/// alphabets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComposedAlphabet {
    /// ASCII 0–63.
    pub ascii_lo: u64,
    /// ASCII 64–127.
    pub ascii_hi: u64,
    /// Past ASCII trusted.
    pub non_ascii: bool,
    /// Untrusted ranges past ASCII, sorted and merged.
    pub excluded: Vec<(u32, u32)>,
}

impl ComposedAlphabet {
    /// Every code point.
    pub fn everything() -> Self {
        ComposedAlphabet {
            ascii_lo: u64::MAX,
            ascii_hi: u64::MAX,
            non_ascii: true,
            excluded: Vec::new(),
        }
    }

    /// Intersect with an atom's alphabet.
    pub fn intersect(&mut self, a: &TrustedAlphabet) {
        self.ascii_lo &= a.ascii_lo;
        self.ascii_hi &= a.ascii_hi;
        self.non_ascii &= a.non_ascii;
        if !self.non_ascii {
            self.excluded.clear();
            return;
        }
        self.excluded.extend_from_slice(a.excluded);
        self.excluded.sort_unstable();
        let mut merged: Vec<(u32, u32)> = Vec::with_capacity(self.excluded.len());
        for &(lo, hi) in &self.excluded {
            match merged.last_mut() {
                Some(last) if lo <= last.1.saturating_add(1) => last.1 = last.1.max(hi),
                _ => merged.push((lo, hi)),
            }
        }
        self.excluded = merged;
    }

    /// Whether `c` is in the alphabet.
    pub fn contains(&self, c: char) -> bool {
        let c = c as u32;
        if c < 64 {
            self.ascii_lo & (1 << c) != 0
        } else if c < 128 {
            self.ascii_hi & (1 << (c - 64)) != 0
        } else {
            self.non_ascii
                && self
                    .excluded
                    .binary_search_by(|&(lo, hi)| {
                        if hi < c {
                            std::cmp::Ordering::Less
                        } else if lo > c {
                            std::cmp::Ordering::Greater
                        } else {
                            std::cmp::Ordering::Equal
                        }
                    })
                    .is_err()
        }
    }
}

/// Why a pattern has no trusted alphabet (it answers `Unknown` for every
/// value).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refused {
    /// A block escape with a name the block table does not know.
    UnknownBlock(String),
    /// A construct the translation and regexml may read differently: `^` or
    /// `$` outside a class, `&&`, `~~` or `--` inside one, a nested class
    /// that is no subtraction, an escape outside XSD's, `(?`, a property
    /// escape without its braces.
    Construct(String),
}

/// The atom kinds of an XSD pattern, in the order they occur (with
/// repetitions), or why the pattern is refused.
///
/// This reads the pattern as written (XSD syntax), not its translation, and
/// classifies every atom by the rule the translation and the regexml rewrite
/// apply to it (`regex_convert`: `convert_property_escape`,
/// `expand_xsd10_class_escape`, `try_expand_category`).
pub fn classify(pattern: &str, version: XsdVersion) -> Result<Vec<AtomKind>, Refused> {
    let refuse = |what: &str| Err(Refused::Construct(format!("{what} in /{pattern}/")));
    let mut kinds = Vec::new();
    let chars: Vec<char> = pattern.chars().collect();
    let mut depth = 0usize;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let in_class = depth > 0;
        match c {
            '\\' => {
                let Some(&next) = chars.get(i + 1) else {
                    return refuse("a trailing backslash");
                };
                i += 2;
                match next {
                    'n' | 'r' | 't' | '\\' | '|' | '.' | '?' | '*' | '+' | '(' | ')' | '{'
                    | '}' | '-' | '[' | ']' | '^' => kinds.push(AtomKind::Literal),
                    's' | 'S' => kinds.push(AtomKind::Space),
                    'i' | 'I' | 'c' | 'C' => kinds.push(AtomKind::NameChar),
                    'd' | 'D' | 'w' | 'W' => kinds.push(match version {
                        XsdVersion::V1_0 if in_class && next == 'D' => AtomKind::Category10Native,
                        XsdVersion::V1_0 => AtomKind::Category10Expanded,
                        XsdVersion::V1_1 if matches!(next, 'd' | 'D') => AtomKind::Digit11,
                        XsdVersion::V1_1 => AtomKind::Word11,
                    }),
                    'p' | 'P' => {
                        if chars.get(i) != Some(&'{') {
                            return refuse("a property escape without braces");
                        }
                        let Some(close) = chars[i..].iter().position(|&c| c == '}') else {
                            return refuse("an unclosed property escape");
                        };
                        let name: String = chars[i + 1..i + close].iter().collect();
                        i += close + 1;
                        let negated = next == 'P';
                        kinds.push(property_kind(&name, negated, in_class, version)?);
                    }
                    _ => return refuse("an escape outside XSD's"),
                }
                continue;
            }
            '[' => {
                // A class opens at depth 0, or as the subtrahend of a
                // subtraction (`-[`) inside one.
                if in_class && (i == 0 || chars[i - 1] != '-') {
                    return refuse("a nested class that is no subtraction");
                }
                depth += 1;
                if chars.get(i + 1) == Some(&'^') {
                    i += 1;
                }
            }
            ']' if in_class => depth -= 1,
            '-' | '&' | '~' if in_class && chars.get(i + 1) == Some(&c) => {
                return refuse("a doubled '-', '&' or '~' inside a class");
            }
            '.' if !in_class => kinds.push(AtomKind::Dot),
            '^' | '$' if !in_class => return refuse("'^' or '$' outside a class"),
            '(' if chars.get(i + 1) == Some(&'?') => return refuse("'(?'"),
            _ => kinds.push(AtomKind::Literal),
        }
        i += 1;
    }
    Ok(kinds)
}

/// The kind of `\p{name}` / `\P{name}`.
fn property_kind(
    name: &str,
    negated: bool,
    in_class: bool,
    version: XsdVersion,
) -> Result<AtomKind, Refused> {
    if let Some(block) = name.strip_prefix("Is").filter(|b| !b.is_empty()) {
        return match crate::regex_xsd_blocks::block_ranges(block, version) {
            Some(_)
                if version == XsdVersion::V1_0 && block == "PrivateUse" && negated && in_class =>
            {
                Ok(AtomKind::PrivateUse10Negated)
            }
            Some(_) => Ok(AtomKind::Block),
            None => Err(Refused::UnknownBlock(name.to_string())),
        };
    }
    if !is_category_name(name) {
        return Err(Refused::Construct(format!(
            "\\p{{{name}}}: no XSD general category"
        )));
    }
    Ok(match version {
        XsdVersion::V1_0 => {
            let expanded = crate::regex_xsd_unicode::expand_xsd_category_body(name)
                .is_some_and(|b| !b.is_empty());
            if expanded && !(in_class && negated) {
                AtomKind::Category10Expanded
            } else {
                AtomKind::Category10Native
            }
        }
        XsdVersion::V1_1 => AtomKind::Category11,
    })
}

/// The general category names of XSD (Datatypes §G.4.2.3).
pub const CATEGORY_NAMES: &[&str] = &[
    "L", "Lu", "Ll", "Lt", "Lm", "Lo", "M", "Mn", "Mc", "Me", "N", "Nd", "Nl", "No", "P", "Pc",
    "Pd", "Ps", "Pe", "Pi", "Pf", "Po", "Z", "Zs", "Zl", "Zp", "S", "Sm", "Sc", "Sk", "So", "C",
    "Cc", "Cf", "Co", "Cn",
];

fn is_category_name(name: &str) -> bool {
    CATEGORY_NAMES.contains(&name)
}

/// The trusted alphabet of a pattern step: the intersection over the atoms
/// of all its patterns.
pub fn step_alphabet(patterns: &[&str], version: XsdVersion) -> Result<ComposedAlphabet, Refused> {
    let mut a = ComposedAlphabet::everything();
    for p in patterns {
        for kind in classify(p, version)? {
            a.intersect(&trusted(kind));
        }
    }
    Ok(a)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literals_and_ranges_trust_everything() {
        let a = step_alphabet(&["[A-Z]{2}[0-9]+", "é|ü"], XsdVersion::V1_0).unwrap();
        assert_eq!(a, ComposedAlphabet::everything());
    }

    #[test]
    fn atoms_intersect() {
        let a = step_alphabet(&["\\s.\\i"], XsdVersion::V1_1).unwrap();
        assert!(a.contains('a') && a.contains(' '));
        assert!(!a.contains('\r') && !a.contains('\x0B') && !a.contains('é'));
        let w = step_alphabet(&["\\w+"], XsdVersion::V1_1).unwrap();
        assert!(w.contains('a') && !w.contains('_') && !w.contains('$') && !w.contains('ä'));
        let s = step_alphabet(&["\\s"], XsdVersion::V1_0).unwrap();
        assert!(s.contains('ä') && !s.contains('\u{A0}') && !s.contains('\u{2005}'));
    }

    #[test]
    fn categories_by_version_and_position() {
        let k = |p: &str, v| classify(p, v).unwrap();
        assert_eq!(
            k("\\p{Lu}", XsdVersion::V1_0),
            [AtomKind::Category10Expanded]
        );
        assert_eq!(
            k("[\\P{Lu}]", XsdVersion::V1_0),
            [AtomKind::Category10Native]
        );
        assert_eq!(k("[\\D]", XsdVersion::V1_0), [AtomKind::Category10Native]);
        assert_eq!(k("[\\W]", XsdVersion::V1_0), [AtomKind::Category10Expanded]);
        assert_eq!(k("\\p{Lu}", XsdVersion::V1_1), [AtomKind::Category11]);
        assert_eq!(k("\\p{IsBasicLatin}", XsdVersion::V1_1), [AtomKind::Block]);
    }

    #[test]
    fn refusals() {
        for p in [
            "a$",
            "^a",
            "[a&&b]",
            "[a--b]",
            "[a~~b]",
            "[a[b]]",
            "\\x41",
            "(?:a)",
            "\\p{IsNoSuchBlock}",
            "\\pL",
            "\\p{Greek}",
        ] {
            assert!(classify(p, XsdVersion::V1_1).is_err(), "{p}");
        }
        assert!(classify("[a-z-[aeiou]]", XsdVersion::V1_1).is_ok());
        assert!(classify("[$^]", XsdVersion::V1_0).is_ok());
    }
}
