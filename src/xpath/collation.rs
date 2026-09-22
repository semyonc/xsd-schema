//! Collations for XPath 2.0 string comparison.
//!
//! XPath and its function library compare strings *under a collation*: a rule
//! that says which strings are equal and how they order. Every implementation
//! must support the Unicode **codepoint** collation, whose URI is
//! [`CODEPOINT_COLLATION_URI`], and this crate implements it directly — it is
//! `str::cmp`, it needs no data, and it is what the engine uses unless a host
//! asks for something else.
//!
//! Every other collation is locale data plus an algorithm, which this crate
//! deliberately does not carry: it takes **no dependencies** for collation.
//! Instead it offers a callback. A host implements [`Collation`] over whatever
//! collator it already has, implements [`CollationResolver`] to map collation
//! URIs to those collations, and installs the resolver on the static context
//! with [`XPathContext::with_collation_resolver`]. From then on
//!
//! * `fn:compare`, `fn:contains`, `fn:starts-with`, `fn:ends-with`,
//!   `fn:substring-before`, `fn:substring-after`, `fn:index-of`,
//!   `fn:distinct-values`, `fn:deep-equal`, `fn:min` and `fn:max` use the
//!   collation named by their `$collation` argument, and
//! * the value and general comparisons (`eq`, `lt`, `=`, `<`, …) and those same
//!   functions called without a `$collation` argument use the static context's
//!   **default collation**, which [`XPathContext::with_default_collation`] sets
//!   and which is the codepoint collation when it is not set.
//!
//! # Errors
//!
//! Both errors are dynamic and are raised where the collation is *needed*, not
//! where the context is built — so an unsupported default collation never
//! disturbs an expression that compares no strings.
//!
//! * **FOCH0002** — the collation URI is not one the implementation supports:
//!   it is not the codepoint URI and either no resolver is installed or the
//!   resolver returned `None`.
//! * **FOCH0004** — the collation has no notion of *collation units*, which the
//!   five substring functions are defined over. A [`Collation`] declares that by
//!   leaving [`Collation::find`] at its default, which answers `None`.
//!
//! # Collation URIs
//!
//! The `$collation` argument is an `xs:string` whose lexical form must conform
//! to `xs:anyURI` (F&O §7.3.1). A **relative** URI is resolved against the
//! base-uri property of the static context ([`XPathContext::with_base_uri`]);
//! when the static context has no base URI the argument is passed to the
//! resolver as given — that is not an error, and a resolver is free to
//! recognise such a short name. The static context's *default collation*
//! property is used as given: XPath 2.0 §2.1.1 makes it an absolute URI.
//!
//! [`CODEPOINT_COLLATION_URI`] is answered without ever consulting the
//! resolver, so a host pays nothing for the collation machinery until it asks
//! for a different collation.
//!
//! # Example
//!
//! A host-side ASCII case-insensitive collation and the resolver that names it:
//!
//! ```
//! use std::cmp::Ordering;
//! use std::rc::Rc;
//!
//! use xsd_schema::namespace::table::NameTable;
//! use xsd_schema::xpath::api::XPathExpr;
//! use xsd_schema::xpath::collation::{Collation, CollationResolver};
//! use xsd_schema::xpath::{RoXmlNavigator, XPathContext};
//!
//! const CASELESS: &str = "http://example.com/collation/ascii-caseless";
//!
//! /// Compares ASCII letters without regard to case.
//! struct AsciiCaseless;
//!
//! impl Collation for AsciiCaseless {
//!     fn compare(&self, a: &str, b: &str) -> Ordering {
//!         a.bytes()
//!             .map(|b| b.to_ascii_lowercase())
//!             .cmp(b.bytes().map(|b| b.to_ascii_lowercase()))
//!     }
//!
//!     /// Optional, but it lets the engine index string comparisons instead of
//!     /// comparing every pair: bytes whose order and equality agree with
//!     /// `compare`.
//!     fn sort_key(&self, s: &str) -> Option<Vec<u8>> {
//!         Some(s.bytes().map(|b| b.to_ascii_lowercase()).collect())
//!     }
//!
//!     /// Optional, and what the five substring functions need: the leftmost
//!     /// shortest match of `needle`, as a byte range of `haystack`.
//!     fn find(&self, haystack: &str, needle: &str) -> Option<Option<(usize, usize)>> {
//!         let (h, n) = (haystack.to_ascii_lowercase(), needle.to_ascii_lowercase());
//!         Some(h.find(&n).map(|start| (start, start + n.len())))
//!     }
//! }
//!
//! /// Maps collation URIs to collations. Only this one URI is known here.
//! #[derive(Debug)]
//! struct HostCollations;
//!
//! impl CollationResolver for HostCollations {
//!     fn resolve(&self, uri: &str) -> Option<Rc<dyn Collation>> {
//!         (uri == CASELESS).then(|| Rc::new(AsciiCaseless) as Rc<dyn Collation>)
//!     }
//! }
//!
//! let names = NameTable::new();
//! let resolver = HostCollations;
//! let ctx = XPathContext::new(&names)
//!     .with_collation_resolver(&resolver)
//!     .with_default_collation(CASELESS);
//!
//! let run = |src: &str| {
//!     XPathExpr::compile(src, &ctx)
//!         .unwrap()
//!         .evaluator(&ctx)
//!         .run::<RoXmlNavigator<'static>>()
//!         .unwrap()
//! };
//!
//! // An explicit `$collation` argument.
//! assert_eq!(
//!     run(&format!("compare('ABC', 'abc', '{CASELESS}')")).as_integer(),
//!     Some(0.into())
//! );
//! // The default collation, used by the comparison operators …
//! assert_eq!(run("'ABC' eq 'abc'").as_bool(), Some(true));
//! assert_eq!(run("('a', 'b') = 'B'").as_bool(), Some(true));
//! // … and by a function called without a `$collation` argument.
//! assert_eq!(run("starts-with('Hello', 'he')").as_bool(), Some(true));
//!
//! // A URI nothing recognises is FOCH0002.
//! let unknown = XPathExpr::compile("compare('a', 'b', 'http://example.com/nope')", &ctx)
//!     .unwrap()
//!     .evaluator(&ctx)
//!     .run::<RoXmlNavigator<'static>>()
//!     .err()
//!     .expect("an unsupported collation raises");
//! assert_eq!(unknown.error_code(), Some("FOCH0002"));
//! ```

