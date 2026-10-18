//! The data layer's commands (D-10): `abt-data` is the layer's binary and
//! `abt data` the front's door to the same functions, over a store
//! directory (`--store DIR`, else `$ABT_STORE`, else `./abt-store`).
//!
//! ```text
//! abt-data serve   [--store DIR] [--socket PATH] [--config FILE] [--clock-rate R [--clock-origin T]] [--grace 'CAL=SECONDS;...']
//!                  [--record synthetic --session S --start T --end T [--symbols A,B] [--bar 1m] [--delay MS] [--seed N]]
//! abt-data ingest  [--store DIR] --adapter norgate --from LAKE [--lists 'NAME=LABEL;...'] [--series 'TICKER=NAME;...']
//!                  [--start DATE] [--end DATE] [--limit N] [--scheme S] [--exceptions FILE] [--source NAME] [--report FILE] [--attributes K=V,...]
//! abt-data ingest  [--store DIR] --adapter databento --from PATH|URL [--class future|equity] [--processing-delay S] [--roll volume|expiry]
//!                  [--session-gap MIN] [--point-values 'ROOT=V;...'] [--settlement 'ROOT=cash;...'] [--commission C] [--start DATE] [--end DATE]
//!                  [--dataset D --schema ohlcv-1m --symbols A,B --stype-in parent --key-env VAR] [--source NAME] [--report FILE] [--attributes K=V,...]
//! abt-data ingest  [--store DIR] --adapter orats --from PATH|URL [--tickers SPY,SPX] [--terms 'SPX=european:cash:am;...'] [--multiplier 100]
//!                  [--snapshots true] [--start DATE] [--end DATE] [--token-env VAR] [--source NAME] [--report FILE] [--attributes K=V,...]
//! abt-data ingest  [--store DIR] --adapter bundle_v2 --from DIR --class C [--base CAL] [--attributes K=V,...]
//! abt-data ingest  [--store DIR] --adapter synthetic --class C [--base CAL] [--days N] [--symbols A,B] [--seed N]
//! abt-data catalog [--store DIR] [--class C | --sources [--config FILE]]
//! abt-data resolve [--store DIR] --demand FILE|- [--config FILE] [--explain] [--dry-run] [--json]
//! abt-data query   [--store DIR] 'TEXT' [--view BINDING|VERSION] [--as-of-seq N] [--explain] [--parquet FILE]
//! abt-data stats   [--store DIR] [--relation R] [--json]
//! abt-data pin|unpin [--store DIR] VERSION | --binding B
//! abt-data gc      [--store DIR] [--disk-budget BYTES]
//! ```
//!
//! `norgate` reads the NDLake lake (D-7.7, `adapters::norgate`), ingested
//! under the source `norgate` with Norgate's `asset_id` as the key.
//! `databento` reads DBN or Parquet files, or the historical API at a URL
//! (`adapters::databento`): futures keyed by `instrument_id` and rolled into
//! continuous series, equities joined to the store's tickers.
//! `orats` reads ORATS's strikes files, or its Data API at a URL
//! (`adapters::orats`): option contracts keyed by their OCC symbols, their
//! underlyings joined to the store's tickers.
//! `bundle_v2` reads a bundle of the first formulation (a directory with its
//! manifest, `adapters::bundle`) under the registry's schemas of `--class`
//! and ingests it through the adapter of keyed rows. `synthetic` is the
//! deterministic markets of `adapters::synthetic`, ingested the same way;
//! `--base` names the bars' calendar (the class's finest by default: `1d`
//! for equities, `1m` for futures). The versions carry the calendar of the
//! bars, `reviewed = complete` for a bundle whose tests passed and for the
//! generators' markets, the bundle's `version`, and what `--attributes`
//! states (a reviewer vouching for a directory, D-2.1).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::exit;
use std::sync::Arc;

use crate::adapters::{bundle, market, synthetic};
use crate::catalog::{classes, render_class};
use crate::query::{execute, explain, ResolvedView};
use crate::service::{listen, DataService};
use crate::Store;
use abt_wire::generated::abt::wire::{Atom, Constant, ConstantKind, Disjunction, Identity, Latest, Op, Point, Predicate, QueryForm, Scan, Section, Series, View};
use abt_wire::scalar::Time;
use arrow::array::RecordBatch;

struct Args {
    cmd: String,
    positional: Vec<String>,
    opts: BTreeMap<String, String>,
    flags: Vec<String>,
}

const FLAGS: [&str; 4] = ["json", "explain", "dry-run", "sources"];

fn parse(args: &[String]) -> Args {
    let mut out = Args {
        cmd: args.first().cloned().unwrap_or_default(),
        positional: Vec::new(),
        opts: BTreeMap::new(),
        flags: Vec::new(),
    };
    let mut i = 1;
    while i < args.len() {
        let a = &args[i];
        if let Some(name) = a.strip_prefix("--") {
            if FLAGS.contains(&name) {
                out.flags.push(name.to_string());
            } else {
                let Some(v) = args.get(i + 1) else { fail(&format!("--{} takes a value", name)) };
                out.opts.insert(name.to_string(), v.clone());
                i += 1;
            }
        } else {
            out.positional.push(a.clone());
        }
        i += 1;
    }
    out
}

fn fail(m: &str) -> ! {
    eprintln!("{}", m);
    exit(2)
}

