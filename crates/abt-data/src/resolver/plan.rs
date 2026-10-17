//! A demand's plan (D-7.4): for each need and each of its relations, the
//! candidates that satisfy it (the versions the store holds, and the
//! offers of the catalog's sources over the parts of the need's span the
//! store does not hold), and the cheapest cover of the span among them,
//! within the demand's budget.
//!
//! A version the store holds costs nothing, so the store's versions are
//! chosen as the transitional resolver chose them: the version a
//! preference names, the finest calendar under `finest`, a reviewed one
//! under `reviewed`, else the first source by name holding a current
//! version (ERRATA E-67). An offer is priced over the part of the span it
//! would fill.

use std::collections::BTreeMap;

use abt_wire::generated::abt::wire::{Attribute, Demand, Need, PreferenceKind};
use abt_wire::scalar::{civil_from_days, parse_timestamp, NANOS_PER_DAY, NANOS_PER_HOUR, NANOS_PER_SECOND};

use super::catalog::{Catalog, Offer, Source, Unit};
use super::cost::{self, Cost};
use super::cover::{cover, Interval};
use crate::catalog::qualified;
use crate::error::DataError;
use crate::registry::VersionRecord;
use crate::store::Store;

/// The registry's relations (D-2.4) a binding names whenever the store holds them:
/// the instruments and their keys, and the contracts' terms the kernel's
/// models read (an option's `option_spec` and its chains, a future's
/// `future_spec` and its root's `continuous`; ERRATA E-70).
pub const IDENTITY: [&str; 7] = ["instrument", "vendor_id", "ticker", "option_spec", "chain_index", "future_spec", "continuous"];

/// How far apart two sources' spans may be and still abut: a weekend and a
/// holiday, the bars no calendar has between one source's last bar and
/// the next's first.
pub const ABUT: i64 = 4 * NANOS_PER_DAY;

/// The bytes a second an offer is estimated to deliver when its source
/// states none, and a request's latency.
const THROUGHPUT: f64 = 1.0e7;
const LATENCY: f64 = 0.1;

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

pub(crate) fn date(t: i64) -> String {
    let (y, m, d) = civil_from_days(t.div_euclid(NANOS_PER_DAY));
    format!("{:04}-{:02}-{:02}", y, m, d)
}

