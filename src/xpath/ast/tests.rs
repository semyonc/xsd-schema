// ============================================================================
// Tests
// ============================================================================

use super::*;

#[test]
fn test_axis_direction() {
    assert!(Axis::Child.is_forward());
    assert!(Axis::Parent.is_reverse());
    assert!(Axis::Ancestor.is_reverse());
    assert!(Axis::Descendant.is_forward());
}

#[test]
fn test_name_test() {
    let any = NameTest::any();
    assert!(any.prefix.is_none());
    assert!(any.local_name.is_none());

    let qname = NameTest::qname("xs".to_string(), "integer".to_string());
    assert_eq!(qname.prefix, Some("xs".to_string()));
    assert_eq!(qname.local_name, Some("integer".to_string()));
}

#[test]
fn test_value_node() {
    let s = ValueNode::String("hello".to_string());
    match s {
        ValueNode::String(v) => assert_eq!(v, "hello"),
        _ => panic!("Expected string"),
    }

    let i = ValueNode::Integer("42".to_string());
    match i {
        ValueNode::Integer(v) => assert_eq!(v, "42"),
        _ => panic!("Expected integer"),
    }
}
