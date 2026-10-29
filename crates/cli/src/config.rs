//! Private schema-v3 device configuration with guarded, atomic credential updates.

use std::ffi::CString;
use std::fs::{File, Metadata};
use std::io::{Read, Write};
use std::net::IpAddr;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use alter_bridge_protocol::limits;
use serde::{Deserialize, Serialize};

const VERSION: u64 = 3;
const MAX_CONFIG_BYTES: u64 = 1024 * 1024;
const CONFIG_NAME: &str = "alter-bridge.json";

/// Validated connected configuration, without native-session aliases.
pub struct Config {
    pub url: String,
    pub device_id: String,
    pub token: String,
    pub disconnect_key: String,
}

/// Persistent preferences; credentials are optional but must always form a complete pair.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConfigFile {
    pub version: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disconnect_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup: Option<serde_json::Value>,
}

/// Contents and filesystem identity used to detect concurrent replacements or edits.
#[derive(PartialEq)]
struct Snapshot {
    raw: Vec<u8>,
    device: u64,
    inode: u64,
    uid: u32,
    mode: u32,
    modified: (i64, i64),
    changed: (i64, i64),
}

/// An owned directory descriptor pins all configuration operations to the checked parent.
struct Directory {
    file: File,
    path: PathBuf,
}

/// One serialized edit, retaining the original snapshot until the final guarded rename.
pub(crate) struct Transaction {
    directory: Directory,
    lock: File,
    original: Option<Snapshot>,
    pub file: ConfigFile,
}

/// A failed write distinguishes untouched credentials from a rename without confirmed durability.
pub(crate) struct SaveError {
    pub written: bool,
    pub message: String,
}

impl ConfigFile {
    /// Creates a credential-free schema-v3 preference document.
    fn empty() -> Self {
        Self {
            version: VERSION,
            url: None,
            device_id: None,
            token: None,
            disconnect_key: None,
            setup: None,
        }
    }

    /// Validates preferences and secret pairing without printing private input values.
    fn validate(&mut self) -> Result<(), String> {
        if self.version != VERSION {
            return Err(format!(
                "config: unsupported version {}; this release requires version 3 and does not migrate existing files",
                self.version
            ));
        }
        if let Some(url) = &self.url {
            self.url = Some(check_url(url)?);
        }
        if self
            .device_id
            .as_deref()
            .is_some_and(|id| !limits::is_device_id(id))
        {
            return Err("config: device_id must be a lowercase slug of 1 to 128 bytes".into());
        }

        match (&self.token, &self.disconnect_key) {
            (None, None) => {}
            (Some(token), Some(key)) => {
                if self.url.is_none() || self.device_id.is_none() {
                    return Err("config: credentials require both url and device_id".into());
                }
                if !limits::is_device_token(token) || !limits::is_disconnect_key(key) {
                    return Err("config: token or disconnect_key has an invalid format".into());
                }
            }
            _ => {
                return Err(
                    "config: token and disconnect_key must be present or absent together".into(),
                );
            }
        }

        Ok(())
    }

    /// Requires saved registration credentials without contacting or activating the server.
    pub(crate) fn connected(&self) -> Result<Config, String> {
        let (Some(url), Some(device_id), Some(token), Some(disconnect_key)) = (
            &self.url,
            &self.device_id,
            &self.token,
            &self.disconnect_key,
        ) else {
            return Err("config: no saved device credentials; use alter-bridge connect device <url> --hexid <code> [--id <device>]".into());
        };

        Ok(Config {
            url: url.clone(),
            device_id: device_id.clone(),
            token: token.clone(),
            disconnect_key: disconnect_key.clone(),
        })
    }
}

/// Returns the canonical configuration path under the user's home.
pub fn path(home: &Path) -> PathBuf {
    home.join(".config").join(CONFIG_NAME)
}

/// Reports whether a host is loopback, the only allowed unencrypted destination.
fn is_loopback(host: &str) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');

    host == "localhost"
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

/// Validates a broker base URL without reflecting potentially secret components into errors.
pub(crate) fn check_url(url: &str) -> Result<String, String> {
    let uri: ureq::http::Uri = url
        .parse()
        .map_err(|_| "config: invalid broker URL".to_string())?;
    let host = uri.host().unwrap_or_default();
    if uri
        .authority()
        .is_some_and(|authority| authority.as_str().contains('@'))
        || uri.query().is_some()
        || url.contains('#')
    {
        return Err(
            "config: broker URL must not contain credentials, a query, or a fragment".into(),
        );
    }

    match uri.scheme_str() {
        Some("https") if !host.is_empty() => {}
        Some("http") if is_loopback(host) => {}
        _ => return Err("config: broker URL requires HTTPS, or HTTP on a loopback host".into()),
    }

    Ok(url.trim_end_matches('/').to_string())
}

/// Refuses special, shared, foreign-owned, or multiply linked credential and lock files.
fn private_file(metadata: &Metadata) -> Result<(), String> {
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    if !metadata.is_file()
        || metadata.uid() != uid
        || metadata.nlink() != 1
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(
            "config: config and lock must be private, owned regular files (mode 600)".into(),
        );
    }

    Ok(())
}

