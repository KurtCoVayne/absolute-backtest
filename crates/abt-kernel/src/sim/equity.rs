//! The equity model's corporate actions and delistings (data-bundle doc,
//! section 4): a split multiplies the position at its ex-date, a dividend's
//! receivable is recorded at the ex-date and paid at the pay date, and a
//! delisted name leaves the book at its last price, less the haircut for its
//! reason.

use super::*;

impl SimExecutor {
    /// Corporate actions at the bar (data-bundle doc, section 4), once, before
    /// the bar's fills and its mark: a split multiplies the position at its
    /// ex-date, a dividend's receivable is recorded at the ex-date and paid
    /// at the pay date. Whether the book changed.
    pub(super) fn corporate_actions(&mut self, k: &mut Kernel, t: i64, result: &mut RunResult) -> bool {
        if self.actions_done == Some(t) {
            return false;
        }
        self.actions_done = Some(t);
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
                let Some(pos) = self.book.positions.get(&sym).copied() else { continue };
                if factor <= 0.0 || factor == 1.0 {
                    continue;
                }
                let exact = pos * factor;
                let kept = if self.cfg.lot == Lot::Whole { exact.trunc() } else { exact };
                let m = k.multiplier(sym);
                let fraction_cash = k.price_at(sym, t).map(|p| (exact - kept) * p * m).unwrap_or(0.0);
                let split = EventKind::Action {
                    action: Action::Split { factor },
                    position: Some(kept),
                    receivable: None,
                    price: None,
                };
                self.record(result, RunEvent::new(t, Some(sym), split).cash(fraction_cash));
                if let Some(lp) = k.last_price.get_mut(&sym) {
                    *lp /= factor;
                }
                book_changed = true;
            }
        }
        // Dividends going ex at t: a receivable for the shares held now, or
        // shares bought with it at the ex-date close.
        let mut reinvest: Vec<(Sym, f64, f64)> = Vec::new();
        let mut declared: Vec<(Sym, f64, f64, i64)> = Vec::new();
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
                                if let Some(&pos) = self.book.positions.get(&sym) {
                                    if self.cfg.reinvest_dividends {
                                        reinvest.push((sym, amount, pos));
                                    } else {
                                        declared.push((sym, amount, pos, pay.max(ex)));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        for (sym, amount, shares, pay) in declared {
            let receivable = shares * amount;
            self.record(result, RunEvent::new(t, Some(sym), EventKind::DividendDeclared { amount, shares, pay, receivable }));
        }
        for (sym, amount, shares) in reinvest {
            let Some(p) = k.price_at(sym, t).filter(|p| *p > 0.0) else { continue };
            let added = shares * amount / p;
            let reinvested = EventKind::Action {
                action: Action::Reinvest { amount, shares, added },
                position: Some(shares + added),
                receivable: None,
                price: Some(p),
            };
            self.record(result, RunEvent::new(t, Some(sym), reinvested));
            book_changed = true;
        }
        let due: Vec<Receivable> = self.book.receivables.iter().filter(|r| r.pay <= t).cloned().collect();
        for r in due {
            let paid = EventKind::Action {
                action: Action::Dividend { amount: r.amount, shares: r.shares },
                position: None,
                receivable: Some(r.id),
                price: None,
            };
            self.record(result, RunEvent::new(t, Some(r.instrument), paid).cash(r.cash));
            book_changed = true;
        }
        // The options the book holds on an underlying going ex (K-5.5.5).
        if k.has_options && !self.cfg.actions_in_prices {
            book_changed |= self.option_adjustments(k, t, result);
        }
        book_changed
    }

    /// Delistings at the bar, after its fills: a delisted name is
    /// force-closed at its last trade less the haircut for the reason.
    pub(super) fn delistings(&mut self, k: &mut Kernel, t: i64, result: &mut RunResult) -> bool {
        let fill_rel = k.rel_id("fill");
        let mut book_changed = false;
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
                let Some(pos) = self.book.positions.get(&sym).copied() else { continue };
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
                let proceeds = if self.settled_daily(k, sym) {
                    -self.traded_cash(k, sym, qty, price) - commission
                } else {
                    -qty * price * m - commission
                };
                result.costs.commissions += commission;
                result.costs.turnover += qty.abs() * price * m;
                k.stores[fill_rel].insert(t, vec![Value::Equity(sym), Value::Time(t), Value::Num(qty), Value::Num(price)]);
                // The forced close takes the position to zero; the action
                // carries the proceeds.
                let fill = FillRecord {
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
                    spread: 0.0,
                    reference: None,
                };
                self.record(result, RunEvent::new(t, Some(sym), EventKind::Fill { fill, remainder: None }));
                let delisted = EventKind::Action {
                    action: Action::Delisting { reason, haircut },
                    position: None,
                    receivable: None,
                    price: Some(price),
                };
                self.record(result, RunEvent::new(t, Some(sym), delisted).cash(proceeds));
                book_changed = true;
            }
        }
        book_changed
    }
}
