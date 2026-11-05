#!/bin/bash
# MW14 profiling suite -- five independent lines of evidence.
# Performance cells only: the rung and ablation runs produce WRONG returns by
# construction. The one cell that is a result is the parity re-check at the end.
set -uo pipefail

ABT_DIR=$HOME/absolute-backtest
BENCH=${BENCH_SCRATCH:-/data/bench}
IN=$BENCH/IN
B=$BENCH/B
P=/data/prof
PROFTGT=/data/target-prof
PROF=$PROFTGT/release/abt
REL=$ABT_DIR/target/release/abt
PY=${BENCH_PY:-}
TSV=$P/cells.tsv
mkdir -p $P

RUN=(run --strategy mw14 --untested --price-relation trclose --actions in-prices
     --compounding off --capital 1000000 --lot fractional --frictionless
     --commission-bps 10 --margin-rate 0 --on-leverage allow
     --delist-proceeds last-price --periods-per-year 52 --window-sums exact
     --start 1991-01-01)

say() { echo "[$(date -u +%H:%M:%S)] $*"; }

# --- preflight: resolve and prove every path before spending an hour ----------
preflight() {
  [ -n "$PY" ] || for c in $(command -v python3.12) /data/venv/bin/python $(command -v python3); do
    [ -x "$c" ] && PY=$c && break
  done
  say "preflight"
  for p in "$ABT_DIR" "$IN" "$B" "$REL" "$PY" "$ABT_DIR/corpus/company/mw14.dsl"; do
    [ -e "$p" ] || { echo "MISSING: $p"; exit 2; }
    echo "  ok $p"
  done
  $PY -c 'import pandas, pyarrow; print("  ok pandas", pandas.__version__)' || exit 2
  pgrep -f mk_engine_bench >/dev/null && { echo "REFUSING: the benchmark is still running"; exit 3; }
  echo "  ok nothing else is running"
  nproc; uptime
}

# cell <tag> <label> <binary> <dsl> [extra args...]
cell() {
  local tag=$1 label=$2 bin=$3 dsl=$4; shift 4
  local log=$P/cell_$tag.log
  say "cell $tag ($label)"
  ( cd "$ABT_DIR" && "$bin" "${RUN[@]}" "$@" --bundle "$B" --timing corpus/env "$dsl" ) \
    >"$log" 2>&1
  local rc=$?
  local t=$(grep -o '^timing .*' "$log" | tail -1)
  [ -n "$t" ] || { say "  cell $tag produced NO timing line (rc $rc) -- see $log"; return 1; }
  local run_s=$(sed -E 's/.*run_s=([0-9.]+).*/\1/' <<<"$t")
  local bars=$(sed -E 's/.*bars=([0-9]+).*/\1/' <<<"$t")
  local wc=$(sed -E 's/.*window_calls=([0-9]+).*/\1/' <<<"$t")
  local wbs=$(sed -E 's/.*window_bars_solved=([0-9]+).*/\1/' <<<"$t")
  local me=$(sed -E 's/.*memo_entries=([0-9]+).*/\1/' <<<"$t")
  local rss=$(sed -E 's/.*peak_rss_mb=([0-9.]+).*/\1/' <<<"$t")
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$tag" "$label" "$run_s" "$bars" "$wc" "$wbs" "$me" "$rss" "$rc" >> $TSV
  say "  run_s=$run_s bars=$bars window_calls=$wc rc=$rc"
}

# --- tools --------------------------------------------------------------------
tools() {
  say "installing perf, valgrind, FlameGraph"
  sudo dnf install -y perf valgrind git >/dev/null 2>&1
  [ -d /data/FlameGraph ] || sudo git clone --depth 1 -q https://github.com/brendangregg/FlameGraph /data/FlameGraph
  sudo chown -R "$USER" /data/FlameGraph 2>/dev/null
  sudo sysctl -w kernel.perf_event_paranoid=1 >/dev/null
  perf --version; valgrind --version
}

# The release profile carries no frame pointers and no DWARF CFI, so the timed
# binary cannot be unwound. This is a SECOND binary, built for the profile only.
build_prof() {
  say "building the profiling binary (frame pointers + full debuginfo)"
  ( cd "$ABT_DIR" && CARGO_TARGET_DIR=$PROFTGT \
      RUSTFLAGS="-C force-frame-pointers=yes -C debuginfo=2" \
      cargo build --release --bin abt ) || exit 2
  ls -l $PROF
}

