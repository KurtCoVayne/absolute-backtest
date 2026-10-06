#!/usr/bin/env bash
# Performance ladder on the real point-in-time S&P 500 bundle (docs/assessment.md
# records the results). Usage:
#
#   scripts/bench.sh LAKE OUTDIR
#
# LAKE is the NDLake root (a local mirror or s3://...). Env: RUNGS="100 250 500 0"
# (asset counts, 0 = every member since FROM), FROM=2014-01-01, LIMIT=1800
# (seconds per cell), ABT=target/release/abt, PY=python3 (with duckdb).
#
# Each rung: convert a subset of the lake (scripts/ingest/norgate_lake_to_inputs.py
# --limit N), build and test the bundle, then run momentum_12_1, low_volatility
# and total_return_momentum (n = a tenth of the names, at most 50; 20 long-only) with --timing on the batch kernel, and on the fold
# kernel at the 500 rung. Prints a Markdown table and the log-log slope of run
# time against names.
#
# Synthetic markets are not used: `abt synth` (src/data.rs) scales drift and
# volatility with the symbol index, so at hundreds of names its prices
# collapse to zero within months (docs/assessment.md).
set -u
LAKE=${1:?usage: scripts/bench.sh LAKE OUTDIR}
OUT=${2:?usage: scripts/bench.sh LAKE OUTDIR}
RUNGS=${RUNGS:-"100 250 500 0"}
FROM=${FROM:-2014-01-01}
LIMIT=${LIMIT:-1800}
ABT=${ABT:-target/release/abt}
PY=${PY:-python3}
CORPUS="corpus/env corpus/lib corpus/strategies"
EXCEPTIONS=scripts/ingest/reviewed/norgate-spx-2014.parquet
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

now() { "$PY" -c 'import time; print(time.time())'; }
since() { "$PY" -c "print(round($(now) - $1, 2))"; }
field() { sed -n "s/.*[ ]$2=\([^ ]*\).*/\1/p" <<<"$1"; }

# cell RUNG NAMES BARS STRATEGY KERNEL -- abt run arguments...
cell() {
  local rung=$1 names=$2 bars=$3 strategy=$4 kernel=$5; shift 5
  local log="$OUT/run-$strategy-$kernel-$rung.log"
  run_limited "$ABT" run --strategy "$strategy" --kernel "$kernel" --timing --quiet "$@" $CORPUS >"$log" 2>&1
  local code=$?
  local line; line=$(grep '^timing ' "$log" | tail -1)
  if [ $code -ne 0 ] || [ -z "$line" ]; then
    local why="exit $code"; [ $code -eq 124 ] && why="timeout ${LIMIT}s"
    printf '%s\t%s\t%s\t%s\t-\t-\t-\t-\t%s\n' "$names" "$bars" "$strategy" "$kernel" "$why" >>"$ROWS"
    echo "  $strategy $kernel $names names: $why (see $log)" >&2
    return
  fi
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\tok\n' "$names" "$bars" "$strategy" "$kernel" \
    "$(field "$line" load_s)" "$(field "$line" run_s)" "$(field "$line" bars_per_s)" "$(field "$line" peak_rss_mb)" >>"$ROWS"
  echo "  $strategy $kernel $names names: $line" >&2
}

