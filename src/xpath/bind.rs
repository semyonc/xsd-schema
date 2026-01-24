//! XPath AST binding phase.
//!
//! This module provides the `bind_node()` function which performs static analysis
//! on a parsed XPath AST. During binding:
//!
//! - Function calls are resolved to `FunctionId` via the global registry
//! - Variable references are resolved to slot indices via `NameBinder`
//! - Namespace prefixes are resolved to namespace URIs
//!
//! Binding must complete successfully before evaluation can proceed.

use crate::namespace::qname::QualifiedName;
use crate::xpath::arena::{AstArena, AstNodeId};
use crate::xpath::ast::AstNode;
use crate::xpath::context::{NameBinder, XPathContext};
use crate::xpath::error::XPathError;
use crate::xpath::functions::FUNCTION_REGISTRY;

/// Bind an AST node and all its children.
///
/// This function performs static analysis on the AST:
/// - Resolves function calls to `FunctionId`
/// - Resolves variable references to slot indices
/// - Validates namespace prefixes
///
/// # Arguments
/// * `arena` - The AST arena containing all nodes
/// * `id` - The ID of the node to bind
/// * `ctx` - The static context for namespace resolution
/// * `binder` - The name binder for variable slot allocation
///
/// # Returns
/// * `Ok(())` if binding succeeds
/// * `Err(XPathError)` with appropriate error code if binding fails
///
/// # Errors
/// * `XPST0081` - Undefined namespace prefix
/// * `XPST0017` - Function not found
/// * `XPST0008` - Undefined variable
pub fn bind_node(
    arena: &mut AstArena,
    id: AstNodeId,
    ctx: &XPathContext<'_>,
    binder: &mut NameBinder,
) -> Result<(), XPathError> {
    // Clone the node to avoid borrow conflicts
    let node = arena.get(id).clone();

    match node {
        AstNode::Expr(expr) => {
            // Bind all items in the expression sequence
            for item_id in &expr.items {
                bind_node(arena, *item_id, ctx, binder)?;
            }
        }

        AstNode::Value(_) => {
            // Literal values need no binding
        }

        AstNode::ContextItem(_) => {
            // Context item needs no binding
        }

        AstNode::VarRef(var_ref) => {
            // Resolve the variable to a slot
            let name = resolve_var_qname(&var_ref.prefix, &var_ref.local_name, ctx)?;
            let var = binder.resolve_with_names(&name, ctx.names)?;

            // Update the node with the resolved slot
            if let AstNode::VarRef(ref mut node) = arena.get_mut(id) {
                node.slot = Some(var.slot);
            }
        }

        AstNode::If(if_node) => {
            // Bind test, then, and else branches
            bind_node(arena, if_node.test, ctx, binder)?;
            bind_node(arena, if_node.then_branch, ctx, binder)?;
            bind_node(arena, if_node.else_branch, ctx, binder)?;
        }

        AstNode::For(for_node) => {
            // For expressions introduce variables into scope
            // Each binding's in_expr is evaluated in the outer scope,
            // then the variable is pushed for the next binding and return_expr
            for binding in &for_node.bindings {
                // Bind the in_expr in current scope
                bind_node(arena, binding.in_expr, ctx, binder)?;

                // Push the variable into scope
                let name = resolve_var_qname(&binding.prefix, &binding.local_name, ctx)?;
                let var = binder.push_var(name);

                // Update the binding with the resolved slot
                if let AstNode::For(ref mut node) = arena.get_mut(id) {
                    for b in &mut node.bindings {
                        if b.local_name == binding.local_name && b.prefix == binding.prefix {
                            b.slot = Some(var.slot);
                            break;
                        }
                    }
                }
            }

            // Bind the return expression with all variables in scope
            bind_node(arena, for_node.return_expr, ctx, binder)?;

            // Pop all the variables (in reverse order)
            for _ in &for_node.bindings {
                binder.pop_var();
            }
        }

        AstNode::Quantified(quant_node) => {
            // Similar to for expressions
            for binding in &quant_node.bindings {
                bind_node(arena, binding.in_expr, ctx, binder)?;

                let name = resolve_var_qname(&binding.prefix, &binding.local_name, ctx)?;
                let var = binder.push_var(name);

                if let AstNode::Quantified(ref mut node) = arena.get_mut(id) {
                    for b in &mut node.bindings {
                        if b.local_name == binding.local_name && b.prefix == binding.prefix {
                            b.slot = Some(var.slot);
                            break;
                        }
                    }
                }
            }

            bind_node(arena, quant_node.satisfies, ctx, binder)?;

            for _ in &quant_node.bindings {
                binder.pop_var();
            }
        }

        AstNode::FunctionCall(func_call) => {
            // First bind all argument expressions
            for arg_id in &func_call.args {
                bind_node(arena, *arg_id, ctx, binder)?;
            }

            // Resolve the function namespace
            let namespace = if func_call.prefix.is_empty() {
                // Empty prefix -> use default function namespace
                ctx.default_function_namespace().to_string()
            } else {
                // Resolve the prefix to a namespace URI
                ctx.resolve_prefix(&func_call.prefix)
                    .ok_or_else(|| XPathError::undefined_prefix(&func_call.prefix))?
                    .to_string()
            };

            // Look up the function in the registry
            let arity = func_call.args.len();
            let entry = FUNCTION_REGISTRY
                .lookup(&namespace, &func_call.local_name, arity)
                .ok_or_else(|| {
                    XPathError::function_not_found(&func_call.local_name, arity, &namespace)
                })?;

            // Store the resolved function ID
            if let AstNode::FunctionCall(ref mut node) = arena.get_mut(id) {
                node.function_id = Some(entry.id);
            }
        }

        AstNode::PathExpr(path_expr) => {
            // Bind all steps in the path
            for step_id in &path_expr.steps {
                bind_node(arena, *step_id, ctx, binder)?;
            }
        }

        AstNode::FilterExpr(filter_expr) => {
            // Bind the base expression and all predicates
            bind_node(arena, filter_expr.base, ctx, binder)?;
            for pred_id in &filter_expr.predicates {
                bind_node(arena, *pred_id, ctx, binder)?;
            }
        }

        AstNode::Range(range_node) => {
            bind_node(arena, range_node.start, ctx, binder)?;
            bind_node(arena, range_node.end, ctx, binder)?;
        }

        AstNode::UnaryOp(unary_op) => {
            bind_node(arena, unary_op.operand, ctx, binder)?;
        }

        AstNode::BinaryOp(binary_op) => {
            bind_node(arena, binary_op.left, ctx, binder)?;
            bind_node(arena, binary_op.right, ctx, binder)?;
        }

        AstNode::PathStep(path_step) => {
            // Bind predicates in the step
            for pred_id in &path_step.predicates {
                bind_node(arena, *pred_id, ctx, binder)?;
            }
        }

        AstNode::TypeExpr(type_expr) => {
            bind_node(arena, type_expr.operand, ctx, binder)?;
        }
    }

    Ok(())
}

