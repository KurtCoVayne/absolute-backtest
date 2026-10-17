//! The option model (K-5.5): an option is an instrument with terms (`spec`),
//! marked at its quote (its mid, its last trade, or the model's price at its
//! implied volatility) and filled against it (the touch or the mid, moved
//! against the order by a fraction of the spread, or the last trade), with a
//! per-contract commission and no slippage or impact (K-5.7). Combos fill all
//! or none. At its expiry an option in the money is exercised or assigned
//! and any other expires; an American short is assigned early by the
//! `exercise` field's rule, and a long is exercised by the program's
//! `exercise(A, Q)`. The terms the book holds are adjusted for the
//! underlying's corporate actions by OCC's conventions. Each decision bar
//! writes the class's kernel relations (K-2.4): `chain`, `spec`, `greeks`.

use super::*;
use crate::functions::{american, bsm, iv, Right, Style};
use crate::kernel::{ChainNeed, OptionMark, OptionTerms};
use crate::syntax::ast::{EarlyShort, ExerciseRule};

/// The columns of the option class's `quote` the executor reads (K-5.5.2).
#[derive(Clone, Copy, Debug)]
pub struct QuoteCols {
    rel: usize,
    entity: usize,
    bid: usize,
    ask: usize,
    last: usize,
    volume: usize,
    open_interest: usize,
}

impl QuoteCols {
    /// The program's `quote`, by its arguments' names; none outside the
    /// option class.
    pub fn of(prog: &crate::check::Program, rel_ids: &HashMap<String, usize>) -> Option<QuoteCols> {
        if prog.class != "option" {
            return None;
        }
        let sig = prog.relations.get("quote")?;
        let at = |n: &str| sig.args.iter().position(|a| a.name == n);
        Some(QuoteCols {
            rel: *rel_ids.get("quote")?,
            entity: sig.args.iter().position(|a| a.ty.is_entity())?,
            bid: at("Bid")?,
            ask: at("Ask")?,
            last: at("Last")?,
            volume: at("Volume")?,
            open_interest: at("OI")?,
        })
    }

    fn read(&self, tu: &[Value]) -> Quote {
        // A value the source does not carry reads as NaN (ERRATA E-71).
        let num = |i: usize| tu[i].as_f64().filter(|x| x.is_finite());
        Quote {
            bid: num(self.bid),
            ask: num(self.ask),
            last: num(self.last),
            volume: num(self.volume),
            open_interest: num(self.open_interest),
        }
    }
}

/// An option's quote at a bar, as `quote` holds it; a value the source does
/// not carry is absent.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Quote {
    pub bid: Option<f64>,
    pub ask: Option<f64>,
    pub last: Option<f64>,
    pub volume: Option<f64>,
    pub open_interest: Option<f64>,
}

impl Quote {
    fn positive_bid(&self) -> Option<f64> {
        self.bid.filter(|b| *b > 0.0)
    }

    fn positive_ask(&self) -> Option<f64> {
        self.ask.filter(|a| *a > 0.0)
    }

    /// The mid (K-5.5.2): of a two-sided quote, or half the ask of a
    /// one-sided one (no positive bid), flagged.
    pub fn mid(&self) -> Option<(f64, bool)> {
        let ask = self.positive_ask()?;
        Some(match self.positive_bid() {
            Some(b) => ((b + ask) / 2.0, false),
            None => (ask / 2.0, true),
        })
    }
}

/// The word `spec` and the chain filters name a right by.
pub fn right_word(r: Right) -> &'static str {
    match r {
        Right::Call => "call",
        Right::Put => "put",
    }
}

/// The word `spec` and the chain filters name a style by.
pub fn style_word(s: Style) -> &'static str {
    match s {
        Style::American => "american",
        Style::European => "european",
    }
}

/// τ in years from the end of `t`'s day to the option's expiry instant (the
/// day count of K-2.5, ACT/365; ERRATA E-71): a PM option on its expiry day
/// has none left, an AM one none from the day before.
pub fn tau(o: &OptionTerms, t: i64) -> f64 {
    let day_end = time::floor_div(t, time::DAY) * time::DAY + time::DAY;
    crate::functions::years(o.expires_at() - day_end).max(0.0)
}

/// The exercise rule of a program without the option fields (never an
/// option program, which states them, K-2.2).
fn default_rule() -> ExerciseRule {
    ExerciseRule {
        itm_at_expiry: true,
        early_short: EarlyShort::Never,
        early_long_decide: false,
        atm_tolerance: 0.0,
    }
}

