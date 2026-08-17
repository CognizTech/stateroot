#!/usr/bin/env bash
# Thin wrapper for `stateroot search` — hybrid search over project state and memory.
set -euo pipefail

if ! command -v stateroot >/dev/null 2>&1; then
  echo "stateroot skill: the 'stateroot' CLI was not found on PATH." >&2
  echo "Install the stateroot CLI, then re-run this command." >&2
  exit 127
fi

exec stateroot search "$@"
