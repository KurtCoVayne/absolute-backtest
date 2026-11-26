#!/usr/bin/env python3
"""scripts/r8l-views.py OUT_DIR ROOT...: a futures root's Databento files, from every pull of them, as one view.

scripts/r8l-store.sh calls it. It is absolute-pipeline's scripts/store/combine_views.py with more sources and
ICE: one ingest of the view then rolls and back-adjusts the root's whole span.

The day files are linked from the history's outright-filtered files (or the vendor's), from live/, from the
extra pull and from the newest pull. Where two hold a day the later source's file is taken, so a day live/
recorded in part (its last) is the newest pull's whole one. A file after LAST, the session the pull was made
in, is left out. The definitions are the files' union, an instrument defined in several kept once (the latest
file's row).

Env: VENDOR (the directory of the pulls, /srv/absolute/data/vendor), REFRESH (the newest pull's directory
name), LAST (the last whole session, YYYY-MM-DD), FILTERED (the outright-filtered history, where there is one).
"""
import os
import sys
from pathlib import Path

import pyarrow as pa
import pyarrow.parquet as pq

V = Path(os.environ.get("VENDOR", "/srv/absolute/data/vendor"))
REFRESH = V / os.environ.get("REFRESH", "dbn-refresh-20261126")
LAST = os.environ.get("LAST", "2026-11-25")
SRC = {
    "GLBX.MDP3": [V / "dbn/GLBX.MDP3", V / "dbn-live/GLBX.MDP3", V / "dbn-extra/GLBX.MDP3", REFRESH / "GLBX.MDP3"],
    "IFUS.IMPACT": [V / "dbn-ifus/IFUS.IMPACT", V / "dbn-extra/IFUS.IMPACT", REFRESH / "IFUS.IMPACT"],
}
FILTERED = Path(os.environ.get("FILTERED", "/srv/absolute/full/outrights/GLBX.MDP3/ohlcv-1m"))

out = Path(sys.argv[1])
for root in sys.argv[2:]:
    ds = next((d for d, dirs in SRC.items() if any((s / "ohlcv-1m" / f"{root}.FUT").is_dir() for s in dirs[:-1])), None)
    if ds is None:
        sys.exit(f"{root}: no history in {V}")
    days = out / root / "ohlcv-1m" / f"{root}.FUT"
    days.mkdir(parents=True, exist_ok=True)
    for f in days.iterdir():
        f.unlink()
    names, per = {}, []
    for i, s in enumerate(SRC[ds]):
        d = s / "ohlcv-1m" / f"{root}.FUT"
        if i == 0 and ds == "GLBX.MDP3" and (FILTERED / f"{root}.FUT.done").exists():
            d = FILTERED / f"{root}.FUT"
        fs = [f for f in sorted(d.glob("*.parquet")) if f.stem <= LAST] if d.is_dir() else []
        for f in fs:
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
    print(f"{root} {ds}: {len(names)} day files {ordered[0][:10]} to {ordered[-1][:10]} ({len(recent)} from Aug 3), {t.num_rows} definitions; taken per source {taken}; sources {per}")
