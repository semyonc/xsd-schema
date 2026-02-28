//! Per-constraint state management and key table types for identity-constraint
//! streaming validation.
//!
//! This module bridges parsed constraint definitions (`IdentityConstraintData`)
//! to runtime validation by providing:
//!
//! - [`CompiledIdentityConstraint`] — pre-compiled selector/field Asttrees
//! - [`KeyFieldValue`] / [`KeySequence`] — extracted field values with XSD equality
//! - [`KeyTable`] — duplicate detection for key/unique, deferred storage for keyref
//! - [`ConstraintStruct`] — per-activation state driving `ActiveAxis` matchers

use crate::ids::NameId;
use crate::namespace::table::NameTable;
use crate::parser::frames::{IdentityKind, QNameRef};
use crate::parser::location::SourceLocation;
use crate::schema::model::XsdVersion;
use crate::types::value::XmlValue;

use super::active_axis::ActiveAxis;
use super::asttree::{Asttree, IdentityXPathError};
use super::errors::{error, error_with_path, ValidationError};

use crate::arenas::IdentityConstraintData;
use crate::ids::IdentityConstraintKey;

// ---------------------------------------------------------------------------
// CompiledIdentityConstraint
// ---------------------------------------------------------------------------

/// Pre-compiled identity constraint with cloneable `Asttree` instances.
///
/// Shared across multiple activations — each activation clones the Asttrees
/// into fresh `ActiveAxis` instances.
pub(crate) struct CompiledIdentityConstraint {
    /// Arena key for this constraint.
    pub key: IdentityConstraintKey,
    /// Constraint QName (interned).
    pub name: NameId,
    /// Constraint kind: Key, Unique, or Keyref.
    pub kind: IdentityKind,
    /// Pre-compiled selector expression.
    pub selector: Asttree,
    /// Pre-compiled field expressions.
    pub fields: Vec<Asttree>,
    /// Keyref target reference (only for `IdentityKind::Keyref`).
    pub refer: Option<QNameRef>,
    /// Resolved arena key of the referenced key/unique constraint (only for keyrefs).
    /// Set by the validator after compilation via `resolve_refer_key()`.
    pub refer_key: Option<IdentityConstraintKey>,
    /// Cached `fields.len()`.
    pub field_count: usize,
    /// Target namespace of the schema that defines this constraint.
    pub target_namespace: Option<NameId>,
}

