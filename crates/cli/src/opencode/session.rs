//! OpenCode session identity, live discovery and queued synthetic input.

use std::path::Path;

use alter_bridge_protocol::Author;
use serde_json::{Value, json};

use super::service::{opencode_bin, service};

/// Reports whether an ID has OpenCode's native `ses_` session form.
pub(crate) fn is_session_id(id: &str) -> bool {
    id.strip_prefix("ses_").is_some_and(|rest| {
        (16..=64).contains(&rest.len()) && rest.bytes().all(|b| b.is_ascii_alphanumeric())
    })
}

/// Lists sessions currently running in OpenCode, named by their stored titles; idle sessions are not reported.
pub(crate) fn active(home: &Path) -> Result<Vec<Author>, String> {
    if opencode_bin(home).is_none() {
        return Ok(Vec::new());
    }
    let service = service(home)?;
    let running = service.call("GET", "/api/session/active", None)?;
    let ids: Vec<String> = running
        .as_object()
        .ok_or("OpenCode active sessions have an unexpected shape")?
        .keys()
        .filter(|id| is_session_id(id))
        .cloned()
        .collect();

    Ok(ids
        .into_iter()
        .map(|id| {
            let name = service
                .call("GET", &format!("/api/session/{id}"), None)
                .ok()
                .and_then(|session| session.get("title")?.as_str().map(str::to_owned))
                .unwrap_or_default();
            Author { id, name }
        })
        .collect())
}

/// Lists owned OpenCode bindings whose sessions still exist unarchived; the always-on service can wake them, so idle counts as available.
pub(crate) fn available(home: &Path, owned: &[Author]) -> Result<Vec<Author>, String> {
    if !super::enabled(home) || !owned.iter().any(|binding| is_session_id(&binding.id)) {
        return Ok(Vec::new());
    }
    let service = service(home)?;

    Ok(owned
        .iter()
        .filter(|binding| is_session_id(&binding.id))
        .filter_map(|binding| {
            let session = service
                .call("GET", &format!("/api/session/{}", binding.id), None)
                .ok()?;
            if session["time"]
                .get("archived")
                .is_some_and(|archived| !archived.is_null())
            {
                return None;
            }
            let name = session
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or(&binding.name)
                .to_owned();

            Some(Author {
                id: binding.id.clone(),
                name,
            })
        })
        .collect())
}

/// Durably queues synthetic text on a session and accepts it only when OpenCode echoes the exact session and delivery.
pub(crate) fn synthetic(
    home: &Path,
    session: &str,
    text: &str,
    description: &str,
    resume: bool,
) -> Result<(), String> {
    if !is_session_id(session) {
        return Err("recipient is not an OpenCode session ID".into());
    }
    let admitted = service(home)?.call(
        "POST",
        &format!("/api/session/{session}/synthetic"),
        Some(json!({"text": text, "description": description, "delivery": "queue", "resume": resume})),
    )?;
    let echoed = admitted.get("sessionID").and_then(Value::as_str) == Some(session)
        && admitted.get("delivery").and_then(Value::as_str) == Some("queue")
        && admitted
            .get("id")
            .and_then(Value::as_str)
            .is_some_and(|id| id.starts_with("msg_"));
    if !echoed {
        return Err("OpenCode did not confirm the queued synthetic input".into());
    }

    Ok(())
}
