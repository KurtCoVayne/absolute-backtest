//! Compiles `schema/abt.fbs` with planus into `$OUT_DIR/abt_generated.rs`
//! (P-1.1): the same translation and code generation `planus rust` runs.

use std::path::PathBuf;

fn main() {
    let schema = PathBuf::from("schema/abt.fbs");
    println!("cargo:rerun-if-changed={}", schema.display());
    let declarations = planus_translation::translate_files(&[&schema]).unwrap_or_else(|| panic!("{}: the schema does not translate (planus printed why)", schema.display()));
    let code = planus_codegen::generate_rust(&declarations, false).unwrap_or_else(|e| panic!("{}: code generation failed: {}", schema.display(), e));
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR is set by cargo")).join("abt_generated.rs");
    std::fs::write(&out, code).unwrap_or_else(|e| panic!("{}: {}", out.display(), e));
}
