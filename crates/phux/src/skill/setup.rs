//! `phux skill setup | status | remove`: a stub `phux` skill in each agent's
//! skill directory, plus one Claude Code `SessionStart` hook.
//!
//! The stub carries no guidance of its own. It tells the agent to run
//! `phux skill`, so what the agent reads always matches the installed binary.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde::Serialize;
use usage::ValueEnum;

use crate::commands::agent::shim::{atomic_write, installed_shim_path};
use crate::commands::json_err::{self, CliError, codes};
use crate::companion::is_executable;

mod hook;
#[cfg(test)]
mod tests;

#[cfg(test)]
use hook::HOOK_COMMAND;
use hook::hook_step;

/// An agent `phux skill setup` knows how to teach.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum SkillAgent {
    /// Claude Code: `.claude/skills`, plus a `SessionStart` hook.
    Claude,
    /// Codex: the shared `.agents/skills`.
    Codex,
    /// `OpenCode`: the shared `.agents/skills`.
    Opencode,
    /// pi: the shared `.agents/skills`.
    Pi,
}

pub(crate) const ALL: [SkillAgent; 4] = [
    SkillAgent::Claude,
    SkillAgent::Codex,
    SkillAgent::Opencode,
    SkillAgent::Pi,
];

impl SkillAgent {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Opencode => "opencode",
            Self::Pi => "pi",
        }
    }

    /// The skill root under a home or repository. Codex, `OpenCode` and pi
    /// all read the Agent Skills location `.agents/skills`.
    const fn skills_dir(self) -> &'static str {
        match self {
            Self::Claude => ".claude/skills",
            _ => ".agents/skills",
        }
    }

    /// Installed: its binary is on `PATH`, or its config directory exists.
    fn detected(self, ctx: &Ctx) -> bool {
        let (bin, config) = match self {
            Self::Claude => ("claude", ".claude"),
            Self::Codex => ("codex", ".codex"),
            Self::Opencode => ("opencode", ".config/opencode"),
            Self::Pi => ("pi", ".pi"),
        };
        ctx.home.join(config).is_dir()
            || ctx.path.as_ref().is_some_and(|path| {
                std::env::split_paths(path).any(|dir| is_executable(&dir.join(bin)))
            })
    }
}

/// Any file carrying this frontmatter key is a stub phux installed. Bump
/// its value in [`STUB`] with every change, so an older stub reads as stale.
const STUB_MARKER: &str = "phux-stub:";
const STUB: &str = r#"---
name: phux
description: Drive phux, the terminal multiplexer built for agents. Use for any task about terminals, sessions, panes, sending keys to or reading a terminal screen, running a command in another pane, or starting, watching and supervising coding agents.
metadata:
  phux-stub: "1"
---

<!-- Installed by `phux skill setup`. Remove it with `phux skill remove`. -->

# phux

Run `phux skill` and follow the guide it prints. The guide is built into the
installed phux binary, so it always matches that version. `phux skill quick`
prints only the core loop and safety rules; `phux skill agent` adds agent
supervision and `phux skill terminal` adds screen and input mechanics.

If `phux skill` fails, start with `phux --help` and `phux doctor`.
"#;

/// Where to install, and what the environment looks like.
#[derive(Debug)]
pub(super) struct Ctx {
    home: PathBuf,
    /// `home`, or the repository root with `--project`.
    base: PathBuf,
    path: Option<OsString>,
    project: bool,
    /// Where the `install-claude` shim would be.
    shim: Option<PathBuf>,
}

impl Ctx {
    fn from_env(project: bool) -> Result<Self, String> {
        let home = std::env::var_os("HOME")
            .filter(|home| !home.is_empty())
            .map(PathBuf::from)
            .ok_or("HOME is unset, so there is no skill directory to use")?;
        let base = if project {
            let cwd = std::env::current_dir()
                .map_err(|err| format!("could not read the current directory: {err}"))?;
            cwd.ancestors()
                .find(|dir| dir.join(".git").exists())
                .unwrap_or(&cwd)
                .to_path_buf()
        } else {
            home.clone()
        };
        Ok(Self {
            home,
            base,
            path: std::env::var_os("PATH"),
            project,
            shim: installed_shim_path(),
        })
    }

    fn skills(&self, agent: SkillAgent) -> PathBuf {
        self.base.join(agent.skills_dir())
    }

    fn settings(&self) -> PathBuf {
        self.base.join(".claude/settings.json")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Mode {
    Setup { force: bool },
    Status,
    Remove,
}

/// One state for a skill or a hook. See docs/agent-skill.md for which verb
/// reports which.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum State {
    Installed,
    Updated,
    UpToDate,
    Stale,
    Missing,
    Conflict,
    Present,
    Skipped,
    Removed,
}

impl State {
    const fn word(self) -> &'static str {
        match self {
            Self::Installed => "installed",
            Self::Updated => "updated",
            Self::UpToDate => "up to date",
            Self::Stale => "stale",
            Self::Missing => "not installed",
            Self::Conflict => "conflict",
            Self::Present => "present",
            Self::Skipped => "skipped",
            Self::Removed => "removed",
        }
    }
}