impl CompiledIdentityConstraint {
    /// Compile an identity constraint from its parsed data.
    ///
    /// The `schema_xpath_default_ns` is the schema-level fallback for the
    /// three-level `xpathDefaultNamespace` cascade (field > selector > schema).
    pub fn compile(
        data: &IdentityConstraintData,
        key: IdentityConstraintKey,
        name_table: &NameTable,
        schema_xpath_default_ns: Option<NameId>,
        target_namespace: Option<NameId>,
        xsd_version: XsdVersion,
    ) -> Result<Self, IdentityXPathError> {
        // Compile selector
        let selector = Asttree::compile_selector(
            &data.selector.xpath,
            &data.selector.ns_snapshot,
            name_table,
            data.selector.xpath_default_namespace.as_deref(),
            schema_xpath_default_ns,
            target_namespace,
            xsd_version,
        )?;

        // Determine the selector-level xpath_default_namespace for cascading to fields.
        // If the selector has its own xpathDefaultNamespace, resolve it to a NameId;
        // otherwise fall back to the schema-level value.
        let selector_level_ns = match &data.selector.xpath_default_namespace {
            Some(val) => match val.as_str() {
                "##targetNamespace" => target_namespace,
                "##local" => None,
                uri => Some(name_table.add(uri)),
            },
            None => schema_xpath_default_ns,
        };

        // Compile fields
        let fields: Vec<Asttree> = data
            .fields
            .iter()
            .map(|f| {
                Asttree::compile_field(
                    &f.xpath,
                    &f.ns_snapshot,
                    name_table,
                    f.xpath_default_namespace.as_deref(),
                    selector_level_ns,
                    target_namespace,
                    xsd_version,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;

        let field_count = fields.len();

        Ok(CompiledIdentityConstraint {
            key,
            name: data.name,
            kind: data.kind,
            selector,
            fields,
            refer: data.refer.clone(),
            refer_key: None,
            field_count,
            target_namespace,
        })
    }
}

// ---------------------------------------------------------------------------
// KeyFieldValue
// ---------------------------------------------------------------------------

/// Per-field extracted value for key sequence equality.
///
/// Equality semantics follow the XSD spec:
/// - If both have `typed_value` with the same `PrimitiveTypeCode`, compare via
///   `XmlValue::PartialEq` (structural comparison of `XmlValueKind`).
/// - Otherwise, compare `string_value` (string equality).
/// - Different primitive types are *never* equal (no type promotion for IC equality).
#[derive(Debug, Clone)]
pub(crate) struct KeyFieldValue {
    pub string_value: String,
    pub typed_value: Option<XmlValue>,
}

impl PartialEq for KeyFieldValue {
    fn eq(&self, other: &Self) -> bool {
        match (&self.typed_value, &other.typed_value) {
            (Some(a), Some(b)) => {
                let prim_a = a.primitive_type();
                let prim_b = b.primitive_type();
                match (prim_a, prim_b) {
                    (Some(pa), Some(pb)) if pa == pb => a == b,
                    (Some(_), Some(_)) => false, // different primitive types → never equal
                    _ => self.string_value == other.string_value,
                }
            }
            _ => self.string_value == other.string_value,
        }
    }
}

impl Eq for KeyFieldValue {}

// ---------------------------------------------------------------------------
// KeySequence
// ---------------------------------------------------------------------------

/// Complete key sequence from one selector match.
///
/// Each slot corresponds to a `<field>` expression. `None` means the field
/// did not select a node (missing value).
#[derive(Debug, Clone)]
pub(crate) struct KeySequence {
    pub fields: Vec<Option<KeyFieldValue>>,
}

impl KeySequence {
    /// Returns `true` if all fields have a value.
    pub fn is_complete(&self) -> bool {
        self.fields.iter().all(|f| f.is_some())
    }
}

impl PartialEq for KeySequence {
    fn eq(&self, other: &Self) -> bool {
        if self.fields.len() != other.fields.len() {
            return false;
        }
        for (a, b) in self.fields.iter().zip(other.fields.iter()) {
            match (a, b) {
                (Some(va), Some(vb)) => {
                    if va != vb {
                        return false;
                    }
                }
                (None, None) => {
                    // Both absent — equal for that slot
                }
                _ => return false,
            }
        }
        true
    }
}

impl Eq for KeySequence {}

// ---------------------------------------------------------------------------
// KeyTable
// ---------------------------------------------------------------------------

/// Collection of key sequences for one constraint activation with duplicate detection.
pub(crate) struct KeyTable {
    /// Arena key identifying which identity constraint produced this table.
    pub ic_key: IdentityConstraintKey,
    pub constraint_name: NameId,
    pub kind: IdentityKind,
    pub sequences: Vec<KeySequence>,
}

impl KeyTable {
    /// Create a new empty key table.
    pub fn new(
        ic_key: IdentityConstraintKey,
        constraint_name: NameId,
        kind: IdentityKind,
    ) -> Self {
        KeyTable {
            ic_key,
            constraint_name,
            kind,
            sequences: Vec::new(),
        }
    }

    /// Add a key sequence, performing duplicate/completeness checks as appropriate.
    ///
    /// - **Key**: error if incomplete (`cvc-identity-constraint.4.2.1`), error if
    ///   duplicate (`cvc-identity-constraint.4.2.2`).
    /// - **Unique**: check duplicate only if complete (incomplete sequences are skipped
    ///   per XSD spec).
    /// - **Keyref**: just store (deferred to `check_keyref_against`).
    pub fn add_sequence(
        &mut self,
        seq: KeySequence,
        name_table: &NameTable,
        element_path: &str,
        location: Option<SourceLocation>,
    ) -> Vec<ValidationError> {
        let mut errors = Vec::new();
        let name = name_table.resolve(self.constraint_name);

        match self.kind {
            IdentityKind::Key => {
                if !seq.is_complete() {
                    errors.push(error_with_path(
                        "cvc-identity-constraint.4.2.1",
                        format!(
                            "Key constraint '{}': not all fields have values",
                            name
                        ),
                        location.clone(),
                        element_path,
                    ));
                }
                // Check for duplicate even if incomplete (per spec, key requires
                // completeness AND uniqueness)
                if self.find_duplicate(&seq).is_some() {
                    errors.push(error_with_path(
                        "cvc-identity-constraint.4.2.2",
                        format!(
                            "Key constraint '{}': duplicate key value detected",
                            name
                        ),
                        location,
                        element_path,
                    ));
                }
            }
            IdentityKind::Unique => {
                // For unique, only check duplicates when the sequence is complete
                if seq.is_complete() && self.find_duplicate(&seq).is_some() {
                    errors.push(error_with_path(
                        "cvc-identity-constraint.4.2.2",
                        format!(
                            "Unique constraint '{}': duplicate key value detected",
                            name
                        ),
                        location,
                        element_path,
                    ));
                }
            }
            IdentityKind::Keyref => {
                // Just store — checked later via check_keyref_against
            }
        }

        self.sequences.push(seq);
        errors
    }

    /// Check all keyref sequences against a target key/unique table.
    ///
    /// Returns `cvc-identity-constraint.4.3` errors for unmatched keyrefs.
    pub fn check_keyref_against(
        &self,
        target: &KeyTable,
        name_table: &NameTable,
    ) -> Vec<ValidationError> {
        let mut errors = Vec::new();
        let name = name_table.resolve(self.constraint_name);

        for seq in &self.sequences {
            if !seq.is_complete() {
                // Incomplete keyref sequences are not checked
                continue;
            }
            let found = target.sequences.iter().any(|ts| ts == seq);
            if !found {
                errors.push(error(
                    "cvc-identity-constraint.4.3",
                    format!(
                        "Keyref constraint '{}': no matching key value found in referenced constraint '{}'",
                        name,
                        name_table.resolve(target.constraint_name)
                    ),
                    None,
                ));
            }
        }

        errors
    }

    /// Linear scan for a duplicate of `seq` in the existing sequences.
    fn find_duplicate(&self, seq: &KeySequence) -> Option<usize> {
        self.sequences.iter().position(|existing| existing == seq)
    }
}

// ---------------------------------------------------------------------------
// FieldCollectionFrame
// ---------------------------------------------------------------------------

/// One level of field collection for a single selector match.
///
/// A stack of these is maintained in `ConstraintStruct` to handle
/// overlapping (nested) selector matches correctly — e.g. `.//item`
/// matching both an outer and inner `<item>`.
struct FieldCollectionFrame {
    /// Field axis matchers (one per `<field>`), cloned from templates.
    fields: Vec<ActiveAxis>,
    /// Current key sequence being collected (one slot per field).
    current_key_sequence: Vec<Option<KeyFieldValue>>,
}

// ---------------------------------------------------------------------------
// ConstraintStruct
// ---------------------------------------------------------------------------

/// Per-constraint activation state during streaming validation.
///
/// Each time an identity constraint's scope element is entered, a fresh
/// `ConstraintStruct` is created. It drives selector and field `ActiveAxis`
/// matchers, collects key sequences, and accumulates them in a `KeyTable`.
///
/// Nested selector matches (e.g. `.//item` with nested `<item>` elements)
/// are handled via a stack of [`FieldCollectionFrame`]s. Each selector hit
/// pushes a new frame; each selector exit pops the top frame and finalizes
/// its key sequence.
pub(crate) struct ConstraintStruct {
    /// Arena key for the compiled constraint (used for deactivation lookup).
    pub ic_key: IdentityConstraintKey,
    /// Selector axis matcher.
    pub selector: ActiveAxis,
    /// Field Asttree templates for cloning into new collection frames.
    field_asttrees: Vec<Asttree>,
    /// Number of fields (cached from compiled constraint).
    field_count: usize,
    /// Stack of active field collection frames (one per nested selector match).
    /// Empty means no selector match is currently active.
    collection_stack: Vec<FieldCollectionFrame>,
    /// Key table accumulating complete key sequences.
    pub key_table: KeyTable,
}

impl ConstraintStruct {
    /// Create a new constraint activation state.
    ///
    /// Stores Asttree templates for cloning into fresh `ActiveAxis` instances
    /// on each selector match.
    pub fn new(compiled: &CompiledIdentityConstraint) -> Self {
        let selector = ActiveAxis::new(compiled.selector.clone());

        ConstraintStruct {
            ic_key: compiled.key,
            selector,
            field_asttrees: compiled.fields.clone(),
            field_count: compiled.field_count,
            collection_stack: Vec::new(),
            key_table: KeyTable::new(compiled.key, compiled.name, compiled.kind),
        }
    }

    /// Whether field collection is active (at least one selector match on the stack).
    #[cfg(test)]
    pub fn collecting_fields(&self) -> bool {
        !self.collection_stack.is_empty()
    }

    /// Activate the constraint for the current scope element.
    ///
    /// Returns `true` if the selector is a bare `.` match (immediate scope match).
    pub fn activate(&mut self) -> bool {
        let is_scope_match = self.selector.activate();
        if is_scope_match {
            // Bare "." — immediately start collecting fields
            self.push_field_collection();
        }
        is_scope_match
    }

    /// Handle an element start event.
    ///
    /// Advances the selector; if it matches, pushes a new field collection
    /// frame. Existing frames continue receiving element events for correct
    /// depth tracking.
    pub fn start_element(&mut self, local_name: NameId, ns: NameId) {
        self.selector.move_to_start_element(local_name, ns);

        // Advance ALL existing frames' field axes first (depth tracking for
        // outer frames that are "paused" by an inner selector match).
        for frame in &mut self.collection_stack {
            for field in &mut frame.fields {
                field.move_to_start_element(local_name, ns);
            }
        }

        if self.selector.entered_match() {
            // New selector match — push a fresh frame (does NOT advance the
            // new frame's fields because the matched element is the field scope root).
            self.push_field_collection();
        }
    }

    /// Return all field indices whose axis matches the given attribute.
    ///
    /// Multiple fields can match the same attribute (e.g. repeated or
    /// overlapping field expressions), so all matching indices are returned.
    pub fn matching_fields(&self, local_name: NameId, ns: NameId) -> Vec<usize> {
        let frame = match self.collection_stack.last() {
            Some(f) => f,
            None => return Vec::new(),
        };
        let mut indices = Vec::new();
        for (i, field) in frame.fields.iter().enumerate() {
            if field.matches_attribute(local_name, ns) {
                indices.push(i);
            }
        }
        indices
    }

    /// Store a field value at the given index in the topmost collection frame.
    pub fn set_field_value(
        &mut self,
        field_idx: usize,
        string_value: String,
        typed_value: Option<XmlValue>,
    ) {
        if let Some(frame) = self.collection_stack.last_mut() {
            if field_idx < frame.current_key_sequence.len() {
                frame.current_key_sequence[field_idx] = Some(KeyFieldValue {
                    string_value,
                    typed_value,
                });
            }
        }
    }

    /// Handle an element end event with text content for element-field matching.
    ///
    /// Advances field axes in ALL frames FIRST (to detect `exited_match` and
    /// set values from text content), THEN advances the selector.
    /// On selector exit, pops the topmost frame and finalizes its key sequence.
    pub fn end_element_with_text(
        &mut self,
        text_content: &str,
        typed_value: Option<&XmlValue>,
        name_table: &NameTable,
        element_path: &str,
        location: Option<SourceLocation>,
    ) -> Vec<ValidationError> {
        let mut errors = Vec::new();

        // 1. Advance field axes in ALL frames FIRST (detect exited element-field matches)
        for frame in &mut self.collection_stack {
            for (field_idx, field) in frame.fields.iter_mut().enumerate() {
                field.end_element();
                if field.exited_match() && frame.current_key_sequence[field_idx].is_none() {
                    // Guard: only set field value when the element has a valid simple
                    // typed_value.  For Mixed/ElementOnly/Empty content types and for
                    // nilled elements, typed_value is None — the element cannot contribute
                    // a meaningful IC field value, so the slot stays None (absent).
                    if typed_value.is_some() {
                        frame.current_key_sequence[field_idx] = Some(KeyFieldValue {
                            string_value: text_content.to_string(),
                            typed_value: typed_value.cloned(),
                        });
                    }
                }
            }
        }

        // 2. Then advance selector
        self.selector.end_element();

        // 3. If selector exits match, pop frame and finalize key sequence
        if self.selector.exited_match() {
            if let Some(frame) = self.collection_stack.pop() {
                let seq = KeySequence {
                    fields: frame.current_key_sequence,
                };
                errors.extend(self.key_table.add_sequence(
                    seq,
                    name_table,
                    element_path,
                    location,
                ));
            }
        }

        errors
    }

    /// Whether the selector axis is still active.
    pub fn is_active(&self) -> bool {
        self.selector.is_active()
    }

    /// Push a new field collection frame: clone Asttrees into fresh ActiveAxis
    /// instances and initialize an empty key sequence.
    fn push_field_collection(&mut self) {
        let fields: Vec<ActiveAxis> = self
            .field_asttrees
            .iter()
            .map(|ast| {
                let mut axis = ActiveAxis::new(ast.clone());
                axis.activate();
                axis
            })
            .collect();
        self.collection_stack.push(FieldCollectionFrame {
            fields,
            current_key_sequence: vec![None; self.field_count],
        });
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::namespace::context::NamespaceContextSnapshot;
    use crate::namespace::table::NameTable;
    use crate::schema::model::XsdVersion;
    use crate::types::value::{XmlAtomicValue, XmlValueKind};
    use crate::types::XmlTypeCode;

    // -- Helpers --

    fn make_string_field(s: &str) -> KeyFieldValue {
        KeyFieldValue {
            string_value: s.to_string(),
            typed_value: None,
        }
    }

    fn make_typed_field(s: &str, type_code: XmlTypeCode, value: XmlValueKind) -> KeyFieldValue {
        KeyFieldValue {
            string_value: s.to_string(),
            typed_value: Some(XmlValue::new(type_code, value)),
        }
    }

    fn make_name_table() -> NameTable {
        NameTable::new()
    }

    // -----------------------------------------------------------------------
    // KeyFieldValue equality
    // -----------------------------------------------------------------------

    #[test]
    fn key_field_value_untyped_same_string_equal() {
        let a = make_string_field("hello");
        let b = make_string_field("hello");
        assert_eq!(a, b);
    }

    #[test]
    fn key_field_value_untyped_different_string_not_equal() {
        let a = make_string_field("hello");
        let b = make_string_field("world");
        assert_ne!(a, b);
    }

    #[test]
    fn key_field_value_typed_same_primitive_same_value_equal() {
        let a = make_typed_field(
            "42",
            XmlTypeCode::Integer,
            XmlValueKind::Atomic(XmlAtomicValue::Integer(42.into())),
        );
        let b = make_typed_field(
            "42",
            XmlTypeCode::Integer,
            XmlValueKind::Atomic(XmlAtomicValue::Integer(42.into())),
        );
        assert_eq!(a, b);
    }

    #[test]
    fn key_field_value_typed_same_primitive_different_value_not_equal() {
        let a = make_typed_field(
            "42",
            XmlTypeCode::Integer,
            XmlValueKind::Atomic(XmlAtomicValue::Integer(42.into())),
        );
        let b = make_typed_field(
            "99",
            XmlTypeCode::Integer,
            XmlValueKind::Atomic(XmlAtomicValue::Integer(99.into())),
        );
        assert_ne!(a, b);
    }

    #[test]
    fn key_field_value_typed_different_primitive_not_equal() {
        // xs:integer(5) vs xs:string("5") — different primitive types → never equal
        let a = make_typed_field(
            "5",
            XmlTypeCode::Integer,
            XmlValueKind::Atomic(XmlAtomicValue::Integer(5.into())),
        );
        let b = make_typed_field(
            "5",
            XmlTypeCode::String,
            XmlValueKind::Atomic(XmlAtomicValue::String("5".to_string())),
        );
        assert_ne!(a, b);
    }

    #[test]
    fn key_field_value_one_typed_one_untyped_fallback_string() {
        // One typed + one untyped → fall back to string comparison
        let a = make_typed_field(
            "hello",
            XmlTypeCode::String,
            XmlValueKind::Atomic(XmlAtomicValue::String("hello".to_string())),
        );
        let b = make_string_field("hello");
        assert_eq!(a, b);

        let c = make_typed_field(
            "42",
            XmlTypeCode::Integer,
            XmlValueKind::Atomic(XmlAtomicValue::Integer(42.into())),
        );
        let d = make_string_field("99");
        assert_ne!(c, d);
    }

    // -----------------------------------------------------------------------
    // KeySequence
    // -----------------------------------------------------------------------

    #[test]
    fn key_sequence_is_complete_all_present() {
        let seq = KeySequence {
            fields: vec![
                Some(make_string_field("a")),
                Some(make_string_field("b")),
            ],
        };
        assert!(seq.is_complete());
    }

    #[test]
    fn key_sequence_is_complete_missing_field() {
        let seq = KeySequence {
            fields: vec![Some(make_string_field("a")), None],
        };
        assert!(!seq.is_complete());
    }

    #[test]
    fn key_sequence_equal() {
        let a = KeySequence {
            fields: vec![
                Some(make_string_field("x")),
                Some(make_string_field("y")),
            ],
        };
        let b = KeySequence {
            fields: vec![
                Some(make_string_field("x")),
                Some(make_string_field("y")),
            ],
        };
        assert_eq!(a, b);
    }

    #[test]
    fn key_sequence_not_equal() {
        let a = KeySequence {
            fields: vec![
                Some(make_string_field("x")),
                Some(make_string_field("y")),
            ],
        };
        let b = KeySequence {
            fields: vec![
                Some(make_string_field("x")),
                Some(make_string_field("z")),
            ],
        };
        assert_ne!(a, b);
    }

    #[test]
    fn key_sequence_both_none_equal() {
        let a = KeySequence {
            fields: vec![Some(make_string_field("x")), None],
        };
        let b = KeySequence {
            fields: vec![Some(make_string_field("x")), None],
        };
        assert_eq!(a, b);
    }

    // -----------------------------------------------------------------------
    // KeyTable duplicate detection
    // -----------------------------------------------------------------------

    #[test]
    fn key_table_key_duplicate_error() {
        let nt = make_name_table();
        let name = nt.add("pk");
        let mut table = KeyTable::new(IdentityConstraintKey::default(), name, IdentityKind::Key);

        let seq1 = KeySequence {
            fields: vec![Some(make_string_field("1"))],
        };
        let errs = table.add_sequence(seq1, &nt, "/root/item[1]", None);
        assert!(errs.is_empty());

        let seq2 = KeySequence {
            fields: vec![Some(make_string_field("1"))],
        };
        let errs = table.add_sequence(seq2, &nt, "/root/item[2]", None);
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].constraint, "cvc-identity-constraint.4.2.2");
    }

    #[test]
    fn key_table_key_incomplete_error() {
        let nt = make_name_table();
        let name = nt.add("pk");
        let mut table = KeyTable::new(IdentityConstraintKey::default(), name, IdentityKind::Key);

        let seq = KeySequence {
            fields: vec![Some(make_string_field("a")), None],
        };
        let errs = table.add_sequence(seq, &nt, "/root/item[1]", None);
        assert!(errs.iter().any(|e| e.constraint == "cvc-identity-constraint.4.2.1"));
    }

    #[test]
    fn key_table_unique_duplicate_error() {
        let nt = make_name_table();
        let name = nt.add("uq");
        let mut table = KeyTable::new(IdentityConstraintKey::default(), name, IdentityKind::Unique);

        let seq1 = KeySequence {
            fields: vec![Some(make_string_field("val"))],
        };
        let errs = table.add_sequence(seq1, &nt, "/root/item[1]", None);
        assert!(errs.is_empty());

        let seq2 = KeySequence {
            fields: vec![Some(make_string_field("val"))],
        };
        let errs = table.add_sequence(seq2, &nt, "/root/item[2]", None);
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].constraint, "cvc-identity-constraint.4.2.2");
    }

