//! The option class's data (D-2.3, D-2.4, D-4.4, D-7.6, D-7.7), on ORATS
//! fixtures in the strikes layout (tests/fixtures/orats: SPY on 2024-03-11
//! as CSV and 2024-03-12 as JSON, SPX on 2024-03-11, a European, cash, AM
//! settled index):
//!
//! - a row is a call and a put keyed by their OCC symbols, with their
//!   quotes, greeks, terms and the chain's snapshot, the underlying joined
//!   to the store's tickers (an unknown ticker refused);
//! - the chain query admits by expiry, moneyness, right and open interest,
//!   and a day without a snapshot is empty and unresolved;
//! - the API is asked once per underlying and trade date, with the token;
//! - the pre-pass binds the quotes of what the chains admit, reports the
//!   days without a snapshot, buys them within the money budget only, and
//!   `resolve_dynamic` buys a free chain on demand.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use abt_data::adapter::{ingest_keyed, instruments, Security};
use abt_data::adapters::orats::{parse, Locator, Options, Orats};
use abt_data::adapters::{Adapter, Fetched};
use abt_data::options::{chain_ids, contracts, occ_symbol};
use abt_data::query::{execute, explain, ResolvedView};
use abt_data::resolver::catalog::{Catalog, Money, Price, Source, SourceKind, Unit};
use abt_data::resolver::{resolve, resolve_with, Sources};
use abt_data::Store;
use abt_wire::generated::abt::wire::{Binding, Budget, Chain, ChainFilters, Demand, Duration, DynamicNeed, Need, QueryForm, ResolveDynamicRequest};
use arrow::array::{Array, AsArray, Float64Array, RecordBatch, StringArray, TimestampNanosecondArray};
use arrow::datatypes::{DataType, Field, Schema, TimeUnit};

const DAY: i64 = 86_400_000_000_000;

fn t(s: &str) -> i64 {
    abt_wire::scalar::parse_timestamp(s).unwrap()
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/orats")
}

fn settings(kv: &[(&str, &str)]) -> BTreeMap<String, String> {
    kv.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

/// The lake's equities SPY (key 101) and SPX (102), with their closes on
/// 2024-03-11 to 2024-03-13.
fn underlyings(tag: &str) -> Store {
    let mut s = Store::temporary(tag).unwrap();
    let securities: Vec<Security> = [("101", "SPY"), ("102", "SPX")]
        .iter()
        .map(|(k, sym)| Security {
            key: k.to_string(),
            ticker: sym.to_string(),
            from: t("2000-01-03"),
            to: None,
        })
        .collect();
    let rows = [
        ("101", "2024-03-11", 510.0),
        ("101", "2024-03-12", 512.5),
        ("101", "2024-03-13", 513.0),
        ("102", "2024-03-11", 5100.0),
        ("102", "2024-03-12", 5125.0),
    ];
    let schema = Arc::new(Schema::new(vec![
        Field::new("A", DataType::Utf8, false),
        Field::new("T", DataType::Timestamp(TimeUnit::Nanosecond, None), false),
        Field::new("P", DataType::Float64, false),
    ]));
    let closes = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from(rows.iter().map(|r| r.0).collect::<Vec<_>>())),
            Arc::new(TimestampNanosecondArray::from(rows.iter().map(|r| t(r.1)).collect::<Vec<_>>())),
            Arc::new(Float64Array::from(rows.iter().map(|r| r.2).collect::<Vec<_>>())),
        ],
    )
    .unwrap();
    ingest_keyed(&mut s, "norgate", &securities, &[], &[("equity.close".to_string(), closes)], "norgate", &BTreeMap::new()).unwrap();
    s
}

fn orats_files(dir: &Path) -> Orats {
    Orats::from_settings(&settings(&[("files", dir.to_str().unwrap()), ("terms", "SPX=european:cash:am")])).unwrap()
}

