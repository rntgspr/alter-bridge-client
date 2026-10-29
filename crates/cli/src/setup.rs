//! Confirmed native provider setup with exact-release assets and guarded user-file merges.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use toml_edit::{DocumentMut, Item, Table, value};
use ureq::tls::{RootCerts, TlsConfig, TlsProvider};

use crate::{Exit, config};

const OWNER: &str = "rntgspr/alter-bridge-client";
const VERSION: &str = env!("CARGO_PKG_VERSION");
const LIMIT: usize = 1024 * 1024;

#[derive(Deserialize)]
struct Manifest {
    version: String,
    files: BTreeMap<String, String>,
}

#[derive(Serialize)]
struct Record {
    version: String,
    tag: String,
    broker: String,
    mcp_entry_sha256: String,
    files: BTreeMap<String, String>,
}

struct Change {
    path: PathBuf,
    previous: Option<Vec<u8>>,
    next: Vec<u8>,
}

/// Returns the stable command path that resolves to the running CLI: the installer's
/// `~/.local/bin/alter-bridge`, else the Homebrew `<prefix>/bin/alter-bridge` above a Cellar keg.
pub(crate) fn stable_bin(home: &Path) -> Option<PathBuf> {
    let running = std::env::current_exe().and_then(fs::canonicalize).ok()?;
    let local = home.join(".local/bin/alter-bridge");

    // Cellar layout: <prefix>/Cellar/alter-bridge/<version>/bin/alter-bridge
    let brew = running
        .ancestors()
        .nth(5)
        .filter(|_| running.ancestors().nth(4).and_then(Path::file_name) == Some("Cellar".as_ref()))
        .map(|prefix| prefix.join("bin/alter-bridge"));

    std::iter::once(local)
        .chain(brew)
        .find(|path| fs::canonicalize(path).ok().as_ref() == Some(&running))
}

/// Returns the real package root bundled beside a Homebrew install, when present.
fn bundled_root(bin: &Path) -> Option<PathBuf> {
    let root = fs::canonicalize(bin.parent()?.join("../share/alter-bridge")).ok()?;
    root.join("native-assets/manifest.json")
        .is_file()
        .then_some(root)
}

/// Accepts only bounded release hooks and safe skill-relative files with one entry per skill.
fn valid_manifest(manifest: &Manifest) -> bool {
    if manifest.files.len() < 3 || manifest.files.len() > 128 {
        return false;
    }
    for hook in [
        "native-assets/claude-hooks.json",
        "native-assets/codex-hooks.json",
    ] {
        if !manifest.files.contains_key(hook) {
            return false;
        }
    }
    let mut skills = std::collections::BTreeSet::new();
    for (path, hash) in &manifest.files {
        if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return false;
        }
        if path.starts_with("native-assets/") {
            if path != "native-assets/claude-hooks.json" && path != "native-assets/codex-hooks.json"
            {
                return false;
            }
            continue;
        }
        let parts: Vec<_> = path.split('/').collect();
        if parts.len() < 3
            || parts[0] != "skills"
            || parts[1].is_empty()
            || parts.iter().any(|part| {
                part.is_empty()
                    || *part == "."
                    || *part == ".."
                    || !part
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
            })
        {
            return false;
        }
        skills.insert(parts[1]);
    }
    !skills.is_empty()
        && skills.into_iter().all(|skill| {
            manifest
                .files
                .contains_key(&format!("skills/{skill}/SKILL.md"))
        })
}

