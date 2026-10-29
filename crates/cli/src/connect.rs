//! Saves issued device credentials before use and clears them only after acknowledged revocation.

use std::path::Path;

use alter_bridge_protocol::{DeviceConnectRequest, DeviceDisconnectRequest, DeviceState, limits};

use crate::{Exit, api::Client, config, daemon, setup};

const CONNECT_USAGE: &str = "usage: alter-bridge connect device [url] [--hexid <six lowercase hex>] [--id <device>]\nSaved credentials are reused before issuance. Missing URL/hexid requires the device skill.\n";
const DISCONNECT_USAGE: &str = "usage: alter-bridge disconnect device --hexid <six lowercase hex> --id <saved-device>\nThe private disconnect key is read from the saved configuration.\n";

/// Parses one optional URL and nonduplicated named arguments without reflecting private input.
fn arguments(
    args: &[String],
    disconnect: bool,
) -> Result<(Option<&str>, Option<&str>, Option<&str>), Exit> {
    let usage = if disconnect {
        DISCONNECT_USAGE
    } else {
        CONNECT_USAGE
    };
    let (mut url, mut code, mut id) = (None, None, None);
    let mut args = args.iter();

    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--hexid" if code.is_none() => {
                code = Some(
                    args.next()
                        .ok_or_else(|| Exit::Usage(usage.into()))?
                        .as_str(),
                );
            }
            "--id" if id.is_none() => {
                id = Some(
                    args.next()
                        .ok_or_else(|| Exit::Usage(usage.into()))?
                        .as_str(),
                );
            }
            value if !disconnect && !value.starts_with('-') && url.is_none() => url = Some(value),
            _ => return Err(Exit::Usage(usage.into())),
        }
    }
    if code.is_some_and(|code| !limits::is_pairing_code(code)) {
        return Err(Exit::Usage(
            "hexid must contain six lowercase hexadecimal characters\n".into(),
        ));
    }
    if id.is_some_and(|id| !limits::is_device_id(id)) {
        return Err(Exit::Usage(
            "device ID must be a lowercase slug of 1 to 128 bytes\n".into(),
        ));
    }
    if disconnect && (code.is_none() || id.is_none()) {
        return Err(Exit::Usage(usage.into()));
    }

    Ok((url, code, id))
}

