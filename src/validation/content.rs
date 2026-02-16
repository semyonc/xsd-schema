//! Content model state dispatch for instance validation
//!
//! Wraps NFA and AllGroup content model states into a unified enum,
//! providing a common interface for advancing the content model
//! and checking completion.

use std::collections::HashSet;

use crate::compiler::{
    AllGroupModel, AllGroupState, TermMatchResult,
    term_matches_with_substitution,
    NfaTable, NfaTerm, StateId,
    advance_states, advance_with_priority, epsilon_closure,
    nfa_term_matches,
    SubstitutionGroupMap, ContentModelMatcher,
};
use crate::ids::{ElementKey, NameId, TypeKey};
use crate::schema::model::XsdVersion;

/// Information about a matched element from the content model
#[derive(Debug, Clone, Copy)]
pub struct ElementMatchInfo {
    /// The element key from the matching NFA term (if any)
    pub element_key: Option<ElementKey>,
    /// The resolved type for local elements (if any)
    pub resolved_type: Option<TypeKey>,
}

/// Unified content model validation state
///
/// Wraps either an NFA-based or AllGroup-based content model into a single
/// enum so that `SchemaValidator` can advance the content model without
/// caring which underlying representation is in use.
#[derive(Debug, Clone)]
pub enum ContentValidatorState {
    /// NFA-based content model (sequence, choice, etc.)
    Nfa {
        nfa: NfaTable,
        active_states: HashSet<StateId>,
    },
    /// All-group content model (unordered particles)
    AllGroup {
        model: AllGroupModel,
        state: AllGroupState,
    },
    /// Simple content (text only, no child elements)
    Simple,
    /// Empty content (no children or text)
    Empty,
}

impl ContentValidatorState {
    /// Create a content validator state from a compiled content model matcher
    pub fn from_matcher(matcher: ContentModelMatcher) -> Self {
        match matcher {
            ContentModelMatcher::Nfa(nfa) => Self::from_nfa(nfa),
            ContentModelMatcher::AllGroup(model) => Self::from_all_group(model),
            ContentModelMatcher::WithOpenContent { nfa, .. } => {
                // For now, treat open content the same as plain NFA.
                // Full open content support will be added in a later task.
                Self::from_nfa(nfa)
            }
        }
    }

    /// Create a content validator state from an NFA table
    ///
    /// Computes the initial epsilon closure from the start state.
    pub fn from_nfa(nfa: NfaTable) -> Self {
        let initial = epsilon_closure(&nfa, std::iter::once(nfa.start_state));
        ContentValidatorState::Nfa {
            nfa,
            active_states: initial,
        }
    }

    /// Create a content validator state from an all-group model
    pub fn from_all_group(model: AllGroupModel) -> Self {
        let state = model.create_state();
        ContentValidatorState::AllGroup { model, state }
    }

    /// Advance the content model with a child element
    ///
    /// Returns `None` if the element was rejected.
    /// Returns `Some(ElementMatchInfo)` if accepted, containing the
    /// `ElementKey` and `resolved_type` from the matching NFA term (if any).
    pub fn advance_element(
        &mut self,
        name: NameId,
        namespace: Option<NameId>,
        target_ns: Option<NameId>,
        xsd_version: XsdVersion,
        subst_groups: Option<&SubstitutionGroupMap>,
    ) -> Option<ElementMatchInfo> {
        match self {
            ContentValidatorState::Nfa { nfa, active_states } => {
                // First, find the matching element info before advancing
                let match_info = find_nfa_match_info(
                    nfa, active_states, name, namespace, target_ns, subst_groups,
                );

                let next = match xsd_version {
                    XsdVersion::V1_0 => advance_states(
                        nfa,
                        active_states.iter().copied(),
                        name,
                        namespace,
                        target_ns,
                        subst_groups,
                    ),
                    XsdVersion::V1_1 => advance_with_priority(
                        nfa,
                        active_states.iter().copied(),
                        name,
                        namespace,
                        target_ns,
                        subst_groups,
                    ),
                };
                if next.is_empty() {
                    return None;
                }
                *active_states = next;
                Some(match_info)
            }
            ContentValidatorState::AllGroup { model, state } => {
                // Try to match against each particle in the all-group
                for (i, particle) in model.particles.iter().enumerate() {
                    if !state.can_accept(i) {
                        continue;
                    }
                    let result = term_matches_with_substitution(
                        &particle.term,
                        name,
                        namespace,
                        target_ns,
                        subst_groups,
                    );
                    if result == TermMatchResult::Match {
                        if state.accept(i) {
                            let (element_key, resolved_type) = match &particle.term {
                                NfaTerm::Element { element_key, resolved_type, .. } => {
                                    (*element_key, *resolved_type)
                                }
                                _ => (None, None),
                            };
                            return Some(ElementMatchInfo { element_key, resolved_type });
                        }
                        return None;
                    }
                }
                None
            }
            ContentValidatorState::Simple | ContentValidatorState::Empty => {
                // Simple and Empty content models do not accept child elements
                None
            }
        }
    }

