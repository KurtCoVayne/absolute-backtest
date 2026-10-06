//! The executor behind a trait (data-bundle doc, section 2: "the executor is
//! in the loop", and the only component that differs between a backtest and
//! a live run). `SimExecutor` is the simulated one of the semantic model's
//! section 6 with the realism models of the data-bundle doc's section 5; a
//! live executor would send orders from `on_decisions` and feed its fills
//! back as events.

use std::collections::{BTreeMap, HashMap, HashSet};

use super::time;
use super::value::{OrderKind, Tif};
use super::{
    Action, ActionRecord, Ctor, Decision, DecisionRecord, ExecConfig, ExposureRecord, FillRecord, Kernel, LiquiditySummary, Lot, OnLeverage, OnMarginCall, OnOversize, OnRuin, RunError, RunResult,
    RunWarning, Sym, Value,
};
use crate::ir::*;

/// What the kernel's driver (the batch loop or the fold) asks of an executor
/// at each decision bar, in this order: `fill` the orders of the previous
/// bar at the new bar, `open_bar` (actions, mark, margin), then, after the
/// strategy decided, `on_decisions`; `finish` when the data ends.
pub trait Executor {
    fn open_bar(&mut self, k: &mut Kernel, t: i64, result: &mut RunResult) -> Result<(), RunError>;
    fn on_decisions(&mut self, k: &mut Kernel, t: i64, by_equity: &BTreeMap<Sym, Vec<(Decision, usize)>>, result: &mut RunResult) -> Result<(), RunError>;
    fn fill(&mut self, k: &mut Kernel, tn: i64, result: &mut RunResult) -> Result<(), RunError>;
    fn finish(&mut self, k: &mut Kernel, result: &mut RunResult);
    /// The executor's state for a checkpoint (data-bundle doc, section 2),
    /// and the state back from one.
    fn checkpoint(&self) -> Result<serde_json::Value, String>;
    fn restore(&mut self, state: serde_json::Value) -> Result<(), String>;
}

/// An order that does not execute at the next bar's close (section 6, order
/// types): market-on-open, market-on-close, limit and stop orders work
/// until they execute, expire by their time in force, or a new decision on
/// the instrument supersedes them.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct Working {
    d: Decision,
    rule: usize,
    /// The decision bar.
    t: i64,
    /// The calendar day of the first bar the order could execute at (what
    /// `day` measures).
    first_day: Option<i64>,
    /// Bars the order has worked.
    bars: u32,
    /// For a market-on-close order, the session's last bar (looked up once).
    session_last: Option<Option<i64>>,
}

/// An order to fill at this step: the decision, its rule, whether it is an
/// open target re-issued, and the base price and bar of an order that
/// executes away from this bar's close.
type Pending = (Decision, usize, bool, Option<(f64, i64)>);

/// What a working order does at a bar.
enum Trigger {
    /// Execute at this base price, at this bar.
    Fill(f64, i64),
    Wait,
    Expire(String),
}

/// The simulated executor: the book, the open targets, the receivables and
/// the counters behind the run's summaries.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct SimExecutor {
    cfg: ExecConfig,
    cash: f64,
    positions: BTreeMap<Sym, f64>,
    /// Decision bars seen so far (what ADV and realized volatility look back over).
    bars: Vec<i64>,
    /// The decisions of the last bar, to be filled at the next.
    pending: BTreeMap<Sym, Vec<(Decision, usize)>>,
    pending_t: Option<i64>,
    open_targets: BTreeMap<Sym, (Decision, usize)>,
    /// Orders working beyond the bar after their decision.
    #[serde(default)]
    working: BTreeMap<Sym, Working>,
    requested_total: f64,
    filled_total: f64,
    participations: Vec<f64>,
    margin_calls: usize,
    /// (pay, sym, cash, amount, shares).
    receivables: Vec<(i64, Sym, f64, f64, f64)>,
    delisted_done: HashSet<Sym>,
    zero_haircut_involuntary: usize,
    not_shortable: usize,
    shorts_held: bool,
}

impl SimExecutor {
    pub fn new(cfg: ExecConfig) -> SimExecutor {
        SimExecutor {
            cash: cfg.initial_cash,
            cfg,
            positions: BTreeMap::new(),
            bars: Vec::new(),
            pending: BTreeMap::new(),
            pending_t: None,
            open_targets: BTreeMap::new(),
            working: BTreeMap::new(),
            requested_total: 0.0,
            filled_total: 0.0,
            participations: Vec::new(),
            margin_calls: 0,
            receivables: Vec::new(),
            delisted_done: HashSet::new(),
            zero_haircut_involuntary: 0,
            not_shortable: 0,
            shorts_held: false,
        }
    }

    /// Commission on an order of `qty` of `sym`: a future's per-contract
    /// rate when the security table gives one, else the per-share schedule.
    /// The basis-point commission on both sides is added on the notional
    /// (`price` per unit, times the multiplier).
    fn commission(&self, k: &Kernel, sym: Sym, qty: f64, price: f64) -> f64 {
        let bps = self.cfg.commission_bps * qty.abs() * price.abs() * k.multiplier(sym) / 10_000.0;
        if let Some(c) = k.commission_per_contract(sym) {
            return c * qty.abs() + bps;
        }
        if self.cfg.commission_per_share == 0.0 && self.cfg.commission_min_per_order == 0.0 {
            bps
        } else {
            (self.cfg.commission_per_share * qty.abs()).max(self.cfg.commission_min_per_order) + bps
        }
    }

