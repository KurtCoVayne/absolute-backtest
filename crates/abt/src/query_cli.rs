//! The query layer's commands, as the front serves them (Q-9): it reaches
//! the data service a command reads through, over `--data SOCKET`, or over
//! a store directory served in this process (`--store DIR`, else
//! `$ABT_STORE`, else `./abt-store`); and the kernels a plan's runs are
//! spread over, at `--kernel SOCKET` (repeatable), or `--kernels N` kernel
//! services in this process (one by default) reaching the same data.

use std::path::PathBuf;
use std::sync::Arc;

use abt_data::service::{inproc, DataService};
use abt_data::Store;
use abt_kernel::kernel::service::KernelService;
use abt_kernel::kernel::DataClient;
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
            Reach::Here(svc) => Ok(inproc(svc)),
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

/// The kernels: every `--kernel SOCKET`, else `--kernels N` services here
/// reaching the command's data.
#[allow(clippy::result_large_err)]
fn kernels(args: &[String], data: &Reach) -> Result<Vec<Connection>, String> {
    let sockets: Vec<String> = args.windows(2).filter(|w| w[0] == "--kernel").map(|w| w[1].clone()).collect();
    if !sockets.is_empty() {
        return sockets
            .iter()
            .map(|s| Connection::connect(std::path::Path::new(s)).map_err(|e| format!("--kernel {}: {}", s, e)))
            .collect();
    }
    let n: usize = match opt(args, "--kernels") {
        Some(n) => n.parse().map_err(|_| format!("--kernels: `{}` is not a count", n))?,
        None => 1,
    };
    (0..n.max(1))
        .map(|_| {
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
                    Arc::new(move || DataClient::connect(inproc(&svc)))
                }
            };
            Ok(abt_kernel::kernel::service::inproc(&KernelService::new(connect)))
        })
        .collect()
}

/// `abt plan`; the exit code.
pub fn plan(args: &[String]) -> i32 {
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
    match connection(args) {
        Ok(conn) => abt_query::cli::query_main(cmd, args, conn),
        Err(e) => {
            eprintln!("{}", e);
            2
        }
    }
}
