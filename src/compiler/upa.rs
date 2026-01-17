//! Unique Particle Attribution (UPA) constraint checking
//!
//! UPA ensures that during validation, each element can be unambiguously attributed
//! to exactly one particle in the content model. This module implements UPA checking
//! for compiled NFA content models.
//!
//! # XSD Version Behavior
//!
//! | Conflict Type        | XSD 1.0 | XSD 1.1            |
//! |---------------------|---------|---------------------|
//! | Element-Element     | Error   | Error               |
//! | Element-Wildcard    | Error   | Allowed (elem wins) |
//! | Wildcard-Wildcard   | Error   | Error               |

use std::collections::HashSet;
use thiserror::Error;

use crate::error::{SchemaError, SchemaResult};
use crate::ids::NameId;
use crate::parser::location::SourceRef;
use crate::schema::model::{SchemaSet, XsdVersion};
use crate::types::complex::NamespaceConstraint;

use super::nfa::{NfaTable, NfaTerm, StateId, TransitionKind};
use super::substitution::{build_substitution_group_map, SubstitutionGroupMap};

/// Result type for internal UPA checking operations
type UpaResult<T> = Result<T, UpaError>;

/// Errors that can occur during UPA checking
#[derive(Error, Debug, Clone)]
enum UpaError {
    /// Two element particles can match the same element
    #[error("UPA violation: elements '{first_name}' and '{second_name}' conflict at state {state_id}")]
    ElementElementConflict {
        first_name: String,
        second_name: String,
        first_name_id: NameId,
        first_namespace: Option<NameId>,
        second_name_id: NameId,
        second_namespace: Option<NameId>,
        state_id: StateId,
        first_location: Option<SourceRef>,
        second_location: Option<SourceRef>,
    },

    /// An element particle and wildcard can match the same element
    #[error("UPA violation: element '{element_name}' conflicts with wildcard at state {state_id}")]
    ElementWildcardConflict {
        element_name: String,
        element_name_id: NameId,
        element_namespace: Option<NameId>,
        wildcard_constraint: NamespaceConstraint,
        state_id: StateId,
        element_location: Option<SourceRef>,
        wildcard_location: Option<SourceRef>,
    },

    /// Two wildcards can match overlapping namespaces
    #[error("UPA violation: wildcards conflict at state {state_id}")]
    WildcardWildcardConflict {
        first_constraint: NamespaceConstraint,
        second_constraint: NamespaceConstraint,
        state_id: StateId,
        first_location: Option<SourceRef>,
        second_location: Option<SourceRef>,
    },
}

impl UpaError {
    /// Get the first source location if available
    pub fn first_location(&self) -> Option<&SourceRef> {
        match self {
            UpaError::ElementElementConflict { first_location, .. } => first_location.as_ref(),
            UpaError::ElementWildcardConflict { element_location, .. } => element_location.as_ref(),
            UpaError::WildcardWildcardConflict { first_location, .. } => first_location.as_ref(),
        }
    }

    /// Get the second source location if available
    pub fn second_location(&self) -> Option<&SourceRef> {
        match self {
            UpaError::ElementElementConflict { second_location, .. } => second_location.as_ref(),
            UpaError::ElementWildcardConflict { wildcard_location, .. } => wildcard_location.as_ref(),
            UpaError::WildcardWildcardConflict { second_location, .. } => second_location.as_ref(),
        }
    }

}

fn upa_error_to_schema_error(schema_set: &SchemaSet, error: UpaError) -> SchemaError {
    let primary = error
        .first_location()
        .and_then(|source| schema_set.source_maps.locate(source));
    let secondary = error
        .second_location()
        .and_then(|source| schema_set.source_maps.locate(source));
    let mut message = error.to_string();
    if let Some(location) = secondary {
        message.push_str(&format!(" (see also {})", location));
    }
    SchemaError::structural("cos-nonambig", message, primary)
}

/// A term reachable from a state via epsilon transitions
#[derive(Debug, Clone)]
struct ReachableTerm {
    term: NfaTerm,
    origin: Option<SourceRef>,
}

/// Collection of reachable terms categorized by type
#[derive(Debug, Default)]
struct ReachableTerms {
    elements: Vec<ReachableTerm>,
    wildcards: Vec<ReachableTerm>,
}

