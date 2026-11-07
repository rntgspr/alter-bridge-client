---
name: abg-release-context
description: Explicitly release held Alter Bridge notices for only the current native chat.
---

# Release current context

Use the current trusted hook/native actor ID (`alter-bridge trusted current actor: {"id":...}`), never another chat or an ID guessed from cwd, names, recent sessions or MCP requests. Without a current ID, stop with that limitation; explicit operator input does not attest model identity. Never accept another actor.

Act only on explicit invocation. Call the MCP `release` tool with `actor` = `{"id": "<trusted-id>"}`. The tool acts only on contexts this device owns. This releases only this context; never release other chats.

Release ends the hold: the context receives notices again, and pending notices for messages stored during the hold are replayed. Releasing a context that is not held is safe.

Report the result `{"actor":{"id":...},"held":false}`. From outside the chat, run `abg release <actor>` instead.