/// The fixtures read and ingested into a store of the underlyings.
fn store_with_chains(tag: &str) -> (Store, Fetched) {
    let mut s = underlyings(tag);
    let o = orats_files(&fixtures());
    let (f, r) = o.read(o.options()).unwrap();
    assert_eq!((r.files, r.rows_in, r.contracts, r.snapshots), (3, 10, 14, 3), "{:?}", r);
    f.ingest(&mut s, "orats", "orats", &BTreeMap::new()).unwrap();
    (s, f)
}

fn id_of(s: &Store, key: &str) -> u64 {
    instruments(s).unwrap().into_iter().find(|i| i.key == key).unwrap_or_else(|| panic!("no instrument {}", key)).id
}

fn relation<'a>(f: &'a Fetched, name: &str) -> &'a RecordBatch {
    &f.relations.iter().find(|(r, _)| r == name).unwrap_or_else(|| panic!("no {}", name)).1
}

fn nums(b: &RecordBatch, col: &str) -> Vec<Option<f64>> {
    let a = b.column_by_name(col).unwrap().as_primitive::<arrow::datatypes::Float64Type>();
    (0..a.len()).map(|i| (!a.is_null(i)).then(|| a.value(i))).collect()
}

fn texts(b: &RecordBatch, col: &str) -> Vec<String> {
    let c = arrow::compute::cast(b.column_by_name(col).unwrap(), &DataType::Utf8).unwrap();
    let a = c.as_string::<i32>();
    (0..a.len()).map(|i| a.value(i).to_string()).collect()
}

#[test]
fn the_strikes_layout_reads_as_csv_and_json_alike() {
    let csv = parse(&std::fs::read_to_string(fixtures().join("SPY/2024-03-11.csv")).unwrap(), "csv").unwrap();
    let json = parse(&std::fs::read_to_string(fixtures().join("SPY/2024-03-12.json")).unwrap(), "json").unwrap();
    assert_eq!((csv.len(), json.len()), (5, 3));
    let r = &csv[0];
    assert_eq!((r.ticker.as_str(), r.trade_date, r.expiry, r.strike), ("SPY", t("2024-03-11"), t("2024-03-15"), 500.0));
    assert_eq!((r.call.bid, r.call.ask, r.put.open_interest), (Some(10.40), Some(10.70), Some(12000.0)));
    assert_eq!(r.quoted_at, Some(t("2024-03-11T20:00:00")));
    let j = &json[1];
    assert_eq!((j.strike, j.stock_price, j.call.iv, j.delta), (510.0, Some(512.5), Some(0.127), Some(0.68)));
    // A row without a strike is refused, named by its line.
    let bad = "ticker,tradeDate,expirDate,strike\nSPY,2024-03-11,2024-03-15,\n";
    let err = parse(bad, "bad.csv").unwrap_err().to_string();
    assert!(err.contains("bad.csv line 2") && err.contains("strike"), "{}", err);
    assert_eq!(occ_symbol("SPY", t("2024-03-15"), "call", 500.0), "SPY   240315C00500000");
    assert_eq!(occ_symbol("SPX", t("2024-03-15"), "put", 5012.5), "SPX   240315P05012500");
}

