use super::*;
use crate::namespace::table::NameTable;
use crate::xpath::arena::SourceSpan;
use crate::xpath::ast::{
    BinaryOpNode, ExprNode, FunctionCallNode, IfNode, RangeNode, UnaryOpKind, UnaryOpNode,
    ValueNode,
};
use crate::xpath::bind::bind_node;
use crate::xpath::context::{NameBinder, XPathContext};
use crate::xpath::RoXmlNavigator;

/// Helper to create a test arena with a function call
fn make_function_call(
    arena: &mut AstArena,
    prefix: &str,
    local_name: &str,
    args: Vec<AstNodeId>,
) -> AstNodeId {
    let span = SourceSpan::new(0, 10);
    let func = FunctionCallNode::new(prefix.to_string(), local_name.to_string(), args, span);
    arena.add(AstNode::FunctionCall(func))
}

/// Helper to wrap a node in an Expr
fn wrap_in_expr(arena: &mut AstArena, node_id: AstNodeId) -> AstNodeId {
    let span = SourceSpan::new(0, 10);
    let expr = ExprNode::single(node_id, span);
    arena.add(AstNode::Expr(expr))
}

/// Helper to bind and eval a manually constructed AST
fn bind_and_eval(
    arena: &mut AstArena,
    root: AstNodeId,
) -> Result<XPathValue<RoXmlNavigator<'static>>, XPathError> {
    let names = NameTable::new();
    let ctx = XPathContext::new(&names);
    let mut binder = NameBinder::new();

    bind_node(arena, root, &ctx, &mut binder)?;

    let mut dyn_ctx: DynamicContext<'_, RoXmlNavigator<'static>> =
        DynamicContext::new(&ctx, binder.len());

    eval_node(arena, root, &mut dyn_ctx)
}

#[test]
fn test_eval_true_false() {
    // Test true()
    let mut arena = AstArena::new();
    let func_id = make_function_call(&mut arena, "", "true", vec![]);
    let root = wrap_in_expr(&mut arena, func_id);

    let result = bind_and_eval(&mut arena, root).unwrap();
    match result {
        XPathValue::Item(XmlItem::Atomic(v)) => {
            assert_eq!(v.as_boolean(), Some(true));
        }
        _ => panic!("Expected boolean true"),
    }

    // Test false()
    let mut arena = AstArena::new();
    let func_id = make_function_call(&mut arena, "", "false", vec![]);
    let root = wrap_in_expr(&mut arena, func_id);

    let result = bind_and_eval(&mut arena, root).unwrap();
    match result {
        XPathValue::Item(XmlItem::Atomic(v)) => {
            assert_eq!(v.as_boolean(), Some(false));
        }
        _ => panic!("Expected boolean false"),
    }
}

#[test]
fn test_eval_concat() {
    let mut arena = AstArena::new();
    let arg1 = arena.add(AstNode::Value(ValueNode::String("Hello".to_string())));
    let arg2 = arena.add(AstNode::Value(ValueNode::String(" ".to_string())));
    let arg3 = arena.add(AstNode::Value(ValueNode::String("World".to_string())));
    let func_id = make_function_call(&mut arena, "", "concat", vec![arg1, arg2, arg3]);
    let root = wrap_in_expr(&mut arena, func_id);

    let result = bind_and_eval(&mut arena, root).unwrap();
    match result {
        XPathValue::Item(XmlItem::Atomic(v)) => {
            assert_eq!(v.as_string(), Some("Hello World"));
        }
        _ => panic!("Expected string"),
    }
}

#[test]
fn test_eval_nested_function() {
    let mut arena = AstArena::new();
    // Create concat('a', 'b')
    let arg1 = arena.add(AstNode::Value(ValueNode::String("a".to_string())));
    let arg2 = arena.add(AstNode::Value(ValueNode::String("b".to_string())));
    let inner_func = make_function_call(&mut arena, "", "concat", vec![arg1, arg2]);
    // Create upper-case(concat('a', 'b'))
    let outer_func = make_function_call(&mut arena, "", "upper-case", vec![inner_func]);
    let root = wrap_in_expr(&mut arena, outer_func);

    let result = bind_and_eval(&mut arena, root).unwrap();
    match result {
        XPathValue::Item(XmlItem::Atomic(v)) => {
            assert_eq!(v.as_string(), Some("AB"));
        }
        _ => panic!("Expected string"),
    }
}

#[test]
fn test_eval_integer_literal() {
    let mut arena = AstArena::new();
    let val = arena.add(AstNode::Value(ValueNode::Integer("42".to_string())));
    let root = wrap_in_expr(&mut arena, val);

    let result = bind_and_eval(&mut arena, root).unwrap();
    match result {
        XPathValue::Item(XmlItem::Atomic(v)) => {
            assert_eq!(
                v.as_integer().map(|i| i.to_string()),
                Some("42".to_string())
            );
        }
        _ => panic!("Expected integer"),
    }
}

#[test]
fn test_eval_double_literal() {
    let mut arena = AstArena::new();
    let val = arena.add(AstNode::Value(ValueNode::Double("2.5".to_string())));
    let root = wrap_in_expr(&mut arena, val);

    let result = bind_and_eval(&mut arena, root).unwrap();
    match result {
        XPathValue::Item(XmlItem::Atomic(v)) => {
            assert!(v.as_double().is_some());
        }
        _ => panic!("Expected double"),
    }
}

#[test]
fn test_eval_string_literal() {
    let mut arena = AstArena::new();
    let val = arena.add(AstNode::Value(ValueNode::String("hello".to_string())));
    let root = wrap_in_expr(&mut arena, val);

    let result = bind_and_eval(&mut arena, root).unwrap();
    match result {
        XPathValue::Item(XmlItem::Atomic(v)) => {
            assert_eq!(v.as_string(), Some("hello"));
        }
        _ => panic!("Expected string"),
    }
}

#[test]
fn test_eval_empty_sequence() {
    let mut arena = AstArena::new();
    let val = arena.add(AstNode::Value(ValueNode::Empty));
    let root = wrap_in_expr(&mut arena, val);

    let result = bind_and_eval(&mut arena, root).unwrap();
    assert!(result.is_empty());
}

#[test]
fn test_eval_if_true() {
    let mut arena = AstArena::new();
    // Create true() for test condition
    let test_func = make_function_call(&mut arena, "", "true", vec![]);
    // Create then/else branches
    let then_val = arena.add(AstNode::Value(ValueNode::String("yes".to_string())));
    let else_val = arena.add(AstNode::Value(ValueNode::String("no".to_string())));
    // Create if node
    let span = SourceSpan::new(0, 30);
    let if_node = IfNode::new(test_func, then_val, else_val, span);
    let if_id = arena.add(AstNode::If(if_node));
    let root = wrap_in_expr(&mut arena, if_id);

    let result = bind_and_eval(&mut arena, root).unwrap();
    match result {
        XPathValue::Item(XmlItem::Atomic(v)) => {
            assert_eq!(v.as_string(), Some("yes"));
        }
        _ => panic!("Expected string 'yes'"),
    }
}

#[test]
fn test_eval_if_false() {
    let mut arena = AstArena::new();
    // Create false() for test condition
    let test_func = make_function_call(&mut arena, "", "false", vec![]);
    // Create then/else branches
    let then_val = arena.add(AstNode::Value(ValueNode::String("yes".to_string())));
    let else_val = arena.add(AstNode::Value(ValueNode::String("no".to_string())));
    // Create if node
    let span = SourceSpan::new(0, 30);
    let if_node = IfNode::new(test_func, then_val, else_val, span);
    let if_id = arena.add(AstNode::If(if_node));
    let root = wrap_in_expr(&mut arena, if_id);

    let result = bind_and_eval(&mut arena, root).unwrap();
    match result {
        XPathValue::Item(XmlItem::Atomic(v)) => {
            assert_eq!(v.as_string(), Some("no"));
        }
        _ => panic!("Expected string 'no'"),
    }
}

#[test]
fn test_eval_not() {
    // Test not(true())
    let mut arena = AstArena::new();
    let true_func = make_function_call(&mut arena, "", "true", vec![]);
    let not_func = make_function_call(&mut arena, "", "not", vec![true_func]);
    let root = wrap_in_expr(&mut arena, not_func);

    let result = bind_and_eval(&mut arena, root).unwrap();
    match result {
        XPathValue::Item(XmlItem::Atomic(v)) => {
            assert_eq!(v.as_boolean(), Some(false));
        }
        _ => panic!("Expected boolean false"),
    }

    // Test not(false())
    let mut arena = AstArena::new();
    let false_func = make_function_call(&mut arena, "", "false", vec![]);
    let not_func = make_function_call(&mut arena, "", "not", vec![false_func]);
    let root = wrap_in_expr(&mut arena, not_func);

    let result = bind_and_eval(&mut arena, root).unwrap();
    match result {
        XPathValue::Item(XmlItem::Atomic(v)) => {
            assert_eq!(v.as_boolean(), Some(true));
        }
        _ => panic!("Expected boolean true"),
    }
}

#[test]
fn test_eval_position_last() {
    let names = NameTable::new();
    let ctx = XPathContext::new(&names);
    let mut binder = NameBinder::new();

    // Test position()
    let mut arena = AstArena::new();
    let func_id = make_function_call(&mut arena, "", "position", vec![]);
    let root = wrap_in_expr(&mut arena, func_id);

    bind_node(&mut arena, root, &ctx, &mut binder).unwrap();

    let mut dyn_ctx: DynamicContext<'_, RoXmlNavigator<'static>> =
        DynamicContext::new(&ctx, binder.len()).with_position(3, 10);

    let result = eval_node(&arena, root, &mut dyn_ctx).unwrap();
    match result {
        XPathValue::Item(XmlItem::Atomic(v)) => {
            assert_eq!(v.as_integer().map(|i| i.to_string()), Some("3".to_string()));
        }
        _ => panic!("Expected integer 3"),
    }

    // Test last()
    let mut arena = AstArena::new();
    let func_id = make_function_call(&mut arena, "", "last", vec![]);
    let root = wrap_in_expr(&mut arena, func_id);

    bind_node(&mut arena, root, &ctx, &mut binder).unwrap();

    let result = eval_node(&arena, root, &mut dyn_ctx).unwrap();
    match result {
        XPathValue::Item(XmlItem::Atomic(v)) => {
            assert_eq!(
                v.as_integer().map(|i| i.to_string()),
                Some("10".to_string())
            );
        }
        _ => panic!("Expected integer 10"),
    }
}

#[test]
fn test_eval_count() {
    // count(()) with empty sequence
    let mut arena = AstArena::new();
    let empty = arena.add(AstNode::Value(ValueNode::Empty));
    let func_id = make_function_call(&mut arena, "", "count", vec![empty]);
    let root = wrap_in_expr(&mut arena, func_id);

    let result = bind_and_eval(&mut arena, root).unwrap();
    match result {
        XPathValue::Item(XmlItem::Atomic(v)) => {
            assert_eq!(v.as_integer().map(|i| i.to_string()), Some("0".to_string()));
        }
        _ => panic!("Expected integer 0"),
    }
}

#[test]
fn test_eval_empty_exists() {
    // Test empty(())
    let mut arena = AstArena::new();
    let empty = arena.add(AstNode::Value(ValueNode::Empty));
    let func_id = make_function_call(&mut arena, "", "empty", vec![empty]);
    let root = wrap_in_expr(&mut arena, func_id);

    let result = bind_and_eval(&mut arena, root).unwrap();
    match result {
        XPathValue::Item(XmlItem::Atomic(v)) => {
            assert_eq!(v.as_boolean(), Some(true));
        }
        _ => panic!("Expected boolean true"),
    }

    // Test exists(())
    let mut arena = AstArena::new();
    let empty = arena.add(AstNode::Value(ValueNode::Empty));
    let func_id = make_function_call(&mut arena, "", "exists", vec![empty]);
    let root = wrap_in_expr(&mut arena, func_id);

    let result = bind_and_eval(&mut arena, root).unwrap();
    match result {
        XPathValue::Item(XmlItem::Atomic(v)) => {
            assert_eq!(v.as_boolean(), Some(false));
        }
        _ => panic!("Expected boolean false"),
    }
}

#[test]
fn test_full_pipeline() {
    let names = NameTable::new();
    let ctx = XPathContext::new(&names);
    let mut binder = NameBinder::new();

    // Build AST for: concat('Hello, ', 'World!')
    let mut arena = AstArena::new();
    let arg1 = arena.add(AstNode::Value(ValueNode::String("Hello, ".to_string())));
    let arg2 = arena.add(AstNode::Value(ValueNode::String("World!".to_string())));
    let func_id = make_function_call(&mut arena, "", "concat", vec![arg1, arg2]);
    let root = wrap_in_expr(&mut arena, func_id);

    // Bind
    bind_node(&mut arena, root, &ctx, &mut binder).expect("bind failed");

    // Eval
    let mut dyn_ctx: DynamicContext<'_, RoXmlNavigator<'static>> =
        DynamicContext::new(&ctx, binder.len());
    let result = eval_node(&arena, root, &mut dyn_ctx).expect("eval failed");

    // Verify
    match result {
        XPathValue::Item(XmlItem::Atomic(v)) => {
            assert_eq!(v.as_string(), Some("Hello, World!"));
        }
        _ => panic!("Expected string 'Hello, World!'"),
    }
}

#[test]
fn test_eval_arithmetic_add() {
    let mut arena = AstArena::new();
    let left = arena.add(AstNode::Value(ValueNode::Integer("5".to_string())));
    let right = arena.add(AstNode::Value(ValueNode::Integer("3".to_string())));
    let span = SourceSpan::new(0, 5);
    let bin_op = BinaryOpNode::new(BinaryOpKind::Add, left, right, span);
    let bin_id = arena.add(AstNode::BinaryOp(bin_op));
    let root = wrap_in_expr(&mut arena, bin_id);

    let result = bind_and_eval(&mut arena, root).unwrap();
    match result {
        XPathValue::Item(XmlItem::Atomic(v)) => {
            assert_eq!(v.as_integer().map(|i| i.to_string()), Some("8".to_string()));
        }
        _ => panic!("Expected integer 8"),
    }
}

#[test]
fn test_eval_arithmetic_sub() {
    let mut arena = AstArena::new();
    let left = arena.add(AstNode::Value(ValueNode::Integer("5".to_string())));
    let right = arena.add(AstNode::Value(ValueNode::Integer("3".to_string())));
    let span = SourceSpan::new(0, 5);
    let bin_op = BinaryOpNode::new(BinaryOpKind::Sub, left, right, span);
    let bin_id = arena.add(AstNode::BinaryOp(bin_op));
    let root = wrap_in_expr(&mut arena, bin_id);

    let result = bind_and_eval(&mut arena, root).unwrap();
    match result {
        XPathValue::Item(XmlItem::Atomic(v)) => {
            assert_eq!(v.as_integer().map(|i| i.to_string()), Some("2".to_string()));
        }
        _ => panic!("Expected integer 2"),
    }
}

#[test]
fn test_eval_arithmetic_mul() {
    let mut arena = AstArena::new();
    let left = arena.add(AstNode::Value(ValueNode::Integer("5".to_string())));
    let right = arena.add(AstNode::Value(ValueNode::Integer("3".to_string())));
    let span = SourceSpan::new(0, 5);
    let bin_op = BinaryOpNode::new(BinaryOpKind::Mul, left, right, span);
    let bin_id = arena.add(AstNode::BinaryOp(bin_op));
    let root = wrap_in_expr(&mut arena, bin_id);

    let result = bind_and_eval(&mut arena, root).unwrap();
    match result {
        XPathValue::Item(XmlItem::Atomic(v)) => {
            assert_eq!(
                v.as_integer().map(|i| i.to_string()),
                Some("15".to_string())
            );
        }
        _ => panic!("Expected integer 15"),
    }
}