impl Kernel<'_> {
    pub fn option_terms(&self, sym: Sym) -> Option<&OptionTerms> {
        self.contracts.get(&sym)?.option.as_ref()
    }

    /// `sym`'s quote keyed at bar `t`.
    pub fn quote(&mut self, sym: Sym, t: i64) -> Option<Quote> {
        let c = self.quote_cols?;
        let key = Value::Equity(sym);
        let tu = self.stores[c.rel].lookup(t, Some(&key)).iter().find(|tu| tu[c.entity] == key)?;
        Some(c.read(tu))
    }

    /// The same, by a scan of the bar's quotes (for readers without the
    /// store's index).
    pub fn quote_scan(&self, sym: Sym, t: i64) -> Option<Quote> {
        let c = self.quote_cols?;
        let key = Value::Equity(sym);
        self.stores[c.rel].by_time.get(&t)?.iter().find(|tu| tu[c.entity] == key).map(|tu| c.read(tu))
    }

    /// The latest bar at or before `t` whose quote of `sym` has a mid, and
    /// the mid.
    fn latest_mid(&self, sym: Sym, t: i64) -> Option<(i64, f64)> {
        let c = self.quote_cols?;
        let key = Value::Equity(sym);
        for (&bt, tus) in self.stores[c.rel].by_time.range(..=t).rev() {
            if let Some(tu) = tus.iter().find(|tu| tu[c.entity] == key) {
                if let Some((m, _)) = c.read(tu).mid() {
                    return Some((bt, m));
                }
            }
        }
        None
    }

    /// The model's price of a unit of `o` at `t` (K-2.5): BSM for a
    /// European, Bjerksund–Stensland 2002 for an American, at the financing
    /// block's cash rate and no dividend yield.
    pub fn model_price(&self, o: &OptionTerms, s: f64, t: i64, sigma: f64) -> f64 {
        let (tau, r) = (tau(o, t), self.cfg.cash_rate);
        match o.style {
            Style::European => bsm::price(s, o.strike, tau, r, 0.0, sigma, o.right),
            Style::American => american::price(s, o.strike, tau, r, 0.0, sigma, o.right),
        }
    }

    /// The volatility at which the model prices a unit of `o` at `p`; none
    /// outside the no-arbitrage bounds.
    pub fn implied(&self, o: &OptionTerms, s: f64, t: i64, p: f64) -> Option<f64> {
        iv::implied_vol(p, s, o.strike, tau(o, t), self.cfg.cash_rate, 0.0, o.right, o.style)
    }

    fn greeks_data(&self) -> bool {
        self.cfg.options.as_ref().is_some_and(|o| o.greeks_data)
    }

    /// The volatility the model marks `sym` at (K-5.5.2): the latest greeks
    /// row's under `greeks data`, else the one its latest quoted mid implies.
    fn mark_iv(&mut self, sym: Sym, t: i64) -> Option<f64> {
        if self.greeks_data() {
            let now = self.now;
            if let Some(v) = self.greeks_rows.get(&sym).and_then(|rows| rows.iter().rev().find(|(rt, r, _)| *rt <= t && *r <= now)).map(|x| x.2[0]) {
                return v.is_finite().then_some(v);
            }
        }
        let (tq, m) = self.latest_mid(sym, t)?;
        let o = self.option_terms(sym)?.clone();
        let s = self.bar_price_quiet(o.underlying, tq)?;
        self.implied(&o, s, tq, m)
    }

    /// An option's mark at decision bar `t` (K-5.5.2) and its source: the
    /// mid of the bar's quote (`mid`; `one_sided` at half the ask), its last
    /// trade (`last`), or the model's price at `mark_iv` (`model`). None when
    /// the bar gives none, which the caller marks at the last mark, `stale`.
    pub fn option_mark(&mut self, sym: Sym, t: i64) -> Option<(f64, &'static str)> {
        let mark = self.cfg.options.as_ref().map(|o| o.mark).unwrap_or(OptionMark::Mid);
        match mark {
            OptionMark::Mid => {
                let (m, one_sided) = self.quote(sym, t)?.mid()?;
                Some((m, if one_sided { "one_sided" } else { "mid" }))
            }
            OptionMark::Last => Some((self.quote(sym, t)?.last?, "last")),
            OptionMark::Model => {
                let o = self.option_terms(sym)?.clone();
                let s = self.bar_price_quiet(o.underlying, t)?;
                let sigma = self.mark_iv(sym, t)?;
                let p = self.model_price(&o, s, t, sigma);
                p.is_finite().then_some((p, "model"))
            }
        }
    }

    /// A holding's mark at `t` and its source (K-5.1): the bar's price, an
    /// option's mark, or the last one seen (`stale`).
    pub fn mark_with_source(&mut self, sym: Sym, t: i64) -> (Option<f64>, &'static str) {
        if self.has_options && self.is_option(sym) {
            if let Some((p, source)) = self.option_mark(sym, t) {
                self.last_price.insert(sym, p);
                return (Some(p), source);
            }
        } else if let Some(p) = self.bar_price(sym, t) {
            return (Some(p), "price");
        }
        match self.last_price.get(&sym) {
            Some(&p) => (Some(p), "stale"),
            None => (None, "none"),
        }
    }

    /// A contract's greeks at `t` (K-5.5.6), `[IV, Delta, Gamma, Theta, Vega,
    /// Rho]`: the binding's row at `t` under `greeks data`; else the model's
    /// at the volatility the mark implies, none where it implies none (a
    /// mark outside the no-arbitrage bounds, ERRATA E-71).
    pub fn option_greeks(&mut self, sym: Sym, t: i64) -> Option<[f64; 6]> {
        if self.greeks_data() {
            let now = self.now;
            return self.greeks_rows.get(&sym)?.iter().rev().find(|(rt, r, _)| *rt == t && *r <= now).map(|x| x.2);
        }
        let o = self.option_terms(sym)?.clone();
        let s = self.bar_price_quiet(o.underlying, t)?;
        let (p, source) = self.option_mark(sym, t)?;
        let sigma = if source == "model" { self.mark_iv(sym, t)? } else { self.implied(&o, s, t, p)? };
        let (tau, r) = (tau(&o, t), self.cfg.cash_rate);
        let g = match o.style {
            Style::European => bsm::greeks(s, o.strike, tau, r, 0.0, sigma, o.right),
            Style::American => american::greeks(s, o.strike, tau, r, 0.0, sigma, o.right),
        };
        Some([sigma, g.delta, g.gamma, g.theta, g.vega, g.rho])
    }

    /// The price an order on `sym` fills at against its quote at `t`
    /// (K-5.5.3), and the quote's mid: `touch + f spread` buys at
    /// `ask + f · (ask − bid)` and sells at `bid − f · (ask − bid)`; `mid + f
    /// spread` moves from the mid by `f` times the half-spread; `last` is the
    /// last trade. A sell needs a bid and a buy an ask, a mid fill both; a
    /// price is never below zero.
    pub fn option_fill(&mut self, sym: Sym, t: i64, buying: bool) -> Result<(f64, f64), String> {
        let (base, f) = self.cfg.options.as_ref().map(|o| (o.fill.clone(), o.spread)).unwrap_or_else(|| ("touch".to_string(), 0.0));
        let name = self.symbols.name(sym).to_string();
        let when = time::format_timestamp(t);
        let q = self.quote(sym, t).ok_or_else(|| format!("no quote for {} at {}", name, when))?;
        match base.as_str() {
            "last" => {
                let p = q.last.ok_or_else(|| format!("no last trade for {} at {}", name, when))?;
                Ok((p, q.mid().map(|m| m.0).unwrap_or(p)))
            }
            "mid" => {
                let (Some(b), Some(a)) = (q.positive_bid(), q.positive_ask()) else {
                    return Err(format!("the quote of {} at {} is not two-sided", name, when));
                };
                let (mid, half) = ((a + b) / 2.0, (a - b) / 2.0);
                Ok((if buying { mid + f * half } else { (mid - f * half).max(0.0) }, mid))
            }
            _ => {
                if buying {
                    let a = q.positive_ask().ok_or_else(|| format!("no ask for {} at {}", name, when))?;
                    let b = q.positive_bid().unwrap_or(0.0);
                    Ok((a + f * (a - b), q.mid().map(|m| m.0).unwrap_or(a)))
                } else {
                    let b = q.positive_bid().ok_or_else(|| format!("no bid for {} at {}", name, when))?;
                    let a = q.positive_ask().unwrap_or(b);
                    Ok(((b - f * (a - b)).max(0.0), (a + b) / 2.0))
                }
            }
        }
    }

    /// The latest price the price relation holds for `sym` keyed at or
    /// before `end`.
    fn price_by(&mut self, sym: Sym, end: i64) -> Option<f64> {
        let id = self.price_rel?;
        if !self.rels[id].stored {
            return self.bar_price_quiet(sym, end);
        }
        let entity_pos = *self.rels[id].entity_positions.first()?;
        let col = self.price_col;
        let key = Value::Equity(sym);
        self.stores[id]
            .by_time
            .range(..=end)
            .rev()
            .find_map(|(_, tus)| tus.iter().find(|tu| tu[entity_pos] == key).and_then(|tu| tu[col].as_f64()))
    }

    /// The underlying's price an option is settled against at its expiry,
    /// processed at `t` (K-5.5.4): for AM settlement the open of the expiry
    /// day's bar; else the last price by the expiry day's end (its close).
    fn settlement_price(&mut self, o: &OptionTerms, t: i64) -> Option<f64> {
        if o.am && time::day_key(t) == time::day_key(o.expiry) {
            if let Some(p) = self.bar_open(o.underlying, t) {
                return Some(p);
            }
        }
        self.price_by(o.underlying, o.expiry + time::DAY - 1).or_else(|| self.last_price.get(&o.underlying).copied())
    }

    /// No decision bar follows `t` within its day.
    fn last_bar_of_day(&self, t: i64) -> bool {
        let end = time::floor_div(t, time::DAY) * time::DAY + time::DAY;
        self.schedule.get(&self.prog.decision_cal).is_none_or(|d| d.range(t + 1..end).next().is_none())
    }

    /// The decision bar after `t`.
    fn next_bar(&self, t: i64) -> Option<i64> {
        self.schedule.get(&self.prog.decision_cal)?.range(t + 1..).next().copied()
    }

    /// The dividends of `u` announced by `t` that go ex after `t` and by the
    /// next decision bar, summed: what an early assignment of a call on the
    /// bar before the ex-date weighs (K-5.5.4).
    fn dividend_ahead(&self, u: Sym, t: i64) -> Option<f64> {
        let id = self.dividend_rel?;
        let next = self.next_bar(t)?;
        let info = &self.rels[id];
        let entity_pos = *info.entity_positions.first()?;
        let sig = &self.prog.relations[&info.name];
        let ex_col = sig.args.iter().enumerate().find(|(i, a)| a.ty == Ty::Timestamp && *i != info.key_pos).map(|(i, _)| i)?;
        let amount_col = sig.args.iter().position(|a| a.mode == Mode::Out && matches!(&a.ty, Ty::Quantity(d) if d.c2 == 2 && d.s2 == -2))?;
        let key = Value::Equity(u);
        let mut total = None;
        for tus in self.stores[id].by_time.range(..=t).map(|(_, v)| v) {
            for tu in tus.iter().filter(|tu| tu[entity_pos] == key) {
                if let (Some(ex), Some(a)) = (tu[ex_col].as_time(), tu[amount_col].as_f64()) {
                    if ex > t && ex <= next {
                        total = Some(total.unwrap_or(0.0) + a);
                    }
                }
            }
        }
        total
    }

    /// The special dividends of `u` going ex at `t` (K-5.5.5), summed.
    fn special_dividend_at(&self, u: Sym, t: i64) -> Option<f64> {
        let id = self.special_dividend_rel?;
        let info = &self.rels[id];
        let entity_pos = *info.entity_positions.first()?;
        let sig = &self.prog.relations[&info.name];
        let ex_col = sig.args.iter().enumerate().find(|(i, a)| a.ty == Ty::Timestamp && *i != info.key_pos).map(|(i, _)| i)?;
        let amount_col = sig.args.iter().position(|a| a.mode == Mode::Out && matches!(&a.ty, Ty::Quantity(d) if d.c2 == 2 && d.s2 == -2))?;
        let key = Value::Equity(u);
        let mut total = None;
        for tus in self.stores[id].by_time.range(..=t).map(|(_, v)| v) {
            for tu in tus.iter().filter(|tu| tu[entity_pos] == key) {
                if let (Some(ex), Some(a)) = (tu[ex_col].as_time(), tu[amount_col].as_f64()) {
                    if ex == t {
                        total = Some(total.unwrap_or(0.0) + a);
                    }
                }
            }
        }
        total
    }

    /// The split factor of `u` effective at `t`, when it has one.
    fn split_at(&self, u: Sym, t: i64) -> Option<f64> {
        let id = self.split_rel?;
        let entity_pos = *self.rels[id].entity_positions.first()?;
        let key = Value::Equity(u);
        self.stores[id]
            .by_time
            .get(&t)?
            .iter()
            .find(|tu| tu[entity_pos] == key)
            .and_then(|tu| tu.iter().rev().find_map(|v| v.as_f64()))
    }

    /// Whether `u` is delisted at `t`.
    fn delisted_at(&self, u: Sym, t: i64) -> bool {
        let Some(id) = self.delisted_rel else { return false };
        let Some(&entity_pos) = self.rels[id].entity_positions.first() else { return false };
        let key = Value::Equity(u);
        self.stores[id].by_time.get(&t).is_some_and(|tus| tus.iter().any(|tu| tu[entity_pos] == key))
    }

    /// The spot a chain need's moneyness is against: its relation's price of
    /// the underlying at `t`.
    fn spot(&mut self, relation: &str, u: Sym, t: i64) -> Option<f64> {
        let id = *self.rel_ids.get(relation)?;
        let sig = self.prog.relations.get(relation)?;
        let col = sig
            .args
            .iter()
            .enumerate()
            .position(|(i, a)| i != self.rels[id].key_pos && a.mode == Mode::Out && matches!(a.ty, Ty::Quantity(_)))?;
        let entity_pos = *self.rels[id].entity_positions.first()?;
        let key = Value::Equity(u);
        if self.rels[id].stored {
            return self.stores[id].lookup(t, Some(&key)).iter().find(|tu| tu[entity_pos] == key).and_then(|tu| tu[col].as_f64());
        }
        let width = sig.args.len();
        let mut pattern: Vec<Option<Value>> = vec![None; width];
        pattern[self.rels[id].key_pos] = Some(Value::Time(t));
        pattern[entity_pos] = Some(key);
        self.call(id, &pattern).ok()?.first().and_then(|(tu, _)| tu[col].as_f64())
    }

    /// The contracts of a chain need at `t` (K-2.4 `chain`, D-4.4): the
    /// underlying's `chain_index` snapshot of `t`'s day taken at or before
    /// `t`, as received by the barrier, admitted by the need's filters with
    /// the bar's open interest (`abt_wire::chain::admits`, the data layer's
    /// own predicate).
    pub(crate) fn chain_at(&mut self, need: &ChainNeed, t: i64) -> Vec<Sym> {
        let day = time::floor_div(t, time::DAY) * time::DAY;
        let now = self.now;
        let rows: Vec<Sym> = match self.chains.get(&need.underlying).and_then(|snaps| snaps.range(day..=t).next_back()) {
            Some((_, rows)) => rows.iter().filter(|(_, r)| *r <= now).map(|(s, _)| *s).collect(),
            None => return Vec::new(),
        };
        let spot = self.spot(&need.spot, need.underlying, t);
        let mut oi: HashMap<Sym, f64> = HashMap::new();
        if let Some(c) = self.quote_cols {
            if let Some(tus) = self.stores[c.rel].by_time.get(&t) {
                for tu in tus {
                    if let (Some(s), Some(n)) = (tu[c.entity].as_equity(), c.read(tu).open_interest) {
                        oi.insert(s, n);
                    }
                }
            }
        }
        rows.into_iter()
            .filter(|s| {
                let Some(o) = self.option_terms(*s) else { return false };
                let terms = abt_wire::chain::Terms {
                    expiry: o.expiry,
                    strike: o.strike,
                    right: right_word(o.right),
                    style: style_word(o.style),
                    open_interest: oi.get(s).map(|n| *n as i64),
                };
                abt_wire::chain::admits(&need.filters, t, spot, &terms)
            })
            .collect()
    }
}