/// Previews one provider installation and performs no download or mutation until confirmed.
pub(crate) fn run(args: &[String], home: &Path) -> Result<(), Exit> {
    let (provider, offline_root) = match args {
        [provider] => (provider, None),
        [provider, flag, root] if flag == "--assets" => (provider, Some(PathBuf::from(root))),
        _ => {
            return Err(Exit::Usage(
                "usage: alter-bridge config codex|claude|opencode [--assets /absolute/package-root]\n"
                    .into(),
            ));
        }
    };
    if provider == "opencode" && offline_root.is_none() {
        return crate::opencode::configure(home);
    }
    if provider != "codex" && provider != "claude" {
        return Err(Exit::Usage(
            "usage: alter-bridge config codex|claude|opencode [--assets /absolute/package-root]\n"
                .into(),
        ));
    }

    let preferences = config::preferences(home).map_err(Exit::Failure)?;
    let broker = preferences.url.as_deref().ok_or_else(|| {
        Exit::Failure("save a broker URL with connect before configuring a provider".into())
    })?;
    if broker.contains(['\n', '\r']) {
        return Err(Exit::Failure(
            "broker URL cannot be quoted safely in provider commands".into(),
        ));
    }
    let codex_root = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".codex"));
    let claude_root = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".claude"));
    let skill_root = if provider == "codex" {
        home.join(".agents")
    } else {
        claude_root.clone()
    };
    let (hooks, mcp) = if provider == "codex" {
        (
            codex_root.join("hooks.json"),
            codex_root.join("config.toml"),
        )
    } else {
        (
            claude_root.join("settings.json"),
            if std::env::var_os("CLAUDE_CONFIG_DIR").is_some() {
                claude_root.join(".claude.json")
            } else {
                home.join(".claude.json")
            },
        )
    };
    if provider == "codex" {
        for requirements in [
            codex_root.join("requirements.toml"),
            PathBuf::from("/etc/codex/requirements.toml"),
        ] {
            let bytes = if requirements.starts_with("/etc/codex") {
                read_system_policy(&requirements)
            } else {
                read_existing(&requirements)
            }
            .map_err(Exit::Failure)?;
            if let Some(bytes) = bytes {
                let document: DocumentMut = std::str::from_utf8(&bytes)
                    .map_err(|_| Exit::Failure("invalid Codex requirements UTF-8".into()))?
                    .parse()
                    .map_err(|_| Exit::Failure("invalid Codex requirements TOML".into()))?;
                if document
                    .get("allow_managed_hooks_only")
                    .and_then(Item::as_value)
                    .and_then(|value| value.as_bool())
                    == Some(true)
                    || document
                        .get("features")
                        .and_then(|features| features.get("hooks"))
                        .and_then(Item::as_value)
                        .and_then(|value| value.as_bool())
                        == Some(false)
                {
                    return Err(Exit::Failure("Codex managed policy blocks user hooks; request a managed installation instead".into()));
                }
            }
        }
        if let Some(bytes) = read_existing(&mcp).map_err(Exit::Failure)? {
            let document: DocumentMut = std::str::from_utf8(&bytes)
                .map_err(|_| Exit::Failure("invalid Codex config UTF-8".into()))?
                .parse()
                .map_err(|_| Exit::Failure("invalid Codex config TOML".into()))?;
            if document
                .get("features")
                .and_then(|features| features.get("hooks"))
                .and_then(Item::as_value)
                .and_then(|value| value.as_bool())
                == Some(false)
            {
                return Err(Exit::Failure(
                    "Codex user config disables hooks; enable them before native setup".into(),
                ));
            }
        }
    } else if let Some(bytes) = read_existing(&hooks).map_err(Exit::Failure)? {
        let settings: Value = serde_json::from_slice(&bytes)
            .map_err(|_| Exit::Failure("invalid Claude settings JSON".into()))?;
        if settings.get("disableAllHooks").and_then(Value::as_bool) == Some(true)
            || settings
                .get("allowManagedHooksOnly")
                .and_then(Value::as_bool)
                == Some(true)
        {
            return Err(Exit::Failure(
                "Claude settings block user hooks; update policy before native setup".into(),
            ));
        }
    }
    if provider == "claude" {
        for path in [
            PathBuf::from("/Library/Application Support/ClaudeCode/managed-settings.json"),
            PathBuf::from("/etc/claude-code/managed-settings.json"),
        ] {
            if let Some(bytes) = read_system_policy(&path).map_err(Exit::Failure)? {
                let settings: Value = serde_json::from_slice(&bytes)
                    .map_err(|_| Exit::Failure("invalid Claude managed settings JSON".into()))?;
                if settings.get("disableAllHooks").and_then(Value::as_bool) == Some(true)
                    || settings
                        .get("allowManagedHooksOnly")
                        .and_then(Value::as_bool)
                        == Some(true)
                {
                    return Err(Exit::Failure(
                        "Claude managed settings block user hooks; request a managed installation"
                            .into(),
                    ));
                }
            }
        }
    }
    let tag = format!("v{VERSION}");
    let bin = stable_bin(home).ok_or_else(|| {
        Exit::Failure(
            "native setup requires the running CLI at ~/.local/bin/alter-bridge or a Homebrew <prefix>/bin/alter-bridge; install this CLI there and run config through that path before configuring hooks".into(),
        )
    })?;
    let bin_text = bin
        .to_str()
        .ok_or_else(|| Exit::Failure("binary path is not UTF-8".into()))?;
    if bin_text.contains(['\n', '\r']) {
        return Err(Exit::Failure(
            "binary path cannot be quoted safely in provider commands".into(),
        ));
    }
    let offline_root = offline_root.or_else(|| bundled_root(&bin));

    let plugin = old_plugin(provider, &mcp, &hooks).map_err(Exit::Failure)?;
    if let Some(root) = &offline_root {
        if !root.is_absolute() {
            return Err(Exit::Usage(
                "--assets requires an absolute package root\n".into(),
            ));
        }
        println!(
            "Source: {} (offline package; exact CLI version)",
            root.display()
        );
    } else {
        println!("Source: https://github.com/{OWNER}/tree/{tag} (exact release tag)");
    }
    println!("Provider: {provider}; broker: {broker}/mcp");
    println!(
        "Skills: {}\nHooks: {}\nMCP: {}",
        skill_root.join("skills").display(),
        hooks.display(),
        mcp.display()
    );
    println!(
        "Managed changes: version-matched skills, SessionStart/UserPromptSubmit hooks, alter-bridge HTTP MCP entry and private setup record"
    );
    if plugin {
        return Err(Exit::Failure(format!(
            "enabled old alter-bridge plugin detected; disable its entry in {} before native setup, then run this command again",
            if provider == "claude" {
                "Claude enabledPlugins"
            } else {
                "Codex plugin config"
            }
        )));
    }
    print!("Proceed? [y/N] ");
    io::stdout()
        .flush()
        .map_err(|_| Exit::Failure("cannot print confirmation".into()))?;
    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .map_err(|_| Exit::Failure("cannot read confirmation".into()))?;
    if !answer.trim().eq_ignore_ascii_case("y") {
        println!("declined; no assets downloaded or changed");
        return Ok(());
    }

    let prior_setup = preferences
        .setup
        .as_ref()
        .and_then(|setup| setup.get(provider));
    let asset = |path: &str| -> Result<Vec<u8>, String> {
        match &offline_root {
            Some(root) => read_bundled(root, path),
            None => fetch(&tag, path),
        }
    };
    let manifest_bytes = asset("native-assets/manifest.json").map_err(Exit::Failure)?;
    let manifest: Manifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|_| Exit::Failure("release asset manifest is invalid".into()))?;
    if manifest.version != VERSION {
        return Err(Exit::Failure(
            "release asset manifest version does not match the CLI".into(),
        ));
    }
    if !valid_manifest(&manifest) {
        return Err(Exit::Failure(
            "release asset manifest has an unexpected file set".into(),
        ));
    }
    let mut assets = BTreeMap::new();
    for name in manifest.files.keys() {
        let bytes = asset(name).map_err(Exit::Failure)?;
        if digest(&bytes) != manifest.files[name] {
            return Err(Exit::Failure(format!(
                "release asset digest mismatch: {name}"
            )));
        }
        assets.insert(name.clone(), bytes);
    }

    let previous_record = preferences
        .setup
        .as_ref()
        .and_then(|setup| setup.get(provider));
    let recorded = previous_record
        .and_then(|record| record.get("files"))
        .and_then(Value::as_object);
    let previous_broker = previous_record
        .and_then(|record| record.get("broker"))
        .and_then(Value::as_str);
    if let Some(old) = previous_broker {
        let expected = format!(
            "{} mcp headers --url {}",
            shell_quote(bin_text),
            shell_quote(old)
        );
        let hash = entry_digest(provider, old, &expected);
        if previous_record
            .and_then(|record| record.get("mcp_entry_sha256"))
            .and_then(Value::as_str)
            != Some(hash.as_str())
        {
            return Err(Exit::Failure(
                "managed MCP ownership record is incomplete or changed".into(),
            ));
        }
    }
    let command = |register: bool| {
        format!(
            "{} hook {}{}",
            shell_quote(bin_text),
            provider,
            if register { " --register-only" } else { "" }
        )
    };
    let hook_asset = if provider == "codex" {
        "native-assets/codex-hooks.json"
    } else {
        "native-assets/claude-hooks.json"
    };
    let template: Value = serde_json::from_slice(&assets[hook_asset])
        .map_err(|_| Exit::Failure("release hook template is invalid".into()))?;
    let old_hooks = read_existing(&hooks).map_err(Exit::Failure)?;
    let hooks_next =
        merge_hooks(old_hooks.as_deref(), &template, &command).map_err(Exit::Failure)?;
    let helper = format!(
        "{} mcp headers --url {}",
        shell_quote(bin_text),
        shell_quote(broker)
    );
    let old_mcp = read_existing(&mcp).map_err(Exit::Failure)?;
    let unowned_entry = old_mcp.as_deref().is_some_and(|bytes| {
        if provider == "claude" {
            serde_json::from_slice::<Value>(bytes)
                .ok()
                .and_then(|root| {
                    root.get("mcpServers")
                        .and_then(|servers| servers.get("alter-bridge"))
                        .cloned()
                })
                .is_some()
        } else {
            std::str::from_utf8(bytes)
                .ok()
                .and_then(|text| text.parse::<DocumentMut>().ok())
                .is_some_and(|doc| {
                    doc.get("mcp_servers")
                        .and_then(|servers| servers.get("alter-bridge"))
                        .is_some()
                })
        }
    });
    let previous_entry = previous_broker
        .or_else(|| (offline_root.is_some() && unowned_entry).then_some(broker))
        .map(|old| {
            (
                format!("{old}/mcp"),
                format!(
                    "{} mcp headers --url {}",
                    shell_quote(bin_text),
                    shell_quote(old)
                ),
            )
        });
    let mcp_next = if provider == "claude" {
        merge_claude_mcp(old_mcp.as_deref(), broker, &helper, previous_entry.as_ref())
            .map_err(Exit::Failure)?
    } else {
        merge_codex_mcp(old_mcp.as_deref(), broker, &helper, previous_entry.as_ref())
            .map_err(Exit::Failure)?
    };

    let mut changes = vec![
        Change {
            path: hooks.clone(),
            previous: old_hooks,
            next: hooks_next,
        },
        Change {
            path: mcp.clone(),
            previous: old_mcp,
            next: mcp_next,
        },
    ];
    for (source, next) in assets {
        if !source.starts_with("skills/") {
            continue;
        }
        let path = skill_root.join(&source);
        let previous = read_existing(&path).map_err(Exit::Failure)?;
        if let Some(old) = &previous {
            if old != &next
                && recorded
                    .and_then(|files| files.get(path.to_str().unwrap_or_default()))
                    .and_then(Value::as_str)
                    != Some(digest(old).as_str())
            {
                return Err(Exit::Failure(format!(
                    "{} has unowned edits; refusing replacement",
                    path.display()
                )));
            }
            if prior_setup.is_none() && (offline_root.is_none() || old != &next) {
                return Err(Exit::Failure(format!(
                    "{} exists without setup ownership; use matching offline assets for previewed reconciliation",
                    path.display()
                )));
            }
        }
        changes.push(Change {
            path,
            previous,
            next,
        });
    }
    let mut obsolete_commands = Vec::new();
    if provider == "claude" {
        if let Some(files) = recorded {
            for (path, expected) in files {
                let path = PathBuf::from(path);
                if path.parent() != Some(claude_root.join("commands").as_path())
                    || path.extension().is_none_or(|extension| extension != "md")
                {
                    continue;
                }
                let Some(bytes) = read_existing(&path).map_err(Exit::Failure)? else {
                    continue;
                };
                if expected.as_str() != Some(digest(&bytes).as_str()) {
                    return Err(Exit::Failure(format!(
                        "{} has modified owned content; refusing removal",
                        path.display()
                    )));
                }
                obsolete_commands.push((path, bytes));
            }
        }
    }
    let mut transaction = config::Transaction::open(home).map_err(Exit::Failure)?;
    if transaction.file.url.as_deref() != Some(broker)
        || transaction.file.setup != preferences.setup
    {
        return Err(Exit::Failure(
            "private config changed during setup; retry".into(),
        ));
    }
    for change in &changes {
        if read_existing(&change.path).map_err(Exit::Failure)? != change.previous {
            return Err(Exit::Failure(format!(
                "{} changed during setup; retry",
                change.path.display()
            )));
        }
    }
    for (path, bytes) in &obsolete_commands {
        if read_existing(path).map_err(Exit::Failure)?.as_deref() != Some(bytes.as_slice()) {
            return Err(Exit::Failure(format!(
                "{} changed during setup; retry",
                path.display()
            )));
        }
    }
    let mut files = BTreeMap::new();
    for change in &changes {
        files.insert(
            change.path.to_string_lossy().into_owned(),
            digest(&change.next),
        );
    }
    let mut setup = transaction.file.setup.clone().unwrap_or_else(|| json!({}));
    let map = setup
        .as_object_mut()
        .ok_or_else(|| Exit::Failure("private setup metadata is invalid".into()))?;
    map.insert(
        provider.clone(),
        serde_json::to_value(Record {
            version: VERSION.into(),
            tag,
            broker: broker.into(),
            mcp_entry_sha256: entry_digest(provider, broker, &helper),
            files,
        })
        .unwrap(),
    );
    let mut changed = Vec::new();
    for change in &changes {
        if change.previous.as_deref() != Some(&change.next) {
            write_guarded(change).map_err(|error| {
                Exit::Failure(format!(
                    "partial setup; changed {} before failure at {}: {error}",
                    changed.join(", "),
                    change.path.display()
                ))
            })?;
            changed.push(change.path.display().to_string());
        }
    }
    for (path, _) in &obsolete_commands {
        fs::remove_file(path).map_err(|_| {
            Exit::Failure(format!(
                "partial setup; cannot remove owned obsolete command {}",
                path.display()
            ))
        })?;
    }
    transaction.file.setup = Some(setup);
    transaction.save().map_err(|error| {
        Exit::Failure(format!(
            "provider files changed, but private metadata save failed: {}",
            error.message
        ))
    })?;
    println!(
        "native {provider} setup installed; reopen {provider} and review hooks with /hooks where supported"
    );
    Ok(())
}