/// Resolve a variable QName from prefix and local name.
fn resolve_var_qname(
    prefix: &str,
    local_name: &str,
    ctx: &XPathContext<'_>,
) -> Result<QualifiedName, XPathError> {
    // Get or intern the local name
    let local_id = ctx.names.get(local_name).ok_or_else(|| {
        // If the name isn't in the table, we can still create a QName with a temporary
        // For now, return an error indicating the variable is not defined
        XPathError::XPST0008 {
            qname: format!(
                "{}{}",
                if prefix.is_empty() {
                    String::new()
                } else {
                    format!("{}:", prefix)
                },
                local_name
            ),
        }
    })?;

    if prefix.is_empty() {
        Ok(QualifiedName::local(local_id))
    } else {
        // Resolve the prefix to a namespace
        let prefix_id = ctx.names.get(prefix).ok_or_else(|| {
            XPathError::undefined_prefix(prefix)
        })?;

        let ns_id = ctx.resolve_prefix_id(prefix_id).ok_or_else(|| {
            XPathError::undefined_prefix(prefix)
        })?;

        Ok(QualifiedName::new(Some(ns_id), local_id, Some(prefix_id)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::namespace::table::NameTable;
    use crate::xpath::arena::SourceSpan;
    use crate::xpath::ast::{ExprNode, FunctionCallNode, IfNode, ValueNode};
    use crate::xpath::functions::FunctionId;

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

    #[test]
    fn test_bind_function_call_default_ns() {
        let names = NameTable::new();
        let ctx = XPathContext::new(&names);
        let mut binder = NameBinder::new();

        let mut arena = AstArena::new();
        // Create string literal arguments
        let arg1 = arena.add(AstNode::Value(ValueNode::String("a".to_string())));
        let arg2 = arena.add(AstNode::Value(ValueNode::String("b".to_string())));
        // Create concat('a', 'b') function call
        let func_id = make_function_call(&mut arena, "", "concat", vec![arg1, arg2]);
        let root = wrap_in_expr(&mut arena, func_id);

        bind_node(&mut arena, root, &ctx, &mut binder).expect("bind failed");

        // Verify the function call has function_id set
        if let AstNode::FunctionCall(func) = arena.get(func_id) {
            assert_eq!(func.function_id, Some(FunctionId::Concat));
        } else {
            panic!("Expected FunctionCall node");
        }
    }

    #[test]
    fn test_bind_true_false() {
        let names = NameTable::new();
        let ctx = XPathContext::new(&names);
        let mut binder = NameBinder::new();

        let mut arena = AstArena::new();
        // Create true() function call
        let func_id = make_function_call(&mut arena, "", "true", vec![]);
        let root = wrap_in_expr(&mut arena, func_id);

        bind_node(&mut arena, root, &ctx, &mut binder).expect("bind failed");

        if let AstNode::FunctionCall(func) = arena.get(func_id) {
            assert_eq!(func.function_id, Some(FunctionId::True));
        } else {
            panic!("Expected FunctionCall node");
        }

        // Test false()
        let mut arena = AstArena::new();
        let func_id = make_function_call(&mut arena, "", "false", vec![]);
        let root = wrap_in_expr(&mut arena, func_id);

        bind_node(&mut arena, root, &ctx, &mut binder).expect("bind failed");

        if let AstNode::FunctionCall(func) = arena.get(func_id) {
            assert_eq!(func.function_id, Some(FunctionId::False));
        } else {
            panic!("Expected FunctionCall node");
        }
    }

    #[test]
    fn test_bind_function_not_found() {
        let names = NameTable::new();
        let ctx = XPathContext::new(&names);
        let mut binder = NameBinder::new();

        let mut arena = AstArena::new();
        // Create unknown() function call
        let func_id = make_function_call(&mut arena, "", "unknown", vec![]);
        let root = wrap_in_expr(&mut arena, func_id);

        let result = bind_node(&mut arena, root, &ctx, &mut binder);

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.error_code(), Some("XPST0017"));
    }

    #[test]
    fn test_bind_undefined_prefix() {
        let names = NameTable::new();
        let ctx = XPathContext::new(&names);
        let mut binder = NameBinder::new();

        let mut arena = AstArena::new();
        // Create foo:bar() function call with undefined prefix
        let func_id = make_function_call(&mut arena, "foo", "bar", vec![]);
        let root = wrap_in_expr(&mut arena, func_id);

        let result = bind_node(&mut arena, root, &ctx, &mut binder);

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.error_code(), Some("XPST0081"));
    }

    #[test]
    fn test_bind_nested_function() {
        let names = NameTable::new();
        let ctx = XPathContext::new(&names);
        let mut binder = NameBinder::new();

        let mut arena = AstArena::new();
        // Create concat('a', 'b')
        let arg1 = arena.add(AstNode::Value(ValueNode::String("a".to_string())));
        let arg2 = arena.add(AstNode::Value(ValueNode::String("b".to_string())));
        let inner_func = make_function_call(&mut arena, "", "concat", vec![arg1, arg2]);
        // Create upper-case(concat('a', 'b'))
        let outer_func = make_function_call(&mut arena, "", "upper-case", vec![inner_func]);
        let root = wrap_in_expr(&mut arena, outer_func);

        bind_node(&mut arena, root, &ctx, &mut binder).expect("bind failed");

        // The outer function should be upper-case
        if let AstNode::FunctionCall(func) = arena.get(outer_func) {
            assert_eq!(func.function_id, Some(FunctionId::UpperCase));
        }

        // The inner function should be concat
        if let AstNode::FunctionCall(func) = arena.get(inner_func) {
            assert_eq!(func.function_id, Some(FunctionId::Concat));
        }
    }

    #[test]
    fn test_bind_if_expression() {
        let names = NameTable::new();
        let ctx = XPathContext::new(&names);
        let mut binder = NameBinder::new();

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

        bind_node(&mut arena, root, &ctx, &mut binder).expect("bind failed");

        // Verify the condition function was bound
        if let AstNode::FunctionCall(func) = arena.get(test_func) {
            assert_eq!(func.function_id, Some(FunctionId::True));
        }
    }

    #[test]
    fn test_bind_literal() {
        let names = NameTable::new();
        let ctx = XPathContext::new(&names);
        let mut binder = NameBinder::new();

        let mut arena = AstArena::new();
        let val = arena.add(AstNode::Value(ValueNode::Integer("42".to_string())));
        let root = wrap_in_expr(&mut arena, val);

        // Binding literals should succeed (no-op)
        bind_node(&mut arena, root, &ctx, &mut binder).expect("bind failed");
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
    fn test_parse_bind_eval_concat() {
        // let result = eval_xpath("concat('a', 'b')");
        // assert_eq!(result.unwrap().as_string(), Some("ab".to_string()));
    }

    #[test]
    #[ignore = "Requires parser implementation"]
    fn test_parse_bind_eval_string_length() {
        // let result = eval_xpath("string-length('hello')");
        // assert_eq!(result.unwrap().as_integer().map(|i| i.to_string()), Some("5".to_string()));
    }

    #[test]
    #[ignore = "Requires parser implementation"]
    fn test_parse_bind_eval_substring() {
        // let result = eval_xpath("substring('hello', 2, 3)");
        // assert_eq!(result.unwrap().as_string(), Some("ell".to_string()));
    }

    #[test]
    #[ignore = "Requires parser implementation"]
    fn test_parse_bind_eval_upper_case() {
        // let result = eval_xpath("upper-case('hello')");
        // assert_eq!(result.unwrap().as_string(), Some("HELLO".to_string()));
    }

    #[test]
    #[ignore = "Requires parser implementation"]
    fn test_parse_bind_eval_boolean_functions() {
        // assert_eq!(eval_xpath("true()").unwrap().as_boolean(), Some(true));
        // assert_eq!(eval_xpath("false()").unwrap().as_boolean(), Some(false));
        // assert_eq!(eval_xpath("not(true())").unwrap().as_boolean(), Some(false));
    }

    #[test]
    #[ignore = "Requires parser implementation"]
    fn test_parse_bind_eval_numeric_functions() {
        // assert_eq!(eval_xpath("abs(-5)").unwrap().as_double(), Some(5.0));
        // assert_eq!(eval_xpath("ceiling(1.5)").unwrap().as_double(), Some(2.0));
        // assert_eq!(eval_xpath("floor(1.5)").unwrap().as_double(), Some(1.0));
        // assert_eq!(eval_xpath("round(1.5)").unwrap().as_double(), Some(2.0));
    }

    #[test]
    #[ignore = "Requires parser implementation"]
    fn test_parse_bind_eval_sequence_functions() {
        // assert_eq!(eval_xpath("empty(())").unwrap().as_boolean(), Some(true));
        // assert_eq!(eval_xpath("exists(1)").unwrap().as_boolean(), Some(true));
        // assert_eq!(eval_xpath("count((1, 2, 3))").unwrap().as_integer().map(|i| i.to_string()), Some("3".to_string()));
    }
}
