//! The run's outputs (K-7.1): the event log, the book and its holdings, the
//! decisions and fills, the coverage, the contract, the binding, the plans,
//! the ledger of every observed relation, the late tuples and the report,
//! written through the data layer's `put` (D-3.6) under `kernel.<run id>.*`
//! with the schemas of P-4 (ERRATA E-52 for what the registry's identity
//! rule changes). A halted run writes what it has; its report's `status` is
//! `halted(reason)`.

use std::collections::HashMap;
use std::sync::Arc;

use abt_wire::generated::abt::wire::{ColumnSpec, ColumnType, RelationSchema, Role};
use arrow::array::{ArrayRef, BinaryArray, BooleanArray, Float64Array, Int64Array, StringArray, TimestampNanosecondArray, UInt64Array};
use arrow::datatypes::{DataType, Field, Schema, TimeUnit};
use arrow::record_batch::RecordBatch;

use super::bind::{Bound, DataClient};
use super::events::EventKind;
use super::{Action, ExecConfig, Kernel, RunError, RunResult, Sym, Value};
use crate::check::Program;
use crate::ir::Ty;

/// The relations every run writes, by the name after `kernel.<run>.`; an
/// observed relation R adds `ledger.R`, and the stream's calendars are
/// `calendar_label` (E-32).
pub const RELATIONS: [&str; 11] = ["events", "book", "holdings", "decisions", "fills", "coverage", "contract", "binding", "plans", "late", "report"];

/// The relations that grow with the run, written incrementally at every
/// checkpoint (K-7.1); the others are written when the run ends or halts.
pub const GROWING: [&str; 6] = ["events", "book", "holdings", "decisions", "fills", "late"];

/// The kernel's instruments by their ids in the data layer.
pub type Ids = HashMap<Sym, u64>;

/// Output relations built: each schema with its rows.
pub type Tables = Vec<(RelationSchema, RecordBatch)>;

/// The data layer's id of every instrument the kernel holds (the binding's
/// symbols renumbered by the kernel, K-4.1).
pub fn instrument_ids(k: &Kernel, bound: &Bound) -> Ids {
    bound.ids.iter().enumerate().filter_map(|(s, &id)| k.remap_syms.get(s).map(|&sym| (sym, id))).collect()
}

/// A column of an output relation: its name, type and role, and its values.
enum Col {
    Entity(Vec<Option<u64>>),
    Time(Vec<Option<i64>>),
    Num(Vec<Option<f64>>),
    Count(Vec<Option<i64>>),
    Label(Vec<Option<String>>),
    Bool(Vec<Option<bool>>),
    Bytes(Vec<Option<String>>),
}

impl Col {
    fn ty(&self) -> ColumnType {
        match self {
            Col::Entity(_) => ColumnType::Entity,
            Col::Time(_) => ColumnType::Time,
            Col::Num(_) => ColumnType::Num,
            Col::Count(_) => ColumnType::Count,
            Col::Label(_) => ColumnType::Label,
            Col::Bool(_) => ColumnType::Bool,
            Col::Bytes(_) => ColumnType::Bytes,
        }
    }
    fn array(&self) -> (DataType, ArrayRef) {
        match self {
            Col::Entity(v) => (DataType::UInt64, Arc::new(UInt64Array::from(v.clone()))),
            Col::Time(v) => (DataType::Timestamp(TimeUnit::Nanosecond, None), Arc::new(TimestampNanosecondArray::from(v.clone()))),
            Col::Num(v) => (DataType::Float64, Arc::new(Float64Array::from(v.clone()))),
            Col::Count(v) => (DataType::Int64, Arc::new(Int64Array::from(v.clone()))),
            Col::Label(v) => (DataType::Utf8, Arc::new(StringArray::from(v.clone()))),
            Col::Bool(v) => (DataType::Boolean, Arc::new(BooleanArray::from(v.clone()))),
            Col::Bytes(v) => (DataType::Binary, Arc::new(BinaryArray::from(v.iter().map(|s| s.as_deref().map(|s| s.as_bytes())).collect::<Vec<_>>()))),
        }
    }
}

/// An output relation being built: its columns with their roles.
struct Table {
    name: String,
    cols: Vec<(String, Role, Col)>,
}

impl Table {
    fn new(run: &str, name: &str) -> Table {
        Table {
            name: format!("kernel.{}.{}", run, name),
            cols: Vec::new(),
        }
    }
    fn col(self, name: &str, role: Role, c: Col) -> Table {
        self.col_named(name.to_string(), role, c)
    }
    fn col_named(mut self, name: String, role: Role, c: Col) -> Table {
        self.cols.push((name, role, c));
        self
    }
    fn finish(self) -> Result<(RelationSchema, RecordBatch), RunError> {
        let schema = RelationSchema {
            name: self.name.clone(),
            class: "kernel".into(),
            columns: self
                .cols
                .iter()
                .map(|(n, role, c)| ColumnSpec {
                    name: n.to_string(),
                    ty: c.ty(),
                    role: *role,
                    dimension: None,
                })
                .collect(),
            calendar: None,
            constraints: None,
            attributes: None,
            schema_version: Some("1.0".into()),
        };
        let (fields, arrays): (Vec<Field>, Vec<ArrayRef>) = self
            .cols
            .iter()
            .map(|(n, _, c)| {
                let (ty, a) = c.array();
                (Field::new(n, ty, true), a)
            })
            .unzip();
        let batch = RecordBatch::try_new(Arc::new(Schema::new(fields)), arrays).map_err(|e| RunError::Internal(format!("{}: {}", self.name, e)))?;
        Ok((schema, batch))
    }
}

