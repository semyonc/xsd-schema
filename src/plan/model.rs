//! The plan as data: integer tables and one byte array, with no pointers
//! inside them.
//!
//! [`PlanData`] owns the tables (built in process from a `SchemaSet`, feature
//! `codegen`); [`PlanRef`] borrows them as slices, which is what the
//! interpreter reads and what generated statics will provide.

/// No entry: a missing name, child, literal or table.
pub const NONE: u32 = u32::MAX;
/// The id of a namespace URI the plan does not know: no name in it matches.
pub const UNKNOWN_NS: u32 = u32::MAX - 1;
/// The id of the XML Schema instance namespace (`xsi:`).
pub const XSI_NS: u32 = u32::MAX - 2;

/// What a type plan allows as content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Content {
    /// Not covered: an element of this type answers `Unknown`.
    Uncovered,
    /// Element-only content, run by the type's DFA.
    ElementOnly,
    /// Character content, checked by the type's value check.
    Simple,
    /// Empty content: no character or element children.
    Empty,
}

/// The plan of one type (as reached through one declaration context).
#[derive(Clone, Copy, Debug)]
pub struct TypePlan {
    /// What the content may be.
    pub content: Content,
    /// The DFA's start state (element-only content).
    pub start: u32,
    /// The value check (simple content).
    pub check: u32,
    /// The type's attribute uses: `attrs[attr_start..attr_end]`.
    pub attr_start: u32,
    /// See `attr_start`.
    pub attr_end: u32,
    /// The bits of the required attribute uses.
    pub required: u64,
}

/// One DFA state of an element-only content model.
#[derive(Clone, Copy, Debug)]
pub struct StatePlan {
    /// Its transitions: `transitions[t_start..t_end]`.
    pub t_start: u32,
    /// See `t_start`.
    pub t_end: u32,
    /// Whether the content may end here.
    pub accept: bool,
}

/// A transition on a child element name.
#[derive(Clone, Copy, Debug)]
pub struct Transition {
    /// The child's name id.
    pub name: u32,
    /// The next state.
    pub next: u32,
    /// The child's plan: its type plan specialized by its declaration.
    pub child: u32,
}

/// [`ChildPlan::flags`]: the declaration has a `default` value.
pub const CHILD_DEFAULT: u8 = 1;
/// [`ChildPlan::flags`]: the declaration has a `fixed` value.
pub const CHILD_FIXED: u8 = 2;
/// [`ChildPlan::flags`]: the declaration is nillable.
pub const CHILD_NILLABLE: u8 = 4;
/// [`ChildPlan::flags`]: the declaration uses a construct the plan does not
/// cover (abstract, type alternatives, an identity constraint other than the
/// narrow `xs:unique`, a deferred type error, no type): the element answers
/// `Unknown`.
pub const CHILD_UNCOVERED: u8 = 8;

/// A child element's plan: its type plan specialized by what its element
/// declaration adds (`default`, `fixed`, `nillable`, identity constraints).
#[derive(Clone, Copy, Debug)]
pub struct ChildPlan {
    /// The type plan.
    pub ty: u32,
    /// `CHILD_*` bits.
    pub flags: u8,
    /// The `default` or `fixed` literal (`NONE` without one).
    pub value: u32,
    /// The narrow `xs:unique` declared on the element (`NONE` without one).
    pub unique: u32,
}

/// The narrow `xs:unique`: selector `./*`, one field `@attr` (unqualified),
/// over attribute types whose literal equality is value equality.
#[derive(Clone, Copy, Debug)]
pub struct UniquePlan {
    /// The name id of the field's attribute.
    pub attr: u32,
}

/// One attribute use of a type.
#[derive(Clone, Copy, Debug)]
pub struct AttrUse {
    /// The attribute's name id.
    pub name: u32,
    /// Its value check.
    pub check: u32,
    /// Its bit in the type's `required` mask and in the seen mask.
    pub bit: u8,
    /// Its `fixed` literal (`NONE` without one).
    pub fixed: u32,
}

/// White-space normalization of a value check (the type's `whiteSpace`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Ws {
    /// `preserve`.
    Preserve,
    /// `replace`.
    Replace,
    /// `collapse`.
    Collapse,
}

