//! The run's event log (K-5.2): every decision, order, fill, drop, corporate
//! action and cash flow of a run, in the order the executor applied it,
//! numbered by the run's own counter. The executor's book changes only by
//! applying an event of the log (`Book::apply`), so the book at any barrier
//! is the fold of the log up to it from the initial capital (Proposition
//! K-5.1), which the kernel checks on itself (K-3.3). The run's decisions,
//! fills, drops and actions are projections of the log (K-7.1).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{Action, Decision, FillRecord, OptionTerms, Sym};

/// An order's identity within a run: one per decision the executor accepts
/// as an order, kept by its re-issues.
pub type OrderId = u64;

/// One event of the log.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunEvent {
    /// The run's own counter, from zero.
    pub seq: u64,
    /// The bar the event belongs to: a decision's and its order's decision
    /// bar, the bar a fill, an action or a cash flow took place at.
    pub t: i64,
    pub instrument: Option<Sym>,
    /// The decision rule that caused it (`usize::MAX`: the program's
    /// `silence flat`).
    pub rule: Option<usize>,
    pub order_id: Option<OrderId>,
    /// The cash the event moved, as the executor applied it.
    pub cash_flow: Option<f64>,
    pub kind: EventKind,
}

impl RunEvent {
    /// An event of `kind` at bar `t`, numbered when it is emitted.
    pub fn new(t: i64, instrument: Option<Sym>, kind: EventKind) -> RunEvent {
        RunEvent {
            seq: 0,
            t,
            instrument,
            rule: None,
            order_id: None,
            cash_flow: None,
            kind,
        }
    }
    pub fn rule(mut self, rule: usize) -> RunEvent {
        self.rule = Some(rule);
        self
    }
    pub fn order(mut self, id: OrderId) -> RunEvent {
        self.order_id = Some(id);
        self
    }
    pub fn cash(mut self, flow: f64) -> RunEvent {
        self.cash_flow = Some(flow);
        self
    }
}

/// What a partial fill's remainder became.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Fate {
    /// A delta order's remainder expires.
    Expired,
    /// A target's remainder re-issues at the next bar.
    Reissued,
}

/// The four flows of funding (K-5.2 `funding`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Funding {
    CashInterest,
    DebitInterest,
    BorrowFee,
    ShortRebate,
}