/// An option's terms in an `adjustment` event's detail.
fn terms_json(o: &super::OptionTerms) -> serde_json::Value {
    serde_json::json!({
        "right": o.right,
        "strike": o.strike,
        "expiry": super::time::format_timestamp(o.expiry),
        "style": o.style,
        "settlement": if o.cash { "cash" } else { "physical" },
        "multiplier": o.multiplier,
    })
}

/// How far a run's writes have put the relations that grow with it: the
/// rows of its event log, book, holdings and late tuples (K-7.1, written
/// incrementally at every checkpoint).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Written {
    events: usize,
    book: usize,
    holdings: usize,
    late: usize,
}

/// Every output relation of a run but the ledgers, built (P-4): the rows
/// the growing relations hold past `from` (what the run's checkpoints have
/// not written), and the relations that describe the run as a whole.
#[allow(clippy::too_many_arguments)]
pub fn tables(run: &str, prog: &Program, bound: &Bound, ids: &Ids, cfg: &ExecConfig, result: &RunResult, status: &str, from: Written) -> Result<Tables, RunError> {
    let (mut out, _) = growing(run, prog, ids, result, from)?;
    out.extend(summaries(run, prog, bound, cfg, result, status)?);
    Ok(out)
}

/// The rows past `from` of the relations that grow with the run: the event
/// log, the book, the holdings, the decisions and fills, the late tuples;
/// and how far they go.
pub fn growing(run: &str, prog: &Program, ids: &Ids, result: &RunResult, from: Written) -> Result<(Tables, Written), RunError> {
    let id_of = |s: Sym| ids.get(&s).copied();
    let rule = |r: Option<usize>| r.map(|r| if r == usize::MAX { "silence".to_string() } else { prog.rule_label(r) });
    let ev = result.events.get(from.events..).unwrap_or_default();
    let mut out = Vec::new();
    // The event log (K-5.2).
    {
        let n = ev.len();
        let mut decision = vec![None; n];
        let mut quantity = vec![None; n];
        let mut price = vec![None; n];
        let mut commission = vec![None; n];
        let mut fee = vec![None; n];
        let mut slippage = vec![None; n];
        let mut impact = vec![None; n];
        let mut spread = vec![None; n];
        let mut participation = vec![None; n];
        let mut partial = vec![None; n];
        let mut forced = vec![None; n];
        let mut source = vec![None; n];
        let mut reason = vec![None; n];
        let mut detail = vec![None; n];
        for (i, e) in ev.iter().enumerate() {
            match &e.kind {
                EventKind::Decision { decision: d } | EventKind::OrderSent { decision: d } => decision[i] = Some(result.describe_decision(d)),
                EventKind::Fill { fill, remainder } => {
                    quantity[i] = Some(fill.quantity);
                    price[i] = Some(fill.price);
                    commission[i] = Some(fill.commission);
                    fee[i] = Some(fill.fee);
                    slippage[i] = Some(fill.slippage);
                    impact[i] = Some(fill.impact);
                    spread[i] = Some(fill.spread);
                    participation[i] = Some(fill.participation);
                    partial[i] = Some(fill.partial);
                    forced[i] = Some(fill.forced);
                    source[i] = Some("sim".to_string());
                    let mut d = serde_json::json!({"at_last_price": fill.at_last_price});
                    if let Some((left, fate)) = remainder {
                        d["remainder"] = serde_json::json!(left);
                        d["fate"] = serde_json::json!(format!("{:?}", fate).to_lowercase());
                    }
                    detail[i] = Some(d.to_string());
                }
                EventKind::Dropped { decided, decision: d, reason: why }
                | EventKind::ExpiredOrder { decided, decision: d, reason: why }
                | EventKind::Rejected { decided, decision: d, reason: why } => {
                    decision[i] = Some(result.describe_decision(d));
                    reason[i] = Some(why.clone());
                    detail[i] = Some(serde_json::json!({"decided": super::time::format_timestamp(*decided)}).to_string());
                }
                EventKind::Satisfied { decided } => detail[i] = Some(serde_json::json!({"decided": super::time::format_timestamp(*decided)}).to_string()),
                EventKind::Replaced { decided, by } => detail[i] = Some(serde_json::json!({"decided": super::time::format_timestamp(*decided), "by": by}).to_string()),
                EventKind::DividendDeclared { amount, shares, pay, receivable } => {
                    detail[i] = Some(serde_json::json!({"amount": amount, "shares": shares, "pay": super::time::format_timestamp(*pay), "receivable": receivable}).to_string())
                }
                EventKind::Action {
                    action,
                    position,
                    receivable,
                    price: p,
                } => {
                    let a = match action {
                        Action::Split { factor } => serde_json::json!({"factor": factor}),
                        Action::Dividend { amount, shares } => serde_json::json!({"amount": amount, "shares": shares}),
                        Action::Delisting { reason: r, haircut } => {
                            reason[i] = Some(r.clone());
                            serde_json::json!({"haircut": haircut})
                        }
                        Action::Reinvest { amount, shares, added } => serde_json::json!({"amount": amount, "shares": shares, "added": added}),
                    };
                    price[i] = *p;
                    detail[i] = Some(serde_json::json!({"action": a, "position": position, "receivable": receivable}).to_string());
                }
                EventKind::Funding { flow, amount } => detail[i] = Some(serde_json::json!({"flow": format!("{:?}", flow), "amount": amount}).to_string()),
                EventKind::MarginCall { equity, requirement, policy } => detail[i] = Some(serde_json::json!({"equity": equity, "requirement": requirement, "policy": policy}).to_string()),
                EventKind::LimitTriggered { limit, policy, orders, detail: d } => {
                    reason[i] = Some(format!("limit `{}`: {}", limit, d));
                    detail[i] = Some(serde_json::json!({"limit": limit, "policy": policy, "orders": orders}).to_string());
                }
                EventKind::Exercise {
                    quantity: q,
                    position,
                    underlying,
                    intrinsic,
                    cash_settled,
                    cause,
                }
                | EventKind::Assignment {
                    quantity: q,
                    position,
                    underlying,
                    intrinsic,
                    cash_settled,
                    cause,
                } => {
                    quantity[i] = Some(*q);
                    price[i] = Some(*underlying);
                    reason[i] = Some(cause.clone());
                    let settlement = if *cash_settled { "cash" } else { "physical" };
                    detail[i] = Some(serde_json::json!({"position": position, "intrinsic": intrinsic, "settlement": settlement}).to_string());
                }
                EventKind::OptionExpiry {
                    quantity: q,
                    underlying,
                    intrinsic,
                    cause,
                } => {
                    quantity[i] = Some(*q);
                    price[i] = Some(*underlying);
                    reason[i] = Some(cause.clone());
                    detail[i] = Some(serde_json::json!({"intrinsic": intrinsic}).to_string());
                }
                EventKind::Adjustment {
                    before,
                    after,
                    position,
                    price_divisor,
                    cause,
                } => {
                    reason[i] = Some(cause.clone());
                    detail[i] = Some(serde_json::json!({"before": terms_json(before), "after": terms_json(after), "position": position, "price_divisor": price_divisor}).to_string());
                }
                EventKind::Settlement {
                    price: p,
                    variation,
                    session,
                    at_close,
                } => {
                    price[i] = Some(*p);
                    detail[i] = Some(serde_json::json!({"variation": variation, "session": super::time::format_timestamp(*session), "at_close": at_close}).to_string());
                }
                EventKind::Roll {
                    from,
                    to,
                    quantity: q,
                    from_price,
                    to_price,
                    adjustment,
                    commissions,
                } => {
                    quantity[i] = Some(*q);
                    commission[i] = Some(*commissions);
                    detail[i] = Some(serde_json::json!({"from": id_of(*from), "to": id_of(*to), "from_price": from_price, "to_price": to_price, "adjustment": adjustment}).to_string());
                }
                EventKind::ContractExpiry { quantity: q, price: p, cash_settled } => {
                    quantity[i] = Some(*q);
                    price[i] = Some(*p);
                    detail[i] = Some(serde_json::json!({"settlement": if *cash_settled { "cash" } else { "physical" }}).to_string());
                }
                EventKind::ForcedLiquidation { requirement, equity, fills } => {
                    detail[i] = Some(serde_json::json!({"requirement": requirement, "equity": equity, "fills": fills}).to_string());
                }
            }
        }
        out.push(
            Table::new(run, "events")
                .col("seq", Role::Entity, Col::Entity(ev.iter().map(|e| Some(e.seq)).collect()))
                .col("t", Role::EventTime, Col::Time(ev.iter().map(|e| Some(e.t)).collect()))
                .col("kind", Role::Value, Col::Label(ev.iter().map(|e| Some(e.kind.name().to_string())).collect()))
                .col("instrument", Role::Value, Col::Entity(ev.iter().map(|e| e.instrument.and_then(id_of)).collect()))
                .col("rule", Role::Value, Col::Label(ev.iter().map(|e| rule(e.rule)).collect()))
                .col("order_id", Role::Value, Col::Count(ev.iter().map(|e| e.order_id.map(|o| o as i64)).collect()))
                .col("decision", Role::Value, Col::Bytes(decision))
                .col("quantity", Role::Value, Col::Num(quantity))
                .col("price", Role::Value, Col::Num(price))
                .col("commission", Role::Value, Col::Num(commission))
                .col("fee", Role::Value, Col::Num(fee))
                .col("slippage", Role::Value, Col::Num(slippage))
                .col("impact", Role::Value, Col::Num(impact))
                .col("spread", Role::Value, Col::Num(spread))
                .col("participation", Role::Value, Col::Num(participation))
                .col("partial", Role::Value, Col::Bool(partial))
                .col("forced", Role::Value, Col::Bool(forced))
                .col("source", Role::Value, Col::Label(source))
                .col("reason", Role::Value, Col::Bytes(reason))
                .col("cash_flow", Role::Value, Col::Num(ev.iter().map(|e| e.cash_flow).collect()))
                .col("detail", Role::Value, Col::Bytes(detail))
                .finish()?,
        );
    }
    // The book at every decision barrier (K-5.1): the book's rows, and its
    // holdings' (E-52).
    let x = result.exposure.get(from.book..).unwrap_or_default();
    out.push(
        Table::new(run, "book")
            .col("t", Role::EventTime, Col::Time(x.iter().map(|r| Some(r.t)).collect()))
            .col("cash", Role::Value, Col::Num(x.iter().map(|r| Some(r.cash)).collect()))
            .col("debit", Role::Value, Col::Num(x.iter().map(|r| Some((-r.cash).max(0.0))).collect()))
            .col("receivables", Role::Value, Col::Num(x.iter().map(|r| Some(r.receivables)).collect()))
            .col("variation_account", Role::Value, Col::Num(x.iter().map(|_| Some(0.0)).collect()))
            .col("nav", Role::Value, Col::Num(x.iter().map(|r| Some(r.equity)).collect()))
            .col("capital", Role::Value, Col::Num(x.iter().map(|r| Some(r.capital)).collect()))
            .col("gross", Role::Value, Col::Num(x.iter().map(|r| Some(r.gross)).collect()))
            .col("net", Role::Value, Col::Num(x.iter().map(|r| Some(r.net)).collect()))
            .col("leverage", Role::Value, Col::Num(x.iter().map(|r| Some(r.leverage)).collect()))
            .col("margin_initial", Role::Value, Col::Num(x.iter().map(|r| Some(r.margin_initial)).collect()))
            .col("margin_maintenance", Role::Value, Col::Num(x.iter().map(|r| Some(r.margin_maintenance)).collect()))
            .col("margin_source", Role::Value, Col::Label(x.iter().map(|_| Some("approx".to_string())).collect()))
            .col("excess_liquidity", Role::Value, Col::Num(x.iter().map(|r| Some(r.equity - r.margin_maintenance)).collect()))
            .finish()?,
    );
    // A holding is of an instrument the data layer has (it was filled at
    // its price).
    let h: Vec<&super::HoldingRecord> = result.holdings.get(from.holdings..).unwrap_or_default().iter().filter(|r| id_of(r.instrument).is_some()).collect();
    out.push(
        Table::new(run, "holdings")
            .col("instrument", Role::Entity, Col::Entity(h.iter().map(|r| id_of(r.instrument)).collect()))
            .col("t", Role::EventTime, Col::Time(h.iter().map(|r| Some(r.t)).collect()))
            .col("position", Role::Value, Col::Num(h.iter().map(|r| Some(r.position)).collect()))
            .col("avg_cost", Role::Value, Col::Num(h.iter().map(|r| Some(r.avg_cost)).collect()))
            .col("mark", Role::Value, Col::Num(h.iter().map(|r| r.mark).collect()))
            .col("mark_source", Role::Value, Col::Label(h.iter().map(|r| Some(r.mark_source.clone())).collect()))
            .finish()?,
    );
    // The decisions and fills: projections of the log, keyed by the
    // event's place in it (E-52).
    let d: Vec<(&super::RunEvent, &super::Decision)> = ev
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::Decision { decision } => Some((e, decision)),
            _ => None,
        })
        .collect();
    out.push(
        Table::new(run, "decisions")
            .col("seq", Role::Entity, Col::Entity(d.iter().map(|(e, _)| Some(e.seq)).collect()))
            .col("t", Role::EventTime, Col::Time(d.iter().map(|(e, _)| Some(e.t)).collect()))
            .col("instrument", Role::Value, Col::Entity(d.iter().map(|(_, x)| id_of(x.equity)).collect()))
            .col("ctor", Role::Value, Col::Label(d.iter().map(|(_, x)| Some(x.ctor.resolved_name().to_string())).collect()))
            .col("amount", Role::Value, Col::Num(d.iter().map(|(_, x)| Some(x.amount)).collect()))
            .col("order", Role::Value, Col::Bytes(d.iter().map(|(_, x)| Some(x.order.to_string())).collect()))
            .col("rule", Role::Value, Col::Label(d.iter().map(|(e, _)| rule(e.rule)).collect()))
            .col("late", Role::Value, Col::Bool(d.iter().map(|(e, _)| Some(result.late_bars.contains(&e.t))).collect()))
            .finish()?,
    );
    let fills: Vec<(u64, &super::FillRecord, Option<u64>)> = ev
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::Fill { fill, .. } => Some((e.seq, fill, e.order_id)),
            _ => None,
        })
        .collect();
    out.push(
        Table::new(run, "fills")
            .col("seq", Role::Entity, Col::Entity(fills.iter().map(|f| Some(f.0)).collect()))
            .col("t", Role::EventTime, Col::Time(fills.iter().map(|f| Some(f.1.t)).collect()))
            .col("instrument", Role::Value, Col::Entity(fills.iter().map(|f| id_of(f.1.equity)).collect()))
            .col("quantity", Role::Value, Col::Num(fills.iter().map(|f| Some(f.1.quantity)).collect()))
            .col("price", Role::Value, Col::Num(fills.iter().map(|f| Some(f.1.price)).collect()))
            .col("commission", Role::Value, Col::Num(fills.iter().map(|f| Some(f.1.commission)).collect()))
            .col("fee", Role::Value, Col::Num(fills.iter().map(|f| Some(f.1.fee)).collect()))
            .col("slippage", Role::Value, Col::Num(fills.iter().map(|f| Some(f.1.slippage)).collect()))
            .col("impact", Role::Value, Col::Num(fills.iter().map(|f| Some(f.1.impact)).collect()))
            .col("participation", Role::Value, Col::Num(fills.iter().map(|f| Some(f.1.participation)).collect()))
            .col("order_id", Role::Value, Col::Count(fills.iter().map(|f| f.2.map(|o| o as i64)).collect()))
            .col(
                "multiplier",
                Role::Value,
                Col::Num(fills.iter().map(|f| Some(result.multipliers.get(&f.1.equity).copied().unwrap_or(1.0))).collect()),
            )
            .col("forced", Role::Value, Col::Bool(fills.iter().map(|f| Some(f.1.forced)).collect()))
            .col("partial", Role::Value, Col::Bool(fills.iter().map(|f| Some(f.1.partial)).collect()))
            .col("source", Role::Value, Col::Label(fills.iter().map(|_| Some("sim".to_string())).collect()))
            .finish()?,
    );
    // The late tuples.
    let l = result.late.get(from.late..).unwrap_or_default();
    out.push(
        Table::new(run, "late")
            .col("seq", Role::Entity, Col::Entity(l.iter().map(|r| Some(r.seq)).collect()))
            .col("t", Role::EventTime, Col::Time(l.iter().map(|r| Some(r.t)).collect()))
            .col("relation", Role::Value, Col::Label(l.iter().map(|r| Some(r.relation.clone())).collect()))
            .col("entity", Role::Value, Col::Entity(l.iter().map(|r| r.entity.and_then(id_of)).collect()))
            .col("r", Role::Value, Col::Time(l.iter().map(|r| Some(r.r)).collect()))
            .col("changed_decision", Role::Value, Col::Bool(l.iter().map(|_| Some(false)).collect()))
            .finish()?,
    );
    let upto = Written {
        events: result.events.len(),
        book: result.exposure.len(),
        holdings: result.holdings.len(),
        late: result.late.len(),
    };
    Ok((out, upto))
}