/// Compute epsilon closure and collect all reachable terms from a start state
///
/// Uses DFS traversal following epsilon transitions. Terms are collected from
/// states in the closure that have a term (these are the states where input
/// can be consumed to make progress).
fn epsilon_closure_with_terms(nfa: &NfaTable, start_state: StateId) -> ReachableTerms {
    let mut result = ReachableTerms::default();
    let mut closure = HashSet::new();
    let mut stack = vec![start_state];

    // Compute epsilon closure
    while let Some(state_id) = stack.pop() {
        if !closure.insert(state_id) {
            continue;
        }

        if let Some(state) = nfa.get_state(state_id) {
            for transition in &state.transitions {
                if transition.kind == TransitionKind::Epsilon {
                    stack.push(transition.target);
                }
            }
        }
    }

    // Collect terms from states in the closure that have terms
    // These are the states where we can consume input
    for &state_id in &closure {
        if let Some(state) = nfa.get_state(state_id) {
            if let Some(term) = &state.term {
                let reachable = ReachableTerm {
                    term: term.clone(),
                    origin: state.origin.clone(),
                };

                match term {
                    NfaTerm::Element { .. } => result.elements.push(reachable),
                    NfaTerm::Wildcard { .. } => result.wildcards.push(reachable),
                }
            }
        }
    }

    result
}

/// Check if two elements have the same qualified name (name + namespace)
fn elements_overlap(
    name1: NameId,
    ns1: Option<NameId>,
    name2: NameId,
    ns2: Option<NameId>,
) -> bool {
    name1 == name2 && ns1 == ns2
}

fn element_substitutable_names(
    term: &NfaTerm,
    substitution_sets: &SubstitutionGroupMap,
) -> HashSet<(NameId, Option<NameId>)> {
    match term {
        NfaTerm::Element {
            name,
            namespace,
            element_key,
        } => {
            if let Some(key) = element_key {
                if let Some(names) = substitution_sets.get(key) {
                    return names.clone();
                }
            }

            let mut names = HashSet::new();
            names.insert((*name, *namespace));
            names
        }
        NfaTerm::Wildcard { .. } => HashSet::new(),
    }
}

fn name_sets_overlap(
    names1: &HashSet<(NameId, Option<NameId>)>,
    names2: &HashSet<(NameId, Option<NameId>)>,
) -> bool {
    if names1.len() > names2.len() {
        return names2.iter().any(|name| names1.contains(name));
    }
    names1.iter().any(|name| names2.contains(name))
}

/// Check if an element namespace matches a wildcard constraint
fn element_wildcard_overlap(
    element_namespace: Option<NameId>,
    wildcard: &NamespaceConstraint,
    target_namespace: Option<NameId>,
    xsd_version: XsdVersion,
) -> bool {
    match wildcard {
        NamespaceConstraint::Any => true,

        NamespaceConstraint::Other => {
            // ##other matches any namespace except the target namespace
            // (and typically excludes absent namespace too in XSD 1.0)
            other_matches_namespace(element_namespace, target_namespace, xsd_version)
        }

        NamespaceConstraint::TargetNamespace => {
            // ##targetNamespace matches only the target namespace
            element_namespace == target_namespace
        }

        NamespaceConstraint::Local => {
            // ##local matches only elements with no namespace
            element_namespace.is_none()
        }

        NamespaceConstraint::List(namespaces) => {
            // List matches if element namespace is in the list
            // None in the list represents absent namespace
            namespaces.contains(&element_namespace)
        }
    }
}

fn other_matches_namespace(
    element_namespace: Option<NameId>,
    target_namespace: Option<NameId>,
    xsd_version: XsdVersion,
) -> bool {
    if element_namespace == target_namespace {
        return false;
    }

    if element_namespace.is_none() && xsd_version == XsdVersion::V1_0 {
        return false;
    }

    true
}

/// Check if two wildcard constraints can match overlapping namespaces
fn wildcards_overlap(
    wc1: &NamespaceConstraint,
    wc2: &NamespaceConstraint,
    target_namespace: Option<NameId>,
    xsd_version: XsdVersion,
) -> bool {
    use NamespaceConstraint::*;

    match (wc1, wc2) {
        // Any matches everything, so overlaps with anything
        (Any, _) | (_, Any) => true,

        // Two ##other constraints overlap (they both match "not target namespace")
        (Other, Other) => true,

        // ##other vs ##targetNamespace: no overlap (mutually exclusive)
        (Other, TargetNamespace) | (TargetNamespace, Other) => false,

        // ##other vs ##local: overlap if target namespace is not None
        // (##other excludes target ns, ##local is None, so overlap if target ns != None)
        (Other, Local) | (Local, Other) => {
            other_matches_namespace(None, target_namespace, xsd_version)
        }

        // ##other vs list: overlap if list has any ns other than target
        (Other, List(ns_list)) | (List(ns_list), Other) => {
            ns_list
                .iter()
                .any(|ns| other_matches_namespace(*ns, target_namespace, xsd_version))
        }

        // ##targetNamespace vs ##targetNamespace: overlap (both match target ns)
        (TargetNamespace, TargetNamespace) => true,

        // ##targetNamespace vs ##local: overlap only if target namespace is None
        (TargetNamespace, Local) | (Local, TargetNamespace) => target_namespace.is_none(),

        // ##targetNamespace vs list: overlap if list contains target namespace
        (TargetNamespace, List(ns_list)) | (List(ns_list), TargetNamespace) => {
            ns_list.contains(&target_namespace)
        }

        // ##local vs ##local: overlap (both match None)
        (Local, Local) => true,

        // ##local vs list: overlap if list contains None
        (Local, List(ns_list)) | (List(ns_list), Local) => ns_list.contains(&None),

        // List vs list: overlap if any namespace is in both lists
        (List(ns_list1), List(ns_list2)) => {
            ns_list1.iter().any(|ns| ns_list2.contains(ns))
        }
    }
}

