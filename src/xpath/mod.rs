//! XPath2 evaluation engine
//!
//! This module provides the full XPath 2.0 parser, binder, and evaluator.
//!
//! ## Core Types (re-exported from `navigator`)
//!
//! - [`DomNavigator`] - Trait for cursor-based XML navigation
//! - [`DomNodeType`] - Node type enumeration (Element, Text, Attribute, etc.)
//! - [`XmlNodeOrder`] - Document order comparison result
//! - [`NamespaceAxisScope`] - Scope filter for namespace axis traversal
//!
//! ## Parser
//!
//! - [`ast`] - AST node types for XPath expressions
//! - [`arena`] - Arena-based storage for AST nodes
//! - [`lexer`] - Stateful tokenizer with lookahead
//! - [`parser`] - LALRPOP-based parser

// Parser modules
pub mod arena;
pub mod ast;
pub mod error;
pub mod lexer;
pub mod parser;
pub mod operators;
pub mod iterator;
pub mod tree_comparer;
pub mod node_test;
pub mod context;
pub mod timsort;
pub mod item_set;
pub mod axis_iterators;

// Core function modules (ported from CoreFuncs.cs)
pub mod boolean;
pub mod atomize;
pub mod cast;
pub mod sequence_ops;
pub mod node_ops;
pub mod quantified;
pub mod string_ops;
pub mod type_info;
pub mod iter_adapters;

// XPath 2.0 function registry and dispatch
pub mod functions;

// AST binding and evaluation phases
pub mod bind;
pub mod eval;

// High-level public API
pub mod api;

// Re-export navigator types for backward compatibility
pub use crate::navigator::{
    DomNavigator, DomNodeType, XmlNodeOrder, NamespaceAxisScope,
    RoXmlNavigator, NavigatorError,
};

// High-level API re-exports
pub use self::api::{XPathExpr, XPathEvaluator, ExternalVar, EvalValue, TypedEvaluator};

// Re-export key parser types
pub use self::arena::{AstArena, AstNodeId, SourceSpan};
pub use self::ast::AstNode;
pub use self::error::XPathError;
pub use self::lexer::{Lexer, Token};
pub use self::parser::{parse, parse_with_options, parse_xpath10, parse_xpath20, ParseError, ParsedXPath};
pub use self::iterator::{
    XmlItem, XmlItemRef, XmlNodeIterator, VecNodeIterator,
    EmptyIterator, BufferedNodeIterator, RangeIterator,
    DocumentOrderNodeIterator, PositionFilterNodeIterator, ItemIterator,
};
pub use self::axis_iterators::{
    AxisTraversal, SequentialAxisNodeIterator,
    SelfAxis, ParentAxis, AncestorAxis, ChildAxis, AttributeAxis,
    NamespaceAxis, FollowingSiblingAxis, PrecedingSiblingAxis,
    DescendantNodeIterator, FollowingNodeIterator, PrecedingNodeIterator,
    SpecialChildNodeIterator, SpecialDescendantNodeIterator, ChildOverDescendantsNodeIterator,
};
pub use self::tree_comparer::TreeComparer;
pub use self::node_test::{NodeTest, matches_name_test, matches_sequence_type};
pub use self::context::{XPathContext, DynamicContext, VarStore, VarSlotId, VarRef, NameBinder};
pub use self::functions::{FunctionId, FunctionArity, FunctionSignature, XPathValue, FUNCTION_REGISTRY};
pub use self::bind::bind_node;
pub use self::eval::eval_node;
pub use self::timsort::{
    timsort, timsort_by, timsort_slice, timsort_slice_by,
    timsort_with_comparer, timsort_slice_with_comparer,
    IComparer, OrdComparer, ReverseComparer, FnComparer,
};
pub use self::item_set::{ItemSet, ItemSetIter, ItemSetIterMut, XPathComparer, XPathEqualityComparer};

/// Selects XPath language version for parsing and evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum XPathMode {
    XPath10,
    #[default]
    XPath20,
}

/// Options for XPath parsing.
#[derive(Debug, Clone)]
pub struct XPathParseOptions {
    pub mode: XPathMode,
}

impl Default for XPathParseOptions {
    fn default() -> Self {
        Self {
            mode: XPathMode::XPath20,
        }
    }
}
