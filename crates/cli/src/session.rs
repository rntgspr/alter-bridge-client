//! Native session discovery from each provider's own live state; no second roster is kept.

use std::os::unix::fs::FileTypeExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use alter_bridge_protocol::Author;
use serde::Deserialize;

/// Identifies a native integration only within this CLI process.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Provider {
    Claude,
    Codex,
    OpenCode,
}

/// One chat recorded by its provider.
#[derive(Debug, Clone, Default)]
pub struct Session {
    pub id: String,
    pub name: String,
}

/// One running interactive Claude session, as `claude agents --json` reports it.
pub struct Agent {
    pub id: String,
    pub name: String,
}

/// Returns the first executable file named `name` on `PATH`.
pub(crate) fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;

    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable(candidate))
}

/// Reports whether a path is a regular file with an execute bit.
pub fn is_executable(path: &Path) -> bool {
    std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

/// Returns the Claude CLI path: the `PATH` entry, else `~/.local/bin/claude`, unchecked.
pub fn claude_bin(home: &Path) -> PathBuf {
    find_on_path("claude").unwrap_or_else(|| home.join(".local").join("bin").join("claude"))
}

/// Runs a command and returns its stdout, or an error naming what failed.
fn output_of(command: &mut Command, what: &str) -> Result<Vec<u8>, String> {
    use std::io::Read;
    use std::time::{Duration, Instant};

    let mut child = command
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|error| format!("session: {what}: {error}"))?;
    let mut stdout = child.stdout.take().expect("piped command stdout");
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout.read_to_end(&mut bytes).map(|_| bytes);
        let _ = sender.send(result);
    });
    let deadline = Instant::now() + Duration::from_secs(3);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            outcome => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "session: {what}: {}",
                    outcome
                        .err()
                        .map_or_else(|| "timed out".into(), |error| error.to_string())
                ));
            }
        }
    };

    if !status.success() {
        return Err(format!("session: {what}: {status}"));
    }

    receiver
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .map_err(|_| format!("session: {what}: output timed out"))?
        .map_err(|error| format!("session: {what}: {error}"))
}

/// Lists live interactive and background Claude sessions reported by the provider.
pub fn claude_agents(home: &Path) -> Result<Vec<Agent>, String> {
    #[derive(Deserialize)]
    struct Entry {
        #[serde(default, rename = "sessionId")]
        session_id: String,
        #[serde(default)]
        name: String,
        #[serde(default)]
        kind: String,
    }

    let stdout = output_of(
        Command::new(claude_bin(home)).args(["agents", "--json"]),
        "claude agents",
    )?;
    let entries: Vec<Entry> = serde_json::from_slice(&stdout)
        .map_err(|error| format!("session: claude agents: unexpected output: {error}"))?;

    Ok(entries
        .into_iter()
        .filter(|entry| matches!(entry.kind.as_str(), "interactive" | "background" | "bg"))
        .map(|entry| Agent {
            id: entry.session_id,
            name: entry.name,
        })
        .collect())
}

/// Resolves a full Claude session ID through the provider's live per-process session records.
pub fn claude_socket(home: &Path, id: &str) -> Result<PathBuf, String> {
    find_claude_socket(home, id)?
        .ok_or_else(|| "the recipient Claude session is not running".to_string())
}

/// Distinguishes an absent Claude destination from failed or ambiguous local discovery.
fn find_claude_socket(home: &Path, id: &str) -> Result<Option<PathBuf>, String> {
    #[derive(Deserialize)]
    struct Entry {
        #[serde(rename = "sessionId")]
        session_id: String,
        pid: u32,
        #[serde(rename = "messagingSocketPath")]
        socket: PathBuf,
    }

    let directory = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".claude"))
        .join("sessions");
    let mut matches = Vec::new();
    let records = match std::fs::read_dir(&directory) {
        Ok(records) => records,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("claude session records unavailable: {error}")),
    };

    for record in records {
        let record =
            record.map_err(|error| format!("claude session record unreadable: {error}"))?;
        let path = record.path();
        if path.extension().is_none_or(|extension| extension != "json") {
            continue;
        }

        let data = std::fs::read(&path)
            .map_err(|error| format!("claude session record unreadable: {error}"))?;
        let entry: Entry = serde_json::from_slice(&data)
            .map_err(|error| format!("claude session record malformed: {error}"))?;
        if entry.session_id != id {
            continue;
        }

        let expected_record = directory.join(format!("{}.json", entry.pid));
        let expected_socket = PathBuf::from(format!("/tmp/cc-socks/{}.sock", entry.pid));
        if path != expected_record || entry.socket != expected_socket {
            return Err("claude session record has an inconsistent socket mapping".to_string());
        }

        let metadata = match std::fs::symlink_metadata(&entry.socket) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("claude destination unavailable: {error}")),
        };
        if !metadata.file_type().is_socket() {
            return Err("claude destination is not a Unix socket".to_string());
        }

        matches.push(entry.socket);
    }

    match matches.as_slice() {
        [socket] => Ok(Some(socket.clone())),
        [] => Ok(None),
        _ => Err("the recipient Claude session has ambiguous socket records".to_string()),
    }
}

