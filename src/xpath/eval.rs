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
use crate::xpath::ast::{
    AstNode, Axis, BinaryOpKind, FilterExprNode, ForBinding, ForNode, ItemTypeNode,
    KindTest, NodeTest as AstNodeTest, OccurrenceIndicator, PathExprNode, PathStepNode,
    QuantifiedNode, QuantifierKind, TypeExprKind, TypeExprNode, ValueNode,
};
use crate::xpath::axis_iterators::{
    AncestorAxis, AttributeAxis, ChildAxis, DescendantNodeIterator, FollowingNodeIterator,
    FollowingSiblingAxis, NamespaceAxis, ParentAxis, PrecedingNodeIterator, PrecedingSiblingAxis,
    SelfAxis, SequentialAxisNodeIterator,
};
use crate::xpath::cast::{cast_to, castable, occurrence_allows_count, resolved_type_to_type_code};
use crate::xpath::context::{DynamicContext, XPathContext};
use crate::xpath::error::XPathError;
use crate::xpath::functions::{
    atomize_to_single_opt, effective_boolean_value, effective_boolean_value_10, XPathValue,
};
use crate::xpath::iterator::{DocumentOrderNodeIterator, VecNodeIterator, XmlItem, XmlNodeIterator};
use crate::xpath::node_ops::{following_node, get_root, preceding_node, same_node};
use crate::xpath::node_test::{matches_item_type_node, NodeTest};
use crate::xpath::operators::{
    eval_binary, eval_numeric_binary_10, eval_range, eval_unary, general_eq_iter,
    general_eq_iter_10, general_ge_iter, general_ge_iter_10, general_gt_iter, general_gt_iter_10,
    general_le_iter, general_le_iter_10, general_lt_iter, general_lt_iter_10, general_ne_iter,
    general_ne_iter_10,
};
use crate::xpath::sequence_ops::{except_nodes, intersect_nodes, union_nodes};
use crate::xpath::{DomNavigator, XPathMode};
use crate::types::{ItemType, NameTest as RuntimeNameTest, SequenceType};

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

            // XPath 1.0: comma-separated sequences are not allowed
            // (the comma operator only appears in function args, which use a separate grammar production)
            if ctx.static_context.mode() == XPathMode::XPath10 {
                return Err(XPathError::XPST0003 {
                    message: "Sequence expressions (comma operator) are not available in XPath 1.0".to_string(),
                });
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
            // XPath 1.0: reject constructs that slipped past the lexer
            if ctx.static_context.mode() == XPathMode::XPath10 {
                if matches!(value_node, ValueNode::Empty) {
                    return Err(XPathError::XPST0003 {
                        message: "Empty sequence () is not available in XPath 1.0".to_string(),
                    });
                }
                if matches!(value_node, ValueNode::Double(_)) {
                    return Err(XPathError::XPST0003 {
                        message: "Double literals (e.g. 1e10) are not available in XPath 1.0".to_string(),
                    });
                }
            }
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
            // Get the resolved function handle
            let handle = func_call.function_handle.ok_or_else(|| {
                XPathError::Internal("Function call not bound".to_string())
            })?;

            // Evaluate all arguments
            let mut args: Vec<XPathValue<N>> = Vec::with_capacity(func_call.args.len());
            for arg_id in &func_call.args {
                args.push(eval_node(arena, *arg_id, ctx)?);
            }

            // Dispatch via the context's eval_function method (supports custom functions)
            ctx.eval_function(handle, args)
        }

        AstNode::For(for_node) => {
            eval_for_expression(arena, for_node, ctx)
        }

        AstNode::Quantified(quant_node) => {
            eval_quantified_expression(arena, quant_node, ctx)
        }

        AstNode::PathExpr(path_expr) => eval_path_expr(arena, path_expr, ctx),

        AstNode::FilterExpr(filter_expr) => eval_filter_expr(arena, filter_expr, ctx),

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
                    let left_bool = if ctx.static_context.mode() == XPathMode::XPath10 {
                        effective_boolean_value_10(&left_val)?
                    } else {
                        effective_boolean_value(&left_val)?
                    };
                    if !left_bool {
                        return Ok(XPathValue::boolean(false));
                    }
                    let right_val = eval_node(arena, bin_op.right, ctx)?;
                    let right_bool = if ctx.static_context.mode() == XPathMode::XPath10 {
                        effective_boolean_value_10(&right_val)?
                    } else {
                        effective_boolean_value(&right_val)?
                    };
                    Ok(XPathValue::boolean(right_bool))
                }
                BinaryOpKind::Or => {
                    let left_val = eval_node(arena, bin_op.left, ctx)?;
                    let left_bool = if ctx.static_context.mode() == XPathMode::XPath10 {
                        effective_boolean_value_10(&left_val)?
                    } else {
                        effective_boolean_value(&left_val)?
                    };
                    if left_bool {
                        return Ok(XPathValue::boolean(true));
                    }
                    let right_val = eval_node(arena, bin_op.right, ctx)?;
                    let right_bool = if ctx.static_context.mode() == XPathMode::XPath10 {
                        effective_boolean_value_10(&right_val)?
                    } else {
                        effective_boolean_value(&right_val)?
                    };
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
                            let is_arithmetic = matches!(
                                bin_op.kind,
                                BinaryOpKind::Add | BinaryOpKind::Sub | BinaryOpKind::Mul |
                                BinaryOpKind::Div | BinaryOpKind::Mod
                            );
                            let result = if is_arithmetic && ctx.static_context.mode() == XPathMode::XPath10 {
                                eval_numeric_binary_10(bin_op.kind, &left, &right)?
                            } else {
                                eval_binary(bin_op.kind, &left, &right)?
                            };
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

                    // XPath 1.0 §3.4: node-set vs boolean → convert node-set to boolean as a whole
                    if ctx.static_context.mode() == XPathMode::XPath10 {
                        let left_is_bool = is_boolean_value(&left_val);
                        let right_is_bool = is_boolean_value(&right_val);
                        let left_has_nodes = has_nodes_or_empty(&left_val);
                        let right_has_nodes = has_nodes_or_empty(&right_val);

                        if (left_is_bool && right_has_nodes) || (right_is_bool && left_has_nodes) {
                            let l = effective_boolean_value_10(&left_val)?;
                            let r = effective_boolean_value_10(&right_val)?;
                            let result = match bin_op.kind {
                                BinaryOpKind::GeneralEq => l == r,
                                BinaryOpKind::GeneralNe => l != r,
                                BinaryOpKind::GeneralLt | BinaryOpKind::GeneralLe |
                                BinaryOpKind::GeneralGt | BinaryOpKind::GeneralGe => {
                                    let ln = if l { 1.0_f64 } else { 0.0 };
                                    let rn = if r { 1.0_f64 } else { 0.0 };
                                    match bin_op.kind {
                                        BinaryOpKind::GeneralLt => ln < rn,
                                        BinaryOpKind::GeneralLe => ln <= rn,
                                        BinaryOpKind::GeneralGt => ln > rn,
                                        BinaryOpKind::GeneralGe => ln >= rn,
                                        _ => unreachable!(),
                                    }
                                }
                                _ => unreachable!(),
                            };
                            return Ok(XPathValue::boolean(result));
                        }
                    }

                    let left_iter = VecNodeIterator::new(left_val.into_vec());
                    let right_iter = VecNodeIterator::new(right_val.into_vec());

                    let result = if ctx.static_context.mode() == XPathMode::XPath10 {
                        match bin_op.kind {
                            BinaryOpKind::GeneralEq => general_eq_iter_10(&left_iter, &right_iter)?,
                            BinaryOpKind::GeneralNe => general_ne_iter_10(&left_iter, &right_iter)?,
                            BinaryOpKind::GeneralLt => general_lt_iter_10(&left_iter, &right_iter)?,
                            BinaryOpKind::GeneralLe => general_le_iter_10(&left_iter, &right_iter)?,
                            BinaryOpKind::GeneralGt => general_gt_iter_10(&left_iter, &right_iter)?,
                            BinaryOpKind::GeneralGe => general_ge_iter_10(&left_iter, &right_iter)?,
                            _ => unreachable!(),
                        }
                    } else {
                        match bin_op.kind {
                            BinaryOpKind::GeneralEq => general_eq_iter(ctx.static_context, &left_iter, &right_iter)?,
                            BinaryOpKind::GeneralNe => general_ne_iter(ctx.static_context, &left_iter, &right_iter)?,
                            BinaryOpKind::GeneralLt => general_lt_iter(ctx.static_context, &left_iter, &right_iter)?,
                            BinaryOpKind::GeneralLe => general_le_iter(ctx.static_context, &left_iter, &right_iter)?,
                            BinaryOpKind::GeneralGt => general_gt_iter(ctx.static_context, &left_iter, &right_iter)?,
                            BinaryOpKind::GeneralGe => general_ge_iter(ctx.static_context, &left_iter, &right_iter)?,
                            _ => unreachable!(),
                        }
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
            // PathStep should not be evaluated directly - it's processed via eval_path_step
            Err(XPathError::Internal(
                "PathStep should not be evaluated directly".to_string(),
            ))
        }

        AstNode::TypeExpr(type_expr) => {
            eval_type_expr(arena, type_expr, ctx)
        }
    }
}

