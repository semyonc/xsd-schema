//! Build script for xsd-schema crate.
//!
//! This script configures LALRPOP to generate the XPath parser
//! when the `xsd11` feature is enabled.

fn main() {
    // With a rerun directive Cargo runs this script again only when a named
    // file changes (this one; LALRPOP names each grammar it processes), not
    // after every change to a file of the package.
    println!("cargo:rerun-if-changed=build.rs");

    // Grammars are searched under `src` only. The crate root also holds
    // `target`, where rustc creates and removes files while this script runs:
    // a search from the root (`lalrpop::process_root`) reads all of it and
    // fails when one of those files disappears under it. The generated parser
    // stays at `$OUT_DIR/xpath/parser.rs`, where `src/xpath/parser.rs`
    // includes it from.
    #[cfg(feature = "xsd11")]
    lalrpop::Configuration::new()
        .set_in_dir("src")
        .emit_rerun_directives(true)
        .process()
        .expect("Failed to process LALRPOP grammar");
}
