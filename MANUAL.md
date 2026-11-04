# Manual operation

Run `abg --version` or `alter-bridge --version` to print
`alter-bridge <package-version>` followed by a newline and exit successfully.
The version comes from the invoked client's Cargo build metadata, not the running
daemon or broker. This query requires no HOME, configuration, network or daemon
and changes no state. Extra arguments are rejected with usage exit code 2.

The native MCP source workflow is described in
the server operator guidance, and agent message handling in
[skills/abg/SKILL.md](skills/abg/SKILL.md). The previous
plugin-based notebook checklist is obsolete. The client installs only through
`brew install rntgspr/tap/alter-bridge`; packaging lives in `scripts/client/package.sh`.
No archive has been built or published for this source version. Renato owns
runtime E2E; no real provider exchange or wake-up result is claimed here.

For a read-only operational report, use `alter-bridge doctor` as described in
its built-in help. Its `ok` findings
cover observed prerequisites; native execution and model consumption remain
unchecked.

For the saved remote broker directory, run `alter-bridge who` or
`alter-bridge who --all`. The command requires active saved device credentials;
it does not require a local daemon or Docker. See the server operator guidance
for connection and activity meanings. Runtime behavior awaits Renato's manual check.

To exercise wake-up, run `alter-bridge probe <actor> [message] [--local] [--async]`.
`<actor>` is an exact ID, name or `device::name` (optional leading `#`). The
default body is `Test probe: please do not reply.` Without `--local` the CLI
joins a transient `abg_probe_<nanoid>` binding, sends through MCP `send`, waits
up to 15s for the delivered receipt (recorded by the local daemon under
`~/.local/state/alter-bridge/probes/`, so restart an outdated daemon first),
then leaves the binding; `--async` returns after the send. `--local` rings one
binding owned by this device directly, writes nothing to the broker and carries
no body. Recipients see a metadata-only probe notice with no reply expected.

After a native chat restart or clear issues a new session ID, run
`abg reconnect context --provider <codex|claude|opencode> --previous-id <old-id> --id <new-id> [--name <friendly-name>] --tools-available`
(or the `abg-reconnect-context` skill). It refuses identical IDs and an old ID
that is not an owned binding of this device, leaves the old binding, then joins
the new ID with the old name unless `--name` is given. Leave and join outcomes
are reported separately; a failed join prints "old left, new not joined" and the
retry command. Messages, rooms and cursors stay with the old ID.
