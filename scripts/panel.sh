#!/bin/bash
# scripts/panel.sh NAME TREE [EXTRA...]
#
# Every program but MW14, one cold run each, so that a change made for one book is measured on all of them
# (docs/handoff/benchmark.md, "The panel"). TREE is a directory holding a release `abt` and its `corpus-v2`
# (a bare name is $BENCH/trees/NAME). Real history only (the owner's rule of Nov 26, 2026): each run reads a
# real store through a data service of its own, over the store's whole history, and holds the machine lock
# for itself alone so other jobs go between:
#   fut  r8l                               on FUT (the book's 26 roots staged from Databento's minute bars)
#   eq   every daily strategy of the corpus on EQ  (the S&P 500 lake staged from 1990, every member)
# `opening_gap` and `resampled_momentum` read minute bars of equities and `pairs_trading` two tickers that no
# market lists: there is no real history for them, so they are not run.
# One JSON line a run in $BENCH/panel-NAME/cells.jsonl: the run's seconds, peak memory, bars, tuples, decisions,
# fills and the SHA-256 of its returns (`--returns`), or why it did not finish. EXTRA goes to `abt run`.
# Env: BENCH=/data/bench, LOCK=/data/m8/machine.lock, PY=python3.12, LIMIT=900 (seconds a run may take),
# ONLY="a b" (these programs alone), PGROUPS="fut eq", EQ, FUT (the stores), INPLACE=1 (serve the store where
# it is and not a copy of it: where the disk does not hold two, as R8L's store on the pipeline machine).
set -u
NAME=$1; TREE=$2; shift 2; EXTRA="$*"
BENCH=${BENCH:-/data/bench}; LOCK=${LOCK:-/data/m8/machine.lock}; PY=${PY:-python3.12}
case $TREE in */*) T=$TREE ;; *) T=$BENCH/trees/$TREE ;; esac
ABT=$T/abt; C=$T/corpus-v2
OUT=$BENCH/panel-$NAME; rm -rf $OUT; mkdir -p $OUT
LIMIT=${LIMIT:-900}; ONLY=${ONLY:-}
EQ=${EQ:-$BENCH/pstore-eq}; FUT=${FUT:-$BENCH/store-futures}
PGROUPS=${PGROUPS:-fut eq}; INPLACE=${INPLACE:-0}
exec 9>>$LOCK
say(){ echo "[$(date -u +%T)] $*" >> $OUT/job.log; }
SRV=
stop(){ if [ -n "$SRV" ]; then pkill -TERM -P $SRV 2>/dev/null || kill -TERM $SRV 2>/dev/null; wait $SRV 2>/dev/null; fi; SRV=; }
trap 'stop; rm -rf '$BENCH'/pwork-'$NAME'-*' EXIT
want(){ [ -z "$ONLY" ] || [[ " $ONLY " == *" $1 "* ]]; }
cell(){ # group program socket files...
  local g=$1 s=$2 sock=$3; shift 3
  want $s || return 0
  flock 9
  # R8L runs as the book is written. The lake's list strategies take the legs scripts/bench.sh gives them
  # (n names a leg; 20 long-only); the others run as written.
  local pargs=()
  if [ "$g" = eq ] && grep -q 'need *lists' $C/strategies/$s.dsl; then
    case $s in
      dividend_capture) ;;
      total_return_momentum) pargs=(--param n=20) ;;
      *) pargs=(--param n=50) ;;
    esac
  fi
  timeout $LIMIT /usr/bin/time -v $ABT run --strategy $s --data $sock --timing $EXTRA "${pargs[@]}" --returns $OUT/$s.returns "$@" > $OUT/$s.log 2>&1; local rc=$?
  flock -u 9
  $PY - "$g" "$s" "$rc" "$OUT/$s.log" "$OUT/$s.returns" "$LIMIT" >> $OUT/cells.jsonl <<'PY'
import hashlib, json, os, re, sys
g, s, rc, log, ret, limit = sys.argv[1:7]
text = open(log, errors='replace').read() if os.path.exists(log) else ''
def f(pat, cast=float):
    m = re.search(pat, text, re.M)
    return cast(m.group(1)) if m else None
out = {'group': g, 'program': s, 'rc': int(rc)}
t = re.search(r'^timing .*$', text, re.M)
if t:
    for k in ('run_s', 'bars', 'symbols', 'tuples', 'decisions', 'peak_rss_mb'):
        m = re.search(r'\b%s=([0-9.]+)' % k, t.group(0))
        out[k] = float(m.group(1)) if m else None
    ph = re.search(r'^phases (.*)$', text, re.M)
    if ph:
        out['phases'] = {k: float(v) for k, v in (x.split('=') for x in ph.group(1).split()) if float(v) >= 0.5}
out['fills'] = f(r'^decisions:\s*\d+\s+fills:\s*(\d+)', int)
out['wall_s'] = None
m = re.search(r'Elapsed \(wall clock\) time \(h:mm:ss or m:ss\): (?:(\d+):)?(\d+):([0-9.]+)', text)
if m:
    out['wall_s'] = round(int(m.group(1) or 0) * 3600 + int(m.group(2)) * 60 + float(m.group(3)), 2)
out['rss_gb'] = f(r'Maximum resident set size \(kbytes\): (\d+)', lambda x: round(int(x) / 1048576, 3))
out['cpu_s'] = round((f(r'User time \(seconds\): ([0-9.]+)') or 0) + (f(r'System time \(seconds\): ([0-9.]+)') or 0), 1)
if os.path.exists(ret) and os.path.getsize(ret) > 0:
    out['returns_sha'] = hashlib.sha256(open(ret, 'rb').read()).hexdigest()[:16]
    out['returns_rows'] = sum(1 for _ in open(ret, 'rb'))
if int(rc) != 0:
    out['why'] = ('timeout %ss' % limit) if int(rc) == 124 else next((l.strip()[:300] for l in text.splitlines() if l.strip() and not l.startswith('\t') and 'Command exited' not in l and not l.startswith('data ') and not l.startswith('derive ') and not l.startswith('plan ') and not l.startswith('rows ') and not l.startswith('warning ')), '')
print(json.dumps(out))
PY
  say "$s rc=$rc $(tail -1 $OUT/cells.jsonl | cut -c1-260)"
}
group(){ # group store
  local g=$1 base=$2
  ST=$BENCH/pwork-$NAME-$g; SOCK=$OUT/$g.sock
  [ -d "$base" ] || { say "$g: no store at $base"; return 1; }
  flock 9
  rm -f $SOCK
  if [ "$INPLACE" = 1 ]; then ST=$base; else rm -rf $ST; cp -a --reflink=auto $base $ST; fi
  /usr/bin/time -v $ABT data serve --store $ST --socket $SOCK > $OUT/$g.serve.log 2>&1 &
  SRV=$!
  local up=1
  for _ in $(seq 1 1200); do [ -S $SOCK ] && grep -q '^serving ' $OUT/$g.serve.log 2>/dev/null && { up=0; break; }; kill -0 $SRV 2>/dev/null || break; sleep 0.5; done
  flock -u 9
  [ $up = 0 ] || { say "$g: the data service did not start: $(tail -2 $OUT/$g.serve.log | tr '\n' ' ' | cut -c1-300)"; endgroup; return 1; }
  }
endgroup(){ stop; [ "$INPLACE" = 1 ] || rm -rf $ST; }
say "panel $NAME: tree $TREE ($(tail -2 $T/snap.log 2>/dev/null | head -1 | cut -c1-90)), extra '$EXTRA', limit ${LIMIT}s, groups $PGROUPS"
one(){ # group store program files...: a cold run, on a copy of the store and a service of its own
  local g=$1 base=$2 s=$3; shift 3
  want $s || return 0
  if group $g $base; then cell $g $s $SOCK "$@"; endgroup; fi
}
for g in $PGROUPS; do
  case $g in
  fut) one fut $FUT r8l $C/catalog $C/lib $C/company/r8l.dsl ;;
  eq)  for f in $C/strategies/*.dsl; do s=$(basename $f .dsl); grep -q 'bars @1m' $f && continue; [ $s = pairs_trading ] && continue; one eq $EQ $s $C/catalog $C/lib $C/strategies; done ;;
  esac
done
say PANEL-DONE
