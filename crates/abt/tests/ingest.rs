//! The vendor adapters through the binary (D-7.7, D-10): `abt data ingest
//! --adapter norgate` reads a lake in NDLake's layout into a store, under
//! Norgate's keys, with its decisions stated on the versions, and a run
//! reads the store: the split, the dividend and the delisting are applied
//! as the lake books them. `--adapter databento` reads a DBN file: futures
//! rolled into a continuous series, and equities joined to the tickers the
//! lake put in the store.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Arc;

use abt_data::adapters::dbn::{self, rtype, Decoded, Header, InstrumentDef, Metadata, Record};
use arrow::array::{ArrayRef, Date32Array, Float64Array, Int64Array, RecordBatch, StringArray};
use arrow::datatypes::{Field, Schema};
use parquet::arrow::ArrowWriter;

/// The program files, relative to the crate root the commands run in.
const CATALOG: &str = "corpus-v2/catalog";
const LIB: &str = "corpus-v2/lib";

/// A fresh directory per call: tests run in parallel.
fn temp_dir(tag: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("abt-ingest-{}-{}-{}", tag, std::process::id(), n));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn abt(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_abt")).args(args).current_dir(abt_tests::repo_root()).output().expect("abt runs")
}

fn text(o: &Output) -> (String, String) {
    (String::from_utf8_lossy(&o.stdout).into_owned(), String::from_utf8_lossy(&o.stderr).into_owned())
}

fn write(path: &Path, cols: Vec<(&str, ArrayRef)>) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let schema = Arc::new(Schema::new(cols.iter().map(|(n, a)| Field::new(*n, a.data_type().clone(), true)).collect::<Vec<_>>()));
    let batch = RecordBatch::try_new(schema.clone(), cols.into_iter().map(|(_, a)| a).collect()).unwrap();
    let mut w = ArrowWriter::try_new(fs::File::create(path).unwrap(), schema, None).unwrap();
    w.write(&batch).unwrap();
    w.close().unwrap();
}

fn strs(v: Vec<&str>) -> ArrayRef {
    Arc::new(StringArray::from(v))
}

/// The days since 1970-01-01 of the `i`th of ten weekdays from 2024-01-08.
fn day(i: usize) -> i32 {
    let monday = (abt_wire::scalar::parse_timestamp("2024-01-08").unwrap() / 86_400_000_000_000) as i32;
    monday + (i / 5 * 7 + i % 5) as i32
}

/// Ten weekdays from 2024-01-08 in NDLake's layout: X (asset 1)
/// throughout, paying 0.5 on day 3; Y (asset 2) split 2-for-1 on day 4,
/// its last print on day 6 at a price that reads as an acquisition. Both
/// are S&P 500 members; X is GICS 45 from day 0.
fn lake() -> PathBuf {
    let dir = temp_dir("lake");
    let mut b: Vec<(i64, &str, usize, f64)> = Vec::new();
    for i in 0..10 {
        b.push((1, "X", i, 10.0 + i as f64));
        if i <= 6 {
            b.push((2, "Y", i, if i < 4 { 40.0 } else { 20.0 }));
        }
    }
    let prices = |f: &dyn Fn(f64) -> f64| -> ArrayRef { Arc::new(Float64Array::from(b.iter().map(|r| f(r.3)).collect::<Vec<_>>())) };
    write(
        &dir.join("curated/bars_by_date/year=2024/part-0.parquet"),
        vec![
            ("asset_id", Arc::new(Int64Array::from(b.iter().map(|r| r.0).collect::<Vec<_>>())) as ArrayRef),
            ("ticker", strs(b.iter().map(|r| r.1).collect())),
            ("source_symbol", strs(b.iter().map(|r| r.1).collect())),
            ("date", Arc::new(Date32Array::from(b.iter().map(|r| day(r.2)).collect::<Vec<_>>()))),
            ("open", prices(&|c| c)),
            ("high", prices(&|c| c + 1.0)),
            ("low", prices(&|c| c - 1.0)),
            ("close", prices(&|c| c)),
            ("volume", Arc::new(Float64Array::from(vec![1_000_000.0; b.len()]))),
            ("series_type", strs(vec!["equity"; b.len()])),
        ],
    );
    write(
        &dir.join("registry/corporate_actions.parquet"),
        vec![
            ("asset_id", Arc::new(Int64Array::from(vec![1, 2])) as ArrayRef),
            ("action_type", strs(vec!["DIV", "SPLIT"])),
            ("ex_date", Arc::new(Date32Array::from(vec![day(3), day(4)]))),
            ("div_cash", Arc::new(Float64Array::from(vec![Some(0.5), None]))),
            ("split_ratio", Arc::new(Float64Array::from(vec![None, Some(2.0)]))),
        ],
    );
    write(
        &dir.join("indexes/index_membership.parquet"),
        vec![
            ("index_name", strs(vec!["S&P 500", "S&P 500"])),
            ("asset_id", Arc::new(Int64Array::from(vec![1, 2])) as ArrayRef),
            ("member_from", Arc::new(Date32Array::from(vec![day(0) - 400, day(0) - 400]))),
            ("member_to", Arc::new(Date32Array::from(vec![None, None]))),
        ],
    );
    write(
        &dir.join("classification/classification_full.parquet"),
        vec![
            ("asset_id", Arc::new(Int64Array::from(vec![1])) as ArrayRef),
            ("scheme", strs(vec!["GICS"])),
            ("level", Arc::new(Int64Array::from(vec![1]))),
            ("code", strs(vec!["45"])),
            ("snapshot_date", Arc::new(Date32Array::from(vec![day(0)]))),
        ],
    );
    write(
        &dir.join("reviewed.parquet"),
        vec![
            ("test", strs(vec!["action reconciliation"])),
            ("symbol", strs(vec!["Y"])),
            ("date", Arc::new(Date32Array::from(vec![day(2)])) as ArrayRef),
            ("reason", strs(vec!["real move: reviewed"])),
        ],
    );
    dir
}