fn usage() -> ! {
    eprintln!(
        "usage:\n  abt data serve   [--store DIR] [--socket PATH] [--config FILE] [--clock-rate R [--clock-origin T]] [--grace 'CAL=SECONDS;...'] [--record synthetic --session S --start T --end T [--symbols A,B] [--bar 1m] [--delay MS] [--seed N]]   (a live session recorded while the service serves; abt run --mode live-sim --session S reads it)\n  abt data ingest  [--store DIR] --adapter norgate --from LAKE [--lists 'NAME=LABEL;...'] [--series 'TICKER=NAME;...'] [--start DATE] [--end DATE] [--limit N] [--scheme S] [--exceptions FILE] [--source NAME] [--report FILE] [--attributes K=V,...]\n  abt data ingest  [--store DIR] --adapter databento --from PATH|URL [--class future|equity] [--processing-delay S] [--roll volume|expiry] [--session-gap MIN] [--point-values 'ROOT=V;...'] [--settlement 'ROOT=cash;...'] [--commission C] [--start DATE] [--end DATE] [--dataset D --schema ohlcv-1m --symbols A,B --stype-in parent --key-env VAR] [--source NAME] [--report FILE] [--attributes K=V,...]\n  abt data ingest  [--store DIR] --adapter orats --from PATH|URL [--tickers SPY,SPX] [--terms 'SPX=european:cash:am;...'] [--multiplier 100] [--snapshots true] [--start DATE] [--end DATE] [--token-env VAR] [--source NAME] [--report FILE] [--attributes K=V,...]   (the option class from ORATS strikes; the underlyings must be in the store)\n  abt data ingest  [--store DIR] --adapter bundle_v2 --from DIR --class CLASS [--base CAL] [--attributes K=V,...]\n  abt data ingest  [--store DIR] --adapter synthetic --class CLASS [--base CAL] [--days N] [--symbols A,B,C] [--seed N]\n  abt data catalog [--store DIR] [--class CLASS | --sources [--config FILE]]\n  abt data resolve [--store DIR] --demand FILE|- [--config FILE] [--explain] [--dry-run] [--json]\n  abt data query   [--store DIR] 'REL [latest] [at T | over T0..T1] [for KEY,...] [where COL OP VALUE [and ...]] [r <= T]' [--view BINDING|VERSION] [--as-of-seq N] [--explain] [--parquet FILE]\n  abt data stats   [--store DIR] [--relation R] [--json]   (--json: each version's record, its attributes included)\n  abt data pin|unpin [--store DIR] VERSION | --binding B\n  abt data gc      [--store DIR] [--disk-budget BYTES]\n\n  --store DIR   the store (default $ABT_STORE, else ./abt-store)\n  --config FILE the source catalog (D-7.1): the sources beside the store, their offers, prices and quotas, as JSON; without it the store is the only source\n  --demand FILE a demand (D-7.2) as JSON, `-` for standard input; --dry-run plans it and buys nothing, --explain prints the plan's steps, costs and notes\n  --parquet F   write a query's answer to F as Parquet (labels and text as strings) instead of printing it; a run's outputs are kernel.<run>.<relation>"
    );
    exit(1)
}

fn store_dir(a: &Args) -> PathBuf {
    a.opts
        .get("store")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("ABT_STORE").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("abt-store"))
}

fn open(a: &Args) -> Store {
    Store::open(&store_dir(a)).unwrap_or_else(|e| fail(&format!("{}: {}", store_dir(a).display(), e)))
}

/// The class and the bars' calendar an ingest of a market is asked for:
/// `--class C` (one the registry knows) and `--base CAL` (the class's
/// finest by default).
fn class_of(a: &Args) -> (String, String) {
    let class = a.opts.get("class").cloned().unwrap_or_else(|| fail("--class names the class (equity, future, option)"));
    let def = classes().into_iter().find(|k| k.name == class).unwrap_or_else(|| fail(&format!("no class `{}`", class)));
    let base = a
        .opts
        .get("base")
        .map(|b| b.trim_start_matches('@').to_string())
        .or_else(|| def.base.map(|(b, _)| b.to_string()))
        .unwrap_or_else(|| "1d".to_string());
    (class, base)
}

