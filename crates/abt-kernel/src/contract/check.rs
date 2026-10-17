//! The checks of a contract (K-3.2 to K-3.5): the preconditions when the
//! demand is bound, the invariants at every decision barrier after the
//! executor's take, the postconditions when the run finishes. A check sets
//! its clause's status in the run's contract; a precondition that refuses
//! is `RunError::Precondition`, an invariant that fails `RunError::Invariant`
//! (the run halts, naming the barrier, the clause and the values).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use super::{Contract, Status};
use crate::check::{LimitKind, LimitPolicy, Program};
use crate::ir::{Kind as RelKind, Lit, Term, WindowKind};
use crate::kernel::bind::{Bound, DataClient};
use crate::kernel::events::{Book, EventKind, Fate, OrderId};
use crate::kernel::time::format_timestamp;
use crate::kernel::{ExecConfig, Kernel, RunError, RunResult, RunWarning, Sym, Value};
use crate::sim::Expected;

// ---------------------------------------------------------------------------
// The data checks the bundle tests were (B-3), now preconditions that warn.

/// Action reconciliation over the closes by instrument, the splits and the
/// dividends by (instrument, ex-date): the total-return ratio between
/// consecutive closes, (close + that day's dividends) × split factor /
/// previous close, is within 25% of one on a split's ex-date and within
/// [0.6, 1.67] on any other bar, and every split falls on a bar with a
/// close. `excused` bars are reviewed exceptions. The problems, and how
/// many exceptions were accepted.
pub fn action_problems<K: Ord + Clone>(
    closes: &BTreeMap<K, BTreeMap<i64, f64>>,
    splits: &BTreeMap<(K, i64), f64>,
    dividends: &BTreeMap<(K, i64), f64>,
    excused: &dyn Fn(&K, i64) -> bool,
    name: &dyn Fn(&K) -> String,
) -> (Vec<String>, usize) {
    let mut problems = Vec::new();
    let mut accepted = 0usize;
    for (sym, s) in closes {
        let mut prev: Option<f64> = None;
        for (&t, &p) in s {
            if let Some(p0) = prev {
                let div = dividends.get(&(sym.clone(), t)).copied().unwrap_or(0.0);
                let split = splits.get(&(sym.clone(), t)).copied();
                let ratio = (p + div) * split.unwrap_or(1.0) / p0;
                let problem = match split {
                    Some(f) if (ratio - 1.0).abs() > 0.25 => Some(format!("{} at {}: a {}:1 split but the close moved from {} to {}", name(sym), format_timestamp(t), f, p0, p)),
                    None if !(0.6..=1.67).contains(&ratio) => Some(if div > 0.0 {
                        format!(
                            "{} at {}: the close moved from {} to {} and a dividend of {} does not explain it",
                            name(sym),
                            format_timestamp(t),
                            p0,
                            p,
                            div
                        )
                    } else {
                        format!("{} at {}: the close moved from {} to {} with no action to explain it", name(sym), format_timestamp(t), p0, p)
                    }),
                    _ => None,
                };
                if let Some(problem) = problem {
                    if excused(sym, t) {
                        accepted += 1;
                    } else {
                        problems.push(problem);
                    }
                }
            }
            prev = Some(p);
        }
    }
    for ((sym, t), f) in splits {
        if !closes.get(sym).is_some_and(|s| s.contains_key(t)) {
            problems.push(format!("{} at {}: a {}:1 split on a bar with no close", name(sym), format_timestamp(*t), f));
        }
    }
    (problems, accepted)
}

/// Delisting coverage: an instrument whose last listed bar precedes the
/// data's last bar has a `delisted` row.
pub fn delisting_problems<K: Ord + Clone>(listed: &[(K, i64)], delisted: &BTreeSet<K>, name: &dyn Fn(&K) -> String) -> Vec<String> {
    let Some(last_bar) = listed.iter().map(|(_, t)| *t).max() else { return Vec::new() };
    let mut last_seen: BTreeMap<K, i64> = BTreeMap::new();
    for (k, t) in listed {
        let e = last_seen.entry(k.clone()).or_insert(*t);
        *e = (*e).max(*t);
    }
    last_seen
        .iter()
        .filter(|(k, t)| **t < last_bar && !delisted.contains(*k))
        .map(|(k, t)| format!("{} leaves the listing at {} with no delisted record", name(k), format_timestamp(*t)))
        .collect()
}

