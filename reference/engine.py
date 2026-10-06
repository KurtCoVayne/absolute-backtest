"""The reference executor (data-bundle doc, section 8): the execution
contract of `docs/semantic-model.md` section 6, written in plain Python over
pandas frames, deliberately slow, sharing nothing with the kernel.

It replays the decisions of a kernel dump (`abt run --dump DIR`) through its
own book and produces fills, the book at every bar, dropped decisions,
actions and the final state, for `diff.py` to compare. Where the model
leaves an operational detail open (the order of orders within a bar, which
price a liquidation uses, when funding accrues), the choice taken here is
the one the model's text states, and a disagreement with the kernel is a
bug in one of the two with the model as referee.

Conventions of the contract, in the order the bar runs them:

1. At every bar T (the decision resolution's time domain, in order):
   the decisions of the previous bar are filled at T (step 2), then the
   bar is opened (step 3): corporate actions, mark to market, margin call,
   the book recorded; then the strategy decides at T.
2. Filling at T (every bar after the first, whether or not the previous
   bar decided anything): funding accrues over the calendar time since the
   previous bar (cash rate on positive cash, margin rate on a debit, borrow
   fee by ADV bucket and rebate on short notional); the book is marked at T;
   orders that reduce a position go first (symbol order within each
   group), decisions on an instrument supersede its open target, other
   open targets re-issue; ruin halts; each order is sized (a bought long
   at the price it will fill at), rounded by the lot, checked for crossing
   zero, capped by participation, priced at the bar's close or the last
   known price for a liquidation, moved by slippage and impact, charged
   commission and fee, checked for borrow and leverage (a bar's costs are
   never leverage), and applied.
3. Opening T: splits multiply the position at the ex-date (a whole-lot
   remainder is cashed at the price), dividends going ex are receivables
   paid at the pay date, a delisted name is force-closed at its last trade
   less the haircut for the reason; then the mark, the maintenance check
   (halt, liquidate pro rata to the level, or allow), and the NAV record.
4. A decision at the last bar has no bar to fill at and is dropped.
"""

from __future__ import annotations

import json
import math
import os
from dataclasses import dataclass, field

import pandas as pd

DAY = 86_400


def epoch(s: str) -> int:
    """Seconds since the epoch of a dump timestamp (UTC)."""
    return int(pd.Timestamp(s, tz="UTC").timestamp())


def stamp(t: int) -> str:
    """A dump timestamp of epoch seconds: the date, with a time when not midnight."""
    ts = pd.Timestamp(t, unit="s", tz="UTC")
    if t % DAY == 0:
        return ts.strftime("%Y-%m-%d")
    return ts.strftime("%Y-%m-%dT%H:%M:%S")


class Halt(Exception):
    """The run halted on a policy (ruin, leverage, oversize, margin call)."""


@dataclass
class Fill:
    t: int
    equity: str
    quantity: float
    price: float
    commission: float = 0.0
    fee: float = 0.0
    slippage: float = 0.0
    impact: float = 0.0
    participation: float = 0.0
    at_last_price: bool = False
    partial: bool = False
    forced: bool = False


@dataclass
class Config:
    raw: dict

    def __getattr__(self, name):
        try:
            return self.raw[name]
        except KeyError as e:
            raise AttributeError(name) from e

    @property
    def compounding(self) -> bool:
        return bool(self.raw.get("compounding", False))

    @property
    def lot_whole(self) -> bool:
        return str(self.raw.get("lot", "Whole")).lower() == "whole"

    def policy(self, name: str) -> str:
        return str(self.raw.get(name, "Halt")).lower()

    def haircut(self, reason: str) -> float:
        for r, x in self.raw.get("delisting_haircuts", []):
            if r == reason:
                return x
        return self.raw.get("delisting_haircut_default", 1.0)

    def borrow_bucket(self, adv):
        buckets = self.raw.get("borrow", [])
        last = buckets[-1] if buckets else {"adv_below": None, "fee_bps": 0.0, "shortable": True}
        if adv is None:
            return last
        for b in buckets:
            below = b.get("adv_below")
            if below is None or adv < below:
                return b
        return last


