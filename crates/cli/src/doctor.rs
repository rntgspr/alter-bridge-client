//! Read-only operator findings; no native client, helper, or MCP session is started.

use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use alter_bridge_protocol::DeviceState;
use serde_json::Value;
use sha2::{Digest, Sha256};
use toml_edit::DocumentMut;

use crate::{Exit, api, config, daemon, setup};

const MAX_PROVIDER_BYTES: u64 = 1024 * 1024;
const PROBE_BUDGET: Duration = Duration::from_secs(4);
const LOG_TAIL_BYTES: u64 = 64 * 1024;

/// Records one independent, actionable finding without private input or raw errors.
fn finding(state: &str, subject: &str, detail: &str, repair: &str) {
    println!("{state}: {subject}: {detail}; next: {repair}");
}

/// Reads one bounded regular file without following a symlink.
fn read_file(path: &Path) -> Result<Option<Vec<u8>>, ()> {
    let file = match fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(()),
    };
    let metadata = file.metadata().map_err(|_| ())?;
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    if !metadata.is_file()
        || metadata.uid() != uid
        || metadata.nlink() != 1
        || metadata.len() > MAX_PROVIDER_BYTES
    {
        return Err(());
    }
    let mut bytes = Vec::new();
    file.take(MAX_PROVIDER_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ())?;
    if bytes.len() as u64 > MAX_PROVIDER_BYTES {
        return Err(());
    }
    Ok(Some(bytes))
}

