#!/usr/bin/env python3
"""Score the sandboxes of the observability study.

  python3 experiments/llm-observability/score.py STUDY [--runs DIR]

For every sandbox NAME under STUDY: checks every attempt with the sandbox's
own abt (attempts.tsv), checks and runs final.dsl with the task's run
conventions (final_check.txt, run.txt), counts the inspection commands in
commands.log, and for mw14 and r8l compares the final run's decisions with
the corpus book's decisions on the same data (compare.txt). The scores go to
STUDY/results/NAME/ and, with --runs DIR, the sandbox's attempts, final.dsl,
run.sh, NOTES.md, commands.log and scores are copied to DIR/NAME/ for the
record. Prints one summary line per sandbox and writes STUDY/results/summary.tsv.
"""
import argparse
import re
import shutil
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
TASK_FLAGS = {
    "mw14": ["--data", "data/weekly", "--price-relation", "trclose", "--actions", "in-prices", "--compounding", "off",
             "--capital", "1000000", "--lot", "fractional", "--frictionless", "--commission-bps", "10", "--margin-rate", "0",
             "--on-leverage", "allow", "--delist-proceeds", "last-price", "--start", "2016-01-01", "--periods-per-year", "52"],
    "r8l": ["--data", "data/sessions", "--price-relation", "close_m", "--compounding", "off", "--capital", "1000000",
            "--lot", "fractional", "--frictionless", "--margin-rate", "0", "--on-leverage", "allow", "--on-margin-call", "allow",
            "--report-by", "day", "--report-calendar", "calendar", "--periods-per-year", "252"],
    "momentum_liquid_monthly": ["--synthetic", "--days", "1000", "--symbols", "A,B,C,D,E,F,G,H,I,J,K,L,M,N,O"],
    "intraday_open_gap": ["--synthetic", "--days", "40"],
    "breakout_with_stop": ["--synthetic", "--days", "1000", "--symbols", "A,B,C,D,E,F,G,H,I,J,K,L"],
    "vol_targeted_trend": ["--synthetic", "--days", "1000", "--symbols", "SPY"],
}
REFERENCE = {"mw14": REPO / "corpus/company/mw14.dsl", "r8l": REPO / "corpus/company/r8l.dsl"}


def run(cmd, cwd, timeout=900):
    try:
        p = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, timeout=timeout)
        return p.returncode, p.stdout + p.stderr
    except subprocess.TimeoutExpired:
        return -1, "TIMEOUT"


def strategy_name(path: Path):
    m = re.search(r"^strategy\s+([A-Za-z0-9_]+)", path.read_text(), re.M)
    return m.group(1) if m else None


def decisions_of(text: str):
    """(bar, symbol, side, magnitude) of every decision line a run printed with
    --all: a delta `short(ES, 1.79, moo_moc)` and a target
    `target_quantity(ES, -1.79, moo_moc)` are the same decision, so the side
    is the sign and the magnitude is rounded to six places."""
    out = set()
    for line in text.splitlines():
        m = re.match(r"\s+(\S+) ([a-z_]+)\(([^,]+), ([-0-9.e]+)", line)
        if m and not line.lstrip().startswith(("fill", "dropped")):
            ctor, amount = m.group(2), float(m.group(4))
            side = -1 if ctor in ("sell", "short") or amount < 0 else 1
            out.add((m.group(1), m.group(3), side, round(abs(amount), 6)))
    return out