p1_perf() {
  say "P1 perf record (full window, cpu-clock, frame-pointer stacks)"
  ( cd "$ABT_DIR" && perf record -e cpu-clock -F 499 -g --call-graph fp \
      -o $P/mw14.data -- $PROF "${RUN[@]}" --end 2026-07-02 --bundle "$B" --timing \
      corpus/env corpus/company/mw14.dsl ) >$P/p1_run.log 2>&1
  grep -o '^timing .*' $P/p1_run.log | tail -1 > $P/p1_timing.txt
  cat $P/p1_timing.txt
  perf report -i $P/mw14.data --stdio --no-children --percent-limit 0.2 \
      --sort symbol 2>/dev/null > $P/p1_self.txt
  perf report -i $P/mw14.data --stdio --children --percent-limit 1.0 \
      --sort symbol 2>/dev/null > $P/p1_tree.txt
  perf report -i $P/mw14.data --stdio --no-children --percent-limit 0.2 --inline \
      --sort srcline 2>/dev/null > $P/p1_srcline.txt
  perf script -i $P/mw14.data > $P/p1.script 2>/dev/null
  /data/FlameGraph/stackcollapse-perf.pl $P/p1.script > $P/p1.folded 2>/dev/null
  /data/FlameGraph/flamegraph.pl --title "" --width 1600 $P/p1.folded > $P/mw14-flamegraph.svg
  wc -l $P/p1.folded $P/p1_self.txt; ls -l $P/mw14-flamegraph.svg
  head -40 $P/p1_self.txt
}

p2_callgrind() {
  say "P2 callgrind (1991-1992 only; call counts and instruction shares, never seconds)"
  ( cd "$ABT_DIR" && valgrind --tool=callgrind --cache-sim=no --branch-sim=no \
      --callgrind-out-file=$P/cg.out \
      $PROF "${RUN[@]}" --end 1992-01-01 --bundle "$B" --timing \
      corpus/env corpus/company/mw14.dsl ) >$P/p2_run.log 2>&1
  callgrind_annotate --threshold=98 $P/cg.out > $P/p2_annotate.txt 2>&1
  head -60 $P/p2_annotate.txt
}

p3_rungs() {
  say "P3 universe rungs"
  for n in 250 500 1000 2000; do
    local sin=$BENCH/IN_$n sb=$BENCH/B_$n
    if [ ! -f $sb/manifest.json ]; then
      $PY "$P/prof_prep.py" subset --inputs "$IN" --out "$sin" --names $n > $P/subset_$n.json || return 1
      rm -rf "$sb"
      ( cd "$ABT_DIR" && $REL bundle build --from "$sin" --env equities_1w --version mw14 \
          --out "$sb" corpus/env corpus/company/mw14.dsl ) > $P/bundle_$n.log 2>&1
    fi
    local log=$P/cell_rung$n.log
    ( cd "$ABT_DIR" && $REL "${RUN[@]}" --end 2026-07-02 --bundle "$sb" --timing \
        corpus/env corpus/company/mw14.dsl ) > "$log" 2>&1
    local t=$(grep -o '^timing .*' "$log" | tail -1)
    printf 'rung%s\t%s names\t%s\t%s\t%s\t%s\t%s\t%s\t0\n' "$n" "$n" \
      "$(sed -E 's/.*run_s=([0-9.]+).*/\1/' <<<"$t")" \
      "$(sed -E 's/.*bars=([0-9]+).*/\1/' <<<"$t")" \
      "$(sed -E 's/.*window_calls=([0-9]+).*/\1/' <<<"$t")" \
      "$(sed -E 's/.*window_bars_solved=([0-9]+).*/\1/' <<<"$t")" \
      "$(sed -E 's/.*memo_entries=([0-9]+).*/\1/' <<<"$t")" \
      "$(sed -E 's/.*peak_rss_mb=([0-9.]+).*/\1/' <<<"$t")" >> $TSV
    say "  rung $n: $t"
  done
  cell rung4240 "4240 names (all)" "$REL" corpus/company/mw14.dsl --end 2026-07-02
}

p4_eras() {
  say "P4 era sweep (cumulative --end, differenced afterwards)"
  for e in 1996-01-01 2004-01-01 2012-01-01 2020-01-01 2026-07-02; do
    cell "era$e" "to $e" "$REL" corpus/company/mw14.dsl --end "$e"
  done
}

p5_ablations() {
  say "P5 relation ablations (full window; WRONG returns by construction)"
  $PY "$P/prof_prep.py" ablate --dsl "$ABT_DIR/corpus/company/mw14.dsl" --out /data/abl || return 1
  for a in ma clenow dvol4 gate cumf lp dial rank; do
    cell "abl_$a" "stub $a" "$REL" /data/abl/mw14_abl_$a.dsl --end 2026-07-02
  done
}

parity() {
  say "parity re-check on the untouched DSL (the only cell that is a result)"
  cell parity "parity" "$REL" corpus/company/mw14.dsl --end 2026-07-02 \
       --returns $P/parity_ret.parquet
  grep -E "CAGR|max drawdown|Sharpe|P&L|cagr|sharpe" $P/cell_parity.log | head -10
}

case "${1:-all}" in
  preflight) preflight ;;
  all) preflight && tools && build_prof && p1_perf && p2_callgrind && p3_rungs \
       && p4_eras && p5_ablations && parity && say "SUITE DONE" && cat $TSV ;;
  *) "$1" ;;
esac
