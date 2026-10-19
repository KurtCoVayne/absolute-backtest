//! Dynamic needs (D-7.6): the chains a program queries per barrier.
//!
//! In replay the pre-pass evaluates each dynamic need's filters on every
//! day the need's spot relation holds a row of the underlying (the days a
//! daily decision calendar queries the chain on; ERRATA E-70), against that
//! row's price, with the one implementation of the filters the data
//! layer's `chain` query and the kernel's `chain` relation read
//! (`abt_wire::chain::admits`). The contracts admitted become a static need
//! of the dynamic need's relation (`option.quote`, `option.greeks`) over
//! the span. A day whose chain the store holds no snapshot of is missing;
//! a source whose offers have the `chain` scope sells it at one request per
//! underlying and day (D-7.7), bought within the demand's money budget
//! (`fill`) or reported over it. A chain the pre-pass did not cover is
//! served on demand by `resolve_dynamic` (P-3.3).

use std::collections::{BTreeMap, BTreeSet};

use abt_wire::generated::abt::wire::{Binding, Demand, DynamicNeed, Need, VersionRef};
use arrow::array::Array;

use super::catalog::Unit;
use super::Sources;
use crate::adapter::ticker_spans;
use crate::adapters::FetchRequest;
use crate::error::DataError;
use crate::options::{chain_ids, contracts};
use crate::query::ResolvedView;
use crate::rows;
use crate::store::{RowFilter, Store};

const DAY: i64 = 86_400_000_000_000;

/// What the pre-pass found of one dynamic need.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChainPlan {
    pub symbol: String,
    pub underlying: u64,
    /// The days the spot relation holds a row on.
    pub days: Vec<i64>,
    /// The days whose chain the store holds no snapshot of.
    pub missing: Vec<i64>,
    /// The contracts the filters admit on some day.
    pub admitted: BTreeSet<u64>,
}

/// The pre-pass's answer: each dynamic need's plan, the static needs they
/// become, and what is reported with the binding.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Prepared {
    pub chains: Vec<ChainPlan>,
    pub needs: Vec<Need>,
    pub notes: Vec<String>,
}

/// The id of a chain's underlying: the need's, else the instrument whose
/// ticker span carries its symbol (the latest span).
fn underlying_of(store: &Store, d: &DynamicNeed) -> Result<(String, u64), DataError> {
    let symbol = d.underlying_symbol.clone().unwrap_or_default();
    if d.underlying != 0 {
        return Ok((if symbol.is_empty() { d.underlying.to_string() } else { symbol }, d.underlying));
    }
    let spans = ticker_spans(store)?;
    let id = spans.get(&symbol).and_then(|v| v.last()).map(|s| s.2).ok_or_else(|| {
        DataError::Identity(format!(
            "the chain's underlying `{}` is carried by no instrument of the store (a chain names its underlying by ticker, D-2.4)",
            symbol
        ))
    })?;
    Ok((symbol, id))
}

/// The spot rows of an underlying: `(t, price)` of the spot relation's
/// current version, in time order.
fn spots(store: &Store, d: &DynamicNeed, underlying: u64) -> Result<Vec<(i64, f64)>, DataError> {
    let relation = d.spot_relation.clone().unwrap_or_else(|| "equity.close".into());
    let Some(v) = store.registry.current(&relation, None).map(str::to_string) else {
        return Err(DataError::Unresolved(format!("the chain's spot `{}` has no version in the store", relation)));
    };
    let s = store.schema(&relation)?;
    let lay = rows::layout(&s);
    let col = match &d.spot_column {
        Some(c) => s.columns.iter().position(|x| x.name == *c),
        None => s.columns.iter().position(|x| x.ty == abt_wire::generated::abt::wire::ColumnType::Num),
    }
    .ok_or_else(|| DataError::Schema(format!("the chain's spot `{}` has no price column", relation)))?;
    let mut out = Vec::new();
    for b in store.read(
        &v,
        &RowFilter {
            identity: Some(vec![underlying]),
            latest_only: true,
            ..RowFilter::default()
        },
    )? {
        let (ts, xs) = (rows::times(&b, lay.t), rows::floats(&b, col));
        for i in 0..b.num_rows() {
            if !xs.is_null(i) {
                out.push((ts.value(i), xs.value(i)));
            }
        }
    }
    out.sort_by_key(|x| x.0);
    Ok(out)
}