/// The kinds of K-5.2 the equity and futures executor emits.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum EventKind {
    /// A decision of the strategy at its bar (`decide_at`).
    Decision { decision: Decision },
    /// The decision accepted as an order (`on_decisions`).
    OrderSent { decision: Decision },
    /// A fill of an order, or a forced one (a margin call, a delisting);
    /// a partial fill carries what its remainder became.
    Fill { fill: FillRecord, remainder: Option<(f64, Fate)> },
    /// An order (or its remainder) that did not fill, and why; `decided` is
    /// its decision bar.
    Dropped { decided: i64, decision: Decision, reason: String },
    /// An order the position already met: nothing to fill.
    Satisfied { decided: i64 },
    /// A working order or an open target superseded by a later decision on
    /// its instrument (`by`, that decision's order).
    Replaced { decided: i64, by: OrderId },
    /// A working order ended by its time in force, or by the end of the
    /// data before it executed.
    ExpiredOrder { decided: i64, decision: Decision, reason: String },
    /// A dividend going ex on a position held: the receivable, paid at
    /// `pay` (its `id` is this event's seq).
    DividendDeclared { amount: f64, shares: f64, pay: i64, receivable: f64 },
    /// A corporate action applied to the book: a split (with the position
    /// it leaves), a dividend paid (the receivable it settles), a delisting,
    /// a reinvestment (with the position it leaves and its price).
    Action {
        action: Action,
        position: Option<f64>,
        receivable: Option<u64>,
        price: Option<f64>,
    },
    /// One flow of funding over the interval to a fill bar.
    Funding { flow: Funding, amount: f64 },
    /// Equity below the maintenance requirement at the bar's mark.
    MarginCall { equity: f64, requirement: f64, policy: String },
    /// An order a pre-trade limit refused (K-3.5 `reject`): terminal.
    Rejected { decided: i64, decision: Decision, reason: String },
    /// A limit the bar's orders would pass, its policy, the orders it
    /// acted on (rejected, scaled, or let through with a warning).
    LimitTriggered { limit: String, policy: String, orders: Vec<OrderId>, detail: String },
    /// Long contracts of an option exercised (K-5.5.4), or short ones
    /// assigned: `quantity` contracts leave the book, `position` is what
    /// remains. `underlying` is the underlying's price at the settlement
    /// instant and `intrinsic` a unit's value there. A cash-settled option
    /// moves `intrinsic × multiplier × quantity` (the event's cash flow); a
    /// physical one delivers the underlying at the strike, a fill of its
    /// own. `cause`: `expiry`, `early` (an early assignment), `decision`
    /// (the program's `exercise`) or `delisted` (its underlying delisted).
    Exercise {
        quantity: f64,
        position: f64,
        underlying: f64,
        intrinsic: f64,
        cash_settled: bool,
        cause: String,
    },
    Assignment {
        quantity: f64,
        position: f64,
        underlying: f64,
        intrinsic: f64,
        cash_settled: bool,
        cause: String,
    },
    /// Contracts of an option that expired out of the money, or within the
    /// tolerance of it: they leave the book with no cash.
    OptionExpiry { quantity: f64, underlying: f64, intrinsic: f64, cause: String },
    /// An option's terms adjusted for its underlying's corporate action
    /// (K-5.5.5, OCC's conventions), from the bar on: the terms before and
    /// after, the position after, the divisor of a unit's price (the
    /// average cost's), and the cause (`split`, `special_dividend`).
    Adjustment {
        before: Box<OptionTerms>,
        after: Box<OptionTerms>,
        position: f64,
        price_divisor: f64,
        cause: String,
    },
    /// A future settled daily at its session's close (K-5.4): the
    /// settlement price, the variation `Q · (S − reference) · point value`
    /// moved to cash, and the session; the price is the position's
    /// reference from now on. `at_close`: no settlement price was bound, so
    /// the close stands in.
    Settlement { price: f64, variation: f64, session: i64, at_close: bool },
    /// A root's continuous series changing contract with a position held
    /// (K-5.4): the old contract closed at its last price and the new
    /// opened at its first, the position carried, with a commission each;
    /// the series' additive adjustment changes by `adjustment`, which is not
    /// P&L.
    Roll {
        from: Sym,
        to: Sym,
        quantity: f64,
        from_price: f64,
        to_price: f64,
        adjustment: f64,
        commissions: f64,
    },
    /// A futures contract held at its last trade (K-5.4): settled at
    /// `price`, cash-settled or closed (a physical contract is not
    /// delivered, warned), the position gone.
    ContractExpiry { quantity: f64, price: f64, cash_settled: bool },
    /// What a margin call under the margin models sold (K-5.6): the
    /// requirement, the equity, and the forced fills by their seq, the
    /// largest maintenance per unit first.
    ForcedLiquidation { requirement: f64, equity: f64, fills: Vec<u64> },
}

impl EventKind {
    /// The K-5.2 name of the kind (`kernel.<run>.events.kind`).
    pub fn name(&self) -> &'static str {
        match self {
            EventKind::Decision { .. } => "decision",
            EventKind::OrderSent { .. } => "order_sent",
            EventKind::Fill { remainder: Some(_), .. } => "partial_fill",
            EventKind::Fill { .. } => "fill",
            EventKind::Dropped { .. } => "dropped",
            EventKind::Satisfied { .. } => "satisfied",
            EventKind::Replaced { .. } => "replaced",
            EventKind::ExpiredOrder { .. } => "expired_order",
            EventKind::DividendDeclared { .. } => "dividend_declared",
            EventKind::Action { action, .. } => match action {
                Action::Split { .. } => "split",
                Action::Dividend { .. } => "dividend_paid",
                Action::Delisting { .. } => "delisted",
                Action::Reinvest { .. } => "dividend_reinvested",
            },
            EventKind::Funding { .. } => "funding",
            EventKind::MarginCall { .. } => "margin_call",
            EventKind::Rejected { .. } => "rejected",
            EventKind::LimitTriggered { .. } => "limit_triggered",
            EventKind::Exercise { .. } => "exercise",
            EventKind::Assignment { .. } => "assignment",
            EventKind::OptionExpiry { .. } => "option_expiry",
            EventKind::Adjustment { .. } => "adjustment",
            EventKind::Settlement { .. } => "settlement",
            EventKind::Roll { .. } => "roll",
            EventKind::ContractExpiry { .. } => "contract_expiry",
            EventKind::ForcedLiquidation { .. } => "forced_liquidation",
        }
    }
}

/// A dividend gone ex and not yet paid.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Receivable {
    /// The seq of the `dividend_declared` event that recorded it.
    pub id: u64,
    pub pay: i64,
    pub instrument: Sym,
    pub cash: f64,
    pub amount: f64,
    pub shares: f64,
}

/// The book X's holdings (K-5.1): cash, positions with their average cost,
/// receivables. Changed only by `apply`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Book {
    pub cash: f64,
    pub positions: BTreeMap<Sym, f64>,
    /// The average cost a unit of each position was opened at (price units).
    pub avg_cost: BTreeMap<Sym, f64>,
    pub receivables: Vec<Receivable>,
    /// Under daily settlement, the price each future held is valued
    /// against: its last settlement, or the fill that opened it (E-10).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub reference: BTreeMap<Sym, f64>,
}

