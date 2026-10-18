//! The query layer's commands as a program serves them (Q-9): `abt-query`
//! is the layer's binary and `abt` the front's door to the same functions.
//! A command reaches the data service it reads through at `--data SOCKET`
//! and the kernels a plan's runs are spread over at `--kernel SOCKET`
//! (repeatable); `abt-query serve` serves the commands over a socket
//! (P-3.5), reaching its data and kernels the same way; with `--query
//! SOCKET` a command is asked of such a service rather than run here, the
//! files it names read here and sent with it.
//!
//! The front adds what one process can do: a store directory served in
//! process (`--store DIR`) and `--kernels N` kernel services in process.
//! Those are the `Links` it passes; the binary's links reach sockets only
//! (Definition A-2.1: a layer's binary links no other layer).

use std::path::PathBuf;
use std::sync::Arc;

use abt_wire::generated::abt::wire::{QueryLayerOp, QueryLayerRequest};
use abt_wire::transport::Connection;

use crate::service::{ConnectData, ConnectKernels, QueryService};

/// A way to a data service: connects anew each time.
pub type Connector = Arc<ConnectData>;

/// What a command's words name as its data.
pub type DataLink = Box<dyn Fn(&[String]) -> Result<Connector, String>>;

/// Kernel services in a process reaching a command's data, for `--kernels N`.
pub type KernelsLink = Box<dyn Fn(&[String], &Connector) -> Result<Vec<Connector>, String>>;

/// How a command's words name what it reaches.
pub struct Links {
    /// The data a command's words name (`--data SOCKET`; the front also
    /// serves `--store DIR` in process).
    pub data: DataLink,
    /// Kernel services in this process reaching `data`, for `--kernels N`
    /// (the front's); `None` refuses `--kernels`.
    pub kernels_here: Option<KernelsLink>,
}

pub fn opt(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

/// A connector to the socket `--data SOCKET` names; the binary's `data`.
pub fn socket_data(args: &[String]) -> Result<Connector, String> {
    let socket = opt(args, "--data").ok_or_else(|| "--data SOCKET names the data service (the front's `abt` also takes --store DIR)".to_string())?;
    Ok(connector_to(PathBuf::from(socket), "--data"))
}

fn connector_to(path: PathBuf, what: &'static str) -> Connector {
    Arc::new(move || Connection::connect(&path).map_err(|e| format!("{} {}: {}", what, path.display(), e)))
}

/// The links of the layer's own binary: sockets only.
pub fn socket_links() -> Links {
    Links {
        data: Box::new(socket_data),
        kernels_here: None,
    }
}

/// Every `--kernel SOCKET`.
fn kernel_sockets(args: &[String]) -> Vec<String> {
    args.windows(2).filter(|w| w[0] == "--kernel").map(|w| w[1].clone()).collect()
}

/// The kernels a command's words name: every `--kernel SOCKET`, else the
/// links' kernels in process (`--kernels N`).
fn kernel_connectors(links: &Links, args: &[String], data: &Connector) -> Result<Vec<Connector>, String> {
    let sockets = kernel_sockets(args);
    if !sockets.is_empty() {
        return Ok(sockets.into_iter().map(|s| connector_to(PathBuf::from(s), "--kernel")).collect());
    }
    match &links.kernels_here {
        Some(here) => here(args, data),
        None => Err("--kernel SOCKET names a kernel service (the front's `abt` also runs --kernels N in process)".to_string()),
    }
}

fn connect_all(ks: &[Connector]) -> Result<Vec<Connection>, String> {
    ks.iter().map(|k| k()).collect()
}

/// `serve [--socket PATH] --data SOCKET --kernel SOCKET...`: the query
/// service (P-3.5) on a socket until the process ends; the exit code.
pub fn serve(links: &Links, args: &[String]) -> i32 {
    let run = || -> Result<i32, String> {
        let path = opt(args, "--socket").map(PathBuf::from).unwrap_or_else(|| abt_wire::transport::default_socket("query"));
        let data = (links.data)(args)?;
        let ks = kernel_connectors(links, args, &data)?;
        let kernels: Arc<ConnectKernels> = Arc::new(move || connect_all(&ks));
        let service = QueryService::new(data, kernels);
        let h = crate::service::listen(&path, service).map_err(|e| format!("{}: {}", path.display(), e))?;
        eprintln!("serving the query layer on {}", path.display());
        let _ = h.join();
        Ok(0)
    };
    run().unwrap_or_else(|e| {
        eprintln!("{}", e);
        2
    })
}

/// A command's words less the transport options, which name what the
/// client reaches, not the service.
fn served_args(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if matches!(args[i].as_str(), "--query" | "--data" | "--store" | "--kernel" | "--kernels") {
            i += 2;
            continue;
        }
        out.push(args[i].clone());
        i += 1;
    }
    out
}

