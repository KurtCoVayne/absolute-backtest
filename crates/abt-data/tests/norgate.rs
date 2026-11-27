//! The Norgate adapter (D-7.7) on a fixture lake in NDLake's layout, its
//! decisions (docs/handoff-data.md, section 4) checked one by one: the
//! point-in-time universe, the latest ingest of a duplicated bar, an open
//! filled from the close, a ticker change as two spans, a collision
//! renamed to the source symbol, a split with a same-day dividend rescaled,
//! a bankruptcy and an acquisition inferred, membership made inclusive and
//! split around a halt, the classification snapshot from its date, a
//! reviewed exception kept as an attribute; then the rows ingested into a
//! store under the registry's ids.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use abt_data::adapters::norgate::{days_of, read_lake, LakeOptions};
use abt_data::Store;
use arrow::array::{ArrayRef, AsArray, Date32Array, Float64Array, Int64Array, RecordBatch, StringArray, TimestampNanosecondArray};
use arrow::datatypes::{Field, Schema};
use parquet::arrow::ArrowWriter;

const DAY: i64 = 86_400_000_000_000;

fn d(k: i64) -> i32 {
    (days_of("2014-01-02").unwrap() + k) as i32
}

fn write(path: &Path, cols: Vec<(&str, ArrayRef)>) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let schema = Arc::new(Schema::new(cols.iter().map(|(n, a)| Field::new(*n, a.data_type().clone(), true)).collect::<Vec<_>>()));
    let batch = RecordBatch::try_new(schema.clone(), cols.into_iter().map(|(_, a)| a).collect()).unwrap();
    let mut w = ArrowWriter::try_new(std::fs::File::create(path).unwrap(), schema, None).unwrap();
    w.write(&batch).unwrap();
    w.close().unwrap();
}

fn s(v: Vec<&str>) -> ArrayRef {
    Arc::new(StringArray::from(v))
}

fn dates(v: Vec<Option<i32>>) -> ArrayRef {
    Arc::new(Date32Array::from(v))
}

