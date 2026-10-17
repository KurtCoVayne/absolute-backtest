//! The Databento adapters (D-7.7) on DBN streams written by the codec, on
//! the bucket's Parquet layout, through a mock historical API and a mock
//! live gateway:
//!
//! - futures: the root from each contract's definition, spreads left out, a
//!   bar without a definition refused; sessions from the bars' gaps; the
//!   roll by the session before's volume, never back to an earlier expiry;
//!   the back-adjusted series, `continuous` and `future_spec`;
//! - equities: the symbols of the metadata's mappings joined to the
//!   store's tickers, a symbol no ticker carries refused (A-D24); receipt
//!   at the bar's close plus the processing delay;
//! - the historical API's request (the key as the basic-auth user, the form)
//!   answered with Zstandard-compressed DBN;
//! - the live gateway's challenge, answer, subscription and stream, every
//!   record stamped at acceptance.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use abt_data::adapter::Security;
use abt_data::adapters::databento::{equities, futures, Databento, Locator, Options, Raw, Roll};
use abt_data::adapters::databento_live::{cram, Gateway};
use abt_data::adapters::dbn::{self, rtype, Decoded, Header, InstrumentDef, Metadata, Record};
use abt_data::adapters::http::Historical;
use abt_data::adapters::{Adapter, Feed, FetchRequest, Fetched};
use abt_data::Store;
use arrow::array::{Array, ArrayRef, AsArray, Float64Array, Int64Array, RecordBatch, StringArray, TimestampNanosecondArray, UInt32Array, UInt64Array};
use arrow::datatypes::{Field, Float64Type, Schema, TimestampNanosecondType};
use parquet::arrow::ArrowWriter;

const MIN: i64 = 60_000_000_000;
const DAY: i64 = 86_400 * 1_000_000_000;

fn t(s: &str) -> i64 {
    abt_wire::scalar::parse_timestamp(s).unwrap()
}

fn meta(schema: Option<u16>, mappings: Vec<dbn::SymbolMapping>) -> Metadata {
    Metadata {
        version: 3,
        dataset: "GLBX.MDP3".into(),
        schema,
        start: t("2024-03-01") as u64,
        end: Some(t("2024-03-31") as u64),
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

fn rec(record: Record) -> Decoded {
    Decoded { record, ts_out: None }
}

fn fixed(x: f64) -> i64 {
    (x * 1e9).round() as i64
}

fn bar(id: u32, start: i64, close: f64, volume: u64) -> Decoded {
    rec(Record::Ohlcv {
        hd: Header {
            rtype: rtype::OHLCV_1M,
            publisher_id: 1,
            instrument_id: id,
            ts_event: start as u64,
        },
        open: fixed(close - 0.25),
        high: fixed(close + 0.5),
        low: fixed(close - 0.5),
        close: fixed(close),
        volume,
    })
}

fn def(id: u32, symbol: &str, class: u8, expiration: &str) -> Decoded {
    rec(Record::InstrumentDef(Box::new(InstrumentDef {
        hd: Header {
            rtype: rtype::INSTRUMENT_DEF,
            publisher_id: 1,
            instrument_id: id,
            ts_event: t("2024-03-01") as u64,
        },
        ts_recv: t("2024-03-01") as u64,
        raw_symbol: symbol.into(),
        asset: "ES".into(),
        instrument_class: class,
        expiration: t(expiration) as u64,
        activation: t("2023-06-16") as u64,
        min_price_increment: fixed(0.25),
        unit_of_measure_qty: fixed(50.0),
        currency: "USD".into(),
        ..InstrumentDef::default()
    })))
}

/// Three sessions of ES (2024-03-11 to 13, 14:30 to 14:34 UTC): H4 (id 1)
/// trades the most on the first two days and M4 (id 2) on the second, so
/// the third session rolls to M4; a spread (id 3) trades beside them.
fn es_stream() -> Vec<u8> {
    let mut records = vec![
        def(1, "ESH4", b'F', "2024-03-15T13:30:00"),
        def(2, "ESM4", b'F', "2024-06-21T13:30:00"),
        def(3, "ESH4-ESM4", b'S', "2024-03-15"),
    ];
    for (d, day) in ["2024-03-11", "2024-03-12", "2024-03-13"].iter().enumerate() {
        for m in 0..5 {
            let start = t(day) + 14 * 60 * MIN + 30 * MIN + m * MIN;
            let (vh, vm) = match d {
                0 => (900, 100),
                1 => (300, 800),
                _ => (50, 1000),
            };
            records.push(bar(1, start, 5000.0 + d as f64 * 10.0 + m as f64, vh));
            records.push(bar(2, start, 5050.0 + d as f64 * 10.0 + m as f64, vm));
            records.push(bar(3, start, -50.0, 5));
        }
    }
    dbn::encode(&meta(Some(6), vec![]), &records)
}

fn column(b: &RecordBatch, name: &str) -> Vec<f64> {
    let c = arrow::compute::cast(b.column_by_name(name).unwrap(), &arrow::datatypes::DataType::Float64).unwrap();
    c.as_primitive::<Float64Type>().values().to_vec()
}

fn times(b: &RecordBatch, name: &str) -> Vec<i64> {
    b.column_by_name(name).unwrap().as_primitive::<TimestampNanosecondType>().values().to_vec()
}

fn strings(b: &RecordBatch, name: &str) -> Vec<String> {
    let c = arrow::compute::cast(b.column_by_name(name).unwrap(), &arrow::datatypes::DataType::Utf8).unwrap();
    let a = c.as_string::<i32>();
    (0..a.len()).map(|i| a.value(i).to_string()).collect()
}

fn relation<'a>(f: &'a Fetched, name: &str) -> &'a RecordBatch {
    &f.relations.iter().find(|(r, _)| r == name).unwrap_or_else(|| panic!("no {}", name)).1
}

