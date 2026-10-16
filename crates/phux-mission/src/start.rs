//! Planning for `mission start`: branch, brief, launch argv, and build env.
//!
//! The git checkout, the session, and the agent process are started by the
//! `phux` binary. This module is the part that does not need a server.

use std::path::{Path, PathBuf};

/// Sentence passed as the harness's initial message. The brief itself is a
/// file argument, never keystrokes.
pub const START_LINE: &str = "Start the task in the brief.";

/// Rules written into `brief.md` when the repo config names none.
pub const DEFAULT_RULES: &str = "\
Work only in this worktree.
Read this brief and do the task.
When you are blocked or finished, stop and say so in the pane.
Do not open a pull request and do not merge.
";

/// Default number of agents working at once.
pub const DEFAULT_MAX_AGENTS: u32 = 4;

/// Paths `mission start` uses for one task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskPaths {
    /// `mission/<n>-<slug>`.
    pub branch: String,
    /// Mission-owned worktree directory.
    pub worktree: PathBuf,
    /// `tasks/<repo>/<n>/brief.md` under the state directory.
    pub brief: PathBuf,
}

/// `mission/<n>-<slug>` from the task number and title.
#[must_use]
pub fn branch_name(number: u64, title: &str) -> String {
    format!("mission/{number}-{}", slug(title))
}

/// Worker tab label, for example `#3 Billing page`.
#[must_use]
pub fn tab_title(number: u64, title: &str) -> String {
    format!("#{number} {title}")
}

/// Lowercase slug: ASCII letters and digits, runs of anything else become
/// one `-`. Empty titles become `task`.
#[must_use]
pub fn slug(title: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.len() > 48 {
        out.truncate(48);
        while out.ends_with('-') {
            out.pop();
        }
    }
    if out.is_empty() {
        out.push_str("task");
    }
    out
}

/// Worktree, branch, and brief paths for one task.
#[must_use]
pub fn task_paths(state_dir: &Path, repo: &str, number: u64, title: &str) -> TaskPaths {
    let slug = slug(title);
    TaskPaths {
        branch: format!("mission/{number}-{slug}"),
        worktree: state_dir
            .join("mission")
            .join("worktrees")
            .join(repo)
            .join(format!("{number}-{slug}")),
        brief: state_dir
            .join("mission")
            .join("tasks")
            .join(repo)
            .join(number.to_string())
            .join("brief.md"),
    }
}

/// Shared Cargo output directory when the repo config does not name one.
#[must_use]
pub fn default_target_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("mission").join("cargo-target")
}

/// Environment for every task worktree: one `CARGO_TARGET_DIR`, plus
/// `RUSTC_WRAPPER` when sccache is configured.
#[must_use]
pub fn build_env(target_dir: &Path, sccache: Option<&str>) -> Vec<(String, String)> {
    let mut env = vec![(
        "CARGO_TARGET_DIR".to_owned(),
        target_dir.display().to_string(),
    )];
    if let Some(wrapper) = sccache.map(str::trim).filter(|wrapper| !wrapper.is_empty()) {
        env.push(("RUSTC_WRAPPER".to_owned(), wrapper.to_owned()));
    }
    env
}

/// `brief.md`: the task, then the rules.
#[must_use]
pub fn brief_markdown(
    number: u64,
    alias: &str,
    repo: &str,
    title: &str,
    description: &str,
    rules: &str,
) -> String {
    let description = if description.is_empty() {
        "(no description)"
    } else {
        description
    };
    format!(
        "# {}\n\nAlias: {alias}\nRepo: {repo}\n\n{description}\n\n## Rules\n\n{rules}",
        tab_title(number, title),
    )
}

/// Split `KIND` or `KIND/MODEL`.
///
/// # Errors
///
/// The kind or model is empty or uses a character a launch id cannot carry.
pub fn parse_agent(text: &str) -> Result<(String, Option<String>), phux_work::Error> {
    let (kind, model) = match text.split_once('/') {
        Some((kind, model)) => (kind, Some(model)),
        None => (text, None),
    };
    if !token_ok(kind, 64) || model.is_some_and(|model| !token_ok(model, 128)) {
        return Err(phux_work::Error::InvalidText {
            field: "agent".to_owned(),
            reason: "must look like KIND or KIND/MODEL (letters, digits, `.`, `_`, `-`)".to_owned(),
        });
    }
    Ok((kind.to_owned(), model.map(str::to_owned)))
}

fn token_ok(text: &str, max: usize) -> bool {
    !text.is_empty()
        && text.len() <= max
        && text
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.')
}