/// The relations that describe the run as a whole, at its last decision
/// bar: the coverage, the contract, the binding, the plans and the report.
fn summaries(run: &str, prog: &Program, bound: &Bound, cfg: &ExecConfig, result: &RunResult, status: &str) -> Result<Tables, RunError> {
    let end = result.bars.last().copied().unwrap_or(0);
    let mut out = Vec::new();
    // The coverage (C summarised per relation).
    let c = &result.coverage;
    out.push(
        Table::new(run, "coverage")
            .col("relation", Role::Label, Col::Label(c.iter().map(|r| Some(r.name.clone())).collect()))
            .col("t", Role::EventTime, Col::Time(c.iter().map(|_| Some(end)).collect()))
            .col("calls", Role::Value, Col::Count(c.iter().map(|r| Some(r.calls as i64)).collect()))
            .col("with_tuples", Role::Value, Col::Count(c.iter().map(|r| Some(r.nonempty as i64)).collect()))
            .col("tuples", Role::Value, Col::Count(c.iter().map(|r| Some(r.tuples as i64)).collect()))
            .col("empty", Role::Value, Col::Bool(c.iter().map(|r| Some(r.calls > 0 && r.tuples == 0)).collect()))
            .col("never_demanded", Role::Value, Col::Bool(c.iter().map(|r| Some(r.calls == 0)).collect()))
            .finish()?,
    );
    // The contract with its statuses (K-3.7).
    let k = &result.contract.clauses;
    out.push(
        Table::new(run, "contract")
            .col("clause", Role::Label, Col::Label(k.iter().map(|c| Some(c.id.clone())).collect()))
            .col("t", Role::EventTime, Col::Time(k.iter().map(|_| Some(end)).collect()))
            .col("kind", Role::Value, Col::Label(k.iter().map(|c| Some(c.kind.name().to_string())).collect()))
            .col("source", Role::Value, Col::Label(k.iter().map(|c| Some(c.source.clone())).collect()))
            .col(
                "discharge",
                Role::Value,
                Col::Label(
                    k.iter()
                        .map(|c| {
                            Some(
                                match c.discharge {
                                    crate::contract::Discharge::Static(_) => "static",
                                    crate::contract::Discharge::Bind => "bind",
                                    crate::contract::Discharge::Runtime => "runtime",
                                    crate::contract::Discharge::Report => "report",
                                }
                                .to_string(),
                            )
                        })
                        .collect(),
                ),
            )
            .col(
                "tier",
                Role::Value,
                Col::Count(
                    k.iter()
                        .map(|c| match (c.discharge, &c.status) {
                            (crate::contract::Discharge::Static(t), _) => Some(t as i64),
                            (_, crate::contract::Status::Proven { tier }) => Some(*tier as i64),
                            _ => None,
                        })
                        .collect(),
                ),
            )
            .col("policy", Role::Value, Col::Label(k.iter().map(|c| Some(c.policy.to_string())).collect()))
            .col(
                "status",
                Role::Value,
                Col::Label(
                    k.iter()
                        .map(|c| {
                            Some(
                                match &c.status {
                                    crate::contract::Status::Open => "open",
                                    crate::contract::Status::Proven { .. } => "proven",
                                    crate::contract::Status::Checked { .. } => "checked",
                                    crate::contract::Status::Violated { .. } => "violated",
                                    crate::contract::Status::Warned { .. } => "warned",
                                    crate::contract::Status::NotApplicable { .. } => "not_applicable",
                                }
                                .to_string(),
                            )
                        })
                        .collect(),
                ),
            )
            .col(
                "detail",
                Role::Value,
                Col::Bytes(k.iter().map(|c| Some(serde_json::json!({"text": c.text, "status": c.status, "fired": c.fired}).to_string())).collect()),
            )
            .finish()?,
    );
    // The binding (β), the parameter point, the program hash and its normal form.
    {
        let nir = crate::compiler::normalise(prog);
        let point: serde_json::Value = prog
            .params
            .iter()
            .map(|(unit, ps)| {
                (
                    unit.clone(),
                    serde_json::json!(ps.iter().map(|(n, p)| (n.clone(), format!("{}", p.value))).collect::<std::collections::BTreeMap<_, _>>()),
                )
            })
            .collect::<serde_json::Map<_, _>>()
            .into();
        let rows: Vec<(String, String)> = bound.binding.pairs.iter().flatten().map(|v| (v.relation.clone(), v.version.clone())).collect();
        let n = rows.len();
        out.push(
            Table::new(run, "binding")
                .col("relation", Role::Label, Col::Label(rows.iter().map(|r| Some(r.0.clone())).collect()))
                .col("t", Role::EventTime, Col::Time(vec![Some(end); n]))
                .col("version", Role::Value, Col::Label(rows.iter().map(|r| Some(r.1.clone())).collect()))
                .col("program_hash", Role::Value, Col::Label(vec![Some(nir.hash.clone()); n]))
                .col("binding_id", Role::Value, Col::Label(vec![bound.binding.id.clone(); n]))
                .col("mode", Role::Value, Col::Label(vec![Some("replay-sim".to_string()); n]))
                .col("deployment_hash", Role::Value, Col::Label(vec![None; n]))
                .col("point", Role::Value, Col::Bytes(vec![Some(point.to_string()); n]))
                .col("normal_form", Role::Value, Col::Bytes(vec![Some(nir.source(prog)); n]))
                .col("configuration", Role::Value, Col::Bytes(vec![Some(serde_json::to_string(cfg).unwrap_or_default()); n]))
                .finish()?,
        );
    }
    // The plans: per rule and literal, its pushdown and the observed counts.
    {
        let mut rule_l = Vec::new();
        let mut pos = Vec::new();
        let mut lit = Vec::new();
        let mut class = Vec::new();
        let mut pushdown = Vec::new();
        let mut calls = Vec::new();
        let mut tuples = Vec::new();
        for (ri, r) in prog.rules.iter().enumerate() {
            for (li, l) in r.body.iter().enumerate() {
                rule_l.push(Some(prog.rule_label(ri)));
                pos.push(Some(li as u64));
                lit.push(Some(crate::compiler::print::render_literal(l, &|v: &str| v.to_string())));
                class.push(Some(
                    match l {
                        crate::ir::Literal::Atom(_) => "generator",
                        crate::ir::Literal::Agg { .. } | crate::ir::Literal::Assign { .. } | crate::ir::Literal::Top { .. } => "computation",
                        _ => "test",
                    }
                    .to_string(),
                ));
                pushdown.push(Some(
                    matches!(l, crate::ir::Literal::Agg { .. }) && crate::kernel::pushdown::eligibility(prog, ri, li).is_ok() && cfg.pushdown,
                ));
                let counts = result.literal_counts.get(ri).and_then(|c| c.get(li)).copied();
                calls.push(counts.map(|c| c.0 as i64));
                tuples.push(counts.map(|c| c.1 as i64));
            }
        }
        let n = rule_l.len();
        out.push(
            Table::new(run, "plans")
                .col("rule", Role::Label, Col::Label(rule_l))
                .col("position", Role::Entity, Col::Entity(pos))
                .col("t", Role::EventTime, Col::Time(vec![Some(end); n]))
                .col("literal", Role::Value, Col::Bytes(lit))
                .col("class", Role::Value, Col::Label(class))
                .col("estimated_cost", Role::Value, Col::Num(vec![None; n]))
                .col("pushdown", Role::Value, Col::Bool(pushdown))
                .col("observed_calls", Role::Value, Col::Count(calls))
                .col("observed_tuples", Role::Value, Col::Count(tuples))
                .finish()?,
        );
    }
    // The report: the status, the warnings with their biases, the counters.
    {
        let mut rows: Vec<(String, String, Option<String>)> = vec![
            ("status".into(), serde_json::json!(status).to_string(), None),
            ("bars".into(), serde_json::json!(result.bars.len()).to_string(), None),
            ("final_cash".into(), serde_json::json!(result.final_cash).to_string(), None),
            ("base_capital".into(), serde_json::json!(result.base_capital).to_string(), None),
            ("periods_per_year".into(), serde_json::json!(result.periods.periods_per_year).to_string(), None),
            ("period_by_day".into(), serde_json::json!(result.periods.by_day).to_string(), None),
            ("costs".into(), serde_json::to_string(&result.costs).unwrap_or_default(), None),
            ("liquidity".into(), serde_json::to_string(&result.liquidity).unwrap_or_default(), None),
            ("funding".into(), serde_json::to_string(&result.funding).unwrap_or_default(), None),
            ("stats".into(), serde_json::to_string(&result.stats).unwrap_or_default(), None),
            ("plan".into(), serde_json::json!(result.plan).to_string(), None),
        ];
        if let Some(days) = &result.periods.days {
            rows.push(("period_days".into(), serde_json::json!(days).to_string(), None));
        }
        if let Some(note) = &result.periods.note {
            rows.push(("period_note".into(), serde_json::json!(note).to_string(), None));
        }
        for (i, w) in result.warnings.iter().enumerate() {
            rows.push((format!("warning.{}", i), serde_json::json!(w.message).to_string(), Some(w.bias.clone())));
        }
        let n = rows.len();
        out.push(
            Table::new(run, "report")
                .col("key", Role::Label, Col::Label(rows.iter().map(|r| Some(r.0.clone())).collect()))
                .col("t", Role::EventTime, Col::Time(vec![Some(end); n]))
                .col("value", Role::Value, Col::Bytes(rows.iter().map(|r| Some(r.1.clone())).collect()))
                .col("bias", Role::Value, Col::Label(rows.iter().map(|r| r.2.clone()).collect()))
                .finish()?,
        );
    }
    Ok(out)
}

