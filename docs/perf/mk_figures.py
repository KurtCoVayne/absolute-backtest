#!/usr/bin/env python3
"""Figures for the MW14 performance issue (KurtCoVayne/absolute-backtest).

Paper style: no embedded titles -- the caption lives in the issue text. One axis per
chart, never two scales. Categorical hues are the validated pair blue/orange in fixed
order (abt, python); reference guides are neutral grey, never a categorical hue.

  python mk_figures.py --bench bench.json --scaling scaling.csv --eras eras.csv \
                       --ablation ablation.csv --out DIR [--only NAME]
"""
from __future__ import annotations

import argparse
import csv
import json
import math
from pathlib import Path

import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt
import numpy as np

ABT, PY = "#2a78d6", "#eb6834"          # validated adjacent pair, light mode
SURFACE = "#fcfcfb"
INK, INK2, GRID = "#0b0b0b", "#52514e", "#d9d8d4"
HOST = "aws-m8g.xlarge"

COLD_PY = ["P0_cold_panel", "P1P4_warm_scratch"]
COLD_ABT = ["A0_ingest", "A1_bundle_build", "A3_run"]
NICE = {"A0_ingest": "ingest\n(lake → facts)", "A1_bundle_build": "bundle build",
        "A3_run": "run\n(1,853 bars)", "A2_bundle_test": "bundle test",
        "P0_cold_panel": "panel build\n(lake → panel)",
        "P1P4_warm_scratch": "book + simulate", "P1P4_warm": "book + simulate"}


def style(ax):
    ax.set_facecolor(SURFACE)
    ax.figure.set_facecolor(SURFACE)
    for s in ("top", "right"):
        ax.spines[s].set_visible(False)
    for s in ("left", "bottom"):
        ax.spines[s].set_color(GRID)
    ax.tick_params(colors=INK2, labelsize=9, length=3)
    ax.grid(True, axis="x", color=GRID, lw=0.6, alpha=0.9)
    ax.set_axisbelow(True)
    for lbl in list(ax.get_xticklabels()) + list(ax.get_yticklabels()):
        lbl.set_color(INK2)


def rows(bench, engine, stage):
    return [r for r in bench["runs"] if r.get("host") == HOST and not r.get("pilot")
            and r["engine"] == engine and r["stage"] == stage]


def composite(bench, engine, stages, field="wall_s"):
    byrep = {}
    for s in stages:
        for r in rows(bench, engine, s):
            byrep.setdefault(r["rep"], {})[s] = r[field]
    return [sum(d[s] for s in stages) for _, d in sorted(byrep.items())
            if all(s in d for s in stages)]


def med(xs):
    return float(np.median(xs)) if len(xs) else float("nan")


# ----------------------------------------------------------------- 1. stages ----
def fig_stages(bench, out: Path):
    order = [("abt", s) for s in COLD_ABT] + [("python", s) for s in COLD_PY]
    labels = [NICE[s] for _, s in order]
    wall = [med([r["wall_s"] for r in rows(bench, e, s)]) for e, s in order]
    cpu = [med([r["user_s"] + r["sys_s"] for r in rows(bench, e, s)]) for e, s in order]
    col = [ABT if e == "abt" else PY for e, _ in order]
    y = np.arange(len(order))
    fig, axes = plt.subplots(1, 2, figsize=(9.2, 3.6), sharey=True)
    for ax, vals, name in zip(axes, (wall, cpu), ("wall clock (s)", "CPU time (s)")):
        style(ax)
        ax.barh(y, vals, color=col, height=0.62)
        ax.set_xscale("log")
        ax.set_xlabel(name, color=INK2, fontsize=9)
        for yi, v in zip(y, vals):
            ax.annotate(f"{v:,.1f}", (v, yi), xytext=(5, 0), textcoords="offset points",
                        va="center", fontsize=8.5, color=INK)
        ax.set_xlim(right=max(vals) * 3.2)
    axes[0].set_yticks(y, labels, fontsize=9)
    axes[0].invert_yaxis()
    handles = [plt.Rectangle((0, 0), 1, 1, color=ABT), plt.Rectangle((0, 0), 1, 1, color=PY)]
    axes[1].legend(handles, ["abt (Rust)", "python (polars)"], frameon=False,
                   fontsize=9, labelcolor=INK2, loc="lower right")
    fig.tight_layout()
    fig.savefig(out / "mw14-engine-stages.png", dpi=200)
    plt.close(fig)
    return {"wall": dict(zip(labels, wall)), "cpu": dict(zip(labels, cpu))}


