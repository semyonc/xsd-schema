//! The validation plan: static tables and pattern DFAs, run by a small
//! interpreter, that answer "definitely valid" or "don't know" for a document
//! without touching a `SchemaSet`.
//!
//! [`check`] answers [`Verdict::Valid`] only when the dynamic validator
//! (`drive_quick_xml` over the same bytes, with `ValidationFlags::default()`
//! and default parse limits, against the set the plan was built from)
//! returns `Ok`, reports no error and finds the root valid. Everything else
//! answers [`Verdict::Unknown`], which means "ask the dynamic validator", not
//! "invalid".
//!
//! Not a public API yet: hidden, and subject to change.

#![forbid(unsafe_code)]

mod interp;
pub mod model;

#[cfg(feature = "codegen")]
pub mod alphabet;
#[cfg(feature = "codegen")]
pub mod builder;
#[cfg(feature = "codegen")]
pub mod delegate;
#[cfg(feature = "codegen")]
pub mod pattern;

#[cfg(all(test, feature = "codegen", feature = "xsd11"))]
mod t8;
#[cfg(all(test, feature = "codegen"))]
mod tests;

pub use interp::{
    check, check_with, in_alphabet, table_accepts, Scratch, Shipped, UnknownReason, ValueTier,
    Verdict,
};
pub use model::{PlanData, PlanRef};

#[cfg(feature = "codegen")]
pub use builder::{build, BuildReport};
#[cfg(feature = "codegen")]
pub use delegate::{dynamic_valid, Delegating};
