//! The executor behind a trait (data-bundle doc, section 2: "the executor is
//! in the loop", and the only component that differs between a backtest and
//! a live run). `SimExecutor` is the simulated one of the semantic model's
//! section 6 with the realism models of the data-bundle doc's section 5; a
//! live executor would send orders from `on_decisions` and feed its fills
//! back as events.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::ir::*;
use crate::kernel::events::{Book, EventKind, Fate, Funding, OrderId, Receivable, RunEvent};
use crate::kernel::time;
use crate::kernel::value::{OrderKind, Tif};
use crate::kernel::{
    Action, Ctor, Decision, ExecConfig, ExposureRecord, FillRecord, HoldingRecord, Kernel, LiquiditySummary, Lot, OnLeverage, OnMarginCall, OnOversize, OnRuin, RunError, RunResult, RunWarning, Sym,
    Value,
};

mod equity;
mod future;
mod limits;
pub mod margin;
pub(crate) mod option;
mod requirement;

pub use limits::LimitState;

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
    /// What the invariants read (K-3.3): the book, the instruments with an
    /// order working (one entry an order), and the book the last bar's
    /// accepted orders would leave.
    fn book(&self) -> Option<&Book> {
        None
    }
    fn working(&self) -> Vec<Sym> {
        Vec::new()
    }
    fn expected(&self) -> Option<&Expected> {
        None
    }
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
    #[serde(default)]
    order_id: OrderId,
}

/// An order to fill at this step: the decision, its rule, whether it is an
/// open target re-issued, the base price and bar of an order that executes
/// away from this bar's close, its order id and its decision bar.
type Pending = (Decision, usize, bool, Option<(f64, i64)>, OrderId, i64);

/// What a working order does at a bar.
enum Trigger {
    /// Execute at this base price, at this bar.
    Fill(f64, i64),
    Wait,
    Expire(String),
}

/// The book a bar's accepted orders would leave at the bar's marks, after
/// the limits acted (what `inv.position_limits` reads, K-3.3).
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Expected {
    pub t: i64,
    pub capital: f64,
    /// Instrument, quantity, the unit's value (price times multiplier).
    pub positions: Vec<(Sym, f64, f64)>,
    /// The instruments the accepted orders grow from the book the limits
    /// sized them against (E-119): a pre-trade limit judges these alone. A
    /// fill of the bar before, at the bar's open, is no growth of the bar's.
    #[serde(default)]
    pub grown: Vec<Sym>,
}

/// The simulated executor: the book, the open targets, the working orders
/// and the counters behind the run's summaries.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct SimExecutor {
    cfg: ExecConfig,
    /// Cash, positions and receivables: changed only by the events the
    /// executor emits (`record`), so that the event log's fold is the book
    /// (K-5.2).
    book: Book,
    /// Decision bars seen so far (what ADV and realized volatility look back over).
    bars: Vec<i64>,
    /// The orders of the last bar's decisions, to be filled at the next.
    pending: BTreeMap<Sym, Vec<(Decision, usize, OrderId)>>,
    pending_t: Option<i64>,
    /// Targets a capped fill left open, re-issued at every bar until met:
    /// the decision, its rule, its order and its decision bar.
    open_targets: BTreeMap<Sym, (Decision, usize, OrderId, i64)>,
    /// Orders working beyond the bar after their decision.
    #[serde(default)]
    working: BTreeMap<Sym, Working>,
    /// Each futures root's `continuous` entry at the last barrier (E-116).
    #[serde(default)]
    roll_seen: BTreeMap<Sym, usize>,
    /// The next order's id.
    #[serde(default)]
    next_order: OrderId,
    #[serde(default)]
    limit_state: LimitState,
    #[serde(default)]
    expected: Expected,
    requested_total: f64,
    filled_total: f64,
    participations: Vec<f64>,
    margin_calls: usize,
    delisted_done: HashSet<Sym>,
    /// The bar whose corporate actions the book carries (applied once,
    /// before the bar's fills).
    #[serde(default)]
    actions_done: Option<i64>,
    zero_haircut_involuntary: usize,
    not_shortable: usize,
    shorts_held: bool,
    /// The terms of the options the book adjusted for their underlyings'
    /// corporate actions (K-5.5.5), by contract.
    #[serde(default)]
    adjusted: BTreeMap<Sym, crate::kernel::OptionTerms>,
    /// Options expired by force (a delisted underlying) and without a
    /// settlement price, for the run's warnings.
    #[serde(default)]
    forced_expiries: usize,
    #[serde(default)]
    unpriced_expiries: usize,
    /// Settlements at the close for want of a settlement price, futures
    /// contracts expired without delivery, bars `approx` margined, and the
    /// models the margin passed over: what the run's warnings report.
    #[serde(default)]
    settled_at_close: usize,
    #[serde(default)]
    physical_expiries: usize,
    #[serde(default)]
    approx_bars: usize,
    #[serde(default)]
    margin_notes: Vec<String>,
}

impl SimExecutor {
    pub fn new(cfg: ExecConfig) -> SimExecutor {
        SimExecutor {
            book: Book::new(cfg.initial_cash),
            cfg,
            bars: Vec::new(),
            pending: BTreeMap::new(),
            pending_t: None,
            open_targets: BTreeMap::new(),
            working: BTreeMap::new(),
            roll_seen: BTreeMap::new(),
            next_order: 0,
            limit_state: LimitState::default(),
            expected: Expected::default(),
            requested_total: 0.0,
            filled_total: 0.0,
            participations: Vec::new(),
            margin_calls: 0,
            delisted_done: HashSet::new(),
            actions_done: None,
            zero_haircut_involuntary: 0,
            not_shortable: 0,
            shorts_held: false,
            adjusted: BTreeMap::new(),
            forced_expiries: 0,
            unpriced_expiries: 0,
            settled_at_close: 0,
            physical_expiries: 0,
            approx_bars: 0,
            margin_notes: Vec::new(),
        }
    }

    /// Emits an event and applies it to the book: the one way the book
    /// changes.
    pub(crate) fn record(&mut self, result: &mut RunResult, e: RunEvent) {
        let seq = result.emit(e) as usize;
        self.book.apply(&result.events[seq]);
    }

    /// The configuration the executor runs with.
    pub(crate) fn cfg(&self) -> &ExecConfig {
        &self.cfg
    }

