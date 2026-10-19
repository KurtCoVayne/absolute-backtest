//! The resolver (D-7). Against the store alone, a demand's needs are bound
//! at the versions that satisfy them (the calendar at least as fine, the
//! attributes the need and the demand require), chosen by the preference
//! (a named version, the finest calendar, the current version), and a need
//! no version satisfies is refused with what each held version lacks.
//! With a catalog's sources: the cover is the cheapest (Proposition D-7.4,
//! against brute force); a demand with money budget 0 never asks a vendor
//! (A-D22), and its binding is what the store holds, with the cheapest
//! plan reported over budget; within budget the gap is bought, ingested
//! and merged with the store's rows into one version; a dry run buys
//! nothing; a quota caps the requests; resolving again is the same binding
//! (Proposition D-7.5), and a new version changes only its relation's
//! pair.

use std::collections::BTreeMap;
use std::sync::Arc;

use abt_data::resolver::{calendar_nanos, resolve};
use abt_data::Store;
use abt_wire::generated::abt::wire::{Attribute, ChainFilters, Demand, DynamicNeed, Need, Preference, PreferenceKind};
use arrow::array::{Float64Array, RecordBatch, TimestampNanosecondArray, UInt64Array};
use arrow::datatypes::{DataType, Field, Schema, TimeUnit};

const DAY: i64 = 86_400 * 1_000_000_000;
const T0: i64 = 19_000 * DAY;

fn closes(days: i64) -> RecordBatch {
    let schema = Arc::new(Schema::new(vec![
        Field::new("A", DataType::UInt64, false),
        Field::new("T", DataType::Timestamp(TimeUnit::Nanosecond, None), false),
        Field::new("P", DataType::Float64, false),
    ]));
    let t: Vec<i64> = (0..days).map(|d| T0 + d * DAY).collect();
    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(UInt64Array::from(vec![1u64; t.len()])),
            Arc::new(TimestampNanosecondArray::from(t.clone())),
            Arc::new(Float64Array::from(t.iter().map(|_| 10.0).collect::<Vec<_>>())),
        ],
    )
    .unwrap()
}

