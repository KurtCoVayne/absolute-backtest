//! The transitional resolver (M-2, M-3): a demand (D-7.2) answered from the
//! store's registry, the store being the only source until the vendors and
//! D-7's cost model land (M-6). Each need's relations are bound at a version
//! that satisfies it (Definition D-7.1: a calendar at least as fine as the
//! need's, the attributes the need asks for, and those the demand
//! requires), chosen by the demand's preference: the version it names, the
//! finest calendar, else the current version (the first source by name when
//! several hold one). The registry's identity relations are bound with
//! them. A need no version satisfies is refused, naming the need, the
//! versions held and what each lacks (V-3.11, K-3.2).

use std::collections::BTreeMap;

use abt_wire::generated::abt::wire::{Attribute, Binding, CostVector, Demand, Need, PlanStep, PreferenceKind, ResolvePlan, VersionRef};
use abt_wire::scalar::{civil_from_days, NANOS_PER_DAY, NANOS_PER_HOUR, NANOS_PER_SECOND};

use crate::adapter::REGISTRY_SOURCE;
use crate::catalog::qualified;
use crate::error::DataError;
use crate::registry::VersionRecord;
use crate::store::Store;

/// The registry's relations every binding holds (D-2.4).
pub const IDENTITY: [&str; 3] = ["instrument", "vendor_id", "ticker"];

/// A calendar's nominal length (`1m`, `@1d`, `1w`), to order calendars by
/// fineness: a finer calendar is a shorter one.
pub fn calendar_nanos(c: &str) -> Option<i64> {
    let c = c.trim_start_matches('@');
    let digits: String = c.chars().take_while(|ch| ch.is_ascii_digit()).collect();
    let n: i64 = digits.parse().ok()?;
    let unit = match &c[digits.len()..] {
        "s" => NANOS_PER_SECOND,
        "m" | "min" => 60 * NANOS_PER_SECOND,
        "h" => NANOS_PER_HOUR,
        "d" => NANOS_PER_DAY,
        "w" => 7 * NANOS_PER_DAY,
        "mo" => 30 * NANOS_PER_DAY,
        "y" => 365 * NANOS_PER_DAY,
        _ => return None,
    };
    Some(n * unit)
}

/// The calendar a version holds: its `calendar` attribute (an adapter's
/// statement of the bars it ingested), else its schema's.
fn version_calendar(store: &Store, v: &VersionRecord) -> Option<String> {
    v.attributes.get("calendar").cloned().or_else(|| store.registry.schema(&v.relation).and_then(|s| s.calendar))
}

fn date(t: i64) -> String {
    let (y, m, d) = civil_from_days(t.div_euclid(NANOS_PER_DAY));
    format!("{:04}-{:02}-{:02}", y, m, d)
}

/// What a version lacks of a need (empty when it satisfies it).
fn lacks(store: &Store, v: &VersionRecord, need: &Need, require: &[Attribute]) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(c) = &need.calendar {
        let held = version_calendar(store, v);
        match (held.as_deref().and_then(calendar_nanos), calendar_nanos(c)) {
            (Some(h), Some(w)) if h > w => out.push(format!(
                "its calendar @{} is coarser than @{}",
                held.unwrap_or_default().trim_start_matches('@'),
                c.trim_start_matches('@')
            )),
            _ => {}
        }
    }
    for a in need.attributes.iter().flatten().chain(require) {
        let ok = match v.attributes.get(&a.key) {
            Some(x) => a.value.is_empty() || *x == a.value,
            None => false,
        };
        if !ok {
            out.push(if a.value.is_empty() {
                format!("no `{}` attribute", a.key)
            } else {
                format!("not {} = {}", a.key, a.value)
            });
        }
    }
    out
}

fn describe(store: &Store, v: &VersionRecord) -> String {
    let span = match (v.from, v.to) {
        (Some(f), Some(t)) => format!("{}..{}", date(f), date(t)),
        _ => "no rows".to_string(),
    };
    // The store's own bookkeeping (`_ordinal`) is no statement of the data.
    let attrs: Vec<String> = v.attributes.iter().filter(|(k, _)| !k.starts_with('_')).map(|(k, x)| format!("{} = {}", k, x)).collect();
    format!(
        "{} from `{}` at @{}, {}, {}",
        &v.version[..v.version.len().min(12)],
        v.source,
        version_calendar(store, v).unwrap_or_else(|| "-".into()).trim_start_matches('@'),
        span,
        if attrs.is_empty() {
            "no attributes".to_string()
        } else {
            format!("attributes {{{}}}", attrs.join(", "))
        }
    )
}

