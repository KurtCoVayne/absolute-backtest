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
    abt_kernel::cli::main()
}
