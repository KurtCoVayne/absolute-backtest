//! The vendor adapters (data-bundle doc, sections 3 and 10): a Norgate-style
//! daily export and a Databento-style minute export each become a catalog
//! dataset of the equity class with stable identifiers, point-in-time
//! membership and status, recorded availability, and a bundle whose
//! manifest says where the data came from and what was decided on the way.

use abt_tests as common;

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use abt_kernel::bundle::{load_bundle, read_manifest, write_bundle};
use abt_kernel::check::{class_program, Program};
use abt_kernel::ingest::{databento_minute, norgate_daily};
use abt_kernel::kernel::time::{format_timestamp, parse_timestamp};
use abt_kernel::kernel::{run, ExecConfig, Value};
use abt_kernel::table::write_text_table;

/// The program files, relative to the crate root the commands run in.
const CATALOG: &str = "corpus-v2/catalog";
const LIB: &str = "corpus-v2/lib";
const TRM: &str = "corpus-v2/strategies/total_return_momentum.dsl";

/// The program whose demand covers the equity class whole: what `abt
/// bundle build` ingests with, so that every relation of the catalog is
/// read and stored.
fn class_prog() -> Program {
    class_program(&common::base_workspace(), "equity").unwrap_or_else(|d| panic!("the equity class should check as a program:\n{}", common::text(&d)))
}

/// A daily strategy that buys ten shares of every listed name it does not
/// hold; it reads the actions, so the executor applies the delisting.
fn hold_daily() -> String {
    common::StrategyParts {
        name: "hold_v2".into(),
        needs: "need bars @1d   need actions, listing".into(),
        params: "    qty : Quantity<Shares> = 10 shares".into(),
        decides: "    decide(T, trade(A, qty)) :- universe(A, T), flat(A, T).".into(),
        ..Default::default()
    }
    .render()
}

/// The same strategy on minute bars, deciding on the minute base and
/// reporting by day (the listing is a daily fact, read as of the minute).
fn hold_minute() -> String {
    common::StrategyParts {
        name: "minute_hold".into(),
        needs: "need bars @1m   need listing".into(),
        calendars: Some("daily = days".into()),
        params: "    qty : Quantity<Shares> = 1 shares".into(),
        decides: "    decide(T, trade(A, qty)) :- universe(A, T), flat(A, T).".into(),
        report: "calendar daily   periods 252 per year".into(),
        ..Default::default()
    }
    .render()
}