/// Runs a read-only query through the `sqlite3` CLI for session IDs and names.
fn query_sqlite(database: &Path, query: &str) -> Result<Vec<Session>, String> {
    #[derive(Deserialize)]
    struct Row {
        id: String,
        #[serde(default)]
        name: Option<String>,
    }

    if !database.is_file() {
        return Err(format!("session: no database at {}", database.display()));
    }

    let uri = format!("file:{}?mode=ro", database.display());
    let stdout = output_of(
        Command::new("sqlite3").args(["-json", &uri, query]),
        "sqlite3",
    )?;
    if stdout.trim_ascii().is_empty() {
        return Ok(Vec::new());
    }

    let rows: Vec<Row> = serde_json::from_slice(&stdout)
        .map_err(|error| format!("session: unexpected sqlite3 output: {error}"))?;

    Ok(rows
        .into_iter()
        .map(|row| Session {
            id: row.id,
            name: row.name.unwrap_or_default(),
        })
        .collect())
}

/// Lists Codex thread titles from the current read-only database snapshot.
fn codex_sessions(home: &Path) -> Result<Vec<Session>, String> {
    let codex = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".codex"));

    query_sqlite(
        &codex.join("state_5.sqlite"),
        "select id, coalesce(name, '') as name, archived from threads order by updated_at desc",
    )
}

/// Reports whether an ID has the provider's full native form, usable without its store.
pub(crate) fn is_full_id(provider: Provider, id: &str) -> bool {
    match provider {
        Provider::Claude | Provider::Codex => {
            let groups: Vec<&str> = id.split('-').collect();
            let lengths = [8, 4, 4, 4, 12];

            groups.len() == lengths.len()
                && groups.iter().zip(lengths).all(|(group, length)| {
                    group.len() == length && group.bytes().all(|b| b.is_ascii_hexdigit())
                })
        }
        Provider::OpenCode => crate::opencode::is_session_id(id),
    }
}

/// Reads only currently loaded Codex thread IDs from the running provider daemon, bounded to three seconds.
fn codex_loaded(home: &Path) -> Result<Vec<String>, String> {
    use std::net::Shutdown;
    use std::time::{Duration, Instant};

    let deadline = Instant::now() + Duration::from_secs(3);
    let codex_home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".codex"));
    let path = codex_home.join("app-server-control/app-server-control.sock");
    let stream = crate::notify::connect_unix(&path, deadline)
        .map_err(|error| format!("Codex discovery unavailable: {error}"))?;
    let cancellation = stream.try_clone().map_err(|error| error.to_string())?;
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = sender.send(codex_loaded_inner(stream, deadline));
    });
    match receiver.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        Ok(result) => result,
        Err(_) => {
            let _ = cancellation.shutdown(Shutdown::Both);
            Err("Codex live session discovery timed out".to_string())
        }
    }
}