#[test]
fn a_row_is_a_call_and_a_put_keyed_by_their_occ_symbols() {
    let (s, f) = store_with_chains("orats-ingest");
    let view = ResolvedView::default();
    let terms = contracts(&s, &view, None).unwrap();
    assert_eq!(terms.len(), 14);
    let spy = id_of(&s, "101");
    let spx = id_of(&s, "102");
    let call = &terms[&id_of(&s, "SPY   240315C00500000")];
    assert_eq!(
        (
            call.underlying,
            call.right.as_str(),
            call.strike,
            call.expiry,
            call.style.as_str(),
            call.settlement.as_str(),
            call.settlement_time.as_str(),
            call.multiplier
        ),
        (spy, "call", 500.0, t("2024-03-15"), "american", "physical", "pm", 100.0)
    );
    let index = &terms[&id_of(&s, "SPX   240315P05100000")];
    assert_eq!(
        (index.underlying, index.style.as_str(), index.settlement.as_str(), index.settlement_time.as_str()),
        (spx, "european", "cash", "am")
    );
    // The quotes and greeks of the CSV's first row: the call's, then the
    // put's (delta less one, no theta or rho; theta a year, vega a unit).
    let q = relation(&f, "option.quote");
    assert_eq!(q.num_rows(), 20);
    let c = texts(q, "A").iter().position(|k| k == "SPY   240315C00500000").unwrap();
    let p = c + 1;
    assert_eq!(texts(q, "A")[p], "SPY   240315P00500000");
    assert_eq!(
        (nums(q, "Bid")[c], nums(q, "Ask")[p], nums(q, "OI")[p], nums(q, "Last")[c]),
        (Some(10.40), Some(0.52), Some(12000.0), None)
    );
    let g = relation(&f, "option.greeks");
    let (delta, theta, vega) = (nums(g, "Delta"), nums(g, "Theta"), nums(g, "Vega"));
    let round = |x: Option<f64>| x.map(|x| (x * 1e9).round() / 1e9);
    assert_eq!((delta[c], round(delta[p])), (Some(0.92), Some(-0.08)));
    assert_eq!((round(theta[c]), theta[p]), (Some(-29.2), None));
    assert_eq!((round(vega[c]), nums(g, "IV")[p]), (Some(6.0), Some(0.152)));
    // Received at the quote time; the underlying's price as a series.
    let r = q.column_by_name("r").unwrap().as_primitive::<arrow::datatypes::TimestampNanosecondType>();
    assert_eq!(r.value(c), t("2024-03-11T20:00:00"));
    let v = relation(&f, "series.value");
    assert_eq!(
        (texts(v, "Name"), nums(v, "V")),
        (vec!["SPX".to_string(), "SPY".to_string(), "SPY".to_string()], vec![Some(5100.0), Some(510.0), Some(512.5)])
    );
    // Each day's snapshot: SPY's five strikes on the 11th, three on the 12th.
    assert_eq!(chain_ids(&s, &view, spy, t("2024-03-11"), None).unwrap().map(|v| v.len()), Some(10));
    assert_eq!(chain_ids(&s, &view, spy, t("2024-03-12"), None).unwrap().map(|v| v.len()), Some(6));
    assert_eq!(chain_ids(&s, &view, spy, t("2024-03-13"), None).unwrap(), None);
    // The registry's relations are kept under the registry's source.
    assert!(s.registry.current("option_spec", Some("registry")).is_some());
    assert!(s.registry.current("chain_index", Some("registry")).is_some());
}

