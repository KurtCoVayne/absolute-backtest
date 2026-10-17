//! A plan's purchases (D-7.5): for each source the plan buys from, its
//! quota checked against the plan's requests, then each part fetched
//! through the source's adapter, ingested under the source's name and
//! sealed into a version, which is the store's from then on (`cached`).
//! The parts then name their versions.

use std::collections::BTreeMap;

use abt_wire::generated::abt::wire::Demand;

use super::plan::{date, Origin, Planned};
use super::Sources;
use crate::adapter::ingest_keyed;
use crate::adapters::FetchRequest;
use crate::error::DataError;
use crate::store::Store;

/// Makes the plan's purchases; returns what was bought, one line each.
pub fn fetch(store: &mut Store, sources: &Sources, demand: &Demand, planned: &mut Planned) -> Result<Vec<String>, DataError> {
    // The quota first, for every source at once: nothing is bought when a
    // source's requests would exceed it.
    let mut requests: BTreeMap<&str, u64> = BTreeMap::new();
    for p in planned.relations.iter().flat_map(|r| &r.parts) {
        if matches!(p.origin, Origin::Fetch(_)) {
            *requests.entry(p.source.as_str()).or_default() += p.cost.requests;
        }
    }
    for (name, n) in &requests {
        let s = sources
            .catalog
            .source(name)
            .ok_or_else(|| DataError::Unresolved(format!("the plan buys from `{}`, which the catalog does not list", name)))?;
        if let Some(limit) = s.quota.as_ref().and_then(|q| q.requests.map(|r| (r, &q.window))) {
            if *n > limit.0 {
                return Err(DataError::OutOfBudget(format!(
                    "the plan makes {} requests of `{}`, whose quota is {} per {}",
                    n, name, limit.0, limit.1
                )));
            }
        }
        if !sources.adapters.contains_key(&s.adapter) {
            return Err(DataError::Unresolved(format!(
                "the source `{}` is fetched by the adapter `{}`, which this service does not have",
                name, s.adapter
            )));
        }
    }
    let mut bought = Vec::new();
    // The versions each (relation, source) ends at: a later part from the
    // same source extends the version an earlier one sealed.
    let mut last: BTreeMap<(String, String), String> = BTreeMap::new();
    let class = demand.class.clone();
    for r in &mut planned.relations {
        for p in &mut r.parts {
            let Origin::Fetch(k) = p.origin else { continue };
            let s = sources.catalog.source(&p.source).expect("checked above");
            let adapter = sources.adapters.get(&s.adapter).expect("checked above");
            let offer = &s.offers[k];
            let (from, to) = (p.from.unwrap_or(i64::MIN), p.to.unwrap_or(i64::MAX));
            let req = FetchRequest {
                source: s.name.clone(),
                class: class.clone(),
                relations: vec![p.relation.clone()],
                calendar: offer.calendar.clone(),
                from,
                to,
                scope: offer.scope.clone(),
            };
            let got = adapter.fetch(&req)?;
            let mut attributes = p.attributes.clone();
            attributes.extend(got.attributes.clone());
            let ingested = ingest_keyed(store, adapter.vendor(), &got.securities, &got.terms, &got.relations, &s.name, &attributes)?;
            let version = ingested
                .versions
                .get(&p.relation)
                .cloned()
                .ok_or_else(|| DataError::NotFound(format!("`{}` returned no rows of `{}` over {}..{}", s.name, p.relation, date(from), date(to))))?;
            bought.push(format!(
                "bought `{}` from `{}` over {}..{}: version {}",
                p.relation,
                s.name,
                date(from),
                date(to),
                &version[..12.min(version.len())]
            ));
            last.insert((p.relation.clone(), s.name.clone()), version);
            p.attributes = attributes;
        }
    }
    for r in &mut planned.relations {
        for p in &mut r.parts {
            if let Origin::Fetch(_) = p.origin {
                if let Some(v) = last.get(&(p.relation.clone(), p.source.clone())) {
                    p.origin = Origin::Stored(v.clone());
                }
            }
        }
    }
    Ok(bought)
}
