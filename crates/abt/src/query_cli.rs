//! The query layer's commands as the front runs them: the layer's own
//! dispatch (`abt_query::dispatch`) with what one process adds: a store
//! directory served in process (`--store DIR`, else `$ABT_STORE`, else
//! `./abt-store`) and `--kernels N` kernel services in process (one by
//! default) reaching the same data; `abt briefs` brings its own market.

use std::path::PathBuf;
use std::sync::Arc;

use abt_data::service::DataService;
use abt_data::Store;
use abt_kernel::kernel::service::KernelService;
use abt_kernel::kernel::DataClient;
use abt_query::dispatch::{self, Connector, Links};
use abt_wire::transport::Connection;

/// The data a command's `--data SOCKET` or `--store DIR` names.
fn reach(args: &[String]) -> Result<Connector, String> {
    if dispatch::opt(args, "--data").is_some() {
        return dispatch::socket_data(args);
    }
    let dir = dispatch::opt(args, "--store")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("ABT_STORE").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("abt-store"));
    if !dir.exists() {
        return Err(format!("--store {}: no store there (name one with --store DIR, or a data service with --data SOCKET)", dir.display()));
    }
    let store = Store::open(&dir).map_err(|e| format!("--store {}: {}", dir.display(), e))?;
    Ok(here(DataService::new(store)))
}

/// A connector to a data service of this process.
fn here(svc: Arc<DataService>) -> Connector {
    Arc::new(move || crate::connect_data(&svc))
}

/// `--kernels N`, one by default.
fn kernel_count(args: &[String]) -> Result<usize, String> {
    match dispatch::opt(args, "--kernels") {
        Some(n) => n.parse::<usize>().map(|n| n.max(1)).map_err(|_| format!("--kernels: `{}` is not a count", n)),
        None => Ok(1),
    }
}

/// `--kernels N` kernel services in this process reaching `data`, each
/// reached by a new in-process connection whenever asked.
#[allow(clippy::result_large_err)]
fn kernels_here(args: &[String], data: &Connector) -> Result<Vec<Connector>, String> {
    let n = kernel_count(args)?;
    Ok((0..n)
        .map(|_| {
            let data = data.clone();
            let connect: Arc<abt_kernel::kernel::service::Connect> = Arc::new(move || data().map_err(abt_kernel::kernel::RunError::Config).and_then(DataClient::connect));
            let svc = KernelService::new(connect);
            let k: Connector = Arc::new(move || crate::connect_kernel(&svc));
            k
        })
        .collect())
}

fn links() -> Links {
    Links {
        data: Box::new(reach),
        kernels_here: Some(Box::new(kernels_here)),
    }
}

/// `abt query serve [--socket PATH] (--data SOCKET | --store DIR) [--kernel
/// SOCKET... | --kernels N]`: the query service (P-3.5); the exit code.
pub fn serve(args: &[String]) -> i32 {
    dispatch::serve(&links(), args)
}

/// `abt plan`; the exit code.
pub fn plan(args: &[String]) -> i32 {
    dispatch::plan(&links(), args)
}

/// `abt lineage` and `abt dispute`; the exit code.
pub fn lineage(cmd: &str, args: &[String]) -> i32 {
    dispatch::lineage(&links(), cmd, args)
}

/// `abt metrics`; the exit code.
pub fn metrics(args: &[String]) -> i32 {
    dispatch::metrics(&links(), args)
}

/// `abt test`, `abt claim`, `abt feed` and `abt activation`; the exit code.
pub fn query(cmd: &str, args: &[String]) -> i32 {
    dispatch::query(&links(), cmd, args)
}

/// `abt briefs`; the exit code. The attempts run on the briefs market, in a
/// store of this process, through `--kernels N` kernel services here (one
/// by default).
pub fn briefs(args: &[String]) -> i32 {
    dispatch::briefs(&links(), args, &|_| crate::briefs_market().map(here))
}

/// A connection to a service of this process, for the tests of the front.
pub fn connection_to(svc: Arc<DataService>) -> Result<Connection, String> {
    crate::connect_data(&svc)
}