#[test]
fn test_eval_logical_and_short_circuit() {
    let mut arena = AstArena::new();
    let left = arena.add(AstNode::Value(ValueNode::Boolean(false)));
    let right = arena.add(AstNode::Value(ValueNode::Boolean(true)));
    let span = SourceSpan::new(0, 10);
    let bin_op = BinaryOpNode::new(BinaryOpKind::And, left, right, span);
    let bin_id = arena.add(AstNode::BinaryOp(bin_op));
    let root = wrap_in_expr(&mut arena, bin_id);

    let result = bind_and_eval(&mut arena, root).unwrap();
    match result {
        XPathValue::Item(XmlItem::Atomic(v)) => {
            assert_eq!(v.as_boolean(), Some(false));
        }
        _ => panic!("Expected boolean false"),
    }
}

#[test]
fn test_eval_logical_or_short_circuit() {
    let mut arena = AstArena::new();
    let left = arena.add(AstNode::Value(ValueNode::Boolean(true)));
    let right = arena.add(AstNode::Value(ValueNode::Boolean(false)));
    let span = SourceSpan::new(0, 10);
    let bin_op = BinaryOpNode::new(BinaryOpKind::Or, left, right, span);
    let bin_id = arena.add(AstNode::BinaryOp(bin_op));
    let root = wrap_in_expr(&mut arena, bin_id);

    let result = bind_and_eval(&mut arena, root).unwrap();
    match result {
        XPathValue::Item(XmlItem::Atomic(v)) => {
            assert_eq!(v.as_boolean(), Some(true));
        }
        _ => panic!("Expected boolean true"),
    }
}

#[test]
fn test_unary_negate() {
    // -5 → -5
    let mut arena = AstArena::new();
    let val = arena.add(AstNode::Value(ValueNode::Integer("5".to_string())));
    let span = SourceSpan::new(0, 2);
    let unary_op = UnaryOpNode::new(UnaryOpKind::Negate, val, span);
    let unary_id = arena.add(AstNode::UnaryOp(unary_op));
    let root = wrap_in_expr(&mut arena, unary_id);

    let result = bind_and_eval(&mut arena, root).unwrap();
    match result {
        XPathValue::Item(XmlItem::Atomic(v)) => {
            assert_eq!(
                v.as_integer().map(|i| i.to_string()),
                Some("-5".to_string())
            );
        }
        _ => panic!("Expected integer -5"),
    }

    // --5 → 5 (double negation)
    let mut arena = AstArena::new();
    let val = arena.add(AstNode::Value(ValueNode::Integer("5".to_string())));
    let span = SourceSpan::new(0, 3);
    let inner_unary = UnaryOpNode::new(UnaryOpKind::Negate, val, span);
    let inner_id = arena.add(AstNode::UnaryOp(inner_unary));
    let outer_unary = UnaryOpNode::new(UnaryOpKind::Negate, inner_id, span);
    let outer_id = arena.add(AstNode::UnaryOp(outer_unary));
    let root = wrap_in_expr(&mut arena, outer_id);

    let result = bind_and_eval(&mut arena, root).unwrap();
    match result {
        XPathValue::Item(XmlItem::Atomic(v)) => {
            assert_eq!(v.as_integer().map(|i| i.to_string()), Some("5".to_string()));
        }
        _ => panic!("Expected integer 5"),
    }

    // -1.5 → -1.5 (double)
    let mut arena = AstArena::new();
    let val = arena.add(AstNode::Value(ValueNode::Double("1.5".to_string())));
    let span = SourceSpan::new(0, 4);
    let unary_op = UnaryOpNode::new(UnaryOpKind::Negate, val, span);
    let unary_id = arena.add(AstNode::UnaryOp(unary_op));
    let root = wrap_in_expr(&mut arena, unary_id);

    let result = bind_and_eval(&mut arena, root).unwrap();
    match result {
        XPathValue::Item(XmlItem::Atomic(v)) => {
            let d = v.as_double().expect("Expected double");
            assert!((d - (-1.5)).abs() < f64::EPSILON);
        }
        _ => panic!("Expected double -1.5"),
    }
}

#[test]
fn test_unary_identity() {
    // +5 → 5
    let mut arena = AstArena::new();
    let val = arena.add(AstNode::Value(ValueNode::Integer("5".to_string())));
    let span = SourceSpan::new(0, 2);
    let unary_op = UnaryOpNode::new(UnaryOpKind::Identity, val, span);
    let unary_id = arena.add(AstNode::UnaryOp(unary_op));
    let root = wrap_in_expr(&mut arena, unary_id);

    let result = bind_and_eval(&mut arena, root).unwrap();
    match result {
        XPathValue::Item(XmlItem::Atomic(v)) => {
            assert_eq!(v.as_integer().map(|i| i.to_string()), Some("5".to_string()));
        }
        _ => panic!("Expected integer 5"),
    }

    // +-5 → -5 (identity then negate in value)
    let mut arena = AstArena::new();
    let val = arena.add(AstNode::Value(ValueNode::Integer("-5".to_string())));
    let span = SourceSpan::new(0, 3);
    let unary_op = UnaryOpNode::new(UnaryOpKind::Identity, val, span);
    let unary_id = arena.add(AstNode::UnaryOp(unary_op));
    let root = wrap_in_expr(&mut arena, unary_id);

    let result = bind_and_eval(&mut arena, root).unwrap();
    match result {
        XPathValue::Item(XmlItem::Atomic(v)) => {
            assert_eq!(
                v.as_integer().map(|i| i.to_string()),
                Some("-5".to_string())
            );
        }
        _ => panic!("Expected integer -5"),
    }
}

#[test]
fn test_unary_empty_sequence() {
    // -() → ()
    let mut arena = AstArena::new();
    let val = arena.add(AstNode::Value(ValueNode::Empty));
    let span = SourceSpan::new(0, 3);
    let unary_op = UnaryOpNode::new(UnaryOpKind::Negate, val, span);
    let unary_id = arena.add(AstNode::UnaryOp(unary_op));
    let root = wrap_in_expr(&mut arena, unary_id);

    let result = bind_and_eval(&mut arena, root).unwrap();
    assert!(result.is_empty());
}

// ========================================================================
// General Comparison Operator Tests
// ========================================================================

#[test]
fn test_general_eq_single() {
    // 1 = 1 → true
    let mut arena = AstArena::new();
    let left = arena.add(AstNode::Value(ValueNode::Integer("1".to_string())));
    let right = arena.add(AstNode::Value(ValueNode::Integer("1".to_string())));
    let span = SourceSpan::new(0, 5);
    let bin_op = BinaryOpNode::new(BinaryOpKind::GeneralEq, left, right, span);
    let bin_id = arena.add(AstNode::BinaryOp(bin_op));
    let root = wrap_in_expr(&mut arena, bin_id);

    let result = bind_and_eval(&mut arena, root).unwrap();
    match result {
        XPathValue::Item(XmlItem::Atomic(v)) => {
            assert_eq!(v.as_boolean(), Some(true));
        }
        _ => panic!("Expected boolean true"),
    }

    // 1 = 2 → false
    let mut arena = AstArena::new();
    let left = arena.add(AstNode::Value(ValueNode::Integer("1".to_string())));
    let right = arena.add(AstNode::Value(ValueNode::Integer("2".to_string())));
    let span = SourceSpan::new(0, 5);
    let bin_op = BinaryOpNode::new(BinaryOpKind::GeneralEq, left, right, span);
    let bin_id = arena.add(AstNode::BinaryOp(bin_op));
    let root = wrap_in_expr(&mut arena, bin_id);

    let result = bind_and_eval(&mut arena, root).unwrap();
    match result {
        XPathValue::Item(XmlItem::Atomic(v)) => {
            assert_eq!(v.as_boolean(), Some(false));
        }
        _ => panic!("Expected boolean false"),
    }
}

#[test]
fn test_general_eq_sequence() {
    // (1, 2, 3) = 2 → true (exists a pair)
    let mut arena = AstArena::new();
    let v1 = arena.add(AstNode::Value(ValueNode::Integer("1".to_string())));
    let v2 = arena.add(AstNode::Value(ValueNode::Integer("2".to_string())));
    let v3 = arena.add(AstNode::Value(ValueNode::Integer("3".to_string())));
    let span = SourceSpan::new(0, 10);
    let left_seq = ExprNode::sequence(vec![v1, v2, v3], span);
    let left = arena.add(AstNode::Expr(left_seq));
    let right = arena.add(AstNode::Value(ValueNode::Integer("2".to_string())));
    let bin_op = BinaryOpNode::new(BinaryOpKind::GeneralEq, left, right, span);
    let bin_id = arena.add(AstNode::BinaryOp(bin_op));
    let root = wrap_in_expr(&mut arena, bin_id);

    let result = bind_and_eval(&mut arena, root).unwrap();
    match result {
        XPathValue::Item(XmlItem::Atomic(v)) => {
            assert_eq!(v.as_boolean(), Some(true));
        }
        _ => panic!("Expected boolean true"),
    }

    // (1, 2, 3) = 4 → false
    let mut arena = AstArena::new();
    let v1 = arena.add(AstNode::Value(ValueNode::Integer("1".to_string())));
    let v2 = arena.add(AstNode::Value(ValueNode::Integer("2".to_string())));
    let v3 = arena.add(AstNode::Value(ValueNode::Integer("3".to_string())));
    let span = SourceSpan::new(0, 10);
    let left_seq = ExprNode::sequence(vec![v1, v2, v3], span);
    let left = arena.add(AstNode::Expr(left_seq));
    let right = arena.add(AstNode::Value(ValueNode::Integer("4".to_string())));
    let bin_op = BinaryOpNode::new(BinaryOpKind::GeneralEq, left, right, span);
    let bin_id = arena.add(AstNode::BinaryOp(bin_op));
    let root = wrap_in_expr(&mut arena, bin_id);

    let result = bind_and_eval(&mut arena, root).unwrap();
    match result {
        XPathValue::Item(XmlItem::Atomic(v)) => {
            assert_eq!(v.as_boolean(), Some(false));
        }
        _ => panic!("Expected boolean false"),
    }
}

#[test]
fn test_general_ne() {
    // 1 != 2 → true
    let mut arena = AstArena::new();
    let left = arena.add(AstNode::Value(ValueNode::Integer("1".to_string())));
    let right = arena.add(AstNode::Value(ValueNode::Integer("2".to_string())));
    let span = SourceSpan::new(0, 6);
    let bin_op = BinaryOpNode::new(BinaryOpKind::GeneralNe, left, right, span);
    let bin_id = arena.add(AstNode::BinaryOp(bin_op));
    let root = wrap_in_expr(&mut arena, bin_id);

    let result = bind_and_eval(&mut arena, root).unwrap();
    match result {
        XPathValue::Item(XmlItem::Atomic(v)) => {
            assert_eq!(v.as_boolean(), Some(true));
        }
        _ => panic!("Expected boolean true"),
    }

    // 1 != 1 → false
    let mut arena = AstArena::new();
    let left = arena.add(AstNode::Value(ValueNode::Integer("1".to_string())));
    let right = arena.add(AstNode::Value(ValueNode::Integer("1".to_string())));
    let span = SourceSpan::new(0, 6);
    let bin_op = BinaryOpNode::new(BinaryOpKind::GeneralNe, left, right, span);
    let bin_id = arena.add(AstNode::BinaryOp(bin_op));
    let root = wrap_in_expr(&mut arena, bin_id);

    let result = bind_and_eval(&mut arena, root).unwrap();
    match result {
        XPathValue::Item(XmlItem::Atomic(v)) => {
            assert_eq!(v.as_boolean(), Some(false));
        }
        _ => panic!("Expected boolean false"),
    }
}

#[test]
fn test_general_lt() {
    // 1 < 2 → true
    let mut arena = AstArena::new();
    let left = arena.add(AstNode::Value(ValueNode::Integer("1".to_string())));
    let right = arena.add(AstNode::Value(ValueNode::Integer("2".to_string())));
    let span = SourceSpan::new(0, 5);
    let bin_op = BinaryOpNode::new(BinaryOpKind::GeneralLt, left, right, span);
    let bin_id = arena.add(AstNode::BinaryOp(bin_op));
    let root = wrap_in_expr(&mut arena, bin_id);

    let result = bind_and_eval(&mut arena, root).unwrap();
    match result {
        XPathValue::Item(XmlItem::Atomic(v)) => {
            assert_eq!(v.as_boolean(), Some(true));
        }
        _ => panic!("Expected boolean true"),
    }

    // 2 < 1 → false
    let mut arena = AstArena::new();
    let left = arena.add(AstNode::Value(ValueNode::Integer("2".to_string())));
    let right = arena.add(AstNode::Value(ValueNode::Integer("1".to_string())));
    let span = SourceSpan::new(0, 5);
    let bin_op = BinaryOpNode::new(BinaryOpKind::GeneralLt, left, right, span);
    let bin_id = arena.add(AstNode::BinaryOp(bin_op));
    let root = wrap_in_expr(&mut arena, bin_id);

    let result = bind_and_eval(&mut arena, root).unwrap();
    match result {
        XPathValue::Item(XmlItem::Atomic(v)) => {
            assert_eq!(v.as_boolean(), Some(false));
        }
        _ => panic!("Expected boolean false"),
    }
}

#[test]
fn test_general_le_gt_ge() {
    // 1 <= 2 → true
    let mut arena = AstArena::new();
    let left = arena.add(AstNode::Value(ValueNode::Integer("1".to_string())));
    let right = arena.add(AstNode::Value(ValueNode::Integer("2".to_string())));
    let span = SourceSpan::new(0, 6);
    let bin_op = BinaryOpNode::new(BinaryOpKind::GeneralLe, left, right, span);
    let bin_id = arena.add(AstNode::BinaryOp(bin_op));
    let root = wrap_in_expr(&mut arena, bin_id);

    let result = bind_and_eval(&mut arena, root).unwrap();
    match result {
        XPathValue::Item(XmlItem::Atomic(v)) => {
            assert_eq!(v.as_boolean(), Some(true));
        }
        _ => panic!("Expected boolean true"),
    }

    // 2 > 1 → true
    let mut arena = AstArena::new();
    let left = arena.add(AstNode::Value(ValueNode::Integer("2".to_string())));
    let right = arena.add(AstNode::Value(ValueNode::Integer("1".to_string())));
    let span = SourceSpan::new(0, 5);
    let bin_op = BinaryOpNode::new(BinaryOpKind::GeneralGt, left, right, span);
    let bin_id = arena.add(AstNode::BinaryOp(bin_op));
    let root = wrap_in_expr(&mut arena, bin_id);

    let result = bind_and_eval(&mut arena, root).unwrap();
    match result {
        XPathValue::Item(XmlItem::Atomic(v)) => {
            assert_eq!(v.as_boolean(), Some(true));
        }
        _ => panic!("Expected boolean true"),
    }

    // 2 >= 2 → true
    let mut arena = AstArena::new();
    let left = arena.add(AstNode::Value(ValueNode::Integer("2".to_string())));
    let right = arena.add(AstNode::Value(ValueNode::Integer("2".to_string())));
    let span = SourceSpan::new(0, 6);
    let bin_op = BinaryOpNode::new(BinaryOpKind::GeneralGe, left, right, span);
    let bin_id = arena.add(AstNode::BinaryOp(bin_op));
    let root = wrap_in_expr(&mut arena, bin_id);

    let result = bind_and_eval(&mut arena, root).unwrap();
    match result {
        XPathValue::Item(XmlItem::Atomic(v)) => {
            assert_eq!(v.as_boolean(), Some(true));
        }
        _ => panic!("Expected boolean true"),
    }
}