def score(d: Path, out: Path):
    out.mkdir(parents=True, exist_ok=True)
    abt = d / "bin" / "abt.real"
    name = d.name
    task = name.split("-haiku-")[0]
    cond = name.split("-haiku-")[1]
    # Attempts.
    rows = []
    attempts = sorted((d / "attempts").glob("*.dsl"), key=lambda p: int(re.sub(r"\D", "", p.stem) or 0))
    for a in attempts:
        code, o = run([str(abt), "check", "env", "lib", str(a)], d)
        summary = next((l for l in o.splitlines() if "checked:" in l), None)
        if summary is None:
            summary = "PARSE: " + (o.splitlines()[0] if o else "")
        codes = {}
        for m in re.finditer(r"^(error|warning) \[([A-Z0-9]+)\]", o, re.M):
            k = f"{m.group(1)}[{m.group(2)}]"
            codes[k] = codes.get(k, 0) + 1
        rows.append((a.name, summary, ";".join(f"{v} {k}" for k, v in sorted(codes.items()))))
    (out / "attempts.tsv").write_text("".join("\t".join(r) + "\n" for r in rows))
    clean_at = next((i + 1 for i, r in enumerate(rows) if "0 error(s)" in r[1]), None)
    # The command log.
    log = (d / "commands.log").read_text() if (d / "commands.log").exists() else ""
    cmds = [l.split(" ", 1)[1] if " " in l else "" for l in log.splitlines()]
    uses = {k: sum(1 for c in cmds if c.startswith(k + " ")) for k in ("check", "run", "show", "query", "explain")}
    uses["ledger"] = sum(1 for c in cmds if "--ledger" in c)
    uses["total"] = len(cmds)
    # Final.
    final = d / "final.dsl"
    result = {"task": task, "cond": cond, "attempts": len(rows), "clean_at": clean_at, "final": final.exists(), **uses}
    if final.exists():
        code, o = run([str(abt), "check", "env", "lib", str(final)], d)
        (out / "final_check.txt").write_text(o)
        result["final_clean"] = "0 error(s)" in o
        strat = strategy_name(final)
        if strat and result["final_clean"]:
            cmd = [str(abt), "run", "--strategy", strat, *TASK_FLAGS[task], "--all", "env", "lib", str(final)]
            code, o = run(cmd, d)
            (out / "run.txt").write_text(" ".join(cmd) + "\n\n" + o)
            m = re.search(r"decisions: (\d+)\s+fills: (\d+)", o)
            result["decisions"] = int(m.group(1)) if m else None
            result["fills"] = int(m.group(2)) if m else None
            result["halted"] = "run halted" in o
            if task in REFERENCE:
                ref = REFERENCE[task]
                rcmd = [str(abt), "run", "--strategy", task, *TASK_FLAGS[task], "--all", "env", "lib", str(ref)]
                _, ro = run(rcmd, d)
                mine, theirs = decisions_of(o), decisions_of(ro)
                inter = len(mine & theirs)
                (out / "compare.txt").write_text(
                    f"reference decisions {len(theirs)}; agent decisions {len(mine)}; in both (bar, symbol, side, magnitude) {inter}\n"
                    f"precision {inter / len(mine) if mine else 0:.3f}  recall {inter / len(theirs) if theirs else 0:.3f}\n"
                )
                result["ref_decisions"] = len(theirs)
                result["matched"] = inter
    return result


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("study", type=Path)
    ap.add_argument("--runs", type=Path)
    a = ap.parse_args()
    results = []
    for d in sorted(a.study.iterdir()):
        if "-haiku-" not in d.name or not d.is_dir():
            continue
        r = score(d, a.study / "results" / d.name)
        results.append(r)
        print(f"{d.name}: attempts {r['attempts']} clean_at {r['clean_at']} final_clean {r.get('final_clean')} decisions {r.get('decisions')} "
              f"matched {r.get('matched', '-')}/{r.get('ref_decisions', '-')} cmds {r['total']} (check {r['check']} run {r['run']} show {r['show']} query {r['query']} explain {r['explain']} ledger {r['ledger']})")
        if a.runs:
            dst = a.runs / d.name
            if dst.exists():
                shutil.rmtree(dst)
            dst.mkdir(parents=True)
            for f in ("final.dsl", "run.sh", "NOTES.md", "commands.log"):
                if (d / f).exists():
                    shutil.copy(d / f, dst / f)
            shutil.copytree(d / "attempts", dst / "attempts")
            shutil.copytree(a.study / "results" / d.name, dst / "score")
    keys = ["task", "cond", "attempts", "clean_at", "final_clean", "decisions", "fills", "halted", "ref_decisions", "matched", "total", "check", "run", "show", "query", "explain", "ledger"]
    with open(a.study / "results" / "summary.tsv", "w") as f:
        f.write("\t".join(keys) + "\n")
        for r in results:
            f.write("\t".join(str(r.get(k, "")) for k in keys) + "\n")


if __name__ == "__main__":
    sys.exit(main())
