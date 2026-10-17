//! The query layer's commands, as the front serves them (Q-9): it reaches
//! the data service a command reads through, over `--data SOCKET`, or over
//! a store directory served in this process (`--store DIR`, else
//! `$ABT_STORE`, else `./abt-store`); and the kernels a plan's runs are
//! spread over, at `--kernel SOCKET` (repeatable), or `--kernels N` kernel
//! services in this process (one by default) reaching the same data. `abt
//! briefs` brings its own market.
//!
//! `abt query serve` serves them over a socket (P-3.5), reaching its data
//! and kernels the same way; with `--query SOCKET` a command is asked of
//! such a service rather than run here, the files it names read here and
//! sent with it.

use std::path::PathBuf;
use std::sync::Arc;

use abt_data::service::DataService;
use abt_data::Store;
use abt_kernel::kernel::service::KernelService;
use abt_kernel::kernel::DataClient;
use abt_wire::generated::abt::wire::{QueryLayerOp, QueryLayerRequest};
use abt_wire::transport::Connection;

fn opt(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

/// Where a command's data is: a socket, or a store served here.
enum Reach {
    Socket(PathBuf),
    Here(Arc<DataService>),
}

impl Reach {
    fn connect(&self) -> Result<Connection, String> {
        match self {
            Reach::Socket(p) => Connection::connect(p).map_err(|e| format!("--data {}: {}", p.display(), e)),
            Reach::Here(svc) => crate::connect_data(svc),
        }
    }
}

/// The data a command's `--data SOCKET` or `--store DIR` names.
fn reach(args: &[String]) -> Result<Reach, String> {
    if let Some(socket) = opt(args, "--data") {
        return Ok(Reach::Socket(PathBuf::from(socket)));
    }
    let dir = opt(args, "--store")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("ABT_STORE").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("abt-store"));
    if !dir.exists() {
        return Err(format!("--store {}: no store there (name one with --store DIR, or a data service with --data SOCKET)", dir.display()));
    }
    let store = Store::open(&dir).map_err(|e| format!("--store {}: {}", dir.display(), e))?;
    Ok(Reach::Here(DataService::new(store)))
}

/// The connection a command's `--data SOCKET` or `--store DIR` names.
fn connection(args: &[String]) -> Result<Connection, String> {
    reach(args)?.connect()
}

/// Every `--kernel SOCKET`.
fn kernel_sockets(args: &[String]) -> Vec<String> {
    args.windows(2).filter(|w| w[0] == "--kernel").map(|w| w[1].clone()).collect()
}

/// `--kernels N`, one by default.
fn kernel_count(args: &[String]) -> Result<usize, String> {
    match opt(args, "--kernels") {
        Some(n) => n.parse::<usize>().map(|n| n.max(1)).map_err(|_| format!("--kernels: `{}` is not a count", n)),
        None => Ok(1),
    }
}

/// A kernel service in this process reaching the data at `data`.
#[allow(clippy::result_large_err)]
fn kernel_here(data: &Reach) -> Arc<KernelService> {
    let connect: Arc<abt_kernel::kernel::service::Connect> = match data {
        Reach::Socket(p) => {
            let p = p.clone();
            Arc::new(move || {
                Connection::connect(&p)
                    .map_err(|e| abt_kernel::kernel::RunError::Request(format!("--data {}: {}", p.display(), e)))
                    .and_then(DataClient::connect)
            })
        }
        Reach::Here(svc) => {
            let svc = svc.clone();
            Arc::new(move || crate::connect_data(&svc).map_err(abt_kernel::kernel::RunError::Config).and_then(DataClient::connect))
        }
    };
    KernelService::new(connect)
}

/// Connections to every `--kernel SOCKET`.
fn connect_kernels(sockets: &[String]) -> Result<Vec<Connection>, String> {
    sockets
        .iter()
        .map(|s| Connection::connect(std::path::Path::new(s)).map_err(|e| format!("--kernel {}: {}", s, e)))
        .collect()
}

/// The kernels: every `--kernel SOCKET`, else `--kernels N` services here
/// reaching the command's data.
fn kernels(args: &[String], data: &Reach) -> Result<Vec<Connection>, String> {
    let sockets = kernel_sockets(args);
    if !sockets.is_empty() {
        return connect_kernels(&sockets);
    }
    (0..kernel_count(args)?).map(|_| crate::connect_kernel(&kernel_here(data))).collect()
}