/// What a value check does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum CheckKind {
    /// Not covered: the value answers `Unknown`.
    Uncovered,
    /// Every value is valid (`xs:string` with no facets).
    Any,
    /// The plan's own check: white space, the lexical form of [`CheckPlan::lex`]
    /// and the facets the check carries.
    Value,
    /// Not covered by the plan itself; only the harness's delegating tier
    /// checks it (on the dynamic validator's own function).
    Delegate,
}

/// The lexical space a [`CheckKind::Value`] check reads its value in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Lex {
    /// The string family and `xs:anyURI`: every normalized value.
    String,
    /// `xs:Name`.
    Name,
    /// `xs:NCName` and `xs:ID`.
    NCName,
    /// `xs:NMTOKEN`.
    NmToken,
    /// `xs:language`.
    Language,
    /// The integer hierarchy (bounds in [`Bounds`]).
    Integer,
    /// `xs:decimal` (bounds in [`Bounds`]).
    Decimal,
    /// `xs:boolean`.
    Boolean,
    /// `xs:float`.
    Float,
    /// `xs:double`.
    Double,
    /// `xs:dateTime` (bounds in [`Bounds`]).
    DateTime,
    /// `xs:date` (bounds in [`Bounds`]).
    Date,
    /// `xs:time` (bounds in [`Bounds`]).
    Time,
    /// `xs:gYearMonth`.
    GYearMonth,
    /// `xs:gYear`.
    GYear,
    /// `xs:gMonthDay`.
    GMonthDay,
    /// `xs:gDay`.
    GDay,
    /// `xs:gMonth`.
    GMonth,
    /// `xs:duration`.
    Duration,
    /// `xs:yearMonthDuration`.
    YearMonthDuration,
    /// `xs:dayTimeDuration`.
    DayTimeDuration,
    /// `xs:hexBinary` (lengths in octets).
    HexBinary,
    /// `xs:base64Binary` (lengths in octets).
    Base64Binary,
}

/// [`CheckPlan::flags`]: the value is an `xs:ID`, unique in the document.
pub const CHECK_ID: u8 = 1;
/// [`CheckPlan::flags`]: the dynamic path matches the patterns against the
/// value as written, so a value its white space would change answers
/// `Unknown` when the check has patterns.
pub const CHECK_RAW_PATTERNS: u8 = 2;
/// [`CheckPlan::flags`]: XSD 1.0 lexical rules (no `+INF`).
pub const CHECK_XSD10: u8 = 4;
/// [`CheckPlan::flags`]: a time zone is required (`explicitTimezone`).
pub const CHECK_TZ_REQUIRED: u8 = 8;
/// [`CheckPlan::flags`]: a time zone is prohibited (`explicitTimezone`).
pub const CHECK_TZ_PROHIBITED: u8 = 16;

/// One value check.
#[derive(Clone, Copy, Debug)]
pub struct CheckPlan {
    /// What it does.
    pub kind: CheckKind,
    /// The lexical space (`Value`).
    pub lex: Lex,
    /// White space, applied before everything else (`Value`).
    pub ws: Ws,
    /// `CHECK_*` bits.
    pub flags: u8,
    /// Minimum length, in characters or (binary) octets (`Value`).
    pub min_len: u32,
    /// Maximum length (`Value`; `NONE` for no bound).
    pub max_len: u32,
    /// The enumeration: `enum_literals[enum_start..enum_end]`, literal ids
    /// sorted by their bytes; `enum_start == NONE` for no enumeration. A
    /// value is compared with the literals as written.
    pub enum_start: u32,
    /// See `enum_start`.
    pub enum_end: u32,
    /// The pattern steps: `pattern_steps[pat_start..pat_end]`.
    pub pat_start: u32,
    /// See `pat_start`.
    pub pat_end: u32,
    /// The bounds and digit facets: `bounds[bounds]` (`NONE` without).
    pub bounds: u32,
    /// A further check the same value must pass (`NONE` without): the
    /// layers of simple content.
    pub next: u32,
    /// The delegating tier's index for this check (`NONE` without one).
    pub delegate: u32,
}

