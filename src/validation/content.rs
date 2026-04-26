//! Content model state dispatch for instance validation
//!
//! Wraps NFA and AllGroup content model states into a unified enum,
//! providing a common interface for advancing the content model
//! and checking completion.

use crate::compiler::{
    AllGroupModel, AllGroupState, OpenContentMode as AllGroupOpenContentMode, TermMatchResult,
    term_matches_with_substitution,
    NfaTable, NfaTerm,
    SubstitutionGroupMap, ContentModelMatcher,
    ActiveStates,
};
use crate::ids::{ElementKey, NameId, TypeKey};
use crate::schema::model::XsdVersion;
use crate::types::complex::{
    NamespaceConstraint, OpenContentMode as TypesOpenContentMode,
    ProcessContents, not_qnames_exclude,
};

/// Open content information carried through validation
#[derive(Debug, Clone)]
pub struct OpenContentInfo {
    /// Open content mode
    pub mode: TypesOpenContentMode,
    /// Namespace constraint for allowed namespaces
    pub namespace_constraint: NamespaceConstraint,
    /// How to process matched content
    pub process_contents: ProcessContents,
    /// QNames excluded by notQName (pre-expanded concrete pairs)
    pub not_qnames: Vec<(Option<NameId>, NameId)>,
}

/// Information about a matched element from the content model
#[derive(Debug, Clone, Copy)]
pub struct ElementMatchInfo {
    /// The element key from the matching NFA term (if any)
    pub element_key: Option<ElementKey>,
    /// The resolved type for local elements (if any)
    pub resolved_type: Option<TypeKey>,
    /// Process contents mode from open content wildcard (if matched via open content)
    pub process_contents: Option<ProcessContents>,
}

