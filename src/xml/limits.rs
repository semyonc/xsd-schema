//! Parse limits for the instance readers.

use std::fmt;

/// Bounds on what one document read may contain, checked on the raw markup
/// before anything of it is interned or built.
///
/// Taken by the instance readers: the quick-xml validation drivers and the
/// typed build ([`ValidationRuntime::set_parse_limits`](crate::validation::ValidationRuntime::set_parse_limits)),
/// and the document builder (`BufferDocumentBuilder::with_parse_limits`).
/// Every limit is `None` (unbounded) by default — [`ParseLimits::default`]
/// and [`ParseLimits::none`] are today's behaviour. Lengths and sizes are
/// counted in bytes **as written** (before references are resolved and
/// before attribute-value normalization; a reference never resolves to more
/// bytes than it is written with, so the written size bounds the resolved
/// one).
///
/// A document that breaks a limit is refused with a [`LimitExceeded`] naming
/// the limit, at the construct that breaks it: nothing of that construct (a
/// start tag's names, prefixes and namespace URIs, a text run) has been
/// interned or built, while what came before it in the document was read
/// normally.
///
/// The limits bound what is interned and built, and the node count — not the
/// reader's peak memory: quick-xml reads a whole construct (a tag, a text
/// run, a comment, a document type declaration) into its buffer before it is
/// checked, so a host also bounds the size of the input it reads.
///
/// Generous values for a server that reads untrusted documents might be:
/// depth 4,096, names 64 KiB, namespace URIs 64 KiB, 10,000 attributes and
/// 1,000 namespace declarations per element, 1,000 prefixes in scope,
/// attribute values 64 MiB, text 1 GiB — and an input size limit.
///
/// `#[non_exhaustive]`: start from [`ParseLimits::none`] (or `default()`) and
/// set what you need, with the `with_*` methods or by assigning the fields.
///
/// ```
/// use xsd_schema::xml::ParseLimits;
///
/// let limits = ParseLimits::none()
///     .with_max_depth(256)
///     .with_max_name_bytes(1024)
///     .with_max_nodes(1_000_000);
/// assert_eq!(limits.max_depth, Some(256));
/// assert_eq!(limits.max_text_bytes, None);
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ParseLimits {
    /// Element nesting depth; the root element is at depth 1.
    pub max_depth: Option<usize>,
    /// Bytes of an element or attribute name (the whole qualified name,
    /// prefix included), of the prefix a namespace declaration declares, and
    /// of a processing-instruction target.
    pub max_name_bytes: Option<usize>,
    /// Bytes of the namespace URI of one namespace declaration.
    pub max_namespace_uri_bytes: Option<usize>,
    /// Attributes on one element, namespace declarations not included.
    pub max_attributes: Option<usize>,
    /// Namespace declarations (`xmlns`, `xmlns:p`) on one element.
    pub max_namespace_declarations: Option<usize>,
    /// Distinct namespace prefixes in scope at any element (the default
    /// namespace counts as one; the implicit `xml` prefix does not), counted
    /// once all of the element's declarations apply: an undeclaration
    /// (`xmlns=""`, or `xmlns:p=""` in XML 1.1) takes its prefix out of the
    /// count until the element closes, a redeclared prefix adds none, and
    /// declaring `xml` adds none.
    pub max_namespaces_in_scope: Option<usize>,
    /// Bytes of one attribute value (namespace declarations included).
    pub max_attribute_value_bytes: Option<usize>,
    /// Bytes of one run of character data (its text and references, up to
    /// the next markup), of one CDATA section, comment or processing
    /// instruction, of one run of white space outside the root element, and
    /// of the document type declaration.
    pub max_text_bytes: Option<usize>,
    /// Nodes in the document: elements, attributes, namespace declarations,
    /// runs of character data, CDATA sections, comments and processing
    /// instructions (white space outside the root element is not a node).
    pub max_nodes: Option<usize>,
}

impl ParseLimits {
    /// No limits — the default.
    pub const fn none() -> Self {
        ParseLimits {
            max_depth: None,
            max_name_bytes: None,
            max_namespace_uri_bytes: None,
            max_attributes: None,
            max_namespace_declarations: None,
            max_namespaces_in_scope: None,
            max_attribute_value_bytes: None,
            max_text_bytes: None,
            max_nodes: None,
        }
    }

    /// Whether no limit is set.
    pub fn is_unlimited(&self) -> bool {
        *self == ParseLimits::none()
    }

    /// The limit `which`, if set.
    pub fn get(&self, which: ParseLimit) -> Option<usize> {
        match which {
            ParseLimit::Depth => self.max_depth,
            ParseLimit::NameLength => self.max_name_bytes,
            ParseLimit::NamespaceUriLength => self.max_namespace_uri_bytes,
            ParseLimit::Attributes => self.max_attributes,
            ParseLimit::NamespaceDeclarations => self.max_namespace_declarations,
            ParseLimit::NamespacesInScope => self.max_namespaces_in_scope,
            ParseLimit::AttributeValueSize => self.max_attribute_value_bytes,
            ParseLimit::TextSize => self.max_text_bytes,
            ParseLimit::Nodes => self.max_nodes,
        }
    }

    /// These limits with [`max_depth`](Self::max_depth) set.
    pub fn with_max_depth(mut self, max: usize) -> Self {
        self.max_depth = Some(max);
        self
    }

