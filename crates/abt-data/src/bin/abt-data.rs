//! `abt-data`: the data layer's program (docs/spec/00-architecture.md,
//! Definition 2.1). It links the data layer and the shared crates and no
//! other layer; `abt data ...` of the front runs the same commands.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    abt_data::cli::main(&args)
}
