//! Native metadata-only notification adapters used by the device daemon.

use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use alter_bridge_protocol::{NoticeKind, Ping};

use crate::{address, session};

/// Time `codex queue` gets to accept a notice before the attempt counts as failed.
const CODEX_TIMEOUT: Duration = Duration::from_secs(10);

/// Maximum time for one Claude socket connection and frame submission.
const CLAUDE_TIMEOUT: Duration = Duration::from_secs(10);

/// Waits for a child up to the timeout; a child still running by then is killed.
fn wait_bounded(
    mut child: std::process::Child,
    what: &str,
    timeout: Duration,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;

    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => return Err(format!("{what} failed: {status}")),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();

                return Err(format!("{what} timed out after {}s", timeout.as_secs()));
            }
            Err(error) => return Err(format!("{what} could not be awaited: {error}")),
        }
    }
}

/// Formats metadata-only message instructions or receipt facts for hooks and daemon delivery.
pub(crate) fn notice(ping: &Ping) -> String {
    if ping.notice_kind == NoticeKind::Receipt {
        let Some(facts) = &ping.receipt else {
            return "alter-bridge: invalid receipt metadata; no inbox operation is requested."
                .into();
        };
        let state = if facts.read_at.is_some() {
            "read"
        } else {
            "delivered"
        };

        return format!(
            "alter-bridge: {state} receipt (metadata-only fact snapshot) for msgid {} in room {} at position {}, recipient {}. Sent: {}; local notification submission: {}; committed MCP inbox retrieval: {}. Facts are cumulative; read takes precedence over delivered regardless of snapshot arrival order. This proves neither network receipt, comprehension nor execution. No inbox fetch, reply, consumption ACK or execution PIN is required for this receipt.",
            ping.message_id,
            ping.room,
            ping.position,
            serde_json::to_string(&facts.recipient).expect("identity serializes"),
            facts.sent_at,
            facts
                .delivered_at
                .as_deref()
                .unwrap_or("not observed in this snapshot"),
            facts
                .read_at
                .as_deref()
                .unwrap_or("not observed in this snapshot")
        );
    }

    format!(
        "alter-bridge: new message from {} ({}) (msgid {}). Load the abg skill and call the MCP inbox tool with actor {}. The notice contains metadata only.",
        address::alias(&ping.author.id),
        ping.author.name,
        ping.message_id,
        serde_json::to_string(&ping.to).expect("identity serializes")
    )
}

/// Queues the notice on the recipient's Codex thread and waits for `codex queue` to accept it.
fn ring_codex(ping: &Ping, notice: &str) -> Result<(), String> {
    let child = Command::new("codex")
        .args(["queue", "--thread", &ping.to.id, "--message", notice])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("codex queue could not start: {error}"))?;

    wait_bounded(child, "codex queue", CODEX_TIMEOUT)
}

/// Connects to a local Unix socket before the deadline using a nonblocking descriptor.
pub(crate) fn connect_unix(socket: &Path, deadline: Instant) -> Result<UnixStream, String> {
    use std::os::unix::ffi::OsStrExt;

    let path = socket.as_os_str().as_bytes();
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    if path.len() >= address.sun_path.len() {
        return Err("socket path is too long".to_string());
    }
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (target, source) in address.sun_path.iter_mut().zip(path) {
        *target = *source as libc::c_char;
    }

    let raw = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if raw < 0 {
        return Err(format!(
            "socket could not open: {}",
            std::io::Error::last_os_error()
        ));
    }
    let owned = unsafe { OwnedFd::from_raw_fd(raw) };
    let flags = unsafe { libc::fcntl(raw, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(raw, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(format!(
            "socket could not become nonblocking: {}",
            std::io::Error::last_os_error()
        ));
    }

    let result = unsafe {
        libc::connect(
            raw,
            (&raw const address).cast::<libc::sockaddr>(),
            std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t,
        )
    };
    if result < 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EINPROGRESS) {
            return Err(format!("socket connection failed: {error}"));
        }

        let remaining = deadline.saturating_duration_since(Instant::now());
        let mut poll = libc::pollfd {
            fd: raw,
            events: libc::POLLOUT,
            revents: 0,
        };
        let waited = unsafe {
            libc::poll(
                &raw mut poll,
                1,
                remaining.as_millis().min(i32::MAX as u128) as i32,
            )
        };
        if waited <= 0 {
            return Err("socket connection timed out or failed".to_string());
        }

        let mut socket_error: libc::c_int = 0;
        let mut length = std::mem::size_of_val(&socket_error) as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                raw,
                libc::SOL_SOCKET,
                libc::SO_ERROR,
                (&raw mut socket_error).cast(),
                &raw mut length,
            )
        } < 0
            || socket_error != 0
        {
            return Err(format!(
                "socket connection failed: {}",
                std::io::Error::from_raw_os_error(socket_error)
            ));
        }
    }

    Ok(UnixStream::from(owned))
}

/// Submits one metadata-only Claude peer frame; success only proves local transport submission.
fn ring_claude(home: &Path, ping: &Ping, notice: &str) -> Result<(), String> {
    let deadline = Instant::now() + CLAUDE_TIMEOUT;
    let socket = session::claude_socket(home, &ping.to.id)?;
    let safe_notice = notice
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    let content = format!(
        "<cross-session-message from=\"alter-bridge\" from-name=\"alter-bridge\" from-mode=\"notification\">\n{safe_notice}\n</cross-session-message>"
    );
    let frame = serde_json::json!({
        "msgV": 1,
        "msg_id": ping.notification_id,
        "type": "user",
        "message": {"role": "user", "content": content},
        "priority": "next",
        "from": "alter-bridge"
    });
    let mut bytes = serde_json::to_vec(&frame)
        .map_err(|error| format!("claude frame serialization failed: {error}"))?;
    bytes.push(b'\n');

    let mut stream = connect_unix(&socket, deadline)?;
    if session::claude_socket(home, &ping.to.id)? != socket {
        return Err("claude destination changed before delivery".to_string());
    }
    let mut written = 0;
    while written < bytes.len() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("claude socket delivery timed out".to_string());
        }

        match stream.write(&bytes[written..]) {
            Ok(0) => {
                return Err("claude socket closed before the full frame was written".to_string());
            }
            Ok(size) => written += size,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                let mut poll = libc::pollfd {
                    fd: stream.as_raw_fd(),
                    events: libc::POLLOUT,
                    revents: 0,
                };
                let waited = unsafe {
                    libc::poll(
                        &raw mut poll,
                        1,
                        remaining.as_millis().min(i32::MAX as u128) as i32,
                    )
                };
                if waited <= 0 {
                    return Err("claude socket write timed out or failed".to_string());
                }
            }
            Err(error) => return Err(format!("claude socket write failed: {error}")),
        }
    }

    Ok(())
}

/// Hands one notice to the recipient's provider; `Ok` means the local transport accepted it,
/// never that a user or model saw or approved it.
pub fn ring(home: &Path, ping: &Ping) -> Result<(), String> {
    let notice = notice(ping);

    match session::resolve_provider(home, &ping.to.id)? {
        session::Provider::Codex => ring_codex(ping, &notice),
        session::Provider::Claude => ring_claude(home, ping, &notice),
        session::Provider::OpenCode => crate::opencode::synthetic(
            home,
            &ping.to.id,
            &notice,
            "alter-bridge notice",
            ping.notice_kind == NoticeKind::Message,
        ),
    }
}
