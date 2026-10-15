#!/usr/bin/env bash
# Translation validation across revisions (docs/spec/05-migration.md, M-1 on):
# build REV in a temporary git worktree, write the golden renderings of every
# corpus-v2 strategy and company book there, and diff them against this
# tree's tests/golden/ (or the path the tree keeps them at). An empty diff
# means the two revisions decided, filled and held the same on the seeded
# synthetic markets.
#
#   scripts/tv.sh REV            # compare REV's renderings with this tree's goldens
#
# The tree's own check is `cargo test --test tv_golden`; this script is for
# comparing with an arbitrary earlier revision.
set -euo pipefail

rev="${1:?usage: scripts/tv.sh REV}"
root="$(git rev-parse --show-toplevel)"
work="$(mktemp -d)"
trap 'git -C "$root" worktree remove --force "$work/tree" >/dev/null 2>&1 || true; rm -rf "$work"' EXIT

git -C "$root" worktree add --detach "$work/tree" "$rev" >/dev/null
golden_of() {
  # The golden directory of a tree: the workspace layout keeps it under the
  # test crate, the single-crate layout under tests/.
  for d in crates/abt-tests/tests/golden tests/golden; do
    if [ -d "$1/$d" ]; then echo "$1/$d"; return; fi
  done
  echo "$1/tests/golden"
}
theirs="$(golden_of "$work/tree")"
mkdir -p "$theirs"
(
  cd "$work/tree"
  ABT_BLESS=1 CARGO_TARGET_DIR="$root/target/tv-$rev" cargo test --release --test tv_golden >/dev/null 2>&1 || {
    echo "tv.sh: the golden run failed at $rev" >&2
    exit 2
  }
)
ours="$(golden_of "$root")"
if diff -ru "$theirs" "$ours"; then
  echo "tv.sh: $rev and the working tree agree on every golden run"
fi