/// What rows of `calendar` stating `attributes` lack of a need (empty when
/// they satisfy it, Definition D-7.1).
fn lacks(calendar: Option<&str>, attributes: &BTreeMap<String, String>, need: &Need, require: &[Attribute]) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(c) = &need.calendar {
        match (calendar.and_then(calendar_nanos), calendar_nanos(c)) {
            (Some(h), Some(w)) if h > w => out.push(format!(
                "its calendar @{} is coarser than @{}",
                calendar.unwrap_or_default().trim_start_matches('@'),
                c.trim_start_matches('@')
            )),
            _ => {}
        }
    }
    for a in need.attributes.iter().flatten().chain(require) {
        let ok = match attributes.get(&a.key) {
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

/// Where a part of a relation's span comes from.
#[derive(Clone, Debug, PartialEq)]
pub enum Origin {
    /// A version the store holds.
    Stored(String),
    /// A purchase from an offer of a source (its index among the source's
    /// offers), not yet made.
    Fetch(usize),
}

/// A part of a relation's span and where it comes from.
#[derive(Clone, Debug, PartialEq)]
pub struct Part {
    pub need: usize,
    /// The store's name of the relation (`equity.close`).
    pub relation: String,
    pub source: String,
    pub origin: Origin,
    pub from: Option<i64>,
    pub to: Option<i64>,
    pub cost: Cost,
    pub calendar: Option<String>,
    pub attributes: BTreeMap<String, String>,
}

/// A relation of a need and the parts its span is bound from, in time
/// order.
#[derive(Clone, Debug, PartialEq)]
pub struct RelationPlan {
    pub need: usize,
    pub relation: String,
    pub parts: Vec<Part>,
    /// The store's best version alone, when a purchase completes the span:
    /// what the relation is bound to when the budget forbids the purchase.
    pub fallback: Option<Part>,
}

/// A demand's plan.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Planned {
    pub relations: Vec<RelationPlan>,
    pub cost: Cost,
    /// The cheapest plan the budget does not allow, and what of the budget
    /// it exceeds, when the plan binds what the store holds instead.
    pub over_budget: Option<(Vec<Part>, Cost, String)>,
    pub notes: Vec<String>,
}

/// The rows a day of bars of `calendar`: one at a day or coarser, a
/// 390-minute session's bars below.
fn bars_a_day(calendar: &str) -> f64 {
    match calendar_nanos(calendar) {
        Some(n) if n >= NANOS_PER_DAY => NANOS_PER_DAY as f64 / n as f64,
        Some(n) => (390.0 * 60.0 * NANOS_PER_SECOND as f64 / n as f64).max(1.0),
        None => 1.0,
    }
}

/// The estimated cost of buying `relations` relations of an offer over
/// `[from, to]` (Definition D-7.2): the rows are the bars of its calendar
/// a day times the days times its instruments; a row of a relation is its
/// value and the entity, `t` and `r` it is keyed by.
pub(crate) fn estimate(catalog: &Catalog, s: &Source, o: &Offer, relations: usize, from: i64, to: i64) -> Cost {
    let days = ((to - from) as f64 / NANOS_PER_DAY as f64).floor() + 1.0;
    let instruments = o.instruments.unwrap_or(1) as f64;
    let rows = days * bars_a_day(&o.calendar) * instruments;
    let bytes = (rows * (32.0 * relations as f64)).ceil() as u64;
    let requests = relations as u64;
    let mut money = 0.0;
    for p in &s.price {
        let units = match p.unit {
            Unit::Request => requests as f64,
            Unit::Gb => bytes as f64 / 1e9,
            Unit::SymbolDay => instruments * days,
            Unit::Month => (days / 30.44).ceil(),
        };
        money += catalog.money(p) * units;
    }
    Cost {
        money,
        seconds: bytes as f64 / s.throughput.unwrap_or(THROUGHPUT) + requests as f64 * LATENCY,
        bytes,
        requests,
        sources: 1,
    }
}

/// The parts of `[from, to]` no held span reaches. A span reaches the next
/// within `ABUT`, and the ends within `ABUT` of the span's: a need from a
/// Saturday is held by a version from the Monday.
pub(crate) fn gaps(from: i64, to: i64, mut held: Vec<(i64, i64)>) -> Vec<(i64, i64)> {
    held.sort();
    let mut out = Vec::new();
    // Every instant before `cursor` is held, or within reach of a held one.
    let mut cursor = from;
    let mut reach: Option<i64> = None;
    for (a, b) in held {
        if b < cursor {
            continue;
        }
        let limit = match reach {
            None => from.saturating_add(ABUT),
            Some(r) => r.saturating_add(ABUT),
        };
        if a > limit {
            out.push((cursor, a - 1));
        }
        let r = reach.map_or(b, |r| r.max(b));
        reach = Some(r);
        cursor = r.saturating_add(1);
        if cursor > to {
            return out;
        }
    }
    match reach {
        Some(r) if r.saturating_add(ABUT) >= to => {}
        _ => out.push((cursor, to)),
    }
    out
}

/// The span a cover must reach: `[from, to]` less `ABUT` at either end (its
/// middle instant when shorter).
fn reach_of(from: i64, to: i64) -> (i64, i64) {
    let (a, b) = (from.saturating_add(ABUT), to.saturating_sub(ABUT));
    if a <= b {
        (a, b)
    } else {
        let m = from + (to - from) / 2;
        (m, m)
    }
}

/// A candidate of a relation's cover.
struct Candidate {
    part: Part,
    from: i64,
    to: i64,
}

/// The plan of one relation of one need.
#[allow(clippy::too_many_arguments)]
fn relation_plan(
    store: &Store,
    catalog: &Catalog,
    demand: &Demand,
    i: usize,
    need: &Need,
    label: &str,
    rel: &str,
    named: Option<&str>,
    prefer: &[PreferenceKind],
    order: &[cost::Key],
    notes: &mut Vec<String>,
) -> Result<RelationPlan, DataError> {
    let require: Vec<Attribute> = demand.require.clone().unwrap_or_default();
    let reg = &store.registry;
    let mut held: Vec<&VersionRecord> = match named {
        Some(v) => reg
            .versions()
            .filter(|r| r.relation == rel && (r.version == v || (v.len() >= 8 && r.version.starts_with(v)) || r.attributes.get("version").map(String::as_str) == Some(v)))
            .collect(),
        None => reg.currents().into_iter().filter(|(r, _, _)| r == rel).filter_map(|(_, _, v)| reg.version(&v)).collect(),
    };
    held.sort_by(|a, b| (&a.source, &a.version).cmp(&(&b.source, &b.version)));
    // The offers that satisfy the need; none when a version is named (a
    // pinned version is bought by nobody, Proposition D-7.5).
    let short = rel.rsplit_once('.').map(|(_, s)| s).unwrap_or(rel);
    let mut offers: Vec<(&Source, usize, &Offer)> = Vec::new();
    if named.is_none() {
        for s in &catalog.sources {
            for (k, o) in s.offers.iter().enumerate() {
                if o.class == demand.class && o.relations.iter().any(|r| r == short) && lacks(Some(&o.calendar), &o.attributes, need, &require).is_empty() {
                    offers.push((s, k, o));
                }
            }
        }
    }
    if held.is_empty() && offers.is_empty() {
        let what = match named {
            Some(v) => format!("no version `{}` of `{}` is held", v, rel),
            None => format!("the store holds no version of `{}`", rel),
        };
        return Err(DataError::Unresolved(format!("need `{}` of class {}: {}", label, demand.class, what)));
    }
    let mut fits: Vec<&VersionRecord> = held
        .iter()
        .copied()
        .filter(|v| lacks(version_calendar(store, v).as_deref(), &v.attributes, need, &require).is_empty())
        .collect();
    // The preferences order the store's versions (each costs nothing).
    if prefer.contains(&PreferenceKind::Finest) {
        fits.sort_by_key(|v| version_calendar(store, v).as_deref().and_then(calendar_nanos).unwrap_or(i64::MAX));
    }
    if prefer.contains(&PreferenceKind::Reviewed) {
        fits.sort_by_key(|v| !v.attributes.contains_key("reviewed"));
    }
    if fits.is_empty() && offers.is_empty() {
        let why: Vec<String> = held
            .iter()
            .map(|v| format!("{}: {}", describe(store, v), lacks(version_calendar(store, v).as_deref(), &v.attributes, need, &require).join("; ")))
            .collect();
        return Err(DataError::Unresolved(format!(
            "need `{}` of class {}: no version of `{}` satisfies it; held: {}",
            label,
            demand.class,
            rel,
            why.join(" | ")
        )));
    }
    let stored = |v: &VersionRecord| Part {
        need: i,
        relation: rel.to_string(),
        source: v.source.clone(),
        origin: Origin::Stored(v.version.clone()),
        from: v.from,
        to: v.to,
        cost: Cost::default(),
        calendar: version_calendar(store, v),
        attributes: v.attributes.iter().filter(|(k, _)| !k.starts_with('_')).map(|(k, x)| (k.clone(), x.clone())).collect(),
    };
    let first = fits.first().map(|v| stored(v));
    // A need without a span takes the first version that satisfies it; a
    // purchase needs a span to be priced.
    if need.from.is_none() && need.to.is_none() {
        return match first {
            Some(p) => Ok(RelationPlan {
                need: i,
                relation: rel.to_string(),
                parts: vec![p],
                fallback: None,
            }),
            None => Err(DataError::Unresolved(format!(
                "need `{}` of class {}: no version of `{}` is held, and a purchase needs the need's span (from, to)",
                label, demand.class, rel
            ))),
        };
    }
    let held_spans: Vec<(i64, i64)> = fits.iter().filter_map(|v| Some((v.from?, v.to?))).collect();
    let span_from = need.from.or_else(|| held_spans.iter().map(|s| s.0).min());
    let span_to = need.to.or_else(|| held_spans.iter().map(|s| s.1).max());
    let (Some(from), Some(to)) = (span_from, span_to) else {
        // Neither a stated span nor a held one: an open-ended purchase is
        // not priced.
        return match first {
            Some(p) => Ok(RelationPlan {
                need: i,
                relation: rel.to_string(),
                parts: vec![p],
                fallback: None,
            }),
            None => Err(DataError::Unresolved(format!(
                "need `{}` of class {}: no version of `{}` is held, and an open-ended span is not bought (the need names `to`)",
                label, demand.class, rel
            ))),
        };
    };
    let mut cands: Vec<Candidate> = fits
        .iter()
        .filter_map(|v| {
            let (a, b) = (v.from?, v.to?);
            Some(Candidate { part: stored(v), from: a, to: b })
        })
        .collect();
    for (g0, g1) in gaps(from, to, held_spans) {
        for (s, k, o) in &offers {
            let a = o.from.as_deref().and_then(parse_timestamp).unwrap_or(i64::MIN).max(g0);
            let b = o.to.as_deref().and_then(parse_timestamp).map(|t| t + NANOS_PER_DAY - 1).unwrap_or(i64::MAX).min(g1);
            if a > b {
                continue;
            }
            let mut attributes = o.attributes.clone();
            attributes.insert("calendar".into(), o.calendar.clone());
            cands.push(Candidate {
                part: Part {
                    need: i,
                    relation: rel.to_string(),
                    source: s.name.clone(),
                    origin: Origin::Fetch(*k),
                    from: Some(a),
                    to: Some(b),
                    cost: estimate(catalog, s, o, 1, a, b),
                    calendar: Some(o.calendar.clone()),
                    attributes,
                },
                from: a,
                to: b,
            });
        }
    }
    let intervals: Vec<Interval<Cost>> = cands
        .iter()
        .map(|c| Interval {
            from: c.from,
            to: c.to,
            cost: c.part.cost,
        })
        .collect();
    let (reach_from, reach_to) = reach_of(from, to);
    match cover(reach_from, reach_to, ABUT, &intervals, |a, b| cost::compare(a, b, order)) {
        Some((_, chain)) => {
            let parts: Vec<Part> = chain.iter().map(|&j| cands[j].part.clone()).collect();
            let buys = parts.iter().any(|p| matches!(p.origin, Origin::Fetch(_)));
            Ok(RelationPlan {
                need: i,
                relation: rel.to_string(),
                parts,
                fallback: if buys { first } else { None },
            })
        }
        None => match first {
            Some(p) => {
                // With no source to buy from, the kernel's precondition on the
                // span says this already (K-3.2).
                if !offers.is_empty() {
                    notes.push(format!(
                        "need `{}`: `{}` is held over {} only, and no source offers the rest of {}..{}",
                        label,
                        rel,
                        match (p.from, p.to) {
                            (Some(a), Some(b)) => format!("{}..{}", date(a), date(b)),
                            _ => "no rows".into(),
                        },
                        date(from),
                        date(to)
                    ));
                }
                Ok(RelationPlan {
                    need: i,
                    relation: rel.to_string(),
                    parts: vec![p],
                    fallback: None,
                })
            }
            None => Err(DataError::Unresolved(format!(
                "need `{}` of class {}: no source covers `{}` over {}..{}",
                label,
                demand.class,
                rel,
                date(from),
                date(to)
            ))),
        },
    }
}

/// The plan of a demand against the store and the catalog's sources.
pub fn plan(store: &Store, catalog: &Catalog, demand: &Demand) -> Result<Planned, DataError> {
    let prefer: Vec<PreferenceKind> = demand.prefer.iter().flatten().map(|p| p.kind).collect();
    let named = demand.prefer.iter().flatten().find(|p| p.kind == PreferenceKind::Version).and_then(|p| p.version.clone());
    let order = cost::order(demand.prefer.as_deref().unwrap_or(&[]));
    let mut out = Planned::default();
    for (i, need) in demand.needs.iter().flatten().enumerate() {
        let label = need.label.clone().unwrap_or_else(|| need.relations.join(", "));
        for rel in &need.relations {
            let rel = if rel.contains('.') { rel.clone() } else { qualified(&demand.class, rel) };
            match relation_plan(store, catalog, demand, i, need, &label, &rel, named.as_deref(), &prefer, &order, &mut out.notes) {
                Ok(rp) => out.relations.push(rp),
                // An optional need's relation that nothing covers is left
                // out of the binding (D-7.1, ERRATA E-72).
                Err(DataError::Unresolved(why)) if need.optional => out.notes.push(format!("`{}` is not bound: {}", rel, why)),
                Err(e) => return Err(e),
            }
        }
    }
    let total = out.relations.iter().flat_map(|r| &r.parts).fold(Cost::default(), |c, p| c + p.cost);
    match cost::exceeds(&total, demand.budget.as_deref()) {
        None => out.cost = total,
        Some(why) => {
            // The budget is a cap (D-7.4): no purchase is made, and a relation
            // a purchase would complete is bound to what the store holds.
            let full: Vec<Part> = out.relations.iter().flat_map(|r| r.parts.clone()).collect();
            for r in &mut out.relations {
                if r.parts.iter().any(|p| matches!(p.origin, Origin::Fetch(_))) {
                    match r.fallback.take() {
                        Some(f) => r.parts = vec![f],
                        None => {
                            return Err(DataError::OutOfBudget(format!(
                                "`{}`: the store holds none of it, and the cheapest plan costs {}: {}",
                                r.relation,
                                describe_cost(&total),
                                why
                            )))
                        }
                    }
                }
            }
            out.notes.push(format!(
                "the cheapest plan that covers every span costs {}: {}; bound to what the store holds",
                describe_cost(&total),
                why
            ));
            out.over_budget = Some((full, total, why));
            out.cost = out.relations.iter().flat_map(|r| &r.parts).fold(Cost::default(), |c, p| c + p.cost);
        }
    }
    Ok(out)
}

/// A cost as the messages state it.
pub fn describe_cost(c: &Cost) -> String {
    format!("money {:.2}, {:.1} s, {} bytes, {} requests", c.money, c.seconds, c.bytes, c.requests)
}
