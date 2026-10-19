//! A plan's purchases (D-7.5): for each source the plan buys from, its
//! quota checked against the plan's requests, then the parts fetched
//! through the source's adapter (one request for the relations of one
//! offer over one span), ingested under the source's name and sealed into
//! versions, which are the store's from then on (`cached`). The parts then
//! name their versions.

use std::collections::BTreeMap;

use abt_wire::generated::abt::wire::Demand;

use super::plan::{date, Origin, Planned};
use super::Sources;
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
        if sources.adapter(s).is_none() {
            return Err(DataError::Unresolved(format!(
                "the source `{}` is fetched by the adapter `{}`, which this service does not have",
                name, s.adapter
            )));
        }
    }
    // One request per (source, offer, span): the relations a plan buys
    // from one offer over one span are fetched together.
    // (source, offer, from, to) to the (relation, part) indices it buys.
    type Span = (String, usize, Option<i64>, Option<i64>);
    let mut groups: BTreeMap<Span, Vec<(usize, usize)>> = BTreeMap::new();
    for (ri, r) in planned.relations.iter().enumerate() {
        for (pi, p) in r.parts.iter().enumerate() {
            if let Origin::Fetch(k) = p.origin {
                groups.entry((p.source.clone(), k, p.from, p.to)).or_default().push((ri, pi));
            }
        }
    }
    let mut bought = Vec::new();
    // The versions each (relation, source) ends at: a later part from the
    // same source extends the version an earlier one sealed.
    let mut last: BTreeMap<(String, String), String> = BTreeMap::new();
    for ((source, k, from, to), members) in groups {
        let s = sources.catalog.source(&source).expect("checked above");
        let adapter = sources.adapter(s).expect("checked above");
        let offer = &s.offers[k];
        let (t0, t1) = (from.unwrap_or(i64::MIN), to.unwrap_or(i64::MAX));
        let mut relations: Vec<String> = members.iter().map(|&(ri, pi)| planned.relations[ri].parts[pi].relation.clone()).collect();
        relations.sort();
        relations.dedup();
        let req = FetchRequest {
            source: s.name.clone(),
            class: demand.class.clone(),
            relations: relations.clone(),
            calendar: offer.calendar.clone(),
            from: t0,
            to: t1,
            scope: offer.scope.clone(),
        };
        // What the adapter returns beside the relations asked for (the
        // registry's relations that key them) is ingested too.
        let got = adapter.fetch(&req)?;
        let (ri, pi) = members[0];
        let offered = planned.relations[ri].parts[pi].attributes.clone();
        let ingested = got.ingest(store, adapter.vendor(), &s.name, &offered)?;
        for &(ri, pi) in &members {
            let p = &mut planned.relations[ri].parts[pi];
            let version = ingested
                .versions
                .get(&p.relation)
                .cloned()
                .ok_or_else(|| DataError::NotFound(format!("`{}` returned no rows of `{}` over {}..{}", s.name, p.relation, date(t0), date(t1))))?;
            bought.push(format!(
                "bought `{}` from `{}` over {}..{}: version {}",
                p.relation,
                s.name,
                date(t0),
                date(t1),
                &version[..12.min(version.len())]
            ));
            last.insert((p.relation.clone(), s.name.clone()), version);
            let mut attributes = offered.clone();
            attributes.extend(got.attributes_of(&p.relation));
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