/// The pre-pass over the store as it is (D-7.6).
pub fn prepare(store: &Store, demand: &Demand) -> Result<Prepared, DataError> {
    let mut out = Prepared::default();
    let view = ResolvedView::default();
    for d in demand.dynamic.iter().flatten() {
        let (symbol, underlying) = underlying_of(store, d)?;
        let spot = spots(store, d, underlying)?;
        let filters = d.filters.as_deref().cloned().unwrap_or_default();
        let terms = contracts(store, &view, None)?;
        let mut plan = ChainPlan {
            symbol: symbol.clone(),
            underlying,
            ..ChainPlan::default()
        };
        for &(t, s) in &spot {
            plan.days.push(t);
            let Some(ids) = chain_ids(store, &view, underlying, t, None)? else {
                plan.missing.push(t);
                continue;
            };
            for id in ids {
                let Some(c) = terms.get(&id) else { continue };
                let admitted = c.expiry >= t
                    && abt_wire::chain::admits(
                        &filters,
                        t,
                        Some(s),
                        &abt_wire::chain::Terms {
                            expiry: c.expiry,
                            strike: c.strike,
                            right: &c.right,
                            style: &c.style,
                            open_interest: None,
                        },
                    );
                if admitted {
                    plan.admitted.insert(id);
                }
            }
        }
        let (from, to) = (spot.first().map(|x| x.0), spot.last().map(|x| x.0));
        out.needs.push(Need {
            relations: vec![d.relation.clone()],
            calendar: None,
            from,
            to,
            scope: None,
            attributes: None,
            label: Some(format!("chain of {} ({})", symbol, d.relation)),
            optional: false,
        });
        out.notes.push(format!(
            "the pre-pass of the chain of {} ({}): {} contracts admitted over {} days{}",
            symbol,
            d.relation,
            plan.admitted.len(),
            plan.days.len(),
            if plan.missing.is_empty() {
                String::new()
            } else {
                format!(", {} days without a snapshot in the store", plan.missing.len())
            }
        ));
        out.chains.push(plan);
    }
    Ok(out)
}

/// The demand with its dynamic needs' static needs added, their
/// underlyings named by id.
pub fn with_static(demand: &Demand, p: &Prepared) -> Demand {
    let mut d = demand.clone();
    d.needs.get_or_insert_with(Vec::new).extend(p.needs.iter().cloned());
    if let Some(dynamic) = d.dynamic.as_mut() {
        for (n, c) in dynamic.iter_mut().zip(&p.chains) {
            n.underlying = c.underlying;
        }
    }
    d
}

/// The cost of a request to a chain source, in money: its price per
/// request (a chain fetch is one request per underlying and day).
fn request_price(sources: &Sources, s: &super::catalog::Source) -> f64 {
    s.price.iter().filter(|p| p.unit == Unit::Request).map(|p| sources.catalog.money(p)).sum()
}