/// Returns SHA-256 for manifest and install metadata comparison.
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Quotes one public path or URL as a POSIX shell argument for provider command fields.
pub(crate) fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

/// Hashes only the bridge-owned MCP fields, independent of unrelated provider settings.
fn entry_digest(provider: &str, broker: &str, helper: &str) -> String {
    let entry = if provider == "claude" {
        json!({"type":"http","url":format!("{broker}/mcp"),"headersHelper":helper})
    } else {
        json!({"url":format!("{broker}/mcp"),"http_headers_helper":helper})
    };
    digest(&serde_json::to_vec(&entry).expect("fixed MCP entry serializes"))
}

/// Confirms the installed provider entry still belongs to the saved broker before launching it.
pub(crate) fn verify_managed_entry(provider: &str, home: &Path) -> Result<(), String> {
    if provider == "opencode" {
        return if crate::opencode::enabled(home) {
            Ok(())
        } else {
            Err("OpenCode integration is not recorded for the saved broker; run alter-bridge config opencode".into())
        };
    }
    let preferences = config::preferences(home)?;
    let broker = preferences.url.ok_or("no saved broker URL")?;
    let record = preferences
        .setup
        .as_ref()
        .and_then(|setup| setup.get(provider))
        .ok_or("provider native setup is not installed; run alter-bridge config first")?;
    if record.get("broker").and_then(Value::as_str) != Some(&broker) {
        return Err(
            "provider MCP endpoint needs refresh; rerun connect or config and reopen the provider"
                .into(),
        );
    }
    let bin = stable_bin(home).ok_or("stable alter-bridge binary path not found")?;
    let bin = bin.to_str().ok_or("binary path is not UTF-8")?;
    let helper = format!(
        "{} mcp headers --url {}",
        shell_quote(bin),
        shell_quote(&broker)
    );
    if record.get("mcp_entry_sha256").and_then(Value::as_str)
        != Some(entry_digest(provider, &broker, &helper).as_str())
    {
        return Err("provider MCP ownership record differs".into());
    }
    let path = if provider == "codex" {
        std::env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".codex"))
            .join("config.toml")
    } else {
        std::env::var_os("CLAUDE_CONFIG_DIR")
            .map(PathBuf::from)
            .map(|root| root.join(".claude.json"))
            .unwrap_or_else(|| home.join(".claude.json"))
    };
    let original = read_existing(&path)?.ok_or("provider MCP config is missing")?;
    let has_entry = if provider == "codex" {
        std::str::from_utf8(&original)
            .ok()
            .and_then(|text| text.parse::<DocumentMut>().ok())
            .is_some_and(|doc| {
                doc.get("mcp_servers")
                    .and_then(|servers| servers.get("alter-bridge"))
                    .is_some()
            })
    } else {
        serde_json::from_slice::<Value>(&original)
            .ok()
            .is_some_and(|root| {
                root.get("mcpServers")
                    .and_then(|servers| servers.get("alter-bridge"))
                    .is_some()
            })
    };
    if !has_entry {
        return Err("owned alter-bridge MCP entry is missing; run config for previewed repair and reopen the provider".into());
    }
    let previous = (format!("{broker}/mcp"), helper.clone());
    if provider == "codex" {
        merge_codex_mcp(Some(&original), &broker, &helper, Some(&previous))?;
    } else {
        merge_claude_mcp(Some(&original), &broker, &helper, Some(&previous))?;
    }
    Ok(())
}

