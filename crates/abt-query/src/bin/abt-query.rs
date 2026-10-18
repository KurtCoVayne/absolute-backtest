//! `abt-query`: the query layer's program (docs/spec/00-architecture.md,
//! Definition 2.1). It links the query layer and the shared crates and no
//! other layer: it reaches its data at `--data SOCKET` and its kernels at
//! `--kernel SOCKET`; `abt plan|metrics|...` of the front runs the same
//! commands, also over a store directory and kernels in its own process.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(abt_query::dispatch::main(&args))
}
