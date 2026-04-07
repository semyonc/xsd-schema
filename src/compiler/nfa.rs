//! NFA data structures for content model validation
//!
//! This module defines the core NFA (Nondeterministic Finite Automaton) structures
//! used to represent compiled XSD content models.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::ids::{ElementKey, NameId, TypeKey};
use crate::parser::location::SourceRef;
use crate::schema::model::XsdVersion;
use crate::types::complex::{NamespaceConstraint, ProcessContents, not_qnames_exclude};
use super::substitution::SubstitutionGroupMap;

/// Unique identifier for NFA states within a table
pub type StateId = u32;

/// Unique identifier for a counter within an NFA.
///
/// u16 is intentional: keeps `TransitionKind` at 4 bytes (vs 8 with u32),
/// halving transition growth. 65K counters is far beyond any real schema.
pub type CounterId = u16;

/// An inclusive range of counter values `[lo, hi]`.
///
/// Used by the `RangedSingle` optimized path to represent a set of
/// possible counter values at a single NFA state, avoiding the O(N)
/// enumeration that scalar counters require for nullable loop bodies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CounterRange {
    pub lo: u32,
    pub hi: u32,
}

impl CounterRange {
    /// Create a range containing a single value.
    pub fn single(v: u32) -> Self {
        Self { lo: v, hi: v }
    }

    /// Create a range `[lo, hi]`. Panics in debug if `lo > hi`.
    pub fn new(lo: u32, hi: u32) -> Self {
        debug_assert!(lo <= hi, "CounterRange::new({lo}, {hi}): lo > hi");
        Self { lo, hi }
    }

    /// True if this range contains no values (`lo > hi`).
    pub fn is_empty(self) -> bool {
        self.lo > self.hi
    }

    /// True if `self` fully contains `other`.
    pub fn subsumes(self, other: Self) -> bool {
        self.lo <= other.lo && self.hi >= other.hi
    }

    /// Widen to include all values in both ranges.
    /// The caller must ensure the ranges are contiguous (adjacent or overlapping).
    pub fn union(self, other: Self) -> Self {
        let result = Self {
            lo: self.lo.min(other.lo),
            hi: self.hi.max(other.hi),
        };
        debug_assert!(result.lo <= result.hi);
        result
    }

    /// Intersect with `[0, max_exclusive - 1]`.  Returns an empty range if
    /// `lo >= max_exclusive`.
    pub fn intersect_below(self, max_exclusive: u32) -> Self {
        if max_exclusive == 0 || self.lo >= max_exclusive {
            // Use lo > hi to signal empty
            return Self { lo: 1, hi: 0 };
        }
        Self {
            lo: self.lo,
            hi: self.hi.min(max_exclusive - 1),
        }
    }

    /// Intersect with `[min_inclusive, u32::MAX]`.  Returns an empty range if
    /// `hi < min_inclusive`.
    pub fn intersect_above(self, min_inclusive: u32) -> Self {
        if self.hi < min_inclusive {
            return Self { lo: 1, hi: 0 };
        }
        Self {
            lo: self.lo.max(min_inclusive),
            hi: self.hi,
        }
    }
}

/// Definition of a counter used by counted NFA loops.
///
/// Each counter tracks how many times a loop body has been completed.
/// `min` is the minimum iterations for the exit guard; `max` is the
/// maximum iterations for the loop guard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CounterDef {
    /// Minimum completed iterations required to exit
    pub min: u32,
    /// Maximum iterations allowed before loop must exit
    pub max: u32,
    /// Whether the loop body can be traversed without consuming any input.
    /// When true and `num_counters == 1`, the `RangedSingle` fast-forward
    /// path is used in epsilon closure.
    pub body_nullable: bool,
}

/// Complete NFA table for a content model
///
/// Represents a compiled content model as a state machine. The NFA can be used
/// for validation by tracking active states and matching input elements.
#[derive(Debug, Clone)]
pub struct NfaTable {
    /// All states in the automaton
    pub states: Vec<NfaState>,
    /// Initial state ID
    pub start_state: StateId,
    /// Accepting state ID (single accept state per Thompson's construction)
    pub accept_state: StateId,
    /// Counter definitions for counted loops (empty if no counted repeats)
    pub counter_defs: Vec<CounterDef>,
}

impl NfaTable {
    /// Create a new NFA table with the given states (no counters)
    pub fn new(states: Vec<NfaState>, start_state: StateId, accept_state: StateId) -> Self {
        Self {
            states,
            start_state,
            accept_state,
            counter_defs: Vec::new(),
        }
    }

    /// Create a new NFA table with counters
    pub fn with_counters(
        states: Vec<NfaState>,
        start_state: StateId,
        accept_state: StateId,
        counter_defs: Vec<CounterDef>,
    ) -> Self {
        Self {
            states,
            start_state,
            accept_state,
            counter_defs,
        }
    }

    /// Check if this NFA has any counted loops
    pub fn has_counters(&self) -> bool {
        !self.counter_defs.is_empty()
    }

    /// Get a state by ID
    pub fn get_state(&self, id: StateId) -> Option<&NfaState> {
        self.states.get(id as usize)
    }

    /// Get a mutable reference to a state by ID
    pub fn get_state_mut(&mut self, id: StateId) -> Option<&mut NfaState> {
        self.states.get_mut(id as usize)
    }

    /// Get the number of states
    pub fn state_count(&self) -> usize {
        self.states.len()
    }

    /// Check if a state is the accept state
    pub fn is_accept(&self, state_id: StateId) -> bool {
        state_id == self.accept_state
    }

    /// Concatenate two NFA tables: self followed by other.
    /// Creates an epsilon transition from self's accept state to other's start state.
    /// Offsets both state IDs and counter IDs in the second table.
    pub fn concat(mut self, mut other: NfaTable) -> NfaTable {
        let state_offset = self.states.len() as StateId;
        let counter_offset = self.counter_defs.len() as CounterId;
        for state in &mut other.states {
            state.id += state_offset;
            for trans in &mut state.transitions {
                trans.target += state_offset;
                trans.kind = trans.kind.offset_counter(counter_offset);
            }
        }
        let other_start = other.start_state + state_offset;
        self.states[self.accept_state as usize].add_epsilon(other_start);
        let new_accept = other.accept_state + state_offset;
        self.states.extend(other.states);
        self.counter_defs.extend(other.counter_defs);
        NfaTable::with_counters(self.states, self.start_state, new_accept, self.counter_defs)
    }

    /// Get all transitions from a given state
    pub fn transitions_from(&self, state_id: StateId) -> &[NfaTransition] {
        self.get_state(state_id)
            .map(|s| s.transitions.as_slice())
            .unwrap_or(&[])
    }
}

/// A single state in the NFA
///
/// Each state can optionally have a term that must be matched to consume input
/// when transitioning through this state. States without terms (epsilon states)
/// are used for branching logic.
#[derive(Debug, Clone)]
pub struct NfaState {
    /// Unique identifier for this state
    pub id: StateId,
    /// The term that must be matched to enter this state via a consuming transition.
    /// None for epsilon states (branching/merging points).
    pub term: Option<NfaTerm>,
    /// Outgoing transitions from this state
    pub transitions: Vec<NfaTransition>,
    /// Source location in the schema for error reporting
    pub origin: Option<SourceRef>,
}

impl NfaState {
    /// Create a new state with no term (epsilon state)
    pub fn epsilon(id: StateId, origin: Option<SourceRef>) -> Self {
        Self {
            id,
            term: None,
            transitions: Vec::new(),
            origin,
        }
    }

    /// Create a new state with a term
    pub fn with_term(id: StateId, term: NfaTerm, origin: Option<SourceRef>) -> Self {
        Self {
            id,
            term: Some(term),
            transitions: Vec::new(),
            origin,
        }
    }

    /// Add a transition to this state
    pub fn add_transition(&mut self, target: StateId, kind: TransitionKind) {
        self.transitions.push(NfaTransition { target, kind });
    }

    /// Add an epsilon transition
    pub fn add_epsilon(&mut self, target: StateId) {
        self.add_transition(target, TransitionKind::Epsilon);
    }

    /// Add a consuming transition
    pub fn add_consume(&mut self, target: StateId) {
        self.add_transition(target, TransitionKind::Consume);
    }

    /// Check if this is an epsilon state (no term)
    pub fn is_epsilon(&self) -> bool {
        self.term.is_none()
    }

    /// Get epsilon transitions from this state
    pub fn epsilon_transitions(&self) -> impl Iterator<Item = StateId> + '_ {
        self.transitions
            .iter()
            .filter(|t| t.kind == TransitionKind::Epsilon)
            .map(|t| t.target)
    }

    /// Get consuming transitions from this state
    pub fn consuming_transitions(&self) -> impl Iterator<Item = StateId> + '_ {
        self.transitions
            .iter()
            .filter(|t| t.kind == TransitionKind::Consume)
            .map(|t| t.target)
    }
}

/// A term that can be matched in the NFA
///
/// Terms represent the actual content that must be matched during validation.
/// Each term corresponds to either a specific element or a wildcard pattern.
#[derive(Debug, Clone)]
pub enum NfaTerm {
    /// Match a specific element
    Element {
        /// Element local name (interned)
        name: NameId,
        /// Element namespace (None = no namespace)
        namespace: Option<NameId>,
        /// Resolved element key (for type lookup during validation)
        element_key: Option<ElementKey>,
        /// Resolved type for local elements (without element key)
        resolved_type: Option<TypeKey>,
    },
    /// Match any element satisfying wildcard constraints
    Wildcard {
        /// Namespace constraint for allowed namespaces
        namespace_constraint: NamespaceConstraint,
        /// How to process matched content
        process_contents: ProcessContents,
        /// Pre-expanded concrete QName exclusions (XSD 1.1 notQName)
        not_qnames: Vec<(Option<NameId>, NameId)>,
    },
}

impl NfaTerm {
    /// Create an element term
    pub fn element(name: NameId, namespace: Option<NameId>, element_key: Option<ElementKey>) -> Self {
        NfaTerm::Element {
            name,
            namespace,
            element_key,
            resolved_type: None,
        }
    }

    /// Create an element term with a resolved type
    pub fn element_with_type(
        name: NameId,
        namespace: Option<NameId>,
        element_key: Option<ElementKey>,
        resolved_type: Option<TypeKey>,
    ) -> Self {
        NfaTerm::Element {
            name,
            namespace,
            element_key,
            resolved_type,
        }
    }

    /// Create a wildcard term
    pub fn wildcard(namespace_constraint: NamespaceConstraint, process_contents: ProcessContents) -> Self {
        NfaTerm::Wildcard {
            namespace_constraint,
            process_contents,
            not_qnames: Vec::new(),
        }
    }

    /// Create a wildcard term with QName exclusions (XSD 1.1)
    pub fn wildcard_with_not_qnames(
        namespace_constraint: NamespaceConstraint,
        process_contents: ProcessContents,
        not_qnames: Vec<(Option<NameId>, NameId)>,
    ) -> Self {
        NfaTerm::Wildcard {
            namespace_constraint,
            process_contents,
            not_qnames,
        }
    }

    /// Check if this term is an element
    pub fn is_element(&self) -> bool {
        matches!(self, NfaTerm::Element { .. })
    }

    /// Check if this term is a wildcard
    pub fn is_wildcard(&self) -> bool {
        matches!(self, NfaTerm::Wildcard { .. })
    }
}

/// A transition between NFA states
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NfaTransition {
    /// Target state ID
    pub target: StateId,
    /// Transition type (epsilon or consuming)
    pub kind: TransitionKind,
}

impl NfaTransition {
    /// Create a new transition
    pub fn new(target: StateId, kind: TransitionKind) -> Self {
        Self { target, kind }
    }

    /// Create an epsilon transition
    pub fn epsilon(target: StateId) -> Self {
        Self::new(target, TransitionKind::Epsilon)
    }

    /// Create a consuming transition
    pub fn consume(target: StateId) -> Self {
        Self::new(target, TransitionKind::Consume)
    }
}

/// Type of transition between NFA states
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TransitionKind {
    /// Epsilon transition (no input consumed, always available)
    Epsilon,
    /// Consuming transition (requires matching the target state's term)
    Consume,
    /// Reset a counter to 0 (entering a counted region)
    CounterReset(CounterId),
    /// Increment a counter by 1 (completed one loop iteration)
    CounterIncrement(CounterId),
    /// Guard: pass only if counter < def.max (loop back)
    CounterMaxGuard(CounterId),
    /// Guard: pass only if counter >= def.min (exit loop)
    CounterMinGuard(CounterId),
}

impl TransitionKind {
    /// Check if this transition is epsilon-like (does not consume input).
    /// All counter transitions are epsilon-like; only `Consume` is not.
    pub fn is_epsilon_like(&self) -> bool {
        !matches!(self, TransitionKind::Consume)
    }