    /// Check whether the content model is in a complete (accepting) state
    ///
    /// For NFA: any active state is the accept state.
    /// For AllGroup: all required particles have been satisfied.
    pub fn is_complete(&self) -> bool {
        match self {
            ContentValidatorState::Nfa { nfa, active_states } => {
                active_states.iter().any(|&s| nfa.is_accept(s))
            }
            ContentValidatorState::AllGroup { model, state } => {
                state.is_satisfied(model)
            }
            ContentValidatorState::Simple | ContentValidatorState::Empty => true,
        }
    }

    /// Non-mutating lookahead: would the given element be accepted?
    ///
    /// This does not change the state of the content model.
    pub fn would_accept(
        &self,
        name: NameId,
        namespace: Option<NameId>,
        target_ns: Option<NameId>,
        xsd_version: XsdVersion,
        subst_groups: Option<&SubstitutionGroupMap>,
    ) -> bool {
        match self {
            ContentValidatorState::Nfa { nfa, active_states } => {
                let next = match xsd_version {
                    XsdVersion::V1_0 => advance_states(
                        nfa,
                        active_states.iter().copied(),
                        name,
                        namespace,
                        target_ns,
                        subst_groups,
                    ),
                    XsdVersion::V1_1 => advance_with_priority(
                        nfa,
                        active_states.iter().copied(),
                        name,
                        namespace,
                        target_ns,
                        subst_groups,
                    ),
                };
                !next.is_empty()
            }
            ContentValidatorState::AllGroup { model, state } => {
                for (i, particle) in model.particles.iter().enumerate() {
                    if !state.can_accept(i) {
                        continue;
                    }
                    let result = term_matches_with_substitution(
                        &particle.term,
                        name,
                        namespace,
                        target_ns,
                        subst_groups,
                    );
                    if result == TermMatchResult::Match {
                        return true;
                    }
                }
                false
            }
            ContentValidatorState::Simple | ContentValidatorState::Empty => false,
        }
    }
}

