---
name: abg-disconnect-device
description: Explicitly disconnect this entire device from Alter Bridge, revoking its registration and disabling all of its chat bindings. Use when the user requests device-wide disconnection rather than leaving only the current context.
---

# Disconnect device

This is a native model skill. Terminal `abg disconnect device` performs deterministic operations and does not invoke this skill.

1. Establish that the request covers the entire device. Explain that revocation disables every chat binding owned by its current registration. For a request limited to this chat, use `abg-disconnect-context` instead. Do not expand an ambiguous chat-level request into device-wide revocation.
2. Use the current six lowercase hexadecimal pairing hexid and the saved device ID explicitly selected for this operation. Request missing inputs; never guess the ID from a chat, cwd or hostname, retrieve the hexid from logs, or confuse it with the six decimal peer PIN used per request or by `abg-handshake`. Saved nonsecret values may be offered for explicit confirmation.
3. Before revoking, end every local handshake session of this chat as `abg` **Handshake session** specifies; while messaging is still available, best-effort send each correlated revoke notice. A notice failure never blocks disconnection; report remote termination as unconfirmed. Other chats on this device lose their bindings but keep any context-only handshake state until their own sessions end.
4. Run `abg disconnect device --hexid <hexid> --id <saved-device>`. The CLI reads the saved server URL and private disconnect key and refuses an ID that differs from the saved device. Never read or print bearer tokens or disconnect keys, run `abg mcp headers`, or request private credentials from the user.
5. Read the actual outcome. Without a valid server acknowledgement, credentials remain saved and the daemon is unchanged; revocation may already have completed. Report that uncertainty and retry only the same operation when requested. Never delete credentials, stop the daemon manually, reconnect, or issue replacement credentials to mask a failure.
6. Report server revocation, local credential clearing and daemon stop separately. After acknowledgement, local save or daemon stop can still fail; report the precise partial result and preserve recovery information. On full success, the saved URL, device ID and setup preferences remain. Do not remove native MCP configuration, uninstall assets, stop the broker, or affect another device.

Disconnecting does not read, acknowledge or send messages. Do not claim that stored history was erased or that already submitted native notices were recalled.
