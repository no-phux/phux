//! First-run onboarding without a window: its steps and which of them show,
//! and what the phux CLI's `--json` answers say. The desk runs the commands;
//! everything read from them is parsed here, loosely, so a newer or older CLI
//! with extra or missing fields still reads.
//!
//! - `phux skill setup|status --json`: per agent
//!   `{agent, detected, skill:{path, state}, hook:{state, reason?}}`.
//! - `phux server permissions --json`: `{bundle, responsible_path, permissions:[{service,
//!   status}]}`, or the bare array.
//! - `phux server permissions --open SERVICE`: prints the server's bundle.
//! - `phux migrate --json --agent ID`: `{sources, agent, config_path,
//!   launched:{session?, pane?}|null, prompt_bytes}`.

use std::path::Path;

use serde_json::Value;

/// The steps, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Welcome,
    Look,
    Agents,
    Migrate,
    Done,
}

impl Step {
    const ALL: [Self; 5] = [
        Self::Welcome,
        Self::Look,
        Self::Agents,
        Self::Migrate,
        Self::Done,
    ];

    /// As stored in the app's settings.
    pub const fn id(self) -> &'static str {
        match self {
            Self::Welcome => "welcome",
            Self::Look => "look",
            Self::Agents => "agents",
            Self::Migrate => "migrate",
            Self::Done => "done",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|step| step.id() == id)
    }

    /// Bringing your setup over needs an agent to do it.
    pub const fn shown(self, agents: bool) -> bool {
        !matches!(self, Self::Migrate) || agents
    }

    pub fn next(self, agents: bool) -> Option<Self> {
        let at = Self::ALL.iter().position(|s| *s == self)?;
        Self::ALL[at + 1..]
            .iter()
            .copied()
            .find(|s| s.shown(agents))
    }

    pub fn back(self, agents: bool) -> Option<Self> {
        let at = Self::ALL.iter().position(|s| *s == self)?;
        Self::ALL[..at]
            .iter()
            .rev()
            .copied()
            .find(|s| s.shown(agents))
    }
}

/// Whether this computer has the privacy panes the permissions card is about.
pub const PERMISSIONS: bool = cfg!(target_os = "macos");

/// The agents the migration can hand the work to, in tile order.
pub const MIGRATORS: [&str; 3] = ["claude", "codex", "opencode"];

/// An agent's name as people say it.
pub fn agent_name(id: &str) -> String {
    match id {
        "claude" => "Claude Code".to_owned(),
        "codex" => "Codex".to_owned(),
        "opencode" => "OpenCode".to_owned(),
        "pi" => "Pi".to_owned(),
        other => other.to_owned(),
    }
}

/// A CLI too old for a command says so in clap's words; anything else is a
/// real failure worth showing.
pub fn unsupported(error: &str) -> bool {
    [
        "unrecognized subcommand",
        "unexpected argument",
        "unknown subcommand",
        "no such command",
        "phux is not installed",
    ]
    .iter()
    .any(|words| error.contains(words))
}

fn json(bytes: &[u8]) -> Result<Value, String> {
    serde_json::from_slice(bytes).map_err(|error| format!("phux printed no JSON: {error}"))
}

fn string(value: &Value, key: &str) -> Option<String> {
    value[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// The list in `value`: the value itself, or the first array under one of `keys`.
fn list<'a>(value: &'a Value, keys: &[&str]) -> &'a [Value] {
    value
        .as_array()
        .or_else(|| keys.iter().find_map(|key| value[*key].as_array()))
        .map_or(&[], Vec::as_slice)
}

/// One agent's skill and hook, as `phux skill status` or `setup` left them.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AgentSkill {
    pub agent: String,
    pub detected: bool,
    pub skill: Option<String>,
    pub hook: Option<String>,
    pub reason: Option<String>,
}

impl AgentSkill {
    /// Whether the skill is in place, whatever the last run did to it.
    pub fn installed(&self) -> bool {
        const IN_PLACE: [&str; 9] = [
            "installed",
            "present",
            "current",
            "up_to_date",
            "unchanged",
            "updated",
            "created",
            "replaced",
            "ok",
        ];
        self.skill.as_deref().is_some_and(|s| IN_PLACE.contains(&s))
    }