    /// Offset counter IDs by the given amount (for fragment/table composition).
    /// Returns self unchanged for non-counter transitions.
    pub fn offset_counter(self, offset: CounterId) -> Self {
        if offset == 0 {
            return self;
        }
        match self {
            TransitionKind::CounterReset(c) => TransitionKind::CounterReset(c + offset),
            TransitionKind::CounterIncrement(c) => TransitionKind::CounterIncrement(c + offset),
            TransitionKind::CounterMaxGuard(c) => TransitionKind::CounterMaxGuard(c + offset),
            TransitionKind::CounterMinGuard(c) => TransitionKind::CounterMinGuard(c + offset),
            other => other,
        }
    }
}

/// Compute the epsilon closure for a set of start states.
///
/// Only follows `TransitionKind::Epsilon` transitions. For NFAs with counter
/// transitions, use `ActiveStates` methods instead — this function will miss
/// states reachable via counter transitions.
pub fn epsilon_closure(
    nfa: &NfaTable,
    start_states: impl IntoIterator<Item = StateId>,
) -> HashSet<StateId> {
    debug_assert!(!nfa.has_counters(), "epsilon_closure called on counted NFA; use ActiveStates instead");
    let mut closure = HashSet::new();
    let mut stack: Vec<StateId> = start_states.into_iter().collect();

    while let Some(state_id) = stack.pop() {
        if !closure.insert(state_id) {
            continue;
        }
        if let Some(state) = nfa.get_state(state_id) {
            for target in state.epsilon_transitions() {
                if !closure.contains(&target) {
                    stack.push(target);
                }
            }
        }
    }

    closure
}

/// Match an element name against an NfaTerm with optional substitution groups.
pub fn term_matches(
    term: &NfaTerm,
    element_name: NameId,
    element_namespace: Option<NameId>,
    target_namespace: Option<NameId>,
    substitution_groups: Option<&SubstitutionGroupMap>,
    xsd_version: XsdVersion,
) -> bool {
    match term {
        NfaTerm::Element {
            name,
            namespace,
            element_key,
            ..
        } => {
            if let (Some(map), Some(key)) = (substitution_groups, element_key) {
                if let Some(names) = map.get(key) {
                    return names.contains(&(element_name, element_namespace));
                }
            }
            *name == element_name && *namespace == element_namespace
        }
        NfaTerm::Wildcard {
            namespace_constraint,
            not_qnames,
            ..
        } => {
            if !namespace_constraint.matches(element_namespace, target_namespace, xsd_version) {
                return false;
            }
            !not_qnames_exclude(not_qnames, element_namespace, element_name)
        }
    }
}

/// Advance NFA states by matching an element and applying epsilon closure.
///
/// Only handles plain epsilon transitions. For NFAs with counter transitions,
/// use `ActiveStates::advance` instead.
pub fn advance_states(
    nfa: &NfaTable,
    start_states: impl IntoIterator<Item = StateId>,
    element_name: NameId,
    element_namespace: Option<NameId>,
    target_namespace: Option<NameId>,
    substitution_groups: Option<&SubstitutionGroupMap>,
    xsd_version: XsdVersion,
) -> HashSet<StateId> {
    let closure = epsilon_closure(nfa, start_states);
    let mut next = HashSet::new();

    for state_id in closure {
        let state = match nfa.get_state(state_id) {
            Some(state) => state,
            None => continue,
        };
        let term = match state.term.as_ref() {
            Some(term) => term,
            None => continue,
        };

        if term_matches(
            term,
            element_name,
            element_namespace,
            target_namespace,
            substitution_groups,
            xsd_version,
        ) {
            for target in state.consuming_transitions() {
                next.insert(target);
            }
        }
    }

    epsilon_closure(nfa, next)
}

/// Advance NFA states with element-over-wildcard priority (XSD 1.1).
///
/// Only handles plain epsilon transitions. For NFAs with counter transitions,
/// use `ActiveStates::advance_with_priority` instead.
pub fn advance_with_priority(
    nfa: &NfaTable,
    start_states: impl IntoIterator<Item = StateId>,
    element_name: NameId,
    element_namespace: Option<NameId>,
    target_namespace: Option<NameId>,
    substitution_groups: Option<&SubstitutionGroupMap>,
    xsd_version: XsdVersion,
) -> HashSet<StateId> {
    let closure = epsilon_closure(nfa, start_states);
    let mut element_targets = HashSet::new();
    let mut wildcard_targets = HashSet::new();

    for state_id in closure {
        let state = match nfa.get_state(state_id) {
            Some(state) => state,
            None => continue,
        };
        let term = match state.term.as_ref() {
            Some(term) => term,
            None => continue,
        };

        if !term_matches(
            term,
            element_name,
            element_namespace,
            target_namespace,
            substitution_groups,
            xsd_version,
        ) {
            continue;
        }

        match term {
            NfaTerm::Element { .. } => {
                for target in state.consuming_transitions() {
                    element_targets.insert(target);
                }
            }
            NfaTerm::Wildcard { .. } => {
                for target in state.consuming_transitions() {
                    wildcard_targets.insert(target);
                }
            }
        }
    }

    let next = if !element_targets.is_empty() {
        element_targets
    } else {
        wildcard_targets
    };

    epsilon_closure(nfa, next)
}

// ---------------------------------------------------------------------------
// Counter-aware runtime types
// ---------------------------------------------------------------------------

/// A single NFA configuration with counter values.
///
/// Represents one "thread" in the NFA simulation where each counter
/// has a specific value. Two configs at the same state but different
/// counter values are distinct.
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct ActiveConfig {
    pub state_id: StateId,
    pub counters: Box<[u32]>,
}

impl ActiveConfig {
    /// Create an initial config at the given state with all counters at 0.
    pub fn initial(state_id: StateId, num_counters: usize) -> Self {
        Self {
            state_id,
            counters: vec![0; num_counters].into_boxed_slice(),
        }
    }

    /// Clone this config with a different state ID.
    fn with_state(&self, new_state: StateId) -> Self {
        Self {
            state_id: new_state,
            counters: self.counters.clone(),
        }
    }

    /// Clone this config with a different state ID and a modified counter.
    fn with_counter_set(&self, new_state: StateId, counter_id: CounterId, value: u32) -> Self {
        let mut new = self.with_state(new_state);
        new.counters[counter_id as usize] = value;
        new
    }
}

/// Key for the `Hybrid` active-state variant.
///
/// Structurally identical to `ActiveConfig` but the slot at `ranged_counter_idx`
/// is **always 0** (sentinel).  All constructors and setters enforce this invariant,
/// so derived `Hash`/`Eq` work correctly without a custom impl.
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct HybridKey {
    state_id: StateId,
    counters: Box<[u32]>,
}

impl HybridKey {
    /// Create a key with all counters at 0 (including the ranged slot).
    fn initial(state_id: StateId, num_counters: usize) -> Self {
        Self {
            state_id,
            counters: vec![0; num_counters].into_boxed_slice(),
        }
    }

    /// Clone with a different state, same counter values.
    fn with_state(&self, new_state: StateId) -> Self {
        Self {
            state_id: new_state,
            counters: self.counters.clone(),
        }
    }

    /// Clone with a modified **scalar** counter.
    ///
    /// Panics (debug) if `counter_id` is the ranged counter.
    fn with_scalar_counter(
        &self,
        new_state: StateId,
        counter_id: CounterId,
        value: u32,
        ranged_counter_idx: usize,
    ) -> Self {
        debug_assert!(
            counter_id as usize != ranged_counter_idx,
            "with_scalar_counter called on ranged counter {counter_id}"
        );
        let mut new = self.with_state(new_state);
        new.counters[counter_id as usize] = value;
        new
    }

    /// Read a counter slot value.
    fn counter(&self, counter_id: CounterId) -> u32 {
        self.counters[counter_id as usize]
    }

    /// Repack for dynamic counter switching.
    ///
    /// Precondition: old ranged counter is dead (`[0,0]` in all configs).
    /// Writes 0 into the old ranged slot, zeros the new ranged slot (sentinel).
    /// Returns `(new_key, extracted_scalar_value)` where `extracted_scalar_value`
    /// is the value that was in the new ranged slot (to become a `CounterRange`).
    fn repacked(
        &self,
        old_ranged_idx: usize,
        new_ranged_idx: usize,
    ) -> (Self, u32) {
        let scalar_val = self.counters[new_ranged_idx];
        let mut new_counters = self.counters.clone();
        new_counters[old_ranged_idx] = 0;
        new_counters[new_ranged_idx] = 0; // new sentinel
        (Self {
            state_id: self.state_id,
            counters: new_counters,
        }, scalar_val)
    }
}

/// Ranged epsilon closure for single-counter NFAs with nullable body.
///
/// Uses a `HashMap<StateId, CounterRange>` merge-map where each state has
/// exactly one contiguous counter range.  On `CounterIncrement`, the fast-
/// forward sets `hi = counter_def.max`, so the closure converges in O(states)
/// worklist iterations instead of O(max).
///
/// **Contiguity invariant**: every counter range at a state is a single
/// contiguous interval.  Reset→[0,0], increment shifts +1, guards clip one
/// side, fast-forward extends hi — all preserve contiguity.  Ranges arriving
/// at the same state from different paths are always adjacent or overlapping
/// (because the counter increments by exactly 1 per iteration), so union
/// preserves contiguity.
fn ranged_single_epsilon_closure(
    nfa: &NfaTable,
    seeds: HashMap<StateId, CounterRange>,
    counter_def: CounterDef,
) -> ActiveStates {
    let mut map: HashMap<StateId, CounterRange> = HashMap::new();
    let mut worklist: VecDeque<(StateId, CounterRange)> = seeds.into_iter().collect();

    while let Some((state_id, range)) = worklist.pop_front() {
        // Check subsumption: if existing range already covers this one, skip.
        if let Some(&existing) = map.get(&state_id) {
            if existing.subsumes(range) {
                continue;
            }
            // Merge (union) — contiguity invariant guarantees this is safe.
            let merged = existing.union(range);
            debug_assert!(merged.lo <= merged.hi, "contiguity invariant violated at state {state_id}");
            map.insert(state_id, merged);
        } else {
            map.insert(state_id, range);
        }

        // Process transitions from this state.
        if let Some(state) = nfa.get_state(state_id) {
            for trans in &state.transitions {
                let next = match trans.kind {
                    TransitionKind::Epsilon => Some(range),
                    TransitionKind::CounterReset(_) => Some(CounterRange::single(0)),
                    TransitionKind::CounterIncrement(_) => {
                        // Fast-forward: body is nullable, so the counter can
                        // reach max via repeated empty body traversals.
                        Some(CounterRange::new(range.lo + 1, counter_def.max))
                    }
                    TransitionKind::CounterMaxGuard(_) => {
                        let clamped = range.intersect_below(counter_def.max);
                        if clamped.is_empty() { None } else { Some(clamped) }
                    }
                    TransitionKind::CounterMinGuard(_) => {
                        let passed = range.intersect_above(counter_def.min);
                        if passed.is_empty() {
                            None
                        } else {
                            // Canonicalize: counter is dead after exit.
                            Some(CounterRange::single(0))
                        }
                    }
                    TransitionKind::Consume => None,
                };
                if let Some(next_range) = next {
                    // Only push if not already subsumed by existing map entry.
                    let dominated = map.get(&trans.target)
                        .is_some_and(|r| r.subsumes(next_range));
                    if !dominated {
                        worklist.push_back((trans.target, next_range));
                    }
                }
            }
        }
    }

    ActiveStates::RangedSingle { state_ranges: map, counter_def }
}

