#!/bin/sh
# Fail if a Linux ELF needs a newer glibc than the release floor.
# Usage: check-glibc.sh <binary> [max_version]
# Default max_version is 2.17 (manylinux2014 / Ubuntu 16.04 / RHEL 7).
set -eu

bin="${1:?usage: check-glibc.sh <binary> [max_version]}"
limit="${2:-2.17}"

if [ ! -f "$bin" ]; then
    echo "check-glibc: missing file: $bin" >&2
    exit 1
fi

highest="$(
    readelf -W --dyn-syms "$bin" 2>/dev/null \
        | grep -oE 'GLIBC_[0-9]+\.[0-9]+' \
        | sed 's/^GLIBC_//' \
        | sort -t. -k1,1n -k2,2n \
        | tail -n 1 || true
)"

if [ -z "$highest" ]; then
    echo "check-glibc: $bin has no GLIBC symbols (static or non-glibc) — ok"
    exit 0
fi

echo "check-glibc: $bin highest GLIBC symbol is $highest (limit $limit)"

newest="$(printf '%s\n' "$highest" "$limit" | sort -t. -k1,1n -k2,2n | tail -n 1)"
if [ "$newest" != "$limit" ]; then
    echo "check-glibc: ERROR $bin requires GLIBC $highest; release floor is $limit" >&2
    readelf -W --dyn-syms "$bin" | grep -E 'GLIBC_' | sort -u >&2 || true
    exit 1
fi
