//! XPath AST evaluation phase.
//!
//! This module provides the `eval_node()` function which evaluates a bound
//! XPath AST at runtime. The AST must be bound using `bind_node()` before
//! evaluation.
//!
//! ## Supported Node Types
//!
//! Currently implemented:
//! - `Value` - Literal values (string, integer, double, boolean, empty)
//! - `ContextItem` - Context item reference (`.`)
//! - `VarRef` - Variable references
//! - `Expr` - Sequence expressions
//! - `If` - Conditional expressions
//! - `FunctionCall` - Function calls (dispatched via `eval_function`)
//!
//! Other node types return `not_implemented` errors for now.

use crate::xpath::arena::{AstArena, AstNodeId};
use crate::xpath::ast::{AstNode, BinaryOpKind, ValueNode};
use crate::xpath::context::DynamicContext;
use crate::xpath::error::XPathError;
use crate::xpath::functions::{atomize_to_single_opt, eval_function, effective_boolean_value, XPathValue};
use crate::xpath::iterator::{VecNodeIterator, XmlItem};
use crate::xpath::node_ops::{following_node, preceding_node, same_node};
use crate::xpath::operators::{
    eval_binary, eval_range, eval_unary, general_eq_iter, general_ge_iter, general_gt_iter,
    general_le_iter, general_lt_iter, general_ne_iter,
};
use crate::xpath::sequence_ops::{except_nodes, intersect_nodes, union_nodes};
use crate::xpath::DomNavigator;

