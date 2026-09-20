//! Reuse of compiled regular expressions across the calls of one evaluation run.
//!
//! [`matches`](crate::xpath::functions::regex::matches),
//! [`replace`](crate::xpath::functions::regex::replace) and
//! [`tokenize`](crate::xpath::functions::regex::tokenize) each turn their
//! `$pattern` and `$flags` arguments into a compiled program before they do any
//! work. Compiling is not cheap: the backend parses the pattern, builds the
//! program, and for a pattern that names a Unicode category or a shorthand that
//! stands for one (`\p{…}`, `\d`, `\w`, `\s`) expands that category's code-point
//! set. On a short input string the compilation costs far more than the match.
//!
//! That would not matter if a pattern were compiled once per expression, but the
//! shape that matters most is
//!
//! ```text
//!     $items[matches(., '\p{Ll}')]
//! ```
//!
//! where **one** compiled expression is evaluated once and its predicate calls
//! `matches` once per item, with a pattern that is a literal and cannot change.
//! Compilation is then the whole cost of the filter, and it is paid again for
//! every item.
//!
//! This module keeps the compiled program of each distinct `(pattern, flags)`
//! pair for the duration of one **run**, so that every later call with that pair
//! costs a key comparison and nothing else.
//!
//! # Where the cache lives, and how long
//!
//! In [`DynamicContext`](crate::xpath::context::DynamicContext), which is created
//! by [`XPathEvaluator::run`](crate::xpath::XPathEvaluator::run) and dropped when
//! that run returns: one cache per run, never in the compiled expression (which
//! is shared and immutable) and never global — so there is no lock on the path of
//! a per-item predicate, nothing outlives the run, and two runs cannot observe
//! each other. It is an empty `Vec` until the run's first regular-expression
//! call, so an expression that uses none pays one pointer-sized field and no
//! allocation.
//!
//! # Why one compiled program can serve every later call
//!
//! `regexml::Regex` owns a `ReProgram` of plain immutable data; every piece of
//! mutable matching state lives in the `ReMatcher` that each `is_match`,
//! `replace_all` or `tokenize` call builds for itself from `&ReProgram`. Reusing
//! one `Regex` therefore produces exactly what a freshly compiled one produces,
//! for any of the three functions in any order.
//!
//! It is handed out as a plain `&Regex`, not as an `Rc`: `Regex` is not `Clone`,
//! so it has to be shared one way or the other, and a reference costs no
//! allocation at all — which matters because the *first* call for a key is the
//! one the cache can only make slower, and every allocation it adds there shows
//! up in an expression that calls `matches` exactly once. The price is that the
//! borrow of the context lasts as long as the regex is in use; the three
//! functions want nothing else from the context by then.
//!
//! The "does this pattern match the zero-length string" answer that `fn:replace`
//! and `fn:tokenize` raise FORX0003 from is a field of the compiled `Regex`,
//! computed once when it is built; reusing the program reuses that answer too.
//!
//! # Failures are cached as well
//!
//! What the compile step produces — an invalid-flags or invalid-pattern error —
//! is a pure function of `(pattern, flags)`: both errors are built from those two
//! strings and nothing else is read. Storing the error and replaying it makes the
//! first and the thousandth call identical *by construction*, instead of leaving
//! that to two code paths agreeing with each other.
//!
//! # The bound
//!
//! A pattern need not be a literal —
//!
//! ```text
//!     $rows[matches(@value, @pattern)]
//! ```
//!
//! computes a new one per item, and an unbounded cache would then hold every
//! pattern the input contains. [`MAX_ENTRIES`] caps the table and the least
//! recently used entry is dropped to make room, so such a run keeps at most
//! [`MAX_ENTRIES`] programs alive and a run that walks through many patterns
//! before settling on one still ends up with the one it uses resident.

use regexml::Regex;

use crate::xpath::error::XPathError;

/// How many distinct `(pattern, flags)` pairs one run keeps compiled.
///
/// A regular-expression argument is nearly always a literal, and an expression
/// with more than a handful of distinct ones is already unusual, so 32 is far
/// above what a real expression asks for — the same reasoning, and the same
/// number, as [`compare_cache::MAX_TRACKED_NODES`](crate::xpath::compare_cache).
/// It is chosen against the memory a compiled program costs: measured with a
/// counting allocator, 32 plain patterns hold ~26 KB and 32 patterns each
/// carrying a Unicode category class ~0.4 MB, which is the worst case this bound
/// admits, per run, for the run's lifetime.
pub(crate) const MAX_ENTRIES: usize = 32;

