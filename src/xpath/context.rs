//! XPath static and dynamic context definitions.
//!
//! This module provides:
//! - `XPathContext` - Static context for expression binding and evaluation
//! - `DynamicContext` - Runtime context for XPath evaluation
//! - `VarStore` - Variable storage (Vec-based arena indexed by VarSlotId)
//! - `NameBinder` - Compile-time variable slot allocation

use crate::ids::NameId;
use crate::namespace::table::NameTable;
use crate::namespace::context::NamespaceContextSnapshot;
use crate::namespace::qname::QualifiedName;
use crate::schema::SchemaSet;
use crate::types::value::{TimezoneOffset, DateTimeValue};

use super::DomNavigator;
use super::XPathMode;
use super::functions::{BuiltinCatalog, BuiltinEvaluator, FunctionCatalog, FunctionEvaluator};
use super::iterator::XmlItem;

// ============================================================================
// XPathContext (static context for bind-time and eval-time)
// ============================================================================

/// XPath 2.0 static context for expression binding and evaluation.
///
/// The static context provides information needed during expression compilation
/// and evaluation:
/// - Namespace prefix resolution
/// - Function registry access
/// - Default namespaces
/// - Schema type information
#[derive(Debug, Clone)]
pub struct XPathContext<'a> {
    /// Name table for string interning
    pub names: &'a NameTable,
    /// Schema set for type information
    pub schema_set: Option<&'a SchemaSet>,
    /// Namespace bindings for prefix resolution
    pub namespaces: NamespaceContextSnapshot,
    /// Default namespace for unprefixed element names
    pub default_element_ns: Option<NameId>,
    /// Default namespace for unprefixed function names (fn: namespace)
    pub default_function_ns: Option<&'static str>,
    /// Implicit timezone
    pub implicit_timezone: Option<TimezoneOffset>,
    /// Base URI for relative URI resolution
    pub base_uri: Option<String>,
    /// XPath language mode (1.0 or 2.0)
    pub mode: XPathMode,
    /// Function catalog for bind-time lookup (None = use builtins).
    function_catalog: Option<&'a dyn FunctionCatalog>,
}

impl<'a> XPathContext<'a> {
    /// Create a new static context with the given name table.
    pub fn new(names: &'a NameTable) -> Self {
        Self {
            names,
            schema_set: None,
            namespaces: NamespaceContextSnapshot::default(),
            default_element_ns: None,
            default_function_ns: Some(super::functions::FN_NAMESPACE),
            implicit_timezone: None,
            base_uri: None,
            mode: XPathMode::XPath20,
            function_catalog: None,
        }
    }

    /// Set the schema set
    pub fn with_schema_set(mut self, schema_set: &'a SchemaSet) -> Self {
        self.schema_set = Some(schema_set);
        self
    }

    /// Set the namespace bindings
    pub fn with_namespaces(mut self, namespaces: NamespaceContextSnapshot) -> Self {
        self.namespaces = namespaces;
        self
    }

    /// Set the default element namespace
    pub fn with_default_element_ns(mut self, ns: NameId) -> Self {
        self.default_element_ns = Some(ns);
        self
    }

    /// Set the default function namespace
    pub fn with_default_function_ns(mut self, ns: &'static str) -> Self {
        self.default_function_ns = Some(ns);
        self
    }

    /// Set the implicit timezone
    pub fn with_implicit_timezone(mut self, tz: TimezoneOffset) -> Self {
        self.implicit_timezone = Some(tz);
        self
    }

    /// Set the base URI
    pub fn with_base_uri(mut self, base_uri: impl Into<String>) -> Self {
        self.base_uri = Some(base_uri.into());
        self
    }

    /// Set the XPath language mode.
    pub fn with_mode(mut self, mode: XPathMode) -> Self {
        self.mode = mode;
        self
    }

    /// Get the XPath language mode.
    pub fn mode(&self) -> XPathMode {
        self.mode
    }

    /// Set the function catalog for custom function support.
    pub fn with_function_catalog(mut self, catalog: &'a dyn FunctionCatalog) -> Self {
        self.function_catalog = Some(catalog);
        self
    }