/// Phase of AllGroupExtension composite validation.
#[cfg(feature = "xsd11")]
#[derive(Debug, Clone)]
pub enum AllGroupExtPhase {
    /// Validating the all-group part (base type particles).
    AllGroup,
    /// Transitioned to the NFA extension part.
    Nfa(ActiveStates),
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
        active_states: ActiveStates,
        open_content: Option<OpenContentInfo>,
    },
    /// All-group content model (unordered particles)
    AllGroup {
        model: AllGroupModel,
        state: AllGroupState,
        /// `true` once a suffix open-content element has been matched —
        /// further declared all-group particles are no longer accepted
        /// (§3.10.4 suffix semantics: declared content first, then wildcard).
        suffix_locked: bool,
    },
    /// All-group base + NFA extension (XSD 1.1 complex type extension).
    #[cfg(feature = "xsd11")]
    AllGroupExtension {
        model: AllGroupModel,
        state: AllGroupState,
        extension_nfa: NfaTable,
        phase: AllGroupExtPhase,
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
            ContentModelMatcher::WithOpenContent { nfa, mode, wildcard } => {
                let oc = wildcard.map(|w| OpenContentInfo {
                    mode,
                    namespace_constraint: w.namespace_constraint,
                    process_contents: w.process_contents,
                    not_qnames: w.not_qnames,
                });
                let initial = ActiveStates::from_nfa(&nfa);
                Self::Nfa { nfa, active_states: initial, open_content: oc }
            }
            #[cfg(feature = "xsd11")]
            ContentModelMatcher::AllGroupExtension { base_model, extension_nfa } => {
                let state = base_model.create_state();
                Self::AllGroupExtension {
                    model: base_model,
                    state,
                    extension_nfa,
                    phase: AllGroupExtPhase::AllGroup,
                }
            }
        }
    }

    /// Create a content validator state from an NFA table
    ///
    /// Computes the initial epsilon closure from the start state.
    pub fn from_nfa(nfa: NfaTable) -> Self {
        let initial = ActiveStates::from_nfa(&nfa);
        ContentValidatorState::Nfa {
            nfa,
            active_states: initial,
            open_content: None,
        }
    }

    /// Create a content validator state from an all-group model
    pub fn from_all_group(model: AllGroupModel) -> Self {
        let state = model.create_state();
        ContentValidatorState::AllGroup { model, state, suffix_locked: false }
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
            ContentValidatorState::Nfa { nfa, active_states, open_content } => {
                // First, find the matching element info before advancing
                let mi = active_states.find_match_info(
                    nfa, name, namespace, target_ns, subst_groups, xsd_version,
                );
                let match_info = ElementMatchInfo {
                    element_key: mi.element_key,
                    resolved_type: mi.resolved_type,
                    process_contents: mi.process_contents,
                };

                let next = match xsd_version {
                    XsdVersion::V1_0 => active_states.clone().advance(
                        nfa, name, namespace, target_ns, subst_groups, xsd_version,
                    ),
                    XsdVersion::V1_1 => active_states.clone().advance_with_priority(
                        nfa, name, namespace, target_ns, subst_groups, xsd_version,
                    ),
                };
                if next.is_empty() {
                    // No NFA transition matched — try open content wildcard
                    if let Some(oc) = open_content {
                        let allow = match oc.mode {
                            TypesOpenContentMode::Interleave => true,
                            TypesOpenContentMode::Suffix => {
                                active_states.contains_accept(nfa)
                            }
                            TypesOpenContentMode::None => false,
                        };
                        if allow && open_content_allows(&oc.namespace_constraint, &oc.not_qnames, name, namespace, target_ns)
                        {
                            // Suffix mode: lock NFA to accept-only so no declared elements
                            // are accepted after the first open-content element (§3.10.4 suffix semantics).
                            if matches!(oc.mode, TypesOpenContentMode::Suffix) {
                                *active_states = ActiveStates::Simple([nfa.accept_state].into());
                            }
                            return Some(ElementMatchInfo {
                                element_key: None,
                                resolved_type: None,
                                process_contents: Some(oc.process_contents),
                            });
                        }
                    }
                    return None;
                }
                *active_states = next;
                Some(match_info)
            }
            ContentValidatorState::AllGroup { model, state, suffix_locked } => {
                // Once the suffix open-content section has begun, declared
                // all-group particles are no longer eligible to match.
                if !*suffix_locked {
                    for (i, particle) in model.particles.iter().enumerate() {
                        if !state.can_accept(model, i) {
                            continue;
                        }
                        let result = term_matches_with_substitution(
                            &particle.term,
                            name,
                            namespace,
                            target_ns,
                            subst_groups,
                            xsd_version,
                        );
                        if result == TermMatchResult::Match {
                            if state.accept(model, i) {
                                let info = match &particle.term {
                                    NfaTerm::Element {
                                        name: term_name,
                                        namespace: term_ns,
                                        element_key,
                                        resolved_type,
                                    } => {
                                        if *term_name == name && *term_ns == namespace {
                                            // Direct match
                                            ElementMatchInfo {
                                                element_key: *element_key,
                                                resolved_type: *resolved_type,
                                                process_contents: None,
                                            }
                                        } else {
                                            // Substitution match — let runtime resolve
                                            ElementMatchInfo {
                                                element_key: None,
                                                resolved_type: None,
                                                process_contents: None,
                                            }
                                        }
                                    }
                                    NfaTerm::Wildcard { process_contents, .. } => {
                                        ElementMatchInfo {
                                            element_key: None,
                                            resolved_type: None,
                                            process_contents: Some(*process_contents),
                                        }
                                    }
                                };
                                return Some(info);
                            }
                            return None;
                        }
                    }
                }
                // No declared particle matched (or suffix lock engaged) —
                // try open content wildcard.
                if let Some(oc) = &model.open_content {
                    let allow = match oc.mode {
                        AllGroupOpenContentMode::Interleave => true,
                        AllGroupOpenContentMode::Suffix => state.is_satisfied(model),
                        AllGroupOpenContentMode::None => false,
                    };
                    if allow
                        && open_content_allows(&oc.namespace_constraint, &oc.not_qnames, name, namespace, target_ns)
                    {
                        // §3.10.4 suffix semantics: once a suffix wildcard
                        // element matches, no declared all-group particle may
                        // match again.
                        if matches!(oc.mode, AllGroupOpenContentMode::Suffix) {
                            *suffix_locked = true;
                        }
                        return Some(ElementMatchInfo {
                            element_key: None,
                            resolved_type: None,
                            process_contents: Some(oc.process_contents),
                        });
                    }
                }
                None
            }
            #[cfg(feature = "xsd11")]
            ContentValidatorState::AllGroupExtension { model, state, extension_nfa, phase } => {
                match phase {
                    AllGroupExtPhase::AllGroup => {
                        // Try to match against all-group particles first
                        for (i, particle) in model.particles.iter().enumerate() {
                            if !state.can_accept(model, i) {
                                continue;
                            }
                            let result = term_matches_with_substitution(
                                &particle.term,
                                name,
                                namespace,
                                target_ns,
                                subst_groups,
                                xsd_version,
                            );
                            if result == TermMatchResult::Match {
                                if state.accept(model, i) {
                                    let info = match &particle.term {
                                        NfaTerm::Element {
                                            name: term_name,
                                            namespace: term_ns,
                                            element_key,
                                            resolved_type,
                                        } => {
                                            if *term_name == name && *term_ns == namespace {
                                                ElementMatchInfo {
                                                    element_key: *element_key,
                                                    resolved_type: *resolved_type,
                                                    process_contents: None,
                                                }
                                            } else {
                                                // Substitution match — let runtime resolve
                                                ElementMatchInfo {
                                                    element_key: None,
                                                    resolved_type: None,
                                                    process_contents: None,
                                                }
                                            }
                                        }
                                        NfaTerm::Wildcard { process_contents, .. } => {
                                            ElementMatchInfo {
                                                element_key: None,
                                                resolved_type: None,
                                                process_contents: Some(*process_contents),
                                            }
                                        }
                                    };
                                    return Some(info);
                                }
                                return None;
                            }
                        }

                        // No all-group particle matched — if all-group is satisfied,
                        // try transitioning to the extension NFA
                        if state.is_satisfied(model) {
                            let initial = ActiveStates::from_nfa(extension_nfa);
                            let mi = initial.find_match_info(
                                extension_nfa, name, namespace, target_ns, subst_groups, xsd_version,
                            );
                            let match_info = ElementMatchInfo {
                                element_key: mi.element_key,
                                resolved_type: mi.resolved_type,
                                process_contents: mi.process_contents,
                            };
                            let next = initial.advance_with_priority(
                                extension_nfa, name, namespace, target_ns, subst_groups, xsd_version,
                            );
                            if !next.is_empty() {
                                *phase = AllGroupExtPhase::Nfa(next);
                                return Some(match_info);
                            }
                        }

                        // Try open content wildcard as final fallback
                        if let Some(oc) = &model.open_content {
                            let allow = match oc.mode {
                                AllGroupOpenContentMode::Interleave => true,
                                AllGroupOpenContentMode::Suffix => state.is_satisfied(model),
                                AllGroupOpenContentMode::None => false,
                            };
                            if allow
                                && open_content_allows(
                                    &oc.namespace_constraint,
                                    &oc.not_qnames,
                                    name,
                                    namespace,
                                    target_ns,
                                )
                            {
                                return Some(ElementMatchInfo {
                                    element_key: None,
                                    resolved_type: None,
                                    process_contents: Some(oc.process_contents),
                                });
                            }
                        }
                        None
                    }
                    AllGroupExtPhase::Nfa(active_states) => {
                        // Standard NFA advancement in extension phase
                        let mi = active_states.find_match_info(
                            extension_nfa, name, namespace, target_ns, subst_groups, xsd_version,
                        );
                        let match_info = ElementMatchInfo {
                            element_key: mi.element_key,
                            resolved_type: mi.resolved_type,
                            process_contents: mi.process_contents,
                        };
                        let next = active_states.clone().advance_with_priority(
                            extension_nfa, name, namespace, target_ns, subst_groups, xsd_version,
                        );
                        if next.is_empty() {
                            // Try open content wildcard fallback
                            if let Some(oc) = &model.open_content {
                                let allow = match oc.mode {
                                    AllGroupOpenContentMode::Interleave => true,
                                    AllGroupOpenContentMode::Suffix => {
                                        active_states.contains_accept(extension_nfa)
                                    }
                                    AllGroupOpenContentMode::None => false,
                                };
                                if allow
                                    && open_content_allows(
                                        &oc.namespace_constraint,
                                        &oc.not_qnames,
                                        name,
                                        namespace,
                                        target_ns,
                                    )
                                {
                                    return Some(ElementMatchInfo {
                                        element_key: None,
                                        resolved_type: None,
                                        process_contents: Some(oc.process_contents),
                                    });
                                }
                            }
                            return None;
                        }
                        *active_states = next;
                        Some(match_info)
                    }
                }
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
            ContentValidatorState::Nfa { nfa, active_states, .. } => {
                active_states.contains_accept(nfa)
            }
            ContentValidatorState::AllGroup { model, state, .. } => {
                // If the outer particle is optional (minOccurs=0) and no children
                // have been consumed, the entire group was skipped — trivially satisfied.
                if model.outer_optional && !state.has_any_consumed() {
                    return true;
                }
                state.is_satisfied(model)
            }
            #[cfg(feature = "xsd11")]
            ContentValidatorState::AllGroupExtension { model, state, extension_nfa, phase } => {
                // All-group must be satisfied (or skipped if outer-optional)
                let all_satisfied = if model.outer_optional && !state.has_any_consumed() {
                    true
                } else {
                    state.is_satisfied(model)
                };
                if !all_satisfied {
                    return false;
                }
                match phase {
                    AllGroupExtPhase::AllGroup => {
                        // Still in all-group phase — extension NFA must accept empty
                        let initial = ActiveStates::from_nfa(extension_nfa);
                        initial.contains_accept(extension_nfa)
                    }
                    AllGroupExtPhase::Nfa(active_states) => {
                        active_states.contains_accept(extension_nfa)
                    }
                }
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
            ContentValidatorState::Nfa { nfa, active_states, open_content } => {
                let next = match xsd_version {
                    XsdVersion::V1_0 => active_states.clone().advance(
                        nfa, name, namespace, target_ns, subst_groups, xsd_version,
                    ),
                    XsdVersion::V1_1 => active_states.clone().advance_with_priority(
                        nfa, name, namespace, target_ns, subst_groups, xsd_version,
                    ),
                };
                if !next.is_empty() {
                    return true;
                }
                // Try open content wildcard fallback
                if let Some(oc) = open_content {
                    let allow = match oc.mode {
                        TypesOpenContentMode::Interleave => true,
                        TypesOpenContentMode::Suffix => {
                            active_states.contains_accept(nfa)
                        }
                        TypesOpenContentMode::None => false,
                    };
                    if allow && open_content_allows(&oc.namespace_constraint, &oc.not_qnames, name, namespace, target_ns) {
                        return true;
                    }
                }
                false
            }
            ContentValidatorState::AllGroup { model, state, suffix_locked } => {
                if !*suffix_locked {
                    for (i, particle) in model.particles.iter().enumerate() {
                        if !state.can_accept(model, i) {
                            continue;
                        }
                        let result = term_matches_with_substitution(
                            &particle.term,
                            name,
                            namespace,
                            target_ns,
                            subst_groups,
                            xsd_version,
                        );
                        if result == TermMatchResult::Match {
                            return true;
                        }
                    }
                }
                // Try open content wildcard fallback
                if let Some(oc) = &model.open_content {
                    let allow = match oc.mode {
                        AllGroupOpenContentMode::Interleave => true,
                        AllGroupOpenContentMode::Suffix => state.is_satisfied(model),
                        AllGroupOpenContentMode::None => false,
                    };
                    if allow && open_content_allows(&oc.namespace_constraint, &oc.not_qnames, name, namespace, target_ns) {
                        return true;
                    }
                }
                false
            }
            #[cfg(feature = "xsd11")]
            ContentValidatorState::AllGroupExtension { model, state, extension_nfa, phase } => {
                match phase {
                    AllGroupExtPhase::AllGroup => {
                        // Check all-group particles
                        for (i, particle) in model.particles.iter().enumerate() {
                            if !state.can_accept(model, i) {
                                continue;
                            }
                            let result = term_matches_with_substitution(
                                &particle.term,
                                name,
                                namespace,
                                target_ns,
                                subst_groups,
                                xsd_version,
                            );
                            if result == TermMatchResult::Match {
                                return true;
                            }
                        }
                        // If all-group is satisfied, check extension NFA start
                        if state.is_satisfied(model) {
                            let initial = ActiveStates::from_nfa(extension_nfa);
                            let next = initial.advance_with_priority(
                                extension_nfa, name, namespace, target_ns, subst_groups, xsd_version,
                            );
                            if !next.is_empty() {
                                return true;
                            }
                        }
                        // Try open content wildcard fallback
                        if let Some(oc) = &model.open_content {
                            let allow = match oc.mode {
                                AllGroupOpenContentMode::Interleave => true,
                                AllGroupOpenContentMode::Suffix => state.is_satisfied(model),
                                AllGroupOpenContentMode::None => false,
                            };
                            if allow
                                && open_content_allows(
                                    &oc.namespace_constraint,
                                    &oc.not_qnames,
                                    name,
                                    namespace,
                                    target_ns,
                                )
                            {
                                return true;
                            }
                        }
                        false
                    }
                    AllGroupExtPhase::Nfa(active_states) => {
                        // Standard NFA lookahead
                        let next = active_states.clone().advance_with_priority(
                            extension_nfa, name, namespace, target_ns, subst_groups, xsd_version,
                        );
                        if !next.is_empty() {
                            return true;
                        }
                        // Try open content wildcard fallback
                        if let Some(oc) = &model.open_content {
                            let allow = match oc.mode {
                                AllGroupOpenContentMode::Interleave => true,
                                AllGroupOpenContentMode::Suffix => {
                                    active_states.contains_accept(extension_nfa)
                                }
                                AllGroupOpenContentMode::None => false,
                            };
                            if allow
                                && open_content_allows(
                                    &oc.namespace_constraint,
                                    &oc.not_qnames,
                                    name,
                                    namespace,
                                    target_ns,
                                )
                            {
                                return true;
                            }
                        }
                        false
                    }
                }
            }
            ContentValidatorState::Simple | ContentValidatorState::Empty => false,
        }
    }
}