    /// The book.
    pub(crate) fn book_ref(&self) -> &Book {
        &self.book
    }

    /// A fill's notional, for the turnover limits (K-3.5).
    pub(crate) fn note_traded(&mut self, at: i64, notional: f64) {
        self.limit_state.traded.push_back((at, notional));
    }

    /// An order's size, and what of one filled with its participation, for
    /// the run's liquidity summary (a broker's orders, K-6.4).
    pub(crate) fn note_requested(&mut self, quantity: f64) {
        self.requested_total += quantity;
    }

    pub(crate) fn note_filled(&mut self, quantity: f64, participation: Option<f64>) {
        self.filled_total += quantity;
        if let Some(p) = participation {
            self.participations.push(p);
        }
    }

    /// The NAV at `t`'s mark (K-5.1): cash, the positions marked at the
    /// bar's price (or the last one), the receivables, in that order.
    pub(crate) fn nav(&self, k: &mut Kernel, t: i64) -> f64 {
        let mut nav = self.book.cash;
        for (&sym, &q) in &self.book.positions {
            if let Some(p) = k.price_at(sym, t) {
                nav += self.position_value(k, sym, q, p);
            }
        }
        for r in &self.book.receivables {
            nav += r.cash;
        }
        nav
    }

    /// The book at `t`'s marks: its NAV (cash, the positions, the
    /// receivables, in that order), its gross and its net exposure.
    fn marked(&self, k: &mut Kernel, t: i64) -> (f64, f64, f64) {
        let mut equity = self.book.cash;
        let mut gross = 0.0;
        let mut net = 0.0;
        for (&sym, &q) in &self.book.positions {
            if let Some(p) = k.price_at(sym, t) {
                let m = k.multiplier(sym);
                equity += self.position_value(k, sym, q, p);
                gross += q.abs() * p * m;
                net += q * p * m;
            }
        }
        for r in &self.book.receivables {
            equity += r.cash;
        }
        (equity, gross, net)
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
        // `commission per_contract P`: a contract's where the catalog states none.
        if let (Some(c), true) = (self.cfg.per_contract, k.is_contract(sym)) {
            return c * qty.abs() + bps;
        }
        if self.cfg.commission_per_share == 0.0 && self.cfg.commission_min_per_order == 0.0 {
            bps
        } else {
            (self.cfg.commission_per_share * qty.abs()).max(self.cfg.commission_min_per_order) + bps
        }
    }