#[test]
fn an_underlying_no_ticker_carries_is_refused() {
    let dir = std::env::temp_dir().join(format!("abt-orats-qqq-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("QQQ.csv"),
        "ticker,tradeDate,expirDate,strike,callBidPrice,callAskPrice\nQQQ,2024-03-11,2024-03-15,440,1.0,1.1\n",
    )
    .unwrap();
    let mut s = underlyings("orats-qqq");
    let o = orats_files(&dir);
    let (f, _) = o.read(o.options()).unwrap();
    let err = f.ingest(&mut s, "orats", "orats", &BTreeMap::new()).unwrap_err().to_string();
    assert!(err.contains("`QQQ` is carried by no instrument"), "{}", err);
    let _ = std::fs::remove_dir_all(&dir);
}

fn chain_form(underlying: u64, at: &str, f: ChainFilters, spot: f64) -> QueryForm {
    QueryForm::Chain(Box::new(Chain {
        underlying,
        t: t(at),
        filters: Some(Box::new(f)),
        spot: Some(spot),
    }))
}

fn days(n: i64) -> Option<Box<Duration>> {
    Some(Box::new(Duration { months: 0, days: n, nanos: 0 }))
}

#[test]
fn the_chain_query_admits_by_expiry_moneyness_right_and_open_interest() {
    let (s, _) = store_with_chains("orats-chain");
    let view = ResolvedView::default();
    let spy = id_of(&s, "101");
    let near_puts = ChainFilters {
        expiry_from: days(0),
        expiry_to: days(10),
        moneyness_lo: Some(0.97),
        moneyness_hi: Some(1.0),
        right: Some("put".into()),
        ..ChainFilters::default()
    };
    let ids = |form: &QueryForm| -> Vec<String> {
        let (_, b) = execute(&s, &view, form, None, false).unwrap();
        b.iter().flat_map(|b| texts(b, "occ")).collect()
    };
    assert_eq!(ids(&chain_form(spy, "2024-03-11", near_puts.clone(), 510.0)), vec!["SPY   240315P00500000", "SPY   240315P00510000"]);
    // At least 15,000 contracts open: the 500 put has 12,000.
    let liquid = ChainFilters {
        min_open_interest: Some(15_000),
        ..near_puts.clone()
    };
    assert_eq!(ids(&chain_form(spy, "2024-03-11", liquid, 510.0)), vec!["SPY   240315P00510000"]);
    // The April expiry within 45 days, calls struck above the spot.
    let april = ChainFilters {
        expiry_from: days(30),
        expiry_to: days(45),
        moneyness_lo: Some(1.0),
        moneyness_hi: Some(1.5),
        right: Some("call".into()),
        ..ChainFilters::default()
    };
    assert_eq!(ids(&chain_form(spy, "2024-03-11", april, 510.0)), vec!["SPY   240419C00520000"]);
    // No snapshot on the 13th: empty, and `explain` says unresolved.
    let none = chain_form(spy, "2024-03-13", near_puts.clone(), 513.0);
    assert!(ids(&none).is_empty());
    assert!(explain(&s, &view, &none).unwrap().unresolved);
    let some = explain(&s, &view, &chain_form(spy, "2024-03-12", near_puts, 512.5)).unwrap();
    assert!(!some.unresolved);
    assert_eq!(some.rows, 6);
}

/// A server answering one request per connection with `answer(request
/// line)`, the request lines it saw.
fn http_server(answers: usize, answer: impl Fn(&str) -> Vec<u8> + Send + 'static) -> (String, std::thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let h = std::thread::spawn(move || {
        let mut seen = Vec::new();
        for _ in 0..answers {
            let (mut s, _) = listener.accept().unwrap();
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut first = String::new();
            r.read_line(&mut first).unwrap();
            loop {
                let mut l = String::new();
                r.read_line(&mut l).unwrap();
                if l == "\r\n" {
                    break;
                }
            }
            let out = answer(&first);
            seen.push(first);
            write!(s, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", out.len()).unwrap();
            s.write_all(&out).unwrap();
        }
        seen
    });
    (format!("http://{}", addr), h)
}

#[test]
fn the_api_is_asked_once_per_underlying_and_trade_date() {
    let env = format!("ABT_TEST_ORATS_TOKEN_{}", std::process::id());
    std::env::set_var(&env, "tok/en");
    let json = std::fs::read_to_string(fixtures().join("SPY/2024-03-12.json")).unwrap();
    let (base, server) = http_server(2, move |line| {
        assert!(line.starts_with("GET /datav2/hist/strikes?token=tok%2Fen&ticker=SPY&tradeDate=2024-03-1"), "{}", line);
        if line.contains("tradeDate=2024-03-12") {
            json.clone().into_bytes()
        } else {
            b"{\"data\": []}".to_vec()
        }
    });
    let o = Orats::new(
        Locator::Api { base, token_env: env.clone() },
        Options {
            tickers: vec!["SPY".into()],
            from: Some(t("2024-03-11")),
            to: Some(t("2024-03-12")),
            ..Options::default()
        },
    );
    let (f, r) = o.read(o.options()).unwrap();
    let seen = server.join().unwrap();
    assert_eq!(seen.len(), 2, "{:?}", seen);
    assert!(seen[0].contains("2024-03-11") && seen[1].contains("2024-03-12"));
    assert_eq!((r.files, r.rows_in, r.contracts), (2, 3, 6));
    assert_eq!(relation(&f, "option.quote").num_rows(), 6);
    // The token never shows in what an error says.
    std::env::remove_var(&env);
    let err = o.read(o.options()).unwrap_err().to_string();
    assert!(err.contains(&env) && !err.contains("tok/en"), "{}", err);
}

