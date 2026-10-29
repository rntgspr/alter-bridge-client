//! Daemon-side OpenCode reconciliation: runtime MCP registration and one actor announcement per live session.

use std::path::Path;

use alter_bridge_protocol::Author;
use serde_json::json;

use super::{mcp, session, setup};

/// Keeps the runtime MCP server registered and announces each newly seen live session's trusted actor once per daemon run.
pub(crate) fn reconcile(home: &Path, broker: &str, token: &str, live: &[Author]) -> Vec<String> {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};

    static ANNOUNCED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

    if !setup::enabled(home) {
        return Vec::new();
    }
    let mut errors = Vec::new();
    if let Err(error) = mcp::ensure(home, broker, token) {
        errors.push(format!("OpenCode MCP registration failed: {error}"));
    }
    let announced = ANNOUNCED.get_or_init(|| Mutex::new(HashSet::new()));
    for author in live
        .iter()
        .filter(|author| session::is_session_id(&author.id))
    {
        if announced.lock().is_ok_and(|seen| seen.contains(&author.id)) {
            continue;
        }
        let text = format!(
            "alter-bridge trusted current actor: {}",
            json!({"id": author.id})
        );
        match session::synthetic(home, &author.id, &text, "alter-bridge actor", false) {
            Ok(()) => {
                if let Ok(mut seen) = announced.lock() {
                    seen.insert(author.id.clone());
                }
            }
            Err(error) => errors.push(format!("OpenCode actor announcement failed: {error}")),
        }
    }

    errors
}