/// Classifies bounded historical daemon log fragments without returning identifiers or reasons.
fn delivery_counts(home: &Path) -> Option<(usize, usize)> {
    let path = home.join("Library/Logs/alter-bridge.log");
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    let metadata = file.metadata().ok()?;
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    if !metadata.is_file()
        || metadata.uid() != uid
        || metadata.nlink() != 1
        || metadata.permissions().mode() & 0o077 != 0
    {
        return None;
    }
    let start = metadata.len().saturating_sub(LOG_TAIL_BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    file.take(LOG_TAIL_BYTES).read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let mut submitted = 0;
    let mut failed = 0;
    for line in text.lines().skip(if start > 0 { 1 } else { 0 }) {
        let Some((timestamp, record)) = line.split_once(" alter-bridge daemon: ping ") else {
            continue;
        };
        if timestamp.parse::<u64>().is_err() {
            continue;
        }
        let mut fields = record.splitn(4, ' ');
        if fields.next().is_none_or(str::is_empty)
            || fields.next() != Some("for")
            || fields.next().is_none_or(str::is_empty)
        {
            continue;
        }

        match fields.next() {
            Some("submitted locally (replay: true)" | "submitted locally (replay: false)") => {
                submitted += 1;
            }
            Some(outcome) if outcome.starts_with("failed: ") => failed += 1,
            _ => {}
        }
    }
    Some((submitted, failed))
}

/// Reports the stable installed binary and public alias without executing either.
fn inspect_binary(home: &Path, attention: &mut bool) {
    let bin =
        crate::setup::stable_bin(home).unwrap_or_else(|| home.join(".local/bin/alter-bridge"));
    let alias = bin.with_file_name("abg");
    let current = fs::canonicalize(std::env::current_exe().unwrap_or_default()).ok();
    let stable = fs::canonicalize(&bin).ok();
    if stable.is_some() && stable == current {
        finding(
            "ok",
            "binary",
            concat!(
                "stable bin path resolves to CLI v",
                env!("CARGO_PKG_VERSION")
            ),
            "none",
        );
    } else {
        *attention = true;
        finding(
            "attention",
            "binary",
            "stable installed path missing or differs from this executable",
            "install the current release at ~/.local/bin/alter-bridge or through Homebrew",
        );
    }
    if fs::symlink_metadata(&alias).is_ok_and(|metadata| metadata.file_type().is_symlink())
        && fs::canonicalize(&alias).ok() == stable
        && stable.is_some()
    {
        finding("ok", "abg alias", "symlink points to stable binary", "none");
    } else {
        *attention = true;
        finding(
            "attention",
            "abg alias",
            "missing or unexpected target",
            "install the abg alias with the release installer",
        );
    }
}

/// Reports provider-owned files and definitions without claiming native execution.
fn inspect_provider(
    provider: &str,
    home: &Path,
    broker: Option<&str>,
    setup_record: Option<&Value>,
    attention: &mut bool,
) {
    let root = if provider == "codex" {
        std::env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".codex"))
    } else {
        std::env::var_os("CLAUDE_CONFIG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".claude"))
    };
    let skill_root = if provider == "codex" {
        home.join(".agents")
    } else {
        root.clone()
    };
    let hooks = root.join(if provider == "codex" {
        "hooks.json"
    } else {
        "settings.json"
    });
    let mcp = if provider == "codex" {
        root.join("config.toml")
    } else if std::env::var_os("CLAUDE_CONFIG_DIR").is_some() {
        root.join(".claude.json")
    } else {
        home.join(".claude.json")
    };
    let repair = if provider == "codex" {
        "alter-bridge config codex; reopen Codex"
    } else {
        "alter-bridge config claude; reopen Claude"
    };
    finding(
        "unchecked",
        &format!("{provider} MCP tools available in current chat"),
        "configuration and registration do not prove model tool availability",
        "inspect actual native tool exposure; reopen provider after setup and resume the context skill",
    );
    let manifest: Value =
        serde_json::from_str(include_str!("../../../native-assets/manifest.json"))
            .expect("bundled native manifest is valid JSON");
    let skill_paths: Vec<PathBuf> = manifest["files"]
        .as_object()
        .expect("bundled native manifest contains files")
        .keys()
        .filter(|path| path.starts_with("skills/"))
        .map(|path| skill_root.join(path))
        .collect();
    let skill_states: Vec<_> = skill_paths.iter().map(|path| read_file(path)).collect();
    let hooks_state = read_file(&hooks);
    let mcp_state = read_file(&mcp);
    let hook_bytes = hooks_state.as_ref().ok().and_then(Option::as_deref);
    let hook_value = hook_bytes
        .and_then(|bytes| serde_json::from_slice::<Value>(bytes).ok())
        .filter(Value::is_object);
    let mcp_bytes = mcp_state.as_ref().ok().and_then(Option::as_deref);
    let codex_document = if provider == "codex" {
        mcp_bytes
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .and_then(|text| text.parse::<DocumentMut>().ok())
    } else {
        None
    };
    let claude_mcp = if provider == "claude" {
        mcp_bytes
            .and_then(|bytes| serde_json::from_slice::<Value>(bytes).ok())
            .filter(Value::is_object)
    } else {
        None
    };
    let legacy_plugin = if provider == "codex" {
        codex_document
            .as_ref()
            .and_then(|doc| doc.get("plugins"))
            .and_then(|item| item.as_table_like())
            .is_some_and(|entries| {
                entries.iter().any(|(name, item)| {
                    name.contains("alter-bridge")
                        && item
                            .get("enabled")
                            .and_then(|value| value.as_bool())
                            .unwrap_or(true)
                })
            })
    } else {
        hook_value
            .as_ref()
            .and_then(|value| value.get("enabledPlugins"))
            .and_then(Value::as_object)
            .is_some_and(|plugins| {
                plugins
                    .iter()
                    .any(|(name, enabled)| name.contains("alter-bridge") && enabled == true)
            })
    };
    let bridge_hook = hook_value
        .as_ref()
        .and_then(|value| value.get("hooks"))
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|events| events.values())
        .filter_map(Value::as_array)
        .flatten()
        .filter_map(|group| group.get("hooks").and_then(Value::as_array))
        .flatten()
        .filter_map(|handler| handler.get("command").and_then(Value::as_str))
        .any(|command| command.contains("alter-bridge"));
    let bridge_mcp = codex_document.as_ref().is_some_and(|doc| {
        doc.get("mcp_servers")
            .and_then(|servers| servers.get("alter-bridge"))
            .is_some()
    }) || claude_mcp.as_ref().is_some_and(|value| {
        value
            .get("mcpServers")
            .and_then(|servers| servers.get("alter-bridge"))
            .is_some()
    });
    if setup_record.is_none()
        && skill_states.iter().all(|state| matches!(state, Ok(None)))
        && !legacy_plugin
        && !bridge_hook
        && !bridge_mcp
        && (matches!(&hooks_state, Ok(None)) || hook_value.is_some())
        && (matches!(&mcp_state, Ok(None)) || codex_document.is_some() || claude_mcp.is_some())
        && hooks_state.is_ok()
        && mcp_state.is_ok()
    {
        finding(
            "unchecked",
            &format!("{provider} setup"),
            "not configured for this provider",
            repair,
        );
        return;
    }
    for (path, state) in skill_paths.iter().zip(skill_states) {
        let label = format!("{provider} {}", path.display());
        match state {
            Ok(Some(bytes)) => {
                let expected = setup_record
                    .and_then(|record| record.get("files"))
                    .and_then(|files| files.get(path.to_string_lossy().as_ref()))
                    .and_then(Value::as_str);
                let digest = format!("{:x}", Sha256::digest(&bytes));
                if expected == Some(digest.as_str()) {
                    finding(
                        "ok",
                        &label,
                        "installed content matches recorded digest",
                        "none",
                    );
                } else {
                    *attention = true;
                    finding(
                        "attention",
                        &label,
                        "version/integrity unrecorded or content differs",
                        repair,
                    );
                }
            }
            _ => {
                *attention = true;
                finding("attention", &label, "missing or unreadable", repair);
            }
        }
    }
    let label = format!("{provider} setup metadata");
    if let Some(record) = setup_record {
        let current = env!("CARGO_PKG_VERSION");
        if record.get("version").and_then(Value::as_str) == Some(current)
            && record.get("tag").and_then(Value::as_str) == Some(format!("v{current}").as_str())
            && broker.is_some_and(|url| record.get("broker").and_then(Value::as_str) == Some(url))
        {
            finding(
                "ok",
                &label,
                "recorded version, tag, and broker match",
                "none",
            );
        } else {
            *attention = true;
            finding(
                "attention",
                &label,
                "recorded version, tag, or broker differs",
                repair,
            );
        }
    } else {
        *attention = true;
        finding("attention", &label, "no native setup record", repair);
    }
    let label = format!("{provider} MCP");
    if broker.is_some() && setup::verify_managed_entry(provider, home).is_ok() {
        finding(
            "ok",
            &label,
            "owned entry and helper command match recorded broker",
            "none",
        );
    } else {
        *attention = true;
        finding(
            "attention",
            &label,
            "entry/helper missing, conflicting, or unverified",
            repair,
        );
    }
    let label = format!("{provider} hooks");
    if let Some(value) = &hook_value {
        let mut valid = hook_bytes.is_some_and(|bytes| setup::verify_hooks(provider, home, bytes));
        if provider == "claude"
            && (value.get("disableAllHooks").and_then(Value::as_bool) == Some(true)
                || value.get("allowManagedHooksOnly").and_then(Value::as_bool) == Some(true))
        {
            valid = false;
        }
        if valid {
            finding(
                "ok",
                &label,
                "one owned definition per event; user file permits hooks",
                "none",
            );
        } else {
            *attention = true;
            finding(
                "attention",
                &label,
                "definition, enablement, or duplication conflict",
                repair,
            );
        }
        if provider == "claude" && legacy_plugin {
            *attention = true;
            finding(
                "attention",
                "claude legacy plugin",
                "enabled duplicate handler",
                "disable the legacy alter-bridge plugin in Claude settings",
            );
        }
    } else {
        *attention = true;
        finding(
            "attention",
            &label,
            "missing or unreadable user hook file",
            repair,
        );
    }
    if provider == "codex" {
        if let Some(doc) = codex_document {
            if legacy_plugin {
                *attention = true;
                finding(
                    "attention",
                    "codex legacy plugin",
                    "enabled duplicate handler",
                    "disable the legacy alter-bridge plugin in Codex config",
                );
            }
            if crate::setup::has_inline_codex_hooks(&doc)
                || doc
                    .get("features")
                    .and_then(|item| item.get("hooks"))
                    .and_then(|item| item.as_bool())
                    == Some(false)
            {
                *attention = true;
                finding(
                    "attention",
                    "codex hook enablement",
                    "inline hooks or disabled user hooks",
                    "reconcile Codex config.toml and rerun alter-bridge config codex",
                );
            }
        }
    }
    finding(
        "unchecked",
        &format!("{provider} runtime"),
        "native version, project trust, managed policy, and hook execution not probed",
        "review provider version, native hook approval, and effective policy manually",
    );
}