impl Directory {
    /// Opens a real owned parent, creating only an absent `.config` directory with mode 700.
    fn open(home: &Path, create: bool) -> Result<Self, String> {
        let path = home.join(".config");
        if create {
            match std::fs::DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(_) => {
                    return Err("config: cannot create the private configuration directory".into());
                }
            }
        }
        let file = File::options()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(&path)
            .map_err(|_| {
                "config: configuration parent must be a readable real directory".to_string()
            })?;
        let metadata = file
            .metadata()
            .map_err(|_| "config: cannot inspect configuration parent".to_string())?;
        // SAFETY: getuid has no preconditions and cannot fail.
        let uid = unsafe { libc::getuid() };
        if !metadata.is_dir() || metadata.uid() != uid || metadata.mode() & 0o022 != 0 {
            return Err(
                "config: configuration parent must be owned and not writable by other users".into(),
            );
        }

        Ok(Self { file, path })
    }

    /// Confirms the checked directory is still the named parent before credential replacement.
    fn unchanged(&self) -> Result<(), String> {
        let pinned = self
            .file
            .metadata()
            .map_err(|_| "config: cannot inspect parent".to_string())?;
        let current = std::fs::symlink_metadata(&self.path)
            .map_err(|_| "config: configuration parent changed; refusing to write".to_string())?;
        if !current.is_dir()
            || pinned.dev() != current.dev()
            || pinned.ino() != current.ino()
            || pinned.uid() != current.uid()
            || current.mode() & 0o022 != 0
        {
            return Err("config: configuration parent changed; refusing to write".into());
        }

        Ok(())
    }

    /// Opens one relative filename without following links or blocking on special files.
    fn open_file(&self, name: &str, flags: i32) -> std::io::Result<File> {
        let name = CString::new(name).expect("internal config filename has no NUL");
        // SAFETY: the descriptor and NUL-terminated filename are valid, with a mode supplied for creation.
        let fd = unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                name.as_ptr(),
                flags | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd == -1 {
            return Err(std::io::Error::last_os_error());
        }

        // SAFETY: openat returned an owned descriptor, transferred exactly once to File.
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    /// Reads bounded private bytes and inode identity from the pinned directory.
    fn snapshot(&self) -> Result<Option<Snapshot>, String> {
        let mut file = match self.open_file(CONFIG_NAME, libc::O_RDONLY) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err("config: cannot safely open configuration".into()),
        };
        let metadata = file
            .metadata()
            .map_err(|_| "config: cannot inspect configuration".to_string())?;
        private_file(&metadata)?;
        let mut raw = Vec::new();
        (&mut file)
            .take(MAX_CONFIG_BYTES + 1)
            .read_to_end(&mut raw)
            .map_err(|_| "config: cannot read configuration".to_string())?;
        if raw.len() as u64 > MAX_CONFIG_BYTES {
            return Err("config: configuration exceeds 1 MiB".into());
        }
        let after = file
            .metadata()
            .map_err(|_| "config: cannot recheck configuration".to_string())?;
        if metadata.uid() != after.uid()
            || metadata.mode() != after.mode()
            || metadata.nlink() != after.nlink()
            || metadata.len() != after.len()
            || (metadata.mtime(), metadata.mtime_nsec()) != (after.mtime(), after.mtime_nsec())
            || (metadata.ctime(), metadata.ctime_nsec()) != (after.ctime(), after.ctime_nsec())
        {
            return Err("config: configuration changed while being read; retry".into());
        }

        Ok(Some(Snapshot {
            raw,
            device: metadata.dev(),
            inode: metadata.ino(),
            uid: metadata.uid(),
            mode: metadata.mode(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        }))
    }

    /// Replaces the destination inside the pinned directory without following the old entry.
    fn replace(&self, temporary: &str) -> Result<(), String> {
        let temporary = CString::new(temporary).expect("internal temporary filename has no NUL");
        let destination = CString::new(CONFIG_NAME).expect("internal config filename has no NUL");
        // SAFETY: both names are valid relative paths in the held directory descriptor.
        let result = unsafe {
            libc::renameat(
                self.file.as_raw_fd(),
                temporary.as_ptr(),
                self.file.as_raw_fd(),
                destination.as_ptr(),
            )
        };
        if result != 0 {
            return Err("config: cannot atomically replace configuration".into());
        }

        Ok(())
    }

    /// Removes only this edit's temporary filename from the pinned parent.
    fn remove_temporary(&self, name: &str) {
        let name = CString::new(name).expect("internal temporary filename has no NUL");
        // SAFETY: the name is a relative single file within the held directory descriptor.
        unsafe {
            libc::unlinkat(self.file.as_raw_fd(), name.as_ptr(), 0);
        }
    }
}

/// Decodes a private snapshot without echoing invalid schema values into diagnostics.
fn decode(snapshot: Option<&Snapshot>) -> Result<ConfigFile, String> {
    let Some(snapshot) = snapshot else {
        return Ok(ConfigFile::empty());
    };
    let mut file: ConfigFile = serde_json::from_slice(&snapshot.raw)
        .map_err(|_| "config: invalid schema; expected version 3 preferences and paired device credentials, without tooling aliases (no automatic migration)".to_string())?;
    file.validate()?;

    Ok(file)
}