/// Evaluate an AST node and return the result.
///
/// This function recursively evaluates the AST, dispatching to appropriate
/// handlers based on node type. The AST must have been bound using `bind_node()`
/// before evaluation.
///
/// # Arguments
/// * `arena` - The AST arena containing all nodes
/// * `id` - The ID of the node to evaluate
/// * `ctx` - The dynamic context for evaluation
///
/// # Returns
/// * `Ok(XPathValue)` containing the evaluation result
/// * `Err(XPathError)` if evaluation fails
///
/// # Errors
/// * `XPDY0002` - Context item is undefined when required
/// * `XPST0008` - Variable is not bound
/// * Various function-specific errors
pub fn eval_node<N: DomNavigator>(
    arena: &AstArena,
    id: AstNodeId,
    ctx: &mut DynamicContext<'_, N>,
) -> Result<XPathValue<N>, XPathError> {
    let node = arena.get(id);

    match node {
        AstNode::Expr(expr) => {
            // Evaluate all items and concatenate results
            if expr.items.is_empty() {
                return Ok(XPathValue::empty());
            }

            if expr.items.len() == 1 {
                // Single item - no concatenation needed
                return eval_node(arena, expr.items[0], ctx);
            }

            // Multiple items - collect all results
            let mut results: Vec<XmlItem<N>> = Vec::new();
            for item_id in &expr.items {
                let value = eval_node(arena, *item_id, ctx)?;
                results.extend(value.into_vec());
            }
            Ok(XPathValue::from_sequence(results))
        }

        AstNode::Value(value_node) => {
            // Convert ValueNode to XPathValue
            eval_value(value_node)
        }

        AstNode::ContextItem(_) => {
            // Return the context item, or error if undefined
            match &ctx.context_item {
                Some(item) => Ok(XPathValue::from_item(item.clone())),
                None => Err(XPathError::XPDY0002 {
                    message: "Context item is undefined".to_string(),
                }),
            }
        }

        AstNode::VarRef(var_ref) => {
            // Get the variable value from the context
            let slot = var_ref.slot.ok_or_else(|| XPathError::Internal(
                "Variable reference not bound".to_string(),
            ))?;

            ctx.get_variable(slot)
                .cloned()
                .ok_or_else(|| XPathError::XPDY0002 {
                    message: format!("Variable ${} is not set", var_ref.local_name),
                })
        }

        AstNode::If(if_node) => {
            // Evaluate condition and return appropriate branch
            let test_value = eval_node(arena, if_node.test, ctx)?;
            let condition = effective_boolean_value(&test_value)?;

            if condition {
                eval_node(arena, if_node.then_branch, ctx)
            } else {
                eval_node(arena, if_node.else_branch, ctx)
            }
        }

        AstNode::FunctionCall(func_call) => {
            // Get the resolved function ID
            let function_id = func_call.function_id.ok_or_else(|| {
                XPathError::Internal("Function call not bound".to_string())
            })?;

            // Evaluate all arguments
            let mut args: Vec<XPathValue<N>> = Vec::with_capacity(func_call.args.len());
            for arg_id in &func_call.args {
                args.push(eval_node(arena, *arg_id, ctx)?);
            }

            // Dispatch to the function
            eval_function(function_id, ctx, args)
        }

        AstNode::For(_) => {
            Err(XPathError::not_implemented("for expression evaluation"))
        }

        AstNode::Quantified(_) => {
            Err(XPathError::not_implemented("quantified expression evaluation"))
        }

        AstNode::PathExpr(_) => {
            Err(XPathError::not_implemented("path expression evaluation"))
        }

        AstNode::FilterExpr(_) => {
            Err(XPathError::not_implemented("filter expression evaluation"))
        }

        AstNode::Range(range) => {
            let start_val = eval_node(arena, range.start, ctx)?;
            let end_val = eval_node(arena, range.end, ctx)?;

            let start_opt = atomize_to_single_opt(start_val)?;
            let end_opt = atomize_to_single_opt(end_val)?;

            match (start_opt, end_opt) {
                (None, _) | (_, None) => Ok(XPathValue::empty()),
                (Some(start), Some(end)) => {
                    let values = eval_range(&start, &end)?;
                    let items: Vec<XmlItem<N>> = values.into_iter().map(XmlItem::Atomic).collect();
                    Ok(XPathValue::from_sequence(items))
                }
            }
        }

        AstNode::UnaryOp(unary_op) => {
            let operand_val = eval_node(arena, unary_op.operand, ctx)?;
            let opt = atomize_to_single_opt(operand_val)?;

            match opt {
                None => Ok(XPathValue::empty()),
                Some(operand) => {
                    let result = eval_unary(unary_op.kind, &operand)?;
                    Ok(XPathValue::from_atomic(result))
                }
            }
        }

        AstNode::BinaryOp(bin_op) => {
            match bin_op.kind {
                // Logical operators - short-circuit evaluation
                BinaryOpKind::And => {
                    let left_val = eval_node(arena, bin_op.left, ctx)?;
                    let left_bool = effective_boolean_value(&left_val)?;
                    if !left_bool {
                        return Ok(XPathValue::boolean(false));
                    }
                    let right_val = eval_node(arena, bin_op.right, ctx)?;
                    let right_bool = effective_boolean_value(&right_val)?;
                    Ok(XPathValue::boolean(right_bool))
                }
                BinaryOpKind::Or => {
                    let left_val = eval_node(arena, bin_op.left, ctx)?;
                    let left_bool = effective_boolean_value(&left_val)?;
                    if left_bool {
                        return Ok(XPathValue::boolean(true));
                    }
                    let right_val = eval_node(arena, bin_op.right, ctx)?;
                    let right_bool = effective_boolean_value(&right_val)?;
                    Ok(XPathValue::boolean(right_bool))
                }

                // Arithmetic and value comparison operators - atomize to single values
                BinaryOpKind::Add | BinaryOpKind::Sub | BinaryOpKind::Mul |
                BinaryOpKind::Div | BinaryOpKind::IDiv | BinaryOpKind::Mod |
                BinaryOpKind::ValueEq | BinaryOpKind::ValueNe |
                BinaryOpKind::ValueLt | BinaryOpKind::ValueLe |
                BinaryOpKind::ValueGt | BinaryOpKind::ValueGe => {
                    let left_val = eval_node(arena, bin_op.left, ctx)?;
                    let right_val = eval_node(arena, bin_op.right, ctx)?;

                    let left_opt = atomize_to_single_opt(left_val)?;
                    let right_opt = atomize_to_single_opt(right_val)?;

                    match (left_opt, right_opt) {
                        (None, _) | (_, None) => Ok(XPathValue::empty()),
                        (Some(left), Some(right)) => {
                            let result = eval_binary(bin_op.kind, &left, &right)?;
                            Ok(XPathValue::from_atomic(result))
                        }
                    }
                }

                // General comparisons - use Cartesian product semantics
                BinaryOpKind::GeneralEq | BinaryOpKind::GeneralNe |
                BinaryOpKind::GeneralLt | BinaryOpKind::GeneralLe |
                BinaryOpKind::GeneralGt | BinaryOpKind::GeneralGe => {
                    let left_val = eval_node(arena, bin_op.left, ctx)?;
                    let right_val = eval_node(arena, bin_op.right, ctx)?;

                    let left_iter = VecNodeIterator::new(left_val.into_vec());
                    let right_iter = VecNodeIterator::new(right_val.into_vec());

                    let result = match bin_op.kind {
                        BinaryOpKind::GeneralEq => general_eq_iter(ctx.static_context, &left_iter, &right_iter)?,
                        BinaryOpKind::GeneralNe => general_ne_iter(ctx.static_context, &left_iter, &right_iter)?,
                        BinaryOpKind::GeneralLt => general_lt_iter(ctx.static_context, &left_iter, &right_iter)?,
                        BinaryOpKind::GeneralLe => general_le_iter(ctx.static_context, &left_iter, &right_iter)?,
                        BinaryOpKind::GeneralGt => general_gt_iter(ctx.static_context, &left_iter, &right_iter)?,
                        BinaryOpKind::GeneralGe => general_ge_iter(ctx.static_context, &left_iter, &right_iter)?,
                        _ => unreachable!(),
                    };
                    Ok(XPathValue::boolean(result))
                }

                // Node comparisons - use node identity/document order
                BinaryOpKind::Is | BinaryOpKind::Before | BinaryOpKind::After => {
                    let left_val = eval_node(arena, bin_op.left, ctx)?;
                    let right_val = eval_node(arena, bin_op.right, ctx)?;

                    let left_node = extract_single_node(left_val)?;
                    let right_node = extract_single_node(right_val)?;

                    // Per XPath 2.0 spec: if either operand is empty, result is empty sequence
                    match (left_node, right_node) {
                        (Some(left), Some(right)) => {
                            let result = match bin_op.kind {
                                BinaryOpKind::Is => same_node(&left, &right),
                                BinaryOpKind::Before => preceding_node(&left, &right),
                                BinaryOpKind::After => following_node(&left, &right),
                                _ => unreachable!(),
                            };
                            Ok(XPathValue::boolean(result))
                        }
                        _ => Ok(XPathValue::empty()),
                    }
                }

                // Sequence operators - node-only, return document order with duplicates removed
                BinaryOpKind::Union | BinaryOpKind::Intersect | BinaryOpKind::Except => {
                    let left_val = eval_node(arena, bin_op.left, ctx)?;
                    let right_val = eval_node(arena, bin_op.right, ctx)?;

                    let left_vec = left_val.into_vec();
                    let right_vec = right_val.into_vec();

                    let result = match bin_op.kind {
                        BinaryOpKind::Union => union_nodes(left_vec, right_vec)?,
                        BinaryOpKind::Intersect => intersect_nodes(left_vec, right_vec)?,
                        BinaryOpKind::Except => except_nodes(left_vec, right_vec)?,
                        _ => unreachable!(),
                    };
                    Ok(XPathValue::from_sequence(result))
                }
            }
        }

        AstNode::PathStep(_) => {
            Err(XPathError::not_implemented("path step evaluation"))
        }

        AstNode::TypeExpr(_) => {
            Err(XPathError::not_implemented("type expression evaluation"))
        }
    }
}

