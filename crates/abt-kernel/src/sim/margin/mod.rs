//! The margin models of K-5.6 (docs/spec/02-kernel.md, with ERRATA E-4):
//! the book's initial and maintenance requirement at a barrier, per
//! instrument, with the model that priced it.
//!
//! Instruments are `u32` ids and times `i64` nanoseconds since the Unix
//! epoch; a year is 365 days. Quantities are signed (shares, contracts) and
//! divisible. No requirement is negative.
//!
//! # Selection
//!
//! The execution block's `margin` field lists models in order of preference
//! ([`Model`]). Each instrument held (a non-zero quantity; a repeated
//! instrument's quantities are summed) takes the first listed model that
//! applies to it:
//!
//! | model | applies to |
//! |---|---|
//! | `published` | an instrument with a [`PublishedRow`] |
//! | `reg_t` | a stock, or an option whose underlying has a price in [`Context::underlyings`]; never a future |
//! | `portfolio` | a stock, or such an option when [`Context::reprice`] values it at the underlying's price and at every point of its grid |
//! | `span` | an instrument with a [`RiskArray`] |
//! | `approx F` | every instrument |
//!
//! A holding whose quantity, mark or terms are not finite, or whose
//! multiplier or point value is not positive, fits no model. Each model
//! passed over is recorded with its reason: in [`Margin::fallbacks`] when a
//! later model priced the instrument (K-2.2 warns of the fall back), in
//! [`Margin::skipped`] when none did, and such an instrument has no row.
//!
//! `reg_t` and `portfolio` are computed per underlying (a stock is its own
//! underlying) and `span` per combined commodity, each over the instruments
//! assigned to it: offsets reach only instruments priced by the same model.
//! Every instrument priced has exactly one [`Requirement`] row with its
//! [`Source`], and the book's initial and maintenance are the rows' sums.
//!
//! # 1. `published`
//!
//! The binding's `margin` relation states the requirement per contract (or
//! per share): an instrument needs |qty| × the row's initial and maintenance
//! (K-5.6, model 1).
//!
//! # 2. `reg_t`
//!
//! The strategy-based rules, per underlying: Regulation T (12 CFR 220.12)
//! for initial margin, FINRA Rule 4210(c) for the maintenance of stock, and
//! FINRA Rule 4210(f)(2) with the strategies of Cboe Rule 10.3 (the Cboe and
//! OCC strategy-based margin rules) for options. Q = |qty|, m = the
//! multiplier, MV = the market value (Q × mark, and × m for an option), S =
//! the underlying's price, K = the strike; an option's premium is its mark.
//!
//! - **Long stock**: initial 50 % of MV, maintenance 25 % (4210(c)).
//! - **Short stock**: initial 50 % of MV (Regulation T's 150 % is the credit
//!   balance: the sale's proceeds plus 50 %). Maintenance at a price of $5
//!   or more: the greater of 30 % of MV and $5 a share; below $5, the greater
//!   of 100 % of MV and $2.50 a share (4210(c)).
//! - **Long option**: paid in full, 100 % of MV when it expires at most nine
//!   months (274 days) after the barrier, 75 % of MV beyond (4210(f)(2)).
//! - **Naked short option**: m × Q × (premium + max(r·S − OTM, floor)). A
//!   call's OTM is max(K − S, 0) and its floor 10 % of S; a put's OTM is
//!   max(S − K, 0) and its floor 10 % of K. r is 20 % on an equity or a
//!   narrow-based index and 15 % on a broad-based index, the floor staying
//!   10 % (4210(f)(2)(E); Cboe Rule 10.3).
//! - **Cash-secured put**: no reduction in a margin account. The cash is
//!   equity already, and beyond the premium the sale brought in the naked
//!   requirement is never above the strike: max(r·S − OTM, 10 % · K) < K at
//!   every S, r·S − OTM being r·S ≤ r·K at or below the strike and
//!   K − (1 − r)·S above it. Securing the put with its strike, as a cash
//!   account must, would never need less.
//! - **Spread**: a short paired with a long of the same underlying, right
//!   and multiplier, the long expiring at or after the short. One unit needs
//!   the pair's maximum loss from the current marks, m × max(0, long premium
//!   − short premium + width), width = max(0, K_long − K_short) for calls and
//!   max(0, K_short − K_long) for puts, in place of both the short's naked
//!   requirement and the long's payment. A debit spread so needs its net
//!   debit, a credit spread its width less the credit.
//! - **Straddle or strangle**: a short call with a short put of the same
//!   multiplier. One unit needs the greater of the two naked requirements
//!   plus m × the premium of the side with the smaller one.
//! - **Covered call**: a short call with m long shares a contract needs
//!   nothing; so does a **covered put**, a short put with m short shares.
//!   The stock keeps its own requirement.
//! - **Initial**: an option's equals its maintenance; stock has its own.
//!
//! The group's requirement is the minimum over admissible pairings (K-5.6),
//! a linear program since quantities are divisible ([`reg_t`] states it,
//! [`lp`] solves it). A pair's requirement is attributed to its short leg, a
//! straddle's to the leg with the larger naked requirement (the call on a
//! tie), and a long option keeps the payment of its unpaired rest. The style
//! of exercise plays no part.
//!
//! # 3. `portfolio`
//!
//! A TIMS-like scenario method (FINRA Rule 4210(g); the OCC's TIMS), per
//! underlying, with no offsets across underlyings. The underlying's price S
//! is shocked at eleven equally spaced points, ends included: −15 %..+15 %
//! for an equity, −8 %..+8 % for a broad-based index and −6 %..+8 % for a
//! narrow-based one, as K-5.6 states the grid. (FINRA 4210(g) itself states
//! +6 %/−8 % for high-capitalisation broad-based indices, ±10 % for other
//! broad-based ones and ±15 % for the rest; [`portfolio::range`] is the one
//! place to change.) A stock with no [`Underlying`] entry is an equity
//! priced at its mark.
//!
//! At a shock x the group's P&L is Σ qty × (value(S·(1 + x)) − value(S)) ×
//! w, with w = m for an option and 1 for stock: a share is worth the price,
//! an option what [`Context::reprice`] gives at its current implied
//! volatility, so the P&L is measured from the model's own value now. The
//! requirement is the worst loss over the grid (0 if none), floored at
//! $0.375 × m for each option contract held; initial = maintenance. It is
//! attributed in proportion to each instrument's loss at the worst point,
//! positive contributions only; when the floor binds or no contribution is
//! positive, in proportion to |contracts × m|, a stock weighted by its
//! |shares|.
//!
//! # 4. `span`
//!
//! CME SPAN (the Standard Portfolio Analysis of Risk), simplified, per
//! combined commodity CC over the instruments with risk arrays. A risk array
//! holds a contract's 16 scenario results (the loss of one long contract
//! positive), its delta and its short option minimum.
//!
//! - **Scan risk** = max(0, max over the scenarios k of Σ qty × array_k).
//! - **Intra-commodity spread charge** = min(Σ positive qty × delta, Σ
//!   |negative qty × delta|) × the CC's charge (0 when none is given): the
//!   inter-month risk the scan's perfect correlation misses.
//! - **Inter-commodity credits**, taken in the order listed (SPAN's priority
//!   order). When the two CCs' net deltas ND have opposite signs, spreads =
//!   min(|ND_a| / ratio_a, |ND_b| / ratio_b), and the credit rate × spreads ×
//!   (pr_a × ratio_a + pr_b × ratio_b) goes to each CC by its own term, pr
//!   being the CC's weighted futures price risk, scan risk / |ND| (0 at a
//!   net delta of 0, which a net delta within 1e-9 of the gross delta is).
//!   A credit consumes the deltas it spreads, so a delta earns one credit at
//!   most.
//! - **Maintenance** = max(scan + intra − credit, the short option minimum
//!   summed over the short option contracts, 0); **initial** = maintenance ×
//!   the CC's initial ratio (1 when none is given).
//!
//! A CC's requirement is attributed in proportion to each contract's loss at
//! the worst scenario, positive contributions only, else by |qty|.
//!
//! # 5. `approx F`
//!
//! F × the notional (K-5.6, model 5): F × |qty| × |price| × point value for a
//! future, F × |qty| × m × S for an option (the underlying's notional; the
//! strike stands in for S when the context has no price), F × |MV| for
//! stock. Initial = maintenance, and [`Margin::approx_used`] is set: the
//! kernel warns at every bar the model is used.
//!
//! # Liquidation
//!
//! A margin call liquidates in [`liquidation_order`]: the instruments priced,
//! by maintenance per unit held (per share or contract), largest first, ties
//! by id (K-5.6: "largest requirement first").