/// What a combo's legs fill at: per leg its price and the mid it is costed
/// against.
type LegPrices = Vec<(f64, f64)>;

impl SimExecutor {
    fn exercise_rule(&self) -> ExerciseRule {
        self.cfg.options.as_ref().map(|o| o.exercise).unwrap_or_else(default_rule)
    }

    /// The kernel's contract terms brought up to the adjustments the book
    /// made (the adjusted terms are the executor's state, restored with it).
    pub(super) fn sync_terms(&self, k: &mut Kernel) {
        for (sym, o) in &self.adjusted {
            if let Some(c) = k.contracts.get_mut(sym) {
                if c.option.as_ref() != Some(o) {
                    c.multiplier = o.multiplier;
                    c.option = Some(o.clone());
                }
            }
        }
    }

    /// Settles `qty` contracts of the option `sym` (signed as the position,
    /// positive long) against the underlying at `s` (K-5.5.4): exercised
    /// (long) or assigned (short), or, when `expiring` and not in the money
    /// beyond the tolerance, expired. A cash-settled option (or any, when
    /// `cash`) moves intrinsic × multiplier × qty; a physical one delivers
    /// the underlying at the strike, a fill with no commission.
    #[allow(clippy::too_many_arguments)]
    fn settle(&mut self, k: &mut Kernel, t: i64, sym: Sym, qty: f64, s: f64, cause: &str, expiring: bool, cash: bool, order: Option<(usize, OrderId)>, result: &mut RunResult) {
        let Some(o) = k.option_terms(sym).cloned() else { return };
        let rule = self.exercise_rule();
        let pos = self.book.position(sym);
        let intrinsic = o.intrinsic(s);
        let itm = o.right.sign() * (s - o.strike) > rule.atm_tolerance * o.strike;
        let tag = |e: RunEvent| match order {
            Some((r, id)) => e.rule(r).order(id),
            None => e,
        };
        if expiring && !(itm && rule.itm_at_expiry) {
            let e = RunEvent::new(
                t,
                Some(sym),
                EventKind::OptionExpiry {
                    quantity: qty,
                    underlying: s,
                    intrinsic,
                    cause: cause.to_string(),
                },
            );
            self.record(result, tag(e));
            return;
        }
        let cash_settled = o.cash || cash;
        let (quantity, position, cause) = (qty.abs(), pos - qty, cause.to_string());
        let kind = if qty > 0.0 {
            EventKind::Exercise {
                quantity,
                position,
                underlying: s,
                intrinsic,
                cash_settled,
                cause,
            }
        } else {
            EventKind::Assignment {
                quantity,
                position,
                underlying: s,
                intrinsic,
                cash_settled,
                cause,
            }
        };
        if cash_settled {
            let flow = qty * intrinsic * o.multiplier;
            self.record(result, tag(RunEvent::new(t, Some(sym), kind).cash(flow)));
            return;
        }
        self.record(result, tag(RunEvent::new(t, Some(sym), kind)));
        // The deliverable: a call exercised buys the underlying at the
        // strike, a put sells it; an assignment is the mirror.
        let delivered = o.right.sign() * qty * o.multiplier;
        let m = k.multiplier(o.underlying);
        let fill = FillRecord {
            t,
            equity: o.underlying,
            quantity: delivered,
            price: o.strike,
            at_last_price: false,
            commission: 0.0,
            fee: 0.0,
            slippage: 0.0,
            impact: 0.0,
            participation: 0.0,
            partial: false,
            forced: false,
            spread: 0.0,
            reference: None,
        };
        self.record(
            result,
            tag(RunEvent::new(t, Some(o.underlying), EventKind::Fill { fill, remainder: None }).cash(-delivered * o.strike * m)),
        );
        let fill_rel = k.rel_id("fill");
        k.stores[fill_rel].insert(t, vec![Value::Equity(o.underlying), Value::Time(t), Value::Num(delivered), Value::Num(o.strike)]);
    }

