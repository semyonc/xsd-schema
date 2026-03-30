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
    let schema_set = create_test_schema_set();
    let name_a = schema_set.name_table.add("a");

    // Build a choice between two elements with the same name
    let builder = FragmentBuilder::new();
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
    let schema_set = create_test_schema_set();
    let name_a = schema_set.name_table.add("a");
    let ns1 = schema_set.name_table.add("http://ns1.example.com");
    let ns2 = schema_set.name_table.add("http://ns2.example.com");

    // Build a choice between two elements with same name but different namespaces
    let builder = FragmentBuilder::new();
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
    let schema_set = create_test_schema_set();
    let name_a = schema_set.name_table.add("a");

    // Build a sequence of two elements with the same name (no conflict in sequence)
    let builder = FragmentBuilder::new();
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
    let schema_set = create_test_schema_set();
    let name_a = schema_set.name_table.add("a");
    let name_b = schema_set.name_table.add("b");

    // Build a choice between two elements with different names
    let builder = FragmentBuilder::new();
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
    let schema_set = create_test_schema_set();
    let name_a = schema_set.name_table.add("a");

    // Build a choice between an element and a ##any wildcard
    let builder = FragmentBuilder::new();
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
    let schema_set = create_test_schema_set_v11();
    let name_a = schema_set.name_table.add("a");

    // Build a choice between an element and a ##any wildcard
    let builder = FragmentBuilder::new();
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
    let schema_set = create_test_schema_set();
    let name_a = schema_set.name_table.add("a");
    let target_ns = schema_set.name_table.add("http://target.example.com");

    // Build a choice between an element in target ns and ##other wildcard
    let builder = FragmentBuilder::new();
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
    let builder = FragmentBuilder::new();
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
    let schema_set = create_test_schema_set();
    let target_ns = schema_set.name_table.add("http://target.example.com");

    // Build a choice between ##targetNamespace and ##other wildcards
    let builder = FragmentBuilder::new();
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
    let schema_set = create_test_schema_set();
    let ns1 = schema_set.name_table.add("http://ns1.example.com");
    let ns2 = schema_set.name_table.add("http://ns2.example.com");

    // Build a choice between two list wildcards with overlapping namespaces
    let builder = FragmentBuilder::new();
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
    let schema_set = create_test_schema_set();
    let ns1 = schema_set.name_table.add("http://ns1.example.com");
    let ns2 = schema_set.name_table.add("http://ns2.example.com");

    // Build a choice between two list wildcards with non-overlapping namespaces
    let builder = FragmentBuilder::new();
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
    let schema_set = create_test_schema_set();
    let name_a = schema_set.name_table.add("a");
    let name_b = schema_set.name_table.add("b");

    // Build nested choice: (a | (a | b))
    let builder = FragmentBuilder::new();
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
    let schema_set = create_test_schema_set();
    let name_a = schema_set.name_table.add("a");

    // Build: a? a (optional a followed by required a)
    // This creates ambiguity: given input "a", could be (empty, a) or (a, ???)
    let builder = FragmentBuilder::new();
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

    let builder = FragmentBuilder::new();
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

    let builder = FragmentBuilder::new();
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

    let builder = FragmentBuilder::new();
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

// ========================================================================
// Not constraint tests
// ========================================================================

#[test]
fn test_element_wildcard_not_excluded() {
    let ns1 = Some(NameId(1));
    // Element in excluded namespace does NOT match Not([ns1])
    assert!(!element_wildcard_overlap(
        ns1,
        &NamespaceConstraint::Not(vec![ns1]),
        None,
        XsdVersion::V1_1
    ));
}

#[test]
fn test_element_wildcard_not_allowed() {
    let ns1 = Some(NameId(1));
    let ns2 = Some(NameId(2));
    // Element in non-excluded namespace matches Not([ns1])
    assert!(element_wildcard_overlap(
        ns2,
        &NamespaceConstraint::Not(vec![ns1]),
        None,
        XsdVersion::V1_1
    ));
}

#[test]
fn test_element_wildcard_not_absent_excluded() {
    // Not([None]) excludes absent namespace
    assert!(!element_wildcard_overlap(
        None,
        &NamespaceConstraint::Not(vec![None]),
        None,
        XsdVersion::V1_1
    ));
}

#[test]
fn test_wildcards_overlap_not_not() {
    let ns1 = Some(NameId(1));
    let ns2 = Some(NameId(2));
    // Not(ns1) vs Not(ns2): both exclude finite sets, infinite remainder overlaps
    assert!(wildcards_overlap(
        &NamespaceConstraint::Not(vec![ns1]),
        &NamespaceConstraint::Not(vec![ns2]),
        None,
        XsdVersion::V1_1
    ));
}

#[test]
fn test_wildcards_overlap_not_any() {
    let ns1 = Some(NameId(1));
    assert!(wildcards_overlap(
        &NamespaceConstraint::Not(vec![ns1]),
        &NamespaceConstraint::Any,
        None,
        XsdVersion::V1_1
    ));
}

#[test]
fn test_wildcards_overlap_not_target_excluded() {
    let target = Some(NameId(1));
    // Not([target]) vs TargetNamespace: no overlap (target is excluded)
    assert!(!wildcards_overlap(
        &NamespaceConstraint::Not(vec![target]),
        &NamespaceConstraint::TargetNamespace,
        target,
        XsdVersion::V1_1
    ));
}

