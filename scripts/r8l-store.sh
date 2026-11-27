#!/bin/bash
# scripts/r8l-store.sh: R8L's 26 roots staged from Databento's minute bars into a store of their own.
#
# The script that staged the store of Nov 26, 2026 on the pipeline machine, where the vendor's files are
# (docs/handoff/benchmark.md, "R8L's store"), with its places made variables; where that run removed the
# slot reserve's file when a step ended, this one writes 0 to it. For each root: one view of every pull of it
# (scripts/r8l-views.py), one ingest with the options of absolute-pipeline's scripts/store/point-values.sh
# (sessions broken at 17:45 New York with a 20-minute gap, undefined instruments dropped, every root's
# point value stated, reviewed=complete), each root's commission per contract per side, and the five bar
# relations consolidated with their tails (ERRATA E-173): every row is then in the base segments, and a
# service or a query opens the store without replaying its log. A root cannot be ingested twice (its
# back-adjustment restates facts), so newer bars are a new store. A root done is skipped: run it again as
# it is after a stop.
#
# One heavy step at a time under the machine's ingest lock, in a memory scope (systemd-run), with two of
# abt-slot's four slots withheld while a step runs. A root is staged once its newest pull is whole
# (its last session's file on disk, nothing written for two minutes), in whatever order the pulls end.
#
# Env: M (the work directory: $M/target/release/abt-data, $M/logs), STORE, VIEWS, LOGS, REFRESH (the newest
# pull, under VENDOR), LAST (its last whole session), ROOTS, CAP and GATE (GB: the scope and the memory a
# step waits for), MIN_DISK_GB, PY (a Python with pyarrow), LOCK, RESERVE.
set -uo pipefail
M=${M:-/srv/absolute/m8}; B=${B:-$M/target/release/abt-data}
STORE=${STORE:-$M/store-futures}; VIEWS=${VIEWS:-$M/views}; LOGS=${LOGS:-$M/logs/stage}; mkdir -p "$LOGS" "$VIEWS"
SUM=$LOGS/summary.log; touch "$SUM"
export VENDOR=${VENDOR:-/srv/absolute/data/vendor} REFRESH=${REFRESH:-dbn-refresh-20261126} LAST=${LAST:-2026-11-25}
C=$VENDOR/$REFRESH
PY=${PY:-/srv/absolute/dbn-pull/venv/bin/python}
LOCK=${LOCK:-/srv/absolute/full/ingest.lock}; RESERVE=${RESERVE:-/srv/absolute/locks/reserve}
CAP=${CAP:-28} GATE=${GATE:-30} MIN_DISK_GB=${MIN_DISK_GB:-8}
HERE=$(cd "$(dirname "$0")" && pwd)
# Dollars per quoted point, the book's own (d20-research config/contract_specs.yaml, bpv). KC and SB are
# quoted in cents a pound on 37,500 and 112,000 lb: their definitions' unit alone would say 100 times as much.
PV='ES=50;NQ=20;RTY=50;6J=12500000;ZT=2000;ZF=1000;ZN=1000;ZB=1000;CL=1000;NG=10000;RB=42000;HO=42000;ZC=50;ZS=50;ZW=50;ZL=600;ZM=100;LE=400;HE=400;GF=500;GC=100;SI=5000;PL=50;ETH=50;SB=1120;KC=375'
# Dollars per contract per side (d20-research config/costs.yaml, retrieved 2026-08-20): the broker's 0.85,
# the exchange's fee and the venue's recovery and regulatory fees.
declare -A COMM=( [ES]=2.297 [NQ]=2.297 [RTY]=2.297 [6J]=2.511 [LE]=3.011 [HE]=3.011 [GF]=3.011 [ETH]=6.411 [ZT]=1.57 [ZF]=1.57 [ZN]=1.72 [ZB]=1.79 [ZC]=3.07 [ZS]=3.07 [ZL]=3.07 [ZW]=3.07 [ZM]=3.07 [CL]=2.41 [HO]=2.41 [RB]=2.41 [NG]=2.51 [PL]=2.56 [GC]=2.56 [SI]=2.56 [SB]=2.96 [KC]=2.96 )
BARS="future.open future.high future.low future.close future.volume"
ROOTS=${ROOTS:-"6J ETH CL ES GC GF HE HO LE NG NQ PL RB RTY SI ZB ZC ZF ZL ZM ZN ZS ZT ZW KC SB"}
say() { echo "$* $(date -u +%FT%TZ)" | tee -a "$SUM"; }
free_slots() { echo 0 > "$RESERVE"; }
stop() { say "== STOPPED $*"; free_slots; exit 1; }
avail() { df -BG --output=avail "$M" | tail -1 | tr -dc 0-9; }
memwait() { while [ "$(free -g | awk '/Mem/{print $7}')" -lt "$1" ]; do sleep 20; done; }
refreshed() { # a root's newest pull is whole: its last session and its definitions on disk, nothing written for two minutes
  local r=$1 d
  for d in GLBX.MDP3 IFUS.IMPACT; do
    [ -f "$C/$d/ohlcv-1m/$r.FUT/$LAST.parquet" ] && [ -f "$C/$d/definition/$r.parquet" ] || continue
    [ -z "$(find "$C/$d/ohlcv-1m/$r.FUT" "$C/$d/definition/$r.parquet" -mmin -2 | head -1)" ] && return 0
  done
  return 1
}
run() { # name, command...
  local name=$1 code t0 rss; shift
  exec 9>>"$LOCK"; flock 9
  echo 2 > "$RESERVE"
  memwait "$GATE"
  t0=$(date +%s)
  sudo systemd-run --scope -p MemoryMax=${CAP}G -p MemorySwapMax=0 --uid="$(id -u)" --gid="$(id -g)" --quiet \
    /usr/bin/time -v eatmydata "$@" > "$LOGS/$name.log" 2> "$LOGS/$name.time"
  code=$?
  free_slots; flock -u 9
  rss=$(grep 'Maximum resident' "$LOGS/$name.time" | awk '{print $NF}')
  echo "$name exit $code wall $(( $(date +%s) - t0 ))s rss_kb $rss cap ${CAP}G $(date -u +%FT%TZ)" | tee -a "$SUM"
  [ "$code" = 0 ] || grep -av '^\s' "$LOGS/$name.time" | grep -aiE 'error|refus|differ|killed' | head -3 | cut -c1-400 | tee -a "$SUM"
  return "$code"
}
trap free_slots EXIT
say "== stage on $(sha256sum "$B" | cut -c1-12): $STORE, roots $ROOTS, disk $(avail)G"
while :; do
  r=; left=0
  for x in $ROOTS; do
    grep -q "^root $x ok" "$SUM" && continue
    left=$((left + 1))
    if [ -z "$r" ] && refreshed "$x"; then r=$x; fi
  done
  [ "$left" -gt 0 ] || break
  # No root's pull is whole yet: wait for the pulls, which must be running.
  if [ -z "$r" ]; then pgrep -f dbn_pull_extra.py >/dev/null || stop "$left roots have no whole pull and none is running"; sleep 30; continue; fi
  [ "$(avail)" -ge "$MIN_DISK_GB" ] || stop "under $MIN_DISK_GB GB of disk before $r"
  $PY "$HERE/r8l-views.py" "$VIEWS" "$r" >> "$SUM" 2>&1 || stop "view of $r"
  run "dbn-$r" "$B" ingest --store "$STORE" --adapter databento --from "$VIEWS/$r" --class future --undefined drop \
    --session-break 17:45 --session-gap 20 --point-values "$PV" --commission "${COMM[$r]}" --attributes reviewed=complete \
    --report "$LOGS/dbn-$r-report.json" || stop "ingest $r"
  for rel in $BARS; do run "cons-$r-$rel" "$B" consolidate --store "$STORE" "$rel" --tails || stop "consolidate $rel after $r"; done
  say "root $r ok: $(grep -aoE '[0-9]+ bars kept' "$LOGS/dbn-$r.time" "$LOGS/dbn-$r.log" 2>/dev/null | tail -1 | cut -d: -f2), store $(du -sBM "$STORE" | cut -f1) ($(du -sBM "$STORE"/log "$STORE"/part "$STORE"/hot 2>/dev/null | awk '{printf "%s %s ", $1, $2}' | sed "s#$STORE/##g")), disk $(avail)G"
done
say "== stage done: $(echo $ROOTS | wc -w) roots, $(du -sh "$STORE" | cut -f1)"
