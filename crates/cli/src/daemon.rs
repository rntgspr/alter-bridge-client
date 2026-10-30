//! `daemon`: holds the notification WebSocket, rings local recipients, and reports each outcome.

use std::collections::{HashSet, VecDeque};
use std::fs::{File, TryLockError};
use std::io::{ErrorKind, Read, Seek, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use alter_bridge_protocol::{
    Author, ClientFrame, DaemonAdvertisement, ErrorBody, ErrorCode, Ping, ServerFrame, limits,
};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket, client_tls_with_config};

use crate::config::{self, Config};
use crate::{Exit, api::Client, launchd, notify, session};

/// Initial ceiling for the randomized reconnect delay.
const BACKOFF_MIN: Duration = Duration::from_secs(1);

/// Largest pause before a reconnect, and the connection age that resets the pause.
const BACKOFF_MAX: Duration = Duration::from_secs(60);

/// Peer silence after which the socket is no longer live.
const READ_TIMEOUT: Duration = Duration::from_secs(240);

/// Time each step of TCP connect, TLS, and the WebSocket upgrade may block.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Longest wait on the socket before finished pings are reported.
const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Time `daemon stop` waits for the daemon to release its lock.
const STOP_TIMEOUT: Duration = Duration::from_secs(5);

/// Accepted notification IDs remembered to answer a replay without ringing again.
const ACCEPTED_MEMORY: usize = 1024;

/// Pings rung at the same time; further ones wait in the queue.
const RING_WORKERS: usize = 4;
const RING_QUEUE: usize = 64;
const DISCOVERY_INTERVAL: Duration = Duration::from_secs(5);

/// Notification socket of one connection.
type Socket = WebSocket<MaybeTlsStream<TcpStream>>;

/// Ring queue and its bookkeeping, shared by every connection of this daemon.
struct Ringer {
    jobs: mpsc::SyncSender<(u64, Ping)>,
    outcomes: mpsc::Receiver<(u64, Ping, Result<(), String>)>,
    /// Notification IDs queued or being rung; a repeat of one is not rung again.
    in_flight: HashSet<String>,
    accepted: VecDeque<String>,
    epoch: Arc<std::sync::atomic::AtomicU64>,
}

/// Writes one log line, prefixed with the Unix time in seconds, to stderr; launchd appends it to the daemon log.
fn log(line: &str) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());

    eprintln!("{now} alter-bridge daemon: {line}");
}

/// Returns the lock file that marks the one running daemon of this user.
fn lock_path(home: &Path) -> PathBuf {
    home.join(".local/state/alter-bridge/daemon.lock")
}

/// Records the daemon PID and its connection state in the lock file it holds.
fn write_state(lock: &mut File, state: &str) {
    let written = lock
        .set_len(0)
        .and_then(|()| lock.rewind())
        .and_then(|()| writeln!(lock, "{}\n{state}", std::process::id()));

    if let Err(error) = written {
        log(&format!("cannot record state: {error}"));
    }
}

/// Creates the private state directory and takes the exclusive daemon lock.
fn acquire_lock(home: &Path) -> Result<File, String> {
    let path = lock_path(home);
    let directory = path.parent().expect("lock path has a parent");

    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(directory)
        .map_err(|error| format!("daemon: cannot create {}: {error}", directory.display()))?;
    let parent = std::fs::symlink_metadata(directory)
        .map_err(|_| "daemon: cannot inspect state directory")?;
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    if !parent.is_dir() || parent.uid() != uid || parent.permissions().mode() & 0o077 != 0 {
        return Err("daemon: state directory must be a private real directory".into());
    }
    let lock = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&path)
        .map_err(|error| format!("daemon: cannot open {}: {error}", path.display()))?;
    let metadata = lock
        .metadata()
        .map_err(|_| "daemon: cannot inspect lock file")?;
    if !metadata.is_file()
        || metadata.uid() != uid
        || metadata.nlink() != 1
        || metadata.permissions().mode() & 0o077 != 0
        || metadata.len() > 128
    {
        return Err("daemon: lock must be a private owned regular file".into());
    }

    match lock.try_lock() {
        Ok(()) => Ok(lock),
        Err(TryLockError::WouldBlock) => Err("daemon: already running".to_string()),
        Err(TryLockError::Error(error)) => {
            Err(format!("daemon: cannot lock {}: {error}", path.display()))
        }
    }
}

