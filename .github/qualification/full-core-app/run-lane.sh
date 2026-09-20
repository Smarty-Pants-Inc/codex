#!/usr/bin/env bash
# PREPARATION ONLY: reviewed a9a7 source, full Core/AppServer packages.
# CI must select isolated finite resources/timeouts; no dispatch or publication grant.
set -euo pipefail
: "${B_SOURCE_HEAD:?exact reviewed generated-clean source HEAD required}"
: "${B_SOURCE_TREE:?exact reviewed generated-clean source tree required}"
: "${B_EVIDENCE_DIR:?new isolated evidence directory required}"
: "${B_LANE:?core or app-server required}"
: "${B_EVIDENCE_PRODUCER:?exact reviewed producer script path required}"
: "${B_EVIDENCE_PRODUCER_SHA256:?exact reviewed producer SHA256 required}"
test "$(sha256sum "$B_EVIDENCE_PRODUCER" | cut -d ' ' -f 1)" = "$B_EVIDENCE_PRODUCER_SHA256"
case "$B_LANE" in core|app-server) ;; *) exit 2 ;; esac
# The reviewed product subject is fixed, not an arbitrary caller-supplied source.
test "$B_SOURCE_HEAD" = a9a7b2ffb4cd4f210586e786fffaaedd27a5b0e8
test "$B_SOURCE_TREE" = 0ec09cfc62bc848c986770b2294b839492b10198
root=$(git rev-parse --show-toplevel)
cd "$root"
test "$(git rev-parse HEAD)" = "$B_SOURCE_HEAD"
test "$(git rev-parse HEAD^{tree})" = "$B_SOURCE_TREE"
git diff --exit-code
git diff --cached --exit-code
test -z "$(git ls-files --others --exclude-standard)"
case "$B_EVIDENCE_DIR" in /*) ;; *) exit 2 ;; esac
B_EVIDENCE_DIR=$(python3 - "$root" "$B_EVIDENCE_DIR" <<'PY'
from pathlib import Path
import sys
if not __debug__:
    raise RuntimeError('optimized Python is not permitted for evidence validation')
root = Path(sys.argv[1]).resolve(strict=True)
requested = Path(sys.argv[2])
assert requested.name not in ('', '.', '..')
assert not requested.exists() and not requested.is_symlink()
parent = requested.parent.resolve(strict=True)
assert parent.is_dir()
selected = parent / requested.name
assert selected != root and root not in selected.parents
print(selected)
PY
)
test "$(uname -s)" = Linux
test "$(uname -m)" = x86_64
test ! -e "$B_EVIDENCE_DIR"
mkdir -m 700 "$B_EVIDENCE_DIR"
evidence=$(cd "$B_EVIDENCE_DIR" && pwd)
git show -s --format='%H %T %P' HEAD > "$evidence/source-before.txt"
: "${B_BUILD_JOBS:?CI-selected positive Cargo build concurrency required}"
: "${B_TEST_THREADS:?CI-selected positive test concurrency required}"
[[ "$B_BUILD_JOBS" =~ ^[1-9][0-9]*$ ]] || exit 2
[[ "$B_TEST_THREADS" =~ ^[1-9][0-9]*$ ]] || exit 2
export CARGO_BUILD_JOBS="$B_BUILD_JOBS" CARGO_INCREMENTAL=0 CARGO_NET_RETRY=0
export NEXTEST_PROFILE=local RUST_MIN_STACK=8388608
record_final() {
  status=$?
  trap - EXIT
  cd "$root"
  printf '%s\n' "$status" > "$evidence/lane-exit.txt"
  git show -s --format='%H %T %P' HEAD > "$evidence/source-after.txt"
  git diff --binary > "$evidence/source.patch"
  git diff --cached --binary > "$evidence/index.patch"
  git status --porcelain=v1 --untracked-files=all > "$evidence/status-after.txt"
  df -Pk . > "$evidence/disk-after.txt"
  if ! python3 "$B_EVIDENCE_PRODUCER" "$root" "$evidence" "$B_LANE"; then
    printf 'evidence-production-failed\n' > "$evidence/producer-failed.txt"
    exit 1
  fi
  exit "$status"
}
trap record_final EXIT
{ uname -a; df -Pk .; free -m; } > "$evidence/resources-before.txt"
sha256sum MODULE.bazel MODULE.bazel.lock codex-rs/Cargo.lock codex-rs/Cargo.toml > "$evidence/dependencies.sha256"
{ rustc --version --verbose; cargo --version; cargo nextest --version; just --version; python3 --version; } > "$evidence/tools.log"
cp MODULE.bazel.lock "$evidence/MODULE.bazel.lock"
printf '%s\n' "$B_LANE" > "$evidence/lane.txt"
printf 'NEXTEST_PROFILE=%s\nRUST_MIN_STACK=%s\nCARGO_BUILD_JOBS=%s\nTEST_THREADS=%s\n' "$NEXTEST_PROFILE" "$RUST_MIN_STACK" "$CARGO_BUILD_JOBS" "$B_TEST_THREADS" > "$evidence/test-environment.txt"
run_phase() {
  phase=$1
  shift
  printf '%q ' "$@" > "$evidence/$phase.command.txt"
  printf '\n' >> "$evidence/$phase.command.txt"
  "$@" 2>&1 | tee "$evidence/$phase.log"
}
run_inventory() {
  phase=$1
  shift
  printf '%q ' "$@" > "$evidence/$phase.command.txt"
  printf '\n' >> "$evidence/$phase.command.txt"
  "$@" > "$evidence/$phase.json" 2> "$evidence/$phase.stderr"
}
cd codex-rs
case "$B_LANE" in
  core)
    run_inventory inventory-core cargo nextest list -p codex-core --locked --message-format json
    run_phase core just test -p codex-core --locked --retries 0 --test-threads "$B_TEST_THREADS" --status-level all --final-status-level all
    run_phase fix just fix -p codex-core --locked
    ;;
  app-server)
    run_inventory inventory-app-server cargo nextest list -p codex-app-server --locked --message-format json
    run_phase app-server just test -p codex-app-server --locked --retries 0 --test-threads "$B_TEST_THREADS" --status-level all --final-status-level all
    run_phase fix just fix -p codex-app-server --locked
    ;;
esac
run_phase fmt just fmt
# No automatic tests after fix/fmt. Any delta is returned for source review.
# Distinguish this failure from an earlier test/fix/fmt phase failure.
printf 'passed\n' > "$evidence/test-fix-fmt-phases.txt"
cd "$root"
if ! git diff --quiet || ! git diff --cached --quiet || test -n "$(git ls-files --others --exclude-standard)"; then
  printf 'dirty-final-source\n' > "$evidence/failure-classification.txt"
  exit 1
fi
printf 'clean\n' > "$evidence/final-clean.txt"
