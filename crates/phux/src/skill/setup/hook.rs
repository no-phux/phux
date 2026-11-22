//! The Claude Code `SessionStart` hook in `settings.json`: merged in by
//! `setup`, reported by `status`, and taken out by `remove`.

use std::path::Path;

use serde_json::{Value, json};

use super::{Ctx, Hook, Mode, State};
use crate::commands::agent::shim::atomic_write;

/// The hook command, and the substring that marks a hook as ours.
const HOOK_MARK: &str = "phux skill session-start";
pub(super) const HOOK_COMMAND: &str = "/bin/sh -c 'phux skill session-start 2>/dev/null || true'";
const HOOK_TIMEOUT: u64 = 5;
/// The first-party Claude Code plugin (`integrations/claude`).
const CLAUDE_PLUGIN: &str = "phux@phux";

fn read_settings(path: &Path) -> Result<Option<Value>, String> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|err| format!("could not parse {}: {err}", path.display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(format!("could not read {}: {err}", path.display())),
    }
}

/// Why phux leaves the hook to something else that already registers phux
/// hooks in Claude Code.
pub(super) fn hook_owner(ctx: &Ctx) -> Option<String> {
    if ctx.shim.as_ref().is_some_and(|shim| shim.exists()) {
        return Some(
            "the claude-in-phux shim (`phux agent install-claude`) registers phux hooks".to_owned(),
        );
    }
    let mut settings = vec![ctx.home.join(".claude/settings.json")];
    if ctx.project {
        settings.push(ctx.settings());
    }
    settings
        .iter()
        .filter_map(|path| read_settings(path).ok().flatten())
        .any(|value| value["enabledPlugins"][CLAUDE_PLUGIN] == Value::Bool(true))
        .then(|| "the phux Claude Code plugin (`phux@phux`) registers phux hooks".to_owned())
}

/// Each `SessionStart` hook command phux owns, as `(group, hook)` indexes.
fn our_hooks(settings: &Value) -> Vec<(usize, usize)> {
    let Some(groups) = settings["hooks"]["SessionStart"].as_array() else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for (g, group) in groups.iter().enumerate() {
        for (h, hook) in group["hooks"].as_array().into_iter().flatten().enumerate() {
            if hook["command"]
                .as_str()
                .is_some_and(|c| c.contains(HOOK_MARK))
            {
                found.push((g, h));
            }
        }
    }
    found
}

fn hook_entry() -> Value {
    json!({ "type": "command", "command": HOOK_COMMAND, "timeout": HOOK_TIMEOUT })
}

pub(super) fn hook_step(ctx: &Ctx, mode: Mode, dry_run: bool) -> Result<Hook, String> {
    let path = ctx.settings();
    let hook = |state| Hook {
        state,
        reason: None,
    };
    if mode != Mode::Remove
        && let Some(reason) = hook_owner(ctx)
    {
        return Ok(Hook {
            state: State::Skipped,
            reason: Some(reason),
        });
    }
    let mut settings = match read_settings(&path) {
        Ok(value) => value.unwrap_or_else(|| json!({})),
        // A settings file phux cannot read is never rewritten.
        Err(reason) => {
            return Ok(Hook {
                state: State::Conflict,
                reason: Some(reason),
            });
        }
    };
    if !settings.is_object() {
        return Ok(Hook {
            state: State::Conflict,
            reason: Some(format!("{} is not a JSON object", path.display())),
        });
    }
    let ours = our_hooks(&settings);
    let current = ours.len() == 1 && {
        let (g, h) = ours[0];
        settings["hooks"]["SessionStart"][g]["hooks"][h] == hook_entry()
    };
    let state = match (mode, ours.is_empty(), current) {
        (Mode::Status | Mode::Remove, true, _) => State::Missing,
        (Mode::Status | Mode::Setup { .. }, false, true) => State::UpToDate,
        (Mode::Status, false, false) => State::Stale,
        (Mode::Setup { .. }, true, _) => State::Installed,
        (Mode::Setup { .. }, false, false) => State::Updated,
        (Mode::Remove, false, _) => State::Removed,
    };
    if dry_run || !matches!(state, State::Installed | State::Updated | State::Removed) {
        return Ok(hook(state));
    }

    remove_hooks(&mut settings, &ours);
    if state != State::Removed {
        let root = settings
            .as_object_mut()
            .ok_or("settings is not an object")?;
        let hooks = root.entry("hooks").or_insert_with(|| json!({}));
        if !hooks.is_object() {
            *hooks = json!({});
        }
        let groups = hooks
            .as_object_mut()
            .ok_or("hooks is not an object")?
            .entry("SessionStart")
            .or_insert_with(|| json!([]));
        if !groups.is_array() {
            *groups = json!([]);
        }
        if let Some(groups) = groups.as_array_mut() {
            groups.push(json!({ "matcher": "", "hooks": [hook_entry()] }));
        }
    }
    let mut bytes = serde_json::to_vec_pretty(&settings)
        .map_err(|err| format!("could not render {}: {err}", path.display()))?;
    bytes.push(b'\n');
    let mode_bits = {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::metadata(&path).map_or(0o644, |meta| meta.permissions().mode() & 0o777)
    };
    atomic_write(&path, &bytes, mode_bits)?;
    Ok(hook(state))
}

/// Drop phux's hooks, then any group or key that only they filled.
fn remove_hooks(settings: &mut Value, ours: &[(usize, usize)]) {
    if ours.is_empty() {
        return;
    }
    let Some(groups) = settings["hooks"]["SessionStart"].as_array_mut() else {
        return;
    };
    for &(g, h) in ours.iter().rev() {
        if let Some(list) = groups[g]["hooks"].as_array_mut() {
            list.remove(h);
        }
    }
    groups.retain(|group| {
        group["hooks"]
            .as_array()
            .is_none_or(|list| !list.is_empty())
    });
    let hooks = settings["hooks"].as_object_mut();
    if let Some(hooks) = hooks {
        if hooks["SessionStart"].as_array().is_some_and(Vec::is_empty) {
            hooks.remove("SessionStart");
        }
        if hooks.is_empty()
            && let Some(root) = settings.as_object_mut()
        {
            root.remove("hooks");
        }
    }
}
