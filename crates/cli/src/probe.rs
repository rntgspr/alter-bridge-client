//! `probe`: sends a no-reply test message to exercise the wake-up pipeline.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use alter_bridge_protocol::{
    Author, DeviceSessionRequest, Identity, MessageType, NoticeKind, Ping,
};
use serde_json::json;

use crate::{Exit, api, config, notify};

/// Author ID prefix that marks a CLI-owned probe binding.
pub(crate) const PREFIX: &str = "abg_probe_";

const DEFAULT_BODY: &str = "Test probe: please do not reply.";
const WAIT: Duration = Duration::from_secs(15);
const POLL: Duration = Duration::from_millis(250);
const USAGE: &str = "usage: alter-bridge probe <actor> [message] [--local] [--async]\n  <actor>: exact ID, name or device::name, optional leading #\n";

/// Reports whether an identity belongs to a CLI-owned probe binding.
pub(crate) fn is_probe(id: &str) -> bool {
    id.starts_with(PREFIX)
}

/// Returns the directory where the daemon records receipts addressed to probe bindings.
fn receipts_dir(home: &Path) -> PathBuf {
    home.join(".local/state/alter-bridge/probes")
}

/// Records a ping addressed to a probe binding so a waiting probe can observe it.
pub(crate) fn record(home: &Path, ping: &Ping) -> Result<(), String> {
    if ping.notice_kind != NoticeKind::Receipt {
        return Ok(());
    }

    let dir = receipts_dir(home);
    fs::create_dir_all(&dir).map_err(|error| format!("probe receipt dir: {error}"))?;
    let text = serde_json::to_string(&ping.receipt).expect("receipt serializes");

    fs::write(dir.join(format!("{}.json", ping.message_id)), text)
        .map_err(|error| format!("probe receipt write: {error}"))
}

/// Generates a 21-character alphanumeric random ID from the system entropy source.
fn nanoid() -> Result<String, Exit> {
    const ALPHABET: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

    let mut bytes = [0u8; 21];
    fs::File::open("/dev/urandom")
        .and_then(|mut source| source.read_exact(&mut bytes))
        .map_err(|error| Exit::Failure(format!("probe: entropy unavailable: {error}")))?;

    Ok(bytes
        .iter()
        .map(|byte| ALPHABET[*byte as usize % ALPHABET.len()] as char)
        .collect())
}

