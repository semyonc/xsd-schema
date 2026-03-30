//! NFA fragment structures and composition helpers
//!
//! This module implements Thompson's construction algorithm for building NFAs
//! from content model particles. Fragments are composable building blocks that
//! can be concatenated, alternated, or repeated.

use crate::parser::location::SourceRef;

use super::nfa::{CounterDef, CounterId, NfaState, NfaTable, NfaTerm, StateId, TransitionKind};

/// A composable NFA fragment with single entry and exit points
///
/// Fragments are the building blocks for constructing complex NFAs using
/// Thompson's construction. They maintain the invariant of having exactly
/// one start state and one end state, which enables easy composition.
#[derive(Debug, Clone)]
pub struct NfaFragment {
    /// All states in this fragment (indices are local to fragment)
    pub states: Vec<NfaState>,
    /// Entry point state index (into states vector)
    pub start: usize,
    /// Exit point state index (into states vector)
    pub end: usize,
    /// Counter definitions for counted loops within this fragment
    pub counter_defs: Vec<CounterDef>,
    /// Whether this fragment can match the empty string (end reachable from
    /// start without consuming any input).  Tracked incrementally through
    /// all composition operations and used to set `CounterDef::body_nullable`.
    pub nullable: bool,
}

impl NfaFragment {
    /// Create a new fragment from states with specified start/end (no counters, not nullable)
    pub fn new(states: Vec<NfaState>, start: usize, end: usize) -> Self {
        debug_assert!(start < states.len(), "start index out of bounds");
        debug_assert!(end < states.len(), "end index out of bounds");
        Self { states, start, end, counter_defs: Vec::new(), nullable: false }
    }

    /// Create a new fragment with counter definitions
    pub fn with_counters(
        states: Vec<NfaState>,
        start: usize,
        end: usize,
        counter_defs: Vec<CounterDef>,
        nullable: bool,
    ) -> Self {
        debug_assert!(start < states.len(), "start index out of bounds");
        debug_assert!(end < states.len(), "end index out of bounds");
        Self { states, start, end, counter_defs, nullable }
    }

    /// Verifies the position-based ID invariant: `state.id == index` for all states.
    ///
    /// This invariant is critical for correctness of all composition operations
    /// (`concat`, `alternate`, etc.) which use position-based offset arithmetic.
    /// `FragmentBuilder` guarantees it at construction; combinators preserve it.
    /// Any new fragment creation path MUST maintain this invariant.
    fn assert_ids_normalized(&self) {
        for (pos, state) in self.states.iter().enumerate() {
            debug_assert_eq!(
                state.id,
                pos as StateId,
                "Fragment ID invariant violated: state at position {} has id {}",
                pos,
                state.id
            );
        }
    }

    /// Get the start state
    pub fn start_state(&self) -> &NfaState {
        &self.states[self.start]
    }

    /// Get the end state
    pub fn end_state(&self) -> &NfaState {
        &self.states[self.end]
    }

    /// Get a mutable reference to a state by local index
    pub fn get_state_mut(&mut self, index: usize) -> Option<&mut NfaState> {
        self.states.get_mut(index)
    }

    /// Concatenate two fragments: self followed by other
    ///
    /// Creates an epsilon transition from self's end state to other's start state.
    /// The resulting fragment starts at self's start and ends at other's end.
    pub fn concat(mut self, mut other: NfaFragment) -> NfaFragment {
        let nullable = self.nullable && other.nullable;

        self.assert_ids_normalized();
        other.assert_ids_normalized();

        let state_offset = self.states.len();
        let counter_offset = self.counter_defs.len() as CounterId;

        // Offset all state IDs and counter IDs in other fragment
        for state in &mut other.states {
            state.id += state_offset as StateId;
            for trans in &mut state.transitions {
                trans.target += state_offset as StateId;
                trans.kind = trans.kind.offset_counter(counter_offset);
            }
        }

        // Add epsilon transition from self.end to other.start
        let other_start = other.start + state_offset;
        self.states[self.end].add_epsilon(other_start as StateId);

        // Merge states and counter defs
        let new_end = other.end + state_offset;
        self.states.extend(other.states);
        self.counter_defs.extend(other.counter_defs);

        NfaFragment::with_counters(self.states, self.start, new_end, self.counter_defs, nullable)
    }

