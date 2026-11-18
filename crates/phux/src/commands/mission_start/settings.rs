//! Repository-local settings, paths, and brief persistence for mission launch.
use std::path::{Path, PathBuf};

use super::super::git::git_text;
use phux_work::mission::{DEFAULT_MAX_AGENTS, DEFAULT_RULES, default_target_dir};

pub(super) struct Settings {
    pub(super) max_agents: u32,
    pub(super) target_dir: PathBuf,
    pub(super) sccache: Option<String>,
    pub(super) agent: Option<String>,
    pub(super) rules: String,
}

/// The cap recorded in `.phux/mission.toml`, when the repository is known.
pub(in crate::commands) fn configured_max_agents(repo_root: &Path) -> Option<u32> {
    if repo_root.as_os_str().is_empty() {
        return None;
    }
    let state_dir = phux_config::instance::state_dir();
    load_settings(repo_root, &state_dir)
        .ok()
        .map(|settings| settings.max_agents)
}

pub(super) fn load_settings(repo: &Path, state_dir: &Path) -> Result<Settings, String> {
    let mut settings = Settings {
        max_agents: DEFAULT_MAX_AGENTS,
        target_dir: default_target_dir(state_dir),
        sccache: None,
        agent: None,
        rules: DEFAULT_RULES.to_owned(),
    };
    let path = repo.join(".phux").join("mission.toml");
    if !path.exists() {
        return Ok(settings);
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|err| format!("could not read {}: {err}", path.display()))?;
    let value: toml::Value = toml::from_str(&text)
        .map_err(|err| format!("{} is not valid toml: {err}", path.display()))?;
    let Some(mission) = value.get("mission") else {
        return Ok(settings);
    };
    if let Some(max) = mission.get("max_agents") {
        let Some(max) = max.as_integer().and_then(|max| u32::try_from(max).ok()) else {
            return Err("[mission] max_agents must be an integer from 1 up".to_owned());
        };
        if max == 0 {
            return Err("[mission] max_agents must be at least 1".to_owned());
        }
        settings.max_agents = max;
    }
    if let Some(dir) = mission
        .get("shared_target_dir")
        .and_then(toml::Value::as_str)
    {
        let dir = PathBuf::from(dir);
        settings.target_dir = if dir.is_absolute() {
            dir
        } else {
            repo.join(dir)
        };
    }
    match mission.get("sccache") {
        None | Some(toml::Value::Boolean(false)) => {}
        Some(toml::Value::Boolean(true)) => settings.sccache = Some("sccache".to_owned()),
        Some(toml::Value::String(bin)) if !bin.is_empty() => settings.sccache = Some(bin.clone()),
        Some(_) => return Err("[mission] sccache must be true or a program name".to_owned()),
    }
    if let Some(agent) = mission.get("agent").and_then(toml::Value::as_str) {
        settings.agent = Some(agent.to_owned());
    }
    if let Some(rules) = mission.get("rules").and_then(toml::Value::as_str) {
        settings.rules = if rules.ends_with('\n') {
            rules.to_owned()
        } else {
            format!("{rules}\n")
        };
    }
    Ok(settings)
}

pub(super) fn repo_root() -> Result<PathBuf, String> {
    let cwd = std::env::current_dir().map_err(|err| err.to_string())?;
    let root = git_text(&cwd, &["rev-parse", "--show-toplevel"])?;
    PathBuf::from(root.trim())
        .canonicalize()
        .map_err(|err| format!("could not canonicalize the git worktree: {err}"))
}

pub(super) fn origin_head(root: &Path) -> Result<String, String> {
    let text = git_text(root, &["rev-parse", "--abbrev-ref", "origin/HEAD"])?;
    let text = text.trim();
    if text.is_empty() || text == "origin/HEAD" {
        return Err(
            "origin has no default branch; set it with `git remote set-head origin <branch>`"
                .to_owned(),
        );
    }
    Ok(text.to_owned())
}

pub(super) fn write_brief(path: &Path, body: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("could not create {}: {err}", parent.display()))?;
    }
    std::fs::write(path, body).map_err(|err| format!("could not write {}: {err}", path.display()))
}