pub mod approx;
pub mod lp;
pub mod portfolio;
pub mod published;
pub mod reg_t;
pub mod span;

use std::collections::BTreeMap;

use crate::functions::{Right, Style};

/// A model of the execution block's `margin` list (K-2.2, K-5.6).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Model {
    Published,
    RegT,
    Portfolio,
    Span,
    /// `approx F`: the fraction `F` of the notional.
    Approx(f64),
}

impl Model {
    /// The source a requirement priced by this model carries.
    pub fn source(self) -> Source {
        match self {
            Model::Published => Source::Published,
            Model::RegT => Source::RegT,
            Model::Portfolio => Source::Portfolio,
            Model::Span => Source::Span,
            Model::Approx(_) => Source::Approx,
        }
    }
}

/// The model that priced a requirement: `margin_by`'s source column (E-4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Source {
    Published,
    RegT,
    Portfolio,
    Span,
    Approx,
}

impl Source {
    /// The name the source column holds.
    pub fn name(self) -> &'static str {
        match self {
            Source::Published => "published",
            Source::RegT => "reg_t",
            Source::Portfolio => "portfolio",
            Source::Span => "span",
            Source::Approx => "approx",
        }
    }
}

/// The class of an underlying (an index is an instrument too).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndexClass {
    Equity,
    BroadIndex,
    NarrowIndex,
}

