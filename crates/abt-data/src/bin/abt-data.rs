//! `abt-data`: the data layer's program (docs/spec/00-architecture.md,
//! Definition 2.1). It links the data layer and the shared crates and no
//! other layer; `abt data ...` of the front runs the same commands.
//!
//! `abt-data --version` (`-V`) prints `abt-data VERSION (BUILD)` and nothing
//! else: the crate's version and the build's identity, the commit the binary
//! was built from (`scripts/build_id.rs`). No command reads the identity
//! (ERRATA E-133).

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if matches!(args.first().map(String::as_str), Some("--version" | "-V")) {
        println!("abt-data {} ({})", env!("CARGO_PKG_VERSION"), env!("ABT_BUILD"));
        return;
    }
    abt_data::cli::main(&args)
}
