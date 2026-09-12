//! Writing a navigable XML tree back out as XML text.
//!
//! [`BufferDocument`](super::BufferDocument) and the read-only roxmltree adapter
//! can both be navigated, but neither could be *written*. This module closes that
//! gap for every backend at once: it is generic over
//! [`DomNavigator`] and uses nothing but
//! navigation plus the node's name and string value, so the same code serializes
//! a `BufferDocNavigator`, a `RoXmlNavigator`, and any navigator a host embedding
//! the engine supplies of its own.
//!
//! ```
//! use bumpalo::Bump;
//! use xsd_schema::document::{serialize, BufferDocument};
//! use xsd_schema::namespace::NameTable;
//!
//! let arena = Bump::new();
//! let names = NameTable::new();
//! let doc = BufferDocument::from_reader_default(
//!     r#"<p:a xmlns:p="urn:x"><p:b k="1">t</p:b><!--c--></p:a>"#.as_bytes(),
//!     &arena,
//!     &names,
//! )?;
//!
//! let xml = serialize::to_string(&doc.create_navigator(), &Default::default())?;
//! assert_eq!(xml, r#"<p:a xmlns:p="urn:x"><p:b k="1">t</p:b><!--c--></p:a>"#);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # What the output looks like
//!
//! Output is UTF-8 bytes with no added whitespace: the serializer writes exactly
//! the nodes of the tree, so `parse → serialize → parse` yields the same tree
//! again. Empty elements are always collapsed to `<a/>`.
//!
//! Escaping follows the rules of *Canonical XML 1.0* §2.3 ("Processing Model",
//! its *Text Nodes* and *Attribute Nodes* items), because those are the rules
//! designed so that a re-parse cannot change the value back: without them XML
//! 1.0's end-of-line handling (§2.11) and attribute-value normalization (§3.3.3)
//! would silently rewrite literal carriage returns and attribute whitespace.
//!
//! | Context | Character | Written as |
//! |---|---|---|
//! | text | `&` `<` `>` | `&amp;` `&lt;` `&gt;` (`>` always, which covers the `]]>` rule of XML 1.0 §2.4 without scanning) |
//! | text | U+000D | `&#xD;` |
//! | attribute value (always `"`-quoted) | `&` `<` `"` | `&amp;` `&lt;` `&quot;` |
//! | attribute value | U+0009 U+000A U+000D | `&#x9;` `&#xA;` `&#xD;` |
//! | comment, PI target, PI data, names | nothing | — |
//!
//! This is an escaping policy, not an implementation of Canonical XML: nothing
//! here sorts attributes, normalizes namespace declarations or expands empty
//! elements.
//!
//! # What is refused
//!
//! A tree parsed from XML cannot hold content that XML cannot express, but a
//! tree built programmatically can. Such content is never dropped or mangled —
//! it is an error:
//!
//! * a character outside the XML 1.0 `Char` production (C0 controls other than
//!   tab, LF and CR; U+FFFE; U+FFFF) anywhere, including in names
//!   ([`SerializeError::InvalidChar`]);
//! * a comment containing `--` or ending in `-`, XML 1.0 §2.5
//!   ([`SerializeError::InvalidComment`]);
//! * a processing instruction whose target is `xml` in any case, or whose data
//!   contains `?>`, XML 1.0 §2.6 ([`SerializeError::InvalidPi`]);
//! * an element or attribute whose prefix is not bound to its namespace URI in
//!   the scope the output creates ([`SerializeError::UnboundName`]).
//!
//! Out of scope for this module: other encodings, CDATA sections, a doctype
//! declaration, and indentation.
//!
//! # Namespace declarations
//!
//! Declarations are written where they are *introduced*, computed by comparing
//! each element's in-scope bindings (the `namespace::` axis, minus `xml`) with
//! the scope the output has already established — see [`serialize_node`] for
//! what that means for a subtree. A declaration that only repeats what is
//! already in scope is dropped; an element that leaves an inherited default
//! namespace gets `xmlns=""`. On one element the declarations come out in
//! prefix order, the default namespace first, because the axis itself has no
//! order to preserve.

use std::io::Write;

use crate::namespace::XML_NAMESPACE;
use crate::navigator::{DomNavigator, DomNodeType, NamespaceAxisScope};

/// Options controlling XML output.
///
/// The default writes no XML declaration, which is what a host wants when the
/// result is embedded in something larger; turn it on for a standalone file.
///
/// ```
/// use xsd_schema::document::SerializeOptions;
///
/// let embedded = SerializeOptions::default();
/// assert!(!embedded.xml_declaration);
///
/// let file = SerializeOptions {
///     xml_declaration: true,
///     standalone: Some(true),
/// };
/// assert_eq!(file.standalone, Some(true));
/// ```
#[derive(Clone, Debug, Default)]
pub struct SerializeOptions {
    /// Write `<?xml version="1.0" encoding="UTF-8"?>` before a document.
    ///
    /// The declaration is written by [`serialize_document`], and by
    /// [`serialize_node`] on a document (`Root`) node; a node written on its own
    /// is a fragment and never gets one.
    pub xml_declaration: bool,
    /// Adds ` standalone="yes"` / ` standalone="no"` to that declaration.
    ///
    /// Ignored when `xml_declaration` is `false`.
    pub standalone: Option<bool>,
}