def _stamp(t) -> str:
    """A timestamp as the kernel writes one in text: the date, with the time
    of day when it is not midnight."""
    if t.hour == 0 and t.minute == 0 and t.second == 0:
        return t.strftime("%Y-%m-%d")
    return t.strftime("%Y-%m-%dT%H:%M:%S")


def read_rows(path: str) -> pd.DataFrame:
    """A Parquet table of the dump as text cells: timestamps formatted as the
    kernel formats them, booleans as `true`/`false`, nulls as empty."""
    if not os.path.exists(path):
        return pd.DataFrame()
    frame = pd.read_parquet(path)
    out = pd.DataFrame(index=frame.index)
    for col in frame.columns:
        series = frame[col]
        if pd.api.types.is_datetime64_any_dtype(series):
            out[col] = [("" if pd.isna(v) else _stamp(v)) for v in series]
        elif pd.api.types.is_bool_dtype(series):
            out[col] = ["true" if v else "false" for v in series]
        else:
            out[col] = ["" if (v is None or (isinstance(v, float) and math.isnan(v))) else repr(v) if isinstance(v, float) else str(v) for v in series]
    return out


_rows = read_rows


@dataclass
class Data:
    """The dataset of a dump: prices and volumes by (symbol, bar), the
    actions by bar."""

    price: dict = field(default_factory=dict)  # (sym, t) -> price
    volume: dict = field(default_factory=dict)  # (sym, t) -> volume
    splits: dict = field(default_factory=dict)  # t -> [(sym, factor)]
    dividends: list = field(default_factory=list)  # (announce, sym, ex, pay, amount)
    delisted: dict = field(default_factory=dict)  # t -> [(sym, reason)]

    @staticmethod
    def load(directory: str, cfg: Config) -> "Data":
        d = Data()
        data_dir = os.path.join(directory, "data")

        def numeric_last(frame: pd.DataFrame):
            # The value column of a `close(A, T, P)`-like relation is its last column.
            return frame.columns[-1]

        if cfg.price_relation:
            frame = _rows(os.path.join(data_dir, f"{cfg.price_relation}.parquet"))
            if not frame.empty:
                col = numeric_last(frame)
                for _, r in frame.iterrows():
                    d.price[(r.iloc[0], epoch(r.iloc[1]))] = float(r[col])
        if cfg.volume_relation:
            frame = _rows(os.path.join(data_dir, f"{cfg.volume_relation}.parquet"))
            if not frame.empty:
                col = numeric_last(frame)
                for _, r in frame.iterrows():
                    d.volume[(r.iloc[0], epoch(r.iloc[1]))] = float(r[col])
        frame = _rows(os.path.join(data_dir, "split.parquet"))
        for _, r in frame.iterrows():
            d.splits.setdefault(epoch(r.iloc[1]), []).append((r.iloc[0], float(r.iloc[-1])))
        frame = _rows(os.path.join(data_dir, "dividend.parquet"))
        for _, r in frame.iterrows():
            # dividend(+A, @T, -Ex, -Pay, -Amount)
            d.dividends.append((epoch(r.iloc[1]), r.iloc[0], epoch(r.iloc[2]), epoch(r.iloc[3]), float(r.iloc[4])))
        frame = _rows(os.path.join(data_dir, "delisted.parquet"))
        for _, r in frame.iterrows():
            d.delisted.setdefault(epoch(r.iloc[1]), []).append((r.iloc[0], r.iloc[2] if len(r) > 2 else ""))
        return d


