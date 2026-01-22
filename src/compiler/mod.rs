//! NFA compilation for XSD content models
//!
//! This module compiles XSD content model particles (sequences, choices, all-groups)
//! into NFAs that can be used for efficient content validation.
//!
//! # Architecture
//!
//! The compiler uses Thompson's construction with composable fragments:
//! - Each element or wildcard becomes a single-state fragment
//! - Sequences concatenate fragments with epsilon transitions
//! - Choices add new start/end states with epsilon branches
//! - Repetition adds epsilon loops based on occurrence constraints
//!
//! # Example
//!
//! ```
//! use xsd_schema::compiler::{CompileContext, FragmentBuilder, NfaTerm, fragment_to_table};
//! use xsd_schema::{SchemaSet, NameId};
//!
//! // Build a simple NFA for a single element
//! let mut builder = FragmentBuilder::new();
//! let term = NfaTerm::element(NameId(1), None, None);
//! let fragment = builder.single_term(term, None);
//! let nfa = fragment_to_table(fragment);
//!
//! assert_eq!(nfa.state_count(), 2); // term state + exit state
//! ```

mod nfa;
mod fragment;
mod compile;
mod error;
mod particle;
mod all_group;
mod upa;
mod substitution;
mod open_content;

pub use nfa::{
    advance_states,
    advance_with_priority,
    epsilon_closure,
    term_matches as nfa_term_matches,
    NfaTable,
    NfaState,
    NfaTerm,
    NfaTransition,
    TransitionKind,
    StateId,
};
pub use fragment::{NfaFragment, FragmentBuilder, fragment_to_table};
pub use compile::{
    CompileContext, compile_content_model_matcher, compile_model_group, compile_particle,
};
pub use error::{NfaCompileError, NfaCompileResult};
pub use particle::{MaxOccurs, CountedParticle, apply_occurs, MAX_OCCURS_LIMIT};
pub use all_group::{
    AllGroupModel, AllParticle, AllGroupState, OpenContentWildcard, OpenContentMode,
    validate_all_group_constraints, term_matches, term_matches_with_substitution, TermMatchResult,
};
pub use upa::check_upa;
pub use substitution::{build_substitution_group_map, SubstitutionGroupMap};
pub use open_content::{
    ContentModelMatcher, OpenContent, OpenContentMode as CompilerOpenContentMode, validate_interleave,
    validate_suffix,
};