/// Selects and validates a device registration, saves it, then requests the new daemon transport.
pub(crate) fn run(args: &[String], home: &Path) -> Result<(), Exit> {
    if matches!(args, [flag] if flag == "--help" || flag == "-h") {
        print!("{CONNECT_USAGE}");
        return Ok(());
    }
    let (url, code, explicit_id) = arguments(args, false)?;
    let mut transaction = config::Transaction::open(home).map_err(Exit::Failure)?;
    if url.is_none() && transaction.file.url.is_none()
        || code.is_none() && transaction.file.token.is_none()
    {
        let mut required = Vec::new();
        if url.is_none() && transaction.file.url.is_none() {
            required.push("server_url");
        }
        if code.is_none() && transaction.file.token.is_none() {
            required.push("current_hexid");
        }
        println!(
            "{}",
            serde_json::json!({"outcome":"missing_input", "required":required, "next":"invoke abg-connect-device with explicit operation inputs"})
        );
        return Ok(());
    }
    let url = url
        .or(transaction.file.url.as_deref())
        .ok_or_else(|| Exit::Usage(CONNECT_USAGE.into()))?;
    let url = config::check_url(url).map_err(Exit::Failure)?;
    let device_id = match explicit_id.or(transaction.file.device_id.as_deref()) {
        Some(id) => id.to_string(),
        None => config::random_device_id().map_err(Exit::Failure)?,
    };
    if transaction.file.token.is_some()
        && transaction.file.device_id.as_deref() != Some(device_id.as_str())
    {
        return Err(Exit::Failure(
            "connect: disconnect the saved device before choosing a different ID".into(),
        ));
    }

    let client = Client::new(&url);
    let registration_state;
    if let Some(code) = code.filter(|_| transaction.file.token.is_none()) {
        let response = client
            .issue(&DeviceConnectRequest {
                code: code.into(),
                device_id: device_id.clone(),
            })
            .map_err(|error| {
                let outcome = if error.issuance_unknown() {
                    "issuance outcome may be unknown; retrying requires a new issuance"
                } else {
                    "check the pairing code and device ID before retrying"
                };
                Exit::Failure(format!(
                    "connect: {}; {outcome}; saved credentials and daemon were not changed",
                    error.public_message()
                ))
            })?;
        if response.device_id != device_id
            || response.generation == 0
            || !limits::is_device_token(&response.token)
            || !limits::is_disconnect_key(&response.disconnect_key)
        {
            return Err(Exit::Failure("connect: invalid issuance response; outcome may be unknown; saved credentials and daemon were not changed".into()));
        }
        transaction.file.token = Some(response.token);
        transaction.file.disconnect_key = Some(response.disconnect_key);
        registration_state = "issued (not activated)";
    } else {
        let saved = transaction.file.connected().map_err(Exit::Failure)?;
        let status = client.status(&saved.token).map_err(|error| {
            Exit::Failure(format!(
                "connect: {}; saved credentials and daemon were not changed",
                error.public_message()
            ))
        })?;
        if status.device_id != device_id
            || status.generation == 0
            || status.state == DeviceState::Revoked
            || (status.state == DeviceState::Issued && status.connected)
        {
            return Err(Exit::Failure("connect: inconsistent device status; saved credentials and daemon were not changed".into()));
        }
        registration_state = match status.state {
            DeviceState::Issued => "issued (not activated)",
            DeviceState::Active => "active",
            DeviceState::Revoked => unreachable!("revoked status rejected above"),
        };
    }

    transaction.file.url = Some(url.clone());
    transaction.file.device_id = Some(device_id.clone());
    transaction.save().map_err(|error| {
        let state = if error.written {
            "configuration was replaced; durability is unconfirmed"
        } else {
            "configuration was not replaced"
        };
        Exit::Failure(format!(
            "connect: {}; {state}; daemon was not changed",
            error.message
        ))
    })?;
    println!("configuration saved to {}", config::path(home).display());
    println!("device {device_id}: {registration_state}");
    if let Some(mut setup_record) = transaction.file.setup.clone() {
        let mut refreshed = Vec::new();
        let mut pending = Vec::new();
        for provider in ["codex", "claude"] {
            let Some(record) = setup_record.get(provider) else {
                continue;
            };
            if record.get("broker").and_then(serde_json::Value::as_str) == Some(url.as_str()) {
                continue;
            }
            let mut candidate = setup_record.clone();
            match setup::refresh_managed_url(provider, home, &mut candidate, &url) {
                Ok(_) => {
                    setup_record = candidate;
                    refreshed.push(provider);
                }
                Err(error) => pending.push(format!("{provider}: {error}")),
            }
        }
        if !refreshed.is_empty() {
            transaction.file.setup = Some(setup_record);
            if let Err(error) = transaction.save() {
                pending.push(format!("private refresh record: {}", error.message));
            }
        }
        if !refreshed.is_empty() {
            eprintln!(
                "alter-bridge: connect: refreshed managed MCP URLs for {}; reopen those providers",
                refreshed.join(", ")
            );
        }
        if !pending.is_empty() {
            eprintln!(
                "alter-bridge: connect: credentials were saved but provider refresh is pending: {}; rerun config for affected providers and reopen them",
                pending.join("; ")
            );
        }
    }
    if let Err(error) = daemon::restart(home) {
        let detail = match error {
            Exit::Failure(message) | Exit::Usage(message) => message,
        };
        eprintln!(
            "alter-bridge: connect: configuration saved, but daemon connection is pending: {detail}; inspect alter-bridge daemon status and ~/Library/Logs/alter-bridge.log"
        );
        println!(
            "{}",
            serde_json::json!({"outcome":"pending", "device_connected":false, "next":"inspect daemon status and enable the LaunchAgent if disabled, then retry"})
        );
        return Ok(());
    }
    let connected = client
        .status(transaction.file.token.as_deref().expect("saved credential"))
        .map(|status| status.connected)
        .unwrap_or(false);
    println!(
        "{}",
        serde_json::json!({"outcome":if connected { "connected" } else { "pending" }, "device_connected":connected, "next":if connected { "invoke abg-connect-context to opt in" } else { "wait for accepted WebSocket presence, then resume" }})
    );

    Ok(())
}