fn attrs(kv: &[(&str, &str)]) -> BTreeMap<String, String> {
    kv.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn need(relations: &[&str], calendar: Option<&str>, attributes: &[(&str, &str)]) -> Need {
    Need {
        relations: relations.iter().map(|r| r.to_string()).collect(),
        calendar: calendar.map(str::to_string),
        from: None,
        to: None,
        scope: None,
        attributes: (!attributes.is_empty()).then(|| {
            attributes
                .iter()
                .map(|(k, v)| Attribute {
                    key: k.to_string(),
                    value: v.to_string(),
                })
                .collect()
        }),
        label: Some(format!("{} test", relations.join(", "))),
        optional: false,
    }
}

fn demand(needs: Vec<Need>, require_reviewed: bool, prefer: Option<Preference>) -> Demand {
    Demand {
        class: "equity".into(),
        needs: Some(needs),
        require: require_reviewed.then(|| {
            vec![Attribute {
                key: "reviewed".into(),
                value: String::new(),
            }]
        }),
        prefer: prefer.map(|p| vec![p]),
        budget: None,
        dynamic: None,
        point: None,
    }
}

/// `equity.close` from two sources: `vendor` at @1d, reviewed complete,
/// version 2026.10; `raw` at @1m, unreviewed.
fn store() -> (Store, String, String) {
    let mut s = Store::temporary("resolver").unwrap();
    let reviewed = s
        .ingest("equity.close", &[closes(20)], "vendor", attrs(&[("reviewed", "complete"), ("calendar", "1d"), ("version", "2026.10")]))
        .unwrap();
    let raw = s.ingest("equity.close", &[closes(30)], "raw", attrs(&[("calendar", "1m")])).unwrap();
    (s, reviewed, raw)
}

fn bound(s: &Store, d: &Demand) -> Result<BTreeMap<String, String>, String> {
    let (b, _) = resolve(s, d).map_err(|e| e.to_string())?;
    Ok(b.pairs.unwrap().into_iter().map(|p| (p.relation, p.version)).collect())
}

#[test]
fn calendars_order_by_their_length() {
    assert!(calendar_nanos("1m").unwrap() < calendar_nanos("@5m").unwrap());
    assert!(calendar_nanos("1h").unwrap() < calendar_nanos("1d").unwrap());
    assert!(calendar_nanos("1d").unwrap() < calendar_nanos("1w").unwrap());
    assert_eq!(calendar_nanos("weekly"), None);
}

#[test]
fn a_need_binds_the_version_that_satisfies_it() {
    let (s, reviewed, raw) = store();
    // No requirement: the first source by name holding a current version.
    let d = demand(vec![need(&["close"], None, &[])], false, None);
    assert_eq!(bound(&s, &d).unwrap()["equity.close"], raw);
    // `complete` asks for reviewed = complete: the vendor's.
    let d = demand(vec![need(&["close"], Some("1d"), &[("reviewed", "complete")])], false, None);
    assert_eq!(bound(&s, &d).unwrap()["equity.close"], reviewed);
    // `require reviewed` asks every version for a `reviewed` attribute.
    let d = demand(vec![need(&["close"], None, &[])], true, None);
    assert_eq!(bound(&s, &d).unwrap()["equity.close"], reviewed);
    // A finer calendar than the reviewed version's: only the raw minute
    // bars hold it.
    let d = demand(vec![need(&["close"], Some("1m"), &[])], false, None);
    assert_eq!(bound(&s, &d).unwrap()["equity.close"], raw);
}

/// A-D56: a `source` preference binds the named source's versions first
/// where it holds the relation, and leaves every other need as it was.
#[test]
fn a_source_preference_binds_that_source_first() {
    use abt_wire::generated::abt::wire::PreferenceKind;
    let (s, reviewed, raw) = store();
    let prefer = |src: &str| {
        Some(Preference {
            kind: PreferenceKind::Source,
            version: Some(src.into()),
        })
    };
    // `raw` sorts first by name; the preference puts `vendor` first.
    let d = demand(vec![need(&["close"], None, &[])], false, prefer("vendor"));
    assert_eq!(bound(&s, &d).unwrap()["equity.close"], reviewed);
    let d = demand(vec![need(&["close"], None, &[])], false, prefer("raw"));
    assert_eq!(bound(&s, &d).unwrap()["equity.close"], raw);
    // A source holding no fitting version changes nothing.
    let d = demand(vec![need(&["close"], None, &[])], false, prefer("nobody"));
    assert_eq!(bound(&s, &d).unwrap()["equity.close"], raw);
}

#[test]
fn an_unsatisfied_need_is_refused_with_what_each_version_lacks() {
    let (s, _, _) = store();
    let d = demand(vec![need(&["close"], Some("1m"), &[("reviewed", "complete")])], false, None);
    let err = bound(&s, &d).unwrap_err();
    assert!(err.contains("need `close test`") && err.contains("equity.close"), "{}", err);
    assert!(err.contains("coarser than @1m"), "the vendor's daily bars: {}", err);
    assert!(err.contains("not reviewed = complete"), "the raw minute bars: {}", err);
    // A relation the store holds no version of.
    let d = demand(vec![need(&["dividend"], None, &[])], false, None);
    let err = bound(&s, &d).unwrap_err();
    assert!(err.contains("no version of `equity.dividend`"), "{}", err);
}

#[test]
fn the_preference_chooses_among_the_versions_that_satisfy() {
    let (s, reviewed, raw) = store();
    // A version named by its attribute, or by its id.
    let by_name = Preference {
        kind: PreferenceKind::Version,
        version: Some("2026.10".into()),
    };
    assert_eq!(bound(&s, &demand(vec![need(&["close"], None, &[])], false, Some(by_name))).unwrap()["equity.close"], reviewed);
    let by_id = Preference {
        kind: PreferenceKind::Version,
        version: Some(raw[..12].to_string()),
    };
    assert_eq!(bound(&s, &demand(vec![need(&["close"], None, &[])], false, Some(by_id))).unwrap()["equity.close"], raw);
    let missing = Preference {
        kind: PreferenceKind::Version,
        version: Some("2026.11".into()),
    };
    let err = bound(&s, &demand(vec![need(&["close"], None, &[])], false, Some(missing))).unwrap_err();
    assert!(err.contains("no version `2026.11`"), "{}", err);
    // The finest calendar.
    let finest = Preference {
        kind: PreferenceKind::Finest,
        version: None,
    };
    assert_eq!(bound(&s, &demand(vec![need(&["close"], None, &[])], false, Some(finest))).unwrap()["equity.close"], raw);
}

#[test]
fn the_plan_states_each_versions_span_and_attributes() {
    let (s, reviewed, _) = store();
    let d = demand(vec![need(&["close"], None, &[("reviewed", "complete")])], false, None);
    let (_, plan) = resolve(&s, &d).unwrap();
    let steps = plan.steps.unwrap();
    assert_eq!(steps.len(), 1);
    let step = &steps[0];
    assert_eq!(step.version.as_deref(), Some(reviewed.as_str()));
    assert_eq!((step.from, step.to), (Some(T0), Some(T0 + 19 * DAY)));
    let a: BTreeMap<String, String> = step.attributes.clone().unwrap().into_iter().map(|a| (a.key, a.value)).collect();
    assert_eq!(a["reviewed"], "complete");
    assert_eq!(a["version"], "2026.10");
}

#[test]
fn a_dynamic_need_names_its_underlying_by_a_ticker_of_the_store() {
    // The chains themselves are tests/options.rs's; an underlying no ticker
    // span carries is refused by name (A-D24).
    let (s, _, _) = store();
    let mut d = demand(vec![need(&["close"], None, &[])], false, None);
    d.dynamic = Some(vec![DynamicNeed {
        relation: "option.quote".into(),
        underlying: 0,
        underlying_symbol: Some("NOPE".into()),
        filters: Some(Box::new(ChainFilters {
            moneyness_lo: Some(0.9),
            moneyness_hi: Some(1.1),
            ..ChainFilters::default()
        })),
        spot_relation: Some("equity.close".into()),
        spot_column: None,
        horizon: None,
    }]);
    let err = resolve(&s, &d).err().unwrap().to_string();
    assert!(err.contains("`NOPE` is carried by no instrument"), "{}", err);
}

// ---- with sources (D-7.1 to D-7.5) ----

use abt_data::adapters::mock::Mock;
use abt_data::adapters::Adapter;
use abt_data::resolver::catalog::{Catalog, Money, Offer, Price, Quota, Source, SourceKind, Unit};
use abt_data::resolver::cost::{compare, order, Cost};
use abt_data::resolver::cover::{cover, Interval};
use abt_data::resolver::{resolve_with, Resolution, Sources};
use abt_wire::generated::abt::wire::Budget;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> i64 {
        (self.next() % n) as i64
    }
}