/// Returns the PID and connection state of the running daemon, or `None` when none holds the lock.
fn running_daemon(home: &Path) -> Option<(i32, String)> {
    let path = lock_path(home);
    let lock = File::options()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&path)
        .ok()?;
    let metadata = lock.metadata().ok()?;
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    if !metadata.is_file()
        || metadata.uid() != uid
        || metadata.nlink() != 1
        || metadata.permissions().mode() & 0o077 != 0
        || metadata.len() > 128
    {
        return None;
    }

    if !matches!(lock.try_lock(), Err(TryLockError::WouldBlock)) {
        return None;
    }

    let mut recorded = String::new();
    lock.take(128).read_to_string(&mut recorded).ok()?;
    let mut lines = recorded.lines();
    let pid = lines.next().and_then(|line| line.parse().ok()).unwrap_or(0);

    Some((pid, lines.next().unwrap_or("starting").to_string()))
}

/// Observes the held daemon lock without starting, stopping, or contacting the daemon.
pub(crate) fn observed_state(home: &Path) -> Option<String> {
    running_daemon(home).map(|(_, state)| state)
}

/// Opens a TCP connection to the first reachable address of the host, bounded by the handshake timeout.
fn connect_tcp(host: &str, port: u16) -> Result<TcpStream, String> {
    let addresses = (host, port)
        .to_socket_addrs()
        .map_err(|error| format!("cannot resolve {host}: {error}"))?;
    let mut failure = format!("{host} resolved to no address");

    for address in addresses {
        match TcpStream::connect_timeout(&address, HANDSHAKE_TIMEOUT) {
            Ok(stream) => return Ok(stream),
            Err(error) => failure = error.to_string(),
        }
    }

    Err(format!("cannot connect: {failure}"))
}

/// Opens only the saved device endpoint with native TLS verification and no redirect following.
fn connect(config: &Config) -> Result<Socket, String> {
    let base = config
        .url
        .strip_prefix("https://")
        .map(|rest| ("wss", rest))
        .or_else(|| config.url.strip_prefix("http://").map(|rest| ("ws", rest)))
        .ok_or("invalid saved broker URL")?;
    let uri: tungstenite::http::Uri = format!("{}://{}/device/ws", base.0, base.1)
        .parse()
        .map_err(|_| "invalid saved broker URL")?;
    let host = uri
        .host()
        .ok_or("saved broker has no host")?
        .trim_matches(&['[', ']'][..])
        .to_string();
    let port = uri
        .port_u16()
        .unwrap_or(if base.0 == "wss" { 443 } else { 80 });
    let authority = uri.authority().expect("URI has host").as_str().to_string();
    let stream = connect_tcp(&host, port)?;
    stream
        .set_read_timeout(Some(HANDSHAKE_TIMEOUT))
        .map_err(|_| "cannot set handshake timeout")?;
    stream
        .set_write_timeout(Some(HANDSHAKE_TIMEOUT))
        .map_err(|_| "cannot set handshake timeout")?;
    let request = tungstenite::http::Request::builder()
        .uri(uri)
        .header("Host", authority)
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header(
            "Sec-WebSocket-Key",
            tungstenite::handshake::client::generate_key(),
        )
        .header("Authorization", format!("Bearer {}", config.token))
        .body(())
        .map_err(|_| "cannot prepare device upgrade")?;
    let limits_config = tungstenite::protocol::WebSocketConfig::default()
        .max_message_size(Some(limits::WEBSOCKET_FRAME_BYTES))
        .max_frame_size(Some(limits::WEBSOCKET_FRAME_BYTES));
    let (mut socket, _) = client_tls_with_config(request, stream, Some(limits_config), None)
        .map_err(|error| match error {
            tungstenite::HandshakeError::Failure(tungstenite::Error::Http(response)) => {
                let code = response
                    .body()
                    .as_ref()
                    .filter(|body| body.len() <= 4096)
                    .and_then(|body| serde_json::from_slice::<ErrorBody>(body).ok())
                    .map(|body| body.error.code);
                match code {
                    Some(ErrorCode::DeviceBusy) => {
                        "device_busy: another daemon owns the live socket".into()
                    }
                    Some(code) => format!(
                        "device upgrade refused with status {} ({code:?})",
                        response.status().as_u16()
                    ),
                    None => format!(
                        "device upgrade refused with status {}",
                        response.status().as_u16()
                    ),
                }
            }
            _ => "device upgrade failed: transport or TLS error".into(),
        })?;
    match socket.get_mut() {
        MaybeTlsStream::Plain(stream) => {
            stream
                .set_read_timeout(Some(POLL_INTERVAL))
                .map_err(|_| "cannot set socket timeout")?;
            stream
                .set_write_timeout(Some(HANDSHAKE_TIMEOUT))
                .map_err(|_| "cannot set socket timeout")?;
        }
        MaybeTlsStream::NativeTls(stream) => {
            stream
                .get_ref()
                .set_read_timeout(Some(POLL_INTERVAL))
                .map_err(|_| "cannot set socket timeout")?;
            stream
                .get_ref()
                .set_write_timeout(Some(HANDSHAKE_TIMEOUT))
                .map_err(|_| "cannot set socket timeout")?;
        }
        _ => return Err("unsupported TLS stream".into()),
    }

    Ok(socket)
}

