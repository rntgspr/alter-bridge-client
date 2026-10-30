#!/bin/sh
# Installs one versioned standalone package after a complete local preview.
set -eu

# Reports a refusal without changing installed files.
fail() {
  printf 'alter-bridge install: %s\n' "$1" >&2
  exit 1
}

[ "$#" -eq 0 ] || fail 'usage: ./scripts/client/install.sh (from the extracted release folder)'
root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd -P)
[ -f "$root/VERSION" ] || fail 'VERSION is missing'
version=$(cat "$root/VERSION")
case "$version" in *[!0-9A-Za-z.-]*|'') fail 'invalid package version' ;; esac
[ "$(basename "$root")" = "alter-bridge-v$version" ] || fail 'package folder and VERSION do not match'
[ -f "$root/SHA256SUMS" ] || fail 'SHA256SUMS is missing'
[ -f "$root/bin/alter-bridge" ] && [ -x "$root/bin/alter-bridge" ] || fail 'universal executable is missing'
[ -f "$root/native-assets/manifest.json" ] || fail 'native assets are missing'
[ -z "$(find "$root" ! -type f ! -type d -print -quit)" ] || fail 'package contains a symlink or special file'
[ "$(plutil -extract version raw -o - "$root/native-assets/manifest.json" 2>/dev/null)" = "$version" ] || fail 'native asset manifest version differs from package'
[ "$(find "$root" -type f | wc -l | tr -d ' ')" = 15 ] || fail 'package file set is not the expected release'
[ "$(wc -l < "$root/SHA256SUMS" | tr -d ' ')" = 14 ] || fail 'checksum file set is incomplete'
for path in VERSION bin/alter-bridge scripts/client/install.sh INSTALL-OFFLINE.md \
  native-assets/manifest.json native-assets/claude-hooks.json native-assets/codex-hooks.json \
  skills/abg/SKILL.md skills/abg-who/SKILL.md skills/abg-doctor/SKILL.md skills/abg-connect-context/SKILL.md skills/abg-connect-device/SKILL.md skills/abg-disconnect-context/SKILL.md skills/abg-disconnect-device/SKILL.md; do
  [ -f "$root/$path" ] || fail "package asset is missing: $path"
  expected=$(awk -v path="$path" '$2 == path { print $1 }' "$root/SHA256SUMS")
  [ "${#expected}" -eq 64 ] || fail "checksum is missing or duplicated: $path"
  actual=$(shasum -a 256 "$root/$path" | awk '{print $1}')
  [ "$actual" = "$expected" ] || fail "checksum mismatch: $path"
done
codesign --verify --strict "$root/bin/alter-bridge" >/dev/null 2>&1 || fail 'executable signature verification failed'
arches=$(lipo -info "$root/bin/alter-bridge") || fail 'cannot inspect executable architectures'
case "$arches" in *aarch64*|*arm64*) ;; *) fail 'arm64 executable slice is missing' ;; esac
case "$arches" in *x86_64*) ;; *) fail 'x86_64 executable slice is missing' ;; esac

store="$HOME/.local/share/alter-bridge"
versions="$store/versions"
target="$versions/v$version"
current="$store/current"
bin_dir="$HOME/.local/bin"
config_dir="$HOME/.config"
config="$config_dir/alter-bridge.json"
launch_dir="$HOME/Library/LaunchAgents"
log_dir="$HOME/Library/Logs"
plist="$launch_dir/com.rntgspr.alter-bridge.plist"
log="$log_dir/alter-bridge.log"

for directory in "$HOME/.local" "$HOME/.local/share" "$store" "$versions" "$bin_dir" "$config_dir" "$HOME/Library" "$launch_dir" "$log_dir"; do
  [ ! -L "$directory" ] || fail "directory is a symlink: $directory"
  [ ! -e "$directory" ] || [ -d "$directory" ] || fail "directory path is occupied: $directory"
  [ ! -d "$directory" ] || [ "$(stat -f %u "$directory")" = "$(id -u)" ] || fail "directory is not owned by this user: $directory"
done
reuse=false
if [ -e "$target" ] || [ -L "$target" ]; then
  [ ! -L "$target" ] && [ -d "$target" ] && [ -f "$target/SHA256SUMS" ] || fail "unrelated versioned target: $target"
  [ -z "$(find "$target" ! -type f ! -type d -print -quit)" ] || fail "versioned target contains a symlink or special file: $target"
  [ "$(find "$target" -type f | wc -l | tr -d ' ')" = 15 ] || fail "versioned target contains unrelated files: $target"
  cmp -s "$root/SHA256SUMS" "$target/SHA256SUMS" || fail "versioned target differs from package: $target"
  for path in VERSION bin/alter-bridge scripts/client/install.sh INSTALL-OFFLINE.md \
    native-assets/manifest.json native-assets/claude-hooks.json native-assets/codex-hooks.json \
    skills/abg/SKILL.md skills/abg-who/SKILL.md skills/abg-doctor/SKILL.md skills/abg-connect-context/SKILL.md skills/abg-connect-device/SKILL.md skills/abg-disconnect-context/SKILL.md skills/abg-disconnect-device/SKILL.md; do
    cmp -s "$root/$path" "$target/$path" || fail "versioned target has changed: $target/$path"
  done
  reuse=true