/// Hybrid epsilon closure for multi-counter NFAs with one ranged counter.
///
/// Combines the `ranged_single_epsilon_closure` fast-forward for the ranged
/// counter with scalar operations for all other counters.  The `HybridKey`
/// captures (state, scalar counter values) while the ranged counter is stored
/// as a `CounterRange` map value.
///
/// **Contiguity invariant** holds per-`HybridKey`: for a fixed set of scalar
/// counter values, the ranged counter progresses identically to `RangedSingle`.
fn hybrid_epsilon_closure(
    nfa: &NfaTable,
    seeds: HashMap<HybridKey, CounterRange>,
    ranged_counter_idx: usize,
    num_counters: usize,
) -> ActiveStates {
    let ranged_id = ranged_counter_idx as CounterId;
    let ranged_def = nfa.counter_defs[ranged_counter_idx];

    let mut map: HashMap<HybridKey, CounterRange> = HashMap::new();
    let mut worklist: VecDeque<(HybridKey, CounterRange)> = seeds.into_iter().collect();

    while let Some((key, range)) = worklist.pop_front() {
        // Subsumption check: if existing range already covers this one, skip.
        if let Some(&existing) = map.get(&key) {
            if existing.subsumes(range) {
                continue;
            }
            let merged = existing.union(range);
            debug_assert!(
                merged.lo <= merged.hi,
                "contiguity invariant violated at state {}",
                key.state_id
            );
            map.insert(key.clone(), merged);
        } else {
            map.insert(key.clone(), range);
        }

        if let Some(state) = nfa.get_state(key.state_id) {
            for trans in &state.transitions {
                let next: Option<(HybridKey, CounterRange)> = match trans.kind {
                    TransitionKind::Epsilon => {
                        Some((key.with_state(trans.target), range))
                    }

                    // --- Ranged counter operations (range arithmetic) ---
                    TransitionKind::CounterReset(c) if c == ranged_id => {
                        Some((key.with_state(trans.target), CounterRange::single(0)))
                    }
                    TransitionKind::CounterIncrement(c) if c == ranged_id => {
                        // Fast-forward: body is nullable, so counter can reach max.
                        Some((
                            key.with_state(trans.target),
                            CounterRange::new(range.lo + 1, ranged_def.max),
                        ))
                    }
                    TransitionKind::CounterMaxGuard(c) if c == ranged_id => {
                        let clamped = range.intersect_below(ranged_def.max);
                        if clamped.is_empty() { None } else {
                            Some((key.with_state(trans.target), clamped))
                        }
                    }
                    TransitionKind::CounterMinGuard(c) if c == ranged_id => {
                        let passed = range.intersect_above(ranged_def.min);
                        if passed.is_empty() {
                            None
                        } else {
                            // Canonicalize: ranged counter is dead after exit.
                            Some((key.with_state(trans.target), CounterRange::single(0)))
                        }
                    }

                    // --- Scalar counter operations ---
                    TransitionKind::CounterReset(c) => {
                        Some((
                            key.with_scalar_counter(trans.target, c, 0, ranged_counter_idx),
                            range,
                        ))
                    }
                    TransitionKind::CounterIncrement(c) => {
                        let val = key.counter(c) + 1;
                        Some((
                            key.with_scalar_counter(trans.target, c, val, ranged_counter_idx),
                            range,
                        ))
                    }
                    TransitionKind::CounterMaxGuard(c) => {
                        if key.counter(c) < nfa.counter_defs[c as usize].max {
                            Some((key.with_state(trans.target), range))
                        } else {
                            None
                        }
                    }
                    TransitionKind::CounterMinGuard(c) => {
                        if key.counter(c) >= nfa.counter_defs[c as usize].min {
                            // Canonicalize scalar counter on exit.
                            Some((
                                key.with_scalar_counter(trans.target, c, 0, ranged_counter_idx),
                                range,
                            ))
                        } else {
                            None
                        }
                    }

                    TransitionKind::Consume => None,
                };

                if let Some((next_key, next_range)) = next {
                    let dominated = map.get(&next_key)
                        .is_some_and(|r| r.subsumes(next_range));
                    if !dominated {
                        worklist.push_back((next_key, next_range));
                    }
                }
            }
        }
    }

    let result = ActiveStates::Hybrid {
        configs: map,
        ranged_counter_idx,
        num_counters,
    };
    result.maybe_switch_ranged_counter(nfa)
}

/// Quad-path active state set for NFA simulation.
///
/// **Invariant**: values are always closure-closed (epsilon closure has been
/// applied). All constructors and advance methods enforce this.
///
/// - `Simple`: counter-free NFA — delegates to existing `HashSet<StateId>` functions.
/// - `Counted`: NFA with multiple counters — tracks `HashSet<ActiveConfig>` (scalar).
/// - `RangedSingle`: single counter with nullable body — stores one `CounterRange`
///   per reachable state, converging epsilon closure in O(states) instead of O(N).
/// - `Hybrid`: multiple counters, one ranged (fast-forward) + others scalar.
///   Collapses the ranged counter's dimension; cost proportional to remaining scalar dims.
#[derive(Debug, Clone)]
pub enum ActiveStates {
    /// Fast path: no counters in this NFA (bit-identical to old code)
    Simple(HashSet<StateId>),
    /// Scalar counted path: configurations carry individual counter values
    Counted {
        configs: HashSet<ActiveConfig>,
        num_counters: usize,
    },
    /// Optimized path for single-counter NFAs with nullable loop body.
    /// Each reachable state maps to one `CounterRange` (contiguity invariant).
    RangedSingle {
        state_ranges: HashMap<StateId, CounterRange>,
        counter_def: CounterDef,
    },
    /// Hybrid path: one counter ranged (fast-forward), others scalar.
    /// The ranged counter's slot in every `HybridKey` is always 0 (sentinel);
    /// the actual ranged value lives in the `CounterRange` map value.
    Hybrid {
        configs: HashMap<HybridKey, CounterRange>,
        ranged_counter_idx: usize,
        num_counters: usize,
    },
}

impl ActiveStates {
    /// Create initial active states from an NFA, picking the right variant.
    pub fn from_nfa(nfa: &NfaTable) -> Self {
        use super::particle::COUNTED_THRESHOLD;

        if nfa.has_counters() {
            // Single counter with nullable body → optimized ranged path
            if nfa.counter_defs.len() == 1 && nfa.counter_defs[0].body_nullable {
                let mut state_ranges = HashMap::new();
                state_ranges.insert(nfa.start_state, CounterRange::single(0));
                let result = ActiveStates::RangedSingle {
                    state_ranges,
                    counter_def: nfa.counter_defs[0],
                };
                return result.epsilon_closure(nfa);
            }

            // Multiple counters: check for a nullable counter worth ranging.
            // Pick the nullable counter with the largest max — that collapses
            // the most configurations.  Only use Hybrid if max > COUNTED_THRESHOLD
            // (below that, Counted is cheap enough and we avoid map overhead).
            // On equal max, prefer the lowest index — for sequential loops this
            // ranges the first loop's counter, keeping later counters at scalar 0.
            if nfa.counter_defs.len() > 1 {
                let best_nullable = nfa.counter_defs.iter()
                    .enumerate()
                    .filter(|(_, def)| def.body_nullable)
                    .max_by_key(|(idx, def)| (def.max, std::cmp::Reverse(*idx)));

                if let Some((ranged_idx, def)) = best_nullable {
                    if def.max > COUNTED_THRESHOLD {
                        let num_counters = nfa.counter_defs.len();
                        let initial_key = HybridKey::initial(nfa.start_state, num_counters);
                        let mut configs = HashMap::new();
                        configs.insert(initial_key, CounterRange::single(0));
                        let result = ActiveStates::Hybrid {
                            configs,
                            ranged_counter_idx: ranged_idx,
                            num_counters,
                        };
                        return result.epsilon_closure(nfa);
                    }
                }
            }

            // Fall through to scalar Counted path
            let initial = ActiveConfig::initial(nfa.start_state, nfa.counter_defs.len());
            let mut configs = HashSet::new();
            configs.insert(initial);
            let result = ActiveStates::Counted {
                configs,
                num_counters: nfa.counter_defs.len(),
            };
            result.epsilon_closure(nfa)
        } else {
            let simple = epsilon_closure(nfa, std::iter::once(nfa.start_state));
            ActiveStates::Simple(simple)
        }
    }

    /// Check if the active set is empty (no reachable states).
    pub fn is_empty(&self) -> bool {
        match self {
            ActiveStates::Simple(s) => s.is_empty(),
            ActiveStates::Counted { configs, .. } => configs.is_empty(),
            ActiveStates::RangedSingle { state_ranges, .. } => state_ranges.is_empty(),
            ActiveStates::Hybrid { configs, .. } => configs.is_empty(),
        }
    }

    /// Check if any active state is the NFA accept state.
    pub fn contains_accept(&self, nfa: &NfaTable) -> bool {
        match self {
            ActiveStates::Simple(s) => s.iter().any(|&id| nfa.is_accept(id)),
            ActiveStates::Counted { configs, .. } => {
                configs.iter().any(|c| nfa.is_accept(c.state_id))
            }
            ActiveStates::RangedSingle { state_ranges, .. } => {
                state_ranges.contains_key(&nfa.accept_state)
            }
            ActiveStates::Hybrid { configs, .. } => {
                configs.keys().any(|k| nfa.is_accept(k.state_id))
            }
        }
    }

    /// Compute epsilon closure (including counter transitions for Counted path).
    pub fn epsilon_closure(self, nfa: &NfaTable) -> Self {
        match self {
            ActiveStates::Simple(states) => {
                ActiveStates::Simple(epsilon_closure(nfa, states))
            }
            ActiveStates::Counted { configs, num_counters } => {
                let mut result: HashSet<ActiveConfig> = HashSet::new();
                let mut stack: Vec<ActiveConfig> = configs.into_iter().collect();

                while let Some(config) = stack.pop() {
                    if !result.insert(config.clone()) {
                        continue;
                    }
                    if let Some(state) = nfa.get_state(config.state_id) {
                        for trans in &state.transitions {
                            let next = match trans.kind {
                                TransitionKind::Epsilon => {
                                    Some(config.with_state(trans.target))
                                }
                                TransitionKind::CounterReset(c) => {
                                    Some(config.with_counter_set(trans.target, c, 0))
                                }
                                TransitionKind::CounterIncrement(c) => {
                                    let val = config.counters[c as usize] + 1;
                                    Some(config.with_counter_set(trans.target, c, val))
                                }
                                TransitionKind::CounterMaxGuard(c) => {
                                    if config.counters[c as usize] < nfa.counter_defs[c as usize].max {
                                        Some(config.with_state(trans.target))
                                    } else {
                                        None
                                    }
                                }
                                TransitionKind::CounterMinGuard(c) => {
                                    if config.counters[c as usize] >= nfa.counter_defs[c as usize].min {
                                        // Canonicalize: zero out counter on exit to collapse
                                        // configs that left the loop at different counter values.
                                        Some(config.with_counter_set(trans.target, c, 0))
                                    } else {
                                        None
                                    }
                                }
                                TransitionKind::Consume => None,
                            };
                            if let Some(next_config) = next {
                                if !result.contains(&next_config) {
                                    stack.push(next_config);
                                }
                            }
                        }
                    }
                }
                ActiveStates::Counted { configs: result, num_counters }
            }
            ActiveStates::RangedSingle { state_ranges, counter_def } => {
                ranged_single_epsilon_closure(nfa, state_ranges, counter_def)
            }
            ActiveStates::Hybrid { configs, ranged_counter_idx, num_counters } => {
                hybrid_epsilon_closure(nfa, configs, ranged_counter_idx, num_counters)
            }
        }
    }

    /// After epsilon closure, check if the current ranged counter is dead
    /// (`[0,0]` in ALL configs — the canonical exit value from `CounterMinGuard`).
    /// If a better nullable counter exists and the repack produces contiguous
    /// ranges, switch the ranged counter.
    ///
    /// The repacked map is already epsilon-closed — no re-closure needed, since
    /// the same set of (state, counter-values) is preserved, just re-encoded.
    fn maybe_switch_ranged_counter(self, nfa: &NfaTable) -> Self {
        use super::particle::COUNTED_THRESHOLD;

        let ActiveStates::Hybrid { configs, ranged_counter_idx, num_counters } = self else {
            return self;
        };

        let dead = CounterRange::single(0);
        if configs.is_empty() || !configs.values().all(|r| *r == dead) {
            return ActiveStates::Hybrid { configs, ranged_counter_idx, num_counters };
        }

        // Single pass: collect per-counter min/max across all configs.
        let nc = nfa.counter_defs.len();
        let mut min_vals = vec![u32::MAX; nc];
        let mut max_vals = vec![0u32; nc];
        for key in configs.keys() {
            for idx in 0..nc {
                let v = key.counter(idx as CounterId);
                min_vals[idx] = min_vals[idx].min(v);
                max_vals[idx] = max_vals[idx].max(v);
            }
        }

        // Pick the best candidate: nullable, large max, widest spread.
        // Spread (max - min + 1) is an upper bound on distinct values;
        // the contiguity guard below rejects non-contiguous cases.
        let mut best: Option<(usize, u32, u32)> = None; // (idx, spread, max)
        for (idx, def) in nfa.counter_defs.iter().enumerate() {
            if idx == ranged_counter_idx || !def.body_nullable || def.max <= COUNTED_THRESHOLD {
                continue;
            }
            if min_vals[idx] == max_vals[idx] {
                continue;
            }
            let spread = max_vals[idx] - min_vals[idx] + 1;
            let dominated = best.is_some_and(|(_, bs, bm)| {
                spread < bs || (spread == bs && def.max <= bm)
            });
            if !dominated {
                best = Some((idx, spread, def.max));
            }
        }

        let Some((new_ranged_idx, _, _)) = best else {
            return ActiveStates::Hybrid { configs, ranged_counter_idx, num_counters };
        };

        // Guarded repack: track (range, count) per key, then verify that
        // range width == count (no phantom values in the interval).
        let mut new_configs: HashMap<HybridKey, (CounterRange, u32)> =
            HashMap::with_capacity(configs.len());
        for old_key in configs.keys() {
            let (new_key, scalar_val) =
                old_key.repacked(ranged_counter_idx, new_ranged_idx);
            let new_range = CounterRange::single(scalar_val);
            new_configs.entry(new_key)
                .and_modify(|(r, count)| { *r = r.union(new_range); *count += 1; })
                .or_insert((new_range, 1));
        }

        for (range, count) in new_configs.values() {
            if range.hi - range.lo + 1 != *count {
                return ActiveStates::Hybrid { configs, ranged_counter_idx, num_counters };
            }
        }

        let repacked = new_configs.into_iter()
            .map(|(k, (r, _))| (k, r))
            .collect();

        ActiveStates::Hybrid {
            configs: repacked,
            ranged_counter_idx: new_ranged_idx,
            num_counters,
        }
    }

