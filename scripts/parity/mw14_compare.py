#!/usr/bin/env python3
"""MW14 parity: abt's weekly returns, from the run's book in the data layer's
store (K-9.1: `kernel.<run>.book`, the NAV at every decision barrier, and the
fixed base the run's report states), against the Python book's canonical
weekly returns (_canon_wkret_mw14_full.npy, 1,852 decision weeks from
week_end 1991-01-04), scored with the book's own `run_mw1_core.stats` (52
weeks a year, risk-free 0, population deviation, CAGR and drawdown
compounded on the fixed-base returns) and its dollar P&L on the fixed $1M.

abt's return at bar k+1 (the NAV change from week k to week k+1 over the base)
is the book's wk_ret of decision week k: the trade at week k's close and the
following week's return. The two series are aligned on that decision week:
the canon's first is `--canon-first` (1991-01-04, the first week of
`run_book`'s window), and its i-th return is abt's i-th decision week from
there. A run that starts earlier (on the data's first week, so that its rows
and recursions warm up on 1990 as the book's features do) has weeks before
the canon's first: they are not compared, and their returns are reported,
since a book that traded there is not the canon's, which starts flat. The
book's last week (no forward return, cost only) has no abt counterpart.

  abt data serve --store STORE --socket SOCKET &
  abt run --strategy mw14 --data SOCKET --start 1990-01-01 corpus-v2/catalog corpus-v2/lib corpus-v2/company/mw14.dsl
      (prints `run RUN: the outputs are kernel.RUN.{...}`)
  python scripts/parity/mw14_compare.py STORE RUN CANON.npy [--weeks N] [--json FILE]

`--json FILE` also writes the comparison as one JSON object (the weeks
compared, the correlation, the counts of weeks equal to the canon to 1e-4,
1e-9 and 8e-15, the largest difference, both sides' statistics, the weeks
decided before the canon's first and their summed absolute return, and a
fingerprint of the run's own returns, `abt_sha256`, by which two runs are
seen to be the same to the last bit): what scripts/bench-mw14.sh records
per run.

It also checks that the run's labels are the weekly series' keys (ERRATA
E-147): every label but the last is a week's `SPXTR_W` key, so the gate and
the VIX read at it are its own week's (`labels_off_key`, 0 expected). The
last is exempt: the equities end inside a week the index runs through (the
lake on Wednesday 2026-06-24, `$SPXTR` to 2026-07-09), keyed at the index's
last day of that week, so the last label reads the week before; it decides
the book's last week only, which is not compared (`last_label_off_key`).
"""
from __future__ import annotations

import argparse
import hashlib
import json
import sys

import numpy as np
import pandas as pd

import abt_store

CANON_FIRST = "1991-01-04"


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


def align(book: pd.DataFrame, base: float | None, canon_first: pd.Timestamp) -> tuple[pd.DataFrame, pd.DataFrame]:
    """abt's return of every decision week (`decided`, the bar the trade was
    decided at; `t`, the bar its return is marked at), split at the canon's
    first decision week: the weeks before it, which the canon does not have,
    and the weeks from it, the canon's in order."""
    b = book.sort_values("t").reset_index(drop=True)
    r = abt_store.bar_returns(b, base)
    r.insert(0, "decided", b["t"].to_numpy()[:-1])
    before = r[r["decided"] < canon_first].reset_index(drop=True)
    after = r[r["decided"] >= canon_first].reset_index(drop=True)
    return before, after


