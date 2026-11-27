#!/usr/bin/env python3
"""scripts/r8l-repair.py: whole session days where a Databento pull kept only a piece.

The script that repaired the pulls of Nov 26, 2026 on the pipeline machine (docs/handoff/benchmark.md, "R8L's
store"), with its places made variables. It runs there, beside the pulls: absolute-pipeline's month-chunked pull
(dbn_pull_extra.py) writes a session day's file from the first chunk that holds any of it and skips the day in the
next chunk, so a session that opens before midnight UTC on a month's last day keeps its first two hours only. This
asks the vendor for the three UTC days around such a day (a New York day's evening is the next UTC day), through
the pulls' own cost gate ($0.01 a request, d20-research's `ingest._gate`), and writes the day whole, with the pulls'
columns, dedupe and atomic files.

The pulls' writer has a second fault (`ingest.ingest_bars`): it groups a chunk's rows by a day series numbered from
zero after dropping the vendor's duplicate bars, so the grouping is by row label and every row after a duplicate
takes the day of the row that many places later. Each day file from the first duplicate of a chunk on holds the
end of the day before and lacks its own end, and the chunk's last rows are written nowhere. Here a row's day is
taken by position. `span` writes a root's days again over such a stretch, and `count` holds every year of a root
against the vendor's own record count.

  r8l-repair.py boundary DAY...           every root of the refresh's jobs: the day's file in the refresh is replaced
                                          when the vendor's day has more rows than it (ONLY=ROOT,... for some roots)
  r8l-repair.py holes OUT ROOTS DAY...    ROOTS (comma-separated, each one's dataset by the refresh's jobs): the day is
                                          written under OUT, a pull of its own, when the vendor's has more rows than
                                          any pull's file of it
  r8l-repair.py pairs OUT FILE            the same for the days of FILE, a "ROOT DAY" a line (scripts/r8l-audit.py's
                                          candidates)
  r8l-repair.py span OUT ROOTS FIRST LAST every session day FIRST..LAST of ROOTS, one request a root and calendar year,
                                          each day whole under OUT; OUT/whole.json then names the root and the stretch, which
                                          scripts/r8l-views.py takes from OUT alone
  r8l-repair.py count OUT_FILE ROOT...    each root's rows in the pulls (a day's fullest file) against the vendor's
                                          record count, by year, then by halves down to the day where they differ;
                                          OUT_FILE gets "ROOT DAY" for each day the vendor counts more of (a `pairs`
                                          file)
  r8l-repair.py range                     what the vendor holds of each dataset of the jobs (its first and last time)

Env: DATABENTO_API_KEY, PYTHONPATH (d20-research's src), VENDOR (the directory of the pulls), REFRESH (the newest
pull's directory name), JOBS (the refresh's job files, "DATASET ROOT START" a line, separated by colons), WORKERS,
LAST (the last session day `count` reads, YYYY-MM-DD), YEARS (the years `count` asks, all of them unless given).
"""
from __future__ import annotations

import json
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


def vendor_range(client: db.Historical, dataset: str) -> tuple[pd.Timestamp, pd.Timestamp]:
    """The vendor's first and last time of the dataset's minute bars (UTC, no zone)."""
    r = client.metadata.get_dataset_range(dataset=dataset)
    r = r.get("schema", {}).get("ohlcv-1m", r)
    return tuple(pd.Timestamp(r[k]).tz_convert("UTC").tz_localize(None).floor("min") for k in ("start", "end"))


RANGE: dict[str, tuple[pd.Timestamp, pd.Timestamp]] = {}


def fetch(client: db.Historical, dataset: str, root: str, start: pd.Timestamp, end: pd.Timestamp) -> pd.DataFrame:
    """The vendor's bars of [start, end) UTC, cut to what it holds, with the pulls' columns and dedupe."""
    start, end = max(start, RANGE[dataset][0]), min(end, RANGE[dataset][1])
    if start >= end:
        return pd.DataFrame()
    s, e = start.strftime("%Y-%m-%dT%H:%M"), end.strftime("%Y-%m-%dT%H:%M")
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
    return df.sort_values(["ts_event", "instrument_id"]).reset_index(drop=True)


