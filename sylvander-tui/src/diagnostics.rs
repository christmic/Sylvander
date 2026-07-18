//! Redacted diagnostic reporting and crash-safe export.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use crate::app::AppState;
use crate::config::TuiConfig;

pub fn report(config: &TuiConfig, state: &AppState) -> String {
    let session: String = state
        .session_id
        .as_deref()
        .map_or_else(|| "new".into(), |id| id.chars().take(8).collect());
    format!(
        "Sylvander TUI diagnostic (redacted)\n\nversion      {}\nconnected    {}\nprotocol     {}\nprotocol caps {}\ntheme        {}\nsocket       {}\nhistory      {}\nworkspace    {}\nbranch       {}\nmodel        {}\nreasoning    {}\nsession      {}\npermissions  {:?}/{:?}/{:?}\ncapabilities 0x{:02x}\nattachments  {} bytes\nrender       {} ms\nreconnect    {} ms\nmessages     {}\nqueued       {}\nturn active  {}\ntokens       {} input / {} output\ncost         {}",
        env!("CARGO_PKG_VERSION"),
        state.connected,
        state
            .protocol_version
            .map_or_else(|| "none".into(), |version| format!("v{version}")),
        state.protocol_capabilities.len(),
        config.theme,
        redacted_path(&config.socket_path),
        config
            .history_path
            .as_deref()
            .map_or_else(|| "disabled".into(), redacted_path),
        redacted_path(&state.metadata.workspace),
        safe_label(&state.metadata.branch),
        safe_label(&state.metadata.model_label()),
        crate::app::reasoning_label(state.metadata.reasoning_effort),
        session,
        state.metadata.permissions.file_access,
        state.metadata.permissions.network_access,
        state.metadata.permissions.approval_policy,
        state.metadata.capabilities,
        state.metadata.max_attachment_bytes,
        config.render_interval.as_millis(),
        config.reconnect_interval.as_millis(),
        state.messages.len(),
        state.queued_prompts.len(),
        state.turn_active,
        state.input_tokens,
        state.output_tokens,
        state
            .cost_nano_usd
            .map_or_else(|| "unavailable".into(), crate::app::format_cost,),
    )
}

pub fn export(report: &str, requested: &Path, workspace: &Path) -> Result<PathBuf, String> {
    let relative_request = !requested.is_absolute();
    if relative_request
        && requested
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err("relative diagnostic path cannot leave the workspace".into());
    }
    let target = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        workspace.join(requested)
    };
    if std::fs::symlink_metadata(&target).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err("refusing to replace a diagnostic-report symlink".into());
    }
    let parent = target
        .parent()
        .ok_or_else(|| "diagnostic report needs a parent directory".to_string())?;
    if !parent.is_dir() {
        return Err(format!(
            "diagnostic directory does not exist: {}",
            parent.display()
        ));
    }
    if relative_request {
        let root = workspace
            .canonicalize()
            .map_err(|error| format!("cannot resolve workspace: {error}"))?;
        let resolved_parent = parent
            .canonicalize()
            .map_err(|error| format!("cannot resolve diagnostic directory: {error}"))?;
        if !resolved_parent.starts_with(root) {
            return Err("relative diagnostic path resolves outside the workspace".into());
        }
    }
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = parent.join(format!(
        ".sylvander-doctor-{}-{unique}.tmp",
        std::process::id()
    ));
    let operation = (|| -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(report.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&temporary, &target)
    })();
    if let Err(error) = operation {
        let _ = std::fs::remove_file(&temporary);
        return Err(format!("diagnostic export failed: {error}"));
    }
    Ok(target)
}

fn redacted_path(path: &Path) -> String {
    path.file_name().map_or_else(
        || "<root>".into(),
        |name| format!("…/{}", name.to_string_lossy()),
    )
}

fn safe_label(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_control())
        .take(120)
        .collect()
}

#[cfg(test)]
#[path = "../tests/unit/diagnostics.rs"]
mod tests;
