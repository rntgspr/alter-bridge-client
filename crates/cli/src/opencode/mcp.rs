//! Runtime registration of the alter-bridge MCP server inside OpenCode.

use std::path::Path;

use serde_json::{Value, json};

use super::service::service;

/// Name of the runtime MCP server the daemon keeps registered in OpenCode.
pub(super) const MCP_NAME: &str = "alter-bridge";

/// Reports the runtime status of the alter-bridge MCP server, or `None` when it is not registered.
pub(crate) fn status(home: &Path) -> Result<Option<String>, String> {
    let servers = service(home)?.call("GET", "/api/mcp", None)?;

    Ok(servers
        .as_array()
        .ok_or("OpenCode MCP list has an unexpected shape")?
        .iter()
        .find(|server| server.get("name").and_then(Value::as_str) == Some(MCP_NAME))
        .map(|server| {
            server["status"]["status"]
                .as_str()
                .unwrap_or("unknown")
                .to_owned()
        }))
}

/// Registers the alter-bridge remote MCP server at runtime with in-memory bearer headers unless it is already connected.
pub(super) fn ensure(home: &Path, broker: &str, token: &str) -> Result<(), String> {
    if status(home)?.as_deref() == Some("connected") {
        return Ok(());
    }
    service(home)?.submit(
        "PUT",
        &format!("/api/experimental/mcp/{MCP_NAME}"),
        json!({"config": {
            "type": "remote",
            "url": format!("{broker}/mcp"),
            "oauth": false,
            "headers": {"Authorization": format!("Bearer {token}")},
        }}),
    )?;

    Ok(())
}
