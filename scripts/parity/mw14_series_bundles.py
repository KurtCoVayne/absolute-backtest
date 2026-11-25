#!/usr/bin/env python3
"""The two series MW14 reads that the lake does not hold, as two class-`series`
bundles the `bundle_v2` adapter ingests (`value(Name, T, V)`, `T` a nanosecond
timestamp, a manifest with `"tests": {}`):

- `series-spxtr`: the `$SPXTR` daily closes of the book's `spxtr_daily.parquet`,
  the close cast to float32 as the book's vendor export forces;
- `series-vix`: the weekly VIX of `vix_weekly.parquet`, keyed at the week's
  Friday as the v1 bundle keyed its weekly rows.

  python scripts/parity/mw14_series_bundles.py SPXTR.parquet VIX.parquet OUT
  abt data ingest --store S --adapter bundle_v2 --from OUT/series-spxtr --class series --base 1d
  abt data ingest --store S --adapter bundle_v2 --from OUT/series-vix   --class series --base 1w

The bundles are ingested without the catalog arguments, unlike every other
ingest. This is the script of the Nov 6 run (docs/handoff/benchmark.md), kept
in the repository so the benchmark stages its own store (scripts/bench-mw14.sh).
"""
import json
import os
import sys

import pandas as pd
import pyarrow as pa
import pyarrow.parquet as pq


def write(out: str, dirname: str, version: str, rows: pd.DataFrame, what: str) -> None:
    d = os.path.join(out, dirname)
    os.makedirs(os.path.join(d, "log", "value"), exist_ok=True)
    rows = rows.sort_values("T").reset_index(drop=True)
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
    s = pd.read_parquet(spx)
    s = s[s["symbol"] == "$SPXTR"]
    write(
        out,
        "series-spxtr",
        "mw14-spxtr",
        pd.DataFrame({"Name": "SPXTR", "T": pd.to_datetime(s["date"]), "V": s["close"].astype("float32").astype("float64")}),
        "SPXTR",
    )
    v = pd.read_parquet(vix)
    wk = pd.to_datetime(v["wk"])
    write(
        out,
        "series-vix",
        "mw14-vix",
        pd.DataFrame({"Name": "VIX", "T": wk + pd.Timedelta(days=4), "V": v["vix"].astype("float64")}).dropna(),
        "VIX (Friday-keyed)",
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