// ============================================================================
// Type Expression Evaluation
// ============================================================================

/// Evaluate a type expression (`instance of`, `treat as`, `cast as`, `castable as`).
fn eval_type_expr<N: DomNavigator>(
    arena: &AstArena,
    type_expr: &TypeExprNode,
    ctx: &mut DynamicContext<'_, N>,
) -> Result<XPathValue<N>, XPathError> {
    // Evaluate the operand
    let operand = eval_node(arena, type_expr.operand, ctx)?;

    match type_expr.kind {
        TypeExprKind::InstanceOf => eval_instance_of(operand, type_expr, ctx),
        TypeExprKind::TreatAs => eval_treat_as(operand, type_expr, ctx),
        TypeExprKind::CastAs => eval_cast_as(operand, type_expr, ctx),
        TypeExprKind::CastableAs => eval_castable_as(operand, type_expr, ctx),
    }
}

/// Evaluate `expr instance of type`.
///
/// Returns true if the value matches the sequence type (cardinality + item type).
fn eval_instance_of<N: DomNavigator>(
    operand: XPathValue<N>,
    type_expr: &TypeExprNode,
    ctx: &DynamicContext<'_, N>,
) -> Result<XPathValue<N>, XPathError> {
    let items = operand.into_vec();
    let count = items.len();

    // Handle empty-sequence() first (special case - only matches empty)
    if type_expr.target_type.item_type.is_none() {
        return Ok(XPathValue::boolean(count == 0));
    }

    // Check cardinality
    if !occurrence_allows_count(type_expr.target_type.occurrence, count) {
        return Ok(XPathValue::boolean(false));
    }

    // Get item type (we know it's Some from the check above)
    let item_type = type_expr.target_type.item_type.as_ref().unwrap();

    // Check each item matches the item type
    for item in &items {
        if !matches_item_type_node(
            item,
            item_type,
            type_expr.resolved_atomic_type.as_ref(),
            ctx.static_context,
        ) {
            return Ok(XPathValue::boolean(false));
        }
    }

    Ok(XPathValue::boolean(true))
}

