//! Build script for xsd-schema crate.
//!
//! This script configures LALRPOP to generate the XPath parser.

fn main() {
    // Configure LALRPOP to generate the parser
    // The generated file will be placed in OUT_DIR
    lalrpop::process_root().expect("Failed to process LALRPOP grammar");
}