pub fn main(args: &[String]) {
    let a = parse(args);
    match a.cmd.as_str() {
        "serve" => {
            let path = a.opts.get("socket").map(PathBuf::from).unwrap_or_else(|| abt_wire::transport::default_socket("data"));
            let (clock, grace) = live_clock(&a);
            let svc = DataService::with_clock(open(&a), sources(&a), clock.clone(), grace);
            let recording = a.opts.get("record").map(|source| record(&a, &svc, source, clock));
            let handle = listen(&path, svc).unwrap_or_else(|e| fail(&format!("{}: {}", path.display(), e)));
            eprintln!("serving {} on {}", store_dir(&a).display(), path.display());
            if let Some(r) = recording {
                match r.join() {
                    Ok(Ok(state)) => eprintln!("the session recorded {} batches, {} rows", state.batches, state.rows),
                    Ok(Err(e)) => eprintln!("the session's feed failed: {}", e),
                    Err(_) => eprintln!("the session's recorder stopped"),
                }
            }
            let _ = handle.join();
        }
        "ingest" => {
            if let Some(v @ ("norgate" | "databento" | "orats")) = a.opts.get("adapter").map(String::as_str) {
                vendor_ingest(&a, v);
                return;
            }
            let (class, base) = class_of(&a);
            let sub_daily = synthetic::is_sub_daily(&base);
            let mut store = open(&a);
            let m = match a.opts.get("adapter").map(String::as_str) {
                Some("bundle_v2") => {
                    let from = a.opts.get("from").cloned().unwrap_or_else(|| fail("--from DIR names the bundle"));
                    let schemas = store.registry.schemas(None);
                    let (m, manifest) = bundle::load(Path::new(&from), &class, &schemas).unwrap_or_else(|e| fail(&e.to_string()));
                    eprintln!("bundle {}@{}{}", manifest.name, manifest.version, if m.unreviewed { ", its tests not passed: unreviewed" } else { "" });
                    m
                }
                Some("synthetic") => {
                    let (names, days, start, seed) = synthetic::defaults(sub_daily);
                    let syms: Vec<String> = a
                        .opts
                        .get("symbols")
                        .map(|s| s.split(',').map(|x| x.trim().to_string()).collect())
                        .unwrap_or_else(|| names.iter().map(|s| s.to_string()).collect());
                    let syms: Vec<&str> = syms.iter().map(String::as_str).collect();
                    let days: usize = a.opts.get("days").map(|d| d.parse().unwrap_or_else(|_| fail("--days takes a number"))).unwrap_or(days);
                    let seed: u64 = a.opts.get("seed").map(|d| d.parse().unwrap_or_else(|_| fail("--seed takes a number"))).unwrap_or(seed);
                    synthetic::market(&class, sub_daily, &syms, start, days, seed)
                }
                _ => fail("--adapter is norgate, databento, orats, bundle_v2 or synthetic"),
            };
            let extra = statements(&a);
            let attributes = market::attributes(&m, &base, &extra);
            market::ingest(&mut store, &m, &class, &attributes).unwrap_or_else(|e| fail(&e.to_string()));
            for (r, s, v) in store.registry.currents() {
                println!("{} {} {}", r, s, v);
            }
        }
        "catalog" => {
            if a.flags.iter().any(|f| f == "sources") {
                print!("{}", sources(&a).catalog.render());
                return;
            }
            let store = open(&a);
            match a.opts.get("class") {
                Some(c) => match classes().into_iter().find(|k| k.name == c) {
                    Some(k) => print!("{}", render_class(&k)),
                    None => fail(&format!("no class `{}`", c)),
                },
                None => {
                    for s in store.registry.schemas(None) {
                        println!("{} ({})", s.name, s.class);
                    }
                }
            }
        }
        "resolve" => {
            let path = a.opts.get("demand").cloned().unwrap_or_else(|| usage());
            let text = if path == "-" {
                let mut t = String::new();
                std::io::Read::read_to_string(&mut std::io::stdin(), &mut t).unwrap_or_else(|e| fail(&format!("standard input: {}", e)));
                t
            } else {
                std::fs::read_to_string(&path).unwrap_or_else(|e| fail(&format!("{}: {}", path, e)))
            };
            // A need without `optional` is required, as a FlatBuffers reader
            // takes the field's absence (E-72).
            let mut value: serde_json::Value = serde_json::from_str(&text).unwrap_or_else(|e| fail(&format!("{}: not a demand: {}", path, e)));
            if let Some(needs) = value.get_mut("needs").and_then(|n| n.as_array_mut()) {
                for need in needs.iter_mut().filter_map(|n| n.as_object_mut()) {
                    need.entry("optional").or_insert(serde_json::Value::Bool(false));
                }
            }
            let demand: abt_wire::generated::abt::wire::Demand = serde_json::from_value(value).unwrap_or_else(|e| fail(&format!("{}: not a demand: {}", path, e)));
            let dry_run = a.flags.iter().any(|f| f == "dry-run");
            let svc = DataService::with_sources(open(&a), sources(&a));
            let r = svc.resolve(&demand, dry_run).unwrap_or_else(|e| {
                eprintln!("{}", e);
                exit(1)
            });
            if a.flags.iter().any(|f| f == "json") {
                let out = serde_json::json!({ "binding": r.binding, "plan": r.plan, "over_budget": r.over_budget, "notes": r.notes });
                println!("{}", out);
                return;
            }
            if a.flags.iter().any(|f| f == "explain") || dry_run {
                for step in r.plan.steps.iter().flatten() {
                    let c = step.cost.as_deref();
                    println!(
                        "need {}: {} from {} {} over {}..{}: money {:.2}, {:.1} s, {} bytes, {} requests",
                        step.need,
                        step.relation,
                        step.source,
                        step.version.as_deref().map(|v| format!("at {}", &v[..12.min(v.len())])).unwrap_or_else(|| "to buy".into()),
                        step.from.map(|t| Time(t).iso()).unwrap_or_else(|| "-".into()),
                        step.to.map(|t| Time(t).iso()).unwrap_or_else(|| "-".into()),
                        c.map_or(0.0, |c| c.money),
                        c.map_or(0.0, |c| c.seconds),
                        c.map_or(0, |c| c.bytes),
                        c.map_or(0, |c| c.requests)
                    );
                }
                if let Some(c) = r.plan.cost.as_deref() {
                    println!("cost: money {:.2}, {:.1} s, {} bytes, {} requests", c.money, c.seconds, c.bytes, c.requests);
                }
                if let Some(o) = &r.over_budget {
                    let c = o.cost.as_deref();
                    println!(
                        "over budget: the cheapest plan covering every span costs money {:.2}, {} bytes, {} requests",
                        c.map_or(0.0, |c| c.money),
                        c.map_or(0, |c| c.bytes),
                        c.map_or(0, |c| c.requests)
                    );
                }
                for n in &r.notes {
                    println!("note: {}", n);
                }
            }
            match &r.binding.id {
                Some(id) => println!("binding {}", id),
                None => println!("binding not recorded (a dry run)"),
            }
            for p in r.binding.pairs.iter().flatten() {
                println!("  {} {}", p.relation, p.version);
            }
        }
        "stats" => {
            let store = open(&a);
            let current: std::collections::BTreeSet<String> = store.registry.currents().into_iter().map(|(_, _, v)| v).collect();
            for v in store.registry.versions().filter(|v| a.opts.get("relation").is_none_or(|r| *r == v.relation)) {
                if a.flags.iter().any(|f| f == "json") {
                    println!("{}", serde_json::json!({ "version": v, "current": current.contains(&v.version) }));
                    continue;
                }
                println!(
                    "{} {} {} rows {} {}..{}{}",
                    v.relation,
                    v.source,
                    &v.version[..16],
                    v.rows,
                    v.from.map(|t| Time(t).iso()).unwrap_or_default(),
                    v.to.map(|t| Time(t).iso()).unwrap_or_default(),
                    if current.contains(&v.version) { " current" } else { "" }
                );
            }
        }
        "query" => {
            let store = open(&a);
            let text = a.positional.first().cloned().unwrap_or_else(|| usage());
            let (form, r_max) = parse_query(&store, &text).unwrap_or_else(|e| fail(&e));
            let mut view = View {
                versions: None,
                binding: None,
                as_of_seq: a.opts.get("as-of-seq").map(|s| s.parse().unwrap_or_else(|_| fail("--as-of-seq takes a number"))),
            };
            if let Some(v) = a.opts.get("view") {
                if store.registry.binding(v).is_some() {
                    view.binding = Some(v.clone());
                } else {
                    let rel = store
                        .registry
                        .version(v)
                        .map(|r| r.relation.clone())
                        .unwrap_or_else(|| fail(&format!("{} is neither a binding nor a version", v)));
                    view.versions = Some(vec![abt_wire::generated::abt::wire::VersionRef { relation: rel, version: v.clone() }]);
                }
            }
            // The typed request first (D-4.5).
            println!(
                "{}",
                serde_json::to_string(&serde_json::json!({"view": view, "form": form, "receipt_filter": {"r_max": r_max}})).unwrap_or_default()
            );
            let resolved = ResolvedView::of(&store, Some(&view)).unwrap_or_else(|e| fail(&e.to_string()));
            if a.flags.iter().any(|f| f == "explain") {
                let e = explain(&store, &resolved, &form).unwrap_or_else(|e| fail(&e.to_string()));
                println!("rows {} bytes {} partitions {} unresolved {}", e.rows, e.bytes, e.partitions, e.unresolved);
                return;
            }
            let (schema, batches) = execute(&store, &resolved, &form, r_max, false).unwrap_or_else(|e| fail(&e.to_string()));
            match a.opts.get("parquet") {
                Some(path) => {
                    let rows = write_parquet(Path::new(path), schema, &batches).unwrap_or_else(|e| fail(&format!("--parquet {}: {}", path, e)));
                    println!("{} rows to {}", rows, path);
                }
                None => print!("{}", render(&batches)),
            }
        }
        "pin" | "unpin" => {
            let mut store = open(&a);
            let (versions, by): (Vec<String>, String) = match (a.positional.first(), a.opts.get("binding")) {
                (Some(v), _) => (vec![v.clone()], "manual".into()),
                (None, Some(b)) => (
                    store
                        .registry
                        .binding(b)
                        .unwrap_or_else(|| fail(&format!("no binding {}", b)))
                        .pairs
                        .unwrap_or_default()
                        .into_iter()
                        .map(|p| p.version)
                        .collect(),
                    format!("binding:{}", b),
                ),
                _ => usage(),
            };
            for v in versions {
                let r = if a.cmd == "pin" { store.pin(&v, &by) } else { store.unpin(&v, &by) };
                r.unwrap_or_else(|e| fail(&e.to_string()));
            }
        }
        "gc" => {
            let mut store = open(&a);
            let budget: u64 = a
                .opts
                .get("disk-budget")
                .map(|s| s.parse().unwrap_or_else(|_| fail("--disk-budget takes a number of bytes")))
                .unwrap_or(200 << 30);
            for v in store.gc(budget).unwrap_or_else(|e| fail(&e.to_string())) {
                println!("evicted {}", v);
            }
        }
        _ => usage(),
    }
}

