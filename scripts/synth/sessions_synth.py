#!/usr/bin/env python3
"""A synthetic market in the layout of `corpus/env/futures_sessions.dsl`, for
running the R8L book and the usability experiment without the company's
TradeStation data. Nothing here is real; the shapes follow
`scripts/ingest/tradestation_sessions.py`:

  roots      a few futures roots, each with its own session window
             (minute k of a session is labelled open + k + 1 minutes, the
             bar's close)
  session    every weekday is a session day of every root (@1d, the date)
  open_d .. close_d, multiplier   the session bar and the point value (@1d)
  open0_m    the open of minute 0, labelled open + 1 min
  close20_m, close30_m   the close of minute 19 (label open + 20 min) and of
             minute 29 (label open + 30 min); a session now and then has no
             print at minute 29, so close30_m sits at minute 28
  open_m, close_m   the entry print (minute 30: label open + 31 min) and the
             exit print (the last minute: label = the session close), their
             open and close
  clock      the decision time, open + 30 min, with S the open, for every
             session of every root
  calendar   the session days of the lead root (ES)
  securities id, ticker, from, to, multiplier, asset_class future, commission

The session path: a gap from the previous close, then a drift over the first
30 minutes with fat tails (so |m| clears 2 x its median often enough), then
a drift to the close that partly continues the morning. Back-adjusted prices
may cross zero for one root (ZN starts low).

  python scripts/synth/sessions_synth.py --out DIR [--days 500] [--seed 7]
"""
import argparse
from datetime import time
from pathlib import Path

import numpy as np
import pandas as pd

ROOTS = {
    # root: (open, close, point value, commission per contract per side, start price, ATR-ish scale)
    "ES": (time(8, 30), time(15, 0), 50.0, 2.297, 4000.0, 30.0),
    "NQ": (time(8, 30), time(15, 0), 20.0, 2.297, 14000.0, 140.0),
    "CL": (time(8, 0), time(13, 30), 1000.0, 2.41, 75.0, 1.8),
    "GC": (time(7, 20), time(12, 30), 100.0, 2.41, 1900.0, 20.0),
    "ZN": (time(7, 20), time(14, 0), 1000.0, 1.72, 2.0, 0.5),
    "6J": (time(7, 20), time(14, 0), 1250.0, 2.297, 0.9, 0.006),
}


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True, type=Path)
    ap.add_argument("--days", type=int, default=500)
    ap.add_argument("--seed", type=int, default=7)
    ap.add_argument("--start", default="2023-01-02")
    a = ap.parse_args()
    rng = np.random.default_rng(a.seed)
    days = pd.bdate_range(a.start, periods=a.days)
    frames = {k: [] for k in ("session", "open_d", "high_d", "low_d", "close_d", "open0_m", "close20_m", "close30_m", "open_m", "close_m", "clock", "multiplier")}
    for root, (op, cl, bpv, comm, p0, scale) in ROOTS.items():
        minutes = int((cl.hour * 60 + cl.minute) - (op.hour * 60 + op.minute))
        prev_close = p0
        D = days.values.astype("datetime64[ns]")
        base = days.values.astype("datetime64[m]") + np.timedelta64(op.hour * 60 + op.minute, "m")
        label = lambda j: (base + np.timedelta64(int(j) + 1, "m")).astype("datetime64[ns]")
        recs = {k: [] for k in frames}
        for i, d in enumerate(days):
            atr = scale * float(np.exp(rng.normal(0, 0.25)))
            gap = rng.standard_t(3) * 0.35 * atr
            o0 = prev_close + gap
            # The first 30 minutes: a fat-tailed drift.
            m30 = rng.standard_t(2.5) * 0.9 * atr
            c20 = o0 + m30 * float(rng.uniform(0.45, 0.8)) + rng.normal(0, 0.1 * atr)
            c30 = o0 + m30
            # The rest of the session partly continues the morning.
            rest = 0.35 * m30 + rng.normal(0, 0.8 * atr)
            close = c30 + rest
            entry_open = c30 + rng.normal(0, 0.03 * atr)
            path = np.array([o0, c20, c30, entry_open, close])
            hi = path.max() + abs(rng.normal(0, 0.3 * atr))
            lo = path.min() - abs(rng.normal(0, 0.3 * atr))
            missing_29 = rng.random() < 0.02
            recs["session"].append((root, D[i]))
            recs["open_d"].append((root, D[i], o0))
            recs["high_d"].append((root, D[i], hi))
            recs["low_d"].append((root, D[i], lo))
            recs["close_d"].append((root, D[i], close))
            recs["multiplier"].append((root, D[i], bpv))
            recs["open0_m"].append((root, label(0)[i], o0))
            recs["close20_m"].append((root, label(19)[i], c20))
            recs["close30_m"].append((root, label(28 if missing_29 else 29)[i], c30))
            recs["open_m"].append((root, label(30)[i], entry_open))
            recs["close_m"].append((root, label(30)[i], entry_open + rng.normal(0, 0.02 * atr)))
            exit_open = close + rng.normal(0, 0.02 * atr)
            recs["open_m"].append((root, label(minutes - 1)[i], exit_open))
            recs["close_m"].append((root, label(minutes - 1)[i], close))
            recs["clock"].append((root, (base[i] + np.timedelta64(30, "m")).astype("datetime64[ns]"), base[i].astype("datetime64[ns]")))
            prev_close = close
        cols = {
            "session": ["A", "T"], "open_d": ["A", "T", "O"], "high_d": ["A", "T", "H"], "low_d": ["A", "T", "L"], "close_d": ["A", "T", "P"],
            "multiplier": ["A", "T", "M"], "open0_m": ["A", "T", "P"], "close20_m": ["A", "T", "P"], "close30_m": ["A", "T", "P"],
            "open_m": ["A", "T", "O"], "close_m": ["A", "T", "P"], "clock": ["A", "T", "S"],
        }
        for k, rows in recs.items():
            frames[k].append(pd.DataFrame(rows, columns=cols[k]))
    out = a.out
    out.mkdir(parents=True, exist_ok=True)
    for rel, parts in frames.items():
        f = pd.concat(parts, ignore_index=True)
        f.to_parquet(out / f"{rel}.parquet", index=False)
        print(f"wrote {rel}: {len(f)}")
    cal = pd.DataFrame({"T": days.values.astype("datetime64[ns]")})
    cal.to_parquet(out / "calendar.parquet", index=False)
    roots = sorted(ROOTS)
    sec = pd.DataFrame({
        "id": roots, "ticker": roots, "from": pd.Timestamp("1990-01-01"), "to": pd.NaT,
        "multiplier": [ROOTS[r][2] for r in roots], "asset_class": "future", "commission_per_contract": [ROOTS[r][3] for r in roots],
    })
    sec.to_parquet(out / "securities.parquet", index=False)
    print("wrote securities:", len(sec))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