#[test]
fn test_general_comparisons_empty() {
    // () = 1 → false
    let mut arena = AstArena::new();
    let left = arena.add(AstNode::Value(ValueNode::Empty));
    let right = arena.add(AstNode::Value(ValueNode::Integer("1".to_string())));
    let span = SourceSpan::new(0, 6);
    let bin_op = BinaryOpNode::new(BinaryOpKind::GeneralEq, left, right, span);
    let bin_id = arena.add(AstNode::BinaryOp(bin_op));
    let root = wrap_in_expr(&mut arena, bin_id);

    let result = bind_and_eval(&mut arena, root).unwrap();
    match result {
        XPathValue::Item(XmlItem::Atomic(v)) => {
            assert_eq!(v.as_boolean(), Some(false));
        }
        _ => panic!("Expected boolean false"),
    }

    // 1 = () → false
    let mut arena = AstArena::new();
    let left = arena.add(AstNode::Value(ValueNode::Integer("1".to_string())));
    let right = arena.add(AstNode::Value(ValueNode::Empty));
    let span = SourceSpan::new(0, 6);
    let bin_op = BinaryOpNode::new(BinaryOpKind::GeneralEq, left, right, span);
    let bin_id = arena.add(AstNode::BinaryOp(bin_op));
    let root = wrap_in_expr(&mut arena, bin_id);

    let result = bind_and_eval(&mut arena, root).unwrap();
    match result {
        XPathValue::Item(XmlItem::Atomic(v)) => {
            assert_eq!(v.as_boolean(), Some(false));
        }
        _ => panic!("Expected boolean false"),
    }
}

// ========================================================================
// Node Comparison Operator Tests
// ========================================================================

#[test]
fn test_node_is_same() {
    // Test that $node is $node → true
    // We use the context item for this test since we can set it up easily
    use crate::xpath::context::NameBinder;

    let doc = roxmltree::Document::parse("<root><a/><b/></root>").expect("parse xml");
    let mut nav = RoXmlNavigator::new(&doc);
    nav.move_to_first_child(); // root
    nav.move_to_first_child(); // a

    let names = NameTable::new();
    let ctx = XPathContext::new(&names);
    let mut binder = NameBinder::new();

    // Build AST for: . is .
    let mut arena = AstArena::new();
    let span = SourceSpan::new(0, 6);
    let left = arena.add(AstNode::ContextItem(
        crate::xpath::ast::ContextItemNode::new(span),
    ));
    let right = arena.add(AstNode::ContextItem(
        crate::xpath::ast::ContextItemNode::new(span),
    ));
    let bin_op = BinaryOpNode::new(BinaryOpKind::Is, left, right, span);
    let bin_id = arena.add(AstNode::BinaryOp(bin_op));
    let root = wrap_in_expr(&mut arena, bin_id);

    bind_node(&mut arena, root, &ctx, &mut binder).unwrap();

    let mut dyn_ctx =
        DynamicContext::new(&ctx, binder.len()).with_context_item(XmlItem::Node(nav.clone()));

    let result = eval_node(&arena, root, &mut dyn_ctx).unwrap();
    match result {
        XPathValue::Item(XmlItem::Atomic(v)) => {
            assert_eq!(v.as_boolean(), Some(true));
        }
        _ => panic!("Expected boolean true"),
    }
}

#[test]
fn test_node_is_empty() {
    // Test that () is $node → empty sequence
    use crate::xpath::context::NameBinder;

    let doc = roxmltree::Document::parse("<root/>").expect("parse xml");
    let mut nav = RoXmlNavigator::new(&doc);
    nav.move_to_first_child(); // root

    let names = NameTable::new();
    let ctx = XPathContext::new(&names);
    let mut binder = NameBinder::new();

    // Build AST for: () is .
    let mut arena = AstArena::new();
    let span = SourceSpan::new(0, 8);
    let left = arena.add(AstNode::Value(ValueNode::Empty));
    let right = arena.add(AstNode::ContextItem(
        crate::xpath::ast::ContextItemNode::new(span),
    ));
    let bin_op = BinaryOpNode::new(BinaryOpKind::Is, left, right, span);
    let bin_id = arena.add(AstNode::BinaryOp(bin_op));
    let root = wrap_in_expr(&mut arena, bin_id);

    bind_node(&mut arena, root, &ctx, &mut binder).unwrap();

    let mut dyn_ctx =
        DynamicContext::new(&ctx, binder.len()).with_context_item(XmlItem::Node(nav.clone()));

    let result = eval_node(&arena, root, &mut dyn_ctx).unwrap();
    assert!(result.is_empty());
}

#[test]
fn test_node_is_type_error() {
    // Test that 1 is $node → type error (XPTY0004)
    use crate::xpath::context::NameBinder;

    let doc = roxmltree::Document::parse("<root/>").expect("parse xml");
    let mut nav = RoXmlNavigator::new(&doc);
    nav.move_to_first_child(); // root

    let names = NameTable::new();
    let ctx = XPathContext::new(&names);
    let mut binder = NameBinder::new();

    // Build AST for: 1 is .
    let mut arena = AstArena::new();
    let span = SourceSpan::new(0, 6);
    let left = arena.add(AstNode::Value(ValueNode::Integer("1".to_string())));
    let right = arena.add(AstNode::ContextItem(
        crate::xpath::ast::ContextItemNode::new(span),
    ));
    let bin_op = BinaryOpNode::new(BinaryOpKind::Is, left, right, span);
    let bin_id = arena.add(AstNode::BinaryOp(bin_op));
    let root = wrap_in_expr(&mut arena, bin_id);

    bind_node(&mut arena, root, &ctx, &mut binder).unwrap();

    let mut dyn_ctx =
        DynamicContext::new(&ctx, binder.len()).with_context_item(XmlItem::Node(nav.clone()));

    let result = eval_node(&arena, root, &mut dyn_ctx);
    assert!(matches!(result, Err(XPathError::XPTY0004 { .. })));
}

#[test]
fn test_node_before_after() {
    // Test << and >> operators with nodes from a document
    // /root/a << /root/b → true (a comes before b)
    // /root/b >> /root/a → true (b comes after a)
    use crate::xpath::bind::bind_node;
    use crate::xpath::context::NameBinder;
    use crate::xpath::parser;

    let doc = roxmltree::Document::parse("<root><a/><b/></root>").expect("parse xml");
    let nav = RoXmlNavigator::new(&doc);

    // Helper to eval xpath with document context
    let eval_with_doc = |expr: &str| -> Result<XPathValue<RoXmlNavigator<'_>>, XPathError> {
        let mut parsed =
            parser::parse(expr).map_err(|e| XPathError::syntax_error(e.to_string()))?;

        let names = NameTable::new();
        let ctx = XPathContext::new(&names);
        let mut binder = NameBinder::new();

        bind_node(&mut parsed.arena, parsed.root, &ctx, &mut binder)?;

        let mut dyn_ctx =
            DynamicContext::new(&ctx, binder.len()).with_context_item(XmlItem::Node(nav.clone()));

        eval_node(&parsed.arena, parsed.root, &mut dyn_ctx)
    };

    // Test: /root/a << /root/b → true (a comes before b in document order)
    let result = eval_with_doc("/root/a << /root/b").unwrap();
    assert_eq!(result.as_bool(), Some(true), "a << b should be true");

    // Test: /root/b >> /root/a → true (b comes after a in document order)
    let result = eval_with_doc("/root/b >> /root/a").unwrap();
    assert_eq!(result.as_bool(), Some(true), "b >> a should be true");

    // Test: /root/a >> /root/b → false (a does not come after b)
    let result = eval_with_doc("/root/a >> /root/b").unwrap();
    assert_eq!(result.as_bool(), Some(false), "a >> b should be false");

    // Test: /root/b << /root/a → false (b does not come before a)
    let result = eval_with_doc("/root/b << /root/a").unwrap();
    assert_eq!(result.as_bool(), Some(false), "b << a should be false");
}

// ========================================================================
// Sequence Operator Tests (Union, Intersect, Except)
// ========================================================================

#[test]
fn test_union_operator_with_atomic_values() {
    // Test union with atomic values (should fail with XPTY0004)
    // (1, 2) | (3, 4) → type error
    let mut arena = AstArena::new();
    let v1 = arena.add(AstNode::Value(ValueNode::Integer("1".to_string())));
    let v2 = arena.add(AstNode::Value(ValueNode::Integer("2".to_string())));
    let v3 = arena.add(AstNode::Value(ValueNode::Integer("3".to_string())));
    let v4 = arena.add(AstNode::Value(ValueNode::Integer("4".to_string())));
    let span = SourceSpan::new(0, 15);
    let left_seq = ExprNode::sequence(vec![v1, v2], span);
    let left = arena.add(AstNode::Expr(left_seq));
    let right_seq = ExprNode::sequence(vec![v3, v4], span);
    let right = arena.add(AstNode::Expr(right_seq));
    let bin_op = BinaryOpNode::new(BinaryOpKind::Union, left, right, span);
    let bin_id = arena.add(AstNode::BinaryOp(bin_op));
    let root = wrap_in_expr(&mut arena, bin_id);

    let result = bind_and_eval(&mut arena, root);
    assert!(result.is_err());
}

#[test]
fn test_intersect_operator_with_atomic_values() {
    // Test intersect with atomic values (should fail with XPTY0004)
    // (1, 2) intersect (2, 3) → type error
    let mut arena = AstArena::new();
    let v1 = arena.add(AstNode::Value(ValueNode::Integer("1".to_string())));
    let v2 = arena.add(AstNode::Value(ValueNode::Integer("2".to_string())));
    let v3 = arena.add(AstNode::Value(ValueNode::Integer("2".to_string())));
    let v4 = arena.add(AstNode::Value(ValueNode::Integer("3".to_string())));
    let span = SourceSpan::new(0, 20);
    let left_seq = ExprNode::sequence(vec![v1, v2], span);
    let left = arena.add(AstNode::Expr(left_seq));
    let right_seq = ExprNode::sequence(vec![v3, v4], span);
    let right = arena.add(AstNode::Expr(right_seq));
    let bin_op = BinaryOpNode::new(BinaryOpKind::Intersect, left, right, span);
    let bin_id = arena.add(AstNode::BinaryOp(bin_op));
    let root = wrap_in_expr(&mut arena, bin_id);

    let result = bind_and_eval(&mut arena, root);
    assert!(result.is_err());
}

#[test]
fn test_except_operator_with_atomic_values() {
    // Test except with atomic values (should fail with XPTY0004)
    // (1, 2) except (2, 3) → type error
    let mut arena = AstArena::new();
    let v1 = arena.add(AstNode::Value(ValueNode::Integer("1".to_string())));
    let v2 = arena.add(AstNode::Value(ValueNode::Integer("2".to_string())));
    let v3 = arena.add(AstNode::Value(ValueNode::Integer("2".to_string())));
    let v4 = arena.add(AstNode::Value(ValueNode::Integer("3".to_string())));
    let span = SourceSpan::new(0, 20);
    let left_seq = ExprNode::sequence(vec![v1, v2], span);
    let left = arena.add(AstNode::Expr(left_seq));
    let right_seq = ExprNode::sequence(vec![v3, v4], span);
    let right = arena.add(AstNode::Expr(right_seq));
    let bin_op = BinaryOpNode::new(BinaryOpKind::Except, left, right, span);
    let bin_id = arena.add(AstNode::BinaryOp(bin_op));
    let root = wrap_in_expr(&mut arena, bin_id);

    let result = bind_and_eval(&mut arena, root);
    assert!(result.is_err());
}

#[test]
fn test_union_operator_with_empty_sequences() {
    // Test union with empty sequences
    // () | () → ()
    let mut arena = AstArena::new();
    let left = arena.add(AstNode::Value(ValueNode::Empty));
    let right = arena.add(AstNode::Value(ValueNode::Empty));
    let span = SourceSpan::new(0, 6);
    let bin_op = BinaryOpNode::new(BinaryOpKind::Union, left, right, span);
    let bin_id = arena.add(AstNode::BinaryOp(bin_op));
    let root = wrap_in_expr(&mut arena, bin_id);

    let result = bind_and_eval(&mut arena, root).unwrap();
    assert!(result.is_empty());
}

#[test]
fn test_union_operator_with_nodes() {
    // Test union with actual nodes
    use crate::xpath::context::NameBinder;

    let doc = roxmltree::Document::parse("<root><a/><b/></root>").expect("parse xml");
    let mut nav = RoXmlNavigator::new(&doc);
    nav.move_to_first_child(); // root
    nav.move_to_first_child(); // a

    let names = NameTable::new();
    let ctx = XPathContext::new(&names);
    let mut binder = NameBinder::new();

    // Build AST for: . | .
    // Same node union should return just one node
    let mut arena = AstArena::new();
    let span = SourceSpan::new(0, 6);
    let left = arena.add(AstNode::ContextItem(
        crate::xpath::ast::ContextItemNode::new(span),
    ));
    let right = arena.add(AstNode::ContextItem(
        crate::xpath::ast::ContextItemNode::new(span),
    ));
    let bin_op = BinaryOpNode::new(BinaryOpKind::Union, left, right, span);
    let bin_id = arena.add(AstNode::BinaryOp(bin_op));
    let root = wrap_in_expr(&mut arena, bin_id);

    bind_node(&mut arena, root, &ctx, &mut binder).unwrap();

    let mut dyn_ctx =
        DynamicContext::new(&ctx, binder.len()).with_context_item(XmlItem::Node(nav.clone()));

    let result = eval_node(&arena, root, &mut dyn_ctx).unwrap();
    // Union of same node with itself should give 1 node (deduplicated)
    assert_eq!(result.into_vec().len(), 1);
}

#[test]
fn test_intersect_operator_with_nodes() {
    // Test intersect with actual nodes
    use crate::xpath::context::NameBinder;

    let doc = roxmltree::Document::parse("<root><a/></root>").expect("parse xml");
    let mut nav = RoXmlNavigator::new(&doc);
    nav.move_to_first_child(); // root
    nav.move_to_first_child(); // a

    let names = NameTable::new();
    let ctx = XPathContext::new(&names);
    let mut binder = NameBinder::new();

    // Build AST for: . intersect .
    // Same node intersect should return that node
    let mut arena = AstArena::new();
    let span = SourceSpan::new(0, 15);
    let left = arena.add(AstNode::ContextItem(
        crate::xpath::ast::ContextItemNode::new(span),
    ));
    let right = arena.add(AstNode::ContextItem(
        crate::xpath::ast::ContextItemNode::new(span),
    ));
    let bin_op = BinaryOpNode::new(BinaryOpKind::Intersect, left, right, span);
    let bin_id = arena.add(AstNode::BinaryOp(bin_op));
    let root = wrap_in_expr(&mut arena, bin_id);

    bind_node(&mut arena, root, &ctx, &mut binder).unwrap();

    let mut dyn_ctx =
        DynamicContext::new(&ctx, binder.len()).with_context_item(XmlItem::Node(nav.clone()));

    let result = eval_node(&arena, root, &mut dyn_ctx).unwrap();
    // Intersect of same node with itself should give 1 node
    assert_eq!(result.into_vec().len(), 1);
}

