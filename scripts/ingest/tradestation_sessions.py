#!/usr/bin/env python3
"""TradeStation 1-minute continuous futures -> the Parquet inputs of `abt bundle
build --from DIR --env futures_sessions` (corpus/env/futures_sessions.dsl) for the
MORNIGHT-R8L book (d20-research scripts/r31_return.py `load`, r37_book.py,
d20research/data/tradestation.py `build_panel_ts` / `daily_frame`, sessions.py).

  python scripts/ingest/tradestation_sessions.py --cache DIR --config DIR --out OUT

DIR (cache) holds <TICKER>.parquet: minute bars `ts, open, high, low, close,
volume`, naive Chicago wall clock, labelled at the bar's START (the d20 cache
convention). DIR (config) holds contract_specs.yaml. The book's session panel
is rebuilt exactly: each root's primary-session windows (sessions.py, ET
windows moved to Chicago by a constant hour), minute index = minutes since the
window opens, the grid width = the most common session length, minutes beyond
it dropped, sessions through 2026-08-14.

Written (bars keyed at their CLOSE, the abt label: a bar starting 08:30 is 08:31):

  session    (A, D)       every session day of the root (@1d)
  open_d .. close_d       the session bar: first open, high, low, last close (@1d)
  open0_m    (A, T, P)    the open of the session's first minute, when it traded
  close20_m  (A, T, P)    the close of the last traded minute before minute 20
  close30_m  (A, T, P)    the close of the last traded minute before minute 30
  open_m/close_m          the prints the book trades at: the first traded minute
                          from minute 30 on (the entry) and the session's last
                          traded minute (the exit)
  calendar   (D)          the ES session days from 2000-01-01 to 2026-08-14: the
                          book's reporting calendar (days without a leg count 0)
  clock      (A, T, S)    the decision time, 30 minutes after the session opens,
                          with S the session's open: for every session the book
                          may trade (an ES session day, the root's first usable
                          year onward, 2000-01-01 to 2026-08-14)
  multiplier (A, D, M)    the contract's point value (6J scaled by 0.01, its TS
                          prices being 100x the exchange's)
  securities              id = root, multiplier, asset_class future, and the
                          commission per contract per side (--commissions JSON)

The book's own fallbacks are not printed into the data: where minute 30 did not
trade, the entry is the next traded minute here, where the book fills at the
minute-29 close.
"""
import argparse
import json
import sys
import time as _time
from datetime import date, time, timedelta
from pathlib import Path

import numpy as np
import pandas as pd
import yaml

TS_TO_ROOT = {
    "EC": "6E", "FV": "ZF", "TU": "ZT", "US": "ZB", "TY": "ZN", "ES": "ES",
    "NQ": "NQ", "RTY": "RTY", "NK": "NKD", "CL": "CL", "NG": "NG", "RB": "RB",
    "HO": "HO", "C": "ZC", "S": "ZS", "BO": "ZL", "W": "ZW", "SM": "ZM",
    "LC": "LE", "LH": "HE", "FC": "GF", "GC": "GC", "HG": "HG", "SI": "SI",
    "PL": "PL", "JY": "6J", "MP1": "6M", "BTC": "BTC", "ETH": "ETH",
    "SB": "SB", "CC": "CC", "CT": "CT", "KC": "KC",
}
HOME26_EXCLUDED = {"6M", "6E", "HG", "CC", "CT", "BTC", "NKD"}
DEV_USABLE_FROM = {
    "ES": 2000, "NQ": 2000, "CC": 2000, "6E": 2001, "6J": 2001, "ZB": 2001,
    "ZF": 2001, "ZN": 2001, "CT": 2001, "KC": 2001, "RTY": 2002, "ZT": 2003,
    "6M": 2003, "NKD": 2006, "CL": 2007, "GC": 2007, "HE": 2007, "HG": 2007,
    "HO": 2007, "LE": 2007, "NG": 2007, "PL": 2007, "RB": 2007, "SI": 2007,
    "ZC": 2007, "ZL": 2007, "ZM": 2007, "ZS": 2007, "ZW": 2007, "GF": 2008,
    "SB": 2008,
}
TS_BPV_SCALE = {"6J": 0.01}
D0, D9 = date(1900, 1, 1), date(2100, 1, 1)
ET_OFFSET = timedelta(hours=1)
LO, HI = date(2000, 1, 1), date(2026, 8, 14)
CT, ET = "CT", "ET"


