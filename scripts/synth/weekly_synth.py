#!/usr/bin/env python3
"""A synthetic market in the layout of `corpus/env/equities_1w.dsl` (one bar per
week keyed at the week's Friday), for running the MW14 book and the usability
experiment without the company's data. Nothing here is real; the shapes are:

  names      N names, each listed over a span of weeks (some join late, some
             delist early, with a `delisted` event the week after the last bar)
  close      a lognormal random walk with a per-name drift that switches
             regime every ~40 weeks (so momentum ranks mean something) and a
             split now and then (the raw close halves, `split` records 2)
  trclose    the running total-return index: close x cumulative split, with a
             small dividend every 13 weeks folded in
  dvol       the week's mean daily dollar volume, lognormal around a per-name
             level (a few names sit below $2M)
  member     SP1500 for most names, in spells
  series     SPXTR (a trending index), VIX (mean-reverting) and VIX_MA4 (its
             4-week mean, precomputed as the book's data is)

  python scripts/synth/weekly_synth.py --out DIR [--names 60] [--weeks 520] [--seed 7]
"""
import argparse
from pathlib import Path

import numpy as np
import pandas as pd


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True, type=Path)
    ap.add_argument("--names", type=int, default=60)
    ap.add_argument("--weeks", type=int, default=520)
    ap.add_argument("--seed", type=int, default=7)
    ap.add_argument("--start", default="2015-01-02")
    a = ap.parse_args()
    rng = np.random.default_rng(a.seed)
    weeks = pd.date_range(a.start, periods=a.weeks, freq="W-FRI")
    n, w = a.names, a.weeks
    rows = []
    splits = []
    members = []
    delisted = []
    for i in range(n):
        name = f"E{i + 1:03d}"
        first = 0 if rng.random() < 0.8 else int(rng.integers(1, w // 2))
        last = w - 1 if rng.random() < 0.85 else int(rng.integers(first + 60, w))
        price = float(rng.uniform(5, 150))
        level = float(np.exp(rng.normal(np.log(8e6), 1.2)))
        tri = price
        cum_split = 1.0
        drift = rng.normal(0.001, 0.004)
        vol = float(rng.uniform(0.02, 0.06))
        in_index = rng.random() < 0.85
        for k in range(first, last + 1):
            if (k - first) % 40 == 0:
                drift = rng.normal(0.001, 0.004)
            ret = float(np.exp(rng.normal(drift, vol)))
            div = price * 0.004 if (k - first) % 13 == 12 and rng.random() < 0.6 else 0.0
            new_price = price * ret
            factor = 1.0
            if rng.random() < 0.004 and new_price > 60:
                factor = 2.0
            tri = tri * (new_price * factor + div) / price if k > first else tri
            price = new_price / factor
            cum_split *= factor
            dvol = float(np.exp(rng.normal(np.log(level), 0.35)))
            rows.append((name, weeks[k], price, tri, dvol))
            if factor != 1.0:
                splits.append((name, weeks[k], factor))
            if in_index and not (k % 150 in range(0, 6) and rng.random() < 0.5):
                members.append((name, weeks[k], "SP1500"))
        if last < w - 1:
            delisted.append((name, weeks[last + 1], "ended"))
    df = pd.DataFrame(rows, columns=["A", "T", "P", "TRI", "D"])
    out = a.out
    out.mkdir(parents=True, exist_ok=True)

    def write(rel, frame):
        frame.to_parquet(out / f"{rel}.parquet", index=False)
        print(f"wrote {rel}: {len(frame)}")

    write("universe", df[["A", "T"]])
    write("close", df[["A", "T", "P"]])
    write("trclose", df[["A", "T"]].assign(P=df["TRI"]))
    write("dvol", df[["A", "T", "D"]])
    write("split", pd.DataFrame(splits, columns=["A", "T", "Factor"]))
    write("member", pd.DataFrame(members, columns=["A", "T", "Idx"]))
    write("delisted", pd.DataFrame(delisted, columns=["A", "T", "Reason"]))
    # Market series.
    spx = 1000.0 * np.cumprod(np.exp(rng.normal(0.0015, 0.02, size=w)))
    vix = np.empty(w)
    v = 18.0
    for k in range(w):
        v = max(9.0, v + 0.15 * (18.0 - v) + rng.normal(0, 2.2))
        vix[k] = v
    vix_ma4 = pd.Series(vix).rolling(4, min_periods=1).mean().to_numpy()
    series = pd.concat(
        [
            pd.DataFrame({"T": weeks, "Name": "SPXTR", "V": spx}),
            pd.DataFrame({"T": weeks, "Name": "VIX", "V": vix}),
            pd.DataFrame({"T": weeks, "Name": "VIX_MA4", "V": vix_ma4}),
        ],
        ignore_index=True,
    )
    write("series", series)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
