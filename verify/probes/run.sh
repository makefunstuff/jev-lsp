#!/usr/bin/env bash
# Regenerates every number cited in docs/research/nvim-lsp-surface.md.
# Usage: verify/probes/run.sh
# Not using `set -e`: each probe runs to completion and reports its own status,
# so one failure does not hide the others. The exit code is the count of failures.

cd "$(dirname "$(realpath "$0")")" || exit 1
fail=0

for p in surface capabilities edit-version undo-granularity streaming trace language; do
  printf '\n===== %s.lua =====\n' "$p"
  if ! nvim --headless -u NONE -l "$p.lua"; then
    printf '!! %s.lua FAILED\n' "$p"
    fail=$((fail + 1))
  fi
done

printf '\n===== %d probe(s) failed =====\n' "$fail"
exit "$fail"