    /// Get the function catalog, using built-in functions as default.
    ///
    /// Returns a reference to the configured catalog, or `BuiltinCatalog` if none set.
    pub fn function_catalog(&self) -> &dyn FunctionCatalog {
        static BUILTIN: BuiltinCatalog = BuiltinCatalog;
        self.function_catalog.unwrap_or(&BUILTIN)
    }

    /// Resolve a prefix to a namespace URI.
    ///
    /// Returns the namespace URI for the given prefix, or None if not found.
    pub fn resolve_prefix(&self, prefix: &str) -> Option<String> {
        if prefix.is_empty() {
            // Empty prefix: use default element namespace
            self.default_element_ns.and_then(|id| self.names.try_resolve(id))
        } else if let Some(prefix_id) = self.names.get(prefix) {
            self.namespaces.resolve_prefix(prefix_id)
                .and_then(|ns_id| self.names.try_resolve(ns_id))
        } else {
            None
        }
    }

    /// Resolve a prefix to a namespace URI using NameId.
    pub fn resolve_prefix_id(&self, prefix_id: NameId) -> Option<NameId> {
        self.namespaces.resolve_prefix(prefix_id)
    }

    /// Get the default function namespace.
    ///
    /// In XPath 1.0 mode, core functions live in no namespace (empty string).
    /// In XPath 2.0 mode, the default is the fn: namespace.
    pub fn default_function_namespace(&self) -> &str {
        match self.mode {
            XPathMode::XPath10 => "",
            XPathMode::XPath20 => self.default_function_ns.unwrap_or(super::functions::FN_NAMESPACE),
        }
    }

    /// Resolve a name from the name table.
    pub fn resolve_name(&self, id: NameId) -> Option<String> {
        self.names.try_resolve(id)
    }
}

// ============================================================================
// NameBinder (compile-time variable slot allocation)
// ============================================================================

/// Variable slot identifier for indexing into VarStore.
pub type VarSlotId = u32;

/// Reference to a variable slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VarRef {
    pub slot: VarSlotId,
}

/// Entry in the NameBinder stack.
#[derive(Debug, Clone)]
pub struct NameSlot {
    pub name: QualifiedName,
    pub slot: VarSlotId,
}

/// Compile-time variable binder for slot allocation.
///
/// Provides stack-based scoping with slot IDs into a data pool.
/// Used during expression binding to assign variable slots.
///
/// External variables (those declared before `mark_external_boundary()` is called)
/// are tracked separately and can be retrieved via `external_vars()`.
#[derive(Debug, Default)]
pub struct NameBinder {
    next_slot: VarSlotId,
    stack: Vec<NameSlot>,
    /// Count of external variables (pushed before mark_external_boundary)
    external_var_count: usize,
}

impl NameBinder {
    /// Create a new empty name binder.
    pub fn new() -> Self {
        Self {
            next_slot: 0,
            stack: Vec::new(),
            external_var_count: 0,
        }
    }

    /// Get the total number of slots allocated.
    ///
    /// Use this after bind() to determine VarStore size.
    pub fn len(&self) -> usize {
        self.next_slot as usize
    }

    /// Check if any slots have been allocated.
    pub fn is_empty(&self) -> bool {
        self.next_slot == 0
    }

    /// Mark the current stack position as the boundary between external variables
    /// and internally-bound variables.
    ///
    /// Call this after pushing all external variables (those provided by the API user)
    /// and before binding the expression (which may introduce for/let/quantified variables).
    pub fn mark_external_boundary(&mut self) {
        self.external_var_count = self.stack.len();
    }

    /// Iterate over external variables (those pushed before `mark_external_boundary()`).
    ///
    /// Returns an iterator of (name, slot) pairs for all external variables.
    pub fn external_vars(&self) -> impl Iterator<Item = (&QualifiedName, VarSlotId)> {
        self.stack
            .iter()
            .take(self.external_var_count)
            .map(|slot| (&slot.name, slot.slot))
    }

    /// Get the number of external variables.
    pub fn external_var_count(&self) -> usize {
        self.external_var_count
    }

    /// Push a new variable binding onto the stack.
    ///
    /// Allocates a new slot and returns a VarRef to it.
    pub fn push_var(&mut self, name: QualifiedName) -> VarRef {
        let slot = self.next_slot;
        self.next_slot += 1;
        self.stack.push(NameSlot { name, slot });
        VarRef { slot }
    }