# -------------------------------------------------------------- 2. rep spread ----
def fig_spread(bench, out: Path):
    order = [("abt", s) for s in COLD_ABT] + [("python", s) for s in COLD_PY]
    fig, ax = plt.subplots(figsize=(8.4, 3.4))
    style(ax)
    ax.grid(True, axis="x", color=GRID, lw=0.6)
    for i, (e, s) in enumerate(order):
        v = [r["wall_s"] for r in rows(bench, e, s)]
        if not v:
            continue
        c = ABT if e == "abt" else PY
        ax.scatter(v, np.full(len(v), i) + np.random.default_rng(i).normal(0, .045, len(v)),
                   s=26, color=c, alpha=.75, linewidths=0, zorder=3)
        m = med(v)
        ax.plot([m, m], [i - .28, i + .28], color=INK, lw=1.6, zorder=4)
        ax.annotate(f"median {m:,.1f} s   (n={len(v)}, spread {max(v)-min(v):,.1f} s)",
                    (m, i), xytext=(0, 14), textcoords="offset points", ha="center",
                    fontsize=8, color=INK2)
    ax.set_xscale("log")
    ax.set_yticks(range(len(order)), [NICE[s] for _, s in order], fontsize=9)
    ax.invert_yaxis()
    ax.set_xlabel("wall clock per repetition (s, log scale)", color=INK2, fontsize=9)
    fig.tight_layout()
    fig.savefig(out / "mw14-rep-spread.png", dpi=200)
    plt.close(fig)


def fig_ecdf(bench, out: Path):
    fig, ax = plt.subplots(figsize=(7.0, 3.4))
    style(ax)
    for eng, stages, c, name in (("abt", COLD_ABT, ABT, "abt (Rust)"),
                                 ("python", COLD_PY, PY, "python (polars)")):
        v = sorted(composite(bench, eng, stages))
        if not v:
            continue
        xs = [v[0]] + [x for x in v for _ in (0, 1)]
        ys = [0.0] + [y for i in range(len(v)) for y in (i / len(v), (i + 1) / len(v))]
        ax.plot(xs, ys, color=c, lw=2, label=f"{name} — median {med(v):,.1f} s")
        ax.scatter(v, [(i + 1) / len(v) for i in range(len(v))], s=22, color=c,
                   zorder=3, linewidths=0)
    ax.set_xscale("log")
    ax.set_xlabel("cold-to-answer, lake bars → 1,852 weekly returns (s, log scale)",
                  color=INK2, fontsize=9)
    ax.set_ylabel("share of repetitions", color=INK2, fontsize=9)
    ax.set_ylim(0, 1.05)
    ax.legend(frameon=False, fontsize=9, labelcolor=INK2, loc="lower right")
    fig.tight_layout()
    fig.savefig(out / "mw14-rep-ecdf.png", dpi=200)
    plt.close(fig)


