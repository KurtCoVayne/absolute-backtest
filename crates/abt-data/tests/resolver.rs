//! The transitional resolver (D-7.2, M-3): a demand's needs bound at the
//! versions that satisfy them (the calendar at least as fine, the
//! attributes the need and the demand require), chosen by the preference
//! (a named version, the finest calendar, the current version), and a need
//! no version satisfies refused with what each held version lacks.

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
fn a_dynamic_need_waits_for_the_option_class() {
    let (s, _, _) = store();
    let mut d = demand(vec![need(&["close"], None, &[])], false, None);
    d.dynamic = Some(vec![DynamicNeed {
        relation: "quote".into(),
        underlying: 0,
        filters: Some(Box::new(ChainFilters {
            expiry_from: None,
            expiry_to: None,
            moneyness_lo: Some(0.9),
            moneyness_hi: Some(1.1),
            right: None,
            style: None,
            strike_lo: None,
            strike_hi: None,
            min_open_interest: None,
        })),
        spot_relation: Some("close".into()),
        spot_column: None,
        horizon: None,
    }]);
    let err = resolve(&s, &d).err().unwrap().to_string();
    assert!(err.contains("option chains"), "{}", err);
}
