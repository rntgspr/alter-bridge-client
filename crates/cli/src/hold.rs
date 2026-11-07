//! `hold` and `release`: toggle the context-hold flag of one binding owned by this device.

use std::path::Path;

use alter_bridge_protocol::Author;
use serde_json::json;

use crate::{Exit, api, config, probe, who};

/// Parses `<actor>`, resolves it to an owned binding, and calls the MCP `hold` or `release` tool.
pub(crate) fn run(tool: &str, args: &[String], home: &Path) -> Result<(), Exit> {
    let [reference] = args else {
        return Err(Exit::Usage(format!(
            "usage: alter-bridge {tool} <actor>\n  <actor>: exact ID, name or device::name, optional leading #\n"
        )));
    };
    if reference.starts_with("--") {
        return Err(Exit::Usage(format!("usage: alter-bridge {tool} <actor>\n")));
    }

    let saved = config::load(home).map_err(Exit::Failure)?;
    let client = api::Client::new(&saved.url);

    let directory = client.directory(&saved.token).map_err(|error| {
        Exit::Failure(format!(
            "{tool}: who: {}; check the broker URL and active device credentials",
            error.public_message()
        ))
    })?;
    let candidates: Vec<(Author, String)> = directory
        .sessions
        .into_iter()
        .filter(|entry| !probe::is_probe(&entry.binding.id))
        .map(|entry| (entry.binding, entry.device_id))
        .collect();
    let target = probe::resolve(tool, reference, &candidates)?;

    let owned = candidates
        .iter()
        .any(|(binding, device)| binding.id == target.id && *device == saved.device_id);
    if !owned {
        return Err(Exit::Failure(format!(
            "{tool}: {} is not owned by this device; only its own device can {tool} it",
            target.id
        )));
    }

    let result = client
        .mcp_tool(&saved.token, tool, json!({ "actor": target.identity() }))
        .map_err(|error| Exit::Failure(format!("{tool}: {error}")))?;
    println!(
        "{} ({}) held: {}",
        who::safe(&target.name),
        who::safe(&target.id),
        result["held"].as_bool().unwrap_or(tool == "hold")
    );

    Ok(())
}