/// What a unit of an instrument is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Terms {
    /// A share, or an index held as an instrument: a unit is worth the mark.
    Stock,
    /// A unit is worth the mark × the point value.
    Future { point_value: f64 },
    /// The mark is the premium per unit of the underlying.
    Option {
        underlying: u32,
        right: Right,
        strike: f64,
        expiry: i64,
        multiplier: f64,
        style: Style,
    },
}

/// A position of the book at the barrier.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Holding {
    pub instrument: u32,
    /// Signed: shares or contracts.
    pub qty: f64,
    pub mark: f64,
    pub terms: Terms,
}

/// An underlying's price and class at the barrier.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Underlying {
    pub id: u32,
    pub price: f64,
    pub class: IndexClass,
}

/// One instrument's requirement and the model that priced it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Requirement {
    pub instrument: u32,
    pub initial: f64,
    pub maintenance: f64,
    pub source: Source,
}

/// A row of the data's `margin` relation, per contract (or per share).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PublishedRow {
    pub initial: f64,
    pub maintenance: f64,
}

/// A contract's SPAN risk array, per contract.
#[derive(Clone, Debug, PartialEq)]
pub struct RiskArray {
    /// The combined commodity the contract belongs to.
    pub combined: String,
    /// The loss of one long contract in each scenario (a gain is negative).
    pub scenarios: [f64; 16],
    pub delta: f64,
    /// The minimum charge of one short option contract.
    pub short_option_min: f64,
}

/// The parameters of the SPAN file beyond the risk arrays, by combined
/// commodity.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SpanParams {
    /// The charge per intra-commodity (inter-month) spread.
    pub intra_spread_charge: BTreeMap<String, f64>,
    /// The inter-commodity spreads, in priority order.
    pub inter_credits: Vec<InterCredit>,
    /// Initial over maintenance (1 when absent).
    pub initial_ratio: BTreeMap<String, f64>,
}

/// An inter-commodity spread: `ratio_a` deltas of `a` against `ratio_b` of
/// `b`, credited at `rate`.
#[derive(Clone, Debug, PartialEq)]
pub struct InterCredit {
    pub a: String,
    pub b: String,
    pub ratio_a: f64,
    pub ratio_b: f64,
    pub rate: f64,
}