    /// The options the book holds, with their positions.
    fn held_options(&self, k: &Kernel) -> Vec<(Sym, f64)> {
        self.book.positions.iter().filter(|(s, _)| k.is_option(**s)).map(|(s, q)| (*s, *q)).collect()
    }

    /// OCC's adjustments at the ex-date (K-5.5.5), before the bar's fills
    /// and marks, of every option the book holds: a whole split `n:1`
    /// multiplies the contracts by `n` and divides the strike by `n`; any
    /// other ratio `r` (a reverse split too) keeps the contracts and takes
    /// the multiplier to `multiplier · r` and the strike to `strike / r`; a
    /// special dividend lowers the strike by its amount.
    pub(super) fn option_adjustments(&mut self, k: &mut Kernel, t: i64, result: &mut RunResult) -> bool {
        let mut changed = false;
        for (sym, pos) in self.held_options(k) {
            let Some(before) = k.option_terms(sym).cloned() else { continue };
            let u = before.underlying;
            let mut after = before.clone();
            let mut position = pos;
            let mut divisor = 1.0;
            let mut cause = Vec::new();
            if let Some(f) = k.split_at(u, t).filter(|f| *f > 0.0 && *f != 1.0) {
                if f >= 2.0 && f.fract() == 0.0 {
                    position = pos * f;
                } else {
                    after.multiplier = before.multiplier * f;
                }
                after.strike = before.strike / f;
                divisor = f;
                cause.push("split");
            }
            if let Some(d) = k.special_dividend_at(u, t).filter(|d| *d != 0.0) {
                after.strike -= d;
                cause.push("special_dividend");
            }
            if cause.is_empty() {
                continue;
            }
            let adjusted = EventKind::Adjustment {
                before: Box::new(before),
                after: Box::new(after.clone()),
                position,
                price_divisor: divisor,
                cause: cause.join(", "),
            };
            self.record(result, RunEvent::new(t, Some(sym), adjusted));
            if let Some(lp) = k.last_price.get_mut(&sym) {
                *lp /= divisor;
            }
            self.adjusted.insert(sym, after);
            self.sync_terms(k);
            changed = true;
        }
        changed
    }