def day_frame(client: db.Historical, dataset: str, root: str, day: str) -> pd.DataFrame:
    df = fetch(client, dataset, root, pd.Timestamp(day) - pd.Timedelta(days=1), pd.Timestamp(day) + pd.Timedelta(days=2))
    if df.empty:
        return df
    # A row's day by position: the day series is numbered from zero, the frame's labels need not be.
    return df[layout.session_day(df["ts_event"], dataset).astype(str).to_numpy() == day].reset_index(drop=True)


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


def span(job: tuple[str, str, str, Path]) -> str:
    root, first, last, out = job
    dataset = DATASET[root]
    client = db.Historical()
    kept, rows = [], 0
    try:
        for y in range(int(first[:4]), int(last[:4]) + 1):  # a request a calendar year, as the pulls' writer asks
            a, b = max(first, f"{y}-01-01"), min(last, f"{y}-12-31")
            df = fetch(client, dataset, root, pd.Timestamp(a) - pd.Timedelta(days=1), pd.Timestamp(b) + pd.Timedelta(days=2))
            if df.empty:
                continue
            days = layout.session_day(df["ts_event"], dataset).astype(str).to_numpy()
            for d in sorted(x for x in set(days) if a <= x <= b):
                write(df[days == d].reset_index(drop=True), out / dataset / "ohlcv-1m" / f"{root}.FUT" / f"{d}.parquet")
                kept.append(d)
                rows += int((days == d).sum())
    except ingest.CostGate as e:
        return f"{root} {first}..{last}: REFUSED by the cost gate: {e}"
    except Exception as e:  # noqa: BLE001
        return f"{root} {first}..{last}: FAILED {type(e).__name__}: {e}"
    if not kept:
        return f"{root} {first}..{last}: FAILED the vendor has no bars"
    return f"{root} {first}..{last}: WHOLE {len(kept)} days, {rows} rows, {kept[0]} to {kept[-1]}"


def session_start(dataset: str, day: str) -> pd.Timestamp:
    """A session day's first instant by the pulls' rule (UTC, no zone)."""
    tz, roll = layout._TZ[dataset]
    return (pd.Timestamp(day).tz_localize(tz) - pd.Timedelta(hours=roll)).tz_convert("UTC").tz_localize(None)


def vendor_rows(client: db.Historical, dataset: str, root: str, a: str, b: str) -> int:
    """The vendor's record count of session days a (in) to b (out)."""
    s, e = max(session_start(dataset, a), RANGE[dataset][0]), min(session_start(dataset, b), RANGE[dataset][1])
    if s >= e:
        return 0
    for attempt in range(6):
        try:
            return int(client.metadata.get_record_count(dataset=dataset, symbols=[f"{root}.FUT"], schema="ohlcv-1m",
                                                        start=s.strftime("%Y-%m-%dT%H:%M"), end=e.strftime("%Y-%m-%dT%H:%M"), stype_in="parent"))
        except db.BentoClientError as x:
            if "symbology_invalid_request" in str(x):
                return 0
            if attempt == 5:
                raise
        except Exception:  # noqa: BLE001
            if attempt == 5:
                raise
        time.sleep(5 * (attempt + 1))
    return 0


