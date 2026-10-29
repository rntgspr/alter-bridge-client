#!/bin/sh
set -eu

label=com.rntgspr.alter-bridge
domain="gui/$(id -u)"
plist="$HOME/Library/LaunchAgents/$label.plist"
launchctl bootout "$domain/$label" >/dev/null 2>&1 || :
rm -f "$plist"