/// One bar of the lake: asset, ticker, source symbol, day, close (and the
/// open, which a NaN marks missing), series type, ingest stamp.
struct B(i64, &'static str, &'static str, i64, f64, f64, &'static str, i64);

fn lake() -> PathBuf {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("abt-lake-{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&dir);
    // Members: 1, 2, 3, 4, 7 of the S&P 500 over 2014; 8 of another index;
    // 9 left before the span.
    let m: Vec<(&str, i64, i32, Option<i32>)> = vec![
        ("S&P 500", 1, d(-200), None),
        ("S&P 500", 2, d(-200), Some(d(15))),
        ("S&P 500", 3, d(-200), None),
        ("S&P 500", 4, d(-200), None),
        ("S&P 500", 7, d(-200), None),
        ("Russell 2000", 8, d(-200), None),
        ("S&P 500", 9, d(-400), Some(d(-300))),
    ];
    write(
        &dir.join("indexes/index_membership.parquet"),
        vec![
            ("index_name", s(m.iter().map(|r| r.0).collect())),
            ("asset_id", Arc::new(Int64Array::from(m.iter().map(|r| r.1).collect::<Vec<_>>()))),
            ("member_from", dates(m.iter().map(|r| Some(r.2)).collect())),
            ("member_to", dates(m.iter().map(|r| r.3).collect())),
        ],
    );
    let mut bars: Vec<B> = Vec::new();
    for k in 0..20 {
        // 1: AAA throughout, a 2:1 split on day 10 (the price halves).
        let close = if k < 10 { 100.0 } else { 50.0 };
        bars.push(B(1, "AAA", "AAA", k, close, if k == 3 { f64::NAN } else { close }, "equity", 2));
        // 2: BBB, then BBX from day 10; no bar on day 7 (a halt).
        if k != 7 {
            bars.push(B(2, if k < 10 { "BBB" } else { "BBX" }, "BBB", k, 20.0, 20.0, "equity", 1));
        }
        // 3: CCCQ until day 12, closing at 0.5: a bankruptcy.
        if k <= 12 {
            bars.push(B(3, "CCCQ", "CCCQ", k, if k == 12 { 0.5 } else { 5.0 }, 5.0, "equity", 1));
        }
        // 4: DDD until day 15 at a steady price: an acquisition; 7 takes
        // the ticker DDD from day 12, so 4 (ending first) is renamed.
        if k <= 15 {
            bars.push(B(4, "DDD", "DDD-201401", k, 30.0, 30.0, "equity", 1));
        }
        if k >= 12 {
            bars.push(B(7, "DDD", "DDD", k, 40.0, 40.0, "equity", 1));
        }
        bars.push(B(8, "OTH", "OTH", k, 10.0, 10.0, "equity", 1));
        // The total-return index, an index series.
        bars.push(B(100, "$SPXTR", "$SPXTR", k, 1000.0 + k as f64, 1000.0, "index", 2));
    }
    // An earlier ingest of the index's day 4 (dropped).
    bars.push(B(100, "$SPXTR", "$SPXTR", 4, 1.0, 1.0, "index", 1));
    // A duplicate of 1's day 5 from an earlier ingest (dropped), an index
    // row of 1 (not an equity), a bar of 2 without a close (dropped).
    bars.push(B(1, "AAA", "AAA", 5, 999.0, 999.0, "equity", 1));
    bars.push(B(1, "AAA", "AAA", 6, 1.0, 1.0, "index", 3));
    bars.push(B(2, "BBB", "BBB", 7, f64::NAN, 20.0, "equity", 1));
    let nan_if = |x: f64| if x.is_nan() { None } else { Some(x) };
    write(
        &dir.join("curated/bars_by_date/year=2014/data.parquet"),
        vec![
            ("asset_id", Arc::new(Int64Array::from(bars.iter().map(|b| b.0).collect::<Vec<_>>()))),
            ("ticker", s(bars.iter().map(|b| b.1).collect())),
            ("source_symbol", s(bars.iter().map(|b| b.2).collect())),
            ("date", dates(bars.iter().map(|b| Some(d(b.3))).collect())),
            ("open", Arc::new(Float64Array::from(bars.iter().map(|b| nan_if(b.5)).collect::<Vec<_>>()))),
            ("high", Arc::new(Float64Array::from(bars.iter().map(|b| nan_if(b.4)).collect::<Vec<_>>()))),
            ("low", Arc::new(Float64Array::from(bars.iter().map(|b| nan_if(b.4)).collect::<Vec<_>>()))),
            ("close", Arc::new(Float64Array::from(bars.iter().map(|b| nan_if(b.4)).collect::<Vec<_>>()))),
            ("volume", Arc::new(Float64Array::from(bars.iter().map(|_| Some(1000.0)).collect::<Vec<_>>()))),
            ("ingest_ts", Arc::new(TimestampNanosecondArray::from(bars.iter().map(|b| b.7 * DAY).collect::<Vec<_>>()))),
            ("series_type", s(bars.iter().map(|b| b.6).collect())),
        ],
    );
    // A 2:1 split of 1 on day 10 with a dividend of 0.5 per old share; a
    // dividend of 3 after its delisting, when no symbol carries it.
    write(
        &dir.join("registry/corporate_actions/data.parquet"),
        vec![
            ("asset_id", Arc::new(Int64Array::from(vec![1, 1, 3]))),
            ("action_type", s(vec!["SPLIT", "DIV", "DIV"])),
            ("ex_date", dates(vec![Some(d(10)), Some(d(10)), Some(d(15))])),
            ("div_cash", Arc::new(Float64Array::from(vec![None, Some(0.5), Some(0.3)]))),
            ("split_ratio", Arc::new(Float64Array::from(vec![Some(2.0), None, None]))),
        ],
    );
    write(
        &dir.join("classification/classification_full/data.parquet"),
        vec![
            ("asset_id", Arc::new(Int64Array::from(vec![1, 1]))),
            ("scheme", s(vec!["GICS", "GICS"])),
            ("level", Arc::new(Int64Array::from(vec![1, 2]))),
            ("code", s(vec!["45", "4510"])),
            ("snapshot_date", dates(vec![Some(d(12)), Some(d(12))])),
        ],
    );
    write(
        &dir.join("reviewed.parquet"),
        vec![
            ("test", s(vec!["action reconciliation", "action reconciliation"])),
            ("symbol", s(vec!["AAA", "ZZZ"])),
            ("date", dates(vec![Some(d(10)), Some(d(10))])),
            ("reason", s(vec!["the split", "not a symbol here"])),
        ],
    );
    dir
}

fn relation<'a>(f: &'a abt_data::adapters::Fetched, name: &str) -> &'a RecordBatch {
    &f.relations.iter().find(|(r, _)| r == name).unwrap_or_else(|| panic!("no {}", name)).1
}

/// The (key, day) rows of a relation.
fn rows(b: &RecordBatch) -> Vec<(String, i64)> {
    let k = b.column(0).as_string::<i32>();
    let t = b.column(1).as_primitive::<arrow::datatypes::TimestampNanosecondType>();
    (0..b.num_rows()).map(|i| (k.value(i).to_string(), t.value(i) / DAY - days_of("2014-01-02").unwrap())).collect()
}

fn of(b: &RecordBatch, key: &str) -> Vec<i64> {
    rows(b).into_iter().filter(|(k, _)| k == key).map(|(_, d)| d).collect()
}