/// What the models read at a barrier.
#[derive(Clone, Copy)]
pub struct Context<'a> {
    pub t: i64,
    pub underlyings: &'a BTreeMap<u32, Underlying>,
    pub published: &'a BTreeMap<u32, PublishedRow>,
    pub risk_arrays: &'a BTreeMap<u32, RiskArray>,
    pub span: &'a SpanParams,
    /// An option's value per unit at a shocked underlying price, at its
    /// current implied volatility; None when the model cannot price it (no
    /// IV).
    pub reprice: &'a dyn Fn(&Holding, f64) -> Option<f64>,
}

/// The book's requirement: a row per instrument priced, and the totals.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Margin {
    /// In instrument order.
    pub rows: Vec<Requirement>,
    pub initial: f64,
    pub maintenance: f64,
    /// Some row is `approx`'s (K-5.6: warned at every bar).
    pub approx_used: bool,
    /// The instruments no listed model applies to, one entry per model with
    /// the reason it does not apply (with no model listed, one entry under
    /// `approx`); they have no row.
    pub skipped: Vec<(u32, Source, String)>,
    /// The models passed over for instruments a later model priced, with
    /// the reason (K-2.2: the fall back is warned).
    pub fallbacks: Vec<(u32, Source, String)>,
}

/// The book's requirement at `ctx.t` under `models`, in order of preference.
pub fn requirement(models: &[Model], holdings: &[Holding], ctx: &Context) -> Margin {
    let mut chosen = Chosen::default();
    let (mut skipped, mut fallbacks) = (Vec::new(), Vec::new());
    for h in held(holdings) {
        let mut passed = Vec::new();
        let choice = models.iter().find_map(|&model| match choose(model, &h, ctx) {
            Ok(choice) => Some(choice),
            Err(why) => {
                passed.push((h.instrument, model.source(), why));
                None
            }
        });
        match choice {
            Some(choice) => {
                fallbacks.append(&mut passed);
                chosen.add(h, choice);
            }
            None if models.is_empty() => skipped.push((h.instrument, Source::Approx, "no margin model is listed".to_string())),
            None => skipped.append(&mut passed),
        }
    }
    let mut rows = chosen.rows(ctx);
    rows.sort_by_key(|r| r.instrument);
    Margin {
        initial: rows.iter().map(|r| r.initial).sum(),
        maintenance: rows.iter().map(|r| r.maintenance).sum(),
        approx_used: rows.iter().any(|r| r.source == Source::Approx),
        rows,
        skipped,
        fallbacks,
    }
}

/// The order a margin call liquidates in: instruments by maintenance per
/// unit held, largest first (ties by id).
pub fn liquidation_order(margin: &Margin, holdings: &[Holding]) -> Vec<(u32, f64)> {
    let held: BTreeMap<u32, f64> = held(holdings).iter().map(|h| (h.instrument, h.qty.abs())).collect();
    let mut order: Vec<(u32, f64)> = margin
        .rows
        .iter()
        .filter_map(|r| {
            let units = *held.get(&r.instrument)?;
            (units > 0.0 && units.is_finite()).then_some((r.instrument, r.maintenance / units))
        })
        .collect();
    order.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    order
}

/// The instruments held, one holding each in id order: a repeated
/// instrument's quantities summed (the first's mark and terms kept), flat
/// ones dropped.
fn held(holdings: &[Holding]) -> Vec<Holding> {
    let mut by_id: BTreeMap<u32, Holding> = BTreeMap::new();
    for h in holdings {
        by_id.entry(h.instrument).and_modify(|first| first.qty += h.qty).or_insert(*h);
    }
    by_id.into_values().filter(|h| h.qty != 0.0).collect()
}

/// What the selection found for a holding: its model and what that needs.
enum Choice {
    Published(PublishedRow),
    RegT,
    Portfolio(Box<portfolio::Leg>),
    Span,
    Approx(f64),
}

/// The holdings each model prices.
#[derive(Default)]
struct Chosen {
    published: Vec<(Holding, PublishedRow)>,
    reg_t: Vec<Holding>,
    portfolio: Vec<portfolio::Leg>,
    span: Vec<Holding>,
    approx: Vec<(Holding, f64)>,
}