/// The cheapest subset whose candidates, in the order of their ends, form a
/// chain covering `[from, to]` as `cover` defines one.
fn brute(from: i64, to: i64, gap: i64, cands: &[Interval<Cost>]) -> Option<Cost> {
    let ord = order(&[]);
    let mut best: Option<Cost> = None;
    for mask in 1u32..(1 << cands.len()) {
        let mut set: Vec<&Interval<Cost>> = (0..cands.len()).filter(|i| mask & (1 << i) != 0).map(|i| &cands[i]).collect();
        set.sort_by_key(|c| c.to);
        let ok = set[0].from <= from && set[0].to >= from && set.windows(2).all(|w| w[1].from <= w[0].to + gap && w[1].to > w[0].to) && set.last().unwrap().to >= to;
        if ok {
            let c = set.iter().fold(Cost::default(), |a, c| a + c.cost);
            if best.is_none_or(|b| compare(&c, &b, &ord) == std::cmp::Ordering::Less) {
                best = Some(c);
            }
        }
    }
    best
}

#[test]
fn the_cover_is_the_cheapest_cover() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let ord = order(&[]);
    for _ in 0..2000 {
        let m = 1 + rng.below(7) as usize;
        let cands: Vec<Interval<Cost>> = (0..m)
            .map(|_| {
                let a = rng.below(100);
                Interval {
                    from: a,
                    to: a + 1 + rng.below(60),
                    cost: Cost {
                        money: rng.below(5) as f64,
                        seconds: rng.below(10) as f64,
                        ..Cost::default()
                    },
                }
            })
            .collect();
        let (from, to) = (rng.below(30), 40 + rng.below(80));
        let dp = cover(from, to, 2, &cands, |a, b| compare(a, b, &ord));
        assert_eq!(dp.as_ref().map(|d| d.0), brute(from, to, 2, &cands), "{:?} over {}..{}", cands, from, to);
        if let Some((total, chain)) = dp {
            // The chain covers, and its cost is the sum of its candidates'.
            assert!(cands[chain[0]].from <= from && cands[*chain.last().unwrap()].to >= to);
            assert!(chain.windows(2).all(|w| cands[w[1]].from <= cands[w[0]].to + 2));
            assert_eq!(chain.iter().fold(Cost::default(), |a, &i| a + cands[i].cost), total);
        }
    }
}

