// ============================================================================
// Function Call
// ============================================================================

/// Function call expression (`prefix:name(args...)`).
#[derive(Debug, Clone)]
pub struct FunctionCallNode {
    /// Namespace prefix (empty string if none, defaults to fn namespace).
    pub prefix: String,
    /// Function local name.
    pub local_name: String,
    /// Argument expressions.
    pub args: Vec<AstNodeId>,
    /// Source location.
    pub span: SourceSpan,
}

impl FunctionCallNode {
    pub fn new(prefix: String, local_name: String, args: Vec<AstNodeId>, span: SourceSpan) -> Self {
        Self {
            prefix,
            local_name,
            args,
            span,
        }
    }
}