/// A bound of a range facet: a decimal `mant / 10^scale` (numbers), or a
/// position on the time line, whole seconds `mant` and the fraction digits
/// `frac` (dates and times).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bound {
    /// Whether the facet is present.
    pub set: bool,
    /// The mantissa, or the whole seconds.
    pub mant: i128,
    /// The scale (numbers).
    pub scale: u32,
    /// The fraction digits, no trailing zero (dates and times).
    pub frac: Span,
}

impl Bound {
    /// No bound.
    pub const NONE: Bound = Bound {
        set: false,
        mant: 0,
        scale: 0,
        frac: Span { start: 0, len: 0 },
    };
}

/// The range and digit facets of a check, with the built-in range of an
/// integer type.
#[derive(Clone, Copy, Debug)]
pub struct Bounds {
    /// `minInclusive`.
    pub min_incl: Bound,
    /// `minExclusive`.
    pub min_excl: Bound,
    /// `maxInclusive`.
    pub max_incl: Bound,
    /// `maxExclusive`.
    pub max_excl: Bound,
    /// The built-in range of an integer type, inclusive.
    pub int_lo: i128,
    /// See `int_lo`.
    pub int_hi: i128,
    /// `totalDigits` (`NONE` without).
    pub total_digits: u32,
    /// `fractionDigits` (`NONE` without).
    pub fraction_digits: u32,
    /// Dates and times: whether the bounds have a time zone. A value whose
    /// time zone presence differs from its bounds' answers `Unknown`.
    pub zoned: bool,
}

impl Bounds {
    /// Whether a range or digit facet is present (an integer type's built-in
    /// range is not one).
    #[inline]
    pub fn has_facets(&self) -> bool {
        self.has_range() || self.total_digits != NONE || self.fraction_digits != NONE
    }

    /// Whether a range facet is present.
    #[inline]
    pub fn has_range(&self) -> bool {
        self.min_incl.set || self.min_excl.set || self.max_incl.set || self.max_excl.set
    }
}

/// One pattern step (a restriction step's patterns, joined): a value must be
/// accepted by its table and lie inside its trusted alphabet.
#[derive(Clone, Copy, Debug)]
pub struct PatternStep {
    /// The table (`NONE`: the step has none, the value answers `Unknown`).
    pub table: u32,
    /// The trusted alphabet.
    pub alphabet: u32,
}

/// A pattern DFA over UTF-8 bytes, as the crate's own table.
#[derive(Clone, Copy, Debug)]
pub struct PatternTable {
    /// The 256-entry byte-class map: `bytes[classes..classes + 256]`.
    pub classes: u32,
    /// Number of byte classes (the row stride).
    pub stride: u32,
    /// The transitions: `pattern_trans[trans + state * stride + class]`.
    pub trans: u32,
    /// Number of states; state 0 is the dead state.
    pub states: u32,
    /// The start state.
    pub start: u32,
    /// Whether a state accepts at the end of input: `bytes[accept + state]`
    /// is 1 or 0.
    pub accept: u32,
}

/// A trusted alphabet: the code points on which both run-time pattern
/// engines agree for every atom of a pattern step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Alphabet {
    /// ASCII code points 0–63, one bit each.
    pub ascii_lo: u64,
    /// ASCII code points 64–127.
    pub ascii_hi: u64,
    /// Whether code points past ASCII are trusted at all.
    pub non_ascii: bool,
    /// Code points past ASCII that are not trusted, as inclusive ranges:
    /// `excluded[excl_start..excl_end]`, sorted, disjoint.
    pub excl_start: u32,
    /// See `excl_start`.
    pub excl_end: u32,
}

/// A name: its namespace id and its local part's bytes.
#[derive(Clone, Copy, Debug)]
pub struct NameEntry {
    /// Namespace id (0 = no namespace).
    pub ns: u32,
    /// `bytes[start..start + len]`.
    pub start: u32,
    /// See `start`.
    pub len: u32,
}

/// A byte string of the blob: `bytes[start..start + len]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    /// Start offset.
    pub start: u32,
    /// Length.
    pub len: u32,
}

/// A global element the document may start with.
#[derive(Clone, Copy, Debug)]
pub struct Root {
    /// Its name id.
    pub name: u32,
    /// Its child plan.
    pub child: u32,
}

