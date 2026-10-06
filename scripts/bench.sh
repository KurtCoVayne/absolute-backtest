#!/usr/bin/env bash
# Performance ladder on synthetic markets (docs/assessment.md records the
# results). Usage: scripts/bench.sh [OUTDIR] ; env NAMES="100 500 1000",
# DAYS=2500, LIMIT=1800 (seconds per cell), ABT=target/release/abt.
#
# For each universe size: `abt synth` daily markets for equities_1d (the
# sma_crossover and momentum_top_n strategies) and equities_1d_v2
# (low_volatility), every strategy with --timing on the batch kernel, the
# fold kernel at 500 names, and at 500 names one bundle build -> bundle test
# -> run --bundle leg to time the Parquet path. Prints a Markdown table and
# the log-log slope of run time against names per strategy.
set -u
OUT=${1:-${TMPDIR:-/tmp}/abt-bench}
NAMES=${NAMES:-"100 500 1000"}
DAYS=${DAYS:-2500}
LIMIT=${LIMIT:-1800}
ABT=${ABT:-target/release/abt}
CORPUS="corpus/env corpus/lib corpus/strategies"
mkdir -p "$OUT"
ROWS="$OUT/rows.tsv"
: >"$ROWS"

# A timeout that exists on macOS too (GNU timeout is not there by default).
run_limited() {
  if command -v timeout >/dev/null 2>&1; then timeout "$LIMIT" "$@"; return $?; fi
  perl -e 'my $s = shift; my $pid = fork(); if ($pid == 0) { exec @ARGV or exit 127 }
           $SIG{ALRM} = sub { kill "TERM", $pid; sleep 2; kill "KILL", $pid; exit 124 };
           alarm $s; waitpid($pid, 0); exit($? >> 8);' "$LIMIT" "$@"
}

symbols() { local n=$1; seq -f "S%04g" 1 "$n" | paste -sd, -; }

field() { sed -n "s/.*[ ]$2=\([^ ]*\).*/\1/p" <<<"$1"; }

# cell NAMES STRATEGY KERNEL LABEL -- abt run arguments...
cell() {
  local n=$1 strategy=$2 kernel=$3 label=$4; shift 4
  local log="$OUT/$label-$strategy-$kernel-$n.log"
  run_limited "$ABT" run --strategy "$strategy" --kernel "$kernel" --timing --quiet "$@" $CORPUS >"$log" 2>&1
  local code=$?
  local line; line=$(grep '^timing ' "$log" | tail -1)
  if [ $code -ne 0 ] || [ -z "$line" ]; then
    local why="exit $code"; [ $code -eq 124 ] && why="timeout ${LIMIT}s"
    printf '%s\t%s\t%s\t%s\t%s\t-\t-\t-\t-\t%s\n' "$n" "$DAYS" "$strategy" "$kernel" "$label" "$why" >>"$ROWS"
    echo "  $label $strategy $kernel n=$n: $why (see $log)" >&2
    return
  fi
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\tok\n' "$n" "$DAYS" "$strategy" "$kernel" "$label" \
    "$(field "$line" load_s)" "$(field "$line" run_s)" "$(field "$line" bars_per_s)" "$(field "$line" peak_rss_mb)" >>"$ROWS"
  echo "  $label $strategy $kernel n=$n: $line" >&2
}

for n in $NAMES; do
  syms=$(symbols "$n")
  echo "== $n names x $DAYS days" >&2
  for env in equities_1d equities_1d_v2; do
    [ -d "$OUT/data-$env-$n" ] || "$ABT" synth --env "$env" --symbols "$syms" --days "$DAYS" --out "$OUT/data-$env-$n" $CORPUS >/dev/null
  done
  for s in sma_crossover momentum_top_n; do cell "$n" "$s" batch csv --data "$OUT/data-equities_1d-$n"; done
  cell "$n" low_volatility batch csv --data "$OUT/data-equities_1d_v2-$n"
  if [ "$n" = 500 ]; then
    for s in sma_crossover momentum_top_n; do cell "$n" "$s" fold csv --data "$OUT/data-equities_1d-$n"; done
    cell "$n" low_volatility fold csv --data "$OUT/data-equities_1d_v2-$n"
    # The Parquet path: build, test, run from the bundle.
    b="$OUT/bundle-$n"; rm -rf "$b"
    t0=$(date +%s.%N 2>/dev/null || python3 -c 'import time; print(time.time())')
    "$ABT" bundle build --synthetic --symbols "$syms" --days "$DAYS" --env equities_1d_v2 --version bench --out "$b" $CORPUS >"$OUT/bundle-build-$n.log" 2>&1
    t1=$(python3 -c 'import time; print(time.time())')
    "$ABT" bundle test "$b" $CORPUS >"$OUT/bundle-test-$n.log" 2>&1
    t2=$(python3 -c 'import time; print(time.time())')
    echo "  bundle build $(python3 -c "print(round($t1-$t0,2))") s, bundle test $(python3 -c "print(round($t2-$t1,2))") s: $(tail -1 "$OUT/bundle-test-$n.log")" >&2
    cell "$n" low_volatility batch bundle --bundle "$b"
  fi
done

echo
if [ "$(uname)" = Darwin ]; then
  echo "Machine: $(sysctl -n machdep.cpu.brand_string), $(sysctl -n hw.ncpu) cores, $(( $(sysctl -n hw.memsize) / 1073741824 )) GB RAM, macOS $(sw_vers -productVersion)"
else
  echo "Machine: $(grep -m1 'model name' /proc/cpuinfo | cut -d: -f2 | xargs), $(nproc) cores, $(awk '/MemTotal/ {printf "%d", $2/1048576}' /proc/meminfo) GB RAM, $(uname -sr)"
fi
echo "abt $(git rev-parse --short HEAD 2>/dev/null), $(rustc --version 2>/dev/null)"
echo
echo "| names | days | strategy | kernel | data | load s | run s | bars/s | peak RSS MB | status |"
echo "| ---: | ---: | --- | --- | --- | ---: | ---: | ---: | ---: | --- |"
awk -F'\t' '{printf "| %s | %s | %s | %s | %s | %s | %s | %s | %s | %s |\n", $1,$2,$3,$4,$5,$6,$7,$8,$9,$10}' "$ROWS"
echo
# Log-log slope of run time against names (batch kernel, CSV data).
python3 - "$ROWS" <<'EOF'
import math, sys
from collections import defaultdict
pts = defaultdict(list)
for line in open(sys.argv[1]):
    n, days, s, kernel, data, load, run, bps, rss, status = line.rstrip("\n").split("\t")
    if status == "ok" and kernel == "batch" and data == "csv" and float(run) > 0:
        pts[s].append((math.log(int(n)), math.log(float(run))))
for s, p in sorted(pts.items()):
    if len(p) < 2:
        continue
    mx = sum(x for x, _ in p) / len(p); my = sum(y for _, y in p) / len(p)
    slope = sum((x - mx) * (y - my) for x, y in p) / sum((x - mx) ** 2 for x, _ in p)
    print(f"log-log slope of run time against names, {s}: {slope:.2f}")
EOF