    /// Whether a working order executes at `tn`, and at what base price.
    fn trigger(&self, k: &mut Kernel, w: &mut Working, tn: i64) -> Trigger {
        let sym = w.d.equity;
        let name = k.symbols.name(sym).to_string();
        let day = time::day_key(tn);
        let first = *w.first_day.get_or_insert(day);
        match w.d.order.kind {
            OrderKind::Market => match k.bar_price(sym, tn) {
                Some(p) => Trigger::Fill(p, tn),
                None => Trigger::Expire(format!("no price for {} at {}", name, time::format_timestamp(tn))),
            },
            OrderKind::Moc => {
                let last = *w.session_last.get_or_insert_with(|| k.session_last_bar(sym, w.t));
                match last {
                    None => Trigger::Expire(format!("moc: no price for {} in the session of {}", name, time::format_timestamp(w.t))),
                    // At the step of its closing bar, like a market order at its fill bar.
                    Some(l) if l <= tn => match k.bar_price(sym, l) {
                        Some(p) => Trigger::Fill(p, l),
                        None => Trigger::Expire(format!("moc: no close for {} at {}", name, time::format_timestamp(l))),
                    },
                    Some(_) => Trigger::Wait,
                }
            }
            OrderKind::Moo | OrderKind::MooMoc => match k.bar_open(sym, tn) {
                Some(o) => Trigger::Fill(o, tn),
                None if day != first => Trigger::Expire(format!("moo: {} did not open in the session after {}", name, time::format_timestamp(w.t))),
                None => Trigger::Wait,
            },
            OrderKind::Limit(level) | OrderKind::Stop(level) => {
                let expired = match w.d.order.tif {
                    Tif::Day => day != first,
                    Tif::Gtc => false,
                    Tif::Bars(n) => w.bars >= n,
                };
                if expired {
                    return Trigger::Expire(format!("{} expired unfilled ({})", w.d.order, name));
                }
                w.bars += 1;
                let (Some(o), Some(h), Some(l)) = (k.bar_open(sym, tn), k.bar_high(sym, tn), k.bar_low(sym, tn)) else {
                    return Trigger::Wait;
                };
                // The side of the order: a delta constructor's, or a target's
                // from where the book is against it at the open.
                let pos = self.positions.get(&sym).copied().unwrap_or(0.0);
                let buying = match w.d.ctor {
                    Ctor::Buy | Ctor::Cover => true,
                    Ctor::Sell | Ctor::Short => false,
                    Ctor::TargetQuantity => w.d.amount > pos,
                    Ctor::TargetWeight => {
                        let base = if self.cfg.compounding { self.cash.max(0.0) } else { self.cfg.initial_cash };
                        w.d.amount * base > pos * o * k.multiplier(sym)
                    }
                };
                let limit = matches!(w.d.order.kind, OrderKind::Limit(_));
                // A limit buys at or below its level, a stop at or above
                // (mirrored for sells); a bar that opens through the level
                // fills at the open.
                let fill = match (limit, buying) {
                    (true, true) => (o <= level).then_some(o).or((l <= level).then_some(level)),
                    (true, false) => (o >= level).then_some(o).or((h >= level).then_some(level)),
                    (false, true) => (o >= level).then_some(o).or((h >= level).then_some(level)),
                    (false, false) => (o <= level).then_some(o).or((l <= level).then_some(level)),
                };
                match fill {
                    Some(p) => Trigger::Fill(p, tn),
                    None => Trigger::Wait,
                }
            }
        }
    }

    /// Rewrite the executor relations at `t` after the book changed outside a fill.
    fn rewrite_book(&self, k: &mut Kernel, t: i64) {
        let position = k.rel_id("position");
        let cash_rel = k.rel_id("cash");
        if let Some(tus) = k.stores[position].by_time.get_mut(&t) {
            tus.clear();
        }
        for (&sym, &q) in &self.positions {
            k.stores[position].insert(t, vec![Value::Equity(sym), Value::Time(t), Value::Num(q)]);
        }
        if let Some(tus) = k.stores[cash_rel].by_time.get_mut(&t) {
            tus.clear();
        }
        k.stores[cash_rel].insert(t, vec![Value::Time(t), Value::Num(self.cash)]);
    }