/// Resolves an actor reference to exactly one binding among the candidates.
pub(crate) fn resolve<'a>(
    command: &str,
    reference: &str,
    candidates: &'a [(Author, String)],
) -> Result<&'a Author, Exit> {
    let reference = reference.strip_prefix('#').unwrap_or(reference);
    let matches: Vec<&(Author, String)> = candidates
        .iter()
        .filter(|(binding, device)| {
            binding.id == reference
                || binding.name == reference
                || format!("{device}::{}", binding.name) == reference
        })
        .collect();

    match matches.as_slice() {
        [(binding, _)] => Ok(binding),
        [] => Err(Exit::Failure(format!(
            "{command}: no participating session matches {reference:?}"
        ))),
        many => Err(Exit::Failure(format!(
            "{command}: {reference:?} is ambiguous ({}); use device::name or the exact ID",
            many.iter()
                .map(|(binding, device)| format!("{device}::{} {}", binding.name, binding.id))
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

/// Parses arguments, then runs the local ephemeral ring or the durable broker probe.
pub(crate) fn run(args: &[String], home: &Path) -> Result<(), Exit> {
    let mut local = false;
    let mut asynchronous = false;
    let mut positional = Vec::new();
    for arg in args {
        match arg.as_str() {
            "--local" => local = true,
            "--async" => asynchronous = true,
            flag if flag.starts_with("--") => return Err(Exit::Usage(USAGE.into())),
            value => positional.push(value),
        }
    }
    let (reference, body) = match positional.as_slice() {
        [reference] => (*reference, DEFAULT_BODY.to_string()),
        [reference, rest @ ..] => (*reference, rest.join(" ")),
        [] => return Err(Exit::Usage(USAGE.into())),
    };
    if local && asynchronous {
        return Err(Exit::Usage(format!(
            "probe: --async has no effect with --local\n{USAGE}"
        )));
    }

    let saved = config::load(home).map_err(Exit::Failure)?;
    let client = api::Client::new(&saved.url);
    let id = nanoid()?;

    if local {
        return ring_local(home, &saved, &client, reference, &id);
    }

    let directory = client
        .directory(&saved.token)
        .map_err(|error| Exit::Failure(format!("probe: who: {}", error.public_message())))?;
    let candidates: Vec<(Author, String)> = directory
        .sessions
        .into_iter()
        .filter(|entry| !is_probe(&entry.binding.id))
        .map(|entry| (entry.binding, entry.device_id))
        .collect();
    let target = resolve("probe", reference, &candidates)?.clone();

    let probe = Author {
        id: format!("{PREFIX}{id}"),
        name: format!("probe@{}", saved.device_id),
    };
    client
        .register_session(
            &saved.token,
            &DeviceSessionRequest {
                id: probe.id.clone(),
                name: probe.name.clone(),
            },
        )
        .map_err(|error| Exit::Failure(format!("probe: join: {}", error.public_message())))?;

    let outcome = send_and_wait(
        home,
        &saved,
        &client,
        &probe,
        &target,
        &id,
        &body,
        asynchronous,
    );
    let left = client.leave_session(&saved.token, &probe.identity());
    if let Err(error) = left {
        eprintln!(
            "alter-bridge: probe: leave {} failed: {}; run `alter-bridge disconnect context --id {}`",
            probe.id,
            error.public_message(),
            probe.id
        );
    }

    outcome
}

/// Sends the probe through MCP `send` and, unless asynchronous, waits for its delivered receipt.
#[allow(clippy::too_many_arguments)]
fn send_and_wait(
    home: &Path,
    saved: &config::Config,
    client: &api::Client,
    probe: &Author,
    target: &Author,
    id: &str,
    body: &str,
    asynchronous: bool,
) -> Result<(), Exit> {
    let message_id = format!("probe-{id}");
    let sent = client
        .mcp_tool(
            &saved.token,
            "send",
            json!({
                "actor": probe.identity(),
                "to": target.identity(),
                "message_id": message_id,
                "type": MessageType::Message,
                "body": body,
            }),
        )
        .map_err(|error| Exit::Failure(format!("probe: send: {error}")))?;
    println!(
        "sent {message_id} to {} ({}) as {} room {} position {}",
        target.name,
        target.id,
        probe.id,
        sent["room"].as_str().unwrap_or("?"),
        sent["position"]
    );
    if asynchronous {
        return Ok(());
    }

    let path = receipts_dir(home).join(format!("{message_id}.json"));
    let started = Instant::now();
    while started.elapsed() < WAIT {
        if let Ok(text) = fs::read_to_string(&path) {
            let facts: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
            if let Some(delivered) = facts["delivered_at"].as_str() {
                let _ = fs::remove_file(&path);
                println!(
                    "delivered at {delivered} ({:.1}s); read: {}",
                    started.elapsed().as_secs_f32(),
                    facts["read_at"].as_str().unwrap_or("no")
                );
                return Ok(());
            }
        }
        std::thread::sleep(POLL);
    }

    let _ = fs::remove_file(&path);
    Err(Exit::Failure(format!(
        "probe: no delivered receipt within {}s; the recipient may not have woken, or the local daemon is outdated (`alter-bridge daemon restart`)",
        WAIT.as_secs()
    )))
}

/// Rings one binding owned by this device directly, without any broker write.
fn ring_local(
    home: &Path,
    saved: &config::Config,
    client: &api::Client,
    reference: &str,
    id: &str,
) -> Result<(), Exit> {
    let owned = client
        .sessions(&saved.token)
        .map_err(|error| Exit::Failure(format!("probe: sessions: {}", error.public_message())))?;
    let candidates: Vec<(Author, String)> = owned
        .into_iter()
        .filter(|binding| !is_probe(&binding.id))
        .map(|binding| (binding, saved.device_id.clone()))
        .collect();
    let target = resolve("probe", reference, &candidates).map_err(|error| match error {
        Exit::Failure(message) => Exit::Failure(format!("{message} on this device (--local)")),
        other => other,
    })?;

    let ping = Ping {
        notice_kind: NoticeKind::Message,
        receipt: None,
        notification_id: format!("{PREFIX}local_{id}"),
        to: Identity {
            id: target.id.clone(),
        },
        author: Author {
            id: format!("{PREFIX}{id}"),
            name: format!("probe@{}", saved.device_id),
        },
        message_id: format!("probe-local-{id}"),
        message_type: MessageType::Message,
        room: "local".into(),
        position: 0,
        replay: false,
    };
    let started = Instant::now();
    notify::ring(home, &ping).map_err(|error| Exit::Failure(format!("probe: ring: {error}")))?;
    println!(
        "rang {} ({}) locally in {:.1}s; nothing was written to the broker",
        target.name,
        target.id,
        started.elapsed().as_secs_f32()
    );

    Ok(())
}