/// Membership: a `member` row's instrument is listed that bar.
pub fn membership_problems<K: Ord + Clone>(members: &[(K, i64)], listed: &BTreeSet<(K, i64)>, name: &dyn Fn(&K) -> String) -> Vec<String> {
    members
        .iter()
        .filter(|(k, t)| !listed.contains(&(k.clone(), *t)))
        .map(|(k, t)| format!("{} is a member at {} but not listed", name(k), format_timestamp(*t)))
        .collect()
}

/// The first five problems and how many more, as a status's detail.
fn first_five(problems: &[String]) -> String {
    let more = if problems.len() > 5 { format!(" (+{} more)", problems.len() - 5) } else { String::new() };
    problems.iter().take(5).cloned().collect::<Vec<_>>().join("; ") + &more
}

fn warned_or_checked(c: &mut Contract, id: &str, problems: &[String], warnings: &mut Vec<RunWarning>, bias: &str) {
    if problems.is_empty() {
        c.set(id, Status::Checked { times: 1 });
    } else {
        let detail = first_five(problems);
        warnings.push(RunWarning {
            bias: bias.into(),
            message: format!("{} ({} found): {}", c.get(id).map(|x| x.text.clone()).unwrap_or_default(), problems.len(), detail),
        });
        c.set(id, Status::Warned { count: problems.len() as u64, detail });
    }
}

// ---------------------------------------------------------------------------
// Preconditions (K-3.2).

/// The memory the derived state may take when no budget is given: half the
/// machine's (K-4.6), or 8 GiB when the machine does not say.
fn default_budget() -> u64 {
    let total = std::fs::read_to_string("/proc/meminfo").ok().and_then(|s| {
        s.lines()
            .find_map(|l| l.strip_prefix("MemTotal:"))
            .and_then(|v| v.split_whitespace().next().and_then(|n| n.parse::<u64>().ok()))
    });
    total.map(|kb| kb * 1024 / 2).unwrap_or(8 << 30)
}

/// The derived state bound of K-4.6: per derived relation, its instruments
/// to the power of its entity arguments, times the bars it is held (its
/// longest window, in bars, plus the memo's retention), times a row's
/// bytes; plus the window state of its aggregates.
pub fn state_bound(prog: &Program, instruments: usize) -> u64 {
    const RETAINED: u64 = 8;
    const VALUE: u64 = 16;
    let ent = instruments.max(1) as u64;
    let mut total: u64 = 0;
    for (name, sig) in &prog.relations {
        if !matches!(sig.kind, RelKind::Derived { .. } | RelKind::Output) {
            continue;
        }
        let a = sig.args.iter().filter(|x| x.ty.is_entity()).count() as u32;
        let mut w: u64 = 0;
        let mut rows: u64 = 0;
        for r in prog.rules.iter().filter(|r| &r.head.name == name) {
            for l in &r.body {
                if let crate::ir::Literal::Agg { conj, .. } = l {
                    for c in conj {
                        if let crate::ir::Literal::Window { kind, dur, .. } = c {
                            let bars = match (kind, dur) {
                                (WindowKind::Rows | WindowKind::PriorRows, crate::ir::Expr::Lit(Lit::Int(n), _)) => *n as u64,
                                (_, crate::ir::Expr::Lit(Lit::Duration(d), _)) => (d.months * 31 + d.days).max(1) as u64,
                                _ => 64,
                            };
                            w = w.max(bars);
                            rows += bars;
                        }
                    }
                }
            }
        }
        let width = VALUE * sig.args.len() as u64 + 32;
        total = total.saturating_add(ent.saturating_pow(a).saturating_mul((w + RETAINED).saturating_mul(width)));
        total = total.saturating_add(ent.saturating_pow(a.max(1)).saturating_mul(rows.saturating_mul(VALUE)));
    }
    total
}

fn le_zero(column: &str) -> abt_wire::generated::abt::wire::Predicate {
    use abt_wire::generated::abt::wire::{Atom, Constant, ConstantKind, Disjunction, Op, Predicate};
    Predicate {
        all: Some(vec![Disjunction {
            atoms: vec![Atom {
                column: column.to_string(),
                op: Op::Le,
                values: Some(vec![Constant {
                    kind: ConstantKind::Num,
                    num: 0.0,
                    int: 0,
                    text: None,
                }]),
            }],
        }]),
    }
}

