//! The element declaration that governs a child element, and its declared
//! type, as one rule over the resolved [`SchemaSet`].
//!
//! The validation runtime picks a child's declaration and type through these
//! two steps, and the validation plan builds its per-child tables through the
//! same two, so both pick child types by one rule.

use crate::ids::{ElementKey, NameId, TypeKey};
use crate::schema::SchemaSet;
use crate::validation::content::ElementMatchInfo;

impl SchemaSet {
    /// Term → declaration: the governing element declaration of a child
    /// element named (`namespace`, `local_name`), given the content-model
    /// term that matched it, if any.
    ///
    /// The term's own declaration first; then the global element table
    /// (§3.10.4 cvc-wildcard), except when `skip` (a skip wildcard suppresses
    /// the global lookup) and when the content model resolved a local
    /// element's type, since a global element of the same name may have a
    /// different type.
    pub(crate) fn governing_element_declaration(
        &self,
        local_name: NameId,
        namespace: Option<NameId>,
        term: Option<ElementMatchInfo>,
        skip: bool,
    ) -> Option<ElementKey> {
        let matched_elem_key = term.and_then(|i| i.element_key);
        let matched_type = term.and_then(|i| i.resolved_type);
        if matched_type.is_some() || skip {
            matched_elem_key
        } else {
            matched_elem_key.or_else(|| self.lookup_element(namespace, local_name))
        }
    }

    /// Declaration → type: the declared {type definition} of the element
    /// declaration `elem_key`, before any `xsi:type` or (XSD 1.1) type
    /// alternative replaces it. `None` when the declaration has no resolved
    /// type.
    pub(crate) fn declared_element_type(&self, elem_key: ElementKey) -> Option<TypeKey> {
        self.arenas.elements[elem_key].resolved_type
    }
}
