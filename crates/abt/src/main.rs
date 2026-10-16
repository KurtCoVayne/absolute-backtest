//! `abt`, the front (docs/spec/00-architecture.md, Definition 2.2): every
//! subcommand is a call on one layer; this binary parses nothing itself and
//! dispatches. Until the layers serve their own commands (M-2 to M-5), the
//! kernel's command line serves all of them, its data through the service
//! `abt::install` registers.

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