/// Why a tree could not be written as XML.
///
/// Every variant but [`Io`](SerializeError::Io) reports content that no
/// well-formed XML document can hold, so it is reported rather than silently
/// repaired; repairing names is the job of the code that builds the tree.
///
/// ```
/// use xsd_schema::document::{serialize, SerializeError};
/// use xsd_schema::navigator::{DomNavigator, RoXmlNavigator};
///
/// // An attribute is not a tree, so it cannot be written on its own.
/// let doc = roxmltree::Document::parse(r#"<a k="v"/>"#)?;
/// let mut attr = RoXmlNavigator::new(&doc);
/// attr.move_to_first_child();
/// attr.move_to_first_attribute();
/// match serialize::to_string(&attr, &Default::default()) {
///     Err(SerializeError::NotSerializable(kind)) => println!("refused {kind:?}"),
///     other => panic!("expected a refusal, got {other:?}"),
/// }
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, thiserror::Error)]
pub enum SerializeError {
    /// The writer failed.
    #[error("failed to write XML: {0}")]
    Io(#[from] std::io::Error),

    /// A character that the XML 1.0 `Char` production does not allow, in a
    /// place where no escape can express it either.
    ///
    /// `context` is one of `"text"`, `"attribute value"`, `"comment"`,
    /// `"processing instruction"` or `"name"`.
    #[error("U+{code_point:04X} is not an XML 1.0 Char and cannot appear in {context}")]
    InvalidChar {
        /// The offending Unicode scalar value.
        code_point: u32,
        /// Where it was found.
        context: &'static str,
    },

    /// Comment content containing `--`, or ending in `-` (XML 1.0 §2.5).
    #[error("comment content is not well-formed (XML 1.0 \u{a7}2.5): {0:?}")]
    InvalidComment(String),

    /// A processing instruction that XML 1.0 §2.6 does not allow.
    #[error("processing instruction {target:?} is not well-formed (XML 1.0 \u{a7}2.6): {reason}")]
    InvalidPi {
        /// The PI target as found in the tree.
        target: String,
        /// What is wrong with it.
        reason: &'static str,
    },

    /// An element or attribute name whose prefix is not bound to its namespace
    /// URI in the scope the output establishes, so the written name would not
    /// re-parse to the same expanded name.
    #[error("prefix {prefix:?} is not bound to namespace {uri:?} in the output scope")]
    UnboundName {
        /// The name's prefix (empty for an unprefixed name).
        prefix: String,
        /// The namespace URI the node claims.
        uri: String,
    },

    /// An attribute or namespace node was asked to serialize itself. Both are
    /// properties of an element, not subtrees, so there is nothing to write.
    #[error("a {0:?} node cannot be serialized on its own")]
    NotSerializable(DomNodeType),
}

/// Writes the document containing `root`, optionally preceded by an XML
/// declaration.
///
/// `root` may be positioned anywhere in the document: the serializer starts from
/// its document root, writing every top-level node — comments and processing
/// instructions around the document element included — in document order.
///
/// ```
/// use bumpalo::Bump;
/// use xsd_schema::document::{serialize, BufferDocument, SerializeOptions};
/// use xsd_schema::namespace::NameTable;
///
/// let arena = Bump::new();
/// let names = NameTable::new();
/// let doc = BufferDocument::from_reader_default(
///     "<?work now?><a/><!--after-->".as_bytes(),
///     &arena,
///     &names,
/// )?;
///
/// let mut out = Vec::new();
/// let opts = SerializeOptions { xml_declaration: true, ..Default::default() };
/// serialize::serialize_document(&doc.create_navigator(), &mut out, &opts)?;
/// assert_eq!(
///     String::from_utf8(out)?,
///     r#"<?xml version="1.0" encoding="UTF-8"?><?work now?><a/><!--after-->"#,
/// );
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn serialize_document<N: DomNavigator, W: Write>(
    root: &N,
    out: W,
    opts: &SerializeOptions,
) -> Result<(), SerializeError> {
    let mut nav = root.clone();
    nav.move_to_root();
    Serializer::new(out, opts).write_document(&nav)
}

/// Writes the node at the cursor.
///
/// * On a document (`Root`) node this is [`serialize_document`].
/// * On an element it writes that subtree. The element receives the namespace
///   declarations it needs, inherited ones included, so the fragment stands on
///   its own.
/// * On a text, comment or processing-instruction node it writes that one node.
/// * On an attribute or namespace node it is
///   [`SerializeError::NotSerializable`].
///
/// ```
/// use bumpalo::Bump;
/// use xsd_schema::document::{serialize, BufferDocument};
/// use xsd_schema::namespace::NameTable;
/// use xsd_schema::navigator::DomNavigator;
///
/// let arena = Bump::new();
/// let names = NameTable::new();
/// let doc = BufferDocument::from_reader_default(
///     r#"<a xmlns:p="urn:x"><p:b/></a>"#.as_bytes(),
///     &arena,
///     &names,
/// )?;
///
/// let mut inner = doc.create_navigator();
/// inner.move_to_first_child(); // <a>
/// inner.move_to_first_child(); // <p:b>
///
/// // `xmlns:p` is declared on <a>, and travels with the subtree.
/// let mut out = Vec::new();
/// serialize::serialize_node(&inner, &mut out, &Default::default())?;
/// assert_eq!(String::from_utf8(out)?, r#"<p:b xmlns:p="urn:x"/>"#);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn serialize_node<N: DomNavigator, W: Write>(
    node: &N,
    out: W,
    opts: &SerializeOptions,
) -> Result<(), SerializeError> {
    let kind = node.node_type();
    let mut ser = Serializer::new(out, opts);
    match kind {
        DomNodeType::Root => ser.write_document(node),
        DomNodeType::Attribute | DomNodeType::Namespace | DomNodeType::All => {
            Err(SerializeError::NotSerializable(kind))
        }
        _ => ser.write_subtree(node),
    }
}

/// [`serialize_node`] into a `String`.
///
/// ```
/// use xsd_schema::document::serialize;
/// use xsd_schema::navigator::{DomNavigator, RoXmlNavigator};
///
/// let doc = roxmltree::Document::parse("<a>1 &lt; 2</a>")?;
/// let mut nav = RoXmlNavigator::new(&doc);
/// nav.move_to_first_child();
///
/// assert_eq!(
///     serialize::to_string(&nav, &Default::default())?,
///     "<a>1 &lt; 2</a>",
/// );
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn to_string<N: DomNavigator>(
    node: &N,
    opts: &SerializeOptions,
) -> Result<String, SerializeError> {
    let mut buf: Vec<u8> = Vec::new();
    serialize_node(node, &mut buf, opts)?;
    // Every byte written came from a `&str`, so the buffer is valid UTF-8.
    Ok(String::from_utf8(buf).expect("serializer writes UTF-8"))
}