#[test]
fn futures_roll_by_volume_into_a_back_adjusted_series() {
    let mut raw = Raw::default();
    raw.add_dbn(&es_stream(), "es").unwrap();
    let opts = Options {
        processing_delay: 2_000_000_000,
        ..Options::default()
    };
    let (f, report) = futures(&raw, &opts).unwrap();
    assert_eq!(report.dropped["not_an_outright_future"], 15, "the spread's bars");
    let es = &report.roots["ES"];
    assert_eq!((es.contracts, es.sessions), (2, 3));
    // H4 traded most in the first session, M4 in the second: the third
    // session holds M4.
    assert_eq!(es.rolls.len(), 1, "{:?}", es.rolls);
    assert!(es.rolls[0].starts_with("2024-03-13 ESH4 -> ESM4, gap 50"), "{:?}", es.rolls);
    let close = relation(&f, "future.close");
    assert_eq!(close.num_rows(), 15);
    let p = column(close, "P");
    // The first two sessions are H4's closes shifted by the gap (M4's last
    // close of the session before minus H4's: 5064 - 5014 = 50); the third
    // is M4's own.
    assert_eq!(p[0], 5000.0 + 50.0);
    assert_eq!(p[9], 5014.0 + 50.0);
    assert_eq!(p[10], 5070.0);
    assert!(strings(close, "A").iter().all(|k| k == "ES.v.0"));
    // Keyed at the bar's close, received two seconds after it.
    let ts = times(close, "T");
    assert_eq!(ts[0], t("2024-03-11") + 14 * 60 * MIN + 31 * MIN);
    assert_eq!(times(close, "r")[0], ts[0] + 2_000_000_000);
    // `continuous` names each bar's contract and adjustment.
    let cont = relation(&f, "continuous");
    assert_eq!(strings(cont, "contract_id")[0], "1");
    assert_eq!(strings(cont, "contract_id")[14], "2");
    assert_eq!(column(cont, "adjustment")[0], 50.0);
    assert_eq!(column(cont, "adjustment")[14], 0.0);
    // The sessions: labelled at their last bar's close, opening at their
    // first bar's start.
    let sessions = relation(&f, "future.session");
    assert_eq!(sessions.num_rows(), 3);
    assert_eq!(times(sessions, "S")[0], t("2024-03-11") + 14 * 60 * MIN + 35 * MIN);
    assert_eq!(times(sessions, "Open")[0], t("2024-03-11") + 14 * 60 * MIN + 30 * MIN);
    let spec = relation(&f, "future_spec");
    assert_eq!(strings(spec, "id"), vec!["1", "2"]);
    assert_eq!(column(spec, "point_value"), vec![50.0, 50.0]);
    assert_eq!(column(spec, "tick"), vec![0.25, 0.25]);
    assert_eq!(times(spec, "expiry")[0], t("2024-03-15T13:30:00"));
    assert_eq!(f.attributes["prices"], "back_adjusted");
    assert_eq!(f.attributes["calendar"], "1m");
    // By expiry, the series holds H4 throughout (it trades every session).
    let (by_expiry, r2) = futures(
        &raw,
        &Options {
            roll: Roll::Expiry,
            ..Options::default()
        },
    )
    .unwrap();
    assert!(r2.roots["ES"].rolls.is_empty());
    assert!(strings(relation(&by_expiry, "future.close"), "A").iter().all(|k| k == "ES.c.0"));
    // Ingested under the registry's ids: the root and the contracts.
    let mut store = Store::temporary("databento-futures").unwrap();
    let got = f.ingest(&mut store, "databento", "databento", &BTreeMap::new()).unwrap();
    for r in ["future.close", "future.session", "continuous", "future_spec"] {
        assert!(got.versions.contains_key(r), "{} not ingested: {:?}", r, got.versions.keys());
    }
    let ins = abt_data::adapter::instruments(&store).unwrap();
    let root = ins.iter().find(|i| i.key == "ES.v.0").unwrap();
    assert_eq!((root.class.as_str(), root.multiplier), ("future", Some(50.0)));
    assert!(ins.iter().any(|i| i.key == "1") && ins.iter().any(|i| i.key == "2"));
}