/// Find the ElementMatchInfo from the NFA term that matches the given element
fn find_nfa_match_info(
    nfa: &NfaTable,
    active_states: &HashSet<StateId>,
    name: NameId,
    namespace: Option<NameId>,
    target_ns: Option<NameId>,
    subst_groups: Option<&SubstitutionGroupMap>,
) -> ElementMatchInfo {
    let closure = epsilon_closure(nfa, active_states.iter().copied());
    for state_id in closure {
        if let Some(state) = nfa.get_state(state_id) {
            if let Some(ref term) = state.term {
                if nfa_term_matches(term, name, namespace, target_ns, subst_groups) {
                    if let NfaTerm::Element { element_key, resolved_type, .. } = term {
                        return ElementMatchInfo {
                            element_key: *element_key,
                            resolved_type: *resolved_type,
                        };
                    }
                }
            }
        }
    }
    ElementMatchInfo {
        element_key: None,
        resolved_type: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::{NfaState, NfaTerm};

    /// Build a simple NFA that accepts a single element with given name_id
    fn single_element_nfa(name: NameId, namespace: Option<NameId>) -> NfaTable {
        // State 0: start (epsilon) -> State 1
        // State 1: element term, consume -> State 2
        // State 2: accept (epsilon)
        let mut s0 = NfaState::epsilon(0, None);
        s0.add_epsilon(1);

        let mut s1 = NfaState::with_term(1, NfaTerm::element(name, namespace, None), None);
        s1.add_consume(2);

        let s2 = NfaState::epsilon(2, None);

        NfaTable::new(vec![s0, s1, s2], 0, 2)
    }

    /// Build an NFA that accepts a sequence: elem_a then elem_b
    fn sequence_nfa(
        name_a: NameId, ns_a: Option<NameId>,
        name_b: NameId, ns_b: Option<NameId>,
    ) -> NfaTable {
        // State 0: start (epsilon) -> State 1
        // State 1: element A, consume -> State 2
        // State 2: epsilon -> State 3
        // State 3: element B, consume -> State 4
        // State 4: accept (epsilon)
        let mut s0 = NfaState::epsilon(0, None);
        s0.add_epsilon(1);

        let mut s1 = NfaState::with_term(1, NfaTerm::element(name_a, ns_a, None), None);
        s1.add_consume(2);

        let mut s2 = NfaState::epsilon(2, None);
        s2.add_epsilon(3);

        let mut s3 = NfaState::with_term(3, NfaTerm::element(name_b, ns_b, None), None);
        s3.add_consume(4);

        let s4 = NfaState::epsilon(4, None);

        NfaTable::new(vec![s0, s1, s2, s3, s4], 0, 4)
    }

    #[test]
    fn test_nfa_single_element_accepted() {
        let name = NameId(1);
        let nfa = single_element_nfa(name, None);
        let mut state = ContentValidatorState::from_nfa(nfa);

        assert!(!state.is_complete(), "should not be complete before any element");
        assert!(state.advance_element(name, None, None, XsdVersion::V1_0, None).is_some());
        assert!(state.is_complete(), "should be complete after matching element");
    }

    #[test]
    fn test_nfa_single_element_rejected() {
        let name = NameId(1);
        let wrong_name = NameId(2);
        let nfa = single_element_nfa(name, None);
        let mut state = ContentValidatorState::from_nfa(nfa);

        assert!(state.advance_element(wrong_name, None, None, XsdVersion::V1_0, None).is_none());
        assert!(!state.is_complete());
    }

    #[test]
    fn test_nfa_sequence() {
        let a = NameId(10);
        let b = NameId(20);
        let nfa = sequence_nfa(a, None, b, None);
        let mut state = ContentValidatorState::from_nfa(nfa);

        assert!(!state.is_complete());
        assert!(state.advance_element(a, None, None, XsdVersion::V1_0, None).is_some());
        assert!(!state.is_complete(), "only first element seen");
        assert!(state.advance_element(b, None, None, XsdVersion::V1_0, None).is_some());
        assert!(state.is_complete(), "both elements matched");
    }

    #[test]
    fn test_nfa_sequence_wrong_order() {
        let a = NameId(10);
        let b = NameId(20);
        let nfa = sequence_nfa(a, None, b, None);
        let mut state = ContentValidatorState::from_nfa(nfa);

        // Try b first - should be rejected
        assert!(state.advance_element(b, None, None, XsdVersion::V1_0, None).is_none());
    }

    #[test]
    fn test_nfa_would_accept() {
        let name = NameId(1);
        let wrong = NameId(2);
        let nfa = single_element_nfa(name, None);
        let state = ContentValidatorState::from_nfa(nfa);

        assert!(state.would_accept(name, None, None, XsdVersion::V1_0, None));
        assert!(!state.would_accept(wrong, None, None, XsdVersion::V1_0, None));
    }

    #[test]
    fn test_all_group_any_order() {
        use crate::compiler::{AllParticle, MaxOccurs};

        let a = NameId(10);
        let b = NameId(20);

        let model = AllGroupModel::new(vec![
            AllParticle::new(NfaTerm::element(a, None, None), 1, MaxOccurs::Bounded(1), None),
            AllParticle::new(NfaTerm::element(b, None, None), 1, MaxOccurs::Bounded(1), None),
        ]);

        // Order: b, a (reversed) should still work
        let mut state = ContentValidatorState::from_all_group(model);
        assert!(!state.is_complete());
        assert!(state.advance_element(b, None, None, XsdVersion::V1_0, None).is_some());
        assert!(!state.is_complete(), "only one of two required particles matched");
        assert!(state.advance_element(a, None, None, XsdVersion::V1_0, None).is_some());
        assert!(state.is_complete(), "both particles satisfied");
    }

    #[test]
    fn test_all_group_missing_required() {
        use crate::compiler::{AllParticle, MaxOccurs};

        let a = NameId(10);
        let b = NameId(20);

        let model = AllGroupModel::new(vec![
            AllParticle::new(NfaTerm::element(a, None, None), 1, MaxOccurs::Bounded(1), None),
            AllParticle::new(NfaTerm::element(b, None, None), 1, MaxOccurs::Bounded(1), None),
        ]);

        let mut state = ContentValidatorState::from_all_group(model);
        assert!(state.advance_element(a, None, None, XsdVersion::V1_0, None).is_some());
        // Don't supply b
        assert!(!state.is_complete(), "b is still required");
    }

    #[test]
    fn test_simple_rejects_elements() {
        let mut state = ContentValidatorState::Simple;
        assert!(state.advance_element(NameId(1), None, None, XsdVersion::V1_0, None).is_none());
        assert!(state.is_complete());
    }

    #[test]
    fn test_empty_rejects_elements() {
        let mut state = ContentValidatorState::Empty;
        assert!(state.advance_element(NameId(1), None, None, XsdVersion::V1_0, None).is_none());
        assert!(state.is_complete());
    }

    #[test]
    fn test_from_matcher_nfa() {
        let name = NameId(1);
        let nfa = single_element_nfa(name, None);
        let matcher = ContentModelMatcher::Nfa(nfa);
        let mut state = ContentValidatorState::from_matcher(matcher);
        assert!(state.advance_element(name, None, None, XsdVersion::V1_0, None).is_some());
        assert!(state.is_complete());
    }

    #[test]
    fn test_from_matcher_all_group() {
        use crate::compiler::{AllParticle, MaxOccurs};

        let a = NameId(5);
        let model = AllGroupModel::new(vec![
            AllParticle::new(NfaTerm::element(a, None, None), 0, MaxOccurs::Bounded(1), None),
        ]);
        let matcher = ContentModelMatcher::AllGroup(model);
        let state = ContentValidatorState::from_matcher(matcher);
        // Optional particle, so complete even without matching
        assert!(state.is_complete());
    }
}