/// The preconditions at bind, before the first barrier (K-3.2): each
/// clause's status in `c`; what a warn policy found, as run warnings; a
/// refusal names its clause.
pub fn preconditions(c: &mut Contract, prog: &Program, bound: &Bound, client: &DataClient, cfg: &ExecConfig) -> Result<Vec<RunWarning>, RunError> {
    let mut warnings = Vec::new();
    let refuse = |clause: &str, message: String| RunError::Precondition { clause: clause.into(), message };
    // The resolver answered the demand with versions that carry what it
    // asked for, or the run would not have been bound (D-7.5).
    c.set("pre.binding", Status::Checked { times: 1 });
    c.set("pre.attributes", Status::Checked { times: 1 });
    // WF-15 checked every label against the class's vocabularies, which the
    // registry generates (D-2.2); a label no bound row carries is warned on
    // by the run itself.
    c.set("pre.vocabulary", Status::Checked { times: 1 });
    // The instruments table names instruments of the binding.
    let rows: Vec<String> = prog
        .rules
        .iter()
        .filter(|r| r.head.name == "instruments")
        .filter_map(|r| match r.head.terms.first() {
            Some(Term::Lit(Lit::Entity(n), _)) => Some(n.clone()),
            _ => None,
        })
        .collect();
    if rows.is_empty() {
        c.set(
            "pre.instruments",
            Status::NotApplicable {
                why: "the universe block has no instruments table".into(),
            },
        );
    } else {
        let names: BTreeSet<&str> = bound.securities.iter().map(|s| s.ticker.as_str()).collect();
        for n in &rows {
            if !names.contains(n.as_str()) && bound.resolve_ticker(n, cfg.as_of).is_err() && bound.symbols.get(n).is_none() {
                return Err(refuse("pre.instruments", format!("`{}` of the instruments table is not an instrument of the binding", n)));
            }
        }
        c.set("pre.instruments", Status::Checked { times: rows.len() as u64 });
    }
    // The span: the resolver's warm-up warnings (E-41).
    let warm: Vec<String> = bound.warnings.iter().filter(|w| w.bias == "warm-up").map(|w| w.message.clone()).collect();
    if warm.is_empty() {
        c.set("pre.span", Status::Checked { times: 1 });
    } else {
        c.set(
            "pre.span",
            Status::Warned {
                count: warm.len() as u64,
                detail: first_five(&warm),
            },
        );
    }
    // The state bound against the budget (K-4.6; at bind, E-9).
    let need = state_bound(prog, bound.ids.len());
    let budget = cfg.memory_budget.unwrap_or_else(default_budget);
    if need > budget {
        return Err(refuse(
            "pre.state_bound",
            format!("the derived state needs about {} bytes and the memory budget is {} bytes", need, budget),
        ));
    }
    c.set("pre.state_bound", Status::Checked { times: 1 });
    // No bound version declares `revisions = none` before the vendors land
    // (M-6): revisions are kept and read at their latest receipt.
    c.set(
        "pre.revisions",
        Status::NotApplicable {
            why: "no bound version declares `revisions = none`".into(),
        },
    );
    // Positive prices, futures excepted (their back-adjusted series cross
    // zero): every stored relation with a price column, the price relation
    // or what a derived price relation is computed from.
    if prog.class == "future" {
        c.set(
            "pre.prices",
            Status::NotApplicable {
                why: "a future's back-adjusted prices may cross zero (K-5.4)".into(),
            },
        );
    } else {
        let mut scanned = 0u64;
        for (rel, (_, schema)) in &bound.stored {
            let Some(sig) = prog.relations.get(rel) else { continue };
            let values: Vec<&abt_wire::generated::abt::wire::ColumnSpec> = schema.columns.iter().filter(|x| x.role == abt_wire::generated::abt::wire::Role::Value).collect();
            // The value columns in the signature's order: its outputs that
            // are neither the instrument nor a label (a label column is the
            // fact's identity, E-35).
            let args: Vec<&crate::ir::Arg> = sig
                .args
                .iter()
                .filter(|a| a.mode == crate::ir::Mode::Out && !a.ty.is_entity() && a.ty != crate::ir::Ty::Label)
                .collect();
            for (arg, col) in args.iter().zip(values.iter()) {
                if !matches!(&arg.ty, crate::ir::Ty::Quantity(d) if d.is_price()) {
                    continue;
                }
                scanned += 1;
                let bad = bound.scan(client, rel, Some(le_zero(&col.name)))?;
                if let Some(row) = bad.iter().find(|r| r.sym.is_some_and(|s| !bound.contracts.contains_key(&s))) {
                    let who = row.sym.map(|s| bound.symbols.name(s).to_string()).unwrap_or_default();
                    return Err(refuse("pre.prices", format!("`{}` of {} at {} is not a positive price", rel, who, format_timestamp(row.t))));
                }
            }
        }
        if scanned == 0 {
            c.set(
                "pre.prices",
                Status::NotApplicable {
                    why: "the binding holds no price column".into(),
                },
            );
        } else {
            c.set("pre.prices", Status::Checked { times: scanned });
        }
    }
    // The calendars' labels are the binding's own: the kernel puts them as
    // `calendar_label` rows from the bound bars (E-36).
    c.set("pre.calendars", Status::Checked { times: 1 });
    // The data checks (the former bundle tests).
    let name = |s: &Sym| bound.symbols.name(*s).to_string();
    if bound.stored.contains_key("close") && (bound.stored.contains_key("split") || bound.stored.contains_key("dividend")) {
        let mut closes: BTreeMap<Sym, BTreeMap<i64, f64>> = BTreeMap::new();
        for r in bound.scan(client, "close", None)? {
            if let (Some(s), Some(Some(p))) = (r.sym, r.nums.first()) {
                closes.entry(s).or_default().insert(r.t, *p);
            }
        }
        let mut splits: BTreeMap<(Sym, i64), f64> = BTreeMap::new();
        for r in bound.scan(client, "split", None)? {
            if let (Some(s), Some(Some(f))) = (r.sym, r.nums.first()) {
                splits.insert((s, r.t), *f);
            }
        }
        let mut dividends: BTreeMap<(Sym, i64), f64> = BTreeMap::new();
        for r in bound.scan(client, "dividend", None)? {
            if let (Some(s), Some(Some(ex)), Some(Some(amount))) = (r.sym, r.times.first(), r.nums.first()) {
                *dividends.entry((s, *ex)).or_insert(0.0) += amount;
            }
        }
        let (problems, _) = action_problems(&closes, &splits, &dividends, &|_, _| false, &name);
        warned_or_checked(c, "pre.actions", &problems, &mut warnings, "corporate-action");
    } else {
        c.set(
            "pre.actions",
            Status::NotApplicable {
                why: "the binding holds no closes with actions".into(),
            },
        );
    }
    let listed: Vec<(Sym, i64)> = if bound.stored.contains_key("listed") {
        bound.scan(client, "listed", None)?.into_iter().filter_map(|r| Some((r.sym?, r.t))).collect()
    } else {
        Vec::new()
    };
    if !listed.is_empty() && bound.stored.contains_key("delisted") {
        let delisted: BTreeSet<Sym> = bound.scan(client, "delisted", None)?.into_iter().filter_map(|r| r.sym).collect();
        let problems = delisting_problems(&listed, &delisted, &name);
        warned_or_checked(c, "pre.delistings", &problems, &mut warnings, "delisting");
    } else {
        c.set(
            "pre.delistings",
            Status::NotApplicable {
                why: "the binding holds no listing with delistings".into(),
            },
        );
    }
    if !listed.is_empty() && bound.stored.contains_key("member") {
        let members: Vec<(Sym, i64)> = bound.scan(client, "member", None)?.into_iter().filter_map(|r| Some((r.sym?, r.t))).collect();
        let on: BTreeSet<(Sym, i64)> = listed.iter().cloned().collect();
        let problems = membership_problems(&members, &on, &name);
        warned_or_checked(c, "pre.membership", &problems, &mut warnings, "survivorship");
    } else {
        c.set(
            "pre.membership",
            Status::NotApplicable {
                why: "the binding holds no membership with a listing".into(),
            },
        );
    }
    c.set("pre.option_inputs", Status::NotApplicable { why: "not an option program".into() });
    c.set(
        "pre.margin_rows",
        Status::NotApplicable {
            why: "no `margin published` model before M-7".into(),
        },
    );
    Ok(warnings)
}