def count(root: str) -> tuple[str, list[str]]:
    dataset = DATASET[root]
    client = db.Historical()
    held: dict[str, int] = {}
    for f in V.glob(f"*/{dataset}/ohlcv-1m/{root}.FUT/*.parquet"):
        held[f.stem] = max(held.get(f.stem, 0), rows_of(f))
    last = os.environ.get("LAST", "2026-11-26")
    stop = (pd.Timestamp(last) + pd.Timedelta(days=1)).strftime("%Y-%m-%d")
    whole = [w.get(root) for w in (json.loads(p.read_text()) for p in V.glob("*/whole.json")) if w.get(root)]
    if whole:  # a stretch written again whole is not the pulls' to answer for
        stop = min(stop, min(w[0] for w in whole))
    have = lambda a, b: sum(n for d, n in held.items() if a <= d < b)
    more, fewer, asked = [], [], [0]

    def differ(a: pd.Timestamp, b: pd.Timestamp) -> None:
        """The days of [a, b) on which the vendor's count is not the pulls', by halving."""
        da, db_ = a.strftime("%Y-%m-%d"), b.strftime("%Y-%m-%d")
        asked[0] += 1
        v, h = vendor_rows(client, dataset, root, da, db_), have(da, db_)
        if v == h:
            return
        if (b - a).days == 1:
            (more if v > h else fewer).append(f"{da} {v}/{h}")
            print(f"{root} {da}: the vendor counts {v}, the pulls hold {h}", flush=True)
        elif asked[0] > 1500:  # a root this far from the vendor's counts is not a matter of days
            more.append(f"{da}..{db_} (no day asked after 1500 counts)")
        else:
            mid = a + pd.Timedelta(days=(b - a).days // 2)
            differ(a, mid)
            differ(mid, b)

    try:
        years = [int(y) for y in os.environ["YEARS"].split(",")] if os.environ.get("YEARS") else range(int(min(held)[:4]), int(stop[:4]) + 1)
        for y in years:
            a, b = f"{y}-01-01", min(f"{y + 1}-01-01", stop)
            if a < b:
                differ(pd.Timestamp(a), pd.Timestamp(b))
    except Exception as e:  # noqa: BLE001
        return f"{root}: FAILED {type(e).__name__}: {e}", []
    line = f"{root}: {len(held)} days to {max(d for d in held if d < stop)}, {have('0', stop)} rows in the pulls; {asked[0]} counts asked; the vendor counts more on {len(more)} days, fewer on {len(fewer)}"
    if more or fewer:
        line += f" (vendor/pulls: more {' '.join(more[:40])}{' ...' if len(more) > 40 else ''}; fewer {' '.join(fewer[:12])}{' ...' if len(fewer) > 12 else ''})"
    return line, [f"{root} {x.split()[0]}" for x in more if len(x.split()[0]) == 10]


mode = sys.argv[1]
RANGE.update({ds: vendor_range(db.Historical(), ds) for ds in sorted(set(DATASET.values()))})
if mode == "range":
    for ds, (a, b) in RANGE.items():
        print(f"{ds} ohlcv-1m: {a} to {b} UTC", flush=True)
    sys.exit(0)
if mode == "count":
    suspects: list[str] = []
    with ThreadPoolExecutor(max_workers=int(os.environ.get("WORKERS", "3"))) as ex:
        for line, days in ex.map(count, sys.argv[3:]):
            suspects += days
            print(line, flush=True)
    Path(sys.argv[2]).write_text("".join(x + "\n" for x in suspects))
    print(f"REPAIR-DONE count: {len(sys.argv) - 3} roots, {len(suspects)} days the vendor counts more of", flush=True)
    sys.exit(0)
if mode == "span":
    out, first, last = Path(sys.argv[2]), sys.argv[4], sys.argv[5]
    done = {}
    with ThreadPoolExecutor(max_workers=int(os.environ.get("WORKERS", "3"))) as ex:
        for root, r in zip(sys.argv[3].split(","), ex.map(span, [(r, first, last, out) for r in sys.argv[3].split(",")])):
            if "WHOLE" in r:
                done[root] = [first, last]
            print(r, flush=True)
    marker = out / "whole.json"
    was = json.loads(marker.read_text()) if marker.exists() else {}
    marker.parent.mkdir(parents=True, exist_ok=True)
    marker.write_text(json.dumps({**was, **done}, indent=1, sort_keys=True))
    print(f"REPAIR-DONE span: {len(done)} roots whole over {first}..{last}", flush=True)
    sys.exit(0)
if mode == "boundary":
    jobs = [(r, d, None) for r in sorted(DATASET) for d in sys.argv[2:]]
    if os.environ.get("ONLY"):
        jobs = [j for j in jobs if j[0] in os.environ["ONLY"].split(",")]
elif mode == "pairs":
    out = Path(sys.argv[2])
    jobs = [(ln.split()[0], ln.split()[1], out) for ln in Path(sys.argv[3]).read_text().splitlines() if ln.strip()]
else:
    out = Path(sys.argv[2])
    jobs = [(r, d, out) for r in sys.argv[3].split(",") for d in sys.argv[4:]]
n = {"WRITTEN": 0, "kept": 0, "other": 0}
with ThreadPoolExecutor(max_workers=int(os.environ.get("WORKERS", "3"))) as ex:
    for r in ex.map(one, jobs):
        n["WRITTEN" if "WRITTEN" in r else "kept" if ": kept" in r else "other"] += 1
        print(r, flush=True)
print(f"REPAIR-DONE {mode}: {len(jobs)} days asked, {n['WRITTEN']} written, {n['kept']} kept, {n['other']} refused or failed", flush=True)