// ── Implementation ───────────────────────────────────────────────────────

/// The `xml` prefix, which is bound implicitly everywhere and is therefore
/// never declared in the output.
const XML_PREFIX: &str = "xml";

/// True for a Unicode scalar the XML 1.0 (Fifth Edition) `Char` production
/// excludes: `Char ::= #x9 | #xA | #xD | [#x20-#xD7FF] | [#xE000-#xFFFD] |
/// [#x10000-#x10FFFF]`. Surrogates cannot occur in a Rust `str`, so what is
/// left is the C0 controls other than tab/LF/CR plus U+FFFE and U+FFFF.
fn is_forbidden_char(ch: char) -> bool {
    match ch {
        '\t' | '\n' | '\r' => false,
        c if (c as u32) < 0x20 => true,
        '\u{FFFE}' | '\u{FFFF}' => true,
        _ => false,
    }
}

/// The single in-scope namespace stack plus the writer.
struct Serializer<'o, W: Write> {
    out: W,
    opts: &'o SerializeOptions,
    /// In-scope `(prefix, uri)` bindings, innermost last. An entry with an
    /// empty prefix *and* an empty URI is an undeclared default namespace.
    /// This is the serializer's only per-node allocation.
    ns_stack: Vec<(String, String)>,
}

impl<'o, W: Write> Serializer<'o, W> {
    fn new(out: W, opts: &'o SerializeOptions) -> Self {
        Self {
            out,
            opts,
            ns_stack: Vec::new(),
        }
    }

    // ── Namespace scope ──────────────────────────────────────────────

    /// The namespace URI `prefix` resolves to in the scope written so far, or
    /// `None` when the prefix is not usable there.
    ///
    /// The empty prefix always resolves (to the empty URI when no default
    /// namespace is in scope); `xml` resolves without a declaration.
    fn in_scope(&self, prefix: &str) -> Option<&str> {
        if prefix == XML_PREFIX {
            return Some(XML_NAMESPACE);
        }
        match self
            .ns_stack
            .iter()
            .rev()
            .find(|(p, _)| p == prefix)
            .map(|(_, u)| u.as_str())
        {
            // A prefixed binding to the empty URI is an XML 1.1 undeclaration;
            // it is never written, so the prefix is not usable here.
            Some("") if !prefix.is_empty() => None,
            Some(uri) => Some(uri),
            None if prefix.is_empty() => Some(""),
            None => None,
        }
    }

    fn is_bound(&self, prefix: &str, uri: &str) -> bool {
        self.in_scope(prefix) == Some(uri)
    }

    /// Whether a non-empty default namespace is in scope, i.e. whether an
    /// element with no default namespace needs `xmlns=""`.
    fn has_default_namespace(&self) -> bool {
        matches!(self.in_scope(""), Some(uri) if !uri.is_empty())
    }

    fn push_binding(&mut self, prefix: &str, uri: &str) {
        self.ns_stack.push((prefix.to_string(), uri.to_string()));
    }

    // ── Node dispatch ────────────────────────────────────────────────