use std::cmp::Ordering;
use std::rc::Rc;

use crate::xpath::context::XPathContext;
use crate::xpath::error::XPathError;

/// The Unicode codepoint collation, which every implementation must support.
///
/// "Every implementation of XPath 2.0 and higher must support this collation"
/// (F&O §7.3.1). It is this crate's default collation, it is answered without
/// consulting a [`CollationResolver`], and it supports collation units, so the
/// five substring functions work under it.
pub const CODEPOINT_COLLATION_URI: &str =
    "http://www.w3.org/2005/xpath-functions/collation/codepoint";

// ============================================================================
// The traits a host implements
// ============================================================================

/// A rule for comparing strings, supplied by the host.
///
/// Only [`compare`](Collation::compare) is required. The other methods have
/// defaults; supplying them lets the engine do more:
///
/// | method | what it unlocks | default |
/// |---|---|---|
/// | [`equals`](Collation::equals) | a cheaper equality than a full compare | `compare(..).is_eq()` |
/// | [`sort_key`](Collation::sort_key) | hash-indexed general comparisons instead of pairwise ones | `None` |
/// | [`find`](Collation::find) | the five substring functions; without it they raise FOCH0004 | `None` |
/// | [`starts_with`](Collation::starts_with) / [`ends_with`](Collation::ends_with) | exactness when the collation has *ignorable* collation units | derived from `equals` |
///
/// # Contract
///
/// `compare` must be a total order: reflexive, antisymmetric, transitive, and
/// stable for the life of the collation. A [`CollationResolver`] must likewise
/// answer the same URI with the same collation throughout one evaluation run —
/// the engine caches collations and indexes built from them.
///
/// There is deliberately **no `Send + Sync` bound**: a host collation may wrap a
/// collator that is neither, and collations are only ever used on the thread
/// that evaluates the expression.
pub trait Collation {
    /// Order `a` against `b` under this collation.
    fn compare(&self, a: &str, b: &str) -> Ordering;