/// What one `(pattern, flags)` pair compiled to: a program, or the error
/// compiling it raised — which is the same error every time (see the module
/// documentation).
type Compiled = Result<Regex, XPathError>;

/// One cached compilation, with the key it was compiled from.
struct Entry {
    pattern: String,
    flags: String,
    compiled: Compiled,
    /// The reading of [`RegexCache::clock`] at this entry's last use, which is
    /// what makes "least recently used" answerable without moving entries about.
    used: u64,
}

/// The compiled regular expressions of one evaluation run.
///
/// Lives in [`DynamicContext`](crate::xpath::context::DynamicContext) and dies
/// with it. Empty and allocation-free until the run's first regular-expression
/// call.
#[derive(Default)]
pub(crate) struct RegexCache {
    /// In the order the keys were first seen. Recency is a number on the entry
    /// rather than its position, so a hit writes one `u64` and an eviction
    /// overwrites one slot: entries are never shifted, which matters because a
    /// compiled program is a large value to move and the run that evicts most is
    /// the one that can least afford the work.
    entries: Vec<Entry>,
    /// Ticks once per call, so that a larger `used` means "more recently".
    /// A `u64` cannot wrap within a run: one tick per regular-expression call.
    clock: u64,
    /// How many calls of this run were answered from the cache, and how many had
    /// to compile. Test-only, so that a test can tell "the behaviour is still
    /// right" from "the cache silently stopped engaging"; the shipped path pays
    /// nothing for them.
    #[cfg(test)]
    hits: u32,
    #[cfg(test)]
    compiles: u32,
}

impl RegexCache {
    /// The program for `(pattern, flags)`, compiling it with `compile` the first
    /// time this run asks for it.
    ///
    /// `compile` is called at most once per distinct key per run. Whatever it
    /// produces — a program or an error — is what every later call with that key
    /// gets back.
    pub(crate) fn get_or_compile(
        &mut self,
        pattern: &str,
        flags: &str,
        compile: impl FnOnce() -> Result<Regex, XPathError>,
    ) -> Result<&Regex, XPathError> {
        let now = self.clock;
        self.clock += 1;

        // One pass answers both questions: where this key is, and — if it is not
        // here at all — which entry is the one to give up. The victim is only
        // ever read when the loop ran to the end, so the early exit cannot leave
        // it half-computed.
        let mut found = None;
        let mut victim = 0usize;
        let mut victim_used = u64::MAX;
        for (index, entry) in self.entries.iter().enumerate() {
            if entry.pattern == pattern && entry.flags == flags {
                found = Some(index);
                break;
            }
            if entry.used < victim_used {
                victim_used = entry.used;
                victim = index;
            }
        }

        let index = match found {
            Some(index) => {
                self.entries[index].used = now;
                #[cfg(test)]
                {
                    self.hits += 1;
                }
                index
            }
            None => {
                #[cfg(test)]
                {
                    self.compiles += 1;
                }
                let compiled = compile();
                if self.entries.len() < MAX_ENTRIES {
                    self.entries.push(Entry {
                        pattern: pattern.to_owned(),
                        flags: flags.to_owned(),
                        compiled,
                        used: now,
                    });
                    self.entries.len() - 1
                } else {
                    // The least recently used key makes room. A run that keeps
                    // computing fresh patterns therefore holds at most
                    // MAX_ENTRIES programs, whatever its input — and, once the
                    // table is full, stops allocating key strings altogether,
                    // because the evicted entry's buffers are written over
                    // instead of dropped and replaced.
                    let slot = &mut self.entries[victim];
                    slot.pattern.clear();
                    slot.pattern.push_str(pattern);
                    slot.flags.clear();
                    slot.flags.push_str(flags);
                    slot.compiled = compiled;
                    slot.used = now;
                    victim
                }
            }
        };

        // One borrow, taken after both arms are done with `&mut self`.
        match &self.entries[index].compiled {
            Ok(regex) => Ok(regex),
            // Only a failing key pays for a clone, and it pays it in place of a
            // second compile.
            Err(err) => Err(err.clone()),
        }
    }

