#!/usr/bin/env bash
# The performance measurements and the targets of docs/spec/00-architecture.md
# 7.3 (G10; docs/assessment.md records the results). Usage:
#
#   scripts/bench.sh LAKE|none OUTDIR
#
# Four parts, each skipped when its input is missing:
#
# 1. The ladder on the real point-in-time S&P 500 lake. LAKE is a local NDLake
#    root (a mirror of the bucket); `none` skips it. Each rung ingests a subset
#    into a store (`abt data ingest --adapter norgate --limit N`, the reviewed
#    exceptions as the versions' attribute), serves it, and runs
#    momentum_12_1, low_volatility and total_return_momentum over the socket
#    with --timing (n is a tenth of the names, at most 50; 20 long-only).
#    Env: RUNGS="100 250 500 0" (asset counts, 0 = every member since FROM),
#    FROM=2014-01-01.
# 2. The ladder on synthetic markets (A9 of docs/assessment.md is fixed, so a
#    market of hundreds of names stays priced): momentum_12_1 in process.
#    Env: SYNTH_RUNGS="100 300" ("" skips it), SYNTH_DAYS=2520.
# 3. The data layer's budgets (D-9.1), from the ignored test
#    crates/abt-data/tests/budgets.rs. Env: BUDGETS=1 (0 skips it).
# 4. The two books' full replays. Env: MW14_STORE, R8L_STORE: stores holding
#    their data (as scripts/parity uses them); unset skips a book.
#
# Env everywhere: LIMIT=1800 (seconds per cell), ABT=target/release/abt,
# PY=python3. Prints Markdown tables, the log-log slope of run time against
# names, and the targets of 7.3 against what was measured.
set -u
LAKE=${1:?usage: scripts/bench.sh LAKE|none OUTDIR}
OUT=${2:?usage: scripts/bench.sh LAKE|none OUTDIR}
RUNGS=${RUNGS:-"100 250 500 0"}
FROM=${FROM:-2014-01-01}
SYNTH_RUNGS=${SYNTH_RUNGS-"100 300"}
SYNTH_DAYS=${SYNTH_DAYS:-2520}
BUDGETS=${BUDGETS:-1}
MW14_STORE=${MW14_STORE:-}
R8L_STORE=${R8L_STORE:-}
LIMIT=${LIMIT:-1800}
ABT=${ABT:-target/release/abt}
PY=${PY:-python3}
CORPUS="corpus-v2/catalog corpus-v2/lib corpus-v2/strategies"
EXCEPTIONS=scripts/reviewed/norgate-spx-2014.parquet
mkdir -p "$OUT"
ROWS="$OUT/rows.tsv"
BOOKS="$OUT/books.tsv"
: >"$ROWS"
: >"$BOOKS"
: >"$OUT/ingest.tsv"

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

# serve STORE SOCKET: a data service in the background; its pid in $server.
serve() {
  rm -f "$2"
  "$ABT" data serve --store "$1" --socket "$2" 2>"$2.log" &
  server=$!
  for _ in $(seq 200); do [ -S "$2" ] && break; sleep 0.05; done
}

# cell RUNG NAMES BARS STRATEGY KERNEL -- abt run arguments...
cell() {
  local rung=$1 names=$2 bars=$3 strategy=$4 kernel=$5; shift 5
  local log="$OUT/run-$strategy-$kernel-$rung.log"
  run_limited "$ABT" run --strategy "$strategy" --timing --quiet "$@" $CORPUS >"$log" 2>&1
  local code=$?
  local line; line=$(grep '^timing ' "$log" | tail -1)
  if [ $code -ne 0 ] || [ -z "$line" ]; then
    local why="exit $code"; [ $code -eq 124 ] && why="timeout ${LIMIT}s"
    printf '%s\t%s\t%s\t%s\t-\t-\t-\t-\t%s\n' "$names" "$bars" "$strategy" "$kernel" "$why" >>"$ROWS"
    echo "  $strategy $kernel $names names: $why (see $log)" >&2
    return
  fi
  [ "$bars" = - ] && bars=$(field "$line" bars)
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\tok\n' "$names" "$bars" "$strategy" "$kernel" \
    "$(field "$line" load_s)" "$(field "$line" run_s)" "$(field "$line" bars_per_s)" "$(field "$line" peak_rss_mb)" >>"$ROWS"
  echo "  $strategy $kernel $names names: $line" >&2
}