impl Chosen {
    fn add(&mut self, h: Holding, choice: Choice) {
        match choice {
            Choice::Published(row) => self.published.push((h, row)),
            Choice::RegT => self.reg_t.push(h),
            Choice::Portfolio(leg) => self.portfolio.push(*leg),
            Choice::Span => self.span.push(h),
            Choice::Approx(f) => self.approx.push((h, f)),
        }
    }

    fn rows(&self, ctx: &Context) -> Vec<Requirement> {
        let mut rows = published::rows(&self.published);
        rows.extend(reg_t::rows(&self.reg_t, ctx));
        rows.extend(portfolio::rows(&self.portfolio));
        rows.extend(span::rows(&self.span, ctx));
        rows.extend(approx::rows(&self.approx, ctx));
        rows
    }
}

/// The model's verdict on a holding: what it needs to price it, or why it
/// does not apply.
fn choose(model: Model, h: &Holding, ctx: &Context) -> Result<Choice, String> {
    valid(h)?;
    match model {
        Model::Published => match ctx.published.get(&h.instrument) {
            Some(row) if row.initial.is_finite() && row.maintenance.is_finite() => Ok(Choice::Published(*row)),
            Some(_) => Err("the published row is not finite".to_string()),
            None => Err("no published row".to_string()),
        },
        Model::RegT => match h.terms {
            Terms::Stock => Ok(Choice::RegT),
            Terms::Future { .. } => Err("a future has no reg_t rule".to_string()),
            Terms::Option { underlying, .. } => priced(ctx, underlying).map(|_| Choice::RegT),
        },
        Model::Portfolio => portfolio::leg(h, ctx).map(|leg| Choice::Portfolio(Box::new(leg))),
        Model::Span => match ctx.risk_arrays.get(&h.instrument) {
            Some(a) if a.scenarios.iter().chain([&a.delta, &a.short_option_min]).all(|x| x.is_finite()) => Ok(Choice::Span),
            Some(_) => Err("the risk array is not finite".to_string()),
            None => Err("no risk array".to_string()),
        },
        Model::Approx(f) => Ok(Choice::Approx(f)),
    }
}

/// A holding every model can read: finite numbers, a positive multiplier
/// or point value.
fn valid(h: &Holding) -> Result<(), String> {
    let terms = match h.terms {
        Terms::Stock => true,
        Terms::Future { point_value } => point_value.is_finite() && point_value > 0.0,
        Terms::Option { strike, multiplier, .. } => strike.is_finite() && multiplier.is_finite() && multiplier > 0.0,
    };
    if !h.qty.is_finite() || !h.mark.is_finite() {
        Err("the quantity or the mark is not finite".to_string())
    } else if !terms {
        Err("the terms are not finite and positive".to_string())
    } else {
        Ok(())
    }
}

/// An underlying with a finite price.
fn priced<'a>(ctx: &Context<'a>, id: u32) -> Result<&'a Underlying, String> {
    ctx.underlyings.get(&id).filter(|u| u.price.is_finite()).ok_or_else(|| format!("the underlying {id} has no price"))
}

/// The underlying an instrument is grouped under: an option's, or the
/// instrument itself.
fn underlying_of(h: &Holding) -> u32 {
    match h.terms {
        Terms::Option { underlying, .. } => underlying,
        _ => h.instrument,
    }
}

/// The first index of the greatest value, with the value.
fn first_max(values: &[f64]) -> (usize, f64) {
    values.iter().enumerate().fold((0, f64::NEG_INFINITY), |best, (i, &v)| if v > best.1 { (i, v) } else { best })
}

/// `total` split in proportion to `weights` (evenly when they sum to
/// nothing).
fn apportion(total: f64, weights: &[f64]) -> Vec<f64> {
    let sum: f64 = weights.iter().sum();
    if sum > 0.0 && sum.is_finite() {
        weights.iter().map(|w| total * w / sum).collect()
    } else {
        vec![total / weights.len().max(1) as f64; weights.len()]
    }
}