    /// What to say about it beside its name.
    pub fn label(&self) -> &'static str {
        match self.skill.as_deref() {
            _ if self.installed() => "Installed",
            Some("stale" | "conflict") => "Needs update",
            _ => "Not installed",
        }
    }

    /// The hook's state, for the label's tooltip.
    pub fn hook_detail(&self) -> Option<String> {
        match (&self.hook, &self.reason) {
            (_, Some(reason)) => Some(format!("Hook: {reason}")),
            (Some(hook), None) => Some(format!("Hook: {}", hook.replace('_', " "))),
            (None, None) => None,
        }
    }
}

pub fn parse_skills(bytes: &[u8]) -> Result<Vec<AgentSkill>, String> {
    let value = json(bytes)?;
    Ok(list(&value, &["agents", "results"])
        .iter()
        .filter_map(|row| {
            Some(AgentSkill {
                agent: string(row, "agent")?,
                detected: row["detected"].as_bool().unwrap_or(false),
                skill: string(&row["skill"], "state").or_else(|| string(row, "skill")),
                hook: string(&row["hook"], "state"),
                reason: string(&row["hook"], "reason"),
            })
        })
        .collect())
}

/// The agents found on this computer that can bring a setup over.
pub fn migrators(skills: &[AgentSkill]) -> Vec<&'static str> {
    MIGRATORS
        .into_iter()
        .filter(|id| skills.iter().any(|s| s.detected && s.agent == *id))
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Service {
    FullDiskAccess,
    ScreenRecording,
    Accessibility,
}

impl Service {
    pub const ALL: [Self; 3] = [
        Self::FullDiskAccess,
        Self::ScreenRecording,
        Self::Accessibility,
    ];

