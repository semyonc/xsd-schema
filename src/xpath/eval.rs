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
use crate::xpath::ast::{AstNode, ValueNode};
use crate::xpath::context::DynamicContext;
use crate::xpath::error::XPathError;
use crate::xpath::functions::{eval_function, effective_boolean_value, XPathValue};
use crate::xpath::iterator::XmlItem;
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

        AstNode::Range(_) => {
            Err(XPathError::not_implemented("range expression evaluation"))
        }

        AstNode::UnaryOp(_) => {
            Err(XPathError::not_implemented("unary operator evaluation"))
        }

        AstNode::BinaryOp(_) => {
            Err(XPathError::not_implemented("binary operator evaluation"))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::namespace::table::NameTable;
    use crate::xpath::arena::SourceSpan;
    use crate::xpath::ast::{ExprNode, FunctionCallNode, IfNode, ValueNode};
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
        let val = arena.add(AstNode::Value(ValueNode::Double("3.14".to_string())));
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