#[test]
fn fewest_sources_orders_before_money() {
    let a = Cost {
        money: 1.0,
        sources: 2,
        ..Cost::default()
    };
    let b = Cost {
        money: 5.0,
        sources: 1,
        ..Cost::default()
    };
    assert_eq!(compare(&a, &b, &order(&[])), std::cmp::Ordering::Less, "money first by default");
    let fewest = Preference {
        kind: PreferenceKind::FewestSources,
        version: None,
    };
    assert_eq!(compare(&a, &b, &order(&[fewest])), std::cmp::Ordering::Greater);
}

fn day(d: i64) -> String {
    let (y, m, dd) = abt_wire::scalar::civil_from_days(T0 / DAY + d);
    format!("{:04}-{:02}-{:02}", y, m, dd)
}

/// `days` closes of instrument 1 from day `first`.
fn closes_from(first: i64, days: i64, price: f64) -> RecordBatch {
    let schema = Arc::new(Schema::new(vec![
        Field::new("A", DataType::UInt64, false),
        Field::new("T", DataType::Timestamp(TimeUnit::Nanosecond, None), false),
        Field::new("P", DataType::Float64, false),
    ]));
    let t: Vec<i64> = (first..first + days).map(|d| T0 + d * DAY).collect();
    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(UInt64Array::from(vec![1u64; t.len()])),
            Arc::new(TimestampNanosecondArray::from(t.clone())),
            Arc::new(Float64Array::from(vec![price; t.len()])),
        ],
    )
    .unwrap()
}

/// A vendor offering daily closes and volumes from day 0 at 5 a request,
/// fetched by a mock holding days 0 to 59 of both.
fn vendor(quota: Option<u64>) -> (Sources, Arc<Mock>) {
    let offer = Offer {
        class: "equity".into(),
        relations: vec!["close".into(), "volume".into()],
        calendar: "1d".into(),
        from: Some(day(0)),
        to: None,
        scope: "all".into(),
        has_receipt: false,
        supplies: vec![],
        attributes: attrs(&[("reviewed", "complete")]),
        instruments: Some(1),
    };
    let catalog = Catalog {
        prices: [("mock.req".to_string(), 5.0)].into_iter().collect(),
        sources: vec![Source {
            name: "databank".into(),
            kind: SourceKind::Vendor,
            adapter: "mock".into(),
            freshness: "historical".into(),
            offers: vec![offer.clone()],
            price: vec![Price {
                unit: Unit::Request,
                money: Money::Named("mock.req".into()),
                currency: "USD".into(),
            }],
            quota: quota.map(|r| Quota {
                window: "1s".into(),
                requests: Some(r),
                bytes: None,
            }),
            throughput: None,
            settings: BTreeMap::new(),
        }],
    };
    let volumes = {
        let b = closes_from(0, 60, 1000.0);
        let schema = Arc::new(Schema::new(vec![
            Field::new("A", DataType::UInt64, false),
            Field::new("T", DataType::Timestamp(TimeUnit::Nanosecond, None), false),
            Field::new("V", DataType::Float64, false),
        ]));
        RecordBatch::try_new(schema, b.columns().to_vec()).unwrap()
    };
    let mock = Arc::new(Mock::new(
        "databank",
        vec![offer],
        vec![],
        vec![("equity.close".into(), closes_from(0, 60, 11.0)), ("equity.volume".into(), volumes)],
    ));
    let adapter: Arc<dyn Adapter> = mock.clone();
    (
        Sources {
            catalog,
            adapters: [("mock".to_string(), adapter)].into_iter().collect(),
        },
        mock,
    )
}