    /// Corporate actions and delistings at the bar (data-bundle doc, section
    /// 4), before the mark: a split multiplies the position at its ex-date, a
    /// dividend's receivable is recorded at the ex-date and paid at the pay
    /// date, a delisted name is force-closed at its last trade less the
    /// haircut for the reason.
    fn actions(&mut self, k: &mut Kernel, t: i64, result: &mut RunResult) -> bool {
        let fill_rel = k.rel_id("fill");
        let mut book_changed = false;
        let split_rel = if self.cfg.actions_in_prices { None } else { k.split_rel };
        let dividend_rel = if self.cfg.actions_in_prices { None } else { k.dividend_rel };
        if let Some(id) = split_rel {
            let entity_pos = k.rels[id].entity_positions.first().copied().unwrap_or(0);
            let splits: Vec<(Sym, f64)> = k.stores[id]
                .by_time
                .get(&t)
                .map(|tus| tus.iter().filter_map(|tu| Some((tu[entity_pos].as_equity()?, tu.iter().rev().find_map(|v| v.as_f64())?))).collect())
                .unwrap_or_default();
            for (sym, factor) in splits {
                let Some(pos) = self.positions.get(&sym).copied() else { continue };
                if factor <= 0.0 || factor == 1.0 {
                    continue;
                }
                let exact = pos * factor;
                let kept = if self.cfg.lot == Lot::Whole { exact.trunc() } else { exact };
                let m = k.multiplier(sym);
                let fraction_cash = k.price_at(sym, t).map(|p| (exact - kept) * p * m).unwrap_or(0.0);
                self.cash += fraction_cash;
                if kept.abs() < 1e-9 {
                    self.positions.remove(&sym);
                } else {
                    self.positions.insert(sym, kept);
                }
                if let Some(lp) = k.last_price.get_mut(&sym) {
                    *lp /= factor;
                }
                result.actions.push(ActionRecord {
                    t,
                    equity: sym,
                    action: Action::Split { factor },
                    cash: fraction_cash,
                });
                book_changed = true;
            }
        }
        // Dividends going ex at t: a receivable for the shares held now, or
        // shares bought with it at the ex-date close.
        let mut reinvest: Vec<(Sym, f64, f64)> = Vec::new();
        if let Some(id) = dividend_rel {
            let info = &k.rels[id];
            let entity_pos = info.entity_positions.first().copied().unwrap_or(0);
            let sig = &k.prog.relations[&info.name];
            let times: Vec<usize> = sig.args.iter().enumerate().filter(|(i, a)| a.ty == Ty::Timestamp && *i != info.key_pos).map(|(i, _)| i).collect();
            let amount_col = sig.args.iter().position(|a| a.mode == Mode::Out && matches!(&a.ty, Ty::Quantity(d) if d.c2 == 2 && d.s2 == -2));
            if let (Some(&ex_col), Some(&pay_col), Some(amount_col)) = (times.first(), times.get(1), amount_col) {
                for tus in k.stores[id].by_time.range(..=t).map(|(_, v)| v) {
                    for tu in tus {
                        if let (Some(sym), Some(ex), Some(pay), Some(amount)) = (tu[entity_pos].as_equity(), tu[ex_col].as_time(), tu[pay_col].as_time(), tu[amount_col].as_f64()) {
                            if ex == t {
                                if let Some(&pos) = self.positions.get(&sym) {
                                    if self.cfg.reinvest_dividends {
                                        reinvest.push((sym, amount, pos));
                                    } else {
                                        self.receivables.push((pay.max(ex), sym, pos * amount, amount, pos));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        for (sym, amount, shares) in reinvest {
            let Some(p) = k.price_at(sym, t).filter(|p| *p > 0.0) else { continue };
            let added = shares * amount / p;
            self.positions.insert(sym, shares + added);
            result.actions.push(ActionRecord {
                t,
                equity: sym,
                action: Action::Reinvest { amount, shares, added },
                cash: 0.0,
            });
            book_changed = true;
        }
        let (due, later): (Vec<_>, Vec<_>) = self.receivables.drain(..).partition(|(pay, ..)| *pay <= t);
        self.receivables = later;
        for (_, sym, amount_cash, amount, shares) in due {
            self.cash += amount_cash;
            result.actions.push(ActionRecord {
                t,
                equity: sym,
                action: Action::Dividend { amount, shares },
                cash: amount_cash,
            });
            book_changed = true;
        }
        if let Some(id) = k.delisted_rel {
            let entity_pos = k.rels[id].entity_positions.first().copied().unwrap_or(0);
            let gone: Vec<(Sym, String)> = k.stores[id]
                .by_time
                .get(&t)
                .map(|tus| {
                    tus.iter()
                        .filter_map(|tu| {
                            let sym = tu[entity_pos].as_equity()?;
                            let reason = tu
                                .iter()
                                .find_map(|v| if let Value::Label(l) = v { Some(k.labels.name(*l).to_string()) } else { None })
                                .unwrap_or_default();
                            Some((sym, reason))
                        })
                        .collect()
                })
                .unwrap_or_default();
            for (sym, reason) in gone {
                if !self.delisted_done.insert(sym) {
                    continue;
                }
                let Some(pos) = self.positions.get(&sym).copied() else { continue };
                let haircut = if self.cfg.delist_at_last_price {
                    0.0
                } else {
                    self.cfg.delisting_haircut(&reason).clamp(0.0, 1.0)
                };
                if haircut == 0.0 && !self.cfg.delist_at_last_price && matches!(reason.as_str(), "bankruptcy" | "regulatory") {
                    self.zero_haircut_involuntary += 1;
                }
                let last = k.price_at(sym, t).unwrap_or(0.0);
                let price = last * (1.0 - haircut);
                let qty = -pos;
                let m = k.multiplier(sym);
                let commission = if self.cfg.delist_at_last_price { 0.0 } else { self.commission(k, sym, qty, price) };
                let proceeds = -qty * price * m - commission;
                self.cash += proceeds;
                self.positions.remove(&sym);
                result.costs.commissions += commission;
                result.costs.turnover += qty.abs() * price * m;
                k.stores[fill_rel].insert(t, vec![Value::Equity(sym), Value::Time(t), Value::Num(qty), Value::Num(price)]);
                result.fills.push(FillRecord {
                    t,
                    equity: sym,
                    quantity: qty,
                    price,
                    at_last_price: true,
                    commission,
                    fee: 0.0,
                    slippage: 0.0,
                    impact: 0.0,
                    participation: 0.0,
                    partial: false,
                    forced: true,
                });
                result.actions.push(ActionRecord {
                    t,
                    equity: sym,
                    action: Action::Delisting { reason, haircut },
                    cash: proceeds,
                });
                book_changed = true;
            }
        }
        book_changed
    }
}

impl Executor for SimExecutor {
    fn open_bar(&mut self, k: &mut Kernel, t: i64, result: &mut RunResult) -> Result<(), RunError> {
        let fill_rel = k.rel_id("fill");
        if self.bars.is_empty() {
            // Cash at the first bar, so that cash-aware rules can fire from the start.
            let cash_rel = k.rel_id("cash");
            k.stores[cash_rel].insert(t, vec![Value::Time(t), Value::Num(self.cash)]);
        }
        self.bars.push(t);
        result.bars.push(t);
        if self.actions(k, t, result) {
            self.rewrite_book(k, t);
        }
        // Mark to market before the bar's decisions.
        let mut equity = self.cash;
        let mut gross = 0.0;
        let mut net = 0.0;
        for (&sym, &q) in &self.positions {
            if let Some(p) = k.price_at(sym, t) {
                let m = k.multiplier(sym);
                equity += q * p * m;
                gross += q.abs() * p * m;
                net += q * p * m;
            }
        }
        // Maintenance: positive equity below the margin of gross exposure is
        // a margin call (a non-positive equity is ruin, judged when an order
        // comes to be filled).
        if gross > 0.0 && equity > 0.0 && equity < self.cfg.maintenance_margin * gross - 1e-9 * gross {
            let message = format!(
                "margin call: equity {:.2} is below {}% of gross exposure {:.2} at {}",
                equity,
                self.cfg.maintenance_margin * 100.0,
                gross,
                time::format_timestamp(t)
            );
            match self.cfg.on_margin_call {
                OnMarginCall::Halt => {
                    return Err(RunError::Risk {
                        t,
                        rule: "(executor)".into(),
                        decision: "(mark to market)".into(),
                        message,
                    });
                }
                OnMarginCall::Allow => self.margin_calls += 1,
                OnMarginCall::Liquidate => {
                    self.margin_calls += 1;
                    // Sell the fraction f of every position with
                    // equity >= maintenance * (1 - f) * gross.
                    let f = 1.0 - equity / (self.cfg.maintenance_margin * gross);
                    let syms: Vec<Sym> = self.positions.keys().copied().collect();
                    for sym in syms {
                        let pos = self.positions[&sym];
                        let Some(p) = k.price_at(sym, t) else { continue };
                        let mut qty = -(pos * f);
                        if self.cfg.lot == Lot::Whole {
                            qty = if qty < 0.0 { qty.floor() } else { qty.ceil() };
                        }
                        if qty == 0.0 || qty.abs() > pos.abs() {
                            qty = -pos;
                        }
                        let m = k.multiplier(sym);
                        let commission = self.commission(k, sym, qty, p);
                        let fee = if qty < 0.0 { qty.abs() * p * m * self.cfg.fee_bps_on_sells / 10_000.0 } else { 0.0 };
                        self.cash -= qty * p * m + commission + fee;
                        equity -= commission + fee;
                        gross -= qty.abs() * p * m;
                        net -= -qty * p * m;
                        let new_pos = pos + qty;
                        if new_pos.abs() < 1e-9 {
                            self.positions.remove(&sym);
                        } else {
                            self.positions.insert(sym, new_pos);
                        }
                        result.costs.commissions += commission;
                        result.costs.fees += fee;
                        result.costs.turnover += qty.abs() * p * m;
                        k.stores[fill_rel].insert(t, vec![Value::Equity(sym), Value::Time(t), Value::Num(qty), Value::Num(p)]);
                        result.fills.push(FillRecord {
                            t,
                            equity: sym,
                            quantity: qty,
                            price: p,
                            at_last_price: false,
                            commission,
                            fee,
                            slippage: 0.0,
                            impact: 0.0,
                            participation: k.bar_volume(sym, t).filter(|v| *v > 0.0).map(|v| qty.abs() / v).unwrap_or(0.0),
                            partial: false,
                            forced: true,
                        });
                    }
                    self.rewrite_book(k, t);
                }
            }
        }
        result.exposure.push(ExposureRecord {
            t,
            cash: self.cash,
            gross,
            net,
            equity,
            leverage: if equity > 0.0 { gross / equity } else { 0.0 },
        });
        result.equity_curve.push((t, equity));
        // `nav(T, N)`: the book's value at T's mark, before T's decisions
        // (data-bundle doc, section 7: what the metrics library reads).
        let nav_rel = k.rel_id("nav");
        k.stores[nav_rel].insert(t, vec![Value::Time(t), Value::Num(equity)]);
        Ok(())
    }

    fn on_decisions(&mut self, _k: &mut Kernel, t: i64, by_equity: &BTreeMap<Sym, Vec<(Decision, usize)>>, _result: &mut RunResult) -> Result<(), RunError> {
        self.pending = by_equity.clone();
        self.pending_t = Some(t);
        Ok(())
    }

    fn fill(&mut self, k: &mut Kernel, tn: i64, result: &mut RunResult) -> Result<(), RunError> {
        let Some(t) = self.pending_t.take() else { return Ok(()) };
        let by_equity = std::mem::take(&mut self.pending);
        let position = k.rel_id("position");
        let cash_rel = k.rel_id("cash");
        let fill_rel = k.rel_id("fill");
        // The fill bar in the executor's own list of bars (what ADV and
        // realized volatility look back over).
        let mut bars = self.bars.clone();
        if bars.last() != Some(&tn) {
            bars.push(tn);
        }
        let end = bars.len() - 1;
        // Funding over the calendar time to the execution bar: interest on
        // cash or on the debit, borrow fee and rebate on short notional.
        let dt = (tn - t) as f64 / (365.0 * 86_400.0);
        if self.cash > 0.0 {
            let i = self.cash * self.cfg.cash_rate * dt;
            self.cash += i;
            result.funding.cash_interest += i;
        } else if self.cash < 0.0 {
            let i = -self.cash * self.cfg.margin_rate * dt;
            self.cash -= i;
            result.funding.margin_interest += i;
        }
        // A future is sold, not borrowed: no borrow fee, no locate.
        let shorts: Vec<(Sym, f64)> = self.positions.iter().filter(|(s, q)| **q < 0.0 && !k.is_future(**s)).map(|(s, q)| (*s, *q)).collect();
        for (sym, q) in shorts {
            self.shorts_held = true;
            let Some(p) = k.price_at(sym, tn) else { continue };
            let notional = q.abs() * p * k.multiplier(sym);
            let bucket = self.cfg.borrow_bucket(k.adv(sym, &bars, end));
            let fee = notional * bucket.fee_bps / 10_000.0 * dt;
            let rebate = notional * self.cfg.short_rebate * dt;
            self.cash -= fee;
            self.cash += rebate;
            result.funding.borrow_fees += fee;
            result.funding.short_rebate += rebate;
        }
        // Mark the book at the execution bar.
        let mut equity_next = self.cash;
        for (&sym, &q) in &self.positions {
            if let Some(p) = k.price_at(sym, tn) {
                equity_next += q * p * k.multiplier(sym);
            }
        }
        // Orders that reduce a position fund the ones that open or add, so
        // within a bar they fill first (symbol order within each group). A
        // decision on an instrument supersedes its open target; the other
        // open targets are re-issued (flagged, so they are not counted as
        // new requests).
        for sym in by_equity.keys() {
            self.open_targets.remove(sym);
            self.working.remove(sym);
        }
        let mut pending: Vec<Pending> = Vec::new();
        for (d, r) in by_equity.values().flatten() {
            if d.order.is_market() {
                pending.push((d.clone(), *r, false, None));
            } else {
                self.working.insert(
                    d.equity,
                    Working {
                        d: d.clone(),
                        rule: *r,
                        t,
                        first_day: None,
                        bars: 0,
                        session_last: None,
                    },
                );
            }
        }
        pending.extend(self.open_targets.values().map(|(d, r)| (d.clone(), *r, true, None)));
        let syms: Vec<Sym> = self.working.keys().copied().collect();
        for sym in syms {
            let Some(mut w) = self.working.remove(&sym) else { continue };
            match self.trigger(k, &mut w, tn) {
                Trigger::Fill(p, at) => pending.push((w.d.clone(), w.rule, false, Some((p, at)))),
                Trigger::Wait => {
                    self.working.insert(sym, w);
                }
                Trigger::Expire(why) => result.dropped.push((w.t, w.d.clone(), why)),
            }
        }
        let mut marks: HashMap<Sym, f64> = HashMap::new();
        for (d, _, _, _) in &pending {
            if let Some(p) = k.price_at(d.equity, tn) {
                // The value of one unit held: price times the multiplier.
                marks.insert(d.equity, p * k.multiplier(d.equity));
            }
        }
        // What a weight is a fraction of: the book's equity, or the fixed
        // capital when profits are not reinvested.
        let base = if self.cfg.compounding { equity_next.max(0.0) } else { self.cfg.initial_cash };
        {
            let positions = &self.positions;
            let reducing = |d: &Decision| -> bool {
                let pos = positions.get(&d.equity).copied().unwrap_or(0.0);
                match d.ctor {
                    Ctor::Sell | Ctor::Cover => pos != 0.0,
                    Ctor::Buy | Ctor::Short => false,
                    Ctor::TargetQuantity => pos != 0.0 && d.amount.abs() < pos.abs() && d.amount * pos >= 0.0,
                    Ctor::TargetWeight => pos != 0.0 && (d.amount == 0.0 || d.amount * pos < 0.0 || d.amount.abs() * base < pos.abs() * marks.get(&d.equity).copied().unwrap_or(0.0)),
                }
            };
            pending.sort_by_key(|(d, _, _, _)| !reducing(d));
        }
        // Ruin: a book without positive equity cannot size or fund an order.
        if !pending.is_empty() && equity_next <= 0.0 && self.cfg.on_ruin == OnRuin::Halt {
            let (d, rule, _, _) = &pending[0];
            return Err(RunError::Risk {
                t,
                rule: k.prog.rule_label(*rule),
                decision: result.describe_decision(d),
                message: format!("ruin: equity at {} is {:.2}, not positive, with orders pending", time::format_timestamp(tn), equity_next),
            });
        }
        let sizing_equity = base;
        // A bar's transaction costs (commission, fee, slippage, impact) are
        // never leverage: a fully invested book stays fully invested after
        // paying them, carrying a debit of at most the bar's costs, which the
        // next sizing sees (and margin interest prices).
        let mut bar_costs = 0.0;
        // Intraday positions opened at this step: flat again at their session's close.
        let mut day_exits: Vec<(Sym, i64, usize)> = Vec::new();
        for (d, rule, reissued, away) in &pending {
            let sym = d.equity;
            let is_target = matches!(d.ctor, Ctor::TargetWeight | Ctor::TargetQuantity);
            let name = k.symbols.name(sym).to_string();
            let pos = self.positions.get(&sym).copied().unwrap_or(0.0);
            // The bar the order executes at, and its base price: this bar's
            // close, or what a working order triggered at.
            let at = away.map(|(_, a)| a).unwrap_or(tn);
            let bar_price = match away {
                Some((p, _)) => Some(*p),
                None => k.bar_price(sym, tn),
            };
            // Slippage against the order: the fixed part plus a multiple of
            // the instrument's realized volatility at the fill bar.
            let slip = self.cfg.slippage_bps / 10_000.0 + self.cfg.slippage_vol_mult * k.realized_vol(sym, &bars, end).unwrap_or(0.0);
            let slipped = |p: f64, buying: bool| if buying { p * (1.0 + slip) } else { p * (1.0 - slip) };
            // Lot rounding applies to what the decision names: a delta order's
            // quantity, or a target's quantity (so a kept name never ends a
            // fraction of a share over its target).
            let lot = self.cfg.lot;
            let round = move |x: f64| -> f64 {
                match lot {
                    Lot::Whole => x.trunc(),
                    Lot::Fractional => x,
                }
            };
            let m = k.multiplier(sym);
            let mut qty = match (d.ctor, bar_price) {
                (Ctor::Buy | Ctor::Cover, _) => round(d.amount),
                (Ctor::Sell | Ctor::Short, _) => -round(d.amount),
                (Ctor::TargetQuantity, _) => round(d.amount) - pos,
                // A long that is bought is sized at the price it will fill at,
                // so the cash it spends is the weight of equity; any other
                // target (a reduction, a short) is sized at the bar price,
                // which is what the position is marked at.
                (Ctor::TargetWeight, Some(p)) => {
                    let target = round(d.amount * sizing_equity / (p * m));
                    let target = if target > pos && target > 0.0 {
                        round(d.amount * sizing_equity / (slipped(p, true) * m))
                    } else {
                        target
                    };
                    target - pos
                }
                // Without a price a weight cannot be sized, except the flat target.
                (Ctor::TargetWeight, None) if d.amount == 0.0 => -pos,
                (Ctor::TargetWeight, None) => {
                    self.open_targets.remove(&sym);
                    result.dropped.push((t, d.clone(), format!("no price for {} at {}", name, time::format_timestamp(tn))));
                    continue;
                }
            };
            if qty == 0.0 {
                // A target that is held: nothing to do, and an open one is reached.
                self.open_targets.remove(&sym);
                continue;
            }
            // Oversize: a delta order that would carry the position across zero.
            let crosses = matches!(d.ctor, Ctor::Sell | Ctor::Cover) && pos != 0.0 && (pos + qty) * pos < 0.0;
            if crosses {
                let message = format!(
                    "{} {} of {} shares {}: the order would cross zero",
                    d.ctor.name(),
                    qty.abs(),
                    pos.abs(),
                    if pos > 0.0 { "held" } else { "short" }
                );
                match self.cfg.on_oversize {
                    OnOversize::Halt => {
                        return Err(RunError::Risk {
                            t,
                            rule: k.prog.rule_label(*rule),
                            decision: result.describe_decision(d),
                            message,
                        });
                    }
                    OnOversize::Clamp => {
                        let excess = qty.abs() - pos.abs();
                        result
                            .dropped
                            .push((t, d.clone(), format!("clamped at position: {} of {} shares filled, {} dropped", pos.abs(), qty.abs(), excess)));
                        qty = -pos;
                    }
                    OnOversize::Allow => {}
                }
            }
            // Participation: a fill is at most the cap times the bar's volume.
            let requested = qty.abs();
            if !*reissued {
                self.requested_total += requested;
            }
            let bar_volume = k.bar_volume(sym, at);
            let mut partial = false;
            if let (true, Some(v)) = (self.cfg.participation_cap > 0.0, bar_volume) {
                let cap = round(self.cfg.participation_cap * v);
                if requested > cap {
                    partial = true;
                    qty = qty.signum() * cap;
                }
            }
            let cap_pct = self.cfg.participation_cap * 100.0;
            let partial_note = move |filled: f64| {
                format!(
                    "partial fill: {} of {} shares (participation cap {}% of volume {})",
                    filled,
                    requested,
                    cap_pct,
                    bar_volume.unwrap_or(0.0)
                )
            };
            if qty == 0.0 {
                if is_target && d.order.is_market() {
                    self.open_targets.insert(sym, (d.clone(), *rule));
                } else {
                    result.dropped.push((t, d.clone(), format!("{}; remainder expired", partial_note(0.0))));
                }
                continue;
            }
            // The order shrinks the position without crossing zero: it is a
            // liquidation (fillable at the last price) and never leverage.
            let reduces = pos != 0.0 && (pos + qty) * pos >= 0.0 && (pos + qty).abs() < pos.abs();
            // Price: the bar's, or the last known one for a liquidation.
            let (p, at_last_price) = match bar_price {
                Some(p) => (p, false),
                None => match k.last_price.get(&sym).copied() {
                    Some(p) if reduces => (p, true),
                    _ => {
                        self.open_targets.remove(&sym);
                        result.dropped.push((t, d.clone(), format!("no price for {} at {}", name, time::format_timestamp(tn))));
                        continue;
                    }
                },
            };
            // Impact: square root in participation of average daily volume.
            let imp = match (self.cfg.impact_coef > 0.0, k.adv(sym, &bars, end)) {
                (true, Some(adv)) if adv > 0.0 => self.cfg.impact_coef * (qty.abs() / adv).sqrt(),
                _ => 0.0,
            };
            let fill_price = if qty > 0.0 { p * (1.0 + slip + imp) } else { p * (1.0 - slip - imp) };
            let commission = self.commission(k, sym, qty, fill_price);
            let fee = if qty < 0.0 { qty.abs() * fill_price * m * self.cfg.fee_bps_on_sells / 10_000.0 } else { 0.0 };
            let cost = qty * fill_price * m + commission + fee;
            let slippage = qty.abs() * p * m * slip;
            let impact = qty.abs() * p * m * imp;
            let allowance = bar_costs + commission + fee + slippage + impact;
            // Borrow availability: an order that opens or adds to a short
            // needs its instrument's ADV bucket to be shortable.
            if pos + qty < 0.0 && pos + qty < pos && !k.is_future(sym) {
                let adv = k.adv(sym, &bars, end);
                let bucket = self.cfg.borrow_bucket(adv);
                if !bucket.shortable {
                    self.not_shortable += 1;
                    self.open_targets.remove(&sym);
                    result.dropped.push((
                        t,
                        d.clone(),
                        format!(
                            "not shortable: ADV {} of {} is below {} (the smallest borrow bucket)",
                            adv.map(|a| format!("{:.0}", a)).unwrap_or_else(|| "unknown".into()),
                            name,
                            bucket.adv_below
                        ),
                    ));
                    continue;
                }
            }
            // Leverage: only an order that adds exposure can borrow, and only
            // up to the configured gross multiple of equity.
            if !reduces {
                let new_cash = self.cash - cost;
                let mut gross = 0.0;
                let mut net = new_cash;
                for (&s2, &q2) in &self.positions {
                    let q2 = if s2 == sym { q2 + qty } else { q2 };
                    let p2 = if s2 == sym { p } else { k.price_at(s2, tn).unwrap_or(0.0) };
                    let m2 = k.multiplier(s2);
                    gross += q2.abs() * p2 * m2;
                    net += q2 * p2 * m2;
                }
                if !self.positions.contains_key(&sym) {
                    gross += qty.abs() * p * m;
                    net += qty * p * m;
                }
                let tol = 1e-9 * (1.0 + net.abs()) + allowance;
                let max_gross = self.cfg.max_gross.max(1.0);
                // Leverage is judged against equity; when profits are not
                // reinvested, against the fixed capital, and cash may go
                // negative (a loss is carried, not refunded by selling).
                let breach = if self.cfg.compounding {
                    new_cash < -(max_gross - 1.0) * net.max(0.0) - tol || gross > max_gross * net + tol
                } else {
                    gross > max_gross * self.cfg.initial_cash + tol
                };
                if breach {
                    let message = format!(
                        "leverage: after the fill cash would be {:.2} and gross exposure {:.2} against equity {:.2} (limit {}x gross)",
                        new_cash, gross, net, max_gross
                    );
                    match self.cfg.on_leverage {
                        OnLeverage::Halt => {
                            return Err(RunError::Risk {
                                t,
                                rule: k.prog.rule_label(*rule),
                                decision: result.describe_decision(d),
                                message,
                            });
                        }
                        OnLeverage::Reject => {
                            result.dropped.push((
                                t,
                                d.clone(),
                                format!("rejected, would exceed {}x equity: cash {:.2}, gross {:.2} against equity {:.2}", max_gross, new_cash, gross, net),
                            ));
                            continue;
                        }
                        OnLeverage::Allow => {}
                    }
                }
            }
            self.cash -= cost;
            let new_pos = pos + qty;
            if new_pos.abs() < 1e-9 {
                self.positions.remove(&sym);
            } else {
                self.positions.insert(sym, new_pos);
            }
            k.last_price.insert(sym, fill_price);
            k.stores[fill_rel].insert(tn, vec![Value::Equity(sym), Value::Time(tn), Value::Num(qty), Value::Num(fill_price)]);
            bar_costs += commission + fee + slippage + impact;
            result.costs.commissions += commission;
            result.costs.fees += fee;
            result.costs.slippage += slippage;
            result.costs.impact += impact;
            result.costs.turnover += qty.abs() * fill_price * m;
            self.filled_total += qty.abs();
            let participation = bar_volume.filter(|v| *v > 0.0).map(|v| qty.abs() / v).unwrap_or(0.0);
            if bar_volume.is_some() {
                self.participations.push(participation);
            }
            result.fills.push(FillRecord {
                t: at,
                equity: sym,
                quantity: qty,
                price: fill_price,
                at_last_price,
                commission,
                fee,
                slippage,
                impact,
                participation,
                partial,
                forced: false,
            });
            if d.order.kind == OrderKind::MooMoc {
                day_exits.push((sym, at, *rule));
            }
            // The remainder of a capped order: a delta order expires, a target
            // re-issues itself at the next bar.
            if partial {
                if is_target && d.order.is_market() {
                    self.open_targets.insert(sym, (d.clone(), *rule));
                } else {
                    result.dropped.push((t, d.clone(), format!("{}; remainder expired", partial_note(qty.abs()))));
                }
            } else if is_target {
                self.open_targets.remove(&sym);
            }
        }
        for (sym, at, rule) in day_exits {
            // Entered at the session's last print: flat again at its close, now.
            if k.session_last_bar(sym, at) == Some(tn) {
                if let (Some(&pos), Some(p)) = (self.positions.get(&sym), k.bar_price(sym, tn)) {
                    let qty = -pos;
                    let m = k.multiplier(sym);
                    let commission = self.commission(k, sym, qty, p);
                    self.cash -= qty * p * m + commission;
                    self.positions.remove(&sym);
                    result.costs.commissions += commission;
                    result.costs.turnover += qty.abs() * p * m;
                    k.last_price.insert(sym, p);
                    k.stores[fill_rel].insert(tn, vec![Value::Equity(sym), Value::Time(tn), Value::Num(qty), Value::Num(p)]);
                    result.fills.push(FillRecord {
                        t: tn,
                        equity: sym,
                        quantity: qty,
                        price: p,
                        at_last_price: false,
                        commission,
                        fee: 0.0,
                        slippage: 0.0,
                        impact: 0.0,
                        participation: 0.0,
                        partial: false,
                        forced: false,
                    });
                    continue;
                }
            }
            let exit = Decision {
                ctor: Ctor::TargetQuantity,
                equity: sym,
                amount: 0.0,
                order: super::value::Order { kind: OrderKind::Moc, tif: Tif::Day },
            };
            self.working.insert(
                sym,
                Working {
                    d: exit,
                    rule,
                    t: at,
                    first_day: None,
                    bars: 0,
                    session_last: None,
                },
            );
        }
        for (&sym, &q) in &self.positions {
            k.stores[position].insert(tn, vec![Value::Equity(sym), Value::Time(tn), Value::Num(q)]);
        }
        k.stores[cash_rel].insert(tn, vec![Value::Time(tn), Value::Num(self.cash)]);
        Ok(())
    }

    fn checkpoint(&self) -> Result<serde_json::Value, String> {
        serde_json::to_value(self).map_err(|e| e.to_string())
    }

    fn restore(&mut self, state: serde_json::Value) -> Result<(), String> {
        *self = serde_json::from_value(state).map_err(|e| format!("executor state: {}", e))?;
        Ok(())
    }

    fn finish(&mut self, k: &mut Kernel, result: &mut RunResult) {
        // A decision on the last bar has no bar to fill at: it is recorded in
        // `decided` like any other, and dropped here so that decisions =
        // fills + dropped.
        if let Some(t) = self.pending_t.take() {
            for ds in std::mem::take(&mut self.pending).values() {
                for (d, _) in ds {
                    result.dropped.push((t, d.clone(), "no next bar".to_string()));
                }
            }
        }
        // A market-on-close order whose session closed within the data is
        // settled at that close in one last step (nothing else fills there).
        let last = self.bars.last().copied();
        let settle = last.is_some_and(|l| {
            self.working
                .values()
                .any(|w| w.d.order.kind == OrderKind::Moc && k.session_last_bar(w.d.equity, w.t).is_some_and(|c| c <= l))
        });
        if let (true, Some(l)) = (settle, last) {
            self.pending_t = Some(l);
            if let Err(e) = self.fill(k, l + 1, result) {
                result.warnings.push(RunWarning {
                    bias: "settlement".into(),
                    message: format!("the closing settlement of the last session failed: {}", e),
                });
            }
        }
        for w in std::mem::take(&mut self.working).into_values() {
            result.dropped.push((w.t, w.d.clone(), format!("{}: the data ended before it executed", w.d.order)));
        }
        result.liquidity = LiquiditySummary {
            fill_ratio: if self.requested_total > 0.0 { self.filled_total / self.requested_total } else { 1.0 },
            avg_participation: if self.participations.is_empty() {
                0.0
            } else {
                self.participations.iter().sum::<f64>() / self.participations.len() as f64
            },
            max_participation: self.participations.iter().cloned().fold(0.0, f64::max),
        };
        let above = self.participations.iter().filter(|p| **p > self.cfg.participation_warn).count();
        if above > 0 {
            result.warnings.push(RunWarning {
                bias: "market-impact".into(),
                message: format!(
                    "{} fills above {}% of bar volume (max {:.1}%); impact is modeled, capacity is limited",
                    above,
                    self.cfg.participation_warn * 100.0,
                    result.liquidity.max_participation * 100.0
                ),
            });
        }
        if self.zero_haircut_involuntary > 0 {
            result.warnings.push(RunWarning {
                bias: "delisting".into(),
                message: format!(
                    "{} involuntary delistings closed with no haircut; the default is a total loss (data-bundle doc, section 4)",
                    self.zero_haircut_involuntary
                ),
            });
        }
        if self.margin_calls > 0 {
            result.warnings.push(RunWarning {
                bias: "leverage".into(),
                message: format!(
                    "{} margin calls (equity below {}% of gross) {}",
                    self.margin_calls,
                    self.cfg.maintenance_margin * 100.0,
                    if self.cfg.on_margin_call == OnMarginCall::Liquidate {
                        "liquidated pro rata at the bar's close"
                    } else {
                        "ignored"
                    }
                ),
            });
        }
        if result.funding.margin_interest > 0.0 {
            result.warnings.push(RunWarning {
                bias: "funding-cost neglect".into(),
                message: format!(
                    "margin interest of {:.2} charged at a constant {}% a year, not a rate series",
                    result.funding.margin_interest,
                    self.cfg.margin_rate * 100.0
                ),
            });
        }
        if self.shorts_held {
            result.warnings.push(RunWarning {
                bias: "shorting".into(),
                message: format!("borrow fees of {:.2} by ADV bucket are a proxy, modeled, not observed", result.funding.borrow_fees),
            });
        }
        if self.not_shortable > 0 {
            result.warnings.push(RunWarning {
                bias: "borrow-availability".into(),
                message: format!(
                    "{} short orders dropped: the instrument's ADV is in the smallest bucket, which is not shortable (a proxy for locate)",
                    self.not_shortable
                ),
            });
        }
        result.final_cash = self.cash;
        result.final_positions = self.positions.clone();
    }
}

impl Kernel<'_> {
    /// The strategy's decisions at `t`, validated (a delta quantity is
    /// positive, one decision per instrument), recorded in the result and in
    /// `decided`, grouped by instrument.
    pub fn decide_at(&mut self, t: i64, result: &mut RunResult) -> Result<BTreeMap<Sym, Vec<(Decision, usize)>>, RunError> {
        let decide = self.rel_id("decide");
        let decided = self.rel_id("decided");
        let delta = self.prog.mode == DecisionMode::Delta;
        let tuples = self.call(decide, &[Some(Value::Time(t)), None])?;
        let mut by_equity: BTreeMap<Sym, Vec<(Decision, usize)>> = BTreeMap::new();
        for (tu, rule) in tuples.iter() {
            let Value::Decision(d) = &tu[1] else {
                return Err(RunError::Internal("decide tuple without a decision".into()));
            };
            if delta && d.amount.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater) {
                return Err(RunError::BadDecision {
                    t,
                    rule: self.prog.rule_label(*rule),
                    decision: result.describe_decision(d),
                    message: "delta decisions require a positive quantity".into(),
                });
            }
            by_equity.entry(d.equity).or_default().push((d.clone(), *rule));
        }
        for (sym, ds) in &by_equity {
            if ds.len() > 1 {
                let (a, ra) = &ds[0];
                let (b, rb) = &ds[1];
                return Err(RunError::Conflict {
                    t,
                    equity: self.symbols.name(*sym).to_string(),
                    first: (result.describe_decision(a), self.prog.rule_label(*ra)),
                    second: (result.describe_decision(b), self.prog.rule_label(*rb)),
                });
            }
        }
        for ds in by_equity.values() {
            for (d, rule) in ds {
                result.decisions.push(DecisionRecord { t, decision: d.clone(), rule: *rule });
                self.stores[decided].insert(t, vec![Value::Time(t), Value::Decision(d.clone())]);
            }
        }
        Ok(by_equity)
    }
}
