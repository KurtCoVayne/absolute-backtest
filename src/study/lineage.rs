//! Lineage (data-bundle doc, section 6): a strategy revision belongs to the
//! lineage it declares with `revises "<hash>"`, and a strategy whose rules
//! are near enough an existing lineage's is attached to it anyway, with a
//! warning, so that renaming is not a way to reset a trial count.
//!
//! The program hash is over a normalised rendering of the checked program:
//! environment and version, resolution, mode, parameters with their types,
//! defaults and ranges, declared relations, the strategy's rules with their
//! variables renamed in order of first appearance, and the library rules
//! the program carries. The strategy's name is not part of it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::check::Program;
use crate::ir::{Builtin, Literal, Rule, WindowKind};

/// Jaccard similarity at or above which a new strategy is attached to an
/// existing lineage (section 10, open question 5: 0.8 by default).
pub const SIMILARITY_THRESHOLD: f64 = 0.8;

fn fnv1a(text: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// A literal rendered in full (`Literal::describe` elides aggregation
/// groups and reductions' orders).
fn render_literal(l: &Literal) -> String {
    match l {
        Literal::Atom(a) => a.to_string(),
        Literal::Neg(a) => format!("not {}", a),
        Literal::Builtin(b, _) => match b {
            Builtin::Prev { t, t1 } => format!("prev({}, {})", t, t1),
            Builtin::Lag { t, n, t1 } => format!("lag({}, {}, {})", t, n, t1),
            Builtin::MonthStart { t } => format!("month_start({})", t),
            Builtin::DayStart { t } => format!("day_start({})", t),
        },
        Literal::Window { var, kind, base, dur, min, .. } => {
            format!("{} in {}({}, {}, min {})", var, if *kind == WindowKind::Window { "window" } else { "prior_window" }, base, dur, min)
        }
        Literal::Cmp { op, lhs, rhs, .. } => format!("{} {} {}", lhs, op, rhs),
        Literal::Assign { var, expr, .. } => format!("{} = {}", var, expr),
        Literal::Agg { var, agg, args, conj, .. } => {
            let a: Vec<String> = args.iter().map(|e| e.to_string()).collect();
            let c: Vec<String> = conj.iter().map(render_literal).collect();
            format!("{} = {}({}) over ({})", var, agg, a.join(", "), c.join(", "))
        }
        Literal::Top { n, atom, by, rank, .. } => {
            let order = by
                .as_ref()
                .map(|b| b.iter().map(|(k, d, _)| format!("{} {:?}", k, d).to_lowercase()).collect::<Vec<_>>().join(", "))
                .unwrap_or_default();
            match (n, rank) {
                (Some(n), _) => format!("top({}, {}, by ({}))", n, atom, order),
                (None, Some((k, ties, _))) => {
                    let ties = if *ties == crate::ir::Ties::Average { "average" } else { "ordinal" };
                    format!("rank({}, by ({}), ties {}, as {})", atom, order, ties, k)
                }
                (None, None) => format!("top(?, {}, by ({}))", atom, order),
            }
        }
        Literal::Resample { inner, to, as_var, min, aggs, .. } => {
            let a: Vec<String> = aggs.iter().map(|(x, f, e)| format!("{} = {}({})", x, f, e)).collect();
            format!("resample({} to {} as {}, min {}, {})", inner, to, as_var, min, a.join(", "))
        }
        Literal::AsOf { atom, at, .. } => format!("{} asof {}", atom, at),
    }
}

/// A rule's text with its variables renamed V0, V1, ... in order of first
/// appearance, so that renaming a variable does not change the hash.
pub fn normalize_rule(rule: &Rule) -> String {
    let body: Vec<String> = rule.body.iter().map(render_literal).collect();
    let text = format!("{} :- {}.", rule.head, body.join(", "));
    rename_variables(&text)
}

fn rename_variables(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut names: BTreeMap<String, usize> = BTreeMap::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '"' {
            // A string literal is copied verbatim.
            let start = i;
            i += 1;
            while i < chars.len() && chars[i] != '"' {
                i += 1;
            }
            i = (i + 1).min(chars.len());
            out.extend(&chars[start..i]);
            continue;
        }
        if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            // A variable starts with an upper-case letter (the parser's rule);
            // `_` alone is a wildcard, left as is.
            let prev_is_digit = start > 0 && chars[start - 1].is_ascii_digit();
            if word.chars().next().map(|c| c.is_ascii_uppercase()).unwrap_or(false) && !prev_is_digit {
                let n = names.len();
                let id = *names.entry(word).or_insert(n);
                out.push_str(&format!("V{}", id));
            } else {
                out.push_str(&word);
            }
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// The strategy's own rules, normalised: the set the similarity compares.
pub fn normalized_rules(prog: &Program) -> Vec<String> {
    let mut v: Vec<String> = prog.rules.iter().filter(|r| r.unit == prog.strategy).map(normalize_rule).collect();
    v.sort();
    v.dedup();
    v
}

/// The normalised text the program hash is over.
pub fn normalized_text(prog: &Program) -> String {
    let mut out = String::new();
    out.push_str(&format!("env {}", prog.environment));
    if let Some(v) = &prog.environment_version {
        out.push_str(&format!("@{}", v));
    }
    out.push_str(&format!("\nresolution {}\nmode {}\n", prog.resolution, prog.mode));
    for (unit, params) in &prog.params {
        let label = if *unit == prog.strategy { "self" } else { unit.as_str() };
        for (name, p) in params {
            out.push_str(&format!("param {}::{} : {} = {}", label, name, p.ty, p.value));
            if let Some((lo, hi)) = &p.range {
                out.push_str(&format!(" in {}..{}", lo, hi));
            }
            out.push('\n');
        }
    }
    for (name, sig) in &prog.relations {
        let args: Vec<String> = sig.args.iter().map(|a| format!("{:?}{}:{}", a.mode, a.name, a.ty).to_lowercase()).collect();
        out.push_str(&format!("rel {}({})\n", name, args.join(", ")));
    }
    for r in &prog.rules {
        let label = if r.unit == prog.strategy { "self" } else { r.unit.as_str() };
        out.push_str(&format!("{}: {}\n", label, normalize_rule(r)));
    }
    out
}

/// The program hash: 16 hex digits of FNV-1a over the normalised text.
pub fn program_hash(prog: &Program) -> String {
    format!("{:016x}", fnv1a(&normalized_text(prog)))
}

/// Jaccard similarity of two rule sets.
pub fn jaccard(a: &[String], b: &[String]) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let inter = a.iter().filter(|x| b.contains(x)).count();
    let union = a.len() + b.len() - inter;
    inter as f64 / union as f64
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Attachment {
    /// The first member of its lineage.
    Root,
    /// Declared with `revises`.
    Revises(String),
    /// Attached by the checker's similarity comparison.
    Similarity { to: String, score: f64 },
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Member {
    pub hash: String,
    pub strategy: String,
    pub rules: Vec<String>,
    pub attachment: Attachment,
    pub at: u64,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Dispute {
    pub hash: String,
    pub reason: String,
    pub at: u64,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Lineage {
    /// The root member's hash.
    pub id: String,
    pub members: Vec<Member>,
    pub disputes: Vec<Dispute>,
}

/// What opening a lineage for a program decided.
#[derive(Clone, Debug, PartialEq)]
pub struct Opened {
    pub lineage: String,
    pub hash: String,
    pub attachment: Attachment,
    /// Set when the program was already a member.
    pub existing: bool,
    /// The similarity warning, when the attachment was not declared.
    pub warning: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Lineages {
    pub lineages: Vec<Lineage>,
}

impl Lineages {
    pub fn load(path: &Path) -> Result<Lineages, String> {
        if !path.exists() {
            return Ok(Lineages::default());
        }
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {}", path.display(), e))?;
        serde_json::from_str(&text).map_err(|e| format!("{}: {}", path.display(), e))
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {}", dir.display(), e))?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(path, text).map_err(|e| format!("{}: {}", path.display(), e))
    }

    pub fn find_member(&self, hash: &str) -> Option<(&Lineage, &Member)> {
        self.lineages.iter().find_map(|l| l.members.iter().find(|m| m.hash == hash).map(|m| (l, m)))
    }

    pub fn lineage_of(&self, hash: &str) -> Option<&Lineage> {
        self.find_member(hash).map(|(l, _)| l)
    }

    /// Open the lineage for a checked program: its own, the one it revises,
    /// the nearest one by similarity above the threshold, or a new one.
    pub fn open(&mut self, prog: &Program, now: u64) -> Result<Opened, String> {
        let hash = program_hash(prog);
        let rules = normalized_rules(prog);
        if let Some((l, m)) = self.find_member(&hash) {
            return Ok(Opened {
                lineage: l.id.clone(),
                hash,
                attachment: m.attachment.clone(),
                existing: true,
                warning: None,
            });
        }
        if let Some(target) = &prog.revises {
            let Some(l) = self.lineages.iter_mut().find(|l| l.members.iter().any(|m| &m.hash == target)) else {
                return Err(format!("`revises \"{}\"`: no lineage holds that program hash", target));
            };
            let attachment = Attachment::Revises(target.clone());
            l.members.push(Member {
                hash: hash.clone(),
                strategy: prog.strategy.clone(),
                rules,
                attachment: attachment.clone(),
                at: now,
            });
            return Ok(Opened {
                lineage: l.id.clone(),
                hash,
                attachment,
                existing: false,
                warning: None,
            });
        }
        let mut best: Option<(usize, String, f64)> = None;
        for (i, l) in self.lineages.iter().enumerate() {
            for m in &l.members {
                let score = jaccard(&rules, &m.rules);
                if score >= SIMILARITY_THRESHOLD && best.as_ref().map(|b| score > b.2).unwrap_or(true) {
                    best = Some((i, m.hash.clone(), score));
                }
            }
        }
        if let Some((i, to, score)) = best {
            let l = &mut self.lineages[i];
            let attachment = Attachment::Similarity { to: to.clone(), score };
            l.members.push(Member {
                hash: hash.clone(),
                strategy: prog.strategy.clone(),
                rules,
                attachment: attachment.clone(),
                at: now,
            });
            let warning = format!(
                "strategy `{}` ({}) is attached to lineage {} by similarity {:.2} with {}; its trials count there (dispute with `abt study dispute`)",
                prog.strategy, hash, l.id, score, to
            );
            return Ok(Opened {
                lineage: l.id.clone(),
                hash,
                attachment,
                existing: false,
                warning: Some(warning),
            });
        }
        self.lineages.push(Lineage {
            id: hash.clone(),
            members: vec![Member {
                hash: hash.clone(),
                strategy: prog.strategy.clone(),
                rules,
                attachment: Attachment::Root,
                at: now,
            }],
            disputes: vec![],
        });
        Ok(Opened {
            lineage: hash.clone(),
            hash,
            attachment: Attachment::Root,
            existing: false,
            warning: None,
        })
    }

    /// Record a dispute of a member's attachment; the attachment stands.
    pub fn dispute(&mut self, hash: &str, reason: &str, now: u64) -> Result<(), String> {
        let l = self
            .lineages
            .iter_mut()
            .find(|l| l.members.iter().any(|m| m.hash == hash))
            .ok_or_else(|| format!("no lineage holds program hash {}", hash))?;
        l.disputes.push(Dispute {
            hash: hash.to_string(),
            reason: reason.to_string(),
            at: now,
        });
        Ok(())
    }
}

/// The lineage file of a study directory.
pub fn lineages_path(dir: &Path) -> PathBuf {
    dir.join("lineages.json")
}