    /// Whether `a` and `b` are equal under this collation.
    ///
    /// The default is `compare(a, b).is_eq()`; override it when equality is
    /// cheaper to decide than order.
    fn equals(&self, a: &str, b: &str) -> bool {
        self.compare(a, b).is_eq()
    }

    /// Bytes whose equality and order agree with [`compare`](Collation::compare)
    /// — `sort_key(a).cmp(&sort_key(b)) == compare(a, b)` — or `None` when this
    /// collation cannot produce them.
    ///
    /// A sort key lets a general comparison such as `$a = $b` be decided with a
    /// hash index in `O(|a| + |b|)` instead of comparing every pair. Without one
    /// the engine still answers the comparison, by comparing pairs.
    ///
    /// The default is `None`.
    fn sort_key(&self, s: &str) -> Option<Vec<u8>> {
        let _ = s;
        None
    }

    /// The leftmost shortest match of `needle` in `haystack`, in collation
    /// units, as a byte range of `haystack`.
    ///
    /// * outer `None` — this collation has no notion of collation units. The
    ///   five substring functions then raise FOCH0004, which F&O §7.5 permits.
    /// * `Some(None)` — no match.
    /// * `Some(Some((start, end)))` — `haystack[start..end]` is the *minimal
    ///   match*: the leftmost, shortest substring whose collation units are
    ///   those of `needle`. Both offsets must be `char` boundaries of
    ///   `haystack`; a range that is not is reported as an internal error
    ///   rather than panicking.
    ///
    /// `fn:contains`, `fn:substring-before` and `fn:substring-after` are derived
    /// from this directly. The default is `None`.
    fn find(&self, haystack: &str, needle: &str) -> Option<Option<(usize, usize)>> {
        let _ = (haystack, needle);
        None
    }

    /// Whether some **prefix** of `haystack` has the collation units of
    /// `needle` — `fn:starts-with`. `None` means "no collation units", as in
    /// [`find`](Collation::find).
    ///
    /// The default gates on `find` (so a collation with no collation units
    /// still raises FOCH0004) and then tests every prefix with
    /// [`equals`](Collation::equals), which is exactly F&O §7.5.2's wording and
    /// stays correct when the collation has *ignorable* collation units — a
    /// case the leftmost minimal match cannot answer, because with `-`
    /// ignorable `starts-with('-ab', 'a')` is true while the minimal match of
    /// `a` in `-ab` starts at byte 1. Override it when the host collator can do
    /// better than `O(n)` equality tests.
    fn starts_with(&self, haystack: &str, needle: &str) -> Option<bool> {
        self.find(haystack, needle)?;
        Some(prefixes(haystack).any(|prefix| self.equals(prefix, needle)))
    }

    /// Whether some **suffix** of `haystack` has the collation units of
    /// `needle` — `fn:ends-with`. `None` means "no collation units", as in
    /// [`find`](Collation::find).
    ///
    /// The default is the mirror of [`starts_with`](Collation::starts_with) and
    /// carries the same reasoning.
    fn ends_with(&self, haystack: &str, needle: &str) -> Option<bool> {
        self.find(haystack, needle)?;
        Some(suffixes(haystack).any(|suffix| self.equals(suffix, needle)))
    }
}