/// Refreshes one previously owned MCP URL after a broker change, without taking the config lock.
pub(crate) fn refresh_managed_url(
    provider: &str,
    home: &Path,
    setup: &mut Value,
    broker: &str,
) -> Result<bool, String> {
    if provider != "codex" && provider != "claude" {
        return Err("unknown provider".into());
    }
    let root = if provider == "codex" {
        std::env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".codex"))
    } else {
        std::env::var_os("CLAUDE_CONFIG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".claude"))
    };
    let path = if provider == "codex" {
        root.join("config.toml")
    } else if std::env::var_os("CLAUDE_CONFIG_DIR").is_some() {
        root.join(".claude.json")
    } else {
        home.join(".claude.json")
    };
    let record = setup
        .get_mut(provider)
        .and_then(Value::as_object_mut)
        .ok_or("provider setup record is absent")?;
    if !record
        .get("files")
        .and_then(Value::as_object)
        .is_some_and(|files| files.contains_key(path.to_string_lossy().as_ref()))
    {
        return Err("provider config root differs from the recorded installation; rerun config for this root".into());
    }
    let previous = record
        .get("broker")
        .and_then(Value::as_str)
        .ok_or("provider setup broker is absent")?
        .to_owned();
    let bin = stable_bin(home).ok_or("stable alter-bridge binary path not found")?;
    let bin = bin.to_str().ok_or("binary path is not UTF-8")?;
    let old_helper = format!(
        "{} mcp headers --url {}",
        shell_quote(bin),
        shell_quote(&previous)
    );
    if record.get("mcp_entry_sha256").and_then(Value::as_str)
        != Some(entry_digest(provider, &previous, &old_helper).as_str())
    {
        return Err("provider MCP ownership record differs".into());
    }
    let helper = format!(
        "{} mcp headers --url {}",
        shell_quote(bin),
        shell_quote(broker)
    );
    let old_entry = (format!("{previous}/mcp"), old_helper);
    let original = read_existing(&path)?;
    if original.is_none() {
        return Err("managed provider MCP file is missing; rerun config".into());
    }
    let next = if provider == "codex" {
        merge_codex_mcp(original.as_deref(), broker, &helper, Some(&old_entry))?
    } else {
        merge_claude_mcp(original.as_deref(), broker, &helper, Some(&old_entry))?
    };
    if original.as_deref() != Some(&next) {
        write_guarded(&Change {
            path: path.clone(),
            previous: original,
            next: next.clone(),
        })?;
    }
    record.insert("broker".into(), Value::String(broker.into()));
    record.insert(
        "mcp_entry_sha256".into(),
        Value::String(entry_digest(provider, broker, &helper)),
    );
    record
        .get_mut("files")
        .and_then(Value::as_object_mut)
        .ok_or("provider setup file record is absent")?
        .insert(
            path.to_string_lossy().into_owned(),
            Value::String(digest(&next)),
        );
    Ok(true)
}