/// A fresh directory per call: tests run in parallel and several build the
/// same fixture, so a tag and the process id alone would collide.
fn temp_dir(tag: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("abt-ingest-{}-{}-{}", tag, std::process::id(), n));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn abt(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_abt")).args(args).current_dir(abt_tests::repo_root()).output().expect("abt runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Ten weekdays from 2024-01-08: X (security E1) throughout, Y (E2) split
/// 2-for-1 on day 5 and delisted by acquisition on day 8.
fn norgate_fixture() -> PathBuf {
    let dir = temp_dir("norgate");
    let days = abt_kernel::data::business_days((2024, 1, 8), 10);
    let d = |i: usize| format_timestamp(days[i]);
    let mut prices = String::from("symbol,date,open,high,low,close,volume\n");
    for (i, _) in days.iter().enumerate() {
        prices.push_str(&format!("X,{},10,11,9,{},1000000\n", d(i), 10.0 + i as f64));
        let y = if i < 4 { 40.0 } else { 20.0 };
        prices.push_str(&format!("Y,{},{y},{y},{y},{y},500000\n", d(i)));
    }
    write_text_table(&dir.join("prices.parquet"), &prices).unwrap();
    write_text_table(&dir.join("symbols.parquet"), &format!("id,symbol,from,to\nE1,X,2020-01-01,\nE2,Y,2020-01-01,{}\n", d(9))).unwrap();
    write_text_table(&dir.join("splits.parquet"), &format!("symbol,ex_date,factor\nY,{},2\n", d(4))).unwrap();
    write_text_table(
        &dir.join("dividends.parquet"),
        &format!("symbol,announce_date,ex_date,pay_date,amount\nX,{},{},{},0.5\n", d(1), d(3), d(6)),
    )
    .unwrap();
    write_text_table(&dir.join("delistings.parquet"), &format!("symbol,date,reason\nY,{},acquisition\n", d(7))).unwrap();
    write_text_table(&dir.join("membership.parquet"), &format!("symbol,index,from,to\nX,SPX,{},\nY,SPX,{},{}\n", d(0), d(0), d(6))).unwrap();
    write_text_table(&dir.join("classification.parquet"), &format!("symbol,scheme,code,from,to\nX,sector,tech,{},\n", d(0))).unwrap();
    dir
}

#[test]
fn a_norgate_export_becomes_the_catalog_with_identities_membership_and_status() {
    let cls = class_prog();
    let prog = common::program(&hold_daily(), "hold_v2");
    let dir = norgate_fixture();
    let ingested = norgate_daily(&dir, &cls).unwrap();
    let ds = &ingested.dataset;
    assert!(ingested.source.contains("norgate"));
    assert_eq!(ds.securities.len(), 2);
    assert_eq!(ds.facts["close"].len(), 20);
    assert_eq!(ds.facts["split"].len(), 1);
    assert_eq!(ds.facts["dividend"].len(), 1);
    // Delisted from day 8 through day 10; out of the listing from then on.
    assert_eq!(ds.facts["delisted"].len(), 3);
    assert_eq!(ds.facts["listed"].len(), 10 + 7);
    // Membership expanded over trading days: X all ten, Y the first seven.
    assert_eq!(ds.facts["member"].len(), 17);
    assert_eq!(ds.facts["classification"].len(), 10);
    assert!(ds.facts.contains_key("ticker"), "the ticker relation is derived from the symbol history");
    // Equities are the identifiers, not the symbols.
    let e2 = ds.symbols.names().iter().position(|n| n == "E2").expect("E2 interned") as u32;
    assert!(ds.facts["split"][0][0] == Value::Equity(e2));
    // The dataset runs, writes a bundle and loads back.
    let r = run(&prog, ds, ExecConfig::frictionless()).unwrap();
    assert!(r.actions.iter().any(|a| matches!(a.action, abt_kernel::kernel::Action::Delisting { .. })));
    let out = dir.join("bundle");
    write_bundle(&cls, ds, &out, "equity", "2026.10").unwrap();
    let (loaded, _) = load_bundle(&cls, &out, true).unwrap();
    assert_eq!(loaded.facts["member"].len(), 17);
    // A symbol no identifier carries is an error, not a silent new equity.
    write_text_table(
        &dir.join("prices.parquet"),
        &format!("symbol,date,open,high,low,close,volume\nZ,{},1,1,1,1,1\n", format_timestamp(parse_timestamp("2024-01-08").unwrap())),
    )
    .unwrap();
    assert!(norgate_daily(&dir, &cls).err().unwrap().contains("symbol history"));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn the_bundle_builder_takes_a_norgate_export_and_records_its_source() {
    let dir = norgate_fixture();
    let out = dir.join("bundle");
    let from = dir.to_string_lossy().into_owned();
    let o = out.to_string_lossy().into_owned();
    let (code, stdout, stderr) = abt(&["bundle", "build", "--from-norgate", &from, "--class", "equity", "--version", "2026.10", "--out", &o, CATALOG, LIB, TRM]);
    assert_eq!(code, 0, "{}\n{}", stdout, stderr);
    assert!(stderr.contains("note: 20 price rows"), "{}", stderr);
    assert!(stdout.contains("wrote bundle `equity@2026.10`"), "{}", stdout);
    let m = read_manifest(&out).unwrap();
    assert!(m.source.as_deref().unwrap_or("").contains("norgate"), "{:?}", m.source);
    assert_eq!(m.processing_delay_seconds, 0);
    let _ = fs::remove_dir_all(&dir);
}

fn databento_fixture(with_recv: bool) -> PathBuf {
    let dir = temp_dir(if with_recv { "databento-recv" } else { "databento" });
    let mut rows = String::from(if with_recv {
        "ts_event,ts_recv,instrument_id,open,high,low,close,volume\n"
    } else {
        "ts_event,instrument_id,open,high,low,close,volume\n"
    });
    for m in 0..5 {
        for (id, px) in [("1001", 100.0), ("1002", 50.0)] {
            let event = format!("2024-01-02T09:{:02}:00", 30 + m);
            // The second bar of 1002 arrives a minute late.
            let recv = if id == "1002" && m == 1 {
                format!("2024-01-02T09:{:02}:30", 32)
            } else {
                format!("2024-01-02T09:{:02}:02", 31 + m)
            };
            if with_recv {
                rows.push_str(&format!("{},{},{},{px},{px},{px},{},{}\n", event, recv, id, px + m as f64, 1000 + m));
            } else {
                rows.push_str(&format!("{},{},{px},{px},{px},{},{}\n", event, id, px + m as f64, 1000 + m));
            }
        }
    }
    write_text_table(&dir.join("ohlcv-1m.parquet"), &rows).unwrap();
    write_text_table(&dir.join("symbology.parquet"), "id,symbol,from,to\nE1,1001,2020-01-01,\nE2,1002,2020-01-01,\n").unwrap();
    dir
}

/// The minute export becomes the equity class's bars at @1m (`close`,
/// `volume`, the listing), keyed at the bar close, with the availability
/// the export records.
#[test]
fn a_databento_export_is_keyed_at_the_bar_close_with_recorded_availability() {
    let prog = common::program(&hold_minute(), "minute_hold");
    let dir = databento_fixture(true);
    let ingested = databento_minute(&dir, &prog, 5).unwrap();
    let ds = &ingested.dataset;
    assert!(ingested.source.contains("consolidated"));
    assert!(ds.facts.contains_key("close"), "the class's bars are `close` and `volume`: {:?}", ds.facts.keys().collect::<Vec<_>>());
    assert_eq!(ds.facts["close"].len(), 10);
    assert_eq!(ds.securities.len(), 2);
    // The first bar's event is 09:30: its key is the close at 09:31.
    let first = ds.facts["close"][0][1].as_time().unwrap();
    assert_eq!(format_timestamp(first), "2024-01-02T09:31:00");
    assert!(ds.availability_of("close").is_some());
    let avail = ds.availability_of("close").unwrap();
    // Received at 09:31:02 plus five seconds of processing.
    assert_eq!(format_timestamp(avail[0]), "2024-01-02T09:31:07");
    // The late bar of 1002 (key 09:32) is available at 09:32:35.
    let late = ds.facts["close"]
        .iter()
        .enumerate()
        .find(|(_, tu)| format_timestamp(tu[1].as_time().unwrap()) == "2024-01-02T09:32:00" && tu[0] == Value::Equity(1))
        .map(|(i, _)| i)
        .unwrap();
    assert_eq!(format_timestamp(avail[late]), "2024-01-02T09:32:35");
    let r = run(&prog, ds, ExecConfig::frictionless()).unwrap();
    assert!(!r.fills.is_empty());
    // Without ts_recv, availability is the bar close plus the delay, or
    // plain bar close at zero delay.
    let plain = databento_fixture(false);
    let i0 = databento_minute(&plain, &prog, 0).unwrap();
    assert!(i0.dataset.availability_of("close").is_none());
    let i5 = databento_minute(&plain, &prog, 5).unwrap();
    assert_eq!(format_timestamp(i5.dataset.availability_of("close").unwrap()[0]), "2024-01-02T09:31:05");
    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&plain);
}

/// `abt bundle build --from-databento` with a processing delay: the
/// manifest records the delay, the source and the recorded availability of
/// the minute bars.
#[test]
fn the_bundle_builder_takes_a_databento_export_with_a_processing_delay() {
    let dir = databento_fixture(true);
    let strategy = dir.join("minute_hold.dsl");
    fs::write(&strategy, hold_minute()).unwrap();
    let strategy = strategy.to_string_lossy().into_owned();
    let out = dir.join("bundle");
    let from = dir.to_string_lossy().into_owned();
    let o = out.to_string_lossy().into_owned();
    let (code, stdout, stderr) = abt(&[
        "bundle",
        "build",
        "--from-databento",
        &from,
        "--processing-delay",
        "5",
        "--class",
        "equity",
        "--version",
        "2026.10",
        "--out",
        &o,
        CATALOG,
        LIB,
        &strategy,
    ]);
    assert_eq!(code, 0, "{}\n{}", stdout, stderr);
    let m = read_manifest(&out).unwrap();
    assert_eq!(m.processing_delay_seconds, 5);
    assert!(m.source.as_deref().unwrap_or("").contains("ts_recv"), "{:?}", m.source);
    assert!(
        m.relations.iter().any(|r| r.name == "close" && r.availability == "recorded"),
        "{:?}",
        m.relations.iter().map(|r| (&r.name, &r.availability)).collect::<Vec<_>>()
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_malformed_processing_delay_is_refused() {
    let dir = databento_fixture(true);
    let from = dir.to_string_lossy().into_owned();
    let o = dir.join("bundle").to_string_lossy().into_owned();
    let (code, _, stderr) = abt(&[
        "bundle",
        "build",
        "--from-databento",
        &from,
        "--processing-delay",
        "soon",
        "--class",
        "equity",
        "--version",
        "1",
        "--out",
        &o,
        CATALOG,
        LIB,
        TRM,
    ]);
    assert_eq!(code, 2);
    assert!(stderr.contains("--processing-delay") && stderr.contains("soon"), "{}", stderr);
    let _ = fs::remove_dir_all(&dir);
}

/// Each adapter produces one base: the minute adapter refuses a daily
/// program and the daily adapter a minute one, naming the base it reads.
#[test]
fn an_adapter_refuses_a_program_of_another_base() {
    let daily = common::program(&hold_daily(), "hold_v2");
    let minute = common::program(&hold_minute(), "minute_hold");
    let dir = databento_fixture(false);
    let err = databento_minute(&dir, &daily, 0).err().unwrap();
    assert!(err.contains("minute") && err.contains("@1d"), "{}", err);
    let _ = fs::remove_dir_all(&dir);
    let dir = norgate_fixture();
    let err = norgate_daily(&dir, &minute).err().unwrap();
    assert!(err.contains("daily") && err.contains("@1m"), "{}", err);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn reviewed_exceptions_travel_from_the_export_into_the_bundle_by_identifier() {
    let dir = norgate_fixture();
    let days = abt_kernel::data::business_days((2024, 1, 8), 10);
    write_text_table(
        &dir.join("exceptions.parquet"),
        &format!("test,symbol,date,reason\naction reconciliation,Y,{},real move: reviewed\n", format_timestamp(days[2])),
    )
    .unwrap();
    let cls = class_prog();
    let ingested = norgate_daily(&dir, &cls).unwrap();
    assert_eq!(ingested.exceptions.len(), 1);
    assert_eq!(ingested.exceptions[0].security, "E2", "the ticker maps to its identifier on the date");
    let out = dir.join("bundle");
    let o = out.to_string_lossy().into_owned();
    let from = dir.to_string_lossy().into_owned();
    let (code, stdout, stderr) = abt(&["bundle", "build", "--from-norgate", &from, "--class", "equity", "--version", "2026.10", "--out", &o, CATALOG, LIB, TRM]);
    assert_eq!(code, 0, "{}\n{}", stdout, stderr);
    let written = abt_kernel::bundle::read_exceptions(&out).unwrap();
    assert_eq!(written, ingested.exceptions);
    let _ = fs::remove_dir_all(&dir);
}