/// The owned plan.
#[derive(Clone, Debug, Default)]
pub struct PlanData {
    /// Namespace URIs by id (id 0 is the empty URI: no namespace).
    pub namespaces: Vec<Span>,
    /// Names by id.
    pub names: Vec<NameEntry>,
    /// Open-addressing table over `names` (`NONE` = empty slot).
    pub name_table: Vec<u32>,
    /// Global elements, sorted by name id.
    pub roots: Vec<Root>,
    /// Type plans.
    pub types: Vec<TypePlan>,
    /// Child plans.
    pub children: Vec<ChildPlan>,
    /// DFA states.
    pub states: Vec<StatePlan>,
    /// DFA transitions.
    pub transitions: Vec<Transition>,
    /// Attribute uses.
    pub attrs: Vec<AttrUse>,
    /// Narrow `xs:unique` constraints.
    pub uniques: Vec<UniquePlan>,
    /// Value checks.
    pub checks: Vec<CheckPlan>,
    /// Range and digit facets of the value checks.
    pub bounds: Vec<Bounds>,
    /// Literal ids of enumerations.
    pub enum_literals: Vec<u32>,
    /// Literals (enumeration values, `default` and `fixed` values).
    pub literals: Vec<Span>,
    /// Pattern steps.
    pub pattern_steps: Vec<PatternStep>,
    /// Pattern tables.
    pub patterns: Vec<PatternTable>,
    /// Pattern transitions.
    pub pattern_trans: Vec<u32>,
    /// Trusted alphabets.
    pub alphabets: Vec<Alphabet>,
    /// Excluded code-point ranges of the alphabets.
    pub excluded: Vec<(u32, u32)>,
    /// The byte blob: names, URIs, literals, byte-class maps, accept flags.
    pub bytes: Vec<u8>,
    /// The value check of `xs:anyURI`, for `xsi:schemaLocation` hints
    /// (`NONE` without one).
    pub hint_check: u32,
    /// For the harness's delegating tier only: the type each delegated
    /// check stands for. Not part of [`PlanRef`]: an emitted plan has no
    /// such tier.
    #[cfg(feature = "codegen")]
    pub delegates: Vec<crate::ids::TypeKey>,
}

/// The plan as slices: what the interpreter reads.
#[derive(Clone, Copy, Debug)]
pub struct PlanRef<'a> {
    /// See [`PlanData::namespaces`].
    pub namespaces: &'a [Span],
    /// See [`PlanData::names`].
    pub names: &'a [NameEntry],
    /// See [`PlanData::name_table`].
    pub name_table: &'a [u32],
    /// See [`PlanData::roots`].
    pub roots: &'a [Root],
    /// See [`PlanData::types`].
    pub types: &'a [TypePlan],
    /// See [`PlanData::children`].
    pub children: &'a [ChildPlan],
    /// See [`PlanData::states`].
    pub states: &'a [StatePlan],
    /// See [`PlanData::transitions`].
    pub transitions: &'a [Transition],
    /// See [`PlanData::attrs`].
    pub attrs: &'a [AttrUse],
    /// See [`PlanData::uniques`].
    pub uniques: &'a [UniquePlan],
    /// See [`PlanData::checks`].
    pub checks: &'a [CheckPlan],
    /// See [`PlanData::bounds`].
    pub bounds: &'a [Bounds],
    /// See [`PlanData::enum_literals`].
    pub enum_literals: &'a [u32],
    /// See [`PlanData::literals`].
    pub literals: &'a [Span],
    /// See [`PlanData::pattern_steps`].
    pub pattern_steps: &'a [PatternStep],
    /// See [`PlanData::patterns`].
    pub patterns: &'a [PatternTable],
    /// See [`PlanData::pattern_trans`].
    pub pattern_trans: &'a [u32],
    /// See [`PlanData::alphabets`].
    pub alphabets: &'a [Alphabet],
    /// See [`PlanData::excluded`].
    pub excluded: &'a [(u32, u32)],
    /// See [`PlanData::bytes`].
    pub bytes: &'a [u8],
    /// See [`PlanData::hint_check`].
    pub hint_check: u32,
}