/// Downloads one bounded file from the CLI's exact GitHub tag without fallback.
fn fetch(tag: &str, path: &str) -> Result<Vec<u8>, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .tls_config(
            TlsConfig::builder()
                .provider(TlsProvider::NativeTls)
                .root_certs(RootCerts::PlatformVerifier)
                .build(),
        )
        .timeout_global(Some(Duration::from_secs(15)))
        .http_status_as_error(false)
        .max_redirects(0)
        .build()
        .into();
    let token = std::env::var("GH_TOKEN")
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            std::env::var("GITHUB_TOKEN")
                .ok()
                .filter(|value| !value.is_empty())
        });
    let url = if token.is_some() {
        format!("https://api.github.com/repos/{OWNER}/contents/{path}?ref={tag}")
    } else {
        format!("https://raw.githubusercontent.com/{OWNER}/refs/tags/{tag}/{path}")
    };
    let request = agent
        .get(&url)
        .header("Accept", "application/vnd.github.raw+json")
        .header("User-Agent", "alter-bridge-native-setup");
    let request = if let Some(token) = &token {
        request.header("Authorization", format!("Bearer {token}"))
    } else {
        request
    };
    let mut response = match request.call() {
        Ok(response) => response,
        Err(_) if token.is_none() => {
            return gh_fetch(tag, path);
        }
        Err(_) => {
            return Err(format!(
                "cannot fetch {path} from {OWNER} tag {tag}; check tag and GitHub authentication"
            ));
        }
    };
    if response.status() != 200 {
        if token.is_none() {
            return gh_fetch(tag, path);
        }
        return Err(format!(
            "GitHub refused {path} at exact tag {tag} (HTTP {}); no fallback was used",
            response.status()
        ));
    }
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take((LIMIT + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| "cannot read release asset".to_string())?;
    if bytes.len() > LIMIT {
        return Err("release asset exceeds 1 MiB".into());
    }
    Ok(bytes)
}

