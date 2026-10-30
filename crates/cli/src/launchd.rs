//! macOS LaunchAgent registration of the user daemon, owned by the client executable.

use std::fs::File;
use std::io::{ErrorKind, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::Exit;

/// launchd label of the user daemon.
pub(crate) const LABEL: &str = "com.rntgspr.alter-bridge";

/// Executable search path given to the daemon, which launchd starts without a login shell.
const SEARCH_PATH: &str = "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";

/// Returns the LaunchAgent definition path of the user daemon.
pub(crate) fn plist_path(home: &Path) -> PathBuf {
    home.join(format!("Library/LaunchAgents/{LABEL}.plist"))
}

/// Returns the log file launchd appends the daemon's output to.
fn log_path(home: &Path) -> PathBuf {
    home.join("Library/Logs/alter-bridge.log")
}

/// Returns the launchd service target of the user daemon in the current GUI domain.
fn service_target() -> String {
    format!("{}/{LABEL}", domain())
}

/// Returns the current user's GUI launchd domain.
fn domain() -> String {
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };

    format!("gui/{uid}")
}

/// Runs launchctl with the given arguments and returns whether it succeeded, keeping its stderr.
fn launchctl(args: &[&str]) -> Result<(bool, String), Exit> {
    let output = Command::new("launchctl")
        .args(args)
        .output()
        .map_err(|error| Exit::Failure(format!("daemon: cannot run launchctl: {error}")))?;

    Ok((
        output.status.success(),
        String::from_utf8_lossy(&output.stderr).trim().to_string(),
    ))
}

/// Returns whether launchd currently has the user daemon loaded.
fn loaded() -> Result<bool, Exit> {
    launchctl(&["print", &service_target()]).map(|(ok, _)| ok)
}

/// Unloads the user daemon from launchd when loaded, so nothing relaunches it; the definition stays.
pub(crate) fn unload() -> Result<(), Exit> {
    if !loaded()? {
        return Ok(());
    }

    let (ok, stderr) = launchctl(&["bootout", &service_target()])?;
    if !ok && loaded()? {
        return Err(Exit::Failure(format!(
            "daemon: launchctl bootout failed: {stderr}"
        )));
    }

    Ok(())
}

/// Escapes text for an XML element body or attribute.
fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Refuses a symlink or a path owned by another user; an absent path is accepted.
fn require_owned(path: &Path, directory: bool) -> Result<(), Exit> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(Exit::Failure(format!(
                "daemon: cannot inspect {}: {error}",
                path.display()
            )));
        }
    };
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    let kind_ok = if directory {
        metadata.is_dir()
    } else {
        metadata.is_file()
    };

    if metadata.file_type().is_symlink() || !kind_ok || metadata.uid() != uid {
        return Err(Exit::Failure(format!(
            "daemon: {} must be a real {} owned by the current user",
            path.display(),
            if directory { "directory" } else { "file" }
        )));
    }

    Ok(())
}

/// Creates a missing private directory, then checks it is a real directory owned by the user.
fn ensure_directory(path: &Path) -> Result<(), Exit> {
    require_owned(path, true)?;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .map_err(|error| {
            Exit::Failure(format!("daemon: cannot create {}: {error}", path.display()))
        })?;

    require_owned(path, true)
}

/// Refuses an existing definition at the managed path that does not describe this daemon.
fn require_managed_definition(plist: &Path) -> Result<bool, Exit> {
    require_owned(plist, false)?;

    match std::fs::read_to_string(plist) {
        Ok(text) if text.contains(&format!("<string>{LABEL}</string>")) => Ok(true),
        Ok(_) => Err(Exit::Failure(format!(
            "daemon: {} is not an alter-bridge LaunchAgent; refusing to replace it",
            plist.display()
        ))),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(Exit::Failure(format!(
            "daemon: cannot read {}: {error}",
            plist.display()
        ))),
    }
}

/// Returns absolute provider root overrides to pass to the daemon, refusing relative ones.
fn provider_roots() -> Result<Vec<(&'static str, String)>, Exit> {
    let mut roots = Vec::new();

    for name in ["CODEX_HOME", "CLAUDE_CONFIG_DIR"] {
        let Some(value) = std::env::var_os(name).filter(|value| !value.is_empty()) else {
            continue;
        };
        let value = value
            .into_string()
            .map_err(|_| Exit::Failure(format!("daemon: {name} must be valid UTF-8")))?;
        if !value.starts_with('/') {
            return Err(Exit::Failure(format!("daemon: {name} must be absolute")));
        }
        roots.push((name, value));
    }

    Ok(roots)
}