#[test]
fn a_futures_bar_without_its_definition_is_refused_naming_it() {
    let records = vec![
        def(1, "ESH4", b'F', "2024-03-15"),
        bar(1, t("2024-03-11T14:30:00"), 5000.0, 10),
        bar(9, t("2024-03-11T14:30:00"), 1.0, 1),
    ];
    let mut raw = Raw::default();
    raw.add_dbn(&dbn::encode(&meta(Some(6), vec![]), &records), "x").unwrap();
    let err = futures(&raw, &Options::default()).err().unwrap().to_string();
    assert!(err.contains("instrument 9") && err.contains("no definition"), "{}", err);
}

/// AAA and BBB as Norgate's assets 1 and 2 in the store, with tickers from
/// 2020.
fn store_with_tickers(tag: &str) -> Store {
    let mut store = Store::temporary(tag).unwrap();
    let sec = |k: &str, s: &str| Security {
        key: k.into(),
        ticker: s.into(),
        from: t("2020-01-01"),
        to: None,
    };
    let schema = Arc::new(Schema::new(vec![
        Field::new("A", arrow::datatypes::DataType::Utf8, false),
        Field::new("T", arrow::datatypes::DataType::Timestamp(arrow::datatypes::TimeUnit::Nanosecond, None), false),
        Field::new("P", arrow::datatypes::DataType::Float64, true),
    ]));
    let rows = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from(vec!["1", "2"])) as ArrayRef,
            Arc::new(TimestampNanosecondArray::from(vec![t("2024-01-02"), t("2024-01-02")])),
            Arc::new(Float64Array::from(vec![10.0, 20.0])),
        ],
    )
    .unwrap();
    abt_data::adapter::ingest_keyed(
        &mut store,
        "norgate",
        &[sec("1", "AAA"), sec("2", "BBB")],
        &[],
        &[("equity.close".into(), rows)],
        "norgate",
        &BTreeMap::new(),
    )
    .unwrap();
    store
}

