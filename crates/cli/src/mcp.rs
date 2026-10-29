//! Private MCP header helper and optional direct provider launchers.

use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

use serde_json::json;

use crate::{Exit, config, setup};

const MCP_USAGE: &str = "usage: alter-bridge mcp headers --url <expected-broker-url>\n       alter-bridge mcp codex|claude [provider args...]\n";

/// Dispatches credential output or replaces this process with the native provider.
pub(crate) fn run(args: &[String], home: &Path) -> Result<(), Exit> {
    match args {
        [command, flag, expected] if command == "headers" && flag == "--url" => {
            let normalized = config::check_url(expected).map_err(Exit::Failure)?;
            if normalized != *expected {
                return Err(Exit::Failure("MCP broker URL is not normalized".into()));
            }
            let saved = config::load(home).map_err(Exit::Failure)?;
            if saved.url != normalized {
                return Err(Exit::Failure(
                    "MCP broker URL differs from the saved connection".into(),
                ));
            }

            println!(
                "{}",
                json!({"Authorization":format!("Bearer {}", saved.token)})
            );
            Ok(())
        }
        [provider, tail @ ..] if provider == "codex" || provider == "claude" => {
            setup::verify_managed_entry(provider, home).map_err(Exit::Failure)?;
            let error = Command::new(provider).args(tail).exec();
            Err(Exit::Failure(format!("cannot launch {provider}: {error}")))
        }
        _ => Err(Exit::Usage(MCP_USAGE.into())),
    }
}
