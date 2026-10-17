#!/usr/bin/env bash
# Performance ladder on the real point-in-time S&P 500 bundle (docs/assessment.md
# records the results). Usage:
#
#   scripts/bench.sh LAKE OUTDIR
#
# LAKE is a local NDLake root (a mirror of the bucket). Env: RUNGS="100 250 500 0"
# (asset counts, 0 = every member since FROM), FROM=2014-01-01, LIMIT=1800
# (seconds per cell), ABT=target/release/abt, PY=python3.
#
# Each rung: ingest a subset of the lake into a store (`abt data ingest --adapter
# norgate --limit N`, the reviewed exceptions as the versions' attribute), serve it,
# then run momentum_12_1, low_volatility and total_return_momentum (n = a tenth of
# the names, at most 50; 20 long-only) over the socket with --timing. Prints a
# Markdown table and the log-log slope of run time against names.
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
CORPUS="corpus-v2/catalog corpus-v2/lib corpus-v2/strategies"
EXCEPTIONS=scripts/reviewed/norgate-spx-2014.parquet
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

# cell RUNG NAMES BARS STRATEGY -- abt run arguments...
cell() {
  local rung=$1 names=$2 bars=$3 strategy=$4 kernel=fold; shift 4
  local log="$OUT/run-$strategy-$rung.log"
  run_limited "$ABT" run --strategy "$strategy" --timing --quiet "$@" $CORPUS >"$log" 2>&1
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
  store="$OUT/store-$rung" sock="$OUT/data-$rung.sock"
  limit=(); [ "$rung" != 0 ] && limit=(--limit "$rung")
  rm -rf "$store"; t=$(now)
  tflag=-v; [ "$(uname)" = Darwin ] && tflag=-l
  /usr/bin/time $tflag "$ABT" data ingest --store "$store" --adapter norgate --from "$LAKE" --start "$FROM" ${limit[@]+"${limit[@]}"} \
    --exceptions "$EXCEPTIONS" --attributes reviewed=complete --report "$OUT/report-$rung.json" >"$OUT/ingest-$rung.log" 2>&1 \
    || { echo "ingest $rung failed: $(grep -v '^ ' "$OUT/ingest-$rung.log" | tail -1)" >&2; continue; }
  ingest_s=$(since "$t")
  names=$(sed -n 's/^norgate: \([0-9]*\) assets.*/\1/p' "$OUT/ingest-$rung.log")
  rows=$(sed -n 's/^norgate: [0-9]* assets, \([0-9]*\) bars.*/\1/p' "$OUT/ingest-$rung.log")
  bars=$("$PY" -c 'import json, sys; print(json.load(open(sys.argv[1]))["days"])' "$OUT/report-$rung.json")
  ingest_rss=$(grep -E 'maximum resident set size|Maximum resident' "$OUT/ingest-$rung.log" | grep -o '[0-9]*' | head -1)
  echo "== rung $rung: $names names, $bars bars, $rows price rows; ingest ${ingest_s}s (peak RSS ${ingest_rss})" >&2
  echo "$names	$rows	$ingest_s	$ingest_rss" >>"$OUT/ingest.tsv"
  rm -f "$sock"
  "$ABT" data serve --store "$store" --socket "$sock" 2>"$OUT/serve-$rung.log" &
  server=$!
  for _ in $(seq 200); do [ -S "$sock" ] && break; sleep 0.05; done
  for s in momentum_12_1 low_volatility total_return_momentum; do
    # Legs of n names each must not overlap: n is a tenth of the universe, at most 50 (20 long-only).
    n=$(( names / 10 )); [ $n -gt 50 ] && n=50; [ "$s" = total_return_momentum ] && [ $n -gt 20 ] && n=20
    cell "$rung" "$names" "$bars" "$s" --data "$sock" --param "n=$n"
  done
  kill "$server" 2>/dev/null; wait "$server" 2>/dev/null
done

echo
if [ "$(uname)" = Darwin ]; then
  echo "Machine: $(sysctl -n machdep.cpu.brand_string), $(sysctl -n hw.ncpu) cores, $(( $(sysctl -n hw.memsize) / 1073741824 )) GB RAM, macOS $(sw_vers -productVersion)"
else
  echo "Machine: $(grep -m1 'model name' /proc/cpuinfo | cut -d: -f2 | xargs), $(nproc) cores, $(awk '/MemTotal/ {printf "%d", $2/1048576}' /proc/meminfo) GB RAM, $(uname -sr)"
fi
echo "abt $(git rev-parse --short HEAD 2>/dev/null), $(rustc --version 2>/dev/null || ~/.cargo/bin/rustc --version 2>/dev/null || echo rustc unknown)"
echo
echo "Ingest (the lake into a store, abt data ingest --adapter norgate):"
echo
echo "| names | price rows | ingest s | ingest peak RSS |"
echo "| ---: | ---: | ---: | ---: |"
awk -F'\t' '{printf "| %s | %s | %s | %s |\n", $1,$2,$3,$4}' "$OUT/ingest.tsv"
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
    if status == "ok" and float(run) > 0:
        pts[s].append((math.log(int(names)), math.log(float(run)), math.log(float(rss))))
for s, p in sorted(pts.items()):
    if len(p) < 2:
        continue
    def slope(i):
        mx = sum(x[0] for x in p) / len(p); my = sum(x[i] for x in p) / len(p)
        return sum((x[0] - mx) * (x[i] - my) for x in p) / sum((x[0] - mx) ** 2 for x in p)
    print(f"log-log slope against names, {s}: run time {slope(1):.2f}, peak RSS {slope(2):.2f}")
EOF
