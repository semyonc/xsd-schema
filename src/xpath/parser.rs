//! XPath 2.0 Parser module.
//!
//! This module provides the public API for parsing XPath 2.0 expressions.
//! It uses the LALRPOP-generated parser with a custom stateful lexer.

use crate::xpath::arena::{AstArena, AstNodeId, SourceSpan};
use crate::xpath::ast::AstNode;
use crate::xpath::lexer::{Lexer, LexerError};
use std::fmt;

// The LALRPOP-generated parser.
// This uses the lalrpop_mod! macro to include the generated code.
// The grammar is defined in src/xpath/parser.lalrpop
lalrpop_util::lalrpop_mod!(
    #[allow(clippy::all)]
    #[allow(unused)]
    #[allow(dead_code)]
    pub xpath_grammar,
    "/xpath/parser.rs"
);

/// Error type for XPath parsing.
#[derive(Debug, Clone)]
pub enum ParseError {
    /// Lexer error (tokenization failed).
    Lexer(LexerError),
    /// Parser error (grammar mismatch).
    Parser {
        message: String,
        location: Option<usize>,
    },
    /// Unexpected end of input.
    UnexpectedEof,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::Lexer(e) => write!(f, "Lexer error: {}", e),
            ParseError::Parser { message, location } => {
                if let Some(loc) = location {
                    write!(f, "Parse error at position {}: {}", loc, message)
                } else {
                    write!(f, "Parse error: {}", message)
                }
            }
            ParseError::UnexpectedEof => write!(f, "Unexpected end of input"),
        }
    }
}

impl std::error::Error for ParseError {}

impl From<LexerError> for ParseError {
    fn from(e: LexerError) -> Self {
        ParseError::Lexer(e)
    }
}

/// Result of parsing an XPath expression.
#[derive(Debug)]
pub struct ParsedXPath {
    /// The arena containing all AST nodes.
    pub arena: AstArena,
    /// The root node ID of the parsed expression.
    pub root: AstNodeId,
    /// Source span of the entire expression.
    pub span: SourceSpan,
}

impl ParsedXPath {
    /// Get a reference to the root AST node.
    pub fn root_node(&self) -> &AstNode {
        self.arena.get(self.root)
    }

    /// Get a reference to any node by ID.
    pub fn get_node(&self, id: AstNodeId) -> &AstNode {
        self.arena.get(id)
    }

    /// Get the number of nodes in the AST.
    pub fn node_count(&self) -> usize {
        self.arena.len()
    }
}

/// Parse an XPath 2.0 expression string.
///
/// Returns a `ParsedXPath` containing the AST arena and root node ID.
///
/// # Example
///
/// ```
/// use xsd_schema::xpath::parser::parse;
///
/// let result = parse("/a/b/c").unwrap();
/// println!("Parsed {} nodes", result.node_count());
/// ```
pub fn parse(input: &str) -> Result<ParsedXPath, ParseError> {
    let mut arena = AstArena::new();
    let lexer = Lexer::new(input);

    let root = xpath_grammar::ExprParser::new()
        .parse(&mut arena, lexer)
        .map_err(|e| match e {
            lalrpop_util::ParseError::InvalidToken { location } => ParseError::Parser {
                message: "Invalid token".to_string(),
                location: Some(location),
            },
            lalrpop_util::ParseError::UnrecognizedEof { location, expected } => {
                ParseError::Parser {
                    message: format!("Unexpected end of input, expected one of: {:?}", expected),
                    location: Some(location),
                }
            }
            lalrpop_util::ParseError::UnrecognizedToken { token, expected } => {
                ParseError::Parser {
                    message: format!(
                        "Unexpected token {:?}, expected one of: {:?}",
                        token.1, expected
                    ),
                    location: Some(token.0),
                }
            }
            lalrpop_util::ParseError::ExtraToken { token } => ParseError::Parser {
                message: format!("Extra token: {:?}", token.1),
                location: Some(token.0),
            },
            lalrpop_util::ParseError::User { error } => ParseError::Lexer(error),
        })?;

    Ok(ParsedXPath {
        arena,
        root,
        span: SourceSpan::new(0, input.len()),
    })
}

/// Parse an XPath expression and return just the root node ID.
///
/// This is a convenience function when you only need the arena and root.
pub fn parse_expr(input: &str, arena: &mut AstArena) -> Result<AstNodeId, ParseError> {
    let lexer = Lexer::new(input);

    xpath_grammar::ExprParser::new()
        .parse(arena, lexer)
        .map_err(|e| match e {
            lalrpop_util::ParseError::InvalidToken { location } => ParseError::Parser {
                message: "Invalid token".to_string(),
                location: Some(location),
            },
            lalrpop_util::ParseError::UnrecognizedEof { location, expected } => {
                ParseError::Parser {
                    message: format!("Unexpected end of input, expected one of: {:?}", expected),
                    location: Some(location),
                }
            }
            lalrpop_util::ParseError::UnrecognizedToken { token, expected } => {
                ParseError::Parser {
                    message: format!(
                        "Unexpected token {:?}, expected one of: {:?}",
                        token.1, expected
                    ),
                    location: Some(token.0),
                }
            }
            lalrpop_util::ParseError::ExtraToken { token } => ParseError::Parser {
                message: format!("Extra token: {:?}", token.1),
                location: Some(token.0),
            },
            lalrpop_util::ParseError::User { error } => ParseError::Lexer(error),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_arithmetic() {
        let result = parse("1 + 2");
        assert!(result.is_ok(), "Parse failed: {:?}", result.err());
    }

    #[test]
    fn test_parse_path() {
        let result = parse("/a/b/c");
        assert!(result.is_ok(), "Parse failed: {:?}", result.err());
    }

    #[test]
    fn test_parse_variable() {
        let result = parse("$x");
        assert!(result.is_ok(), "Parse failed: {:?}", result.err());
    }

    #[test]
    fn test_parse_function() {
        let result = parse("fn:count(*)");
        assert!(result.is_ok(), "Parse failed: {:?}", result.err());
    }

    #[test]
    fn test_lexer_errors() {
        // Test that lexer errors are properly propagated
        // Most inputs should lex successfully
        let result = parse("'unclosed string");
        assert!(result.is_err());
    }
}