# ----------------------------------------------------------------- 3. scaling ----
def fig_scaling(path: Path, out: Path):
    d = list(csv.DictReader(open(path)))
    n = np.array([float(r["names"]) for r in d])
    t = np.array([float(r["run_s"]) for r in d])
    wc = np.array([float(r["window_calls"]) for r in d])
    o = np.argsort(n)
    n, t, wc = n[o], t[o], wc[o]
    slope, _ = np.polyfit(np.log(n), np.log(t), 1)
    fig, axes = plt.subplots(1, 2, figsize=(9.6, 3.9))
    ax = axes[0]
    style(ax); ax.grid(True, which="both", color=GRID, lw=0.6)
    for p, lab in ((1.0, "linear in names"), (2.0, "quadratic")):
        g = t[-1] * (n / n[-1]) ** p
        ax.plot(n, g, color="#9b9a95", lw=1.1, ls="--", zorder=1)
        j = 1 if p == 1.0 else 2
        ax.annotate(lab, (n[j], g[j]), xytext=(0, 7), textcoords="offset points",
                    fontsize=8, color="#75746f", ha="center")
    ax.plot(n, t, color=ABT, lw=2, marker="o", ms=6, zorder=3,
            label=f"measured — fitted slope {slope:.2f}")
    for ni, ti in zip(n, t):
        ax.annotate(f"{ti:,.0f} s", (ni, ti), xytext=(0, -14),
                    textcoords="offset points", ha="center", fontsize=8, color=INK)
    ax.set_xscale("log"); ax.set_yscale("log")
    ax.minorticks_off()
    ax.set_xticks(n, [f"{int(v):,}" for v in n], fontsize=8.5)
    ax.set_ylim(top=t[-1] * 3.0)
    ax.set_xlabel("names in the bundle", color=INK2, fontsize=9)
    ax.set_ylabel("kernel run time (s)", color=INK2, fontsize=9)
    ax.legend(frameon=False, fontsize=9, labelcolor=INK2, loc="upper left")

    ax = axes[1]
    style(ax); ax.grid(True, axis="y", color=GRID, lw=0.6)
    per = t / wc * 1e6
    ax.plot(n, per, color=ABT, lw=2, marker="o", ms=6)
    for ni, pi in zip(n, per):
        ax.annotate(f"{pi:.1f}", (ni, pi), xytext=(0, 8), textcoords="offset points",
                    ha="center", fontsize=8.5, color=INK)
    ax.set_xscale("log")
    ax.minorticks_off()
    ax.set_xticks(n, [f"{int(v):,}" for v in n], fontsize=8.5)
    ax.set_ylim(0, max(per) * 1.25)
    ax.set_xlabel("names in the bundle", color=INK2, fontsize=9)
    ax.set_ylabel("µs per aggregate evaluation", color=INK2, fontsize=9)
    fig.tight_layout()
    fig.savefig(out / "mw14-universe-scaling.png", dpi=200)
    plt.close(fig)
    return slope


# -------------------------------------------------------------------- 4. eras ----
def fig_eras(path: Path, out: Path):
    d = list(csv.DictReader(open(path)))
    lab = [r["era"] for r in d]
    sec = [float(r["seconds"]) for r in d]
    names = [float(r["mean_eligible"]) for r in d]
    x = np.arange(len(d))
    fig, axes = plt.subplots(2, 1, figsize=(7.4, 4.6), sharex=True,
                             gridspec_kw={"height_ratios": [2, 1]})
    style(axes[0]); style(axes[1])
    for ax in axes:
        ax.grid(True, axis="y", color=GRID, lw=0.6)
        ax.grid(False, axis="x")
    axes[0].bar(x, sec, color=ABT, width=.66)
    for xi, v in zip(x, sec):
        axes[0].annotate(f"{v:,.0f}", (xi, v), xytext=(0, 3), textcoords="offset points",
                         ha="center", fontsize=8, color=INK)
    axes[0].set_ylabel("kernel seconds in the era", color=INK2, fontsize=9)
    axes[1].bar(x, names, color="#9b9a95", width=.66)
    axes[1].set_ylabel("mean eligible names", color=INK2, fontsize=9)
    axes[1].set_xticks(x, lab, fontsize=9)
    fig.tight_layout()
    fig.savefig(out / "mw14-era-cost.png", dpi=200)
    plt.close(fig)