fn equity_stream(symbols: &[(&str, u32)]) -> Vec<u8> {
    let mappings = symbols
        .iter()
        .map(|(s, id)| dbn::SymbolMapping {
            raw_symbol: s.to_string(),
            intervals: vec![dbn::MappingInterval {
                start_date: 20240102,
                end_date: 20240103,
                symbol: id.to_string(),
            }],
        })
        .collect();
    let mut records = Vec::new();
    for m in 0..3 {
        for (_, id) in symbols {
            records.push(bar(*id, t("2024-01-02T14:30:00") + m * MIN, 100.0 + *id as f64 + m as f64, 1000));
        }
    }
    let mut md = meta(Some(6), mappings);
    md.dataset = "XNAS.ITCH".into();
    dbn::encode(&md, &records)
}

#[test]
fn equities_are_joined_to_the_stores_tickers() {
    let mut raw = Raw::default();
    raw.add_dbn(&equity_stream(&[("AAA", 100), ("BBB", 200)]), "eq").unwrap();
    let opts = Options {
        class: "equity".into(),
        processing_delay: 5_000_000_000,
        ..Options::default()
    };
    let (f, report) = equities(&raw, &opts).unwrap();
    assert_eq!((report.bars_out, report.symbols), (6, 2));
    let close = relation(&f, "equity.close");
    assert_eq!(strings(close, "A")[0], "AAA");
    assert_eq!(times(close, "T")[0], t("2024-01-02T14:31:00"));
    assert_eq!(times(close, "r")[0], t("2024-01-02T14:31:05"));
    assert_eq!(f.attributes["dataset"], "XNAS.ITCH");
    let mut store = store_with_tickers("databento-eq");
    let got = f.ingest(&mut store, "databento", "databento", &BTreeMap::new()).unwrap();
    // The rows carry the Norgate assets' ids.
    let v = &got.versions["equity.close"];
    let rows = store.read_all(v).unwrap();
    let ids: Vec<u64> = rows.column(0).as_primitive::<arrow::datatypes::UInt64Type>().values().to_vec();
    let ins = abt_data::adapter::instruments(&store).unwrap();
    let id_of = |k: &str| ins.iter().find(|i| i.key == k).unwrap().id;
    assert!(ids.iter().all(|i| *i == id_of("1") || *i == id_of("2")), "{:?}", ids);
    // A symbol no ticker carries refuses the read, naming the row.
    let mut raw = Raw::default();
    raw.add_dbn(&equity_stream(&[("AAA", 100), ("ZZZ", 300)]), "eq").unwrap();
    let (f, _) = equities(&raw, &opts).unwrap();
    let err = f.ingest(&mut store, "databento", "databento", &BTreeMap::new()).err().unwrap().to_string();
    assert!(err.contains("`ZZZ`") && err.contains("equity."), "{}", err);
}

