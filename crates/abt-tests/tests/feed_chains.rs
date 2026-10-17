//! The `chains` feed statistic (Q-6) over the ORATS fixtures of abt-data
//! (SPY's snapshots of 2024-03-11 and 2024-03-12, SPX's of 2024-03-11):
//! per underlying, its days, contracts a day, expiries, strikes, the share
//! of its contract-days quoted both ways, and their mean open interest,
//! each computed by hand below.

use std::collections::BTreeMap;
use std::sync::Arc;

use abt_data::adapter::{ingest_keyed, instruments, Security};
use abt_data::adapters::orats::Orats;
use abt_data::resolver::Sources;
use abt_data::service::{inproc, DataService};
use abt_data::Store;
use abt_query::data::Data;
use abt_query::sheet::Cell;
use arrow::array::{Float64Array, RecordBatch, StringArray, TimestampNanosecondArray};
use arrow::datatypes::{DataType, Field, Schema, TimeUnit};

fn t(s: &str) -> i64 {
    abt_wire::scalar::parse_timestamp(s).unwrap()
}

#[test]
fn the_chains_statistic_describes_each_underlyings_snapshots() {
    let mut s = Store::temporary("feed-chains").unwrap();
    let securities: Vec<Security> = [("101", "SPY"), ("102", "SPX")]
        .iter()
        .map(|(k, sym)| Security {
            key: k.to_string(),
            ticker: sym.to_string(),
            from: t("2000-01-03"),
            to: None,
        })
        .collect();
    let schema = Arc::new(Schema::new(vec![
        Field::new("A", DataType::Utf8, false),
        Field::new("T", DataType::Timestamp(TimeUnit::Nanosecond, None), false),
        Field::new("P", DataType::Float64, false),
    ]));
    let closes = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from(vec!["101", "102"])),
            Arc::new(TimestampNanosecondArray::from(vec![t("2024-03-11"), t("2024-03-11")])),
            Arc::new(Float64Array::from(vec![510.0, 5100.0])),
        ],
    )
    .unwrap();
    ingest_keyed(&mut s, "norgate", &securities, &[], &[("equity.close".to_string(), closes)], "norgate", &BTreeMap::new()).unwrap();
    let dir = abt_tests::repo_root().join("crates/abt-data/tests/fixtures/orats");
    let o = Orats::from_settings(&[("files".to_string(), dir.to_string_lossy().to_string())].into_iter().collect()).unwrap();
    let (f, _) = o.read(o.options()).unwrap();
    f.ingest(&mut s, "orats", "orats", &BTreeMap::new()).unwrap();
    let ids: BTreeMap<String, u64> = instruments(&s).unwrap().into_iter().map(|i| (i.key, i.id)).collect();
    let binding: Vec<(String, String)> = ["option.quote", "option_spec", "chain_index"]
        .iter()
        .map(|r| (r.to_string(), s.registry.current(r, None).unwrap().to_string()))
        .collect();
    let svc = DataService::with_sources(s, Sources::store_only());
    let data = Data::connect(inproc(&svc)).unwrap();
    let sheet = abt_query::feed::statistic(&data, &binding, "chains", None, (None, None)).unwrap();
    assert_eq!(sheet.rows.len(), 2, "{}", sheet.text());
    let row = |key: &str| sheet.rows.iter().find(|r| r[0] == Cell::Int(ids[key] as i64)).unwrap_or_else(|| panic!("{}", sheet.text())).clone();
    // SPY: ten contracts on the 11th, six on the 12th; two expiries, strikes
    // 500 to 520; fifteen of its sixteen contract-days quoted (the April 520
    // call bid nothing on the 11th); open interest (102,000 + 80,350) / 16.
    assert_eq!(
        row("101"),
        vec![
            Cell::Int(ids["101"] as i64),
            Cell::Int(2),
            Cell::Num(8.0),
            Cell::Int(10),
            Cell::Int(2),
            Cell::Num(500.0),
            Cell::Num(520.0),
            Cell::Num(15.0 / 16.0),
            Cell::Num(182_350.0 / 16.0),
        ]
    );
    // SPX: four contracts on one day, every one quoted, (900 + 1,200 +
    // 3,000 + 3,200) / 4 open.
    assert_eq!(
        row("102"),
        vec![
            Cell::Int(ids["102"] as i64),
            Cell::Int(1),
            Cell::Num(4.0),
            Cell::Int(4),
            Cell::Int(1),
            Cell::Num(5000.0),
            Cell::Num(5100.0),
            Cell::Num(1.0),
            Cell::Num(2075.0),
        ]
    );
}