/// Performs explicit operator/native context control; arguments do not attest model identity.
pub(crate) fn context(args: &[String], home: &Path, leave: bool) -> Result<(), Exit> {
    let usage = "usage: abg connect context --provider codex|claude|opencode --id <trusted-native-id> --name <friendly-name> [--tools-available] [--status]\nabg disconnect context --id <trusted-native-id>\nUse the native connection skill for the current chat; explicit --id is operator input, not identity attestation.\n";
    if matches!(args, [flag] if flag == "--help" || flag == "-h") {
        print!("{usage}");
        return Ok(());
    }
    let (mut id, mut name, mut provider) = (None, None, None);
    let (mut tools, mut status_only) = (false, false);
    let mut values = args.iter();
    while let Some(value) = values.next() {
        match value.as_str() {
            "--id" if id.is_none() => id = values.next().map(String::as_str),
            "--name" if name.is_none() && !leave => name = values.next().map(String::as_str),
            "--provider" if provider.is_none() && !leave => {
                provider = values.next().map(String::as_str)
            }
            "--tools-available" if !tools && !leave => tools = true,
            "--status" if !status_only && !leave => status_only = true,
            _ => return Err(Exit::Usage(usage.into())),
        }
    }
    let Some(id) = id.filter(|id| {
        !id.is_empty() && id.len() <= limits::IDENTITY_ID_BYTES && !id.chars().any(char::is_control)
    }) else {
        println!(
            "{}",
            serde_json::json!({"outcome":"missing_input", "required":["trusted_native_id"], "next":"invoke the native context skill; a bare terminal has no current chat identity"})
        );
        return Ok(());
    };
    let preferences = config::preferences(home).map_err(Exit::Failure)?;
    let saved = match preferences.connected() {
        Ok(saved) => saved,
        Err(_) => {
            println!(
                "{}",
                serde_json::json!({"outcome":"device_required", "next":"invoke abg-connect-device"})
            );
            return Ok(());
        }
    };
    let client = Client::new(&saved.url);
    let status = client
        .status(&saved.token)
        .map_err(|error| Exit::Failure(error.public_message()))?;
    if status.state != DeviceState::Active || (!leave && !status.connected) {
        println!(
            "{}",
            serde_json::json!({"outcome":"device_pending", "device_connected":false, "next":"invoke abg-connect-device once, then resume"})
        );
        return Ok(());
    }
    let actor = alter_bridge_protocol::Identity { id: id.into() };
    if leave {
        let outcome = client
            .leave_session(&saved.token, &actor)
            .map_err(|error| Exit::Failure(error.public_message()))?;
        println!("{outcome}");
        return Ok(());
    }
    let Some(provider) =
        provider.filter(|provider| matches!(*provider, "codex" | "claude" | "opencode"))
    else {
        println!(
            "{}",
            serde_json::json!({"outcome":"missing_input", "required":["provider"]})
        );
        return Ok(());
    };
    let native = match provider {
        "opencode" => crate::session::Provider::OpenCode,
        "codex" => crate::session::Provider::Codex,
        _ => crate::session::Provider::Claude,
    };
    if !crate::session::is_full_id(native, id) {
        return Err(Exit::Failure(format!(
            "context ID does not have the native {provider} session form"
        )));
    }
    let configured = setup::verify_managed_entry(provider, home);
    let joined = client
        .sessions(&saved.token)
        .map_err(|error| Exit::Failure(error.public_message()))?
        .iter()
        .any(|binding| binding.id == id);
    if status_only || configured.is_err() || !tools {
        println!(
            "{}",
            serde_json::json!({"outcome":if status_only { "status" } else { "pending" }, "device_connected":true, "native_configuration_installed":configured.is_ok(), "context_registered":joined, "mcp_tools_available":if tools { Some(true) } else { None }, "diagnostic":configured.err(), "next":"use config only if required; reopen the provider after setup, then confirm actual MCP tools before joining"})
        );
        return Ok(());
    }
    let Some(name) = name.filter(|name| !name.trim().is_empty()) else {
        println!(
            "{}",
            serde_json::json!({"outcome":"missing_input", "required":["friendly_name"]})
        );
        return Ok(());
    };
    let response = client
        .register_session(
            &saved.token,
            &alter_bridge_protocol::DeviceSessionRequest {
                id: id.into(),
                name: name.into(),
            },
        )
        .map_err(|error| Exit::Failure(error.public_message()))?;
    if response.actor != actor || response.device_id != saved.device_id {
        return Err(Exit::Failure(
            "broker returned a different context identity".into(),
        ));
    }
    println!(
        "{}",
        serde_json::json!({"outcome":"joined", "actor":actor, "device_connected":true, "native_configuration_installed":true, "context_registered":true, "mcp_tools_available":true})
    );

    Ok(())
}

