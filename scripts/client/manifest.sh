#!/bin/sh
# Prints the exact native hook and skill inventory for this source version.
set -eu

cd "$(dirname "$0")/../.."
version=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\([^"]*\)"/\1/p' Cargo.toml)
[ -n "$version" ] || { printf 'workspace version is missing\n' >&2; exit 1; }
paths=$(mktemp)
trap 'rm -f "$paths"' EXIT HUP INT TERM

for path in native-assets/claude-hooks.json native-assets/codex-hooks.json; do
  [ -f "$path" ] && [ ! -L "$path" ] || { printf 'hook asset is missing or linked: %s\n' "$path" >&2; exit 1; }
  printf '%s\n' "$path" >> "$paths"
done

[ -d skills ] && [ ! -L skills ] || { printf 'skills root is missing or linked\n' >&2; exit 1; }
[ -z "$(find skills ! -type f ! -type d -print -quit)" ] || { printf 'skills contain a link or special file\n' >&2; exit 1; }
find skills -type f -print | LC_ALL=C sort >> "$paths"

for path in $(find skills -mindepth 1 -maxdepth 1 -type d -print | LC_ALL=C sort); do
  [ -f "$path/SKILL.md" ] || { printf 'skill entry is missing: %s\n' "$path" >&2; exit 1; }
done

[ "$(find skills -mindepth 1 -maxdepth 1 -type d | wc -l | tr -d ' ')" -gt 0 ] || { printf 'no skills found\n' >&2; exit 1; }

while IFS= read -r path; do
  case "$path" in
    skills/*/*|native-assets/claude-hooks.json|native-assets/codex-hooks.json) ;;
    *) printf 'invalid native asset path: %s\n' "$path" >&2; exit 1 ;;
  esac
  case "$path" in *[!A-Za-z0-9_./-]*|*/../*|*/./*|*//*|*/.) printf 'unsafe native asset path: %s\n' "$path" >&2; exit 1 ;; esac
  hash=$(shasum -a 256 "$path" | awk '{print $1}')
  printf '%s\t%s\n' "$path" "$hash"
done < "$paths" | jq -Rn --arg version "$version" '[inputs | split("\t") | {(.[0]): .[1]}] | add as $files | {version: $version, files: $files}'
