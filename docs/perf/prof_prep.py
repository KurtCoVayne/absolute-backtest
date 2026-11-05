"""Prepare the external-attribution cells: universe subsets and DSL ablations.

Both kinds of cell produce WRONG returns by construction -- a subset moves every
cross-sectional rank, a stub changes the book. They are performance cells only.

  python prof_prep.py subset --inputs IN --out DIR --names N
  python prof_prep.py ablate --dsl corpus/company/mw14.dsl --out DIR
"""
from __future__ import annotations

import argparse
import json
import shutil
import sys
from pathlib import Path

import pandas as pd

# A-keyed input relations (the gate's `series` is not keyed by asset).
A_KEYED = ["universe", "close", "trclose", "dvol", "split", "member", "delisted"]


def subset(inputs: Path, out: Path, n: int) -> dict:
    out.mkdir(parents=True, exist_ok=True)
    mem = pd.read_parquet(inputs / "member.parquet")
    # The N assets with the most index-member weeks: keeps the cross-section dense
    # rather than mostly empty, so the rung measures width, not sparsity.
    keep = set(mem["A"].value_counts().head(n).index)
    assert len(keep) == min(n, mem["A"].nunique()), (len(keep), n)
    rows = {}
    for f in sorted(inputs.glob("*.parquet")):
        name = f.stem
        df = pd.read_parquet(f)
        if name in A_KEYED:
            df = df[df["A"].isin(keep)]
        elif name == "securities":
            df = df[df["id"].isin(keep)]
        df.to_parquet(out / f.name, index=False)
        rows[name] = int(len(df))
    rep = json.loads((inputs / "ingest-report.json").read_text())
    rep["subset_names"] = len(keep)
    rep["subset_rows"] = rows
    (out / "ingest-report.json").write_text(json.dumps(rep, indent=1, default=str))
    return {"names": len(keep), "rows": rows}


# Each ablation is one exact text substitution, asserted to apply exactly once.
ABLATIONS: dict[str, list[tuple[str, str]]] = {
    # The 40-row mean, demanded for the whole cross-section through stay/ext.
    "ma": [("  ma(A, T, M) :- universe(A, T), M = mean(P) over (T1 in rows(T, ma_w, min ma_w), adj(A, T1, P)).",
            "  ma(A, T, M) :- universe(A, T), adj(A, T, M)."),],
    # Both 18-row aggregates (ols_beta and corr) of R1.
    "clenow": [("""  clenow(A, T, S) :- universe(A, T),
      B = ols_beta(log(P / one_price), K) over (T1 in rows(T, clenow_w, min clenow_w), adj(A, T1, P), row(A, T1, K)),
      R = corr(log(P / one_price), K) over (T1 in rows(T, clenow_w, min clenow_w), adj(A, T1, P), row(A, T1, K)),
      S = (exp(B * weeks_a_year) - 1) * R * R.""",
                "  clenow(A, T, 0) :- universe(A, T)."),],
    # The 4-row liquidity mean.
    "dvol4": [("  dvol4(A, T, D) :- universe(A, T), D = mean(V) over (T1 in rows(T, dvol_w, min dvol_w), dvol(A, T1, V)).",
               "  dvol4(A, T, D) :- universe(A, T), dvol(A, T, D)."),],
    # The one per-bar window: the 52-row index mean.
    "gate": [("""  gate(T) :- series(T, "SPXTR", V), M = mean(X) over (T1 in rows(T, gate_w, min gate_w), series(T1, "SPXTR", X)), V > M.""",
              "  gate(T) :- bar(T)."),],
    # The split-factor recursion (adj collapses to close).
    "cumf": [("""  cumf(A, T, C) :- universe(A, T), not seen_before(A, T), split1(A, T, C).
  cumf(A, T, C) :- seen_before(A, T), prev(T, T1), cumf(A, _, C0) asof T1, split1(A, T, F), C = C0 * F.""",
              "  cumf(A, T, 1) :- universe(A, T)."),],
    # The latch recursion (and, with it, stay -> ma for the whole cross-section).
    "lp": [("""  lp(A, T, 1) :- universe(A, T), enter(A, T).
  lp(A, T, L) :- universe(A, T), not enter(A, T), stay(A, T), seen_before(A, T), prev(T, T1), lp(A, _, L) asof T1.
  lp(A, T, 0) :- universe(A, T), not enter(A, T), stay(A, T), not seen_before(A, T).
  lp(A, T, 0) :- universe(A, T), not enter(A, T), not stay(A, T).""",
            """  lp(A, T, 1) :- universe(A, T), enter(A, T).
  lp(A, T, 0) :- universe(A, T), not enter(A, T)."""),],
    # R7's two counts enumerate the whole bar block; restricting them to the book's
    # own names is a SEMANTIC change (the latch keeps names that left the universe) --
    # the cell prices the enumeration, it does not propose the rewrite.
    "dial": [("""  n_latched(T, N) :- bar(T), N = count(A) over (universe(A, T), lp(A, T, 1), ext(A, T, _)).""",
              "  n_latched(T, N) :- bar(T), N = count(A) over (inbook(A, T), ext(A, T, _))."),
             ("""  n_crowded(T, N) :- bar(T), N = count(A) over (universe(A, T), lp(A, T, 1), ext(A, T, E), E > crowd_ext).""",
              "  n_crowded(T, N) :- bar(T), N = count(A) over (inbook(A, T), ext(A, T, E), E > crowd_ext)."),],
    # The cross-sectional rank aggregate of R1.
    "rank": [("  xrank(A, T, K) :- bar(T), rank(scored(A, T, S), by (S desc, A asc), as K).",
              "  xrank(A, T, K) :- scored(A, T, K)."),],
}


def ablate(dsl: Path, out: Path) -> list[str]:
    out.mkdir(parents=True, exist_ok=True)
    src = dsl.read_text()
    made = []
    for name, subs in ABLATIONS.items():
        txt = src
        for old, new in subs:
            if txt.count(old) != 1:
                raise SystemExit(f"ablation {name}: pattern occurs {txt.count(old)} times, not 1:\n{old[:120]}")
            txt = txt.replace(old, new)
        p = out / f"mw14_abl_{name}.dsl"
        p.write_text(txt)
        made.append(str(p))
    return made


def main() -> int:
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("subset"); s.add_argument("--inputs", required=True)
    s.add_argument("--out", required=True); s.add_argument("--names", type=int, required=True)
    a = sub.add_parser("ablate"); a.add_argument("--dsl", required=True); a.add_argument("--out", required=True)
    args = ap.parse_args()
    if args.cmd == "subset":
        print(json.dumps(subset(Path(args.inputs), Path(args.out), args.names)))
    else:
        for p in ablate(Path(args.dsl), Path(args.out)):
            print(p)
    return 0


if __name__ == "__main__":
    sys.exit(main())