# 1. The lake.
if [ "$LAKE" != none ]; then
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
    serve "$store" "$sock"
    for s in momentum_12_1 low_volatility total_return_momentum; do
      # Legs of n names each must not overlap: n is a tenth of the universe, at most 50 (20 long-only).
      n=$(( names / 10 )); [ $n -gt 50 ] && n=50; [ "$s" = total_return_momentum ] && [ $n -gt 20 ] && n=20
      cell "$rung" "$names" "$bars" "$s" lake --data "$sock" --param "n=$n"
    done
    kill "$server" 2>/dev/null; wait "$server" 2>/dev/null
  done
fi

# 2. Synthetic markets.
for names in $SYNTH_RUNGS; do
  syms=$("$PY" -c "import sys; print(','.join('S%03d' % i for i in range(int(sys.argv[1]))))" "$names")
  n=$(( names / 10 )); [ $n -gt 50 ] && n=50
  cell "$names" "$names" - momentum_12_1 synthetic --synthetic --days "$SYNTH_DAYS" --symbols "$syms" --param "n=$n"
done

# 3. The data layer's budgets.
BUDGET_LOG="$OUT/budgets.log"
: >"$BUDGET_LOG"
if [ "$BUDGETS" = 1 ]; then
  echo "== the data layer's budgets (crates/abt-data/tests/budgets.rs)" >&2
  cargo test -q -p abt-data --release --test budgets -- --ignored --nocapture >"$BUDGET_LOG" 2>&1 \
    || echo "  the budgets test failed (see $BUDGET_LOG)" >&2
fi

# 4. The books.
book() {
  local name=$1 store=$2 sock="$OUT/data-$1.sock"
  [ -n "$store" ] || return
  serve "$store" "$sock"
  local log="$OUT/run-$name.log"
  run_limited "$ABT" run --strategy "$name" --data "$sock" --timing --quiet corpus-v2/catalog corpus-v2/lib "corpus-v2/company/$name.dsl" >"$log" 2>&1
  local line; line=$(grep '^timing ' "$log" | tail -1)
  kill "$server" 2>/dev/null; wait "$server" 2>/dev/null
  if [ -z "$line" ]; then echo "  $name: no timing (see $log)" >&2; return; fi
  printf '%s\t%s\t%s\t%s\n' "$name" "$(field "$line" run_s)" "$(field "$line" peak_rss_mb)" "$(field "$line" bars)" >>"$BOOKS"
  echo "  $name: $line" >&2
}
book mw14 "$MW14_STORE"
book r8l "$R8L_STORE"

echo
if [ "$(uname)" = Darwin ]; then
  echo "Machine: $(sysctl -n machdep.cpu.brand_string), $(sysctl -n hw.ncpu) cores, $(( $(sysctl -n hw.memsize) / 1073741824 )) GB RAM, macOS $(sw_vers -productVersion)"
else
  echo "Machine: $(grep -m1 'model name' /proc/cpuinfo | cut -d: -f2 | xargs), $(nproc) cores, $(awk '/MemTotal/ {printf "%d", $2/1048576}' /proc/meminfo) GB RAM, $(uname -sr)"
fi
echo "abt $(git rev-parse --short HEAD 2>/dev/null), $(rustc --version 2>/dev/null || ~/.cargo/bin/rustc --version 2>/dev/null || echo rustc unknown)"
if [ -s "$OUT/ingest.tsv" ]; then
  echo
  echo "Ingest (the lake into a store, abt data ingest --adapter norgate):"
  echo
  echo "| names | price rows | ingest s | ingest peak RSS |"
  echo "| ---: | ---: | ---: | ---: |"
  awk -F'\t' '{printf "| %s | %s | %s | %s |\n", $1,$2,$3,$4}' "$OUT/ingest.tsv"
fi
if [ -s "$ROWS" ]; then
  echo
  echo "| names | bars | strategy | data | load s | run s | bars/s | peak RSS MB | status |"
  echo "| ---: | ---: | --- | --- | ---: | ---: | ---: | ---: | --- |"
  awk -F'\t' '{printf "| %s | %s | %s | %s | %s | %s | %s | %s | %s |\n", $1,$2,$3,$4,$5,$6,$7,$8,$9}' "$ROWS"
