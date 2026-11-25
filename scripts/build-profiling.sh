#!/usr/bin/env bash
# The profiling build: the release build with frame pointers and full debug
# info, in target/profiling (Cargo.toml, [profile.profiling]). Usage:
#
#   scripts/build-profiling.sh            # builds target/profiling/abt
#   perf record -e cpu-clock -F 299 -g --call-graph fp -o perf.data -- \
#     target/profiling/abt run --strategy mw14 --data S.sock --timing ...
#   perf report -i perf.data --stdio --no-children -g none --sort symbol --percent-limit 0.4
#
# The flag is here because Cargo has no stable profile key for frame
# pointers; the directory is the profile's own, so the release build that
# scripts/bench-mw14.sh times is left as it is.
set -euo pipefail
cd "$(dirname "$0")/.."
RUSTFLAGS="${RUSTFLAGS:-} -C force-frame-pointers=yes" cargo build --profile profiling --bin abt "$@"
echo "target/profiling/abt"
