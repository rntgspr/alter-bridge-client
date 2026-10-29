#!/usr/bin/env bash
# Builds the checked standalone macOS client package; publication is separate.
set -euo pipefail

# Stops packaging when the source manifest does not describe the exact release.
fail() {
  printf 'release: %s\n' "$1" >&2
  exit 1
}

[[ $# -eq 1 ]] || fail 'usage: scripts/client/package.sh v<workspace-version>'
cd "$(dirname "$0")/../.."
version=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\([^"]*\)"/\1/p' Cargo.toml)
tag="v$version"
[[ "$1" == "$tag" ]] || fail "tag must match workspace version: $tag"
cmp -s native-assets/manifest.json <(sh scripts/client/manifest.sh) || fail 'native asset manifest differs from source tree; regenerate it with sh scripts/client/manifest.sh'

bash scripts/client/build.sh

target_root="${CARGO_TARGET_DIR:-target}"
mkdir -p dist
stage=$(mktemp -d "$PWD/dist/.alter-bridge.XXXXXX")
trap 'rm -rf "$stage"' EXIT
package="$stage/alter-bridge-$tag"
mkdir -p "$package/bin" "$package/scripts/client" "$package/native-assets"
lipo -create -output "$package/bin/alter-bridge" \
  "$target_root/aarch64-apple-darwin/release/alter-bridge" \
  "$target_root/x86_64-apple-darwin/release/alter-bridge"
lipo "$package/bin/alter-bridge" -verify_arch arm64 x86_64 || fail 'universal executable lacks a required architecture'
identity="${CODESIGN_IDENTITY:--}"
if [[ "$identity" == '-' ]]; then
  codesign --force --sign - --timestamp=none "$package/bin/alter-bridge"
else
  codesign --force --sign "$identity" "$package/bin/alter-bridge"
fi
codesign --verify --strict "$package/bin/alter-bridge"

cp INSTALL-OFFLINE.md "$package/"
cp scripts/client/install.sh "$package/scripts/client/"
cp scripts/client/install-daemon.sh scripts/client/ensure-daemon.sh scripts/client/uninstall-daemon.sh "$package/scripts/client/"
cp native-assets/manifest.json "$package/native-assets/"
while IFS= read -r path; do
  mkdir -p "$package/$(dirname "$path")"
  cp "$path" "$package/$path"
done < <(jq -r '.files | keys[]' native-assets/manifest.json)
printf '%s\n' "$version" > "$package/VERSION"
(
  cd "$package"
  find . -type f ! -path ./SHA256SUMS -print | sed 's#^./##' | LC_ALL=C sort | while IFS= read -r path; do
    shasum -a 256 "$path"
  done > SHA256SUMS
)
(cd "$stage" && zip -q -r "alter-bridge-$tag.zip" "alter-bridge-$tag")
mv -f "$stage/alter-bridge-$tag.zip" "dist/alter-bridge-$tag.zip"
(cd dist && shasum -a 256 "alter-bridge-$tag.zip" > "alter-bridge-$tag.zip.sha256")
printf 'Package: dist/alter-bridge-%s.zip\n' "$tag"
