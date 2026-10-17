//! The book's margin requirement at a decision bar by the execution
//! block's models (K-5.6, `margin published, reg_t, ...`): the holdings in
//! the margin module's terms, the underlyings of the options, the binding's
//! published margins, and an option repriced at the volatility its mark
//! implies. Its rows are `margin_by`, its totals `margin_req` and the book
//! record's, and a margin call below its maintenance halts, is allowed, or
//! liquidates the smallest set of contracts that restores it, the largest
//! maintenance per unit first.

use super::margin::{self as m, Context, Holding, IndexClass, Margin, Model, PublishedRow, SpanParams, Terms, Underlying};
use super::*;

impl SimExecutor {
    /// The execution block's margin models, in order of preference.
    pub(super) fn margin_models(&self) -> Vec<Model> {
        self.cfg
            .margin_models
            .iter()
            .map(|x| match x {
                crate::check::MarginModel::Published => Model::Published,
                crate::check::MarginModel::RegT => Model::RegT,
                crate::check::MarginModel::Portfolio => Model::Portfolio,
                crate::check::MarginModel::Span => Model::Span,
                crate::check::MarginModel::Approx(f) => Model::Approx(*f),
            })
            .collect()
    }

    /// The book's holdings at `t` as the models see them (an instrument
    /// without a mark has none), and its options' underlyings: an index's
    /// when the option settles in cash (a broad-based index, ERRATA E-72),
    /// an equity's otherwise.
    fn margin_holdings(&self, k: &mut Kernel, t: i64) -> (Vec<Holding>, BTreeMap<u32, Underlying>) {
        let mut holdings = Vec::new();
        let mut underlyings = BTreeMap::new();
        let held: Vec<(Sym, f64)> = self.book.positions.iter().map(|(s, q)| (*s, *q)).collect();
        for (sym, q) in held {
            let Some(mark) = k.price_at(sym, t) else { continue };
            let terms = if let Some(o) = k.option_terms(sym).cloned() {
                if let Some(s) = k.price_at(o.underlying, t) {
                    let class = if o.cash { IndexClass::BroadIndex } else { IndexClass::Equity };
                    underlyings.insert(o.underlying, Underlying { id: o.underlying, price: s, class });
                }
                Terms::Option {
                    underlying: o.underlying,
                    right: o.right,
                    strike: o.strike,
                    expiry: o.expires_at(),
                    multiplier: o.multiplier,
                    style: o.style,
                }
            } else if k.is_future(sym) {
                Terms::Future { point_value: k.multiplier(sym) }
            } else {
                Terms::Stock
            };
            holdings.push(Holding { instrument: sym, qty: q, mark, terms });
        }
        (holdings, underlyings)
    }

    /// The binding's published margins at `t` (K-5.6, model 1): the
    /// `margin` relation's row of each instrument at the bar, per contract.
    fn published_margins(&self, k: &mut Kernel, t: i64) -> BTreeMap<u32, PublishedRow> {
        let mut out = BTreeMap::new();
        let Some(&id) = k.rel_ids.get("margin") else { return out };
        if !k.rels[id].stored {
            return out;
        }
        let Some(&entity_pos) = k.rels[id].entity_positions.first() else { return out };
        let key_pos = k.rels[id].key_pos;
        let values: Vec<usize> = (0..k.prog.relations["margin"].args.len()).filter(|&i| i != entity_pos && i != key_pos).collect();
        let (lo, hi) = k.decision_bucket(t);
        for (_, tus) in k.stores[id].by_time.range(lo..=hi) {
            for tu in tus {
                if let (Some(sym), Some(&i0), Some(&i1)) = (tu[entity_pos].as_equity(), values.first(), values.get(1)) {
                    if let (Some(initial), Some(maintenance)) = (tu[i0].as_f64(), tu[i1].as_f64()) {
                        if initial.is_finite() && maintenance.is_finite() {
                            out.insert(sym, PublishedRow { initial, maintenance });
                        }
                    }
                }
            }
        }
        out
    }

    /// The book's requirement at `t` under the listed models, and the
    /// holdings it was computed over.
    pub(super) fn requirement(&self, k: &mut Kernel, t: i64) -> (Margin, Vec<Holding>) {
        let models = self.margin_models();
        let (holdings, underlyings) = self.margin_holdings(k, t);
        let published = self.published_margins(k, t);
        // Each option at the volatility its mark implies now.
        let mut vols: BTreeMap<u32, (crate::kernel::OptionTerms, f64)> = BTreeMap::new();
        for h in &holdings {
            if let Terms::Option { underlying, .. } = h.terms {
                let (Some(o), Some(u)) = (k.option_terms(h.instrument).cloned(), underlyings.get(&underlying)) else {
                    continue;
                };
                if let Some(v) = k.implied(&o, u.price, t, h.mark) {
                    vols.insert(h.instrument, (o, v));
                }
            }
        }
        let kr: &Kernel = k;
        let reprice = |h: &Holding, s: f64| -> Option<f64> {
            let (o, v) = vols.get(&h.instrument)?;
            let p = kr.model_price(o, s, t, *v);
            p.is_finite().then_some(p)
        };
        let (risk_arrays, span) = (BTreeMap::new(), SpanParams::default());
        let ctx = Context {
            t,
            underlyings: &underlyings,
            published: &published,
            risk_arrays: &risk_arrays,
            span: &span,
            reprice: &reprice,
        };
        (m::requirement(&models, &holdings, &ctx), holdings)
    }