#[derive(Debug, Serialize)]
pub(super) struct Skill {
    pub(super) path: PathBuf,
    pub(super) state: State,
}

#[derive(Debug, Serialize)]
pub(super) struct Hook {
    pub(super) state: State,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) reason: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct AgentReport {
    pub(super) agent: &'static str,
    pub(super) detected: bool,
    pub(super) skill: Skill,
    pub(super) hook: Hook,
}

#[derive(Debug, Serialize)]
pub(super) struct Report {
    schema: &'static str,
    action: &'static str,
    dry_run: bool,
    scope: &'static str,
    pub(super) agents: Vec<AgentReport>,
}

/// `phux skill setup`, `status` and `remove`.
pub(crate) fn run(
    mode: Mode,
    agents: &[SkillAgent],
    project: bool,
    dry_run: bool,
    json: bool,
) -> ExitCode {
    let report = Ctx::from_env(project).and_then(|ctx| apply(&ctx, mode, agents, dry_run));
    match report {
        Ok(report) if json => crate::output::json(&report),
        Ok(report) => {
            print_human(&report);
            ExitCode::SUCCESS
        }
        Err(err) => json_err::emit(
            json,
            &CliError::new(codes::IO, err, "fix the path above and re-run"),
            1,
        ),
    }
}

/// The work behind [`run`], against an explicit [`Ctx`].
pub(super) fn apply(
    ctx: &Ctx,
    mode: Mode,
    explicit: &[SkillAgent],
    dry_run: bool,
) -> Result<Report, String> {
    let detected = ALL.map(|agent| agent.detected(ctx));
    let targets: Vec<SkillAgent> = if !explicit.is_empty() {
        explicit.to_vec()
    } else if !matches!(mode, Mode::Setup { .. }) {
        ALL.to_vec()
    } else if detected.contains(&true) {
        ALL.into_iter()
            .zip(detected)
            .filter(|(_, d)| *d)
            .map(|(a, _)| a)
            .collect()
    } else {
        // No agent found: the shared Agent Skills directory only.
        vec![SkillAgent::Codex, SkillAgent::Opencode, SkillAgent::Pi]
    };

    // Agents sharing a directory share one result, so it is written once.
    let mut done: Vec<(PathBuf, State)> = Vec::new();
    let mut agents = Vec::new();
    for (agent, detected) in ALL.into_iter().zip(detected) {
        let skills = ctx.skills(agent);
        let path = skills.join("phux").join("SKILL.md");
        let targeted = targets.contains(&agent);
        let state = if !targeted {
            State::Skipped
        } else if let Some((_, state)) = done.iter().find(|(dir, _)| *dir == skills) {
            *state
        } else {
            let state = skill_step(&skills, mode, dry_run)?;
            done.push((skills, state));
            state
        };
        let hook = if agent != SkillAgent::Claude {
            Hook {
                state: State::Skipped,
                reason: Some(format!(
                    "phux installs no session hook for {}",
                    agent.name()
                )),
            }
        } else if !targeted {
            Hook {
                state: State::Skipped,
                reason: Some("Claude Code is not a target".to_owned()),
            }
        } else {
            hook_step(ctx, mode, dry_run)?
        };
        agents.push(AgentReport {
            agent: agent.name(),
            detected,
            skill: Skill { path, state },
            hook,
        });
    }
    Ok(Report {
        schema: "phux.skill-setup/v1",
        action: match mode {
            Mode::Setup { .. } => "setup",
            Mode::Status => "status",
            Mode::Remove => "remove",
        },
        dry_run,
        scope: if ctx.project { "project" } else { "user" },
        agents,
    })
}

fn is_stub(text: &str) -> bool {
    text.lines()
        .any(|line| line.trim_start().starts_with(STUB_MARKER))
}

/// The stub's state in one skill root, changing nothing.
fn skill_status(skills: &Path) -> State {
    match std::fs::read_to_string(skills.join("phux/SKILL.md")) {
        Ok(text) if text == STUB => State::UpToDate,
        Ok(text) if is_stub(&text) => State::Stale,
        Ok(_) => State::Conflict,
        Err(_) if skills.join("phux").symlink_metadata().is_ok() => State::Conflict,
        // `npx skills add no-phux/skills` installs the full guide as `using-phux`.
        Err(_) if skills.join("using-phux/SKILL.md").exists() => State::Present,
        Err(_) => State::Missing,
    }
}

