---
name: abg-connect-context
description: Explicitly join the current native chat to Alter Bridge.
---

# Connect current context

This is an explicit opt-in operation. Startup, normal prompts, MCP initialization and discovery never authorize joining.

1. Obtain the current exact neutral actor ID from trusted lifecycle/hook context. Never infer it from cwd, names, recent sessions, MCP request IDs or another chat. Without it, stop and explain that a bare terminal has no native identity. An explicit operator-supplied ID is operator input, not cryptographic model attestation.
2. Determine the native provider, then run `abg connect context --provider <codex|claude|opencode> --id <trusted-id> --status`. Retain inputs for this operation.
3. If device_required/device_pending, invoke the installed `abg-connect-device` skill once, carrying the supplied URL/hexid. Resume only after accepted device connection succeeds. Cancellation, missing input or failure stops this flow; do not recursively invoke skills or loop enrollment.
4. Inspect native configuration. Run `abg config <provider>` only when needed and honor its preview, confirmation, ownership and trust gates. An owned entry with missing/conflicting authentication needs that specific diagnostic and previewed repair. Never overwrite unrelated/unowned edits. After setup, return pending with instructions to reopen/reload the provider and invoke this skill again. Registration or a UI badge does not prove MCP readiness.
5. Confirm the actual Alter Bridge tools are exposed in this chat. If unavailable, report pending and resume after native reload; do not join. Never execute the secret-output header helper to diagnose this.
6. Use a native title already available in trusted metadata or Renato's explicit friendly name. If absent, request the name. Never use cwd, guessed model name or the ID as a friendly name. The name becomes the context's `#name` reference (see the `abg` skill); when it has whitespace or collides with another connected name, say that it needs `#device::name` or the ID and let Renato keep or change it.
7. Run `abg connect context --provider <provider> --id <trusted-id> --name <friendly-name> --tools-available`. This last flag records the skill's observation of tools; CLI cannot inspect the model tool registry.
8. Report device connection, native configuration, actual tools and context registration independently. Repeated explicit join may refresh the name. No inbox retrieval, message consumption or send is part of connecting.

MCP authorization comes privately from saved device credentials through the URL-bound helper. Never request/print a bearer, place it literally in config, or use a token value as `bearer_token_env_var`.