/// Evaluate `expr treat as type`.
///
/// Returns the value unchanged if it matches the type, otherwise raises XPTY0004.
fn eval_treat_as<N: DomNavigator>(
    operand: XPathValue<N>,
    type_expr: &TypeExprNode,
    ctx: &DynamicContext<'_, N>,
) -> Result<XPathValue<N>, XPathError> {
    let items = operand.into_vec();
    let count = items.len();

    // Check cardinality
    if !occurrence_allows_count(type_expr.target_type.occurrence, count) {
        return Err(XPathError::XPTY0004 {
            expected: format_sequence_type(&type_expr.target_type, ctx),
            found: format!("sequence of {} items", count),
        });
    }

    // Handle empty-sequence()
    let item_type = match &type_expr.target_type.item_type {
        None => {
            // empty-sequence() - only accepts empty
            if count == 0 {
                return Ok(XPathValue::empty());
            } else {
                return Err(XPathError::XPTY0004 {
                    expected: "empty-sequence()".to_string(),
                    found: format!("sequence of {} items", count),
                });
            }
        }
        Some(it) => it,
    };

    // Check each item matches the item type
    for item in &items {
        if !matches_item_type_node(
            item,
            item_type,
            type_expr.resolved_atomic_type.as_ref(),
            ctx.static_context,
        ) {
            return Err(XPathError::XPTY0004 {
                expected: format_sequence_type(&type_expr.target_type, ctx),
                found: format_item_type(item),
            });
        }
    }

    // Return the original value
    Ok(XPathValue::from_sequence(items))
}

/// Evaluate `expr cast as type`.
///
/// Atomizes the operand and casts to the target atomic type.
fn eval_cast_as<N: DomNavigator>(
    operand: XPathValue<N>,
    type_expr: &TypeExprNode,
    ctx: &DynamicContext<'_, N>,
) -> Result<XPathValue<N>, XPathError> {
    // Cast only works with atomic types
    let item_type = type_expr.target_type.item_type.as_ref().ok_or_else(|| {
        XPathError::XPTY0004 {
            expected: "atomic type".to_string(),
            found: "empty-sequence()".to_string(),
        }
    })?;

    // The item type must be Atomic for cast
    if !matches!(item_type, ItemTypeNode::Atomic(_)) {
        return Err(XPathError::XPTY0004 {
            expected: "atomic type".to_string(),
            found: "non-atomic type".to_string(),
        });
    }

    // Atomize the operand to get at most one atomic value
    let atomic_opt = atomize_to_single_opt(operand)?;

    // Check cardinality
    let allows_empty = matches!(
        type_expr.target_type.occurrence,
        OccurrenceIndicator::ZeroOrOne | OccurrenceIndicator::ZeroOrMore
    );

    match atomic_opt {
        None => {
            if allows_empty {
                Ok(XPathValue::empty())
            } else {
                Err(XPathError::XPTY0004 {
                    expected: format_sequence_type(&type_expr.target_type, ctx),
                    found: "empty-sequence()".to_string(),
                })
            }
        }
        Some(value) => {
            // Get target type code from resolved QName
            let qname = type_expr.resolved_atomic_type.as_ref().ok_or_else(|| {
                XPathError::Internal("Cast target type not resolved".to_string())
            })?;
            let target_type = resolved_type_to_type_code(qname, ctx.static_context.names)?;

            // Perform the cast
            let result = cast_to(&value, target_type)?;
            Ok(XPathValue::from_atomic(result))
        }
    }
}

/// Evaluate `expr castable as type`.
///
/// Returns true if the cast would succeed, false otherwise.
fn eval_castable_as<N: DomNavigator>(
    operand: XPathValue<N>,
    type_expr: &TypeExprNode,
    ctx: &DynamicContext<'_, N>,
) -> Result<XPathValue<N>, XPathError> {
    // Cast only works with atomic types
    let item_type = type_expr.target_type.item_type.as_ref();
    if !matches!(item_type, Some(ItemTypeNode::Atomic(_))) {
        return Ok(XPathValue::boolean(false));
    }

    // Atomize the operand
    let atomic_opt = match atomize_to_single_opt(operand) {
        Ok(opt) => opt,
        Err(_) => return Ok(XPathValue::boolean(false)), // More than one item
    };

    // Check cardinality
    let allows_empty = matches!(
        type_expr.target_type.occurrence,
        OccurrenceIndicator::ZeroOrOne | OccurrenceIndicator::ZeroOrMore
    );

    match atomic_opt {
        None => {
            // Empty sequence - allowed if occurrence allows it
            Ok(XPathValue::boolean(allows_empty))
        }
        Some(value) => {
            // Get target type code from resolved QName
            let qname = match type_expr.resolved_atomic_type.as_ref() {
                Some(q) => q,
                None => return Ok(XPathValue::boolean(false)),
            };
            let target_type = match resolved_type_to_type_code(qname, ctx.static_context.names) {
                Ok(tc) => tc,
                Err(_) => return Ok(XPathValue::boolean(false)),
            };

            // Check if castable
            Ok(XPathValue::boolean(castable(&value, target_type)))
        }
    }
}

/// Format a sequence type for error messages.
fn format_sequence_type<N: DomNavigator>(
    seq_type: &crate::xpath::ast::SequenceTypeNode,
    _ctx: &DynamicContext<'_, N>,
) -> String {
    let item_str = match &seq_type.item_type {
        None => "empty-sequence()".to_string(),
        Some(ItemTypeNode::Item) => "item()".to_string(),
        Some(ItemTypeNode::Atomic(qname)) => {
            if qname.prefix.is_empty() {
                qname.local.clone()
            } else {
                format!("{}:{}", qname.prefix, qname.local)
            }
        }
        Some(ItemTypeNode::Kind(kind)) => format_kind_test(kind),
    };

    let occ_str = match seq_type.occurrence {
        OccurrenceIndicator::One => "",
        OccurrenceIndicator::ZeroOrOne => "?",
        OccurrenceIndicator::ZeroOrMore => "*",
        OccurrenceIndicator::OneOrMore => "+",
    };

    format!("{}{}", item_str, occ_str)
}