    /// Advance NFA states by matching an element (XSD 1.0 — no priority).
    pub fn advance(
        self,
        nfa: &NfaTable,
        element_name: NameId,
        element_namespace: Option<NameId>,
        target_namespace: Option<NameId>,
        substitution_groups: Option<&SubstitutionGroupMap>,
        xsd_version: XsdVersion,
    ) -> Self {
        match self {
            ActiveStates::Simple(states) => {
                ActiveStates::Simple(advance_states(
                    nfa, states, element_name, element_namespace,
                    target_namespace, substitution_groups, xsd_version,
                ))
            }
            ActiveStates::Counted { configs, num_counters } => {
                // Configs are already closure-closed (invariant).
                // Find matching terms and follow Consume transitions.
                let mut next_configs = HashSet::new();
                for config in &configs {
                    if let Some(state) = nfa.get_state(config.state_id) {
                        if let Some(ref term_val) = state.term {
                            if term_matches(term_val, element_name, element_namespace,
                                target_namespace, substitution_groups, xsd_version) {
                                for trans in &state.transitions {
                                    if trans.kind == TransitionKind::Consume {
                                        next_configs.insert(config.with_state(trans.target));
                                    }
                                }
                            }
                        }
                    }
                }
                let result = ActiveStates::Counted { configs: next_configs, num_counters };
                result.epsilon_closure(nfa)
            }
            ActiveStates::RangedSingle { state_ranges, counter_def } => {
                let mut next_seeds: HashMap<StateId, CounterRange> = HashMap::new();
                for (&state_id, &range) in &state_ranges {
                    if let Some(state) = nfa.get_state(state_id) {
                        if let Some(ref term_val) = state.term {
                            if term_matches(term_val, element_name, element_namespace,
                                target_namespace, substitution_groups, xsd_version) {
                                for trans in &state.transitions {
                                    if trans.kind == TransitionKind::Consume {
                                        next_seeds.entry(trans.target)
                                            .and_modify(|r| *r = r.union(range))
                                            .or_insert(range);
                                    }
                                }
                            }
                        }
                    }
                }
                let result = ActiveStates::RangedSingle { state_ranges: next_seeds, counter_def };
                result.epsilon_closure(nfa)
            }
            ActiveStates::Hybrid { configs, ranged_counter_idx, num_counters } => {
                let mut next_configs: HashMap<HybridKey, CounterRange> = HashMap::new();
                for (key, &range) in &configs {
                    if let Some(state) = nfa.get_state(key.state_id) {
                        if let Some(ref term_val) = state.term {
                            if term_matches(term_val, element_name, element_namespace,
                                target_namespace, substitution_groups, xsd_version) {
                                for trans in &state.transitions {
                                    if trans.kind == TransitionKind::Consume {
                                        let new_key = key.with_state(trans.target);
                                        next_configs.entry(new_key)
                                            .and_modify(|r| *r = r.union(range))
                                            .or_insert(range);
                                    }
                                }
                            }
                        }
                    }
                }
                let result = ActiveStates::Hybrid {
                    configs: next_configs, ranged_counter_idx, num_counters,
                };
                result.epsilon_closure(nfa)
            }
        }
    }

    /// Advance NFA states with element-over-wildcard priority (XSD 1.1).
    pub fn advance_with_priority(
        self,
        nfa: &NfaTable,
        element_name: NameId,
        element_namespace: Option<NameId>,
        target_namespace: Option<NameId>,
        substitution_groups: Option<&SubstitutionGroupMap>,
        xsd_version: XsdVersion,
    ) -> Self {
        match self {
            ActiveStates::Simple(states) => {
                ActiveStates::Simple(advance_with_priority(
                    nfa, states, element_name, element_namespace,
                    target_namespace, substitution_groups, xsd_version,
                ))
            }
            ActiveStates::Counted { configs, num_counters } => {
                let mut element_configs = HashSet::new();
                let mut wildcard_configs = HashSet::new();

                for config in &configs {
                    if let Some(state) = nfa.get_state(config.state_id) {
                        if let Some(ref term_val) = state.term {
                            if term_matches(term_val, element_name, element_namespace,
                                target_namespace, substitution_groups, xsd_version) {
                                let target_set = match term_val {
                                    NfaTerm::Element { .. } => &mut element_configs,
                                    NfaTerm::Wildcard { .. } => &mut wildcard_configs,
                                };
                                for trans in &state.transitions {
                                    if trans.kind == TransitionKind::Consume {
                                        target_set.insert(config.with_state(trans.target));
                                    }
                                }
                            }
                        }
                    }
                }

                let next = if !element_configs.is_empty() {
                    element_configs
                } else {
                    wildcard_configs
                };

                let result = ActiveStates::Counted { configs: next, num_counters };
                result.epsilon_closure(nfa)
            }
            ActiveStates::RangedSingle { state_ranges, counter_def } => {
                let mut element_seeds: HashMap<StateId, CounterRange> = HashMap::new();
                let mut wildcard_seeds: HashMap<StateId, CounterRange> = HashMap::new();

                for (&state_id, &range) in &state_ranges {
                    if let Some(state) = nfa.get_state(state_id) {
                        if let Some(ref term_val) = state.term {
                            if term_matches(term_val, element_name, element_namespace,
                                target_namespace, substitution_groups, xsd_version) {
                                let target_map = match term_val {
                                    NfaTerm::Element { .. } => &mut element_seeds,
                                    NfaTerm::Wildcard { .. } => &mut wildcard_seeds,
                                };
                                for trans in &state.transitions {
                                    if trans.kind == TransitionKind::Consume {
                                        target_map.entry(trans.target)
                                            .and_modify(|r| *r = r.union(range))
                                            .or_insert(range);
                                    }
                                }
                            }
                        }
                    }
                }

                let next = if !element_seeds.is_empty() {
                    element_seeds
                } else {
                    wildcard_seeds
                };

                let result = ActiveStates::RangedSingle { state_ranges: next, counter_def };
                result.epsilon_closure(nfa)
            }
            ActiveStates::Hybrid { configs, ranged_counter_idx, num_counters } => {
                let mut element_configs: HashMap<HybridKey, CounterRange> = HashMap::new();
                let mut wildcard_configs: HashMap<HybridKey, CounterRange> = HashMap::new();

                for (key, &range) in &configs {
                    if let Some(state) = nfa.get_state(key.state_id) {
                        if let Some(ref term_val) = state.term {
                            if term_matches(term_val, element_name, element_namespace,
                                target_namespace, substitution_groups, xsd_version) {
                                let target_map = match term_val {
                                    NfaTerm::Element { .. } => &mut element_configs,
                                    NfaTerm::Wildcard { .. } => &mut wildcard_configs,
                                };
                                for trans in &state.transitions {
                                    if trans.kind == TransitionKind::Consume {
                                        let new_key = key.with_state(trans.target);
                                        target_map.entry(new_key)
                                            .and_modify(|r| *r = r.union(range))
                                            .or_insert(range);
                                    }
                                }
                            }
                        }
                    }
                }

                let next = if !element_configs.is_empty() {
                    element_configs
                } else {
                    wildcard_configs
                };

                let result = ActiveStates::Hybrid {
                    configs: next, ranged_counter_idx, num_counters,
                };
                result.epsilon_closure(nfa)
            }
        }
    }

    /// Find the matching term info from active states (for content validation).
    ///
    /// Returns the element key and resolved type of the first matching term.
    /// Since ActiveStates is closure-closed, all reachable term states are
    /// already present — no additional epsilon closure needed for Counted path.
    pub fn find_match_info(
        &self,
        nfa: &NfaTable,
        name: NameId,
        namespace: Option<NameId>,
        target_ns: Option<NameId>,
        subst_groups: Option<&SubstitutionGroupMap>,
        xsd_version: XsdVersion,
    ) -> MatchInfo {
        match self {
            ActiveStates::Simple(states) => {
                // Delegate to existing function (it does epsilon_closure internally,
                // which is redundant but harmless since states are already closed)
                let closure = epsilon_closure(nfa, states.iter().copied());
                find_match_info_in_states(nfa, closure.iter().copied(), name, namespace, target_ns, subst_groups, xsd_version)
            }
            ActiveStates::Counted { configs, .. } => {
                // Configs are closure-closed — iterate directly
                find_match_info_in_states(nfa, configs.iter().map(|c| c.state_id), name, namespace, target_ns, subst_groups, xsd_version)
            }
            ActiveStates::RangedSingle { state_ranges, .. } => {
                find_match_info_in_states(nfa, state_ranges.keys().copied(), name, namespace, target_ns, subst_groups, xsd_version)
            }
            ActiveStates::Hybrid { configs, .. } => {
                find_match_info_in_states(nfa, configs.keys().map(|k| k.state_id), name, namespace, target_ns, subst_groups, xsd_version)
            }
        }
    }

    /// Collect expected element terms from reachable states (for error messages).
    ///
    /// Returns (local_name, namespace, element_key) for each reachable Element term.
    pub fn expected_element_terms(&self, nfa: &NfaTable) -> Vec<(NameId, Option<NameId>, Option<ElementKey>)> {
        let mut result = Vec::new();
        match self {
            ActiveStates::Simple(states) => {
                let closure = epsilon_closure(nfa, states.iter().copied());
                for state_id in closure {
                    if let Some(state) = nfa.get_state(state_id) {
                        if let Some(NfaTerm::Element { name, namespace, element_key, .. }) = &state.term {
                            result.push((*name, *namespace, *element_key));
                        }
                    }
                }
            }
            ActiveStates::Counted { configs, .. } => {
                let mut seen = HashSet::new();
                for config in configs {
                    if !seen.insert(config.state_id) {
                        continue; // Skip duplicate state IDs
                    }
                    if let Some(state) = nfa.get_state(config.state_id) {
                        if let Some(NfaTerm::Element { name, namespace, element_key, .. }) = &state.term {
                            result.push((*name, *namespace, *element_key));
                        }
                    }
                }
            }
            ActiveStates::RangedSingle { state_ranges, .. } => {
                for &state_id in state_ranges.keys() {
                    if let Some(state) = nfa.get_state(state_id) {
                        if let Some(NfaTerm::Element { name, namespace, element_key, .. }) = &state.term {
                            result.push((*name, *namespace, *element_key));
                        }
                    }
                }
            }
            ActiveStates::Hybrid { configs, .. } => {
                let mut seen = HashSet::new();
                for key in configs.keys() {
                    if !seen.insert(key.state_id) {
                        continue;
                    }
                    if let Some(state) = nfa.get_state(key.state_id) {
                        if let Some(NfaTerm::Element { name, namespace, element_key, .. }) = &state.term {
                            result.push((*name, *namespace, *element_key));
                        }
                    }
                }
            }
        }
        result
    }
}

/// Match info returned from term lookup in active states.
#[derive(Debug, Clone, Copy, Default)]
pub struct MatchInfo {
    pub element_key: Option<ElementKey>,
    pub resolved_type: Option<TypeKey>,
    pub process_contents: Option<ProcessContents>,
}