/// The ledger of every observed relation (V-8.1, Definition V-8.3):
/// `kernel.<run>.ledger.<R>` holds R's tuples at every bar of R's calendar
/// inside the run, its `where` bindings applied, and the instances no
/// decision demanded that are stuck, flagged with the halt they would have
/// been. A bar's rows are keyed by their place among them (E-52); R's
/// arguments are values. With the notes of what cannot be observed.
pub fn ledgers(run: &str, prog: &Program, k: &mut Kernel, ids: &Ids, result: &RunResult) -> Result<(Tables, Vec<String>), RunError> {
    let mut out = Vec::new();
    let mut notes = Vec::new();
    for o in &prog.observes {
        match ledger(run, &format!("ledger.{}", o.rel), prog, k, ids, result, &o.rel, &o.wheres)? {
            Ok(t) => out.push(t),
            Err(note) => notes.push(format!("`observe {}`: {}", o.rel, note)),
        }
    }
    Ok((out, notes))
}

/// `ledger R into NAME` (V-8.3): R's ledger written as `kernel.<run>.NAME`;
/// the rows written.
pub fn ledger_into(run: &str, prog: &Program, k: &mut Kernel, client: &DataClient, result: &RunResult, rel: &str, into: &str) -> Result<usize, RunError> {
    if !prog.relations.contains_key(rel) {
        return Err(RunError::Request(format!("`{}` is not a relation of {}", rel, prog.strategy)));
    }
    let ids = k.data_ids.clone();
    match ledger(run, into, prog, k, &ids, result, rel, &[])? {
        Ok((schema, batch)) => {
            let n = batch.num_rows();
            write(client, vec![(schema, batch)])?;
            Ok(n)
        }
        Err(note) => Err(RunError::Request(format!("`ledger {}`: {}", rel, note))),
    }
}