/// Check element-element conflicts in reachable terms
fn check_element_element_conflicts(
    elements: &[ReachableTerm],
    from_state_id: StateId,
    schema_set: &SchemaSet,
    substitution_sets: &SubstitutionGroupMap,
) -> UpaResult<()> {
    let element_sets: Vec<HashSet<(NameId, Option<NameId>)>> = elements
        .iter()
        .map(|elem| element_substitutable_names(&elem.term, substitution_sets))
        .collect();

    for i in 0..elements.len() {
        for j in (i + 1)..elements.len() {
            let elem1 = &elements[i];
            let elem2 = &elements[j];

            if let (
                NfaTerm::Element {
                    name: name1,
                    namespace: ns1,
                    ..
                },
                NfaTerm::Element {
                    name: name2,
                    namespace: ns2,
                    ..
                },
            ) = (&elem1.term, &elem2.term)
            {
                if elements_overlap(*name1, *ns1, *name2, *ns2)
                    || name_sets_overlap(&element_sets[i], &element_sets[j])
                {
                    let first_name = schema_set
                        .name_table
                        .try_resolve(*name1)
                        .unwrap_or("<unknown>")
                        .to_string();
                    let second_name = schema_set
                        .name_table
                        .try_resolve(*name2)
                        .unwrap_or("<unknown>")
                        .to_string();

                    return Err(UpaError::ElementElementConflict {
                        first_name,
                        second_name,
                        first_name_id: *name1,
                        first_namespace: *ns1,
                        second_name_id: *name2,
                        second_namespace: *ns2,
                        state_id: from_state_id,
                        first_location: elem1.origin.clone(),
                        second_location: elem2.origin.clone(),
                    });
                }
            }
        }
    }
    Ok(())
}

/// Check element-wildcard conflicts in reachable terms (XSD 1.0 only)
fn check_element_wildcard_conflicts(
    elements: &[ReachableTerm],
    wildcards: &[ReachableTerm],
    from_state_id: StateId,
    target_namespace: Option<NameId>,
    xsd_version: XsdVersion,
    schema_set: &SchemaSet,
) -> UpaResult<()> {
    for elem in elements {
        if let NfaTerm::Element {
            name,
            namespace: elem_ns,
            ..
        } = &elem.term
        {
            for wc in wildcards {
                if let NfaTerm::Wildcard {
                    namespace_constraint,
                    ..
                } = &wc.term
                {
                    if element_wildcard_overlap(
                        *elem_ns,
                        namespace_constraint,
                        target_namespace,
                        xsd_version,
                    ) {
                        let element_name = schema_set
                            .name_table
                            .try_resolve(*name)
                            .unwrap_or("<unknown>")
                            .to_string();

                        return Err(UpaError::ElementWildcardConflict {
                            element_name,
                            element_name_id: *name,
                            element_namespace: *elem_ns,
                            wildcard_constraint: namespace_constraint.clone(),
                            state_id: from_state_id,
                            element_location: elem.origin.clone(),
                            wildcard_location: wc.origin.clone(),
                        });
                    }
                }
            }
        }
    }
    Ok(())
}