/// Format a kind test for error messages.
fn format_kind_test(kind: &crate::xpath::ast::KindTest) -> String {
    use crate::xpath::ast::KindTest;
    match kind {
        KindTest::AnyKind => "node()".to_string(),
        KindTest::Text => "text()".to_string(),
        KindTest::Comment => "comment()".to_string(),
        KindTest::ProcessingInstruction(None) => "processing-instruction()".to_string(),
        KindTest::ProcessingInstruction(Some(name)) => {
            format!("processing-instruction('{}')", name)
        }
        KindTest::Document(None) => "document-node()".to_string(),
        KindTest::Document(Some(inner)) => {
            format!("document-node({})", format_kind_test(inner))
        }
        KindTest::Element(test) => {
            if let Some(ref qname) = test.name {
                if qname.prefix.is_empty() {
                    format!("element({})", qname.local)
                } else {
                    format!("element({}:{})", qname.prefix, qname.local)
                }
            } else {
                "element()".to_string()
            }
        }
        KindTest::Attribute(test) => {
            if let Some(ref qname) = test.name {
                if qname.prefix.is_empty() {
                    format!("attribute({})", qname.local)
                } else {
                    format!("attribute({}:{})", qname.prefix, qname.local)
                }
            } else {
                "attribute()".to_string()
            }
        }
        KindTest::SchemaElement(name) => format!("schema-element({})", name),
        KindTest::SchemaAttribute(name) => format!("schema-attribute({})", name),
    }
}

/// Format an XmlItem type for error messages.
fn format_item_type<N: DomNavigator>(item: &XmlItem<N>) -> String {
    match item {
        XmlItem::Node(nav) => {
            use crate::xpath::DomNodeType;
            match nav.node_type() {
                DomNodeType::Root => "document-node()".to_string(),
                DomNodeType::Element => format!("element({})", nav.local_name()),
                DomNodeType::Attribute => format!("attribute({})", nav.local_name()),
                DomNodeType::Text
                | DomNodeType::Whitespace
                | DomNodeType::SignificantWhitespace => "text()".to_string(),
                DomNodeType::Comment => "comment()".to_string(),
                DomNodeType::ProcessingInstruction => "processing-instruction()".to_string(),
                DomNodeType::Namespace => "namespace-node()".to_string(),
                DomNodeType::All => "node()".to_string(),
            }
        }
        XmlItem::Atomic(value) => {
            format!("{:?}", value.type_code)
        }
    }
}

// ============================================================================
// Path Expression Evaluation
// ============================================================================

/// Evaluate a path expression.
///
/// Implements XPath 2.0 path expression semantics:
/// - Root-only path (`/`): Returns the document root
/// - Absolute paths (`/a/b`): Start from document root
/// - Relative paths (`a/b`): Start from context node
/// - Paths are evaluated left-to-right, chaining steps
fn eval_path_expr<N: DomNavigator>(
    arena: &AstArena,
    path_expr: &PathExprNode,
    ctx: &mut DynamicContext<'_, N>,
) -> Result<XPathValue<N>, XPathError> {
    // Handle root-only path: "/"
    if path_expr.is_absolute && path_expr.steps.is_empty() {
        let context_node = ctx.require_context_node()?;
        let root = get_root(context_node);
        return Ok(XPathValue::from_node(root));
    }

    // Check if the first step is a "primary expression" that doesn't require context.
    // This includes function calls, literals, parenthesized expressions, and variable references.
    // PathStep and ContextItem DO require context.
    let first_is_primary = path_expr.steps.first().is_some_and(|&step_id| {
        matches!(
            arena.get(step_id),
            AstNode::FilterExpr(_)
                | AstNode::FunctionCall(_)
                | AstNode::Value(_)
                | AstNode::Expr(_)
                | AstNode::VarRef(_)
        )
    });

    // Determine the starting nodes based on path type
    let starting_nodes: Vec<N> = if path_expr.is_absolute {
        // Absolute path: start from document root
        let context_node = ctx.require_context_node()?;
        vec![get_root(context_node)]
    } else if first_is_primary {
        // First step is a primary expression - no initial context nodes needed
        Vec::new()
    } else {
        // Relative path: start from context node
        let context_node = ctx.require_context_node()?;
        vec![context_node.clone()]
    };

    // Check if any step is a FilterExpr as the first step (special case)
    // or if this path might need document order sorting
    let needs_doc_order = path_needs_document_order(arena, path_expr);

    // Process steps sequentially
    let mut current_nodes: Vec<XmlItem<N>> =
        starting_nodes.into_iter().map(XmlItem::Node).collect();

    for (step_idx, &step_id) in path_expr.steps.iter().enumerate() {
        let step_node = arena.get(step_id);

        current_nodes = match step_node {
            AstNode::PathStep(path_step) => {
                eval_path_step(arena, path_step, current_nodes, ctx, step_idx == 0)?
            }
            AstNode::FilterExpr(filter_expr) => {
                // FilterExpr as a step - evaluate it and use its result
                if step_idx == 0 {
                    // First step is a FilterExpr - evaluate it directly
                    let result = eval_filter_expr(arena, filter_expr, ctx)?;
                    result.into_vec()
                } else {
                    // FilterExpr in a later position - this is applied to each node in sequence
                    let mut results = Vec::new();
                    for item in current_nodes {
                        // Set context to this item and evaluate the filter expression
                        let saved_context = ctx.context_item.take();
                        let saved_pos = ctx.context_position;
                        let saved_size = ctx.context_size;

                        ctx.context_item = Some(item);
                        ctx.context_position = 1;
                        ctx.context_size = 1;

                        let step_result = eval_filter_expr(arena, filter_expr, ctx)?;
                        results.extend(step_result.into_vec());

                        ctx.context_item = saved_context;
                        ctx.context_position = saved_pos;
                        ctx.context_size = saved_size;
                    }
                    results
                }
            }
            _ => {
                // Other expression types (like function calls, parenthesized exprs) as steps
                if step_idx == 0 && current_nodes.is_empty() {
                    // First step is a primary expression (function call, etc.) with no initial context
                    // Evaluate it directly
                    let result = eval_node(arena, step_id, ctx)?;
                    result.into_vec()
                } else {
                    // Evaluate for each node in the current sequence
                    let mut results = Vec::new();
                    for item in current_nodes {
                        let saved_context = ctx.context_item.take();
                        let saved_pos = ctx.context_position;
                        let saved_size = ctx.context_size;

                        ctx.context_item = Some(item);
                        ctx.context_position = 1;
                        ctx.context_size = 1;

                        let step_result = eval_node(arena, step_id, ctx)?;
                        results.extend(step_result.into_vec());

                        ctx.context_item = saved_context;
                        ctx.context_position = saved_pos;
                        ctx.context_size = saved_size;
                    }
                    results
                }
            }
        };

        // Early exit if sequence becomes empty
        if current_nodes.is_empty() {
            return Ok(XPathValue::empty());
        }
    }

    // Apply document order if needed (for paths with reverse axes)
    if needs_doc_order && !current_nodes.is_empty() {
        let iter = VecNodeIterator::new(current_nodes);
        let doc_order_iter = DocumentOrderNodeIterator::new(iter)?;
        let mut doc_order_iter = doc_order_iter;
        current_nodes = collect_iterator(&mut doc_order_iter)?;
    }

    Ok(XPathValue::from_sequence(current_nodes))
}

