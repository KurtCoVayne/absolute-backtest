#!/usr/bin/env bash
# The falsification pass (docs/spec/TRACE.md, the falsification column):
# every patch under scripts/mutations/ breaks one proposition's
# implementation; the test its header names must then fail. Usage:
#
#   bash scripts/mutate.sh [ID...]
#
# A patch's header carries `# test: <command>` (run from the repository's
# root; a non-zero exit is the expected failure) and `# proposition: <id>`.
# Each patch is applied with `git apply`, the test run, the patch reverted
# (`git apply -R`), whatever the test did. The pass fails when a patch does
# not apply, when its test passes under the mutation (the test does not
# catch it), or when the tree is left changed. The working tree must be
# clean for the files a patch touches.
set -u
cd "$(dirname "$0")/.."
status=0
patches=()
if [ $# -gt 0 ]; then
  for id in "$@"; do patches+=("scripts/mutations/$id.patch"); done
else
  for p in scripts/mutations/*.patch; do patches+=("$p"); done
fi
for p in "${patches[@]}"; do
  id=$(basename "$p" .patch)
  test_cmd=$(sed -n 's/^# test: //p' "$p" | head -1)
  prop=$(sed -n 's/^# proposition: //p' "$p" | head -1)
  if [ -z "$test_cmd" ]; then
    echo "FAIL $id: no '# test:' line"; status=1; continue
  fi
  if ! git apply --check "$p" 2>/dev/null; then
    echo "FAIL $id ($prop): the patch does not apply to the tree"; status=1; continue
  fi
  git apply "$p"
  if sh -c "$test_cmd" >"scripts/mutations/$id.log" 2>&1; then
    echo "FAIL $id ($prop): the test passed under the mutation; see scripts/mutations/$id.log"; status=1
  else
    echo "ok   $id ($prop): the test fails under the mutation"
  fi
  if ! git apply -R "$p"; then
    echo "FAIL $id: the patch did not revert; the tree is changed"; status=1
  fi
  rm -f "scripts/mutations/$id.log"
done
exit $status
