#!/usr/bin/env python3
"""The series MW14 reads that the lake does not hold, as two class-`series`
bundles the `bundle_v2` adapter ingests (`value(Name, T, V)`, `T` a nanosecond
timestamp, a manifest with `"tests": {}`):

- `series-spxtr`: the `$SPXTR` daily closes of the book's `spxtr_daily.parquet`,
  the close cast to float32 as the book's vendor export forces;
- `series-weekly`: four weekly series, each row keyed at its week's last
  trading day, the last `$SPXTR` date of the ISO week, which is the label the
  kernel gives the week (a holiday Friday's week is labelled Thursday):
  - `VIX` and `VIX_MA4`: the weekly VIX of `vix_weekly.parquet` and the
    mart's own 4-week mean of it, which the book joins on the week (`vix.py`,
    `book.py` 338-351);
  - `SPXTR_W` and `SPXTR_W_MA52`: the book's weekly total-return index of
    `$SPXTR` (`data.py` `attach_gate_index`: the daily close ratios in
    float32, their product over the week and the running product of those)
    and its 52-week mean (`book.py` 334), both from the file's first week
    (1988), so the gate is defined from 1990's first week, as the book's.

  python scripts/parity/mw14_series_bundles.py SPXTR.parquet VIX.parquet OUT
  abt data ingest --store S --adapter bundle_v2 --from OUT/series-spxtr  --class series --base 1d
  abt data ingest --store S --adapter bundle_v2 --from OUT/series-weekly --class series --base 1w

The bundles are ingested without the catalog arguments, unlike every other
ingest. scripts/bench-mw14.sh stages its store with this script;
scripts/parity/test_mw14_series_bundles.py tests the frames it writes.
"""
from __future__ import annotations

import json
import math
import os
import sys

import numpy as np
import pandas as pd
import pyarrow as pa
import pyarrow.parquet as pq

# The book's gate window, in weeks (book.py `gate_ma`).
GATE_WEEKS = 52


def spxtr_daily(frame: pd.DataFrame) -> pd.DataFrame:
    """The `$SPXTR` rows of the book's export: `date` (a day) and `close` in
    float32, in date order."""
    s = frame[frame["symbol"] == "$SPXTR"]
    out = pd.DataFrame({"date": pd.to_datetime(s["date"]).dt.normalize(), "close": s["close"].astype("float32")})
    return out.sort_values("date", kind="stable").reset_index(drop=True)


def week_of(days: pd.Series) -> pd.Series:
    """The ISO week of each day as its Monday (polars' `truncate("1w")`)."""
    return days - pd.to_timedelta(days.dt.weekday, unit="D")


def week_ends(daily: pd.DataFrame) -> pd.Series:
    """The last `$SPXTR` date of each ISO week, by the week's Monday."""
    d = daily.assign(wk=week_of(daily["date"]))
    return d.groupby("wk")["date"].max()


def gate_index(daily: pd.DataFrame) -> pd.DataFrame:
    """The book's gate frame: per ISO week of `$SPXTR`, its last date
    (`week_end`), the weekly total-return index `tri` and its 52-week mean
    `ma` (NaN for the first 51 weeks).

    As `attach_gate_index` computes it: the daily ratio close / previous
    close in float32 (none on the first day), the product of a week's ratios
    in float32 taken in date order (1 for a week with none), the running
    product of the weekly factors in float32, cast to float64. The mean is
    over the index's own weeks; a sum of 52 float32 values is exact in
    float64, so the mean is the book's to the bit whatever the order of
    summation.
    """
    close = daily["close"].to_numpy(np.float32)
    ratio = np.full(len(close), np.nan, np.float32)
    ratio[1:] = close[1:] / close[:-1]
    wk = week_of(daily["date"]).to_numpy()
    weeks, starts = np.unique(wk, return_index=True)
    ends = list(starts[1:]) + [len(close)]
    factors = []
    for a, b in zip(starts, ends):
        f = None
        for x in ratio[a:b]:
            if np.isnan(x):
                continue
            f = x if f is None else np.float32(f * x)
        factors.append(np.float32(1.0) if f is None else f)
    tri = np.empty(len(factors), np.float32)
    acc = None
    for i, f in enumerate(factors):
        acc = f if acc is None else np.float32(acc * f)
        tri[i] = acc
    tri64 = tri.astype(np.float64)
    ma = np.full(len(tri64), np.nan)
    for i in range(GATE_WEEKS - 1, len(tri64)):
        ma[i] = math.fsum(tri64[i - GATE_WEEKS + 1 : i + 1]) / GATE_WEEKS
    ends_by_week = week_ends(daily)
    return pd.DataFrame({"wk": pd.to_datetime(weeks), "week_end": ends_by_week.loc[pd.to_datetime(weeks)].to_numpy(), "tri": tri64, "ma": ma})


