# Standalone macOS package

This describes local packages built from source and a future matching release.
The incompatible identity contract has not been published. The existing `v0.7.5` tag lacks
the native manifest and cannot supply the new online assets; a later explicit
version bump and matching release are required.

The client ZIP contains a universal `alter-bridge` executable, daemon files,
native provider assets and `SHA256SUMS`. The server is distributed as a Docker
image; Compose/Caddy deployment files live in the source checkout. A notebook
client needs no local Docker.
The two command names, `alter-bridge` and `abg`, point to the same installed
executable. The package is signed ad hoc by default. An ad hoc signature checks
code integrity but does not provide Developer ID identity or notarization;
Gatekeeper acceptance is not promised. A release builder may set
`CODESIGN_IDENTITY` to a Developer ID certificate, subject to Apple's separate
notarization process. See [Apple Developer ID](https://developer.apple.com/developer-id/)
and [Apple notarization](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution).

1. Transfer `alter-bridge-v<VERSION>.zip` and its `.sha256` file to the Mac.
2. Verify the ZIP with `shasum -a 256 -c alter-bridge-v<VERSION>.zip.sha256`,
   then extract it. Inside `alter-bridge-v<VERSION>`, run
   `shasum -a 256 -c SHA256SUMS`.
3. Run `./scripts/client/install.sh` from the extracted folder. Read the preview and confirm.
   The installer refuses unrelated files at either command name or its
   versioned target. It creates a private credential-free version-3 config
   only when absent, keeps existing config, logs, profiles, and volumes, and
   installs the user LaunchAgent. It does not connect a device or configure a
   provider. An existing unsupported config is left untouched for deliberate
   operator repair.
4. Connect the device with `abg connect device <broker-url> --hexid <pairing-code>`.
   Then run `abg config codex --assets <absolute-package-root>` or
   `abg config claude --assets <absolute-package-root>` to use the bundled
   version-matched assets without GitHub access. This operation previews the
   native changes and asks for separate confirmation. The installed package
   root at `~/.local/share/alter-bridge/versions/v<VERSION>` remains available
   after the transferred extraction folder is removed.

For an independent server host, select the matching published image with
`ALTER_BRIDGE_IMAGE` and use the server distribution's Compose file.
See the server operator guidance for image build, pairing-code inspection,
stop and restart. No native server installation is needed.

After reopening the provider, invoke the native `$abg-connect-context` skill in
Codex or `/abg-connect-context` in Claude to explicitly join the current chat.
The device skill and context join/leave skills plus Claude command wrappers are
bundled with exact-version digests. MCP setup alone does not join a chat.
Invoke `$abg-who` in Codex or `/abg-who` in
Claude to list connected agents by name, full session ID and device. The broker
does not expose a provider; the daemon resolves the local delivery adapter.
Provider discovery and execution need manual verification. This identity
contract is incompatible with earlier provider-qualified clients; update server
and clients together before use, preserving existing volumes for operator review.

Omit `--assets` only when the exact CLI release tag is available on GitHub;
online setup never falls back to another tag or branch. `abg doctor` reports
local and broker observations. Native startup, message delivery, and provider
behavior require the operator's manual end-to-end check.