/// An answer as a Parquet file, for the tools that read one (the parity
/// scripts, pandas): a label column as its text, a bytes column whose
/// values are all text as text. Returns the rows written.
fn write_parquet(path: &Path, schema: arrow::datatypes::SchemaRef, batches: &[RecordBatch]) -> Result<usize, String> {
    use arrow::array::{Array, AsArray};
    use arrow::datatypes::{DataType, Field, Schema};
    let is_text = |i: usize| batches.iter().all(|b| b.column(i).as_binary::<i32>().iter().all(|v| v.is_none_or(|v| std::str::from_utf8(v).is_ok())));
    let fields: Vec<Field> = schema
        .fields()
        .iter()
        .enumerate()
        .map(|(i, f)| match f.data_type() {
            DataType::Dictionary(_, v) if **v == DataType::Utf8 => Field::new(f.name(), DataType::Utf8, true),
            DataType::Binary if is_text(i) => Field::new(f.name(), DataType::Utf8, true),
            _ => f.as_ref().clone(),
        })
        .collect();
    let schema = Arc::new(Schema::new(fields));
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut w = parquet::arrow::ArrowWriter::try_new(file, schema.clone(), None).map_err(|e| e.to_string())?;
    let mut rows = 0;
    for b in batches {
        let cols: Vec<arrow::array::ArrayRef> = b
            .columns()
            .iter()
            .zip(schema.fields())
            .map(|(c, f)| match (c.data_type(), f.data_type()) {
                (DataType::Binary, DataType::Utf8) => {
                    let b = c.as_binary::<i32>();
                    let s: Vec<Option<&str>> = (0..b.len()).map(|i| (!b.is_null(i)).then(|| std::str::from_utf8(b.value(i)).unwrap_or_default())).collect();
                    Ok(Arc::new(arrow::array::StringArray::from(s)) as arrow::array::ArrayRef)
                }
                (from, to) if from != to => arrow::compute::cast(c, to).map_err(|e| e.to_string()),
                _ => Ok(c.clone()),
            })
            .collect::<Result<_, String>>()?;
        let b = RecordBatch::try_new(schema.clone(), cols).map_err(|e| e.to_string())?;
        rows += b.num_rows();
        w.write(&b).map_err(|e| e.to_string())?;
    }
    w.close().map_err(|e| e.to_string())?;
    Ok(rows)
}

