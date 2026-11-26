//! `[restore]`: bringing panes back after a reboot or a crash.

use serde::{Deserialize, Serialize};

/// The `command` element a program's resume token replaces.
pub const RESUME_TOKEN_PLACEHOLDER: &str = "${PHUX_RESUME_TOKEN}";

/// `[restore]` table: bringing panes back after a reboot or a crash.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct RestoreCfg {
    /// Run `phux restore` when the server starts: relaunch every pane a
    /// server stop ended, and resume agents whose integration supports it.
    /// Nothing is typed into a relaunched program.
    pub on_start: bool,
    /// `[[restore.program]]`: the command a relaunch runs for a pane whose
    /// program handed over a resume token (OSC 7501 `resume=`), by `app`.
    /// A program with no entry here is never resumed. Legacy: new programs
    /// report with OSC 7577 instead.
    #[serde(deserialize_with = "programs")]
    pub program: Vec<RestoreProgram>,
    /// Accept OSC 7577 program-resume reports and, when a record is trusted
    /// and listed in `[[restore.trust]]`, relaunch that command after a crash
    /// or reboot. Off by default. A server running as root never resumes one.
    pub program_resume: bool,
    /// `[[restore.trust]]`: programs a trusted OSC 7577 record may relaunch.
    /// Each `command` is the program, a bare name or an absolute path. There are
    /// no built-in rules; an empty list resumes nothing.
    #[serde(deserialize_with = "trust")]
    pub trust: Vec<RestoreTrust>,
    /// Lines of each pane's output to keep on disk, at most 10000, and show
    /// above a divider when the pane starts again after the server stopped.
    /// `0`, the default, keeps nothing, and deletes what an earlier setting
    /// kept when the server next starts. The files are readable by you only
    /// and not encrypted.
    #[serde(deserialize_with = "scrollback")]
    #[schemars(range(max = 10_000))]
    pub scrollback: u32,
    /// Panes whose output is never kept: each entry is a session's name, a
    /// pane's name (`phux rename`), or the program a pane was started with.
    pub scrollback_exclude: Vec<String>,
}

/// Most lines [`RestoreCfg::scrollback`] keeps: what one screen read covers.
const MAX_RESTORE_SCROLLBACK: u32 = 10_000;

/// One `[[restore.program]]` entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RestoreProgram {
    /// The OSC 7501 `app` this entry resumes.
    pub app: String,
    /// The argv to run, without a shell. Each element that is exactly
    /// [`RESUME_TOKEN_PLACEHOLDER`] becomes the token.
    pub command: Vec<String>,
}

/// One `[[restore.trust]]` entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RestoreTrust {
    /// Program a resume command may relaunch: a bare name or an absolute path.
    pub command: String,
}

impl RestoreCfg {
    /// Whether `argv`'s program is listed in [`Self::trust`].
    #[must_use]
    pub fn trusts_program(&self, argv: &[String]) -> bool {
        let Some(program) = argv.first() else {
            return false;
        };
        self.trust.iter().any(|entry| entry.command == *program)
    }

    /// The argv that resumes `app` with `token`, when the user listed one.
    #[must_use]
    pub fn resume_argv(&self, app: &str, token: &str) -> Option<Vec<String>> {
        let entry = self.program.iter().find(|entry| entry.app == app)?;
        Some(
            entry
                .command
                .iter()
                .map(|arg| {
                    if arg == RESUME_TOKEN_PLACEHOLDER {
                        token.to_owned()
                    } else {
                        arg.clone()
                    }
                })
                .collect(),
        )
    }
}

fn scrollback<'de, D: serde::Deserializer<'de>>(de: D) -> Result<u32, D::Error> {
    let lines = u32::deserialize(de)?;
    if lines > MAX_RESTORE_SCROLLBACK {
        return Err(serde::de::Error::custom(format!(
            "restore.scrollback must be at most {MAX_RESTORE_SCROLLBACK}"
        )));
    }
    Ok(lines)
}

/// The token is one whole argument after the program name, or the entry is
/// refused: embedded in a longer string it could be read as source or an
/// option, and as the first element the pane would choose what runs.
fn programs<'de, D: serde::Deserializer<'de>>(de: D) -> Result<Vec<RestoreProgram>, D::Error> {
    use serde::de::Error;

    let entries = Vec::<RestoreProgram>::deserialize(de)?;
    for (index, entry) in entries.iter().enumerate() {
        let app = &entry.app;
        let refuse = |why: &str| Err(D::Error::custom(format!("restore.program {app:?}: {why}")));
        // The OSC 7501 `app` grammar; anything else could never match.
        if !(1..=32).contains(&app.len())
            || !app
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.+-".contains(&b))
        {
            return refuse("`app` must be 1 to 32 bytes of [A-Za-z0-9_.+-]");
        }
        if entries
            .iter()
            .take(index)
            .any(|earlier| earlier.app == *app)
        {
            return refuse("`app` is listed twice");
        }
        let Some((program, args)) = entry.command.split_first() else {
            return refuse("`command` must not be empty");
        };
        if program.is_empty() || program.contains(RESUME_TOKEN_PLACEHOLDER) {
            return refuse("`command` must start with a program name");
        }
        if !args.iter().any(|arg| arg == RESUME_TOKEN_PLACEHOLDER)
            || args.iter().any(|arg| {
                arg != RESUME_TOKEN_PLACEHOLDER && arg.contains(RESUME_TOKEN_PLACEHOLDER)
            })
        {
            return refuse(&format!(
                "`command` needs {RESUME_TOKEN_PLACEHOLDER:?} as a whole argument, and nowhere else"
            ));
        }
    }
    Ok(entries)
}