    /// Pop the most recent variable binding.
    ///
    /// Used by for/some/every expressions after binding their body.
    pub fn pop_var(&mut self) {
        self.stack.pop();
    }

    /// Resolve a variable name to its slot.
    ///
    /// Searches from the top of the stack (most recent binding first).
    /// Returns XPST0008 if the variable is not bound.
    pub fn resolve(&self, name: &QualifiedName) -> Result<VarRef, super::error::XPathError> {
        // Walk stack from end to beginning (last-in, first-out scoping)
        for entry in self.stack.iter().rev() {
            if entry.name == *name {
                return Ok(VarRef { slot: entry.slot });
            }
        }
        // Format QName for error - we don't have access to NameTable here,
        // so we use the raw NameId values in the message
        Err(super::error::XPathError::XPST0008 {
            qname: format!("$var(local={})", name.local_name.0),
        })
    }

    /// Resolve a variable name to its slot, with NameTable for error messages.
    ///
    /// Same as `resolve()` but provides better error messages.
    pub fn resolve_with_names(
        &self,
        name: &QualifiedName,
        names: &NameTable,
    ) -> Result<VarRef, super::error::XPathError> {
        // Walk stack from end to beginning (last-in, first-out scoping)
        for entry in self.stack.iter().rev() {
            if entry.name == *name {
                return Ok(VarRef { slot: entry.slot });
            }
        }
        // Format QName for error using NameTable
        let local = names.try_resolve(name.local_name).unwrap_or_else(|| "<unknown>".to_string());
        let qname_str = if let Some(prefix_id) = name.prefix {
            let prefix = names.try_resolve(prefix_id).unwrap_or_else(|| "<unknown>".to_string());
            format!("{}:{}", prefix, local)
        } else {
            local.to_string()
        };
        Err(super::error::XPathError::XPST0008 { qname: qname_str })
    }
}

// ============================================================================
// VarStore (variable storage - Vec-based arena)
// ============================================================================

/// Variable storage for XPath evaluation.
///
/// Stores variable values indexed by VarSlotId.
/// Size is determined by NameBinder::len() after binding.
#[derive(Debug, Clone)]
pub struct VarStore<V> {
    /// Variable values indexed by slot ID
    values: Vec<Option<V>>,
}

impl<V> VarStore<V> {
    /// Create a new variable store with the given size.
    ///
    /// The size should be NameBinder::len() after binding.
    pub fn new(size: usize) -> Self {
        let mut values = Vec::with_capacity(size);
        values.resize_with(size, || None);
        Self { values }
    }

    /// Get a variable value by slot ID.
    pub fn get(&self, slot: VarSlotId) -> Option<&V> {
        self.values.get(slot as usize).and_then(|v| v.as_ref())
    }

    /// Set a variable value.
    pub fn set(&mut self, slot: VarSlotId, value: V) {
        if let Some(cell) = self.values.get_mut(slot as usize) {
            *cell = Some(value);
        }
    }

    /// Clear a variable slot.
    pub fn clear_slot(&mut self, slot: VarSlotId) {
        if let Some(cell) = self.values.get_mut(slot as usize) {
            *cell = None;
        }
    }

    /// Clear all variable values.
    pub fn clear(&mut self) {
        for cell in &mut self.values {
            *cell = None;
        }
    }

    /// Get the number of slots.
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Check if the store is empty.
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

impl<V> Default for VarStore<V> {
    fn default() -> Self {
        Self::new(0)
    }
}

// ============================================================================
// DynamicContext (for eval-time)
// ============================================================================

/// XPath 2.0 dynamic context for expression evaluation.
///
/// The dynamic context provides runtime information:
/// - Current context item (node or atomic value)
/// - Context position and size (for predicates)
/// - Variable bindings (indexed by VarSlotId)
/// - Current date/time (stable for duration of query)
/// - Implicit timezone
pub struct DynamicContext<'a, N: DomNavigator> {
    /// Reference to the static context
    pub static_context: &'a XPathContext<'a>,
    /// Current context item (if any)
    pub context_item: Option<XmlItem<N>>,
    /// Current context position (1-based)
    pub context_position: usize,
    /// Current context size
    pub context_size: usize,
    /// Current date/time (stable for entire query evaluation)
    pub current_datetime: Option<DateTimeValue>,
    /// Implicit timezone
    pub implicit_timezone: Option<TimezoneOffset>,
    /// Base URI for resolving relative URIs
    pub base_uri: Option<String>,
    /// Variable bindings (indexed by VarSlotId from NameBinder)
    pub variables: VarStore<super::functions::XPathValue<N>>,
    /// Function evaluator for eval-time dispatch (None = use builtins).
    function_evaluator: Option<&'a dyn FunctionEvaluator<N>>,
}

