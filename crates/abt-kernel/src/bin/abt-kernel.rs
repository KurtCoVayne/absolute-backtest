//! `abt-kernel`: the kernel's program (docs/spec/00-architecture.md,
//! Definition 2.1). It links the kernel and the shared crates and no other
//! layer: it reads its data through a data service (`--data SOCKET`) and
//! has no in-process store or synthetic market (those are the front's,
//! which links every layer). `abt-kernel serve|check|contract|show|run|
//! query|status|control` is the kernel service and its client commands
//! (`abt kernel ...` of the front); every other command is a program
//! command run here (`abt check|run|query|show|contract|ir|hash|...`).

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("kernel") => abt_kernel::kernel_cli::main(&args[1..]),
        Some("serve" | "status" | "control") => abt_kernel::kernel_cli::main(&args),
        Some("show") if args.get(1).map(String::as_str) == Some("plan") => {
            let mut rest = vec!["plan".to_string()];
            rest.extend(args[2..].iter().cloned());
            abt_kernel::cli::main_with(rest)
        }
        _ => abt_kernel::cli::main_with(args),
    }
}