for rung in $RUNGS; do
  inputs="$OUT/inputs-$rung" bundle="$OUT/bundle-$rung"
  limit=(); [ "$rung" != 0 ] && limit=(--limit "$rung")
  t=$(now)
  "$PY" scripts/ingest/norgate_lake_to_inputs.py --lake "$LAKE" --out "$inputs" --from "$FROM" ${limit[@]+"${limit[@]}"} --exceptions "$EXCEPTIONS" 2>"$OUT/convert-$rung.log" || { echo "convert $rung failed" >&2; continue; }
  convert_s=$(since "$t")
  rm -rf "$bundle"; t=$(now)
  tflag=-v; [ "$(uname)" = Darwin ] && tflag=-l
  /usr/bin/time $tflag "$ABT" bundle build --from-norgate "$inputs" --env equities_1d_v2 --version bench --out "$bundle" $CORPUS >"$OUT/build-$rung.log" 2>&1 \
    || { echo "bundle build $rung failed: $(grep -v '^ ' "$OUT/build-$rung.log" | tail -1)" >&2; continue; }
  build_s=$(since "$t"); t=$(now)
  "$ABT" bundle test "$bundle" $CORPUS >"$OUT/test-$rung.log" 2>&1
  test_s=$(since "$t")
  names=$(grep -o '[0-9]* securities' "$OUT/build-$rung.log" | head -1 | cut -d' ' -f1)
  bars=$(grep -o 'over [0-9]* trading days' "$OUT/build-$rung.log" | head -1 | cut -d' ' -f2)
  rows=$(grep -o '^note: [0-9]* price rows' "$OUT/build-$rung.log" | cut -d' ' -f2)
  build_rss=$(grep -E 'maximum resident set size|Maximum resident' "$OUT/build-$rung.log" | grep -o '[0-9]*' | head -1)
  echo "== rung $rung: $names names, $bars bars, $rows price rows; convert ${convert_s}s, bundle build ${build_s}s (peak RSS ${build_rss}), bundle test ${test_s}s: $(tail -1 "$OUT/test-$rung.log")" >&2
  echo "$names	$rows	$convert_s	$build_s	$build_rss	$test_s	$(tail -1 "$OUT/test-$rung.log")" >>"$OUT/ingest.tsv"
  for s in momentum_12_1 low_volatility total_return_momentum; do
    # Legs of n names each must not overlap: n is a tenth of the universe, at most 50 (20 long-only).
    n=$(( names / 10 )); [ $n -gt 50 ] && n=50; [ "$s" = total_return_momentum ] && [ $n -gt 20 ] && n=20
    cell "$rung" "$names" "$bars" "$s" batch --bundle "$bundle" --param "n=$n"
  done
  if [ "$rung" = 500 ]; then
    cell "$rung" "$names" "$bars" low_volatility fold --bundle "$bundle" --param "n=$(( names / 10 > 50 ? 50 : names / 10 ))"
  fi
done

echo
if [ "$(uname)" = Darwin ]; then
  echo "Machine: $(sysctl -n machdep.cpu.brand_string), $(sysctl -n hw.ncpu) cores, $(( $(sysctl -n hw.memsize) / 1073741824 )) GB RAM, macOS $(sw_vers -productVersion)"
else
  echo "Machine: $(grep -m1 'model name' /proc/cpuinfo | cut -d: -f2 | xargs), $(nproc) cores, $(awk '/MemTotal/ {printf "%d", $2/1048576}' /proc/meminfo) GB RAM, $(uname -sr)"
fi
echo "abt $(git rev-parse --short HEAD 2>/dev/null), $(rustc --version 2>/dev/null || ~/.cargo/bin/rustc --version 2>/dev/null || echo rustc unknown)"
echo
echo "Ingest (convert = lake to Parquet inputs, build = inputs to bundle):"
echo
echo "| names | price rows | convert s | bundle build s | build peak RSS bytes | bundle test s | test result |"
echo "| ---: | ---: | ---: | ---: | ---: | ---: | --- |"
awk -F'\t' '{printf "| %s | %s | %s | %s | %s | %s | %s |\n", $1,$2,$3,$4,$5,$6,$7}' "$OUT/ingest.tsv"
echo
echo "| names | bars | strategy | kernel | load s | run s | bars/s | peak RSS MB | status |"
echo "| ---: | ---: | --- | --- | ---: | ---: | ---: | ---: | --- |"
awk -F'\t' '{printf "| %s | %s | %s | %s | %s | %s | %s | %s | %s |\n", $1,$2,$3,$4,$5,$6,$7,$8,$9}' "$ROWS"
echo
"$PY" - "$ROWS" <<'EOF'
import math, sys
from collections import defaultdict
pts = defaultdict(list)
for line in open(sys.argv[1]):
    names, bars, s, kernel, load, run, bps, rss, status = line.rstrip("\n").split("\t")
    if status == "ok" and kernel == "batch" and float(run) > 0:
        pts[s].append((math.log(int(names)), math.log(float(run)), math.log(float(rss))))
for s, p in sorted(pts.items()):
    if len(p) < 2:
        continue
    def slope(i):
        mx = sum(x[0] for x in p) / len(p); my = sum(x[i] for x in p) / len(p)
        return sum((x[0] - mx) * (x[i] - my) for x in p) / sum((x[0] - mx) ** 2 for x in p)
    print(f"log-log slope against names, {s}: run time {slope(1):.2f}, peak RSS {slope(2):.2f}")
EOF