impl Transaction {
    /// Takes the config lock and validates the original bytes before any remote operation.
    pub(crate) fn open(home: &Path) -> Result<Self, String> {
        let directory = Directory::open(home, true)?;
        let lock = directory
            .open_file(".alter-bridge-config.lock", libc::O_RDWR | libc::O_CREAT)
            .map_err(|_| "config: cannot safely open configuration lock".to_string())?;
        private_file(
            &lock
                .metadata()
                .map_err(|_| "config: cannot inspect configuration lock".to_string())?,
        )?;
        lock.try_lock().map_err(|_| {
            "config: another configuration command is running; retry when it finishes".to_string()
        })?;
        directory.unchanged()?;
        let original = directory.snapshot()?;
        let file = decode(original.as_ref())?;

        Ok(Self {
            directory,
            lock,
            original,
            file,
        })
    }

    /// Persists validated preferences, refusing concurrent changes before the atomic rename.
    pub(crate) fn save(&mut self) -> Result<(), SaveError> {
        let mut written = false;
        let result = (|| -> Result<(), String> {
            self.file.validate()?;
            let mut bytes = serde_json::to_vec_pretty(&self.file)
                .map_err(|_| "config: cannot serialize configuration".to_string())?;
            bytes.push(b'\n');
            if bytes.len() as u64 > MAX_CONFIG_BYTES {
                return Err("config: configuration exceeds 1 MiB".into());
            }
            let temporary = format!(
                ".alter-bridge-{}-{}.tmp",
                std::process::id(),
                random_device_id()?
            );
            let mut file = self
                .directory
                .open_file(&temporary, libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL)
                .map_err(|_| "config: cannot create private temporary configuration".to_string())?;
            let saved = (|| -> Result<(), String> {
                file.set_permissions(std::fs::Permissions::from_mode(0o600))
                    .map_err(|_| {
                        "config: cannot set private configuration permissions".to_string()
                    })?;
                private_file(
                    &file.metadata().map_err(|_| {
                        "config: cannot inspect temporary configuration".to_string()
                    })?,
                )?;
                file.write_all(&bytes)
                    .and_then(|()| file.flush())
                    .and_then(|()| file.sync_all())
                    .map_err(|_| "config: cannot persist temporary configuration".to_string())?;
                self.directory.unchanged()?;
                if self.directory.snapshot()? != self.original {
                    return Err("config: configuration changed during the request; refusing to overwrite it".into());
                }
                private_file(
                    &self
                        .lock
                        .metadata()
                        .map_err(|_| "config: cannot inspect configuration lock".to_string())?,
                )?;
                let named_lock = self
                    .directory
                    .open_file(".alter-bridge-config.lock", libc::O_RDONLY)
                    .and_then(|file| file.metadata())
                    .map_err(|_| {
                        "config: configuration lock changed; refusing to write".to_string()
                    })?;
                let held_lock = self
                    .lock
                    .metadata()
                    .map_err(|_| "config: cannot inspect held lock".to_string())?;
                if named_lock.dev() != held_lock.dev() || named_lock.ino() != held_lock.ino() {
                    return Err("config: configuration lock changed; refusing to write".into());
                }
                self.directory.replace(&temporary)?;
                written = true;
                self.directory.unchanged().map_err(|_| "config: configuration was replaced in its original parent, but the named directory changed".to_string())?;
                self.directory.file.sync_all().map_err(|_| "config: configuration was replaced, but directory durability could not be confirmed".to_string())?;
                let current = self.directory.snapshot()?;
                if current.as_ref().map(|snapshot| snapshot.raw.as_slice())
                    != Some(bytes.as_slice())
                {
                    return Err(
                        "config: configuration changed immediately after replacement".into(),
                    );
                }
                self.original = current;

                Ok(())
            })();
            self.directory.remove_temporary(&temporary);
            saved
        })();

        result.map_err(|message| SaveError { written, message })
    }
}

/// Reads preferences without creating files, taking a write lock, or touching the network.
pub(crate) fn preferences(home: &Path) -> Result<ConfigFile, String> {
    let directory = Directory::open(home, false)?;
    let original = directory.snapshot()?;
    directory.unchanged()?;

    decode(original.as_ref())
}

/// Loads a complete registration without filesystem or network mutation.
pub fn load(home: &Path) -> Result<Config, String> {
    preferences(home)?.connected()
}

/// Generates an unbiased eight-character device label from operating-system randomness.
pub(crate) fn random_device_id() -> Result<String, String> {
    let mut source = File::open("/dev/urandom")
        .map_err(|_| "config: operating-system randomness unavailable".to_string())?;
    let alphabet = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut id = String::with_capacity(8);

    while id.len() < 8 {
        let mut byte = [0u8];
        source
            .read_exact(&mut byte)
            .map_err(|_| "config: operating-system randomness failed".to_string())?;
        if byte[0] < 252 {
            id.push(alphabet[usize::from(byte[0]) % alphabet.len()] as char);
        }
    }

    Ok(id)
}