    /// These limits with [`max_name_bytes`](Self::max_name_bytes) set.
    pub fn with_max_name_bytes(mut self, max: usize) -> Self {
        self.max_name_bytes = Some(max);
        self
    }

    /// These limits with [`max_namespace_uri_bytes`](Self::max_namespace_uri_bytes) set.
    pub fn with_max_namespace_uri_bytes(mut self, max: usize) -> Self {
        self.max_namespace_uri_bytes = Some(max);
        self
    }

    /// These limits with [`max_attributes`](Self::max_attributes) set.
    pub fn with_max_attributes(mut self, max: usize) -> Self {
        self.max_attributes = Some(max);
        self
    }

    /// These limits with [`max_namespace_declarations`](Self::max_namespace_declarations) set.
    pub fn with_max_namespace_declarations(mut self, max: usize) -> Self {
        self.max_namespace_declarations = Some(max);
        self
    }

    /// These limits with [`max_namespaces_in_scope`](Self::max_namespaces_in_scope) set.
    pub fn with_max_namespaces_in_scope(mut self, max: usize) -> Self {
        self.max_namespaces_in_scope = Some(max);
        self
    }

    /// These limits with [`max_attribute_value_bytes`](Self::max_attribute_value_bytes) set.
    pub fn with_max_attribute_value_bytes(mut self, max: usize) -> Self {
        self.max_attribute_value_bytes = Some(max);
        self
    }

    /// These limits with [`max_text_bytes`](Self::max_text_bytes) set.
    pub fn with_max_text_bytes(mut self, max: usize) -> Self {
        self.max_text_bytes = Some(max);
        self
    }

    /// These limits with [`max_nodes`](Self::max_nodes) set.
    pub fn with_max_nodes(mut self, max: usize) -> Self {
        self.max_nodes = Some(max);
        self
    }
}

/// Which of the [`ParseLimits`] a document broke.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ParseLimit {
    /// [`ParseLimits::max_depth`].
    Depth,
    /// [`ParseLimits::max_name_bytes`].
    NameLength,
    /// [`ParseLimits::max_namespace_uri_bytes`].
    NamespaceUriLength,
    /// [`ParseLimits::max_attributes`].
    Attributes,
    /// [`ParseLimits::max_namespace_declarations`].
    NamespaceDeclarations,
    /// [`ParseLimits::max_namespaces_in_scope`].
    NamespacesInScope,
    /// [`ParseLimits::max_attribute_value_bytes`].
    AttributeValueSize,
    /// [`ParseLimits::max_text_bytes`].
    TextSize,
    /// [`ParseLimits::max_nodes`].
    Nodes,
}

impl ParseLimit {
    /// The name of the [`ParseLimits`] field, e.g. `"max_depth"`.
    pub fn field_name(self) -> &'static str {
        match self {
            ParseLimit::Depth => "max_depth",
            ParseLimit::NameLength => "max_name_bytes",
            ParseLimit::NamespaceUriLength => "max_namespace_uri_bytes",
            ParseLimit::Attributes => "max_attributes",
            ParseLimit::NamespaceDeclarations => "max_namespace_declarations",
            ParseLimit::NamespacesInScope => "max_namespaces_in_scope",
            ParseLimit::AttributeValueSize => "max_attribute_value_bytes",
            ParseLimit::TextSize => "max_text_bytes",
            ParseLimit::Nodes => "max_nodes",
        }
    }

    /// What the limit counts, for a message.
    fn what(self) -> &'static str {
        match self {
            ParseLimit::Depth => "element depth",
            ParseLimit::NameLength => "name length in bytes",
            ParseLimit::NamespaceUriLength => "namespace URI length in bytes",
            ParseLimit::Attributes => "attributes on one element",
            ParseLimit::NamespaceDeclarations => "namespace declarations on one element",
            ParseLimit::NamespacesInScope => "namespace prefixes in scope",
            ParseLimit::AttributeValueSize => "attribute value size in bytes",
            ParseLimit::TextSize => "text size in bytes",
            ParseLimit::Nodes => "nodes in the document",
        }
    }
}

impl fmt::Display for ParseLimit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.field_name())
    }
}

/// A document broke one of the [`ParseLimits`].
///
/// `Display`: `parse limit max_depth exceeded at byte 1234: element depth 257,
/// the limit is 256`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LimitExceeded {
    limit: ParseLimit,
    max: usize,
    found: usize,
    position: u64,
}

impl LimitExceeded {
    pub(crate) fn new(limit: ParseLimit, max: usize, found: usize, position: u64) -> Self {
        LimitExceeded {
            limit,
            max,
            found,
            position,
        }
    }

    /// The limit broken.
    pub fn limit(&self) -> ParseLimit {
        self.limit
    }

    /// Its configured maximum.
    pub fn max(&self) -> usize {
        self.max
    }

    /// The count or size found (greater than [`max`](Self::max)).
    pub fn found(&self) -> usize {
        self.found
    }

    /// The byte offset in the input (a leading byte-order mark included) of
    /// the construct that broke the limit: the `<` of the tag, the start of
    /// the attribute name or of the text.
    pub fn position(&self) -> u64 {
        self.position
    }
}

impl fmt::Display for LimitExceeded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "parse limit {} exceeded at byte {}: {} {}, the limit is {}",
            self.limit,
            self.position,
            self.limit.what(),
            self.found,
            self.max
        )
    }
}

impl std::error::Error for LimitExceeded {}
