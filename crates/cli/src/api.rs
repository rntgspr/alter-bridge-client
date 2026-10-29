//! Bounded device-control HTTP requests with private credentials and masked diagnostics.

use std::time::Duration;

use alter_bridge_protocol::Author;
use alter_bridge_protocol::{
    DeviceConnectRequest, DeviceConnectResponse, DeviceDisconnectRequest, DeviceDisconnectResponse,
    DeviceInboxCheckRequest, DeviceSessionRequest, DeviceSessionResponse, DeviceStatusResponse,
    DirectoryResponse, ErrorBody, ErrorCode, InboxCheckResponse, limits,
};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use ureq::tls::{RootCerts, TlsConfig, TlsProvider};

const RESPONSE_BYTES: u64 = 64 * 1024;

/// A private transport failure or a bounded server status with only a typed public code.
pub(crate) enum CallError {
    Transport,
    Timeout,
    Tls,
    InvalidResponse,
    Status {
        status: u16,
        code: Option<ErrorCode>,
    },
}

impl From<ureq::Error> for CallError {
    /// Retains public transport categories while discarding private underlying diagnostics.
    fn from(error: ureq::Error) -> Self {
        match error {
            ureq::Error::Timeout(_) => Self::Timeout,
            ureq::Error::Io(error) if error.kind() == std::io::ErrorKind::TimedOut => Self::Timeout,
            ureq::Error::Tls(_) | ureq::Error::NativeTls(_) => Self::Tls,
            _ => Self::Transport,
        }
    }
}

impl CallError {
    /// Separates response decoding failures from observed timeout and TLS failures.
    fn decode(error: ureq::Error) -> Self {
        match Self::from(error) {
            Self::Transport => Self::InvalidResponse,
            error => error,
        }
    }

    /// Distinguishes definite client refusals from failures that may follow a committed issuance.
    pub(crate) fn issuance_unknown(&self) -> bool {
        !matches!(
            self,
            Self::Status {
                status: 400..=499,
                ..
            }
        )
    }

    /// Renders only known protocol codes, never URLs, request bodies, secrets, or server prose.
    pub(crate) fn public_message(&self) -> String {
        match self {
            Self::Transport => "connection request failed; cause unknown".into(),
            Self::Timeout => "request timed out".into(),
            Self::Tls => "TLS verification or handshake failed".into(),
            Self::InvalidResponse => "server response was invalid or unreadable".into(),
            Self::Status { status, code } => {
                let code = code
                    .and_then(|code| serde_json::to_value(code).ok())
                    .and_then(|code| code.as_str().map(str::to_owned))
                    .unwrap_or_else(|| "unexpected_status".into());

                format!("server refused ({status}, {code})")
            }
        }
    }
}

/// A client for one broker base URL; it never retries issuance or follows redirects.
pub(crate) struct Client {
    url: String,
    agent: ureq::Agent,
}

impl Client {
    /// Reads a public readiness route without credentials, redirects, or unbounded response data.
    pub(crate) fn public_status(&self, route: &str, expected: &str) -> Result<bool, CallError> {
        let mut response = self
            .agent
            .get(format!("{}{route}", self.url))
            .call()
            .map_err(CallError::from)?;
        if response.status().as_u16() != 200 {
            return Ok(false);
        }
        let body: serde_json::Value = response
            .body_mut()
            .with_config()
            .limit(1024)
            .read_json()
            .map_err(CallError::decode)?;
        Ok(body.get("status").and_then(serde_json::Value::as_str) == Some(expected))
    }
    /// Configures native platform trust and a ten-second global bound for each control request.
    pub(crate) fn new(url: &str) -> Self {
        Self::with_timeout(url, Duration::from_secs(10))
    }

    /// Configures one control client with a caller-owned total request deadline.
    pub(crate) fn with_timeout(url: &str, timeout: Duration) -> Self {
        let agent = ureq::Agent::config_builder()
            .tls_config(
                TlsConfig::builder()
                    .provider(TlsProvider::NativeTls)
                    .root_certs(RootCerts::PlatformVerifier)
                    .build(),
            )
            .timeout_global(Some(timeout))
            .http_status_as_error(false)
            .max_redirects(0)
            .build()
            .into();

        Self {
            url: url.into(),
            agent,
        }
    }