/// A demand of SPY's closes with a dynamic need of its puts expiring within
/// ten days, struck within 5 % of the close.
fn put_demand(money: f64) -> Demand {
    Demand {
        class: "option".into(),
        needs: Some(vec![Need {
            relations: vec!["equity.close".into()],
            calendar: None,
            from: None,
            to: None,
            scope: None,
            attributes: None,
            label: Some("bars".into()),
        }]),
        require: None,
        prefer: None,
        budget: Some(Box::new(Budget {
            money,
            bytes: None,
            requests: None,
            seconds: None,
        })),
        dynamic: Some(vec![DynamicNeed {
            relation: "option.quote".into(),
            underlying: 0,
            underlying_symbol: Some("SPY".into()),
            filters: Some(Box::new(ChainFilters {
                expiry_from: days(0),
                expiry_to: days(10),
                moneyness_lo: Some(0.95),
                moneyness_hi: Some(1.05),
                right: Some("put".into()),
                ..ChainFilters::default()
            })),
            spot_relation: Some("equity.close".into()),
            spot_column: Some("P".into()),
            horizon: None,
        }]),
        point: None,
    }
}

fn pairs(b: &Binding) -> BTreeMap<String, String> {
    b.pairs.clone().unwrap_or_default().into_iter().map(|p| (p.relation, p.version)).collect()
}

#[test]
fn the_prepass_binds_the_quotes_of_the_contracts_its_chains_admit() {
    let (s, _) = store_with_chains("orats-prepass");
    let d = put_demand(0.0);
    let p = abt_data::resolver::prepass::prepare(&s, &d).unwrap();
    let plan = &p.chains[0];
    // The 11th admits the three March puts, the 12th two of them; the 13th
    // has no snapshot.
    assert_eq!((plan.days.len(), plan.missing.clone(), plan.admitted.len()), (3, vec![t("2024-03-13")], 3));
    let (b, _) = resolve(&s, &d).unwrap();
    let bound = pairs(&b);
    assert_eq!(bound["option.quote"], s.registry.current("option.quote", Some("orats")).unwrap());
    assert!(bound.contains_key("option_spec") && bound.contains_key("chain_index"));
    let dynamic = b.dynamic.clone().unwrap();
    assert_eq!(dynamic[0].underlying, id_of(&s, "101"));
}

/// The fixtures with a strikes file of the 13th, in a directory of their
/// own: the source a missing chain is bought from.
fn vendor_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("abt-orats-vendor-{}-{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SPY-2024-03-13.csv"),
        "ticker,tradeDate,expirDate,strike,stockPrice,callBidPrice,callAskPrice,putBidPrice,putAskPrice,putOpenInterest,quoteDate\n\
         SPY,2024-03-13,2024-03-15,505,513,8.5,8.8,0.20,0.23,9000,2024-03-13T20:00:00Z\n\
         SPY,2024-03-13,2024-03-15,515,513,0.90,1.00,2.80,2.95,8000,2024-03-13T20:00:00Z\n",
    )
    .unwrap();
    dir
}

