//! XPath static context (schema-aware namespace and type hooks).
//!
//! This is a minimal, forward-compatible analogue of XPath2Context.

use crate::ids::NameId;
use crate::namespace::table::NameTable;
use crate::schema::SchemaSet;
use crate::types::value::TimezoneOffset;

#[derive(Debug, Clone)]
pub struct XPathContext<'a> {
    pub names: &'a NameTable,
    pub schema_set: Option<&'a SchemaSet>,
    pub default_element_ns: Option<NameId>,
    pub implicit_timezone: Option<TimezoneOffset>,
    pub base_uri: Option<String>,
}

impl<'a> XPathContext<'a> {
    pub fn new(names: &'a NameTable) -> Self {
        Self {
            names,
            schema_set: None,
            default_element_ns: None,
            implicit_timezone: None,
            base_uri: None,
        }
    }

    pub fn with_schema_set(mut self, schema_set: &'a SchemaSet) -> Self {
        self.schema_set = Some(schema_set);
        self
    }

    pub fn with_default_element_ns(mut self, ns: NameId) -> Self {
        self.default_element_ns = Some(ns);
        self
    }

    pub fn with_implicit_timezone(mut self, tz: TimezoneOffset) -> Self {
        self.implicit_timezone = Some(tz);
        self
    }

    pub fn with_base_uri(mut self, base_uri: impl Into<String>) -> Self {
        self.base_uri = Some(base_uri.into());
        self
    }

    pub fn resolve_name(&self, id: NameId) -> Option<&str> {
        self.names.try_resolve(id)
    }
}
