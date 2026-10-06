#!/usr/bin/env bash
# Shared source identity helpers for release evidence scripts.
# Generated reports, bundles, and soak launcher state are outputs, not source.

evidence_source_commit() {
  local root="$1"
  git -C "$root" rev-parse --verify HEAD 2>/dev/null || printf 'unknown\n'
}

evidence_source_branch() {
  local root="$1"
  local branch
  branch="$(git -C "$root" branch --show-current 2>/dev/null || true)"
  printf '%s\n' "${branch:-detached}"
}

evidence_source_worktree_state() {
  local root="$1"
  local changes
  if ! changes="$(git -C "$root" status --porcelain --untracked-files=all -- . \
    ':(exclude)certification/reports/**' \
    ':(exclude)certification/bundles/**' \
    ':(exclude).run/**' 2>/dev/null)"; then
    printf 'unknown\n'
  elif [[ -n "$changes" ]]; then
    printf 'dirty\n'
  else
    printf 'clean\n'
  fi
}
