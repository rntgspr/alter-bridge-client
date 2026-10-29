#!/usr/bin/env bash
# Tears down the local development environment after a confirmed preview: broker containers and images, the daemon's launchd job and the installed client.
set -Eeuo pipefail

step=preflight

# Reports the failed step while preserving the original command output and status.
report_error() {
  local result=$?
  printf '\nDevelopment cleanup failed during %s (exit %s): %s\n' "$step" "$result" "$BASH_COMMAND" >&2
  printf 'Earlier steps may already have completed; no automatic rollback was performed.\n' >&2
  exit "$result"
}

# Refuses explicitly, because macOS bash 3.2 does not apply errexit to a failed [[ ]] assertion.
fail() {
  printf 'Development cleanup refused during %s: %s\nNothing was changed.\n' "$step" "$1" >&2
  exit 1
}

trap report_error ERR

[[ $# -eq 0 ]] || { printf 'usage: bash scripts/client/dev-cleanup.sh\n' >&2; exit 2; }
[[ $(uname -s) == Darwin ]] || { printf 'Development cleanup requires macOS.\n' >&2; exit 1; }
cd "$(dirname "$0")/../.."

compose=(docker compose -f ../server/deploy/server/compose.yaml --profile tls)
label=com.rntgspr.alter-bridge
domain="gui/$(id -u)"
plist="$HOME/Library/LaunchAgents/$label.plist"
store="$HOME/.local/share/alter-bridge"
bin_dir="$HOME/.local/bin"
for tool in launchctl plutil; do
  command -v "$tool" >/dev/null || fail "required tool is missing: $tool"
done

step=inventory
containers=
images=
if ! command -v docker >/dev/null; then
  docker_state='not installed; containers and images skipped'
elif ! docker info >/dev/null 2>&1; then
  docker_state='engine not running; containers and images skipped'
else
  docker_state=running
  containers=$("${compose[@]}" ps -a --format '{{.Name}}' | LC_ALL=C sort)
  images=$({
    docker image ls --filter reference=alter-bridge-server --filter reference='ghcr.io/rntgspr/alter-bridge-server' --format '{{.Repository}}:{{.Tag}}'
    docker image ls --filter label=com.docker.compose.project=alter-bridge --format '{{.Repository}}:{{.Tag}} {{.ID}}'
  } | awk '$1 ~ /<none>/ { print $2; next } { print $1 }' | LC_ALL=C sort -u)
fi

loaded=false
if launchctl print "$domain/$label" >/dev/null 2>&1; then
  loaded=true
fi
if [[ -e "$plist" || -L "$plist" ]]; then
  [[ -f "$plist" && ! -L "$plist" && $(stat -f %u "$plist") == "$(id -u)" ]] || fail "LaunchAgent is not an owned regular file: $plist"
  [[ $(plutil -extract Label raw -o - "$plist" 2>/dev/null) == "$label" ]] || fail "LaunchAgent has an unrelated label: $plist"
fi

paths=()
for name in alter-bridge abg; do
  link="$bin_dir/$name"
  [[ -e "$link" || -L "$link" ]] || continue
  [[ -L "$link" && $(readlink "$link") == "$store/current/bin/alter-bridge" ]] || fail "unrelated executable path: $link"
  paths+=("$link")
done
while IFS= read -r entry; do
  [[ -L "$entry" ]] || fail "installer leftover is not a symlink: $entry"
  paths+=("$entry")
done < <([[ ! -d "$bin_dir" ]] || find "$bin_dir" -mindepth 1 -maxdepth 1 \( -name '.alter-bridge-*' -o -name '.abg-*' \) -print | LC_ALL=C sort)
while IFS= read -r entry; do
  [[ -f "$entry" && ! -L "$entry" && $(stat -f %u "$entry") == "$(id -u)" ]] || fail "LaunchAgent leftover is not an owned regular file: $entry"
  paths+=("$entry")
done < <([[ ! -d "$HOME/Library/LaunchAgents" ]] || find "$HOME/Library/LaunchAgents" -mindepth 1 -maxdepth 1 -name '.alter-bridge.??????' -print | LC_ALL=C sort)
if [[ -e "$store" || -L "$store" ]]; then
  [[ -d "$store" && ! -L "$store" && $(stat -f %u "$store") == "$(id -u)" ]] || fail "client store is not an owned real directory: $store"
  paths+=("$store")
fi

if [[ -z "$containers" && -z "$images" && $loaded == false && ! -e "$plist" && ${#paths[@]} -eq 0 ]]; then
  printf 'Nothing to clean (Docker: %s).\n' "$docker_state"
  exit 0
fi

step=confirmation
printf 'Cleanup preview\n'
printf '  Docker: %s\n' "$docker_state"
printf '  Broker containers (compose down, volumes kept):\n'; printf '    %s\n' ${containers:-none}
printf '  Broker images:\n'; printf '    %s\n' ${images:-none}
printf '  Daemon job: %s (loaded: %s)\n' "$domain/$label" "$loaded"
printf '  LaunchAgent: %s\n' "$([[ -e "$plist" ]] && printf '%s' "$plist" || printf none)"
printf '  Client paths:\n'; [[ ${#paths[@]} -eq 0 ]] && printf '    none\n' || printf '    %s\n' "${paths[@]}"
printf '  Keep: ~/.config/alter-bridge.json (credentials), logs, provider files, Docker volumes\n'
printf '  Note: provider hooks call ~/.local/bin/alter-bridge and fail until the client is reinstalled.\n'
printf 'Proceed? [y/N] '
IFS= read -r answer || answer=
[[ "$answer" == [yY] ]] || { printf 'declined; nothing changed\n'; exit 0; }

step=containers
[[ -z "$containers" ]] || "${compose[@]}" down --remove-orphans

step=images
[[ -z "$images" ]] || docker image rm $images

step=daemon
[[ $loaded == false ]] || launchctl bootout "$domain/$label"
rm -f -- "$plist"

step=client
[[ ${#paths[@]} -eq 0 ]] || rm -rf -- "${paths[@]}"
printf '\nDevelopment cleanup completed. Reinstall from a package with scripts/client/install.sh before using dev-install again.\n'
