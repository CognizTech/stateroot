#!/usr/bin/env bash
# Thin wrapper for `stateroot resume` — session start in a stateroot project.
# Run this unpiped. Never `| head`, `| tail`, or any line limiter — the full
# digest is the state of record.
set -euo pipefail

if ! command -v stateroot >/dev/null 2>&1; then
  echo "stateroot skill: the 'stateroot' CLI was not found on PATH." >&2
  echo "Install the stateroot CLI, then re-run this command." >&2
  exit 127
fi

exec stateroot resume "$@"