fn write_parquet(path: &Path, cols: Vec<(&str, ArrayRef)>) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let schema = Arc::new(Schema::new(cols.iter().map(|(n, a)| Field::new(*n, a.data_type().clone(), true)).collect::<Vec<_>>()));
    let batch = RecordBatch::try_new(schema.clone(), cols.into_iter().map(|(_, a)| a).collect()).unwrap();
    let mut w = ArrowWriter::try_new(std::fs::File::create(path).unwrap(), schema, None).unwrap();
    w.write(&batch).unwrap();
    w.close().unwrap();
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("abt-databento-{}-{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The bucket's layout: a day's minute bars of a root's contracts with
/// decoded prices, and the root's definitions.
#[test]
fn the_buckets_parquet_layout_reads_as_dbn_does() {
    let dir = scratch("bucket");
    let starts: Vec<i64> = (0..3).map(|m| t("2024-03-11T14:30:00") + m * MIN).collect();
    let n = starts.len();
    let mut ids = Vec::new();
    let mut ev = Vec::new();
    let mut px = Vec::new();
    for (k, s) in starts.iter().enumerate() {
        for id in [1u32, 2] {
            ids.push(id);
            ev.push(*s);
            px.push(5000.0 + id as f64 * 50.0 + k as f64);
        }
    }
    write_parquet(
        &dir.join("GLBX.MDP3/ohlcv-1m/ES.FUT/2024-03-11.parquet"),
        vec![
            ("ts_event", Arc::new(TimestampNanosecondArray::from(ev).with_timezone("UTC")) as ArrayRef),
            ("instrument_id", Arc::new(UInt32Array::from(ids.clone()))),
            ("symbol", Arc::new(StringArray::from(ids.iter().map(|i| if *i == 1 { "ESH4" } else { "ESM4" }).collect::<Vec<_>>()))),
            ("open", Arc::new(Float64Array::from(px.clone()))),
            ("high", Arc::new(Float64Array::from(px.clone()))),
            ("low", Arc::new(Float64Array::from(px.clone()))),
            ("close", Arc::new(Float64Array::from(px.clone()))),
            ("volume", Arc::new(UInt64Array::from(vec![10u64; 2 * n]))),
        ],
    );
    write_parquet(
        &dir.join("GLBX.MDP3/definition/ES.parquet"),
        vec![
            ("instrument_id", Arc::new(UInt32Array::from(vec![1u32, 2])) as ArrayRef),
            ("raw_symbol", Arc::new(StringArray::from(vec!["ESH4", "ESM4"]))),
            ("asset", Arc::new(StringArray::from(vec!["ES", "ES"]))),
            ("expiration", Arc::new(TimestampNanosecondArray::from(vec![t("2024-03-15"), t("2024-06-21")]))),
            ("activation", Arc::new(TimestampNanosecondArray::from(vec![t("2023-06-16"), t("2023-09-15")]))),
            ("min_price_increment", Arc::new(Float64Array::from(vec![0.25, 0.25]))),
            ("unit_of_measure_qty", Arc::new(Float64Array::from(vec![50.0, 50.0]))),
            ("currency", Arc::new(StringArray::from(vec!["USD", "USD"]))),
            ("instrument_class", Arc::new(StringArray::from(vec!["F", "F"]))),
            ("volume_hint", Arc::new(Int64Array::from(vec![0, 0]))),
        ],
    );
    let settings: BTreeMap<String, String> = [("files", dir.to_string_lossy().as_ref()), ("point_values", "ES=50"), ("settlement", "ES=cash")]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let d = Databento::from_settings(&settings).unwrap();
    let (f, report) = d.read(d.options()).unwrap();
    assert_eq!(report.files, 2);
    assert_eq!(report.roots["ES"].contracts, 2);
    // One session: the nearest expiry, H4.
    let close = relation(&f, "future.close");
    assert_eq!(column(close, "P"), vec![5050.0, 5051.0, 5052.0]);
    assert_eq!(strings(relation(&f, "future_spec"), "settlement"), vec!["cash", "cash"]);
    // A purchase of the bars carries the registry's relations with it.
    let req = FetchRequest {
        source: "databento".into(),
        class: "future".into(),
        relations: vec!["future.close".into()],
        calendar: "1m".into(),
        from: t("2024-03-11"),
        to: t("2024-03-12"),
        scope: "all".into(),
    };
    let got = d.fetch(&req).unwrap();
    let names: Vec<&str> = got.relations.iter().map(|(r, _)| r.as_str()).collect();
    assert_eq!(names, vec!["future.close", "continuous", "future_spec"]);
    assert!(Databento::from_settings(&[("files".to_string(), "x".to_string()), ("api".to_string(), "http://y".to_string())].into_iter().collect()).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A server answering one request per connection with `answer(request
/// line, headers, body)`.
fn http_server(answers: usize, answer: impl Fn(&str, &str, &str) -> Vec<u8> + Send + 'static) -> (String, std::thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let h = std::thread::spawn(move || {
        let mut seen = Vec::new();
        for _ in 0..answers {
            let (mut s, _) = listener.accept().unwrap();
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut first = String::new();
            r.read_line(&mut first).unwrap();
            let mut headers = String::new();
            let mut len = 0;
            loop {
                let mut l = String::new();
                r.read_line(&mut l).unwrap();
                if l == "\r\n" {
                    break;
                }
                if let Some(v) = l.to_ascii_lowercase().strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap();
                }
                headers.push_str(&l);
            }
            let mut body = vec![0; len];
            r.read_exact(&mut body).unwrap();
            let body = String::from_utf8(body).unwrap();
            let out = answer(&first, &headers, &body);
            seen.push(format!("{}{}", first, body));
            write!(s, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", out.len()).unwrap();
            s.write_all(&out).unwrap();
        }
        seen
    });
    (format!("http://{}", addr), h)
}

#[test]
fn the_historical_api_is_asked_with_the_key_and_answers_dbn() {
    let key = "db-test-key-ABCDE";
    let env = format!("ABT_TEST_DATABENTO_KEY_{}", std::process::id());
    std::env::set_var(&env, key);
    let expected_auth = format!("Basic {}", abt_data::adapters::http::base64(format!("{}:", key).as_bytes()));
    let stream = es_stream();
    let (base, server) = http_server(2, move |line, headers, body| {
        assert!(line.starts_with("POST /v0/timeseries.get_range "), "{}", line);
        assert!(headers.contains(&expected_auth), "{}", headers);
        assert!(body.contains("dataset=GLBX.MDP3") && body.contains("stype_in=parent") && body.contains("symbols=ES.FUT"), "{}", body);
        zstd::stream::encode_all(&stream[..], 3).unwrap()
    });
    let api = Historical {
        base,
        dataset: "GLBX.MDP3".into(),
        schema: "ohlcv-1m".into(),
        symbols: vec!["ES.FUT".into()],
        stype_in: "parent".into(),
        key_env: env.clone(),
    };
    let d = Databento::new(
        Locator::Api(api),
        Options {
            from: Some(t("2024-03-11")),
            to: Some(t("2024-03-14")),
            ..Options::default()
        },
    );
    let (f, report) = d.read(d.options()).unwrap();
    assert_eq!(report.roots["ES"].sessions, 3);
    assert_eq!(relation(&f, "future.close").num_rows(), 15);
    let seen = server.join().unwrap();
    // The definitions first, then the bars.
    assert!(seen[0].contains("schema=definition") && seen[1].contains("schema=ohlcv-1m"), "{:?}", seen);
    assert!(seen[0].contains(&format!("start={}", t("2024-03-11"))), "{:?}", seen);
    std::env::remove_var(&env);
    // Without the key, the request is not made.
    let d = Databento::new(
        Locator::Api(Historical {
            base: "http://127.0.0.1:9".into(),
            key_env: "ABT_TEST_NO_SUCH_KEY".into(),
            ..Historical::default()
        }),
        Options::default(),
    );
    assert!(d.read(d.options()).err().unwrap().to_string().contains("$ABT_TEST_NO_SUCH_KEY"));
}

#[test]
fn the_challenge_answer_is_the_hash_of_challenge_and_key_with_the_bucket() {
    // Computed independently: sha256("t7kNhwj4xqR0QYjzFKtBEG2ec2pXJ4FK|32-character-with-lots-of-filler").
    assert_eq!(
        cram("t7kNhwj4xqR0QYjzFKtBEG2ec2pXJ4FK", "32-character-with-lots-of-filler"),
        "42e2c6c6a874b2f4498bcaf0541be406901a23adef1fb2843d5426f6d2387d14-iller"
    );
}

/// A gateway: greeting, challenge, the answer checked, the subscription
/// read, then a DBN stream of a symbol mapping and two bars.
fn gateway(accept: bool) -> (String, std::thread::JoinHandle<(String, String)>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let h = std::thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        let mut r = BufReader::new(s.try_clone().unwrap());
        s.write_all(b"lsg_version=0.7.2\ncram=t7kNhwj4xqR0QYjzFKtBEG2ec2pXJ4FK\n").unwrap();
        let mut auth = String::new();
        r.read_line(&mut auth).unwrap();
        if !accept {
            s.write_all(b"success=0|error=Authentication failed.\n").unwrap();
            return (auth, String::new());
        }
        s.write_all(b"success=1|session_id=5\n").unwrap();
        let mut sub = String::new();
        r.read_line(&mut sub).unwrap();
        let mut start = String::new();
        r.read_line(&mut start).unwrap();
        assert_eq!(start, "start_session\n");
        let mut md = meta(None, vec![]);
        md.stype_in = None;
        let mapping = rec(Record::SymbolMapping {
            hd: Header {
                rtype: rtype::SYMBOL_MAPPING,
                publisher_id: 1,
                instrument_id: 7,
                ts_event: t("2024-03-11T14:30:00") as u64,
            },
            stype_in: Some(3),
            stype_in_symbol: "ES.v.0".into(),
            stype_out: Some(0),
            stype_out_symbol: "ESM4".into(),
            start_ts: t("2024-03-11") as u64,
            end_ts: t("2024-03-12") as u64,
        });
        let bytes = dbn::encode(&md, &[mapping, bar(7, t("2024-03-11T14:30:00"), 5100.0, 10), bar(7, t("2024-03-11T14:31:00"), 5101.0, 12)]);
        // In pieces, as a socket delivers them.
        for c in bytes.chunks(17) {
            s.write_all(c).unwrap();
            s.flush().unwrap();
        }
        (auth, sub)
    });
    (addr, h)
}

