# Manual operation

Run `abg --version` or `alter-bridge --version` to print
`alter-bridge <package-version>` followed by a newline and exit successfully.
The version comes from the invoked client's Cargo build metadata, not the running
daemon or broker. This query requires no HOME, configuration, network or daemon
and changes no state. Extra arguments are rejected with usage exit code 2.

The native MCP source workflow is described in
the server operator guidance, and agent message handling in
[skills/abg/SKILL.md](skills/abg/SKILL.md). The previous
plugin-based notebook checklist is obsolete. Client packaging, the server image
and confirmed setup are described in [INSTALL-OFFLINE.md](INSTALL-OFFLINE.md).
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