/// One relation's ledger as the relation `kernel.<run>.<name>`, or why it
/// has none.
#[allow(clippy::too_many_arguments)]
fn ledger(
    run: &str,
    name: &str,
    prog: &Program,
    k: &mut Kernel,
    ids: &Ids,
    result: &RunResult,
    rel_name: &str,
    wheres: &[(String, crate::Lit)],
) -> Result<Result<(RelationSchema, RecordBatch), String>, RunError> {
    let (Some(&first), Some(&last)) = (result.bars.first(), result.bars.last()) else {
        return Ok(Err("the run has no bar".into()));
    };
    let Some(sig) = prog.relations.get(rel_name) else {
        return Ok(Err("not a relation of the program".into()));
    };
    let Some(rel) = k.relation_id(rel_name) else {
        return Ok(Err("not a relation the kernel holds".into()));
    };
    let info = k.rels[rel].clone();
    let width = sig.args.len();
    let mut rows: Vec<(i64, Vec<Option<Value>>, Option<String>)> = Vec::new();
    if !info.inputs.is_empty() {
        return Ok(Err("the relation takes inputs; its extension is not enumerable".into()));
    }
    let mut pattern: Vec<Option<Value>> = vec![None; width];
    for (v, lit) in wheres {
        if let Some(i) = sig.args.iter().position(|a| a.name == *v) {
            pattern[i] = Some(super::lit_value(lit, &mut k.symbols, &mut k.labels, &k.literal_equities));
        }
    }
    // A block hold-out's bars are left out (Q-2.3).
    let mask = k.cfg.embargo_mask.clone();
    let bars: Vec<i64> = k
        .bars_at(&sig.cal)
        .into_iter()
        .filter(|&b| b >= first && b <= last && !mask.iter().any(|&(a, z)| a <= b && b < z))
        .collect();
    for t in bars {
        pattern[info.key_pos] = Some(Value::Time(t));
        let (mut live, mut stuck) = k.observe_at(rel, &pattern)?;
        live.sort();
        stuck.sort_by(|a, b| a.0.cmp(&b.0));
        rows.extend(live.into_iter().map(|tu| (t, tu.into_iter().map(Some).collect(), None)));
        rows.extend(stuck.into_iter().map(|(p, e)| (t, p, Some(e.to_string()))));
    }
    // The row's place among its bar's rows.
    let mut place: Vec<Option<u64>> = Vec::with_capacity(rows.len());
    for (i, r) in rows.iter().enumerate() {
        place.push(Some(if i > 0 && rows[i - 1].0 == r.0 { place[i - 1].unwrap_or(0) + 1 } else { 0 }));
    }
    let mut table = Table::new(run, name)
        .col("bar", Role::EventTime, Col::Time(rows.iter().map(|r| Some(r.0)).collect()))
        .col("row", Role::Entity, Col::Entity(place));
    let reserved = ["bar", "row", "stuck", "stuck_literal"];
    for (j, a) in sig.args.iter().enumerate() {
        let values = rows.iter().map(|r| r.1[j].as_ref());
        let col = match &a.ty {
            Ty::Entity => Col::Entity(values.map(|v| v.and_then(|v| v.as_equity()).and_then(|s| ids.get(&s).copied())).collect()),
            Ty::Timestamp => Col::Time(values.map(|v| v.and_then(|v| v.as_time())).collect()),
            Ty::Count => Col::Count(
                values
                    .map(|v| match v {
                        Some(Value::Count(c)) => Some(*c),
                        _ => None,
                    })
                    .collect(),
            ),
            Ty::Quantity(_) => Col::Num(
                values
                    .map(|v| match v {
                        Some(Value::Num(x)) => Some(*x),
                        Some(Value::Count(c)) => Some(*c as f64),
                        _ => None,
                    })
                    .collect(),
            ),
            Ty::Label => Col::Label(values.map(|v| v.map(|v| k.show(v))).collect()),
            _ => Col::Bytes(values.map(|v| v.map(|v| k.show(v))).collect()),
        };
        let mut name = a.name.clone();
        while reserved.contains(&name.as_str()) || sig.args.iter().enumerate().any(|(i, b)| i != j && b.name == name) {
            name.push('_');
        }
        table = table.col_named(name, Role::Value, col);
    }
    Ok(Ok(table
        .col("stuck", Role::Value, Col::Bool(rows.iter().map(|r| Some(r.2.is_some())).collect()))
        .col("stuck_literal", Role::Value, Col::Bytes(rows.iter().map(|r| r.2.clone()).collect()))
        .finish()?))
}

/// Writes the run's outputs through the data layer: the versions put, by
/// relation. The rows of a relation that grows with the run are added to
/// what the run's earlier writes put (a row already there must be the same
/// row: a resumed run is the run); every other relation describes the
/// execution that wrote it whole and replaces the last (E-56).
pub fn write(client: &DataClient, tables: Tables) -> Result<Vec<(String, String)>, RunError> {
    let mut out = Vec::new();
    for (schema, batch) in tables {
        let name = schema.name.clone();
        // `kernel.<run>.<relation>`: a ledger's is `ledger.<R>`.
        let grows = name.splitn(3, '.').nth(2).is_some_and(|r| GROWING.contains(&r));
        let version = if grows { client.put(schema, &[batch])? } else { client.replace(schema, &[batch])? };
        out.push((name, version));
    }
    Ok(out)
}
