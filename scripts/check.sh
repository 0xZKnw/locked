#!/usr/bin/env bash
# Everything that has to hold before this is worth showing anyone.
#
# The order is deliberate: the invariant first, because a suite that passes on a
# build where the loop can open a socket is testing the wrong program.
set -uo pipefail
cd "$(dirname "$0")/.."

fail=0
step() {
  printf '\n\033[1m── %s\033[0m\n' "$1"
  shift
  "$@" || { fail=1; printf '\033[31mfailed: %s\033[0m\n' "$1"; }
}

step "egress invariant" bash scripts/check-egress-isolation.sh
step "rust — unit and end to end" cargo test --workspace
step "rust — lints" cargo clippy --workspace --all-targets -- -D warnings
step "window — store behaviour" bash -c 'cd gui && npx vitest run'
step "window — build" bash -c 'cd gui && npx vite build'

if [ "$fail" -eq 0 ]; then
  printf '\n\033[32mall green\033[0m\n'
else
  printf '\n\033[31msomething is red — see above\033[0m\n'
fi
exit "$fail"