    /// Options at their expiry and options whose underlying is delisted
    /// (K-5.5.4, K-5.5.5), before the bar's marks: an expiring option is
    /// settled at the first decision bar of its expiry day (AM settlement)
    /// or its last (PM), against the underlying's open or close of the day;
    /// a delisted underlying forces the expiry at intrinsic against its
    /// delisting price, in cash, warned.
    pub(super) fn option_expiries(&mut self, k: &mut Kernel, t: i64, result: &mut RunResult) -> bool {
        let mut changed = false;
        for (sym, q) in self.held_options(k) {
            let Some(o) = k.option_terms(sym).cloned() else { continue };
            if k.delisted_at(o.underlying, t) {
                let s = k.price_at(o.underlying, t).unwrap_or(0.0);
                self.settle(k, t, sym, q, s, "delisted", true, true, None, result);
                self.forced_expiries += 1;
                changed = true;
                continue;
            }
            let (day, expiry_day) = (time::day_key(t), time::day_key(o.expiry));
            let due = day > expiry_day || (day == expiry_day && (o.am || k.last_bar_of_day(t)));
            if !due {
                continue;
            }
            match k.settlement_price(&o, t) {
                Some(s) => self.settle(k, t, sym, q, s, "expiry", true, false, None, result),
                None => {
                    self.unpriced_expiries += 1;
                    let e = RunEvent::new(
                        t,
                        Some(sym),
                        EventKind::OptionExpiry {
                            quantity: q,
                            underlying: f64::NAN,
                            intrinsic: 0.0,
                            cause: "expiry".into(),
                        },
                    );
                    self.record(result, e);
                }
            }
            changed = true;
        }
        changed
    }