/// A table as text: its columns, then a line per row.
fn render(batches: &[RecordBatch]) -> String {
    let mut out = String::new();
    let opts = arrow::util::display::FormatOptions::default().with_null("");
    for (i, b) in batches.iter().enumerate() {
        if i == 0 {
            out.push_str(&b.schema().fields().iter().map(|f| f.name().clone()).collect::<Vec<_>>().join(" | "));
            out.push('\n');
        }
        let cols: Vec<arrow::array::ArrayRef> = b
            .columns()
            .iter()
            .map(|c| match c.data_type() {
                arrow::datatypes::DataType::Timestamp(_, _) => {
                    let t = c.as_any().downcast_ref::<arrow::array::TimestampNanosecondArray>();
                    let s: Vec<Option<String>> = (0..c.len()).map(|k| t.filter(|t| !arrow::array::Array::is_null(*t, k)).map(|t| Time(t.value(k)).iso())).collect();
                    Arc::new(arrow::array::StringArray::from(s)) as arrow::array::ArrayRef
                }
                // Bytes that are text (a run's JSON, a decision) print as text.
                arrow::datatypes::DataType::Binary => {
                    let b = c.as_any().downcast_ref::<arrow::array::BinaryArray>().expect("a binary column");
                    let s: Vec<Option<String>> = (0..c.len())
                        .map(|k| {
                            (!arrow::array::Array::is_null(b, k)).then(|| match std::str::from_utf8(b.value(k)) {
                                Ok(text) => text.to_string(),
                                Err(_) => b.value(k).iter().map(|x| format!("{:02x}", x)).collect(),
                            })
                        })
                        .collect();
                    Arc::new(arrow::array::StringArray::from(s)) as arrow::array::ArrayRef
                }
                _ => c.clone(),
            })
            .collect();
        let fmts: Vec<_> = cols
            .iter()
            .map(|c| arrow::util::display::ArrayFormatter::try_new(c.as_ref(), &opts))
            .collect::<Result<_, _>>()
            .unwrap_or_default();
        for r in 0..b.num_rows() {
            out.push_str(&fmts.iter().map(|f| f.value(r).to_string()).collect::<Vec<_>>().join(" | "));
            out.push('\n');
        }
    }
    out
}

fn time(s: &str) -> Result<i64, String> {
    Time::parse_iso(s.trim()).map(|t| t.0).ok_or_else(|| format!("`{}` is not a time (YYYY-MM-DD[THH:MM[:SS]])", s))
}

/// An instrument key (a vendor key the registry holds) as its id.
fn key_id(store: &Store, key: &str) -> Result<u64, String> {
    let ins = crate::adapter::instruments(store).map_err(|e| e.to_string())?;
    ins.iter().find(|i| i.key == key).map(|i| i.id).ok_or_else(|| format!("`{}` is no instrument key of the store", key))
}