/// Check if a path expression needs document order sorting.
///
/// Returns true if the path contains:
/// - Any reverse axis (parent, ancestor, preceding, preceding-sibling, ancestor-or-self)
/// - FilterExpr at non-first position
/// - Descendant/DescendantOrSelf/Following axis followed by non-Attribute/non-Namespace steps
///   (these can produce duplicates when input nodes have overlapping descendants)
fn path_needs_document_order(arena: &AstArena, path_expr: &PathExprNode) -> bool {
    let len = path_expr.steps.len();
    for (idx, &step_id) in path_expr.steps.iter().enumerate() {
        match arena.get(step_id) {
            AstNode::PathStep(step) => {
                // Reverse axes always need sorting
                if step.axis.is_reverse() {
                    return true;
                }
                // Descendant/DescendantOrSelf/Following axes need sorting if followed
                // by non-attribute/non-namespace steps (can produce duplicates)
                if matches!(
                    step.axis,
                    Axis::Descendant | Axis::DescendantOrSelf | Axis::Following
                ) {
                    for s in (idx + 1)..len {
                        if let AstNode::PathStep(next_step) = arena.get(path_expr.steps[s]) {
                            if !matches!(next_step.axis, Axis::Attribute | Axis::Namespace) {
                                return true;
                            }
                        }
                    }
                }
            }
            AstNode::FilterExpr(_) if idx > 0 => {
                return true;
            }
            _ => {}
        }
    }
    false
}

/// Evaluate a single path step against a sequence of nodes.
fn eval_path_step<N: DomNavigator>(
    arena: &AstArena,
    step: &PathStepNode,
    input_nodes: Vec<XmlItem<N>>,
    ctx: &mut DynamicContext<'_, N>,
    _is_first_step: bool,
) -> Result<Vec<XmlItem<N>>, XPathError> {
    // Convert input to nodes only (XPTY0019 if atomic values present)
    let nodes: Vec<N> = input_nodes
        .into_iter()
        .map(|item| match item {
            XmlItem::Node(n) => Ok(n),
            XmlItem::Atomic(_) => Err(XPathError::XPTY0019),
        })
        .collect::<Result<Vec<_>, _>>()?;

    if nodes.is_empty() {
        return Ok(Vec::new());
    }

    // Convert the step's node test to runtime NodeTest
    let node_test = step_to_node_test(step, ctx.static_context);

    // Create the base iterator from input nodes
    let base_iter = VecNodeIterator::new(nodes.into_iter().map(XmlItem::Node).collect());

    // Apply axis iterator
    let xpath_ctx = ctx.static_context.clone();
    let stepped_items = apply_axis_iterator(step.axis, node_test, xpath_ctx, base_iter)?;

    // Apply predicates if any
    if step.predicates.is_empty() {
        Ok(stepped_items)
    } else {
        eval_predicates(arena, &step.predicates, ctx, stepped_items)
    }
}

/// Convert a PathStepNode to a runtime NodeTest.
fn step_to_node_test(step: &PathStepNode, ctx: &XPathContext<'_>) -> Option<NodeTest> {
    // If we have a resolved_test from binding, use it
    if let Some(ref resolved) = step.resolved_test {
        return Some(NodeTest::Name(resolved.clone()));
    }

    // Otherwise, convert from AST node test
    match &step.test {
        AstNodeTest::Name(name_test) => {
            // Convert AST NameTest to runtime NameTest
            match (&name_test.prefix, &name_test.local_name) {
                (None, None) => {
                    // * - wildcard
                    Some(NodeTest::Name(RuntimeNameTest::Wildcard))
                }
                (None, Some(local)) => {
                    // *:local - namespace wildcard
                    let local_id = ctx.names.add(local);
                    Some(NodeTest::Name(RuntimeNameTest::NamespaceWildcard(local_id)))
                }
                (Some(prefix), None) => {
                    // prefix:* - local wildcard
                    if let Some(ns_uri) = ctx.resolve_prefix(prefix) {
                        let ns_id = ctx.names.add(&ns_uri);
                        Some(NodeTest::Name(RuntimeNameTest::LocalWildcard(ns_id)))
                    } else {
                        None // Unknown prefix
                    }
                }
                (Some(prefix), Some(local)) => {
                    // prefix:local - specific QName
                    let local_id = ctx.names.add(local);
                    let ns_uri = if prefix.is_empty() {
                        ctx.default_element_ns
                    } else {
                        ctx.resolve_prefix(prefix).map(|s| ctx.names.add(&s))
                    };
                    let qname =
                        crate::namespace::qname::QualifiedName::new(ns_uri, local_id, None);
                    Some(NodeTest::Name(RuntimeNameTest::QName(qname)))
                }
            }
        }
        AstNodeTest::Kind(kind_test) => {
            // Convert AST KindTest to SequenceType
            let seq_type = kind_test_to_sequence_type(kind_test);
            Some(NodeTest::Type(seq_type))
        }
    }
}