/// A daily strategy that buys ten shares of every listed name it does not
/// hold; it reads the actions, so the executor applies the delisting.
fn hold_daily(dir: &Path) -> String {
    let src = abt_tests::StrategyParts {
        name: "hold_v2".into(),
        needs: "need bars @1d   need actions, listing".into(),
        params: "    qty : Quantity<Shares> = 10 shares".into(),
        decides: "    decide(T, trade(A, qty)) :- universe(A, T), flat(A, T).".into(),
        ..Default::default()
    }
    .render();
    let path = dir.join("hold_v2.dsl");
    fs::write(&path, src).unwrap();
    path.to_string_lossy().into_owned()
}

/// A command run against the store served on a socket (`--data SOCKET`
/// appended), the service stopped after.
fn served(store: &str, socket: &Path, args: &[&str]) -> Output {
    let sock = socket.to_string_lossy().into_owned();
    let mut server = Command::new(env!("CARGO_BIN_EXE_abt"))
        .args(["data", "serve", "--store", store, "--socket", &sock])
        .current_dir(abt_tests::repo_root())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    for _ in 0..400 {
        if socket.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    let mut all: Vec<&str> = args.to_vec();
    all.extend(["--data", &sock]);
    let o = abt(&all);
    let _ = server.kill();
    let _ = server.wait();
    o
}

/// The run id a run over a store prints.
fn run_id(o: &Output) -> String {
    let out = text(o).0;
    let line = out.lines().find(|l| l.starts_with("run ")).unwrap_or_else(|| panic!("no run line: {}", out));
    line.trim_start_matches("run ").split(':').next().unwrap().to_string()
}

#[test]
fn a_norgate_lake_is_ingested_into_a_store_and_a_run_reads_it() {
    let dir = lake();
    let store = dir.join("store").to_string_lossy().into_owned();
    let from = dir.to_string_lossy().into_owned();
    let report = dir.join("report.json").to_string_lossy().into_owned();
    let exceptions = dir.join("reviewed.parquet").to_string_lossy().into_owned();
    let o = abt(&[
        "data",
        "ingest",
        "--store",
        &store,
        "--adapter",
        "norgate",
        "--from",
        &from,
        "--start",
        "2024-01-01",
        "--exceptions",
        &exceptions,
        "--attributes",
        "reviewed=complete",
        "--report",
        &report,
    ]);
    let (out, err) = text(&o);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out, err);
    assert!(err.contains("norgate: 2 assets, 17 bars from 2024-01-08 to 2024-01-19; 1 splits, 1 dividends, 1 delistings"), "{}", err);
    assert!(err.contains("1 reviewed exceptions"), "{}", err);
    assert!(out.contains("equity.close norgate "), "{}", out);
    let r: serde_json::Value = serde_json::from_str(&fs::read_to_string(&report).unwrap()).unwrap();
    assert_eq!(r["delisting_reasons"]["acquisition"], 1, "{}", r);
    // The versions state the adapter's decisions and the reviewer's.
    let o = abt(&["data", "stats", "--store", &store, "--relation", "equity.dividend", "--json"]);
    let v: serde_json::Value = serde_json::from_str(text(&o).0.lines().next().unwrap()).unwrap();
    let attrs = &v["version"]["attributes"];
    assert_eq!(attrs["dividend_dates"], "ex_only", "{}", v);
    assert_eq!(attrs["reviewed"], "complete", "{}", v);
    assert!(attrs["reviewed_exceptions"].as_str().unwrap().contains("\"key\":\"2\""), "{}", v);
    // A run over the store, served: Y's delisting is applied on day 7.
    let strategy = hold_daily(&dir);
    let o = served(&store, &dir.join("data.sock"), &["run", "--strategy", "hold_v2", "--quiet", CATALOG, LIB, &strategy]);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", text(&o).0, text(&o).1);
    let run = run_id(&o);
    let o = abt(&["data", "query", "--store", &store, &format!("kernel.{}.events", run)]);
    let events = text(&o).0;
    assert!(events.lines().any(|l| l.contains("delisted") && l.contains("2024-01-17")), "{}", events);
    assert!(events.lines().any(|l| l.contains("split") && l.contains("2024-01-12")), "{}", events);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_lake_without_the_universe_is_refused_naming_it() {
    let dir = lake();
    let store = dir.join("store").to_string_lossy().into_owned();
    let from = dir.to_string_lossy().into_owned();
    let o = abt(&["data", "ingest", "--store", &store, "--adapter", "norgate", "--from", &from, "--lists", "Russell 2000=R2K"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(text(&o).1.contains("Russell 2000"), "{}", text(&o).1);
    let o = abt(&["data", "ingest", "--store", &store, "--adapter", "norgate", "--from", &from, "--start", "someday"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(text(&o).1.contains("someday"), "{}", text(&o).1);
    let _ = fs::remove_dir_all(&dir);
}

fn t(s: &str) -> i64 {
    abt_wire::scalar::parse_timestamp(s).unwrap()
}

fn meta(dataset: &str, mappings: Vec<dbn::SymbolMapping>) -> Metadata {
    Metadata {
        version: 3,
        dataset: dataset.into(),
        schema: Some(6),
        start: t("2024-01-01") as u64,
        end: Some(t("2024-04-01") as u64),
        limit: None,
        stype_in: Some(1),
        stype_out: 0,
        ts_out: false,
        symbol_cstr_len: 71,
        symbols: vec![],
        partial: vec![],
        not_found: vec![],
        mappings,
    }
}

fn bar(id: u32, start: i64, close: f64, volume: u64) -> Decoded {
    let px = (close * 1e9).round() as i64;
    Decoded {
        record: Record::Ohlcv {
            hd: Header {
                rtype: rtype::OHLCV_1M,
                publisher_id: 1,
                instrument_id: id,
                ts_event: start as u64,
            },
            open: px,
            high: px,
            low: px,
            close: px,
            volume,
        },
        ts_out: None,
    }
}

fn def(id: u32, symbol: &str, expiration: &str) -> Decoded {
    Decoded {
        record: Record::InstrumentDef(Box::new(InstrumentDef {
            hd: Header {
                rtype: rtype::INSTRUMENT_DEF,
                publisher_id: 1,
                instrument_id: id,
                ts_event: t("2024-03-01") as u64,
            },
            ts_recv: t("2024-03-01") as u64,
            raw_symbol: symbol.into(),
            asset: "ES".into(),
            instrument_class: b'F',
            expiration: t(expiration) as u64,
            activation: t("2023-06-16") as u64,
            min_price_increment: 250_000_000,
            unit_of_measure_qty: 50_000_000_000,
            currency: "USD".into(),
            ..InstrumentDef::default()
        })),
        ts_out: None,
    }
}

#[test]
fn a_dbn_file_of_futures_becomes_a_continuous_series_in_a_store() {
    let dir = temp_dir("dbn-futures");
    let mut records = vec![def(1, "ESH4", "2024-03-15T13:30:00"), def(2, "ESM4", "2024-06-21T13:30:00")];
    for (d, day) in ["2024-03-11", "2024-03-12", "2024-03-13"].iter().enumerate() {
        for m in 0..5 {
            let start = t(day) + (14 * 60 + 30 + m) * 60_000_000_000;
            let (vh, vm) = [(900, 100), (300, 800), (50, 1000)][d];
            records.push(bar(1, start, 5000.0 + (10 * d as i64 + m) as f64, vh));
            records.push(bar(2, start, 5050.0 + (10 * d as i64 + m) as f64, vm));
        }
    }
    let file = dir.join("es.dbn");
    fs::write(&file, dbn::encode(&meta("GLBX.MDP3", vec![]), &records)).unwrap();
    let store = dir.join("store").to_string_lossy().into_owned();
    let from = file.to_string_lossy().into_owned();
    let o = abt(&[
        "data",
        "ingest",
        "--store",
        &store,
        "--adapter",
        "databento",
        "--from",
        &from,
        "--processing-delay",
        "2",
        "--settlement",
        "ES=cash",
    ]);
    let (out, err) = text(&o);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out, err);
    assert!(err.contains("databento: 1 files, 15 bars kept of 30 from 2024-03-11T14:31:00"), "{}", err);
    assert!(err.contains("1 roots, 2 contracts, 1 rolls"), "{}", err);
    for r in ["future.close databento ", "future.session databento ", "continuous databento ", "future_spec databento "] {
        assert!(out.contains(r), "{} not written: {}", r, out);
    }
    // The series is back-adjusted: the first session's closes are H4's
    // plus the roll's gap.
    let o = abt(&["data", "query", "--store", &store, "future.close over 2024-03-11..2024-03-14"]);
    let rows = text(&o).0;
    assert_eq!(rows.lines().count(), 2 + 15, "{}", rows);
    assert!(rows.lines().nth(2).unwrap().contains("| 5050.0 |"), "{}", rows);
    let o = abt(&["data", "stats", "--store", &store, "--relation", "future.close", "--json"]);
    let v: serde_json::Value = serde_json::from_str(text(&o).0.lines().next().unwrap()).unwrap();
    assert_eq!(v["version"]["attributes"]["prices"], "back_adjusted", "{}", v);
    assert_eq!(v["version"]["attributes"]["roll"], "volume", "{}", v);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_dbn_file_of_equities_is_joined_to_the_lakes_tickers() {
    let dir = lake();
    let store = dir.join("store").to_string_lossy().into_owned();
    let from = dir.to_string_lossy().into_owned();
    let o = abt(&["data", "ingest", "--store", &store, "--adapter", "norgate", "--from", &from, "--start", "2024-01-01"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o).1);
    let write_stream = |name: &str, symbols: &[(&str, u32)]| -> String {
        let mappings = symbols
            .iter()
            .map(|(s, id)| dbn::SymbolMapping {
                raw_symbol: s.to_string(),
                intervals: vec![dbn::MappingInterval {
                    start_date: 20240109,
                    end_date: 20240110,
                    symbol: id.to_string(),
                }],
            })
            .collect();
        let mut records = Vec::new();
        for m in 0..3 {
            for (_, id) in symbols {
                records.push(bar(*id, t("2024-01-09T14:30:00") + m * 60_000_000_000, 10.0 + m as f64, 100));
            }
        }
        let path = dir.join(name);
        fs::write(&path, dbn::encode(&meta("XNAS.ITCH", mappings), &records)).unwrap();
        path.to_string_lossy().into_owned()
    };
    let file = write_stream("eq.dbn", &[("X", 100), ("Y", 200)]);
    let o = abt(&["data", "ingest", "--store", &store, "--adapter", "databento", "--class", "equity", "--from", &file]);
    let (out, err) = text(&o);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out, err);
    assert!(err.contains("6 bars kept of 6") && err.contains("2 symbols"), "{}", err);
    let version = out
        .lines()
        .find_map(|l| l.strip_prefix("equity.close databento "))
        .unwrap_or_else(|| panic!("no databento version: {}", out))
        .to_string();
    // The minute bars are the lake's assets (ids 1 and 2), by symbol at the
    // date.
    let o = abt(&["data", "query", "--store", &store, "equity.close over 2024-01-09T14:31:00..2024-01-09T14:33:00", "--view", &version]);
    let rows = text(&o).0;
    assert_eq!(rows.lines().count(), 2 + 6, "{}", rows);
    assert!(rows.lines().skip(2).all(|l| l.starts_with("1 | ") || l.starts_with("2 | ")), "{}", rows);
    // A symbol the lake does not carry refuses the read, naming it.
    let file = write_stream("bad.dbn", &[("X", 100), ("NOPE", 300)]);
    let o = abt(&["data", "ingest", "--store", &store, "--adapter", "databento", "--class", "equity", "--from", &file]);
    assert_eq!(o.status.code(), Some(2));
    assert!(text(&o).1.contains("`NOPE`"), "{}", text(&o).1);
    let _ = fs::remove_dir_all(&dir);
}
