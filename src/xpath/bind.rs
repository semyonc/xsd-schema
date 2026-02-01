//! XPath AST binding phase.
//!
//! This module provides the `bind_node()` function which performs static analysis
//! on a parsed XPath AST. During binding:
//!
//! - Function calls are resolved to `FunctionId` via the global registry
//! - Variable references are resolved to slot indices via `NameBinder`
//! - Namespace prefixes are resolved to namespace URIs
//! - Name tests are resolved to interned QNames
//! - Type expressions are resolved to interned atomic type QNames
//!
//! Binding must complete successfully before evaluation can proceed.

use crate::namespace::qname::QualifiedName;
use crate::types::NameTest as ResolvedNameTest;
use crate::xpath::arena::{AstArena, AstNodeId};
use crate::xpath::ast::{AstNode, ItemTypeNode, NameTest, NodeTest, QName};
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

            // Resolve the name test if present
            let resolved = resolve_node_test(&path_step.test, ctx)?;
            if let AstNode::PathStep(ref mut node) = arena.get_mut(id) {
                node.resolved_test = resolved;
            }
        }

        AstNode::TypeExpr(type_expr) => {
            bind_node(arena, type_expr.operand, ctx, binder)?;

            // Resolve atomic type QName if present
            if let Some(ItemTypeNode::Atomic(ref qname)) = type_expr.target_type.item_type {
                let resolved = resolve_atomic_type_qname(qname, ctx)?;
                if let AstNode::TypeExpr(ref mut node) = arena.get_mut(id) {
                    node.resolved_atomic_type = Some(resolved);
                }
            }
        }
    }

    Ok(())
}

/// Resolve a variable QName from prefix and local name.
///
/// This function interns the local name and prefix into the NameTable using `add()`,
/// which always succeeds (returning existing NameId or creating new one).
/// Only namespace prefix resolution can fail if the prefix is not bound.
fn resolve_var_qname(
    prefix: &str,
    local_name: &str,
    ctx: &XPathContext<'_>,
) -> Result<QualifiedName, XPathError> {
    // Intern the local name (always succeeds)
    let local_id = ctx.names.add(local_name);

    if prefix.is_empty() {
        Ok(QualifiedName::local(local_id))
    } else {
        // Intern the prefix
        let prefix_id = ctx.names.add(prefix);

        // Resolve prefix to namespace - THIS can still fail legitimately
        let ns_id = ctx.resolve_prefix_id(prefix_id).ok_or_else(|| {
            XPathError::undefined_prefix(prefix)
        })?;

        Ok(QualifiedName::new(Some(ns_id), local_id, Some(prefix_id)))
    }
}

/// Resolve a NodeTest to a ResolvedNameTest.
///
/// For Name tests, converts AST-level NameTest (strings) to type-system NameTest (NameIds).
/// For Kind tests, returns None (kind tests don't need name resolution at this level).
fn resolve_node_test(
    test: &NodeTest,
    ctx: &XPathContext<'_>,
) -> Result<Option<ResolvedNameTest>, XPathError> {
    match test {
        NodeTest::Name(name_test) => {
            let resolved = resolve_name_test(name_test, ctx)?;
            Ok(Some(resolved))
        }
        NodeTest::Kind(_) => {
            // Kind tests (node(), text(), element(), etc.) don't need name resolution
            // The QNames inside element()/attribute() tests could be resolved,
            // but that's handled separately during evaluation
            Ok(None)
        }
    }
}

/// Resolve an AST-level NameTest to a type-system NameTest with interned names.
///
/// Handles all wildcard patterns:
/// - `*` -> Wildcard
/// - `prefix:*` -> LocalWildcard (namespace URI)
/// - `*:local` -> NamespaceWildcard (local name)
/// - `prefix:local` or `local` -> QName
fn resolve_name_test(
    name_test: &NameTest,
    ctx: &XPathContext<'_>,
) -> Result<ResolvedNameTest, XPathError> {
    match (&name_test.prefix, &name_test.local_name) {
        // * - wildcard matches any name
        (None, None) => Ok(ResolvedNameTest::Wildcard),

        // *:local - any namespace with specific local name
        (None, Some(local)) => {
            let local_id = ctx.names.add(local);
            Ok(ResolvedNameTest::NamespaceWildcard(local_id))
        }

        // prefix:* - any local name in namespace
        (Some(prefix), None) => {
            if prefix.is_empty() {
                // Empty prefix with wildcard local = default namespace wildcard
                // Use default element namespace if set
                if let Some(ns_id) = ctx.default_element_ns {
                    Ok(ResolvedNameTest::LocalWildcard(ns_id))
                } else {
                    // No default namespace - matches no-namespace elements
                    // Use empty string as namespace
                    let empty_ns = ctx.names.add("");
                    Ok(ResolvedNameTest::LocalWildcard(empty_ns))
                }
            } else {
                let prefix_id = ctx.names.add(prefix);
                let ns_id = ctx.resolve_prefix_id(prefix_id).ok_or_else(|| {
                    XPathError::undefined_prefix(prefix)
                })?;
                Ok(ResolvedNameTest::LocalWildcard(ns_id))
            }
        }

        // prefix:local - specific QName
        (Some(prefix), Some(local)) => {
            let local_id = ctx.names.add(local);
            if prefix.is_empty() {
                // No prefix - use default element namespace if set
                let ns_id = ctx.default_element_ns;
                Ok(ResolvedNameTest::QName(QualifiedName::new(ns_id, local_id, None)))
            } else {
                let prefix_id = ctx.names.add(prefix);
                let ns_id = ctx.resolve_prefix_id(prefix_id).ok_or_else(|| {
                    XPathError::undefined_prefix(prefix)
                })?;
                Ok(ResolvedNameTest::QName(QualifiedName::new(Some(ns_id), local_id, Some(prefix_id))))
            }
        }
    }
}

/// Resolve an atomic type QName (e.g., xs:integer) to interned form.
///
/// Atomic types use the XML Schema namespace by default when unprefixed.
fn resolve_atomic_type_qname(
    qname: &QName,
    ctx: &XPathContext<'_>,
) -> Result<QualifiedName, XPathError> {
    let local_id = ctx.names.add(&qname.local);

    if qname.prefix.is_empty() {
        // Unprefixed atomic types: in XPath 2.0, unprefixed type names in
        // cast/instance-of use the default element namespace, not xs:
        // But for compatibility, many implementations treat them as xs: types
        // Use default element namespace if set, otherwise no namespace
        let ns_id = ctx.default_element_ns;
        Ok(QualifiedName::new(ns_id, local_id, None))
    } else {
        let prefix_id = ctx.names.add(&qname.prefix);
        let ns_id = ctx.resolve_prefix_id(prefix_id).ok_or_else(|| {
            XPathError::undefined_prefix(&qname.prefix)
        })?;
        Ok(QualifiedName::new(Some(ns_id), local_id, Some(prefix_id)))
    }
}

#[cfg(test)]
#[path = "bind_tests.rs"]
mod bind_tests;
