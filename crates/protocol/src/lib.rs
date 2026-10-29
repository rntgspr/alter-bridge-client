//! Shared message, device lifecycle and notification protocol types.

use serde::{Deserialize, Serialize};

/// Size limits of the contract.
pub mod limits {
    /// Largest shared directory response in bytes.
    pub const DIRECTORY_RESPONSE_BYTES: usize = 1_048_576;
    /// Largest accepted HTTP request body, in bytes.
    pub const REQUEST_BODY_BYTES: usize = 1_048_576;

    /// Longest `message_id`, `thread`, or `in_reply_to`, in characters.
    pub const TOKEN_FIELD_CHARS: usize = 128;

    /// Longest identity `id`, in bytes.
    pub const IDENTITY_ID_BYTES: usize = 256;

    /// Longest binding `name`, in bytes.
    pub const BINDING_NAME_BYTES: usize = 128;

    /// Default and maximum inbox `limit`.
    pub const INBOX_LIMIT: u32 = 100;

    /// Content size after which an inbox response stops adding messages, in bytes.
    pub const INBOX_CONTENT_BYTES: usize = 4 * 1_048_576;

    /// Most `ack` entries in one read acknowledgement or inbox response.
    pub const ACK_ENTRIES: usize = 1000;

    /// First resend delay after a `ping_fail`, in seconds.
    pub const RESEND_DELAY_SECS: u64 = 5;

    /// Largest resend delay after repeated `ping_fail` frames, in seconds.
    pub const RESEND_DELAY_MAX_SECS: u64 = 60;

    /// Largest WebSocket frame in either direction, in bytes.
    pub const WEBSOCKET_FRAME_BYTES: usize = REQUEST_BODY_BYTES;

    /// Longest `ping_fail.reason`, in bytes.
    pub const PING_FAIL_REASON_BYTES: usize = 256;

    /// Most pending notifications kept per recipient identity.
    pub const PENDING_NOTIFICATIONS: usize = 100;

    /// Interval between server WebSocket pings, in seconds.
    pub const WEBSOCKET_PING_SECS: u64 = 30;

    /// Length of the startup pairing code, in lowercase hexadecimal characters.
    pub const PAIRING_CODE_CHARS: usize = 6;

    /// Failed pairing code attempts accepted per sliding minute, across all clients.
    pub const PAIRING_FAILURES_PER_MINUTE: usize = 5;

    /// Checks the pairing code form: exactly six lowercase hexadecimal characters.
    pub fn is_pairing_code(value: &str) -> bool {
        value.len() == PAIRING_CODE_CHARS
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }

    /// Validates a device label of lowercase alphanumeric segments separated by single hyphens.
    pub fn is_device_id(value: &str) -> bool {
        !value.is_empty()
            && value.len() <= 128
            && value.split('-').all(|segment| {
                !segment.is_empty()
                    && segment
                        .bytes()
                        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
            })
    }

    /// Checks the private device bearer representation without accepting other credentials.
    pub fn is_device_token(value: &str) -> bool {
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }

    /// Checks the canonical lowercase UUID-v4 disconnect key emitted by the server.
    pub fn is_disconnect_key(value: &str) -> bool {
        value.len() == 36
            && value.bytes().enumerate().all(|(index, byte)| match index {
                8 | 13 | 18 | 23 => byte == b'-',
                14 => byte == b'4',
                19 => matches!(byte, b'8' | b'9' | b'a' | b'b'),
                _ => byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte),
            })
    }
}

/// One participating session with independent device and session presence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectoryEntry {
    pub binding: Author,
    pub device_id: String,
    pub connected: bool,
    pub active: bool,
}

/// Shared directory result for HTTP and MCP callers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectoryResponse {
    pub sessions: Vec<DirectoryEntry>,
}

/// Credential issuance request; issuance alone does not claim the device label.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceConnectRequest {
    pub code: String,
    pub device_id: String,
}

/// One issuance response containing secrets that are never retained in plaintext server-side.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceConnectResponse {
    pub device_id: String,
    pub generation: u64,
    pub token: String,
    pub disconnect_key: String,
}

/// Device registration lifecycle, independent of the accepted notification socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceState {
    Issued,
    Active,
    Revoked,
}

/// A nonactivating credential status without any private material.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceStatusResponse {
    pub device_id: String,
    pub generation: u64,
    pub state: DeviceState,
    pub connected: bool,
}

/// Generation-specific revocation authenticated with the current pairing code and private key.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceDisconnectRequest {
    pub code: String,
    pub device_id: String,
    pub disconnect_key: String,
}

/// Stable acknowledgement returned again when the same generation's disconnect is retried.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceDisconnectResponse {
    pub revoked_generation: u64,
}

