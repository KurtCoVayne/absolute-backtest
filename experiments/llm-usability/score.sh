#!/bin/bash
# Score one sandbox of the LLM usability study into $STUDY/results/<name>/:
#   attempts.tsv     the checker's summary and diagnostic codes for every attempt
#   final_check.txt  the check of final.dsl
#   run.txt          final.dsl run with the reference flags (docs/parity-*.md)
#   compare.txt      the parity comparison against the book
#   STUDY=/tmp/lang-study experiments/llm-usability/score.sh $STUDY/r8l-sonnet-docs
# Needs the bundles and reference files of docs/parity-inputs.md, and a Python
# with pandas and pyarrow ($PY).
set -uo pipefail
REPO=$(cd "$(dirname "$0")/../.." && pwd)
STUDY=${STUDY:?set STUDY}
PY=${PY:-python3}
ABT=$REPO/target/release/abt
d=$1; name=$(basename "$d"); out=$STUDY/results/$name; mkdir -p "$out"
f=${2:-$d/final.dsl}
: > "$out/attempts.tsv"
for a in $(ls "$d"/attempts/*.dsl 2>/dev/null | sort -V); do
  o=$($ABT check "$d/env" "$d/lib" "$a" 2>&1)
  sum=$(echo "$o" | grep -E "checked:" | tail -1)
  [ -z "$sum" ] && sum="PARSE: $(echo "$o" | head -1 | sed 's#.*attempts/##')"
  codes=$(echo "$o" | grep -oE "^(error|warning) \[[A-Z0-9]+\]" | sort | uniq -c | tr -s ' ' | tr '\n' ';')
  printf '%s\t%s\t%s\n' "$(basename "$a")" "$sum" "$codes" >> "$out/attempts.tsv"
done
[ -f "$f" ] || { echo "$name: no final.dsl"; exit 0; }
$ABT check "$d/env" "$d/lib" "$f" > "$out/final_check.txt" 2>&1
strat=$(grep -oE '^strategy [A-Za-z0-9_]+' "$f" | head -1 | cut -d' ' -f2)
case $name in
  mw14*)
    $ABT run --strategy "$strat" --bundle ~/data/abt/b_mw14 --untested --price-relation trclose --actions in-prices \
      --compounding off --capital 1000000 --lot fractional --frictionless --commission-bps 10 --margin-rate 0 \
      --on-leverage allow --delist-proceeds last-price --start 1991-01-01 --end 2026-07-02 --periods-per-year 52 \
      --window-sums exact --returns "$out/R.parquet" "$d/env" "$d/lib" "$f" > "$out/run.txt" 2>&1
    $PY "$REPO/scripts/parity/mw14_compare.py" "$out/R.parquet" \
      ~/data/ref/mw14/artifacts/_canon_wkret_mw14_full.npy > "$out/compare.txt" 2>&1 ;;
  r8l*)
    rm -rf "$out/dump"
    $ABT run --strategy "$strat" --bundle ~/data/abt/b_r8l_fb --price-relation close_m --compounding off \
      --capital 1000000 --lot fractional --frictionless --margin-rate 0 --on-leverage allow --on-margin-call allow \
      --report-by day --report-calendar calendar --periods-per-year 252 --window-sums exact \
      --returns "$out/R.parquet" --dump "$out/dump" "$d/env" "$d/lib" "$f" > "$out/run.txt" 2>&1
    $PY "$REPO/scripts/parity/r8l_compare.py" "$out/dump" "$out/R.parquet" \
      ~/data/ref/r8l/results/leg_flags.parquet > "$out/compare.txt" 2>&1 ;;
esac
echo "$name scored"
