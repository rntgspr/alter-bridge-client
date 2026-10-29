#!/bin/sh
set -eu

if [ "$#" -ne 1 ]; then
  printf 'usage: ensure.sh /absolute/path/to/alter-bridge\n' >&2
  exit 2
fi

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
binary=$(readlink -f "$1")
stable="$HOME/.local/share/alter-bridge/current/bin/alter-bridge"
if [ -x "$stable" ] && [ "$(readlink -f "$stable")" = "$binary" ]; then
  binary="$stable"
fi
plist="$HOME/Library/LaunchAgents/com.rntgspr.alter-bridge.plist"
if [ -f "$plist" ] && [ "$(plutil -extract ProgramArguments.0 raw -o - "$plist" 2>/dev/null)" = "$binary" ] && "$binary" daemon status >/dev/null 2>&1; then
  exit 0
fi

exec sh "$here/install-daemon.sh" "$binary"
