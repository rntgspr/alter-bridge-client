//! Local device lifecycle, native MCP setup and metadata-only notifications.

mod address;
mod api;
mod config;
mod connect;
mod daemon;
mod doctor;
mod hook;
mod launchd;
mod mcp;
mod notify;
mod opencode;
mod session;
mod setup;
mod who;

use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "usage: alter-bridge <command> [args]
  alter-bridge --version  print the client package version
commands:
  daemon  manage device WebSocket (run | status | stop | restart | install | uninstall)
  connect device [url] [--hexid <six lowercase hex>] [--id <device>]
  connect context --provider codex|claude|opencode --id <native-id> --name <friendly-name> [--tools-available] [--status]
  disconnect device --hexid <six lowercase hex> --id <saved-device>
  disconnect context --id <native-id>
  config codex|claude [--assets /absolute/package-root]  preview and install native MCP, hooks, and skill
  config opencode  preview and record daemon-managed OpenCode integration
  doctor  inspect local setup and nonactivating broker state (0 observed healthy, 1 attention, 2 invocation)
  who [--all]  list participating broker sessions (connected devices by default)
  hook codex|claude [--register-only]  native lifecycle hook
  mcp headers --url <broker>|codex|claude [args...]  helper or provider launcher

Messaging uses native MCP tools. This build does not support v1.
";

/// How a command ends when it does not succeed.
pub enum Exit {
    /// Wrong invocation: the text is printed as is and the process exits 2.
    Usage(String),
    /// Refusal or failure: the text is printed after `alter-bridge: ` and the process exits 1.
    Failure(String),
}

/// Returns the user's home, the root of the configuration, state, and provider stores.
fn home() -> Result<PathBuf, Exit> {
    match std::env::var_os("HOME") {
        Some(home) if !home.is_empty() && home != "/" => Ok(PathBuf::from(home)),
        _ => Err(Exit::Failure(
            "HOME must name the user's home directory".to_string(),
        )),
    }
}

/// Dispatches operator commands without reading obsolete client credentials.
fn dispatch(args: &[String]) -> Result<(), Exit> {
    let Some((command, rest)) = args.split_first() else {
        return Err(Exit::Usage(USAGE.to_string()));
    };

    if command == "--version" {
        if !rest.is_empty() {
            return Err(Exit::Usage(USAGE.to_string()));
        }

        println!("alter-bridge {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    let home = home()?;

    match command.as_str() {
        "daemon" => daemon::run(rest, &home),
        "connect" => match rest.split_first() {
            Some((mode, args)) if mode == "device" => connect::run(args, &home),
            Some((mode, args)) if mode == "context" => connect::context(args, &home, false),
            _ => Err(Exit::Usage(USAGE.into())),
        },
        "disconnect" => match rest.split_first() {
            Some((mode, args)) if mode == "device" => connect::disconnect(args, &home),
            Some((mode, args)) if mode == "context" => connect::context(args, &home, true),
            _ => Err(Exit::Usage(USAGE.into())),
        },
        "config" => setup::run(rest, &home),
        "doctor" => doctor::run(rest, &home),
        "who" => who::run(rest, &home),
        "hook" => hook::run(rest, &home),
        "mcp" => mcp::run(rest, &home),
        "-h" | "--help" | "help" => {
            print!("{USAGE}");
            Ok(())
        }
        unknown => Err(Exit::Usage(format!(
            "alter-bridge: unknown command {unknown:?}\n{USAGE}"
        ))),
    }
}

/// Runs the requested command and maps its outcome to the process exit code.
fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    match dispatch(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(Exit::Usage(text)) => {
            eprint!("{text}");
            ExitCode::from(2)
        }
        Err(Exit::Failure(message)) => {
            eprintln!("alter-bridge: {message}");
            ExitCode::FAILURE
        }
    }
}