/// The store holds days 0 to 19 of closes from the lake.
fn lake() -> (Store, String) {
    let mut s = Store::temporary("resolver-sources").unwrap();
    let v = s
        .ingest("equity.close", &[closes_from(0, 20, 10.0)], "lake", attrs(&[("calendar", "1d"), ("reviewed", "complete")]))
        .unwrap();
    (s, v)
}

/// Closes over days 0 to 40, at `money` budget.
fn spanned(relations: &[&str], money: Option<f64>) -> Demand {
    let mut n = need(relations, Some("1d"), &[]);
    n.from = Some(T0);
    n.to = Some(T0 + 40 * DAY);
    let mut d = demand(vec![n], false, None);
    d.budget = money.map(|m| {
        Box::new(Budget {
            money: m,
            bytes: None,
            requests: None,
            seconds: None,
        })
    });
    d
}

fn pair(r: &Resolution, rel: &str) -> String {
    r.binding.pairs.clone().unwrap().into_iter().find(|p| p.relation == rel).map(|p| p.version).unwrap_or_default()
}

#[test]
fn a_demand_with_money_budget_zero_never_asks_a_vendor() {
    let (mut s, held) = lake();
    let (sources, mock) = vendor(None);
    let r = resolve_with(&mut s, &sources, &spanned(&["close"], None), false).unwrap();
    assert!(mock.requests().is_empty(), "no vendor connection: {:?}", mock.requests());
    // Bound to what the store holds, the cheapest full plan reported.
    assert_eq!(pair(&r, "equity.close"), held);
    let over = r.over_budget.as_ref().expect("the plan over budget");
    assert_eq!(over.cost.as_ref().unwrap().money, 5.0);
    assert!(over.steps.iter().flatten().any(|st| st.source == "databank" && st.version.is_none()));
    assert!(r.notes.iter().any(|n| n.contains("bound to what the store holds") && n.contains("money 5.00")), "{:?}", r.notes);
    // A relation the store holds none of cannot fall back: refused, naming
    // the cheapest plan.
    let e = resolve_with(&mut s, &sources, &spanned(&["volume"], None), false).unwrap_err();
    assert!(matches!(e, abt_data::error::DataError::OutOfBudget(_)) && e.to_string().contains("money 5.00"), "{}", e);
    assert!(mock.requests().is_empty());
}

#[test]
fn within_budget_the_gap_is_bought_and_merged_and_resolving_again_buys_nothing() {
    let (mut s, held) = lake();
    let (sources, mock) = vendor(None);
    // A dry run plans the purchase and makes none.
    let dry = resolve_with(&mut s, &sources, &spanned(&["close"], Some(10.0)), true).unwrap();
    assert!(mock.requests().is_empty());
    let steps = dry.plan.steps.clone().unwrap();
    assert_eq!(steps.len(), 2, "{:?}", steps);
    assert_eq!((steps[0].source.as_str(), steps[0].version.as_deref()), ("lake", Some(held.as_str())));
    assert_eq!((steps[1].source.as_str(), steps[1].version.as_deref()), ("databank", None));
    assert_eq!(dry.binding.id, None);
    // Bought: the gap after the lake's last day, through the adapter.
    let r = resolve_with(&mut s, &sources, &spanned(&["close"], Some(10.0)), false).unwrap();
    let asked = mock.requests();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].relations, vec!["equity.close".to_string()]);
    assert!(asked[0].from > T0 + 19 * DAY && asked[0].from <= T0 + 20 * DAY && asked[0].to == T0 + 40 * DAY, "{:?}", asked[0]);
    assert_eq!(r.plan.cost.as_ref().unwrap().money, 5.0);
    assert!(r.notes.iter().any(|n| n.starts_with("bought `equity.close` from `databank`")), "{:?}", r.notes);
    // One version of the merged rows: the lake's twenty days, then the
    // vendor's twenty-one.
    let merged = pair(&r, "equity.close");
    assert_ne!(merged, held);
    let rows = s.read_all(&merged).unwrap();
    assert_eq!(rows.num_rows(), 41);
    let price = rows.column(2).as_any().downcast_ref::<Float64Array>().unwrap();
    assert_eq!((price.value(0), price.value(19), price.value(20), price.value(40)), (10.0, 10.0, 11.0, 11.0));
    let v = s.registry.version(&merged).unwrap();
    assert!(v.attributes.get("cover").is_some_and(|c| c.contains("lake@") && c.contains("databank@")), "{:?}", v.attributes);
    assert_eq!(v.attributes.get("reviewed").map(String::as_str), Some("complete"), "what both parts state");
    // Resolving again finds the purchase in the store: nothing is bought
    // and the binding is the same (Proposition D-7.5).
    let again = resolve_with(&mut s, &sources, &spanned(&["close"], Some(10.0)), false).unwrap();
    assert_eq!(mock.requests().len(), 1);
    assert_eq!(again.binding.pairs, r.binding.pairs);
    assert_eq!(again.plan.cost.as_ref().unwrap().money, 0.0);
}