/// Starts the ring workers, which outlive every connection of this daemon.
fn start_ringer(home: &Path) -> Ringer {
    let (jobs, queued) = mpsc::sync_channel::<(u64, Ping)>(RING_QUEUE);
    let (reports, outcomes) = mpsc::channel();
    let queued = Arc::new(Mutex::new(queued));
    let epoch = Arc::new(std::sync::atomic::AtomicU64::new(1));

    for _ in 0..RING_WORKERS {
        let queued = Arc::clone(&queued);
        let reports = reports.clone();
        let home = home.to_path_buf();
        let epoch = Arc::clone(&epoch);

        std::thread::spawn(move || {
            loop {
                let next = queued.lock().expect("ring queue lock").recv();
                let Ok((job_epoch, ping)) = next else { return };
                if epoch.load(std::sync::atomic::Ordering::Acquire) != job_epoch {
                    continue;
                }
                let outcome = if epoch.load(std::sync::atomic::Ordering::Acquire) != job_epoch {
                    continue;
                } else {
                    config::load(&home).and_then(|saved| {
                        let joined =
                            crate::api::Client::with_timeout(&saved.url, Duration::from_secs(2))
                                .sessions(&saved.token)
                                .map_err(|error| error.public_message())?;
                        if !joined.iter().any(|binding| binding.id == ping.to.id) {
                            return Err(
                                "recipient context has left; local delivery cancelled".into()
                            );
                        }

                        notify::ring(&home, &ping)
                    })
                };

                if reports.send((job_epoch, ping, outcome)).is_err() {
                    return;
                }
            }
        });
    }

    Ringer {
        jobs,
        outcomes,
        in_flight: HashSet::new(),
        accepted: VecDeque::new(),
        epoch,
    }
}

/// Shortens a failure reason to the contract's byte limit without splitting a character.
fn bounded_reason(reason: &str) -> String {
    let mut end = reason.len().min(limits::PING_FAIL_REASON_BYTES);
    while !reason.is_char_boundary(end) {
        end -= 1;
    }

    reason[..end].to_string()
}

/// Queues one ping for ringing; returns a frame only when the outcome is already known.
fn accept_ping(ringer: &mut Ringer, ping: Ping, active: &HashSet<String>) -> Option<ClientFrame> {
    let notification_id = ping.notification_id.clone();

    if ringer.accepted.contains(&notification_id) {
        log(&format!(
            "ping {notification_id} was already accepted; acknowledged again without ringing"
        ));

        return Some(ClientFrame::PingAck { notification_id });
    }

    if ringer.in_flight.contains(&notification_id) {
        log(&format!(
            "ping {notification_id} is already being rung; the repeat was not rung again"
        ));

        return None;
    }

    let local = active.contains(&ping.to.id);
    if !local {
        let outcome =
            Err("the recipient is not an active native session on this device".to_string());

        return Some(report_outcome(ringer, &ping, outcome));
    }

    ringer.in_flight.insert(notification_id);
    let epoch = ringer.epoch.load(std::sync::atomic::Ordering::Acquire);
    if ringer.jobs.try_send((epoch, ping.clone())).is_err() {
        ringer.in_flight.remove(&ping.notification_id);
        return Some(ClientFrame::PingFail {
            notification_id: ping.notification_id,
            reason: "local notification queue is full".into(),
        });
    }

    None
}