    /// Early assignment of American shorts at the bar's close (K-5.5.4),
    /// by the `exercise` field's `early_short`: a call on the bar before an
    /// ex-dividend date when its extrinsic value is below the dividend (or
    /// the threshold); a put on any bar when its extrinsic value is below
    /// the financing of its strike to expiry, `K · (1 − e^{−rτ})` (or the
    /// threshold). Only an option in the money is assigned.
    pub(super) fn early_assignments(&mut self, k: &mut Kernel, t: i64, result: &mut RunResult) -> bool {
        let rule = self.exercise_rule();
        if rule.early_short == EarlyShort::Never {
            return false;
        }
        let mut changed = false;
        for (sym, q) in self.held_options(k) {
            if q >= 0.0 {
                continue;
            }
            let Some(o) = k.option_terms(sym).cloned() else { continue };
            if o.style != Style::American {
                continue;
            }
            let (Some(s), Some((mark, _))) = (k.bar_price_quiet(o.underlying, t), k.option_mark(sym, t)) else {
                continue;
            };
            let intrinsic = o.intrinsic(s);
            if o.right.sign() * (s - o.strike) <= rule.atm_tolerance * o.strike {
                continue;
            }
            let extrinsic = mark - intrinsic;
            let threshold = match (o.right, rule.early_short) {
                (Right::Call, EarlyShort::Dividend) => k.dividend_ahead(o.underlying, t),
                (Right::Call, EarlyShort::Threshold(x)) => k.dividend_ahead(o.underlying, t).map(|_| x),
                (Right::Put, EarlyShort::Dividend) => Some(o.strike * (1.0 - (-k.cfg.cash_rate * tau(&o, t)).exp())),
                (Right::Put, EarlyShort::Threshold(x)) => Some(x),
                (_, EarlyShort::Never) => None,
            };
            if threshold.is_some_and(|x| extrinsic < x) {
                self.settle(k, t, sym, q, s, "early", false, false, None, result);
                changed = true;
            }
        }
        changed
    }