    /// `Executor::fill` over the bar history `bars` (ending at the fill bar).
    fn fill_over(&mut self, k: &mut Kernel, tn: i64, result: &mut RunResult, bars: &[i64]) -> Result<(), RunError> {
        let Some(t) = self.pending_t.take() else { return Ok(()) };
        self.sync_terms(k);
        let by_equity = std::mem::take(&mut self.pending);
        let position = k.rel_id("position");
        let cash_rel = k.rel_id("cash");
        let fill_rel = k.rel_id("fill");
        let end = bars.len() - 1;

        // Funding over the calendar time to the execution bar: interest on
        // cash or on the debit, borrow fee and rebate on short notional.
        let dt = time::secs_f64(tn - t) / (365.0 * 86_400.0);
        if self.book.cash > 0.0 {
            let i = self.book.cash * self.cfg.cash_rate * dt;
            if i != 0.0 {
                self.record(
                    result,
                    RunEvent::new(
                        tn,
                        None,
                        EventKind::Funding {
                            flow: Funding::CashInterest,
                            amount: i,
                        },
                    )
                    .cash(i),
                );
            }
            result.funding.cash_interest += i;
        } else if self.book.cash < 0.0 {
            let i = -self.book.cash * self.cfg.margin_rate * dt;
            if i != 0.0 {
                self.record(
                    result,
                    RunEvent::new(
                        tn,
                        None,
                        EventKind::Funding {
                            flow: Funding::DebitInterest,
                            amount: i,
                        },
                    )
                    .cash(-i),
                );
            }
            result.funding.margin_interest += i;
        }
        // Then the execution bar's corporate actions: a split multiplies the
        // quantity held, so the borrow fee and the orders are sized against
        // the post-split book at the bar's (post-split) prices; a dividend
        // going ex accrues to the shares held through the bar. Interest was
        // earned on the cash held over the interval, before a payment at
        // the bar.
        self.corporate_actions(k, tn, result);
        // A contract (a future, an option) is sold, not borrowed: no borrow
        // fee, no locate.
        let shorts: Vec<(Sym, f64)> = self.book.positions.iter().filter(|(s, q)| **q < 0.0 && !k.is_contract(**s)).map(|(s, q)| (*s, *q)).collect();
        for (sym, q) in shorts {
            self.shorts_held = true;
            let Some(p) = k.price_at(sym, tn) else { continue };
            let notional = q.abs() * p * k.multiplier(sym);
            let bucket = self.cfg.borrow_bucket(k.adv(sym, bars, end));
            let fee = notional * bucket.fee_bps / 10_000.0 * dt;
            let rebate = notional * self.cfg.short_rebate * dt;
            if fee != 0.0 {
                self.record(
                    result,
                    RunEvent::new(
                        tn,
                        Some(sym),
                        EventKind::Funding {
                            flow: Funding::BorrowFee,
                            amount: fee,
                        },
                    )
                    .cash(-fee),
                );
            }
            if rebate != 0.0 {
                self.record(
                    result,
                    RunEvent::new(
                        tn,
                        Some(sym),
                        EventKind::Funding {
                            flow: Funding::ShortRebate,
                            amount: rebate,
                        },
                    )
                    .cash(rebate),
                );
            }
            result.funding.borrow_fees += fee;
            result.funding.short_rebate += rebate;
        }
        // Mark the book at the execution bar: its NAV.
        let equity_next = self.nav(k, tn);
        // Orders that reduce a position fund the ones that open or add, so
        // within a bar they fill first (symbol order within each group). A
        // decision on an instrument supersedes its open target; the other
        // open targets are re-issued (flagged, so they are not counted as
        // new requests).
        // A combo's decision supersedes what works on any of its legs (K-5.8).
        for ds in by_equity.values() {
            let Some((d, _, by)) = ds.first() else { continue };
            for sym in d.instruments() {
                if let Some((_, rule, id, decided)) = self.open_targets.remove(&sym) {
                    self.record(result, RunEvent::new(t, Some(sym), EventKind::Replaced { decided, by: *by }).rule(rule).order(id));
                }
                if let Some(key) = self.working_on(sym) {
                    if let Some(w) = self.working.remove(&key) {
                        self.record(result, RunEvent::new(t, Some(key), EventKind::Replaced { decided: w.t, by: *by }).rule(w.rule).order(w.order_id));
                    }
                }
            }
        }
        let mut pending: Vec<Pending> = Vec::new();
        // Combos fill all or none, after the bar's single orders (K-5.5.3).
        let mut combos: Vec<(Decision, usize, OrderId, i64)> = Vec::new();
        let mut combos_working: Vec<Working> = Vec::new();
        for (d, r, id) in by_equity.values().flatten() {
            if d.ctor == Ctor::Combo && d.order.is_market() {
                combos.push((d.clone(), *r, *id, t));
            } else if d.order.is_market() {
                pending.push((d.clone(), *r, false, None, *id, t));
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
                        order_id: *id,
                    },
                );
            }
        }
        pending.extend(self.open_targets.values().map(|(d, r, id, decided)| (d.clone(), *r, true, None, *id, *decided)));
        let syms: Vec<Sym> = self.working.keys().copied().collect();
        for sym in syms {
            let Some(mut w) = self.working.remove(&sym) else { continue };
            if w.d.ctor == Ctor::Combo {
                // A working combo is a net limit under its time in force.
                let day = time::day_key(tn);
                let first = *w.first_day.get_or_insert(day);
                let expired = match w.d.order.tif {
                    Tif::Day => day != first,
                    Tif::Gtc => false,
                    Tif::Bars(n) => w.bars >= n,
                };
                if expired {
                    let reason = format!("{} expired unfilled (the combo on {})", w.d.order, k.symbols.name(sym));
                    self.record(
                        result,
                        RunEvent::new(
                            tn,
                            Some(sym),
                            EventKind::ExpiredOrder {
                                decided: w.t,
                                decision: w.d.clone(),
                                reason,
                            },
                        )
                        .rule(w.rule)
                        .order(w.order_id),
                    );
                } else {
                    w.bars += 1;
                    combos_working.push(w);
                }
                continue;
            }
            match self.trigger(k, &mut w, tn) {
                Trigger::Fill(p, at) => pending.push((w.d.clone(), w.rule, false, Some((p, at)), w.order_id, w.t)),
                Trigger::Wait => {
                    self.working.insert(sym, w);
                }
                Trigger::Expire(why) => self.record(
                    result,
                    RunEvent::new(
                        tn,
                        Some(sym),
                        EventKind::ExpiredOrder {
                            decided: w.t,
                            decision: w.d.clone(),
                            reason: why,
                        },
                    )
                    .rule(w.rule)
                    .order(w.order_id),
                ),
            }
        }
        let mut marks: HashMap<Sym, f64> = HashMap::new();
        for (d, ..) in &pending {
            if let Some(p) = k.price_at(d.equity, tn) {
                // The value of one unit held: price times the multiplier.
                marks.insert(d.equity, p * k.multiplier(d.equity));
            }
        }
        // What a weight is a fraction of: the book's equity, or the fixed
        // capital when profits are not reinvested.
        let base = if self.cfg.compounding { equity_next.max(0.0) } else { self.cfg.initial_cash };
        {
            let positions = &self.book.positions;
            let reducing = |d: &Decision| -> bool {
                let pos = positions.get(&d.equity).copied().unwrap_or(0.0);
                match d.ctor {
                    Ctor::Trade => pos != 0.0 && d.amount * pos < 0.0,
                    Ctor::HoldQuantity => pos != 0.0 && d.amount.abs() < pos.abs() && d.amount * pos >= 0.0,
                    Ctor::HoldWeight => pos != 0.0 && (d.amount == 0.0 || d.amount * pos < 0.0 || d.amount.abs() * base < pos.abs() * marks.get(&d.equity).copied().unwrap_or(0.0)),
                    Ctor::Combo | Ctor::Exercise => false,
                }
            };
            pending.sort_by_key(|(d, ..)| !reducing(d));
        }
        // Ruin: a book without positive equity cannot size or fund an order.
        let first_order = pending
            .first()
            .map(|(d, rule, ..)| (d, rule))
            .or_else(|| combos.first().map(|(d, rule, ..)| (d, rule)))
            .or_else(|| combos_working.first().map(|w| (&w.d, &w.rule)));
        if let (Some((d, rule)), true) = (first_order, equity_next <= 0.0 && self.cfg.on_ruin == OnRuin::Halt) {
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
        let mut day_exits: Vec<(Sym, i64, usize, OrderId)> = Vec::new();
        for (d, rule, reissued, away, id, decided) in &pending {
            let sym = d.equity;
            let is_target = d.ctor.is_target();
            let name = k.symbols.name(sym).to_string();
            // An option fills against its quote, with no slippage or impact (K-5.7).
            let option = k.has_options && k.is_option(sym);
            let pos = self.book.position(sym);
            let dropped = |reason: String| {
                RunEvent::new(
                    tn,
                    Some(sym),
                    EventKind::Dropped {
                        decided: t,
                        decision: d.clone(),
                        reason,
                    },
                )
                .rule(*rule)
                .order(*id)
            };
            // The bar the order executes at, and its base price: this bar's
            // close, or what a working order triggered at.
            let at = away.map(|(_, a)| a).unwrap_or(tn);
            let bar_price = match away {
                Some((p, _)) => Some(*p),
                None => k.bar_price(sym, tn),
            };
            // Slippage against the order: the fixed part plus a multiple of
            // the instrument's realized volatility at the fill bar.
            let slip = if option {
                0.0
            } else {
                self.cfg.slippage_bps / 10_000.0 + self.cfg.slippage_vol_mult * k.realized_vol(sym, bars, end).unwrap_or(0.0)
            };
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
                // A signed delta: its sign is the side.
                (Ctor::Trade, _) => round(d.amount),
                (Ctor::HoldQuantity, _) => round(d.amount) - pos,
                // A long that is bought is sized at the price it will fill at,
                // so the cash it spends is the weight of equity; any other
                // target (a reduction, a short) is sized at the bar price,
                // which is what the position is marked at.
                (Ctor::HoldWeight, Some(p)) => {
                    let target = round(d.amount * sizing_equity / (p * m));
                    let target = if target > pos && target > 0.0 {
                        round(d.amount * sizing_equity / (slipped(p, true) * m))
                    } else {
                        target
                    };
                    target - pos
                }
                // Without a price a weight cannot be sized, except the flat target.
                (Ctor::HoldWeight, None) if d.amount == 0.0 => -pos,
                (Ctor::HoldWeight, None) => {
                    self.open_targets.remove(&sym);
                    self.record(result, dropped(format!("no price for {} at {}", name, time::format_timestamp(tn))));
                    continue;
                }
                (Ctor::Combo | Ctor::Exercise, _) => continue,
            };
            if qty == 0.0 {
                // A target that is held: nothing to do, and an open one is reached.
                self.open_targets.remove(&sym);
                self.record(result, RunEvent::new(tn, Some(sym), EventKind::Satisfied { decided: *decided }).rule(*rule).order(*id));
                continue;
            }
            // Oversize: a delta order that would carry the position across zero.
            let crosses = d.ctor == Ctor::Trade && pos != 0.0 && (pos + qty) * pos < 0.0;
            if crosses {
                let message = format!("trade of {} against {} {}: the order would cross zero", qty, pos.abs(), if pos > 0.0 { "held" } else { "short" });
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
                        self.record(result, dropped(format!("clamped at position: {} of {} shares filled, {} dropped", pos.abs(), qty.abs(), excess)));
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
                    self.open_targets.insert(sym, (d.clone(), *rule, *id, *decided));
                } else {
                    self.record(result, dropped(format!("{}; remainder expired", partial_note(0.0))));
                }
                continue;
            }
            // The order shrinks the position without crossing zero: it is a
            // liquidation (fillable at the last price) and never leverage.
            let reduces = pos != 0.0 && (pos + qty) * pos >= 0.0 && (pos + qty).abs() < pos.abs();
            // Price: the bar's, or the last known one for a liquidation; an
            // option's against its quote, costed against its mid.
            let (p, mid, at_last_price) = if option {
                match away {
                    Some((p, _)) => (*p, k.quote(sym, at).and_then(|q| q.mid()).map(|m| m.0).unwrap_or(*p), false),
                    None => match k.option_fill(sym, tn, qty > 0.0) {
                        Ok((p, mid)) => (p, mid, false),
                        Err(why) => {
                            self.open_targets.remove(&sym);
                            self.record(result, dropped(why));
                            continue;
                        }
                    },
                }
            } else {
                match bar_price {
                    Some(p) => (p, p, false),
                    None => match k.last_price.get(&sym).copied() {
                        Some(p) if reduces => (p, p, true),
                        _ => {
                            self.open_targets.remove(&sym);
                            self.record(result, dropped(format!("no price for {} at {}", name, time::format_timestamp(tn))));
                            continue;
                        }
                    },
                }
            };
            // Impact: square root in participation of average daily volume.
            let imp = match (self.cfg.impact_coef > 0.0 && !option, k.adv(sym, bars, end)) {
                (true, Some(adv)) if adv > 0.0 => self.cfg.impact_coef * (qty.abs() / adv).sqrt(),
                _ => 0.0,
            };
            let fill_price = if qty > 0.0 { p * (1.0 + slip + imp) } else { p * (1.0 - slip - imp) };
            let commission = self.commission(k, sym, qty, fill_price);
            let fee = if qty < 0.0 { qty.abs() * fill_price * m * self.cfg.fee_bps_on_sells / 10_000.0 } else { 0.0 };
            let cost = self.traded_cash(k, sym, qty, fill_price) + commission + fee;
            let slippage = qty.abs() * p * m * slip;
            let impact = qty.abs() * p * m * imp;
            let allowance = bar_costs + commission + fee + slippage + impact;
            // Borrow availability: an order that opens or adds to a short
            // needs its instrument's ADV bucket to be shortable.
            if pos + qty < 0.0 && pos + qty < pos && !k.is_contract(sym) {
                let adv = k.adv(sym, bars, end);
                let bucket = self.cfg.borrow_bucket(adv);
                if !bucket.shortable {
                    self.not_shortable += 1;
                    self.open_targets.remove(&sym);
                    self.record(
                        result,
                        dropped(format!(
                            "not shortable: ADV {} of {} is below {} (the smallest borrow bucket)",
                            adv.map(|a| format!("{:.0}", a)).unwrap_or_else(|| "unknown".into()),
                            name,
                            bucket.adv_below
                        )),
                    );
                    continue;
                }
            }
            // Leverage: only an order that adds exposure can borrow, and only
            // up to the configured gross multiple of equity.
            if !reduces {
                let new_cash = self.book.cash - cost;
                let mut gross = 0.0;
                let mut net = new_cash;
                // A future margined daily is valued against its reference
                // (the fill's when it opens), E-10.
                let after_ref = self.fill_reference(k, sym, fill_price);
                for (&s2, &q2) in &self.book.positions {
                    let q2 = if s2 == sym { q2 + qty } else { q2 };
                    let p2 = if s2 == sym { p } else { k.price_at(s2, tn).unwrap_or(0.0) };
                    let m2 = k.multiplier(s2);
                    gross += q2.abs() * p2 * m2;
                    net += if s2 == sym { self.value_at(k, s2, q2, p2, after_ref) } else { self.position_value(k, s2, q2, p2) };
                }
                if !self.book.positions.contains_key(&sym) {
                    gross += qty.abs() * p * m;
                    net += self.value_at(k, sym, qty, p, after_ref);
                }
                // The NAV after the fill counts the receivables (K-5.1).
                for r in &self.book.receivables {
                    net += r.cash;
                }
                let tol = 1e-9 * (1.0 + net.abs()) + allowance;
                let max_gross = self.cfg.max_gross.max(1.0);
                // Leverage is judged against equity; when profits are not
                // reinvested, against the fixed capital, and cash may go
                // negative (a loss is carried, not refunded by selling). A
                // receivable is a claim to cash: the borrowing it covers
                // until it is paid is not leverage (K-5.1).
                let mut liquid = new_cash;
                for r in &self.book.receivables {
                    liquid += r.cash;
                }
                let breach = if self.cfg.compounding {
                    liquid < -(max_gross - 1.0) * net.max(0.0) - tol || gross > max_gross * net + tol
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
                            self.record(
                                result,
                                dropped(format!(
                                    "rejected, would exceed {}x equity: cash {:.2}, gross {:.2} against equity {:.2}",
                                    max_gross, new_cash, gross, net
                                )),
                            );
                            continue;
                        }
                        OnLeverage::Allow => {}
                    }
                }
            }
            let participation = bar_volume.filter(|v| *v > 0.0).map(|v| qty.abs() / v).unwrap_or(0.0);
            let reissue = is_target && d.order.is_market();
            let fill = FillRecord {
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
                spread: if option { (fill_price - mid) * qty * m } else { 0.0 },
                reference: self.fill_reference(k, sym, fill_price),
                broker: None,
            };
            let remainder = partial.then(|| (requested - qty.abs(), if reissue { Fate::Reissued } else { Fate::Expired }));
            self.record(result, RunEvent::new(at, Some(sym), EventKind::Fill { fill, remainder }).rule(*rule).order(*id).cash(-cost));
            self.limit_state.traded.push_back((at, qty.abs() * fill_price * m));
            k.last_price.insert(sym, fill_price);
            k.stores[fill_rel].insert(tn, vec![Value::Equity(sym), Value::Time(tn), Value::Num(qty), Value::Num(fill_price)]);
            bar_costs += commission + fee + slippage + impact;
            result.costs.commissions += commission;
            result.costs.fees += fee;
            result.costs.slippage += slippage;
            result.costs.impact += impact;
            result.costs.turnover += qty.abs() * fill_price * m;
            self.filled_total += qty.abs();
            if bar_volume.is_some() {
                self.participations.push(participation);
            }
            if d.order.kind == OrderKind::MooMoc {
                day_exits.push((sym, at, *rule, *id));
            }
            // The remainder of a capped order: a delta order expires, a target
            // re-issues itself at the next bar.
            if partial {
                if reissue {
                    self.open_targets.insert(sym, (d.clone(), *rule, *id, *decided));
                } else {
                    self.record(result, dropped(format!("{}; remainder expired", partial_note(qty.abs()))));
                }
            } else if is_target {
                self.open_targets.remove(&sym);
            }
        }
        // The combos, each all or none at this bar; a limit combo whose net
        // is not met works on.
        for (d, rule, id, decided) in combos {
            self.fill_combo(k, tn, &d, rule, id, decided, bars, result)?;
        }
        for w in combos_working {
            if self.fill_combo(k, tn, &w.d, w.rule, w.order_id, w.t, bars, result)? {
                self.working.insert(w.d.equity, w);
            }
        }
        for (sym, at, rule, id) in day_exits {
            // Entered at the session's last print: flat again at its close,
            // now; known once the session has closed (before it, a later
            // print may still come, and the exit waits as a working `moc`).
            if k.session_close(sym, at).is_none_or(|end| tn >= end) && k.session_last_bar(sym, at) == Some(tn) {
                if let (Some(&pos), Some(p)) = (self.book.positions.get(&sym), k.bar_price(sym, tn)) {
                    let qty = -pos;
                    let m = k.multiplier(sym);
                    let commission = self.commission(k, sym, qty, p);
                    let flow = self.traded_cash(k, sym, qty, p) + commission;
                    let reference = self.fill_reference(k, sym, p);
                    let fill = FillRecord {
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
                        spread: 0.0,
                        reference,
                        broker: None,
                    };
                    self.record(result, RunEvent::new(tn, Some(sym), EventKind::Fill { fill, remainder: None }).rule(rule).order(id).cash(-flow));
                    self.limit_state.traded.push_back((tn, qty.abs() * p * m));
                    result.costs.commissions += commission;
                    result.costs.turnover += qty.abs() * p * m;
                    k.last_price.insert(sym, p);
                    k.stores[fill_rel].insert(tn, vec![Value::Equity(sym), Value::Time(tn), Value::Num(qty), Value::Num(p)]);
                    continue;
                }
            }
            let exit = Decision::single(Ctor::HoldQuantity, sym, 0.0, crate::kernel::value::Order { kind: OrderKind::Moc, tif: Tif::Day });
            self.working.insert(
                sym,
                Working {
                    d: exit,
                    rule,
                    t: at,
                    first_day: None,
                    bars: 0,
                    session_last: None,
                    order_id: id,
                },
            );
        }
        for (&sym, &q) in &self.book.positions {
            k.stores[position].insert(tn, vec![Value::Equity(sym), Value::Time(tn), Value::Num(q)]);
        }
        k.stores[cash_rel].insert(tn, vec![Value::Time(tn), Value::Num(self.book.cash)]);
        Ok(())
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
                // The session's last print is known once the session has
                // closed: before it, the order waits (the fold has not seen
                // the rest of the session; the batch driver must not use it).
                if k.session_close(sym, w.t).is_some_and(|end| tn < end) {
                    return Trigger::Wait;
                }
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
                // An option's limit is against its quote: a buy fills when
                // its fill price is at or below the level, a sell at or above.
                if k.has_options && k.is_option(sym) {
                    let buying = match w.d.ctor {
                        Ctor::HoldQuantity => w.d.amount > self.book.position(sym),
                        _ => w.d.amount > 0.0,
                    };
                    let limit = matches!(w.d.order.kind, OrderKind::Limit(_));
                    return match k.option_fill(sym, tn, buying) {
                        Ok((p, _)) if limit && ((buying && p <= level) || (!buying && p >= level)) => Trigger::Fill(p, tn),
                        Ok((p, _)) if !limit && ((buying && p >= level) || (!buying && p <= level)) => Trigger::Fill(p, tn),
                        _ => Trigger::Wait,
                    };
                }
                let (Some(o), Some(h), Some(l)) = (k.bar_open(sym, tn), k.bar_high(sym, tn), k.bar_low(sym, tn)) else {
                    return Trigger::Wait;
                };
                // The side of the order: a delta constructor's, or a target's
                // from where the book is against it at the open.
                let pos = self.book.position(sym);
                let buying = match w.d.ctor {
                    Ctor::Trade | Ctor::Combo | Ctor::Exercise => w.d.amount > 0.0,
                    Ctor::HoldQuantity => w.d.amount > pos,
                    Ctor::HoldWeight => {
                        let base = if self.cfg.compounding { self.book.cash.max(0.0) } else { self.cfg.initial_cash };
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

    /// The working order acting on `sym`: its own, or a combo with a leg on it.
    fn working_on(&self, sym: Sym) -> Option<Sym> {
        if self.working.contains_key(&sym) {
            return Some(sym);
        }
        self.working
            .iter()
            .find(|(_, w)| w.d.ctor == Ctor::Combo && w.d.legs.iter().any(|(s, _)| *s == sym))
            .map(|(key, _)| *key)
    }

    /// Rewrite the executor relations at `t` after the book changed outside a fill.
    fn rewrite_book(&self, k: &mut Kernel, t: i64) {
        let position = k.rel_id("position");
        let cash_rel = k.rel_id("cash");
        if let Some(tus) = k.stores[position].by_time.get_mut(&t) {
            tus.clear();
        }
        for (&sym, &q) in &self.book.positions {
            k.stores[position].insert(t, vec![Value::Equity(sym), Value::Time(t), Value::Num(q)]);
        }
        if let Some(tus) = k.stores[cash_rel].by_time.get_mut(&t) {
            tus.clear();
        }
        k.stores[cash_rel].insert(t, vec![Value::Time(t), Value::Num(self.book.cash)]);
    }
}

impl SimExecutor {
    /// The first half of `open_bar` (K-6.1): the bar's own events before
    /// its marks (corporate actions, delistings, option expiries and
    /// assignments, a future's roll, expiry and settlement); whether they
    /// changed the book.
    pub(crate) fn open_actions(&mut self, k: &mut Kernel, t: i64, result: &mut RunResult) -> bool {
        if self.bars.is_empty() {
            // Cash at the first bar, so that cash-aware rules can fire from the start.
            let cash_rel = k.rel_id("cash");
            k.stores[cash_rel].insert(t, vec![Value::Time(t), Value::Num(self.book.cash)]);
        }
        self.bars.push(t);
        result.bars.push(t);
        self.sync_terms(k);
        let mut changed = self.corporate_actions(k, t, result);
        changed |= self.delistings(k, t, result);
        // Options at their expiry, then the early assignments, at the bar's
        // close and before its marks (K-5.5.4).
        if k.has_options {
            changed |= self.option_expiries(k, t, result);
            changed |= self.early_assignments(k, t, result);
        }
        // A future's roll, its contract's expiry, then the session's
        // settlement (K-5.4).
        changed |= self.rolls(k, t, result);
        changed |= self.contract_expiries(k, t, result);
        changed |= self.settlements(k, t, result);
        changed
    }

    /// The second half: the executor relations rewritten when the book
    /// changed outside a fill, then the marks, the margin and the bar's
    /// records.
    pub(crate) fn open_marks(&mut self, k: &mut Kernel, t: i64, result: &mut RunResult, changed: bool) -> Result<(), RunError> {
        let fill_rel = k.rel_id("fill");
        if changed {
            self.rewrite_book(k, t);
        }
        // Mark to market before the bar's decisions: the NAV counts the
        // receivables (K-5.1).
        let (mut equity, mut gross, mut net) = self.marked(k, t);
        // The margin models' requirement (K-5.6), when the block lists any:
        // a margin call is the equity below its maintenance.
        let mut by_models = (!self.cfg.margin_models.is_empty()).then(|| self.requirement(k, t));
        if let Some((margin, holdings)) = &by_models {
            if self.margin_call_by_models(k, t, equity, margin, holdings, result)? {
                self.rewrite_book(k, t);
                (equity, gross, net) = self.marked(k, t);
                by_models = Some(self.requirement(k, t));
            }
        }
        // Maintenance: positive equity below the margin of gross exposure is
        // a margin call (a non-positive equity is ruin, judged when an order
        // comes to be filled).
        if by_models.is_none() && gross > 0.0 && equity > 0.0 && equity < self.cfg.maintenance_margin * gross - 1e-9 * gross {
            let message = format!(
                "margin call: equity {:.2} is below {}% of gross exposure {:.2} at {}",
                equity,
                self.cfg.maintenance_margin * 100.0,
                gross,
                time::format_timestamp(t)
            );
            let policy = match self.cfg.on_margin_call {
                OnMarginCall::Halt => "halt",
                OnMarginCall::Allow => "allow",
                OnMarginCall::Liquidate => "liquidate",
            };
            let call = EventKind::MarginCall {
                equity,
                requirement: self.cfg.maintenance_margin * gross,
                policy: policy.into(),
            };
            self.record(result, RunEvent::new(t, None, call));
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
                    let syms: Vec<Sym> = self.book.positions.keys().copied().collect();
                    for sym in syms {
                        let pos = self.book.position(sym);
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
                        let flow = self.traded_cash(k, sym, qty, p) + commission + fee;
                        let reference = self.fill_reference(k, sym, p);
                        equity -= commission + fee;
                        gross -= qty.abs() * p * m;
                        net -= -qty * p * m;
                        result.costs.commissions += commission;
                        result.costs.fees += fee;
                        result.costs.turnover += qty.abs() * p * m;
                        k.stores[fill_rel].insert(t, vec![Value::Equity(sym), Value::Time(t), Value::Num(qty), Value::Num(p)]);
                        let fill = FillRecord {
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
                            spread: 0.0,
                            reference,
                            broker: None,
                        };
                        self.record(result, RunEvent::new(t, Some(sym), EventKind::Fill { fill, remainder: None }).cash(-flow));
                    }
                    self.rewrite_book(k, t);
                }
            }
        }
        let base = if self.cfg.compounding { equity.max(0.0) } else { self.cfg.initial_cash };
        self.limit_state.observe(t, equity, self.cfg.initial_cash);
        result.exposure.push(ExposureRecord {
            t,
            cash: self.book.cash,
            gross,
            net,
            equity,
            leverage: if equity > 0.0 { gross / equity } else { 0.0 },
            receivables: self.book.receivable_cash(),
            capital: base,
            margin_initial: by_models.as_ref().map(|(m, _)| m.initial).unwrap_or(gross / self.cfg.max_gross.max(1.0)),
            margin_maintenance: by_models.as_ref().map(|(m, _)| m.maintenance).unwrap_or(self.cfg.maintenance_margin * gross),
            variation: if self.cfg.settlement_daily { self.variation(k, t) } else { 0.0 },
            margin_source: self.margin_source(by_models.as_ref().map(|(m, _)| m)),
        });
        let (initial, maintenance) = match &by_models {
            Some((m, _)) => (m.initial, m.maintenance),
            None => (gross / self.cfg.max_gross.max(1.0), self.cfg.maintenance_margin * gross),
        };
        self.write_requirement(k, t, initial, maintenance, by_models.as_ref().map(|(m, _)| m));
        result.equity_curve.push((t, equity));
        // `nav(T, N)`: the book's value at T's mark, before T's decisions
        // (data-bundle doc, section 7: what the metrics library reads).
        let nav_rel = k.rel_id("nav");
        k.stores[nav_rel].insert(t, vec![Value::Time(t), Value::Num(equity)]);
        // `capital(T, C)`: what a weight is a fraction of (the fixed base or
        // the equity); `mark(A, T, P)`: the price each holding is marked at.
        let capital_rel = k.rel_id("capital");
        k.stores[capital_rel].insert(t, vec![Value::Time(t), Value::Num(base)]);
        let mark_rel = k.rel_id("mark");
        let held: Vec<(Sym, f64)> = self.book.positions.iter().map(|(s, q)| (*s, *q)).collect();
        for (sym, q) in held {
            let (mark, source) = k.mark_with_source(sym, t);
            if let Some(p) = mark {
                k.stores[mark_rel].insert(t, vec![Value::Equity(sym), Value::Time(t), Value::Num(p)]);
            }
            result.holdings.push(HoldingRecord {
                t,
                instrument: sym,
                position: q,
                avg_cost: self.book.avg_cost.get(&sym).copied().unwrap_or(0.0),
                mark,
                mark_source: source.into(),
                option: None,
            });
        }
        if k.quote_cols.is_some() {
            let greeks = self.write_option_relations(k, t);
            // The holdings of options carry their underlying and Greeks
            // (E-73).
            for h in result.holdings.iter_mut().rev().take_while(|h| h.t == t) {
                let Some(o) = k.option_terms(h.instrument).cloned() else { continue };
                h.option = Some(crate::kernel::OptionHolding {
                    underlying: o.underlying,
                    spot: k.bar_price_quiet(o.underlying, t),
                    expiry: o.expiry,
                    multiplier: o.multiplier,
                    greeks: greeks.get(&h.instrument).copied(),
                });
            }
        }
        self.write_contract_relation(k, t);
        Ok(())
    }
}

