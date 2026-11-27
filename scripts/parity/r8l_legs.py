#!/usr/bin/env python3
"""scripts/parity/r8l_legs.py STORE RUN LEGS_CSV [--f F] [--base B] [--end DAY] [--legs-out FILE]

R8L run on a store of the vendor's own minute bars (scripts/r8l-store.sh), beside the Python book's legs.

This is not the parity of docs/parity-r8l.md, which is on the book's own bars: the book's are TradeStation's
back-adjusted continuous contracts from 2000, the store's are Databento's contracts from 2010, rolled on volume
and back-adjusted by the adapter. It answers what the parity cannot: whether the store's sessions are the
book's. A store of whole electronic days makes another rule of `r8l.dsl`, with other legs on other days; a store
of each root's primary session makes the book's legs wherever the two vendors' bars agree on the signal.

It prints, over the days both cover (each root from its 251st session in the store, past the rule's longest
window, the median of 250 sessions, to the book's last day): the legs each side makes, the legs both make (market and day), their directions and the
correlation of their P&L, then each side's fixed-base scorecard on ES's session days there (docs/parity-r8l.md's
conventions: daily P&L over the base, mean x 252, population Sharpe, additive drawdown).

LEGS_CSV is d20-research's results/r40/census_legs.csv (uni, mkt, day, kind, dir, net: the book's legs, HOME-26
main and sidecar, `net` in units of the risk per leg) or a file of those columns. RUN is the id `abt run`
printed; its outputs are read from the store (scripts/parity/abt_store.py)."""
import argparse
import sys

import numpy as np
import pandas as pd

import abt_store

WARM = 250


def stats(v: np.ndarray) -> dict:
    c = np.cumsum(v)
    return {
        "ann_ret": v.mean() * 252,
        "sharpe": v.mean() / v.std() * np.sqrt(252) if v.std() > 0 else float("nan"),
        "maxDD": (np.maximum.accumulate(c) - c).max(),
        "total": v.sum(),
        "pf": np.where(v > 0, v, 0).sum() / max(-np.where(v < 0, v, 0).sum(), 1e-12),
    }


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("store")
    ap.add_argument("run")
    ap.add_argument("legs")
    ap.add_argument("--f", type=float, default=0.006493912071090746)
    ap.add_argument("--base", type=float, default=1e6)
    ap.add_argument("--end", default="2026-08-14", help="the book's last day")
    ap.add_argument("--legs-out", help="write the run's legs here (mkt, day, dir, amount, pnl_abt, fills), as CSV")
    args = ap.parse_args()
    D = args.f * args.base
    end = pd.Timestamp(args.end)

    py = pd.read_csv(args.legs)
    if "uni" in py:
        py = py[py.uni == "home26"]
    py = py[py.kind.isin(["main", "sidecar"])].copy()
    py["day"] = pd.to_datetime(py["day"]).dt.normalize()
    py["pnl"] = D * py["net"]

    # The store's instruments: a root's continuous series is ROOT.v.0.
    names = {i: k[: -len(".v.0")] for i, k in abt_store.keys(args.store).items() if k.endswith(".v.0")}
    ses = abt_store.query(args.store, "future.session")
    ses = ses[ses["A"].astype("int64").isin(names)].copy()
    ses["mkt"] = ses["A"].astype("int64").map(names)
    ses["day"] = ses["S"].dt.normalize()
    days = ses.groupby("mkt")["day"].apply(lambda d: sorted(set(d)))
    first = {m: d[WARM] for m, d in days.items() if len(d) > WARM}
    covered = {(m, x) for m, d in days.items() for x in d[WARM:] if x <= end}

    dec = abt_store.relation(args.store, args.run, "decisions")
    dec["mkt"] = dec["instrument"].astype("int64").map(names)
    dec["day"] = dec["t"].dt.normalize()
    dec["dir"] = np.where(dec["ctor"].isin(["sell", "short"]), -1, 1) * np.sign(dec["amount"])
    events = abt_store.relation(args.store, args.run, "events")
    fills = events[events["kind"].isin(["fill", "partial_fill"])].copy()
    fills["mkt"] = fills["instrument"].astype("int64").map(names)
    fills["day"] = fills["t"].dt.normalize()
    leg = fills.groupby(["mkt", "day"]).agg(pnl_abt=("cash_flow", "sum"), fills=("cash_flow", "size")).reset_index()
    ab = dec[["mkt", "day", "dir", "amount"]].merge(leg, on=["mkt", "day"], how="left")
    if args.legs_out:
        ab.assign(day=ab["day"].dt.date).to_csv(args.legs_out, index=False)
    print(f"the run: {len(dec)} decisions, {len(fills)} fills, {len(ab)} legs over {ab.day.min().date()} .. {ab.day.max().date()}; "
          f"legs without two fills {int((ab.fills != 2).sum())}; P&L {ab.pnl_abt.sum():,.2f}")
    print(f"the book: {len(py)} legs over {py.day.min().date()} .. {py.day.max().date()}; P&L {py.pnl.sum():,.2f} at {D:,.2f} a unit")

    inside = lambda d: [(m, x) in covered for m, x in zip(d["mkt"], d["day"])]
    py_c, ab_c = py[inside(py)], ab[inside(ab)]
    m = py_c.merge(ab_c, on=["mkt", "day"], how="outer", indicator=True, suffixes=("_py", "_abt"))
    both = m[m["_merge"] == "both"]
    print(f"\nover the days both cover ({len(covered)} market-days, {len(first)} roots, from {min(first.values()).date()} to {end.date()}):")
    print(f"  legs: the book {len(py_c)}, abt {len(ab_c)}; both {len(both)} ({len(both) / max(len(py_c), 1):.1%} of the book's), "
          f"the book only {int((m['_merge'] == 'left_only').sum())}, abt only {int((m['_merge'] == 'right_only').sum())}")
    if len(both):
        same = both["dir_py"] == both["dir_abt"]
        print(f"  of both: the same direction {same.mean():.2%}; leg P&L correlation {np.corrcoef(both['pnl'], both['pnl_abt'].fillna(0))[0, 1]:.4f}; "
              f"P&L the book {both['pnl'].sum():,.0f}, abt {both['pnl_abt'].sum():,.0f}")
    per = m.assign(both=m["_merge"] == "both", book=m["_merge"] != "right_only", abt=m["_merge"] != "left_only").groupby("mkt")[["book", "abt", "both"]].sum()
    per["share"] = (per["both"] / per["book"].clip(lower=1)).round(3)
    print("  by root (the book's legs, abt's, both, the share of the book's):")
    print("   " + "  ".join(f"{k} {int(r.book)}/{int(r.abt)}/{int(r.both)} {r.share:.0%}" for k, r in per.iterrows()))

    if "ES" in days.index:
        cal = [d for d in days["ES"] if first["ES"] <= d <= end]
        va = ab_c.groupby("day")["pnl_abt"].sum().reindex(cal).fillna(0.0).to_numpy() / args.base
        vp = py_c.groupby("day")["pnl"].sum().reindex(cal).fillna(0.0).to_numpy() / args.base
        sa, sp = stats(va), stats(vp)
        print(f"\nthe scorecard on ES's {len(cal)} session days there ({cal[0].date()} .. {cal[-1].date()}), legs summed by day over the base:")
        print(f"{'':10s} {'the book':>12s} {'abt':>12s}")
        for k in sp:
            print(f"{k:10s} {sp[k]:12.6f} {sa[k]:12.6f}")
        print(f"daily correlation {np.corrcoef(va, vp)[0, 1]:.4f}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
