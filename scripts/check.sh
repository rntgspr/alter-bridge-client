#!/bin/sh
# Checks client source compilation, script syntax and the native asset manifest without starting services.
set -eu

cd "$(dirname "$0")/.."
cargo fmt --all -- --check
cargo build --workspace --locked
sh -n scripts/check.sh scripts/client/install.sh scripts/client/manifest.sh
bash -n scripts/client/build.sh scripts/client/package.sh scripts/client/dev-install.sh scripts/client/dev-cleanup.sh
generated=$(mktemp)
trap 'rm -f "$generated"' EXIT HUP INT TERM
sh scripts/client/manifest.sh > "$generated"
cmp -s native-assets/manifest.json "$generated" || { printf 'native asset manifest differs from source tree; regenerate it with sh scripts/client/manifest.sh\n' >&2; exit 1; }