/// Returns the executable the LaunchAgent starts: the stable current path when it is this binary.
fn launch_binary(home: &Path) -> Result<PathBuf, Exit> {
    let executable = std::env::current_exe()
        .and_then(|path| path.canonicalize())
        .map_err(|error| Exit::Failure(format!("daemon: cannot resolve executable: {error}")))?;
    let stable = home.join(".local/share/alter-bridge/current/bin/alter-bridge");

    if stable
        .canonicalize()
        .is_ok_and(|resolved| resolved == executable)
    {
        Ok(stable)
    } else {
        Ok(executable)
    }
}

/// Renders the LaunchAgent definition that runs `daemon run` with crash-only supervision.
fn render(home: &Path, binary: &Path) -> Result<String, Exit> {
    let binary = binary
        .to_str()
        .ok_or_else(|| Exit::Failure("daemon: executable path must be valid UTF-8".into()))?;
    let log = log_path(home);
    let log = log
        .to_str()
        .ok_or_else(|| Exit::Failure("daemon: log path must be valid UTF-8".into()))?;
    let search_path = format!("{}/.local/bin:{SEARCH_PATH}", home.display());
    let mut environment = format!(
        "<key>PATH</key><string>{}</string>",
        xml_escape(&search_path)
    );

    for (name, value) in provider_roots()? {
        environment.push_str(&format!(
            "<key>{name}</key><string>{}</string>",
            xml_escape(&value)
        ));
    }

    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>{LABEL}</string>
<key>ProgramArguments</key><array><string>{binary}</string><string>daemon</string><string>run</string></array>
<key>RunAtLoad</key><true/>
<key>KeepAlive</key><dict><key>Crashed</key><true/></dict>
<key>EnvironmentVariables</key><dict>{environment}</dict>
<key>StandardOutPath</key><string>{log}</string>
<key>StandardErrorPath</key><string>{log}</string>
</dict></plist>
"#,
        binary = xml_escape(binary),
        log = xml_escape(log),
    ))
}

/// Writes or reconciles the user's LaunchAgent atomically and loads it; reports registration only.
pub(crate) fn install(home: &Path) -> Result<(), Exit> {
    let plist = plist_path(home);
    let agents = plist.parent().expect("plist path has a parent");
    let logs = home.join("Library/Logs");

    ensure_directory(agents)?;
    ensure_directory(&logs)?;
    require_owned(&log_path(home), false)?;
    require_managed_definition(&plist)?;

    let contents = render(home, &launch_binary(home)?)?;
    let temporary = agents.join(format!(".{LABEL}.{}.tmp", std::process::id()));
    let written = File::options()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&temporary)
        .and_then(|mut file| {
            file.write_all(contents.as_bytes())
                .and_then(|()| file.sync_all())
        });
    if let Err(error) = written {
        let _ = std::fs::remove_file(&temporary);
        return Err(Exit::Failure(format!(
            "daemon: cannot write {}: {error}",
            temporary.display()
        )));
    }

    let linted = Command::new("plutil").arg("-lint").arg(&temporary).output();
    if !linted.as_ref().is_ok_and(|output| output.status.success()) {
        let _ = std::fs::remove_file(&temporary);
        return Err(Exit::Failure(
            "daemon: generated LaunchAgent failed plutil -lint".into(),
        ));
    }

    if let Err(error) = unload() {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }
    std::fs::rename(&temporary, &plist).map_err(|error| {
        let _ = std::fs::remove_file(&temporary);
        Exit::Failure(format!(
            "daemon: cannot replace {}: {error}",
            plist.display()
        ))
    })?;

    let plist_arg = plist.to_string_lossy();
    let (ok, stderr) = launchctl(&["bootstrap", &domain(), &plist_arg])?;
    if !ok {
        return Err(Exit::Failure(format!(
            "daemon: launchctl bootstrap failed: {stderr}"
        )));
    }

    Ok(())
}

/// Unloads the user daemon and removes only its owned LaunchAgent; state, credentials and logs stay.
pub(crate) fn uninstall(home: &Path) -> Result<(), Exit> {
    let plist = plist_path(home);
    let present = require_managed_definition(&plist)?;

    unload()?;

    if present {
        std::fs::remove_file(&plist).map_err(|error| {
            Exit::Failure(format!(
                "daemon: cannot remove {}: {error}",
                plist.display()
            ))
        })?;
    }

    Ok(())
}
