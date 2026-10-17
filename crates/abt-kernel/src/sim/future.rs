//! The futures model (K-5.4). A program trades a root through its
//! continuous series, or a contract itself. Under `settlement none` a
//! future is booked as a fully paid position, as R8L's book always was.
//! Under `settlement daily` it is margined: a fill moves only its costs and
//! the variation of the units it closes against the position's reference,
//! each session's close moves the variation since the reference to cash at
//! the settlement price and makes that price the reference, and the NAV
//! counts the position against its reference (E-10). A roll of the
//! continuous series records the two contracts' fills and commissions; a
//! contract held at its last trade is settled and closed.

use super::*;

impl Kernel<'_> {
    /// The settlement price of `sym` at decision bar `t`, from the binding's
    /// `settlement` relation when it holds one.
    pub fn settlement_of(&mut self, sym: Sym, t: i64) -> Option<f64> {
        let id = *self.rel_ids.get("settlement")?;
        if !self.rels[id].stored {
            return None;
        }
        let entity_pos = *self.rels[id].entity_positions.first()?;
        let key_pos = self.rels[id].key_pos;
        let col = (0..self.prog.relations["settlement"].args.len()).find(|&i| i != entity_pos && i != key_pos)?;
        let key = Value::Equity(sym);
        let (lo, hi) = self.decision_bucket(t);
        // A price the source does not carry (NaN, E-71) is none.
        let price = if self.rels[id].cal == self.prog.decision_cal {
            self.stores[id].lookup(t, Some(&key)).iter().find(|tu| tu[entity_pos] == key).and_then(|tu| tu[col].as_f64())
        } else {
            self.stores[id]
                .by_time
                .range(lo..=hi)
                .rev()
                .find_map(|(_, tus)| tus.iter().find(|tu| tu[entity_pos] == key).and_then(|tu| tu[col].as_f64()))
        };
        price.filter(|p| p.is_finite())
    }

    /// The contract a root's continuous series holds at `t`, received by the
    /// barrier, with the series' adjustment: its entry in `continuous`.
    fn roll_at(&self, root: Sym, t: i64) -> Option<(usize, &crate::kernel::bind::Roll)> {
        let rolls = self.rolls.get(&root)?;
        let now = self.now;
        rolls.iter().enumerate().rev().find(|(_, r)| r.t <= t && r.r <= now)
    }
}

impl SimExecutor {
    /// A future margined daily (`settlement daily`), valued against its
    /// reference.
    pub(super) fn settled_daily(&self, k: &Kernel, sym: Sym) -> bool {
        self.cfg.settlement_daily && k.is_future(sym)
    }

    /// What `q` units of `sym` at `p` add to the NAV: the notional, or for a
    /// future margined daily, the variation against `reference` (E-10).
    pub(super) fn value_at(&self, k: &Kernel, sym: Sym, q: f64, p: f64, reference: Option<f64>) -> f64 {
        match reference {
            Some(r) if self.settled_daily(k, sym) => q * (p - r) * k.multiplier(sym),
            _ => q * p * k.multiplier(sym),
        }
    }

    /// The book's value of its position in `sym` at `p` (K-5.1).
    pub(super) fn position_value(&self, k: &Kernel, sym: Sym, q: f64, p: f64) -> f64 {
        self.value_at(k, sym, q, p, self.book.reference.get(&sym).copied())
    }

    /// The reference a future margined daily is valued against after a
    /// fill at `price`: the position's, or the fill's when it opens one.
    pub(super) fn fill_reference(&self, k: &Kernel, sym: Sym, price: f64) -> Option<f64> {
        if !self.settled_daily(k, sym) {
            return None;
        }
        Some(self.book.reference.get(&sym).copied().filter(|_| self.book.positions.contains_key(&sym)).unwrap_or(price))
    }

    /// The cash a fill of `qty` at `price` moves before its costs: the
    /// notional, or for a future margined daily, the variation of the units
    /// against the reference.
    pub(super) fn traded_cash(&self, k: &Kernel, sym: Sym, qty: f64, price: f64) -> f64 {
        match self.fill_reference(k, sym, price) {
            Some(r) => qty * (price - r) * k.multiplier(sym),
            None => qty * price * k.multiplier(sym),
        }
    }