impl<'a, N: DomNavigator> DynamicContext<'a, N> {
    /// Create a new dynamic context with the given static context.
    ///
    /// The var_count should be NameBinder::len() after binding.
    pub fn new(static_context: &'a XPathContext<'a>, var_count: usize) -> Self {
        Self {
            static_context,
            context_item: None,
            context_position: 0,
            context_size: 0,
            current_datetime: None,
            implicit_timezone: static_context.implicit_timezone,
            base_uri: static_context.base_uri.clone(),
            variables: VarStore::new(var_count),
            function_evaluator: None,
        }
    }

    /// Set the context item.
    pub fn with_context_item(mut self, item: XmlItem<N>) -> Self {
        self.context_item = Some(item);
        self.context_position = 1;
        self.context_size = 1;
        self
    }

    /// Set the context node.
    pub fn with_context_node(self, node: N) -> Self {
        self.with_context_item(XmlItem::Node(node))
    }

    /// Set context position and size (for predicate evaluation).
    pub fn with_position(mut self, position: usize, size: usize) -> Self {
        self.context_position = position;
        self.context_size = size;
        self
    }

    /// Set the current date/time.
    pub fn with_current_datetime(mut self, dt: DateTimeValue) -> Self {
        self.current_datetime = Some(dt);
        self
    }

    /// Set the implicit timezone.
    pub fn with_implicit_timezone(mut self, tz: TimezoneOffset) -> Self {
        self.implicit_timezone = Some(tz);
        self
    }

    /// Get the context item, returning an error if undefined.
    pub fn require_context_item(&self) -> Result<&XmlItem<N>, super::error::XPathError> {
        self.context_item.as_ref().ok_or_else(|| super::error::XPathError::XPDY0002 {
            message: "Context item is undefined".to_string(),
        })
    }

    /// Get the context node, returning an error if undefined or not a node.
    ///
    /// Returns XPTY0020 if the context item is not a node (per XPath 2.0 spec for axis steps).
    pub fn require_context_node(&self) -> Result<&N, super::error::XPathError> {
        match self.context_item.as_ref() {
            Some(XmlItem::Node(node)) => Ok(node),
            Some(XmlItem::Atomic(_)) => Err(super::error::XPathError::XPTY0020),
            None => Err(super::error::XPathError::XPDY0002 {
                message: "Context item is undefined".to_string(),
            }),
        }
    }

    /// Get a variable value by slot ID.
    pub fn get_variable(&self, slot: VarSlotId) -> Option<&super::functions::XPathValue<N>> {
        self.variables.get(slot)
    }

    /// Set a variable value.
    pub fn set_variable(&mut self, slot: VarSlotId, value: super::functions::XPathValue<N>) {
        self.variables.set(slot, value);
    }

    /// Set the function evaluator for custom function support.
    pub fn with_function_evaluator(mut self, evaluator: &'a dyn FunctionEvaluator<N>) -> Self {
        self.function_evaluator = Some(evaluator);
        self
    }

    /// Get the function evaluator, using built-in functions as default.
    ///
    /// Returns a reference to the configured evaluator, or `BuiltinEvaluator` if none set.
    pub fn function_evaluator(&self) -> &dyn FunctionEvaluator<N> {
        static BUILTIN: BuiltinEvaluator = BuiltinEvaluator;
        self.function_evaluator.unwrap_or(&BUILTIN)
    }

    /// Check if a custom function evaluator is configured.
    pub fn has_custom_evaluator(&self) -> bool {
        self.function_evaluator.is_some()
    }