/// Durable participant key containing one opaque session ID.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub id: String,
}

/// Author snapshot stored with a message or registered to a device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Author {
    pub id: String,
    pub name: String,
}

impl Author {
    /// Validates opaque session ID and display-name lengths and rejects control characters.
    pub fn is_valid(&self) -> bool {
        !self.id.is_empty()
            && self.id.len() <= limits::IDENTITY_ID_BYTES
            && !self.id.chars().any(char::is_control)
            && !self.name.is_empty()
            && self.name.len() <= limits::BINDING_NAME_BYTES
            && !self.name.chars().any(char::is_control)
    }

    /// Returns the identity key of the author, without the display name.
    pub fn identity(&self) -> Identity {
        Identity {
            id: self.id.clone(),
        }
    }
}

/// Kind of a message, stored in its content frontmatter.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageType {
    #[default]
    Message,
    Question,
    Result,
    Ack,
}

impl MessageType {
    /// Returns the wire spelling of the message type.
    pub fn as_str(self) -> &'static str {
        match self {
            MessageType::Message => "message",
            MessageType::Question => "question",
            MessageType::Result => "result",
            MessageType::Ack => "ack",
        }
    }
}

/// Arguments of the MCP send tool, also used by the store after actor authorization.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SendRequest {
    pub actor: Identity,
    pub to: Identity,
    pub message_id: String,
    #[serde(rename = "type", default)]
    pub message_type: MessageType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_reply_to: Option<String>,
    pub body: String,
}

/// Result of committing a message or reusing its idempotency record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendResponse {
    pub message_id: String,
    pub room: String,
    pub position: u64,
    pub ts: String,
    pub duplicate: bool,
}

/// One unread message's notice metadata, or no pending message; never an acknowledgement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboxCheckResponse {
    pub ping: Option<Ping>,
}

/// Native hook registration, scoped to the authenticating device generation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceSessionRequest {
    pub id: String,
    pub name: String,
}

/// The registered actor and the device that owns it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceSessionResponse {
    pub actor: Identity,
    pub device_id: String,
}

/// Metadata-only prompt check for one registered actor.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceInboxCheckRequest {
    pub actor: Identity,
}

/// Bounded inbox retrieval or nonconsuming peek request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InboxRequest {
    pub actor: Identity,
    pub limit: Option<u32>,
}

/// One pending message of an inbox response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboxMessage {
    pub room: String,
    pub position: u64,
    pub author: Author,
    pub content: String,
}

/// Consumed position of one room, as acknowledged or as returned by a read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AckEntry {
    pub room: String,
    pub position: u64,
}

/// Selected inbox page and its cursor boundary; inbox commits it before success, peek does not.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboxResponse {
    pub messages: Vec<InboxMessage>,
    pub ack: Vec<AckEntry>,
    pub more: bool,
}

/// Stable daemon metadata and the local identities currently active on it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DaemonAdvertisement {
    pub id: String,
    pub name: String,
    pub sessions: Vec<Author>,
}

/// Distinguishes unread-message notices from server-authoritative status facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeKind {
    Message,
    Receipt,
}

/// Independent durable facts about one exact message and its original recipient.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReceiptFacts {
    pub recipient: Identity,
    pub sent_at: String,
    pub delivered_at: Option<String>,
    pub read_at: Option<String>,
}

/// Metadata-only message or receipt notice sent on the notification WebSocket.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ping {
    pub notice_kind: NoticeKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receipt: Option<ReceiptFacts>,
    pub notification_id: String,
    pub to: Identity,
    pub author: Author,
    pub message_id: String,
    pub message_type: MessageType,
    pub room: String,
    pub position: u64,
    pub replay: bool,
}

/// Frame sent by the server on the notification WebSocket.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerFrame {
    Ping(Ping),
    Error {
        code: ErrorCode,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        notification_id: Option<String>,
    },
}

/// Frame sent by the daemon on the notification WebSocket.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClientFrame {
    Advertise {
        daemon: DaemonAdvertisement,
    },
    PingAck {
        notification_id: String,
    },
    PingFail {
        notification_id: String,
        reason: String,
    },
}

/// Error code of a non-2xx response or of a WebSocket error frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidRequest,
    Unauthenticated,
    Forbidden,
    IdempotencyConflict,
    PayloadTooLarge,
    Internal,
    Unavailable,
    IdentityConflict,
    RateLimited,
    NotFound,
    DeviceExists,
    DeviceBusy,
    DeviceNotActive,
    DeviceRevoked,
    GenerationMismatch,
}

/// Body of every non-2xx response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorBody {
    pub error: ErrorDetail,
}

/// Code and public description of an error.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorDetail {
    pub code: ErrorCode,
    pub message: String,
}