    /// How many calls of this run were answered from the cache.
    #[cfg(test)]
    pub(crate) fn hits(&self) -> u32 {
        self.hits
    }

    /// How many distinct keys this run compiled.
    #[cfg(test)]
    pub(crate) fn compiles(&self) -> u32 {
        self.compiles
    }

    /// How many entries are resident. Never more than [`MAX_ENTRIES`].
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    /// The resident keys, in the order the entries are stored.
    #[cfg(test)]
    pub(crate) fn keys(&self) -> Vec<(&str, &str)> {
        self.entries
            .iter()
            .map(|entry| (entry.pattern.as_str(), entry.flags.as_str()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real compilation of a pattern that is known to be valid — what the
    /// production closure does, minus the dialect pre-pass these tests are not
    /// about.
    fn ok(pattern: &str) -> Result<Regex, XPathError> {
        Regex::xpath(pattern, "").map_err(|_| XPathError::invalid_regex_pattern(pattern))
    }

    #[test]
    fn a_repeated_key_compiles_once() {
        let mut cache = RegexCache::default();
        let mut compiles = 0;
        for _ in 0..50 {
            let result = cache.get_or_compile("a+", "", || {
                compiles += 1;
                ok("a+")
            });
            assert!(result.unwrap().is_match("aaa"));
        }
        assert_eq!(compiles, 1);
        assert_eq!(cache.compiles(), 1);
        assert_eq!(cache.hits(), 49);
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn flags_are_part_of_the_key() {
        let mut cache = RegexCache::default();
        let mut compiles = 0;
        for flags in ["", "i", "", "i"] {
            let _ = cache.get_or_compile("a", flags, || {
                compiles += 1;
                Regex::xpath("a", flags).map_err(|_| XPathError::invalid_regex_pattern("a"))
            });
        }
        assert_eq!(compiles, 2);
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn a_failure_is_replayed_unchanged() {
        let mut cache = RegexCache::default();
        let mut compiles = 0;
        let mut seen = Vec::new();
        for _ in 0..5 {
            let result = cache.get_or_compile("[", "", || {
                compiles += 1;
                Err(XPathError::invalid_regex_pattern("["))
            });
            seen.push(format!("{:?}", result.map(|_| ()).unwrap_err()));
        }
        assert_eq!(compiles, 1);
        assert!(seen.windows(2).all(|pair| pair[0] == pair[1]), "{seen:?}");
    }

    #[test]
    fn the_table_never_grows_past_the_cap() {
        let mut cache = RegexCache::default();
        let mut compiles = 0;
        for i in 0..MAX_ENTRIES * 4 {
            let pattern = format!("x{i}y");
            let result = cache.get_or_compile(&pattern, "", || {
                compiles += 1;
                ok(&pattern)
            });
            assert!(result.unwrap().is_match(&format!("x{i}y")));
            assert!(cache.len() <= MAX_ENTRIES, "{} entries", cache.len());
        }
        // Every key was new, so every call compiled and nothing was ever hit.
        assert_eq!(compiles, MAX_ENTRIES * 4);
        assert_eq!(cache.hits(), 0);
        assert_eq!(cache.len(), MAX_ENTRIES);
    }

    #[test]
    fn the_least_recently_used_key_is_the_one_dropped() {
        let mut cache = RegexCache::default();
        let keys: Vec<String> = (0..MAX_ENTRIES).map(|i| format!("k{i}")).collect();
        for key in &keys {
            let _ = cache.get_or_compile(key, "", || ok(key));
        }
        assert_eq!(cache.len(), MAX_ENTRIES);

        // Touch the oldest entry: it becomes the most recently used, and the
        // one inserted just after it becomes the oldest.
        let _ = cache.get_or_compile(&keys[0], "", || panic!("must be a hit"));
        assert_eq!(cache.hits(), 1);

        // One new key evicts exactly one entry — the one now least recently
        // used, which is `k1`, not the key just touched.
        let _ = cache.get_or_compile("fresh", "", || ok("fresh"));
        assert_eq!(cache.len(), MAX_ENTRIES);
        let resident = cache.keys();
        assert!(resident.contains(&("fresh", "")));
        assert!(resident.contains(&(keys[0].as_str(), "")));
        assert!(!resident.contains(&(keys[1].as_str(), "")));
    }
}