/// Queries the Codex control socket for a complete paginated snapshot of loaded threads.
fn codex_loaded_inner(
    stream: std::os::unix::net::UnixStream,
    deadline: std::time::Instant,
) -> Result<Vec<String>, String> {
    use std::time::Instant;
    use tungstenite::Message;

    stream
        .set_nonblocking(false)
        .map_err(|error| error.to_string())?;
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err("Codex discovery timed out".into());
    }
    stream
        .set_read_timeout(Some(remaining))
        .map_err(|error| error.to_string())?;
    stream
        .set_write_timeout(Some(remaining))
        .map_err(|error| error.to_string())?;
    let (mut socket, _) = tungstenite::client("ws://localhost/", stream)
        .map_err(|_| "Codex discovery handshake failed".to_string())?;
    socket.send(Message::Text(serde_json::json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"alter-bridge","version":"1"}}}).to_string().into()))
        .map_err(|error| format!("Codex discovery initialize failed: {error}"))?;
    let mut ids = Vec::new();
    let mut cursor = None;
    let mut stage = "initialize";
    while let Some(remaining) = deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
    {
        socket
            .get_mut()
            .set_read_timeout(Some(remaining))
            .map_err(|error| error.to_string())?;
        socket
            .get_mut()
            .set_write_timeout(Some(remaining))
            .map_err(|error| error.to_string())?;
        let message = socket
            .read()
            .map_err(|_| format!("Codex discovery failed at {stage}"))?;
        let Message::Text(line) = message else {
            if matches!(message, Message::Ping(_)) {
                socket
                    .flush()
                    .map_err(|error| format!("Codex discovery pong failed: {error}"))?;
            } else if matches!(message, Message::Close(_)) {
                return Err(format!("Codex discovery closed at {stage}"));
            }
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        match value.get("id").and_then(serde_json::Value::as_u64) {
            Some(1) if stage == "initialize" && value.get("result").is_some() => {
                stage = "thread/loaded/list";
                socket
                    .send(Message::Text(
                        serde_json::json!({"method":"initialized"})
                            .to_string()
                            .into(),
                    ))
                    .map_err(|error| format!("Codex discovery failed at {stage}: {error}"))?;
                socket
                    .send(Message::Text(
                        serde_json::json!({"id":2,"method":"thread/loaded/list","params":{}})
                            .to_string()
                            .into(),
                    ))
                    .map_err(|error| format!("Codex discovery failed at {stage}: {error}"))?;
            }
            Some(2) if stage == "thread/loaded/list" => {
                let Some(result) = value.get("result") else {
                    break;
                };
                let Some(data) = result.get("data").and_then(serde_json::Value::as_array) else {
                    break;
                };
                for id in data {
                    let Some(id) = id.as_str() else {
                        return Err("Codex discovery returned an invalid thread ID".into());
                    };
                    ids.push(id.to_owned());
                }
                let next = match result.get("nextCursor") {
                    None | Some(serde_json::Value::Null) => None,
                    Some(serde_json::Value::String(next)) => Some(next.as_str()),
                    _ => return Err("Codex discovery returned an invalid cursor".into()),
                };
                if let Some(next) = next {
                    if cursor.as_deref() == Some(next) {
                        break;
                    }
                    cursor = Some(next.to_owned());
                    socket.send(Message::Text(serde_json::json!({"id":2,"method":"thread/loaded/list","params":{"cursor":next}}).to_string().into())).map_err(|error| format!("Codex discovery failed at {stage}: {error}"))?;
                } else {
                    return Ok(ids);
                }
            }
            Some(1) => break,
            _ => {}
        }
    }
    Err(format!(
        "Codex live session discovery did not complete at {stage}"
    ))
}

/// Resolves an opaque ID against live native providers immediately before delivery.
pub(crate) fn resolve_provider(home: &Path, id: &str) -> Result<Provider, String> {
    if crate::opencode::is_session_id(id) {
        return Ok(Provider::OpenCode);
    }
    let claude = find_claude_socket(home, id)?.is_some();
    let codex = codex_loaded(home)?.iter().any(|loaded| loaded == id);

    match (claude, codex) {
        (true, false) => Ok(Provider::Claude),
        (false, true) => Ok(Provider::Codex),
        (true, true) => Err("recipient ID matches multiple native providers".into()),
        (false, false) => Err("recipient ID is not a running native session".into()),
    }
}

/// Lists unambiguous active IDs from Claude sockets, loaded Codex threads and running OpenCode sessions, excluding historical stores.
pub(crate) fn active(home: &Path) -> (Vec<Author>, Vec<String>) {
    let mut errors = Vec::new();
    let mut active: Vec<Author> = claude_agents(home)
        .unwrap_or_else(|error| {
            errors.push(error);
            Vec::new()
        })
        .into_iter()
        .filter(|agent| match find_claude_socket(home, &agent.id) {
            Ok(Some(_)) => true,
            Ok(None) => false,
            Err(error) => {
                errors.push(error);
                false
            }
        })
        .map(|agent| Author {
            id: agent.id,
            name: agent.name,
        })
        .collect();

    let known = codex_sessions(home).unwrap_or_else(|error| {
        errors.push(error);
        Vec::new()
    });
    active.extend(
        codex_loaded(home)
            .unwrap_or_else(|error| {
                errors.push(error);
                Vec::new()
            })
            .into_iter()
            .map(|id| {
                let name = known
                    .iter()
                    .find(|session| session.id == id)
                    .map(|session| session.name.clone())
                    .unwrap_or_default();
                Author { id, name }
            }),
    );
    active.retain(|author| is_full_id(Provider::Claude, &author.id));
    active.extend(crate::opencode::active(home).unwrap_or_else(|error| {
        errors.push(error);
        Vec::new()
    }));
    for author in &mut active {
        if author.name.is_empty()
            || author.name.len() > 128
            || author.name.chars().any(char::is_control)
        {
            author.name = author.id.clone();
        }
    }
    let mut counts = std::collections::HashMap::new();
    for author in &active {
        *counts.entry(author.id.clone()).or_insert(0usize) += 1;
    }
    active.retain(|author| counts.get(&author.id) == Some(&1));

    (active, errors)
}