/// Records the outcome of one rung ping and returns the frame that reports it.
fn report_outcome(ringer: &mut Ringer, ping: &Ping, outcome: Result<(), String>) -> ClientFrame {
    let notification_id = ping.notification_id.clone();
    ringer.in_flight.remove(&notification_id);

    match outcome {
        Ok(()) => {
            if ringer.accepted.len() == ACCEPTED_MEMORY {
                ringer.accepted.pop_front();
            }
            ringer.accepted.push_back(notification_id.clone());
            log(&format!(
                "ping {notification_id} for {} submitted locally (replay: {})",
                ping.to.id, ping.replay
            ));

            ClientFrame::PingAck { notification_id }
        }
        Err(reason) => {
            log(&format!(
                "ping {notification_id} for {} failed: {reason}",
                ping.to.id
            ));

            ClientFrame::PingFail {
                notification_id,
                reason: bounded_reason(&reason),
            }
        }
    }
}

/// Sends one client frame; an error is the reason the connection ended.
fn send_frame(socket: &mut Socket, frame: &ClientFrame) -> Result<(), String> {
    let text = serde_json::to_string(frame).expect("client frames serialize");

    socket
        .send(Message::text(text))
        .map_err(|error| format!("connection lost while reporting a ping: {error}"))
}

/// Serves one connection until it ends, reading the socket while pings are rung; returns why it ended.
fn serve(
    socket: &mut Socket,
    ringer: &mut Ringer,
    config: &Config,
    updates: &mpsc::Receiver<Result<(Config, DaemonAdvertisement), String>>,
    advertisement: DaemonAdvertisement,
) -> (String, bool) {
    let mut last_heard = Instant::now();
    let mut active = HashSet::new();
    let mut last_refresh = Instant::now();
    if let Err(ended) = send_frame(
        socket,
        &ClientFrame::Advertise {
            daemon: advertisement,
        },
    ) {
        return (ended, false);
    }

    loop {
        loop {
            let update = match updates.try_recv() {
                Ok(update) => update,
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    return ("session discovery stopped".into(), false);
                }
            };
            let (current, advertisement) = match update {
                Ok(update) => update,
                Err(error) => {
                    ringer
                        .epoch
                        .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                    let _ = socket.close(None);
                    return (error, false);
                }
            };
            if current.url != config.url
                || current.token != config.token
                || current.device_id != config.device_id
            {
                ringer
                    .epoch
                    .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                let _ = socket.close(None);
                return ("credentials or server changed".into(), false);
            }
            active = advertisement
                .sessions
                .iter()
                .map(|identity| identity.id.clone())
                .collect();
            last_refresh = Instant::now();
            if let Err(ended) = send_frame(
                socket,
                &ClientFrame::Advertise {
                    daemon: advertisement,
                },
            ) {
                return (ended, false);
            }
        }
        if last_refresh.elapsed() > DISCOVERY_INTERVAL * 4 && !active.is_empty() {
            active.clear();
            let empty = DaemonAdvertisement {
                id: config.device_id.clone(),
                name: config.device_id.clone(),
                sessions: Vec::new(),
            };
            if let Err(ended) = send_frame(socket, &ClientFrame::Advertise { daemon: empty }) {
                return (ended, false);
            }
        }
        while let Ok((epoch, ping, outcome)) = ringer.outcomes.try_recv() {
            if epoch != ringer.epoch.load(std::sync::atomic::Ordering::Acquire) {
                continue;
            }
            let frame = report_outcome(ringer, &ping, outcome);

            if let Err(ended) = send_frame(socket, &frame) {
                return (ended, false);
            }
        }

        let received = socket.read();
        let timed_out = matches!(
            &received,
            Err(tungstenite::Error::Io(error))
                if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut)
        );

        if timed_out {
            if last_heard.elapsed() >= READ_TIMEOUT {
                return (
                    format!("connection lost: silent for {}s", READ_TIMEOUT.as_secs()),
                    false,
                );
            }

            continue;
        }
        last_heard = Instant::now();

        let text = match received {
            Ok(Message::Text(text)) => text,
            Ok(Message::Close(frame)) => {
                let code = frame.map(|frame| u16::from(frame.code));
                if code == Some(4003) {
                    ringer
                        .epoch
                        .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                }

                return (
                    format!("closed by the server with code {}", code.unwrap_or(1000)),
                    code == Some(4003),
                );
            }
            Ok(Message::Ping(payload)) => {
                if socket.send(Message::Pong(payload)).is_err() {
                    return ("pong failed".into(), false);
                }
                continue;
            }
            Ok(_) => continue,
            Err(error) => return (format!("connection lost: {error}"), false),
        };

        let reply = match serde_json::from_str::<ServerFrame>(&text) {
            Ok(ServerFrame::Ping(ping)) => accept_ping(ringer, ping, &active),
            Ok(ServerFrame::Error {
                code,
                notification_id,
            }) => {
                if matches!(code, ErrorCode::DeviceRevoked | ErrorCode::Unauthenticated) {
                    ringer
                        .epoch
                        .fetch_add(1, std::sync::atomic::Ordering::AcqRel);

                    return ("server rejected device credentials".into(), true);
                }
                log(&format!(
                    "server rejected a frame: {code:?} (notification {notification_id:?})"
                ));
                continue;
            }
            Err(error) => {
                log(&format!("ignored an unreadable server frame: {error}"));
                continue;
            }
        };

        if let Some(frame) = reply
            && let Err(ended) = send_frame(socket, &frame)
        {
            return (ended, false);
        }
    }
}