def label_key_misses(labels: pd.Series, keys: pd.Series) -> tuple[list, pd.Timestamp | None]:
    """The run's labels, ascending, that are no key of the weekly series, but
    the last; and the last label when it is none (else None)."""
    t = sorted(set(pd.DatetimeIndex(labels)))
    have = set(pd.DatetimeIndex(keys))
    off = [x for x in t[:-1] if x not in have]
    return off, (t[-1] if t and t[-1] not in have else None)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("store")
    ap.add_argument("run")
    ap.add_argument("canon")
    ap.add_argument("--weeks", type=int, default=None, help="compare the first N weeks only")
    ap.add_argument("--worst", type=int, default=10)
    ap.add_argument("--canon-first", default=CANON_FIRST, help="the canon's first decision week (default %(default)s)")
    ap.add_argument("--json", default=None, help="also write the comparison to this file as JSON")
    args = ap.parse_args()
    # The return per decision bar: the report's calendar is the decision clock.
    base = abt_store.report(args.store, args.run).get("base_capital")
    first = pd.Timestamp(args.canon_first)
    book = abt_store.relation(args.store, args.run, "book")
    before, a = align(book, base, first)
    if a.empty or a["decided"].iloc[0] != first:
        got = "none" if a.empty else str(a["decided"].iloc[0].date())
        print(f"abt's first decision week from {first.date()} is {got}: the run does not decide at the canon's first week, so the two series cannot be aligned")
        return 1
    if len(before):
        print(
            f"abt decided {len(before)} weeks before the canon's first ({before['decided'].iloc[0].date()} .. {before['decided'].iloc[-1].date()}), not compared; "
            f"their summed |return| is {np.abs(before['ret']).sum():.3e} (0 when the book held nothing there, as the canon's)"
        )
    series = abt_store.query(args.store, "series.value")
    off, last_off = label_key_misses(book["t"], series.loc[series["Name"].astype(str) == "SPXTR_W", "T"])
    print(
        f"run labels that are no SPXTR_W key: {len(off)} of {book['t'].nunique() - 1}, the last label exempt"
        + (f" ({', '.join(str(x.date()) for x in off[:5])}{', ...' if len(off) > 5 else ''})" if off else "")
        + (f"; the last, {last_off.date()}, reads the week before (E-147)" if last_off is not None else "")
    )
    canon = np.load(args.canon)
    n = min(len(a), len(canon))
    if args.weeks:
        n = min(n, args.weeks)
    ra = a["ret"].to_numpy()[:n]
    rc = canon[:n]
    sa, sc = stats(ra), stats(rc)
    print(f"weeks compared: {n} (abt {len(a)} from the canon's first, canon {len(canon)}); decision weeks {a['decided'].iloc[0].date()} .. {a['decided'].iloc[n - 1].date()}")
    print(f"{'':10s} {'canon':>14s} {'abt':>14s} {'diff':>12s}")
    for k in ("cagr", "maxdd", "sharpe", "vol", "pnl"):
        print(f"{k:10s} {sc[k]:14.6f} {sa[k]:14.6f} {sa[k] - sc[k]:12.6f}")
    d = ra - rc
    print(f"weekly correlation {np.corrcoef(ra, rc)[0, 1]:.6f}; |diff| mean {np.abs(d).mean():.2e}, max {np.abs(d).max():.2e}; "
          f"weeks equal to 1e-9: {(np.abs(d) < 1e-9).mean():.1%}, to 1e-4: {(np.abs(d) < 1e-4).mean():.1%}")
    if args.json:
        with open(args.json, "w") as f:
            json.dump(
                {
                    "weeks": int(n),
                    "abt_weeks": int(len(a)),
                    "canon_weeks": int(len(canon)),
                    "first_week": str(a["t"].iloc[0].date()),
                    "first_decision_week": str(a["decided"].iloc[0].date()),
                    "before_canon_weeks": int(len(before)),
                    "before_canon_abs_sum": float(np.abs(before["ret"]).sum()),
                    "labels_off_key": len(off),
                    "labels_off_key_first": [str(x.date()) for x in off[:10]],
                    "last_label_off_key": None if last_off is None else str(last_off.date()),
                    "correlation": float(np.corrcoef(ra, rc)[0, 1]),
                    "max_abs_diff": float(np.abs(d).max()),
                    "mean_abs_diff": float(np.abs(d).mean()),
                    "equal_1e-4": int((np.abs(d) < 1e-4).sum()),
                    "equal_1e-9": int((np.abs(d) < 1e-9).sum()),
                    "equal_8e-15": int((np.abs(d) <= 8e-15).sum()),
                    # The run's own weekly returns from the canon's first
                    # week, every bit: two runs with the same fingerprint
                    # decided and filled the same.
                    "abt_sha256": hashlib.sha256(np.ascontiguousarray(a["ret"].to_numpy(), dtype="<f8").tobytes()).hexdigest(),
                    "abt": {k: float(v) for k, v in sa.items()},
                    "canon": {k: float(v) for k, v in sc.items()},
                },
                f,
            )
    if len(canon) == 1852:
        print("canon reference (RESULTS.md MW-126): cagr 0.2089947950, maxdd -0.2617817195, sharpe 1.0199, pnl 7,534,877")
    t = a["decided"].to_numpy()[:n]
    worst = np.argsort(-np.abs(d))[: args.worst]
    print("largest weekly differences, by decision week:")
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
