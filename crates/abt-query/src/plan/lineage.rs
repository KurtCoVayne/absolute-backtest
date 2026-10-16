//! Lineages and disputes as rows (Q-2.5, A-D20). A program belongs to the
//! lineage it is a member of, or to the nearest lineage by the Jaccard
//! similarity of its normalised rules when that is at least 0.8 (attached
//! with a warning, so that renaming is not a way to reset a count), or
//! starts its own. Members are rows of `query.lineages`, disputes of
//! `query.disputes`. A lineage's trials are every run ever made of its
//! members, in or out of a plan (Proposition Q-2.2: the kernel's
//! `binding` of every run), and every test and claim judged on them.

use std::collections::{BTreeMap, BTreeSet};

use abt_wire::generated::abt::wire::Role;

use crate::data::{Data, Error};
use crate::kernel::Checked;
use crate::relation::{self, Col, Rows};

/// The similarity at or above which a new program is attached to an
/// existing lineage.
pub const SIMILARITY_THRESHOLD: f64 = 0.8;

pub const LINEAGES: &str = "query.lineages";
pub const DISPUTES: &str = "query.disputes";
pub const TRIALS: &str = "query.trials";

/// A member of a lineage.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct Member {
    pub lineage: String,
    pub program_hash: String,
    pub strategy: String,
    /// `root`, or `similar` to `similar_to` at `similarity`.
    pub attachment: String,
    pub similar_to: Option<String>,
    pub similarity: Option<f64>,
    pub rules: Vec<String>,
    pub at: i64,
}

/// A dispute of a member's attachment.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct Dispute {
    pub program_hash: String,
    pub lineage: String,
    pub reason: String,
    pub at: i64,
}

/// The lineage a program was found in or attached to.
#[derive(Clone, Debug, PartialEq)]
pub struct Opened {
    pub lineage: String,
    pub existing: bool,
    pub warning: Option<String>,
}

/// The rules of a program's normal form (the kernel's rendering, its
/// variables canonical and its bodies in canonical order), each on one
/// line, sorted.
pub fn rules(normal_form: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    for line in normal_form.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if cur.is_empty() && !line.contains(":-") {
            continue;
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(line);
        if cur.ends_with('.') {
            out.push(std::mem::take(&mut cur));
        }
    }
    out.sort();
    out.dedup();
    out
}

pub fn jaccard(a: &[String], b: &[String]) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let inter = a.iter().filter(|x| b.contains(x)).count();
    let union = a.len() + b.len() - inter;
    inter as f64 / union as f64
}

/// Every member of every lineage.
pub fn members(data: &Data) -> Result<Vec<Member>, Error> {
    let Some(b) = relation::read(data, LINEAGES)? else { return Ok(vec![]) };
    let (l, h, s, a, to, sim, r, t) = (
        relation::texts(&b, "lineage")?,
        relation::texts(&b, "program_hash")?,
        relation::texts(&b, "strategy")?,
        relation::texts(&b, "attachment")?,
        relation::texts(&b, "similar_to")?,
        relation::nums(&b, "similarity")?,
        relation::texts(&b, "rules")?,
        relation::ints(&b, "t")?,
    );
    let mut out: Vec<Member> = (0..b.num_rows())
        .map(|i| Member {
            lineage: l[i].clone().unwrap_or_default(),
            program_hash: h[i].clone().unwrap_or_default(),
            strategy: s[i].clone().unwrap_or_default(),
            attachment: a[i].clone().unwrap_or_default(),
            similar_to: to[i].clone(),
            similarity: sim[i],
            rules: r[i].as_deref().and_then(|j| serde_json::from_str(j).ok()).unwrap_or_default(),
            at: t[i].unwrap_or(0),
        })
        .collect();
    out.sort_by(|x, y| (x.at, &x.program_hash).cmp(&(y.at, &y.program_hash)));
    Ok(out)
}

fn put_member(data: &Data, m: &Member) -> Result<(), Error> {
    let (schema, batch) = Rows::new(LINEAGES)
        .col("program_hash", Role::Label, Col::Label(vec![Some(m.program_hash.clone())]))
        .col("t", Role::EventTime, Col::Time(vec![Some(m.at)]))
        .col("lineage", Role::Value, Col::Label(vec![Some(m.lineage.clone())]))
        .col("strategy", Role::Value, Col::Bytes(vec![Some(m.strategy.clone())]))
        .col("attachment", Role::Value, Col::Label(vec![Some(m.attachment.clone())]))
        .col("similar_to", Role::Value, Col::Label(vec![m.similar_to.clone()]))
        .col("similarity", Role::Value, Col::Num(vec![m.similarity]))
        .col("rules", Role::Value, Col::Bytes(vec![Some(serde_json::to_string(&m.rules).unwrap_or_default())]))
        .finish()?;
    data.put(schema, &batch, false).map(|_| ())
}