/// Every prefix of `s`, shortest first, including `""` and `s` itself.
fn prefixes(s: &str) -> impl Iterator<Item = &str> {
    s.char_indices()
        .map(|(index, _)| index)
        .chain(std::iter::once(s.len()))
        .map(|end| &s[..end])
}

/// Every suffix of `s`, longest first, including `s` itself and `""`.
fn suffixes(s: &str) -> impl Iterator<Item = &str> {
    s.char_indices()
        .map(|(index, _)| index)
        .chain(std::iter::once(s.len()))
        .map(|start| &s[start..])
}

/// Maps a collation URI to a [`Collation`], supplied by the host.
///
/// Installed on the static context with
/// [`XPathContext::with_collation_resolver`]. It is never asked about
/// [`CODEPOINT_COLLATION_URI`], which the engine answers itself, and it is
/// asked only when a collation is actually needed to compare strings.
///
/// The `Rc` return (rather than a borrowed reference) lets a resolver build a
/// collation on demand from a parameterised URI — a UCA URI carrying a locale
/// and a strength, say — instead of having to own every collation up front.
///
/// [`std::fmt::Debug`] is a supertrait so that the resolver can live in
/// [`XPathContext`], which derives `Debug`; this matches
/// [`FunctionCatalog`](crate::xpath::functions::FunctionCatalog).
pub trait CollationResolver: std::fmt::Debug {
    /// The collation named by `uri`, or `None` if this host does not support it
    /// (which the caller reports as FOCH0002).
    ///
    /// `uri` is **absolute**: a relative `$collation` argument has already been
    /// resolved against the static context's base URI, when it has one.
    fn resolve(&self, uri: &str) -> Option<Rc<dyn Collation>>;
}

// ============================================================================
// The collation a call actually uses
// ============================================================================

/// The collation resolved for one function call or operator evaluation, owning
/// what it needs (the `Rc`s) so that it can outlive the resolution call.
///
/// `Codepoint` is not an `Rc<dyn Collation>` on purpose: it is the overwhelming
/// majority of all comparisons, and it must cost exactly what it cost before
/// this module existed.
#[derive(Clone)]
pub(crate) enum ActiveCollation {
    /// The Unicode codepoint collation: `str::cmp`, no indirection.
    Codepoint,
    /// A host collation, with the URI it was resolved from (for FOCH0004).
    Custom(Rc<str>, Rc<dyn Collation>),
    /// A URI nothing recognises. Carried rather than raised so that the error
    /// happens where F&O §7.3.1 puts it — when the collation is *needed* to
    /// compare two strings — and not in an expression that compares none. A
    /// caller that wants it raised straight away asks for
    /// [`require`](ActiveCollation::require).
    Unsupported(Rc<str>),
}

impl ActiveCollation {
    /// A borrowed handle, which is what the comparison helpers take.
    #[inline]
    pub(crate) fn as_ref(&self) -> CollationRef<'_> {
        match self {
            ActiveCollation::Codepoint => CollationRef::Codepoint,
            ActiveCollation::Custom(_, collation) => CollationRef::Custom(&**collation),
            ActiveCollation::Unsupported(uri) => CollationRef::Unsupported(uri),
        }
    }

    /// The URI this collation was resolved from.
    #[inline]
    pub(crate) fn uri(&self) -> &str {
        match self {
            ActiveCollation::Codepoint => CODEPOINT_COLLATION_URI,
            ActiveCollation::Custom(uri, _) | ActiveCollation::Unsupported(uri) => uri,
        }
    }

    /// FOCH0002 now, for a caller that needs the collation whatever the values
    /// turn out to be — which is every function that takes a `$collation`
    /// argument and compares strings, as against the comparison operators,
    /// whose operands may well be numbers.
    #[inline]
    pub(crate) fn require(self) -> Result<Self, XPathError> {
        match self {
            ActiveCollation::Unsupported(uri) => Err(XPathError::unknown_collation(&*uri)),
            other => Ok(other),
        }
    }
}

