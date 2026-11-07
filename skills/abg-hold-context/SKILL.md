---
name: abg-hold-context
description: Explicitly hold Alter Bridge notices for only the current native chat.
---

# Hold current context

Use the current trusted hook/native actor ID (`alter-bridge trusted current actor: {"id":...}`), never another chat or an ID guessed from cwd, names, recent sessions or MCP requests. Without a current ID, stop with that limitation; explicit operator input does not attest model identity. Never accept another actor.

Act only on explicit invocation. Call the MCP `hold` tool with `actor` = `{"id": "<trusted-id>"}`. The tool acts only on contexts this device owns. This holds only this context; never hold other chats, disconnect, stop the daemon or change credentials.

While held, this context receives no notice at all: no ping, no replay and no prompt-hook notice. Messages stay stored, the context stays connected, and explicit `inbox`/`peek` still work. Holding is repeat-safe.

Report the result `{"actor":{"id":...},"held":true}`. To release, use `abg-release-context` in this chat, or run `abg release <actor>` from outside it.