/// The text form of a query (D-4.5): a relation, then clauses.
fn parse_query(store: &Store, text: &str) -> Result<(QueryForm, Option<i64>), String> {
    let mut words = text.split_whitespace().peekable();
    let rel_word = words.next().ok_or("an empty query")?;
    let relation = rel_word.split('(').next().unwrap_or(rel_word).to_string();
    let schema = store.schema(&relation).map_err(|e| e.to_string())?;
    let (mut latest, mut at, mut over, mut keys, mut atoms, mut r_max) = (false, None, None, Vec::new(), Vec::new(), None);
    while let Some(w) = words.next() {
        match w {
            "latest" => latest = true,
            "at" => at = Some(time(words.next().ok_or("`at` takes a time")?)?),
            "over" => {
                let span = words.next().ok_or("`over` takes T0..T1")?;
                let (a, b) = span.split_once("..").ok_or("`over` takes T0..T1")?;
                over = Some((time(a)?, time(b)?));
            }
            "for" => {
                let list = words.next().ok_or("`for` takes keys")?;
                for k in list.trim_matches(|c| c == '(' || c == ')').split(',').filter(|k| !k.is_empty()) {
                    keys.push(key_id(store, k.trim())?);
                }
            }
            "where" | "and" => {
                let col = words.next().ok_or("`where` takes COL OP VALUE")?;
                let op = words.next().ok_or("`where` takes COL OP VALUE")?;
                let val = words.next().ok_or("`where` takes COL OP VALUE")?;
                let spec = schema.columns.iter().find(|c| c.name == col).ok_or(format!("`{}` has no column `{}`", relation, col))?;
                let op = match op {
                    "=" => Op::Eq,
                    "!=" => Op::Ne,
                    "<" => Op::Lt,
                    "<=" => Op::Le,
                    ">" => Op::Gt,
                    ">=" => Op::Ge,
                    o => return Err(format!("`{}` is not an operator (= != < <= > >=)", o)),
                };
                let c = match spec.ty {
                    abt_wire::generated::abt::wire::ColumnType::Label => Constant {
                        kind: ConstantKind::Label,
                        num: 0.0,
                        int: 0,
                        text: Some(val.trim_matches('"').to_string()),
                    },
                    abt_wire::generated::abt::wire::ColumnType::Time => Constant {
                        kind: ConstantKind::Time,
                        num: 0.0,
                        int: time(val)?,
                        text: None,
                    },
                    abt_wire::generated::abt::wire::ColumnType::Entity => Constant {
                        kind: ConstantKind::Entity,
                        num: 0.0,
                        int: key_id(store, val)? as i64,
                        text: None,
                    },
                    _ => Constant {
                        kind: ConstantKind::Num,
                        num: val.parse().map_err(|_| format!("`{}` is not a number", val))?,
                        int: 0,
                        text: None,
                    },
                };
                atoms.push(Atom {
                    column: col.to_string(),
                    op,
                    values: Some(vec![c]),
                });
            }
            "r" => {
                if words.next() != Some("<=") {
                    return Err("the receipt filter is `r <= T`".into());
                }
                r_max = Some(time(words.next().ok_or("`r <=` takes a time")?)?);
            }
            other => return Err(format!("`{}` is not a clause (latest, at, over, for, where, and, r <=)", other)),
        }
    }
    let ident = |k: u64| {
        Box::new(Identity {
            values: vec![Constant {
                kind: ConstantKind::Entity,
                num: 0.0,
                int: k as i64,
                text: None,
            }],
        })
    };
    let predicate = (!atoms.is_empty()).then(|| {
        Box::new(Predicate {
            all: Some(atoms.into_iter().map(|a| Disjunction { atoms: vec![a] }).collect()),
        })
    });
    let form = match (latest, at, over, keys.as_slice()) {
        (true, Some(t), None, [k]) => QueryForm::Latest(Box::new(Latest {
            relation,
            identity: Some(ident(*k)),
            t_max: t,
        })),
        (true, Some(t), None, []) => QueryForm::Latest(Box::new(Latest { relation, identity: None, t_max: t })),
        (false, Some(t), None, [k]) => QueryForm::Point(Box::new(Point {
            relation,
            identity: Some(ident(*k)),
            t,
        })),
        (false, Some(t), None, []) if predicate.is_none() => QueryForm::Section(Box::new(Section { relation, t })),
        (false, Some(t), None, []) => QueryForm::Scan(Box::new(Scan {
            relation,
            t0: t,
            t1: t,
            predicate,
            columns: None,
        })),
        (false, None, Some((t0, t1)), [k]) => QueryForm::Series(Box::new(Series {
            relation,
            identity: Some(ident(*k)),
            t0,
            t1,
        })),
        (false, None, Some((t0, t1)), []) => QueryForm::Scan(Box::new(Scan {
            relation,
            t0,
            t1,
            predicate,
            columns: None,
        })),
        (false, None, None, []) => QueryForm::Scan(Box::new(Scan {
            relation,
            t0: i64::MIN,
            t1: i64::MAX,
            predicate,
            columns: None,
        })),
        _ => return Err("one of: `at T` with at most one key, `latest at T`, `over T0..T1` with at most one key, or none".into()),
    };
    Ok((form, r_max))
}

/// The clock of the service's live streams: the wall clock, or with
/// `--clock-rate R` one running R times as fast from `--clock-origin T`
/// (default: one bar before the recorded session's start), a rehearsal's;
/// and the calendars' graces, `--grace 'base=0.25;days=0'` in seconds.
fn live_clock(a: &Args) -> (Arc<dyn crate::live::Clock>, BTreeMap<String, i64>) {
    let mut grace = BTreeMap::new();
    for kv in a.opts.get("grace").map(|s| s.split(';').map(str::to_string).collect::<Vec<_>>()).unwrap_or_default() {
        if kv.trim().is_empty() {
            continue;
        }
        let (k, v) = kv.split_once('=').unwrap_or_else(|| fail("--grace takes CALENDAR=SECONDS[;CALENDAR=SECONDS...]"));
        let secs: f64 = v.trim().parse().unwrap_or_else(|_| fail(&format!("--grace: `{}` is not a number of seconds", v)));
        grace.insert(k.trim().to_string(), (secs * 1e9).round() as i64);
    }
    let Some(rate) = a.opts.get("clock-rate") else {
        return (Arc::new(crate::live::WallClock), grace);
    };
    let rate: f64 = rate.parse().ok().filter(|r: &f64| *r > 0.0).unwrap_or_else(|| fail("--clock-rate takes a positive number"));
    let origin = match a.opts.get("clock-origin").or(a.opts.get("start")) {
        Some(t) => parse_time(t) - bar_of(a) * 2,
        None => fail("--clock-rate starts the clock at --clock-origin T (or two bars before --start)"),
    };
    (Arc::new(crate::live::ScaledClock::new(origin, rate)), grace)
}

