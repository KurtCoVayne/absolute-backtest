//! A plan's binding (D-7.5): each relation at one version, the
//! registry's identity relations at theirs, and the demand's point. A
//! relation whose span is covered by parts of several versions is bound
//! to a version of the parts' rows merged under the source `cover`, each
//! part contributing its share of the span (from the end of the part
//! before it to its own end), so a binding still names one version per
//! relation (ERRATA E-67).

use std::collections::BTreeMap;

use abt_wire::generated::abt::wire::{Attribute, Binding, Demand, PlanStep, VersionRef};

use super::plan::{date, Origin, Part, Planned, IDENTITY};
use crate::adapter::REGISTRY_SOURCE;
use crate::error::DataError;
use crate::store::{RowFilter, Store};

/// The source a merged version is written under.
pub const COVER: &str = "cover";

fn version_of(p: &Part) -> Result<&str, DataError> {
    match &p.origin {
        Origin::Stored(v) => Ok(v),
        Origin::Fetch(_) => Err(DataError::Malformed(format!("`{}` from `{}` is bound before it is bought", p.relation, p.source))),
    }
}

/// The version a relation's parts are bound at.
fn merged(store: &mut Store, relation: &str, parts: &[Part]) -> Result<String, DataError> {
    if let [one] = parts {
        return Ok(version_of(one)?.to_string());
    }
    let mut rows = Vec::new();
    let mut shares = Vec::new();
    let mut after: Option<i64> = None;
    for p in parts {
        let v = version_of(p)?;
        let f = RowFilter {
            t0: after.map(|a| a + 1),
            t1: p.to,
            ..RowFilter::default()
        };
        rows.extend(store.read(v, &f)?);
        shares.push(format!("{}@{} to {}", p.source, &v[..12.min(v.len())], p.to.map(date).unwrap_or_else(|| "the end".into())));
        after = p.to;
    }
    // What every part states alike is stated of the whole.
    let mut attributes: BTreeMap<String, String> = parts[0].attributes.clone();
    attributes.retain(|k, v| parts.iter().all(|p| p.attributes.get(k) == Some(v)));
    attributes.insert("cover".into(), shares.join("; "));
    store.ingest_stored(relation, &rows, COVER, attributes)
}

/// The binding of a plan whose purchases are made, and the plan's steps.
pub fn bind(store: &mut Store, demand: &Demand, planned: &Planned) -> Result<(Binding, Vec<PlanStep>), DataError> {
    let mut pairs: BTreeMap<String, String> = BTreeMap::new();
    let mut steps = Vec::new();
    for r in &planned.relations {
        let v = merged(store, &r.relation, &r.parts)?;
        pairs.insert(r.relation.clone(), v);
        for p in &r.parts {
            steps.push(PlanStep {
                need: p.need as u32,
                source: p.source.clone(),
                relation: p.relation.clone(),
                version: Some(version_of(p)?.to_string()),
                cost: Some(Box::new(p.cost.wire())),
                from: p.from,
                to: p.to,
                attributes: Some(
                    p.attributes
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
        if let Some(v) = store.registry.current(rel, Some(REGISTRY_SOURCE)) {
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
    Ok((binding, steps))
}

/// The steps of parts not all bought (a dry run, a plan over budget): a
/// part to buy names no version yet.
pub fn part_steps<'a>(parts: impl IntoIterator<Item = &'a Part>) -> Vec<PlanStep> {
    parts
        .into_iter()
        .map(|p| PlanStep {
            need: p.need as u32,
            source: p.source.clone(),
            relation: p.relation.clone(),
            version: match &p.origin {
                Origin::Stored(v) => Some(v.clone()),
                Origin::Fetch(_) => None,
            },
            cost: Some(Box::new(p.cost.wire())),
            from: p.from,
            to: p.to,
            attributes: Some(
                p.attributes
                    .iter()
                    .filter(|(k, _)| !k.starts_with('_'))
                    .map(|(key, value)| Attribute {
                        key: key.clone(),
                        value: value.clone(),
                    })
                    .collect(),
            ),
        })
        .collect()
}

/// The steps of a plan before its purchases.
pub fn steps(planned: &Planned) -> Vec<PlanStep> {
    part_steps(planned.relations.iter().flat_map(|r| &r.parts))
}
