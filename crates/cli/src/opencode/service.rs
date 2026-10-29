//! Authenticated transport to the local OpenCode background service.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use base64::Engine;
use serde_json::Value;

/// Total bound for one local OpenCode request, including service URL resolution.
const TIMEOUT: Duration = Duration::from_secs(5);

/// Largest local OpenCode response accepted, so a misbehaving service cannot exhaust memory.
const RESPONSE_LIMIT: u64 = 4 * 1024 * 1024;

/// The authenticated loopback base URL of the running OpenCode background service.
pub(super) struct Service {
    url: String,
    authorization: String,
    agent: ureq::Agent,
}

/// Finds the OpenCode CLI on `PATH`, else at its standalone installer location, which launchd's PATH omits.
pub(super) fn opencode_bin(home: &Path) -> Option<PathBuf> {
    crate::session::find_on_path("opencode").or_else(|| {
        Some(home.join(".opencode/bin/opencode")).filter(|path| crate::session::is_executable(path))
    })
}

/// Returns OpenCode's private configuration directory, honoring `XDG_CONFIG_HOME`.
fn config_dir(home: &Path) -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home.join(".config"))
        .join("opencode")
}

/// Resolves the service URL with a bounded `opencode service status` and its password from the private service file.
pub(super) fn service(home: &Path) -> Result<Service, String> {
    let bin = opencode_bin(home).ok_or("OpenCode CLI is not installed")?;
    let mut child = Command::new(bin)
        .args(["service", "status"])
        .env("HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("OpenCode service unavailable: {error}"))?;
    let deadline = std::time::Instant::now() + TIMEOUT;
    let exited = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.success(),
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(25))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("OpenCode service status timed out".into());
            }
        }
    };
    let mut output = String::new();
    if let Some(stdout) = child.stdout.take() {
        use std::io::Read;
        let _ = stdout.take(4096).read_to_string(&mut output);
    }
    let url = output.trim().trim_end_matches('/').to_owned();
    let loopback = ["http://127.0.0.1:", "http://localhost:", "http://[::1]:"]
        .iter()
        .any(|prefix| {
            url.strip_prefix(prefix)
                .is_some_and(|port| !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()))
        });
    if !exited || !loopback {
        return Err("OpenCode background service is not running on loopback".into());
    }

    let raw = std::fs::read(config_dir(home).join("service.json"))
        .map_err(|_| "OpenCode service credentials are unreadable".to_string())?;
    let password = serde_json::from_slice::<Value>(&raw)
        .ok()
        .and_then(|value| value.get("password")?.as_str().map(str::to_owned))
        .filter(|password| !password.is_empty())
        .ok_or("OpenCode service credentials are invalid")?;
    let authorization = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("opencode:{password}"))
    );
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .http_status_as_error(false)
        .max_redirects(0)
        .build()
        .into();

    Ok(Service {
        url,
        authorization,
        agent,
    })
}

impl Service {
    /// Sends one authenticated request and returns the response once its status is 2xx.
    fn send(
        &self,
        method: &str,
        route: &str,
        body: Option<Value>,
    ) -> Result<ureq::http::Response<ureq::Body>, String> {
        let url = format!("{}{route}", self.url);
        let result = match (method, body) {
            ("GET", _) => self
                .agent
                .get(&url)
                .header("Authorization", &self.authorization)
                .call(),
            ("PUT", Some(body)) => self
                .agent
                .put(&url)
                .header("Authorization", &self.authorization)
                .send_json(body),
            ("POST", Some(body)) => self
                .agent
                .post(&url)
                .header("Authorization", &self.authorization)
                .send_json(body),
            _ => return Err("unsupported OpenCode request".into()),
        };
        let response = result.map_err(|_| format!("OpenCode {method} {route} failed"))?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(format!("OpenCode {method} {route} refused ({status})"));
        }

        Ok(response)
    }

    /// Sends one authenticated request and returns the `data` member of its JSON response.
    pub(super) fn call(
        &self,
        method: &str,
        route: &str,
        body: Option<Value>,
    ) -> Result<Value, String> {
        let value: Value = self
            .send(method, route, body)?
            .body_mut()
            .with_config()
            .limit(RESPONSE_LIMIT)
            .read_json()
            .map_err(|_| format!("OpenCode {method} {route} returned invalid JSON"))?;

        value
            .get("data")
            .cloned()
            .ok_or_else(|| format!("OpenCode {method} {route} returned no data"))
    }

    /// Sends one authenticated request whose success is its 2xx status alone; the body is not interpreted.
    pub(super) fn submit(&self, method: &str, route: &str, body: Value) -> Result<(), String> {
        self.send(method, route, Some(body)).map(|_| ())
    }
}
