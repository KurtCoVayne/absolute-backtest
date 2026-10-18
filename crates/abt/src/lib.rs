//! The front (docs/spec/00-architecture.md, Definition 2.2): the one crate
//! that links the three layers, so the one place they are wired together in
//! a process. `install` registers with the kernel the data service it reaches
//! a dataset held in memory through (ERRATA E-36): the dataset is ingested
//! into a temporary store by the data layer's adapter of keyed rows, and the
//! kernel talks to the service over an in-process channel, the same frames as
//! a socket carries.

use std::collections::BTreeMap;
use std::sync::Arc;

pub mod query_cli;

use abt_data::adapter::{hot_bars, ingest_empty, ingest_keyed_with, securities_of, terms_of};
use abt_data::service::DataService;
use abt_data::Store;
use abt_kernel::check::Program;
use abt_kernel::kernel::bind::{keyed_tables, schema_in};
use abt_kernel::kernel::service::KernelService;
use abt_kernel::kernel::{Connector, Dataset};
use abt_wire::rpc::{serve, Service};
use abt_wire::transport::{bind, Connection};

/// Whether the layers of this process reach each other over Unix sockets
/// rather than in-process channels: `ABT_TRANSPORT=socket`. The frames are
/// the same either way (P-2.1), so the suite runs both ways, and CI runs it
/// over sockets too (M-9, ERRATA E-81).
pub fn over_sockets() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("ABT_TRANSPORT").is_ok_and(|v| v == "socket"))
}

/// A connection to a service of this process over a socket listened on for
/// it alone: accepted once, its file removed once connected.
fn socket_to(service: Arc<dyn Service>) -> Result<Connection, String> {
    static COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let path = std::env::temp_dir().join("abt-sockets").join(format!("{}-{}.sock", std::process::id(), n));
    let listener = bind(&path).map_err(|e| format!("{}: {}", path.display(), e))?;
    std::thread::spawn(move || {
        if let Ok((stream, _)) = listener.accept() {
            if let Ok(conn) = Connection::socket(stream) {
                let _ = serve(conn, service);
            }
        }
    });
    let conn = Connection::connect(&path).map_err(|e| format!("{}: {}", path.display(), e));
    let _ = std::fs::remove_file(&path);
    conn
}

/// A connection to a data service of this process: an in-process channel,
/// or a socket under `ABT_TRANSPORT=socket`.
pub fn connect_data(service: &Arc<DataService>) -> Result<Connection, String> {
    if over_sockets() {
        socket_to(service.clone())
    } else {
        Ok(abt_data::service::inproc(service))
    }
}

/// A connection to a kernel service of this process, as `connect_data`.
pub fn connect_kernel(service: &Arc<KernelService>) -> Result<Connection, String> {
    if over_sockets() {
        socket_to(service.clone())
    } else {
        Ok(abt_kernel::kernel::service::inproc(service))
    }
}

/// The source a dataset held in memory is ingested under.
pub const SOURCE: &str = "dataset";

/// What a dataset's versions state of it (D-2.1): the calendar of its bars,
/// `reviewed = complete` unless it is unreviewed, the bundle version it was
/// loaded from; `extra` (a reviewer's statements) last.
pub fn dataset_attributes(ds: &Dataset, base: &str, extra: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut a = BTreeMap::new();
    a.insert("calendar".to_string(), base.trim_start_matches('@').to_string());
    if !ds.unreviewed {
        a.insert("reviewed".to_string(), "complete".to_string());
    }
    if let Some(v) = &ds.version {
        a.insert("version".to_string(), v.clone());
    }
    a.extend(extra.iter().map(|(k, v)| (k.clone(), v.clone())));
    a
}