/// Reads a safe manifest-listed asset from a real, bounded offline package tree.
fn read_bundled(root: &Path, path: &str) -> Result<Vec<u8>, String> {
    if !root.is_absolute()
        || root.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
        || (path != "native-assets/manifest.json" && !valid_manifest_path(path))
    {
        return Err("offline asset path is not allowed".into());
    }
    for ancestor in root
        .ancestors()
        .filter(|ancestor| ancestor != &Path::new("/"))
    {
        let metadata = fs::symlink_metadata(ancestor)
            .map_err(|_| "offline package root is unavailable".to_string())?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err("offline package path must use real directories".into());
        }
    }
    let mut current = root.to_path_buf();
    for component in Path::new(path).components() {
        current.push(component);
        let metadata =
            fs::symlink_metadata(&current).map_err(|_| format!("offline asset missing: {path}"))?;
        if metadata.file_type().is_symlink() {
            return Err(format!("offline asset cannot be a symlink: {path}"));
        }
        if current != root.join(path) && !metadata.is_dir() {
            return Err(format!("offline asset parent is not a directory: {path}"));
        }
    }
    let metadata =
        fs::symlink_metadata(&current).map_err(|_| format!("offline asset missing: {path}"))?;
    if !metadata.is_file() || metadata.len() > LIMIT as u64 {
        return Err(format!(
            "offline asset must be a regular file under 1 MiB: {path}"
        ));
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&current)
        .map_err(|_| format!("cannot open offline asset: {path}"))?;
    let opened = file
        .metadata()
        .map_err(|_| format!("cannot inspect offline asset: {path}"))?;
    if !opened.is_file() || opened.len() > LIMIT as u64 {
        return Err(format!("offline asset changed: {path}"));
    }
    let mut bytes = Vec::new();
    file.take((LIMIT + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| format!("cannot read offline asset: {path}"))?;
    if bytes.len() > LIMIT {
        return Err(format!("offline asset exceeds 1 MiB: {path}"));
    }
    Ok(bytes)
}

/// Rejects traversal, absolute paths, and unexpected asset namespaces before I/O.
fn valid_manifest_path(path: &str) -> bool {
    if matches!(
        path,
        "native-assets/claude-hooks.json" | "native-assets/codex-hooks.json"
    ) {
        return true;
    }
    let parts: Vec<_> = path.split('/').collect();
    parts.len() >= 3
        && parts[0] == "skills"
        && parts.iter().all(|part| {
            !part.is_empty()
                && *part != "."
                && *part != ".."
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
        })
}

/// Uses the existing authenticated gh session without exposing a token in arguments or diagnostics.
fn gh_fetch(tag: &str, path: &str) -> Result<Vec<u8>, String> {
    let endpoint = format!("repos/{OWNER}/contents/{path}?ref={tag}");
    let mut child = Command::new("gh")
        .args(["api", &endpoint, "--hostname", "github.com", "-H", "Accept: application/vnd.github.raw+json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| format!("cannot fetch {path} at exact tag {tag}; authenticate with GH_TOKEN, GITHUB_TOKEN, or gh auth login"))?;
    let stdout = child.stdout.take().ok_or("gh stdout unavailable")?;
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout
            .take((LIMIT + 1) as u64)
            .read_to_end(&mut bytes)
            .map(|_| bytes);
        sender.send(result).ok();
    });
    let deadline = Instant::now() + Duration::from_secs(15);
    let bytes = match receiver.recv_timeout(Duration::from_secs(15)) {
        Ok(Ok(bytes)) => bytes,
        _ => {
            child.kill().ok();
            child.wait().ok();
            return Err(format!(
                "gh timed out or failed while fetching {path} at exact tag {tag}"
            ));
        }
    };
    if bytes.len() > LIMIT {
        child.kill().ok();
        child.wait().ok();
        return Err("release asset exceeds 1 MiB".into());
    }
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|_| "cannot inspect gh exit status")?
        {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().ok();
            child.wait().ok();
            return Err(format!(
                "gh timed out while fetching {path} at exact tag {tag}"
            ));
        }
        thread::sleep(Duration::from_millis(10));
    };
    if !status.success() {
        return Err(format!(
            "gh could not fetch {path} at exact tag {tag}; check repository access and release tag"
        ));
    }
    Ok(bytes)
}

/// Reads an existing regular file without following symlinks or accepting oversized data.
fn read_existing(path: &Path) -> Result<Option<Vec<u8>>, String> {
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(format!("cannot safely open {}", path.display())),
    };
    let metadata = file
        .metadata()
        .map_err(|_| format!("cannot inspect {}", path.display()))?;
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    if !metadata.is_file()
        || metadata.len() > LIMIT as u64
        || metadata.uid() != uid
        || metadata.nlink() != 1
    {
        return Err(format!(
            "{} must be a small owned regular file",
            path.display()
        ));
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take((LIMIT + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| format!("cannot read {}", path.display()))?;
    let named = fs::symlink_metadata(path)
        .map_err(|_| format!("{} changed while being read", path.display()))?;
    if bytes.len() > LIMIT
        || named.dev() != metadata.dev()
        || named.ino() != metadata.ino()
        || named.len() != metadata.len()
        || named.mtime() != metadata.mtime()
        || named.mtime_nsec() != metadata.mtime_nsec()
    {
        return Err(format!("{} changed while being read", path.display()));
    }
    Ok(Some(bytes))
}

/// Reads a root-owned managed policy as a bounded regular file without following a link.
fn read_system_policy(path: &Path) -> Result<Option<Vec<u8>>, String> {
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("cannot safely read managed policy".into()),
    };
    let metadata = file
        .metadata()
        .map_err(|_| "cannot inspect managed policy")?;
    if !metadata.is_file() || metadata.uid() != 0 || metadata.len() > LIMIT as u64 {
        return Err("managed policy must be a small root-owned regular file".into());
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take((LIMIT + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| "cannot read managed policy")?;
    if bytes.len() > LIMIT {
        return Err("managed policy exceeds size limit".into());
    }
    Ok(Some(bytes))
}

/// Refuses symlinked or non-directory ancestors before a provider destination is created.
fn safe_parent(parent: &Path) -> Result<(), String> {
    for ancestor in parent.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => return Err(format!("{} is not a real directory", ancestor.display())),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => return Err(format!("cannot inspect {}", ancestor.display())),
        }
    }
    Ok(())
}

