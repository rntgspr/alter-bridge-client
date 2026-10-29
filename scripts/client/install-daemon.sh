#!/bin/sh
set -eu

if [ "$#" -ne 1 ] || [ ! -x "$1" ]; then
  printf 'usage: install.sh /absolute/path/to/alter-bridge\n' >&2
  exit 2
fi

case "$1" in
  /*) ;;
  *) printf 'alter-bridge: executable path must be absolute\n' >&2; exit 2 ;;
esac

umask 077
label=com.rntgspr.alter-bridge
domain="gui/$(id -u)"
directory="$HOME/Library/LaunchAgents"
plist="$directory/$label.plist"
log="$HOME/Library/Logs/alter-bridge.log"
if [ -L "$directory" ] || [ -L "$HOME/Library/Logs" ] || [ -L "$plist" ] || [ -L "$log" ]; then
  printf 'alter-bridge: daemon paths must not be symlinks\n' >&2
  exit 1
fi
for existing in "$plist" "$log"; do
  if [ -e "$existing" ] && { [ ! -f "$existing" ] || [ "$(stat -f %u "$existing")" != "$(id -u)" ]; }; then
    printf 'alter-bridge: existing daemon files must be owned regular files\n' >&2
    exit 1
  fi
done
if [ ! -d "$directory" ]; then
  mkdir -m 700 -p "$directory"
fi
if [ ! -d "$HOME/Library/Logs" ]; then
  mkdir -m 700 -p "$HOME/Library/Logs"
fi
if [ "$(stat -f %u "$directory")" != "$(id -u)" ] || [ "$(stat -f %u "$HOME/Library/Logs")" != "$(id -u)" ]; then
  printf 'alter-bridge: daemon directories must be owned by the current user\n' >&2
  exit 1
fi

# Escape XML text before inserting a user-selected executable path.
xml_escape() {
  printf '%s' "$1" | sed -e 's/\&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g' -e 's/"/\&quot;/g' -e "s/'/\&apos;/g"
}

binary=$(xml_escape "$1")
log_path=$(xml_escape "$log")
search_path=$(xml_escape "$HOME/.local/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin")
provider_roots=
if [ -n "${CODEX_HOME:-}" ]; then
  case "$CODEX_HOME" in /*) ;; *) printf 'alter-bridge: CODEX_HOME must be absolute\n' >&2; exit 1 ;; esac
  provider_roots="$provider_roots<key>CODEX_HOME</key><string>$(xml_escape "$CODEX_HOME")</string>"
fi
if [ -n "${CLAUDE_CONFIG_DIR:-}" ]; then
  case "$CLAUDE_CONFIG_DIR" in /*) ;; *) printf 'alter-bridge: CLAUDE_CONFIG_DIR must be absolute\n' >&2; exit 1 ;; esac
  provider_roots="$provider_roots<key>CLAUDE_CONFIG_DIR</key><string>$(xml_escape "$CLAUDE_CONFIG_DIR")</string>"
fi
tmp=$(mktemp "$directory/.alter-bridge.XXXXXX")
trap 'rm -f "$tmp"' EXIT HUP INT TERM
cat > "$tmp" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>$label</string>
<key>ProgramArguments</key><array><string>$binary</string><string>daemon</string><string>run</string></array>
<key>RunAtLoad</key><true/>
<key>KeepAlive</key><dict><key>Crashed</key><true/></dict>
<key>EnvironmentVariables</key><dict><key>PATH</key><string>$search_path</string>$provider_roots</dict>
<key>StandardOutPath</key><string>$log_path</string>
<key>StandardErrorPath</key><string>$log_path</string>
</dict></plist>
EOF
plutil -lint "$tmp" >/dev/null
chmod 600 "$tmp"
launchctl bootout "$domain/$label" >/dev/null 2>&1 || :
mv -f "$tmp" "$plist"
launchctl bootstrap "$domain" "$plist"
