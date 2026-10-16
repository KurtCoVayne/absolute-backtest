//! The query layer's commands, as the front serves them (Q-9): it reaches
//! the data service a command reads through, over `--data SOCKET`, or over
//! a store directory served in this process (`--store DIR`, else
//! `$ABT_STORE`, else `./abt-store`).

use std::path::PathBuf;

use abt_data::service::{inproc, DataService};
use abt_data::Store;
use abt_wire::transport::Connection;

/// The connection a command's `--data SOCKET` or `--store DIR` names.
fn connection(args: &[String]) -> Result<Connection, String> {
    let opt = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    if let Some(socket) = opt("--data") {
        return Connection::connect(std::path::Path::new(&socket)).map_err(|e| format!("--data {}: {}", socket, e));
    }
    let dir = opt("--store")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("ABT_STORE").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("abt-store"));
    if !dir.exists() {
        return Err(format!("--store {}: no store there (name one with --store DIR, or a data service with --data SOCKET)", dir.display()));
    }
    let store = Store::open(&dir).map_err(|e| format!("--store {}: {}", dir.display(), e))?;
    Ok(inproc(&DataService::new(store)))
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