fn chain_source(dir: &Path, price: f64) -> Sources {
    let catalog = Catalog {
        prices: [("orats.req".to_string(), price)].into_iter().collect(),
        sources: vec![Source {
            name: "orats".into(),
            kind: SourceKind::Vendor,
            adapter: "orats".into(),
            freshness: "historical".into(),
            offers: orats_files(dir).offers(),
            price: vec![Price {
                unit: Unit::Request,
                money: Money::Named("orats.req".into()),
                currency: "USD".into(),
            }],
            quota: None,
            throughput: None,
            settings: BTreeMap::new(),
        }],
    };
    let adapter: Arc<dyn Adapter> = Arc::new(orats_files(dir));
    Sources {
        catalog,
        adapters: [("orats".to_string(), adapter)].into_iter().collect(),
    }
}

#[test]
fn the_prepass_buys_a_missing_chain_within_the_budget_only() {
    let dir = vendor_dir("buy");
    let sources = chain_source(&dir, 2.0);
    // Budget 0: reported, not bought.
    let (mut s, _) = store_with_chains("orats-buy-none");
    let r = resolve_with(&mut s, &sources, &put_demand(0.0), false).unwrap();
    assert!(r.notes.iter().any(|n| n.contains("cost 2.00 from `orats`, over the money budget 0.00")), "{:?}", r.notes);
    assert_eq!(chain_ids(&s, &ResolvedView::default(), id_of(&s, "101"), t("2024-03-13"), None).unwrap(), None);
    // Budget 5: one request bought, the 13th's chain is in the store.
    let (mut s, _) = store_with_chains("orats-buy-one");
    let r = resolve_with(&mut s, &sources, &put_demand(5.0), false).unwrap();
    assert!(r.notes.iter().any(|n| n.starts_with("bought the chains of SPY on 1 days from `orats` for 2.00")), "{:?}", r.notes);
    assert_eq!(chain_ids(&s, &ResolvedView::default(), id_of(&s, "101"), t("2024-03-13"), None).unwrap().map(|v| v.len()), Some(4));
    // The 13th's two March puts are admitted beside the 11th's three.
    assert!(r.notes.iter().any(|n| n.contains("5 contracts admitted over 3 days")), "{:?}", r.notes);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn resolve_dynamic_buys_a_free_chain_on_demand() {
    let dir = vendor_dir("dynamic");
    let (s, _) = store_with_chains("orats-dynamic");
    let spy = id_of(&s, "101");
    // The binding of the store as it is, before the vendor is a source.
    let (binding, _) = resolve(&s, &put_demand(0.0)).unwrap();
    let before = pairs(&binding);
    let svc = abt_data::service::DataService::with_sources(s, chain_source(&dir, 0.0));
    let need = put_demand(0.0).dynamic.unwrap().remove(0);
    let req = ResolveDynamicRequest {
        binding: Box::new(binding.clone()),
        need: Box::new(need.clone()),
        underlying: spy,
        t: t("2024-03-13"),
    };
    let after = svc.resolve_dynamic(&req).unwrap();
    let now = pairs(&after);
    assert_ne!(before["chain_index"], now["chain_index"]);
    assert_ne!(before["option.quote"], now["option.quote"]);
    assert!(after.id.is_some());
    let st = svc.store();
    assert_eq!(chain_ids(&st, &ResolvedView::default(), spy, t("2024-03-13"), None).unwrap().map(|v| v.len()), Some(4));
    drop(st);
    // A priced chain is not bought on demand.
    let (s2, _) = store_with_chains("orats-dynamic-priced");
    let (b2, _) = resolve(&s2, &put_demand(0.0)).unwrap();
    let svc2 = abt_data::service::DataService::with_sources(s2, chain_source(&dir, 1.0));
    let err = svc2
        .resolve_dynamic(&ResolveDynamicRequest {
            binding: Box::new(b2),
            need: Box::new(need),
            underlying: spy,
            t: t("2024-03-13") + DAY / 2,
        })
        .unwrap_err()
        .to_string();
    assert!(err.contains("bought only when free"), "{}", err);
    let _ = std::fs::remove_dir_all(&dir);
}
