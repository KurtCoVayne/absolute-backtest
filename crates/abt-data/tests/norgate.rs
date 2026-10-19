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
