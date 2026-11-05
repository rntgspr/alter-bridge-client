//! Local, directional, one-day peer trust granted by the receiving chat's user.

use std::fs::File;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use alter_bridge_protocol::limits;
use serde::{Deserialize, Serialize};

use crate::Exit;

const TRUST_NAME: &str = "alter-bridge-trust.json";
const TTL_SECS: u64 = 86_400;
const USAGE: &str = "usage: alter-bridge trust grant --id <self> --peer <id> [--name <name>]\n       alter-bridge trust revoke --id <self> --peer <id>\n       alter-bridge trust list --id <self>\n";

/// One trusted peer of an actor, as exposed to the hook and the list command.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct Peer {
    pub(crate) peer: String,
    pub(crate) name: Option<String>,
    pub(crate) expires_at: u64,
}

/// One stored actor-to-peer grant.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Entry {
    actor: String,
    peer: String,
    name: Option<String>,
    expires_at: u64,
}

/// The on-disk trust store.
#[derive(Debug, Serialize, Deserialize)]
struct Store {
    version: u32,
    entries: Vec<Entry>,
}

/// Returns the trust store path next to the device configuration.
fn path(home: &Path) -> PathBuf {
    home.join(".config").join(TRUST_NAME)
}

/// Returns the current unix time in seconds, or zero when the clock is before the epoch.
pub(crate) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

/// Reports whether an identity ID is acceptable, matching `connect context` validation.
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= limits::IDENTITY_ID_BYTES && !id.chars().any(char::is_control)
}

/// Loads the store; a missing file is an empty store.
fn load(home: &Path) -> Result<Store, String> {
    let text = match std::fs::read_to_string(path(home)) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Store {
                version: 1,
                entries: Vec::new(),
            });
        }
        Err(_) => return Err("trust: cannot read the trust store".into()),
    };

    serde_json::from_str(&text).map_err(|_| "trust: invalid trust store".to_string())
}

/// Prunes expired entries and writes the store atomically with mode 600.
fn save(home: &Path, mut store: Store) -> Result<(), String> {
    let at = now();
    store.entries.retain(|entry| entry.expires_at > at);

    let dir = home.join(".config");
    match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err("trust: cannot create the private configuration directory".into()),
    }

    let body = serde_json::to_vec(&store)
        .map_err(|_| "trust: cannot encode the trust store".to_string())?;
    let temp = dir.join(format!(".{TRUST_NAME}.{}.tmp", std::process::id()));
    let written = File::options()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temp)
        .and_then(|mut file| {
            file.write_all(&body)?;
            file.sync_all()
        });
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
        return Err("trust: cannot write the trust store".into());
    }

    std::fs::rename(&temp, path(home)).map_err(|_| {
        let _ = std::fs::remove_file(&temp);
        "trust: cannot replace the trust store".to_string()
    })
}

/// Returns the unexpired trusted peers of an actor; any read error yields none.
pub(crate) fn active(home: &Path, actor: &str) -> Vec<Peer> {
    let at = now();

    load(home)
        .map(|store| {
            store
                .entries
                .into_iter()
                .filter(|entry| entry.actor == actor && entry.expires_at > at)
                .map(|entry| Peer {
                    peer: entry.peer,
                    name: entry.name,
                    expires_at: entry.expires_at,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Removes every trust entry owned by an actor.
pub(crate) fn clear_actor(home: &Path, actor: &str) -> Result<(), String> {
    let mut store = load(home)?;
    store.entries.retain(|entry| entry.actor != actor);

    save(home, store)
}

/// Removes the whole trust store; a missing file is already clear.
pub(crate) fn clear_all(home: &Path) -> Result<(), String> {
    match std::fs::remove_file(path(home)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err("trust: cannot remove the trust store".into()),
    }
}

/// Runs `trust grant|revoke|list` and prints one JSON line.
pub(crate) fn run(args: &[String], home: &Path) -> Result<(), Exit> {
    let Some((action, rest)) = args.split_first() else {
        return Err(Exit::Usage(USAGE.into()));
    };

    let (mut id, mut peer, mut name) = (None, None, None);
    let mut values = rest.iter();
    while let Some(flag) = values.next() {
        match flag.as_str() {
            "--id" if id.is_none() => id = values.next().map(String::as_str),
            "--peer" if peer.is_none() => peer = values.next().map(String::as_str),
            "--name" if name.is_none() && action == "grant" => {
                name = values.next().map(String::as_str)
            }
            _ => return Err(Exit::Usage(USAGE.into())),
        }
    }

    let Some(id) = id.filter(|id| valid_id(id)) else {
        return Err(Exit::Usage(USAGE.into()));
    };
    let needs_peer = action != "list";
    let peer = match peer {
        Some(peer) if needs_peer && valid_id(peer) => Some(peer),
        None if !needs_peer => None,
        _ => return Err(Exit::Usage(USAGE.into())),
    };
    if name.is_some_and(|name| !valid_id(name)) {
        return Err(Exit::Usage(USAGE.into()));
    }

    match action.as_str() {
        "grant" => {
            let peer = peer.expect("grant requires peer");
            let mut store = load(home).map_err(Exit::Failure)?;
            let expires_at = now() + TTL_SECS;
            store
                .entries
                .retain(|entry| !(entry.actor == id && entry.peer == peer));
            store.entries.push(Entry {
                actor: id.into(),
                peer: peer.into(),
                name: name.map(String::from),
                expires_at,
            });
            save(home, store).map_err(Exit::Failure)?;
            println!(
                "{}",
                serde_json::json!({"outcome":"granted", "peer":peer, "name":name, "expires_at":expires_at})
            );
        }
        "revoke" => {
            let peer = peer.expect("revoke requires peer");
            let mut store = load(home).map_err(Exit::Failure)?;
            let before = store.entries.len();
            store
                .entries
                .retain(|entry| !(entry.actor == id && entry.peer == peer));
            let removed = store.entries.len() != before;
            save(home, store).map_err(Exit::Failure)?;
            println!(
                "{}",
                serde_json::json!({"outcome":"revoked", "peer":peer, "removed":removed})
            );
        }
        "list" => {
            println!("{}", serde_json::json!({"peers": active(home, id)}));
        }
        _ => return Err(Exit::Usage(USAGE.into())),
    }

    Ok(())
}
