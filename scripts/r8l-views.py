#!/usr/bin/env python3
"""scripts/r8l-views.py OUT_DIR ROOT...: a futures root's Databento files, from every pull of them, as one view.

scripts/r8l-store.sh calls it. It is absolute-pipeline's scripts/store/combine_views.py with more sources and
ICE: one ingest of the view then rolls and back-adjusts the root's whole span.

The day files are linked from the history's outright-filtered files (or the vendor's), from live/, from the
extra pull, from the newest pull and from the repair of the days a pull cut. Where two hold a day the file
with more rows is taken, and the later source's when they hold as many: a day live/ recorded in part (its
last) is then the newest pull's whole one, and a session day a pull chunked by month kept only the first piece
of (the two hours before midnight UTC of a month's last day; docs/handoff/benchmark.md, "R8L's store") is the
whole one of whichever pull has it. A stretch of a root written again whole (scripts/r8l-repair.py span: the days
a pull framed off their day, REFRAME/whole.json) is taken from that pull alone, since a file of the others there
holds another day's bars. A file after LAST, the last whole session, is left out. The definitions are the files'
union, an instrument defined in several kept once (the latest file's row).

Env: VENDOR (the directory of the pulls, /srv/absolute/data/vendor), REFRESH (the newest pull's directory
name), REPAIR (the repair's, where there is one), REFRAME (the pull of the stretches written again whole, where
there is one), LAST (the last whole session, YYYY-MM-DD), FILTERED (the outright-filtered history, where there
is one).
"""
import json
import os
import sys
from pathlib import Path

import pyarrow as pa
import pyarrow.parquet as pq

V = Path(os.environ.get("VENDOR", "/srv/absolute/data/vendor"))
REFRESH = V / os.environ.get("REFRESH", "dbn-refresh-20261126")
REPAIR = V / os.environ.get("REPAIR", "dbn-repair-20261126")
REFRAME = V / os.environ.get("REFRAME", "dbn-reframe-20261127")
LAST = os.environ.get("LAST", "2026-11-26")
# The last three of each are the newest pull, the repair and the stretches written again whole; the others are
# the history a root must have.
SRC = {
    "GLBX.MDP3": [V / "dbn/GLBX.MDP3", V / "dbn-live/GLBX.MDP3", V / "dbn-extra/GLBX.MDP3", REFRESH / "GLBX.MDP3", REPAIR / "GLBX.MDP3", REFRAME / "GLBX.MDP3"],
    "IFUS.IMPACT": [V / "dbn-ifus/IFUS.IMPACT", V / "dbn-extra/IFUS.IMPACT", REFRESH / "IFUS.IMPACT", REPAIR / "IFUS.IMPACT", REFRAME / "IFUS.IMPACT"],
}
WHOLE = json.loads((REFRAME / "whole.json").read_text()) if (REFRAME / "whole.json").exists() else {}
FILTERED = Path(os.environ.get("FILTERED", "/srv/absolute/full/outrights/GLBX.MDP3/ohlcv-1m"))

out = Path(sys.argv[1])
for root in sys.argv[2:]:
    ds = next((d for d, dirs in SRC.items() if any((s / "ohlcv-1m" / f"{root}.FUT").is_dir() for s in dirs[:-3])), None)
    if ds is None:
        sys.exit(f"{root}: no history in {V}")
    days = out / root / "ohlcv-1m" / f"{root}.FUT"
    days.mkdir(parents=True, exist_ok=True)
    for f in days.iterdir():
        f.unlink()
    names, per, fuller = {}, [], 0
    first, last = WHOLE.get(root, ("9", "0"))
    for i, s in enumerate(SRC[ds]):
        d = s / "ohlcv-1m" / f"{root}.FUT"
        if i == 0 and ds == "GLBX.MDP3" and (FILTERED / f"{root}.FUT.done").exists():
            d = FILTERED / f"{root}.FUT"
        fs = [f for f in sorted(d.glob("*.parquet")) if f.stem <= LAST] if d.is_dir() else []
        if i < len(SRC[ds]) - 1:  # the stretch written again whole is that pull's alone
            fs = [f for f in fs if not first <= f.stem <= last]
        for f in fs:
            held = names.get(f.name)
            # Two sources hold the day: the one with more rows (the files' footers), the later on a tie.
            if held is not None and pq.read_metadata(held[1]).num_rows > pq.read_metadata(f).num_rows:
                fuller += 1
                continue
            names[f.name] = (i, f)
        per.append((d.parent.parent.parent.name if "outrights" not in str(d) else "outrights", len(fs), fs[0].stem if fs else "-", fs[-1].stem if fs else "-"))
    for name, (_, f) in names.items():
        (days / name).symlink_to(f)
    defs = [pq.read_table(p) for p in (s / "definition" / f"{root}.parquet" for s in SRC[ds]) if p.exists()]
    t = pa.concat_tables(defs, promote_options="permissive") if len(defs) > 1 else defs[0]
    ids = t.column("instrument_id").to_pylist()
    last = {i: k for k, i in enumerate(ids)}
    t = t.take(pa.array(sorted(last.values())))
    (out / root / "definition").mkdir(parents=True, exist_ok=True)
    pq.write_table(t, out / root / "definition" / f"{root}.parquet")
    taken = [sum(1 for i, _ in names.values() if i == k) for k in range(len(SRC[ds]))]
    ordered = sorted(names)
    recent = [n for n in ordered if n >= "2026-08-03"]
    print(f"{root} {ds}: {len(names)} day files {ordered[0][:10]} to {ordered[-1][:10]} ({len(recent)} from Aug 3), {t.num_rows} definitions; taken per source {taken} ({fuller} days an earlier source holds more of); sources {per}")