#[test]
fn test_except_operator_with_nodes() {
    // Test except with actual nodes
    use crate::xpath::context::NameBinder;

    let doc = roxmltree::Document::parse("<root><a/></root>").expect("parse xml");
    let mut nav = RoXmlNavigator::new(&doc);
    nav.move_to_first_child(); // root
    nav.move_to_first_child(); // a

    let names = NameTable::new();
    let ctx = XPathContext::new(&names);
    let mut binder = NameBinder::new();

    // Build AST for: . except .
    // Same node except should return empty (node minus itself = empty)
    let mut arena = AstArena::new();
    let span = SourceSpan::new(0, 12);
    let left = arena.add(AstNode::ContextItem(
        crate::xpath::ast::ContextItemNode::new(span),
    ));
    let right = arena.add(AstNode::ContextItem(
        crate::xpath::ast::ContextItemNode::new(span),
    ));
    let bin_op = BinaryOpNode::new(BinaryOpKind::Except, left, right, span);
    let bin_id = arena.add(AstNode::BinaryOp(bin_op));
    let root = wrap_in_expr(&mut arena, bin_id);

    bind_node(&mut arena, root, &ctx, &mut binder).unwrap();

    let mut dyn_ctx =
        DynamicContext::new(&ctx, binder.len()).with_context_item(XmlItem::Node(nav.clone()));

    let result = eval_node(&arena, root, &mut dyn_ctx).unwrap();
    // Except of same node with itself should give empty sequence
    assert!(result.is_empty());
}

// ========================================================================
// Range Expression Tests
// ========================================================================

#[test]
fn test_range_basic() {
    // 1 to 5 -> (1, 2, 3, 4, 5)
    let mut arena = AstArena::new();
    let start = arena.add(AstNode::Value(ValueNode::Integer("1".to_string())));
    let end = arena.add(AstNode::Value(ValueNode::Integer("5".to_string())));
    let span = SourceSpan::new(0, 6);
    let range = RangeNode::new(start, end, span);
    let range_id = arena.add(AstNode::Range(range));
    let root = wrap_in_expr(&mut arena, range_id);

    let result = bind_and_eval(&mut arena, root).unwrap();
    let items = result.into_vec();
    assert_eq!(items.len(), 5);
    // Verify values are 1, 2, 3, 4, 5
    for (i, item) in items.iter().enumerate() {
        match item {
            XmlItem::Atomic(v) => {
                assert_eq!(
                    v.as_integer().map(|x| x.to_string()),
                    Some((i + 1).to_string())
                );
            }
            _ => panic!("Expected atomic integer"),
        }
    }
}

#[test]
fn test_range_empty() {
    // 5 to 3 -> () (empty sequence when start > end)
    let mut arena = AstArena::new();
    let start = arena.add(AstNode::Value(ValueNode::Integer("5".to_string())));
    let end = arena.add(AstNode::Value(ValueNode::Integer("3".to_string())));
    let span = SourceSpan::new(0, 6);
    let range = RangeNode::new(start, end, span);
    let range_id = arena.add(AstNode::Range(range));
    let root = wrap_in_expr(&mut arena, range_id);

    let result = bind_and_eval(&mut arena, root).unwrap();
    assert!(result.is_empty());
}

#[test]
fn test_range_single() {
    // 5 to 5 -> (5)
    let mut arena = AstArena::new();
    let start = arena.add(AstNode::Value(ValueNode::Integer("5".to_string())));
    let end = arena.add(AstNode::Value(ValueNode::Integer("5".to_string())));
    let span = SourceSpan::new(0, 6);
    let range = RangeNode::new(start, end, span);
    let range_id = arena.add(AstNode::Range(range));
    let root = wrap_in_expr(&mut arena, range_id);

    let result = bind_and_eval(&mut arena, root).unwrap();
    let items = result.into_vec();
    assert_eq!(items.len(), 1);
    match &items[0] {
        XmlItem::Atomic(v) => {
            assert_eq!(v.as_integer().map(|x| x.to_string()), Some("5".to_string()));
        }
        _ => panic!("Expected atomic integer"),
    }
}

#[test]
fn test_range_empty_start_operand() {
    // () to 5 -> ()
    let mut arena = AstArena::new();
    let start = arena.add(AstNode::Value(ValueNode::Empty));
    let end = arena.add(AstNode::Value(ValueNode::Integer("5".to_string())));
    let span = SourceSpan::new(0, 7);
    let range = RangeNode::new(start, end, span);
    let range_id = arena.add(AstNode::Range(range));
    let root = wrap_in_expr(&mut arena, range_id);

    let result = bind_and_eval(&mut arena, root).unwrap();
    assert!(result.is_empty());
}

#[test]
fn test_range_empty_end_operand() {
    // 1 to () -> ()
    let mut arena = AstArena::new();
    let start = arena.add(AstNode::Value(ValueNode::Integer("1".to_string())));
    let end = arena.add(AstNode::Value(ValueNode::Empty));
    let span = SourceSpan::new(0, 7);
    let range = RangeNode::new(start, end, span);
    let range_id = arena.add(AstNode::Range(range));
    let root = wrap_in_expr(&mut arena, range_id);

    let result = bind_and_eval(&mut arena, root).unwrap();
    assert!(result.is_empty());
}

// ========================================================================
// For Expression Tests
// ========================================================================

/// Helper to create a for expression with bindings and return expr
fn make_for_expr(
    arena: &mut AstArena,
    names: &NameTable,
    var_names: &[&str],
    in_exprs: Vec<AstNodeId>,
    return_expr: AstNodeId,
) -> AstNodeId {
    use crate::xpath::ast::ForBinding;

    let span = SourceSpan::new(0, 50);
    let bindings: Vec<ForBinding> = var_names
        .iter()
        .zip(in_exprs)
        .map(|(name, in_expr)| {
            let _ = names.add(name); // Ensure name is in table
            ForBinding::new(String::new(), name.to_string(), in_expr, span)
        })
        .collect();

    let for_node = crate::xpath::ast::ForNode::new(bindings, return_expr, span);
    arena.add(AstNode::For(for_node))
}

/// Helper to create a sequence of integers
fn make_int_sequence(arena: &mut AstArena, values: &[i64]) -> AstNodeId {
    let span = SourceSpan::new(0, 10);
    let items: Vec<AstNodeId> = values
        .iter()
        .map(|v| arena.add(AstNode::Value(ValueNode::Integer(v.to_string()))))
        .collect();
    let expr = ExprNode::sequence(items, span);
    arena.add(AstNode::Expr(expr))
}

/// Helper to create a variable reference
fn make_var_ref(arena: &mut AstArena, name: &str) -> AstNodeId {
    use crate::xpath::ast::VarRefNode;
    let span = SourceSpan::new(0, 5);
    let var_ref = VarRefNode::new(String::new(), name.to_string(), span);
    arena.add(AstNode::VarRef(var_ref))
}

#[test]
fn test_for_single_binding() {
    // for $i in (1, 2, 3) return $i
    // Expected: (1, 2, 3)
    let names = NameTable::new();
    let ctx = XPathContext::new(&names);
    let mut binder = NameBinder::new();

    let mut arena = AstArena::new();
    let seq = make_int_sequence(&mut arena, &[1, 2, 3]);
    let var_ref = make_var_ref(&mut arena, "i");
    let for_id = make_for_expr(&mut arena, &names, &["i"], vec![seq], var_ref);
    let root = wrap_in_expr(&mut arena, for_id);

    bind_node(&mut arena, root, &ctx, &mut binder).unwrap();
    let mut dyn_ctx: DynamicContext<'_, RoXmlNavigator<'static>> =
        DynamicContext::new(&ctx, binder.len());

    let result = eval_node(&arena, root, &mut dyn_ctx).unwrap();
    let items = result.into_vec();
    assert_eq!(items.len(), 3);

    // Verify values are 1, 2, 3
    for (i, item) in items.iter().enumerate() {
        match item {
            XmlItem::Atomic(v) => {
                assert_eq!(
                    v.as_integer().map(|x| x.to_string()),
                    Some((i as i64 + 1).to_string())
                );
            }
            _ => panic!("Expected atomic integer"),
        }
    }
}

#[test]
fn test_for_empty_sequence() {
    // for $i in () return $i
    // Expected: ()
    let names = NameTable::new();
    let ctx = XPathContext::new(&names);
    let mut binder = NameBinder::new();

    let mut arena = AstArena::new();
    let empty = arena.add(AstNode::Value(ValueNode::Empty));
    let var_ref = make_var_ref(&mut arena, "i");
    let for_id = make_for_expr(&mut arena, &names, &["i"], vec![empty], var_ref);
    let root = wrap_in_expr(&mut arena, for_id);

    bind_node(&mut arena, root, &ctx, &mut binder).unwrap();
    let mut dyn_ctx: DynamicContext<'_, RoXmlNavigator<'static>> =
        DynamicContext::new(&ctx, binder.len());

    let result = eval_node(&arena, root, &mut dyn_ctx).unwrap();
    assert!(result.is_empty());
}

#[test]
fn test_for_multiple_bindings() {
    // for $i in (1, 2), $j in (10, 20) return $i + $j
    // Expected: (11, 21, 12, 22) - Cartesian product order
    let names = NameTable::new();
    let ctx = XPathContext::new(&names);
    let mut binder = NameBinder::new();

    let mut arena = AstArena::new();
    let seq1 = make_int_sequence(&mut arena, &[1, 2]);
    let seq2 = make_int_sequence(&mut arena, &[10, 20]);
    let var_i = make_var_ref(&mut arena, "i");
    let var_j = make_var_ref(&mut arena, "j");
    let span = SourceSpan::new(0, 10);
    let add = BinaryOpNode::new(BinaryOpKind::Add, var_i, var_j, span);
    let add_id = arena.add(AstNode::BinaryOp(add));
    let for_id = make_for_expr(&mut arena, &names, &["i", "j"], vec![seq1, seq2], add_id);
    let root = wrap_in_expr(&mut arena, for_id);

    bind_node(&mut arena, root, &ctx, &mut binder).unwrap();
    let mut dyn_ctx: DynamicContext<'_, RoXmlNavigator<'static>> =
        DynamicContext::new(&ctx, binder.len());

    let result = eval_node(&arena, root, &mut dyn_ctx).unwrap();
    let items = result.into_vec();
    assert_eq!(items.len(), 4);

    // Verify values: 1+10=11, 1+20=21, 2+10=12, 2+20=22
    let expected = [11i64, 21, 12, 22];
    for (i, item) in items.iter().enumerate() {
        match item {
            XmlItem::Atomic(v) => {
                assert_eq!(
                    v.as_integer().map(|x| x.to_string()),
                    Some(expected[i].to_string())
                );
            }
            _ => panic!("Expected atomic integer"),
        }
    }
}

#[test]
fn test_for_return_empty() {
    // for $i in (1, 2, 3) return ()
    // Expected: ()
    let names = NameTable::new();
    let ctx = XPathContext::new(&names);
    let mut binder = NameBinder::new();

    let mut arena = AstArena::new();
    let seq = make_int_sequence(&mut arena, &[1, 2, 3]);
    let empty = arena.add(AstNode::Value(ValueNode::Empty));
    let for_id = make_for_expr(&mut arena, &names, &["i"], vec![seq], empty);
    let root = wrap_in_expr(&mut arena, for_id);

    bind_node(&mut arena, root, &ctx, &mut binder).unwrap();
    let mut dyn_ctx: DynamicContext<'_, RoXmlNavigator<'static>> =
        DynamicContext::new(&ctx, binder.len());

    let result = eval_node(&arena, root, &mut dyn_ctx).unwrap();
    assert!(result.is_empty());
}

// ========================================================================
// Quantified Expression Tests
// ========================================================================

/// Helper to create a quantified expression
fn make_quantified_expr(
    arena: &mut AstArena,
    names: &NameTable,
    kind: QuantifierKind,
    var_names: &[&str],
    in_exprs: Vec<AstNodeId>,
    satisfies: AstNodeId,
) -> AstNodeId {
    use crate::xpath::ast::ForBinding;

    let span = SourceSpan::new(0, 50);
    let bindings: Vec<ForBinding> = var_names
        .iter()
        .zip(in_exprs)
        .map(|(name, in_expr)| {
            let _ = names.add(name); // Ensure name is in table
            ForBinding::new(String::new(), name.to_string(), in_expr, span)
        })
        .collect();

    let quant_node = crate::xpath::ast::QuantifiedNode::new(kind, bindings, satisfies, span);
    arena.add(AstNode::Quantified(quant_node))
}

#[test]
fn test_some_true() {
    // some $x in (1, 2, 3) satisfies $x > 2
    // Expected: true (3 > 2)
    let names = NameTable::new();
    let ctx = XPathContext::new(&names);
    let mut binder = NameBinder::new();

    let mut arena = AstArena::new();
    let seq = make_int_sequence(&mut arena, &[1, 2, 3]);
    let var_x = make_var_ref(&mut arena, "x");
    let two = arena.add(AstNode::Value(ValueNode::Integer("2".to_string())));
    let span = SourceSpan::new(0, 10);
    let gt = BinaryOpNode::new(BinaryOpKind::GeneralGt, var_x, two, span);
    let gt_id = arena.add(AstNode::BinaryOp(gt));
    let quant_id = make_quantified_expr(
        &mut arena,
        &names,
        QuantifierKind::Some,
        &["x"],
        vec![seq],
        gt_id,
    );
    let root = wrap_in_expr(&mut arena, quant_id);

    bind_node(&mut arena, root, &ctx, &mut binder).unwrap();
    let mut dyn_ctx: DynamicContext<'_, RoXmlNavigator<'static>> =
        DynamicContext::new(&ctx, binder.len());

    let result = eval_node(&arena, root, &mut dyn_ctx).unwrap();
    match result {
        XPathValue::Item(XmlItem::Atomic(v)) => {
            assert_eq!(v.as_boolean(), Some(true));
        }
        _ => panic!("Expected boolean true"),
    }
}

#[test]
fn test_some_false() {
    // some $x in (1, 2, 3) satisfies $x > 5
    // Expected: false
    let names = NameTable::new();
    let ctx = XPathContext::new(&names);
    let mut binder = NameBinder::new();

    let mut arena = AstArena::new();
    let seq = make_int_sequence(&mut arena, &[1, 2, 3]);
    let var_x = make_var_ref(&mut arena, "x");
    let five = arena.add(AstNode::Value(ValueNode::Integer("5".to_string())));
    let span = SourceSpan::new(0, 10);
    let gt = BinaryOpNode::new(BinaryOpKind::GeneralGt, var_x, five, span);
    let gt_id = arena.add(AstNode::BinaryOp(gt));
    let quant_id = make_quantified_expr(
        &mut arena,
        &names,
        QuantifierKind::Some,
        &["x"],
        vec![seq],
        gt_id,
    );
    let root = wrap_in_expr(&mut arena, quant_id);

    bind_node(&mut arena, root, &ctx, &mut binder).unwrap();
    let mut dyn_ctx: DynamicContext<'_, RoXmlNavigator<'static>> =
        DynamicContext::new(&ctx, binder.len());

    let result = eval_node(&arena, root, &mut dyn_ctx).unwrap();
    match result {
        XPathValue::Item(XmlItem::Atomic(v)) => {
            assert_eq!(v.as_boolean(), Some(false));
        }
        _ => panic!("Expected boolean false"),
    }
}

#[test]
fn test_some_empty_sequence() {
    // some $x in () satisfies $x > 0
    // Expected: false (no items to test)
    let names = NameTable::new();
    let ctx = XPathContext::new(&names);
    let mut binder = NameBinder::new();

    let mut arena = AstArena::new();
    let empty = arena.add(AstNode::Value(ValueNode::Empty));
    let var_x = make_var_ref(&mut arena, "x");
    let zero = arena.add(AstNode::Value(ValueNode::Integer("0".to_string())));
    let span = SourceSpan::new(0, 10);
    let gt = BinaryOpNode::new(BinaryOpKind::GeneralGt, var_x, zero, span);
    let gt_id = arena.add(AstNode::BinaryOp(gt));
    let quant_id = make_quantified_expr(
        &mut arena,
        &names,
        QuantifierKind::Some,
        &["x"],
        vec![empty],
        gt_id,
    );
    let root = wrap_in_expr(&mut arena, quant_id);

    bind_node(&mut arena, root, &ctx, &mut binder).unwrap();
    let mut dyn_ctx: DynamicContext<'_, RoXmlNavigator<'static>> =
        DynamicContext::new(&ctx, binder.len());

    let result = eval_node(&arena, root, &mut dyn_ctx).unwrap();
    match result {
        XPathValue::Item(XmlItem::Atomic(v)) => {
            assert_eq!(v.as_boolean(), Some(false));
        }
        _ => panic!("Expected boolean false"),
    }
}