fi
if [ -e "$current" ] || [ -L "$current" ]; then
  [ -L "$current" ] || fail "current target is not a symlink: $current"
  old_target=$(readlink "$current")
  case "$old_target" in versions/v*/*|versions/v|versions/v*[!0-9A-Za-z.-]*) fail "current target is unrelated: $current" ;; versions/v*) ;; *) fail "current target is unrelated: $current" ;; esac
  [ ! -L "$store/$old_target" ] && [ -d "$store/$old_target" ] || fail "current target is not a real installed version: $current"
fi
for name in alter-bridge abg; do
  link="$bin_dir/$name"
  if [ -e "$link" ] || [ -L "$link" ]; then
    [ -L "$link" ] && [ "$(readlink "$link")" = "$current/bin/alter-bridge" ] || fail "unrelated executable path: $link"
  fi
done
if [ -e "$config" ] || [ -L "$config" ]; then
  [ ! -L "$config" ] && [ -f "$config" ] && [ "$(stat -f %u "$config")" = "$(id -u)" ] || fail 'existing config must be an owned regular file'
  [ "$(stat -f %Lp "$config")" = 600 ] || fail 'existing config must have mode 600'
  [ "$(plutil -extract version raw -o - "$config" 2>/dev/null)" = 3 ] || fail 'existing config is invalid or unsupported; repair it explicitly before installation'
fi
for value in "${CODEX_HOME:-}" "${CLAUDE_CONFIG_DIR:-}"; do
  case "$value" in ''|/*) ;; *) fail 'provider config roots must be absolute for LaunchAgent installation' ;; esac
done
for file in "$plist" "$log"; do
  [ ! -L "$file" ] || fail "daemon path is a symlink: $file"
  [ ! -e "$file" ] || { [ -f "$file" ] && [ "$(stat -f %u "$file")" = "$(id -u)" ]; } || fail "daemon path is not an owned regular file: $file"
done
if [ -e "$plist" ]; then
  [ "$(plutil -extract Label raw -o - "$plist" 2>/dev/null)" = com.rntgspr.alter-bridge ] || fail 'existing LaunchAgent has an unrelated label'
  [ "$(plutil -extract ProgramArguments.0 raw -o - "$plist" 2>/dev/null)" = "$current/bin/alter-bridge" ] || fail 'existing LaunchAgent points to an unrelated executable'
fi

printf 'Install preview\n  Package: %s\n  Binary: %s\n  Commands: %s and %s\n  Config: %s (create version 3 only if absent)\n  LaunchAgent: %s\n  Native providers: unchanged\n' "$root" "$target/bin/alter-bridge" "$bin_dir/alter-bridge" "$bin_dir/abg" "$config" "$plist"
printf 'Proceed? [y/N] '
IFS= read -r answer || answer=
[ "$answer" = y ] || [ "$answer" = Y ] || { printf 'declined; no changes made\n'; exit 0; }

umask 077
mkdir -p "$versions" "$bin_dir" "$config_dir"
if [ "$reuse" = false ]; then
  stage=$(mktemp -d "$versions/.v$version.XXXXXX") || fail 'could not create private package staging directory'
  trap 'rm -rf "$stage"' EXIT HUP INT TERM
  cp -R "$root/." "$stage/" || fail 'could not copy package into versioned staging directory'
  for path in VERSION bin/alter-bridge scripts/client/install.sh INSTALL-OFFLINE.md \
    native-assets/manifest.json native-assets/claude-hooks.json native-assets/codex-hooks.json \
    skills/abg/SKILL.md skills/abg-who/SKILL.md skills/abg-doctor/SKILL.md skills/abg-connect-context/SKILL.md skills/abg-connect-device/SKILL.md skills/abg-disconnect-context/SKILL.md skills/abg-disconnect-device/SKILL.md SHA256SUMS; do
    cmp -s "$root/$path" "$stage/$path" || fail "staged package differs: $path"
  done
  [ ! -e "$target" ] && [ ! -L "$target" ] || fail "versioned target appeared during copy: $target"
  mv "$stage" "$target" || fail 'could not promote complete package into versioned store'
  trap - EXIT HUP INT TERM
fi
if [ ! -e "$config" ]; then
  ( set -C; printf '{\n  "version": 3\n}\n' > "$config" ) || fail 'could not create private config'
  chmod 600 "$config"
fi
next="$store/.current-$$"
ln -s "versions/v$version" "$next"
mv -fh "$next" "$current"
for name in alter-bridge abg; do
  link="$bin_dir/$name"
  next="$bin_dir/.$name-$$"
  ln -s "$current/bin/alter-bridge" "$next"
  mv -fh "$next" "$link"
done
"$current/bin/alter-bridge" daemon install >/dev/null || fail 'payload installed, but LaunchAgent installation failed; inspect the daemon path'
printf 'Installed alter-bridge v%s. Connect a device, then run abg config codex|claude to set up a provider.\n' "$version"