    /// Requests one nonowning issuance without authenticating with the returned credential.
    pub(crate) fn issue(
        &self,
        request: &DeviceConnectRequest,
    ) -> Result<DeviceConnectResponse, CallError> {
        let response = self
            .agent
            .post(format!("{}/device/connect", self.url))
            .send_json(request)
            .map_err(CallError::from)?;

        read_response(response, 201)
    }

    /// Looks up an already saved token without activating it or claiming a socket.
    pub(crate) fn status(&self, token: &str) -> Result<DeviceStatusResponse, CallError> {
        let response = self
            .agent
            .get(format!("{}/device/status", self.url))
            .header("Authorization", format!("Bearer {token}"))
            .call()
            .map_err(CallError::from)?;

        read_response(response, 200)
    }

    /// Reads only bindings owned by this active device, without activating credentials.
    pub(crate) fn sessions(&self, token: &str) -> Result<Vec<Author>, CallError> {
        #[derive(Deserialize)]
        struct Response {
            sessions: Vec<Author>,
        }

        let response = self
            .agent
            .get(format!("{}/device/sessions", self.url))
            .header("Authorization", format!("Bearer {token}"))
            .call()
            .map_err(CallError::from)?;

        read_response_with_limit::<Response>(response, 200, 1024 * 1024).map(|body| body.sessions)
    }

    /// Reads the shared directory with the active device bearer and its explicit size ceiling.
    pub(crate) fn directory(&self, token: &str) -> Result<DirectoryResponse, CallError> {
        let response = self
            .agent
            .get(format!("{}/device/directory", self.url))
            .header("Authorization", format!("Bearer {token}"))
            .call()
            .map_err(CallError::from)?;

        read_response_with_limit(response, 200, limits::DIRECTORY_RESPONSE_BYTES as u64)
    }

    /// Registers the native hook identity using the active device credential.
    pub(crate) fn register_session(
        &self,
        token: &str,
        request: &DeviceSessionRequest,
    ) -> Result<DeviceSessionResponse, CallError> {
        let response = self
            .agent
            .post(format!("{}/device/sessions", self.url))
            .header("Authorization", format!("Bearer {token}"))
            .send_json(request)
            .map_err(CallError::from)?;

        read_response(response, 200)
    }

    /// Explicitly leaves one owned context without revoking the device credential.
    pub(crate) fn leave_session(
        &self,
        token: &str,
        actor: &alter_bridge_protocol::Identity,
    ) -> Result<serde_json::Value, CallError> {
        let response = self
            .agent
            .delete(format!("{}/device/sessions", self.url))
            .header("Authorization", format!("Bearer {token}"))
            .force_send_body()
            .send_json(actor)
            .map_err(CallError::from)?;

        read_response(response, 200)
    }

    /// Checks the oldest unread notice without reading or acknowledging its message.
    pub(crate) fn inbox_check(
        &self,
        token: &str,
        request: &DeviceInboxCheckRequest,
    ) -> Result<InboxCheckResponse, CallError> {
        let response = self
            .agent
            .post(format!("{}/device/inbox/check", self.url))
            .header("Authorization", format!("Bearer {token}"))
            .send_json(request)
            .map_err(CallError::from)?;

        read_response(response, 200)
    }

    /// Revokes only the registration selected by the saved disconnect key and current pairing code.
    pub(crate) fn disconnect(
        &self,
        request: &DeviceDisconnectRequest,
    ) -> Result<DeviceDisconnectResponse, CallError> {
        let response = self
            .agent
            .post(format!("{}/device/disconnect", self.url))
            .send_json(request)
            .map_err(CallError::from)?;

        read_response(response, 200)
    }
}

/// Decodes only the expected successful shape or a typed public error, with a bounded body.
fn read_response<T: DeserializeOwned>(
    response: ureq::http::Response<ureq::Body>,
    expected: u16,
) -> Result<T, CallError> {
    read_response_with_limit(response, expected, RESPONSE_BYTES)
}

/// Decodes a control response with the route's documented byte ceiling.
fn read_response_with_limit<T: DeserializeOwned>(
    mut response: ureq::http::Response<ureq::Body>,
    expected: u16,
    limit: u64,
) -> Result<T, CallError> {
    let status = response.status().as_u16();
    if status != expected {
        let code = response
            .body_mut()
            .with_config()
            .limit(RESPONSE_BYTES)
            .read_json::<ErrorBody>()
            .ok()
            .map(|body| body.error.code);

        return Err(CallError::Status { status, code });
    }

    response
        .body_mut()
        .with_config()
        .limit(limit)
        .read_json()
        .map_err(CallError::decode)
}
