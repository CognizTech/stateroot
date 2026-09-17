#!/usr/bin/env bash
# Build one verified VSIX + stateroot-extension.json for GitHub releases.
# Usage: packaging/extension/package-release.sh [dist-dir]
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
ext="$root/editors/vscode"
dist="${1:-"$root/dist"}"
mkdir -p "$dist"

version="$(node -p "require('$ext/package.json').version")"
if [[ ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "package-release: invalid editors/vscode package version: $version" >&2
  exit 1
fi

cd "$ext"
if [[ "${SKIP_NPM_CI:-}" != "1" ]]; then
  npm ci
fi
npm test
out="$dist/stateroot-vscode-${version}.vsix"
npx --no-install vsce package --out "$out"
node "$root/packaging/extension/inspect-vsix.cjs" "$out"
echo "package-release: $out"