    /// What `phux server permissions` calls it.
    pub const fn id(self) -> &'static str {
        match self {
            Self::FullDiskAccess => "full_disk_access",
            Self::ScreenRecording => "screen_recording",
            Self::Accessibility => "accessibility",
        }
    }

    pub const fn title(self) -> &'static str {
        match self {
            Self::FullDiskAccess => "Full Disk Access",
            Self::ScreenRecording => "Screen Recording",
            Self::Accessibility => "Accessibility",
        }
    }

    pub const fn why(self) -> &'static str {
        match self {
            Self::FullDiskAccess => {
                "Lets the shells and agents in phux open files in protected folders."
            }
            Self::ScreenRecording => {
                "Lets agents in phux capture a window or the screen when you ask them to."
            }
            Self::Accessibility => {
                "Lets agents in phux read and drive other apps' controls for you."
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grant {
    Granted,
    Denied,
    NotDetermined,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Permissions {
    /// The app macOS holds responsible for the server: what to drag into a list.
    pub responsible_path: Option<String>,
    pub services: Vec<(Service, Grant)>,
}

impl Permissions {
    pub fn grant(&self, service: Service) -> Grant {
        self.services
            .iter()
            .find(|(s, _)| *s == service)
            .map_or(Grant::Unknown, |(_, g)| *g)
    }

    pub fn granted(&self) -> usize {
        Service::ALL
            .into_iter()
            .filter(|s| self.grant(*s) == Grant::Granted)
            .count()
    }
}

pub fn parse_permissions(bytes: &[u8]) -> Result<Permissions, String> {
    let value = json(bytes)?;
    let services = list(&value, &["permissions", "services"])
        .iter()
        .filter_map(|row| {
            let id = row["service"].as_str()?;
            let service = Service::ALL.into_iter().find(|s| s.id() == id)?;
            let grant = match row["status"].as_str() {
                Some("granted") => Grant::Granted,
                Some("denied") => Grant::Denied,
                Some("not_determined") => Grant::NotDetermined,
                _ => Grant::Unknown,
            };
            Some((service, grant))
        })
        .collect();
    Ok(Permissions {
        responsible_path: string(&value, "bundle").or_else(|| string(&value, "responsible_path")),
        services,
    })
}

/// The bundle `--open` printed: a JSON field when it printed JSON, else its
/// last line when that is a path.
pub fn parse_opened(bytes: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(bytes);
    if let Ok(value) = serde_json::from_str::<Value>(text.trim()) {
        return ["bundle_path", "bundle", "responsible_path", "path"]
            .iter()
            .find_map(|key| string(&value, key));
    }
    let line = text.lines().map(str::trim).rfind(|l| !l.is_empty())?;
    line.starts_with('/').then(|| line.to_owned())
}

/// Where `phux migrate` started its agent.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Migration {
    pub agent: Option<String>,
    pub session: Option<String>,
    /// The pane's terminal number (`@N`).
    pub pane: Option<u32>,
    /// What it found to bring over, by name.
    pub sources: Vec<String>,
}

pub fn parse_migration(bytes: &[u8]) -> Result<Migration, String> {
    let value = json(bytes)?;
    let launched = &value["launched"];
    let pane = &launched["pane"];
    let pane = pane.as_u64().map(|n| n.to_string()).or_else(|| {
        pane.as_str().map(|s| {
            s.rsplit('/')
                .next()
                .unwrap_or(s)
                .trim_start_matches('@')
                .to_owned()
        })
    });
    Ok(Migration {
        agent: string(&value, "agent"),
        session: string(launched, "session"),
        pane: pane.and_then(|p| p.parse().ok()),
        sources: list(&value, &["sources"])
            .iter()
            .filter_map(|s| {
                s.as_str().map(str::to_owned).or_else(|| {
                    ["name", "kind", "source", "path"]
                        .iter()
                        .find_map(|key| string(s, key))
                })
            })
            .collect(),
    })
}

/// A new session's name, as the CLI's default names it: the folder's base
/// name with anything a selector reserves made `-`, unique among `taken`.
pub fn session_name(folder: &Path, taken: &[String]) -> String {
    let base: String = folder
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "._-".contains(c) {
                c
            } else {
                '-'
            }
        })
        .collect();
    let base = base.trim_matches(['-', '.']);
    let base = if base.is_empty() { "home" } else { base };
    (1..)
        .map(|n| match n {
            1 => base.to_owned(),
            n => format!("{base}-{n}"),
        })
        .find(|name| !taken.contains(name))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bringing_your_setup_is_skipped_without_an_agent() {
        assert_eq!(Step::Agents.next(false), Some(Step::Done));
        assert_eq!(Step::Done.back(false), Some(Step::Agents));
        assert_eq!(Step::Agents.next(true), Some(Step::Migrate));
        assert_eq!(Step::Done.back(true), Some(Step::Migrate));
        assert_eq!(Step::Welcome.back(true), None);
        assert_eq!(Step::Done.next(true), None);
        for step in Step::ALL {
            assert_eq!(Step::from_id(step.id()), Some(step));
        }
        assert_eq!(Step::from_id("gone"), None);
    }

    #[test]
    fn skills_read_per_agent_and_tolerate_shapes() {
        let status = br#"[
            {"agent":"claude","detected":true,
             "skill":{"path":"/h/.claude/skills/phux/SKILL.md","state":"installed"},
             "hook":{"state":"installed"},"extra":1},
            {"agent":"codex","detected":true,"skill":{"state":"missing"},
             "hook":{"state":"skipped","reason":"codex has no hooks"}},
            {"agent":"opencode","detected":false,"skill":{"state":"missing"}},
            {"detected":true}
        ]"#;
        let skills = parse_skills(status).unwrap();
        assert_eq!(skills.len(), 3, "a row without an agent is dropped");
        assert!(skills[0].installed());
        assert!(!skills[1].installed());
        assert_eq!(skills[0].label(), "Installed");
        assert_eq!(skills[1].label(), "Not installed");
        assert_eq!(
            skills[1].hook_detail().as_deref(),
            Some("Hook: codex has no hooks")
        );
        assert_eq!(skills[0].hook_detail().as_deref(), Some("Hook: installed"));
        let stale = AgentSkill {
            skill: Some("stale".into()),
            ..AgentSkill::default()
        };
        assert_eq!(stale.label(), "Needs update");
        assert_eq!(stale.hook_detail(), None);
        assert_eq!(migrators(&skills), ["claude", "codex"]);
        let wrapped = br#"{"agents":[{"agent":"claude","skill":{"state":"updated"}}]}"#;
        let skills = parse_skills(wrapped).unwrap();
        assert!(skills[0].installed() && !skills[0].detected);
        assert!(
            migrators(&skills).is_empty(),
            "undetected agents migrate nothing"
        );
        assert!(parse_skills(b"usage: phux skill").is_err());
    }

    #[test]
    fn permissions_count_what_is_granted() {
        let status = br#"{"responsible_path":"/Apps/phux Server.app","services":[
            {"service":"full_disk_access","status":"granted"},
            {"service":"screen_recording","status":"denied"},
            {"service":"accessibility","status":"not_determined"},
            {"service":"camera","status":"granted"}]}"#;
        let perms = parse_permissions(status).unwrap();
        assert_eq!(perms.granted(), 1);
        assert_eq!(perms.grant(Service::ScreenRecording), Grant::Denied);
        assert_eq!(perms.grant(Service::Accessibility), Grant::NotDetermined);
        assert_eq!(
            perms.responsible_path.as_deref(),
            Some("/Apps/phux Server.app")
        );
        let shipped = br#"{"executable":"/x/phux","responsible_path":"/Apps/phux Server.app",
            "bundle":"/S/Current/phux Server.app",
            "permissions":[{"service":"screen_recording","status":"granted"}]}"#;
        let perms = parse_permissions(shipped).unwrap();
        assert_eq!(perms.grant(Service::ScreenRecording), Grant::Granted);
        assert_eq!(
            perms.responsible_path.as_deref(),
            Some("/S/Current/phux Server.app")
        );
        let bare = br#"[{"service":"accessibility","status":"granted"}]"#;
        let perms = parse_permissions(bare).unwrap();
        assert_eq!(perms.granted(), 1);
        assert_eq!(perms.grant(Service::FullDiskAccess), Grant::Unknown);
        assert_eq!(perms.responsible_path, None);
    }

    #[test]
    fn the_opened_bundle_comes_from_json_or_the_last_path() {
        assert_eq!(
            parse_opened(b"Opening Full Disk Access\n/Apps/phux Server.app\n").as_deref(),
            Some("/Apps/phux Server.app")
        );
        assert_eq!(
            parse_opened(br#"{"bundle_path":"/B.app"}"#).as_deref(),
            Some("/B.app")
        );
        assert_eq!(parse_opened(b"opened\n"), None);
        assert_eq!(parse_opened(b""), None);
    }

    #[test]
    fn a_migration_names_where_its_agent_runs() {
        let out = br#"{"sources":["tmux",{"name":"ghostty","path":"/x"}],"agent":"claude",
            "config_path":"/h/.config/phux/config.kdl",
            "launched":{"session":"migrate","pane":"@12"},"prompt_bytes":4096}"#;
        let m = parse_migration(out).unwrap();
        assert_eq!(m.agent.as_deref(), Some("claude"));
        assert_eq!(m.session.as_deref(), Some("migrate"));
        assert_eq!(m.pane, Some(12));
        assert_eq!(m.sources, ["tmux", "ghostty"]);
        let numeric = parse_migration(br#"{"launched":{"pane":7}}"#).unwrap();
        assert_eq!((numeric.pane, numeric.session), (Some(7), None));
        let none = parse_migration(br#"{"launched":null}"#).unwrap();
        assert_eq!(none, Migration::default());
    }

    #[test]
    fn an_old_cli_reads_as_unsupported() {
        assert!(unsupported(
            "`phux skill setup --json` failed: error: unrecognized subcommand 'setup'"
        ));
        assert!(unsupported("error: unexpected argument '--json' found"));
        assert!(!unsupported("permission denied"));
        assert!(!unsupported(
            r#"{"error":{"message":"claude is not installed"}}"#
        ));
    }

    #[test]
    fn a_session_is_named_after_the_folder() {
        let taken = vec!["james".to_owned()];
        assert_eq!(session_name(Path::new("/Users/me"), &[]), "me");
        assert_eq!(session_name(Path::new("/Users/james"), &taken), "james-2");
        assert_eq!(session_name(Path::new("/home/a b@c"), &[]), "a-b-c");
        assert_eq!(session_name(Path::new("/"), &[]), "home");
    }
}
