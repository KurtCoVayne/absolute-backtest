#!/usr/bin/env python3
"""scripts/r8l-audit.py OUT_JSON ROOT...: the session days of a root's pulls that look cut or are framed off their
day, from the files' footers.

Two checks. The first is exact: a file's first and last bar belong to the day it is named for, by the pulls' own
rule (d20-research's layout: Chicago time seven hours on for CME's datasets, New York's calendar day for ICE's). A
file that fails it was framed by a shifted grouping (scripts/r8l-repair.py's head) and holds another day's bars;
OUT_JSON lists them under `off_frame` (pull/root: days), and scripts/r8l-repair.py span writes the stretch again.

The second is a guess the vendor settles. For each root, each day's file is the one scripts/r8l-views.py takes (the
one with more rows among the pulls that hold the day, and a stretch written again whole from that pull alone). A day is a candidate when its file is short beside the six before and the six after it that fall on
the same weekday (a root's Monday can open later than its other days): under a quarter of their median rows, or
its last bar more than two hours before theirs, or its first more than two hours after. A holiday's early close looks the same in a footer, so the vendor is the judge: scripts/r8l-repair.py asks it
for each candidate and writes the day only where it holds more rows than the files do.

A day short in most of the roots at once is a holiday or the vendor's own outage: OUT_JSON lists those under `wide`
(day: roots) so that a few roots can be asked first, and the others under `narrow` ([root, day]).

Env: as scripts/r8l-views.py (VENDOR, REFRESH, REPAIR, REFRAME, LAST, FILTERED).
"""
import json
import os
import statistics
import sys
from datetime import date, datetime, timedelta, timezone
from pathlib import Path
from zoneinfo import ZoneInfo

import pyarrow.parquet as pq

V = Path(os.environ.get("VENDOR", "/srv/absolute/data/vendor"))
REFRESH = V / os.environ.get("REFRESH", "dbn-refresh-20261126")
REPAIR = V / os.environ.get("REPAIR", "dbn-repair-20261126")
REFRAME = V / os.environ.get("REFRAME", "dbn-reframe-20261127")
LAST = os.environ.get("LAST", "2026-11-26")
SRC = {
    "GLBX.MDP3": [V / "dbn/GLBX.MDP3", V / "dbn-live/GLBX.MDP3", V / "dbn-extra/GLBX.MDP3", REFRESH / "GLBX.MDP3", REPAIR / "GLBX.MDP3", REFRAME / "GLBX.MDP3"],
    "IFUS.IMPACT": [V / "dbn-ifus/IFUS.IMPACT", V / "dbn-extra/IFUS.IMPACT", REFRESH / "IFUS.IMPACT", REPAIR / "IFUS.IMPACT", REFRAME / "IFUS.IMPACT"],
}
WHOLE = json.loads((REFRAME / "whole.json").read_text()) if (REFRAME / "whole.json").exists() else {}
# The pulls' day rule (d20-research d20research/data/layout.py): the zone, and the hours a bar's local time is moved on.
RULE = {"GLBX.MDP3": ("America/Chicago", 7), "IFUS.IMPACT": ("America/New_York", 0)}
FILTERED = Path(os.environ.get("FILTERED", "/srv/absolute/full/outrights/GLBX.MDP3/ohlcv-1m"))


def footer(f: Path):
    """Rows, first and last bar (minutes from the Unix epoch), from the file's footer."""
    m = pq.read_metadata(f)
    i = m.schema.names.index("ts_event") if "ts_event" in m.schema.names else None
    lo = hi = None
    if i is not None:
        for g in range(m.num_row_groups):
            st = m.row_group(g).column(i).statistics
            if st is None or not st.has_min_max:
                continue
            a, b = (x.timestamp() / 60 if isinstance(x, datetime) else (x.value if hasattr(x, "value") else int(x)) / 60e9 for x in (st.min, st.max))
            lo, hi = (a if lo is None else min(lo, a)), (b if hi is None else max(hi, b))
    return m.num_rows, lo, hi