    /// Evaluate a function using the configured evaluator.
    ///
    /// This method exists to work around borrow checker issues with calling
    /// `self.function_evaluator().eval(handle, self, args)` where the evaluator
    /// borrow conflicts with the mutable self borrow.
    pub fn eval_function(
        &mut self,
        handle: super::functions::FunctionHandle,
        args: Vec<super::functions::XPathValue<N>>,
    ) -> Result<super::functions::XPathValue<N>, super::error::XPathError> {
        // Fast path: built-in handles with no custom evaluator go directly to BuiltinEvaluator
        if handle.is_builtin() && self.function_evaluator.is_none() {
            return BuiltinEvaluator.eval(handle, self, args);
        }

        // Route through the custom evaluator (e.g. XPath10Evaluator intercepts builtins).
        // For custom handles, we need to call through the configured evaluator.
        // Get the evaluator pointer before borrowing self mutably.
        match self.function_evaluator {
            Some(evaluator) => {
                // SAFETY: The evaluator reference has lifetime 'a which is valid
                // for the duration of this DynamicContext. We're converting to a
                // raw pointer and back to work around the borrow checker, but the
                // reference is valid for this call.
                let evaluator_ptr = evaluator as *const dyn FunctionEvaluator<N>;
                // Re-borrow as shared reference for the call
                let evaluator_ref = unsafe { &*evaluator_ptr };
                evaluator_ref.eval(handle, self, args)
            }
            None => {
                // Fallback for custom handles without a configured evaluator
                BuiltinEvaluator.eval(handle, self, args)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_var_store() {
        let mut store: VarStore<i32> = VarStore::new(3);

        assert!(store.get(0).is_none());
        store.set(0, 42);
        assert_eq!(store.get(0), Some(&42));

        store.set(1, 100);
        assert_eq!(store.get(1), Some(&100));

        store.clear_slot(0);
        assert!(store.get(0).is_none());

        store.clear();
        assert!(store.get(1).is_none());
    }

    #[test]
    fn test_xpath_context_default_function_ns() {
        let names = NameTable::new();
        let ctx = XPathContext::new(&names);
        assert_eq!(ctx.default_function_namespace(), super::super::functions::FN_NAMESPACE);
    }

    #[test]
    fn test_name_binder_push_pop() {
        let names = NameTable::new();
        let mut binder = NameBinder::new();
        assert!(binder.is_empty());
        assert_eq!(binder.len(), 0);

        let x_id = names.add("x");
        let y_id = names.add("y");

        let name1 = QualifiedName::local(x_id);
        let ref1 = binder.push_var(name1.clone());
        assert_eq!(ref1.slot, 0);
        assert_eq!(binder.len(), 1);

        let name2 = QualifiedName::local(y_id);
        let ref2 = binder.push_var(name2.clone());
        assert_eq!(ref2.slot, 1);
        assert_eq!(binder.len(), 2);

        // Resolve should find the variables
        let resolved1 = binder.resolve(&name1).unwrap();
        assert_eq!(resolved1.slot, 0);

        let resolved2 = binder.resolve(&name2).unwrap();
        assert_eq!(resolved2.slot, 1);

        // Pop y, x should still be resolvable
        binder.pop_var();
        let resolved1_again = binder.resolve(&name1).unwrap();
        assert_eq!(resolved1_again.slot, 0);

        // y should not be resolvable after pop
        let err = binder.resolve(&name2);
        assert!(err.is_err());
    }

    #[test]
    fn test_name_binder_shadowing() {
        let names = NameTable::new();
        let mut binder = NameBinder::new();

        let x_id = names.add("x");
        let name = QualifiedName::local(x_id);

        // Push x (slot 0)
        let ref1 = binder.push_var(name.clone());
        assert_eq!(ref1.slot, 0);

        // Push x again (slot 1, shadows slot 0)
        let ref2 = binder.push_var(name.clone());
        assert_eq!(ref2.slot, 1);

        // Resolve should find the shadowing slot
        let resolved = binder.resolve(&name).unwrap();
        assert_eq!(resolved.slot, 1);

        // Pop the shadow, should now resolve to original
        binder.pop_var();
        let resolved_after_pop = binder.resolve(&name).unwrap();
        assert_eq!(resolved_after_pop.slot, 0);
    }

    #[test]
    fn test_name_binder_unbound_error() {
        let names = NameTable::new();
        let binder = NameBinder::new();
        let undefined_id = names.add("undefined");
        let name = QualifiedName::local(undefined_id);
        let result = binder.resolve(&name);
        assert!(matches!(result, Err(super::super::error::XPathError::XPST0008 { .. })));
    }
}