    /// Alternate two fragments: self | other
    ///
    /// Creates a new start state with epsilon transitions to both fragments,
    /// and a new end state that both fragments converge to.
    pub fn alternate(mut self, mut other: NfaFragment) -> NfaFragment {
        let nullable = self.nullable || other.nullable;

        self.assert_ids_normalized();
        other.assert_ids_normalized();

        // Create new start and end states
        let new_start_id = (self.states.len() + other.states.len()) as StateId;
        let new_end_id = new_start_id + 1;

        let mut new_start = NfaState::epsilon(new_start_id, None);
        let new_end = NfaState::epsilon(new_end_id, None);

        // Offset other fragment's state IDs and counter IDs
        let other_state_offset = self.states.len();
        let counter_offset = self.counter_defs.len() as CounterId;
        for state in &mut other.states {
            state.id += other_state_offset as StateId;
            for trans in &mut state.transitions {
                trans.target += other_state_offset as StateId;
                trans.kind = trans.kind.offset_counter(counter_offset);
            }
        }

        // Add epsilon from new start to both fragment starts
        new_start.add_epsilon(self.start as StateId);
        new_start.add_epsilon((other.start + other_state_offset) as StateId);

        // Add epsilon from both fragment ends to new end
        self.states[self.end].add_epsilon(new_end_id);
        other.states[other.end].add_epsilon(new_end_id);

        // Merge all states and counter defs
        let mut states = self.states;
        states.extend(other.states);
        states.push(new_start);
        states.push(new_end);

        let mut counter_defs = self.counter_defs;
        counter_defs.extend(other.counter_defs);

        NfaFragment::with_counters(states, new_start_id as usize, new_end_id as usize, counter_defs, nullable)
    }

    /// Make fragment optional: self?
    ///
    /// Adds an epsilon transition from start to end, allowing the fragment
    /// to be skipped entirely.
    pub fn optional(mut self) -> NfaFragment {
        self.assert_ids_normalized();
        // Add epsilon from start to end
        let end_id = self.end as StateId;
        self.states[self.start].add_epsilon(end_id);
        self.nullable = true;
        self
    }

    /// Kleene star: self*
    ///
    /// Allows zero or more repetitions of the fragment.
    /// Adds loop back from end to start, plus makes it optional.
    pub fn repeat_star(mut self) -> NfaFragment {
        self.assert_ids_normalized();
        // Add epsilon loop from end back to start
        let start_id = self.start as StateId;
        self.states[self.end].add_epsilon(start_id);

        // Make optional (zero occurrences allowed) — already normalized
        let end_id = self.end as StateId;
        self.states[self.start].add_epsilon(end_id);
        self.nullable = true;
        self
    }

    /// Plus repetition: self+
    ///
    /// Requires at least one occurrence, then allows more.
    /// Adds loop back from end to start (no optional bypass).
    pub fn repeat_plus(mut self) -> NfaFragment {
        self.assert_ids_normalized();
        // Add epsilon loop from end back to start
        let start_id = self.start as StateId;
        self.states[self.end].add_epsilon(start_id);
        // nullable iff one occurrence can match empty
        // (self.nullable is already set from the body)
        self
    }

    /// Repeat exactly n times: self{n}
    ///
    /// Creates n concatenated copies of the fragment.
    /// For n=0, returns an epsilon fragment.
    pub fn repeat_exact(self, n: u32) -> NfaFragment {
        if n == 0 {
            return FragmentBuilder::new().epsilon_fragment();
        }

        // nullable: all n copies must be nullable → self.nullable
        // (concat propagates: a.nullable && b.nullable)
        let mut result = self.clone();
        for _ in 1..n {
            result = result.concat(self.clone());
        }
        result
    }