    /// Each session's settlement of the futures margined daily (K-5.4), at
    /// the bar that closes the session, before the marks: the variation
    /// `Q · (S − reference) · point value` to cash at the settlement price
    /// (the close where the binding has none, counted for the warning).
    pub(super) fn settlements(&mut self, k: &mut Kernel, t: i64, result: &mut RunResult) -> bool {
        if !self.cfg.settlement_daily {
            return false;
        }
        let held: Vec<(Sym, f64)> = self.book.positions.iter().filter(|(s, _)| k.is_future(**s)).map(|(s, q)| (*s, *q)).collect();
        let mut changed = false;
        for (sym, q) in held {
            // A daily bar closes its session; an intraday one when the
            // barrier has passed its session's end.
            let session = match k.session_close(sym, t) {
                None => time::floor_div(t, time::DAY) * time::DAY,
                Some(end) if k.now >= end => end,
                Some(_) => continue,
            };
            let (price, at_close) = match k.settlement_of(sym, t) {
                Some(s) => (s, false),
                None => match k.bar_price(sym, t) {
                    Some(c) => (c, true),
                    None => continue,
                },
            };
            if at_close {
                self.settled_at_close += 1;
            }
            let r = self.book.reference.get(&sym).copied().unwrap_or(price);
            let variation = q * (price - r) * k.multiplier(sym);
            let e = EventKind::Settlement { price, variation, session, at_close };
            self.record(result, RunEvent::new(t, Some(sym), e).cash(variation));
            changed = true;
        }
        changed
    }

    /// A root's continuous series changing contract at `t` with a position
    /// held (K-5.4): a `roll` with the old contract's fill and the new
    /// one's, a commission each, the position carried on the root. Both are
    /// at the series' last price before the bar, where the adapter measures
    /// the gap between the two: the old contract less its adjustment, the
    /// new one less its own. The bar's P&L over the two contracts is then
    /// the series'; the change of adjustment is not P&L.
    pub(super) fn rolls(&mut self, k: &mut Kernel, t: i64, result: &mut RunResult) -> bool {
        let held: Vec<(Sym, f64)> = self.book.positions.iter().filter(|(s, _)| k.rolls.contains_key(*s)).map(|(s, q)| (*s, *q)).collect();
        let mut changed = false;
        for (root, q) in held {
            let Some((i, now)) = k.roll_at(root, t).map(|(i, r)| (i, r.clone())) else { continue };
            if now.t != t || i == 0 {
                continue;
            }
            let before = k.rolls[&root][i - 1].clone();
            if before.contract == now.contract {
                continue;
            }
            let prev_bar = self.bars.iter().rev().copied().find(|b| *b < t);
            let (Some(series_now), series_before) = (k.price_at(root, t), prev_bar.and_then(|b| k.price_at(root, b))) else {
                continue;
            };
            let at = series_before.unwrap_or(series_now);
            let (from_price, to_price) = (at - before.adjustment, at - now.adjustment);
            // Each fill at the root's commission per contract.
            let commissions = self.commission(k, root, q, from_price) + self.commission(k, root, q, to_price);
            result.costs.commissions += commissions;
            let e = EventKind::Roll {
                from: before.contract,
                to: now.contract,
                quantity: q,
                from_price,
                to_price,
                adjustment: now.adjustment - before.adjustment,
                commissions,
            };
            self.record(result, RunEvent::new(t, Some(root), e).cash(-commissions));
            changed = true;
        }
        changed
    }

    /// A futures contract held at its last trade (K-5.4, the contract
    /// traded itself, not through a continuous series): settled at the
    /// settlement price, else its close, and closed; a physical contract is
    /// not delivered, warned.
    pub(super) fn contract_expiries(&mut self, k: &mut Kernel, t: i64, result: &mut RunResult) -> bool {
        let held: Vec<(Sym, f64)> = self.book.positions.iter().map(|(s, q)| (*s, *q)).collect();
        let mut changed = false;
        for (sym, q) in held {
            let Some(spec) = k.contracts.get(&sym).and_then(|c| c.future_spec.clone()) else { continue };
            let last = spec.last_trade.unwrap_or(spec.expiry);
            if time::day_key(t) < time::day_key(last) {
                continue;
            }
            let Some(price) = k.settlement_of(sym, t).or_else(|| k.price_at(sym, t)) else { continue };
            // The position closed at the price: its value, or under daily
            // settlement the variation since the reference, to cash.
            let flow = -self.traded_cash(k, sym, -q, price);
            if !spec.cash {
                self.physical_expiries += 1;
            }
            let e = EventKind::ContractExpiry {
                quantity: q,
                price,
                cash_settled: spec.cash,
            };
            self.record(result, RunEvent::new(t, Some(sym), e).cash(flow));
            changed = true;
        }
        changed
    }

    /// The future class's `contract` relation at `t` (K-2.4): for every root
    /// with a continuous series, the contract it holds, its expiry and its
    /// point value.
    pub(super) fn write_contract_relation(&self, k: &mut Kernel, t: i64) {
        let Some(&rel) = k.rel_ids.get("contract") else { return };
        let roots: Vec<Sym> = k.rolls.keys().copied().collect();
        for root in roots {
            let Some(c) = k.roll_at(root, t).map(|(_, r)| r.contract) else { continue };
            let (expiry, pv) = match k.contracts.get(&c).and_then(|x| x.future_spec.as_ref()) {
                Some(s) => (s.expiry, s.point_value),
                None => continue,
            };
            k.stores[rel].insert(t, vec![Value::Equity(c), Value::Time(t), Value::Equity(root), Value::Time(expiry), Value::Num(pv)]);
        }
    }
}
