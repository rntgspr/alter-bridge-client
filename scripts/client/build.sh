#!/usr/bin/env bash
# Compiles both macOS client architectures without packaging or installing them.
set -euo pipefail

if [[ $# -ne 0 ]]; then
  printf 'usage: scripts/client/build.sh\n' >&2
  exit 2
fi

cd "$(dirname "$0")/../.."
for target in aarch64-apple-darwin x86_64-apple-darwin; do
  cargo build --release --locked --package alter-bridge-cli --target "$target"
done