    fn write_document<N: DomNavigator>(&mut self, node: &N) -> Result<(), SerializeError> {
        if self.opts.xml_declaration {
            self.out
                .write_all(br#"<?xml version="1.0" encoding="UTF-8""#)?;
            match self.opts.standalone {
                Some(true) => self.out.write_all(br#" standalone="yes""#)?,
                Some(false) => self.out.write_all(br#" standalone="no""#)?,
                None => {}
            }
            self.out.write_all(b"?>")?;
        }
        if node.node_type() != DomNodeType::Root {
            // A fragment tree whose visible root is an element (an assertion
            // fragment, say) has no document node to walk into.
            return self.write_subtree(node);
        }
        let mut child = node.clone();
        if child.move_to_first_child() {
            loop {
                self.write_subtree(&child)?;
                if !child.move_to_next_sibling() {
                    break;
                }
            }
        }
        Ok(())
    }

    /// Writes the subtree at the cursor, iteratively: `frames` holds one
    /// `ns_stack` watermark per open element, so descending never recurses and
    /// a deep document cannot overflow the Rust stack.
    fn write_subtree<N: DomNavigator>(&mut self, start: &N) -> Result<(), SerializeError> {
        let mut nav = start.clone();
        let mut frames: Vec<usize> = Vec::new();
        loop {
            let mut descended = false;
            match nav.node_type() {
                DomNodeType::Element => {
                    let watermark = self.ns_stack.len();
                    self.write_start_tag(&nav)?;
                    let mut child = nav.clone();
                    if child.move_to_first_child() {
                        self.out.write_all(b">")?;
                        frames.push(watermark);
                        nav = child;
                        descended = true;
                    } else {
                        self.out.write_all(b"/>")?;
                        self.ns_stack.truncate(watermark);
                    }
                }
                kind => self.write_leaf(&nav, kind)?,
            }
            if descended {
                continue;
            }
            // Advance to the next sibling, closing elements on the way up.
            loop {
                if frames.is_empty() {
                    return Ok(());
                }
                if nav.move_to_next_sibling() {
                    break;
                }
                nav.move_to_parent();
                let watermark = frames.pop().expect("checked non-empty");
                self.write_end_tag(&nav)?;
                self.ns_stack.truncate(watermark);
            }
        }
    }

    fn write_leaf<N: DomNavigator>(
        &mut self,
        nav: &N,
        kind: DomNodeType,
    ) -> Result<(), SerializeError> {
        match kind {
            DomNodeType::Text | DomNodeType::Whitespace | DomNodeType::SignificantWhitespace => {
                self.write_text(&nav.value_ref())
            }
            DomNodeType::Comment => {
                let value = nav.value_ref();
                check_chars(&value, "comment")?;
                if value.contains("--") || value.ends_with('-') {
                    return Err(SerializeError::InvalidComment(value.into_owned()));
                }
                self.out.write_all(b"<!--")?;
                self.out.write_all(value.as_bytes())?;
                self.out.write_all(b"-->")?;
                Ok(())
            }
            DomNodeType::ProcessingInstruction => {
                let target = nav.local_name();
                check_chars(target, "name")?;
                if target.eq_ignore_ascii_case(XML_PREFIX) {
                    return Err(SerializeError::InvalidPi {
                        target: target.to_string(),
                        reason: "the target `xml` is reserved",
                    });
                }
                let data = nav.value_ref();
                check_chars(&data, "processing instruction")?;
                if data.contains("?>") {
                    return Err(SerializeError::InvalidPi {
                        target: target.to_string(),
                        reason: "the data contains `?>`",
                    });
                }
                self.out.write_all(b"<?")?;
                self.out.write_all(target.as_bytes())?;
                if !data.is_empty() {
                    self.out.write_all(b" ")?;
                    self.out.write_all(data.as_bytes())?;
                }
                self.out.write_all(b"?>")?;
                Ok(())
            }
            other => Err(SerializeError::NotSerializable(other)),
        }
    }

    // ── Elements ─────────────────────────────────────────────────────

    /// Writes `<qname` plus the declarations this element introduces and all of
    /// its attributes, leaving the caller to add `>` or `/>`.
    fn write_start_tag<N: DomNavigator>(&mut self, nav: &N) -> Result<(), SerializeError> {
        self.out.write_all(b"<")?;
        self.write_qname(nav.prefix(), nav.local_name())?;
        self.write_namespace_declarations(nav)?;

        // Invariant, not fixup: the name we just wrote must read back the same.
        let (prefix, uri) = (nav.prefix(), nav.namespace_uri());
        if !self.is_bound(prefix, uri) {
            return Err(SerializeError::UnboundName {
                prefix: prefix.to_string(),
                uri: uri.to_string(),
            });
        }

        let mut attr = nav.clone();
        if attr.move_to_first_attribute() {
            loop {
                let (prefix, uri) = (attr.prefix(), attr.namespace_uri());
                // An unprefixed attribute is in no namespace: the default
                // namespace does not apply to it (Namespaces in XML §6.2).
                let bound = if prefix.is_empty() {
                    uri.is_empty()
                } else {
                    self.is_bound(prefix, uri)
                };
                if !bound {
                    return Err(SerializeError::UnboundName {
                        prefix: prefix.to_string(),
                        uri: uri.to_string(),
                    });
                }
                self.out.write_all(b" ")?;
                self.write_qname(prefix, attr.local_name())?;
                self.out.write_all(b"=\"")?;
                self.write_attribute_value(&attr.value_ref())?;
                self.out.write_all(b"\"")?;
                if !attr.move_to_next_attribute() {
                    break;
                }
            }
        }
        Ok(())
    }

    /// Emits exactly the declarations that change the scope: the element's
    /// in-scope bindings minus what is already bound identically, plus
    /// `xmlns=""` when the element leaves an inherited default namespace.
    ///
    /// The declarations are collected onto the in-scope stack first and written
    /// from there in prefix order (the default namespace first). The
    /// `namespace::` axis has no defined order and the backends disagree on the
    /// one they use, so sorting is what makes the output a function of the tree
    /// rather than of the navigator behind it. It needs no allocation of its
    /// own: the entries are sorted in place, above this element's watermark.
    fn write_namespace_declarations<N: DomNavigator>(
        &mut self,
        nav: &N,
    ) -> Result<(), SerializeError> {
        let watermark = self.ns_stack.len();
        let mut axis_has_default = false;
        let mut ns = nav.clone();
        if ns.move_to_first_namespace(NamespaceAxisScope::ExcludeXml) {
            loop {
                let prefix = ns.local_name();
                let uri = ns.value_ref();
                // `ExcludeXml` already drops it; belt and braces, because the
                // `xml` prefix must never be declared.
                if prefix != XML_PREFIX {
                    if prefix.is_empty() {
                        if uri.is_empty() {
                            // An undeclared default namespace, recorded by the
                            // tree; handled below, where the inherited scope is
                            // known.
                        } else {
                            axis_has_default = true;
                            if !self.is_bound("", &uri) {
                                self.push_binding("", &uri);
                            }
                        }
                    } else if !uri.is_empty() && !self.is_bound(prefix, &uri) {
                        self.push_binding(prefix, &uri);
                    }
                    // A prefixed binding to the empty URI is an XML 1.1
                    // undeclaration, which is never written.
                }
                if !ns.move_to_next_namespace(NamespaceAxisScope::ExcludeXml) {
                    break;
                }
            }
        }
        if !axis_has_default && self.has_default_namespace() {
            self.push_binding("", "");
        }

        self.ns_stack[watermark..].sort_by(|(a, _), (b, _)| a.cmp(b));
        // Disjoint field borrows: the writer and the stack are independent.
        let out = &mut self.out;
        for (prefix, uri) in &self.ns_stack[watermark..] {
            write_declaration(out, prefix, uri)?;
        }
        Ok(())
    }

    fn write_end_tag<N: DomNavigator>(&mut self, nav: &N) -> Result<(), SerializeError> {
        self.out.write_all(b"</")?;
        self.write_qname(nav.prefix(), nav.local_name())?;
        self.out.write_all(b">")?;
        Ok(())
    }

    fn write_qname(&mut self, prefix: &str, local: &str) -> Result<(), SerializeError> {
        write_qname(&mut self.out, prefix, local)
    }

    fn write_text(&mut self, text: &str) -> Result<(), SerializeError> {
        write_text(&mut self.out, text)
    }

    fn write_attribute_value(&mut self, value: &str) -> Result<(), SerializeError> {
        write_attribute_value(&mut self.out, value)
    }
}

// ── Writing, independent of the cursor ───────────────────────────────────

fn write_declaration<W: Write>(out: &mut W, prefix: &str, uri: &str) -> Result<(), SerializeError> {
    check_chars(prefix, "name")?;
    if prefix.is_empty() {
        out.write_all(b" xmlns=\"")?;
    } else {
        out.write_all(b" xmlns:")?;
        out.write_all(prefix.as_bytes())?;
        out.write_all(b"=\"")?;
    }
    write_attribute_value(out, uri)?;
    out.write_all(b"\"")?;
    Ok(())
}

fn write_qname<W: Write>(out: &mut W, prefix: &str, local: &str) -> Result<(), SerializeError> {
    check_chars(prefix, "name")?;
    check_chars(local, "name")?;
    if !prefix.is_empty() {
        out.write_all(prefix.as_bytes())?;
        out.write_all(b":")?;
    }
    out.write_all(local.as_bytes())?;
    Ok(())
}

fn write_text<W: Write>(out: &mut W, text: &str) -> Result<(), SerializeError> {
    let mut written = 0;
    for (offset, ch) in text.char_indices() {
        let escape = match ch {
            '&' => "&amp;",
            '<' => "&lt;",
            // Always escaped, which covers the `]]>` prohibition of XML 1.0
            // §2.4 without scanning for it.
            '>' => "&gt;",
            // A literal CR would be turned into LF by the end-of-line handling
            // of XML 1.0 §2.11 on re-parse.
            '\r' => "&#xD;",
            c if is_forbidden_char(c) => {
                return Err(SerializeError::InvalidChar {
                    code_point: c as u32,
                    context: "text",
                })
            }
            _ => continue,
        };
        out.write_all(&text.as_bytes()[written..offset])?;
        out.write_all(escape.as_bytes())?;
        written = offset + ch.len_utf8();
    }
    out.write_all(&text.as_bytes()[written..])?;
    Ok(())
}

fn write_attribute_value<W: Write>(out: &mut W, value: &str) -> Result<(), SerializeError> {
    let mut written = 0;
    for (offset, ch) in value.char_indices() {
        let escape = match ch {
            '&' => "&amp;",
            '<' => "&lt;",
            // The quote character, because values are always `"`-quoted.
            '"' => "&quot;",
            // Whitespace would be collapsed to a space by the attribute-value
            // normalization of XML 1.0 §3.3.3 on re-parse.
            '\t' => "&#x9;",
            '\n' => "&#xA;",
            '\r' => "&#xD;",
            c if is_forbidden_char(c) => {
                return Err(SerializeError::InvalidChar {
                    code_point: c as u32,
                    context: "attribute value",
                })
            }
            _ => continue,
        };
        out.write_all(&value.as_bytes()[written..offset])?;
        out.write_all(escape.as_bytes())?;
        written = offset + ch.len_utf8();
    }
    out.write_all(&value.as_bytes()[written..])?;
    Ok(())
}

/// Rejects content that cannot be written at all, for the contexts that are
/// copied out verbatim (comments, PI targets and data, names).
fn check_chars(text: &str, context: &'static str) -> Result<(), SerializeError> {
    match text.chars().find(|&c| is_forbidden_char(c)) {
        Some(c) => Err(SerializeError::InvalidChar {
            code_point: c as u32,
            context,
        }),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{BufferDocument, BufferDocumentBuilder, BufferDocumentOptions};
    use crate::namespace::NameTable;
    use crate::navigator::RoXmlNavigator;
    use bumpalo::Bump;

    /// Parses `xml` into a `BufferDocument` and returns its serialization.
    fn round(xml: &str) -> String {
        let arena = Bump::new();
        let names = NameTable::new();
        let doc = BufferDocument::from_reader_default(xml.as_bytes(), &arena, &names)
            .expect("the fixture parses");
        to_string(&doc.create_navigator(), &SerializeOptions::default()).expect("serializes")
    }

    /// Serializes `xml` through the roxmltree backend instead.
    fn round_roxml(xml: &str) -> String {
        let doc = roxmltree::Document::parse(xml).expect("the fixture parses");
        to_string(&RoXmlNavigator::new(&doc), &SerializeOptions::default()).expect("serializes")
    }

    /// Builds a tree by hand, so that content XML cannot express can be tested.
    fn built<F>(f: F) -> Result<String, SerializeError>
    where
        F: FnOnce(&mut BufferDocumentBuilder<'_>),
    {
        let arena = Bump::new();
        let names = NameTable::new();
        let mut builder =
            BufferDocumentBuilder::new(&arena, &names, None, BufferDocumentOptions::default())
                .expect("builder");
        f(&mut builder);
        let doc = builder.finalize().expect("finalize");
        to_string(&doc.create_navigator(), &SerializeOptions::default())
    }

    // ── Escaping matrix (§4.2) ───────────────────────────────────────

    #[test]
    fn text_escapes_ampersand_lt_and_gt() {
        // `>` is escaped unconditionally, which is what makes the `]]>`
        // prohibition of XML 1.0 §2.4 impossible to hit.
        assert_eq!(
            round("<a>&amp; &lt; &gt; ]]&gt; ' \"</a>"),
            "<a>&amp; &lt; &gt; ]]&gt; ' \"</a>"
        );
    }

    #[test]
    fn text_escapes_carriage_return_only() {
        // CR must become a reference (XML 1.0 §2.11 would fold it to LF);
        // tab and LF survive as themselves in text.
        let out = built(|b| {
            b.start_element("a", "", "", &[]).unwrap();
            b.end_of_attributes();
            b.text("x\ry\tz\nw");
            b.end_element().unwrap();
        })
        .expect("serializes");
        assert_eq!(out, "<a>x&#xD;y\tz\nw</a>");
    }

    #[test]
    fn attribute_escapes_quote_lt_and_ampersand_but_not_gt() {
        assert_eq!(
            round(r#"<a k="&amp; &lt; &gt; &quot; '"/>"#),
            r#"<a k="&amp; &lt; > &quot; '"/>"#
        );
    }

    #[test]
    fn attribute_escapes_all_three_whitespace_characters() {
        // Written as references because XML 1.0 §3.3.3 would otherwise
        // normalize them to plain spaces on re-parse.
        let out = built(|b| {
            b.start_element("a", "", "", &[]).unwrap();
            b.attribute("k", "", "", "x\ty\nz\rw").unwrap();
            b.end_of_attributes();
            b.end_element().unwrap();
        })
        .expect("serializes");
        assert_eq!(out, r#"<a k="x&#x9;y&#xA;z&#xD;w"/>"#);
        // And the round trip holds: a re-parse gives the value back.
        let doc = roxmltree::Document::parse(&out).expect("output parses");
        assert_eq!(doc.root_element().attribute("k"), Some("x\ty\nz\rw"));
    }

    #[test]
    fn text_and_attribute_escaping_differ_only_where_specified() {
        // One fixture, both contexts: `>` in text vs `>` in an attribute.
        assert_eq!(round(r#"<a k="&gt;">&gt;</a>"#), r#"<a k=">">&gt;</a>"#);
    }

    // ── Refused content ──────────────────────────────────────────────

    #[test]
    fn forbidden_character_in_text_is_an_error() {
        let err = built(|b| {
            b.start_element("a", "", "", &[]).unwrap();
            b.end_of_attributes();
            b.text("x\u{1}y");
            b.end_element().unwrap();
        })
        .expect_err("U+0001 is not an XML 1.0 Char");
        assert!(matches!(
            err,
            SerializeError::InvalidChar {
                code_point: 1,
                context: "text"
            }
        ));
    }

    #[test]
    fn forbidden_character_in_attribute_value_is_an_error() {
        let err = built(|b| {
            b.start_element("a", "", "", &[]).unwrap();
            b.attribute("k", "", "", "\u{FFFF}").unwrap();
            b.end_of_attributes();
            b.end_element().unwrap();
        })
        .expect_err("U+FFFF is not an XML 1.0 Char");
        assert!(matches!(
            err,
            SerializeError::InvalidChar {
                code_point: 0xFFFF,
                context: "attribute value"
            }
        ));
    }

    #[test]
    fn forbidden_character_in_name_is_an_error() {
        let err = built(|b| {
            b.start_element("a\u{b}b", "", "", &[]).unwrap();
            b.end_of_attributes();
            b.end_element().unwrap();
        })
        .expect_err("U+000B is not an XML 1.0 Char");
        assert!(matches!(
            err,
            SerializeError::InvalidChar {
                code_point: 0xB,
                context: "name"
            }
        ));
    }

    #[test]
    fn comment_with_double_hyphen_is_an_error() {
        let err = built(|b| {
            b.start_element("a", "", "", &[]).unwrap();
            b.end_of_attributes();
            b.comment("x--y").unwrap();
            b.end_element().unwrap();
        })
        .expect_err("XML 1.0 §2.5 forbids `--` inside a comment");
        assert!(matches!(err, SerializeError::InvalidComment(s) if s == "x--y"));
    }

    #[test]
    fn comment_ending_in_hyphen_is_an_error() {
        let err = built(|b| {
            b.start_element("a", "", "", &[]).unwrap();
            b.end_of_attributes();
            b.comment("x-").unwrap();
            b.end_element().unwrap();
        })
        .expect_err("XML 1.0 §2.5 forbids a comment ending in `-`");
        assert!(matches!(err, SerializeError::InvalidComment(s) if s == "x-"));
    }

    #[test]
    fn forbidden_character_in_comment_is_an_error() {
        let err = built(|b| {
            b.start_element("a", "", "", &[]).unwrap();
            b.end_of_attributes();
            b.comment("x\u{0}y").unwrap();
            b.end_element().unwrap();
        })
        .expect_err("NUL is not an XML 1.0 Char");
        assert!(matches!(
            err,
            SerializeError::InvalidChar {
                code_point: 0,
                context: "comment"
            }
        ));
    }

    #[test]
    fn reserved_pi_target_is_an_error() {
        for target in ["xml", "XML", "xMl"] {
            let err = built(|b| {
                b.start_element("a", "", "", &[]).unwrap();
                b.end_of_attributes();
                b.processing_instruction(target, "x").unwrap();
                b.end_element().unwrap();
            })
            .expect_err("XML 1.0 §2.6 reserves the target `xml`");
            assert!(
                matches!(&err, SerializeError::InvalidPi { target: t, .. } if t == target),
                "{err:?}"
            );
        }
    }

    #[test]
    fn pi_data_containing_the_close_delimiter_is_an_error() {
        let err = built(|b| {
            b.start_element("a", "", "", &[]).unwrap();
            b.end_of_attributes();
            b.processing_instruction("go", "x?>y").unwrap();
            b.end_element().unwrap();
        })
        .expect_err("PI data cannot contain `?>`");
        assert!(matches!(
            err,
            SerializeError::InvalidPi {
                reason: "the data contains `?>`",
                ..
            }
        ));
    }

    #[test]
    fn unbound_element_prefix_is_an_error() {
        // A name the output scope cannot express: refused, never repaired.
        let err = built(|b| {
            b.start_element("x", "urn:x", "p", &[]).unwrap();
            b.end_of_attributes();
            b.end_element().unwrap();
        })
        .expect_err("prefix `p` is not declared anywhere");
        assert!(matches!(err, SerializeError::UnboundName { prefix, uri }
            if prefix == "p" && uri == "urn:x"));
    }

    #[test]
    fn unbound_attribute_prefix_is_an_error() {
        let err = built(|b| {
            b.start_element("a", "", "", &[]).unwrap();
            b.attribute("k", "urn:x", "p", "v").unwrap();
            b.end_of_attributes();
            b.end_element().unwrap();
        })
        .expect_err("prefix `p` is not declared anywhere");
        assert!(matches!(err, SerializeError::UnboundName { prefix, .. } if prefix == "p"));
    }

    #[test]
    fn unprefixed_attribute_in_a_namespace_is_an_error() {
        // An unprefixed attribute name always re-parses to no namespace
        // (Namespaces in XML §6.2), so this name cannot be written.
        let err = built(|b| {
            b.start_element("a", "urn:d", "", &[("", "urn:d")]).unwrap();
            b.attribute("k", "urn:d", "", "v").unwrap();
            b.end_of_attributes();
            b.end_element().unwrap();
        })
        .expect_err("an unprefixed attribute cannot be in a namespace");
        assert!(matches!(err, SerializeError::UnboundName { prefix, uri }
            if prefix.is_empty() && uri == "urn:d"));
    }

    #[test]
    fn the_xml_prefix_needs_no_declaration() {
        // `xml` is bound everywhere and is never written, but names using it
        // still pass the scope check.
        assert_eq!(round(r#"<a xml:lang="en"/>"#), r#"<a xml:lang="en"/>"#);
    }

    #[test]
    fn attribute_node_is_not_serializable() {
        let doc = roxmltree::Document::parse(r#"<a k="v"/>"#).expect("parses");
        let mut nav = RoXmlNavigator::new(&doc);
        assert!(nav.move_to_first_child());
        assert!(nav.move_to_first_attribute());
        let err = to_string(&nav, &SerializeOptions::default()).expect_err("attribute is refused");
        assert!(matches!(
            err,
            SerializeError::NotSerializable(DomNodeType::Attribute)
        ));
    }

    #[test]
    fn namespace_node_is_not_serializable() {
        let doc = roxmltree::Document::parse(r#"<a xmlns:p="urn:x"/>"#).expect("parses");
        let mut nav = RoXmlNavigator::new(&doc);
        assert!(nav.move_to_first_child());
        assert!(nav.move_to_first_namespace(NamespaceAxisScope::ExcludeXml));
        let err = to_string(&nav, &SerializeOptions::default()).expect_err("namespace is refused");
        assert!(matches!(
            err,
            SerializeError::NotSerializable(DomNodeType::Namespace)
        ));
    }

    // ── Namespace declarations (§4.3) ────────────────────────────────

    #[test]
    fn redundant_redeclaration_is_dropped() {
        // The child re-declares what it already inherits; the output declares
        // it once, where it is introduced.
        assert_eq!(
            round(r#"<p:a xmlns:p="urn:x"><p:b xmlns:p="urn:x"/></p:a>"#),
            r#"<p:a xmlns:p="urn:x"><p:b/></p:a>"#
        );
    }

    #[test]
    fn shadowing_redeclaration_is_kept() {
        assert_eq!(
            round(r#"<p:a xmlns:p="urn:x"><p:b xmlns:p="urn:y"/></p:a>"#),
            r#"<p:a xmlns:p="urn:x"><p:b xmlns:p="urn:y"/></p:a>"#
        );
    }

    #[test]
    fn default_namespace_undeclaration_is_written() {
        assert_eq!(
            round(r#"<a xmlns="urn:d"><b xmlns=""><c/></b></a>"#),
            r#"<a xmlns="urn:d"><b xmlns=""><c/></b></a>"#
        );
    }

    #[test]
    fn undeclaration_is_not_repeated_on_descendants() {
        let out = round(r#"<a xmlns="urn:d"><b xmlns=""><c><d/></c></b></a>"#);
        assert_eq!(out, r#"<a xmlns="urn:d"><b xmlns=""><c><d/></c></b></a>"#);
        assert_eq!(out.matches(r#"xmlns="""#).count(), 1);
    }

    #[test]
    fn undeclaration_is_not_written_without_a_default_namespace() {
        assert_eq!(round("<a><b/></a>"), "<a><b/></a>");
    }

    #[test]
    fn subtree_gains_the_declarations_it_inherits() {
        let arena = Bump::new();
        let names = NameTable::new();
        let doc = BufferDocument::from_reader_default(
            r#"<a xmlns:p="urn:x" xmlns:q="urn:y" xmlns="urn:d"><p:b q:k="v"/></a>"#.as_bytes(),
            &arena,
            &names,
        )
        .expect("parses");
        let mut inner = doc.create_navigator();
        assert!(inner.move_to_first_child());
        assert!(inner.move_to_first_child());

        let out = to_string(&inner, &SerializeOptions::default()).expect("serializes");
        // Every binding in scope at <p:b> is re-established on it: the two
        // prefixes it uses and the default namespace it sits in.
        let reparsed = roxmltree::Document::parse(&out).expect("output parses");
        let elem = reparsed.root_element();
        assert_eq!(elem.tag_name().namespace(), Some("urn:x"));
        assert_eq!(elem.tag_name().name(), "b");
        assert_eq!(elem.attribute(("urn:y", "k")), Some("v"));
        assert!(out.contains(r#"xmlns:p="urn:x""#), "{out}");
        assert!(out.contains(r#"xmlns:q="urn:y""#), "{out}");
        assert!(out.contains(r#"xmlns="urn:d""#), "{out}");
    }

    #[test]
    fn declarations_come_out_in_prefix_order() {
        // Source order is not preserved — the axis does not have it to give —
        // but the order is a function of the tree, so both backends agree.
        let xml = r#"<a xmlns:z="urn:z" xmlns:b="urn:b" xmlns="urn:d"/>"#;
        let expected = r#"<a xmlns="urn:d" xmlns:b="urn:b" xmlns:z="urn:z"/>"#;
        assert_eq!(round(xml), expected);
        assert_eq!(round_roxml(xml), expected);
    }

    #[test]
    fn declarations_are_scoped_to_their_element() {
        // `q` is introduced on the first child only, so the second child must
        // not carry it.
        assert_eq!(
            round(r#"<a><b xmlns:q="urn:y"/><c/></a>"#),
            r#"<a><b xmlns:q="urn:y"/><c/></a>"#
        );
    }

    // ── Node kinds (§4.4) ────────────────────────────────────────────

    #[test]
    fn empty_element_is_collapsed() {
        assert_eq!(round("<a><b></b></a>"), "<a><b/></a>");
    }

    #[test]
    fn pi_with_empty_data_omits_the_space() {
        assert_eq!(round("<a><?go?></a>"), "<a><?go?></a>");
        assert_eq!(round("<a><?go now?></a>"), "<a><?go now?></a>");
    }

    #[test]
    fn comments_and_pis_around_the_document_element_are_kept() {
        assert_eq!(
            round("<!--before--><?go?><a/><?stop?><!--after-->"),
            "<!--before--><?go?><a/><?stop?><!--after-->"
        );
    }

    #[test]
    fn mixed_content_keeps_node_order() {
        assert_eq!(
            round("<a>t1<b/>t2<!--c-->t3<?pi d?>t4</a>"),
            "<a>t1<b/>t2<!--c-->t3<?pi d?>t4</a>"
        );
    }

    #[test]
    fn attribute_order_is_document_order() {
        assert_eq!(
            round(r#"<a z="1" m="2" a="3"/>"#),
            r#"<a z="1" m="2" a="3"/>"#
        );
    }

    #[test]
    fn a_single_leaf_node_serializes_on_its_own() {
        let arena = Bump::new();
        let names = NameTable::new();
        let doc =
            BufferDocument::from_reader_default("<a>text<!--c--></a>".as_bytes(), &arena, &names)
                .expect("parses");
        let mut nav = doc.create_navigator();
        assert!(nav.move_to_first_child());
        assert!(nav.move_to_first_child());
        assert_eq!(
            to_string(&nav, &SerializeOptions::default()).expect("serializes"),
            "text"
        );
        assert!(nav.move_to_next_sibling());
        assert_eq!(
            to_string(&nav, &SerializeOptions::default()).expect("serializes"),
            "<!--c-->"
        );
    }

    // ── The XML declaration ──────────────────────────────────────────

    #[test]
    fn xml_declaration_is_written_on_request() {
        let arena = Bump::new();
        let names = NameTable::new();
        let doc =
            BufferDocument::from_reader_default("<a/>".as_bytes(), &arena, &names).expect("parses");
        let nav = doc.create_navigator();

        let mut out = Vec::new();
        let opts = SerializeOptions {
            xml_declaration: true,
            standalone: None,
        };
        serialize_document(&nav, &mut out, &opts).expect("serializes");
        assert_eq!(
            String::from_utf8(out).unwrap(),
            r#"<?xml version="1.0" encoding="UTF-8"?><a/>"#
        );

        for (standalone, expected) in [(true, "yes"), (false, "no")] {
            let mut out = Vec::new();
            let opts = SerializeOptions {
                xml_declaration: true,
                standalone: Some(standalone),
            };
            serialize_document(&nav, &mut out, &opts).expect("serializes");
            assert_eq!(
                String::from_utf8(out).unwrap(),
                format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="{expected}"?><a/>"#)
            );
        }
    }

    #[test]
    fn no_xml_declaration_by_default() {
        assert_eq!(round("<a/>"), "<a/>");
    }

    #[test]
    fn serialize_document_starts_from_the_document_root() {
        let arena = Bump::new();
        let names = NameTable::new();
        let doc = BufferDocument::from_reader_default("<a><b/></a>".as_bytes(), &arena, &names)
            .expect("parses");
        let mut inner = doc.create_navigator();
        assert!(inner.move_to_first_child());
        assert!(inner.move_to_first_child());

        let mut out = Vec::new();
        serialize_document(&inner, &mut out, &SerializeOptions::default()).expect("serializes");
        assert_eq!(String::from_utf8(out).unwrap(), "<a><b/></a>");
    }

    #[test]
    fn serialize_node_on_the_root_writes_the_document() {
        let arena = Bump::new();
        let names = NameTable::new();
        let doc = BufferDocument::from_reader_default("<a/><!--x-->".as_bytes(), &arena, &names)
            .expect("parses");
        assert_eq!(round("<a/><!--x-->"), "<a/><!--x-->");
        assert_eq!(doc.create_navigator().node_type(), DomNodeType::Root);
    }

    // ── Backend independence ─────────────────────────────────────────

    #[test]
    fn both_backends_produce_identical_output() {
        // The serializer talks to `DomNavigator` only, so the store behind it
        // cannot change the bytes.
        for xml in [
            "<a/>",
            "<a>t1<b/>t2<!--c--><?pi d?></a>",
            r#"<a z="1" m="2"/>"#,
            r#"<p:a xmlns:p="urn:x" xmlns="urn:d"><p:b p:k="v"><c/></p:b></a>"#
                .replace("</a>", "</p:a>")
                .as_str(),
            r#"<a xmlns="urn:d"><b xmlns=""/></a>"#,
            r#"<a xml:lang="en">&amp;&lt;&gt;</a>"#,
            "<!--before--><a/><!--after-->",
        ] {
            assert_eq!(round(xml), round_roxml(xml), "backends differ on {xml}");
        }
    }

    #[test]
    fn deep_nesting_does_not_recurse() {
        // 50_000 levels: an implementation that descended by recursion would
        // overflow the stack here.
        let depth = 50_000;
        let mut xml = String::new();
        for _ in 0..depth {
            xml.push_str("<a>");
        }
        for _ in 0..depth {
            xml.push_str("</a>");
        }
        // The innermost element is childless, so it collapses.
        let expected = format!(
            "{}<a/>{}",
            "<a>".repeat(depth - 1),
            "</a>".repeat(depth - 1)
        );
        assert_eq!(round(&xml), expected);
    }

    #[test]
    fn both_backends_report_pi_data() {
        // Both navigators report a PI's data as its string value, so the data
        // survives on either backend.
        assert_eq!(round("<a><?pi d?></a>"), "<a><?pi d?></a>");
        assert_eq!(round_roxml("<a><?pi d?></a>"), "<a><?pi d?></a>");
        // Trailing whitespace is part of the data (XML 1.0 §2.6).
        assert_eq!(round("<a><?pi d ?></a>"), "<a><?pi d ?></a>");
        assert_eq!(round_roxml("<a><?pi d ?></a>"), "<a><?pi d ?></a>");
    }
}
