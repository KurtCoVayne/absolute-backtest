#!/usr/bin/env python3
"""MORNIGHT-R8L parity: abt's legs and daily P&L (abt run ... --dump DIR
--returns FILE --report-by day --report-calendar calendar) against the Python
book's golden legs (results/r40/leg_flags.parquet, HOME-26 main and sidecar
legs: R8L is R8 without the late leg) and its fixed-base scorecard
(scripts/r39_fixed.py `path_stats_fixed`: daily P&L over the $1M base on the
ES calendar, mean x 252, population Sharpe, additive drawdown).

  python scripts/parity/r8l_compare.py DUMP RETURNS LEG_FLAGS [--f F]
"""
import argparse
import sys

import numpy as np
import pandas as pd


def stats(v: np.ndarray) -> dict:
    c = np.cumsum(v)
    return {
        "ann_ret": v.mean() * 252,
        "vol": v.std() * np.sqrt(252),
        "sharpe": v.mean() / v.std() * np.sqrt(252),
        "maxDD": (np.maximum.accumulate(c) - c).max(),
        "total_$M": v.sum(),
        "pf": np.where(v > 0, v, 0).sum() / max(-np.where(v < 0, v, 0).sum(), 1e-12),
    }


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("dump")
    ap.add_argument("returns")
    ap.add_argument("leg_flags")
    ap.add_argument("--f", type=float, default=0.006493912071090746)
    ap.add_argument("--base", type=float, default=1e6)
    args = ap.parse_args()
    lf = pd.read_parquet(args.leg_flags)
    py = lf[(lf.uni == "home26") & lf.kind.isin(["main", "sidecar"])].copy()
    py["day"] = pd.to_datetime(py["day"]).dt.normalize()
    D = args.f * args.base
    py["pnl"] = D * py["net"]
    # abt legs: decisions (one per market-session) and their two fills.
    dec = pd.read_parquet(f"{args.dump}/decisions.parquet")
    dec["day"] = pd.to_datetime(dec["t"]).dt.normalize()
    # A target quantity is signed; a delta order (sell, short) records a positive amount.
    dec["dir"] = np.where(dec["ctor"].isin(["sell", "short"]), -1, 1) * np.sign(dec["amount"])
    fills = pd.read_parquet(f"{args.dump}/fills.parquet")
    fills["day"] = pd.to_datetime(fills["t"]).dt.normalize()
    fills["flow"] = 0.0
    sec = pd.read_parquet(f"{args.dump}/data/securities.parquet").set_index("id")
    fills["mult"] = fills["equity"].map(sec["multiplier"])
    fills["flow"] = -fills["quantity"] * fills["price"] * fills["mult"] - fills["commission"]
    entries = fills[fills.groupby(["equity", "day"]).cumcount() == 0].rename(columns={"price": "e_abt"})
    exits = fills[fills.groupby(["equity", "day"]).cumcount() == 1].rename(columns={"price": "x_abt"})
    leg = fills.groupby(["equity", "day"])["flow"].sum().rename("pnl_abt").reset_index()
    ab = dec.rename(columns={"equity": "mkt"})[["mkt", "day", "dir", "amount"]].merge(
        leg.rename(columns={"equity": "mkt"}), on=["mkt", "day"], how="left")
    ab = ab.merge(entries.rename(columns={"equity": "mkt"})[["mkt", "day", "e_abt"]], on=["mkt", "day"], how="left")
    ab = ab.merge(exits.rename(columns={"equity": "mkt"})[["mkt", "day", "x_abt"]], on=["mkt", "day"], how="left")
    m = py.merge(ab, on=["mkt", "day"], how="outer", indicator=True, suffixes=("_py", "_abt"))
    both = m[m["_merge"] == "both"]
    print(f"legs: python {len(py)}, abt {len(ab)}; matched {len(both)}, python only {int((m['_merge'] == 'left_only').sum())}, abt only {int((m['_merge'] == 'right_only').sum())}")
    print(f"matched direction equal: {(both['dir_py'] == both['dir_abt']).mean():.4%}")
    pe = np.isclose(both["pnl"], both["pnl_abt"], rtol=1e-9, atol=1e-6)
    print(f"matched leg P&L equal to 1e-6: {pe.mean():.4%}; P&L sum python {both['pnl'].sum():,.2f} abt {both['pnl_abt'].sum():,.2f}")
    if "fallback_fill" in both:
        print(f"  of the unequal: fallback-fill legs {int((~pe & both['fallback_fill']).sum())} of {int((~pe).sum())}")
    print("python-only legs (first 10):")
    print(m[m["_merge"] == "left_only"][["mkt", "day", "kind", "dir_py", "m", "g", "m_med", "pnl"]].head(10).to_string())
    print("abt-only legs (first 10):")
    print(m[m["_merge"] == "right_only"][["mkt", "day", "dir_abt", "pnl_abt"]].head(10).to_string())
    # The scorecard on the ES calendar.
    r = pd.read_parquet(args.returns)
    v_abt = r["pnl"].to_numpy() / args.base
    pyd = py.groupby("day")["pnl"].sum()
    cal = pd.to_datetime(r["t"]).dt.normalize()
    v_py = pyd.reindex(cal).fillna(0.0).to_numpy() / args.base
    sa, sp = stats(v_abt), stats(v_py)
    print(f"\ncalendar days {len(cal)} ({cal.iloc[0].date()} .. {cal.iloc[-1].date()})")
    print(f"{'':10s} {'python':>12s} {'abt':>12s} {'diff':>10s}")
    for k in sp:
        print(f"{k:10s} {sp[k]:12.6f} {sa[k]:12.6f} {sa[k] - sp[k]:10.6f}")
    print("reference (results/r42 scorecard): ann 0.1895, sharpe 1.2777, maxDD 0.1967, total 5.15, pf 1.3113, legs 21,892")
    print(f"daily correlation {np.corrcoef(v_abt, v_py)[0, 1]:.6f}; days equal to 1e-9 {np.isclose(v_abt, v_py, atol=1e-9).mean():.2%}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