#[test]
fn test_every_true() {
    // every $x in (1, 2, 3) satisfies $x > 0
    // Expected: true
    let names = NameTable::new();
    let ctx = XPathContext::new(&names);
    let mut binder = NameBinder::new();

    let mut arena = AstArena::new();
    let seq = make_int_sequence(&mut arena, &[1, 2, 3]);
    let var_x = make_var_ref(&mut arena, "x");
    let zero = arena.add(AstNode::Value(ValueNode::Integer("0".to_string())));
    let span = SourceSpan::new(0, 10);
    let gt = BinaryOpNode::new(BinaryOpKind::GeneralGt, var_x, zero, span);
    let gt_id = arena.add(AstNode::BinaryOp(gt));
    let quant_id = make_quantified_expr(
        &mut arena,
        &names,
        QuantifierKind::Every,
        &["x"],
        vec![seq],
        gt_id,
    );
    let root = wrap_in_expr(&mut arena, quant_id);

    bind_node(&mut arena, root, &ctx, &mut binder).unwrap();
    let mut dyn_ctx: DynamicContext<'_, RoXmlNavigator<'static>> =
        DynamicContext::new(&ctx, binder.len());

    let result = eval_node(&arena, root, &mut dyn_ctx).unwrap();
    match result {
        XPathValue::Item(XmlItem::Atomic(v)) => {
            assert_eq!(v.as_boolean(), Some(true));
        }
        _ => panic!("Expected boolean true"),
    }
}

#[test]
fn test_every_false() {
    // every $x in (1, 2, 3) satisfies $x > 2
    // Expected: false (1 and 2 are not > 2)
    let names = NameTable::new();
    let ctx = XPathContext::new(&names);
    let mut binder = NameBinder::new();

    let mut arena = AstArena::new();
    let seq = make_int_sequence(&mut arena, &[1, 2, 3]);
    let var_x = make_var_ref(&mut arena, "x");
    let two = arena.add(AstNode::Value(ValueNode::Integer("2".to_string())));
    let span = SourceSpan::new(0, 10);
    let gt = BinaryOpNode::new(BinaryOpKind::GeneralGt, var_x, two, span);
    let gt_id = arena.add(AstNode::BinaryOp(gt));
    let quant_id = make_quantified_expr(
        &mut arena,
        &names,
        QuantifierKind::Every,
        &["x"],
        vec![seq],
        gt_id,
    );
    let root = wrap_in_expr(&mut arena, quant_id);

    bind_node(&mut arena, root, &ctx, &mut binder).unwrap();
    let mut dyn_ctx: DynamicContext<'_, RoXmlNavigator<'static>> =
        DynamicContext::new(&ctx, binder.len());

    let result = eval_node(&arena, root, &mut dyn_ctx).unwrap();
    match result {
        XPathValue::Item(XmlItem::Atomic(v)) => {
            assert_eq!(v.as_boolean(), Some(false));
        }
        _ => panic!("Expected boolean false"),
    }
}

#[test]
fn test_every_empty_vacuous_truth() {
    // every $x in () satisfies $x > 0
    // Expected: true (vacuous truth)
    let names = NameTable::new();
    let ctx = XPathContext::new(&names);
    let mut binder = NameBinder::new();

    let mut arena = AstArena::new();
    let empty = arena.add(AstNode::Value(ValueNode::Empty));
    let var_x = make_var_ref(&mut arena, "x");
    let zero = arena.add(AstNode::Value(ValueNode::Integer("0".to_string())));
    let span = SourceSpan::new(0, 10);
    let gt = BinaryOpNode::new(BinaryOpKind::GeneralGt, var_x, zero, span);
    let gt_id = arena.add(AstNode::BinaryOp(gt));
    let quant_id = make_quantified_expr(
        &mut arena,
        &names,
        QuantifierKind::Every,
        &["x"],
        vec![empty],
        gt_id,
    );
    let root = wrap_in_expr(&mut arena, quant_id);

    bind_node(&mut arena, root, &ctx, &mut binder).unwrap();
    let mut dyn_ctx: DynamicContext<'_, RoXmlNavigator<'static>> =
        DynamicContext::new(&ctx, binder.len());

    let result = eval_node(&arena, root, &mut dyn_ctx).unwrap();
    match result {
        XPathValue::Item(XmlItem::Atomic(v)) => {
            assert_eq!(v.as_boolean(), Some(true));
        }
        _ => panic!("Expected boolean true (vacuous truth)"),
    }
}

#[test]
fn test_some_multiple_bindings() {
    // some $x in (1, 2), $y in (3, 4) satisfies $x + $y = 5
    // Expected: true (1+4=5 or 2+3=5)
    let names = NameTable::new();
    let ctx = XPathContext::new(&names);
    let mut binder = NameBinder::new();

    let mut arena = AstArena::new();
    let seq1 = make_int_sequence(&mut arena, &[1, 2]);
    let seq2 = make_int_sequence(&mut arena, &[3, 4]);
    let var_x = make_var_ref(&mut arena, "x");
    let var_y = make_var_ref(&mut arena, "y");
    let five = arena.add(AstNode::Value(ValueNode::Integer("5".to_string())));
    let span = SourceSpan::new(0, 20);
    let add = BinaryOpNode::new(BinaryOpKind::Add, var_x, var_y, span);
    let add_id = arena.add(AstNode::BinaryOp(add));
    let eq = BinaryOpNode::new(BinaryOpKind::GeneralEq, add_id, five, span);
    let eq_id = arena.add(AstNode::BinaryOp(eq));
    let quant_id = make_quantified_expr(
        &mut arena,
        &names,
        QuantifierKind::Some,
        &["x", "y"],
        vec![seq1, seq2],
        eq_id,
    );
    let root = wrap_in_expr(&mut arena, quant_id);

    bind_node(&mut arena, root, &ctx, &mut binder).unwrap();
    let mut dyn_ctx: DynamicContext<'_, RoXmlNavigator<'static>> =
        DynamicContext::new(&ctx, binder.len());

    let result = eval_node(&arena, root, &mut dyn_ctx).unwrap();
    match result {
        XPathValue::Item(XmlItem::Atomic(v)) => {
            assert_eq!(v.as_boolean(), Some(true));
        }
        _ => panic!("Expected boolean true"),
    }
}

// ========================================================================
// Dependent Binding Tests (for/quantified with dependent bindings)
// ========================================================================

/// Helper to create a for expression with dependent bindings.
/// The second binding's in_expr is a function of the first binding's variable.
fn make_dependent_for_expr(
    arena: &mut AstArena,
    names: &NameTable,
    var1_name: &str,
    in_expr1: AstNodeId,
    var2_name: &str,
    in_expr2: AstNodeId, // Can reference var1
    return_expr: AstNodeId,
) -> AstNodeId {
    use crate::xpath::ast::ForBinding;

    let span = SourceSpan::new(0, 50);
    let _ = names.add(var1_name);
    let _ = names.add(var2_name);

    let bindings = vec![
        ForBinding::new(String::new(), var1_name.to_string(), in_expr1, span),
        ForBinding::new(String::new(), var2_name.to_string(), in_expr2, span),
    ];

    let for_node = crate::xpath::ast::ForNode::new(bindings, return_expr, span);
    arena.add(AstNode::For(for_node))
}

#[test]
fn test_for_dependent_bindings() {
    // for $x in (1, 2, 3), $y in ($x + 1) return $y
    // Expected: (2, 3, 4) - $y depends on $x
    let names = NameTable::new();
    let ctx = XPathContext::new(&names);
    let mut binder = NameBinder::new();

    let mut arena = AstArena::new();
    let seq1 = make_int_sequence(&mut arena, &[1, 2, 3]);

    // $y in ($x + 1) - create addition expression
    let var_x_in_expr = make_var_ref(&mut arena, "x");
    let one = arena.add(AstNode::Value(ValueNode::Integer("1".to_string())));
    let span = SourceSpan::new(0, 10);
    let add = BinaryOpNode::new(BinaryOpKind::Add, var_x_in_expr, one, span);
    let add_id = arena.add(AstNode::BinaryOp(add));

    // return $y
    let var_y = make_var_ref(&mut arena, "y");

    let for_id = make_dependent_for_expr(&mut arena, &names, "x", seq1, "y", add_id, var_y);
    let root = wrap_in_expr(&mut arena, for_id);

    bind_node(&mut arena, root, &ctx, &mut binder).unwrap();
    let mut dyn_ctx: DynamicContext<'_, RoXmlNavigator<'static>> =
        DynamicContext::new(&ctx, binder.len());

    let result = eval_node(&arena, root, &mut dyn_ctx).unwrap();
    let items = result.into_vec();
    assert_eq!(items.len(), 3);

    // Verify values are 2, 3, 4 (corresponding to $x+1 for $x in 1,2,3)
    let expected = [2i64, 3, 4];
    for (i, item) in items.iter().enumerate() {
        match item {
            XmlItem::Atomic(v) => {
                assert_eq!(
                    v.as_integer().map(|x| x.to_string()),
                    Some(expected[i].to_string()),
                    "Expected {} at index {}",
                    expected[i],
                    i
                );
            }
            _ => panic!("Expected atomic integer at index {}", i),
        }
    }
}

#[test]
fn test_for_dependent_range_binding() {
    // for $x in (1 to 3), $y in (1 to $x) return $y
    // When $x=1: $y in (1) -> 1
    // When $x=2: $y in (1,2) -> 1, 2
    // When $x=3: $y in (1,2,3) -> 1, 2, 3
    // Expected: (1, 1, 2, 1, 2, 3)
    let names = NameTable::new();
    let ctx = XPathContext::new(&names);
    let mut binder = NameBinder::new();

    let mut arena = AstArena::new();

    // $x in (1 to 3)
    let one_a = arena.add(AstNode::Value(ValueNode::Integer("1".to_string())));
    let three = arena.add(AstNode::Value(ValueNode::Integer("3".to_string())));
    let span = SourceSpan::new(0, 10);
    let range1 = RangeNode::new(one_a, three, span);
    let range1_id = arena.add(AstNode::Range(range1));

    // $y in (1 to $x) - range depends on $x
    let one_b = arena.add(AstNode::Value(ValueNode::Integer("1".to_string())));
    let var_x_in_expr = make_var_ref(&mut arena, "x");
    let range2 = RangeNode::new(one_b, var_x_in_expr, span);
    let range2_id = arena.add(AstNode::Range(range2));

    // return $y
    let var_y = make_var_ref(&mut arena, "y");

    let for_id = make_dependent_for_expr(&mut arena, &names, "x", range1_id, "y", range2_id, var_y);
    let root = wrap_in_expr(&mut arena, for_id);

    bind_node(&mut arena, root, &ctx, &mut binder).unwrap();
    let mut dyn_ctx: DynamicContext<'_, RoXmlNavigator<'static>> =
        DynamicContext::new(&ctx, binder.len());

    let result = eval_node(&arena, root, &mut dyn_ctx).unwrap();
    let items = result.into_vec();

    // Expected: (1, 1, 2, 1, 2, 3)
    let expected = [1i64, 1, 2, 1, 2, 3];
    assert_eq!(items.len(), expected.len());

    for (i, item) in items.iter().enumerate() {
        match item {
            XmlItem::Atomic(v) => {
                assert_eq!(
                    v.as_integer().map(|x| x.to_string()),
                    Some(expected[i].to_string()),
                    "Expected {} at index {}",
                    expected[i],
                    i
                );
            }
            _ => panic!("Expected atomic integer at index {}", i),
        }
    }
}

#[test]
fn test_some_dependent_bindings() {
    // some $x in (1, 2), $y in ($x * 2) satisfies $y > 3
    // When $x=1: $y=2, 2>3 is false
    // When $x=2: $y=4, 4>3 is true -> short-circuit, return true
    // Expected: true
    let names = NameTable::new();
    let ctx = XPathContext::new(&names);
    let mut binder = NameBinder::new();

    let mut arena = AstArena::new();
    let seq = make_int_sequence(&mut arena, &[1, 2]);

    // $y in ($x * 2)
    let var_x_in_expr = make_var_ref(&mut arena, "x");
    let two = arena.add(AstNode::Value(ValueNode::Integer("2".to_string())));
    let span = SourceSpan::new(0, 10);
    let mul = BinaryOpNode::new(BinaryOpKind::Mul, var_x_in_expr, two, span);
    let mul_id = arena.add(AstNode::BinaryOp(mul));

    // satisfies $y > 3
    let var_y = make_var_ref(&mut arena, "y");
    let three = arena.add(AstNode::Value(ValueNode::Integer("3".to_string())));
    let gt = BinaryOpNode::new(BinaryOpKind::GeneralGt, var_y, three, span);
    let gt_id = arena.add(AstNode::BinaryOp(gt));

    // Build bindings manually for quantified expression
    use crate::xpath::ast::ForBinding;
    let _ = names.add("x");
    let _ = names.add("y");
    let bindings = vec![
        ForBinding::new(String::new(), "x".to_string(), seq, span),
        ForBinding::new(String::new(), "y".to_string(), mul_id, span),
    ];
    let quant_node =
        crate::xpath::ast::QuantifiedNode::new(QuantifierKind::Some, bindings, gt_id, span);
    let quant_id = arena.add(AstNode::Quantified(quant_node));
    let root = wrap_in_expr(&mut arena, quant_id);

    bind_node(&mut arena, root, &ctx, &mut binder).unwrap();
    let mut dyn_ctx: DynamicContext<'_, RoXmlNavigator<'static>> =
        DynamicContext::new(&ctx, binder.len());

    let result = eval_node(&arena, root, &mut dyn_ctx).unwrap();
    match result {
        XPathValue::Item(XmlItem::Atomic(v)) => {
            assert_eq!(v.as_boolean(), Some(true));
        }
        _ => panic!("Expected boolean true"),
    }
}

#[test]
fn test_every_dependent_bindings() {
    // every $x in (1, 2), $y in (1 to $x) satisfies $y <= $x
    // When $x=1: $y in (1), 1 <= 1 is true
    // When $x=2: $y in (1,2), 1 <= 2 is true, 2 <= 2 is true
    // All satisfied -> true
    // Expected: true
    let names = NameTable::new();
    let ctx = XPathContext::new(&names);
    let mut binder = NameBinder::new();

    let mut arena = AstArena::new();
    let seq = make_int_sequence(&mut arena, &[1, 2]);

    // $y in (1 to $x)
    let one = arena.add(AstNode::Value(ValueNode::Integer("1".to_string())));
    let var_x_in_expr = make_var_ref(&mut arena, "x");
    let span = SourceSpan::new(0, 10);
    let range = RangeNode::new(one, var_x_in_expr, span);
    let range_id = arena.add(AstNode::Range(range));

    // satisfies $y <= $x
    let var_y = make_var_ref(&mut arena, "y");
    let var_x = make_var_ref(&mut arena, "x");
    let le = BinaryOpNode::new(BinaryOpKind::GeneralLe, var_y, var_x, span);
    let le_id = arena.add(AstNode::BinaryOp(le));

    // Build bindings manually for quantified expression
    use crate::xpath::ast::ForBinding;
    let _ = names.add("x");
    let _ = names.add("y");
    let bindings = vec![
        ForBinding::new(String::new(), "x".to_string(), seq, span),
        ForBinding::new(String::new(), "y".to_string(), range_id, span),
    ];
    let quant_node =
        crate::xpath::ast::QuantifiedNode::new(QuantifierKind::Every, bindings, le_id, span);
    let quant_id = arena.add(AstNode::Quantified(quant_node));
    let root = wrap_in_expr(&mut arena, quant_id);

    bind_node(&mut arena, root, &ctx, &mut binder).unwrap();
    let mut dyn_ctx: DynamicContext<'_, RoXmlNavigator<'static>> =
        DynamicContext::new(&ctx, binder.len());

    let result = eval_node(&arena, root, &mut dyn_ctx).unwrap();
    match result {
        XPathValue::Item(XmlItem::Atomic(v)) => {
            assert_eq!(v.as_boolean(), Some(true));
        }
        _ => panic!("Expected boolean true"),
    }
}