fn parse_time(t: &str) -> i64 {
    abt_wire::scalar::parse_timestamp(t).unwrap_or_else(|| fail(&format!("`{}` is not a date or a time", t)))
}

/// The bar a recorded session plans (`--bar 1m`).
fn bar_of(a: &Args) -> i64 {
    let bar = a.opts.get("bar").map(String::as_str).unwrap_or("1m");
    crate::resolver::calendar_nanos(bar).unwrap_or_else(|| fail(&format!("--bar: `{}` is not a bar (1m, 5m, 1h)", bar)))
}

/// `abt data serve --record synthetic --session S --symbols A,B --start T
/// --end T [--bar 1m] [--delay MS] [--seed N]`: a rehearsal session (D-5.4)
/// of the synthetic feed's bars of `--symbols` at every bar from `--start`
/// to `--end`, each arriving `--delay` milliseconds after its close (50 by
/// default), recorded while the service serves. A vendor's live feed is
/// recorded through the data layer's `DataService::record` with its
/// adapter's subscription (ERRATA E-69).
fn record(a: &Args, svc: &Arc<DataService>, source: &str, clock: Arc<dyn crate::live::Clock>) -> crate::live::Recording {
    if source != "synthetic" {
        fail(&format!(
            "--record {}: the command line records the synthetic feed; a vendor's live feed is recorded through its adapter's subscription (ERRATA E-69)",
            source
        ));
    }
    let session = a.opts.get("session").cloned().unwrap_or_else(|| fail("--record names its session with --session S"));
    let (start, end) = (
        parse_time(a.opts.get("start").map(String::as_str).unwrap_or_else(|| fail("--record needs --start T"))),
        parse_time(a.opts.get("end").map(String::as_str).unwrap_or_else(|| fail("--record needs --end T"))),
    );
    let bar = bar_of(a);
    let labels: Vec<i64> = (0..).map(|k| start + k * bar).take_while(|t| *t <= end).collect();
    let symbols: Vec<String> = a
        .opts
        .get("symbols")
        .map(|s| s.split(',').map(|x| x.trim().to_string()).collect())
        .unwrap_or_else(|| vec!["AAA".into(), "BBB".into()]);
    let syms: Vec<&str> = symbols.iter().map(String::as_str).collect();
    let delay = a.opts.get("delay").map(|d| d.parse::<f64>().unwrap_or_else(|_| fail("--delay takes milliseconds"))).unwrap_or(50.0);
    let seed = a.opts.get("seed").map(|d| d.parse().unwrap_or_else(|_| fail("--seed takes a number"))).unwrap_or(7);
    let feed = crate::live::SyntheticFeed::new(clock, &syms, labels.clone(), (delay * 1e6).round() as i64, seed);
    let prelude = feed.prelude().unwrap_or_else(|e| fail(&e.to_string()));
    svc.open_session(&session, "synthetic", &prelude, &labels).unwrap_or_else(|e| fail(&e.to_string()));
    eprintln!("recording session `{}`: {} bars of {} from {}", session, labels.len(), symbols.join(", "), Time(start).iso());
    svc.record(&session, "synthetic", Box::new(feed))
}

/// A reviewer's statements of the versions an ingest writes: `--attributes
/// reviewed=complete,receipt=recorded`.
fn statements(a: &Args) -> BTreeMap<String, String> {
    let mut extra = BTreeMap::new();
    for kv in a.opts.get("attributes").map(|s| s.split(',').map(str::to_string).collect::<Vec<_>>()).unwrap_or_default() {
        let (k, v) = kv.split_once('=').unwrap_or_else(|| fail("--attributes takes KEY=VALUE[,KEY=VALUE...]"));
        extra.insert(k.trim().to_string(), v.trim().to_string());
    }
    extra
}

