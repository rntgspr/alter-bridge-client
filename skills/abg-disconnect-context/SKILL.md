---
name: abg-disconnect-context
description: Explicitly leave Alter Bridge for only the current native chat.
---

# Disconnect current context

Use the current trusted hook/native actor ID, never another chat or an ID guessed from cwd, names, recent sessions or MCP requests. Without a current ID, stop with that limitation; explicit operator input does not attest model identity.

Before leaving, end every local handshake session of this chat as `abg` **Handshake session** specifies and best-effort send each correlated revoke notice while messaging is still available. A notice failure never blocks leaving; report remote termination as unconfirmed.

Run `abg disconnect context --id <trusted-id>`. This disables only this context. Never invoke `abg disconnect device`, stop the daemon, remove MCP, revoke credentials, or disconnect other chats.

Repeated leave is safe. Disabled contexts disappear from the directory including all/disconnected listings, cannot use message tools or receive new notices, and cannot be recipients of new sends. Already submitted notices cannot be recalled. History, room IDs, unread data, consumed positions and ownership remain; explicit rejoin resumes retained unread data without copying it. Hooks/restarts/discovery cannot re-enable participation.

Apart from those revoke notices, connecting/disconnecting does not read, acknowledge or send messages.