// ---------------------------------------------------------------------------
// Invariants (K-3.3).

/// What the invariants keep between barriers: the book the log folds to
/// so far, and how far into the log it is.
#[derive(Clone, Debug, Default)]
pub struct Invariants {
    shadow: Book,
    applied: usize,
    decisions_seen: usize,
}

/// What one barrier's checks read, gathered before they run (a fault is
/// made in it for the acceptance tests, G7).
struct Observed {
    nav: Option<(f64, f64)>,
    duplicate_decision: Option<String>,
    received: i64,
    pushed: i64,
    duplicate_tuple: Option<String>,
    gross: Option<(f64, f64)>,
    position_limits: Option<String>,
    fold: Option<String>,
    working: Option<String>,
}

impl Invariants {
    /// From the log a run already holds (a run resumed from a checkpoint).
    pub fn new(initial_cash: f64, result: &RunResult) -> Invariants {
        let mut inv = Invariants {
            shadow: Book::new(initial_cash),
            applied: 0,
            decisions_seen: 0,
        };
        for e in &result.events {
            inv.shadow.apply(e);
        }
        inv.applied = result.events.len();
        inv.decisions_seen = result.events.iter().filter(|e| matches!(e.kind, EventKind::Decision { .. })).count();
        inv
    }

    /// The invariants at decision barrier `t`, after the executor's take.
    #[allow(clippy::too_many_arguments)]
    pub fn check(&mut self, k: &mut Kernel, book: Option<&Book>, working: &[Sym], expected: Option<&Expected>, result: &mut RunResult, t: i64, at: i64) -> Result<(), RunError> {
        let mut o = self.observe(k, book, working, expected, result, t);
        k.received_r = i64::MIN;
        k.pushed_read_r = i64::MIN;
        // A fault made deliberately (G7).
        if let Some((clause, from)) = k.cfg.inject.clone() {
            if t >= from {
                match clause.as_str() {
                    "inv.nav" => o.nav = o.nav.map(|(a, b)| (a + 1.0, b)),
                    "inv.one_decision" => o.duplicate_decision = Some("an injected second decision".into()),
                    "inv.no_read_past" => o.received = at + 1,
                    "inv.keyed_store" => o.duplicate_tuple = Some("an injected second tuple".into()),
                    "inv.gross" => o.gross = Some((f64::INFINITY, 0.0)),
                    "inv.position_limits" => o.position_limits = Some("an injected position above the limit".into()),
                    "inv.log_fold" => o.fold = Some("an injected change to the book outside the log".into()),
                    "inv.pushdown_receipt" => o.pushed = at + 1,
                    "inv.working_orders" => o.working = Some("an injected second working order".into()),
                    _ => {}
                }
            }
        }
        // A failure halts the run, and the clause is violated in the
        // contract a halted run writes (K-7.1).
        let fail = |c: &mut crate::contract::Contract, clause: &str, message: String| {
            c.set(clause, Status::Violated { at: Some(t), detail: message.clone() });
            RunError::Invariant { clause: clause.into(), t, message }
        };
        let c = &mut result.contract;
        if let Some((recorded, computed)) = o.nav {
            if recorded.to_bits() != computed.to_bits() {
                return Err(fail(
                    c,
                    "inv.nav",
                    format!("the NAV recorded is {} and cash, the marked positions and the receivables are {}", recorded, computed),
                ));
            }
        }
        c.checked("inv.nav");
        if let Some(d) = o.duplicate_decision {
            return Err(fail(c, "inv.one_decision", d));
        }
        c.checked("inv.one_decision");
        if o.received > at {
            return Err(fail(
                c,
                "inv.no_read_past",
                format!("a tuple received at {} precedes the barrier at {}", format_timestamp(o.received), format_timestamp(at)),
            ));
        }
        c.checked("inv.no_read_past");
        if let Some(d) = o.duplicate_tuple {
            return Err(fail(c, "inv.keyed_store", d));
        }
        c.checked("inv.keyed_store");
        if let Some((gross, allowed)) = o.gross {
            if gross > allowed {
                return Err(fail(
                    c,
                    "inv.gross",
                    format!("gross exposure {} after the bar's fills is above {} (the leverage field's multiple of equity)", gross, allowed),
                ));
            }
        }
        c.checked("inv.gross");
        if let Some(d) = o.position_limits {
            return Err(fail(c, "inv.position_limits", d));
        }
        c.checked("inv.position_limits");
        if let Some(d) = o.fold {
            return Err(fail(c, "inv.log_fold", d));
        }
        c.checked("inv.log_fold");
        if o.pushed > at {
            return Err(fail(
                c,
                "inv.pushdown_receipt",
                format!("a pushed-down answer read at {} waits on a receipt at {}", format_timestamp(at), format_timestamp(o.pushed)),
            ));
        }
        c.checked("inv.pushdown_receipt");
        if let Some(d) = o.working {
            return Err(fail(c, "inv.working_orders", d));
        }
        c.checked("inv.working_orders");
        Ok(())
    }

