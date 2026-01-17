//! NFA data structures for content model validation
//!
//! This module defines the core NFA (Nondeterministic Finite Automaton) structures
//! used to represent compiled XSD content models.

use std::collections::HashSet;

use crate::ids::{ElementKey, NameId};
use crate::parser::location::SourceRef;
use crate::types::complex::{NamespaceConstraint, ProcessContents};
use super::substitution::SubstitutionGroupMap;

/// Unique identifier for NFA states within a table
pub type StateId = u32;

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
}

impl NfaTable {
    /// Create a new NFA table with the given states
    pub fn new(states: Vec<NfaState>, start_state: StateId, accept_state: StateId) -> Self {
        Self {
            states,
            start_state,
            accept_state,
        }
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
    },
    /// Match any element satisfying wildcard constraints
    Wildcard {
        /// Namespace constraint for allowed namespaces
        namespace_constraint: NamespaceConstraint,
        /// How to process matched content
        process_contents: ProcessContents,
    },
}

impl NfaTerm {
    /// Create an element term
    pub fn element(name: NameId, namespace: Option<NameId>, element_key: Option<ElementKey>) -> Self {
        NfaTerm::Element {
            name,
            namespace,
            element_key,
        }
    }

    /// Create a wildcard term
    pub fn wildcard(namespace_constraint: NamespaceConstraint, process_contents: ProcessContents) -> Self {
        NfaTerm::Wildcard {
            namespace_constraint,
            process_contents,
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
}

/// Compute the epsilon closure for a set of start states.
pub fn epsilon_closure(
    nfa: &NfaTable,
    start_states: impl IntoIterator<Item = StateId>,
) -> HashSet<StateId> {
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
) -> bool {
    match term {
        NfaTerm::Element {
            name,
            namespace,
            element_key,
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
            ..
        } => wildcard_matches(namespace_constraint, element_namespace, target_namespace),
    }
}

/// Advance NFA states by matching an element and applying epsilon closure.
pub fn advance_states(
    nfa: &NfaTable,
    start_states: impl IntoIterator<Item = StateId>,
    element_name: NameId,
    element_namespace: Option<NameId>,
    target_namespace: Option<NameId>,
    substitution_groups: Option<&SubstitutionGroupMap>,
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
        ) {
            for target in state.consuming_transitions() {
                next.insert(target);
            }
        }
    }

    epsilon_closure(nfa, next)
}

/// Advance NFA states with element-over-wildcard priority (XSD 1.1).
pub fn advance_with_priority(
    nfa: &NfaTable,
    start_states: impl IntoIterator<Item = StateId>,
    element_name: NameId,
    element_namespace: Option<NameId>,
    target_namespace: Option<NameId>,
    substitution_groups: Option<&SubstitutionGroupMap>,
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

fn wildcard_matches(
    constraint: &NamespaceConstraint,
    element_namespace: Option<NameId>,
    target_namespace: Option<NameId>,
) -> bool {
    match constraint {
        NamespaceConstraint::Any => true,
        NamespaceConstraint::Other => element_namespace != target_namespace,
        NamespaceConstraint::TargetNamespace => element_namespace == target_namespace,
        NamespaceConstraint::Local => element_namespace.is_none(),
        NamespaceConstraint::List(list) => list.contains(&element_namespace),
    }
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
        let next = advance_states(&nfa, [0], NameId(1), None, None, None);
        assert_eq!(next, make_set(&[3, 4]));

        let next = advance_states(&nfa, [0], NameId(2), None, None, None);
        assert_eq!(next, make_set(&[4]));
    }

    #[test]
    fn test_advance_with_priority_prefers_element() {
        let nfa = make_priority_nfa();
        let next = advance_with_priority(&nfa, [0], NameId(1), None, None, None);
        assert_eq!(next, make_set(&[3]));

        let next = advance_with_priority(&nfa, [0], NameId(2), None, None, None);
        assert_eq!(next, make_set(&[4]));
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
            Some(&map)
        ));
    }
}
