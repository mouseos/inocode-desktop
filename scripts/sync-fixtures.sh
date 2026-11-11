#!/bin/sh
# ref/contract-fixtures (ino-code monorepo, canonical) -> tests/fixtures/contract (this repo's copy).
# Run from the monorepo checkout after fixtures change; commit the result.
set -e
here="$(cd "$(dirname "$0")/.." && pwd)"
src="$here/../../ref/contract-fixtures"
[ -d "$src" ] || { echo "not found: $src" >&2; exit 1; }
mkdir -p "$here/tests/fixtures/contract"
cp "$src"/*.json "$here/tests/fixtures/contract/"
echo "synced $(ls "$here/tests/fixtures/contract" | wc -l) files"
