//! `abt`, the front (docs/spec/00-architecture.md, Definition 2.2): every
//! subcommand is a call on one layer, and this binary parses nothing itself
//! but dispatches. `abt data ...` goes to the data layer's commands; `abt
//! metrics|plan|lineage|dispute|test|claim|feed|activation|briefs` to the
//! query layer's; `abt kernel ...` and the program commands (`check`, `run`,
//! `query`, `show`, ...) to the kernel's. `abt::install` registers the
//! in-process data services the kernel reaches a dataset or a store
//! directory through.

fn main() {
    abt::install();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("data") {
        return abt::data_cli::main(&args[1..]);
    }
    if args.first().map(String::as_str) == Some("kernel") {
        return abt_kernel::kernel_cli::main(&args[1..]);
    }
    match args.first().map(String::as_str) {
        Some("metrics") => std::process::exit(abt::query_cli::metrics(&args[1..])),
        Some("plan") => std::process::exit(abt::query_cli::plan(&args[1..])),
        Some(cmd @ ("lineage" | "dispute")) => std::process::exit(abt::query_cli::lineage(cmd, &args[1..])),
        Some(cmd @ ("test" | "claim" | "feed" | "activation")) => std::process::exit(abt::query_cli::query(cmd, &args[1..])),
        Some("briefs") => std::process::exit(abt::query_cli::briefs(&args[1..])),
        // The query layer's service (P-3.5); `abt query` otherwise asks the
        // kernel's query language of a run.
        Some("query") if args.get(1).map(String::as_str) == Some("serve") => std::process::exit(abt::query_cli::serve(&args[2..])),
        // The compiler's plan of a program (`show plan`; the query layer's
        // plans are `abt plan`).
        Some("show") if args.get(1).map(String::as_str) == Some("plan") => {
            let mut rest = vec!["plan".to_string()];
            rest.extend(args[2..].iter().cloned());
            return abt_kernel::cli::main_with(rest);
        }
        _ => {}
    }
    abt_kernel::cli::main()
}