/// Convert an AST KindTest to a SequenceType.
fn kind_test_to_sequence_type(kind: &KindTest) -> SequenceType {
    match kind {
        KindTest::AnyKind => SequenceType::node(),
        KindTest::Text => SequenceType::one(ItemType::Text),
        KindTest::Comment => SequenceType::one(ItemType::Comment),
        KindTest::ProcessingInstruction(target) => {
            SequenceType::one(ItemType::ProcessingInstruction(target.clone()))
        }
        KindTest::Document(inner) => {
            let inner_type = inner.as_ref().map(|k| Box::new(kind_test_to_item_type(k)));
            SequenceType::one(ItemType::Document(inner_type))
        }
        KindTest::Element(_) => {
            // For simplicity, treat as element() without name/type constraints
            // The actual name test is handled separately
            SequenceType::one(ItemType::Element(None, None))
        }
        KindTest::Attribute(_) => {
            // For simplicity, treat as attribute() without name/type constraints
            SequenceType::one(ItemType::Attribute(None, None))
        }
        KindTest::SchemaElement(_) | KindTest::SchemaAttribute(_) => {
            // Schema-aware types - treat as generic element/attribute for now
            SequenceType::node()
        }
    }
}

/// Convert an AST KindTest to an ItemType (for nested tests like document-node(element(...))).
fn kind_test_to_item_type(kind: &KindTest) -> ItemType {
    match kind {
        KindTest::AnyKind => ItemType::AnyNode,
        KindTest::Text => ItemType::Text,
        KindTest::Comment => ItemType::Comment,
        KindTest::ProcessingInstruction(target) => {
            ItemType::ProcessingInstruction(target.clone())
        }
        KindTest::Document(inner) => {
            let inner_type = inner.as_ref().map(|k| Box::new(kind_test_to_item_type(k)));
            ItemType::Document(inner_type)
        }
        KindTest::Element(_) => ItemType::Element(None, None),
        KindTest::Attribute(_) => ItemType::Attribute(None, None),
        KindTest::SchemaElement(_) | KindTest::SchemaAttribute(_) => ItemType::AnyNode,
    }
}

/// Apply an axis iterator to a base iterator.
fn apply_axis_iterator<N: DomNavigator>(
    axis: Axis,
    node_test: Option<NodeTest>,
    ctx: XPathContext<'_>,
    base_iter: VecNodeIterator<N>,
) -> Result<Vec<XmlItem<N>>, XPathError> {
    match axis {
        Axis::Child => {
            let mut iter =
                SequentialAxisNodeIterator::new(ctx, node_test, false, base_iter, ChildAxis);
            collect_iterator(&mut iter)
        }
        Axis::Descendant => {
            let mut iter = DescendantNodeIterator::new(ctx, node_test, false, base_iter);
            collect_iterator(&mut iter)
        }
        Axis::DescendantOrSelf => {
            let mut iter = DescendantNodeIterator::new(ctx, node_test, true, base_iter);
            collect_iterator(&mut iter)
        }
        Axis::Attribute => {
            let mut iter =
                SequentialAxisNodeIterator::new(ctx, node_test, false, base_iter, AttributeAxis);
            collect_iterator(&mut iter)
        }
        Axis::SelfAxis => {
            // SelfAxis returns current node via move_to_first, so match_self=false
            let mut iter =
                SequentialAxisNodeIterator::new(ctx, node_test, false, base_iter, SelfAxis);
            collect_iterator(&mut iter)
        }
        Axis::Parent => {
            let mut iter =
                SequentialAxisNodeIterator::new(ctx, node_test, false, base_iter, ParentAxis);
            collect_iterator(&mut iter)
        }
        Axis::Ancestor => {
            let mut iter =
                SequentialAxisNodeIterator::new(ctx, node_test, false, base_iter, AncestorAxis);
            collect_iterator(&mut iter)
        }
        Axis::AncestorOrSelf => {
            let mut iter =
                SequentialAxisNodeIterator::new(ctx, node_test, true, base_iter, AncestorAxis);
            collect_iterator(&mut iter)
        }
        Axis::FollowingSibling => {
            let mut iter = SequentialAxisNodeIterator::new(
                ctx,
                node_test,
                false,
                base_iter,
                FollowingSiblingAxis,
            );
            collect_iterator(&mut iter)
        }
        Axis::PrecedingSibling => {
            let mut iter = SequentialAxisNodeIterator::new(
                ctx,
                node_test,
                false,
                base_iter,
                PrecedingSiblingAxis,
            );
            collect_iterator(&mut iter)
        }
        Axis::Following => {
            let mut iter = FollowingNodeIterator::new(ctx, node_test, base_iter);
            collect_iterator(&mut iter)
        }
        Axis::Preceding => {
            let mut iter = PrecedingNodeIterator::new(ctx, node_test, base_iter);
            collect_iterator(&mut iter)
        }
        Axis::Namespace => {
            let mut iter = SequentialAxisNodeIterator::new(
                ctx,
                node_test,
                false,
                base_iter,
                NamespaceAxis::default(),
            );
            collect_iterator(&mut iter)
        }
    }
}