/// A trust entry names one program, a bare name or an absolute path, once.
fn trust<'de, D: serde::Deserializer<'de>>(de: D) -> Result<Vec<RestoreTrust>, D::Error> {
    use serde::de::Error;
    let entries = Vec::<RestoreTrust>::deserialize(de)?;
    for (index, entry) in entries.iter().enumerate() {
        let command = &entry.command;
        let refuse = |why: &str| {
            Err(D::Error::custom(format!(
                "restore.trust {command:?}: {why}"
            )))
        };
        if command.is_empty()
            || command.chars().any(
                |ch| matches!(ch, '\u{0000}'..='\u{001F}' | '\u{007F}' | '\u{0080}'..='\u{009F}'),
            )
        {
            return refuse("`command` must be a bare name or an absolute path");
        }
        let bare = !command.starts_with('/');
        if bare && (command.starts_with('-') || command.contains('/')) {
            return refuse("`command` must be a bare name or an absolute path");
        }
        if !bare && command.len() < 2 {
            return refuse("`command` must be a bare name or an absolute path");
        }
        if entries
            .iter()
            .take(index)
            .any(|earlier| earlier.command == *command)
        {
            return refuse("`command` is listed twice");
        }
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::parse_with_defaults;

    fn load(program: &str) -> Result<super::RestoreCfg, String> {
        parse_with_defaults(program, Path::new("config.toml"))
            .map(|config| config.restore)
            .map_err(|err| err.to_string())
    }

    #[test]
    fn an_entry_substitutes_the_token_as_whole_arguments_only() {
        let restore = load(
            r#"
[[restore.program]]
app = "nvim"
command = ["nvim", "-S", "${PHUX_RESUME_TOKEN}", "$HOME", "${PHUX_RESUME_TOKEN}"]
"#,
        )
        .expect("valid");
        assert_eq!(
            restore.resume_argv("nvim", "a b; rm -rf /"),
            Some(
                ["nvim", "-S", "a b; rm -rf /", "$HOME", "a b; rm -rf /"]
                    .map(str::to_owned)
                    .to_vec()
            )
        );
        assert_eq!(restore.resume_argv("vim", "abc"), None);
        assert!(load("").expect("defaults").program.is_empty());
    }

    #[test]
    fn scrollback_is_off_by_default_and_capped() {
        let restore = load("[restore]\nscrollback = 10000\nscrollback-exclude = [\"psql\"]\n")
            .expect("valid");
        assert_eq!(restore.scrollback, 10_000);
        assert_eq!(restore.scrollback_exclude, ["psql"]);
        assert_eq!(load("").expect("defaults").scrollback, 0);
        let err = load("[restore]\nscrollback = 10001\n").expect_err("over the cap");
        assert!(err.contains("at most 10000"), "{err}");
    }

    #[test]
    fn entries_that_could_not_resume_safely_are_refused_at_load() {
        for (entry, why) in [
            (
                r#"app = "", command = ["x", "${PHUX_RESUME_TOKEN}"]"#,
                "`app` must be",
            ),
            (
                r#"app = "my app", command = ["x", "${PHUX_RESUME_TOKEN}"]"#,
                "`app` must be",
            ),
            (r#"app = "x", command = []"#, "must not be empty"),
            (
                r#"app = "x", command = ["x", "--fixed"]"#,
                "as a whole argument",
            ),
            (
                r#"app = "x", command = ["${PHUX_RESUME_TOKEN}"]"#,
                "program name",
            ),
            (
                r#"app = "x", command = ["", "${PHUX_RESUME_TOKEN}"]"#,
                "program name",
            ),
            (
                r#"app = "x", command = ["sh", "-c", "run ${PHUX_RESUME_TOKEN}"]"#,
                "as a whole argument",
            ),
            (
                r#"app = "x", command = ["x", "${PHUX_RESUME_TOKEN}", "--id=${PHUX_RESUME_TOKEN}"]"#,
                "as a whole argument",
            ),
            (
                r#"app = "x", command = ["x", "${PHUX_RESUME_TOKEN}"], shell = true"#,
                "unknown field",
            ),
        ] {
            let err = load(&format!("restore.program = [{{ {entry} }}]")).expect_err(entry);
            assert!(err.contains(why), "{entry}: {err}");
        }
        let twice = r#"
[[restore.program]]
app = "x"
command = ["x", "${PHUX_RESUME_TOKEN}"]
[[restore.program]]
app = "x"
command = ["y", "${PHUX_RESUME_TOKEN}"]
"#;
        assert!(load(twice).expect_err("duplicate").contains("listed twice"));
    }
}