/// Sends generation-specific revocation before clearing credentials and stopping the local daemon.
pub(crate) fn disconnect(args: &[String], home: &Path) -> Result<(), Exit> {
    if matches!(args, [flag] if flag == "--help" || flag == "-h") {
        print!("{DISCONNECT_USAGE}");
        return Ok(());
    }
    let (_, code, id) = arguments(args, true)?;
    let mut transaction = config::Transaction::open(home).map_err(Exit::Failure)?;
    let saved = transaction.file.connected().map_err(Exit::Failure)?;
    if id != Some(saved.device_id.as_str()) {
        return Err(Exit::Failure(
            "disconnect: --id must match the saved device; credentials and daemon were not changed"
                .into(),
        ));
    }

    let response = Client::new(&saved.url).disconnect(&DeviceDisconnectRequest {
        code: code.expect("disconnect requires code").into(),
        device_id: saved.device_id.clone(),
        disconnect_key: saved.disconnect_key,
    }).map_err(|error| Exit::Failure(format!(
        "disconnect: {}; no acknowledgement received, so credentials were retained and daemon was not changed; revocation may already have completed; retry the same disconnect",
        error.public_message()
    )))?;
    if response.revoked_generation == 0 {
        return Err(Exit::Failure("disconnect: invalid acknowledgement; credentials were retained and daemon was not changed; retry the same disconnect".into()));
    }

    transaction.file.token = None;
    transaction.file.disconnect_key = None;
    transaction.save().map_err(|error| {
        let state = if error.written {
            "configuration was replaced; durability is unconfirmed"
        } else {
            "configuration was not replaced; saved credentials remain for recovery"
        };
        Exit::Failure(format!(
            "disconnect: server acknowledged revocation; {}; {state}; daemon was not changed",
            error.message
        ))
    })?;
    println!(
        "device {} disconnected; saved URL, ID and setup preferences retained",
        saved.device_id
    );
    daemon::run(&["stop".into()], home).map_err(|error| {
        let detail = match error { Exit::Failure(message) | Exit::Usage(message) => message };
        Exit::Failure(format!("disconnect: server acknowledged and local credentials cleared, but daemon stop failed: {detail}"))
    })
}
