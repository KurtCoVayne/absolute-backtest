#!/bin/bash
# Build the twelve agent sandboxes of the observability study under $STUDY:
# six tasks x two conditions. Each holds the language description (README
# excerpt and semantic-model.md), corpus/env and corpus/lib, a release abt
# behind a wrapper that logs every invocation to commands.log, the task, and
# the synthetic data the task names. The `obs` condition gets the abt of this
# tree and docs/observability.md; the `base` condition gets the abt built from
# commit 0d9bc2b (OLD_ABT) and no observability docs. Nothing else: no
# corpus/company, no corpus/strategies, no parity docs.
#   STUDY=/tmp/obs-study OLD_ABT=/path/to/old/abt experiments/llm-observability/make_sandboxes.sh
set -euo pipefail
REPO=$(cd "$(dirname "$0")/../.." && pwd)
STUDY=${STUDY:?set STUDY to an empty directory}
OLD_ABT=${OLD_ABT:?set OLD_ABT to the abt binary built from 0d9bc2b}
NEW_ABT=${NEW_ABT:-$REPO/target/release/abt}
X=$REPO/experiments/llm-observability
mkdir -p "$STUDY/common/docs" "$STUDY/common/data"
{
  echo "# abt: the strategy language (excerpt of the project README)"; echo
  sed -n '/^## The surface syntax in one page/,/^## Reading the kernel/p' "$REPO/README.md" | sed '$d'; echo
  echo "## Executor features (orders, rows windows, ranks, metrics)"; echo; echo
  sed -n '/^They use what the executor/,/^`--periods-per-year`, `--returns FILE`)./p' "$REPO/README.md" |
    sed 's/^They use what the executor and the DSL gained for them: fixed-base/The language and executor also provide: fixed-base/'
  echo; echo; echo 'Run `bin/abt` with no arguments for the full command-line usage.'
} > "$STUDY/common/docs/README.md"
cp "$REPO/docs/semantic-model.md" "$STUDY/common/docs/"
python3 "$REPO/scripts/synth/weekly_synth.py" --out "$STUDY/common/data/weekly" > /dev/null
python3 "$REPO/scripts/synth/sessions_synth.py" --out "$STUDY/common/data/sessions" > /dev/null
mk() { # task condition
  local d=$STUDY/$1-haiku-$2
  rm -rf "$d"; mkdir -p "$d/bin" "$d/attempts"
  if [ "$2" = obs ]; then cp "$NEW_ABT" "$d/bin/abt.real"; else cp "$OLD_ABT" "$d/bin/abt.real"; fi
  cat > "$d/bin/abt" <<WRAP
#!/bin/bash
# Logs every invocation (the study counts them), then runs abt.
echo "\$(date +%H:%M:%S) \$*" >> "$d/commands.log"
exec "$d/bin/abt.real" "\$@"
WRAP
  chmod +x "$d/bin/abt"
  cp -R "$STUDY/common/docs" "$d/"
  [ "$2" = obs ] && cp "$REPO/docs/observability.md" "$d/docs/"
  cp -R "$REPO/corpus/env" "$d/env"; cp -R "$REPO/corpus/lib" "$d/lib"
  case $1 in
    mw14) mkdir -p "$d/data"; cp -R "$STUDY/common/data/weekly" "$d/data/" ;;
    r8l) mkdir -p "$d/data"; cp -R "$STUDY/common/data/sessions" "$d/data/" ;;
  esac
  cp "$X/tasks/$1.md" "$d/TASK.md"
  sed "s#SANDBOX#$d#g" "$X/prompt-$2.md" > "$d/PROMPT.md"
}
for t in mw14 r8l momentum_liquid_monthly intraday_open_gap breakout_with_stop vol_targeted_trend; do
  for c in base obs; do mk $t $c; done
done
echo "sandboxes in $STUDY; each holds PROMPT.md, the prompt with its path filled in"