class Reference:
    """The reference book and executor."""

    def __init__(self, directory: str):
        with open(os.path.join(directory, "config.json")) as f:
            config = json.load(f)
        self.cfg = Config(config["exec"])
        self.cfg.raw["price_relation"] = config.get("price_relation")
        self.cfg.raw["volume_relation"] = config.get("volume_relation")
        self.symbols: list[str] = list(config["symbols"])
        self.order = {s: i for i, s in enumerate(self.symbols)}
        self.bars: list[int] = [epoch(b) for b in config["bars"]]
        self.data = Data.load(directory, self.cfg)
        decisions = _rows(os.path.join(directory, "decisions.parquet"))
        self.decisions: dict[int, list[tuple]] = {}
        for _, r in decisions.iterrows():
            self.decisions.setdefault(epoch(r["t"]), []).append((r["equity"], r["ctor"], float(r["amount"]), r["rule"]))
        # The book.
        self.cash = float(self.cfg.initial_cash)
        self.positions: dict[str, float] = {}
        self.last_price: dict[str, float] = {}
        self.open_targets: dict[str, tuple] = {}
        self.receivables: list[tuple] = []
        self.delisted_done: set[str] = set()
        self.seen: list[int] = []
        # The record.
        self.fills: list[Fill] = []
        self.nav: list[dict] = []
        self.dropped: list[tuple] = []
        self.actions: list[tuple] = []

    # ---- prices ----

    def bar_price(self, sym: str, t: int):
        p = self.data.price.get((sym, t))
        if p is not None:
            self.last_price[sym] = p
        return p

    def price_at(self, sym: str, t: int):
        p = self.bar_price(sym, t)
        if p is not None:
            return p
        return self.last_price.get(sym)

    def bar_volume(self, sym: str, t: int):
        return self.data.volume.get((sym, t))

    def adv(self, sym: str, bars: list[int], end: int):
        window = max(int(self.cfg.adv_window), 1)
        lo = max(0, end - (window - 1))
        vols = [v for v in (self.bar_volume(sym, t) for t in bars[lo : end + 1]) if v is not None]
        if not vols:
            return None
        return sum(vols) / len(vols)

    def realized_vol(self, sym: str, bars: list[int], end: int):
        if self.cfg.slippage_vol_mult == 0.0:
            return None
        lo = max(0, end - int(self.cfg.vol_window))
        rets = []
        prev = None
        for t in bars[lo : end + 1]:
            p = self.bar_price(sym, t)
            if prev is not None and p is not None:
                rets.append(math.log(p / prev))
            if p is not None:
                prev = p
        if len(rets) < max(int(self.cfg.vol_min_obs), 2):
            return None
        n = len(rets)
        mean = sum(rets) / n
        return math.sqrt(sum((r - mean) ** 2 for r in rets) / (n - 1))

    def commission(self, qty: float) -> float:
        per = float(self.cfg.commission_per_share)
        minimum = float(self.cfg.commission_min_per_order)
        if per == 0.0 and minimum == 0.0:
            return 0.0
        return max(per * abs(qty), minimum)

    def rnd(self, x: float) -> float:
        return math.trunc(x) if self.cfg.lot_whole else x

    def sorted_positions(self):
        return sorted(self.positions.items(), key=lambda kv: self.order[kv[0]])

    def set_position(self, sym: str, q: float):
        if abs(q) < 1e-9:
            self.positions.pop(sym, None)
        else:
            self.positions[sym] = q

    # ---- the bar ----

    def run(self):
        # Every bar after the first fills the previous bar's decisions (none
        # is still a fill step: funding accrues over the bar and open
        # targets re-issue), then opens.
        for k, t in enumerate(self.bars):
            if k > 0:
                prev = self.bars[k - 1]
                self.fill(prev, t, self.decisions.get(prev, []))
            self.open_bar(t)
        last = self.bars[-1] if self.bars else None
        for sym, ctor, amount, rule in self.decisions.get(last, []):
            self.dropped.append((last, sym, ctor, amount, "no next bar"))
        return self

    def open_bar(self, t: int):
        self.seen.append(t)
        self.actions_at(t)
        equity = self.cash
        gross = 0.0
        net = 0.0
        for sym, q in self.sorted_positions():
            p = self.price_at(sym, t)
            if p is not None:
                equity += q * p
                gross += abs(q) * p
                net += q * p
        mm = float(self.cfg.maintenance_margin)
        if gross > 0.0 and equity > 0.0 and equity < mm * gross - 1e-9 * gross:
            policy = self.cfg.policy("on_margin_call")
            if policy == "halt":
                raise Halt(f"margin call at {stamp(t)}: equity {equity:.2f} below {mm * 100}% of gross {gross:.2f}")
            if policy == "liquidate":
                f = 1.0 - equity / (mm * gross)
                for sym, pos in self.sorted_positions():
                    p = self.price_at(sym, t)
                    if p is None:
                        continue
                    qty = -(pos * f)
                    if self.cfg.lot_whole:
                        qty = math.floor(qty) if qty < 0.0 else math.ceil(qty)
                    if qty == 0.0 or abs(qty) > abs(pos):
                        qty = -pos
                    commission = self.commission(qty)
                    fee = abs(qty) * p * float(self.cfg.fee_bps_on_sells) / 10_000.0 if qty < 0.0 else 0.0
                    self.cash -= qty * p + commission + fee
                    equity -= commission + fee
                    gross -= abs(qty) * p
                    net -= -qty * p
                    self.set_position(sym, pos + qty)
                    vol = self.bar_volume(sym, t)
                    self.fills.append(
                        Fill(t, sym, qty, p, commission, fee, 0.0, 0.0, abs(qty) / vol if vol else 0.0, False, False, True)
                    )
        self.nav.append({"t": t, "equity": equity, "cash": self.cash, "gross": gross, "net": net, "leverage": gross / equity if equity > 0.0 else 0.0})

    def actions_at(self, t: int):
        for sym, factor in self.data.splits.get(t, []):
            pos = self.positions.get(sym)
            if pos is None or factor <= 0.0 or factor == 1.0:
                continue
            exact = pos * factor
            kept = math.trunc(exact) if self.cfg.lot_whole else exact
            p = self.price_at(sym, t)
            fraction_cash = (exact - kept) * p if p is not None else 0.0
            self.cash += fraction_cash
            self.set_position(sym, kept)
            if sym in self.last_price:
                self.last_price[sym] /= factor
            self.actions.append((t, sym, "split", factor, fraction_cash))
        for announce, sym, ex, pay, amount in self.data.dividends:
            if announce <= t and ex == t and sym in self.positions:
                pos = self.positions[sym]
                self.receivables.append((max(pay, ex), sym, pos * amount, amount, pos))
        due = [r for r in self.receivables if r[0] <= t]
        self.receivables = [r for r in self.receivables if r[0] > t]
        for _, sym, amount_cash, amount, shares in due:
            self.cash += amount_cash
            self.actions.append((t, sym, "dividend", f"{amount} x {shares}", amount_cash))
        for sym, reason in self.data.delisted.get(t, []):
            if sym in self.delisted_done:
                continue
            self.delisted_done.add(sym)
            pos = self.positions.get(sym)
            if pos is None:
                continue
            haircut = min(max(self.cfg.haircut(reason), 0.0), 1.0)
            last = self.price_at(sym, t)
            last = last if last is not None else 0.0
            price = last * (1.0 - haircut)
            qty = -pos
            commission = self.commission(qty)
            proceeds = -qty * price - commission
            self.cash += proceeds
            self.positions.pop(sym, None)
            self.fills.append(Fill(t, sym, qty, price, commission, 0.0, 0.0, 0.0, 0.0, True, False, True))
            self.actions.append((t, sym, "delisting", f"{reason} {haircut}", proceeds))

    # ---- filling ----

    def fill(self, t: int, tn: int, decisions: list[tuple]):
        cfg = self.cfg
        bars = list(self.seen)
        if not bars or bars[-1] != tn:
            bars.append(tn)
        end = len(bars) - 1
        dt = (tn - t) / (365.0 * 86_400.0)
        if self.cash > 0.0:
            self.cash += self.cash * float(cfg.cash_rate) * dt
        elif self.cash < 0.0:
            self.cash -= -self.cash * float(cfg.margin_rate) * dt
        for sym, q in self.sorted_positions():
            if q >= 0.0:
                continue
            p = self.price_at(sym, tn)
            if p is None:
                continue
            notional = abs(q) * p
            bucket = cfg.borrow_bucket(self.adv(sym, bars, end))
            fee = notional * float(bucket.get("fee_bps", 0.0)) / 10_000.0 * dt
            rebate = notional * float(cfg.short_rebate) * dt
            self.cash -= fee
            self.cash += rebate
        equity_next = self.cash
        for sym, q in self.sorted_positions():
            p = self.price_at(sym, tn)
            if p is not None:
                equity_next += q * p
        # Group the bar's decisions by instrument (symbol order), then the
        # open targets of the instruments without a decision.
        by_equity: dict[str, list] = {}
        for sym, ctor, amount, rule in decisions:
            by_equity.setdefault(sym, []).append((sym, ctor, amount, rule, False))
        for sym in by_equity:
            self.open_targets.pop(sym, None)
        pending = []
        for sym in sorted(by_equity, key=lambda s: self.order[s]):
            pending.extend(by_equity[sym])
        for sym in sorted(self.open_targets, key=lambda s: self.order[s]):
            d = self.open_targets[sym]
            pending.append((d[0], d[1], d[2], d[3], True))
        marks = {}
        for sym, *_ in pending:
            p = self.price_at(sym, tn)
            if p is not None:
                marks[sym] = p

        def reducing(d) -> bool:
            sym, ctor, amount = d[0], d[1], d[2]
            pos = self.positions.get(sym, 0.0)
            if ctor in ("sell", "cover"):
                return pos != 0.0
            if ctor in ("buy", "short"):
                return False
            if ctor == "target_quantity":
                return pos != 0.0 and abs(amount) < abs(pos) and amount * pos >= 0.0
            # target_weight
            base = max(equity_next, 0.0) if cfg.compounding else float(cfg.initial_cash)
            return pos != 0.0 and (amount == 0.0 or amount * pos < 0.0 or abs(amount) * base < abs(pos) * marks.get(sym, 0.0))

        pending.sort(key=lambda d: not reducing(d))
        if pending and equity_next <= 0.0 and cfg.policy("on_ruin") == "halt":
            raise Halt(f"ruin at {stamp(tn)}: equity {equity_next:.2f}")
        # What a weight is a fraction of: equity, or the fixed capital when
        # profits are not reinvested (`compounding` off, the default).
        sizing_equity = max(equity_next, 0.0) if cfg.compounding else float(cfg.initial_cash)
        bar_costs = 0.0
        for sym, ctor, amount, rule, reissued in pending:
            is_target = ctor in ("target_weight", "target_quantity")
            pos = self.positions.get(sym, 0.0)
            bar_price = self.bar_price(sym, tn)
            vol = self.realized_vol(sym, bars, end)
            slip = float(cfg.slippage_bps) / 10_000.0 + float(cfg.slippage_vol_mult) * (vol if vol is not None else 0.0)

            def slipped(p, buying):
                return p * (1.0 + slip) if buying else p * (1.0 - slip)

            if ctor in ("buy", "cover"):
                qty = self.rnd(amount)
            elif ctor in ("sell", "short"):
                qty = -self.rnd(amount)
            elif ctor == "target_quantity":
                qty = self.rnd(amount) - pos
            elif bar_price is not None:
                target = self.rnd(amount * sizing_equity / bar_price)
                if target > pos and target > 0.0:
                    target = self.rnd(amount * sizing_equity / slipped(bar_price, True))
                qty = target - pos
            elif amount == 0.0:
                qty = -pos
            else:
                self.open_targets.pop(sym, None)
                self.dropped.append((t, sym, ctor, amount, f"no price for {sym} at {stamp(tn)}"))
                continue
            if qty == 0.0:
                self.open_targets.pop(sym, None)
                continue
            crosses = ctor in ("sell", "cover") and pos != 0.0 and (pos + qty) * pos < 0.0
            if crosses:
                policy = cfg.policy("on_oversize")
                if policy == "halt":
                    raise Halt(f"oversize {ctor} of {abs(qty)} against {abs(pos)} at {stamp(tn)}")
                if policy == "clamp":
                    self.dropped.append((t, sym, ctor, amount, "clamped at position"))
                    qty = -pos
            requested = abs(qty)
            bar_volume = self.bar_volume(sym, tn)
            partial = False
            if float(cfg.participation_cap) > 0.0 and bar_volume is not None:
                cap = self.rnd(float(cfg.participation_cap) * bar_volume)
                if requested > cap:
                    partial = True
                    qty = math.copysign(cap, qty)
            if qty == 0.0:
                if is_target:
                    self.open_targets[sym] = (sym, ctor, amount, rule)
                else:
                    self.dropped.append((t, sym, ctor, amount, "partial fill: 0; remainder expired"))
                continue
            reduces = pos != 0.0 and (pos + qty) * pos >= 0.0 and abs(pos + qty) < abs(pos)
            if bar_price is not None:
                p, at_last = bar_price, False
            elif reduces and sym in self.last_price:
                p, at_last = self.last_price[sym], True
            else:
                self.open_targets.pop(sym, None)
                self.dropped.append((t, sym, ctor, amount, f"no price for {sym} at {stamp(tn)}"))
                continue
            adv = self.adv(sym, bars, end)
            imp = float(cfg.impact_coef) * math.sqrt(abs(qty) / adv) if float(cfg.impact_coef) > 0.0 and adv is not None and adv > 0.0 else 0.0
            fill_price = p * (1.0 + slip + imp) if qty > 0.0 else p * (1.0 - slip - imp)
            commission = self.commission(qty)
            fee = abs(qty) * fill_price * float(cfg.fee_bps_on_sells) / 10_000.0 if qty < 0.0 else 0.0
            cost = qty * fill_price + commission + fee
            slippage = abs(qty) * p * slip
            impact = abs(qty) * p * imp
            allowance = bar_costs + commission + fee + slippage + impact
            if pos + qty < 0.0 and pos + qty < pos:
                bucket = cfg.borrow_bucket(adv)
                if not bucket.get("shortable", True):
                    self.open_targets.pop(sym, None)
                    self.dropped.append((t, sym, ctor, amount, "not shortable"))
                    continue
            if not reduces:
                new_cash = self.cash - cost
                gross = 0.0
                net = new_cash
                for s2, q2 in self.sorted_positions():
                    q2 = q2 + qty if s2 == sym else q2
                    if s2 == sym:
                        p2 = p
                    else:
                        p2 = self.price_at(s2, tn)
                        p2 = p2 if p2 is not None else 0.0
                    gross += abs(q2) * p2
                    net += q2 * p2
                if sym not in self.positions:
                    gross += abs(qty) * p
                    net += qty * p
                tol = 1e-9 * (1.0 + abs(net)) + allowance
                max_gross = max(float(cfg.max_gross), 1.0)
                if cfg.compounding:
                    breach = new_cash < -(max_gross - 1.0) * max(net, 0.0) - tol or gross > max_gross * net + tol
                else:
                    # On a fixed base leverage is gross against the capital; cash may go negative.
                    breach = gross > max_gross * float(cfg.initial_cash) + tol
                if breach:
                    policy = cfg.policy("on_leverage")
                    if policy == "halt":
                        raise Halt(f"leverage at {stamp(tn)}: cash {new_cash:.2f} gross {gross:.2f} equity {net:.2f}")
                    if policy == "reject":
                        self.dropped.append((t, sym, ctor, amount, "rejected: leverage"))
                        continue
            self.cash -= cost
            self.set_position(sym, pos + qty)
            self.last_price[sym] = fill_price
            bar_costs += commission + fee + slippage + impact
            participation = abs(qty) / bar_volume if bar_volume else 0.0
            self.fills.append(Fill(tn, sym, qty, fill_price, commission, fee, slippage, impact, participation, at_last, partial, False))
            if partial:
                if is_target:
                    self.open_targets[sym] = (sym, ctor, amount, rule)
                else:
                    self.dropped.append((t, sym, ctor, amount, "partial fill; remainder expired"))
            elif is_target:
                self.open_targets.pop(sym, None)

    # ---- output ----

    def frames(self):
        fills = pd.DataFrame(
            [
                {
                    "t": stamp(f.t),
                    "equity": f.equity,
                    "quantity": f.quantity,
                    "price": f.price,
                    "commission": f.commission,
                    "fee": f.fee,
                    "slippage": f.slippage,
                    "impact": f.impact,
                    "participation": f.participation,
                    "at_last_price": f.at_last_price,
                    "partial": f.partial,
                    "forced": f.forced,
                }
                for f in self.fills
            ]
        )
        nav = pd.DataFrame([{**r, "t": stamp(r["t"])} for r in self.nav])
        final = {"cash": self.cash, **{s: q for s, q in self.sorted_positions()}}
        return fills, nav, final


def run_dump(directory: str) -> Reference:
    return Reference(directory).run()


if __name__ == "__main__":
    import sys

    ref = run_dump(sys.argv[1])
    fills, nav, final = ref.frames()
    print(fills.to_string())
    print(nav.tail().to_string())
    print(final)
