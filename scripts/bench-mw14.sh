#!/usr/bin/env bash
# The MW14 benchmark: the full replay (1991 to 2026, the S&P 1500 through the
# Norgate adapter) timed against the firm's canon, as one command that stages
# its own store. docs/handoff/benchmark.md is the record of the Nov 6 run this
# script reproduces; docs/assessment.md records its results. Usage:
#
#   scripts/bench-mw14.sh LAKE SPXTR.parquet VIX.parquet CANON.npy [N]
#
#   LAKE    an NDLake root (curated/bars_by_date, registry, indexes, classification)
#   SPXTR   the book's spxtr_daily.parquet ($SPXTR daily closes, from 1988)
#   VIX     the book's vix_weekly.parquet (the weekly VIX and its 4-week mean)
#   CANON   _canon_wkret_mw14_full.npy, the book's 1,852 weekly returns
#   N       repetitions (default 5)
#
# What it does:
#
# 1. Stages the store once. The store is keyed by a hash of what it is made
#    from: every file of the lake, the two series files, the staging recipe,
#    and the code that writes a store (the sources of crates/abt-data and
#    crates/abt-codec and the series script, with their uncommitted changes)
#    and the adapter's options (every span holds every asset ever a member,
#    `member_to` a member day, the lake's values: E-149). A second call with
#    the same inputs reuses it: $BENCH/store-<hash>. RESTAGE=1 stages again;
#    KEY_ONLY=1 prints the store's path and stops.
#    STORE=DIR names a store already staged and skips this step; LAKE, SPXTR
#    and VIX may then be `-`. Such a store must never have served a run: a
#    run's id is a hash of the program and the binding, not of the engine,
#    so a store that holds `kernel.<run>.*` of an earlier build refuses the
#    outputs of a build that decides differently. The script refuses it.
# 2. For each repetition: copies the base store (a reflink where the file
#    system has them), serves the copy, and runs the book under
#    `/usr/bin/time -v` with --timing and ABT_TIMING_RELS=1. A repetition
#    starts its own data service, so each is cold and the service's CPU time
#    and peak memory are the run's alone.
# 3. Compares each run with the canon (scripts/parity/mw14_compare.py); the
#    comparison carries a fingerprint of the run's own returns
#    (`compare.abt_sha256`): the same in two runs that decided the same.
# 4. Prints one JSON line per repetition on stdout, a failed one too (its
#    `rc` is the run's exit code, -1 when the data service did not start),
#    and appends it to $OUT/runs.jsonl: the front's wall, user and system seconds and peak
#    memory, the data service's CPU seconds and peak memory, the tuples
#    received (in all and per relation) and trimmed, the seconds per phase and
#    per derived relation, the weekly correlation and the count of weeks equal
#    to the canon to 1e-4, 1e-9 and 8e-15. `phases_s` is the run's `phases`
#    line, every key of it: `bind`, `check`, `build`, `spec`, `push`, `open`,
#    `stream`, `write` and `post` follow one another, and `unclocked_s` is
#    what they leave of the run's seconds; `wait`, `decode`, `insert`,
#    `fill`, `decide`, `exec`, `inv`, `observe`, `warm`, `trim`, `evict` and
#    `loop_other` are the parts of `stream` and add up to it. Each relation
#    of `derive` carries its derivations that left a stuck instance or a halt
#    (`stuck_bars`) and those of at most 16 tuples (`small_bars`); `front`
#    carries the resident memory when the stream met its first barrier and
#    the peak until then; `prefetch_skipped` counts the rows-window block
#    solves left out because no row would read them.
#
# Env: BENCH=/data/bench (stores and logs), OUT=$BENCH/run-<time> (this call's
# logs), ABT=target/release/abt, PY=python3, THREADS (--threads, default the
# engine's), START=1990-01-01 and END=2026-07-02 (the data's first week, so
# that the rows, the latch and the gate warm up on 1990 as the book's features
# do, while the program trades from its `first_trade`, 1991-01-04, the canon's
# first decision week; and the bar after the canon's last: E-146), EXTRA (more
# arguments to `abt run`),
# KEEP=1 (keep each repetition's copy of the store), WRAP (a command the run
# is started under, for a profile: ABT=target/profiling/abt WRAP='perf record
# -e cpu-clock -F 299 -g --call-graph fp -o perf.data --'; such a repetition's
# seconds are the profiler's too).
#
# It runs on Linux (GNU time, /proc); the full replay is not for a laptop.
set -euo pipefail
usage() { sed -n '2,/^set -euo/p' "$0" | sed '$d' | sed 's/^# \{0,1\}//'; exit 2; }
[ $# -ge 4 ] || usage
# Paths are the caller's: made absolute before the script moves to the repository.
abs() { case "$1" in -|"") echo "$1" ;; *) readlink -m -- "$1" ;; esac; }
LAKE=$(abs "$1"); SPXTR=$(abs "$2"); VIX=$(abs "$3"); CANON=$(abs "$4"); N=${5:-5}
BENCH=$(abs "${BENCH:-/data/bench}")
[ -z "${OUT:-}" ] || OUT=$(abs "$OUT")
[ -z "${STORE:-}" ] || STORE=$(readlink -f -- "$STORE")
[ -z "${ABT:-}" ] || ABT=$(abs "$ABT")
cd "$(dirname "$0")/.."
ROOT=$PWD
ABT=${ABT:-$ROOT/target/release/abt}
PY=${PY:-python3}
START=${START:-1990-01-01}
END=${END:-2026-07-02}
THREADS=${THREADS:-}
EXTRA=${EXTRA:-}
WRAP=${WRAP:-}
KEEP=${KEEP:-0}
OUT=${OUT:-$BENCH/run-$(date -u +%Y%m%dT%H%M%S)}
CORPUS="corpus-v2/catalog corpus-v2/lib corpus-v2/company/mw14.dsl"
SPANS=${SPANS:-"1990-01-01:1994-12-31 1995-01-01:1999-12-31 2000-01-01:2004-12-31 2005-01-01:2009-12-31 2010-01-01:2014-12-31 2015-01-01:2019-12-31 2020-01-01:2026-12-31"}
# The adapter's options for the book (E-149): every span holds every asset that
# is a member at any time from the first span's start to the last span's end, as
# the book reads every bar of every name ever a member; `member_to` is a member
# day, as the book's test is inclusive; the lake's prices and amounts unrounded.
NORGATE_OPTS="--universe-start ${SPANS%%:*} --universe-end ${SPANS##*:} --member-to last_day --values lake"
# LISTS and SPANS may be the caller's: the book's rules on another real membership (the Russell 3000 as
# 'Russell 1000=SP500;Russell Mid Cap=SP400;Russell 2000=SP600', whose union it is), in spans a 16 GB machine holds.
LISTS=${LISTS:-'S&P 500=SP500;S&P MidCap 400=SP400;S&P SmallCap 600=SP600'}
TICKERS='SPY=ETF;QQQ=ETF'
mkdir -p "$BENCH" "$OUT"
say() { echo "[$(date -u +%H:%M:%S)] $*" >&2; }
[ -x "$ABT" ] || { echo "no abt binary at $ABT (cargo build --release, or ABT=...)" >&2; exit 2; }
[ -f "$CANON" ] || { echo "no canon at $CANON" >&2; exit 2; }
TIME=/usr/bin/time
"$TIME" -v true >/dev/null 2>&1 || { echo "GNU time is needed at /usr/bin/time (-v)" >&2; exit 2; }
"$PY" -c 'import numpy, pandas, pyarrow' 2>/dev/null || { echo "$PY cannot import numpy, pandas and pyarrow (the series bundles and the comparison need them): PY=..." >&2; exit 2; }