/// A command asked of the query service at `--query SOCKET` (P-3.5), with
/// what it reads from files; `None` when no service is named. Its output is
/// printed as the command would print it; the exit code (1 when a file it
/// names cannot be read, as here; 2 when the service cannot be reached).
fn ask_service(op: QueryLayerOp, args: &[String]) -> Option<i32> {
    let socket = opt(args, "--query")?;
    let words = served_args(args);
    let mut r = QueryLayerRequest {
        op,
        args: None,
        plan: None,
        sources: None,
        strategy: None,
    };
    let read = match op {
        QueryLayerOp::Plan => crate::cli::plan_files(&words).map(|f| {
            if let Some((text, prog)) = f {
                r.plan = Some(text);
                r.sources = Some(prog.sources);
                r.strategy = Some(prog.strategy);
            }
        }),
        QueryLayerOp::Lineage | QueryLayerOp::Dispute => crate::cli::lineage_files(&words).map(|f| {
            if let Some(prog) = f {
                r.sources = Some(prog.sources);
                r.strategy = Some(prog.strategy);
            }
        }),
        _ => Ok(()),
    };
    if let Err(e) = read {
        eprintln!("{}", e);
        return Some(1);
    }
    r.args = Some(words);
    let asked = Connection::connect(std::path::Path::new(&socket))
        .map_err(|e| format!("--query {}: {}", socket, e))
        .and_then(|conn| crate::service::call(conn, r));
    Some(match asked {
        Ok(a) => {
            print!("{}", a.out);
            eprint!("{}", a.err);
            a.status
        }
        Err(e) => {
            eprintln!("{}", e);
            2
        }
    })
}

fn failing(e: String) -> i32 {
    eprintln!("{}", e);
    2
}

/// `plan`; the exit code.
pub fn plan(links: &Links, args: &[String]) -> i32 {
    if let Some(code) = ask_service(QueryLayerOp::Plan, args) {
        return code;
    }
    let run = || -> Result<i32, String> {
        let data = (links.data)(args)?;
        let ks = connect_all(&kernel_connectors(links, args, &data)?)?;
        Ok(crate::cli::plan_main(args, data()?, ks))
    };
    run().unwrap_or_else(failing)
}

/// `lineage` and `dispute`; the exit code.
pub fn lineage(links: &Links, cmd: &str, args: &[String]) -> i32 {
    let op = if cmd == "dispute" { QueryLayerOp::Dispute } else { QueryLayerOp::Lineage };
    if let Some(code) = ask_service(op, args) {
        return code;
    }
    let run = || -> Result<i32, String> {
        let data = (links.data)(args)?;
        let ks = connect_all(&kernel_connectors(links, args, &data)?)?;
        Ok(crate::cli::lineage_main(cmd, args, data()?, ks))
    };
    run().unwrap_or_else(failing)
}

/// `metrics`; the exit code.
pub fn metrics(links: &Links, args: &[String]) -> i32 {
    if let Some(code) = ask_service(QueryLayerOp::Metrics, args) {
        return code;
    }
    match (links.data)(args).and_then(|d| d()) {
        Ok(conn) => crate::cli::metrics_main(args, conn),
        Err(e) => failing(e),
    }
}

/// `test`, `claim`, `feed` and `activation`; the exit code.
pub fn query(links: &Links, cmd: &str, args: &[String]) -> i32 {
    let op = match cmd {
        "test" => QueryLayerOp::Test,
        "claim" => QueryLayerOp::Claim,
        "feed" => QueryLayerOp::Feed,
        _ => QueryLayerOp::Activation,
    };
    if let Some(code) = ask_service(op, args) {
        return code;
    }
    match (links.data)(args).and_then(|d| d()) {
        Ok(conn) => crate::cli::query_main(cmd, args, conn),
        Err(e) => failing(e),
    }
}

/// `briefs` over the data `market` reaches (the briefs market: the front
/// makes it in process, the binary reads it from the service `--data`
/// names) and the kernels the words name; the exit code.
pub fn briefs(links: &Links, args: &[String], market: &dyn Fn(&[String]) -> Result<Connector, String>) -> i32 {
    let run = || -> Result<i32, String> {
        let data = market(args)?;
        let ks = connect_all(&kernel_connectors(links, args, &data)?)?;
        Ok(crate::cli::briefs_main(args, data()?, ks))
    };
    run().unwrap_or_else(failing)
}

/// The layer's own command line (`abt-query CMD ...`), sockets only; the
/// exit code.
pub fn main(args: &[String]) -> i32 {
    let links = socket_links();
    match args.first().map(String::as_str) {
        Some("serve") => serve(&links, &args[1..]),
        Some("metrics") => metrics(&links, &args[1..]),
        Some("plan") => plan(&links, &args[1..]),
        Some(cmd @ ("lineage" | "dispute")) => lineage(&links, cmd, &args[1..]),
        Some(cmd @ ("test" | "claim" | "feed" | "activation")) => query(&links, cmd, &args[1..]),
        Some("briefs") => briefs(&links, &args[1..], &socket_data),
        _ => {
            eprintln!(
                "usage:\n  abt-query serve    [--socket PATH] --data SOCKET --kernel SOCKET...\n  abt-query plan     run|validate|... PLAN <files...> --data SOCKET --kernel SOCKET... | --query SOCKET\n  abt-query metrics  --run RUN --data SOCKET | --query SOCKET\n  abt-query lineage|dispute ... --data SOCKET --kernel SOCKET... | --query SOCKET\n  abt-query test|claim|feed|activation ... --data SOCKET | --query SOCKET\n  abt-query briefs   ... --data SOCKET --kernel SOCKET...\n\nThe front `abt` runs the same commands, also over a store directory (--store DIR) and kernels in its own process (--kernels N)."
            );
            2
        }
    }
}