/// Evaluate a ValueNode to an XPathValue.
fn eval_value<N: DomNavigator>(value: &ValueNode) -> Result<XPathValue<N>, XPathError> {
    match value {
        ValueNode::Empty => Ok(XPathValue::empty()),

        ValueNode::String(s) => Ok(XPathValue::string(s.clone())),

        ValueNode::Boolean(b) => Ok(XPathValue::boolean(*b)),

        ValueNode::Integer(s) => {
            // Parse integer string to BigInt
            let i: num_bigint::BigInt = s.parse().map_err(|_| {
                XPathError::FORG0001 {
                    value: s.clone(),
                    target_type: "xs:integer".to_string(),
                }
            })?;
            Ok(XPathValue::integer(i))
        }

        ValueNode::Decimal(s) => {
            // For now, treat decimal as double
            let d: f64 = s.parse().unwrap_or(f64::NAN);
            Ok(XPathValue::double(d))
        }

        ValueNode::Double(s) => {
            let d: f64 = s.parse().unwrap_or(f64::NAN);
            Ok(XPathValue::double(d))
        }

        ValueNode::Typed(xml_value) => {
            Ok(XPathValue::from_atomic(xml_value.clone()))
        }
    }
}

/// Extract a single node from an XPathValue for node comparison operators.
/// Returns Ok(None) for empty sequence, Ok(Some(node)) for single node,
/// or Err for type errors (non-node or multiple items).
fn extract_single_node<N: DomNavigator>(value: XPathValue<N>) -> Result<Option<N>, XPathError> {
    match value {
        XPathValue::Empty => Ok(None),
        XPathValue::Item(XmlItem::Node(node)) => Ok(Some(node)),
        XPathValue::Item(XmlItem::Atomic(_)) => Err(XPathError::XPTY0004 {
            expected: "node()".to_string(),
            found: "atomic value".to_string(),
        }),
        XPathValue::Sequence(items) => {
            if items.len() == 1 {
                match items.into_iter().next().unwrap() {
                    XmlItem::Node(node) => Ok(Some(node)),
                    XmlItem::Atomic(_) => Err(XPathError::XPTY0004 {
                        expected: "node()".to_string(),
                        found: "atomic value".to_string(),
                    }),
                }
            } else if items.is_empty() {
                Ok(None)
            } else {
                Err(XPathError::more_than_one_item())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::namespace::table::NameTable;
    use crate::xpath::arena::SourceSpan;
    use crate::xpath::ast::{BinaryOpNode, ExprNode, FunctionCallNode, IfNode, RangeNode, UnaryOpNode, UnaryOpKind, ValueNode};
    use crate::xpath::bind::bind_node;
    use crate::xpath::context::{NameBinder, XPathContext};
    use crate::xpath::RoXmlNavigator;

    /// Helper to create a test arena with a function call
    fn make_function_call(arena: &mut AstArena, prefix: &str, local_name: &str, args: Vec<AstNodeId>) -> AstNodeId {
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
    fn bind_and_eval(arena: &mut AstArena, root: AstNodeId) -> Result<XPathValue<RoXmlNavigator<'static>>, XPathError> {
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
                assert_eq!(v.as_integer().map(|i| i.to_string()), Some("42".to_string()));
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
            DynamicContext::new(&ctx, binder.len())
                .with_position(3, 10);

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
                assert_eq!(v.as_integer().map(|i| i.to_string()), Some("10".to_string()));
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
                assert_eq!(v.as_integer().map(|i| i.to_string()), Some("15".to_string()));
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
                assert_eq!(v.as_integer().map(|i| i.to_string()), Some("-5".to_string()));
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
                assert_eq!(v.as_integer().map(|i| i.to_string()), Some("-5".to_string()));
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
        let left = arena.add(AstNode::ContextItem(crate::xpath::ast::ContextItemNode::new(span)));
        let right = arena.add(AstNode::ContextItem(crate::xpath::ast::ContextItemNode::new(span)));
        let bin_op = BinaryOpNode::new(BinaryOpKind::Is, left, right, span);
        let bin_id = arena.add(AstNode::BinaryOp(bin_op));
        let root = wrap_in_expr(&mut arena, bin_id);

        bind_node(&mut arena, root, &ctx, &mut binder).unwrap();

        let mut dyn_ctx = DynamicContext::new(&ctx, binder.len())
            .with_context_item(XmlItem::Node(nav.clone()));

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
        let right = arena.add(AstNode::ContextItem(crate::xpath::ast::ContextItemNode::new(span)));
        let bin_op = BinaryOpNode::new(BinaryOpKind::Is, left, right, span);
        let bin_id = arena.add(AstNode::BinaryOp(bin_op));
        let root = wrap_in_expr(&mut arena, bin_id);

        bind_node(&mut arena, root, &ctx, &mut binder).unwrap();

        let mut dyn_ctx = DynamicContext::new(&ctx, binder.len())
            .with_context_item(XmlItem::Node(nav.clone()));

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
        let right = arena.add(AstNode::ContextItem(crate::xpath::ast::ContextItemNode::new(span)));
        let bin_op = BinaryOpNode::new(BinaryOpKind::Is, left, right, span);
        let bin_id = arena.add(AstNode::BinaryOp(bin_op));
        let root = wrap_in_expr(&mut arena, bin_id);

        bind_node(&mut arena, root, &ctx, &mut binder).unwrap();

        let mut dyn_ctx = DynamicContext::new(&ctx, binder.len())
            .with_context_item(XmlItem::Node(nav.clone()));

        let result = eval_node(&arena, root, &mut dyn_ctx);
        assert!(matches!(result, Err(XPathError::XPTY0004 { .. })));
    }

    #[test]
    #[ignore = "Requires path evaluation for different nodes"]
    fn test_node_before_after() {
        // $a << $b where a precedes b → true
        // $a >> $b where a follows b → true
        // These tests require path evaluation to get different nodes
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
        let left = arena.add(AstNode::ContextItem(crate::xpath::ast::ContextItemNode::new(span)));
        let right = arena.add(AstNode::ContextItem(crate::xpath::ast::ContextItemNode::new(span)));
        let bin_op = BinaryOpNode::new(BinaryOpKind::Union, left, right, span);
        let bin_id = arena.add(AstNode::BinaryOp(bin_op));
        let root = wrap_in_expr(&mut arena, bin_id);

        bind_node(&mut arena, root, &ctx, &mut binder).unwrap();

        let mut dyn_ctx = DynamicContext::new(&ctx, binder.len())
            .with_context_item(XmlItem::Node(nav.clone()));

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
        let left = arena.add(AstNode::ContextItem(crate::xpath::ast::ContextItemNode::new(span)));
        let right = arena.add(AstNode::ContextItem(crate::xpath::ast::ContextItemNode::new(span)));
        let bin_op = BinaryOpNode::new(BinaryOpKind::Intersect, left, right, span);
        let bin_id = arena.add(AstNode::BinaryOp(bin_op));
        let root = wrap_in_expr(&mut arena, bin_id);

        bind_node(&mut arena, root, &ctx, &mut binder).unwrap();

        let mut dyn_ctx = DynamicContext::new(&ctx, binder.len())
            .with_context_item(XmlItem::Node(nav.clone()));

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
        let left = arena.add(AstNode::ContextItem(crate::xpath::ast::ContextItemNode::new(span)));
        let right = arena.add(AstNode::ContextItem(crate::xpath::ast::ContextItemNode::new(span)));
        let bin_op = BinaryOpNode::new(BinaryOpKind::Except, left, right, span);
        let bin_id = arena.add(AstNode::BinaryOp(bin_op));
        let root = wrap_in_expr(&mut arena, bin_id);

        bind_node(&mut arena, root, &ctx, &mut binder).unwrap();

        let mut dyn_ctx = DynamicContext::new(&ctx, binder.len())
            .with_context_item(XmlItem::Node(nav.clone()));

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
}

// ============================================================================
// Integration Tests (Parse -> Bind -> Eval)
// ============================================================================

#[cfg(test)]
mod integration_tests {
    //! Integration tests for the full parse -> bind -> eval pipeline.
    //!
    //! These tests are marked `#[ignore]` until the XPath parser is implemented.
    //! They serve as a specification for the expected behavior of the full pipeline.

    #[test]
    #[ignore = "Requires parser implementation"]
    fn test_parse_bind_eval_if_expression() {
        // let result = eval_xpath("if (true()) then 'yes' else 'no'");
        // assert_eq!(result.unwrap().as_string(), Some("yes".to_string()));
    }

    #[test]
    #[ignore = "Requires parser implementation"]
    fn test_parse_bind_eval_nested_functions() {
        // let result = eval_xpath("upper-case(concat('a', 'b'))");
        // assert_eq!(result.unwrap().as_string(), Some("AB".to_string()));
    }

    #[test]
    #[ignore = "Requires parser implementation"]
    fn test_parse_bind_eval_variable_reference() {
        // let result = eval_xpath_with_var("$x + 1", "x", 5);
        // assert_eq!(result.unwrap().as_integer().map(|i| i.to_string()), Some("6".to_string()));
    }

    #[test]
    #[ignore = "Requires parser implementation"]
    fn test_parse_bind_eval_comparison() {
        // assert_eq!(eval_xpath("1 = 1").unwrap().as_boolean(), Some(true));
        // assert_eq!(eval_xpath("1 = 2").unwrap().as_boolean(), Some(false));
        // assert_eq!(eval_xpath("1 < 2").unwrap().as_boolean(), Some(true));
    }

    #[test]
    #[ignore = "Requires parser implementation"]
    fn test_parse_bind_eval_arithmetic() {
        // assert_eq!(eval_xpath("1 + 2").unwrap().as_integer().map(|i| i.to_string()), Some("3".to_string()));
        // assert_eq!(eval_xpath("5 - 3").unwrap().as_integer().map(|i| i.to_string()), Some("2".to_string()));
        // assert_eq!(eval_xpath("2 * 3").unwrap().as_integer().map(|i| i.to_string()), Some("6".to_string()));
    }
}
