//! Read-only terminal view of the configured broker's shared session directory.

use std::path::Path;

use crate::{Exit, api, config};

/// Renders broker-provided data as inert terminal text.
fn safe(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_control() || character == '\u{1b}' {
                ' '
            } else {
                character
            }
        })
        .collect::<String>()
        .replace('\u{2028}', " ")
        .replace('\u{2029}', " ")
}

/// Lists participating sessions without starting a daemon or registering this terminal.
pub(crate) fn run(args: &[String], home: &Path) -> Result<(), Exit> {
    let all = match args {
        [] => false,
        [flag] if flag == "--all" => true,
        _ => return Err(Exit::Usage("usage: alter-bridge who [--all]\n".into())),
    };
    let saved = config::load(home).map_err(Exit::Failure)?;
    let directory = api::Client::new(&saved.url)
        .directory(&saved.token)
        .map_err(|error| {
            Exit::Failure(format!(
                "who: {}; check the broker URL and active device credentials",
                error.public_message()
            ))
        })?;
    let sessions: Vec<_> = directory
        .sessions
        .into_iter()
        .filter(|entry| all || entry.connected)
        .collect();
    if sessions.is_empty() {
        println!("No matching sessions.");
        return Ok(());
    }

    if all {
        println!("NAME\tSESSION ID\tDEVICE ID\tACTIVE\tCONNECTED");
    } else {
        println!("NAME\tSESSION ID\tDEVICE ID\tACTIVE");
    }
    for entry in sessions {
        let name = safe(&entry.binding.name);
        let id = safe(&entry.binding.id);
        let device = safe(&entry.device_id);
        if all {
            println!(
                "{name}\t{id}\t{device}\t{}\t{}",
                entry.active, entry.connected
            );
        } else {
            println!("{name}\t{id}\t{device}\t{}", entry.active);
        }
    }

    Ok(())
}