    #[test]
    fn key_table_unique_incomplete_no_error() {
        let nt = make_name_table();
        let name = nt.add("uq");
        let mut table = KeyTable::new(IdentityConstraintKey::default(), name, IdentityKind::Unique);

        let seq = KeySequence {
            fields: vec![None],
        };
        let errs = table.add_sequence(seq, &nt, "/root/item[1]", None);
        assert!(errs.is_empty());
    }

    #[test]
    fn key_table_keyref_no_error() {
        let nt = make_name_table();
        let name = nt.add("fk");
        let mut table = KeyTable::new(IdentityConstraintKey::default(), name, IdentityKind::Keyref);

        let seq = KeySequence {
            fields: vec![Some(make_string_field("anything"))],
        };
        let errs = table.add_sequence(seq, &nt, "/root/item[1]", None);
        assert!(errs.is_empty());
    }

    #[test]
    fn check_keyref_against_matching() {
        let nt = make_name_table();
        let pk_name = nt.add("pk");
        let fk_name = nt.add("fk");

        let mut key_table = KeyTable::new(IdentityConstraintKey::default(), pk_name, IdentityKind::Key);
        key_table.sequences.push(KeySequence {
            fields: vec![Some(make_string_field("1"))],
        });

        let mut keyref_table = KeyTable::new(IdentityConstraintKey::default(), fk_name, IdentityKind::Keyref);
        keyref_table.sequences.push(KeySequence {
            fields: vec![Some(make_string_field("1"))],
        });

        let errs = keyref_table.check_keyref_against(&key_table, &nt);
        assert!(errs.is_empty());
    }