/// Launch template ids to try for an agent kind, preferred first.
///
/// `claude` launches `claude-code`. `opencode-v2` launches `opencode` when
/// that is the template id on disk.
#[must_use]
pub fn integration_ids(kind: &str) -> Vec<&str> {
    match kind {
        "claude" => vec!["claude-code", "claude"],
        "opencode-v2" => vec!["opencode-v2", "opencode"],
        other => vec![other],
    }
}

/// Argv appended to a harness template so the brief is a file, not typed keys.
///
/// `integration_id` is the launch template id (`claude-code`, `grok`, `omp`,
/// `opencode`, `pi`, or `opencode-v2`).
#[must_use]
pub fn brief_argv(integration_id: &str, brief: &Path, model: Option<&str>) -> Vec<String> {
    let brief = brief.display().to_string();
    let mut args = match integration_id {
        "grok" => vec!["--prompt-file".to_owned(), brief],
        "opencode" | "opencode-v2" => vec![
            "run".to_owned(),
            "--file".to_owned(),
            brief,
            START_LINE.to_owned(),
        ],
        "pi" | "omp" => vec![
            "--append-system-prompt".to_owned(),
            brief,
            START_LINE.to_owned(),
        ],
        _ => vec![
            "--append-system-prompt-file".to_owned(),
            brief,
            START_LINE.to_owned(),
        ],
    };
    if let Some(model) = model.map(str::trim).filter(|model| !model.is_empty()) {
        let model_flag = vec!["--model".to_owned(), model.to_owned()];
        if args.last().is_some_and(|last| last == START_LINE) {
            let at = args.len() - 1;
            args.splice(at..at, model_flag);
        } else {
            args.extend(model_flag);
        }
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_titles_into_branch_names_and_tab_titles() {
        assert_eq!(slug("Billing page"), "billing-page");
        assert_eq!(slug("  "), "task");
        assert_eq!(slug("A--B"), "a-b");
        assert_eq!(branch_name(3, "Billing page"), "mission/3-billing-page");
        assert_eq!(tab_title(3, "Billing page"), "#3 Billing page");
    }

    #[test]
    fn paths_stay_under_the_mission_root() {
        let paths = task_paths(Path::new("/state"), "phux", 3, "Billing page");
        assert_eq!(
            paths.worktree,
            PathBuf::from("/state/mission/worktrees/phux/3-billing-page")
        );
        assert_eq!(
            paths.brief,
            PathBuf::from("/state/mission/tasks/phux/3/brief.md")
        );
        assert_eq!(paths.branch, "mission/3-billing-page");
    }

    #[test]
    fn build_env_shares_one_target_dir_and_optional_sccache() {
        let env = build_env(Path::new("/cache/target"), None);
        assert_eq!(
            env,
            vec![("CARGO_TARGET_DIR".into(), "/cache/target".into())]
        );
        let with = build_env(Path::new("/cache/target"), Some("sccache"));
        assert_eq!(with[1], ("RUSTC_WRAPPER".into(), "sccache".into()));
    }

    #[test]
    fn brief_contains_the_task_and_the_rules() {
        let text = brief_markdown(3, "phux/3", "phux", "Billing page", "Fix it", "Be brief.\n");
        assert!(text.starts_with("# #3 Billing page\n"));
        assert!(text.contains("Alias: phux/3"));
        assert!(text.contains("Fix it"));
        assert!(text.contains("## Rules\n\nBe brief."));
    }

    #[test]
    fn harness_argv_passes_the_brief_as_a_file() {
        let brief = Path::new("/mission/tasks/phux/3/brief.md");
        let file = brief.display().to_string();
        assert_eq!(
            brief_argv("claude-code", brief, Some("opus")),
            vec![
                "--append-system-prompt-file".to_owned(),
                file.clone(),
                "--model".to_owned(),
                "opus".to_owned(),
                START_LINE.to_owned(),
            ]
        );
        assert_eq!(
            &brief_argv("grok", brief, None)[..2],
            ["--prompt-file".to_owned(), file.clone()].as_slice()
        );
        assert_eq!(
            brief_argv("pi", brief, None)[0].as_str(),
            "--append-system-prompt"
        );
        assert_eq!(
            brief_argv("omp", brief, None)[0].as_str(),
            "--append-system-prompt"
        );
        assert_eq!(
            &brief_argv("opencode-v2", brief, None)[..3],
            ["run".to_owned(), "--file".to_owned(), file].as_slice()
        );
        assert!(parse_agent("claude/opus").is_ok());
        assert!(parse_agent("opencode-v2").is_ok());
        assert!(parse_agent("/opus").is_err());
    }
}