/// `abt data ingest --adapter norgate|databento|orats --from LOCATOR`: what a
/// vendor's adapter reads (D-7.7), ingested under its source (`--source`,
/// the adapter's name by default) with the vendor's keys; `--report FILE`
/// writes what the read did as JSON.
fn vendor_ingest(a: &Args, adapter: &str) {
    let from = a.opts.get("from").cloned().unwrap_or_else(|| fail("--from LOCATOR names what the adapter reads"));
    let source = a.opts.get("source").cloned().unwrap_or_else(|| adapter.to_string());
    let (got, vendor, report) = match adapter {
        "norgate" => {
            let mut settings = BTreeMap::new();
            settings.insert("lake".to_string(), from);
            for (opt, key) in [
                ("lists", "lists"),
                ("series", "series"),
                ("start", "from"),
                ("end", "to"),
                ("limit", "limit"),
                ("scheme", "scheme"),
                ("exceptions", "exceptions"),
            ] {
                if let Some(v) = a.opts.get(opt) {
                    settings.insert(key.to_string(), v.clone());
                }
            }
            let n = crate::adapters::norgate::Norgate::from_settings(&settings).unwrap_or_else(|e| fail(&e.to_string()));
            let (got, r) = n.read().unwrap_or_else(|e| fail(&e.to_string()));
            eprintln!(
                "norgate: {} assets, {} bars from {} to {}; {} splits, {} dividends, {} delistings, {} membership intervals, {} reviewed exceptions",
                r.universe_assets,
                r.rows_out,
                r.first_day.as_deref().unwrap_or("-"),
                r.last_day.as_deref().unwrap_or("-"),
                r.splits,
                r.dividends,
                r.delisting_reasons.values().sum::<usize>(),
                r.membership_intervals,
                r.exceptions
            );
            (got, "norgate", serde_json::to_value(&r).unwrap_or_default())
        }
        "databento" => {
            let mut settings = BTreeMap::new();
            let key = if from.starts_with("http://") || from.starts_with("https://") { "api" } else { "files" };
            settings.insert(key.to_string(), from);
            for (opt, key) in [
                ("class", "class"),
                ("processing-delay", "processing_delay"),
                ("roll", "roll"),
                ("session-gap", "session_gap"),
                ("point-values", "point_values"),
                ("settlement", "settlement"),
                ("commission", "commission"),
                ("start", "from"),
                ("end", "to"),
                ("dataset", "dataset"),
                ("schema", "schema"),
                ("symbols", "symbols"),
                ("stype-in", "stype_in"),
                ("key-env", "key_env"),
            ] {
                if let Some(v) = a.opts.get(opt) {
                    settings.insert(key.to_string(), v.clone());
                }
            }
            let d = crate::adapters::databento::Databento::from_settings(&settings).unwrap_or_else(|e| fail(&e.to_string()));
            let (got, r) = d.read(d.options()).unwrap_or_else(|e| fail(&e.to_string()));
            let rolls: usize = r.roots.values().map(|x| x.rolls.len()).sum();
            eprintln!(
                "databento: {} files, {} bars kept of {} from {} to {}; {}",
                r.files,
                r.bars_out,
                r.bars_in,
                r.first.as_deref().unwrap_or("-"),
                r.last.as_deref().unwrap_or("-"),
                if d.options().class == "equity" {
                    format!("{} symbols", r.symbols)
                } else {
                    format!("{} roots, {} contracts, {} rolls", r.roots.len(), r.roots.values().map(|x| x.contracts).sum::<usize>(), rolls)
                }
            );
            for (why, n) in r.dropped.iter().filter(|(_, n)| **n > 0) {
                eprintln!("note: {} rows left out: {}", n, why.replace('_', " "));
            }
            (got, "databento", serde_json::to_value(&r).unwrap_or_default())
        }
        "orats" => {
            let mut settings = BTreeMap::new();
            let key = if from.starts_with("http://") || from.starts_with("https://") { "api" } else { "files" };
            settings.insert(key.to_string(), from);
            for (opt, key) in [
                ("tickers", "tickers"),
                ("terms", "terms"),
                ("multiplier", "multiplier"),
                ("snapshots", "snapshots"),
                ("start", "from"),
                ("end", "to"),
                ("token-env", "token_env"),
            ] {
                if let Some(v) = a.opts.get(opt) {
                    settings.insert(key.to_string(), v.clone());
                }
            }
            let o = crate::adapters::orats::Orats::from_settings(&settings).unwrap_or_else(|e| fail(&e.to_string()));
            let (got, r) = o.read(o.options()).unwrap_or_else(|e| fail(&e.to_string()));
            eprintln!(
                "orats: {} files, {} rows kept of {}; {} contracts in {} snapshots of {}",
                r.files,
                r.rows_kept,
                r.rows_in,
                r.contracts,
                r.snapshots,
                r.tickers.iter().cloned().collect::<Vec<_>>().join(", ")
            );
            let report = serde_json::json!({ "files": r.files, "rows_in": r.rows_in, "rows_kept": r.rows_kept, "contracts": r.contracts, "snapshots": r.snapshots, "tickers": r.tickers });
            (got, "orats", report)
        }
        _ => fail(&format!("no adapter `{}`", adapter)),
    };
    if let Some(path) = a.opts.get("report") {
        let text = serde_json::to_string_pretty(&report).unwrap_or_default();
        std::fs::write(path, text + "\n").unwrap_or_else(|e| fail(&format!("{}: {}", path, e)));
    }
    let mut store = open(a);
    got.ingest(&mut store, vendor, &source, &statements(a)).unwrap_or_else(|e| fail(&e.to_string()));
    for (r, s, v) in store.registry.currents() {
        println!("{} {} {}", r, s, v);
    }
}

/// The sources beside the store: the catalog `--config FILE` names, with
/// the adapters its sources are fetched by.
fn sources(a: &Args) -> crate::resolver::Sources {
    let Some(path) = a.opts.get("config") else { return crate::resolver::Sources::store_only() };
    let catalog = crate::resolver::Catalog::load(Path::new(path)).unwrap_or_else(|e| fail(&e.to_string()));
    let (adapters, missing) = crate::adapters::of_catalog(&catalog);
    for m in missing {
        eprintln!("note: {}: no adapter {}; it is planned, not bought from", path, m);
    }
    crate::resolver::Sources { catalog, adapters }
}