/// Collect iterator results into a Vec.
fn collect_iterator<I: XmlNodeIterator>(iter: &mut I) -> Result<Vec<XmlItem<I::Navigator>>, XPathError> {
    let mut results = Vec::new();
    while iter.move_next()? {
        if let Some(item_ref) = iter.current() {
            let item = match item_ref {
                crate::xpath::iterator::XmlItemRef::Node(n) => XmlItem::Node(n.clone()),
                crate::xpath::iterator::XmlItemRef::Atomic(v) => XmlItem::Atomic(v.clone()),
            };
            results.push(item);
        }
    }
    Ok(results)
}

// ============================================================================
// Filter Expression Evaluation
// ============================================================================

/// Evaluate a filter expression (`expr[predicate][predicate]...`).
fn eval_filter_expr<N: DomNavigator>(
    arena: &AstArena,
    filter_expr: &FilterExprNode,
    ctx: &mut DynamicContext<'_, N>,
) -> Result<XPathValue<N>, XPathError> {
    // Evaluate the base expression
    let base_value = eval_node(arena, filter_expr.base, ctx)?;

    // If no predicates, return base value directly
    if filter_expr.predicates.is_empty() {
        return Ok(base_value);
    }

    // Apply predicates
    let items = base_value.into_vec();
    let filtered = eval_predicates(arena, &filter_expr.predicates, ctx, items)?;
    Ok(XPathValue::from_sequence(filtered))
}

/// Evaluate predicates on a sequence of items.
///
/// Each predicate is evaluated in order. For each predicate:
/// - If the predicate evaluates to a number, select the item at that position
/// - Otherwise, use effective boolean value to filter
fn eval_predicates<N: DomNavigator>(
    arena: &AstArena,
    predicates: &[AstNodeId],
    ctx: &mut DynamicContext<'_, N>,
    mut items: Vec<XmlItem<N>>,
) -> Result<Vec<XmlItem<N>>, XPathError> {
    for &pred_id in predicates {
        if items.is_empty() {
            break;
        }

        let size = items.len();
        let mut filtered = Vec::new();

        for (idx, item) in items.into_iter().enumerate() {
            let position = idx + 1; // 1-based position

            // Save current context
            let saved_item = ctx.context_item.take();
            let saved_pos = ctx.context_position;
            let saved_size = ctx.context_size;

            // Set predicate context
            ctx.context_item = Some(item.clone());
            ctx.context_position = position;
            ctx.context_size = size;

            // Evaluate predicate
            let pred_result = eval_node(arena, pred_id, ctx)?;

            // Restore context
            ctx.context_item = saved_item;
            ctx.context_position = saved_pos;
            ctx.context_size = saved_size;

            // Check if item should be included
            let is_10 = ctx.static_context.mode() == XPathMode::XPath10;
            let include = match &pred_result {
                XPathValue::Item(XmlItem::Atomic(value)) if value.type_code.is_numeric() => {
                    // XPath 1.0 §2.4 and 2.0: exact comparison, no rounding
                    let num = crate::xpath::atomize::to_number(value);
                    if num.is_nan() {
                        false
                    } else {
                        (position as f64) == num
                    }
                }
                _ => {
                    if is_10 {
                        effective_boolean_value_10(&pred_result)?
                    } else {
                        effective_boolean_value(&pred_result)?
                    }
                }
            };

            if include {
                filtered.push(item);
            }
        }

        items = filtered;
    }

    Ok(items)
}

use std::ops::ControlFlow;

// ============================================================================
// For Expression Evaluation
// ============================================================================

/// Evaluate a for expression (`for $x in X, $y in Y return expr`).
///
/// Semantics per XPath 2.0 spec:
/// - Evaluate each binding's `in_expr` to produce a sequence
/// - Iterate through all combinations (Cartesian product for multiple bindings)
/// - For each combination, bind variables and evaluate `return_expr`
/// - Concatenate all results into a single sequence
fn eval_for_expression<N: DomNavigator>(
    arena: &AstArena,
    for_node: &ForNode,
    ctx: &mut DynamicContext<'_, N>,
) -> Result<XPathValue<N>, XPathError> {
    // Collect results from iterating over Cartesian product
    let mut results: Vec<XmlItem<N>> = Vec::new();

    // For the for expression, we use XPathError as the break type since we only
    // break on errors (never short-circuit for other reasons)
    match eval_for_bindings(
        arena,
        &for_node.bindings,
        0,
        for_node.return_expr,
        ctx,
        &mut |result| match result {
            Ok(value) => {
                results.extend(value.into_vec());
                ControlFlow::Continue(())
            }
            Err(e) => ControlFlow::Break(e),
        },
    ) {
        ControlFlow::Continue(()) => {}
        ControlFlow::Break(e) => return Err(e),
    }

    Ok(XPathValue::from_sequence(results))
}