/// Runs the sole device socket and reloads saved credentials across reconnects.
fn run_daemon(home: &Path) -> Result<(), Exit> {
    let mut lock = acquire_lock(home).map_err(Exit::Failure)?;
    let mut ringer = start_ringer(home);
    let mut previous: Option<(String, String, String)> = None;
    let mut backoff = BACKOFF_MIN;
    loop {
        write_state(&mut lock, "disconnected");
        let config = config::load(home).map_err(Exit::Failure)?;
        let credential = (
            config.url.clone(),
            config.device_id.clone(),
            config.token.clone(),
        );
        if previous.as_ref() != Some(&credential) {
            ringer
                .epoch
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            ringer.in_flight.clear();
            ringer.accepted.clear();
            while ringer.outcomes.try_recv().is_ok() {}
            previous = Some(credential);
            backoff = BACKOFF_MIN;
        }
        match connect(&config) {
            Ok(mut socket) => {
                let connected_at = Instant::now();
                write_state(&mut lock, "connected");
                let (updates, receiver) = mpsc::sync_channel(2);
                let home = home.to_path_buf();
                let discovery_config = config::load(&home).map_err(Exit::Failure)?;
                let discovery_epoch = Arc::clone(&ringer.epoch);
                let epoch = discovery_epoch.load(std::sync::atomic::Ordering::Acquire);
                std::thread::spawn(move || {
                    while discovery_epoch.load(std::sync::atomic::Ordering::Acquire) == epoch {
                        let result = advertisement(&home, &discovery_config);
                        if matches!(
                            updates.try_send(result),
                            Err(mpsc::TrySendError::Disconnected(_))
                        ) {
                            break;
                        }
                        std::thread::sleep(DISCOVERY_INTERVAL);
                    }
                });
                let empty = DaemonAdvertisement {
                    id: config.device_id.clone(),
                    name: config.device_id.clone(),
                    sessions: Vec::new(),
                };
                let (reason, revoked) = serve(&mut socket, &mut ringer, &config, &receiver, empty);
                log(&reason);
                write_state(&mut lock, "disconnected");
                if revoked {
                    ringer
                        .epoch
                        .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                    log("credentials revoked; stopping retry");
                    return Ok(());
                }
                if connected_at.elapsed() >= BACKOFF_MAX {
                    backoff = BACKOFF_MIN;
                }
            }
            Err(error) => {
                log(&error);
                if error.contains("status 401") {
                    ringer
                        .epoch
                        .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                    return Ok(());
                }
            }
        }
        let mut random =
            File::open("/dev/urandom").map_err(|error| Exit::Failure(error.to_string()))?;
        let mut bytes = [0u8; 8];
        random
            .read_exact(&mut bytes)
            .map_err(|error| Exit::Failure(error.to_string()))?;
        let ceiling = backoff.as_millis() as u64;
        let until = Instant::now() + Duration::from_millis(1 + u64::from_le_bytes(bytes) % ceiling);
        while Instant::now() < until {
            let pause = until
                .saturating_duration_since(Instant::now())
                .min(POLL_INTERVAL);
            std::thread::sleep(pause);
            if let Ok(current) = config::load(home)
                && (current.url != config.url
                    || current.device_id != config.device_id
                    || current.token != config.token)
            {
                ringer
                    .epoch
                    .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                break;
            }
        }
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

/// Intersects registered bindings with current local provider activity in a background refresh.
fn advertisement(home: &Path, prior: &Config) -> Result<(Config, DaemonAdvertisement), String> {
    let config = config::load(home)?;
    if config.url != prior.url || config.device_id != prior.device_id || config.token != prior.token
    {
        return Err("credentials or server changed".into());
    }
    let owned = Client::new(&config.url)
        .sessions(&config.token)
        .map_err(|error| {
            log(&format!(
                "owned session refresh failed: {}; registration may be pending",
                error.public_message()
            ));
            error.public_message()
        })
        .unwrap_or_default();
    let (mut local, mut discovery_errors) = session::active(home);
    match crate::opencode::available(home, &owned) {
        Ok(idle) => local.extend(
            idle.into_iter()
                .filter(|session| !local.iter().any(|known| known.id == session.id))
                .collect::<Vec<_>>(),
        ),
        Err(error) => discovery_errors.push(error),
    }
    discovery_errors.extend(crate::opencode::reconcile(
        home,
        &config.url,
        &config.token,
        &local,
    ));
    for error in discovery_errors {
        log(&format!("native discovery failed: {error}"));
    }
    let unregistered = local
        .iter()
        .filter(|candidate| !owned.iter().any(|binding| binding.id == candidate.id))
        .count();
    if unregistered > 0 {
        log(&format!(
            "registration_pending: {unregistered} active native sessions lack an owned binding"
        ));
    }

    let sessions: Vec<Author> = local
        .into_iter()
        .filter(|candidate| owned.iter().any(|binding| binding.id == candidate.id))
        .collect();
    if sessions.len() > 1000 {
        return Err("daemon: more than 1000 active identities cannot be advertised".into());
    }

    let device_id = config.device_id.clone();
    Ok((
        config,
        DaemonAdvertisement {
            id: device_id.clone(),
            name: device_id,
            sessions,
        },
    ))
}

/// Reports whether a daemon is running and connected; a stopped daemon is a failure.
fn status(home: &Path) -> Result<(), Exit> {
    match running_daemon(home) {
        Some((pid, state)) => {
            println!("running ({state}, pid {pid})");

            Ok(())
        }
        None => Err(Exit::Failure("daemon: not running".to_string())),
    }
}

/// Ends launchd supervision first, so nothing restarts the daemon, then stops the process.
fn stop(home: &Path) -> Result<(), Exit> {
    launchd::unload()?;

    if let Some((pid, _)) = running_daemon(home)
        && pid > 1
    {
        // SAFETY: kill only sends a signal; the PID was recorded by the process holding the lock.
        unsafe { libc::kill(pid, libc::SIGTERM) };
    }

    let deadline = Instant::now() + STOP_TIMEOUT;
    while running_daemon(home).is_some() {
        if Instant::now() >= deadline {
            return Err(Exit::Failure(
                "daemon: still running after stop".to_string(),
            ));
        }

        std::thread::sleep(Duration::from_millis(50));
    }

    println!("stopped");

    Ok(())
}

/// Reinstalls and reloads the launchd job, then waits briefly for an accepted device socket.
pub(crate) fn restart(home: &Path) -> Result<(), Exit> {
    config::load(home).map_err(Exit::Failure)?;

    stop(home)?;
    launchd::install(home)?;

    let deadline = Instant::now() + STOP_TIMEOUT;
    while Instant::now() < deadline {
        if running_daemon(home).is_some_and(|(_, state)| state == "connected") {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    Err(Exit::Failure("daemon started, but device connection is pending; inspect daemon status and ~/Library/Logs/alter-bridge.log".into()))
}

/// Runs the notification daemon lifecycle command.
pub fn run(args: &[String], home: &Path) -> Result<(), Exit> {
    match args {
        [mode] if mode == "run" => run_daemon(home),
        [mode] if mode == "status" => status(home),
        [mode] if mode == "stop" => stop(home),
        [mode] if mode == "restart" => restart(home),
        [mode] if mode == "install" => launchd::install(home).map(|()| println!("installed")),
        [mode] if mode == "uninstall" => launchd::uninstall(home).map(|()| println!("uninstalled")),
        _ => Err(Exit::Usage(
            "usage: alter-bridge daemon run|status|stop|restart|install|uninstall\n".to_string(),
        )),
    }
}
