#!/bin/bash
# Build the twelve agent sandboxes of the LLM usability study under $STUDY.
# Each holds the language description (README excerpt and semantic-model.md),
# corpus/env and corpus/lib, a release abt, the task, and (the `ex` condition)
# four example strategies that solve other problems. Nothing else: no
# corpus/company, no parity docs, no reference results.
#   STUDY=/tmp/lang-study experiments/llm-usability/make_sandboxes.sh
set -euo pipefail
REPO=$(cd "$(dirname "$0")/../.." && pwd)
STUDY=${STUDY:?set STUDY to an empty directory}
X=$REPO/experiments/llm-usability
mkdir -p "$STUDY/common/docs" "$STUDY/common/examples"
{
  echo "# abt: the strategy language (excerpt of the project README)"; echo
  sed -n '/^## The surface syntax in one page/,/^## Reading the kernel/p' "$REPO/README.md" | sed '$d'; echo
  echo "## Executor features (orders, rows windows, ranks, metrics)"; echo; echo
  sed -n '/^They use what the executor/,/^`--periods-per-year`, `--returns FILE`)./p' "$REPO/README.md" |
    sed 's/^They use what the executor and the DSL gained for them: fixed-base/The language and executor also provide: fixed-base/'
  echo; echo; echo 'Run `bin/abt` with no arguments for the full command-line usage.'
} > "$STUDY/common/docs/README.md"
cp "$REPO/docs/semantic-model.md" "$STUDY/common/docs/"
for f in momentum_12_1 pairs_trading opening_gap volatility_targeting; do
  cp "$REPO/corpus/strategies/$f.dsl" "$STUDY/common/examples/"
done
mk() { # name task condition
  local d=$STUDY/$1
  mkdir -p "$d/bin" "$d/attempts"
  cp "$REPO/target/release/abt" "$d/bin/"
  cp -R "$STUDY/common/docs" "$d/"
  cp -R "$REPO/corpus/env" "$d/env"; cp -R "$REPO/corpus/lib" "$d/lib"
  [ "$3" = ex ] && cp -R "$STUDY/common/examples" "$d/examples"
  cp "$X/tasks/$2.md" "$d/TASK.md"
}
for m in haiku sonnet; do
  for c in docs ex; do mk mw14-$m-$c mw14 $c; mk r8l-$m-$c r8l $c; done
  for b in momentum_liquid_monthly intraday_open_gap; do mk $b-$m-docs $b docs; done
done
echo "sandboxes in $STUDY; give each agent prompt.md with SANDBOX replaced by its directory"