    /// The program's `exercise(A, Q)` at the decision bar's close (K-5.5.4,
    /// `early_long decide`): Q long contracts of an American option
    /// exercised against the underlying's price at the bar; dropped under
    /// `early_long never`, for a European, or beyond the long position.
    pub(super) fn exercise_decision(&mut self, k: &mut Kernel, t: i64, d: &Decision, rule: usize, id: OrderId, result: &mut RunResult) {
        let sym = d.equity;
        let name = k.symbols.name(sym).to_string();
        let held = self.book.position(sym);
        let drop = |reason: String| {
            RunEvent::new(
                t,
                Some(sym),
                EventKind::Dropped {
                    decided: t,
                    decision: d.clone(),
                    reason,
                },
            )
            .rule(rule)
            .order(id)
        };
        let why = match k.option_terms(sym).cloned() {
            None => Some(format!("{} is not an option", name)),
            Some(_) if !self.exercise_rule().early_long_decide => Some("the execution block's `exercise` has `early_long never`".to_string()),
            Some(o) if o.style == Style::European => Some(format!("{} is European: it is exercised only at its expiry", name)),
            Some(_) if d.amount <= 0.0 || d.amount > held + 1e-9 => Some(format!("exercise of {} contracts of {} against {} held long", d.amount, name, held)),
            Some(o) => match k.bar_price_quiet(o.underlying, t) {
                None => Some(format!("no price for {}'s underlying at {}", name, time::format_timestamp(t))),
                Some(s) => {
                    self.settle(k, t, sym, d.amount, s, "decision", false, false, Some((rule, id)), result);
                    None
                }
            },
        };
        if let Some(reason) = why {
            self.record(result, drop(reason));
        }
    }

    /// The prices a combo's legs fill at, at `tn`, all or none (K-5.5.3):
    /// an option's against its quote, any other instrument's at the bar's
    /// price moved by the slippage; or the first leg that has none.
    fn combo_prices(&mut self, k: &mut Kernel, tn: i64, legs: &[(Sym, f64)], bars: &[i64]) -> Result<LegPrices, String> {
        let end = bars.len().saturating_sub(1);
        let mut out = Vec::with_capacity(legs.len());
        for &(sym, q) in legs {
            let name = k.symbols.name(sym).to_string();
            if k.is_option(sym) {
                out.push(k.option_fill(sym, tn, q > 0.0).map_err(|why| format!("leg {}: {}", name, why))?);
            } else {
                let p = k.bar_price(sym, tn).ok_or_else(|| format!("leg {}: no price at {}", name, time::format_timestamp(tn)))?;
                let slip = self.cfg.slippage_bps / 10_000.0 + self.cfg.slippage_vol_mult * k.realized_vol(sym, bars, end).unwrap_or(0.0);
                out.push((if q > 0.0 { p * (1.0 + slip) } else { p * (1.0 - slip) }, p));
            }
        }
        Ok(out)
    }

    /// A combo's net price per unit (K-5.5.3): the legs' signed prices over
    /// the greatest common divisor of their whole quantities (the smallest
    /// quantity when one is fractional), positive a debit.
    fn combo_net(legs: &[(Sym, f64)], prices: &[(f64, f64)]) -> f64 {
        let whole = legs.iter().all(|(_, q)| q.fract() == 0.0);
        let unit = if whole {
            legs.iter().map(|(_, q)| q.abs() as u64).fold(0, gcd).max(1) as f64
        } else {
            legs.iter().map(|(_, q)| q.abs()).fold(f64::INFINITY, f64::min)
        };
        legs.iter().zip(prices).map(|((_, q), (p, _))| q * p).sum::<f64>() / unit
    }

