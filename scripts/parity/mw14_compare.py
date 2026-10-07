#!/usr/bin/env python3
"""MW14 parity: abt's weekly returns (abt run ... --returns FILE) against the
Python book's canonical weekly returns (_canon_wkret_mw14_full.npy, 1,852
decision weeks from week_end 1991-01-04), scored with the book's own
`run_mw1_core.stats` (52 weeks a year, risk-free 0, population deviation,
CAGR and drawdown compounded on the fixed-base returns) and its dollar P&L
on the fixed $1M.

abt's return at bar k+1 (the NAV change from week k to week k+1 over the base)
is the book's wk_ret of decision week k: the trade at week k's close and the
following week's return. The book's last week (no forward return, cost only)
has no abt counterpart.

  python scripts/parity/mw14_compare.py ABT_RETURNS.parquet CANON.npy [--weeks N]
"""
import argparse
import sys

import numpy as np
import pandas as pd


def stats(r: np.ndarray) -> dict:
    eq = np.cumprod(1 + r)
    return {
        "weeks": len(r),
        "cagr": eq[-1] ** (52 / len(r)) - 1,
        "vol": r.std() * np.sqrt(52),
        "sharpe": r.mean() / r.std() * np.sqrt(52),
        "maxdd": (eq / np.maximum.accumulate(eq) - 1).min(),
        "pnl": r.sum() * 1e6,
    }


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("abt")
    ap.add_argument("canon")
    ap.add_argument("--weeks", type=int, default=None, help="compare the first N weeks only")
    ap.add_argument("--worst", type=int, default=10)
    args = ap.parse_args()
    a = pd.read_parquet(args.abt)
    canon = np.load(args.canon)
    n = min(len(a), len(canon))
    if args.weeks:
        n = min(n, args.weeks)
    ra = a["ret"].to_numpy()[:n]
    rc = canon[:n]
    sa, sc = stats(ra), stats(rc)
    print(f"weeks compared: {n} (abt {len(a)}, canon {len(canon)}); abt weeks {a['t'].iloc[0].date()} .. {a['t'].iloc[n - 1].date()}")
    print(f"{'':10s} {'canon':>14s} {'abt':>14s} {'diff':>12s}")
    for k in ("cagr", "maxdd", "sharpe", "vol", "pnl"):
        print(f"{k:10s} {sc[k]:14.6f} {sa[k]:14.6f} {sa[k] - sc[k]:12.6f}")
    d = ra - rc
    print(f"weekly correlation {np.corrcoef(ra, rc)[0, 1]:.6f}; |diff| mean {np.abs(d).mean():.2e}, max {np.abs(d).max():.2e}; "
          f"weeks equal to 1e-9: {(np.abs(d) < 1e-9).mean():.1%}, to 1e-4: {(np.abs(d) < 1e-4).mean():.1%}")
    if len(canon) == 1852:
        print("canon reference (RESULTS.md MW-126): cagr 0.2089947950, maxdd -0.2617817195, sharpe 1.0199, pnl 7,534,877")
    t = a["t"].to_numpy()[:n]
    worst = np.argsort(-np.abs(d))[: args.worst]
    print(f"largest weekly differences (decision week of the book = the bar before):")
    for i in sorted(worst):
        print(f"  {pd.Timestamp(t[i]).date()}  canon {rc[i]: .6f}  abt {ra[i]: .6f}  diff {d[i]: .6f}")
    eras = [("1990s", "1991", "1999"), ("2000s", "2000", "2009"), ("2010s", "2010", "2019"), ("2020-26", "2020", "2026")]
    years = pd.to_datetime(t).year
    for name, lo, hi in eras:
        m = (years >= int(lo)) & (years <= int(hi))
        if m.sum() > 10:
            print(f"  era {name}: canon cagr {stats(rc[m])['cagr']:.4f}  abt {stats(ra[m])['cagr']:.4f}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
