//! Opt-in OpenCode setup recorded only in the private alter-bridge config.

use std::path::Path;

use serde_json::{Value, json};

use super::mcp::MCP_NAME;

/// Reports whether `config opencode` recorded opt-in OpenCode integration for the saved broker.
pub(crate) fn enabled(home: &Path) -> bool {
    crate::config::preferences(home).is_ok_and(|preferences| {
        preferences
            .setup
            .as_ref()
            .and_then(|setup| setup.get("opencode"))
            .and_then(|record| record.get("broker"))
            .and_then(Value::as_str)
            .is_some_and(|broker| preferences.url.as_deref() == Some(broker))
    })
}

/// Previews and records opt-in OpenCode integration in the private alter-bridge config without touching OpenCode files.
pub(crate) fn configure(home: &Path) -> Result<(), crate::Exit> {
    use crate::Exit;
    use std::io::{self, Write};

    let preferences = crate::config::preferences(home).map_err(Exit::Failure)?;
    let broker = preferences.url.ok_or_else(|| {
        Exit::Failure("save a broker URL with connect before configuring a provider".into())
    })?;
    println!(
        "Provider: opencode; broker: {broker}/mcp\nOpenCode files: unchanged (skills are shared from ~/.agents/skills)\nDaemon: registers runtime MCP server {MCP_NAME} with in-memory credentials, announces each live session's actor once and queues notices as synthetic input\nManaged changes: private setup record only"
    );
    print!("Proceed? [y/N] ");
    io::stdout()
        .flush()
        .map_err(|_| Exit::Failure("cannot print confirmation".into()))?;
    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .map_err(|_| Exit::Failure("cannot read confirmation".into()))?;
    if !answer.trim().eq_ignore_ascii_case("y") {
        println!("declined; nothing changed");
        return Ok(());
    }

    let mut transaction = crate::config::Transaction::open(home).map_err(Exit::Failure)?;
    if transaction.file.url.as_deref() != Some(broker.as_str()) {
        return Err(Exit::Failure(
            "configuration changed after preview; rerun config".into(),
        ));
    }
    let mut setup = transaction.file.setup.clone().unwrap_or_else(|| json!({}));
    setup
        .as_object_mut()
        .ok_or_else(|| Exit::Failure("private setup metadata is invalid".into()))?
        .insert(
            "opencode".into(),
            json!({"version": env!("CARGO_PKG_VERSION"), "broker": broker}),
        );
    transaction.file.setup = Some(setup);
    transaction.save().map_err(|error| {
        Exit::Failure(format!("private metadata save failed: {}", error.message))
    })?;
    println!(
        "OpenCode integration recorded; the daemon applies it on its next discovery pass. Invoke abg-connect-context in an OpenCode session after its actor announcement appears."
    );

    Ok(())
}
