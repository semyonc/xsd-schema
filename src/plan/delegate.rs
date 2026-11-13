//! The harness's delegating value tier (feature `codegen`): with a
//! `SchemaSet` at hand, a value check the plan does not cover asks the
//! dynamic validator's own function (`validate_simple_type`) instead of
//! answering `Unknown`. Emitted and `'static` plans have no such tier.

use crate::ids::TypeKey;
use crate::schema::SchemaSet;
use crate::validation::validate_simple_type;

use super::interp::ValueTier;
use super::model::PlanData;

/// The delegating tier over the set a plan was built from.
pub struct Delegating<'a> {
    set: &'a SchemaSet,
    types: &'a [TypeKey],
}

impl<'a> Delegating<'a> {
    /// The tier for `plan`, built from `set`.
    pub fn new(set: &'a SchemaSet, plan: &'a PlanData) -> Self {
        Delegating {
            set,
            types: &plan.delegates,
        }
    }
}

impl ValueTier for Delegating<'_> {
    fn delegating(&self) -> bool {
        true
    }

    fn value(&mut self, delegate: u32, value: &str) -> bool {
        match self.types.get(delegate as usize) {
            Some(&tk) => validate_simple_type(value, tk, self.set).is_ok(),
            None => false,
        }
    }
}

/// The dynamic verdict the plan's `Valid` is defined against:
/// `drive_quick_xml_bytes` with `ValidationFlags::default()` and default
/// parse limits returns `Ok`, the sink receives no error, and the root is
/// valid.
pub fn dynamic_valid(validator: &crate::validation::SchemaValidator<'_>, input: &[u8]) -> bool {
    use crate::validation::{drive_quick_xml_bytes, CollectingValidationSink, SchemaValidity};
    let (mut errors, mut warnings) = (Vec::new(), Vec::new());
    let sink = CollectingValidationSink {
        errors: &mut errors,
        warnings: &mut warnings,
    };
    let mut runtime = validator.start_run(sink);
    let outcome = drive_quick_xml_bytes(input, &mut runtime, validator.schema_set);
    drop(runtime);
    matches!(outcome, Ok(o) if o.root_validity == Some(SchemaValidity::Valid)) && errors.is_empty()
}