/// `abt query serve [--socket PATH] (--data SOCKET | --store DIR) [--kernel
/// SOCKET... | --kernels N]`: the query service (P-3.5) on a socket until
/// the process ends; the exit code.
pub fn serve(args: &[String]) -> i32 {
    let run = || -> Result<i32, String> {
        let path = opt(args, "--socket").map(PathBuf::from).unwrap_or_else(|| abt_wire::transport::default_socket("query"));
        let data = Arc::new(reach(args)?);
        let sockets = kernel_sockets(args);
        let kernels: Arc<abt_query::service::ConnectKernels> = if sockets.is_empty() {
            // Kernel services here, made once; each request connects anew.
            let services: Vec<Arc<KernelService>> = (0..kernel_count(args)?).map(|_| kernel_here(&data)).collect();
            Arc::new(move || services.iter().map(crate::connect_kernel).collect())
        } else {
            Arc::new(move || connect_kernels(&sockets))
        };
        let reach = data.clone();
        let service = abt_query::service::QueryService::new(Arc::new(move || reach.connect()), kernels);
        let h = abt_query::service::listen(&path, service).map_err(|e| format!("{}: {}", path.display(), e))?;
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
        QueryLayerOp::Plan => abt_query::cli::plan_files(&words).map(|f| {
            if let Some((text, prog)) = f {
                r.plan = Some(text);
                r.sources = Some(prog.sources);
                r.strategy = Some(prog.strategy);
            }
        }),
        QueryLayerOp::Lineage | QueryLayerOp::Dispute => abt_query::cli::lineage_files(&words).map(|f| {
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
        .and_then(|conn| abt_query::service::call(conn, r));
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

/// `abt plan`; the exit code.
pub fn plan(args: &[String]) -> i32 {
    if let Some(code) = ask_service(QueryLayerOp::Plan, args) {
        return code;
    }
    let run = || -> Result<i32, String> {
        let data = reach(args)?;
        let ks = kernels(args, &data)?;
        Ok(abt_query::cli::plan_main(args, data.connect()?, ks))
    };
    run().unwrap_or_else(|e| {
        eprintln!("{}", e);
        2
    })
}

/// `abt lineage` and `abt dispute`; the exit code.
pub fn lineage(cmd: &str, args: &[String]) -> i32 {
    let op = if cmd == "dispute" { QueryLayerOp::Dispute } else { QueryLayerOp::Lineage };
    if let Some(code) = ask_service(op, args) {
        return code;
    }
    let run = || -> Result<i32, String> {
        let data = reach(args)?;
        let ks = kernels(args, &data)?;
        Ok(abt_query::cli::lineage_main(cmd, args, data.connect()?, ks))
    };
    run().unwrap_or_else(|e| {
        eprintln!("{}", e);
        2
    })
}

/// `abt metrics`; the exit code.
pub fn metrics(args: &[String]) -> i32 {
    if let Some(code) = ask_service(QueryLayerOp::Metrics, args) {
        return code;
    }
    match connection(args) {
        Ok(conn) => abt_query::cli::metrics_main(args, conn),
        Err(e) => {
            eprintln!("{}", e);
            2
        }
    }
}

/// `abt test`, `abt claim`, `abt feed` and `abt activation`; the exit code.
pub fn query(cmd: &str, args: &[String]) -> i32 {
    let op = match cmd {
        "test" => QueryLayerOp::Test,
        "claim" => QueryLayerOp::Claim,
        "feed" => QueryLayerOp::Feed,
        _ => QueryLayerOp::Activation,
    };
    if let Some(code) = ask_service(op, args) {
        return code;
    }
    match connection(args) {
        Ok(conn) => abt_query::cli::query_main(cmd, args, conn),
        Err(e) => {
            eprintln!("{}", e);
            2
        }
    }
}

/// `abt briefs`; the exit code. The attempts run on the briefs market, in a
/// store of this process, through `--kernels N` kernel services here (one
/// by default).
pub fn briefs(args: &[String]) -> i32 {
    let run = || -> Result<i32, String> {
        let data = Reach::Here(crate::briefs_market()?);
        let ks = kernels(args, &data)?;
        Ok(abt_query::cli::briefs_main(args, data.connect()?, ks))
    };
    run().unwrap_or_else(|e| {
        eprintln!("{}", e);
        2
    })
}
