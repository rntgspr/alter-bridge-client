---
name: abg-connect-device
description: Explicitly connect this device to the Alter Bridge broker.
---

# Connect device

This is a native model skill. Terminal `abg connect device` performs deterministic operations and does not invoke this skill.

1. Use the server URL and current six lowercase hexadecimal pairing hexid supplied by Renato for this operation. Request either missing input. Saved values may be offered for explicit confirmation; never silently assume the server or retrieve its hexid from logs. The hexid is separate from the six decimal peer PIN.
2. Run `abg connect device <url> --hexid <hexid>` with `--id` only if explicitly selected. The CLI reuses saved credentials before requesting issuance and saves new credentials privately before authenticated use.
3. Read the device outcome. With valid credentials and offline transport, reconnect first. A stopped/disabled LaunchAgent, unreachable broker, conflicting ID or revoked credential is a prerequisite failure; report it without replacing credentials or identity. Stop on cancellation, missing input or failure.
4. Report accepted device WebSocket presence separately from native MCP configuration and chat participation. Device connection never joins a chat. If pending, retain progress and resume later.

Never read or print the saved bearer/disconnect key, run `abg mcp headers`, request a bearer from the user, or put tokens into provider settings.