    fn observe(&mut self, k: &mut Kernel, book: Option<&Book>, working: &[Sym], expected: Option<&Expected>, result: &RunResult, t: i64) -> Observed {
        // NAV: the book record at `t` against its parts, summed as the
        // executor sums them.
        let nav = match (book, result.exposure.last()) {
            (Some(b), Some(x)) if x.t == t => {
                let mut n = b.cash;
                for (&sym, &q) in &b.positions {
                    if let Some(p) = k.price_at(sym, t) {
                        n += q * p * k.multiplier(sym);
                    }
                }
                for r in &b.receivables {
                    n += r.cash;
                }
                // The record precedes the bar's margin-call fills, and an
                // exercise the program decided at the bar's close (K-5.5.4):
                // it is the book's only when there were none.
                let after = result.events[self.applied..].iter().any(|e| match &e.kind {
                    EventKind::MarginCall { policy, .. } => policy == "liquidate",
                    EventKind::Exercise { cause, .. } => cause == "decision",
                    _ => false,
                });
                (!after).then_some((x.equity, n))
            }
            _ => None,
        };
        // One decision per instrument.
        let mut seen: BTreeSet<Sym> = BTreeSet::new();
        let mut duplicate_decision = None;
        let mut decisions = 0usize;
        for e in result.events[self.applied..].iter() {
            if let EventKind::Decision { decision } = &e.kind {
                decisions += 1;
                if e.t == t && !seen.insert(decision.equity) {
                    duplicate_decision = Some(format!("two decisions on {} at the bar", k.symbols.name(decision.equity)));
                }
            }
        }
        self.decisions_seen += decisions;
        // Keyedness: the stored relations' tuples at `t`, one per identity.
        let mut duplicate_tuple = None;
        for (id, info) in k.rels.iter().enumerate() {
            if !matches!(k.prog.relations.get(&info.name).map(|s| &s.kind), Some(RelKind::Primitive { .. })) {
                continue;
            }
            let Some(tus) = k.stores[id].by_time.get(&t) else { continue };
            let mut ids: BTreeSet<Vec<Value>> = BTreeSet::new();
            for tu in tus {
                let key: Vec<Value> = info.identity.iter().map(|&p| tu[p].clone()).collect();
                if !ids.insert(key) {
                    duplicate_tuple = Some(format!("`{}` holds two tuples of one identity at the bar", info.name));
                    break;
                }
            }
            if duplicate_tuple.is_some() {
                break;
            }
        }
        // Gross after the bar's fills that added exposure, against the
        // leverage field when it refuses or halts (the costs of the bar are
        // never leverage; `allow` lets a fill borrow past the multiple).
        let mut gross = None;
        if k.cfg.max_gross.is_finite() && k.cfg.on_leverage != crate::kernel::OnLeverage::Allow {
            let adding = result.events[self.applied..].iter().any(|e| match &e.kind {
                EventKind::Fill { fill, .. } if !fill.forced && fill.t == t => {
                    let before = self.shadow.position(fill.equity);
                    (before + fill.quantity).abs() > before.abs()
                }
                _ => false,
            });
            if let (true, Some(x)) = (adding, result.exposure.last().filter(|x| x.t == t)) {
                let costs: f64 = result.events[self.applied..]
                    .iter()
                    .filter_map(|e| match &e.kind {
                        EventKind::Fill { fill, .. } if fill.t == t => Some(fill.commission + fill.fee + fill.slippage + fill.impact),
                        _ => None,
                    })
                    .sum();
                let max = k.cfg.max_gross.max(1.0);
                let base = if k.cfg.compounding { x.equity.max(0.0) } else { k.cfg.initial_cash };
                gross = Some((x.gross, max * base + costs + 1e-9 * (1.0 + x.gross + base)));
            }
        }
        // The names and weight limits on the book the accepted orders would
        // leave (the limits are pre-trade, K-3.5).
        let mut position_limits = None;
        if let Some(e) = expected.filter(|e| e.t == t) {
            for l in k.cfg.limits.iter().filter(|l| matches!(l.policy, LimitPolicy::Reject | LimitPolicy::Clamp)) {
                match l.kind {
                    LimitKind::Names => {
                        let n = e.positions.iter().filter(|(_, q, _)| *q != 0.0).count();
                        if n as f64 > l.bound {
                            position_limits = Some(format!("{} names after the accepted orders, above `{}`", n, l));
                        }
                    }
                    LimitKind::Weight => {
                        let bound = l.bound * e.capital;
                        if let Some((s, q, pm)) = e.positions.iter().find(|(_, q, pm)| (q * pm).abs() > bound * (1.0 + 1e-9) + 1e-9) {
                            position_limits = Some(format!("{} at {} after the accepted orders, above `{}`", k.symbols.name(*s), (q * pm).abs(), l));
                        }
                    }
                    _ => {}
                }
            }
        }
        // The log's fold against the book.
        for e in &result.events[self.applied..] {
            self.shadow.apply(e);
        }
        self.applied = result.events.len();
        let fold = book.and_then(|b| {
            let cash = b.cash.to_bits() == self.shadow.cash.to_bits();
            let positions = b.positions.len() == self.shadow.positions.len() && b.positions.iter().zip(&self.shadow.positions).all(|((s1, q1), (s2, q2))| s1 == s2 && q1.to_bits() == q2.to_bits());
            let receivables =
                b.receivables.len() == self.shadow.receivables.len() && b.receivables.iter().zip(&self.shadow.receivables).all(|(x, y)| x.id == y.id && x.cash.to_bits() == y.cash.to_bits());
            (!(cash && positions && receivables)).then(|| {
                format!(
                    "the book holds cash {} and {} positions; the log folds to cash {} and {} positions",
                    b.cash,
                    b.positions.len(),
                    self.shadow.cash,
                    self.shadow.positions.len()
                )
            })
        });
        // Working orders, one per instrument.
        let mut w: HashMap<Sym, usize> = HashMap::new();
        let mut working_twice = None;
        for s in working {
            let n = w.entry(*s).or_default();
            *n += 1;
            if *n > 1 {
                working_twice = Some(format!("two orders working on {}", k.symbols.name(*s)));
            }
        }
        Observed {
            nav,
            duplicate_decision,
            received: k.received_r,
            pushed: k.pushed_read_r,
            duplicate_tuple,
            gross,
            position_limits,
            fold,
            working: working_twice,
        }
    }
}