/// A borrowed collation, as the comparison helpers take it.
///
/// Three states rather than `Option<&dyn Collation>` so that "the URI named no
/// collation this implementation supports" can travel *into* the comparison and
/// raise there — the pairwise loop sees a numeric pair and a string pair alike,
/// and only the string pair may raise FOCH0002.
#[derive(Clone, Copy, Default)]
pub(crate) enum CollationRef<'c> {
    /// The Unicode codepoint collation: the first arm, and the one every hot
    /// loop takes.
    #[default]
    Codepoint,
    /// A host collation.
    Custom(&'c dyn Collation),
    /// FOCH0002, if and when two strings are compared.
    Unsupported(&'c str),
}

impl std::fmt::Debug for CollationRef<'_> {
    /// A host collation is opaque — the trait carries no `Debug` bound, on
    /// purpose — so it is printed by shape, which is all a `Debug`-derived
    /// container needs.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CollationRef::Codepoint => f.write_str("Codepoint"),
            CollationRef::Custom(_) => f.write_str("Custom(<collation>)"),
            CollationRef::Unsupported(uri) => write!(f, "Unsupported({uri:?})"),
        }
    }
}

impl CollationRef<'_> {
    /// Whether this is the codepoint collation, i.e. whether every string
    /// comparison is `str::cmp` and no host code is involved.
    #[inline]
    pub(crate) fn is_codepoint(&self) -> bool {
        matches!(self, CollationRef::Codepoint)
    }

    /// Order two string values under this collation.
    #[inline]
    pub(crate) fn compare(&self, a: &str, b: &str) -> Result<Ordering, XPathError> {
        match self {
            CollationRef::Codepoint => Ok(a.cmp(b)),
            CollationRef::Custom(collation) => Ok(collation.compare(a, b)),
            CollationRef::Unsupported(uri) => Err(XPathError::unknown_collation(*uri)),
        }
    }

    /// Whether two string values are equal under this collation.
    #[inline]
    pub(crate) fn equals(&self, a: &str, b: &str) -> Result<bool, XPathError> {
        match self {
            CollationRef::Codepoint => Ok(a == b),
            CollationRef::Custom(collation) => Ok(collation.equals(a, b)),
            CollationRef::Unsupported(uri) => Err(XPathError::unknown_collation(*uri)),
        }
    }

    /// The host collation, when there is one. `None` covers both the codepoint
    /// collation and an unsupported URI — neither of which a collation-unit
    /// operation can be performed with.
    #[inline]
    pub(crate) fn custom(&self) -> Option<&dyn Collation> {
        match self {
            CollationRef::Custom(collation) => Some(*collation),
            _ => None,
        }
    }
}

// ============================================================================
// Resolution — the one place that turns a URI into a collation
// ============================================================================

/// The static context's default collation.
///
/// The counterpart of [`resolve_collation_cached`] for the call sites that have
/// only the static context — the general-comparison operators, whose signatures
/// are public and fixed — and that never take an explicit `$collation`
/// argument.
///
/// `XPathContext` stores an unset default — and an explicit codepoint default —
/// as `None`, so this is a discriminant test on the hot path.
pub(crate) fn resolve_default(context: &XPathContext<'_>) -> ActiveCollation {
    match context.default_collation_uri() {
        None => ActiveCollation::Codepoint,
        Some(uri) => lookup(context, uri),
    }
}

/// Ask the host resolver.
fn lookup(context: &XPathContext<'_>, uri: &str) -> ActiveCollation {
    match context
        .collation_resolver()
        .and_then(|resolver| resolver.resolve(uri))
    {
        Some(collation) => ActiveCollation::Custom(Rc::from(uri), collation),
        None => ActiveCollation::Unsupported(Rc::from(uri)),
    }
}