impl Executor for SimExecutor {
    fn open_bar(&mut self, k: &mut Kernel, t: i64, result: &mut RunResult) -> Result<(), RunError> {
        let changed = self.open_actions(k, t, result);
        self.open_marks(k, t, result, changed)
    }

    fn on_decisions(&mut self, k: &mut Kernel, t: i64, by_equity: &BTreeMap<Sym, Vec<(Decision, usize)>>, result: &mut RunResult) -> Result<(), RunError> {
        // Every decision becomes an order, with its id (K-5.2 `order_sent`),
        // then the pre-trade limits act on the bar's orders (K-3.5).
        let mut pending: BTreeMap<Sym, Vec<(Decision, usize, OrderId)>> = BTreeMap::new();
        let mut exercises: Vec<(Decision, usize, OrderId)> = Vec::new();
        for (sym, ds) in by_equity {
            for (d, rule) in ds {
                let id = self.next_order;
                self.next_order += 1;
                self.record(result, RunEvent::new(t, Some(*sym), EventKind::OrderSent { decision: d.clone() }).rule(*rule).order(id));
                if d.ctor == Ctor::Exercise {
                    exercises.push((d.clone(), *rule, id));
                } else {
                    pending.entry(*sym).or_default().push((d.clone(), *rule, id));
                }
            }
        }
        if !self.cfg.limits.is_empty() {
            self.apply_limits(k, t, &mut pending, result)?;
        }
        // An exercise is not an order to the market: it happens at the
        // bar's close (K-5.5.4).
        for (d, rule, id) in exercises {
            self.exercise_decision(k, t, &d, rule, id, result);
        }
        self.pending = pending;
        self.pending_t = Some(t);
        Ok(())
    }