    /// `margin_req` and `margin_by` at `t` (K-2.4, E-4), and what the run's
    /// warnings count: the bars `approx` priced, the models passed over.
    pub(super) fn write_requirement(&mut self, k: &mut Kernel, t: i64, initial: f64, maintenance: f64, margin: Option<&Margin>) {
        if let Some(&rel) = k.rel_ids.get("margin_req") {
            k.stores[rel].insert(t, vec![Value::Time(t), Value::Num(initial), Value::Num(maintenance)]);
        }
        let Some(margin) = margin else { return };
        if let Some(&rel) = k.rel_ids.get("margin_by") {
            for r in &margin.rows {
                let Some(source) = k.labels.get(r.source.name()).map(Value::Label) else { continue };
                k.stores[rel].insert(t, vec![Value::Equity(r.instrument), Value::Time(t), Value::Num(r.initial), Value::Num(r.maintenance), source]);
            }
        }
        if margin.approx_used {
            self.approx_bars += 1;
        }
        for (sym, source, why) in margin.fallbacks.iter().chain(&margin.skipped) {
            let note = format!("{} for {}: {}", source.name(), k.symbols.name(*sym), why);
            if !self.margin_notes.contains(&note) && self.margin_notes.len() < 20 {
                self.margin_notes.push(note);
            }
        }
    }

    /// A margin call under the models (K-5.6): the equity below the
    /// maintenance. `liquidate` sells, at the bar's marks, whole units of
    /// the instruments in the order of their maintenance per unit, largest
    /// first, until the maintenance the book keeps is covered.
    pub(super) fn margin_call_by_models(&mut self, k: &mut Kernel, t: i64, equity: f64, margin: &Margin, holdings: &[Holding], result: &mut RunResult) -> Result<bool, RunError> {
        let tol = 1e-9 * (1.0 + margin.maintenance.abs());
        if margin.maintenance <= 0.0 || equity >= margin.maintenance - tol {
            return Ok(false);
        }
        let policy = match self.cfg.on_margin_call {
            OnMarginCall::Halt => "halt",
            OnMarginCall::Allow => "allow",
            OnMarginCall::Liquidate => "liquidate",
        };
        let call = EventKind::MarginCall {
            equity,
            requirement: margin.maintenance,
            policy: policy.into(),
        };
        self.record(result, RunEvent::new(t, None, call));
        match self.cfg.on_margin_call {
            OnMarginCall::Halt => Err(RunError::Risk {
                t,
                rule: "(executor)".into(),
                decision: "(mark to market)".into(),
                message: format!(
                    "margin call: equity {:.2} is below the maintenance {:.2} the margin models require at {}",
                    equity,
                    margin.maintenance,
                    time::format_timestamp(t)
                ),
            }),
            OnMarginCall::Allow => {
                self.margin_calls += 1;
                Ok(false)
            }
            OnMarginCall::Liquidate => {
                self.margin_calls += 1;
                let fill_rel = k.rel_id("fill");
                let mut short = margin.maintenance - equity;
                let mut fills = Vec::new();
                for (inst, per_unit) in m::liquidation_order(margin, holdings) {
                    if short <= tol {
                        break;
                    }
                    if per_unit <= 0.0 {
                        continue;
                    }
                    let sym = inst as Sym;
                    let pos = self.book.position(sym);
                    let Some(p) = k.price_at(sym, t) else { continue };
                    let mut units = (short / per_unit).min(pos.abs());
                    if self.cfg.lot == Lot::Whole {
                        units = units.ceil().min(pos.abs());
                    }
                    if units <= 0.0 {
                        continue;
                    }
                    let qty = -pos.signum() * units;
                    let commission = self.commission(k, sym, qty, p);
                    let flow = self.traded_cash(k, sym, qty, p) + commission;
                    let m = k.multiplier(sym);
                    result.costs.commissions += commission;
                    result.costs.turnover += qty.abs() * p * m;
                    k.stores[fill_rel].insert(t, vec![Value::Equity(sym), Value::Time(t), Value::Num(qty), Value::Num(p)]);
                    let fill = FillRecord {
                        t,
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
                        forced: true,
                        spread: 0.0,
                        reference: self.fill_reference(k, sym, p),
                    };
                    fills.push(result.events.len() as u64);
                    self.record(result, RunEvent::new(t, Some(sym), EventKind::Fill { fill, remainder: None }).cash(-flow));
                    short -= units * per_unit - commission;
                }
                let e = EventKind::ForcedLiquidation {
                    requirement: margin.maintenance,
                    equity,
                    fills,
                };
                self.record(result, RunEvent::new(t, None, e));
                Ok(true)
            }
        }
    }
}