fi
echo
"$PY" - "$ROWS" "$BOOKS" "$BUDGET_LOG" <<'EOF'
import math, re, sys
from collections import defaultdict
rows_path, books_path, budget_path = sys.argv[1:4]
pts = defaultdict(list)
for line in open(rows_path):
    names, bars, s, data, load, run, bps, rss, status = line.rstrip("\n").split("\t")
    if status == "ok" and float(run) > 0:
        pts[(s, data)].append((math.log(int(names)), math.log(float(run)), math.log(float(rss))))
for (s, data), p in sorted(pts.items()):
    if len(p) < 2:
        continue
    def slope(i):
        mx = sum(x[0] for x in p) / len(p); my = sum(x[i] for x in p) / len(p)
        return sum((x[0] - mx) * (x[i] - my) for x in p) / sum((x[0] - mx) ** 2 for x in p)
    print(f"log-log slope against names, {s} ({data}): run time {slope(1):.2f}, peak RSS {slope(2):.2f}")

# The targets of 7.3 against what was measured here.
budget = open(budget_path).read()
def grab(pattern, cast=float):
    m = re.search(pattern, budget)
    return cast(m.group(1)) if m else None
ingest = grab(r"ingest: \d+ rows in [\d.]+ s = (\d+) rows/s")
disk = grab(r"footprint on disk: \d+ bytes, ([\d.]+) B per value")
values = grab(r"footprint on disk: \d+ bytes, [\d.]+ B per value \((\d+) values\)", int)
resident_bytes = grab(r"resident: \d+ row groups, (\d+) bytes", int)
section = re.search(r"section of (\d+) entities resident: median (\d+) us", budget)
point = grab(r"point resident: median (\d+) us")
books = {}
for line in open(books_path):
    name, run, rss, bars = line.rstrip("\n").split("\t")
    books[name] = (float(run), float(rss), int(bars) if bars.isdigit() else None)

def verdict(ok):
    return "" if ok is None else ("within" if ok else "over")
table = []
table.append(("Sustained ingest", "200,000 rows/s",
              f"{ingest:,.0f} rows/s" if ingest else "not run", verdict(ingest and ingest >= 200_000)))
if section:
    n, us = int(section.group(1)), int(section.group(2))
    table.append(("Cross-section of one bar", "< 1 ms", f"{us} µs at {n:,} names, resident", verdict(us < 1000)))
else:
    table.append(("Cross-section of one bar", "< 1 ms", "not run", ""))
table.append(("Point lookup", "O(log n)", f"{point:.0f} µs, resident" if point else "not run", ""))
res = resident_bytes / values if resident_bytes and values else None
table.append(("Storage per stored value", "≤ 16 B resident, ≤ 4 B on disk",
              f"{res:.1f} B resident, {disk:.2f} B on disk" if res and disk else "not run",
              verdict(res <= 16 and disk <= 4) if res and disk else ""))
for name, label, (t_max, rss_max) in [("mw14", "MW14 full replay", (60, 1536)), ("r8l", "R8L full replay", (3, 614))]:
    if name in books:
        run, rss, bars = books[name]
        table.append((label, f"≤ {t_max} s, ≤ {rss_max / 1024:.1f} GB", f"{run:.1f} s, {rss / 1024:.2f} GB", verdict(run <= t_max and rss <= rss_max)))
        if bars:
            table.append((f"Barrier to decision, {name}", "< 50 ms live", f"{1000 * run / bars:.1f} ms a bar in replay (mean)", ""))
    else:
        table.append((label, f"≤ {t_max} s, ≤ {rss_max / 1024:.1f} GB", "not run (no store given)", ""))
for label, value in [("Instruments live", "10,000 at @1m"), ("History resident", "30 years @1d; 5 years @1m"),
                     ("Full-minute futures bundle (47M bars)", "resident")]:
    table.append((label, value, "not measured by this script", ""))
print()
print("Targets (docs/spec/00-architecture.md 7.3):")
print()
print("| Target | Value | Measured | |")
print("| --- | --- | --- | --- |")
for row in table:
    print("| " + " | ".join(row) + " |")
EOF