def W(o, c, tz, f=D0, u=D9):
    return (o, c, tz, f, u)


SESSIONS = {
    **{r: [W(time(7, 20), time(14, 0), CT)] for r in ("ZT", "ZF", "ZN", "ZB")},
    **{r: [W(time(8, 30), time(15, 0), CT)] for r in ("ES", "NQ", "RTY")},
    "NKD": [W(time(8, 0), time(15, 15), CT)],
    **{r: [W(time(8, 0), time(13, 30), CT)] for r in ("CL", "NG", "RB", "HO")},
    **{r: [W(time(8, 20), time(13, 30), ET)] for r in ("GC", "SI", "HG", "PL")},
    **{r: [W(time(7, 20), time(14, 0), CT)] for r in ("6J", "6E", "6L", "6M")},
    **{r: [W(time(9, 30), time(13, 15), CT, D0, date(2012, 4, 9)), W(time(8, 30), time(13, 20), CT, date(2012, 4, 9))]
       for r in ("ZC", "ZS", "ZW", "ZL", "ZM")},
    **{r: [W(time(9, 5), time(13, 0), CT, D0, date(2010, 6, 7)), W(time(8, 30), time(13, 5), CT, date(2010, 6, 7))] for r in ("LE", "GF")},
    "HE": [W(time(9, 10), time(13, 0), CT, D0, date(2010, 6, 7)), W(time(8, 30), time(13, 5), CT, date(2010, 6, 7))],
    **{r: [W(time(8, 30), time(15, 0), CT)] for r in ("BTC", "ETH")},
    "SB": [W(time(9, 0), time(13, 0), ET, D0, date(2008, 3, 3)), W(time(3, 30), time(13, 0), ET, date(2008, 3, 3))],
    "KC": [W(time(9, 0), time(13, 30), ET, D0, date(2008, 3, 3)), W(time(4, 15), time(13, 30), ET, date(2008, 3, 3))],
    "CC": [W(time(8, 30), time(13, 30), ET, D0, date(2008, 3, 3)), W(time(4, 45), time(13, 30), ET, date(2008, 3, 3))],
    "CT": [W(time(10, 30), time(14, 20), ET, D0, date(2008, 3, 3)), W(time(8, 0), time(14, 20), ET, date(2008, 3, 3))],
}


def to_ct(t: time, tz: str) -> time:
    if tz == ET:
        return (pd.Timestamp.combine(date(2000, 1, 3), t) - ET_OFFSET).time()
    return t


def log(msg: str) -> None:
    print(msg, file=sys.stderr, flush=True)


def panel(cache: Path, ticker: str):
    """build_panel_ts: the session grid (days x minutes) of open/high/low/close."""
    df = pd.read_parquet(cache / f"{ticker}.parquet")
    d = df["ts"].dt.date.to_numpy()
    tmin = (df["ts"].dt.hour * 60 + df["ts"].dt.minute).to_numpy()
    keep = np.zeros(len(df), bool)
    midx = np.full(len(df), -1)
    opens = {}
    for o, c, tz, f, u in SESSIONS[TS_TO_ROOT[ticker]]:
        o, c = to_ct(o, tz), to_ct(c, tz)
        om, cm = o.hour * 60 + o.minute, c.hour * 60 + c.minute
        era = (d >= f) & (d < u) & (tmin >= om) & (tmin < cm)
        keep |= era
        midx[era] = tmin[era] - om
        opens[(f, u)] = om
    df = df[keep].copy()
    df["session_day"] = d[keep]
    df["minute_idx"] = midx[keep]
    df = df[df["session_day"] <= HI]
    m = int(df.groupby("session_day")["minute_idx"].max().mode().iloc[0]) + 1
    days = np.array(sorted(df["session_day"].unique()))
    idx = {dd: i for i, dd in enumerate(days)}
    shape = (len(days), m)
    arrs = {k: np.full(shape, np.nan) for k in ("open", "high", "low", "close")}
    rows = df["session_day"].map(idx).to_numpy()
    cols = df["minute_idx"].to_numpy()
    ok = cols < m
    for k in arrs:
        arrs[k][rows[ok], cols[ok]] = df[k].to_numpy(float)[ok]
    # The window's open (minutes after midnight, Chicago) per day.
    open_min = np.array([next(om for (f, u), om in opens.items() if f <= dd < u) for dd in days])
    return days, open_min, arrs, m