#[test]
fn test_every_dependent_bindings_fails() {
    // every $x in (1, 2, 3), $y in (1 to $x) satisfies $y < $x
    // When $x=1: $y in (1), 1 < 1 is FALSE -> short-circuit, return false
    // Expected: false
    let names = NameTable::new();
    let ctx = XPathContext::new(&names);
    let mut binder = NameBinder::new();

    let mut arena = AstArena::new();
    let seq = make_int_sequence(&mut arena, &[1, 2, 3]);

    // $y in (1 to $x)
    let one = arena.add(AstNode::Value(ValueNode::Integer("1".to_string())));
    let var_x_in_expr = make_var_ref(&mut arena, "x");
    let span = SourceSpan::new(0, 10);
    let range = RangeNode::new(one, var_x_in_expr, span);
    let range_id = arena.add(AstNode::Range(range));

    // satisfies $y < $x (strictly less than)
    let var_y = make_var_ref(&mut arena, "y");
    let var_x = make_var_ref(&mut arena, "x");
    let lt = BinaryOpNode::new(BinaryOpKind::GeneralLt, var_y, var_x, span);
    let lt_id = arena.add(AstNode::BinaryOp(lt));

    // Build bindings manually for quantified expression
    use crate::xpath::ast::ForBinding;
    let _ = names.add("x");
    let _ = names.add("y");
    let bindings = vec![
        ForBinding::new(String::new(), "x".to_string(), seq, span),
        ForBinding::new(String::new(), "y".to_string(), range_id, span),
    ];
    let quant_node =
        crate::xpath::ast::QuantifiedNode::new(QuantifierKind::Every, bindings, lt_id, span);
    let quant_id = arena.add(AstNode::Quantified(quant_node));
    let root = wrap_in_expr(&mut arena, quant_id);

    bind_node(&mut arena, root, &ctx, &mut binder).unwrap();
    let mut dyn_ctx: DynamicContext<'_, RoXmlNavigator<'static>> =
        DynamicContext::new(&ctx, binder.len());

    let result = eval_node(&arena, root, &mut dyn_ctx).unwrap();
    match result {
        XPathValue::Item(XmlItem::Atomic(v)) => {
            assert_eq!(v.as_boolean(), Some(false));
        }
        _ => panic!("Expected boolean false"),
    }
}

// ============================================================================
// Integration Tests (Parse -> Bind -> Eval)
// ============================================================================

#[cfg(test)]
mod integration_tests {
    //! Integration tests for the full parse -> bind -> eval pipeline.

    use crate::namespace::table::NameTable;
    use crate::xpath::bind::bind_node;
    use crate::xpath::context::{DynamicContext, NameBinder, XPathContext};
    use crate::xpath::error::XPathError;
    use crate::xpath::eval::eval_node;
    use crate::xpath::functions::XPathValue;
    use crate::xpath::parser;
    use crate::xpath::RoXmlNavigator;

    /// Helper to parse, bind, and evaluate an XPath expression without context item
    fn eval_xpath(expr: &str) -> Result<XPathValue<RoXmlNavigator<'static>>, XPathError> {
        let mut parsed =
            parser::parse(expr).map_err(|e| XPathError::syntax_error(e.to_string()))?;

        let names = NameTable::new();
        let ctx = XPathContext::new(&names);
        let mut binder = NameBinder::new();

        bind_node(&mut parsed.arena, parsed.root, &ctx, &mut binder)?;

        let mut dyn_ctx: DynamicContext<'_, RoXmlNavigator<'static>> =
            DynamicContext::new(&ctx, binder.len());

        eval_node(&parsed.arena, parsed.root, &mut dyn_ctx)
    }

    /// Helper to parse, bind, and evaluate with a variable bound
    fn eval_xpath_with_var(
        expr: &str,
        var_name: &str,
        var_value: i64,
    ) -> Result<XPathValue<RoXmlNavigator<'static>>, XPathError> {
        use crate::namespace::qname::QualifiedName;
        use crate::types::value::XmlValue;
        use crate::xpath::iterator::XmlItem;

        let mut parsed =
            parser::parse(expr).map_err(|e| XPathError::syntax_error(e.to_string()))?;

        let names = NameTable::new();
        let ctx = XPathContext::new(&names);
        let mut binder = NameBinder::new();

        // Push the variable into scope before binding
        let var_name_id = names.add(var_name);
        let qname = QualifiedName::local(var_name_id);
        let var_ref = binder.push_var(qname);

        bind_node(&mut parsed.arena, parsed.root, &ctx, &mut binder)?;

        let mut dyn_ctx: DynamicContext<'_, RoXmlNavigator<'static>> =
            DynamicContext::new(&ctx, binder.len());

        // Set the variable value
        dyn_ctx.set_variable(
            var_ref.slot,
            XPathValue::Item(XmlItem::Atomic(XmlValue::integer(
                num_bigint::BigInt::from(var_value),
            ))),
        );

        eval_node(&parsed.arena, parsed.root, &mut dyn_ctx)
    }

    #[test]
    fn test_parse_bind_eval_if_expression() {
        let result = eval_xpath("if (true()) then 'yes' else 'no'").unwrap();
        assert_eq!(result.as_str(), Some("yes".to_string()));
    }

    #[test]
    fn test_parse_bind_eval_nested_functions() {
        let result = eval_xpath("upper-case(concat('a', 'b'))").unwrap();
        assert_eq!(result.as_str(), Some("AB".to_string()));
    }

    #[test]
    fn test_parse_bind_eval_variable_reference() {
        let result = eval_xpath_with_var("$x + 1", "x", 5).unwrap();
        assert_eq!(
            result.as_integer().map(|i| i.to_string()),
            Some("6".to_string())
        );
    }

    #[test]
    fn test_parse_bind_eval_comparison() {
        assert_eq!(eval_xpath("1 = 1").unwrap().as_bool(), Some(true));
        assert_eq!(eval_xpath("1 = 2").unwrap().as_bool(), Some(false));
        assert_eq!(eval_xpath("1 < 2").unwrap().as_bool(), Some(true));
    }

    #[test]
    fn test_parse_bind_eval_arithmetic() {
        assert_eq!(
            eval_xpath("1 + 2")
                .unwrap()
                .as_integer()
                .map(|i| i.to_string()),
            Some("3".to_string())
        );
        assert_eq!(
            eval_xpath("5 - 3")
                .unwrap()
                .as_integer()
                .map(|i| i.to_string()),
            Some("2".to_string())
        );
        assert_eq!(
            eval_xpath("2 * 3")
                .unwrap()
                .as_integer()
                .map(|i| i.to_string()),
            Some("6".to_string())
        );
    }
}

// ========================================================================
// Type Expression Tests (instance of, treat as, cast as, castable as)
// ========================================================================

mod type_expr_tests {
    use crate::namespace::context::NamespaceContextSnapshot;
    use crate::namespace::table::well_known;
    use crate::namespace::table::NameTable;
    use crate::xpath::arena::{AstArena, AstNodeId, SourceSpan};
    use crate::xpath::ast::{
        AstNode, ExprNode, ItemTypeNode as AstItemTypeNode, OccurrenceIndicator, QName as AstQName,
        SequenceTypeNode, TypeExprKind, TypeExprNode, ValueNode,
    };
    use crate::xpath::bind::bind_node;
    use crate::xpath::context::{DynamicContext, NameBinder, XPathContext};
    use crate::xpath::error::XPathError;
    use crate::xpath::eval::eval_node;
    use crate::xpath::functions::XPathValue;
    use crate::xpath::iterator::XmlItem;
    use crate::xpath::RoXmlNavigator;

    /// Helper to wrap a node in an Expr
    fn wrap_in_expr(arena: &mut AstArena, node_id: AstNodeId) -> AstNodeId {
        let span = SourceSpan::new(0, 10);
        let expr = ExprNode::single(node_id, span);
        arena.add(AstNode::Expr(expr))
    }

    /// Helper to bind and eval a manually constructed AST
    fn bind_and_eval(
        arena: &mut AstArena,
        root: AstNodeId,
    ) -> Result<XPathValue<RoXmlNavigator<'static>>, XPathError> {
        let names = NameTable::new();
        // Set up namespace context with "xs" prefix bound to XSD namespace
        let xs_prefix = names.add("xs");
        let ns_snapshot = NamespaceContextSnapshot {
            default_ns: None,
            bindings: vec![(xs_prefix, well_known::XS_NAMESPACE)],
        };
        let ctx = XPathContext::new(&names).with_namespaces(ns_snapshot);
        let mut binder = NameBinder::new();

        bind_node(arena, root, &ctx, &mut binder)?;

        let mut dyn_ctx: DynamicContext<'_, RoXmlNavigator<'static>> =
            DynamicContext::new(&ctx, binder.len());

