//! `abt data` (D-10): the data layer's commands, over a store directory
//! (`--store DIR`, else `$ABT_STORE`, else `./abt-store`).
//!
//! ```text
//! abt data serve   [--store DIR] [--socket PATH]
//! abt data ingest  [--store DIR] --adapter bundle_v2 --from DIR --class C <files...>
//! abt data ingest  [--store DIR] --adapter synthetic --class C [--days N] [--symbols A,B] [--seed N] <files...>
//! abt data catalog [--store DIR] [--class C]
//! abt data query   [--store DIR] 'TEXT' [--view BINDING|VERSION] [--as-of-seq N] [--json]
//! abt data stats   [--store DIR] [--relation R]
//! abt data pin|unpin [--store DIR] VERSION | --binding B
//! abt data gc      [--store DIR] [--disk-budget BYTES]
//! ```
//!
//! `bundle_v2` reads the present bundle formats (a bundle directory with its
//! manifest, or a directory of one Parquet file per relation) with the
//! kernel's loaders and ingests them through the adapter of keyed rows; the
//! class files among `<files...>` name the relations (ERRATA E-38).
//! `synthetic` is the present generators, ingested the same way.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::exit;
use std::sync::Arc;

use abt_data::catalog::{classes, render_class};
use abt_data::query::{execute, explain, ResolvedView};
use abt_data::service::{listen, DataService};
use abt_data::Store;
use abt_kernel::check::{class_program_at, Program, Workspace};
use abt_kernel::data::{load_parquet_dir, synthetic_for_program};
use abt_wire::generated::abt::wire::{Atom, Constant, ConstantKind, Disjunction, Identity, Latest, Op, Point, Predicate, QueryForm, Scan, Section, Series, View};
use abt_wire::scalar::Time;
use arrow::array::RecordBatch;

struct Args {
    cmd: String,
    positional: Vec<String>,
    opts: BTreeMap<String, String>,
    flags: Vec<String>,
}

const FLAGS: [&str; 2] = ["json", "explain"];

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
        "usage:\n  abt data serve   [--store DIR] [--socket PATH]\n  abt data ingest  [--store DIR] --adapter bundle_v2 --from DIR --class CLASS <files...>\n  abt data ingest  [--store DIR] --adapter synthetic --class CLASS [--days N] [--symbols A,B,C] [--seed N] <files...>\n  abt data catalog [--store DIR] [--class CLASS]\n  abt data query   [--store DIR] 'REL [latest] [at T | over T0..T1] [for KEY,...] [where COL OP VALUE [and ...]] [r <= T]' [--view BINDING|VERSION] [--as-of-seq N] [--explain]\n  abt data stats   [--store DIR] [--relation R]\n  abt data pin|unpin [--store DIR] VERSION | --binding B\n  abt data gc      [--store DIR] [--disk-budget BYTES]\n\n  --store DIR   the store (default $ABT_STORE, else ./abt-store)"
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

fn workspace(files: &[String]) -> Workspace {
    let mut ws = Workspace::new();
    let mut paths = Vec::new();
    fn collect(p: &Path, out: &mut Vec<PathBuf>) {
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
    for f in files {
        collect(Path::new(f), &mut paths);
    }
    paths.sort();
    for p in paths {
        let src = std::fs::read_to_string(&p).unwrap_or_else(|e| fail(&format!("{}: {}", p.display(), e)));
        if let Err(e) = ws.add_source(&src) {
            fail(&format!("{}:{}: parse error: {}", p.display(), e.span, e.message));
        }
    }
    ws
}

fn class_program(a: &Args) -> Program {
    let class = a.opts.get("class").cloned().unwrap_or_else(|| fail("--class CLASS names the class the rows are of"));
    let ws = workspace(&a.positional);
    class_program_at(&ws, &class, None).unwrap_or_else(|diags| {
        for d in &diags {
            eprintln!("{}", d);
        }
        fail(&format!("the class `{}` does not check; pass its class file and the library among the files", class))
    })
}

/// `abt data`.
pub fn main(args: &[String]) {
    let a = parse(args);
    match a.cmd.as_str() {
        "serve" => {
            let path = a.opts.get("socket").map(PathBuf::from).unwrap_or_else(|| abt_wire::transport::default_socket("data"));
            let svc = DataService::new(open(&a));
            let handle = listen(&path, svc).unwrap_or_else(|e| fail(&format!("{}: {}", path.display(), e)));
            eprintln!("serving {} on {}", store_dir(&a).display(), path.display());
            let _ = handle.join();
        }
        "ingest" => {
            let prog = class_program(&a);
            let ds = match a.opts.get("adapter").map(String::as_str) {
                Some("bundle_v2") => {
                    let from = a.opts.get("from").cloned().unwrap_or_else(|| fail("--from DIR names the bundle"));
                    let dir = Path::new(&from);
                    if dir.join("manifest.json").exists() {
                        let (ds, m) = abt_kernel::bundle::load_bundle(&prog, dir, true).unwrap_or_else(|e| fail(&e));
                        eprintln!("bundle {}@{}", m.name, m.version);
                        ds
                    } else {
                        let (ds, notes) = load_parquet_dir(&prog, dir).unwrap_or_else(|e| fail(&e));
                        for n in notes {
                            eprintln!("note: {}", n);
                        }
                        ds
                    }
                }
                Some("synthetic") => {
                    let syms: Vec<String> = a
                        .opts
                        .get("symbols")
                        .map(|s| s.split(',').map(|x| x.trim().to_string()).collect())
                        .unwrap_or_else(|| ["AAA", "XYZ", "BBB", "SPY", "DDD"].iter().map(|s| s.to_string()).collect());
                    let syms: Vec<&str> = syms.iter().map(String::as_str).collect();
                    let days: usize = a
                        .opts
                        .get("days")
                        .map(|d| d.parse().unwrap_or_else(|_| fail("--days takes a number")))
                        .unwrap_or(if prog.base.is_sub_daily() { 40 } else { 500 });
                    let seed: u64 = a.opts.get("seed").map(|d| d.parse().unwrap_or_else(|_| fail("--seed takes a number"))).unwrap_or(7);
                    let start = if prog.base.is_sub_daily() { (2024, 1, 2) } else { (2022, 1, 3) };
                    synthetic_for_program(&prog, &syms, start, days, seed)
                }
                _ => fail("--adapter is bundle_v2 or synthetic"),
            };
            let mut store = open(&a);
            crate::ingest_dataset(&mut store, &ds, &prog.class).unwrap_or_else(|e| fail(&e));
            for (r, s, v) in store.registry.currents() {
                println!("{} {} {}", r, s, v);
            }
        }
        "catalog" => {
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
        "stats" => {
            let store = open(&a);
            let current: std::collections::BTreeSet<String> = store.registry.currents().into_iter().map(|(_, _, v)| v).collect();
            for v in store.registry.versions().filter(|v| a.opts.get("relation").is_none_or(|r| *r == v.relation)) {
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
            let (_, batches) = execute(&store, &resolved, &form, r_max, false).unwrap_or_else(|e| fail(&e.to_string()));
            print!("{}", render(&batches));
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
    let ins = abt_data::adapter::instruments(store).map_err(|e| e.to_string())?;
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