/// `uri` resolved against the static base URI when it is relative and a base
/// URI exists; otherwise `uri` itself.
fn absolutize<'u>(context: &XPathContext<'_>, uri: &'u str) -> std::borrow::Cow<'u, str> {
    use std::borrow::Cow;
    if crate::xpath::functions::uri::is_absolute_uri(uri) {
        return Cow::Borrowed(uri);
    }
    match context.base_uri.as_deref() {
        Some(base) => match crate::xpath::functions::uri::resolve_uri_reference(uri, base) {
            Ok(resolved) => Cow::Owned(resolved),
            // An unresolvable pair is not FORG0009 here: the argument simply
            // names no collation this implementation supports, which the caller
            // reports as FOCH0002.
            Err(()) => Cow::Borrowed(uri),
        },
        None => Cow::Borrowed(uri),
    }
}

// ============================================================================
// The collation-unit operations the five substring functions need
// ============================================================================

/// The minimal match of `needle` in `haystack`, or FOCH0004.
///
/// F&O §7.5 defines `fn:contains`, `fn:substring-before` and
/// `fn:substring-after` over collation units and adds that "if the specified
/// collation does not support collation units an error MAY be raised
/// \[err:FOCH0004\]"; this crate raises it.
///
/// A zero-length `needle` is answered here rather than left to the host: F&O
/// fixes the result for it ("if the value of `$arg2` is the zero-length string,
/// then the function returns true" and its substring counterparts), and the
/// empty match at the start is that result.
pub(crate) fn collated_find(
    collation: &dyn Collation,
    uri: &str,
    haystack: &str,
    needle: &str,
) -> Result<Option<(usize, usize)>, XPathError> {
    let found = collation
        .find(haystack, needle)
        .ok_or_else(|| XPathError::collation_no_units(uri))?;
    if needle.is_empty() {
        return Ok(Some((0, 0)));
    }
    Ok(found)
}

/// `fn:starts-with` under a host collation, or FOCH0004.
pub(crate) fn collated_starts_with(
    collation: &dyn Collation,
    uri: &str,
    haystack: &str,
    needle: &str,
) -> Result<bool, XPathError> {
    if needle.is_empty() {
        // Still gate on collation units, so that the answer for a collation
        // without them does not depend on the argument.
        collated_find(collation, uri, haystack, needle)?;
        return Ok(true);
    }
    collation
        .starts_with(haystack, needle)
        .ok_or_else(|| XPathError::collation_no_units(uri))
}

/// `fn:ends-with` under a host collation, or FOCH0004.
pub(crate) fn collated_ends_with(
    collation: &dyn Collation,
    uri: &str,
    haystack: &str,
    needle: &str,
) -> Result<bool, XPathError> {
    if needle.is_empty() {
        collated_find(collation, uri, haystack, needle)?;
        return Ok(true);
    }
    collation
        .ends_with(haystack, needle)
        .ok_or_else(|| XPathError::collation_no_units(uri))
}

/// Slice `haystack` at a byte offset a host collation reported, without ever
/// panicking on an offset that is out of range or not a `char` boundary.
pub(crate) fn slice_at(haystack: &str, range: std::ops::Range<usize>) -> Result<&str, XPathError> {
    haystack.get(range.clone()).ok_or_else(|| {
        XPathError::internal(format!(
            "collation reported the byte range {}..{}, which is not a character range of the \
             {}-byte argument",
            range.start,
            range.end,
            haystack.len()
        ))
    })
}

// ============================================================================
// Per-run memo
// ============================================================================