    fn fill(&mut self, k: &mut Kernel, tn: i64, result: &mut RunResult) -> Result<(), RunError> {
        // The fill bar in the executor's own list of bars (what ADV and
        // realized volatility look back over), extended by the fill bar for
        // this step only: moved out and back rather than copied.
        let mut bars = std::mem::take(&mut self.bars);
        let pushed = bars.last() != Some(&tn);
        if pushed {
            bars.push(tn);
        }
        let out = self.fill_over(k, tn, result, &bars);
        if pushed {
            bars.pop();
        }
        self.bars = bars;
        out
    }

    fn checkpoint(&self) -> Result<serde_json::Value, String> {
        serde_json::to_value(self).map_err(|e| e.to_string())
    }

    fn restore(&mut self, state: serde_json::Value) -> Result<(), String> {
        *self = serde_json::from_value(state).map_err(|e| format!("executor state: {}", e))?;
        Ok(())
    }

    fn book(&self) -> Option<&Book> {
        Some(&self.book)
    }

    fn working(&self) -> Vec<Sym> {
        self.working.keys().chain(self.open_targets.keys()).copied().collect()
    }

    fn expected(&self) -> Option<&Expected> {
        Some(&self.expected)
    }

    fn finish(&mut self, k: &mut Kernel, result: &mut RunResult) {
        // A decision on the last bar has no bar to fill at: it is recorded in
        // `decided` like any other, and dropped here so that decisions =
        // fills + dropped.
        if let Some(t) = self.pending_t.take() {
            for ds in std::mem::take(&mut self.pending).values() {
                for (d, rule, id) in ds {
                    let dropped = EventKind::Dropped {
                        decided: t,
                        decision: d.clone(),
                        reason: "no next bar".to_string(),
                    };
                    self.record(result, RunEvent::new(t, Some(d.equity), dropped).rule(*rule).order(*id));
                }
            }
        }
        // A market-on-close order whose session closed within the data is
        // settled at that close in one last step (nothing else fills there).
        let last = self.bars.last().copied();
        let settle = last.is_some_and(|l| {
            self.working
                .values()
                .any(|w| w.d.order.kind == OrderKind::Moc && k.session_close(w.d.equity, w.t).is_none_or(|end| end <= l) && k.session_last_bar(w.d.equity, w.t).is_some_and(|c| c <= l))
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
        let end = self.bars.last().copied();
        for w in std::mem::take(&mut self.working).into_values() {
            let expired = EventKind::ExpiredOrder {
                decided: w.t,
                decision: w.d.clone(),
                reason: format!("{}: the data ended before it executed", w.d.order),
            };
            self.record(result, RunEvent::new(end.unwrap_or(w.t), Some(w.d.equity), expired).rule(w.rule).order(w.order_id));
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
        if self.settled_at_close > 0 {
            result.warnings.push(RunWarning {
                bias: "settlement".into(),
                message: format!("{} daily settlements at the close: the binding holds no settlement price for them (K-5.4)", self.settled_at_close),
            });
        }
        if self.physical_expiries > 0 {
            result.warnings.push(RunWarning {
                bias: "settlement".into(),
                message: format!(
                    "{} physically settled futures contracts closed at their final settlement: the simulation does not deliver (K-5.4)",
                    self.physical_expiries
                ),
            });
        }
        if self.approx_bars > 0 {
            result.warnings.push(RunWarning {
                bias: "margin".into(),
                message: format!(
                    "margin `approx` priced the book at {} of {} bars: a fraction of the notional, the owner's open item (K-5.6, A-8)",
                    self.approx_bars,
                    self.bars.len()
                ),
            });
        }
        for note in &self.margin_notes {
            result.warnings.push(RunWarning {
                bias: "margin".into(),
                message: format!("a margin model passed over: {}", note),
            });
        }
        if self.forced_expiries > 0 {
            result.warnings.push(RunWarning {
                bias: "delisting".into(),
                message: format!(
                    "{} option positions forced to expire at intrinsic, in cash, when their underlying was delisted (mergers and spin-offs are not modelled, K-5.5.5)",
                    self.forced_expiries
                ),
            });
        }
        if self.unpriced_expiries > 0 {
            result.warnings.push(RunWarning {
                bias: "options".into(),
                message: format!("{} option positions expired worthless with no price for their underlying at expiry", self.unpriced_expiries),
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
        result.final_cash = self.book.cash;
        result.final_positions = self.book.positions.clone();
    }
}

impl Kernel<'_> {
    /// The strategy's decisions at `t`, validated (a delta quantity is
    /// positive, one decision per instrument), recorded in the result and in
    /// `decided`, grouped by instrument.
    pub fn decide_at(&mut self, t: i64, result: &mut RunResult) -> Result<BTreeMap<Sym, Vec<(Decision, usize)>>, RunError> {
        let decide = self.rel_id("decide");
        let decided = self.rel_id("decided");
        // With a session-offset clock, decisions are taken at the clock
        // bars only; a bar at which no instrument's clock strikes decides nothing.
        let clock_syms: Option<Vec<Sym>> = self.clock_rel.map(|cid| {
            let epos = self.rels[cid].entity_positions.first().copied().unwrap_or(0);
            self.stores[cid]
                .by_time
                .get(&t)
                .map(|tus| tus.iter().filter_map(|tu| tu[epos].as_equity()).collect())
                .unwrap_or_default()
        });
        if clock_syms.as_ref().map(|v| v.is_empty()).unwrap_or(false) {
            return Ok(BTreeMap::new());
        }
        let tuples = self.call(decide, &[Some(Value::Time(t)), None])?;
        let mut by_equity: BTreeMap<Sym, Vec<(Decision, usize)>> = BTreeMap::new();
        for (tu, rule) in tuples.iter() {
            let Value::Decision(d) = &tu[1] else {
                return Err(RunError::Internal("decide tuple without a decision".into()));
            };
            if !d.amount.is_finite() {
                return Err(RunError::BadDecision {
                    t,
                    rule: self.prog.rule_label(*rule),
                    decision: result.describe_decision(d),
                    message: "the amount is not a finite number".into(),
                });
            }
            by_equity.entry(d.equity).or_default().push(((**d).clone(), *rule));
        }
        // `silence flat`: an instrument held with no decision at its clock
        // bar is closed (language-v2, 4.4); a combo decides each of its legs.
        if self.cfg.silence_flat {
            let decided_on: HashSet<Sym> = by_equity.values().flatten().flat_map(|(d, _)| d.instruments()).collect();
            let position = self.rel_id("position");
            let held: Vec<Sym> = self.stores[position]
                .by_time
                .get(&t)
                .map(|tus| tus.iter().filter(|tu| tu[2].as_f64().map(|q| q != 0.0).unwrap_or(false)).filter_map(|tu| tu[0].as_equity()).collect())
                .unwrap_or_default();
            for sym in held {
                if decided_on.contains(&sym) {
                    continue;
                }
                if let Some(cs) = &clock_syms {
                    if !cs.contains(&sym) {
                        continue;
                    }
                }
                let order = crate::kernel::value::Order {
                    kind: match self.prog.default_order.as_str() {
                        "moo" => OrderKind::Moo,
                        "moc" => OrderKind::Moc,
                        _ => OrderKind::Market,
                    },
                    tif: Tif::Day,
                };
                by_equity.entry(sym).or_default().push((Decision::single(Ctor::HoldQuantity, sym, 0.0, order), usize::MAX));
            }
        }
        // One decision per instrument per bar (F-5): a combo counts as a
        // decision on each of its legs (K-2.3).
        let mut on: HashMap<Sym, (&Decision, usize)> = HashMap::new();
        for ds in by_equity.values() {
            for (d, rule) in ds {
                for sym in d.instruments() {
                    if let Some((a, ra)) = on.insert(sym, (d, *rule)) {
                        let label = |r: usize| if r == usize::MAX { "silence".to_string() } else { self.prog.rule_label(r) };
                        return Err(RunError::Conflict {
                            t,
                            equity: self.symbols.name(sym).to_string(),
                            first: (result.describe_decision(a), label(ra)),
                            second: (result.describe_decision(d), label(*rule)),
                        });
                    }
                }
            }
        }
        for ds in by_equity.values() {
            for (d, rule) in ds {
                result.emit(RunEvent::new(t, Some(d.equity), EventKind::Decision { decision: d.clone() }).rule(*rule));
                self.stores[decided].insert(t, vec![Value::Time(t), Value::Decision(Box::new(d.clone()))]);
            }
        }
        Ok(by_equity)
    }
}