# ---------------------------------------------------------------- 5. ablation ----
def fig_ablation(path: Path, out: Path):
    d = sorted(csv.DictReader(open(path)), key=lambda r: -float(r["seconds_saved"]))
    lab = [r["stubbed"] for r in d]
    sav = [float(r["seconds_saved"]) for r in d]
    tot = float(d[0]["baseline_s"])
    y = np.arange(len(sav))
    fig, ax = plt.subplots(figsize=(7.8, 3.8))
    style(ax)
    ax.barh(y, sav, color=ABT, height=.62)
    ax.axvline(0, color=INK2, lw=1)
    for yi, v in zip(y, sav):
        off = 6 if v >= 0 else -6
        ax.annotate(f"{v:+,.1f} s   {v / tot:+.1%}", (v, yi), xytext=(off, 0),
                    textcoords="offset points", va="center",
                    ha="left" if v >= 0 else "right", fontsize=8.5, color=INK)
    ax.set_yticks(y, lab, fontsize=9)
    ax.invert_yaxis()
    lo, hi = min(sav), max(sav)
    ax.set_xlim(lo - (hi - lo) * 0.30, hi + (hi - lo) * 0.30)
    ax.set_xlabel(f"change in kernel time against the {tot:,.0f} s baseline when the "
                  f"relation is stubbed\n(positive = time removed; the stubs are not "
                  f"additive and each changes the book)", color=INK2, fontsize=9)
    fig.tight_layout()
    fig.savefig(out / "mw14-relation-ablation.png", dpi=200)
    plt.close(fig)


# ----------------------------------------------------------- 6. relation calls ----
def fig_relations(path: Path, out: Path, top: int = 13):
    d = sorted(csv.DictReader(open(path)), key=lambda r: -int(r["calls"]))
    d = [r for r in d if int(r["calls"]) > 0][:top]
    CHAIN = {"seen_before", "adj", "cumf", "split1", "row"}
    lab = [r["relation"] for r in d]
    val = [int(r["calls"]) / 1e6 for r in d]
    y = np.arange(len(d))
    fig, ax = plt.subplots(figsize=(7.6, 4.4))
    style(ax)
    ax.barh(y, val, color=ABT, height=.62)
    for yi, (v, r) in enumerate(zip(val, d)):
        ax.annotate(f"{v:,.2f}M   {float(r['calls_per_bar']):,.0f}/bar", (v, yi),
                    xytext=(5, 0), textcoords="offset points", va="center",
                    fontsize=8.5, color=INK)
    # The split-adjustment chain, bracketed rather than recoloured.
    idx = [i for i, n in enumerate(lab) if n in CHAIN]
    if idx:
        lo, hi = min(idx), max(idx)
        xb = max(val) * 1.42
        ax.plot([xb, xb * 1.02, xb * 1.02, xb], [lo - .3, lo - .3, hi + .3, hi + .3],
                color=INK2, lw=1)
        share = sum(val[i] for i in idx) / sum(int(r["calls"]) for r in
                    csv.DictReader(open(path))) * 1e6
        ax.annotate(f"the split-adjustment chain:\n{share:.0%} of all derivations",
                    (xb * 1.04, (lo + hi) / 2), fontsize=8.5, color=INK2, va="center")
    ax.set_yticks(y, lab, fontsize=9)
    ax.invert_yaxis()
    ax.set_xlim(right=max(val) * 2.05)
    ax.set_xlabel("derivations over the run (millions of memo misses)", color=INK2, fontsize=9)
    fig.tight_layout()
    fig.savefig(out / "mw14-relation-calls.png", dpi=200)
    plt.close(fig)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bench"); ap.add_argument("--scaling"); ap.add_argument("--eras")
    ap.add_argument("--ablation"); ap.add_argument("--relations")
    ap.add_argument("--out", required=True)
    ap.add_argument("--only")
    a = ap.parse_args()
    out = Path(a.out); out.mkdir(parents=True, exist_ok=True)
    want = (lambda n: a.only in (None, n))
    if a.bench and want("stages"):
        b = json.load(open(a.bench))
        fig_stages(b, out); fig_spread(b, out); fig_ecdf(b, out)
        print("[fig] stages, rep spread, ecdf")
    if a.scaling and want("scaling"):
        print(f"[fig] scaling, fitted log-log slope {fig_scaling(Path(a.scaling), out):.3f}")
    if a.eras and want("eras"):
        fig_eras(Path(a.eras), out); print("[fig] eras")
    if a.ablation and want("ablation"):
        fig_ablation(Path(a.ablation), out); print("[fig] ablation")
    if a.relations and want("relations"):
        fig_relations(Path(a.relations), out); print("[fig] relation calls")


if __name__ == "__main__":
    main()