/// The binding of a demand and the plan that chose it.
pub fn resolve(store: &Store, demand: &Demand) -> Result<(Binding, ResolvePlan), DataError> {
    if let Some(d) = demand.dynamic.as_ref().and_then(|d| d.first()) {
        return Err(DataError::Unresolved(format!(
            "the demand asks for `{}` chains, and no source offers option chains yet (the option class and its adapters land at M-7)",
            d.relation
        )));
    }
    let require: Vec<Attribute> = demand.require.clone().unwrap_or_default();
    let prefer = demand.prefer.as_ref().and_then(|p| p.first());
    let named = prefer.filter(|p| p.kind == PreferenceKind::Version).and_then(|p| p.version.clone());
    let finest = prefer.is_some_and(|p| p.kind == PreferenceKind::Finest);
    let reg = &store.registry;
    let mut pairs: BTreeMap<String, String> = BTreeMap::new();
    let mut steps = Vec::new();
    for (i, need) in demand.needs.iter().flatten().enumerate() {
        let label = need.label.clone().unwrap_or_else(|| need.relations.join(", "));
        for rel in &need.relations {
            let rel = if rel.contains('.') { rel.clone() } else { qualified(&demand.class, rel) };
            let mut held: Vec<&VersionRecord> = match &named {
                Some(v) => reg
                    .versions()
                    .filter(|r| r.relation == rel && (r.version == *v || (v.len() >= 8 && r.version.starts_with(v.as_str())) || r.attributes.get("version") == Some(v)))
                    .collect(),
                None => reg.currents().into_iter().filter(|(r, _, _)| *r == rel).filter_map(|(_, _, v)| reg.version(&v)).collect(),
            };
            held.sort_by(|a, b| (&a.source, &a.version).cmp(&(&b.source, &b.version)));
            if held.is_empty() {
                let what = match &named {
                    Some(v) => format!("no version `{}` of `{}` is held", v, rel),
                    None => format!("the store holds no version of `{}`", rel),
                };
                return Err(DataError::Unresolved(format!("need `{}` of class {}: {}", label, demand.class, what)));
            }
            let fits: Vec<&VersionRecord> = held.iter().copied().filter(|v| lacks(store, v, need, &require).is_empty()).collect();
            let chosen = if finest {
                fits.iter()
                    .copied()
                    .min_by_key(|v| (version_calendar(store, v).as_deref().and_then(calendar_nanos).unwrap_or(i64::MAX), v.source.clone()))
            } else {
                fits.first().copied()
            };
            let Some(v) = chosen else {
                let why: Vec<String> = held.iter().map(|v| format!("{}: {}", describe(store, v), lacks(store, v, need, &require).join("; "))).collect();
                return Err(DataError::Unresolved(format!(
                    "need `{}` of class {}: no version of `{}` satisfies it; held: {}",
                    label,
                    demand.class,
                    rel,
                    why.join(" | ")
                )));
            };
            pairs.insert(rel.clone(), v.version.clone());
            steps.push(PlanStep {
                need: i as u32,
                source: v.source.clone(),
                relation: rel.clone(),
                version: Some(v.version.clone()),
                cost: Some(Box::new(CostVector {
                    money: 0.0,
                    seconds: 0.0,
                    bytes: 0,
                    requests: 0,
                })),
                from: v.from,
                to: v.to,
                attributes: Some(
                    v.attributes
                        .iter()
                        .filter(|(k, _)| !k.starts_with('_'))
                        .map(|(key, value)| Attribute {
                            key: key.clone(),
                            value: value.clone(),
                        })
                        .collect(),
                ),
            });
        }
    }
    for rel in IDENTITY {
        if let Some(v) = reg.current(rel, Some(REGISTRY_SOURCE)) {
            pairs.insert(rel.to_string(), v.to_string());
        }
    }
    // The binding carries the point the demand is at (D-7.5): its id covers
    // it, so two points are two bindings.
    let mut point = demand.point.clone().unwrap_or_default();
    point.sort_by(|a, b| a.name.cmp(&b.name));
    let binding = Binding {
        pairs: Some(pairs.into_iter().map(|(relation, version)| VersionRef { relation, version }).collect()),
        point: (!point.is_empty()).then_some(point),
        dynamic: None,
        id: None,
    };
    let plan = ResolvePlan {
        steps: Some(steps),
        cost: Some(Box::new(CostVector {
            money: 0.0,
            seconds: 0.0,
            bytes: 0,
            requests: 0,
        })),
    };
    Ok((binding, plan))
}