    /// Counted repeat: uses counter transitions for self{min,max}.
    ///
    /// Produces a compact loop structure with 3 extra states (entry, guard, exit)
    /// regardless of min/max values. Counter tracks completed iterations.
    ///
    /// Structure:
    /// ```text
    /// entry --CounterReset(c)--> body_start
    /// body_end --CounterIncrement(c)--> guard
    /// guard --CounterMaxGuard(c)--> body_start   [loop if count < max]
    /// guard --CounterMinGuard(c)--> exit          [exit if count >= min]
    /// [if min == 0: entry --Epsilon--> exit]      [bypass]
    /// ```
    pub fn repeat_counted(mut self, min: u32, max: u32) -> NfaFragment {
        debug_assert!(min <= max, "repeat_counted: min ({min}) > max ({max})");

        // Capture body nullability *before* adding counter infrastructure.
        let body_nullable = self.nullable;

        self.assert_ids_normalized();

        // Allocate counter
        let counter_id = self.counter_defs.len() as CounterId;
        self.counter_defs.push(CounterDef { min, max, body_nullable });

        // Allocate new states: entry, guard, exit
        let entry_idx = self.states.len();
        let guard_idx = entry_idx + 1;
        let exit_idx = entry_idx + 2;

        let entry_id = entry_idx as StateId;
        let guard_id = guard_idx as StateId;
        let exit_id = exit_idx as StateId;
        let body_start_id = self.start as StateId;

        // entry → CounterReset → body_start
        let mut entry = NfaState::epsilon(entry_id, None);
        entry.add_transition(body_start_id, TransitionKind::CounterReset(counter_id));

        // Optional bypass: entry → exit (if min == 0)
        if min == 0 {
            entry.add_epsilon(exit_id);
        }

        // body_end → CounterIncrement → guard
        self.states[self.end].add_transition(guard_id, TransitionKind::CounterIncrement(counter_id));

        // guard → CounterMaxGuard → body_start (loop back)
        // guard → CounterMinGuard → exit (exit loop)
        let mut guard = NfaState::epsilon(guard_id, None);
        guard.add_transition(body_start_id, TransitionKind::CounterMaxGuard(counter_id));
        guard.add_transition(exit_id, TransitionKind::CounterMinGuard(counter_id));

        let exit = NfaState::epsilon(exit_id, None);

        self.states.push(entry);
        self.states.push(guard);
        self.states.push(exit);

        // The counted loop is nullable if min==0 (bypass edge) or body is nullable
        // (all min iterations can complete without consuming input).
        let nullable = min == 0 || body_nullable;

        NfaFragment::with_counters(
            self.states,
            entry_idx,
            exit_idx,
            self.counter_defs,
            nullable,
        )
    }

    /// Repeat between min and max times: self{min,max}
    ///
    /// Creates min mandatory copies followed by (max-min) optional copies.
    /// If max is None, creates min copies followed by a star.
    pub fn repeat_range(self, min: u32, max: Option<u32>) -> NfaFragment {
        match (min, max) {
            (0, Some(0)) => FragmentBuilder::new().epsilon_fragment(),
            (0, Some(1)) => self.optional(),
            (0, None) => self.repeat_star(),
            (1, Some(1)) => self,
            (1, None) => self.repeat_plus(),
            (n, Some(m)) if n == m => self.repeat_exact(n),
            (n, Some(m)) => {
                // n mandatory + (m-n) optional
                let mut result = self.clone().repeat_exact(n);
                for _ in n..m {
                    result = result.concat(self.clone().optional());
                }
                result
            }
            (n, None) => {
                // n mandatory + star
                let mandatory = self.clone().repeat_exact(n);
                mandatory.concat(self.repeat_star())
            }
        }
    }
}

/// Builder for constructing NFA fragments with fragment-local state IDs.
///
/// Fragments are created with position-based IDs (`state.id == index`),
/// which is the invariant required by all composition operations.
/// The builder is stateless — each fragment starts with IDs from 0.
#[derive(Debug)]
pub struct FragmentBuilder;

impl FragmentBuilder {
    /// Create a new fragment builder
    pub fn new() -> Self {
        Self
    }

    /// Build a single-term fragment
    ///
    /// Creates a fragment with one term state (id=0) and one epsilon exit
    /// state (id=1). The term state has a consuming transition to the exit.
    pub fn single_term(&self, term: NfaTerm, origin: Option<SourceRef>) -> NfaFragment {
        let mut term_state = NfaState::with_term(0, term, origin);
        let exit_state = NfaState::epsilon(1, None);

        // Add consuming transition from term state to exit
        term_state.add_consume(1);

        NfaFragment::new(vec![term_state, exit_state], 0, 1)
    }

    /// Build an epsilon-only fragment
    ///
    /// Creates a minimal fragment that matches nothing (empty string).
    /// Used for optional content and as base case for empty sequences.
    pub fn epsilon_fragment(&self) -> NfaFragment {
        let state = NfaState::epsilon(0, None);
        let mut frag = NfaFragment::new(vec![state], 0, 0);
        frag.nullable = true;
        frag
    }
}