// ---------------------------------------------------------------------------
// Postconditions (K-3.4) and the limits' report (K-3.5).

/// What an order's events say of it at the end: its outcome, or open.
pub fn order_outcomes(result: &RunResult) -> BTreeMap<OrderId, &'static str> {
    let mut out = BTreeMap::new();
    for e in &result.events {
        let Some(id) = e.order_id else { continue };
        let o = match &e.kind {
            EventKind::OrderSent { .. } => "open",
            EventKind::Fill {
                remainder: Some((_, Fate::Reissued)), ..
            } => "open",
            EventKind::Fill {
                remainder: Some((_, Fate::Expired)), ..
            } => "open",
            EventKind::Fill { .. } => "filled",
            EventKind::Dropped { .. } | EventKind::Rejected { .. } => "dropped",
            EventKind::Satisfied { .. } => "met",
            EventKind::Replaced { .. } => "replaced",
            EventKind::ExpiredOrder { .. } => "expired",
            _ => continue,
        };
        out.insert(id, o);
    }
    out
}

/// The postconditions when the run finishes, and the limits' report.
pub fn postconditions(result: &mut RunResult, prog: &Program, initial_cash: f64, observed: bool) {
    // Every decision is one order; every order ends once or is open.
    let decisions = result.events.iter().filter(|e| matches!(e.kind, EventKind::Decision { .. })).count();
    let outcomes = order_outcomes(result);
    let open = outcomes.values().filter(|o| **o == "open").count();
    let status = if outcomes.len() == decisions && open <= result.symbols.len() {
        Status::Checked { times: outcomes.len() as u64 }
    } else {
        Status::Violated {
            at: None,
            detail: format!("{} decisions, {} orders with an outcome, {} open at the end", decisions, outcomes.len(), open),
        }
    };
    result.contract.set("post.decisions", status);
    // Coverage (W8): the first empty relation of each path to `decide`.
    let (_, roots) = crate::observe::coverage_report(prog, &result.coverage, result.bars.len());
    if roots.is_empty() {
        result.contract.set("post.coverage", Status::Checked { times: result.coverage.len() as u64 });
    } else {
        let detail: Vec<String> = roots.iter().map(|r| format!("[W8 never fired] `{}`", r.relation)).collect();
        result.contract.set(
            "post.coverage",
            Status::Warned {
                count: roots.len() as u64,
                detail: first_five(&detail),
            },
        );
    }
    result.contract.set(
        "post.ledgers",
        if observed {
            Status::Checked { times: 1 }
        } else {
            Status::NotApplicable { why: "no relation observed".into() }
        },
    );
    result.contract.set(
        "post.truncation",
        Status::NotApplicable {
            why: "the acceptance suite's (G5: tests/fold.rs, tests/fold_checkpoint.rs)".into(),
        },
    );
    // Final cash: initial + fills (signed notional) - costs + dividends,
    // settlements and fractions + funding, from the log's categories.
    let mut fills = 0.0;
    let mut actions = 0.0;
    let mut funding = 0.0;
    for e in &result.events {
        let flow = e.cash_flow.unwrap_or(0.0);
        match &e.kind {
            EventKind::Fill { .. } => fills += flow,
            EventKind::Action { .. } => actions += flow,
            EventKind::Funding { .. } => funding += flow,
            _ => {}
        }
    }
    let total = initial_cash + fills + actions + funding;
    let tol = 1e-9 * (1.0 + total.abs() + result.final_cash.abs());
    result.contract.set(
        "post.final_cash",
        if (total - result.final_cash).abs() <= tol {
            Status::Checked { times: 1 }
        } else {
            Status::Violated {
                at: None,
                detail: format!("the log's flows give {} and the book's cash is {}", total, result.final_cash),
            }
        },
    );
    // The warned bind clauses, with their counts.
    let warned: Vec<String> = result
        .contract
        .clauses
        .iter()
        .filter_map(|c| match &c.status {
            Status::Warned { count, .. } if c.kind == super::Kind::Pre => Some(format!("{} ({})", c.id, count)),
            _ => None,
        })
        .collect();
    result.contract.set("post.bind_warnings", Status::Checked { times: warned.len() as u64 });
    if let Some(c) = result.contract.clauses.iter_mut().find(|c| c.id == "post.bind_warnings") {
        c.fired.clear();
    }
    // Every limit with its trigger count and bars.
    let mut fired: BTreeMap<String, Vec<i64>> = BTreeMap::new();
    for e in &result.events {
        if let EventKind::LimitTriggered { limit, .. } = &e.kind {
            let kind = limit.split_whitespace().next().unwrap_or_default().to_string();
            fired.entry(kind).or_default().push(e.t);
        }
    }
    let barriers = result.bars.len() as u64;
    for l in &prog.exec.limits {
        let id = format!("limit.{}", l.kind.name());
        let bars = fired.remove(l.kind.name()).unwrap_or_default();
        if let Some(c) = result.contract.clauses.iter_mut().find(|c| c.id == id) {
            c.fired = bars.clone();
            if !matches!(c.status, Status::Proven { .. }) {
                c.status = if bars.is_empty() {
                    Status::Checked { times: barriers }
                } else if l.policy == LimitPolicy::Warn {
                    Status::Warned {
                        count: bars.len() as u64,
                        detail: format!("first at {}", format_timestamp(bars[0])),
                    }
                } else {
                    Status::Checked { times: barriers }
                };
            }
        }
    }
    result.contract.set(
        "post.limits",
        if prog.exec.limits.is_empty() {
            Status::NotApplicable {
                why: "the execution block has no limits".into(),
            }
        } else {
            Status::Checked { times: prog.exec.limits.len() as u64 }
        },
    );
    // A runtime clause the run never reached (no decision barrier) is not
    // checked: say so.
    for c in result.contract.clauses.iter_mut() {
        if c.status == Status::Open {
            c.status = Status::NotApplicable {
                why: "no decision barrier reached it".into(),
            };
        }
    }
}
