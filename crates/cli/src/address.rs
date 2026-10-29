//! Opaque session IDs used in metadata-only notices.

/// Renders a session ID without inferring its native provider.
pub fn alias(id: &str) -> &str {
    id
}