/// The lineage of the checked program: the one it is a member of, or the
/// nearest by its rules (attached with a warning), or its own (a new
/// member row in either of the last two cases).
pub fn open(data: &Data, checked: &Checked, strategy: &str, now: i64) -> Result<Opened, Error> {
    let all = members(data)?;
    if let Some(m) = all.iter().find(|m| m.program_hash == checked.program_hash) {
        return Ok(Opened {
            lineage: m.lineage.clone(),
            existing: true,
            warning: None,
        });
    }
    let mine = rules(&checked.normal_form);
    let mut best: Option<(&Member, f64)> = None;
    for m in &all {
        let score = jaccard(&mine, &m.rules);
        if score >= SIMILARITY_THRESHOLD && best.is_none_or(|(_, b)| score > b) {
            best = Some((m, score));
        }
    }
    let (member, warning) = match best {
        Some((to, score)) => (
            Member {
                lineage: to.lineage.clone(),
                program_hash: checked.program_hash.clone(),
                strategy: strategy.to_string(),
                attachment: "similar".into(),
                similar_to: Some(to.program_hash.clone()),
                similarity: Some(score),
                rules: mine,
                at: now,
            },
            Some(format!(
                "strategy `{}` is new, but {:.0}% of its rules are those of `{}` ({}): it joins lineage {} and its trials count there (dispute with `abt dispute`)",
                strategy,
                score * 100.0,
                to.strategy,
                &to.program_hash[..16.min(to.program_hash.len())],
                &to.lineage[..16.min(to.lineage.len())]
            )),
        ),
        None => (
            Member {
                lineage: checked.program_hash.clone(),
                program_hash: checked.program_hash.clone(),
                strategy: strategy.to_string(),
                attachment: "root".into(),
                similar_to: None,
                similarity: None,
                rules: mine,
                at: now,
            },
            None,
        ),
    };
    put_member(data, &member)?;
    Ok(Opened {
        lineage: member.lineage,
        existing: false,
        warning,
    })
}

/// Every dispute.
pub fn disputes(data: &Data) -> Result<Vec<Dispute>, Error> {
    let Some(b) = relation::read(data, DISPUTES)? else { return Ok(vec![]) };
    let (h, l, r, t) = (
        relation::texts(&b, "program_hash")?,
        relation::texts(&b, "lineage")?,
        relation::texts(&b, "reason")?,
        relation::ints(&b, "t")?,
    );
    Ok((0..b.num_rows())
        .map(|i| Dispute {
            program_hash: h[i].clone().unwrap_or_default(),
            lineage: l[i].clone().unwrap_or_default(),
            reason: r[i].clone().unwrap_or_default(),
            at: t[i].unwrap_or(0),
        })
        .collect())
}

/// A dispute of the attachment of the program `program_hash`: a row; the
/// attachment and the counts stand until a reviewer settles it.
pub fn dispute(data: &Data, program_hash: &str, reason: &str, now: i64) -> Result<Dispute, Error> {
    let m = members(data)?
        .into_iter()
        .find(|m| m.program_hash == program_hash)
        .ok_or_else(|| Error::Missing(format!("program {} is in no lineage (run it in a plan first)", program_hash)))?;
    let (schema, batch) = Rows::new(DISPUTES)
        .col("program_hash", Role::Label, Col::Label(vec![Some(program_hash.to_string())]))
        .col("t", Role::EventTime, Col::Time(vec![Some(now)]))
        .col("lineage", Role::Value, Col::Label(vec![Some(m.lineage.clone())]))
        .col("reason", Role::Value, Col::Bytes(vec![Some(reason.to_string())]))
        .finish()?;
    data.put(schema, &batch, false)?;
    Ok(Dispute {
        program_hash: program_hash.to_string(),
        lineage: m.lineage,
        reason: reason.to_string(),
        at: now,
    })
}

/// A lineage's trials (Proposition Q-2.2): every run of a member the kernel
/// recorded, in or out of a plan, and every test and claim judged on one.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
pub struct Counted {
    pub count: usize,
    pub by_kind: BTreeMap<String, usize>,
    /// The per-period Sharpe ratios of the runs judged in plans (the DSR's
    /// variance, Q-3.2).
    pub sharpes: Vec<f64>,
    pub members: Vec<String>,
}

pub fn trials(data: &Data, lineage: &str) -> Result<Counted, Error> {
    let members: BTreeSet<String> = members(data)?.into_iter().filter(|m| m.lineage == lineage).map(|m| m.program_hash).collect();
    let mut runs: BTreeSet<String> = BTreeSet::new();
    for (rel, _) in data.relations("kernel.", ".binding")? {
        let Some(b) = relation::read(data, &rel)? else { continue };
        let hash = relation::texts(&b, "program_hash")?.into_iter().flatten().next().unwrap_or_default();
        if members.contains(&hash) {
            if let Some(run) = rel.strip_prefix("kernel.").and_then(|r| r.strip_suffix(".binding")) {
                runs.insert(run.to_string());
            }
        }
    }
    let mut by_kind: BTreeMap<String, usize> = BTreeMap::new();
    let mut judged: BTreeMap<String, (String, Option<f64>)> = BTreeMap::new();
    if let Some(b) = relation::read(data, TRIALS)? {
        let (id, kind, program, sharpe) = (
            relation::texts(&b, "trial_id")?,
            relation::texts(&b, "kind")?,
            relation::texts(&b, "program_hash")?,
            relation::nums(&b, "sharpe")?,
        );
        for i in 0..b.num_rows() {
            if program[i].as_ref().is_some_and(|p| members.contains(p)) {
                if let (Some(id), Some(k)) = (&id[i], &kind[i]) {
                    judged.entry(id.clone()).or_insert((k.clone(), sharpe[i]));
                }
            }
        }
    }
    let mut sharpes = Vec::new();
    let mut count = 0;
    for (id, (kind, sharpe)) in &judged {
        *by_kind.entry(kind.clone()).or_default() += 1;
        if kind == "trial" || kind == "reveal" {
            if let Some(s) = sharpe.filter(|s| s.is_finite()) {
                sharpes.push(s);
            }
            // A run judged in a plan is a run of the kernel's.
            runs.remove(id);
        }
        count += 1;
    }
    // The runs no plan judged: trials of kind `run`.
    if !runs.is_empty() {
        by_kind.insert("run".into(), runs.len());
    }
    count += runs.len();
    Ok(Counted {
        count,
        by_kind,
        sharpes,
        members: members.into_iter().collect(),
    })
}