def first_finite(a):
    has = np.isfinite(a)
    j = np.argmax(has, axis=1)
    out = a[np.arange(a.shape[0]), j]
    out[~has.any(axis=1)] = np.nan
    return out, np.where(has.any(axis=1), j, -1)


def last_finite(a):
    """The last finite value per row, and its column (-1 when none)."""
    has = np.isfinite(a)
    rev = has[:, ::-1]
    j = a.shape[1] - 1 - np.argmax(rev, axis=1)
    out = a[np.arange(a.shape[0]), j]
    none = ~has.any(axis=1)
    out[none] = np.nan
    j[none] = -1
    return out, j


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--cache", required=True, type=Path)
    ap.add_argument("--config", required=True, type=Path)
    ap.add_argument("--commissions", required=True, type=Path, help="JSON {root: commission per contract per side}")
    ap.add_argument("--out", required=True, type=Path)
    ap.add_argument("--universe", default="home26", choices=["home26"])
    args = ap.parse_args()
    started = _time.time()
    out = args.out
    out.mkdir(parents=True, exist_ok=True)
    specs = yaml.safe_load(open(args.config / "contract_specs.yaml"))
    specs = specs.get("specs", specs)
    comm = json.loads(args.commissions.read_text())
    tickers = [t for t, r in sorted(TS_TO_ROOT.items()) if r not in HOME26_EXCLUDED]
    report: dict = {"tickers": tickers, "roots": {}}
    es_days = None
    frames = {k: [] for k in ("session", "open_d", "high_d", "low_d", "close_d", "open0_m", "close20_m", "close30_m",
                              "open_m", "close_m", "clock", "multiplier")}
    clocks = []
    for t in sorted(tickers, key=lambda x: x != "ES"):
        root = TS_TO_ROOT[t]
        days, open_min, a, m = panel(args.cache, t)
        if root == "ES":
            es_days = set(days)
        dts = pd.to_datetime(pd.Series(days))
        base = dts.values.astype("datetime64[m]") + open_min.astype("timedelta64[m]")
        label = lambda j: base + (j + 1).astype("timedelta64[m]")  # close label of minute j
        o_d, _ = first_finite(a["open"])
        h_d = np.nanmax(a["high"], axis=1)
        l_d = np.nanmin(a["low"], axis=1)
        c_d, jx = last_finite(a["close"])
        D = dts.values.astype("datetime64[ns]")
        frames["session"].append(pd.DataFrame({"A": root, "T": D}))
        for rel, col, v in (("open_d", "O", o_d), ("high_d", "H", h_d), ("low_d", "L", l_d), ("close_d", "P", c_d)):
            ok = np.isfinite(v)
            frames[rel].append(pd.DataFrame({"A": root, "T": D[ok], col: v[ok]}))
        o0 = a["open"][:, 0]
        ok = np.isfinite(o0)
        frames["open0_m"].append(pd.DataFrame({"A": root, "T": label(np.zeros(len(days), int))[ok].astype("datetime64[ns]"), "P": o0[ok]}))
        for rel, k in (("close20_m", 20), ("close30_m", 30)):
            v, j = last_finite(a["close"][:, :k])
            ok = j >= 0
            frames[rel].append(pd.DataFrame({"A": root, "T": label(j)[ok].astype("datetime64[ns]"), "P": v[ok]}))
        # The entry: the first minute from 30 on that traded (its open); the exit:
        # the last traded minute (its close). Both are real prints of the grid.
        sub = a["open"][:, 30:] if m > 30 else np.full((len(days), 0), np.nan)
        eo, je = first_finite(sub) if sub.shape[1] else (np.full(len(days), np.nan), np.full(len(days), -1))
        je = np.where(je >= 0, je + 30, -1)
        entry_close = np.where(je >= 0, a["close"][np.arange(len(days)), np.clip(je, 0, m - 1)], np.nan)
        prints = []
        ok = je >= 0
        prints.append(pd.DataFrame({"A": root, "T": label(je)[ok].astype("datetime64[ns]"), "O": eo[ok], "P": entry_close[ok]}))
        ok = jx >= 0
        xo = a["open"][np.arange(len(days)), np.clip(jx, 0, m - 1)]
        prints.append(pd.DataFrame({"A": root, "T": label(jx)[ok].astype("datetime64[ns]"), "O": xo[ok], "P": c_d[ok]}))
        pr = pd.concat(prints).drop_duplicates(subset=["A", "T"])
        # A print whose open did not trade (a close-only minute) is marked at its close.
        pr["O"] = pr["O"].fillna(pr["P"])
        frames["open_m"].append(pr[["A", "T", "O"]])
        frames["close_m"].append(pr[["A", "T", "P"]])
        # The decision clock, for the sessions the protocol trades.
        floor = DEV_USABLE_FROM.get(root, 0)
        yr = dts.dt.year.to_numpy()
        clocks.append(pd.DataFrame({"A": root, "day": days, "T": (base + np.timedelta64(30, "m")).astype("datetime64[ns]"),
                                    "S": base.astype("datetime64[ns]"), "year": yr}))
        bpv = float(specs[root]["bpv"]) * TS_BPV_SCALE.get(root, 1.0)
        frames["multiplier"].append(pd.DataFrame({"A": root, "T": D, "M": bpv}))
        report["roots"][root] = {"ticker": t, "sessions": int(len(days)), "grid_minutes": m, "bpv_effective": bpv,
                                 "first": str(days[0]), "last": str(days[-1]), "usable_from": floor}
        log(f"{root:4s} {t:4s} sessions {len(days)} grid {m} bpv {bpv}")
    ck = pd.concat(clocks)
    ck = ck[ck["day"].map(lambda d: d in es_days) & (ck["day"] >= LO) & (ck["day"] <= HI)
            & (ck["year"] >= ck["A"].map(lambda r: DEV_USABLE_FROM.get(r, 0)))]
    frames["clock"].append(ck[["A", "T", "S"]])
    cal = sorted(d for d in es_days if LO <= d <= HI)
    frames["calendar"] = [pd.DataFrame({"T": pd.to_datetime(pd.Series(cal))})]
    for rel, parts in frames.items():
        f = pd.concat(parts, ignore_index=True)
        f.to_parquet(out / f"{rel}.parquet", index=False)
        report.setdefault("written", {})[rel] = int(len(f))
        log(f"wrote {rel}: {len(f)}")
    roots = sorted(report["roots"])
    sec = pd.DataFrame({"id": roots, "ticker": roots, "from": pd.Timestamp("1990-01-01"), "to": pd.NaT,
                        "multiplier": [report["roots"][r]["bpv_effective"] for r in roots],
                        "asset_class": "future", "commission_per_contract": [float(comm[r]) for r in roots]})
    sec.to_parquet(out / "securities.parquet", index=False)
    report["seconds"] = round(_time.time() - started, 1)
    (out / "ingest-report.json").write_text(json.dumps(report, indent=1, default=str))
    log(f"done in {report['seconds']} s")
    return 0


if __name__ == "__main__":
    sys.exit(main())