def day_of(minutes: float, ds: str) -> str:
    zone, roll = RULE[ds]
    return (datetime.fromtimestamp(minutes * 60, timezone.utc).astimezone(ZoneInfo(zone)) + timedelta(hours=roll)).date().isoformat()


def days_of(root: str) -> tuple[dict[str, tuple[int, float | None, float | None]], dict[str, list[str]]]:
    """The file the view takes of each day (rows, first and last bar), and each pull's files framed off their day."""
    ds = next((d for d, dirs in SRC.items() if any((s / "ohlcv-1m" / f"{root}.FUT").is_dir() for s in dirs[:-3])), None)
    if ds is None:
        sys.exit(f"{root}: no history in {V}")
    best: dict[str, tuple[int, float | None, float | None]] = {}
    off: dict[str, list[str]] = {}
    first, last = WHOLE.get(root, ("9", "0"))
    for i, s in enumerate(SRC[ds]):
        d = s / "ohlcv-1m" / f"{root}.FUT"
        if i == 0 and ds == "GLBX.MDP3" and (FILTERED / f"{root}.FUT.done").exists():
            d = FILTERED / f"{root}.FUT"
        for f in sorted(d.glob("*.parquet")) if d.is_dir() else []:
            if f.stem > LAST:
                continue
            got = footer(f)
            if got[1] is not None and (day_of(got[1], ds) != f.stem or day_of(got[2], ds) != f.stem):
                off.setdefault(f"{s.parent.name}/{root}", []).append(f.stem)
            if i < len(SRC[ds]) - 1 and first <= f.stem <= last:
                continue
            if f.stem not in best or got[0] >= best[f.stem][0]:
                best[f.stem] = got
    return best, off


def midnight(day: str) -> float:
    y, m, d = map(int, day.split("-"))
    return datetime(y, m, d, tzinfo=timezone.utc).timestamp() / 60


out, roots = sys.argv[1], sys.argv[2:]
flagged: dict[str, list[str]] = {}
live: dict[str, int] = {}
off_frame: dict[str, list[str]] = {}
for root in roots:
    best, off = days_of(root)
    off_frame.update(off)
    days = [d for d in sorted(best) if date.fromisoformat(d).weekday() < 5]
    n = 0
    for d in days:
        live[d] = live.get(d, 0) + 1
    for wd in range(5):
        same = [d for d in days if date.fromisoformat(d).weekday() == wd]
        for k, d in enumerate(same):
            near = [(x, best[x]) for x in same[max(0, k - 6):k] + same[k + 1:k + 7]]
            if len(near) < 6:
                continue
            rows, lo, hi = best[d]
            short = rows < 0.25 * statistics.median(x[0] for _, x in near)
            # The first and last bar as minutes from the day's midnight UTC, beside the neighbours' medians.
            ends = [x[2] - midnight(day) for day, x in near if x[2] is not None]
            starts = [x[1] - midnight(day) for day, x in near if x[1] is not None]
            if lo is not None and hi is not None and ends and starts:
                short = short or hi - midnight(d) < statistics.median(ends) - 120 or lo - midnight(d) > statistics.median(starts) + 120
            if short:
                flagged.setdefault(d, []).append(root)
                n += 1
    framed = "; ".join(f"{k.split('/')[0]} {len(v)} ({v[0]} to {v[-1]})" for k, v in off.items()) or "none"
    print(f"{root}: {len(days)} weekdays with a file, {n} short beside their neighbours; files framed off their day: {framed}", flush=True)
wide = {d: r for d, r in sorted(flagged.items()) if len(r) >= 0.6 * live[d] and len(r) >= 5}
narrow = [[r, d] for d, rs in sorted(flagged.items()) if d not in wide for r in rs]
json.dump({"wide": wide, "narrow": narrow, "off_frame": off_frame}, open(out, "w"))
print(f"framed off their day: {sum(len(v) for v in off_frame.values())} files in {len(off_frame)} pulls of a root")
print(f"candidates: {sum(len(r) for r in flagged.values())} root-days on {len(flagged)} days; {len(wide)} days short in most roots ({sum(len(r) for r in wide.values())} root-days), {len(narrow)} root-days otherwise")