def weekly_rows(gate: pd.DataFrame, vix: pd.DataFrame, ends: pd.Series) -> tuple[pd.DataFrame, int]:
    """The rows of the four weekly series (`Name`, `T`, `V`), each keyed at
    its week's last `$SPXTR` date; a VIX week with no `$SPXTR` date is keyed
    at its Friday, and their count is returned with the rows. A missing
    value has no row: the program's as-of read then takes the latest
    earlier one, as the book fills the joined frame forward."""
    v = vix.assign(wk=pd.to_datetime(vix["wk"]).dt.normalize())
    key = v["wk"].map(ends)
    unkeyed = int(key.isna().sum())
    key = key.fillna(v["wk"] + pd.Timedelta(days=4))
    parts = [
        pd.DataFrame({"Name": "VIX", "T": key, "V": v["vix"].astype("float64")}),
        pd.DataFrame({"Name": "VIX_MA4", "T": key, "V": v["vix_ma4"].astype("float64")}),
        pd.DataFrame({"Name": "SPXTR_W", "T": gate["week_end"], "V": gate["tri"]}),
        pd.DataFrame({"Name": "SPXTR_W_MA52", "T": gate["week_end"], "V": gate["ma"]}),
    ]
    rows = pd.concat(parts, ignore_index=True).dropna()
    return rows.sort_values(["T", "Name"], kind="stable").reset_index(drop=True), unkeyed


def write(out: str, dirname: str, version: str, rows: pd.DataFrame, what: str) -> None:
    d = os.path.join(out, dirname)
    os.makedirs(os.path.join(d, "log", "value"), exist_ok=True)
    rows = rows.sort_values("T", kind="stable").reset_index(drop=True)
    parts = []
    for ym, g in rows.groupby(rows["T"].dt.strftime("%Y-%m")):
        p = f"log/value/{ym}.parquet"
        t = pa.table(
            {
                "Name": pa.array(g["Name"], pa.string()),
                "T": pa.array(g["T"].values.astype("datetime64[ns]"), pa.timestamp("ns")),
                "V": pa.array(g["V"].astype("float64"), pa.float64()),
            }
        )
        pq.write_table(t, os.path.join(d, p))
        parts.append(p)
    manifest = {
        "format": 2,
        "name": "series",
        "version": version,
        "as_of": "2026-07-31",
        "processing_delay_seconds": 0,
        "source": None,
        # Fixed, so that two stagings of the same inputs give the same bundle.
        "created_at": "2026-11-06",
        "tests": {},
        "relations": [
            {
                "name": "value",
                "signature": "value(+Name: Label, @T: Timestamp, -V: Scalar)",
                "resolution": "@1d",
                "availability": "bar_close",
                "rows": int(len(rows)),
                "partitions": parts,
            }
        ],
    }
    with open(os.path.join(d, "manifest.json"), "w") as f:
        json.dump(manifest, f, indent=1)
    print(dirname, what, len(rows), rows["T"].min().date(), rows["T"].max().date(), len(parts), "partitions")


def main() -> int:
    if len(sys.argv) != 4:
        print(__doc__, file=sys.stderr)
        return 2
    spx, vix, out = sys.argv[1:4]
    daily = spxtr_daily(pd.read_parquet(spx))
    write(
        out,
        "series-spxtr",
        "mw14-spxtr",
        pd.DataFrame({"Name": "SPXTR", "T": daily["date"], "V": daily["close"].astype("float64")}),
        "SPXTR",
    )
    ends = week_ends(daily)
    rows, unkeyed = weekly_rows(gate_index(daily), pd.read_parquet(vix), ends)
    write(out, "series-weekly", "mw14-weekly", rows, "VIX, VIX_MA4, SPXTR_W, SPXTR_W_MA52")
    short = int((ends.dt.weekday != 4).sum())
    print(f"weeks whose last $SPXTR date is not a Friday: {short}; VIX weeks with no $SPXTR date (keyed at the Friday): {unkeyed}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
