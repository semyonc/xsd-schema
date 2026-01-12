//! XPath 2.0 Abstract Syntax Tree (AST) definitions.
//!
//! This module defines the AST node types for the XPath 2.0 parser.
//! All types are stubbed (fields defined but minimal behavior) to enable
//! parser development. Full evaluation semantics will be added later.

use crate::xpath::arena::{AstNodeId, SourceSpan};

include!("ast/core.rs");
include!("ast/expressions.rs");
include!("ast/control_flow.rs");
include!("ast/functions.rs");
include!("ast/paths.rs");
include!("ast/operators.rs");
include!("ast/types.rs");
include!("ast/tests.rs");