impl PlanData {
    /// The plan as slices.
    pub fn as_ref(&self) -> PlanRef<'_> {
        PlanRef {
            namespaces: &self.namespaces,
            names: &self.names,
            name_table: &self.name_table,
            roots: &self.roots,
            types: &self.types,
            children: &self.children,
            states: &self.states,
            transitions: &self.transitions,
            attrs: &self.attrs,
            uniques: &self.uniques,
            checks: &self.checks,
            bounds: &self.bounds,
            enum_literals: &self.enum_literals,
            literals: &self.literals,
            pattern_steps: &self.pattern_steps,
            patterns: &self.patterns,
            pattern_trans: &self.pattern_trans,
            alphabets: &self.alphabets,
            excluded: &self.excluded,
            bytes: &self.bytes,
            hint_check: self.hint_check,
        }
    }
}

/// The hash of a name for the open-addressing table.
#[inline]
pub fn hash_name(ns: u32, local: &[u8]) -> u32 {
    let mut h = 0x811c_9dc5u32 ^ ns.wrapping_mul(0x9E37_79B1);
    for &b in local {
        h = (h ^ u32::from(b)).wrapping_mul(0x0100_0193);
    }
    h ^ (h >> 15)
}

/// Whether two names are the same bytes. Names are short: compared here,
/// eight bytes at a time (the last eight overlapping), where the general
/// comparison is a call.
#[inline]
fn same_bytes(a: &[u8], b: &[u8]) -> bool {
    #[inline(always)]
    fn word(s: &[u8], at: usize) -> u64 {
        let mut w = [0u8; 8];
        w.copy_from_slice(&s[at..at + 8]);
        u64::from_ne_bytes(w)
    }
    let n = a.len();
    if n != b.len() {
        return false;
    }
    if n < 8 {
        let mut i = 0;
        while i < n {
            if a[i] != b[i] {
                return false;
            }
            i += 1;
        }
        return true;
    }
    let mut i = 0;
    while i + 8 < n {
        if word(a, i) != word(b, i) {
            return false;
        }
        i += 8;
    }
    word(a, n - 8) == word(b, n - 8)
}

impl<'a> PlanRef<'a> {
    /// The bytes of a span.
    #[inline]
    pub fn span(&self, s: Span) -> &'a [u8] {
        self.bytes
            .get(s.start as usize..(s.start as usize).saturating_add(s.len as usize))
            .unwrap_or_default()
    }

    /// The name id of (`ns`, `local`), or `NONE`.
    #[inline]
    pub fn lookup(&self, ns: u32, local: &[u8]) -> u32 {
        let len = self.name_table.len();
        if len == 0 || ns == UNKNOWN_NS {
            return NONE;
        }
        let mask = len - 1;
        let mut i = hash_name(ns, local) as usize & mask;
        for _ in 0..len {
            let e = self.name_table[i];
            if e == NONE {
                return NONE;
            }
            let n = self.names[e as usize];
            if n.ns == ns
                && self.span(Span {
                    start: n.start,
                    len: n.len,
                }) == local
            {
                return e;
            }
            i = (i + 1) & mask;
        }
        NONE
    }

    /// Whether the name `id` is (`ns`, `local`).
    #[inline]
    pub fn name_is(&self, id: u32, ns: u32, local: &[u8]) -> bool {
        self.names.get(id as usize).is_some_and(|n| {
            n.ns == ns
                && n.len as usize == local.len()
                && same_bytes(
                    self.span(Span {
                        start: n.start,
                        len: n.len,
                    }),
                    local,
                )
        })
    }

    /// The namespace id of a URI (`""` is 0), or `UNKNOWN_NS`.
    #[inline]
    pub fn namespace_id(&self, uri: &[u8]) -> u32 {
        if uri.is_empty() {
            return 0;
        }
        for (i, s) in self.namespaces.iter().enumerate() {
            if self.span(*s) == uri {
                return i as u32;
            }
        }
        UNKNOWN_NS
    }

    /// The child plan of a global element, or `NONE`.
    #[inline]
    pub fn root(&self, name: u32) -> u32 {
        match self.roots.binary_search_by_key(&name, |r| r.name) {
            Ok(i) => self.roots[i].child,
            Err(_) => NONE,
        }
    }
}