/// Recursively iterate over Cartesian product of bindings with lazy evaluation.
///
/// This helper handles the recursive iteration for for/quantified expressions.
/// For each binding, it evaluates the `in_expr` (allowing dependent bindings),
/// iterates over its sequence, and recursively processes remaining bindings.
/// When all bindings are processed, it evaluates the body.
///
/// IMPORTANT: Each binding's `in_expr` is evaluated lazily, AFTER all previous
/// binding variables have been bound. This allows dependent bindings like:
/// `for $x in 1 to 3, $y in $x+1 return $y`
///
/// The function is generic over the break type `B`, allowing callers to use
/// different types for different control flow needs:
/// - For expressions use `B = XPathError` (only break on errors)
/// - Quantified expressions use `B = QuantifiedExit` (break on short-circuit or error)
fn eval_for_bindings<N: DomNavigator, B>(
    arena: &AstArena,
    bindings: &[ForBinding],
    binding_index: usize,
    body_id: AstNodeId,
    ctx: &mut DynamicContext<'_, N>,
    collector: &mut impl FnMut(Result<XPathValue<N>, XPathError>) -> ControlFlow<B>,
) -> ControlFlow<B> {
    if binding_index >= bindings.len() {
        // All bindings processed, evaluate the body
        let result = eval_node(arena, body_id, ctx);
        return collector(result);
    }

    let binding = &bindings[binding_index];
    let slot = match binding.slot {
        Some(s) => s,
        None => {
            return collector(Err(XPathError::Internal(
                "For binding slot not assigned".to_string(),
            )))
        }
    };

    // LAZY EVALUATION: Evaluate in_expr NOW (previous variables are already bound)
    let seq_value = match eval_node(arena, binding.in_expr, ctx) {
        Ok(v) => v,
        Err(e) => return collector(Err(e)),
    };
    let items = seq_value.into_vec();

    // If binding sequence is empty, we simply don't iterate (produces empty result)
    if items.is_empty() {
        return ControlFlow::Continue(());
    }

    // Iterate over each item in the current binding's sequence
    for item in items {
        // Set the variable for this binding
        ctx.set_variable(slot, XPathValue::from_item(item));

        // Recursively process remaining bindings
        if let cf @ ControlFlow::Break(_) = eval_for_bindings(
            arena,
            bindings,
            binding_index + 1,
            body_id,
            ctx,
            collector,
        ) {
            return cf;
        }
    }

    ControlFlow::Continue(())
}

// ============================================================================
// Quantified Expression Evaluation
// ============================================================================

/// Exit type for quantified expression short-circuit evaluation.
///
/// This enum cleanly distinguishes between a legitimate short-circuit exit
/// (when the quantified expression's answer is determined) and an actual error.
enum QuantifiedExit {
    /// Short-circuit: the quantified expression's answer is determined.
    ShortCircuit,
    /// A real error occurred during evaluation.
    Error(XPathError),
}

/// Evaluate a quantified expression (`some/every $x in X satisfies expr`).
///
/// Semantics per XPath 2.0 spec:
/// - `some`: Returns true if at least one combination satisfies the expression
/// - `every`: Returns true if all combinations satisfy (including empty - vacuous truth)
/// - Short-circuit evaluation when result is determined
///
/// NOTE: Bindings are evaluated lazily, allowing dependent bindings like:
/// `some $x in (1,2), $y in ($x*2) satisfies $y > 3`
fn eval_quantified_expression<N: DomNavigator>(
    arena: &AstArena,
    quant_node: &QuantifiedNode,
    ctx: &mut DynamicContext<'_, N>,
) -> Result<XPathValue<N>, XPathError> {
    // Track whether we had any iterations (for vacuous truth handling)
    let mut had_any_iteration = false;
    let mut found_some = false;
    let mut all_satisfied = true;

    // Use QuantifiedExit as the break type to cleanly distinguish between
    // short-circuit (answer found) and real errors
    match eval_for_bindings(
        arena,
        &quant_node.bindings,
        0,
        quant_node.satisfies,
        ctx,
        &mut |result| {
            had_any_iteration = true;
            match result {
                Ok(value) => match effective_boolean_value(&value) {
                    Ok(satisfied) => {
                        match quant_node.kind {
                            QuantifierKind::Some => {
                                if satisfied {
                                    found_some = true;
                                    return ControlFlow::Break(QuantifiedExit::ShortCircuit);
                                }
                            }
                            QuantifierKind::Every => {
                                if !satisfied {
                                    all_satisfied = false;
                                    return ControlFlow::Break(QuantifiedExit::ShortCircuit);
                                }
                            }
                        }
                        ControlFlow::Continue(())
                    }
                    Err(e) => ControlFlow::Break(QuantifiedExit::Error(e)),
                },
                Err(e) => ControlFlow::Break(QuantifiedExit::Error(e)),
            }
        },
    ) {
        ControlFlow::Continue(()) => {} // Completed all iterations
        ControlFlow::Break(QuantifiedExit::ShortCircuit) => {} // Found answer early
        ControlFlow::Break(QuantifiedExit::Error(e)) => return Err(e),
    }

    // Handle vacuous truth: if no iterations occurred (empty binding),
    // "every" is vacuously true, "some" is false
    if !had_any_iteration {
        return match quant_node.kind {
            QuantifierKind::Some => Ok(XPathValue::boolean(false)),
            QuantifierKind::Every => Ok(XPathValue::boolean(true)),
        };
    }

    match quant_node.kind {
        QuantifierKind::Some => Ok(XPathValue::boolean(found_some)),
        QuantifierKind::Every => Ok(XPathValue::boolean(all_satisfied)),
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

/// Check if an XPathValue is a single boolean value (XPath 1.0 §3.4 helper).
fn is_boolean_value<N: DomNavigator>(val: &XPathValue<N>) -> bool {
    matches!(val, XPathValue::Item(XmlItem::Atomic(v)) if v.type_code == crate::types::XmlTypeCode::Boolean)
}

/// Check if an XPathValue contains nodes or is empty (i.e., is a node-set in XPath 1.0 terms).
///
/// In XPath 1.0, the empty result of a path expression is an empty node-set,
/// so `XPathValue::Empty` is treated as a node-set (boolean false).
fn has_nodes_or_empty<N: DomNavigator>(val: &XPathValue<N>) -> bool {
    match val {
        XPathValue::Empty => true,
        XPathValue::Item(XmlItem::Node(_)) => true,
        XPathValue::Sequence(items) => items.iter().any(|i| matches!(i, XmlItem::Node(_))),
        _ => false,
    }
}

#[cfg(test)]
#[path = "eval_tests.rs"]
mod eval_tests;