#[test]
fn a_live_session_is_authenticated_subscribed_and_stamped_at_acceptance() {
    let key = "32-character-with-lots-of-filler";
    let (addr, h) = gateway(true);
    let g = Gateway {
        addr,
        dataset: "GLBX.MDP3".into(),
        schema: "ohlcv-1m".into(),
        stype_in: "continuous".into(),
        symbols: vec!["ES.v.0".into()],
        ..Gateway::default()
    };
    let before = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as i64;
    let mut s = g.connect_with(key).unwrap();
    assert_eq!(s.session_id, "5");
    let mut got = Vec::new();
    while let Some(a) = s.next().unwrap() {
        got.push(a);
    }
    let (auth, sub) = h.join().unwrap();
    assert!(
        auth.starts_with("auth=42e2c6c6a874b2f4498bcaf0541be406901a23adef1fb2843d5426f6d2387d14-iller|dataset=GLBX.MDP3|encoding=dbn|compression=none|ts_out=0|client=abt"),
        "{}",
        auth
    );
    assert_eq!(sub, "schema=ohlcv-1m|stype_in=continuous|symbols=ES.v.0|snapshot=0|is_last=1\n");
    assert_eq!(got.len(), 3);
    assert!(got.iter().all(|a| a.at >= before));
    assert_eq!(s.symbol(7), Some("ES.v.0"));
    let f = s.rows("future", &got, 0).unwrap();
    let close = relation(&f, "future.close");
    assert_eq!(strings(close, "A"), vec!["ES.v.0", "ES.v.0"]);
    assert_eq!(column(close, "P"), vec![5100.0, 5101.0]);
    // Received at acceptance, never before the bar's close.
    let (tt, rr) = (times(close, "T"), times(close, "r"));
    assert!(tt.iter().zip(&rr).all(|(t, r)| r >= t));
    assert_eq!(strings(relation(&f, "continuous"), "contract_id"), vec!["7", "7"]);
    // The session as a feed: one bar's rows at a time, then the end.
    let (addr, h) = gateway(true);
    let g = Gateway {
        addr,
        dataset: "GLBX.MDP3".into(),
        stype_in: "continuous".into(),
        symbols: vec!["ES.v.0".into()],
        ..Gateway::default()
    };
    let mut feed = abt_data::adapters::databento_live::LiveFeed::new(g.connect_with(key).unwrap(), "future", 0);
    let first = feed.next().unwrap().unwrap();
    assert_eq!(column(relation(&first, "future.close"), "P"), vec![5100.0]);
    assert_eq!(column(relation(&feed.next().unwrap().unwrap(), "future.close"), "P"), vec![5101.0]);
    assert!(feed.next().unwrap().is_none());
    h.join().unwrap();
    // A refused session names the gateway's reason.
    let (addr, h) = gateway(false);
    let g = Gateway {
        addr,
        dataset: "GLBX.MDP3".into(),
        ..Gateway::default()
    };
    let err = g.connect_with(key).err().unwrap().to_string();
    assert!(err.contains("Authentication failed."), "{}", err);
    h.join().unwrap();
    let _ = DAY;
}