/// Distinguishes inline hook definitions from Codex's persisted hook trust state.
pub(crate) fn has_inline_codex_hooks(document: &DocumentMut) -> bool {
    document.get("hooks").is_some_and(|hooks| {
        hooks.as_table_like().is_none_or(|table| {
            table
                .iter()
                .any(|(name, value)| name != "state" || value.as_table_like().is_none())
        })
    })
}

/// Finds enabled legacy plugin declarations without changing provider policy.
fn old_plugin(provider: &str, mcp: &Path, hooks: &Path) -> Result<bool, String> {
    let bytes = read_existing(hooks)?;
    if provider == "claude" {
        let settings: Value = bytes
            .as_deref()
            .map(serde_json::from_slice)
            .transpose()
            .map_err(|_| "invalid Claude settings JSON".to_string())?
            .unwrap_or_else(|| json!({}));
        Ok(settings
            .get("enabledPlugins")
            .and_then(Value::as_object)
            .is_some_and(|entries| {
                entries
                    .iter()
                    .any(|(name, enabled)| name.contains("alter-bridge") && enabled == true)
            }))
    } else {
        let bytes = read_existing(mcp)?;
        let doc: DocumentMut = bytes
            .as_deref()
            .map(|value| {
                std::str::from_utf8(value)
                    .map_err(|_| "invalid Codex config UTF-8".to_string())?
                    .parse()
                    .map_err(|_| "invalid Codex config TOML".to_string())
            })
            .transpose()?
            .unwrap_or_default();
        if has_inline_codex_hooks(&doc) {
            return Err("Codex config.toml already declares inline hooks; move them to hooks.json before native setup to keep one representation per user layer".into());
        }
        Ok(doc
            .get("plugins")
            .and_then(Item::as_table_like)
            .is_some_and(|entries| {
                entries.iter().any(|(name, item)| {
                    name.contains("alter-bridge")
                        && item
                            .get("enabled")
                            .and_then(Item::as_value)
                            .and_then(|value| value.as_bool())
                            .unwrap_or(true)
                })
            }))
    }
}

/// Adds exactly one owned command to each lifecycle event and preserves other handlers.
fn merge_hooks(
    existing: Option<&[u8]>,
    template: &Value,
    command: &dyn Fn(bool) -> String,
) -> Result<Vec<u8>, String> {
    let mut root: Value = existing
        .map(serde_json::from_slice)
        .transpose()
        .map_err(|_| "invalid provider hooks JSON".to_string())?
        .unwrap_or_else(|| json!({}));
    let hooks = root
        .as_object_mut()
        .ok_or("provider hooks root must be an object")?
        .entry("hooks")
        .or_insert_with(|| json!({}));
    let hooks = hooks
        .as_object_mut()
        .ok_or("provider hooks must be an object")?;
    for (event, register) in [("SessionStart", true), ("UserPromptSubmit", false)] {
        let template_group = template
            .get("hooks")
            .and_then(|hooks| hooks.get(event))
            .and_then(Value::as_array)
            .and_then(|groups| groups.first())
            .cloned()
            .ok_or("release hook template misses lifecycle event")?;
        let array = hooks
            .entry(event)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or("lifecycle hook must be an array")?;
        let mut owned = template_group;
        owned["hooks"][0]["command"] = Value::String(command(register));
        let matching: Vec<usize> = array
            .iter()
            .enumerate()
            .filter_map(|(index, group)| {
                group
                    .get("hooks")
                    .and_then(Value::as_array)
                    .is_some_and(|handlers| {
                        handlers.iter().any(|handler| {
                            handler
                                .get("command")
                                .and_then(Value::as_str)
                                .is_some_and(|text| {
                                    text.contains("alter-bridge") && text.contains(" hook ")
                                })
                        })
                    })
                    .then_some(index)
            })
            .collect();
        if matching.len() > 1 {
            return Err(format!(
                "multiple existing bridge hooks for {event}; reconcile manually"
            ));
        }
        if let Some(index) = matching.first() {
            if array[*index] != owned {
                return Err(format!(
                    "existing bridge hook for {event} differs from owned definition; reconcile manually"
                ));
            }
        } else {
            array.push(owned);
        }
    }
    serde_json::to_vec_pretty(&root).map_err(|_| "cannot serialize provider hooks".to_string())
}

/// Checks the installed owned hook groups against the bundled release template without writes.
pub(crate) fn verify_hooks(provider: &str, home: &Path, existing: &[u8]) -> bool {
    let template = if provider == "codex" {
        include_bytes!("../../../native-assets/codex-hooks.json").as_slice()
    } else {
        include_bytes!("../../../native-assets/claude-hooks.json").as_slice()
    };
    let Ok(template) = serde_json::from_slice::<Value>(template) else {
        return false;
    };
    let Some(bin) = stable_bin(home)
        .as_deref()
        .and_then(Path::to_str)
        .map(shell_quote)
    else {
        return false;
    };
    let command = |register: bool| {
        format!(
            "{bin} hook {provider}{}",
            if register { " --register-only" } else { "" }
        )
    };
    let Ok(merged) = merge_hooks(Some(existing), &template, &command) else {
        return false;
    };
    let original: Result<Value, _> = serde_json::from_slice(existing);
    let result: Result<Value, _> = serde_json::from_slice(&merged);
    matches!((original, result), (Ok(before), Ok(after)) if before == after)
}