        eval_node(arena, root, &mut dyn_ctx)
    }

    /// Helper to create a TypeExpr node with atomic target type
    fn make_type_expr(
        arena: &mut AstArena,
        kind: TypeExprKind,
        operand: AstNodeId,
        type_name: &str,
        occurrence: OccurrenceIndicator,
    ) -> AstNodeId {
        let span = SourceSpan::new(0, 20);
        let qname = AstQName::new("xs".to_string(), type_name.to_string());
        let item_type = AstItemTypeNode::Atomic(qname);
        let target_type = SequenceTypeNode::single(item_type, occurrence, span);
        let type_expr = TypeExprNode::new(kind, operand, target_type, span);
        arena.add(AstNode::TypeExpr(type_expr))
    }

    /// Helper to create a TypeExpr node with item() target type
    fn make_type_expr_item(
        arena: &mut AstArena,
        kind: TypeExprKind,
        operand: AstNodeId,
        occurrence: OccurrenceIndicator,
    ) -> AstNodeId {
        let span = SourceSpan::new(0, 20);
        let item_type = AstItemTypeNode::Item;
        let target_type = SequenceTypeNode::single(item_type, occurrence, span);
        let type_expr = TypeExprNode::new(kind, operand, target_type, span);
        arena.add(AstNode::TypeExpr(type_expr))
    }

    /// Helper to create an empty-sequence() type expression
    fn make_type_expr_empty_seq(
        arena: &mut AstArena,
        kind: TypeExprKind,
        operand: AstNodeId,
    ) -> AstNodeId {
        let span = SourceSpan::new(0, 20);
        let target_type = SequenceTypeNode::empty(span);
        let type_expr = TypeExprNode::new(kind, operand, target_type, span);
        arena.add(AstNode::TypeExpr(type_expr))
    }

    #[test]
    fn test_instance_of_atomic_matching() {
        // 42 instance of xs:integer -> true
        let mut arena = AstArena::new();
        let val = arena.add(AstNode::Value(ValueNode::Integer("42".to_string())));
        let type_expr = make_type_expr(
            &mut arena,
            TypeExprKind::InstanceOf,
            val,
            "integer",
            OccurrenceIndicator::One,
        );
        let root = wrap_in_expr(&mut arena, type_expr);

        let result = bind_and_eval(&mut arena, root).unwrap();
        match result {
            XPathValue::Item(XmlItem::Atomic(v)) => {
                assert_eq!(v.as_boolean(), Some(true));
            }
            _ => panic!("Expected boolean true"),
        }
    }

    #[test]
    fn test_instance_of_atomic_not_matching() {
        // 42 instance of xs:string -> false
        let mut arena = AstArena::new();
        let val = arena.add(AstNode::Value(ValueNode::Integer("42".to_string())));
        let type_expr = make_type_expr(
            &mut arena,
            TypeExprKind::InstanceOf,
            val,
            "string",
            OccurrenceIndicator::One,
        );
        let root = wrap_in_expr(&mut arena, type_expr);

        let result = bind_and_eval(&mut arena, root).unwrap();
        match result {
            XPathValue::Item(XmlItem::Atomic(v)) => {
                assert_eq!(v.as_boolean(), Some(false));
            }
            _ => panic!("Expected boolean false"),
        }
    }

    #[test]
    fn test_instance_of_string() {
        // "hello" instance of xs:string -> true
        let mut arena = AstArena::new();
        let val = arena.add(AstNode::Value(ValueNode::String("hello".to_string())));
        let type_expr = make_type_expr(
            &mut arena,
            TypeExprKind::InstanceOf,
            val,
            "string",
            OccurrenceIndicator::One,
        );
        let root = wrap_in_expr(&mut arena, type_expr);

        let result = bind_and_eval(&mut arena, root).unwrap();
        match result {
            XPathValue::Item(XmlItem::Atomic(v)) => {
                assert_eq!(v.as_boolean(), Some(true));
            }
            _ => panic!("Expected boolean true"),
        }
    }

    #[test]
    fn test_instance_of_cardinality_too_many() {
        // (1, 2) instance of xs:integer -> false (too many items)
        let mut arena = AstArena::new();
        let v1 = arena.add(AstNode::Value(ValueNode::Integer("1".to_string())));
        let v2 = arena.add(AstNode::Value(ValueNode::Integer("2".to_string())));
        let span = SourceSpan::new(0, 5);
        let seq = ExprNode::sequence(vec![v1, v2], span);
        let seq_id = arena.add(AstNode::Expr(seq));
        let type_expr = make_type_expr(
            &mut arena,
            TypeExprKind::InstanceOf,
            seq_id,
            "integer",
            OccurrenceIndicator::One,
        );
        let root = wrap_in_expr(&mut arena, type_expr);

        let result = bind_and_eval(&mut arena, root).unwrap();
        match result {
            XPathValue::Item(XmlItem::Atomic(v)) => {
                assert_eq!(v.as_boolean(), Some(false));
            }
            _ => panic!("Expected boolean false"),
        }
    }

    #[test]
    fn test_instance_of_cardinality_star() {
        // (1, 2) instance of xs:integer* -> true
        let mut arena = AstArena::new();
        let v1 = arena.add(AstNode::Value(ValueNode::Integer("1".to_string())));
        let v2 = arena.add(AstNode::Value(ValueNode::Integer("2".to_string())));
        let span = SourceSpan::new(0, 5);
        let seq = ExprNode::sequence(vec![v1, v2], span);
        let seq_id = arena.add(AstNode::Expr(seq));
        let type_expr = make_type_expr(
            &mut arena,
            TypeExprKind::InstanceOf,
            seq_id,
            "integer",
            OccurrenceIndicator::ZeroOrMore,
        );
        let root = wrap_in_expr(&mut arena, type_expr);

        let result = bind_and_eval(&mut arena, root).unwrap();
        match result {
            XPathValue::Item(XmlItem::Atomic(v)) => {
                assert_eq!(v.as_boolean(), Some(true));
            }
            _ => panic!("Expected boolean true"),
        }
    }

    #[test]
    fn test_instance_of_empty_sequence() {
        // () instance of xs:integer? -> true
        let mut arena = AstArena::new();
        let empty = arena.add(AstNode::Value(ValueNode::Empty));
        let type_expr = make_type_expr(
            &mut arena,
            TypeExprKind::InstanceOf,
            empty,
            "integer",
            OccurrenceIndicator::ZeroOrOne,
        );
        let root = wrap_in_expr(&mut arena, type_expr);

        let result = bind_and_eval(&mut arena, root).unwrap();
        match result {
            XPathValue::Item(XmlItem::Atomic(v)) => {
                assert_eq!(v.as_boolean(), Some(true));
            }
            _ => panic!("Expected boolean true"),
        }

        // () instance of xs:integer -> false
        let mut arena = AstArena::new();
        let empty = arena.add(AstNode::Value(ValueNode::Empty));
        let type_expr = make_type_expr(
            &mut arena,
            TypeExprKind::InstanceOf,
            empty,
            "integer",
            OccurrenceIndicator::One,
        );
        let root = wrap_in_expr(&mut arena, type_expr);

        let result = bind_and_eval(&mut arena, root).unwrap();
        match result {
            XPathValue::Item(XmlItem::Atomic(v)) => {
                assert_eq!(v.as_boolean(), Some(false));
            }
            _ => panic!("Expected boolean false"),
        }
    }

    #[test]
    fn test_instance_of_item() {
        // 42 instance of item() -> true
        let mut arena = AstArena::new();
        let val = arena.add(AstNode::Value(ValueNode::Integer("42".to_string())));
        let type_expr = make_type_expr_item(
            &mut arena,
            TypeExprKind::InstanceOf,
            val,
            OccurrenceIndicator::One,
        );
        let root = wrap_in_expr(&mut arena, type_expr);

        let result = bind_and_eval(&mut arena, root).unwrap();
        match result {
            XPathValue::Item(XmlItem::Atomic(v)) => {
                assert_eq!(v.as_boolean(), Some(true));
            }
            _ => panic!("Expected boolean true"),
        }
    }

    #[test]
    fn test_instance_of_empty_sequence_type() {
        // () instance of empty-sequence() -> true
        let mut arena = AstArena::new();
        let empty = arena.add(AstNode::Value(ValueNode::Empty));
        let type_expr = make_type_expr_empty_seq(&mut arena, TypeExprKind::InstanceOf, empty);
        let root = wrap_in_expr(&mut arena, type_expr);

        let result = bind_and_eval(&mut arena, root).unwrap();
        match result {
            XPathValue::Item(XmlItem::Atomic(v)) => {
                assert_eq!(v.as_boolean(), Some(true));
            }
            _ => panic!("Expected boolean true"),
        }

        // 42 instance of empty-sequence() -> false
        let mut arena = AstArena::new();
        let val = arena.add(AstNode::Value(ValueNode::Integer("42".to_string())));
        let type_expr = make_type_expr_empty_seq(&mut arena, TypeExprKind::InstanceOf, val);
        let root = wrap_in_expr(&mut arena, type_expr);

        let result = bind_and_eval(&mut arena, root).unwrap();
        match result {
            XPathValue::Item(XmlItem::Atomic(v)) => {
                assert_eq!(v.as_boolean(), Some(false));
            }
            _ => panic!("Expected boolean false"),
        }
    }

    #[test]
    fn test_treat_as_success() {
        // "hello" treat as xs:string -> "hello"
        let mut arena = AstArena::new();
        let val = arena.add(AstNode::Value(ValueNode::String("hello".to_string())));
        let type_expr = make_type_expr(
            &mut arena,
            TypeExprKind::TreatAs,
            val,
            "string",
            OccurrenceIndicator::One,
        );
        let root = wrap_in_expr(&mut arena, type_expr);

        let result = bind_and_eval(&mut arena, root).unwrap();
        match result {
            XPathValue::Item(XmlItem::Atomic(v)) => {
                assert_eq!(v.as_string(), Some("hello"));
            }
            _ => panic!("Expected string 'hello'"),
        }
    }

    #[test]
    fn test_treat_as_failure() {
        // 42 treat as xs:string -> XPTY0004 error
        let mut arena = AstArena::new();
        let val = arena.add(AstNode::Value(ValueNode::Integer("42".to_string())));
        let type_expr = make_type_expr(
            &mut arena,
            TypeExprKind::TreatAs,
            val,
            "string",
            OccurrenceIndicator::One,
        );
        let root = wrap_in_expr(&mut arena, type_expr);

        let result = bind_and_eval(&mut arena, root);
        assert!(matches!(result, Err(XPathError::XPTY0004 { .. })));
    }

    #[test]
    fn test_treat_as_empty_optional() {
        // () treat as xs:integer? -> ()
        let mut arena = AstArena::new();
        let empty = arena.add(AstNode::Value(ValueNode::Empty));
        let type_expr = make_type_expr(
            &mut arena,
            TypeExprKind::TreatAs,
            empty,
            "integer",
            OccurrenceIndicator::ZeroOrOne,
        );
        let root = wrap_in_expr(&mut arena, type_expr);

        let result = bind_and_eval(&mut arena, root).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_cast_as_string_to_integer() {
        // "42" cast as xs:integer -> 42
        let mut arena = AstArena::new();
        let val = arena.add(AstNode::Value(ValueNode::String("42".to_string())));
        let type_expr = make_type_expr(
            &mut arena,
            TypeExprKind::CastAs,
            val,
            "integer",
            OccurrenceIndicator::One,
        );
        let root = wrap_in_expr(&mut arena, type_expr);

        let result = bind_and_eval(&mut arena, root).unwrap();
        match result {
            XPathValue::Item(XmlItem::Atomic(v)) => {
                assert_eq!(
                    v.as_integer().map(|i| i.to_string()),
                    Some("42".to_string())
                );
            }
            _ => panic!("Expected integer 42"),
        }
    }

    #[test]
    fn test_cast_as_double_to_integer() {
        // 42.7 cast as xs:integer -> 42 (truncated)
        let mut arena = AstArena::new();
        let val = arena.add(AstNode::Value(ValueNode::Double("42.7".to_string())));
        let type_expr = make_type_expr(
            &mut arena,
            TypeExprKind::CastAs,
            val,
            "integer",
            OccurrenceIndicator::One,
        );
        let root = wrap_in_expr(&mut arena, type_expr);

        let result = bind_and_eval(&mut arena, root).unwrap();
        match result {
            XPathValue::Item(XmlItem::Atomic(v)) => {
                assert_eq!(
                    v.as_integer().map(|i| i.to_string()),
                    Some("42".to_string())
                );
            }
            _ => panic!("Expected integer 42"),
        }
    }

    #[test]
    fn test_cast_as_empty_optional() {
        // () cast as xs:integer? -> ()
        let mut arena = AstArena::new();
        let empty = arena.add(AstNode::Value(ValueNode::Empty));
        let type_expr = make_type_expr(
            &mut arena,
            TypeExprKind::CastAs,
            empty,
            "integer",
            OccurrenceIndicator::ZeroOrOne,
        );
        let root = wrap_in_expr(&mut arena, type_expr);

        let result = bind_and_eval(&mut arena, root).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_cast_as_empty_required() {
        // () cast as xs:integer -> XPTY0004 error
        let mut arena = AstArena::new();
        let empty = arena.add(AstNode::Value(ValueNode::Empty));
        let type_expr = make_type_expr(
            &mut arena,
            TypeExprKind::CastAs,
            empty,
            "integer",
            OccurrenceIndicator::One,
        );
        let root = wrap_in_expr(&mut arena, type_expr);

        let result = bind_and_eval(&mut arena, root);
        assert!(matches!(result, Err(XPathError::XPTY0004 { .. })));
    }

    #[test]
    fn test_cast_as_invalid() {
        // "abc" cast as xs:integer -> FORG0001 error
        let mut arena = AstArena::new();
        let val = arena.add(AstNode::Value(ValueNode::String("abc".to_string())));
        let type_expr = make_type_expr(
            &mut arena,
            TypeExprKind::CastAs,
            val,
            "integer",
            OccurrenceIndicator::One,
        );
        let root = wrap_in_expr(&mut arena, type_expr);

        let result = bind_and_eval(&mut arena, root);
        assert!(matches!(result, Err(XPathError::FORG0001 { .. })));
    }

    #[test]
    fn test_castable_as_success() {
        // "42" castable as xs:integer -> true
        let mut arena = AstArena::new();
        let val = arena.add(AstNode::Value(ValueNode::String("42".to_string())));
        let type_expr = make_type_expr(
            &mut arena,
            TypeExprKind::CastableAs,
            val,
            "integer",
            OccurrenceIndicator::One,
        );
        let root = wrap_in_expr(&mut arena, type_expr);

        let result = bind_and_eval(&mut arena, root).unwrap();
        match result {
            XPathValue::Item(XmlItem::Atomic(v)) => {
                assert_eq!(v.as_boolean(), Some(true));
            }
            _ => panic!("Expected boolean true"),
        }
    }

    #[test]
    fn test_castable_as_failure() {
        // "abc" castable as xs:integer -> false
        let mut arena = AstArena::new();
        let val = arena.add(AstNode::Value(ValueNode::String("abc".to_string())));
        let type_expr = make_type_expr(
            &mut arena,
            TypeExprKind::CastableAs,
            val,
            "integer",
            OccurrenceIndicator::One,
        );
        let root = wrap_in_expr(&mut arena, type_expr);

        let result = bind_and_eval(&mut arena, root).unwrap();
        match result {
            XPathValue::Item(XmlItem::Atomic(v)) => {
                assert_eq!(v.as_boolean(), Some(false));
            }
            _ => panic!("Expected boolean false"),
        }
    }

    #[test]
    fn test_castable_as_empty_optional() {
        // () castable as xs:integer? -> true
        let mut arena = AstArena::new();
        let empty = arena.add(AstNode::Value(ValueNode::Empty));
        let type_expr = make_type_expr(
            &mut arena,
            TypeExprKind::CastableAs,
            empty,
            "integer",
            OccurrenceIndicator::ZeroOrOne,
        );
        let root = wrap_in_expr(&mut arena, type_expr);

        let result = bind_and_eval(&mut arena, root).unwrap();
        match result {
            XPathValue::Item(XmlItem::Atomic(v)) => {
                assert_eq!(v.as_boolean(), Some(true));
            }
            _ => panic!("Expected boolean true"),
        }
    }

    #[test]
    fn test_castable_as_empty_required() {
        // () castable as xs:integer -> false
        let mut arena = AstArena::new();
        let empty = arena.add(AstNode::Value(ValueNode::Empty));
        let type_expr = make_type_expr(
            &mut arena,
            TypeExprKind::CastableAs,
            empty,
            "integer",
            OccurrenceIndicator::One,
        );
        let root = wrap_in_expr(&mut arena, type_expr);

        let result = bind_and_eval(&mut arena, root).unwrap();
        match result {
            XPathValue::Item(XmlItem::Atomic(v)) => {
                assert_eq!(v.as_boolean(), Some(false));
            }
            _ => panic!("Expected boolean false"),
        }
    }
} // end type_expr_tests

// ============================================================================
// Path Expression Tests
// ============================================================================

mod path_expr_tests {
    use super::*;
    use crate::xpath::ast::{
        Axis, ExprNode, FilterExprNode, KindTest, NameTest as AstNameTest, NodeTest as AstNodeTest,
        PathExprNode, PathStepNode,
    };
    use crate::xpath::bind::bind_node;
    use crate::xpath::context::NameBinder;

    /// Helper to create a path step
    fn make_path_step(arena: &mut AstArena, axis: Axis, test: AstNodeTest) -> AstNodeId {
        let span = SourceSpan::new(0, 10);
        let step = PathStepNode::new(axis, test, span);
        arena.add(AstNode::PathStep(step))
    }

    /// Helper to create a name test for any element
    fn wildcard_name_test() -> AstNodeTest {
        AstNodeTest::Name(AstNameTest::any())
    }

    /// Helper to create a kind test for node()
    fn node_kind_test() -> AstNodeTest {
        AstNodeTest::Kind(KindTest::AnyKind)
    }

    #[test]
    fn test_path_root_only() {
        // Test "/" - returns document root
        let doc = roxmltree::Document::parse("<root><a/><b/></root>").expect("parse xml");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child(); // root
        nav.move_to_first_child(); // a

        let names = NameTable::new();
        let ctx = XPathContext::new(&names);
        let mut binder = NameBinder::new();

        let mut arena = AstArena::new();
        let span = SourceSpan::new(0, 1);
        // Create root-only path "/"
        let path = PathExprNode::root_only(span);
        let path_id = arena.add(AstNode::PathExpr(path));
        let root = wrap_in_expr(&mut arena, path_id);

        bind_node(&mut arena, root, &ctx, &mut binder).unwrap();

        let mut dyn_ctx =
            DynamicContext::new(&ctx, binder.len()).with_context_item(XmlItem::Node(nav.clone()));

        let result = eval_node(&arena, root, &mut dyn_ctx).unwrap();
        match result {
            XPathValue::Item(XmlItem::Node(n)) => {
                assert!(
                    matches!(n.node_type(), crate::xpath::DomNodeType::Root),
                    "Expected document root"
                );
            }
            _ => panic!("Expected single node result"),
        }
    }

    #[test]
    fn test_path_absolute_child() {
        // Test "/child::*" - returns root element
        let doc = roxmltree::Document::parse("<root><a/><b/></root>").expect("parse xml");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child(); // position at root
        nav.move_to_first_child(); // position at a

        let names = NameTable::new();
        let ctx = XPathContext::new(&names);
        let mut binder = NameBinder::new();

        let mut arena = AstArena::new();
        let span = SourceSpan::new(0, 10);

        // Create step: child::*
        let step_id = make_path_step(&mut arena, Axis::Child, wildcard_name_test());

        // Create absolute path with that step
        let path = PathExprNode::absolute(vec![step_id], span);
        let path_id = arena.add(AstNode::PathExpr(path));
        let root = wrap_in_expr(&mut arena, path_id);

        bind_node(&mut arena, root, &ctx, &mut binder).unwrap();

        let mut dyn_ctx =
            DynamicContext::new(&ctx, binder.len()).with_context_item(XmlItem::Node(nav.clone()));

        let result = eval_node(&arena, root, &mut dyn_ctx).unwrap();
        match result {
            XPathValue::Item(XmlItem::Node(n)) => {
                assert_eq!(n.local_name(), "root");
            }
            _ => panic!("Expected single node result for /*"),
        }
    }

    #[test]
    fn test_path_relative_child() {
        // Test "child::*" from <root> - should return <a> and <b>
        let doc = roxmltree::Document::parse("<root><a/><b/></root>").expect("parse xml");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child(); // position at root

        let names = NameTable::new();
        let ctx = XPathContext::new(&names);
        let mut binder = NameBinder::new();

        let mut arena = AstArena::new();
        let span = SourceSpan::new(0, 10);

        // Create step: child::*
        let step_id = make_path_step(&mut arena, Axis::Child, wildcard_name_test());

        // Create relative path
        let path = PathExprNode::relative(vec![step_id], span);
        let path_id = arena.add(AstNode::PathExpr(path));
        let root = wrap_in_expr(&mut arena, path_id);

        bind_node(&mut arena, root, &ctx, &mut binder).unwrap();

        let mut dyn_ctx =
            DynamicContext::new(&ctx, binder.len()).with_context_item(XmlItem::Node(nav.clone()));

        let result = eval_node(&arena, root, &mut dyn_ctx).unwrap();
        let items = result.into_vec();
        assert_eq!(items.len(), 2);
        match (&items[0], &items[1]) {
            (XmlItem::Node(a), XmlItem::Node(b)) => {
                assert_eq!(a.local_name(), "a");
                assert_eq!(b.local_name(), "b");
            }
            _ => panic!("Expected two nodes"),
        }
    }

    #[test]
    fn test_path_descendant_or_self() {
        // Test "descendant-or-self::node()" from <root> - should include root and descendants
        let doc = roxmltree::Document::parse("<root><a><c/></a><b/></root>").expect("parse xml");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child(); // position at root

        let names = NameTable::new();
        let ctx = XPathContext::new(&names);
        let mut binder = NameBinder::new();

        let mut arena = AstArena::new();
        let span = SourceSpan::new(0, 10);

        // Create step: descendant-or-self::node()
        let step_id = make_path_step(&mut arena, Axis::DescendantOrSelf, node_kind_test());

        // Create relative path
        let path = PathExprNode::relative(vec![step_id], span);
        let path_id = arena.add(AstNode::PathExpr(path));
        let root = wrap_in_expr(&mut arena, path_id);

        bind_node(&mut arena, root, &ctx, &mut binder).unwrap();

        let mut dyn_ctx =
            DynamicContext::new(&ctx, binder.len()).with_context_item(XmlItem::Node(nav.clone()));

        let result = eval_node(&arena, root, &mut dyn_ctx).unwrap();
        let items = result.into_vec();
        // Should include: root, a, c, b
        assert_eq!(items.len(), 4);
    }

    #[test]
    fn test_path_parent_axis() {
        // Test "parent::*" from <a> - should return <root>
        let doc = roxmltree::Document::parse("<root><a/><b/></root>").expect("parse xml");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child(); // position at root
        nav.move_to_first_child(); // position at a

        let names = NameTable::new();
        let ctx = XPathContext::new(&names);
        let mut binder = NameBinder::new();

        let mut arena = AstArena::new();
        let span = SourceSpan::new(0, 10);

        // Create step: parent::*
        let step_id = make_path_step(&mut arena, Axis::Parent, wildcard_name_test());

        // Create relative path
        let path = PathExprNode::relative(vec![step_id], span);
        let path_id = arena.add(AstNode::PathExpr(path));
        let root = wrap_in_expr(&mut arena, path_id);

        bind_node(&mut arena, root, &ctx, &mut binder).unwrap();

        let mut dyn_ctx =
            DynamicContext::new(&ctx, binder.len()).with_context_item(XmlItem::Node(nav.clone()));

        let result = eval_node(&arena, root, &mut dyn_ctx).unwrap();
        match result {
            XPathValue::Item(XmlItem::Node(n)) => {
                assert_eq!(n.local_name(), "root");
            }
            _ => panic!("Expected single node result for parent::*"),
        }
    }

    #[test]
    fn test_path_with_predicate_position() {
        // Test "child::*[1]" - returns first child only
        let doc = roxmltree::Document::parse("<root><a/><b/><c/></root>").expect("parse xml");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child(); // position at root

        let names = NameTable::new();
        let ctx = XPathContext::new(&names);
        let mut binder = NameBinder::new();

        let mut arena = AstArena::new();
        let span = SourceSpan::new(0, 10);

        // Create predicate: 1
        let pred = arena.add(AstNode::Value(ValueNode::Integer("1".to_string())));

        // Create step: child::* with predicate [1]
        let step =
            PathStepNode::with_predicates(Axis::Child, wildcard_name_test(), vec![pred], span);
        let step_id = arena.add(AstNode::PathStep(step));

        // Create relative path
        let path = PathExprNode::relative(vec![step_id], span);
        let path_id = arena.add(AstNode::PathExpr(path));
        let root = wrap_in_expr(&mut arena, path_id);

        bind_node(&mut arena, root, &ctx, &mut binder).unwrap();

        let mut dyn_ctx =
            DynamicContext::new(&ctx, binder.len()).with_context_item(XmlItem::Node(nav.clone()));

        let result = eval_node(&arena, root, &mut dyn_ctx).unwrap();
        match result {
            XPathValue::Item(XmlItem::Node(n)) => {
                assert_eq!(n.local_name(), "a");
            }
            _ => panic!("Expected single node result for *[1]"),
        }
    }

    #[test]
    fn test_path_with_predicate_boolean() {
        // Test "child::*[true()]" - returns all children
        let doc = roxmltree::Document::parse("<root><a/><b/></root>").expect("parse xml");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child(); // position at root

        let names = NameTable::new();
        let ctx = XPathContext::new(&names);
        let mut binder = NameBinder::new();

        let mut arena = AstArena::new();
        let span = SourceSpan::new(0, 10);

        // Create predicate: true()
        let pred = make_function_call(&mut arena, "", "true", vec![]);

        // Create step: child::* with predicate [true()]
        let step =
            PathStepNode::with_predicates(Axis::Child, wildcard_name_test(), vec![pred], span);
        let step_id = arena.add(AstNode::PathStep(step));

        // Create relative path
        let path = PathExprNode::relative(vec![step_id], span);
        let path_id = arena.add(AstNode::PathExpr(path));
        let root = wrap_in_expr(&mut arena, path_id);

        bind_node(&mut arena, root, &ctx, &mut binder).unwrap();

        let mut dyn_ctx =
            DynamicContext::new(&ctx, binder.len()).with_context_item(XmlItem::Node(nav.clone()));

        let result = eval_node(&arena, root, &mut dyn_ctx).unwrap();
        let items = result.into_vec();
        assert_eq!(items.len(), 2, "Expected all children with [true()]");
    }

    #[test]
    fn test_path_multi_step() {
        // Test "child::*/child::*" - returns grandchildren
        let doc = roxmltree::Document::parse("<root><a><x/><y/></a><b><z/></b></root>")
            .expect("parse xml");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child(); // position at root

        let names = NameTable::new();
        let ctx = XPathContext::new(&names);
        let mut binder = NameBinder::new();

        let mut arena = AstArena::new();
        let span = SourceSpan::new(0, 10);

        // Create two steps: child::* / child::*
        let step1 = make_path_step(&mut arena, Axis::Child, wildcard_name_test());
        let step2 = make_path_step(&mut arena, Axis::Child, wildcard_name_test());

        // Create relative path with both steps
        let path = PathExprNode::relative(vec![step1, step2], span);
        let path_id = arena.add(AstNode::PathExpr(path));
        let root = wrap_in_expr(&mut arena, path_id);

        bind_node(&mut arena, root, &ctx, &mut binder).unwrap();

        let mut dyn_ctx =
            DynamicContext::new(&ctx, binder.len()).with_context_item(XmlItem::Node(nav.clone()));

        let result = eval_node(&arena, root, &mut dyn_ctx).unwrap();
        let items = result.into_vec();
        assert_eq!(items.len(), 3, "Expected 3 grandchildren: x, y, z");
        let names: Vec<String> = items
            .iter()
            .filter_map(|item| match item {
                XmlItem::Node(n) => Some(n.local_name().to_string()),
                _ => None,
            })
            .collect();
        assert!(names.contains(&"x".to_string()));
        assert!(names.contains(&"y".to_string()));
        assert!(names.contains(&"z".to_string()));
    }

    #[test]
    fn test_filter_expr() {
        // Test "(1, 2, 3)[2]" - returns 2
        let names = NameTable::new();
        let ctx = XPathContext::new(&names);
        let mut binder = NameBinder::new();

        let mut arena = AstArena::new();
        let span = SourceSpan::new(0, 10);

        // Create sequence (1, 2, 3)
        let v1 = arena.add(AstNode::Value(ValueNode::Integer("1".to_string())));
        let v2 = arena.add(AstNode::Value(ValueNode::Integer("2".to_string())));
        let v3 = arena.add(AstNode::Value(ValueNode::Integer("3".to_string())));
        let expr = ExprNode::sequence(vec![v1, v2, v3], span);
        let base_id = arena.add(AstNode::Expr(expr));

        // Create predicate: 2
        let pred = arena.add(AstNode::Value(ValueNode::Integer("2".to_string())));

        // Create filter expression
        let filter = FilterExprNode::new(base_id, vec![pred], span);
        let filter_id = arena.add(AstNode::FilterExpr(filter));
        let root = wrap_in_expr(&mut arena, filter_id);

        bind_node(&mut arena, root, &ctx, &mut binder).unwrap();

        let mut dyn_ctx: DynamicContext<'_, RoXmlNavigator<'static>> =
            DynamicContext::new(&ctx, binder.len());

        let result = eval_node(&arena, root, &mut dyn_ctx).unwrap();
        match result {
            XPathValue::Item(XmlItem::Atomic(v)) => {
                assert_eq!(v.as_integer().map(|i| i.to_string()), Some("2".to_string()));
            }
            _ => panic!("Expected single integer result"),
        }
    }

    #[test]
    fn test_path_self_axis() {
        // Test "self::*" from <root> - should return <root>
        let doc = roxmltree::Document::parse("<root><a/></root>").expect("parse xml");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child(); // position at root

        let names = NameTable::new();
        let ctx = XPathContext::new(&names);
        let mut binder = NameBinder::new();

        let mut arena = AstArena::new();
        let span = SourceSpan::new(0, 10);

        // Create step: self::*
        let step_id = make_path_step(&mut arena, Axis::SelfAxis, wildcard_name_test());

        // Create relative path
        let path = PathExprNode::relative(vec![step_id], span);
        let path_id = arena.add(AstNode::PathExpr(path));
        let root = wrap_in_expr(&mut arena, path_id);

        bind_node(&mut arena, root, &ctx, &mut binder).unwrap();

        let mut dyn_ctx =
            DynamicContext::new(&ctx, binder.len()).with_context_item(XmlItem::Node(nav.clone()));

        let result = eval_node(&arena, root, &mut dyn_ctx).unwrap();
        match result {
            XPathValue::Item(XmlItem::Node(n)) => {
                assert_eq!(n.local_name(), "root");
            }
            _ => panic!("Expected single node result for self::*"),
        }
    }
}

// ============================================================================
// XPath 1.0 Mode Integration Tests
// ============================================================================

mod xpath10_eval_tests {
    use crate::namespace::table::NameTable;
    use crate::xpath::bind::bind_node;
    use crate::xpath::context::{DynamicContext, NameBinder, XPathContext};
    use crate::xpath::error::XPathError;
    use crate::xpath::eval::eval_node;
    use crate::xpath::functions::XPathValue;
    use crate::xpath::functions::{XPath10Catalog, XPath10Evaluator};
    use crate::xpath::iterator::XmlItem;
    use crate::xpath::parser;
    use crate::xpath::parser::parse_with_mode;
    use crate::xpath::RoXmlNavigator;
    use crate::xpath::XPathMode;

    /// Helper to parse, bind, and evaluate an XPath 1.0 expression without context item
    fn eval_xpath10(expr: &str) -> Result<XPathValue<RoXmlNavigator<'static>>, XPathError> {
        let mut parsed = parse_with_mode(expr, XPathMode::XPath10)
            .map_err(|e| XPathError::syntax_error(e.to_string()))?;

        let names = NameTable::new();
        let catalog = XPath10Catalog;
        let ctx = XPathContext::new(&names)
            .with_mode(XPathMode::XPath10)
            .with_function_catalog(&catalog);
        let mut binder = NameBinder::new();

        bind_node(&mut parsed.arena, parsed.root, &ctx, &mut binder)?;

        let evaluator = XPath10Evaluator;
        let mut dyn_ctx: DynamicContext<'_, RoXmlNavigator<'static>> =
            DynamicContext::new(&ctx, binder.len()).with_function_evaluator(&evaluator);

        eval_node(&parsed.arena, parsed.root, &mut dyn_ctx)
    }

    /// Helper to parse, bind, and evaluate an XPath 2.0 expression without context item
    fn eval_xpath20(expr: &str) -> Result<XPathValue<RoXmlNavigator<'static>>, XPathError> {
        let mut parsed =
            parser::parse(expr).map_err(|e| XPathError::syntax_error(e.to_string()))?;

        let names = NameTable::new();
        let ctx = XPathContext::new(&names);
        let mut binder = NameBinder::new();

        bind_node(&mut parsed.arena, parsed.root, &ctx, &mut binder)?;

        let mut dyn_ctx: DynamicContext<'_, RoXmlNavigator<'static>> =
            DynamicContext::new(&ctx, binder.len());

        eval_node(&parsed.arena, parsed.root, &mut dyn_ctx)
    }

    #[test]
    fn test_xpath10_arithmetic_returns_double() {
        // In XPath 1.0, 1 + 2 = 3.0 (double), not 3 (integer)
        let result = eval_xpath10("1 + 2").unwrap();
        match result {
            XPathValue::Item(XmlItem::Atomic(v)) => {
                assert_eq!(v.type_code, crate::types::XmlTypeCode::Double);
                assert_eq!(v.as_double(), Some(3.0));
            }
            _ => panic!("Expected atomic double"),
        }
    }

    #[test]
    fn test_xpath10_arithmetic_mul() {
        let result = eval_xpath10("3 * 4").unwrap();
        match result {
            XPathValue::Item(XmlItem::Atomic(v)) => {
                assert_eq!(v.type_code, crate::types::XmlTypeCode::Double);
                assert_eq!(v.as_double(), Some(12.0));
            }
            _ => panic!("Expected atomic double"),
        }
    }

    #[test]
    fn test_xpath10_comparison_eq_boolean_priority() {
        // "1" = true() → XPath 1.0: both to boolean → true = true → true
        let result = eval_xpath10("'1' = true()").unwrap();
        match result {
            XPathValue::Item(XmlItem::Atomic(v)) => {
                assert_eq!(v.as_boolean(), Some(true));
            }
            _ => panic!("Expected boolean true"),
        }
    }

    #[test]
    fn test_xpath10_comparison_lt_numeric_coercion() {
        // 3 < 10 → numeric comparison → true
        let result = eval_xpath10("3 < 10").unwrap();
        match result {
            XPathValue::Item(XmlItem::Atomic(v)) => {
                assert_eq!(v.as_boolean(), Some(true));
            }
            _ => panic!("Expected boolean true"),
        }
    }

    #[test]
    fn test_xpath10_comparison_eq_string() {
        // 'abc' = 'abc' → string comparison → true
        let result = eval_xpath10("'abc' = 'abc'").unwrap();
        match result {
            XPathValue::Item(XmlItem::Atomic(v)) => {
                assert_eq!(v.as_boolean(), Some(true));
            }
            _ => panic!("Expected boolean true"),
        }
    }

    #[test]
    fn test_xpath10_comparison_eq_string_ne() {
        // 'abc' = 'def' → string comparison → false
        let result = eval_xpath10("'abc' = 'def'").unwrap();
        match result {
            XPathValue::Item(XmlItem::Atomic(v)) => {
                assert_eq!(v.as_boolean(), Some(false));
            }
            _ => panic!("Expected boolean false"),
        }
    }

    #[test]
    fn test_xpath10_rejects_sequence_expression() {
        // (1, 2, 3) uses the comma operator which is not available in XPath 1.0
        let result = eval_xpath10("(1, 2, 3)");
        match result {
            Err(ref e) => assert_eq!(e.error_code(), Some("XPST0003")),
            Ok(_) => panic!("Expected XPST0003 error for comma sequence in 1.0 mode"),
        }
    }

    #[test]
    fn test_xpath20_predicate_no_rounding() {
        // (1, 2, 3)[2.5] in XPath 2.0 → 2.5 matches no position → empty
        let result = eval_xpath20("(1, 2, 3)[2.5]").unwrap();
        assert!(
            matches!(result, XPathValue::Empty),
            "Expected empty sequence for [2.5] predicate in 2.0 mode",
        );
    }

    #[test]
    fn test_xpath10_and_with_ebv() {
        // true() and true() → true
        let result = eval_xpath10("true() and true()").unwrap();
        match result {
            XPathValue::Item(XmlItem::Atomic(v)) => {
                assert_eq!(v.as_boolean(), Some(true));
            }
            _ => panic!("Expected boolean true"),
        }
    }

    #[test]
    fn test_xpath10_or_with_ebv() {
        // false() or true() → true
        let result = eval_xpath10("false() or true()").unwrap();
        match result {
            XPathValue::Item(XmlItem::Atomic(v)) => {
                assert_eq!(v.as_boolean(), Some(true));
            }
            _ => panic!("Expected boolean true"),
        }
    }
}