    /// A combo at `tn` (K-5.5.3, K-5.8): every leg priced, then filled at
    /// once under one order id; dropped with the leg named when a leg has no
    /// price, and all or none against the leverage rule and the
    /// participation cap. A `limit(P)` combo fills when its net is at or
    /// below `P` (a debit; a credit is negative) and otherwise works:
    /// whether it is still working.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn fill_combo(&mut self, k: &mut Kernel, tn: i64, d: &Decision, rule: usize, id: OrderId, decided: i64, bars: &[i64], result: &mut RunResult) -> Result<bool, RunError> {
        let dropped = |reason: String| RunEvent::new(tn, Some(d.equity), EventKind::Dropped { decided, decision: d.clone(), reason }).rule(rule).order(id);
        let prices = match self.combo_prices(k, tn, &d.legs, bars) {
            Ok(p) => p,
            Err(why) => {
                self.record(result, dropped(format!("combo dropped: {}", why)));
                return Ok(false);
            }
        };
        if let OrderKind::Limit(level) = d.order.kind {
            let net = Self::combo_net(&d.legs, &prices);
            if net > level {
                return Ok(true);
            }
        }
        if self.cfg.participation_cap > 0.0 {
            for &(sym, q) in &d.legs {
                if let Some(v) = k.bar_volume(sym, tn) {
                    if q.abs() > self.cfg.participation_cap * v {
                        let why = format!(
                            "combo dropped, all or none: leg {} of {} exceeds the participation cap of {} of volume {}",
                            k.symbols.name(sym),
                            q,
                            self.cfg.participation_cap,
                            v
                        );
                        self.record(result, dropped(why));
                        return Ok(false);
                    }
                }
            }
        }
        let costs: Vec<(f64, f64)> = d
            .legs
            .iter()
            .zip(&prices)
            .map(|(&(sym, q), &(p, _))| {
                let commission = self.commission(k, sym, q, p);
                (self.traded_cash(k, sym, q, p) + commission, commission)
            })
            .collect();
        let cost: f64 = costs.iter().map(|c| c.0).sum();
        // Leverage: the book after every leg, judged once.
        let new_cash = self.book.cash - cost;
        let (mut gross, mut net) = (0.0, new_cash);
        let mut after: BTreeMap<Sym, (f64, f64)> = BTreeMap::new();
        for (&s2, &q2) in &self.book.positions {
            let p2 = k.price_at(s2, tn).unwrap_or(0.0);
            after.insert(s2, (q2, p2));
        }
        for (&(sym, q), &(p, _)) in d.legs.iter().zip(&prices) {
            let e = after.entry(sym).or_insert((0.0, p));
            e.0 += q;
            e.1 = p;
        }
        for (&s2, &(q2, p2)) in &after {
            let m2 = k.multiplier(s2);
            gross += q2.abs() * p2 * m2;
            net += q2 * p2 * m2;
        }
        for r in &self.book.receivables {
            net += r.cash;
        }
        let commissions: f64 = costs.iter().map(|c| c.1).sum();
        let tol = 1e-9 * (1.0 + net.abs()) + commissions;
        let max_gross = self.cfg.max_gross.max(1.0);
        let liquid = new_cash + self.book.receivable_cash();
        let breach = if self.cfg.compounding {
            liquid < -(max_gross - 1.0) * net.max(0.0) - tol || gross > max_gross * net + tol
        } else {
            gross > max_gross * self.cfg.initial_cash + tol
        };
        if breach {
            let message = format!(
                "leverage: after the combo cash would be {:.2} and gross exposure {:.2} against equity {:.2} (limit {}x gross)",
                new_cash, gross, net, max_gross
            );
            match self.cfg.on_leverage {
                OnLeverage::Halt => {
                    return Err(RunError::Risk {
                        t: decided,
                        rule: k.prog.rule_label(rule),
                        decision: result.describe_decision(d),
                        message,
                    })
                }
                OnLeverage::Reject => {
                    self.record(result, dropped(format!("combo rejected, all or none: {}", message)));
                    return Ok(false);
                }
                OnLeverage::Allow => {}
            }
        }
        let fill_rel = k.rel_id("fill");
        for ((&(sym, q), &(p, mid)), &(flow, commission)) in d.legs.iter().zip(&prices).zip(&costs) {
            let m = k.multiplier(sym);
            let fill = FillRecord {
                t: tn,
                equity: sym,
                quantity: q,
                price: p,
                at_last_price: false,
                commission,
                fee: 0.0,
                slippage: if k.is_option(sym) { 0.0 } else { (p - mid).abs() * q.abs() * m },
                impact: 0.0,
                participation: k.bar_volume(sym, tn).filter(|v| *v > 0.0).map(|v| q.abs() / v).unwrap_or(0.0),
                partial: false,
                forced: false,
                spread: if k.is_option(sym) { (p - mid) * q * m } else { 0.0 },
                reference: self.fill_reference(k, sym, p),
            };
            result.costs.commissions += commission;
            result.costs.turnover += q.abs() * p * m;
            self.requested_total += q.abs();
            self.filled_total += q.abs();
            self.limit_state.traded.push_back((tn, q.abs() * p * m));
            self.record(result, RunEvent::new(tn, Some(sym), EventKind::Fill { fill, remainder: None }).rule(rule).order(id).cash(-flow));
            k.last_price.insert(sym, p);
            k.stores[fill_rel].insert(tn, vec![Value::Equity(sym), Value::Time(tn), Value::Num(q), Value::Num(p)]);
        }
        Ok(false)
    }

    /// The class's kernel relations at decision bar `t` (K-2.4): `chain`
    /// for each of the program's chains, then `spec` and `greeks` for every
    /// contract in one or held. The Greeks it wrote, by contract.
    pub(super) fn write_option_relations(&mut self, k: &mut Kernel, t: i64) -> BTreeMap<Sym, [f64; 6]> {
        let mut written = BTreeMap::new();
        let (Some(&spec_rel), Some(&chain_rel), Some(&greeks_rel)) = (k.rel_ids.get("spec"), k.rel_ids.get("chain"), k.rel_ids.get("greeks")) else {
            return written;
        };
        let mut contracts: BTreeSet<Sym> = BTreeSet::new();
        for need in k.chain_needs.clone() {
            for a in k.chain_at(&need, t) {
                k.stores[chain_rel].insert(t, vec![Value::Equity(need.underlying), Value::Time(t), Value::Equity(a)]);
                contracts.insert(a);
            }
        }
        contracts.extend(self.held_options(k).into_iter().map(|(s, _)| s));
        let label = |k: &Kernel, w: &str| k.labels.get(w).map(Value::Label);
        for a in contracts {
            let Some(o) = k.option_terms(a).cloned() else { continue };
            let settlement = if o.cash { "cash" } else { "physical" };
            if let (Some(right), Some(style), Some(settlement)) = (label(k, right_word(o.right)), label(k, style_word(o.style)), label(k, settlement)) {
                k.stores[spec_rel].insert(
                    t,
                    vec![
                        Value::Equity(a),
                        Value::Time(t),
                        Value::Equity(o.underlying),
                        right,
                        Value::Num(o.strike),
                        Value::Time(o.expiry),
                        style,
                        settlement,
                        Value::Num(o.multiplier),
                    ],
                );
            }
            if let Some(g) = k.option_greeks(a, t) {
                let mut tu = vec![Value::Equity(a), Value::Time(t)];
                tu.extend(g.iter().map(|x| Value::Num(*x)));
                k.stores[greeks_rel].insert(t, tu);
                written.insert(a, g);
            }
        }
        written
    }
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}