# ---- 1. the store, staged once ------------------------------------------------
if [ -n "${STORE:-}" ]; then
  BASE=$STORE
  [ -d "$BASE" ] || { echo "STORE=$BASE is not a directory" >&2; exit 2; }
  if ls "$BASE/manifest" 2>/dev/null | grep -q '^kernel\.'; then
    echo "STORE=$BASE holds a run's outputs (manifest/kernel.*): it has served a run, and a build that decides differently would be refused when it writes the same run id. Stage a store with this script, or name one that was never served." >&2
    exit 2
  fi
  say "store: $BASE (given, not staged here)"
else
  for f in "$LAKE/curated/bars_by_date" "$SPXTR" "$VIX"; do [ -e "$f" ] || { echo "missing input: $f" >&2; exit 2; }; done
  # The two normalisations a lake copied from macOS needs (benchmark.md, "Traps"):
  # AppleDouble files read as corrupt Parquet, and the membership file must be
  # in a directory.
  if [ -n "$(find "$LAKE" -name '._*' -print -quit)" ]; then say "lake: deleting AppleDouble ._* files"; find "$LAKE" -name '._*' -delete; fi
  if [ ! -d "$LAKE/indexes/index_membership" ] && ls "$LAKE"/indexes/index_membership-*.parquet >/dev/null 2>&1; then
    say "lake: moving indexes/index_membership-*.parquet into indexes/index_membership/"
    mkdir -p "$LAKE/indexes/index_membership" && mv "$LAKE"/indexes/index_membership-*.parquet "$LAKE/indexes/index_membership/"
  fi
  # What writes a store: the data layer's and the block format's sources (not
  # their tests) and the series script.
  WRITER="crates/abt-data/src crates/abt-codec/src scripts/parity/mw14_series_bundles.py"
  # shellcheck disable=SC2086
  KEY=$( { (cd "$LAKE" && find . -type f | LC_ALL=C sort | tr '\n' '\0' | xargs -0 sha256sum); sha256sum < "$SPXTR"; sha256sum < "$VIX"; echo "$SPANS|$LISTS|$TICKERS|$NORGATE_OPTS|reviewed=complete"
           git rev-parse HEAD:crates/abt-data/src HEAD:crates/abt-codec/src HEAD:scripts/parity/mw14_series_bundles.py 2>/dev/null || echo no-git; git diff HEAD -- $WRITER 2>/dev/null || true; } | sha256sum | cut -c1-16)
  BASE=$BENCH/store-$KEY
  # KEY_ONLY=1: say which store this call would use, and stop.
  if [ "${KEY_ONLY:-0}" = 1 ]; then echo "$BASE"; exit 0; fi
  [ "${RESTAGE:-0}" != 1 ] || rm -rf "$BASE"
  if [ -f "$BASE/.staged" ]; then
    say "store: $BASE (staged $(cat "$BASE/.staged"))"
  else
    say "store: staging $BASE"
    rm -rf "$BASE" "$BENCH/series-$KEY"
    "$PY" scripts/parity/mw14_series_bundles.py "$SPXTR" "$VIX" "$BENCH/series-$KEY" >"$OUT/series.log" 2>&1
    for b in series-spxtr:1d series-weekly:1w; do
      "$ABT" data ingest --store "$BASE" --adapter bundle_v2 --from "$BENCH/series-$KEY/${b%%:*}" --class series --base "${b##*:}" >"$OUT/ingest_${b%%:*}.log" 2>&1
      say "  ${b%%:*} staged"
    done
    # In five-year chunks: the adapter holds every bar of a span in Arrow
    # before staging (2 to 8 GB a chunk).
    for span in $SPANS; do
      a=${span%%:*}; b=${span##*:}
      # shellcheck disable=SC2086
      "$ABT" data ingest --store "$BASE" --adapter norgate --from "$LAKE" --lists "$LISTS" --tickers "$TICKERS" --start "$a" --end "$b" $NORGATE_OPTS --attributes reviewed=complete >"$OUT/ingest_$a.log" 2>&1
      say "  $a staged: $(grep '^norgate:' "$OUT/ingest_$a.log" | cut -c1-100)"
    done
    date -u +%Y-%m-%dT%H:%M:%SZ >"$BASE/.staged"
  fi
fi

# ---- 2 to 4. the repetitions --------------------------------------------------
COMMIT=$(git rev-parse --short HEAD 2>/dev/null || echo unknown)
DIRTY=$(git status --porcelain 2>/dev/null | grep -vc '^??' || true)
MACHINE="$(uname -m) $(nproc) cpus $(awk '/MemTotal/{printf "%.0f GB", $2/1048576}' /proc/meminfo)"
SRV=; ST=
# The service is the child of `time`, which does not pass a signal on: end the
# child, and `time` ends with it and prints the service's totals.
stop_service() {
  [ -n "$SRV" ] || return 0
  pkill -TERM -P "$SRV" 2>/dev/null || kill -TERM "$SRV" 2>/dev/null || true
  wait "$SRV" 2>/dev/null || true
  SRV=
}
cleanup() { stop_service; if [ "$KEEP" != 1 ] && [ -n "$ST" ]; then rm -rf "$ST"; fi; true; }
trap cleanup EXIT
trap 'exit 130' INT TERM
for i in $(seq 1 "$N"); do
  D=$OUT/rep$i; mkdir -p "$D"
  ST=$BENCH/work-$$-$i
  rm -rf "$ST"
  cp -a --reflink=auto "$BASE" "$ST"
  SOCK=$D/data.sock
  rm -f "$SOCK"; : >"$D/run.log"
  "$TIME" -v "$ABT" data serve --store "$ST" --socket "$SOCK" >"$D/serve.log" 2>&1 &
  SRV=$!
  t0=$(date +%s.%N)
  # The service says `serving ...` once it is bound; the socket file alone
  # could be an earlier call's.
  UP=0
  for _ in $(seq 1 1200); do
    if [ -S "$SOCK" ] && grep -q '^serving ' "$D/serve.log" 2>/dev/null; then UP=1; break; fi
    kill -0 "$SRV" 2>/dev/null || break
    sleep 0.5
  done
  READY=$(awk -v a="$t0" -v b="$(date +%s.%N)" 'BEGIN{printf "%.2f", b - a}')
  RC=0
  if [ "$UP" = 1 ]; then
    say "rep $i: serving after ${READY}s"
    # shellcheck disable=SC2086
    ABT_TIMING_RELS=1 "$TIME" -v $WRAP "$ABT" run --strategy mw14 --data "$SOCK" --timing --history window --start "$START" --end "$END" ${THREADS:+--threads $THREADS} $EXTRA $CORPUS >"$D/run.log" 2>&1 || RC=$?
  else
    say "rep $i: the data service did not start; see $D/serve.log"
    RC=-1
  fi
  # `run <id>: the outputs are ...`; a refused run prints `run refused: ...`.
  RUN=$(grep -oE '^run [0-9a-f]+: the outputs' "$D/run.log" | head -1 | cut -d' ' -f2 | tr -d ':' || true)
  : >"$D/compare.json"
  if [ "$RC" = 0 ] && [ -n "$RUN" ]; then
    ABT="$ABT" "$PY" scripts/parity/mw14_compare.py "$ST" "$RUN" "$CANON" --json "$D/compare.json" >"$D/compare.log" 2>&1 || say "rep $i: the comparison failed; see $D/compare.log"
  fi
  # The service's own totals: GNU time prints them when it ends.
  stop_service
  [ "$KEEP" = 1 ] || rm -rf "$ST"
  ST=
  "$PY" - "$D" "$i" "$RC" "$RUN" "$READY" "$COMMIT" "$DIRTY" "$MACHINE" "${THREADS:-default}" "$BASE" <<'PY' | tee -a "$OUT/runs.jsonl"
import json, re, sys
d, rep, rc, run, ready, commit, dirty, machine, threads, base = sys.argv[1:11]

def gnu_time(path):
    out = {}
    try:
        text = open(path, errors="replace").read()
    except OSError:
        return out
    for key, pat in (("user_s", r"User time \(seconds\): ([0-9.]+)"), ("sys_s", r"System time \(seconds\): ([0-9.]+)"), ("rss_kb", r"Maximum resident set size \(kbytes\): ([0-9]+)")):
        m = re.findall(pat, text)
        if m:
            out[key] = float(m[-1])
    m = re.findall(r"Elapsed \(wall clock\) time \(h:mm:ss or m:ss\): ([0-9:.]+)", text)
    if m:
        s = 0.0
        for part in m[-1].split(":"):
            s = s * 60 + float(part)
        out["wall_s"] = s
    return out

log = open(f"{d}/run.log", errors="replace").read()
front, serve = gnu_time(f"{d}/run.log"), gnu_time(f"{d}/serve.log")
timing = {}
m = re.search(r"^timing (.*)$", log, re.M)
if m:
    for k, v in re.findall(r"(\w+)=(\S+)", m.group(1)):
        try:
            timing[k] = float(v) if "." in v else int(v)
        except ValueError:
            timing[k] = v
phases = {}
m = re.search(r"^phases (.*)$", log, re.M)
if m:
    phases = {k: float(v) for k, v in re.findall(r"(\w+)=([0-9.]+)", m.group(1))}
# The run's seconds no phase's clock covers: the phases that follow one another, taken from them.
unclocked = None
if phases and isinstance(timing.get("run_s"), float):
    unclocked = round(timing["run_s"] - sum(phases.get(k, 0.0) for k in ("bind", "check", "build", "spec", "push", "open", "stream", "write", "post")), 3)
derive = {}
for name, s, c, stuck, small in re.findall(r"^derive (\S+)\s+([0-9.]+) s\s+([0-9]+) calls(?: stuck_bars=([0-9]+) small_bars=([0-9]+))?", log, re.M):
    derive[name] = {"s": float(s), "calls": int(c)}
    if stuck:
        derive[name].update(stuck_bars=int(stuck), small_bars=int(small))
rows = {name: {"received": int(r), "held": int(h)} for name, r, h in re.findall(r"^rows (\S+)\s+received=([0-9]+) held=([0-9]+)", log, re.M)}
try:
    compare = json.load(open(f"{d}/compare.json"))
except (OSError, ValueError):
    compare = {}
gb = lambda kb: None if kb is None else round(kb / 1048576, 3)
line = {
    "rep": int(rep), "rc": int(rc), "run": run or None, "commit": commit, "dirty_files": int(dirty or 0), "machine": machine, "threads": threads, "store": base,
    "serve_ready_s": round(float(ready), 2),
    "front": {"wall_s": front.get("wall_s"), "user_s": front.get("user_s"), "sys_s": front.get("sys_s"), "rss_gb": gb(front.get("rss_kb")), "run_s": timing.get("run_s"),
              "rss_at_first_barrier_mb": timing.get("rss_at_first_barrier_mb"), "peak_rss_at_first_barrier_mb": timing.get("peak_rss_at_first_barrier_mb")},
    "service": {"cpu_s": None if "user_s" not in serve else round(serve["user_s"] + serve.get("sys_s", 0.0), 2), "user_s": serve.get("user_s"), "sys_s": serve.get("sys_s"), "rss_gb": gb(serve.get("rss_kb"))},
    "tuples": timing.get("tuples"), "trimmed_rows": timing.get("trimmed_rows"), "bars": timing.get("bars"), "symbols": timing.get("symbols"), "decisions": timing.get("decisions"),
    "prefetch_skipped": timing.get("prefetch_skipped"),
    "phases_s": phases, "unclocked_s": unclocked, "derive": derive, "rows": rows, "compare": compare,
}
print(json.dumps(line, separators=(",", ":")))
PY
  say "rep $i: rc=$RC run_s=$(grep -o 'run_s=[0-9.]*' "$D/run.log" | head -1 | cut -d= -f2) $(grep -o 'weekly correlation [0-9.]*' "$D/compare.log" 2>/dev/null | head -1)"
done
say "done: $OUT/runs.jsonl"
