//! Passive native lifecycle identity context and opted-in unread metadata checks.

use std::io::{self, Read};
use std::path::Path;
use std::time::{Duration, Instant};

use alter_bridge_protocol::{DeviceInboxCheckRequest, Identity};
use serde::Deserialize;
use serde_json::json;

use crate::{Exit, api::Client, config, notify, session};

const HOOK_USAGE: &str = "usage: alter-bridge hook codex|claude [--register-only]\n";
const HOOK_BYTES: u64 = 64 * 1024;
const NETWORK_BUDGET: Duration = Duration::from_secs(2);

#[derive(Deserialize)]
struct Input {
    session_id: String,
    hook_event_name: String,
}

/// Runs a fail-open provider hook using only the native ID supplied on stdin.
pub(crate) fn run(args: &[String], home: &Path) -> Result<(), Exit> {
    let (provider, register_only) = match args {
        [provider] => (provider.as_str(), false),
        [provider, flag] if flag == "--register-only" => (provider.as_str(), true),
        _ => return Err(Exit::Usage(HOOK_USAGE.into())),
    };
    let platform = match provider {
        "codex" => session::Provider::Codex,
        "claude" => session::Provider::Claude,
        _ => return Err(Exit::Usage(HOOK_USAGE.into())),
    };
    let mut bytes = Vec::new();
    let input = io::stdin().take(HOOK_BYTES + 1).read_to_end(&mut bytes);
    let input = if input.is_ok() && bytes.len() as u64 <= HOOK_BYTES {
        serde_json::from_slice::<Input>(&bytes).ok()
    } else {
        None
    };
    let expected_event = if register_only {
        "SessionStart"
    } else {
        "UserPromptSubmit"
    };
    let Some(input) = input.filter(|input| input.hook_event_name == expected_event) else {
        eprintln!("alter-bridge hook: invalid lifecycle input");
        return Ok(());
    };
    if !session::is_full_id(platform, &input.session_id) {
        eprintln!("alter-bridge hook: invalid native session ID");
        return Ok(());
    }

    let actor = Identity {
        id: input.session_id,
    };
    let mut context = format!(
        "alter-bridge trusted current actor: {}",
        serde_json::to_string(&actor).expect("identity serializes")
    );

    if register_only {
        print_context(expected_event, &context);
        return Ok(());
    }

    let started = Instant::now();
    let outcome = (|| -> Result<Option<String>, String> {
        let saved = config::load(home)?;
        let joined = Client::with_timeout(&saved.url, NETWORK_BUDGET)
            .sessions(&saved.token)
            .map_err(|error| error.public_message())?;
        if !joined.iter().any(|binding| binding.id == actor.id) {
            return Ok(None);
        }
        let remaining = NETWORK_BUDGET.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err("metadata check timed out".into());
        }
        let response = Client::with_timeout(&saved.url, remaining)
            .inbox_check(
                &saved.token,
                &DeviceInboxCheckRequest {
                    actor: actor.clone(),
                },
            )
            .map_err(|error| error.public_message())?;
        match response.ping {
            Some(ping) if ping.to == actor => Ok(Some(notify::notice(&ping))),
            Some(_) => Err("server returned another recipient's notice".into()),
            None => Ok(None),
        }
    })();

    if let Ok(Some(notice)) = outcome.as_ref() {
        context.push('\n');
        context.push_str(notice);
    }

    print_context(expected_event, &context);

    Ok(())
}

/// Emits the provider's documented model-visible context envelope.
fn print_context(event: &str, context: &str) {
    println!(
        "{}",
        json!({"hookSpecificOutput":{"hookEventName":event,"additionalContext":context}})
    );
}