/// Two lists: the universe is the members of either, each a member of its
/// own; an index series read under its name.
#[test]
fn several_lists_and_an_index_series() {
    let dir = lake();
    let opts = LakeOptions {
        lists: vec![("S&P 500".into(), "SPX".into()), ("Russell 2000".into(), "R2K".into())],
        series: vec![("$SPXTR".into(), "SPXTR".into())],
        ..LakeOptions::default()
    };
    let (f, report) = read_lake(&dir, &opts).unwrap();
    assert_eq!(report.universe_assets, 6);
    let member = relation(&f, "equity.member");
    let lists = member.column(2).as_string::<i32>();
    let list_of = |key: &str| -> Vec<String> {
        let mut v: Vec<String> = (0..member.num_rows()).filter(|&i| rows(member)[i].0 == key).map(|i| lists.value(i).to_string()).collect();
        v.dedup();
        v
    };
    assert_eq!(list_of("8"), vec!["R2K"]);
    assert_eq!(list_of("1"), vec!["SPX"]);
    let series = relation(&f, "series.value");
    assert_eq!(series.num_rows(), 20);
    assert_eq!(report.series_rows["SPXTR"], 20);
    let names = series.column(0).as_string::<i32>();
    let values = series.column(2).as_primitive::<arrow::datatypes::Float64Type>();
    assert!((0..20).all(|i| names.value(i) == "SPXTR"));
    assert_eq!(values.value(4), 1004.0, "the later ingest of day 4");
    // An index is not an equity of the universe.
    assert!(f.securities.iter().all(|s| s.key != "100"));
    let mut store = Store::temporary("norgate-lists").unwrap();
    let got = f.ingest(&mut store, "norgate", "norgate", &BTreeMap::new()).unwrap();
    assert_eq!(store.read_all(&got.versions["series.value"]).unwrap().num_rows(), 20);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A named ticker beyond the lists (E-100): asset 8 (OTH, a Russell 2000
/// member only) joins the universe as a member of `ETF` over its listing.
#[test]
fn a_named_ticker_joins_the_universe_under_its_label() {
    let dir = lake();
    write(
        &dir.join("registry/security_master/data.parquet"),
        vec![
            ("asset_id", Arc::new(Int64Array::from(vec![8, 9]))),
            ("ticker", s(vec!["OTH", "OLD"])),
            ("listing_date", dates(vec![Some(d(-500)), Some(d(-500))])),
            ("delisting_date", dates(vec![None, Some(d(-300))])),
        ],
    );
    let opts = LakeOptions {
        tickers: vec![("oth".into(), "ETF".into()), ("OLD".into(), "ETF".into())],
        ..LakeOptions::default()
    };
    let (f, _) = read_lake(&dir, &opts).unwrap();
    let member = relation(&f, "equity.member");
    let lists = member.column(2).as_string::<i32>();
    let of8: Vec<&str> = (0..member.num_rows()).filter(|&i| rows(member)[i].0 == "8").map(|i| lists.value(i)).collect();
    assert_eq!(of8.len(), 20, "a member every day of the span");
    assert!(of8.iter().all(|l| *l == "ETF"));
    // A ticker delisted before the span stays out.
    assert!(f.securities.iter().all(|s| s.key != "9"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// The adapter as a source of the resolver: a catalog's settings
/// configure it, and its offers are its lists' and series'.
#[test]
fn the_adapter_is_configured_by_its_settings() {
    use abt_data::adapters::norgate::Norgate;
    use abt_data::adapters::{Adapter, FetchRequest};
    let dir = lake();
    let settings: BTreeMap<String, String> = [
        ("lake", dir.to_string_lossy().as_ref()),
        ("lists", "S&P 500=SPX; Russell 2000=R2K"),
        ("series", "$SPXTR=SPXTR"),
        ("from", "2014-01-01"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    let n = Norgate::from_settings(&settings).unwrap();
    let offers = n.offers();
    assert_eq!(offers.len(), 2);
    assert_eq!(offers[0].scope, "list(SPX, R2K)");
    assert_eq!(offers[1].class, "series");
    let req = FetchRequest {
        source: "norgate".into(),
        class: "equity".into(),
        relations: vec!["equity.close".into(), "equity.split".into()],
        calendar: "1d".into(),
        from: days_of("2014-01-02").unwrap() * DAY,
        to: (days_of("2014-01-02").unwrap() + 9) * DAY,
        scope: "list(SPX, R2K)".into(),
    };
    let got = n.fetch(&req).unwrap();
    let names: Vec<&str> = got.relations.iter().map(|(r, _)| r.as_str()).collect();
    assert_eq!(names, vec!["equity.close", "equity.split"]);
    assert!(rows(relation(&got, "equity.close")).iter().all(|(_, d)| *d <= 9));
    assert!(Norgate::from_settings(&[("lake".to_string(), "x".to_string()), ("colour".to_string(), "red".to_string())].into_iter().collect()).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_lake_becomes_the_equity_class_with_its_decisions() {
    let dir = lake();
    let opts = LakeOptions {
        from: days_of("2014-01-01").unwrap(),
        exceptions: Some(dir.join("reviewed.parquet")),
        ..LakeOptions::default()
    };
    let (f, report) = read_lake(&dir, &opts).unwrap();
    // The universe: ever a member over the span; 8 and 9 are not.
    assert_eq!(report.universe_assets, 5);
    assert!(f.relations.iter().all(|(_, b)| rows(b).iter().all(|(k, _)| k != "8" && k != "9")));
    // The duplicate dropped (the later ingest kept), the bar without a
    // close dropped, the missing open filled.
    assert_eq!(report.drops["duplicate_asset_date"], 1);
    assert_eq!(report.drops["missing_or_nonpositive_close"], 1);
    assert_eq!(report.filled["open"], 1);
    let close = relation(&f, "equity.close");
    let price = close.column(2).as_primitive::<arrow::datatypes::Float64Type>();
    let day5 = rows(close).iter().position(|(k, d)| k == "1" && *d == 5).unwrap();
    assert_eq!(price.value(day5), 100.0, "the later ingest of day 5");
    let open = relation(&f, "equity.open");
    let day3 = rows(open).iter().position(|(k, d)| k == "1" && *d == 3).unwrap();
    assert_eq!(open.column(2).as_primitive::<arrow::datatypes::Float64Type>().value(day3), 100.0, "the open is the close");
    // Ticker spans: 2 changed from BBB to BBX; 4 ends first of the two
    // DDDs and takes its source symbol.
    let spans: Vec<(String, String, i64, Option<i64>)> = f
        .securities
        .iter()
        .map(|s| {
            (
                s.key.clone(),
                s.ticker.clone(),
                s.from / DAY - days_of("2014-01-02").unwrap(),
                s.to.map(|t| t / DAY - days_of("2014-01-02").unwrap()),
            )
        })
        .collect();
    assert!(spans.contains(&("2".into(), "BBB".into(), 0, Some(9))), "{:?}", spans);
    assert!(spans.contains(&("2".into(), "BBX".into(), 10, None)), "{:?}", spans);
    assert!(spans.contains(&("4".into(), "DDD-201401".into(), 0, Some(16))), "{:?}", spans);
    assert!(spans.contains(&("7".into(), "DDD".into(), 12, None)), "{:?}", spans);
    assert_eq!(report.ticker_collisions_renamed, 1);
    // The split, and the same-day dividend per new share.
    assert_eq!(of(relation(&f, "equity.split"), "1"), vec![10]);
    let div = relation(&f, "equity.dividend");
    assert_eq!(rows(div), vec![("1".to_string(), 10)]);
    assert_eq!(div.column(4).as_primitive::<arrow::datatypes::Float64Type>().value(0), 0.25);
    assert_eq!(report.dividends_rescaled_for_same_day_split, 1);
    assert_eq!(report.drops["dividend_outside_symbol_span_or_nonpositive"], 1);
    // Delistings: the first day after the last print, every day after it.
    let delisted = relation(&f, "equity.delisted");
    assert_eq!(of(delisted, "3"), (13..20).collect::<Vec<_>>());
    assert_eq!(of(delisted, "4"), (16..20).collect::<Vec<_>>());
    let reasons = delisted.column(2).as_string::<i32>();
    let reason_of = |key: &str| (0..delisted.num_rows()).find(|&i| rows(delisted)[i].0 == key).map(|i| reasons.value(i).to_string());
    assert_eq!(reason_of("3").as_deref(), Some("bankruptcy"));
    assert_eq!(reason_of("4").as_deref(), Some("acquisition"));
    assert!(of(relation(&f, "equity.listed"), "3").iter().all(|d| *d <= 12));
    // Membership: 2 to the day before its member_to, split around its halt.
    assert_eq!(of(relation(&f, "equity.member"), "2"), (0..15).filter(|k| *k != 7).collect::<Vec<_>>());
    assert_eq!(report.membership_intervals_split_at_missing_bars, 1);
    // Classification: level 1 from the snapshot on.
    let cls = relation(&f, "equity.classification");
    assert_eq!(of(cls, "1"), (12..20).collect::<Vec<_>>());
    assert_eq!(cls.column(3).as_string::<i32>().value(0), "45");
    assert_eq!(report.classification_snapshot.as_deref(), Some("2014-01-14"));
    // The deeper levels of the same snapshot over the same days (E-98): the
    // fixture has the industry group, not the industry or sub-industry.
    let ind = relation(&f, "equity.industry");
    assert_eq!(of(ind, "1"), (12..20).collect::<Vec<_>>());
    assert_eq!(ind.column(2).as_string::<i32>().value(0), "4510");
    assert!(ind.column(3).is_null(0) && ind.column(4).is_null(0));
    // The reviewed exception that names an exported symbol is kept.
    assert_eq!(report.exceptions, 1);
    assert!(f.attributes["reviewed_exceptions"].contains("\"key\":\"1\""), "{}", f.attributes["reviewed_exceptions"]);
    assert_eq!(f.relation_attributes["equity.dividend"]["dividend_dates"], "ex_only");
    assert_eq!(f.relation_attributes["equity.delisted"]["delisting_reason"], "inferred");
    // Ingested: the registry's ids for the keys, the tickers by span.
    let mut store = Store::temporary("norgate").unwrap();
    let got = f.ingest(&mut store, "norgate", "norgate", &BTreeMap::new()).unwrap();
    let v = store.registry.current("equity.close", Some("norgate")).unwrap().to_string();
    assert_eq!(v, got.versions["equity.close"]);
    assert_eq!(store.read_all(&v).unwrap().num_rows(), close.num_rows());
    let dv = store.registry.current("equity.dividend", Some("norgate")).unwrap().to_string();
    let attrs = &store.registry.version(&dv).unwrap().attributes;
    assert_eq!(attrs["dividend_dates"], "ex_only");
    assert_eq!(attrs["ticker_history"], "final");
    assert!(!store.registry.version(&v).unwrap().attributes.contains_key("dividend_dates"));
    let _ = std::fs::remove_dir_all(&dir);
}

// ------------------------------------------------------------ a lake of two spans (E-149)

/// The days since 1970-01-01 of a date.
fn on(s: &str) -> i64 {
    days_of(s).unwrap()
}

/// The trading days of the two-span lake: the last seven of 2014 and the
/// first six of 2015. The week of 2014-12-29 straddles the spans.
const SESSIONS: [&str; 13] = [
    "2014-12-22",
    "2014-12-23",
    "2014-12-24",
    "2014-12-26",
    "2014-12-29",
    "2014-12-30",
    "2014-12-31",
    "2015-01-02",
    "2015-01-05",
    "2015-01-06",
    "2015-01-07",
    "2015-01-08",
    "2015-01-09",
];

/// A close the lake holds: a single-precision value widened, as NDLake's
/// prices are (every one an exact `f32`, most off any decimal grid as an
/// `f64`).
fn lake_close(asset: i64, k: usize) -> f64 {
    (asset as f32 + 0.013 + 0.07 * k as f32) as f64
}

/// The S&P 500 members and their spells (`member_to` the first trading
/// day out, as the lake's builder writes it):
/// - 11 joins on 2015-01-05 and trades in both spans;
/// - 12 leaves on 2014-12-24 and trades on through 2015;
/// - 13 is a member throughout and prints last on 2014-12-31, the first
///   span's last trading day;
/// - 14 is a member throughout and trades throughout, with a 2:1 split and
///   a dividend on 2015-01-06;
/// - 15 leaves on Wednesday 2015-01-07 and trades through the week;
/// - 16 leaves on Monday 2015-01-05, its last print.
fn two_span_lake() -> PathBuf {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("abt-lake2-{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&dir);
    let m: Vec<(i64, &str, Option<&str>)> = vec![
        (11, "2015-01-05", None),
        (12, "2013-01-02", Some("2014-12-24")),
        (13, "2013-01-02", Some("2015-01-02")),
        (14, "2013-01-02", None),
        (15, "2013-01-02", Some("2015-01-07")),
        (16, "2013-01-02", Some("2015-01-05")),
    ];
    let date = |s: &str| Some(on(s) as i32);
    write(
        &dir.join("indexes/index_membership.parquet"),
        vec![
            ("index_name", s(m.iter().map(|_| "S&P 500").collect())),
            ("asset_id", Arc::new(Int64Array::from(m.iter().map(|r| r.0).collect::<Vec<_>>()))),
            ("member_from", dates(m.iter().map(|r| date(r.1)).collect())),
            ("member_to", dates(m.iter().map(|r| r.2.and_then(date)).collect())),
        ],
    );
    for year in ["2014", "2015"] {
        let mut rows: Vec<(i64, String, i64, f64)> = Vec::new();
        for (k, day) in SESSIONS.iter().enumerate().filter(|(_, d)| d.starts_with(year)) {
            for asset in 11..=16 {
                let last = match asset {
                    13 => "2014-12-31",
                    16 => "2015-01-05",
                    _ => "9999-12-31",
                };
                if *day <= last {
                    rows.push((asset, format!("T{}", asset), on(day), lake_close(asset, k)));
                }
            }
        }
        let closes: Vec<Option<f64>> = rows.iter().map(|r| Some(r.3)).collect();
        write(
            &dir.join(format!("curated/bars_by_date/year={}/data.parquet", year)),
            vec![
                ("asset_id", Arc::new(Int64Array::from(rows.iter().map(|r| r.0).collect::<Vec<_>>()))),
                ("ticker", s(rows.iter().map(|r| r.1.as_str()).collect())),
                ("source_symbol", s(rows.iter().map(|r| r.1.as_str()).collect())),
                ("date", dates(rows.iter().map(|r| Some(r.2 as i32)).collect())),
                ("open", Arc::new(Float64Array::from(closes.clone()))),
                ("high", Arc::new(Float64Array::from(closes.clone()))),
                ("low", Arc::new(Float64Array::from(closes.clone()))),
                ("close", Arc::new(Float64Array::from(closes))),
                ("volume", Arc::new(Float64Array::from(rows.iter().map(|_| Some(1000.0)).collect::<Vec<_>>()))),
                ("ingest_ts", Arc::new(TimestampNanosecondArray::from(rows.iter().map(|_| DAY).collect::<Vec<_>>()))),
                ("series_type", s(rows.iter().map(|_| "equity").collect())),
            ],
        );
    }
    // 14's split and its same-day dividend; 11's dividend, an `f32` amount.
    write(
        &dir.join("registry/corporate_actions/data.parquet"),
        vec![
            ("asset_id", Arc::new(Int64Array::from(vec![14, 14, 11]))),
            ("action_type", s(vec!["SPLIT", "DIV", "DIV"])),
            ("ex_date", dates(vec![date("2015-01-06"), date("2015-01-06"), date("2014-12-26")])),
            ("div_cash", Arc::new(Float64Array::from(vec![None, Some(0.3f32 as f64), Some(0.11f32 as f64)]))),
            ("split_ratio", Arc::new(Float64Array::from(vec![Some(2.0), None, None]))),
        ],
    );
    dir
}

/// The options of one span of the staging; `window`: the universe window
/// names both spans, as `scripts/bench-mw14.sh` passes it.
fn span(from: &str, to: &str, window: bool) -> LakeOptions {
    LakeOptions {
        from: on(from),
        to: Some(on(to)),
        universe_from: window.then(|| on("2014-01-01")),
        universe_to: window.then(|| on("2015-12-31")),
        ..LakeOptions::default()
    }
}

fn keys(b: &RecordBatch) -> std::collections::BTreeSet<String> {
    rows(b).into_iter().map(|(k, _)| k).collect()
}

/// The dates of a key's rows.
fn dates_of(b: &RecordBatch, key: &str) -> Vec<String> {
    let k = b.column(0).as_string::<i32>();
    let t = b.column(1).as_primitive::<arrow::datatypes::TimestampNanosecondType>();
    (0..b.num_rows())
        .filter(|&i| k.value(i) == key)
        .map(|i| {
            let (y, m, dd) = abt_wire::scalar::civil_from_days(t.value(i) / DAY);
            format!("{:04}-{:02}-{:02}", y, m, dd)
        })
        .collect()
}

/// D4 of the Nov 25 parity measurements: a span keeps an asset only where
/// one of its spells overlaps the span, so a staging in spans lacks the
/// bars of a span in which the asset is not a member (the lake's 4,437,783
/// bars from 1990 of 2,303 assets). With the universe window every span
/// holds every bar of every asset ever a member; membership is unchanged.
#[test]
fn a_staging_in_spans_holds_every_asset_of_the_universe_window_in_every_span() {
    let dir = two_span_lake();
    let all: std::collections::BTreeSet<String> = (11..=16).map(|a| a.to_string()).collect();
    // Without the window: 11 (a member from 2015) has no 2014 bar, 12 (gone
    // from the index in 2014) no 2015 bar.
    let (f1, _) = read_lake(&dir, &span("2014-01-01", "2014-12-31", false)).unwrap();
    let (f2, _) = read_lake(&dir, &span("2015-01-01", "2015-12-31", false)).unwrap();
    assert!(!keys(relation(&f1, "equity.close")).contains("11"));
    assert!(!keys(relation(&f2, "equity.close")).contains("12"));
    // With it, both spans hold every asset that trades in them.
    let (w1, r1) = read_lake(&dir, &span("2014-01-01", "2014-12-31", true)).unwrap();
    let (w2, r2) = read_lake(&dir, &span("2015-01-01", "2015-12-31", true)).unwrap();
    assert_eq!((r1.universe_assets, r2.universe_assets), (6, 6));
    assert_eq!(keys(relation(&w1, "equity.close")), all);
    assert_eq!(dates_of(relation(&w1, "equity.close"), "11"), SESSIONS[..7].to_vec());
    assert_eq!(dates_of(relation(&w2, "equity.close"), "12"), SESSIONS[7..].to_vec());
    // A bar of a span is a bar of the lake: the spans add up to its rows.
    let lake_bars = 6 * 7 + 4 * 6 + 2;
    assert_eq!(relation(&w1, "equity.close").num_rows() + relation(&w2, "equity.close").num_rows(), lake_bars);
    // Membership does not follow the window: 11 is no member in 2014, 12
    // none in 2015.
    assert!(dates_of(relation(&w1, "equity.member"), "11").is_empty());
    assert!(dates_of(relation(&w2, "equity.member"), "12").is_empty());
    assert_eq!(dates_of(relation(&w2, "equity.member"), "11"), SESSIONS[8..].to_vec());
    assert!(w1.attributes["universe"].contains("from 2014-01-01 to 2015-12-31"), "{}", w1.attributes["universe"]);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The delisting date is the first trading day after the last print
/// (decision 3); in a span followed by others, a last print on the span's
/// last trading day was taken for a current listing, and the next span,
/// holding no bar of the asset, emitted nothing: no delisting at all. The
/// lake's next year tells the two apart, and the delisting is that day's
/// row.
#[test]
fn an_asset_whose_last_print_is_a_spans_last_day_is_delisted_on_the_next_spans_first() {
    let dir = two_span_lake();
    let (w1, r1) = read_lake(&dir, &span("2014-01-01", "2014-12-31", true)).unwrap();
    let delisted = relation(&w1, "equity.delisted");
    assert_eq!(dates_of(delisted, "13"), vec!["2015-01-02"]);
    assert!(dates_of(delisted, "14").is_empty(), "14 trades on in 2015");
    assert_eq!(r1.delisting_reasons.values().sum::<usize>(), 1);
    let span13 = w1.securities.iter().find(|s| s.key == "13").unwrap();
    assert_eq!(span13.to, Some(on("2015-01-02") * DAY));
    assert!(w1.securities.iter().filter(|s| s.key == "14").all(|s| s.to.is_none()));
    // The rows before it are listed, as for a delisting inside a span.
    assert_eq!(dates_of(relation(&w1, "equity.listed"), "13"), SESSIONS[..7].to_vec());
    // Staged in its two spans, the store holds the delisting once.
    let mut store = Store::temporary("norgate-spans").unwrap();
    w1.ingest(&mut store, "norgate", "norgate", &BTreeMap::new()).unwrap();
    let (w2, _) = read_lake(&dir, &span("2015-01-01", "2015-12-31", true)).unwrap();
    let got = w2.ingest(&mut store, "norgate", "norgate", &BTreeMap::new()).unwrap();
    // 13's row, and 16's four from the day after its last print on 2015-01-05.
    assert_eq!(store.read_all(&got.versions["equity.delisted"]).unwrap().num_rows(), 1 + 4);
    // A span alone, or the last of the window, is read as before.
    let (f1, _) = read_lake(&dir, &span("2014-01-01", "2014-12-31", false)).unwrap();
    assert!(dates_of(relation(&f1, "equity.delisted"), "13").is_empty());
    assert!(f1.securities.iter().filter(|s| s.key == "13").all(|s| s.to.is_none()));
    let _ = std::fs::remove_dir_all(&dir);
}

/// D5: the lake's `member_to` is the first trading day out (its builder,
/// `pipeline/ndlake/membership.py`), and the adapter's member rows end the
/// day before. The Python book counts a name a member in a week when
/// `member_from <= its own last bar of the week <= member_to`, so it reads
/// `member_to` as a member day. With `member_to_inclusive` the store's row
/// at a name's last bar of each week is the book's test, asset-week by
/// asset-week; any member row in the week's bucket is not.
#[test]
fn member_to_counted_as_a_member_day_is_the_books_test_at_the_names_last_bar_of_the_week() {
    let dir = two_span_lake();
    let spells: BTreeMap<&str, (&str, &str)> = [
        ("12", ("2013-01-02", "2014-12-24")),
        ("13", ("2013-01-02", "2015-01-02")),
        ("15", ("2013-01-02", "2015-01-07")),
        ("16", ("2013-01-02", "2015-01-05")),
    ]
    .into_iter()
    .collect();
    let opts = |inclusive: bool| LakeOptions {
        member_to_inclusive: inclusive,
        ..span("2014-01-01", "2015-12-31", false)
    };
    let (half_open, _) = read_lake(&dir, &opts(false)).unwrap();
    let (inclusive, _) = read_lake(&dir, &opts(true)).unwrap();
    // 15 leaves on Wednesday 2015-01-07: a member through Tuesday, through
    // Wednesday under the book's reading.
    assert_eq!(dates_of(relation(&half_open, "equity.member"), "15"), SESSIONS[..10].to_vec());
    assert_eq!(dates_of(relation(&inclusive, "equity.member"), "15"), SESSIONS[..11].to_vec());
    assert_eq!(inclusive.relation_attributes["equity.member"]["member_to"], "last_day");
    assert!(!half_open.relation_attributes.get("equity.member").is_some_and(|a| a.contains_key("member_to")));
    // Each (asset, ISO week) of the closed spells: the book's test, the
    // store's row at the name's last bar, and any row in the week.
    let week_of = |d: &str| (on(d) - 4).div_euclid(7);
    for (key, (from, to)) in &spells {
        let bars = dates_of(relation(&inclusive, "equity.close"), key);
        let mut weeks: BTreeMap<i64, String> = BTreeMap::new();
        for b in &bars {
            weeks.insert(week_of(b), b.clone());
        }
        for (w, last) in &weeks {
            let book = from <= &last.as_str() && last.as_str() <= *to;
            let at_last = |f: &abt_data::adapters::Fetched| dates_of(relation(f, "equity.member"), key).contains(last);
            let any = dates_of(relation(&half_open, "equity.member"), key).iter().any(|d| week_of(d) == *w);
            assert_eq!(at_last(&inclusive), book, "{} in the week of {}", key, last);
            match (*key, last.as_str()) {
                // Out on Wednesday, trading to Friday: in the store's bucket,
                // not in the book (578 asset-weeks on Nov 25).
                ("15", "2015-01-09") => assert!(any && !book),
                // Out on Monday, its last print: in the book, not in the
                // half-open store (23 asset-weeks).
                ("16", "2015-01-05") => assert!(book && !at_last(&half_open) && !any),
                _ => assert_eq!(at_last(&half_open), book, "{} in the week of {}", key, last),
            }
        }
    }
    // A spell whose last day is the span's first: under the book's reading
    // 16 is a member on Monday 2015-01-05, its last print, so a span from
    // that day holds its bar and its member row there; under the half-open
    // one it left before the span.
    let from_monday = |inclusive: bool| LakeOptions {
        member_to_inclusive: inclusive,
        ..span("2015-01-05", "2015-12-31", false)
    };
    let (at_start, _) = read_lake(&dir, &from_monday(true)).unwrap();
    assert_eq!(dates_of(relation(&at_start, "equity.close"), "16"), vec!["2015-01-05"]);
    assert_eq!(dates_of(relation(&at_start, "equity.member"), "16"), vec!["2015-01-05"]);
    let (gone, _) = read_lake(&dir, &from_monday(false)).unwrap();
    assert!(!keys(relation(&gone, "equity.close")).contains("16"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// D8: the adapter rounds prices and dividend amounts to six decimals
/// (decision 9), and NDLake's values are single-precision floats widened,
/// almost all off that grid: 10,369,369 closes and 133,440 amounts moved by
/// up to 5e-7. With `lake_values` the store holds the lake's bits, through
/// a staged version and the appends of the next span alike (the hot store
/// keeps a value off every decimal grid as its raw bits, A-D54). A split
/// day's dividend is the amount per new share either way (decision 6).
#[test]
fn the_lakes_values_reach_the_store_bit_for_bit_through_a_staging_and_an_append() {
    let dir = two_span_lake();
    let lake_rows = |asset_days: &[(i64, Vec<usize>)]| -> Vec<(i64, u64)> {
        let mut v: Vec<(i64, u64)> = asset_days
            .iter()
            .flat_map(|(a, ks)| ks.iter().map(move |&k| (on(SESSIONS[k]) * DAY, lake_close(*a, k).to_bits())))
            .collect();
        v.sort();
        v
    };
    let every: Vec<(i64, Vec<usize>)> = (11..=16)
        .map(|a| {
            let last = match a {
                13 => 6,
                16 => 8,
                _ => 12,
            };
            (a, (0..=last).collect())
        })
        .collect();
    let stored = |store: &Store, version: &str, column: &str| -> Vec<(i64, u64)> {
        let b = store.read_all(version).unwrap();
        let t = b.column_by_name("T").unwrap().as_primitive::<arrow::datatypes::TimestampNanosecondType>();
        let p = b.column_by_name(column).unwrap().as_primitive::<arrow::datatypes::Float64Type>();
        let mut v: Vec<(i64, u64)> = (0..b.num_rows()).map(|i| (t.value(i), p.value(i).to_bits())).collect();
        v.sort();
        v
    };
    let mut store = Store::temporary("norgate-lake-values").unwrap();
    let mut versions = BTreeMap::new();
    for (from, to) in [("2014-01-01", "2014-12-31"), ("2015-01-01", "2015-12-31")] {
        let opts = LakeOptions {
            lake_values: true,
            ..span(from, to, true)
        };
        let (f, _) = read_lake(&dir, &opts).unwrap();
        assert_eq!(f.attributes["values"], "lake");
        versions = f.ingest(&mut store, "norgate", "norgate", &BTreeMap::new()).unwrap().versions;
    }
    for (relation, column) in [("equity.close", "P"), ("equity.open", "O"), ("equity.high", "H"), ("equity.low", "L")] {
        assert_eq!(stored(&store, &versions[relation], column), lake_rows(&every), "{}", relation);
    }
    let div = store.read_all(&versions["equity.dividend"]).unwrap();
    let mut amounts: Vec<u64> = div
        .column_by_name("Amount")
        .unwrap()
        .as_primitive::<arrow::datatypes::Float64Type>()
        .values()
        .iter()
        .map(|x| x.to_bits())
        .collect();
    amounts.sort();
    let mut want = vec![(0.11f32 as f64).to_bits(), (0.3f32 as f64 / 2.0).to_bits()];
    want.sort();
    assert_eq!(amounts, want, "the lake's amounts, the split day's per new share");
    // Rounded (the default), every value is on the 1e-6 grid and no close
    // is the lake's.
    let (f, report) = read_lake(&dir, &span("2015-01-01", "2015-12-31", true)).unwrap();
    assert!(!f.attributes.contains_key("values"));
    assert_eq!(report.dividends_rescaled_for_same_day_split, 1);
    let close = relation(&f, "equity.close").column(2).as_primitive::<arrow::datatypes::Float64Type>();
    assert!(close.values().iter().all(|x| (x * 1e6).round() / 1e6 == *x));
    assert!(close.values().iter().all(|x| (*x as f32) as f64 != *x));
    let amount = relation(&f, "equity.dividend").column(4).as_primitive::<arrow::datatypes::Float64Type>();
    assert_eq!(amount.values().to_vec(), vec![0.15]);
    let _ = std::fs::remove_dir_all(&dir);
}