impl Book {
    pub fn new(cash: f64) -> Book {
        Book { cash, ..Book::default() }
    }

    pub fn position(&self, sym: Sym) -> f64 {
        self.positions.get(&sym).copied().unwrap_or(0.0)
    }

    /// The receivables' cash, in the order they were recorded.
    pub fn receivable_cash(&self) -> f64 {
        let mut total = 0.0;
        for r in &self.receivables {
            total += r.cash;
        }
        total
    }

    fn set_position(&mut self, sym: Sym, q: f64) {
        if q.abs() < 1e-9 {
            self.positions.remove(&sym);
            self.avg_cost.remove(&sym);
            self.reference.remove(&sym);
        } else {
            self.positions.insert(sym, q);
        }
    }

    /// The book after `e`. Every change the executor makes to its holdings
    /// is one of these, so that the log's fold is the book.
    pub fn apply(&mut self, e: &RunEvent) {
        if let Some(x) = e.cash_flow {
            self.cash += x;
        }
        match &e.kind {
            EventKind::Fill { fill, .. } => {
                let sym = fill.equity;
                let pos = self.position(sym);
                let new = pos + fill.quantity;
                // Average cost: an addition averages in at the fill price, a
                // reduction keeps it, a crossing opens the rest at the price.
                if new.abs() >= 1e-9 {
                    let avg = self.avg_cost.get(&sym).copied().unwrap_or(fill.price);
                    let cost = if pos == 0.0 || pos * new < 0.0 {
                        fill.price
                    } else if new.abs() > pos.abs() {
                        (avg * pos.abs() + fill.price * fill.quantity.abs()) / new.abs()
                    } else {
                        avg
                    };
                    self.avg_cost.insert(sym, cost);
                }
                self.set_position(sym, new);
                if let (Some(r), true) = (fill.reference, new.abs() >= 1e-9) {
                    self.reference.insert(sym, r);
                }
            }
            EventKind::DividendDeclared { amount, shares, pay, receivable } => {
                if let Some(sym) = e.instrument {
                    self.receivables.push(Receivable {
                        id: e.seq,
                        pay: *pay,
                        instrument: sym,
                        cash: *receivable,
                        amount: *amount,
                        shares: *shares,
                    });
                }
            }
            EventKind::Action { action, position, receivable, price } => {
                let Some(sym) = e.instrument else { return };
                match action {
                    Action::Split { factor } => {
                        if let Some(c) = self.avg_cost.get_mut(&sym) {
                            *c /= factor;
                        }
                        if let Some(q) = position {
                            self.set_position(sym, *q);
                        }
                    }
                    Action::Reinvest { shares, added, .. } => {
                        if let (Some(q), Some(p)) = (position, price) {
                            let avg = self.avg_cost.get(&sym).copied().unwrap_or(*p);
                            if q.abs() >= 1e-9 {
                                self.avg_cost.insert(sym, (avg * shares.abs() + p * added.abs()) / q.abs());
                            }
                            self.set_position(sym, *q);
                        }
                    }
                    Action::Dividend { .. } => {
                        if let Some(id) = receivable {
                            self.receivables.retain(|r| r.id != *id);
                        }
                    }
                    Action::Delisting { .. } => {}
                }
            }
            EventKind::Exercise { position, .. } | EventKind::Assignment { position, .. } => {
                if let Some(sym) = e.instrument {
                    self.set_position(sym, *position);
                }
            }
            EventKind::OptionExpiry { .. } => {
                if let Some(sym) = e.instrument {
                    self.set_position(sym, 0.0);
                }
            }
            EventKind::Settlement { price, .. } => {
                if let Some(sym) = e.instrument {
                    if self.positions.contains_key(&sym) {
                        self.reference.insert(sym, *price);
                    }
                }
            }
            EventKind::ContractExpiry { .. } => {
                if let Some(sym) = e.instrument {
                    self.set_position(sym, 0.0);
                }
            }
            EventKind::Adjustment { position, price_divisor, .. } => {
                if let Some(sym) = e.instrument {
                    if let Some(c) = self.avg_cost.get_mut(&sym) {
                        *c /= price_divisor;
                    }
                    self.set_position(sym, *position);
                }
            }
            _ => {}
        }
    }

    /// The fold of a log from an initial cash.
    pub fn fold(initial_cash: f64, events: &[RunEvent]) -> Book {
        let mut b = Book::new(initial_cash);
        for e in events {
            b.apply(e);
        }
        b
    }
}
