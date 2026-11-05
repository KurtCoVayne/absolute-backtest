"""Turn the profiling suite's raw outputs into the CSVs the figures read.

  python prof_summarize.py --prof DIR --out DIR
"""
from __future__ import annotations

import argparse
import csv
import re
from pathlib import Path

COV = re.compile(r"^\s{2}(\S+)\s+(\d+) calls\s+(\d+) with tuples\s+(\d+) tuples")
TIM = re.compile(r"(\w+)=([-\d.]+)")
ERAS = ["1996-01-01", "2004-01-01", "2012-01-01", "2020-01-01", "2026-07-02"]
ABL = ["ma", "clenow", "dvol4", "gate", "cumf", "lp", "dial", "rank"]


def cells(prof: Path) -> dict[str, dict]:
    out = {}
    f = prof / "cells.tsv"
    if f.exists():
        for r in csv.reader(open(f), delimiter="\t"):
            if len(r) < 9:
                continue
            out[r[0]] = {"label": r[1], "run_s": float(r[2]), "bars": int(r[3]),
                         "window_calls": int(r[4]), "window_bars_solved": int(r[5]),
                         "memo_entries": int(r[6]), "peak_rss_mb": float(r[7] or 0),
                         "rc": r[8]}
    return out


def timing(log: Path) -> dict:
    if not log.exists():
        return {}
    for line in reversed(log.read_text(errors="replace").splitlines()):
        if line.startswith("timing "):
            return {k: float(v) if "." in v else int(v) for k, v in TIM.findall(line)}
    return {}


def coverage(log: Path) -> dict[str, tuple[int, int, int]]:
    d = {}
    if not log.exists():
        return d
    for line in log.read_text(errors="replace").splitlines():
        m = COV.match(line)
        if m:
            d[m.group(1)] = (int(m.group(2)), int(m.group(3)), int(m.group(4)))
    return d


def write(path: Path, rows: list[dict], cols: list[str]) -> None:
    with open(path, "w", newline="") as fh:
        w = csv.DictWriter(fh, fieldnames=cols)
        w.writeheader()
        w.writerows(rows)
    print(f"[csv] {path.name}: {len(rows)} rows")


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--prof", required=True); ap.add_argument("--out", required=True)
    a = ap.parse_args()
    prof, out = Path(a.prof), Path(a.out)
    out.mkdir(parents=True, exist_ok=True)
    c = cells(prof)

    # --- scaling ------------------------------------------------------------
    rows = []
    for n in (250, 500, 1000, 2000, 4240):
        k = f"rung{n}"
        if k in c:
            cov = coverage(prof / f"cell_{k}.log")
            el = cov.get("eligible", (0, 0, 0))[2]
            rows.append({"names": n, "run_s": c[k]["run_s"], "bars": c[k]["bars"],
                         "window_calls": c[k]["window_calls"],
                         "memo_entries": c[k]["memo_entries"],
                         "eligible_tuples": el,
                         "mean_eligible": round(el / max(c[k]["bars"], 1), 1)})
    if rows:
        write(out / "scaling.csv", rows, list(rows[0]))

    # --- eras (cumulative --end, differenced) -------------------------------
    rows, prev = [], None
    for e in ERAS:
        k = f"era{e}"
        if k not in c:
            continue
        cov = coverage(prof / f"cell_{k}.log")
        cur = {"run_s": c[k]["run_s"], "bars": c[k]["bars"],
               "el": cov.get("eligible", (0, 0, 0))[2],
               "wc": c[k]["window_calls"]}
        lo = "1991" if prev is None else rows[-1]["to"][:4]
        d = {"era": f"{lo}–{e[:4]}", "to": e,
             "seconds": round(cur["run_s"] - (prev["run_s"] if prev else 0), 2),
             "bars": cur["bars"] - (prev["bars"] if prev else 0),
             "window_calls": cur["wc"] - (prev["wc"] if prev else 0),
             "cum_run_s": cur["run_s"]}
        d["mean_eligible"] = round((cur["el"] - (prev["el"] if prev else 0)) /
                                   max(d["bars"], 1), 1)
        d["s_per_bar"] = round(d["seconds"] / max(d["bars"], 1), 4)
        rows.append(d); prev = cur
    if rows:
        write(out / "eras.csv", rows, list(rows[0]))

    # --- ablations ----------------------------------------------------------
    base = c.get("parity") or c.get("rung4240") or c.get("era2026-07-02")
    rows = []
    if base:
        for name in ABL:
            k = f"abl_{name}"
            if k not in c:
                continue
            rows.append({"stubbed": name, "run_s": c[k]["run_s"],
                         "baseline_s": base["run_s"],
                         "seconds_saved": round(base["run_s"] - c[k]["run_s"], 2),
                         "share": round((base["run_s"] - c[k]["run_s"]) / base["run_s"], 4),
                         "window_calls": c[k]["window_calls"],
                         "window_calls_baseline": base["window_calls"],
                         "memo_entries": c[k]["memo_entries"]})
    if rows:
        rows.sort(key=lambda r: -r["seconds_saved"])
        write(out / "ablation.csv", rows, list(rows[0]))

    # --- per-relation coverage of the baseline ------------------------------
    src = prof / ("cell_parity.log" if (prof / "cell_parity.log").exists()
                  else "cell_rung4240.log")
    cov = coverage(src)
    if cov:
        bars = (base or {}).get("bars", 1) or 1
        rows = [{"relation": k, "calls": v[0], "with_tuples": v[1], "tuples": v[2],
                 "calls_per_bar": round(v[0] / bars, 1)}
                for k, v in sorted(cov.items(), key=lambda kv: -kv[1][0])]
        write(out / "relations.csv", rows, list(rows[0]))

    # --- perf self time -----------------------------------------------------
    f = prof / "p1_self.txt"
    if f.exists():
        rows = []
        for line in f.read_text(errors="replace").splitlines():
            m = re.match(r"\s*(\d+\.\d+)%\s+(.*?)\s*$", line)
            if m and not line.lstrip().startswith("#"):
                rows.append({"pct": float(m.group(1)), "symbol": m.group(2)})
        if rows:
            write(out / "perf_self.csv", rows, ["pct", "symbol"])
            print(f"      sampled total {sum(r['pct'] for r in rows):.1f}% listed")


if __name__ == "__main__":
    main()
