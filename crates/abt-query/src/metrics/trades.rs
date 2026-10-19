//! The rows the red flags read (Q-3.1, ERRATA E-95; the pipeline's
//! guideline sections 12 and 13): a run's return and drawdown on its fixed
//! base, its round trips and their statistics, where its profit comes from
//! by entity and by calendar year, and how much of it was made before 1990.
//!
//! Conventions, beside the module's:
//!
//! - The base is the report block's fixed capital, or the first NAV of the
//!   bars kept when returns compound. `return` is the mean bar P&L over the
//!   base times the periods a year; the drawdown on the base is the deepest
//!   fall of the cumulative P&L from its peak, over the base; MAR is the
//!   one over the other.
//! - A bar's P&L is its NAV less the previous bar's, for every bar kept (a
//!   mask keeps a bar's own P&L whether or not the bar before it is kept,
//!   as `masked_curve` keeps its return).
//! - A trade is a round trip of the fills kept: a security's position from
//!   flat back to flat, its P&L the cash flows net of commissions and fees
//!   (`convention_metrics`'s), closed at its last fill's time and
//!   attributed to the security. A position still open at the end is no
//!   trade. A trade with a P&L of exactly zero is neither a win nor a loss.
//! - The round-turn cost per contract or per share is twice the fills'
//!   commissions and fees over the quantity they filled.
//! - A share of the net profit is undefined (NaN) when the net profit is
//!   not positive; a ratio over nothing is NaN; a largest loss among no
//!   losing trade is 0.

use std::collections::BTreeMap;

use crate::run::Run;
use crate::time;

/// A security's share of a run's profit.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Contribution {
    /// The data layer's id of the instrument (none for a fill without one).
    pub entity: Option<u64>,
    /// The net P&L of its trades.
    pub net_profit: f64,
    /// Its share of the run's net profit over every trade (NaN when that is
    /// not positive).
    pub share: f64,
    pub trades: usize,
    /// The share of its trades that won.
    pub hit_rate: f64,
}

/// A calendar year's row of the sub-period table.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct YearRow {
    pub year: i64,
    /// The net P&L of the year's bars.
    pub net_profit: f64,
    /// The trades closed in the year.
    pub trades: usize,
}

/// The fixed-base and round-trip metrics of a run (E-95).
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TradeMetrics {
    /// The fixed base the returns are over.
    pub base: f64,
    /// The bars' P&L, summed.
    pub net_profit: f64,
    /// The mean bar P&L over the base, times the periods a year (`return`).
    pub annual_return: f64,
    /// The deepest fall of the cumulative P&L from its peak, over the base.
    pub max_drawdown_base: f64,
    /// `annual_return` over `max_drawdown_base` (NaN without a drawdown).
    pub mar: f64,
    pub trades: usize,
    /// The winning trades' P&L over the losing trades' (∞ without a loss,
    /// NaN without a trade).
    pub profit_factor: f64,
    /// The share of trades that won (0 without a trade).
    pub hit_rate: f64,
    /// The mean winning trade over the mean losing trade.
    pub avg_win_loss: f64,
    /// The mean P&L per trade.
    pub avg_trade: f64,
    /// Twice the fills' commissions and fees over the quantity filled.
    pub round_turn_cost: f64,
    /// `avg_trade` over `round_turn_cost`.
    pub avg_trade_to_cost: f64,
    /// The deepest losing trade, as a positive number.
    pub largest_loss: f64,
    /// The skewness of the trades' P&L.
    pub trade_skew: f64,
    /// The largest entity's share of the net profit over every trade.
    pub top_market_share: f64,
    /// The share of entities traded whose trades made a profit.
    pub profitable_market_share: f64,
    /// The share of the bars' net profit made before 1990-01-01.
    pub profit_before_1990_share: f64,
    /// Per entity, by id.
    pub by_entity: Vec<Contribution>,
    /// Per calendar year of the bars kept.
    pub by_year: Vec<YearRow>,
}

/// 1990-01-01, in nanoseconds since the epoch.
pub const YEAR_1990: i64 = 7305 * time::DAY;

fn mean(x: &[f64]) -> f64 {
    if x.is_empty() {
        f64::NAN
    } else {
        x.iter().sum::<f64>() / x.len() as f64
    }
}

fn share(part: f64, whole: f64) -> f64 {
    if whole > 0.0 {
        part / whole
    } else {
        f64::NAN
    }
}

/// A round trip: the security, its net P&L and when it closed.
struct Trade {
    entity: Option<u64>,
    pnl: f64,
    closed: i64,
}

/// The round trips among the fills kept, in the order they close.
fn round_trips(r: &Run, keep: &dyn Fn(i64) -> bool) -> Vec<Trade> {
    let mut open: BTreeMap<Option<u64>, (f64, f64)> = BTreeMap::new();
    let mut out = Vec::new();
    for f in r.fills.iter().filter(|f| keep(f.t)) {
        let (pos, flow) = open.entry(f.instrument).or_insert((0.0, 0.0));
        *pos += f.quantity;
        *flow += -f.quantity * f.price * f.multiplier - f.commission - f.fee;
        if pos.abs() < 1e-9 {
            out.push(Trade {
                entity: f.instrument,
                pnl: *flow,
                closed: f.t,
            });
            open.remove(&f.instrument);
        }
    }
    out
}