fn stat(id: u32, day: &str, kind: u16, price: f64, quantity: i64) -> Decoded {
    rec(Record::Statistics {
        hd: Header {
            rtype: rtype::STATISTICS,
            publisher_id: 1,
            instrument_id: id,
            ts_event: (t(day) + 21 * 60 * MIN) as u64,
        },
        ts_recv: (t(day) + 21 * 60 * MIN + 2 * MIN) as u64,
        ts_ref: t(day) as u64,
        price: if kind == 3 { fixed(price) } else { dbn::UNDEF_PRICE },
        quantity: if kind == 9 { quantity } else { dbn::UNDEF_STAT_QUANTITY },
        sequence: 1,
        ts_in_delta: 0,
        stat_type: kind,
        channel_id: 0,
        update_action: 1,
        stat_flags: 0,
    })
}

#[test]
fn the_venues_settlement_prices_and_open_interest_are_the_series_back_adjusted() {
    // ES's three sessions, with H4's settlements on the first two days and
    // M4's on the third (the session after the roll), and their open
    // interest; one of H4's prices published twice, the later kept.
    let mut raw = Raw::default();
    raw.add_dbn(&es_stream(), "es").unwrap();
    let stats = vec![
        stat(1, "2024-03-11", 3, 5003.50, 0),
        stat(1, "2024-03-12", 3, 5013.00, 0),
        stat(1, "2024-03-12", 3, 5013.25, 0),
        stat(2, "2024-03-13", 3, 5074.75, 0),
        stat(1, "2024-03-11", 9, 0.0, 2_000_000),
        stat(2, "2024-03-13", 9, 0.0, 1_500_000),
        // M4's settlement of a session the series holds H4 at: not the series'.
        stat(2, "2024-03-11", 3, 5053.00, 0),
    ];
    raw.add_dbn(&dbn::encode(&meta(Some(6), vec![]), &stats), "stats").unwrap();
    let (f, _) = futures(&raw, &Options::default()).unwrap();
    let s = relation(&f, "future.settlement");
    // H4 + the gap of 50 before the roll, M4's own after it.
    assert_eq!(column(s, "P"), vec![5053.50, 5063.25, 5074.75]);
    assert!(strings(s, "A").iter().all(|k| k == "ES.v.0"));
    // Keyed at the session's last close, received when published.
    assert_eq!(times(s, "T")[0], t("2024-03-11") + 14 * 60 * MIN + 35 * MIN);
    assert_eq!(times(s, "r")[0], t("2024-03-11") + 21 * 60 * MIN + 2 * MIN);
    let oi = relation(&f, "future.open_interest");
    assert_eq!(column(oi, "N"), vec![2_000_000.0, 1_500_000.0]);
}