/// Check wildcard-wildcard conflicts in reachable terms
fn check_wildcard_wildcard_conflicts(
    wildcards: &[ReachableTerm],
    from_state_id: StateId,
    target_namespace: Option<NameId>,
    xsd_version: XsdVersion,
) -> UpaResult<()> {
    for i in 0..wildcards.len() {
        for j in (i + 1)..wildcards.len() {
            let wc1 = &wildcards[i];
            let wc2 = &wildcards[j];

            if let (
                NfaTerm::Wildcard {
                    namespace_constraint: constraint1,
                    ..
                },
                NfaTerm::Wildcard {
                    namespace_constraint: constraint2,
                    ..
                },
            ) = (&wc1.term, &wc2.term)
            {
                if wildcards_overlap(constraint1, constraint2, target_namespace, xsd_version) {
                    return Err(UpaError::WildcardWildcardConflict {
                        first_constraint: constraint1.clone(),
                        second_constraint: constraint2.clone(),
                        state_id: from_state_id,
                        first_location: wc1.origin.clone(),
                        second_location: wc2.origin.clone(),
                    });
                }
            }
        }
    }
    Ok(())
}

/// Check UPA constraints for an NFA content model
///
/// This function analyzes the NFA to detect ambiguities where the same input
/// element could match multiple particles in the content model.
///
/// # Arguments
///
/// * `nfa` - The compiled NFA content model to check
/// * `schema_set` - The schema set for name resolution and version info
/// * `target_namespace` - The target namespace of the schema containing this content model
///
/// # Returns
///
/// * `Ok(())` if the content model satisfies UPA
/// * `Err(SchemaError)` describing the first conflict found
///
/// # XSD Version Differences
///
/// - XSD 1.0: Element-element, element-wildcard, and wildcard-wildcard conflicts are all errors
/// - XSD 1.1: Element-wildcard conflicts are allowed (element declaration takes priority)
pub fn check_upa(
    nfa: &NfaTable,
    schema_set: &SchemaSet,
    target_namespace: Option<NameId>,
) -> SchemaResult<()> {
    let xsd_version = schema_set.xsd_version;
    let substitution_sets = build_substitution_group_map(schema_set);

    // Check from each state in the NFA
    for state in &nfa.states {
        let reachable = epsilon_closure_with_terms(nfa, state.id);

        // Check element-element conflicts (always an error)
        if let Err(error) = check_element_element_conflicts(
            &reachable.elements,
            state.id,
            schema_set,
            &substitution_sets,
        )
        {
            return Err(upa_error_to_schema_error(schema_set, error));
        }

        // Check element-wildcard conflicts (XSD 1.0 only)
        if xsd_version == XsdVersion::V1_0 {
            if let Err(error) = check_element_wildcard_conflicts(
                &reachable.elements,
                &reachable.wildcards,
                state.id,
                target_namespace,
                xsd_version,
                schema_set,
            ) {
                return Err(upa_error_to_schema_error(schema_set, error));
            }
        }

        // Check wildcard-wildcard conflicts (always an error)
        if let Err(error) =
            check_wildcard_wildcard_conflicts(
                &reachable.wildcards,
                state.id,
                target_namespace,
                xsd_version,
            )
        {
            return Err(upa_error_to_schema_error(schema_set, error));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::{FragmentBuilder, NfaTerm, fragment_to_table};
    use crate::types::complex::ProcessContents;
    use crate::error::SchemaError;
    use crate::schema::model::DerivationSet;

    fn assert_cos_nonambig(error: SchemaError) {
        match error {
            SchemaError::StructuralError { constraint, .. } => {
                assert_eq!(constraint, "cos-nonambig");
            }
            _ => panic!("Expected cos-nonambig structural error"),
        }
    }

    fn create_test_schema_set() -> SchemaSet {
        SchemaSet::new()
    }

    fn create_test_schema_set_v11() -> SchemaSet {
        SchemaSet::with_version(XsdVersion::V1_1)
    }

    fn element_data(name: NameId, target_namespace: Option<NameId>) -> crate::arenas::ElementDeclData {
        crate::arenas::ElementDeclData {
            name: Some(name),
            target_namespace,
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

    // ========================================================================
    // Element-Element conflict tests
    // ========================================================================

    #[test]
    fn test_element_element_same_name_conflict() {
        let mut schema_set = create_test_schema_set();
        let name_a = schema_set.name_table.add("a");

        // Build a choice between two elements with the same name
        let mut builder = FragmentBuilder::new();
        let term1 = NfaTerm::element(name_a, None, None);
        let term2 = NfaTerm::element(name_a, None, None);
        let frag1 = builder.single_term(term1, None);
        let frag2 = builder.single_term(term2, None);
        let choice = frag1.alternate(frag2);
        let nfa = fragment_to_table(choice);

        let result = check_upa(&nfa, &schema_set, None);
        assert!(result.is_err());
        assert_cos_nonambig(result.unwrap_err());
    }

    #[test]
    fn test_element_element_different_namespace_no_conflict() {
        let mut schema_set = create_test_schema_set();
        let name_a = schema_set.name_table.add("a");
        let ns1 = schema_set.name_table.add("http://ns1.example.com");
        let ns2 = schema_set.name_table.add("http://ns2.example.com");

        // Build a choice between two elements with same name but different namespaces
        let mut builder = FragmentBuilder::new();
        let term1 = NfaTerm::element(name_a, Some(ns1), None);
        let term2 = NfaTerm::element(name_a, Some(ns2), None);
        let frag1 = builder.single_term(term1, None);
        let frag2 = builder.single_term(term2, None);
        let choice = frag1.alternate(frag2);
        let nfa = fragment_to_table(choice);

        let result = check_upa(&nfa, &schema_set, None);
        assert!(result.is_ok());
    }

    #[test]
    fn test_element_element_sequence_no_conflict() {
        let mut schema_set = create_test_schema_set();
        let name_a = schema_set.name_table.add("a");

        // Build a sequence of two elements with the same name (no conflict in sequence)
        let mut builder = FragmentBuilder::new();
        let term1 = NfaTerm::element(name_a, None, None);
        let term2 = NfaTerm::element(name_a, None, None);
        let frag1 = builder.single_term(term1, None);
        let frag2 = builder.single_term(term2, None);
        let sequence = frag1.concat(frag2);
        let nfa = fragment_to_table(sequence);

        let result = check_upa(&nfa, &schema_set, None);
        assert!(result.is_ok());
    }

    #[test]
    fn test_element_element_different_names_no_conflict() {
        let mut schema_set = create_test_schema_set();
        let name_a = schema_set.name_table.add("a");
        let name_b = schema_set.name_table.add("b");

        // Build a choice between two elements with different names
        let mut builder = FragmentBuilder::new();
        let term1 = NfaTerm::element(name_a, None, None);
        let term2 = NfaTerm::element(name_b, None, None);
        let frag1 = builder.single_term(term1, None);
        let frag2 = builder.single_term(term2, None);
        let choice = frag1.alternate(frag2);
        let nfa = fragment_to_table(choice);

        let result = check_upa(&nfa, &schema_set, None);
        assert!(result.is_ok());
    }

    // ========================================================================
    // Element-Wildcard conflict tests
    // ========================================================================

    #[test]
    fn test_element_wildcard_any_conflict_xsd10() {
        let mut schema_set = create_test_schema_set();
        let name_a = schema_set.name_table.add("a");

        // Build a choice between an element and a ##any wildcard
        let mut builder = FragmentBuilder::new();
        let term1 = NfaTerm::element(name_a, None, None);
        let term2 = NfaTerm::wildcard(NamespaceConstraint::Any, ProcessContents::Lax);
        let frag1 = builder.single_term(term1, None);
        let frag2 = builder.single_term(term2, None);
        let choice = frag1.alternate(frag2);
        let nfa = fragment_to_table(choice);

        let result = check_upa(&nfa, &schema_set, None);
        assert!(result.is_err());
        assert_cos_nonambig(result.unwrap_err());
    }

    #[test]
    fn test_element_wildcard_any_allowed_xsd11() {
        let mut schema_set = create_test_schema_set_v11();
        let name_a = schema_set.name_table.add("a");

        // Build a choice between an element and a ##any wildcard
        let mut builder = FragmentBuilder::new();
        let term1 = NfaTerm::element(name_a, None, None);
        let term2 = NfaTerm::wildcard(NamespaceConstraint::Any, ProcessContents::Lax);
        let frag1 = builder.single_term(term1, None);
        let frag2 = builder.single_term(term2, None);
        let choice = frag1.alternate(frag2);
        let nfa = fragment_to_table(choice);

        // XSD 1.1 allows element-wildcard conflicts (element takes priority)
        let result = check_upa(&nfa, &schema_set, None);
        assert!(result.is_ok());
    }

    #[test]
    fn test_element_wildcard_other_with_target_no_overlap() {
        let mut schema_set = create_test_schema_set();
        let name_a = schema_set.name_table.add("a");
        let target_ns = schema_set.name_table.add("http://target.example.com");

        // Build a choice between an element in target ns and ##other wildcard
        let mut builder = FragmentBuilder::new();
        let term1 = NfaTerm::element(name_a, Some(target_ns), None);
        let term2 = NfaTerm::wildcard(NamespaceConstraint::Other, ProcessContents::Lax);
        let frag1 = builder.single_term(term1, None);
        let frag2 = builder.single_term(term2, None);
        let choice = frag1.alternate(frag2);
        let nfa = fragment_to_table(choice);

        // ##other excludes target namespace, so no overlap
        let result = check_upa(&nfa, &schema_set, Some(target_ns));
        assert!(result.is_ok());
    }

    // ========================================================================
    // Wildcard-Wildcard conflict tests
    // ========================================================================

    #[test]
    fn test_wildcard_wildcard_any_any_conflict() {
        let schema_set = create_test_schema_set();

        // Build a choice between two ##any wildcards
        let mut builder = FragmentBuilder::new();
        let term1 = NfaTerm::wildcard(NamespaceConstraint::Any, ProcessContents::Lax);
        let term2 = NfaTerm::wildcard(NamespaceConstraint::Any, ProcessContents::Strict);
        let frag1 = builder.single_term(term1, None);
        let frag2 = builder.single_term(term2, None);
        let choice = frag1.alternate(frag2);
        let nfa = fragment_to_table(choice);

        let result = check_upa(&nfa, &schema_set, None);
        assert!(result.is_err());
        assert_cos_nonambig(result.unwrap_err());
    }

    #[test]
    fn test_wildcard_wildcard_target_vs_other_no_conflict() {
        let mut schema_set = create_test_schema_set();
        let target_ns = schema_set.name_table.add("http://target.example.com");

        // Build a choice between ##targetNamespace and ##other wildcards
        let mut builder = FragmentBuilder::new();
        let term1 = NfaTerm::wildcard(NamespaceConstraint::TargetNamespace, ProcessContents::Lax);
        let term2 = NfaTerm::wildcard(NamespaceConstraint::Other, ProcessContents::Lax);
        let frag1 = builder.single_term(term1, None);
        let frag2 = builder.single_term(term2, None);
        let choice = frag1.alternate(frag2);
        let nfa = fragment_to_table(choice);

        // ##targetNamespace and ##other are mutually exclusive
        let result = check_upa(&nfa, &schema_set, Some(target_ns));
        assert!(result.is_ok());
    }

    #[test]
    fn test_wildcard_wildcard_list_overlap() {
        let mut schema_set = create_test_schema_set();
        let ns1 = schema_set.name_table.add("http://ns1.example.com");
        let ns2 = schema_set.name_table.add("http://ns2.example.com");

        // Build a choice between two list wildcards with overlapping namespaces
        let mut builder = FragmentBuilder::new();
        let term1 = NfaTerm::wildcard(
            NamespaceConstraint::List(vec![Some(ns1), Some(ns2)]),
            ProcessContents::Lax,
        );
        let term2 = NfaTerm::wildcard(
            NamespaceConstraint::List(vec![Some(ns2)]),
            ProcessContents::Lax,
        );
        let frag1 = builder.single_term(term1, None);
        let frag2 = builder.single_term(term2, None);
        let choice = frag1.alternate(frag2);
        let nfa = fragment_to_table(choice);

        let result = check_upa(&nfa, &schema_set, None);
        assert!(result.is_err());
        assert_cos_nonambig(result.unwrap_err());
    }

    #[test]
    fn test_wildcard_wildcard_list_no_overlap() {
        let mut schema_set = create_test_schema_set();
        let ns1 = schema_set.name_table.add("http://ns1.example.com");
        let ns2 = schema_set.name_table.add("http://ns2.example.com");

        // Build a choice between two list wildcards with non-overlapping namespaces
        let mut builder = FragmentBuilder::new();
        let term1 = NfaTerm::wildcard(
            NamespaceConstraint::List(vec![Some(ns1)]),
            ProcessContents::Lax,
        );
        let term2 = NfaTerm::wildcard(
            NamespaceConstraint::List(vec![Some(ns2)]),
            ProcessContents::Lax,
        );
        let frag1 = builder.single_term(term1, None);
        let frag2 = builder.single_term(term2, None);
        let choice = frag1.alternate(frag2);
        let nfa = fragment_to_table(choice);

        let result = check_upa(&nfa, &schema_set, None);
        assert!(result.is_ok());
    }

    // ========================================================================
    // Complex pattern tests
    // ========================================================================

    #[test]
    fn test_nested_choice_with_conflict() {
        let mut schema_set = create_test_schema_set();
        let name_a = schema_set.name_table.add("a");
        let name_b = schema_set.name_table.add("b");

        // Build nested choice: (a | (a | b))
        let mut builder = FragmentBuilder::new();
        let term_a1 = NfaTerm::element(name_a, None, None);
        let term_a2 = NfaTerm::element(name_a, None, None);
        let term_b = NfaTerm::element(name_b, None, None);

        let frag_a1 = builder.single_term(term_a1, None);
        let frag_a2 = builder.single_term(term_a2, None);
        let frag_b = builder.single_term(term_b, None);

        let inner_choice = frag_a2.alternate(frag_b);
        let outer_choice = frag_a1.alternate(inner_choice);
        let nfa = fragment_to_table(outer_choice);

        let result = check_upa(&nfa, &schema_set, None);
        assert!(result.is_err());
    }

    #[test]
    fn test_optional_element_before_same_element() {
        let mut schema_set = create_test_schema_set();
        let name_a = schema_set.name_table.add("a");

        // Build: a? a (optional a followed by required a)
        // This creates ambiguity: given input "a", could be (empty, a) or (a, ???)
        let mut builder = FragmentBuilder::new();
        let term1 = NfaTerm::element(name_a, None, None);
        let term2 = NfaTerm::element(name_a, None, None);

        let frag1 = builder.single_term(term1, None);
        let optional_a = frag1.optional();
        let frag2 = builder.single_term(term2, None);
        let sequence = optional_a.concat(frag2);
        let nfa = fragment_to_table(sequence);

        // This should detect a conflict at the initial state
        let result = check_upa(&nfa, &schema_set, None);
        assert!(result.is_err());
    }

    // ========================================================================
    // Helper function tests
    // ========================================================================

    #[test]
    fn test_elements_overlap_same() {
        let name = NameId(1);
        let ns = Some(NameId(2));
        assert!(elements_overlap(name, ns, name, ns));
    }

    #[test]
    fn test_elements_overlap_different_name() {
        assert!(!elements_overlap(NameId(1), None, NameId(2), None));
    }

    #[test]
    fn test_elements_overlap_different_ns() {
        let name = NameId(1);
        assert!(!elements_overlap(name, Some(NameId(2)), name, Some(NameId(3))));
    }

    #[test]
    fn test_element_wildcard_any() {
        assert!(element_wildcard_overlap(
            None,
            &NamespaceConstraint::Any,
            None,
            XsdVersion::V1_0
        ));
        assert!(element_wildcard_overlap(
            Some(NameId(1)),
            &NamespaceConstraint::Any,
            None,
            XsdVersion::V1_0
        ));
    }

    #[test]
    fn test_substitution_group_head_member_conflict() {
        let mut schema_set = create_test_schema_set();
        let head_name = schema_set.name_table.add("head");
        let member_name = schema_set.name_table.add("member");

        let head_key = schema_set
            .arenas
            .alloc_element(element_data(head_name, None));
        let member_key = schema_set
            .arenas
            .alloc_element(element_data(member_name, None));
        schema_set
            .arenas
            .elements
            .get_mut(member_key)
            .unwrap()
            .resolved_substitution_groups
            .push(head_key);

        let mut builder = FragmentBuilder::new();
        let term1 = NfaTerm::element(head_name, None, Some(head_key));
        let term2 = NfaTerm::element(member_name, None, Some(member_key));
        let frag1 = builder.single_term(term1, None);
        let frag2 = builder.single_term(term2, None);
        let choice = frag1.alternate(frag2);
        let nfa = fragment_to_table(choice);

        let result = check_upa(&nfa, &schema_set, None);
        assert!(result.is_err());
        assert_cos_nonambig(result.unwrap_err());
    }

    #[test]
    fn test_substitution_group_transitive_conflict() {
        let mut schema_set = create_test_schema_set();
        let head_name = schema_set.name_table.add("head");
        let mid_name = schema_set.name_table.add("mid");
        let leaf_name = schema_set.name_table.add("leaf");

        let head_key = schema_set
            .arenas
            .alloc_element(element_data(head_name, None));
        let mid_key = schema_set
            .arenas
            .alloc_element(element_data(mid_name, None));
        let leaf_key = schema_set
            .arenas
            .alloc_element(element_data(leaf_name, None));

        schema_set
            .arenas
            .elements
            .get_mut(mid_key)
            .unwrap()
            .resolved_substitution_groups
            .push(head_key);
        schema_set
            .arenas
            .elements
            .get_mut(leaf_key)
            .unwrap()
            .resolved_substitution_groups
            .push(mid_key);

        let mut builder = FragmentBuilder::new();
        let term1 = NfaTerm::element(head_name, None, Some(head_key));
        let term2 = NfaTerm::element(leaf_name, None, Some(leaf_key));
        let frag1 = builder.single_term(term1, None);
        let frag2 = builder.single_term(term2, None);
        let choice = frag1.alternate(frag2);
        let nfa = fragment_to_table(choice);

        let result = check_upa(&nfa, &schema_set, None);
        assert!(result.is_err());
        assert_cos_nonambig(result.unwrap_err());
    }

    #[test]
    fn test_substitution_group_blocked_no_conflict() {
        let mut schema_set = create_test_schema_set();
        let head_name = schema_set.name_table.add("head");
        let member_name = schema_set.name_table.add("member");

        let head_key = schema_set
            .arenas
            .alloc_element(element_data(head_name, None));
        let member_key = schema_set
            .arenas
            .alloc_element(element_data(member_name, None));
        schema_set
            .arenas
            .elements
            .get_mut(member_key)
            .unwrap()
            .resolved_substitution_groups
            .push(head_key);
        schema_set
            .arenas
            .elements
            .get_mut(head_key)
            .unwrap()
            .block = DerivationSet::SUBSTITUTION;

        let mut builder = FragmentBuilder::new();
        let term1 = NfaTerm::element(head_name, None, Some(head_key));
        let term2 = NfaTerm::element(member_name, None, Some(member_key));
        let frag1 = builder.single_term(term1, None);
        let frag2 = builder.single_term(term2, None);
        let choice = frag1.alternate(frag2);
        let nfa = fragment_to_table(choice);

        let result = check_upa(&nfa, &schema_set, None);
        assert!(result.is_ok());
    }

    #[test]
    fn test_element_wildcard_other() {
        let target_ns = Some(NameId(1));
        let other_ns = Some(NameId(2));

        // Element in target namespace does NOT match ##other
        assert!(!element_wildcard_overlap(
            target_ns,
            &NamespaceConstraint::Other,
            target_ns,
            XsdVersion::V1_0
        ));

        // Element in other namespace matches ##other
        assert!(element_wildcard_overlap(
            other_ns,
            &NamespaceConstraint::Other,
            target_ns,
            XsdVersion::V1_0
        ));
    }

    #[test]
    fn test_element_wildcard_other_absent_namespace_xsd10() {
        let target_ns = Some(NameId(1));

        assert!(!element_wildcard_overlap(
            None,
            &NamespaceConstraint::Other,
            target_ns,
            XsdVersion::V1_0
        ));
    }

    #[test]
    fn test_element_wildcard_other_absent_namespace_xsd11() {
        let target_ns = Some(NameId(1));

        assert!(element_wildcard_overlap(
            None,
            &NamespaceConstraint::Other,
            target_ns,
            XsdVersion::V1_1
        ));
    }

    #[test]
    fn test_element_wildcard_target_namespace() {
        let target_ns = Some(NameId(1));

        // Element in target namespace matches ##targetNamespace
        assert!(element_wildcard_overlap(
            target_ns,
            &NamespaceConstraint::TargetNamespace,
            target_ns,
            XsdVersion::V1_0
        ));

        // Element with no namespace does NOT match ##targetNamespace (if target is set)
        assert!(!element_wildcard_overlap(
            None,
            &NamespaceConstraint::TargetNamespace,
            target_ns,
            XsdVersion::V1_0
        ));
    }

    #[test]
    fn test_element_wildcard_local() {
        // Element with no namespace matches ##local
        assert!(element_wildcard_overlap(
            None,
            &NamespaceConstraint::Local,
            Some(NameId(1)),
            XsdVersion::V1_0
        ));

        // Element with namespace does NOT match ##local
        assert!(!element_wildcard_overlap(
            Some(NameId(2)),
            &NamespaceConstraint::Local,
            Some(NameId(1)),
            XsdVersion::V1_0
        ));
    }

    #[test]
    fn test_wildcards_overlap_any() {
        let target = Some(NameId(1));
        assert!(wildcards_overlap(
            &NamespaceConstraint::Any,
            &NamespaceConstraint::Any,
            target,
            XsdVersion::V1_0
        ));
        assert!(wildcards_overlap(
            &NamespaceConstraint::Any,
            &NamespaceConstraint::Other,
            target,
            XsdVersion::V1_0
        ));
    }

    #[test]
    fn test_wildcards_overlap_target_other() {
        let target = Some(NameId(1));
        // ##targetNamespace and ##other are mutually exclusive
        assert!(!wildcards_overlap(
            &NamespaceConstraint::TargetNamespace,
            &NamespaceConstraint::Other,
            target,
            XsdVersion::V1_0
        ));
    }

    #[test]
    fn test_wildcards_overlap_other_local_xsd10() {
        let target = Some(NameId(1));
        assert!(!wildcards_overlap(
            &NamespaceConstraint::Other,
            &NamespaceConstraint::Local,
            target,
            XsdVersion::V1_0
        ));
    }

    #[test]
    fn test_wildcards_overlap_other_local_xsd11() {
        let target = Some(NameId(1));
        assert!(wildcards_overlap(
            &NamespaceConstraint::Other,
            &NamespaceConstraint::Local,
            target,
            XsdVersion::V1_1
        ));
    }
}