fn skill_step(skills: &Path, mode: Mode, dry_run: bool) -> Result<State, String> {
    let status = skill_status(skills);
    let dir = skills.join("phux");
    let file = dir.join("SKILL.md");
    let next = match (mode, status) {
        (Mode::Setup { .. }, State::Stale) => State::Updated,
        (Mode::Remove, State::UpToDate | State::Stale) => State::Removed,
        (Mode::Setup { force }, state)
            if state != State::UpToDate
                && (force || !matches!(state, State::Conflict | State::Present)) =>
        {
            State::Installed
        }
        // Status, and every case setup or remove leaves alone.
        (_, state) => return Ok(state),
    };
    if dry_run {
        return Ok(next);
    }
    if next == State::Removed {
        std::fs::remove_file(&file)
            .map_err(|err| format!("could not remove {}: {err}", file.display()))?;
        // Only when phux's file was the last thing in it.
        let _ = std::fs::remove_dir(&dir);
        return Ok(next);
    }
    // `--force` over a symlinked skill replaces the link, never its target.
    if dir.symlink_metadata().is_ok_and(|meta| meta.is_symlink()) {
        std::fs::remove_file(&dir)
            .map_err(|err| format!("could not replace {}: {err}", dir.display()))?;
    }
    if dir.is_file() {
        return Err(format!(
            "{} is a file, not a skill directory",
            dir.display()
        ));
    }
    atomic_write(&file, STUB.as_bytes(), 0o644)?;
    Ok(next)
}

fn print_human(report: &Report) {
    for agent in &report.agents {
        let skipped = agent.skill.state == State::Skipped;
        if skipped && !agent.detected {
            outln!("{}: not detected, skipped", agent.agent);
            continue;
        }
        let hook = if agent.agent == "claude" && !skipped {
            let reason = agent
                .hook
                .reason
                .as_ref()
                .map(|reason| format!(" ({reason})"))
                .unwrap_or_default();
            format!("; SessionStart hook {}{reason}", agent.hook.state.word())
        } else {
            String::new()
        };
        outln!(
            "{}: skill {} ({}){hook}",
            agent.agent,
            agent.skill.state.word(),
            agent.skill.path.display()
        );
    }
    let skills = report.agents.iter().map(|agent| agent.skill.state);
    if skills.clone().any(|state| state == State::Conflict) {
        hint!(
            "a `phux` skill phux did not install was left alone; `phux skill setup --force` \
             replaces it"
        );
    }
    if report.dry_run {
        hint!("dry run: nothing was changed");
    }
}

/// `phux doctor`'s view: is a phux skill installed for any user agent?
pub(crate) fn doctor_summary() -> Result<(Vec<&'static str>, Vec<&'static str>), String> {
    let ctx = Ctx::from_env(false)?;
    let report = apply(&ctx, Mode::Status, &[], false)?;
    let pick = |want: &[State]| {
        report
            .agents
            .iter()
            .filter(|agent| want.contains(&agent.skill.state))
            .map(|agent| agent.agent)
            .collect::<Vec<_>>()
    };
    Ok((
        pick(&[State::UpToDate, State::Present]),
        pick(&[State::Stale]),
    ))
}

/// The hidden `phux skill session-start` hook: one line of context for an
/// agent's new session. Always exits 0.
pub(crate) fn session_start(socket: Option<PathBuf>) -> ExitCode {
    let pane = std::env::var("PHUX_TERMINAL_ID")
        .ok()
        .and_then(|id| id.parse::<u32>().ok());
    let guide = "Run `phux skill` for the guide to driving phux panes, sessions and agents.";
    match pane {
        Some(id) => {
            let session = session_name(socket, id)
                .map(|name| format!(" in session \"{name}\""))
                .unwrap_or_default();
            outln!("phux: this agent runs inside phux pane @{id}{session}. {guide}");
        }
        None => outln!("phux is installed, but this agent is not inside a phux pane. {guide}"),
    }
    ExitCode::SUCCESS
}

/// The pane's session name, if its server answers within two seconds.
fn session_name(socket: Option<PathBuf>, id: u32) -> Option<String> {
    let socket = socket.unwrap_or_else(phux_server::runtime::default_socket_path);
    let rt = crate::commands::cli_runtime().ok()?;
    let state = rt
        .block_on(async {
            tokio::time::timeout(
                std::time::Duration::from_secs(2),
                phux_client::state::get_state(&socket),
            )
            .await
        })
        .ok()?
        .ok()?;
    let snapshot = state.into_snapshot_ignoring_degradation();
    let pane = phux_protocol::ids::ResourceId::local(id);
    let session = phux_client::spawn::ownership_for_terminal(&snapshot, &pane)?;
    snapshot
        .sessions
        .into_iter()
        .find(|candidate| candidate.id == session)
        .map(|session| session.name)
}