    #[test]
    fn check_keyref_against_missing() {
        let nt = make_name_table();
        let pk_name = nt.add("pk");
        let fk_name = nt.add("fk");

        let key_table = KeyTable::new(IdentityConstraintKey::default(), pk_name, IdentityKind::Key);

        let mut keyref_table = KeyTable::new(IdentityConstraintKey::default(), fk_name, IdentityKind::Keyref);
        keyref_table.sequences.push(KeySequence {
            fields: vec![Some(make_string_field("missing"))],
        });

        let errs = keyref_table.check_keyref_against(&key_table, &nt);
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].constraint, "cvc-identity-constraint.4.3");
    }

    // -----------------------------------------------------------------------
    // CompiledIdentityConstraint::compile
    // -----------------------------------------------------------------------

    fn make_selector_result(xpath: &str) -> crate::parser::frames::SelectorResult {
        crate::parser::frames::SelectorResult {
            xpath: xpath.to_string(),
            xpath_default_namespace: None,
            ns_snapshot: NamespaceContextSnapshot::default(),
            id: None,
            annotation: None,
            source: None,
        }
    }

    fn make_field_result(xpath: &str) -> crate::parser::frames::FieldResult {
        crate::parser::frames::FieldResult {
            xpath: xpath.to_string(),
            xpath_default_namespace: None,
            ns_snapshot: NamespaceContextSnapshot::default(),
            id: None,
            annotation: None,
            source: None,
        }
    }

    fn make_identity_data(
        kind: IdentityKind,
        name: NameId,
        selector_xpath: &str,
        field_xpaths: &[&str],
    ) -> IdentityConstraintData {
        IdentityConstraintData {
            kind,
            name,
            ref_name: None,
            refer: None,
            selector: make_selector_result(selector_xpath),
            fields: field_xpaths
                .iter()
                .map(|x| make_field_result(x))
                .collect(),
            id: None,
            annotation: None,
            source: None,
        }
    }

    #[test]
    fn compile_simple_constraint() {
        let nt = make_name_table();
        let name = nt.add("testKey");
        let key = IdentityConstraintKey::default();

        let data = make_identity_data(IdentityKind::Key, name, "./item", &["@id"]);

        let compiled = CompiledIdentityConstraint::compile(
            &data,
            key,
            &nt,
            None,
            None,
            XsdVersion::V1_0,
        );
        assert!(compiled.is_ok());
        let compiled = compiled.unwrap();
        assert_eq!(compiled.field_count, 1);
        assert_eq!(compiled.kind, IdentityKind::Key);
        assert_eq!(compiled.name, name);
    }

    #[test]
    fn compile_invalid_xpath_propagates_error() {
        let nt = make_name_table();
        let name = nt.add("badKey");
        let key = IdentityConstraintKey::default();

        let data = make_identity_data(IdentityKind::Key, name, "///invalid", &["@id"]);

        let result = CompiledIdentityConstraint::compile(
            &data,
            key,
            &nt,
            None,
            None,
            XsdVersion::V1_0,
        );
        assert!(result.is_err());
    }

    // -----------------------------------------------------------------------
    // ConstraintStruct lifecycle
    // -----------------------------------------------------------------------

    #[test]
    fn constraint_struct_lifecycle() {
        let nt = make_name_table();
        let name = nt.add("testKey");
        let key = IdentityConstraintKey::default();

        let data = make_identity_data(IdentityKind::Key, name, "./item", &["@id"]);
        let compiled = CompiledIdentityConstraint::compile(
            &data,
            key,
            &nt,
            None,
            None,
            XsdVersion::V1_0,
        )
        .unwrap();

        let mut cs = ConstraintStruct::new(&compiled);

        // Activate at scope element
        let scope_match = cs.activate();
        assert!(!scope_match); // ./item is not a bare "."
        assert!(cs.is_active());

        // Start element: "item" (the selector target)
        let item_name = nt.add("item");
        let empty_ns = NameId(0);
        cs.start_element(item_name, empty_ns);

        // The selector should have matched "item"
        assert!(cs.collecting_fields());

        // Check attribute "@id"
        let id_name = nt.add("id");
        let matches = cs.matching_fields(id_name, empty_ns);
        assert_eq!(matches, vec![0]);

        // Set field value
        cs.set_field_value(0, "val1".to_string(), None);

        // End element — should finalize key sequence
        let errors = cs.end_element_with_text("", None, &nt, "/root/item[1]", None);
        assert!(errors.is_empty());

        // Key table should have one sequence
        assert_eq!(cs.key_table.sequences.len(), 1);
        assert!(cs.key_table.sequences[0].is_complete());
        assert_eq!(
            cs.key_table.sequences[0].fields[0]
                .as_ref()
                .unwrap()
                .string_value,
            "val1"
        );
    }

    /// Nested selector matches: `.//item` with `<item><item/></item>`.
    /// Both outer and inner matches should produce independent key sequences.
    #[test]
    fn constraint_struct_nested_selector() {
        let nt = make_name_table();
        let name = nt.add("uq");
        let key = IdentityConstraintKey::default();

        let data = make_identity_data(IdentityKind::Unique, name, ".//item", &["@id"]);
        let compiled = CompiledIdentityConstraint::compile(
            &data,
            key,
            &nt,
            None,
            None,
            XsdVersion::V1_0,
        )
        .unwrap();

        let mut cs = ConstraintStruct::new(&compiled);
        cs.activate();

        let item = nt.add("item");
        let id = nt.add("id");
        let ns = NameId(0);

        // Outer <item>
        cs.start_element(item, ns);
        assert!(cs.collecting_fields());
        let m = cs.matching_fields(id, ns);
        assert_eq!(m, vec![0]);
        cs.set_field_value(0, "outer".to_string(), None);

        // Inner <item> (nested match — pushes second frame)
        cs.start_element(item, ns);
        // Now two frames on the stack
        let m = cs.matching_fields(id, ns);
        assert_eq!(m, vec![0]); // top frame's field
        cs.set_field_value(0, "inner".to_string(), None);

        // End inner </item> — finalizes inner sequence
        let errors = cs.end_element_with_text("", None, &nt, "/root/item/item", None);
        assert!(errors.is_empty());
        assert_eq!(cs.key_table.sequences.len(), 1);
        assert_eq!(
            cs.key_table.sequences[0].fields[0]
                .as_ref()
                .unwrap()
                .string_value,
            "inner"
        );

        // Outer frame is still active
        assert!(cs.collecting_fields());

        // End outer </item> — finalizes outer sequence
        let errors = cs.end_element_with_text("", None, &nt, "/root/item", None);
        assert!(errors.is_empty());
        assert_eq!(cs.key_table.sequences.len(), 2);
        assert_eq!(
            cs.key_table.sequences[1].fields[0]
                .as_ref()
                .unwrap()
                .string_value,
            "outer"
        );

        // No more active frames
        assert!(!cs.collecting_fields());
    }

    /// Multiple fields matching the same attribute should all be populated.
    #[test]
    fn constraint_struct_multi_field_same_attr() {
        let nt = make_name_table();
        let name = nt.add("uq2");
        let key = IdentityConstraintKey::default();

        // Two fields both matching @id
        let data = make_identity_data(IdentityKind::Unique, name, "./item", &["@id", "@id"]);
        let compiled = CompiledIdentityConstraint::compile(
            &data,
            key,
            &nt,
            None,
            None,
            XsdVersion::V1_0,
        )
        .unwrap();

        let mut cs = ConstraintStruct::new(&compiled);
        cs.activate();

        let item = nt.add("item");
        let id = nt.add("id");
        let ns = NameId(0);

        cs.start_element(item, ns);
        let matches = cs.matching_fields(id, ns);
        assert_eq!(matches, vec![0, 1]); // both fields match

        cs.set_field_value(0, "v".to_string(), None);
        cs.set_field_value(1, "v".to_string(), None);

        let errors = cs.end_element_with_text("", None, &nt, "/root/item", None);
        assert!(errors.is_empty());
        assert!(cs.key_table.sequences[0].is_complete());
    }
}