/// Prints local findings first, then bounded nonactivating remote observations.
pub(crate) fn run(args: &[String], home: &Path) -> Result<(), Exit> {
    if !args.is_empty() {
        return Err(Exit::Usage("usage: alter-bridge doctor\nexit: 0 observed checks healthy, 1 attention, 2 invalid invocation\n".into()));
    }
    let mut attention = false;

    finding(
        "unchecked",
        "server operation versus device connection",
        "a local server is started through Docker/Compose; device connection targets an existing local, LAN or online server and does not start it",
        "local server operator: follow deployment Compose instructions; existing-server client: alter-bridge connect device <url> --hexid <code> [--id <device>]; remote clients do not need local Docker",
    );

    inspect_binary(home, &mut attention);
    let path = config::path(home);
    let config_exists = match fs::symlink_metadata(&path) {
        Ok(metadata) => {
            if !metadata.is_file() {
                attention = true;
                finding(
                    "attention",
                    "config path",
                    "not a regular private file",
                    "inspect ~/.config/alter-bridge.json privately",
                );
            }
            true
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            attention = true;
            finding(
                "attention",
                "config path",
                "missing",
                "alter-bridge connect device <url> --hexid <code> [--id <device>]",
            );
            false
        }
        Err(_) => {
            attention = true;
            finding(
                "unknown",
                "config path",
                "cannot inspect",
                "inspect ~/.config/alter-bridge.json privately",
            );
            true
        }
    };
    let preferences = match config::preferences(home) {
        Ok(file) => {
            if config_exists {
                let mode = fs::symlink_metadata(&path)
                    .map(|metadata| metadata.permissions().mode() & 0o777)
                    .unwrap_or(0);
                finding(
                    "ok",
                    "config",
                    &format!("schema 3; private file ~/.config/alter-bridge.json mode {mode:03o}"),
                    "none",
                );
            }
            Some(file)
        }
        Err(error) => {
            attention = true;
            let detail = if error.contains("unsupported version") {
                "unsupported config schema; version 3 required without automatic migration"
            } else if error.contains("private") || error.contains("owned") {
                "unsafe config permissions or ownership"
            } else {
                "malformed or unreadable config"
            };
            finding(
                "attention",
                "config",
                detail,
                "inspect ~/.config/alter-bridge.json privately; repair it before reconnecting",
            );
            None
        }
    };
    let credential_repair = if preferences.is_some() {
        "alter-bridge connect device <url> --hexid <code> [--id <device>]"
    } else {
        "inspect ~/.config/alter-bridge.json privately; repair schema and permissions before reconnecting"
    };
    let broker = preferences.as_ref().and_then(|file| file.url.as_deref());
    if let Some(url) = broker {
        finding("ok", "broker URL", url, "none");
    } else {
        attention = true;
        finding(
            "attention",
            "broker URL",
            "absent or unchecked",
            credential_repair,
        );
    }
    let device = preferences
        .as_ref()
        .and_then(|file| file.device_id.as_deref());
    if let Some(id) = device {
        finding("ok", "device ID", id, "none");
    } else {
        attention = true;
        finding(
            "attention",
            "device ID",
            "absent or unchecked",
            credential_repair,
        );
    }
    let token = preferences.as_ref().and_then(|file| file.token.as_deref());
    let key = preferences
        .as_ref()
        .and_then(|file| file.disconnect_key.as_deref());
    if token.is_some() && key.is_some() {
        finding(
            "ok",
            "credentials",
            "token present [masked]; disconnect key present [masked]",
            "none",
        );
    } else {
        attention = true;
        finding(
            "attention",
            "credentials",
            "token/key absent or unchecked [masked]",
            credential_repair,
        );
    }
    let setup_record = preferences.as_ref().and_then(|file| file.setup.as_ref());
    inspect_provider(
        "codex",
        home,
        broker,
        setup_record.and_then(|value| value.get("codex")),
        &mut attention,
    );
    inspect_provider(
        "claude",
        home,
        broker,
        setup_record.and_then(|value| value.get("claude")),
        &mut attention,
    );
    if crate::opencode::enabled(home) {
        match crate::opencode::mcp_status(home) {
            Ok(Some(state)) if state == "connected" => finding(
                "ok",
                "opencode runtime MCP",
                "alter-bridge server connected in the OpenCode service",
                "none",
            ),
            Ok(state) => {
                attention = true;
                finding(
                    "attention",
                    "opencode runtime MCP",
                    &format!(
                        "alter-bridge server {}",
                        state.as_deref().unwrap_or("not registered")
                    ),
                    "keep the daemon running; it re-registers on its next discovery pass",
                );
            }
            Err(error) => {
                attention = true;
                finding(
                    "attention",
                    "opencode service",
                    &error,
                    "start OpenCode so its background service runs, then rerun doctor",
                );
            }
        }
    } else if setup_record.is_some_and(|value| value.get("opencode").is_some()) {
        attention = true;
        finding(
            "attention",
            "opencode setup",
            "recorded for a different broker",
            "rerun alter-bridge config opencode",
        );
    } else {
        finding(
            "optional",
            "opencode setup",
            "not configured",
            "run alter-bridge config opencode if you use OpenCode",
        );
    }
    let plist = crate::launchd::plist_path(home);
    match read_file(&plist) {
        Ok(Some(bytes))
            if std::str::from_utf8(&bytes).ok().is_some_and(|text| {
                text.contains("com.rntgspr.alter-bridge")
                    && text.contains("<string>daemon</string><string>run</string>")
            }) =>
        {
            finding(
                "ok",
                "launchd definition",
                "owned job file present; service load unchecked",
                "none",
            )
        }
        _ => {
            attention = true;
            finding(
                "attention",
                "launchd definition",
                "missing, unreadable, or unexpected",
                "alter-bridge daemon install",
            );
        }
    }
    match daemon::observed_state(home).as_deref() {
        Some("connected") => finding(
            "ok",
            "daemon local lock",
            "running and last recorded connected",
            "none",
        ),
        Some("starting" | "disconnected") => {
            attention = true;
            finding(
                "attention",
                "daemon local lock",
                "running but no recorded accepted connection",
                "alter-bridge daemon status; inspect ~/Library/Logs/alter-bridge.log; alter-bridge daemon restart if needed",
            );
        }
        Some(_) => {
            attention = true;
            finding(
                "unknown",
                "daemon local lock",
                "unrecognized local state",
                "inspect private daemon lock and log; rerun alter-bridge doctor",
            );
        }
        None => {
            attention = true;
            finding(
                "attention",
                "daemon local lock",
                "no running daemon observed",
                "alter-bridge daemon restart",
            );
        }
    }
    match delivery_counts(home) {
        Some((submitted, failed)) => finding(
            "unchecked",
            "delivery log tail",
            &format!(
                "historical local submissions {submitted}, failures {failed}; device epoch and model reading unverified"
            ),
            "inspect ~/Library/Logs/alter-bridge.log privately for detail; test delivery manually",
        ),
        None => finding(
            "unchecked",
            "delivery log tail",
            "private log unavailable or unsafe; no delivery outcome inferred",
            "inspect ~/Library/Logs/alter-bridge.log privately and test delivery manually",
        ),
    }
    if let Some(url) = broker {
        let started = Instant::now();
        for (route, subject, expected) in [
            ("/healthz", "broker reachability", "ok"),
            ("/readyz", "broker readiness", "ready"),
        ] {
            let remaining = PROBE_BUDGET.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                attention = true;
                finding(
                    "unknown",
                    subject,
                    "shared probe deadline exhausted",
                    "check broker service and rerun alter-bridge doctor",
                );
                continue;
            }
            match api::Client::with_timeout(url, remaining).public_status(route, expected) {
                Ok(true) => finding(
                    "ok",
                    subject,
                    "HTTP 200 and expected health payload",
                    "none",
                ),
                Ok(false) => {
                    attention = true;
                    finding(
                        "attention",
                        subject,
                        "unexpected HTTP status or health payload",
                        "check broker service and rerun alter-bridge doctor",
                    );
                }
                Err(error) => {
                    attention = true;
                    finding(
                        "unknown",
                        subject,
                        &error.public_message(),
                        "check broker URL, TLS trust, and service; rerun alter-bridge doctor",
                    );
                }
            }
        }
        if let Some(token) = token {
            let remaining = PROBE_BUDGET.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                attention = true;
                finding(
                    "unknown",
                    "device state",
                    "shared probe deadline exhausted",
                    "rerun alter-bridge doctor",
                );
            } else {
                let client = api::Client::with_timeout(url, remaining);
                match client.status(token) {
                    Ok(status)
                        if Some(status.device_id.as_str()) == device
                            && status.generation > 0
                            && (!status.connected || status.state == DeviceState::Active) =>
                    {
                        let state = match status.state {
                            DeviceState::Issued => "issued",
                            DeviceState::Active => "active",
                            DeviceState::Revoked => "revoked",
                        };
                        if status.state != DeviceState::Active {
                            attention = true;
                        }
                        finding(
                            if status.state == DeviceState::Active {
                                "ok"
                            } else {
                                "attention"
                            },
                            "device state",
                            state,
                            "if issued, start the configured daemon; if revoked, connect a new device with a fresh pairing code",
                        );
                        finding(
                            if status.connected { "ok" } else { "attention" },
                            "broker accepted WS",
                            if status.connected {
                                "accepted"
                            } else {
                                "none observed"
                            },
                            "alter-bridge daemon status; inspect daemon log; restart daemon if needed",
                        );
                        if !status.connected {
                            attention = true;
                        }
                        if status.state == DeviceState::Active {
                            let remaining = PROBE_BUDGET.saturating_sub(started.elapsed());
                            if remaining.is_zero() {
                                attention = true;
                                finding(
                                    "unknown",
                                    "native registration",
                                    "shared probe deadline exhausted",
                                    "rerun alter-bridge doctor",
                                );
                            } else {
                                match api::Client::with_timeout(url, remaining).sessions(token) {
                                    Ok(sessions) if !sessions.is_empty() => finding(
                                        "ok",
                                        "native registration",
                                        "owned bindings recorded; live provider execution unchecked",
                                        "none",
                                    ),
                                    Ok(_) => {
                                        finding(
                                            "unchecked",
                                            "native registration",
                                            "no explicitly participating contexts; device and MCP remain independent",
                                            "invoke the native abg-connect-context skill only for a chat that should participate",
                                        );
                                    }
                                    Err(error) => {
                                        attention = true;
                                        finding(
                                            "unknown",
                                            "native registration",
                                            &error.public_message(),
                                            "reopen provider and rerun alter-bridge doctor",
                                        );
                                    }
                                }
                            }
                        } else {
                            finding(
                                "unchecked",
                                "native registration",
                                "not queried for inactive credential",
                                "start daemon and reopen provider, then rerun alter-bridge doctor",
                            );
                        }
                    }
                    Ok(_) => {
                        attention = true;
                        finding(
                            "attention",
                            "device state",
                            "broker returned an inconsistent device identity or lifecycle",
                            "inspect private config and broker status; reconnect deliberately only after resolving the conflict",
                        );
                    }
                    Err(api::CallError::Status {
                        status: 401,
                        code: Some(alter_bridge_protocol::ErrorCode::DeviceRevoked),
                    }) => {
                        attention = true;
                        finding(
                            "attention",
                            "device state",
                            "revoked",
                            "alter-bridge connect device <url> --hexid <code> [--id <device>]",
                        );
                    }
                    Err(api::CallError::Status {
                        status: 401,
                        code: Some(alter_bridge_protocol::ErrorCode::Unauthenticated),
                    }) => {
                        attention = true;
                        finding(
                            "attention",
                            "device state",
                            "saved token not authenticated",
                            "inspect private config and broker; reconnect with a new pairing code if the token is invalid",
                        );
                    }
                    Err(api::CallError::Status {
                        code: Some(alter_bridge_protocol::ErrorCode::IdentityConflict),
                        ..
                    }) => {
                        attention = true;
                        finding(
                            "attention",
                            "device state",
                            "device identity conflict",
                            "inspect registered device ownership before reconnecting",
                        );
                    }
                    Err(error) => {
                        attention = true;
                        finding(
                            "unknown",
                            "device state",
                            &error.public_message(),
                            "inspect private config, broker, and daemon log; rerun alter-bridge doctor",
                        );
                    }
                }
            }
        } else {
            finding(
                "unchecked",
                "device state",
                "no saved token",
                "alter-bridge connect device <url> --hexid <code> [--id <device>]",
            );
        }
    } else {
        finding(
            "unchecked",
            "remote checks",
            "validated broker URL unavailable",
            "repair private config and rerun alter-bridge doctor",
        );
    }
    if attention {
        Err(Exit::Failure("doctor found attention items".into()))
    } else {
        Ok(())
    }
}