/// Merges the named Claude user MCP entry while retaining other servers and keys.
fn merge_claude_mcp(
    existing: Option<&[u8]>,
    broker: &str,
    helper: &str,
    previous: Option<&(String, String)>,
) -> Result<Vec<u8>, String> {
    let mut root: Value = existing
        .map(serde_json::from_slice)
        .transpose()
        .map_err(|_| "invalid Claude user config JSON".to_string())?
        .unwrap_or_else(|| json!({}));
    let servers = root
        .as_object_mut()
        .ok_or("Claude user config must be an object")?
        .entry("mcpServers")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("Claude mcpServers must be an object")?;
    if let Some(current) = servers.get("alter-bridge") {
        let Some((old_url, old_helper)) = previous else {
            return Err("Claude alter-bridge MCP entry exists without setup ownership".into());
        };
        if current != &json!({"type":"http","url":old_url,"headersHelper":old_helper})
            && current
                != &json!({"type":"http","url":format!("{broker}/mcp"),"headersHelper":helper})
        {
            return Err("Claude alter-bridge MCP entry has unowned edits".into());
        }
    }
    servers.insert(
        "alter-bridge".into(),
        json!({"type":"http","url":format!("{broker}/mcp"),"headersHelper":helper}),
    );
    serde_json::to_vec_pretty(&root).map_err(|_| "cannot serialize Claude user config".to_string())
}

/// Merges the named Codex HTTP MCP table without changing unrelated TOML tables.
fn merge_codex_mcp(
    existing: Option<&[u8]>,
    broker: &str,
    helper: &str,
    previous: Option<&(String, String)>,
) -> Result<Vec<u8>, String> {
    let mut doc: DocumentMut = existing
        .map(|bytes| {
            std::str::from_utf8(bytes)
                .map_err(|_| "invalid Codex config UTF-8".to_string())?
                .parse()
                .map_err(|_| "invalid Codex config TOML".to_string())
        })
        .transpose()?
        .unwrap_or_default();
    if !doc.as_table().contains_key("mcp_servers") {
        if previous.is_some() {
            return Err("managed Codex MCP table is missing; rerun config".into());
        }
        doc["mcp_servers"] = Item::Table(Table::new());
    }
    if !doc["mcp_servers"].is_table_like() {
        return Err("Codex mcp_servers must be a table".into());
    }
    if let Some(current) = doc["mcp_servers"].get("alter-bridge") {
        let Some((old_url, old_helper)) = previous else {
            return Err("Codex alter-bridge MCP entry exists without setup ownership".into());
        };
        let current_url = current
            .get("url")
            .and_then(Item::as_value)
            .and_then(|value| value.as_str());
        let current_helper = current
            .get("http_headers_helper")
            .and_then(Item::as_value)
            .and_then(|value| value.as_str());
        let matches_previous = current_url == Some(old_url) && current_helper == Some(old_helper);
        let next_url = format!("{broker}/mcp");
        let matches_next = current_url == Some(next_url.as_str()) && current_helper == Some(helper);

        if (!matches_previous && !matches_next)
            || current.as_table_like().is_none_or(|table| table.len() != 2)
        {
            return Err("Codex alter-bridge MCP entry has unowned edits".into());
        }
    }
    let mut table = Table::new();
    table["url"] = value(format!("{broker}/mcp"));
    table["http_headers_helper"] = value(helper);
    doc["mcp_servers"]["alter-bridge"] = if doc["mcp_servers"].is_inline_table() {
        Item::Value(toml_edit::Value::InlineTable(table.into_inline_table()))
    } else {
        Item::Table(table)
    };
    Ok(doc.to_string().into_bytes())
}

/// Writes one guarded provider file with a private temporary, backup and atomic rename.
fn write_guarded(change: &Change) -> Result<(), String> {
    let parent = change
        .path
        .parent()
        .ok_or("provider destination has no parent")?;
    safe_parent(parent)?;
    fs::create_dir_all(parent).map_err(|_| "cannot create provider directory".to_string())?;
    safe_parent(parent)?;
    if read_existing(&change.path)? != change.previous {
        return Err("destination changed during setup".into());
    }
    let mode = 0o600;
    if let Some(old) = &change.previous {
        let backup = change
            .path
            .with_extension(format!("alter-bridge-backup-{}", std::process::id()));
        if backup.exists() {
            return Err("backup path already exists".into());
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&backup)
            .map_err(|_| "cannot create backup".to_string())?;
        file.write_all(old)
            .and_then(|()| file.sync_all())
            .map_err(|_| "cannot persist backup".to_string())?;
    }
    let temporary = change
        .path
        .with_extension(format!("alter-bridge-{}.tmp", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(&temporary)
        .map_err(|_| "cannot create provider temporary".to_string())?;
    let written = file
        .write_all(&change.next)
        .and_then(|()| file.sync_all())
        .map_err(|_| "cannot persist provider temporary".to_string());
    if written.is_err() {
        fs::remove_file(&temporary).ok();
        return Err("cannot persist provider temporary".into());
    }
    if read_existing(&change.path)? != change.previous {
        fs::remove_file(&temporary).ok();
        return Err("destination changed during setup".into());
    }
    if fs::rename(&temporary, &change.path).is_err() {
        fs::remove_file(&temporary).ok();
        return Err("cannot replace provider destination".into());
    }
    fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| "provider file replaced, but directory sync failed".to_string())?;
    Ok(())
}