/// The collations of one evaluation run, memoised by URI.
///
/// A call such as `compare($a, $b, $uri)` inside a predicate resolves the same
/// URI once per item, and a host resolver may build a real collator each time.
/// This keeps what the resolver answered for the duration of one **run**, the
/// same place and lifetime as
/// [`regex_cache`](crate::xpath::regex_cache): in
/// [`DynamicContext`](crate::xpath::context::DynamicContext), empty and
/// allocation-free until the first call that needs a non-codepoint collation,
/// dropped with the context, never global and never in the compiled expression.
///
/// The codepoint collation never reaches the memo — it is decided before
/// resolution begins — so an expression that uses no other collation never
/// allocates here.
///
/// One entry is enough: an expression names one collation URI in the
/// overwhelming majority of cases, and the entry is replaced, not grown, when a
/// second URI appears, so a run that alternates between two URIs costs a
/// resolver call each time rather than unbounded memory.
/// One memo entry: the URI, and what the resolver answered for it — `None`
/// being "this host does not support it".
type MemoEntry = (Rc<str>, Option<Rc<dyn Collation>>);

#[derive(Default)]
pub(crate) struct CollationCache {
    /// The last URI resolved, and what the resolver answered for it — including
    /// `None`, "this host does not support it", which is just as much a
    /// function of the URI as a collation is.
    last: Option<MemoEntry>,
    /// How many lookups this run answered from the memo, and how many reached
    /// the resolver. Test-only, so that a test can tell "still correct" from
    /// "the memo silently stopped engaging".
    #[cfg(test)]
    hits: u32,
    #[cfg(test)]
    misses: u32,
}

impl CollationCache {
    /// What the resolver answers for `uri`, asking it at most once per distinct
    /// URI in a row.
    fn get_or_resolve(
        &mut self,
        uri: &str,
        resolve: impl FnOnce() -> Option<Rc<dyn Collation>>,
    ) -> Option<Rc<dyn Collation>> {
        if let Some((cached_uri, cached)) = &self.last {
            if &**cached_uri == uri {
                #[cfg(test)]
                {
                    self.hits += 1;
                }
                return cached.clone();
            }
        }
        #[cfg(test)]
        {
            self.misses += 1;
        }
        let resolved = resolve();
        self.last = Some((Rc::from(uri), resolved.clone()));
        resolved
    }

    /// How many lookups of this run were answered from the memo.
    #[cfg(test)]
    pub(crate) fn hits(&self) -> u32 {
        self.hits
    }

    /// How many lookups of this run reached the resolver.
    #[cfg(test)]
    pub(crate) fn misses(&self) -> u32 {
        self.misses
    }
}

/// [`resolve_collation`], memoised in the dynamic context of this run.
///
/// Used by every call site that has a `DynamicContext` — the function library
/// and the value comparisons. The general-comparison operators see only the
/// static context (their signatures are public and fixed) and resolve directly;
/// under the codepoint collation both cost the same `Option::is_none()`.
pub(crate) fn resolve_collation_cached<N: crate::xpath::DomNavigator>(
    context: &mut crate::xpath::context::DynamicContext<'_, N>,
    explicit: Option<&str>,
) -> ActiveCollation {
    let static_context = context.static_context;
    let uri = match explicit {
        None => match static_context.default_collation_uri() {
            None => return ActiveCollation::Codepoint,
            Some(uri) => std::borrow::Cow::Borrowed(uri),
        },
        Some(uri) => {
            if uri == CODEPOINT_COLLATION_URI {
                return ActiveCollation::Codepoint;
            }
            let absolute = absolutize(static_context, uri);
            if absolute == CODEPOINT_COLLATION_URI {
                return ActiveCollation::Codepoint;
            }
            absolute
        }
    };

    let resolver = static_context.collation_resolver();
    let resolved = context
        .collation_cache_mut()
        .get_or_resolve(&uri, || resolver.and_then(|r| r.resolve(&uri)));
    match resolved {
        Some(collation) => ActiveCollation::Custom(Rc::from(&*uri), collation),
        None => ActiveCollation::Unsupported(Rc::from(&*uri)),
    }
}

#[cfg(test)]
#[path = "collation_tests.rs"]
mod collation_tests;