/// The missing days' chains bought from a source offering chains, when
/// their cost is within `money`; what was bought or why not, as notes.
pub fn fill(store: &mut Store, sources: &Sources, p: &mut Prepared, money: f64, dry_run: bool) -> Result<Vec<String>, DataError> {
    let mut notes = Vec::new();
    for plan in &mut p.chains {
        if plan.missing.is_empty() {
            continue;
        }
        let Some(source) = sources.catalog.sources.iter().find(|s| s.offers.iter().any(|o| o.class == "option" && o.scope == "chain")) else {
            notes.push(format!(
                "no source offers the chains of {}: {} days stay without a snapshot, the kernel's `chain` empty on them",
                plan.symbol,
                plan.missing.len()
            ));
            continue;
        };
        let cost = request_price(sources, source) * plan.missing.len() as f64;
        // An unpriced placeholder (A-8) is never within a budget.
        if !cost.is_finite() || cost > money + 1e-9 {
            notes.push(format!(
                "the chains of {} on {} days cost {:.2} from `{}`, over the money budget {:.2}: not bought",
                plan.symbol,
                plan.missing.len(),
                cost,
                source.name,
                money
            ));
            continue;
        }
        if dry_run {
            notes.push(format!("would buy the chains of {} on {} days from `{}` for {:.2}", plan.symbol, plan.missing.len(), source.name, cost));
            continue;
        }
        let Some(adapter) = sources.adapter(source) else {
            notes.push(format!("the source `{}` has no adapter: the chains of {} are not bought", source.name, plan.symbol));
            continue;
        };
        let (from, to) = (plan.missing[0], *plan.missing.last().unwrap_or(&plan.missing[0]) + DAY - 1);
        let got = adapter.fetch(&FetchRequest {
            source: source.name.clone(),
            class: "option".into(),
            relations: vec!["option.quote".into(), "option.greeks".into()],
            calendar: "1d".into(),
            from,
            to,
            scope: format!("chain({})", plan.symbol),
        })?;
        got.ingest(store, adapter.vendor(), &source.name, &BTreeMap::new())?;
        notes.push(format!("bought the chains of {} on {} days from `{}` for {:.2}", plan.symbol, plan.missing.len(), source.name, cost));
        plan.missing.clear();
    }
    Ok(notes)
}

/// `resolve_dynamic` (P-3.3): the chain of `underlying` on `t`'s day, which
/// the pre-pass did not cover, bought from a source offering chains when
/// a request to it costs nothing (a request carries no budget of its own);
/// the binding again, with the versions of the option class's relations and
/// of the registry's that the purchase extended.
pub fn resolve_dynamic(store: &mut Store, sources: &Sources, binding: &Binding, need: &DynamicNeed, underlying: u64, t: i64) -> Result<Binding, DataError> {
    let (symbol, id) = underlying_of(store, &DynamicNeed { underlying, ..need.clone() })?;
    if chain_ids(store, &ResolvedView::default(), id, t, None)?.is_some() {
        return Ok(binding.clone());
    }
    let source = sources
        .catalog
        .sources
        .iter()
        .find(|s| s.offers.iter().any(|o| o.class == "option" && o.scope == "chain"))
        .ok_or_else(|| DataError::Unresolved(format!("no source offers the chain of {} (`{}`)", symbol, need.relation)))?;
    let price = request_price(sources, source);
    if price != 0.0 {
        return Err(DataError::OutOfBudget(format!(
            "the chain of {} on {} costs {:.2} from `{}`; an on-demand chain is bought only when free, resolve the demand again with a budget",
            symbol,
            abt_wire::scalar::Time(t).iso(),
            price,
            source.name
        )));
    }
    let adapter = sources.adapter(source).ok_or_else(|| DataError::Unresolved(format!("the source `{}` has no adapter", source.name)))?;
    let day = t.div_euclid(DAY) * DAY;
    let got = adapter.fetch(&FetchRequest {
        source: source.name.clone(),
        class: "option".into(),
        relations: vec!["option.quote".into(), "option.greeks".into()],
        calendar: "1d".into(),
        from: day,
        to: day + DAY - 1,
        scope: format!("chain({})", symbol),
    })?;
    let ingested = got.ingest(store, adapter.vendor(), &source.name, &BTreeMap::new())?;
    let mut pairs: BTreeMap<String, String> = binding.pairs.clone().unwrap_or_default().into_iter().map(|p| (p.relation, p.version)).collect();
    for (relation, version) in ingested.versions {
        if pairs.contains_key(&relation) || matches!(relation.as_str(), "option_spec" | "chain_index") {
            pairs.insert(relation, version);
        }
    }
    let mut out = binding.clone();
    out.pairs = Some(pairs.into_iter().map(|(relation, version)| VersionRef { relation, version }).collect());
    out.id = None;
    Ok(out)
}