/// Find match info from an iterator of state IDs.
///
/// Uses element-over-wildcard priority: if any Element term matches, its info
/// is returned immediately; a Wildcard match is kept as a fallback and only
/// returned when no Element matches.  This mirrors the priority rule in
/// `advance_with_priority`.
fn find_match_info_in_states(
    nfa: &NfaTable,
    state_ids: impl Iterator<Item = StateId>,
    name: NameId,
    namespace: Option<NameId>,
    target_ns: Option<NameId>,
    subst_groups: Option<&SubstitutionGroupMap>,
    xsd_version: XsdVersion,
) -> MatchInfo {
    let mut wildcard_info: Option<MatchInfo> = None;

    for state_id in state_ids {
        if let Some(state) = nfa.get_state(state_id) {
            if let Some(ref term) = state.term {
                if term_matches(term, name, namespace, target_ns, subst_groups, xsd_version) {
                    match term {
                        NfaTerm::Element {
                            name: term_name,
                            namespace: term_ns,
                            element_key,
                            resolved_type,
                        } => {
                            // Element match — highest priority, return immediately
                            return if *term_name == name && *term_ns == namespace {
                                // Direct match — return term's declaration info
                                MatchInfo {
                                    element_key: *element_key,
                                    resolved_type: *resolved_type,
                                    process_contents: None,
                                }
                            } else {
                                // Substitution match — let runtime resolve the actual member
                                MatchInfo {
                                    element_key: None,
                                    resolved_type: None,
                                    process_contents: None,
                                }
                            };
                        }
                        NfaTerm::Wildcard { process_contents, .. } => {
                            // Keep first wildcard as fallback
                            if wildcard_info.is_none() {
                                wildcard_info = Some(MatchInfo {
                                    element_key: None,
                                    resolved_type: None,
                                    process_contents: Some(*process_contents),
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    wildcard_info.unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    use crate::compiler::build_substitution_group_map;
    use crate::schema::model::{DerivationSet, SchemaSet};

    fn element_data(name: NameId) -> crate::arenas::ElementDeclData {
        crate::arenas::ElementDeclData {
            name: Some(name),
            target_namespace: None,
            ref_name: None,
            type_ref: None,
            inline_type: None,
            substitution_group: Vec::new(),
            default_value: None,
            fixed_value: None,
            nillable: false,
            is_abstract: false,
            min_occurs: 1,
            max_occurs: Some(1),
            block: DerivationSet::empty(),
            final_derivation: DerivationSet::empty(),
            form: None,
            id: None,
            alternatives: Vec::new(),
            identity_constraints: Vec::new(),
            pending_ic_refs: vec![],
            annotation: None,
            source: None,
            resolved_type: None,
            resolved_ref: None,
            resolved_substitution_groups: Vec::new(),
        }
    }

    fn make_set(ids: &[StateId]) -> HashSet<StateId> {
        ids.iter().copied().collect()
    }

    #[test]
    fn test_nfa_state_creation() {
        let state = NfaState::epsilon(0, None);
        assert_eq!(state.id, 0);
        assert!(state.is_epsilon());
        assert!(state.transitions.is_empty());
    }

    #[test]
    fn test_nfa_state_with_term() {
        let term = NfaTerm::element(NameId(1), None, None);
        let state = NfaState::with_term(1, term, None);
        assert_eq!(state.id, 1);
        assert!(!state.is_epsilon());
    }

    #[test]
    fn test_nfa_state_transitions() {
        let mut state = NfaState::epsilon(0, None);
        state.add_epsilon(1);
        state.add_consume(2);

        let epsilons: Vec<_> = state.epsilon_transitions().collect();
        assert_eq!(epsilons, vec![1]);

        let consuming: Vec<_> = state.consuming_transitions().collect();
        assert_eq!(consuming, vec![2]);
    }

    #[test]
    fn test_nfa_table() {
        let states = vec![
            NfaState::epsilon(0, None),
            NfaState::with_term(1, NfaTerm::element(NameId(1), None, None), None),
            NfaState::epsilon(2, None),
        ];
        let table = NfaTable::new(states, 0, 2);

        assert_eq!(table.state_count(), 3);
        assert_eq!(table.start_state, 0);
        assert_eq!(table.accept_state, 2);
        assert!(table.is_accept(2));
        assert!(!table.is_accept(0));
    }

    #[test]
    fn test_nfa_term() {
        let elem = NfaTerm::element(NameId(1), Some(NameId(2)), None);
        assert!(elem.is_element());
        assert!(!elem.is_wildcard());

        let wild = NfaTerm::wildcard(NamespaceConstraint::Any, ProcessContents::Lax);
        assert!(!wild.is_element());
        assert!(wild.is_wildcard());
    }

    #[test]
    fn test_transition_kinds() {
        let eps = NfaTransition::epsilon(1);
        assert_eq!(eps.kind, TransitionKind::Epsilon);

        let cons = NfaTransition::consume(2);
        assert_eq!(cons.kind, TransitionKind::Consume);
    }

    #[test]
    fn test_epsilon_closure_basic() {
        let mut s0 = NfaState::epsilon(0, None);
        s0.add_epsilon(1);
        let mut s1 = NfaState::epsilon(1, None);
        s1.add_epsilon(2);
        let s2 = NfaState::with_term(2, NfaTerm::element(NameId(1), None, None), None);

        let nfa = NfaTable::new(vec![s0, s1, s2], 0, 2);
        let closure = epsilon_closure(&nfa, [0]);

        assert_eq!(closure, make_set(&[0, 1, 2]));
    }

    fn make_priority_nfa() -> NfaTable {
        let mut start = NfaState::epsilon(0, None);
        start.add_epsilon(1);
        start.add_epsilon(2);

        let mut elem_state = NfaState::with_term(1, NfaTerm::element(NameId(1), None, None), None);
        let mut wild_state = NfaState::with_term(
            2,
            NfaTerm::wildcard(NamespaceConstraint::Any, ProcessContents::Lax),
            None,
        );

        elem_state.add_consume(3);
        wild_state.add_consume(4);

        let exit_elem = NfaState::epsilon(3, None);
        let exit_wild = NfaState::epsilon(4, None);

        NfaTable::new(
            vec![start, elem_state, wild_state, exit_elem, exit_wild],
            0,
            3,
        )
    }

    #[test]
    fn test_advance_states_matches_element_and_wildcard() {
        let nfa = make_priority_nfa();
        let next = advance_states(&nfa, [0], NameId(1), None, None, None, XsdVersion::V1_0);
        assert_eq!(next, make_set(&[3, 4]));

        let next = advance_states(&nfa, [0], NameId(2), None, None, None, XsdVersion::V1_0);
        assert_eq!(next, make_set(&[4]));
    }

    #[test]
    fn test_advance_with_priority_prefers_element() {
        let nfa = make_priority_nfa();
        let next = advance_with_priority(&nfa, [0], NameId(1), None, None, None, XsdVersion::V1_1);
        assert_eq!(next, make_set(&[3]));

        let next = advance_with_priority(&nfa, [0], NameId(2), None, None, None, XsdVersion::V1_1);
        assert_eq!(next, make_set(&[4]));
    }

    #[test]
    fn test_find_match_info_prefers_element_over_wildcard() {
        // make_priority_nfa has element(NameId(1)) at state 1 and wildcard at state 2,
        // both reachable from state 0.  Regardless of HashSet iteration order,
        // find_match_info should prefer the element match.
        let nfa = make_priority_nfa();
        let active = ActiveStates::Simple(epsilon_closure(&nfa, [0]));

        // NameId(1) matches both element and wildcard — element should win
        let mi = active.find_match_info(&nfa, NameId(1), None, None, None, XsdVersion::V1_1);
        assert!(
            mi.process_contents.is_none(),
            "element match should not have process_contents, got {:?}",
            mi.process_contents,
        );

        // NameId(2) matches only the wildcard
        let mi2 = active.find_match_info(&nfa, NameId(2), None, None, None, XsdVersion::V1_1);
        assert!(
            mi2.process_contents.is_some(),
            "wildcard-only match should have process_contents",
        );
    }

    #[test]
    fn test_term_matches_substitution_groups() {
        let mut schema_set = SchemaSet::new();
        let head_name = schema_set.name_table.add("head");
        let member_name = schema_set.name_table.add("member");

        let head_key = schema_set.arenas.alloc_element(element_data(head_name));
        let member_key = schema_set.arenas.alloc_element(element_data(member_name));

        schema_set
            .arenas
            .elements
            .get_mut(member_key)
            .unwrap()
            .resolved_substitution_groups
            .push(head_key);

        let map = build_substitution_group_map(&schema_set);
        let term = NfaTerm::element(head_name, None, Some(head_key));

        assert!(term_matches(
            &term,
            member_name,
            None,
            None,
            Some(&map),
            XsdVersion::V1_0,
        ));
    }

    #[test]
    fn test_find_match_info_substitution_returns_none_for_head_key() {
        // When a member matches via substitution group, find_match_info should
        // return element_key: None (not the head's key) so the runtime resolves
        // the actual member declaration via lookup_element.
        let mut schema_set = SchemaSet::new();
        let head_name = schema_set.name_table.add("head");
        let member_name = schema_set.name_table.add("member");

        let mut head_data = element_data(head_name);
        head_data.is_abstract = true;
        let head_key = schema_set.arenas.alloc_element(head_data);
        let member_key = schema_set.arenas.alloc_element(element_data(member_name));

        schema_set
            .arenas
            .elements
            .get_mut(member_key)
            .unwrap()
            .resolved_substitution_groups
            .push(head_key);

        let map = build_substitution_group_map(&schema_set);

        // Build a simple NFA: start --[head]--> accept
        let builder = crate::compiler::fragment::FragmentBuilder::new();
        let frag = builder.single_term(
            NfaTerm::element(head_name, None, Some(head_key)),
            None,
        );
        let nfa = crate::compiler::fragment::fragment_to_table(frag);
        let active = ActiveStates::from_nfa(&nfa);

        // Match with member name — should return element_key: None
        let mi = active.find_match_info(&nfa, member_name, None, None, Some(&map), XsdVersion::V1_0);
        assert!(
            mi.element_key.is_none(),
            "substitution match should not return head's element_key"
        );
        assert!(mi.resolved_type.is_none());

        // Abstract head's own name doesn't match (excluded from subst map,
        // and subst map lookup short-circuits before direct name comparison)
        let mi_head = active.find_match_info(&nfa, head_name, None, None, Some(&map), XsdVersion::V1_0);
        assert!(
            mi_head.element_key.is_none(),
            "abstract head should not match its own name via subst map"
        );
    }

    #[test]
    fn test_find_match_info_direct_match_returns_element_key() {
        // When a non-abstract head matches directly, find_match_info should
        // return the term's element_key.
        let mut schema_set = SchemaSet::new();
        let head_name = schema_set.name_table.add("head");
        let member_name = schema_set.name_table.add("member");

        // Non-abstract head
        let head_key = schema_set.arenas.alloc_element(element_data(head_name));
        let member_key = schema_set.arenas.alloc_element(element_data(member_name));

        schema_set
            .arenas
            .elements
            .get_mut(member_key)
            .unwrap()
            .resolved_substitution_groups
            .push(head_key);

        let map = build_substitution_group_map(&schema_set);

        let builder = crate::compiler::fragment::FragmentBuilder::new();
        let frag = builder.single_term(
            NfaTerm::element(head_name, None, Some(head_key)),
            None,
        );
        let nfa = crate::compiler::fragment::fragment_to_table(frag);
        let active = ActiveStates::from_nfa(&nfa);

        // Direct match with head name — should return head's element_key
        let mi = active.find_match_info(&nfa, head_name, None, None, Some(&map), XsdVersion::V1_0);
        assert_eq!(
            mi.element_key,
            Some(head_key),
            "direct match should return the term's element_key"
        );

        // Substitution match with member name — should return None
        let mi_member = active.find_match_info(&nfa, member_name, None, None, Some(&map), XsdVersion::V1_0);
        assert!(
            mi_member.element_key.is_none(),
            "substitution match should not return head's element_key"
        );
    }

    // -----------------------------------------------------------------------
    // Counted NFA tests
    // -----------------------------------------------------------------------

    use crate::compiler::fragment::{FragmentBuilder, fragment_to_table};

    /// Build a counted NFA for element `a{min,max}` and return (nfa, name_id).
    fn make_counted_element_nfa(min: u32, max: u32) -> (NfaTable, NameId) {
        let name = NameId(100);
        let builder = FragmentBuilder::new();
        let frag = builder.single_term(NfaTerm::element(name, None, None), None);
        let counted = frag.repeat_counted(min, max);
        let nfa = fragment_to_table(counted);
        (nfa, name)
    }

    fn advance_n(active: ActiveStates, nfa: &NfaTable, name: NameId, n: usize) -> ActiveStates {
        let mut state = active;
        for _ in 0..n {
            state = state.advance(nfa, name, None, None, None, XsdVersion::V1_0);
        }
        state
    }

    #[test]
    fn test_counted_nfa_has_counters() {
        let (nfa, _) = make_counted_element_nfa(2, 5);
        assert!(nfa.has_counters());
        assert_eq!(nfa.counter_defs.len(), 1);
        assert_eq!(nfa.counter_defs[0].min, 2);
        assert_eq!(nfa.counter_defs[0].max, 5);
        // Counted NFA: body(2 states) + entry + guard + exit = 5 states
        assert_eq!(nfa.state_count(), 5);
    }

    #[test]
    fn test_counted_nfa_compact_state_count() {
        // Large maxOccurs should NOT create many states
        let (nfa, _) = make_counted_element_nfa(0, 1000);
        assert_eq!(nfa.state_count(), 5); // Still just 5 states
    }

    #[test]
    fn test_counted_active_states_is_counted_variant() {
        let (nfa, _) = make_counted_element_nfa(2, 5);
        let active = ActiveStates::from_nfa(&nfa);
        assert!(matches!(active, ActiveStates::Counted { .. }));
    }

    #[test]
    fn test_counted_element_a_3_5() {
        let (nfa, a) = make_counted_element_nfa(3, 5);

        // 2 occurrences: not complete, would accept more
        let s2 = advance_n(ActiveStates::from_nfa(&nfa), &nfa, a, 2);
        assert!(!s2.is_empty());
        assert!(!s2.contains_accept(&nfa));

        // 3 occurrences: complete (min satisfied)
        let s3 = advance_n(ActiveStates::from_nfa(&nfa), &nfa, a, 3);
        assert!(s3.contains_accept(&nfa));

        // 4 occurrences: still complete
        let s4 = advance_n(ActiveStates::from_nfa(&nfa), &nfa, a, 4);
        assert!(s4.contains_accept(&nfa));

        // 5 occurrences: complete (max)
        let s5 = advance_n(ActiveStates::from_nfa(&nfa), &nfa, a, 5);
        assert!(s5.contains_accept(&nfa));

        // 6 occurrences: rejected (past max)
        let s6 = advance_n(ActiveStates::from_nfa(&nfa), &nfa, a, 6);
        assert!(s6.is_empty());
    }

    #[test]
    fn test_counted_element_a_0_100() {
        let (nfa, a) = make_counted_element_nfa(0, 100);

        // 0 occurrences: complete (min=0)
        let s0 = ActiveStates::from_nfa(&nfa);
        assert!(s0.contains_accept(&nfa));

        // 100 occurrences: complete (max)
        let s100 = advance_n(ActiveStates::from_nfa(&nfa), &nfa, a, 100);
        assert!(s100.contains_accept(&nfa));

        // 101 occurrences: rejected
        let s101 = advance_n(ActiveStates::from_nfa(&nfa), &nfa, a, 101);
        assert!(s101.is_empty());
    }

    #[test]
    fn test_counted_element_exact_17() {
        let (nfa, a) = make_counted_element_nfa(17, 17);

        // 16: not complete
        let s16 = advance_n(ActiveStates::from_nfa(&nfa), &nfa, a, 16);
        assert!(!s16.contains_accept(&nfa));

        // 17: complete
        let s17 = advance_n(ActiveStates::from_nfa(&nfa), &nfa, a, 17);
        assert!(s17.contains_accept(&nfa));

        // 18: rejected
        let s18 = advance_n(ActiveStates::from_nfa(&nfa), &nfa, a, 18);
        assert!(s18.is_empty());
    }

    #[test]
    fn test_counted_sequence_a_b() {
        // Build a{2,50} followed by b
        let a = NameId(100);
        let b = NameId(200);
        let builder = FragmentBuilder::new();

        let frag_a = builder.single_term(NfaTerm::element(a, None, None), None);
        let counted_a = frag_a.repeat_counted(2, 50);

        let frag_b = builder.single_term(NfaTerm::element(b, None, None), None);
        let seq = counted_a.concat(frag_b);
        let nfa = fragment_to_table(seq);

        assert!(nfa.has_counters());

        // 1 a + b: should fail (min=2 not satisfied)
        let s = ActiveStates::from_nfa(&nfa);
        let s = s.advance(&nfa, a, None, None, None, XsdVersion::V1_0); // 1 a
        let s = s.advance(&nfa, b, None, None, None, XsdVersion::V1_0); // b
        assert!(!s.contains_accept(&nfa)); // min not satisfied

        // 2 a + b: should succeed
        let s = ActiveStates::from_nfa(&nfa);
        let s = advance_n(s, &nfa, a, 2);
        let s = s.advance(&nfa, b, None, None, None, XsdVersion::V1_0);
        assert!(s.contains_accept(&nfa));

        // 50 a + b: should succeed
        let s = ActiveStates::from_nfa(&nfa);
        let s = advance_n(s, &nfa, a, 50);
        let s = s.advance(&nfa, b, None, None, None, XsdVersion::V1_0);
        assert!(s.contains_accept(&nfa));

        // 51 a: should be rejected
        let s = ActiveStates::from_nfa(&nfa);
        let s = advance_n(s, &nfa, a, 51);
        assert!(s.is_empty());
    }

    #[test]
    fn test_dead_counter_collapse() {
        // (a?){0,200} followed by b
        // After the nullable loop exits, all configs should collapse at b.
        let a = NameId(100);
        let b = NameId(200);
        let builder = FragmentBuilder::new();

        // Build a? (optional element)
        let frag_a = builder.single_term(NfaTerm::element(a, None, None), None);
        let opt_a = frag_a.optional();

        // Counted loop: (a?){0,200}
        let counted = opt_a.repeat_counted(0, 200);

        // Followed by b
        let frag_b = builder.single_term(NfaTerm::element(b, None, None), None);
        let seq = counted.concat(frag_b);
        let nfa = fragment_to_table(seq);

        // Single counter + nullable body → should use RangedSingle
        let initial = ActiveStates::from_nfa(&nfa);
        assert!(matches!(&initial, ActiveStates::RangedSingle { .. }),
            "Expected RangedSingle for nullable body counted NFA");

        // Initial state should be complete (min=0 loop can be skipped)
        // After advancing with b (skipping the loop entirely), should accept
        let after_b = initial.clone().advance(&nfa, b, None, None, None, XsdVersion::V1_0);
        assert!(after_b.contains_accept(&nfa));

        // With RangedSingle, the state map has O(states) entries, not O(200).
        if let ActiveStates::RangedSingle { state_ranges, .. } = &after_b {
            assert!(
                state_ranges.len() <= 5,
                "Expected O(1) ranged entries after loop exit, got {}",
                state_ranges.len()
            );
        }
    }

    #[test]
    fn test_counted_exact_prefix_plus_star() {
        // a{17,unbounded}: counted exact(17) + star
        // Built via apply_occurs to test the dispatch
        use crate::compiler::particle::{apply_occurs, MaxOccurs};

        let a = NameId(100);
        let builder = FragmentBuilder::new();
        let frag = builder.single_term(NfaTerm::element(a, None, None), None);
        let result = apply_occurs(frag, 17, MaxOccurs::Unbounded);
        let nfa = fragment_to_table(result);

        // Should use counted path (has counters for the prefix)
        assert!(nfa.has_counters());
        // Should be compact — NOT 17*2 = 34 states from unrolling
        assert!(nfa.state_count() < 15, "expected compact NFA, got {} states", nfa.state_count());

        // 16 occurrences: not complete (min=17)
        let s16 = advance_n(ActiveStates::from_nfa(&nfa), &nfa, a, 16);
        assert!(!s16.contains_accept(&nfa));

        // 17 occurrences: complete
        let s17 = advance_n(ActiveStates::from_nfa(&nfa), &nfa, a, 17);
        assert!(s17.contains_accept(&nfa));

        // 100 occurrences: still complete (unbounded)
        let s100 = advance_n(ActiveStates::from_nfa(&nfa), &nfa, a, 100);
        assert!(s100.contains_accept(&nfa));
    }

    #[test]
    fn test_is_epsilon_like() {
        assert!(TransitionKind::Epsilon.is_epsilon_like());
        assert!(!TransitionKind::Consume.is_epsilon_like());
        assert!(TransitionKind::CounterReset(0).is_epsilon_like());
        assert!(TransitionKind::CounterIncrement(0).is_epsilon_like());
        assert!(TransitionKind::CounterMaxGuard(0).is_epsilon_like());
        assert!(TransitionKind::CounterMinGuard(0).is_epsilon_like());
    }

    #[test]
    fn test_offset_counter() {
        assert_eq!(
            TransitionKind::CounterReset(0).offset_counter(3),
            TransitionKind::CounterReset(3)
        );
        assert_eq!(
            TransitionKind::CounterIncrement(2).offset_counter(5),
            TransitionKind::CounterIncrement(7)
        );
        assert_eq!(TransitionKind::Epsilon.offset_counter(10), TransitionKind::Epsilon);
        assert_eq!(TransitionKind::Consume.offset_counter(10), TransitionKind::Consume);
    }

    // -----------------------------------------------------------------------
    // CounterRange unit tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_counter_range_operations() {
        let r = CounterRange::single(5);
        assert_eq!(r.lo, 5);
        assert_eq!(r.hi, 5);
        assert!(!r.is_empty());
        assert!(r.subsumes(r));

        let r2 = CounterRange::new(3, 7);
        assert!(r2.subsumes(r));
        assert!(!r.subsumes(r2));

        // Union
        let u = CounterRange::single(2).union(CounterRange::new(3, 5));
        assert_eq!(u, CounterRange::new(2, 5));

        // intersect_below
        assert_eq!(CounterRange::new(1, 5).intersect_below(4), CounterRange::new(1, 3));
        assert!(CounterRange::new(5, 8).intersect_below(5).is_empty());
        assert!(CounterRange::new(0, 10).intersect_below(0).is_empty());

        // intersect_above
        assert_eq!(CounterRange::new(1, 5).intersect_above(3), CounterRange::new(3, 5));
        assert!(CounterRange::new(1, 3).intersect_above(5).is_empty());
    }

    // -----------------------------------------------------------------------
    // RangedSingle tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_ranged_closure_convergence() {
        // (a?){0,10000} — must use RangedSingle and converge in O(states)
        let a = NameId(100);
        let builder = FragmentBuilder::new();
        let frag = builder.single_term(NfaTerm::element(a, None, None), None);
        let opt = frag.optional();
        let counted = opt.repeat_counted(0, 10_000);
        let nfa = fragment_to_table(counted);

        assert!(nfa.has_counters());
        assert!(nfa.counter_defs[0].body_nullable);

        let initial = ActiveStates::from_nfa(&nfa);
        assert!(matches!(&initial, ActiveStates::RangedSingle { .. }));

        // Should have O(states) entries, not O(10000)
        if let ActiveStates::RangedSingle { state_ranges, .. } = &initial {
            assert!(
                state_ranges.len() <= 10,
                "Expected O(states) entries, got {}",
                state_ranges.len()
            );
        }

        // Should be accepting (min=0)
        assert!(initial.contains_accept(&nfa));

        // Advance with 'a' 5 times — should still be accepting
        let s5 = advance_n(initial, &nfa, a, 5);
        assert!(s5.contains_accept(&nfa));

        // Still RangedSingle throughout
        assert!(matches!(&s5, ActiveStates::RangedSingle { .. }));
    }

    #[test]
    fn test_nullable_body_accepts_range() {
        // (a?){3,5}: accepts "", "a", "aa", "aaa", "aaaa", "aaaaa", rejects "aaaaaa"
        let a = NameId(100);
        let builder = FragmentBuilder::new();
        let frag = builder.single_term(NfaTerm::element(a, None, None), None);
        let opt = frag.optional();
        let counted = opt.repeat_counted(3, 5);
        let nfa = fragment_to_table(counted);

        assert!(nfa.counter_defs[0].body_nullable);
        let initial = ActiveStates::from_nfa(&nfa);
        assert!(matches!(&initial, ActiveStates::RangedSingle { .. }));

        // "" — accepted (body nullable, 3 empty iterations satisfy min)
        assert!(initial.contains_accept(&nfa));

        // "a" through "aaaaa" — all accepted
        for n in 1..=5 {
            let s = advance_n(initial.clone(), &nfa, a, n);
            assert!(s.contains_accept(&nfa), "Expected accepting after {n} a's");
        }

        // "aaaaaa" — rejected (max=5 iterations, each consuming at most 1 a)
        let s6 = advance_n(initial, &nfa, a, 6);
        assert!(s6.is_empty(), "Expected rejection after 6 a's");
    }

    #[test]
    fn test_choice_body_nullable() {
        // (a|b?){0,1000} — body is nullable (b? branch), should use RangedSingle
        let a = NameId(100);
        let b = NameId(200);
        let builder = FragmentBuilder::new();
        let frag_a = builder.single_term(NfaTerm::element(a, None, None), None);
        let frag_b = builder.single_term(NfaTerm::element(b, None, None), None);
        let choice = frag_a.alternate(frag_b.optional());
        let counted = choice.repeat_counted(0, 1000);
        let nfa = fragment_to_table(counted);

        assert!(nfa.counter_defs[0].body_nullable);
        let initial = ActiveStates::from_nfa(&nfa);
        assert!(matches!(&initial, ActiveStates::RangedSingle { .. }));

        if let ActiveStates::RangedSingle { state_ranges, .. } = &initial {
            assert!(state_ranges.len() <= 15,
                "Expected O(states) entries, got {}", state_ranges.len());
        }

        // Accepts "a", "b", "ab", ""
        assert!(initial.contains_accept(&nfa));
        let s_a = initial.clone().advance(&nfa, a, None, None, None, XsdVersion::V1_0);
        assert!(s_a.contains_accept(&nfa));
        let s_b = initial.clone().advance(&nfa, b, None, None, None, XsdVersion::V1_0);
        assert!(s_b.contains_accept(&nfa));
    }

    #[test]
    fn test_non_nullable_body_stays_counted() {
        // (a){0,5} — body is NOT nullable, should use Counted (not RangedSingle)
        let (nfa, a) = make_counted_element_nfa(0, 5);
        assert!(!nfa.counter_defs[0].body_nullable);

        let initial = ActiveStates::from_nfa(&nfa);
        assert!(matches!(&initial, ActiveStates::Counted { .. }));

        // Still correct: accepts 0..5 a's
        assert!(initial.contains_accept(&nfa)); // min=0
        for n in 1..=5 {
            let s = advance_n(initial.clone(), &nfa, a, n);
            assert!(s.contains_accept(&nfa), "Expected accepting after {n} a's");
        }
        let s6 = advance_n(initial, &nfa, a, 6);
        assert!(s6.is_empty());
    }

    #[test]
    fn test_multi_counter_stays_counted() {
        // a{0,100} followed by b{0,100} — 2 counters, should use Counted
        let a = NameId(100);
        let b = NameId(200);
        let builder = FragmentBuilder::new();
        let frag_a = builder.single_term(NfaTerm::element(a, None, None), None);
        let counted_a = frag_a.repeat_counted(0, 100);
        let frag_b = builder.single_term(NfaTerm::element(b, None, None), None);
        let counted_b = frag_b.repeat_counted(0, 100);
        let seq = counted_a.concat(counted_b);
        let nfa = fragment_to_table(seq);

        assert_eq!(nfa.counter_defs.len(), 2);
        let initial = ActiveStates::from_nfa(&nfa);
        assert!(matches!(&initial, ActiveStates::Counted { .. }),
            "Multi-counter NFA should use Counted, not RangedSingle");
    }

    #[test]
    fn test_nested_nullable_uses_hybrid() {
        // ((a?){0,50}){0,50} — 2 nullable counters → Hybrid
        let a = NameId(100);
        let builder = FragmentBuilder::new();
        let frag = builder.single_term(NfaTerm::element(a, None, None), None);
        let inner = frag.optional().repeat_counted(0, 50);
        let outer = inner.repeat_counted(0, 50);
        let nfa = fragment_to_table(outer);

        assert_eq!(nfa.counter_defs.len(), 2);
        let initial = ActiveStates::from_nfa(&nfa);
        assert!(matches!(&initial, ActiveStates::Hybrid { .. }),
            "Nested nullable counted NFA should use Hybrid, got {:?}",
            std::mem::discriminant(&initial));
    }

    // -----------------------------------------------------------------------
    // Hybrid variant tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_nested_nullable_hybrid_correctness() {
        // ((a?){0,100}){0,50}: inner up to 100 per outer iteration, outer up to 50
        let a = NameId(100);
        let builder = FragmentBuilder::new();
        let frag = builder.single_term(NfaTerm::element(a, None, None), None);
        let inner = frag.optional().repeat_counted(0, 100);
        let outer = inner.repeat_counted(0, 50);
        let nfa = fragment_to_table(outer);

        assert_eq!(nfa.counter_defs.len(), 2);
        let initial = ActiveStates::from_nfa(&nfa);
        assert!(matches!(&initial, ActiveStates::Hybrid { .. }));

        // Empty accepted (min=0 on both loops)
        assert!(initial.contains_accept(&nfa));

        // "a"*100: accepted (1 outer iteration, 100 inner = max inner)
        let s100 = advance_n(initial.clone(), &nfa, a, 100);
        assert!(s100.contains_accept(&nfa));

        // "a"*5000: accepted (50 outer * 100 inner)
        let s5000 = advance_n(initial.clone(), &nfa, a, 5000);
        assert!(s5000.contains_accept(&nfa));

        // "a"*5001: rejected (exceeds 50*100)
        let s5001 = advance_n(initial, &nfa, a, 5001);
        assert!(s5001.is_empty(), "Expected rejection after 5001 a's");
    }

    #[test]
    fn test_hybrid_convergence() {
        // ((a?){0,100}){0,50}: verify configs are O(states * outer_max), not O(M*N*states)
        let a = NameId(100);
        let builder = FragmentBuilder::new();
        let frag = builder.single_term(NfaTerm::element(a, None, None), None);
        let inner = frag.optional().repeat_counted(0, 100);
        let outer = inner.repeat_counted(0, 50);
        let nfa = fragment_to_table(outer);

        let initial = ActiveStates::from_nfa(&nfa);
        if let ActiveStates::Hybrid { configs, .. } = &initial {
            // With Hybrid, inner counter is ranged → O(states * outer_max) entries.
            // Without Hybrid (Counted), would be O(states * 100 * 50) = O(5000 * states).
            let max_expected = nfa.state_count() * 51; // outer goes 0..50
            assert!(
                configs.len() <= max_expected,
                "Expected at most {} entries, got {} (state_count={})",
                max_expected, configs.len(), nfa.state_count()
            );
        } else {
            panic!("Expected Hybrid variant");
        }
    }

    #[test]
    fn test_sequential_nullable_hybrid() {
        // (a?){0,1000} followed by (b?){0,1000}
        let a = NameId(100);
        let b = NameId(200);
        let builder = FragmentBuilder::new();
        let frag_a = builder.single_term(NfaTerm::element(a, None, None), None);
        let counted_a = frag_a.optional().repeat_counted(0, 1000);
        let frag_b = builder.single_term(NfaTerm::element(b, None, None), None);
        let counted_b = frag_b.optional().repeat_counted(0, 1000);
        let seq = counted_a.concat(counted_b);
        let nfa = fragment_to_table(seq);

        assert_eq!(nfa.counter_defs.len(), 2);
        assert!(nfa.counter_defs[0].body_nullable);
        assert!(nfa.counter_defs[1].body_nullable);

        let initial = ActiveStates::from_nfa(&nfa);
        assert!(matches!(&initial, ActiveStates::Hybrid { .. }));

        // Empty: accepted (both min=0)
        assert!(initial.contains_accept(&nfa));

        // "a": accepted
        let s = initial.clone().advance(&nfa, a, None, None, None, XsdVersion::V1_0);
        assert!(s.contains_accept(&nfa));

        // "b": accepted
        let s = initial.clone().advance(&nfa, b, None, None, None, XsdVersion::V1_0);
        assert!(s.contains_accept(&nfa));

        // "a"*1000 + "b"*1000: accepted
        let s = advance_n(initial.clone(), &nfa, a, 1000);
        assert!(s.contains_accept(&nfa));
        let s = advance_n(s, &nfa, b, 1000);
        assert!(s.contains_accept(&nfa));

        // "a"*1001: rejected (exceeds first loop's max)
        let s = advance_n(initial, &nfa, a, 1001);
        assert!(s.is_empty(), "Expected rejection after 1001 a's");
    }

    #[test]
    fn test_mixed_nullable_nonnullable_hybrid() {
        // (a?){0,500} followed by b{0,500}: one nullable, one not
        let a = NameId(100);
        let b = NameId(200);
        let builder = FragmentBuilder::new();
        let frag_a = builder.single_term(NfaTerm::element(a, None, None), None);
        let counted_a = frag_a.optional().repeat_counted(0, 500);
        let frag_b = builder.single_term(NfaTerm::element(b, None, None), None);
        let counted_b = frag_b.repeat_counted(0, 500);
        let seq = counted_a.concat(counted_b);
        let nfa = fragment_to_table(seq);

        assert_eq!(nfa.counter_defs.len(), 2);
        assert!(nfa.counter_defs[0].body_nullable);
        assert!(!nfa.counter_defs[1].body_nullable);

        let initial = ActiveStates::from_nfa(&nfa);
        assert!(matches!(&initial, ActiveStates::Hybrid { .. }));

        // Verify ranged counter is counter 0 (the nullable one)
        if let ActiveStates::Hybrid { ranged_counter_idx, .. } = &initial {
            assert_eq!(*ranged_counter_idx, 0,
                "Should range the nullable counter (index 0)");
        }

        // Empty: accepted (both min=0)
        assert!(initial.contains_accept(&nfa));

        // "a"*500 + "b"*500: accepted
        let s = advance_n(initial.clone(), &nfa, a, 500);
        let s = advance_n(s, &nfa, b, 500);
        assert!(s.contains_accept(&nfa));

        // "b"*501: rejected
        let s = advance_n(initial, &nfa, b, 501);
        assert!(s.is_empty());
    }

    #[test]
    fn test_hybrid_forced_outer_ranged() {
        // ((a?){0,100}){0,50}: manually verify correctness with the algorithm
        // choosing whichever counter it picks, by testing boundary conditions
        // that exercise both counters.
        let a = NameId(100);
        let builder = FragmentBuilder::new();
        let frag = builder.single_term(NfaTerm::element(a, None, None), None);
        let inner = frag.optional().repeat_counted(0, 100);
        let outer = inner.repeat_counted(0, 50);
        let nfa = fragment_to_table(outer);

        let initial = ActiveStates::from_nfa(&nfa);

        // Boundary: exactly 100 a's per iteration, 50 iterations = 5000 total
        let s = advance_n(initial.clone(), &nfa, a, 5000);
        assert!(s.contains_accept(&nfa), "5000 a's should be accepted (50*100)");

        // Boundary: 101 a's — requires second outer iteration (first handles 100, second 1)
        let s = advance_n(initial.clone(), &nfa, a, 101);
        assert!(s.contains_accept(&nfa), "101 a's should be accepted (needs 2 outer iterations)");

        // Boundary: 0 a's
        assert!(initial.contains_accept(&nfa), "0 a's should be accepted");

        // Boundary: 5001 — exceeds total capacity
        let s = advance_n(initial, &nfa, a, 5001);
        assert!(s.is_empty(), "5001 a's should be rejected");
    }

    #[test]
    fn test_hybrid_selection_different_maxima() {
        // (a?){0,10} followed by (b?){0,1000}: should range counter 1 (max=1000)
        let a = NameId(100);
        let b = NameId(200);
        let builder = FragmentBuilder::new();
        let frag_a = builder.single_term(NfaTerm::element(a, None, None), None);
        let counted_a = frag_a.optional().repeat_counted(0, 10);
        let frag_b = builder.single_term(NfaTerm::element(b, None, None), None);
        let counted_b = frag_b.optional().repeat_counted(0, 1000);
        let seq = counted_a.concat(counted_b);
        let nfa = fragment_to_table(seq);

        assert_eq!(nfa.counter_defs.len(), 2);
        let initial = ActiveStates::from_nfa(&nfa);
        // counter 0 max=10 (below COUNTED_THRESHOLD=16 — but counter 1 max=1000 qualifies)
        assert!(matches!(&initial, ActiveStates::Hybrid { .. }),
            "Expected Hybrid, got {:?}", std::mem::discriminant(&initial));

        if let ActiveStates::Hybrid { ranged_counter_idx, .. } = &initial {
            assert_eq!(*ranged_counter_idx, 1,
                "Should range counter 1 (max=1000), not counter 0 (max=10)");
        }

        // Correctness: "a"*10 + "b"*1000 accepted
        let s = advance_n(initial.clone(), &nfa, a, 10);
        let s = advance_n(s, &nfa, b, 1000);
        assert!(s.contains_accept(&nfa));

        // "a"*11 rejected
        let s = advance_n(initial, &nfa, a, 11);
        assert!(s.is_empty());
    }

    #[test]
    fn test_hybrid_small_max_stays_counted() {
        // (a?){0,5} followed by (b?){0,5}: both nullable but max ≤ COUNTED_THRESHOLD → Counted
        let a = NameId(100);
        let b = NameId(200);
        let builder = FragmentBuilder::new();
        let frag_a = builder.single_term(NfaTerm::element(a, None, None), None);
        let counted_a = frag_a.optional().repeat_counted(0, 5);
        let frag_b = builder.single_term(NfaTerm::element(b, None, None), None);
        let counted_b = frag_b.optional().repeat_counted(0, 5);
        let seq = counted_a.concat(counted_b);
        let nfa = fragment_to_table(seq);

        assert_eq!(nfa.counter_defs.len(), 2);
        let initial = ActiveStates::from_nfa(&nfa);
        assert!(matches!(&initial, ActiveStates::Counted { .. }),
            "Small-max nullable counters should stay Counted, got {:?}",
            std::mem::discriminant(&initial));
    }

    #[test]
    fn test_xsd11_priority_ranged() {
        // Nullable counted body containing element 'a' + wildcard *.
        // advance_with_priority should prefer element over wildcard.
        use crate::types::complex::NamespaceConstraint;
        let a = NameId(100);
        let b = NameId(200);
        let builder = FragmentBuilder::new();

        // Element term for 'a' (mandatory)
        let elem = builder.single_term(NfaTerm::element(a, None, None), None);
        // Wildcard term matching anything (optional → makes body nullable)
        let wc = builder.single_term(
            NfaTerm::Wildcard {
                namespace_constraint: NamespaceConstraint::Any,
                process_contents: ProcessContents::Lax,
                not_qnames: Vec::new(),
            },
            None,
        );
        // (a | *?){0,100} — nullable body via the *? branch
        let choice = elem.alternate(wc.optional());
        let counted = choice.repeat_counted(0, 100);
        let nfa = fragment_to_table(counted);

        assert!(nfa.counter_defs[0].body_nullable);
        let initial = ActiveStates::from_nfa(&nfa);
        assert!(matches!(&initial, ActiveStates::RangedSingle { .. }));

        // advance_with_priority for 'a': element branch exists, so wildcard
        // should not be chosen.  The result should be non-empty.
        let next = initial.clone().advance_with_priority(&nfa, a, None, None, None, XsdVersion::V1_1);
        assert!(!next.is_empty());
        assert!(next.contains_accept(&nfa));

        // advance_with_priority for 'b': only wildcard matches, should work
        let next_b = initial.advance_with_priority(&nfa, b, None, None, None, XsdVersion::V1_1);
        assert!(!next_b.is_empty());
        assert!(next_b.contains_accept(&nfa));
    }

    // -----------------------------------------------------------------------
    // Dynamic ranged-counter switching tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_initial_tiebreak_prefers_first() {
        // (a?){0,1000} followed by (b?){0,1000}: equal max → should range counter 0
        let a = NameId(100);
        let b = NameId(200);
        let builder = FragmentBuilder::new();
        let frag_a = builder.single_term(NfaTerm::element(a, None, None), None);
        let counted_a = frag_a.optional().repeat_counted(0, 1000);
        let frag_b = builder.single_term(NfaTerm::element(b, None, None), None);
        let counted_b = frag_b.optional().repeat_counted(0, 1000);
        let seq = counted_a.concat(counted_b);
        let nfa = fragment_to_table(seq);

        let initial = ActiveStates::from_nfa(&nfa);
        if let ActiveStates::Hybrid { ranged_counter_idx, .. } = &initial {
            assert_eq!(*ranged_counter_idx, 0,
                "Equal max: should prefer first counter (index 0)");
        } else {
            panic!("Expected Hybrid variant");
        }
    }

    #[test]
    fn test_dynamic_switch_sequential_equal_max() {
        // (a?){0,1000} followed by (b?){0,1000}
        // After feeding a's and then b, the ranged counter should switch.
        let a = NameId(100);
        let b = NameId(200);
        let builder = FragmentBuilder::new();
        let frag_a = builder.single_term(NfaTerm::element(a, None, None), None);
        let counted_a = frag_a.optional().repeat_counted(0, 1000);
        let frag_b = builder.single_term(NfaTerm::element(b, None, None), None);
        let counted_b = frag_b.optional().repeat_counted(0, 1000);
        let seq = counted_a.concat(counted_b);
        let nfa = fragment_to_table(seq);

        let initial = ActiveStates::from_nfa(&nfa);
        // Feed 1000 a's then 1 b to move fully into second loop
        let after_a = advance_n(initial, &nfa, a, 1000);
        let after_b = after_a.advance(&nfa, b, None, None, None, XsdVersion::V1_0);

        if let ActiveStates::Hybrid { ranged_counter_idx, configs, .. } = &after_b {
            assert_eq!(*ranged_counter_idx, 1,
                "Should have dynamically switched to counter 1");
            // Config count should be O(states), not O(1000 * states)
            let max_expected = nfa.state_count() * 2;
            assert!(configs.len() <= max_expected,
                "Expected <= {} configs after switch, got {}", max_expected, configs.len());
        } else {
            panic!("Expected Hybrid variant");
        }

        // Correctness: b*999 more (total 1000 b) accepted
        let s = advance_n(after_b.clone(), &nfa, b, 999);
        assert!(s.contains_accept(&nfa));

        // b*1001 total rejected
        let s = advance_n(after_b, &nfa, b, 1000);
        assert!(s.is_empty());
    }

    #[test]
    fn test_dynamic_switch_three_counters() {
        // (a?){0,500} (b?){0,500} (c?){0,500}: cascading switches 0→1→2
        let a = NameId(100);
        let b = NameId(200);
        let c = NameId(300);
        let builder = FragmentBuilder::new();
        let frag_a = builder.single_term(NfaTerm::element(a, None, None), None);
        let counted_a = frag_a.optional().repeat_counted(0, 500);
        let frag_b = builder.single_term(NfaTerm::element(b, None, None), None);
        let counted_b = frag_b.optional().repeat_counted(0, 500);
        let frag_c = builder.single_term(NfaTerm::element(c, None, None), None);
        let counted_c = frag_c.optional().repeat_counted(0, 500);
        let seq = counted_a.concat(counted_b).concat(counted_c);
        let nfa = fragment_to_table(seq);

        assert_eq!(nfa.counter_defs.len(), 3);
        let initial = ActiveStates::from_nfa(&nfa);

        // After 500 a's + 1 b → switch to counter 1
        let after_a = advance_n(initial, &nfa, a, 500);
        assert!(after_a.contains_accept(&nfa));
        let after_b1 = after_a.advance(&nfa, b, None, None, None, XsdVersion::V1_0);
        if let ActiveStates::Hybrid { ranged_counter_idx, .. } = &after_b1 {
            assert_eq!(*ranged_counter_idx, 1,
                "Should switch to counter 1 after first loop exits");
        }

        // After 500 b's + 1 c → switch to counter 2
        let after_b = advance_n(after_b1, &nfa, b, 499);
        assert!(after_b.contains_accept(&nfa));
        let after_c1 = after_b.advance(&nfa, c, None, None, None, XsdVersion::V1_0);
        if let ActiveStates::Hybrid { ranged_counter_idx, .. } = &after_c1 {
            assert_eq!(*ranged_counter_idx, 2,
                "Should switch to counter 2 after second loop exits");
        }

        // 500 c's accepted, 501 rejected
        let after_c = advance_n(after_c1.clone(), &nfa, c, 499);
        assert!(after_c.contains_accept(&nfa));
        let over = advance_n(after_c1, &nfa, c, 500);
        assert!(over.is_empty());
    }

    #[test]
    fn test_no_switch_when_ranged_still_active() {
        // (a?){0,1000} followed by (b?){0,1000}
        // After 5 a's, ranged counter is still active — no switch.
        let a = NameId(100);
        let b = NameId(200);
        let builder = FragmentBuilder::new();
        let frag_a = builder.single_term(NfaTerm::element(a, None, None), None);
        let counted_a = frag_a.optional().repeat_counted(0, 1000);
        let frag_b = builder.single_term(NfaTerm::element(b, None, None), None);
        let counted_b = frag_b.optional().repeat_counted(0, 1000);
        let seq = counted_a.concat(counted_b);
        let nfa = fragment_to_table(seq);

        let initial = ActiveStates::from_nfa(&nfa);
        let after_5a = advance_n(initial, &nfa, a, 5);
        if let ActiveStates::Hybrid { ranged_counter_idx, .. } = &after_5a {
            assert_eq!(*ranged_counter_idx, 0,
                "Should NOT switch while first loop is still active");
        }
    }

    #[test]
    fn test_no_switch_non_nullable_candidate() {
        // (a?){0,1000} followed by b{0,1000}: second counter is NOT nullable
        let a = NameId(100);
        let b = NameId(200);
        let builder = FragmentBuilder::new();
        let frag_a = builder.single_term(NfaTerm::element(a, None, None), None);
        let counted_a = frag_a.optional().repeat_counted(0, 1000);
        let frag_b = builder.single_term(NfaTerm::element(b, None, None), None);
        let counted_b = frag_b.repeat_counted(0, 1000); // NOT nullable
        let seq = counted_a.concat(counted_b);
        let nfa = fragment_to_table(seq);

        let initial = ActiveStates::from_nfa(&nfa);
        // Feed b's to exit first loop (counter 0 dead) and enter second
        let after_b = advance_n(initial, &nfa, b, 100);
        if let ActiveStates::Hybrid { ranged_counter_idx, .. } = &after_b {
            assert_eq!(*ranged_counter_idx, 0,
                "Should NOT switch: counter 1 is not nullable");
        }

        // Correctness: b*1000 accepted
        let s = advance_n(after_b.clone(), &nfa, b, 900);
        assert!(s.contains_accept(&nfa));
    }

    #[test]
    fn test_no_switch_nested_inner_active() {
        // ((a?){0,100}){0,50}: inner counter should not cause a switch
        // while it still has active ranges.
        let a = NameId(100);
        let builder = FragmentBuilder::new();
        let frag = builder.single_term(NfaTerm::element(a, None, None), None);
        let inner = frag.optional().repeat_counted(0, 100);
        let outer = inner.repeat_counted(0, 50);
        let nfa = fragment_to_table(outer);

        // During active processing, the ranged counter should not switch
        let initial = ActiveStates::from_nfa(&nfa);
        let after_a = advance_n(initial.clone(), &nfa, a, 50);
        if let ActiveStates::Hybrid { ranged_counter_idx, .. } = &after_a {
            // The initial ranged counter (inner, max=100) should stay
            let initial_idx = if let ActiveStates::Hybrid { ranged_counter_idx, .. } = &initial {
                *ranged_counter_idx
            } else { unreachable!() };
            assert_eq!(*ranged_counter_idx, initial_idx,
                "Should NOT switch while processing nested loops");
        }
    }

    #[test]
    fn test_switch_config_count_drops() {
        // (a?){0,1000} followed by (b?){0,1000}
        // After first 'b', config count should be small.
        let a = NameId(100);
        let b = NameId(200);
        let builder = FragmentBuilder::new();
        let frag_a = builder.single_term(NfaTerm::element(a, None, None), None);
        let counted_a = frag_a.optional().repeat_counted(0, 1000);
        let frag_b = builder.single_term(NfaTerm::element(b, None, None), None);
        let counted_b = frag_b.optional().repeat_counted(0, 1000);
        let seq = counted_a.concat(counted_b);
        let nfa = fragment_to_table(seq);

        let initial = ActiveStates::from_nfa(&nfa);
        // Feed one 'b': exits first loop, enters second loop
        let after_b = initial.advance(&nfa, b, None, None, None, XsdVersion::V1_0);
        if let ActiveStates::Hybrid { configs, ranged_counter_idx, .. } = &after_b {
            assert_eq!(*ranged_counter_idx, 1,
                "Should switch to counter 1 after 'b'");
            // After switch, configs should be compact: O(states) not O(1000*states)
            assert!(configs.len() < nfa.state_count() * 2,
                "Config count {} should be << state_count {} after switch",
                configs.len(), nfa.state_count());
        }
    }

    #[test]
    fn test_differential_counted_vs_hybrid_switch() {
        // (a?){0,100} followed by (b?){0,100}
        // Build the same NFA, force one to Counted and the other to Hybrid.
        // Compare accept/reject decisions at every step.
        let a = NameId(100);
        let b = NameId(200);
        let builder = FragmentBuilder::new();
        let frag_a = builder.single_term(NfaTerm::element(a, None, None), None);
        let counted_a = frag_a.optional().repeat_counted(0, 100);
        let frag_b = builder.single_term(NfaTerm::element(b, None, None), None);
        let counted_b = frag_b.optional().repeat_counted(0, 100);
        let seq = counted_a.concat(counted_b);
        let nfa = fragment_to_table(seq);

        // Hybrid path (normal)
        let hybrid = ActiveStates::from_nfa(&nfa);

        // Force Counted path
        let initial_config = ActiveConfig::initial(nfa.start_state, nfa.counter_defs.len());
        let mut counted_configs = HashSet::new();
        counted_configs.insert(initial_config);
        let counted = ActiveStates::Counted {
            configs: counted_configs,
            num_counters: nfa.counter_defs.len(),
        }.epsilon_closure(&nfa);

        // Compare: feed a*50, then b*50, checking at each step
        let mut h = hybrid;
        let mut c = counted;
        for _ in 0..50 {
            h = h.advance(&nfa, a, None, None, None, XsdVersion::V1_0);
            c = c.advance(&nfa, a, None, None, None, XsdVersion::V1_0);
            assert_eq!(h.contains_accept(&nfa), c.contains_accept(&nfa),
                "Hybrid/Counted disagree on accept during a-feeding");
            assert_eq!(h.is_empty(), c.is_empty(),
                "Hybrid/Counted disagree on empty during a-feeding");
        }
        for _ in 0..50 {
            h = h.advance(&nfa, b, None, None, None, XsdVersion::V1_0);
            c = c.advance(&nfa, b, None, None, None, XsdVersion::V1_0);
            assert_eq!(h.contains_accept(&nfa), c.contains_accept(&nfa),
                "Hybrid/Counted disagree on accept during b-feeding");
            assert_eq!(h.is_empty(), c.is_empty(),
                "Hybrid/Counted disagree on empty during b-feeding");
        }
        // Both should accept after a*50 + b*50
        assert!(h.contains_accept(&nfa));
        assert!(c.contains_accept(&nfa));

        // Feed one more b — still accepted (total b=51 ≤ 100)
        h = h.advance(&nfa, b, None, None, None, XsdVersion::V1_0);
        c = c.advance(&nfa, b, None, None, None, XsdVersion::V1_0);
        assert_eq!(h.contains_accept(&nfa), c.contains_accept(&nfa));
    }
}