/// Check if an open content wildcard allows the given element.
/// Combines namespace matching with notQName exclusion checking.
/// Open content is an XSD 1.1 feature, so V1_1 semantics always apply.
fn open_content_allows(
    ns_constraint: &NamespaceConstraint,
    not_qnames: &[(Option<NameId>, NameId)],
    name: NameId,
    namespace: Option<NameId>,
    target_ns: Option<NameId>,
) -> bool {
    ns_constraint.matches(namespace, target_ns, XsdVersion::V1_1)
        && !not_qnames_exclude(not_qnames, namespace, name)
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

    // -- Open content tests --------------------------------------------------

    use crate::compiler::{AllParticle, MaxOccurs, OpenContentWildcard};
    use crate::compiler::OpenContentMode as AllGroupOCMode;

    fn all_group_with_open_content(
        mode: AllGroupOCMode,
        ns_constraint: NamespaceConstraint,
    ) -> AllGroupModel {
        let a = NameId(10);
        let mut model = AllGroupModel::new(vec![
            AllParticle::new(NfaTerm::element(a, None, None), 1, MaxOccurs::Bounded(1), None),
        ]);
        model.open_content = Some(OpenContentWildcard {
            namespace_constraint: ns_constraint,
            process_contents: ProcessContents::Lax,
            mode,
            not_qnames: Vec::new(),
        });
        model
    }

    #[test]
    fn test_all_group_open_content_interleave() {
        let model = all_group_with_open_content(
            AllGroupOCMode::Interleave,
            NamespaceConstraint::Any,
        );
        let mut state = ContentValidatorState::from_all_group(model);

        let extra = NameId(99);
        let a = NameId(10);

        // Extra element accepted via open content before the declared particle
        let info = state.advance_element(extra, None, None, XsdVersion::V1_1, None);
        assert!(info.is_some(), "interleave should accept extra element");
        assert!(info.unwrap().process_contents.is_some());

        // Declared particle still works
        assert!(state.advance_element(a, None, None, XsdVersion::V1_1, None).is_some());
        assert!(state.is_complete());

        // Extra element accepted after declared particle too
        let info2 = state.advance_element(extra, None, None, XsdVersion::V1_1, None);
        assert!(info2.is_some(), "interleave should accept extra element after satisfaction");
    }

    #[test]
    fn test_all_group_open_content_suffix() {
        let model = all_group_with_open_content(
            AllGroupOCMode::Suffix,
            NamespaceConstraint::Any,
        );
        let mut state = ContentValidatorState::from_all_group(model);

        let extra = NameId(99);
        let a = NameId(10);

        // Extra element rejected before the required particle is satisfied
        assert!(
            state.advance_element(extra, None, None, XsdVersion::V1_1, None).is_none(),
            "suffix should reject extra element before satisfaction"
        );

        // Satisfy the declared particle
        assert!(state.advance_element(a, None, None, XsdVersion::V1_1, None).is_some());
        assert!(state.is_complete());

        // Now extra element should be accepted
        assert!(
            state.advance_element(extra, None, None, XsdVersion::V1_1, None).is_some(),
            "suffix should accept extra element after satisfaction"
        );
    }

    #[test]
    fn test_nfa_open_content_interleave() {
        let a = NameId(10);
        let nfa = single_element_nfa(a, None);
        let oc = OpenContentInfo {
            mode: TypesOpenContentMode::Interleave,
            namespace_constraint: NamespaceConstraint::Any,
            process_contents: ProcessContents::Lax,
            not_qnames: Vec::new(),
        };
        let initial = ActiveStates::from_nfa(&nfa);
        let mut state = ContentValidatorState::Nfa {
            nfa,
            active_states: initial,
            open_content: Some(oc),
        };

        let extra = NameId(99);

        // Extra element accepted via open content before the declared element
        let info = state.advance_element(extra, None, None, XsdVersion::V1_1, None);
        assert!(info.is_some(), "interleave should accept extra element before NFA match");

        // Declared element still works
        assert!(state.advance_element(a, None, None, XsdVersion::V1_1, None).is_some());
        assert!(state.is_complete());
    }

    #[test]
    fn test_nfa_open_content_suffix() {
        let a = NameId(10);
        let nfa = single_element_nfa(a, None);
        let oc = OpenContentInfo {
            mode: TypesOpenContentMode::Suffix,
            namespace_constraint: NamespaceConstraint::Any,
            process_contents: ProcessContents::Lax,
            not_qnames: Vec::new(),
        };
        let initial = ActiveStates::from_nfa(&nfa);
        let mut state = ContentValidatorState::Nfa {
            nfa,
            active_states: initial,
            open_content: Some(oc),
        };

        let extra = NameId(99);
        let a = NameId(10);

        // Extra element rejected before accept state
        assert!(
            state.advance_element(extra, None, None, XsdVersion::V1_1, None).is_none(),
            "suffix should reject extra element before accept state"
        );

        // Match declared element to reach accept state
        assert!(state.advance_element(a, None, None, XsdVersion::V1_1, None).is_some());
        assert!(state.is_complete());

        // Now extra element accepted
        assert!(
            state.advance_element(extra, None, None, XsdVersion::V1_1, None).is_some(),
            "suffix should accept extra element after accept state"
        );
    }

    #[test]
    fn test_open_content_namespace_constraint() {
        let target_ns = Some(NameId(100));
        let other_ns = Some(NameId(200));

        let model = all_group_with_open_content(
            AllGroupOCMode::Interleave,
            NamespaceConstraint::Other, // Only accept elements from other namespaces
        );
        let mut state = ContentValidatorState::from_all_group(model);

        let extra = NameId(99);

        // Element from target namespace should be rejected by open content
        assert!(
            state.advance_element(extra, target_ns, target_ns, XsdVersion::V1_1, None).is_none(),
            "open content with ##other should reject target namespace"
        );

        // Element from other namespace should be accepted
        assert!(
            state.advance_element(extra, other_ns, target_ns, XsdVersion::V1_1, None).is_some(),
            "open content with ##other should accept other namespace"
        );
    }

    #[test]
    fn test_would_accept_with_open_content() {
        let model = all_group_with_open_content(
            AllGroupOCMode::Interleave,
            NamespaceConstraint::Any,
        );
        let state = ContentValidatorState::from_all_group(model);

        let extra = NameId(99);
        let a = NameId(10);

        // Both declared and extra elements should be accepted in lookahead
        assert!(state.would_accept(a, None, None, XsdVersion::V1_1, None));
        assert!(state.would_accept(extra, None, None, XsdVersion::V1_1, None));

        // NFA version
        let nfa = single_element_nfa(a, None);
        let oc = OpenContentInfo {
            mode: TypesOpenContentMode::Interleave,
            namespace_constraint: NamespaceConstraint::Any,
            process_contents: ProcessContents::Lax,
            not_qnames: Vec::new(),
        };
        let initial = ActiveStates::from_nfa(&nfa);
        let state = ContentValidatorState::Nfa {
            nfa,
            active_states: initial,
            open_content: Some(oc),
        };
        assert!(state.would_accept(a, None, None, XsdVersion::V1_1, None));
        assert!(state.would_accept(extra, None, None, XsdVersion::V1_1, None));
    }

    // -- AllGroupExtension tests (XSD 1.1) ------------------------------------

    #[cfg(feature = "xsd11")]
    fn make_all_group_extension_state(
        all_particles: Vec<AllParticle>,
        ext_nfa: NfaTable,
    ) -> ContentValidatorState {
        let model = AllGroupModel::new(all_particles);
        let matcher = ContentModelMatcher::AllGroupExtension {
            base_model: model,
            extension_nfa: ext_nfa,
        };
        ContentValidatorState::from_matcher(matcher)
    }

    /// all(A, B) + seq(C): accepts A,B,C and B,A,C; rejects C,A,B and A,C,B
    #[cfg(feature = "xsd11")]
    #[test]
    fn test_all_group_extension_basic_composite() {
        let a = NameId(10);
        let b = NameId(20);
        let c = NameId(30);

        let particles = vec![
            AllParticle::new(NfaTerm::element(a, None, None), 1, MaxOccurs::Bounded(1), None),
            AllParticle::new(NfaTerm::element(b, None, None), 1, MaxOccurs::Bounded(1), None),
        ];
        let ext_nfa = single_element_nfa(c, None);

        // A, B, C → accepted
        let mut state = make_all_group_extension_state(particles.clone(), ext_nfa.clone());
        assert!(state.advance_element(a, None, None, XsdVersion::V1_1, None).is_some());
        assert!(state.advance_element(b, None, None, XsdVersion::V1_1, None).is_some());
        assert!(state.advance_element(c, None, None, XsdVersion::V1_1, None).is_some());
        assert!(state.is_complete());

        // B, A, C → accepted (reversed all-group order)
        let mut state = make_all_group_extension_state(particles.clone(), ext_nfa.clone());
        assert!(state.advance_element(b, None, None, XsdVersion::V1_1, None).is_some());
        assert!(state.advance_element(a, None, None, XsdVersion::V1_1, None).is_some());
        assert!(state.advance_element(c, None, None, XsdVersion::V1_1, None).is_some());
        assert!(state.is_complete());

        // C, A, B → rejected (C before all-group is satisfied)
        let mut state = make_all_group_extension_state(particles.clone(), ext_nfa.clone());
        assert!(state.advance_element(c, None, None, XsdVersion::V1_1, None).is_none());

        // A, C, B → rejected (C before B satisfies all-group)
        let mut state = make_all_group_extension_state(particles, ext_nfa);
        assert!(state.advance_element(a, None, None, XsdVersion::V1_1, None).is_some());
        assert!(state.advance_element(c, None, None, XsdVersion::V1_1, None).is_none());
    }

    /// all(A?, B?) + seq(C): accepts C alone (all-group satisfied empty)
    #[cfg(feature = "xsd11")]
    #[test]
    fn test_all_group_extension_optional_all_group() {
        let a = NameId(10);
        let b = NameId(20);
        let c = NameId(30);

        let particles = vec![
            AllParticle::new(NfaTerm::element(a, None, None), 0, MaxOccurs::Bounded(1), None),
            AllParticle::new(NfaTerm::element(b, None, None), 0, MaxOccurs::Bounded(1), None),
        ];
        let ext_nfa = single_element_nfa(c, None);

        // C alone → accepted (all-group is satisfied with zero occurrences)
        let mut state = make_all_group_extension_state(particles, ext_nfa);
        assert!(state.advance_element(c, None, None, XsdVersion::V1_1, None).is_some());
        assert!(state.is_complete());
    }

    /// is_complete checks: after A,B,C → complete; after A,B → not complete
    #[cfg(feature = "xsd11")]
    #[test]
    fn test_all_group_extension_is_complete() {
        let a = NameId(10);
        let b = NameId(20);
        let c = NameId(30);

        let particles = vec![
            AllParticle::new(NfaTerm::element(a, None, None), 1, MaxOccurs::Bounded(1), None),
            AllParticle::new(NfaTerm::element(b, None, None), 1, MaxOccurs::Bounded(1), None),
        ];
        let ext_nfa = single_element_nfa(c, None);

        let mut state = make_all_group_extension_state(particles, ext_nfa);
        assert!(!state.is_complete(), "not complete initially");

        assert!(state.advance_element(a, None, None, XsdVersion::V1_1, None).is_some());
        assert!(!state.is_complete(), "not complete after A only");

        assert!(state.advance_element(b, None, None, XsdVersion::V1_1, None).is_some());
        assert!(!state.is_complete(), "not complete after A,B — extension C still required");

        assert!(state.advance_element(c, None, None, XsdVersion::V1_1, None).is_some());
        assert!(state.is_complete(), "complete after A,B,C");
    }

    /// would_accept lookahead: initially A/B accepted, C not; after A,B only C accepted
    #[cfg(feature = "xsd11")]
    #[test]
    fn test_all_group_extension_would_accept() {
        let a = NameId(10);
        let b = NameId(20);
        let c = NameId(30);

        let particles = vec![
            AllParticle::new(NfaTerm::element(a, None, None), 1, MaxOccurs::Bounded(1), None),
            AllParticle::new(NfaTerm::element(b, None, None), 1, MaxOccurs::Bounded(1), None),
        ];
        let ext_nfa = single_element_nfa(c, None);

        let mut state = make_all_group_extension_state(particles, ext_nfa);

        // Initially: A and B accepted, C not (all-group not yet satisfied)
        assert!(state.would_accept(a, None, None, XsdVersion::V1_1, None));
        assert!(state.would_accept(b, None, None, XsdVersion::V1_1, None));
        assert!(!state.would_accept(c, None, None, XsdVersion::V1_1, None));

        // After A,B: only C is accepted
        state.advance_element(a, None, None, XsdVersion::V1_1, None);
        state.advance_element(b, None, None, XsdVersion::V1_1, None);
        assert!(!state.would_accept(a, None, None, XsdVersion::V1_1, None));
        assert!(!state.would_accept(b, None, None, XsdVersion::V1_1, None));
        assert!(state.would_accept(c, None, None, XsdVersion::V1_1, None));
    }

    // -- Not constraint and notQName tests -----------------------------------

    #[test]
    fn test_open_content_not_namespace_constraint() {
        // Open content with Not([ns1]) should reject ns1 but accept others
        let ns1 = Some(NameId(100));
        let ns2 = Some(NameId(200));
        let a = NameId(10);
        let extra = NameId(99);

        let mut model = AllGroupModel::new(vec![
            AllParticle::new(NfaTerm::element(a, None, None), 1, MaxOccurs::Bounded(1), None),
        ]);
        model.open_content = Some(OpenContentWildcard {
            namespace_constraint: NamespaceConstraint::Not(vec![ns1]),
            process_contents: ProcessContents::Lax,
            mode: AllGroupOCMode::Interleave,
            not_qnames: Vec::new(),
        });
        let mut state = ContentValidatorState::from_all_group(model);

        // Element from excluded namespace rejected
        assert!(
            state.advance_element(extra, ns1, None, XsdVersion::V1_1, None).is_none(),
            "Not([ns1]) should reject elements from ns1"
        );

        // Element from other namespace accepted
        assert!(
            state.advance_element(extra, ns2, None, XsdVersion::V1_1, None).is_some(),
            "Not([ns1]) should accept elements from ns2"
        );
    }

    #[test]
    fn test_open_content_not_qnames_exclusion() {
        // Open content with notQName excluding specific element
        let a = NameId(10);
        let excluded = NameId(50);
        let allowed = NameId(60);

        let mut model = AllGroupModel::new(vec![
            AllParticle::new(NfaTerm::element(a, None, None), 1, MaxOccurs::Bounded(1), None),
        ]);
        model.open_content = Some(OpenContentWildcard {
            namespace_constraint: NamespaceConstraint::Any,
            process_contents: ProcessContents::Lax,
            mode: AllGroupOCMode::Interleave,
            not_qnames: vec![(None, excluded)],  // exclude (absent ns, excluded)
        });
        let mut state = ContentValidatorState::from_all_group(model);

        // Excluded element rejected even though namespace matches
        assert!(
            state.advance_element(excluded, None, None, XsdVersion::V1_1, None).is_none(),
            "notQName should reject excluded element"
        );

        // Non-excluded element accepted
        assert!(
            state.advance_element(allowed, None, None, XsdVersion::V1_1, None).is_some(),
            "notQName should accept non-excluded element"
        );
    }

    #[test]
    fn test_nfa_open_content_not_qnames_exclusion() {
        // Same test but for NFA path
        let a = NameId(10);
        let excluded = NameId(50);
        let allowed = NameId(60);
        let nfa = single_element_nfa(a, None);
        let oc = OpenContentInfo {
            mode: TypesOpenContentMode::Interleave,
            namespace_constraint: NamespaceConstraint::Any,
            process_contents: ProcessContents::Lax,
            not_qnames: vec![(None, excluded)],
        };
        let initial = ActiveStates::from_nfa(&nfa);
        let mut state = ContentValidatorState::Nfa {
            nfa,
            active_states: initial,
            open_content: Some(oc),
        };

        // Excluded element rejected
        assert!(
            state.advance_element(excluded, None, None, XsdVersion::V1_1, None).is_none(),
            "NFA open content notQName should reject excluded element"
        );

        // Non-excluded element accepted
        assert!(
            state.advance_element(allowed, None, None, XsdVersion::V1_1, None).is_some(),
            "NFA open content notQName should accept non-excluded element"
        );
    }

    #[test]
    fn test_would_accept_respects_not_qnames() {
        let a = NameId(10);
        let excluded = NameId(50);
        let allowed = NameId(60);

        let mut model = AllGroupModel::new(vec![
            AllParticle::new(NfaTerm::element(a, None, None), 1, MaxOccurs::Bounded(1), None),
        ]);
        model.open_content = Some(OpenContentWildcard {
            namespace_constraint: NamespaceConstraint::Any,
            process_contents: ProcessContents::Lax,
            mode: AllGroupOCMode::Interleave,
            not_qnames: vec![(None, excluded)],
        });
        let state = ContentValidatorState::from_all_group(model);

        assert!(!state.would_accept(excluded, None, None, XsdVersion::V1_1, None));
        assert!(state.would_accept(allowed, None, None, XsdVersion::V1_1, None));
    }
}