/// Ingests a dataset of `class` into a store, its versions carrying
/// `attributes`. A dataset holds its class's relations, and the series
/// class's: one it has no tuples of is provided empty (a market without
/// delistings), as a relation of a dataset always was.
pub fn ingest_dataset(store: &mut Store, ds: &Dataset, class: &str, attributes: &BTreeMap<String, String>) -> Result<(), String> {
    let schemas = store.registry.schemas(None);
    let (sec, terms, relations) = keyed_tables(ds, &|name| schema_in(&schemas, class, name))?;
    let securities = securities_of(&sec).map_err(|e| e.to_string())?;
    let terms = terms_of(&terms).map_err(|e| e.to_string())?;
    ingest_keyed_with(store, SOURCE, &securities, &terms, &relations, SOURCE, attributes, &hot_bars(&relations)).map_err(|e| e.to_string())?;
    for s in schemas.iter().filter(|s| s.class == class || s.class == "series") {
        ingest_empty(store, &s.name, SOURCE, attributes).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// A data service holding a dataset of a program's class at its base, over
/// a temporary store removed when the service ends.
pub fn serve_dataset(ds: &Dataset, prog: &Program) -> Result<Arc<DataService>, String> {
    let mut store = Store::temporary("run").map_err(|e| e.to_string())?;
    let attributes = dataset_attributes(ds, &prog.base.to_string(), &BTreeMap::new());
    ingest_dataset(&mut store, ds, &prog.class, &attributes)?;
    Ok(DataService::new(store))
}

/// The provider the kernel reaches a dataset through: a service in this
/// process holding it, reached by a new in-process connection each time
/// one is asked for.
pub fn provider(ds: &Dataset, prog: &Program) -> Result<Connector, String> {
    let svc = serve_dataset(ds, prog)?;
    Ok(Arc::new(move || connect_data(&svc)))
}

/// The provider the kernel reaches a store directory through: a service in
/// this process over it, its runs' outputs written to it.
pub fn store_provider(dir: &std::path::Path) -> Result<Connector, String> {
    let store = Store::open(dir).map_err(|e| format!("{}: {}", dir.display(), e))?;
    let svc = DataService::new(store);
    Ok(Arc::new(move || connect_data(&svc)))
}

/// Registers with the kernel the providers of in-process data services
/// (over a dataset, over a store directory), and the query layer's metrics
/// as what `abt run` prints a run's returns with (E-66); idempotent.
pub fn install() {
    abt_kernel::kernel::install_provider(Box::new(provider));
    abt_kernel::kernel::install_store_provider(Box::new(store_provider));
    abt_kernel::cli::install_reporter(Box::new(|conn, run, calendar| abt_query::cli::run_lines(conn, run, calendar).map(|r| (r.lines, r.returns))));
}

/// The names of the market the briefs harness runs attempts on.
pub const BRIEFS_SYMBOLS: [&str; 5] = ["AAA", "BBB", "CCC", "DDD", "SPY"];

/// The market the briefs harness runs attempts on (E-65): the synthetic
/// daily equity market of its five names over 320 business days from
/// 2022-01-03 (seed 11), in a temporary store.
pub fn briefs_market() -> Result<Arc<DataService>, String> {
    let ds = abt_kernel::data::synthetic_daily(&BRIEFS_SYMBOLS, (2022, 1, 3), 320, 11);
    let mut store = Store::temporary("briefs").map_err(|e| e.to_string())?;
    ingest_dataset(&mut store, &ds, "equity", &dataset_attributes(&ds, "@1d", &BTreeMap::new()))?;
    Ok(DataService::new(store))
}

/// `abt data ingest --adapter bundle_v2 --from DIR ...` of a directory
/// without a manifest: one Parquet file per relation (typed or text
/// columns), the kernel's test loader `data::load_parquet_dir`, which the
/// front keeps because it links the kernel; the data layer's own program
/// reads bundles with manifests only (ERRATA E-83). `Some(exit code)` when
/// the words are that command; `None` otherwise (the data layer's).
pub fn loose_ingest(args: &[String]) -> Option<i32> {
    let opt = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    if args.first().map(String::as_str) != Some("ingest") || opt("--adapter").as_deref() != Some("bundle_v2") {
        return None;
    }
    let from = opt("--from")?;
    let dir = std::path::PathBuf::from(&from);
    if dir.join("manifest.json").exists() {
        return None;
    }
    let run = || -> Result<(), String> {
        let class = opt("--class").ok_or("--class names the class (equity, future, option)")?;
        let store = opt("--store")
            .map(std::path::PathBuf::from)
            .or_else(|| std::env::var_os("ABT_STORE").map(std::path::PathBuf::from))
            .unwrap_or_else(|| std::path::PathBuf::from("abt-store"));
        let mut files: Vec<String> = Vec::new();
        let mut i = 1;
        while i < args.len() {
            if args[i].starts_with("--") {
                i += 2;
                continue;
            }
            files.push(args[i].clone());
            i += 1;
        }
        let mut paths = Vec::new();
        fn collect(p: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            if p.is_dir() {
                if let Ok(rd) = std::fs::read_dir(p) {
                    for e in rd.flatten() {
                        collect(&e.path(), out);
                    }
                }
            } else if p.extension().is_some_and(|x| x == "dsl") {
                out.push(p.to_path_buf());
            }
        }
        for f in &files {
            collect(std::path::Path::new(f), &mut paths);
        }
        paths.sort();
        let mut ws = abt_kernel::check::Workspace::new();
        for p in paths {
            let src = std::fs::read_to_string(&p).map_err(|e| format!("{}: {}", p.display(), e))?;
            ws.add_source(&src).map_err(|e| format!("{}:{}: parse error: {}", p.display(), e.span, e.message))?;
        }
        let prog = abt_kernel::check::class_program_at(&ws, &class, None).map_err(|d| d.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n"))?;
        let (ds, notes) = abt_kernel::data::load_parquet_dir(&prog, &dir)?;
        for n in notes {
            eprintln!("note: {}", n);
        }
        let mut extra = BTreeMap::new();
        for kv in opt("--attributes").map(|s| s.split(',').map(str::to_string).collect::<Vec<_>>()).unwrap_or_default() {
            let (k, v) = kv.split_once('=').ok_or("--attributes takes KEY=VALUE[,KEY=VALUE...]")?;
            extra.insert(k.trim().to_string(), v.trim().to_string());
        }
        let attributes = dataset_attributes(&ds, &prog.base.to_string(), &extra);
        let mut store = Store::open(&store).map_err(|e| format!("{}: {}", store.display(), e))?;
        ingest_dataset(&mut store, &ds, &prog.class, &attributes)?;
        for (r, s, v) in store.registry.currents() {
            println!("{} {} {}", r, s, v);
        }
        Ok(())
    };
    Some(match run() {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{}", e);
            2
        }
    })
}
