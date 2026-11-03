---
name: abg-reconnect-context
description: Explicitly move the current native chat's Alter Bridge binding from its previous native ID to its new one after a restart or clear.
---

# Reconnect current context

This is an explicit operation. Startup, hooks, prompts and discovery never authorize reconnecting.

1. Take the new exact actor ID only from trusted lifecycle/hook context of this chat. Never infer it from cwd, names, recent sessions, MCP request IDs or another chat. Without it, stop.
2. Resolve the previous ID with the `abg` skill's who-only directory procedure from Renato's `#name`, `#device::name` or explicit ID. An ambiguous name stops with a request for `#device::name` or the exact ID. The previous ID must be a binding of this device and must differ from the new ID.
3. Apply the `abg-connect-context` readiness gates: device connected, native configuration installed and actual Alter Bridge MCP tools visible in this chat. Missing input, cancellation or failure stops this flow.
4. Show `#name previous-id -> new-id` (and a new name when Renato gave one) and wait for Renato's explicit confirmation. Run nothing before it.
5. When the MCP `reconnect` tool is visible, call it with `actor` = new ID, `previous_id` and optional `name`. Otherwise run `abg reconnect context --provider <codex|claude|opencode> --previous-id <previous-id> --id <new-id> [--name <friendly-name>] --tools-available`.
6. Report leave and join outcomes independently. On `old_left_new_not_joined`, report that the old binding was left and the new one was not joined, with the returned retry. Messages, rooms and cursors stay with the previous ID; nothing is migrated, and peers keep the old thread.

Reconnecting never reads, acknowledges or sends messages.