/// The metrics over the bars and fills `keep` admits, at `periods_per_year`
/// periods a year.
pub fn trade_metrics(r: &Run, periods_per_year: f64, keep: &dyn Fn(i64) -> bool) -> TradeMetrics {
    let mut m = TradeMetrics::default();
    // The bars' P&L, on the fixed base.
    let pnl: Vec<(i64, f64)> = r.book.windows(2).filter(|w| keep(w[1].t)).map(|w| (w[1].t, w[1].nav - w[0].nav)).collect();
    m.base = r.base_capital().or_else(|| r.book.iter().find(|b| keep(b.t)).map(|b| b.nav)).unwrap_or(0.0);
    m.net_profit = pnl.iter().map(|(_, p)| *p).sum();
    let (mut cum, mut peak, mut fall) = (0.0f64, 0.0f64, 0.0f64);
    for &(_, p) in &pnl {
        cum += p;
        peak = peak.max(cum);
        fall = fall.max(peak - cum);
    }
    if m.base > 0.0 && !pnl.is_empty() {
        m.annual_return = m.net_profit / pnl.len() as f64 / m.base * periods_per_year;
        m.max_drawdown_base = fall / m.base;
    }
    m.mar = if m.max_drawdown_base > 0.0 { m.annual_return / m.max_drawdown_base } else { f64::NAN };
    m.profit_before_1990_share = share(pnl.iter().filter(|(t, _)| *t < YEAR_1990).map(|(_, p)| *p).sum(), m.net_profit);
    // The years.
    for &(t, p) in &pnl {
        let y = time::month_key(t).0;
        match m.by_year.last_mut() {
            Some(row) if row.year == y => row.net_profit += p,
            _ => m.by_year.push(YearRow { year: y, net_profit: p, trades: 0 }),
        }
    }
    // The trades.
    let trades = round_trips(r, keep);
    let pnls: Vec<f64> = trades.iter().map(|t| t.pnl).collect();
    let wins: Vec<f64> = pnls.iter().copied().filter(|p| *p > 0.0).collect();
    let losses: Vec<f64> = pnls.iter().copied().filter(|p| *p < 0.0).collect();
    m.trades = trades.len();
    let (gain, loss) = (wins.iter().sum::<f64>(), -losses.iter().sum::<f64>());
    m.profit_factor = if trades.is_empty() {
        f64::NAN
    } else if loss > 0.0 {
        gain / loss
    } else {
        f64::INFINITY
    };
    m.hit_rate = if trades.is_empty() { 0.0 } else { wins.len() as f64 / trades.len() as f64 };
    m.avg_win_loss = if wins.is_empty() {
        f64::NAN
    } else if losses.is_empty() {
        f64::INFINITY
    } else {
        mean(&wins) / -mean(&losses)
    };
    m.avg_trade = mean(&pnls);
    let (fees, filled): (f64, f64) = r.fills.iter().filter(|f| keep(f.t)).fold((0.0, 0.0), |(c, q), f| (c + f.commission + f.fee, q + f.quantity.abs()));
    m.round_turn_cost = if filled > 0.0 { 2.0 * fees / filled } else { 0.0 };
    m.avg_trade_to_cost = if m.round_turn_cost > 0.0 { m.avg_trade / m.round_turn_cost } else { f64::NAN };
    m.largest_loss = -losses.iter().copied().fold(0.0, f64::min);
    m.trade_skew = if pnls.is_empty() { f64::NAN } else { super::skewness(&pnls) };
    for t in &trades {
        let y = time::month_key(t.closed).0;
        if let Some(row) = m.by_year.iter_mut().find(|row| row.year == y) {
            row.trades += 1;
        }
    }
    // The entities.
    let mut by: BTreeMap<Option<u64>, Contribution> = BTreeMap::new();
    for t in &trades {
        let c = by.entry(t.entity).or_insert_with(|| Contribution {
            entity: t.entity,
            ..Default::default()
        });
        c.net_profit += t.pnl;
        c.trades += 1;
        if t.pnl > 0.0 {
            c.hit_rate += 1.0;
        }
    }
    let total: f64 = by.values().map(|c| c.net_profit).sum();
    for c in by.values_mut() {
        c.hit_rate /= c.trades as f64;
        c.share = share(c.net_profit, total);
    }
    m.by_entity = by.into_values().collect();
    m.top_market_share = m.by_entity.iter().map(|c| c.share).fold(f64::NAN, f64::max);
    m.profitable_market_share = if m.by_entity.is_empty() {
        f64::NAN
    } else {
        m.by_entity.iter().filter(|c| c.net_profit > 0.0).count() as f64 / m.by_entity.len() as f64
    };
    m
}
