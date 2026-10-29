#!/usr/bin/env bash
# Installs the client, pairs it with the Compose broker and configures native providers.
set -Eeuo pipefail
umask 077

step=preflight
stage=
connection_output=

# Reports the failed step while preserving the original command output and status.
report_error() {
  local result=$?
  printf '\nDevelopment update failed during %s (exit %s): %s\n' "$step" "$result" "$BASH_COMMAND" >&2
  printf 'Earlier steps may already have completed; no automatic rollback was performed.\n' >&2
  exit "$result"
}

# Removes only an unpublished staging directory when the script exits.
cleanup() {
  if [[ -n "$stage" && -d "$stage" ]]; then
    rm -rf -- "$stage"
  fi
  if [[ -n "$connection_output" ]]; then
    rm -f -- "$connection_output"
  fi
}

trap report_error ERR
trap cleanup EXIT

[[ $# -eq 0 ]] || { printf 'usage: bash scripts/client/dev-install.sh\n' >&2; exit 2; }
[[ $(uname -s) == Darwin ]] || { printf 'Development installation requires macOS.\n' >&2; exit 1; }
cd "$(dirname "$0")/../.."

store="$HOME/.local/share/alter-bridge"
current="$store/current"
bin_dir="$HOME/.local/bin"
config="$HOME/.config/alter-bridge.json"
for tool in cargo jq shasum codesign launchctl plutil docker; do
  command -v "$tool" >/dev/null
done
for directory in "$HOME/.local" "$HOME/.local/share" "$store" "$store/versions" "$bin_dir"; do
  [[ ! -L "$directory" ]]
  if [[ -e "$directory" ]]; then
    [[ -d "$directory" && $(stat -f %u "$directory") == "$(id -u)" ]]
  fi
done
if [[ -e "$current" || -L "$current" ]]; then
  [[ -L "$current" ]]
  previous=$(readlink "$current")
  [[ "$previous" =~ ^versions/v[0-9A-Za-z.-]+$ && ! -L "$store/$previous" && -d "$store/$previous" ]]
fi
for name in alter-bridge abg; do
  if [[ -e "$bin_dir/$name" || -L "$bin_dir/$name" ]]; then
    [[ -L "$bin_dir/$name" && $(readlink "$bin_dir/$name") == "$current/bin/alter-bridge" ]]
  fi
done
[[ ! -L "$HOME/.config" && ! -L "$config" ]]
if [[ -e "$HOME/.config" ]]; then
  [[ -d "$HOME/.config" && $(stat -f %u "$HOME/.config") == "$(id -u)" ]]
fi
if [[ -e "$config" ]]; then
  [[ -f "$config" && $(stat -f %u "$config") == "$(id -u)" && $(stat -f %Lp "$config") == 600 ]]
  jq -e '.version == 3' "$config" >/dev/null
fi
providers=()
for provider in codex claude; do
  if command -v "$provider" >/dev/null ||
    { [[ -f "$config" ]] && jq -e --arg provider "$provider" '.setup[$provider] != null' "$config" >/dev/null; }; then
    providers+=("$provider")
  fi
done
[[ ${#providers[@]} -gt 0 ]] || { printf 'Install Codex or Claude before configuring native development integration.\n' >&2; exit 1; }
plist="$HOME/Library/LaunchAgents/com.rntgspr.alter-bridge.plist"
[[ ! -L "$plist" ]]
if [[ -e "$plist" ]]; then
  [[ -f "$plist" && $(stat -f %u "$plist") == "$(id -u)" ]]
  [[ $(plutil -extract Label raw -o - "$plist") == com.rntgspr.alter-bridge ]]
  [[ $(plutil -extract ProgramArguments.0 raw -o - "$plist") == "$current/bin/alter-bridge" ]]
fi
docker compose -f ../server/deploy/server/compose.yaml ps --status running --services | grep -qx server
hexid=$(sh ../server/scripts/server/hexid.sh)
url="http://127.0.0.1:${ALTER_BRIDGE_PORT:-8787}"
if [[ -f "$config" ]]; then
  jq -e --arg url "$url" '.token == null or .url == $url' "$config" >/dev/null || {
    printf 'Saved credentials belong to another broker; refusing to replace them.\n' >&2
    exit 1
  }
fi

step=build
printf '\nBuilding the local client...\n'
cargo build --locked --package alter-bridge-cli
binary="${CARGO_TARGET_DIR:-target}/debug/alter-bridge"
[[ -x "$binary" ]]

step=assets
mkdir -p "$store/versions" "$bin_dir" "$HOME/.config"
stage=$(mktemp -d "$store/versions/.dev.XXXXXX")
mkdir -p "$stage/bin" "$stage/native-assets" "$stage/scripts/client"
cp "$binary" "$stage/bin/alter-bridge"
codesign --force --sign - --timestamp=none "$stage/bin/alter-bridge"
codesign --verify --strict "$stage/bin/alter-bridge"
cp -R skills "$stage/skills"
cp native-assets/claude-hooks.json native-assets/codex-hooks.json "$stage/native-assets/"
cp scripts/client/install-daemon.sh "$stage/scripts/client/"
sh scripts/client/manifest.sh > "$stage/native-assets/manifest.json"
while IFS=$'\t' read -r path expected; do
  actual=$(shasum -a 256 "$stage/$path")
  [[ ${actual%% *} == "$expected" ]]
done < <(jq -r '.files | to_entries[] | [.key, .value] | @tsv' "$stage/native-assets/manifest.json")
version=$(jq -r .version "$stage/native-assets/manifest.json")
printf '%s\n' "$version" > "$stage/VERSION"
destination="$store/versions/v$version-dev.$(date +%Y%m%d%H%M%S).$$"
[[ ! -e "$destination" && ! -L "$destination" ]]

step=installation
mv "$stage" "$destination"
stage=
next="$store/.current-dev-$$"
ln -s "versions/$(basename "$destination")" "$next"
mv -fh "$next" "$current"
for name in alter-bridge abg; do
  if [[ ! -e "$bin_dir/$name" && ! -L "$bin_dir/$name" ]]; then
    ln -s "$current/bin/alter-bridge" "$bin_dir/$name"
  fi
done
if [[ ! -e "$config" ]]; then
  (set -C; printf '{"version":3}\n' > "$config")
fi
printf 'Installed development build: %s\n' "$destination"

step=device-connection
printf '\nConnecting this device to the local Docker broker...\n'
connection_output=$(mktemp)
"$bin_dir/abg" connect device "$url" --hexid "$hexid" | tee "$connection_output"
unset hexid
jq -e -s 'map(select(.device_connected == true)) | length > 0' < <(sed -n '/^{/p' "$connection_output") >/dev/null || {
  printf 'Device connection was not confirmed. Inspect the daemon log before retrying.\n' >&2
  exit 1
}

step=providers
for provider in "${providers[@]}"; do
  printf '\nInstalling %s skills, hooks and HTTP MCP profile for %s/mcp...\n' "$provider" "$url"
  printf 'y\n' | "$bin_dir/abg" config "$provider" --assets "$destination"
done

step=restart
printf '\nRestarting the daemon...\n'
"$bin_dir/abg" daemon restart
printf '\nDevelopment update completed: %s\n' "$destination"
printf 'Reopen your provider to load the MCP profile, then invoke abg-connect-context in this conversation.\n'
printf 'Please test conversation connection through the skill and abg-who against the Docker broker.\n'
printf 'MCP authentication and conversation participation remain unverified until that manual test.\n'