#[test]
fn a_quota_caps_the_requests_a_plan_makes() {
    let (mut s, _) = lake();
    let (sources, mock) = vendor(Some(0));
    let e = resolve_with(&mut s, &sources, &spanned(&["close"], Some(10.0)), false).unwrap_err();
    assert!(e.to_string().contains("quota is 0 per 1s"), "{}", e);
    assert!(mock.requests().is_empty());
}

#[test]
fn a_new_version_changes_only_its_relations_pair() {
    let mut s = Store::temporary("resolver-reproducible").unwrap();
    s.ingest("equity.close", &[closes(20)], "lake", attrs(&[("calendar", "1d")])).unwrap();
    let vol = {
        let b = closes(20);
        let schema = Arc::new(Schema::new(vec![
            Field::new("A", DataType::UInt64, false),
            Field::new("T", DataType::Timestamp(TimeUnit::Nanosecond, None), false),
            Field::new("V", DataType::Float64, false),
        ]));
        RecordBatch::try_new(schema, b.columns().to_vec()).unwrap()
    };
    s.ingest("equity.volume", &[vol], "lake", attrs(&[("calendar", "1d")])).unwrap();
    let d = demand(vec![need(&["close", "volume"], None, &[])], false, None);
    let first = bound(&s, &d).unwrap();
    assert_eq!(bound(&s, &d).unwrap(), first, "the same catalog state, the same binding");
    s.ingest("equity.close", &[closes_from(20, 5, 12.0)], "lake", attrs(&[("calendar", "1d")])).unwrap();
    let second = bound(&s, &d).unwrap();
    assert_ne!(second["equity.close"], first["equity.close"]);
    assert_eq!(second["equity.volume"], first["equity.volume"]);
}

#[test]
fn the_catalog_is_checked_as_it_is_read() {
    let ok = r#"{ "prices": { "databento.gb": 0.5 },
        "sources": [ { "name": "databento", "kind": "vendor", "adapter": "databento",
          "offers": [ { "class": "future", "relations": ["close"], "calendar": "1m", "from": "2010-06-06" } ],
          "price": [ { "unit": "gb", "money": "databento.gb" } ] } ] }"#;
    let c = Catalog::parse(ok).unwrap();
    assert_eq!(c.money(&c.sources[0].price[0]), 0.5);
    assert!(c.render().contains("offers future.{close} @1m from 2010-06-06"), "{}", c.render());
    for (text, why) in [
        (ok.replace("\"databento.gb\": 0.5", "\"other\": 0.5"), "is not given in `prices`"),
        (ok.replace("\"1m\"", "\"minutely\""), "is not a calendar"),
        (ok.replace("2010-06-06", "June 2010"), "is not a date"),
        (ok.replace("\"name\": \"databento\"", "\"name\": \"store\""), "the store's own name"),
    ] {
        assert!(Catalog::parse(&text).is_err_and(|e| e.contains(why)), "{}", why);
    }
}
