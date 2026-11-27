#!/usr/bin/env python3
"""scripts/r8l-repair.py: whole session days where a Databento pull kept only a piece.

The script that repaired the pulls of Nov 26, 2026 on the pipeline machine (docs/handoff/benchmark.md, "R8L's
store"), with its places made variables. It runs there, beside the pulls: absolute-pipeline's month-chunked pull
(dbn_pull_extra.py) writes a session day's file from the first chunk that holds any of it and skips the day in the
next chunk, so a session that opens before midnight UTC on a month's last day keeps its first two hours only. This
asks the vendor for the two UTC days around such a day, through the pulls' own cost gate ($0.01 a request, d20-research's
`ingest._gate`), and writes the day whole, with the pulls' columns, dedupe and atomic files.

  r8l-repair.py boundary DAY...           every root of the refresh's jobs: the day's file in the refresh is replaced
                                          when the vendor's day has more rows than it (ONLY=ROOT,... for some roots)
  r8l-repair.py holes OUT ROOTS DAY...    ROOTS (comma-separated, each one's dataset by the refresh's jobs): the day is
                                          written under OUT, a pull of its own, when the vendor's has more rows than
                                          any pull's file of it

Env: DATABENTO_API_KEY, PYTHONPATH (d20-research's src), VENDOR (the directory of the pulls), REFRESH (the newest
pull's directory name), JOBS (the refresh's job files, "DATASET ROOT START" a line, separated by colons), WORKERS.
"""
from __future__ import annotations

import os
import sys
import tempfile
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import databento as db
import pandas as pd
import pyarrow.parquet as pq

from d20research.data import ingest, layout

V = Path(os.environ.get("VENDOR", "/srv/absolute/data/vendor"))
REFRESH = V / os.environ.get("REFRESH", "dbn-refresh-20261126")
JOBS = [Path(p) for p in os.environ.get("JOBS", "/srv/absolute/dbn-pull/refresh-20261126/jobs.txt:/srv/absolute/dbn-pull/refresh-20261126-r8l/jobs.txt").split(":")]
DATASET = {ln.split()[1]: ln.split()[0] for j in JOBS for ln in j.read_text().splitlines() if ln.strip()}


def rows_of(p: Path) -> int:
    try:
        return pq.read_metadata(p).num_rows
    except Exception:  # noqa: BLE001 - no file
        return 0


def day_frame(client: db.Historical, dataset: str, root: str, day: str) -> pd.DataFrame:
    s = (pd.Timestamp(day) - pd.Timedelta(days=1)).strftime("%Y-%m-%d")
    e = (pd.Timestamp(day) + pd.Timedelta(days=1)).strftime("%Y-%m-%d")
    ingest._gate(client, dataset=dataset, symbols=[f"{root}.FUT"], schema="ohlcv-1m", start=s, end=e, max_cost_usd=0.01)
    df = pd.DataFrame()
    for attempt in range(6):
        try:
            df = client.timeseries.get_range(dataset=dataset, symbols=[f"{root}.FUT"], schema="ohlcv-1m", start=s, end=e,
                                             stype_in="parent").to_df(price_type="float")
            break
        except db.BentoClientError as x:
            if "symbology_invalid_request" in str(x):
                return pd.DataFrame()
            if attempt == 5:
                raise
        except Exception:  # noqa: BLE001 - a server error, a stream cut short: retry
            if attempt == 5:
                raise
        time.sleep(10 * (attempt + 1))
    if df.empty:
        return df
    df = df.reset_index()
    df = df[[c for c in ingest.BAR_COLUMNS if c in df.columns]].drop_duplicates(subset=["instrument_id", "ts_event"], keep="first")
    df["volume"] = df["volume"].astype("uint64")
    df["instrument_id"] = df["instrument_id"].astype("uint32")
    df["symbol"] = df["symbol"].astype(str)
    df = df[layout.session_day(df["ts_event"], dataset).astype(str).to_numpy() == day]
    return df.sort_values(["ts_event", "instrument_id"]).reset_index(drop=True)


def write(df: pd.DataFrame, path: Path) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, tmp = tempfile.mkstemp(dir=path.parent, suffix=".tmp")
    os.close(fd)
    df.to_parquet(tmp, index=False)
    os.replace(tmp, path)


def one(job: tuple[str, str, Path | None]) -> str:
    root, day, out = job
    dataset = DATASET[root]
    client = db.Historical()
    try:
        df = day_frame(client, dataset, root, day)
    except ingest.CostGate as e:
        return f"{root} {day}: REFUSED by the cost gate: {e}"
    except Exception as e:  # noqa: BLE001 - one day's failure never stops the rest (a root not listed that day is one)
        return f"{root} {day}: FAILED {type(e).__name__}: {e}"
    if out is None:
        path = REFRESH / dataset / "ohlcv-1m" / f"{root}.FUT" / f"{day}.parquet"
        had = rows_of(path)
    else:
        path = out / dataset / "ohlcv-1m" / f"{root}.FUT" / f"{day}.parquet"
        had = max([rows_of(p) for p in V.glob(f"*/{dataset}/ohlcv-1m/{root}.FUT/{day}.parquet")] + [0])
    if len(df) > had:
        write(df, path)
        return f"{root} {day}: WRITTEN {len(df)} rows (had {had})"
    return f"{root} {day}: kept, the vendor's day has {len(df)} rows and a file {had}"


mode = sys.argv[1]
if mode == "boundary":
    jobs = [(r, d, None) for r in sorted(DATASET) for d in sys.argv[2:]]
    if os.environ.get("ONLY"):
        jobs = [j for j in jobs if j[0] in os.environ["ONLY"].split(",")]
else:
    out = Path(sys.argv[2])
    jobs = [(r, d, out) for r in sys.argv[3].split(",") for d in sys.argv[4:]]
n = {"WRITTEN": 0, "kept": 0, "other": 0}
with ThreadPoolExecutor(max_workers=int(os.environ.get("WORKERS", "3"))) as ex:
    for r in ex.map(one, jobs):
        n["WRITTEN" if "WRITTEN" in r else "kept" if ": kept" in r else "other"] += 1
        print(r, flush=True)
print(f"REPAIR-DONE {mode}: {len(jobs)} days asked, {n['WRITTEN']} written, {n['kept']} kept, {n['other']} refused or failed", flush=True)