impl Default for FragmentBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Convert a fragment to a complete NFA table.
///
/// Asserts the fragment's ID invariant (`state.id == position`) and
/// wraps the states into an `NfaTable` with the fragment's start/end
/// as start/accept states.
pub fn fragment_to_table(fragment: NfaFragment) -> NfaTable {
    fragment.assert_ids_normalized();

    let start_state = fragment.start as StateId;
    let accept_state = fragment.end as StateId;

    NfaTable::with_counters(fragment.states, start_state, accept_state, fragment.counter_defs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::NameId;

    fn make_element_term(name: u32) -> NfaTerm {
        NfaTerm::element(NameId(name), None, None)
    }

    #[test]
    fn test_single_term_fragment() {
        let builder = FragmentBuilder::new();
        let frag = builder.single_term(make_element_term(1), None);

        assert_eq!(frag.states.len(), 2);
        assert_eq!(frag.start, 0);
        assert_eq!(frag.end, 1);
        assert!(frag.states[0].term.is_some());
        assert!(frag.states[1].term.is_none()); // epsilon exit
    }

    #[test]
    fn test_epsilon_fragment() {
        let builder = FragmentBuilder::new();
        let frag = builder.epsilon_fragment();

        assert_eq!(frag.states.len(), 1);
        assert_eq!(frag.start, 0);
        assert_eq!(frag.end, 0); // Same state
        assert!(frag.states[0].term.is_none());
    }

    #[test]
    fn test_concat() {
        let builder = FragmentBuilder::new();
        let a = builder.single_term(make_element_term(1), None);
        let b = builder.single_term(make_element_term(2), None);

        let concat = a.concat(b);

        // a(2 states) + b(2 states) = 4 states
        assert_eq!(concat.states.len(), 4);
        assert_eq!(concat.start, 0); // a's start
        assert_eq!(concat.end, 3); // b's end (offset by 2)

        // Check epsilon from a's end to b's start
        let a_end = &concat.states[1];
        assert!(a_end.epsilon_transitions().any(|t| t == 2));
    }

    #[test]
    fn test_alternate() {
        let builder = FragmentBuilder::new();
        let a = builder.single_term(make_element_term(1), None);
        let b = builder.single_term(make_element_term(2), None);

        let alt = a.alternate(b);

        // a(2) + b(2) + new_start(1) + new_end(1) = 6 states
        assert_eq!(alt.states.len(), 6);

        // New start should have epsilon to both a.start and b.start
        let new_start = &alt.states[alt.start];
        let eps: Vec<_> = new_start.epsilon_transitions().collect();
        assert_eq!(eps.len(), 2);
        assert!(eps.contains(&0)); // a's start
        assert!(eps.contains(&2)); // b's start (offset by 2)
    }

    #[test]
    fn test_optional() {
        let builder = FragmentBuilder::new();
        let frag = builder.single_term(make_element_term(1), None);
        let opt = frag.optional();

        // Check epsilon from start to end (bypass)
        let start = &opt.states[opt.start];
        assert!(start.epsilon_transitions().any(|t| t == opt.end as StateId));
    }

    #[test]
    fn test_repeat_star() {
        let builder = FragmentBuilder::new();
        let frag = builder.single_term(make_element_term(1), None);
        let star = frag.repeat_star();

        // Check epsilon loop from end to start
        let end = &star.states[star.end];
        assert!(end.epsilon_transitions().any(|t| t == star.start as StateId));

        // Check optional bypass
        let start = &star.states[star.start];
        assert!(start.epsilon_transitions().any(|t| t == star.end as StateId));
    }

    #[test]
    fn test_repeat_plus() {
        let builder = FragmentBuilder::new();
        let frag = builder.single_term(make_element_term(1), None);
        let plus = frag.repeat_plus();

        // Check epsilon loop from end to start
        let end = &plus.states[plus.end];
        assert!(end.epsilon_transitions().any(|t| t == plus.start as StateId));

        // Should NOT have optional bypass
        let start = &plus.states[plus.start];
        assert!(!start.epsilon_transitions().any(|t| t == plus.end as StateId));
    }

    #[test]
    fn test_repeat_exact() {
        let builder = FragmentBuilder::new();
        let frag = builder.single_term(make_element_term(1), None);
        let exact = frag.repeat_exact(3);

        // 3 copies of 2-state fragment connected = 6 states
        assert_eq!(exact.states.len(), 6);
    }

    #[test]
    fn test_repeat_range() {
        let builder = FragmentBuilder::new();

        // {0,1} = optional
        let frag1 = builder.single_term(make_element_term(1), None);
        let opt = frag1.repeat_range(0, Some(1));
        let start = &opt.states[opt.start];
        assert!(start.epsilon_transitions().any(|t| t == opt.end as StateId));

        // {2,4} = 2 mandatory + 2 optional
        let frag2 = builder.single_term(make_element_term(2), None);
        let range = frag2.repeat_range(2, Some(4));
        // 2*2 mandatory + 2*2 optional = 8 states
        assert_eq!(range.states.len(), 8);
    }

    #[test]
    fn test_fragment_to_table() {
        let builder = FragmentBuilder::new();
        let frag = builder.single_term(make_element_term(1), None);
        let table = fragment_to_table(frag);

        assert_eq!(table.start_state, 0);
        assert_eq!(table.accept_state, 1);
        assert_eq!(table.state_count(), 2);
    }
}
