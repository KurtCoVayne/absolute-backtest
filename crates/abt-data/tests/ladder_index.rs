//! E-109: staging a minute relation's ladder builds each level's index of
//! its bucket keys once, not once per bar of the level above (the
//! 15-minute level looked every 5-minute bar's label up in an index rebuilt
//! over all of the level's labels: quadratic, a GLBX root's staging never
//! finished). One test in its own binary: the count of indexes built is
//! process-wide.

use std::collections::BTreeMap;

use abt_data::adapters::databento::{futures, Options, Raw};
use abt_data::adapters::dbn::{self, rtype, Decoded, Header, InstrumentDef, Metadata, Record};
use abt_data::hot::ladder;
use abt_data::Store;

const MIN: i64 = 60_000_000_000;

fn t(s: &str) -> i64 {
    abt_wire::scalar::parse_timestamp(s).unwrap()
}

fn rec(record: Record) -> Decoded {
    Decoded { record, ts_out: None }
}

fn fixed(x: f64) -> i64 {
    (x * 1e9).round() as i64
}

#[test]
fn a_ladder_level_is_indexed_once_not_per_bar_above_it() {
    let mut records = vec![rec(Record::InstrumentDef(Box::new(InstrumentDef {
        hd: Header {
            rtype: rtype::INSTRUMENT_DEF,
            publisher_id: 1,
            instrument_id: 1,
            ts_event: t("2024-01-01") as u64,
        },
        ts_recv: t("2024-01-01") as u64,
        raw_symbol: "ESH4".into(),
        asset: "ES".into(),
        instrument_class: b'F',
        expiration: t("2024-03-15T13:30:00") as u64,
        activation: t("2023-06-01") as u64,
        min_price_increment: fixed(0.25),
        unit_of_measure_qty: fixed(50.0),
        currency: "USD".into(),
        ..InstrumentDef::default()
    })))];
    // 20,000 minute bars without a break: 4,000 five-minute buckets, each
    // a bar of the 15-minute level's source.
    let start = t("2024-01-02");
    for m in 0..20_000i64 {
        let px = 4800.0 + ((m % 97) as f64) * 0.25;
        records.push(rec(Record::Ohlcv {
            hd: Header {
                rtype: rtype::OHLCV_1M,
                publisher_id: 1,
                instrument_id: 1,
                ts_event: (start + m * MIN) as u64,
            },
            open: fixed(px),
            high: fixed(px + 0.5),
            low: fixed(px - 0.5),
            close: fixed(px + 0.25),
            volume: 10,
        }));
    }
    let meta = Metadata {
        version: 3,
        dataset: "GLBX.MDP3".into(),
        schema: Some(6),
        start: t("2024-01-01") as u64,
        end: Some(t("2024-02-01") as u64),
        limit: None,
        stype_in: Some(1),
        stype_out: 0,
        ts_out: false,
        symbol_cstr_len: 71,
        symbols: vec![],
        partial: vec![],
        not_found: vec![],
        mappings: vec![],
    };
    let mut raw = Raw::default();
    raw.add_dbn(&dbn::encode(&meta, &records), "fixture").unwrap();
    let (fetched, _) = futures(&raw, &Options::default()).unwrap();
    let mut store = Store::temporary(&format!("ladder-index-{}", std::process::id())).unwrap();
    let reviewed: BTreeMap<String, String> = [("reviewed".to_string(), "complete".to_string())].into_iter().collect();
    let before = ladder::indexes_built();
    fetched.ingest(&mut store, "databento", "databento", &reviewed).unwrap();
    let built = ladder::indexes_built() - before;
    // Five bar relations (open, high, low, close, volume), seven levels
    // each, built and then verified: some seventy indexes. Per bar of the
    // level above, the 15-minute level alone took 4,000 per relation.
    assert!(built > 0, "the ladder was staged");
    assert!(built <= 200, "{} label indexes built staging 20,000 minute bars", built);
}