#[test]
fn test_wildcards_overlap_not_target_not_excluded() {
    let target = Some(NameId(1));
    let ns2 = Some(NameId(2));
    // Not([ns2]) vs TargetNamespace: overlaps (target is not excluded)
    assert!(wildcards_overlap(
        &NamespaceConstraint::Not(vec![ns2]),
        &NamespaceConstraint::TargetNamespace,
        target,
        XsdVersion::V1_1
    ));
}

#[test]
fn test_wildcards_overlap_not_list_partial() {
    let ns1 = Some(NameId(1));
    let ns2 = Some(NameId(2));
    // Not([ns1]) vs List([ns1, ns2]): overlaps because ns2 is not excluded
    assert!(wildcards_overlap(
        &NamespaceConstraint::Not(vec![ns1]),
        &NamespaceConstraint::List(vec![ns1, ns2]),
        None,
        XsdVersion::V1_1
    ));
}

#[test]
fn test_wildcards_overlap_not_list_all_excluded() {
    let ns1 = Some(NameId(1));
    // Not([ns1]) vs List([ns1]): no overlap (all list items excluded)
    assert!(!wildcards_overlap(
        &NamespaceConstraint::Not(vec![ns1]),
        &NamespaceConstraint::List(vec![ns1]),
        None,
        XsdVersion::V1_1
    ));
}

// ========================================================================
// notQName UPA tests
// ========================================================================

#[test]
fn test_element_wildcard_no_overlap_via_not_qnames() {
    // When a wildcard's notQName excludes a specific element,
    // they should not be considered overlapping
    let schema_set = create_test_schema_set();
    let name_a = schema_set.name_table.add("a");

    let builder = FragmentBuilder::new();
    let term1 = NfaTerm::element(name_a, None, None);
    // Wildcard that explicitly excludes element "a" (absent ns)
    let term2 = NfaTerm::wildcard_with_not_qnames(
        NamespaceConstraint::Any,
        ProcessContents::Lax,
        vec![(None, name_a)],
    );
    let frag1 = builder.single_term(term1, None);
    let frag2 = builder.single_term(term2, None);
    let choice = frag1.alternate(frag2);
    let nfa = fragment_to_table(choice);

    // XSD 1.0: normally element vs ##any wildcard would conflict,
    // but notQName excludes this element, so no overlap
    let result = check_upa(&nfa, &schema_set, None);
    assert!(result.is_ok());
}

#[test]
fn test_element_wildcard_subst_member_namespace_conflict() {
    // Head element is in target namespace (no overlap with ##other wildcard),
    // but a substitution group member is in another namespace (overlaps).
    // UPA should detect this conflict.
    let mut schema_set = create_test_schema_set();
    let target_ns = schema_set.name_table.add("http://target.example.com");
    let other_ns = schema_set.name_table.add("http://other.example.com");
    let head_name = schema_set.name_table.add("head");
    let member_name = schema_set.name_table.add("member");

    let head_key = schema_set
        .arenas
        .alloc_element(element_data(head_name, Some(target_ns)));
    let member_key = schema_set
        .arenas
        .alloc_element(element_data(member_name, Some(other_ns)));
    schema_set
        .arenas
        .elements
        .get_mut(member_key)
        .unwrap()
        .resolved_substitution_groups
        .push(head_key);

    let builder = FragmentBuilder::new();
    // Element in target namespace with substitution group
    let term1 = NfaTerm::element(head_name, Some(target_ns), Some(head_key));
    // ##other wildcard — matches other_ns but not target_ns
    let term2 = NfaTerm::wildcard(NamespaceConstraint::Other, ProcessContents::Lax);
    let frag1 = builder.single_term(term1, None);
    let frag2 = builder.single_term(term2, None);
    let choice = frag1.alternate(frag2);
    let nfa = fragment_to_table(choice);

    // XSD 1.0: head element is in target_ns (no overlap with ##other),
    // BUT member is in other_ns (DOES overlap with ##other).
    // UPA should detect conflict.
    let result = check_upa(&nfa, &schema_set, Some(target_ns));
    assert!(result.is_err(), "should detect conflict via substitution member in other namespace");
    assert_cos_nonambig(result.unwrap_err());
}

#[test]
fn test_element_wildcard_subst_member_excluded_by_not_qnames() {
    // Head element is excluded by notQName, but substitution member is NOT excluded.
    // UPA should still detect conflict.
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

    let builder = FragmentBuilder::new();
    let term1 = NfaTerm::element(head_name, None, Some(head_key));
    // Wildcard excludes head but NOT member
    let term2 = NfaTerm::wildcard_with_not_qnames(
        NamespaceConstraint::Any,
        ProcessContents::Lax,
        vec![(None, head_name)], // only head excluded
    );
    let frag1 = builder.single_term(term1, None);
    let frag2 = builder.single_term(term2, None);
    let choice = frag1.alternate(frag2);
    let nfa = fragment_to_table(choice);

    // XSD 1.0: head is excluded by notQName, but member is not.
    // The wildcard can still match "member", so UPA conflict exists.
    let result = check_upa(&nfa, &schema_set, None);
    assert!(result.is_err(), "should detect conflict: member not excluded by notQName");
    assert_cos_nonambig(result.unwrap_err());
}
