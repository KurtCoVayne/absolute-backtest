//! The resolver (D-7): a demand's needs bound at the versions the store
//! holds, or bought from the sources of the catalog, at the least cost the
//! demand's preferences order, within its budget.
//!
//! - `catalog`: the sources, their offers, prices and quotas (D-7.1), from
//!   a configuration file; the store is a source of every price zero.
//! - `cost`: the cost vector, its orders, the budget (D-7.3).
//! - `cover`: the cheapest cover of a span (Proposition D-7.4).
//! - `plan`: each need's candidates and its cover, within the budget.
//! - `fetch`: the plan's purchases through the adapters (D-7.5).
//! - `bind`: the binding, one version per relation.
//! - `prepass`: dynamic needs (D-7.6).

use std::collections::BTreeMap;
use std::sync::Arc;

use abt_wire::generated::abt::wire::{Binding, Demand, ResolvePlan};

use crate::adapters::Adapter;
use crate::error::DataError;
use crate::store::Store;

pub mod bind;
pub mod catalog;
pub mod cost;
pub mod cover;
pub mod fetch;
pub mod plan;
pub mod prepass;

pub use catalog::Catalog;
pub use plan::{calendar_nanos, IDENTITY};

/// What a resolution may draw on beside the store: the catalog's sources
/// and the adapters they are fetched by, by the source's name (an adapter
/// its settings configure) or the adapter's.
#[derive(Clone, Default)]
pub struct Sources {
    pub catalog: Catalog,
    pub adapters: BTreeMap<String, Arc<dyn Adapter>>,
}

impl Sources {
    /// The store alone.
    pub fn store_only() -> Sources {
        Sources::default()
    }

    /// The adapter a source is fetched by: the one configured for it, else
    /// the one of its adapter's name.
    pub fn adapter(&self, source: &catalog::Source) -> Option<&Arc<dyn Adapter>> {
        self.adapters.get(&source.name).or_else(|| self.adapters.get(&source.adapter))
    }
}

/// A demand resolved.
#[derive(Clone, Debug)]
pub struct Resolution {
    pub binding: Binding,
    pub plan: ResolvePlan,
    /// The cheapest plan the budget did not allow, when the binding is the
    /// store's instead.
    pub over_budget: Option<ResolvePlan>,
    /// What the resolution bought, and what it could not cover.
    pub notes: Vec<String>,
}

fn wire_plan(steps: Vec<abt_wire::generated::abt::wire::PlanStep>, cost: &cost::Cost) -> ResolvePlan {
    ResolvePlan {
        steps: Some(steps),
        cost: Some(Box::new(cost.wire())),
    }
}

/// The binding of a demand against the store alone, and its plan (what
/// the store's versions satisfy, nothing bought).
pub fn resolve(store: &Store, demand: &Demand) -> Result<(Binding, ResolvePlan), DataError> {
    let prepared = prepass::prepare(store, demand)?;
    let demand = &prepass::with_static(demand, &prepared);
    let planned = plan::plan(store, &Catalog::store_only(), demand)?;
    // Against the store alone every part is a version it holds, so binding
    // writes nothing; it takes the store mutably for a merge only.
    let mut pairs: BTreeMap<String, String> = BTreeMap::new();
    for r in &planned.relations {
        if let [p] = r.parts.as_slice() {
            if let plan::Origin::Stored(v) = &p.origin {
                pairs.insert(r.relation.clone(), v.clone());
                continue;
            }
        }
        return Err(DataError::Malformed(format!("`{}`: a store-only plan of several parts", r.relation)));
    }
    for rel in IDENTITY {
        if let Some(v) = store.registry.current(rel, Some(crate::adapter::REGISTRY_SOURCE)) {
            pairs.insert(rel.to_string(), v.to_string());
        }
    }
    let mut point = demand.point.clone().unwrap_or_default();
    point.sort_by(|a, b| a.name.cmp(&b.name));
    let binding = Binding {
        pairs: Some(pairs.into_iter().map(|(relation, version)| abt_wire::generated::abt::wire::VersionRef { relation, version }).collect()),
        point: (!point.is_empty()).then_some(point),
        dynamic: demand.dynamic.clone(),
        id: None,
    };
    Ok((binding, wire_plan(bind::steps(&planned), &planned.cost)))
}

/// The resolution of a demand (D-7.4, D-7.5): its plan; unless a dry run,
/// its purchases made and ingested, and its binding (which the caller
/// records, pinning its versions).
pub fn resolve_with(store: &mut Store, sources: &Sources, demand: &Demand, dry_run: bool) -> Result<Resolution, DataError> {
    // The dynamic needs' chains first (D-7.6): what the store lacks of them
    // bought within the money budget, then their contracts a static need.
    let mut prepared = prepass::prepare(store, demand)?;
    let money = demand.budget.as_ref().map(|b| b.money).unwrap_or(0.0);
    let mut chain_notes = prepass::fill(store, sources, &mut prepared, money, dry_run)?;
    if !dry_run && chain_notes.iter().any(|n| n.starts_with("bought")) {
        prepared = prepass::prepare(store, demand)?;
    }
    chain_notes.extend(prepared.notes.iter().cloned());
    let demand = &prepass::with_static(demand, &prepared);
    let mut planned = plan::plan(store, &sources.catalog, demand)?;
    let over_budget = planned.over_budget.as_ref().map(|(parts, cost, _)| wire_plan(bind::part_steps(parts), cost));
    let mut notes = chain_notes;
    notes.extend(planned.notes.clone());
    if dry_run {
        // The binding a dry run reports names the versions the store holds;
        // a part to buy is in the plan, unbound.
        let mut pairs = BTreeMap::new();
        for r in &planned.relations {
            if let [p] = r.parts.as_slice() {
                if let plan::Origin::Stored(v) = &p.origin {
                    pairs.insert(r.relation.clone(), v.clone());
                }
            }
        }
        let binding = Binding {
            pairs: Some(pairs.into_iter().map(|(relation, version)| abt_wire::generated::abt::wire::VersionRef { relation, version }).collect()),
            point: None,
            dynamic: demand.dynamic.clone(),
            id: None,
        };
        return Ok(Resolution {
            binding,
            plan: wire_plan(bind::steps(&planned), &planned.cost),
            over_budget,
            notes,
        });
    }
    notes.extend(fetch::fetch(store, sources, demand, &mut planned)?);
    let (binding, steps) = bind::bind(store, demand, &planned)?;
    Ok(Resolution {
        binding,
        plan: wire_plan(steps, &planned.cost),
        over_budget,
        notes,
    })
}
