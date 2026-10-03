#!/usr/bin/env bash

storage_matrix_release_status() {
  local report="$1"
  if grep -qE '^Overall status: FAIL$|^Result: FAIL$' "$report"; then
    printf 'FAIL'
  elif grep -q '^Overall status: PASS$' "$report"; then
    printf 'PASS'
  elif grep -q '^Overall status: PASS_WITH_WARNINGS$' "$report" \
    && grep -q '^| hardware qualification | PASS |' "$report"; then
    printf 'PASS'
  elif grep -q '^Overall status: PASS_WITH_SKIPS$' "$report"; then
    printf 'SKIP'
  else
    printf 'FAIL'
  fi
}
