//! Agent integration templates (`integrations/<id>.toml`, ADR-0042): the
//! launchable agents a plugin ships.
//!
//! Only what the launcher needs is modeled (`id`, `display_name`, `kind`,
//! `[launch]`, `[session_identity]`, `[agent_identity]`); other keys are
//! ignored, not rejected, so the package format can grow.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Placeholder for the owning plugin's root in a `[launch] command`,
/// substituted per argv element (never shell-evaluated).
pub const PLUGIN_ROOT_PLACEHOLDER: &str = "${PHUX_PLUGIN_ROOT}";

/// Placeholder replaced with a provider-native session identity when an
/// integration is resumed. Replacement is per argv element; it is never
/// evaluated by a shell.
pub const SESSION_ID_PLACEHOLDER: &str = "${PHUX_AGENT_SESSION_ID}";

/// Maximum UTF-8 byte length of a provider-native session identity.
pub const MAX_SESSION_ID_BYTES: usize = 1_024;

const MAX_RESUME_ARGS: usize = 16;
const MAX_RESUME_ARG_BYTES: usize = 4_096;
const MAX_RESUME_ARGV_BYTES: usize = 16 * 1_024;
const MAX_REQUIRED_EXECUTABLES: usize = 16;
const MAX_EXECUTABLE_NAME_BYTES: usize = 255;

/// A parsed agent integration template (`integrations/<id>.toml`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrationTemplate {
    /// Stable integration id (the `phux launch <id>` name).
    pub id: String,
    /// Human-readable display name, when declared.
    pub display_name: Option<String>,
    /// Open-vocabulary kind slug (e.g. `terminal-agent`), when declared.
    pub kind: Option<String>,
    /// The `[launch]` command; without one the template is not launchable.
    pub launch: Option<IntegrationLaunch>,
    /// Canonical path the template was loaded from.
    pub template_path: PathBuf,
    /// Provider-native session identity and restore policy, when declared.
    pub session_identity: Option<IntegrationSessionIdentity>,
    /// The launched agent's self-declared identity, when declared.
    pub agent_identity: Option<IntegrationAgentIdentity>,
}

/// A template's `[agent_identity]`: the identity the launched program
/// declares (ADR-0040).
///
/// Its `kind` is the detection slug (`claude`), not the
/// template's package category, so `phux agent start --kind` can find the
/// integration that launches it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrationAgentIdentity {
    /// Default agent name the launch wrapper declares, when present.
    pub name: Option<String>,
    /// Detection-manifest kind slug the agent identifies as, when present.
    pub kind: Option<String>,
}

/// The `[launch]` section: the argv to spawn and where to run it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrationLaunch {
    /// Command argv, non-empty; may contain [`PLUGIN_ROOT_PLACEHOLDER`].
    pub command: Vec<String>,
    /// Executables that must be on `PATH` for the launch to be offered.
    pub required_executables: Vec<String>,
    /// Directory the launched program runs in.
    pub working_directory: LaunchWorkingDirectory,
}

/// A template's `[session_identity]` policy; natively restorable only with
/// structured `resume_args`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrationSessionIdentity {
    /// Provider-native identity, or only the phux session.
    pub mode: SessionIdentityMode,
    /// Name of the environment variable carrying the opaque native identity.
    pub native_env: String,
    /// Mechanism used to reconstruct the process after restart.
    pub restore: SessionRestoreMode,
    /// Structured arguments appended to the launch command on native resume.
    pub resume_args: Option<Vec<String>>,
    /// Structured arguments that set a caller-supplied identity on a fresh
    /// session, for providers that accept one.
    pub fresh_args: Option<Vec<String>>,
}

/// Source policy for an integration's durable session identity.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SessionIdentityMode {
    /// Prefer a provider-native identity when supplied.
    NativeOrPhux,
    /// The integration has no provider-native resume mechanism.
    Phux,
}

/// Restore mechanism declared by an integration template.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SessionRestoreMode {
    /// Invoke the external agent CLI with structured resume arguments.
    ExternalCli,
    /// Recreate only the surrounding phux session.
    PhuxSession,
}

/// Failure to apply a provider-native identity to a launch argv.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum SessionResumeError {
    /// The template does not opt into provider-native restore.
    #[error("integration does not declare provider-native resume arguments")]
    Unsupported,
    /// The supplied provider identity is unsafe or exceeds its bound.
    #[error("invalid provider-native session identity: {0}")]
    InvalidIdentity(String),
    /// Placeholder expansion would exceed the bounded argv budget.
    #[error("expanded resume argv exceeds {MAX_RESUME_ARGV_BYTES} bytes")]
    ArgvTooLarge,
}

impl IntegrationSessionIdentity {
    /// Whether this policy can reconstruct a provider-native session.
    #[must_use]
    pub const fn supports_native_restore(&self) -> bool {
        matches!(self.mode, SessionIdentityMode::NativeOrPhux)
            && matches!(self.restore, SessionRestoreMode::ExternalCli)
            && self.resume_args.is_some()
    }

    /// Whether the provider also accepts a caller-supplied fresh identity.
    #[must_use]
    pub const fn supports_native_fresh(&self) -> bool {
        self.supports_native_restore() && self.fresh_args.is_some()
    }

    /// Append this policy's resume arguments to `launch_argv`, substituting
    /// the session placeholder without shell evaluation.
    ///
    /// # Errors
    ///
    /// When the policy is not natively restorable, the identity fails
    /// [`validate_native_session_id`], or the argv exceeds its byte budget.
    pub fn resume_argv(
        &self,
        launch_argv: &[String],
        native_id: &str,
    ) -> Result<Vec<String>, SessionResumeError> {
        if !self.supports_native_restore() {
            return Err(SessionResumeError::Unsupported);
        }
        append_identity_args(
            launch_argv,
            self.resume_args
                .as_deref()
                .ok_or(SessionResumeError::Unsupported)?,
            native_id,
        )
    }

    /// Append the provider's fresh-session identity arguments.
    ///
    /// # Errors
    ///
    /// As [`Self::resume_argv`], for `fresh_args`.
    pub fn fresh_argv(
        &self,
        launch_argv: &[String],
        native_id: &str,
    ) -> Result<Vec<String>, SessionResumeError> {
        if !self.supports_native_fresh() {
            return Err(SessionResumeError::Unsupported);
        }
        append_identity_args(
            launch_argv,
            self.fresh_args
                .as_deref()
                .ok_or(SessionResumeError::Unsupported)?,
            native_id,
        )
    }
}

fn append_identity_args(
    launch_argv: &[String],
    identity_args: &[String],
    native_id: &str,
) -> Result<Vec<String>, SessionResumeError> {
    validate_native_session_id(native_id)?;
    let mut argv = Vec::with_capacity(launch_argv.len() + identity_args.len());
    argv.extend_from_slice(launch_argv);
    argv.extend(identity_args.iter().map(|arg| {
        if arg == SESSION_ID_PLACEHOLDER {
            native_id.to_owned()
        } else {
            arg.clone()
        }
    }));
    let bytes = argv.iter().try_fold(0usize, |total, arg| {
        total.checked_add(arg.len()).and_then(|n| n.checked_add(1))
    });
    if bytes.is_none_or(|bytes| bytes > MAX_RESUME_ARGV_BYTES) {
        return Err(SessionResumeError::ArgvTooLarge);
    }
    Ok(argv)
}

/// Where a launched integration's program runs.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum LaunchWorkingDirectory {
    /// The directory `phux launch` was invoked from (default).
    #[default]
    Workspace,
    /// The owning plugin's root directory.
    #[serde(rename = "plugin-root")]
    PluginRoot,
}

/// Error raised while reading or validating an integration template.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum IntegrationError {
    /// I/O failure while reading the template.
    #[error("integration template io: {0}")]
    Io(#[from] std::io::Error),
    /// TOML parse failure.
    #[error("{}: {message}", path.display())]
    Parse {
        /// Template path.
        path: PathBuf,
        /// Parse message.
        message: String,
    },
    /// Schema validation failure after TOML parsing.
    #[error("{0}")]
    Invalid(String),
}

#[derive(Debug, Deserialize)]
struct RawTemplate {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    launch: Option<RawLaunch>,
    #[serde(default)]
    session_identity: Option<RawSessionIdentity>,
    #[serde(default)]
    agent_identity: Option<RawAgentIdentity>,
}

#[derive(Debug, Deserialize)]
struct RawAgentIdentity {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    kind: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawLaunch {
    #[serde(default)]
    command: Vec<String>,
    #[serde(default)]
    required_executables: Vec<String>,
    #[serde(default)]
    working_directory: LaunchWorkingDirectory,
}

#[derive(Debug, Deserialize)]
struct RawSessionIdentity {
    mode: SessionIdentityMode,
    native_env: String,
    restore: SessionRestoreMode,
    #[serde(default)]
    resume_args: Option<Vec<String>>,
    #[serde(default)]
    fresh_args: Option<Vec<String>>,
}

/// Load and validate an integration template from `path`.
///
/// # Errors
///
/// When the file cannot be read or parsed, or violates the template schema.
pub fn load_integration_template(path: &Path) -> Result<IntegrationTemplate, IntegrationError> {
    let text = std::fs::read_to_string(path)?;
    parse_integration_template(&text, path)
}

/// Parse and validate a template from `text`, attributing errors to `path`.
///
/// # Errors
///
/// See [`load_integration_template`].
pub fn parse_integration_template(
    text: &str,
    path: &Path,
) -> Result<IntegrationTemplate, IntegrationError> {
    let raw: RawTemplate = toml::from_str(text).map_err(|err| IntegrationError::Parse {
        path: path.to_path_buf(),
        message: err.message().to_owned(),
    })?;

    let id = raw
        .id
        .map(|id| id.trim().to_owned())
        .filter(|id| !id.is_empty())
        .ok_or_else(|| {
            IntegrationError::Invalid(format!(
                "{}: integration template is missing a non-empty `id`",
                path.display()
            ))
        })?;

    let launch = raw
        .launch
        .map(|raw_launch| build_launch(&id, path, raw_launch))
        .transpose()?;
    let session_identity = raw
        .session_identity
        .map(|raw_session| build_session_identity(&id, path, raw_session))
        .transpose()?;
    if let (Some(launch), Some(session)) = (&launch, &session_identity) {
        validate_identity_execution(&id, path, launch, session)?;
    }

    Ok(IntegrationTemplate {
        id,
        display_name: raw.display_name.as_deref().and_then(trim_optional),
        kind: raw.kind.as_deref().and_then(trim_optional),
        launch,
        session_identity,
        template_path: path.to_path_buf(),
        agent_identity: raw
            .agent_identity
            .map(|raw_identity| IntegrationAgentIdentity {
                name: raw_identity.name.as_deref().and_then(trim_optional),
                kind: raw_identity.kind.as_deref().and_then(trim_optional),
            }),
    })
}

fn invalid(path: &Path, id: &str, message: &str) -> IntegrationError {
    IntegrationError::Invalid(format!("{}: integration {id:?} {message}", path.display()))
}

fn build_launch(
    id: &str,
    path: &Path,
    raw: RawLaunch,
) -> Result<IntegrationLaunch, IntegrationError> {
    if raw.command.is_empty() {
        return Err(invalid(
            path,
            id,
            "`[launch] command` must be a non-empty argv",
        ));
    }
    if raw.command[0].trim().is_empty() {
        return Err(invalid(
            path,
            id,
            "`[launch] command[0]` (the program) must not be blank",
        ));
    }
    validate_required_executables(id, path, &raw.required_executables)?;
    Ok(IntegrationLaunch {
        command: raw.command,
        required_executables: raw.required_executables,
        working_directory: raw.working_directory,
    })
}

fn validate_required_executables(
    id: &str,
    path: &Path,
    executables: &[String],
) -> Result<(), IntegrationError> {
    let invalid_count = executables.len() > MAX_REQUIRED_EXECUTABLES;
    let invalid_name = executables.iter().any(|name| {
        name.is_empty()
            || name.trim() != name
            || name.len() > MAX_EXECUTABLE_NAME_BYTES
            || name.contains(['/', '\\'])
    });
    if invalid_count || invalid_name {
        return Err(invalid(
            path,
            id,
            &format!(
                "`[launch] required_executables` must contain at most \
             {MAX_REQUIRED_EXECUTABLES} non-empty PATH executable names of at most \
             {MAX_EXECUTABLE_NAME_BYTES} bytes"
            ),
        ));
    }
    Ok(())
}

fn build_session_identity(
    id: &str,
    path: &Path,
    raw: RawSessionIdentity,
) -> Result<IntegrationSessionIdentity, IntegrationError> {
    let native_env = raw.native_env.trim();
    if native_env.len() > 128
        || !valid_env_name(native_env)
        || !native_env.starts_with("PHUX_")
        || !native_env.ends_with("_SESSION_ID")
    {
        return Err(invalid(
            path,
            id,
            "`[session_identity] native_env` must be a \
             dedicated PHUX_*_SESSION_ID name",
        ));
    }
    if raw.resume_args.is_some()
        && (!matches!(raw.mode, SessionIdentityMode::NativeOrPhux)
            || !matches!(raw.restore, SessionRestoreMode::ExternalCli))
    {
        return Err(invalid(
            path,
            id,
            "`resume_args` requires mode \
             `native-or-phux` and restore `external-cli`",
        ));
    }
    if raw.fresh_args.is_some() && raw.resume_args.is_none() {
        return Err(invalid(path, id, "`fresh_args` requires `resume_args`"));
    }
    if let Some(args) = &raw.resume_args {
        validate_identity_args(id, path, "resume_args", args)?;
    }
    if let Some(args) = &raw.fresh_args {
        validate_identity_args(id, path, "fresh_args", args)?;
    }
    Ok(IntegrationSessionIdentity {
        mode: raw.mode,
        native_env: native_env.to_owned(),
        restore: raw.restore,
        resume_args: raw.resume_args,
        fresh_args: raw.fresh_args,
    })
}

fn validate_identity_args(
    id: &str,
    path: &Path,
    field: &str,
    args: &[String],
) -> Result<(), IntegrationError> {
    if args.is_empty() || args.len() > MAX_RESUME_ARGS {
        return Err(invalid(
            path,
            id,
            &format!("`{field}` must contain 1..={MAX_RESUME_ARGS} elements"),
        ));
    }
    if args
        .iter()
        .any(|arg| arg.is_empty() || arg.len() > MAX_RESUME_ARG_BYTES)
    {
        return Err(invalid(
            path,
            id,
            &format!(
                "`{field}` elements must contain \
             1..={MAX_RESUME_ARG_BYTES} bytes"
            ),
        ));
    }
    if args
        .iter()
        .filter(|arg| arg.as_str() == SESSION_ID_PLACEHOLDER)
        .count()
        != 1
        || args
            .iter()
            .any(|arg| arg != SESSION_ID_PLACEHOLDER && arg.contains(SESSION_ID_PLACEHOLDER))
    {
        return Err(invalid(
            path,
            id,
            &format!(
                "`{field}` must contain exactly one \
             standalone {SESSION_ID_PLACEHOLDER:?} argument"
            ),
        ));
    }
    Ok(())
}

fn validate_identity_execution(
    id: &str,
    path: &Path,
    launch: &IntegrationLaunch,
    session: &IntegrationSessionIdentity,
) -> Result<(), IntegrationError> {
    if !session.supports_native_restore() {
        return Ok(());
    }
    let program = launch.command.first().map_or("", String::as_str);
    let basename = program
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(program)
        .to_ascii_lowercase();
    let shell = matches!(
        basename.as_str(),
        "sh" | "bash"
            | "dash"
            | "zsh"
            | "ksh"
            | "fish"
            | "pwsh"
            | "powershell"
            | "powershell.exe"
            | "cmd"
            | "cmd.exe"
    );
    let fixed_shell_source = shell
        && launch.command.get(1).is_some_and(|source| {
            source
                .strip_prefix(PLUGIN_ROOT_PLACEHOLDER)
                .is_some_and(|relative| relative.starts_with(['/', '\\']) && relative.len() > 1)
        });
    let unbounded_interpreter = matches!(
        basename.as_str(),
        "python"
            | "python3"
            | "python.exe"
            | "node"
            | "node.exe"
            | "deno"
            | "bun"
            | "ruby"
            | "perl"
            | "lua"
            | "php"
            | "osascript"
            | "env"
            | "busybox"
    );
    if (shell && !fixed_shell_source) || unbounded_interpreter {
        return Err(invalid(
            path,
            id,
            &format!(
                "native session identity must not be \
             exposed to interpreter or evaluator source in {program:?}"
            ),
        ));
    }
    Ok(())
}

fn valid_env_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    matches!(bytes.next(), Some(b'A'..=b'Z' | b'a'..=b'z' | b'_'))
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

/// Validate one opaque provider-native session identity.
///
/// # Errors
///
/// Returns an error when the value is empty, padded, option-shaped, contains
/// control characters, or exceeds [`MAX_SESSION_ID_BYTES`].
pub fn validate_native_session_id(native_id: &str) -> Result<(), SessionResumeError> {
    if native_id.is_empty() {
        return Err(SessionResumeError::InvalidIdentity(
            "value must not be empty".to_owned(),
        ));
    }
    if native_id.len() > MAX_SESSION_ID_BYTES {
        return Err(SessionResumeError::InvalidIdentity(format!(
            "value exceeds {MAX_SESSION_ID_BYTES} UTF-8 bytes"
        )));
    }
    if native_id.trim() != native_id {
        return Err(SessionResumeError::InvalidIdentity(
            "value must not have leading or trailing whitespace".to_owned(),
        ));
    }
    if native_id.starts_with('-') {
        return Err(SessionResumeError::InvalidIdentity(
            "value must not begin with '-'".to_owned(),
        ));
    }
    if native_id.chars().any(char::is_control) {
        return Err(SessionResumeError::InvalidIdentity(
            "value must not contain control characters".to_owned(),
        ));
    }
    Ok(())
}

fn trim_optional(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

/// Expand a launch `command` into a spawnable argv: every
/// [`PLUGIN_ROOT_PLACEHOLDER`] occurrence is replaced with `plugin_root`,
/// then `extra_args` are appended verbatim.
///
/// This is a pure, per-element string substitution — no shell, no globbing,
/// no word-splitting — so a template value that embeds the placeholder
/// stays a single argv element and untrusted content cannot inject extra
/// arguments or commands.
#[must_use]
pub fn expand_launch_argv(
    command: &[String],
    plugin_root: &Path,
    extra_args: &[String],
) -> Vec<String> {
    let root = plugin_root.display().to_string();
    let mut argv: Vec<String> = command
        .iter()
        .map(|part| part.replace(PLUGIN_ROOT_PLACEHOLDER, &root))
        .collect();
    argv.extend(extra_args.iter().cloned());
    argv
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLAUDE: &str = r#"
schema_version = 1
id = "claude-code"
display_name = "Claude Code"
kind = "terminal-agent"
first_party = true
capabilities = ["terminal-control"]

[detect]
mode = "opt-in"
command = "claude"

[session_identity]
mode = "native-or-phux"
native_env = "PHUX_CLAUDE_SESSION_ID"
restore = "external-cli"
resume_args = ["--resume", "${PHUX_AGENT_SESSION_ID}"]
fresh_args = ["--session-id", "${PHUX_AGENT_SESSION_ID}"]

[agent_identity]
name = "claude"
kind = "claude"

[launch]
command = ["sh", "${PHUX_PLUGIN_ROOT}/scripts/phux-agent-wrap.sh", "--name", "claude", "--kind", "claude", "--", "claude"]
required_executables = ["claude"]
working_directory = "workspace"
"#;

    fn parse(text: &str) -> Result<IntegrationTemplate, IntegrationError> {
        parse_integration_template(text, Path::new("claude-code.toml"))
    }

    #[test]
    fn parses_launch_and_ignores_unmodeled_keys() {
        let template = parse(CLAUDE).expect("valid template parses");
        assert_eq!(template.id, "claude-code");
        assert_eq!(template.display_name.as_deref(), Some("Claude Code"));
        assert_eq!(template.kind.as_deref(), Some("terminal-agent"));
        let session = template.session_identity.expect("session identity");
        assert!(session.supports_native_restore());
        assert!(session.supports_native_fresh());
        assert_eq!(session.native_env, "PHUX_CLAUDE_SESSION_ID");
        let launch = template.launch.expect("launch present");
        assert_eq!(launch.command[0], "sh");
        assert_eq!(launch.command.last().unwrap(), "claude");
        assert_eq!(launch.required_executables, ["claude"]);
        assert_eq!(launch.working_directory, LaunchWorkingDirectory::Workspace);
        let identity = template.agent_identity.expect("agent identity present");
        assert_eq!(identity.name.as_deref(), Some("claude"));
        assert_eq!(identity.kind.as_deref(), Some("claude"));
    }

    /// Optional sections default sensibly: no identity, a workspace
    /// working directory, blank strings as `None`, no `[launch]` as
    /// not-launchable.
    #[test]
    fn optional_sections_normalize() {
        let bare = parse("id = \"bare\"\n[launch]\ncommand = [\"sh\", \"-c\", \"true\"]\n")
            .expect("valid");
        assert_eq!(bare.agent_identity, None);
        assert_eq!(
            bare.launch.expect("launch").working_directory,
            LaunchWorkingDirectory::Workspace
        );

        let rooted = parse(
            "id = \"x\"\n[agent_identity]\nname = \"  \"\nkind = \"\"\n\
             [launch]\ncommand = [\"sh\"]\nworking_directory = \"plugin-root\"\n",
        )
        .expect("valid");
        let identity = rooted.agent_identity.expect("section present");
        assert_eq!((identity.name, identity.kind), (None, None));
        assert_eq!(
            rooted.launch.expect("launch").working_directory,
            LaunchWorkingDirectory::PluginRoot
        );

        let detect_only = parse("id = \"detect-only\"\n").expect("valid");
        assert!(detect_only.launch.is_none());
    }

    /// Schema violations are errors, including a malformed
    /// `[agent_identity]` (a typo must not silently drop the `--kind`
    /// default it provides).
    #[test]
    fn malformed_templates_are_rejected() {
        for text in [
            "display_name = \"No Id\"\n[launch]\ncommand = [\"sh\"]\n",
            "id = \"empty\"\n[launch]\ncommand = []\n",
            "id = \"blank\"\n[launch]\ncommand = [\"  \", \"arg\"]\n",
        ] {
            assert!(
                matches!(parse(text), Err(IntegrationError::Invalid(_))),
                "{text}"
            );
        }
        for body in [
            "[agent_identity]\nkind = 3",
            "[agent_identity]\nkind = [\"claude\"]",
            "agent_identity = \"claude\"",
        ] {
            let text = format!("id = \"bad\"\n{body}\n[launch]\ncommand = [\"sh\"]\n");
            assert!(
                matches!(parse(&text), Err(IntegrationError::Parse { .. })),
                "{text}"
            );
        }
    }

    #[test]
    fn launch_executable_requirements_are_optional_and_bare() {
        let without = parse(
            r#"
id = "without"
[launch]
command = ["agent"]
"#,
        )
        .expect("requirements are optional");
        assert!(without.launch.unwrap().required_executables.is_empty());

        for requirement in ["", " grok", "grok ", "bin/grok", "bin\\grok"] {
            let text = format!(
                r#"
id = "bad-requirement"
[launch]
command = ["agent"]
required_executables = [{requirement:?}]
"#
            );
            assert!(
                matches!(parse(&text), Err(IntegrationError::Invalid(_))),
                "accepted invalid executable requirement {requirement:?}"
            );
        }

        for requirements in [
            format!("{:?}", vec!["grok"; MAX_REQUIRED_EXECUTABLES + 1]),
            format!("[{:?}]", "x".repeat(MAX_EXECUTABLE_NAME_BYTES + 1)),
        ] {
            let text = format!(
                r#"
id = "oversized-requirement"
[launch]
command = ["agent"]
required_executables = {requirements}
"#
            );
            assert!(matches!(parse(&text), Err(IntegrationError::Invalid(_))));
        }
    }

    /// The identity is one argv element, never shell-split, with distinct
    /// resume and fresh arguments.
    #[test]
    fn native_identity_is_one_structured_argument() {
        let session = parse(CLAUDE)
            .expect("valid")
            .session_identity
            .expect("policy");
        let claude = ["claude".to_owned()];
        let hostile = "team session; printf definitely-not-shell";
        assert_eq!(
            session.resume_argv(&claude, hostile).expect("bounded"),
            ["claude", "--resume", hostile]
        );
        assert_eq!(
            session.fresh_argv(&claude, "0123-abcd").expect("fresh"),
            ["claude", "--session-id", "0123-abcd"]
        );
    }

    #[test]
    fn resume_policy_rejects_bad_env_and_placeholder_counts() {
        for (native_env, resume_args) in [
            ("BAD-NAME", r#"["--resume", "${PHUX_AGENT_SESSION_ID}"]"#),
            ("BASH_ENV", r#"["--resume", "${PHUX_AGENT_SESSION_ID}"]"#),
            (
                "PHUX_AGENT_ID",
                r#"["--resume", "${PHUX_AGENT_SESSION_ID}"]"#,
            ),
            ("PHUX_GOOD_SESSION_ID", r#"["--resume"]"#),
            (
                "PHUX_GOOD_SESSION_ID",
                r#"["${PHUX_AGENT_SESSION_ID}", "${PHUX_AGENT_SESSION_ID}"]"#,
            ),
            (
                "PHUX_GOOD_SESSION_ID",
                r#"["--resume=${PHUX_AGENT_SESSION_ID}"]"#,
            ),
        ] {
            let text = format!(
                r#"
id = "bad"

[session_identity]
mode = "native-or-phux"
native_env = "{native_env}"
restore = "external-cli"
resume_args = {resume_args}
[launch]
command = ["agent"]
"#
            );
            assert!(
                matches!(parse(&text), Err(IntegrationError::Invalid(_))),
                "{text}"
            );
        }

        let wrapper_command = r#"command = ["sh", "${PHUX_PLUGIN_ROOT}/scripts/phux-agent-wrap.sh", "--name", "claude", "--kind", "claude", "--", "claude"]"#;
        for command in [r#"["sh", "-xc"]"#, r#"["/usr/bin/env", "sh", "-c"]"#] {
            let evaluator = CLAUDE
                .replace(wrapper_command, &format!("command = {command}"))
                .replace(
                    r#"resume_args = ["--resume", "${PHUX_AGENT_SESSION_ID}"]"#,
                    r#"resume_args = ["${PHUX_AGENT_SESSION_ID}"]"#,
                );
            assert!(matches!(
                parse(&evaluator),
                Err(IntegrationError::Invalid(_))
            ));
        }

        let fixed_wrapper = CLAUDE.replace(
            wrapper_command,
            r#"command = ["/bin/bash", "${PHUX_PLUGIN_ROOT}/trusted/wrapper.sh"]"#,
        );
        parse(&fixed_wrapper).expect("a plugin-owned fixed interpreter source is bounded");
    }

    #[test]
    fn legacy_session_policy_without_resume_args_stays_launchable() {
        let template = parse(
            r#"
id = "legacy"
[session_identity]
mode = "native-or-phux"
native_env = "PHUX_LEGACY_SESSION_ID"
restore = "external-cli"
[launch]
command = ["legacy-agent"]
"#,
        )
        .expect("pre-resume template remains parseable");
        assert!(
            !template
                .session_identity
                .expect("session section")
                .supports_native_restore()
        );
    }

    #[test]
    fn native_identity_bounds_and_controls_are_rejected() {
        let template = parse(CLAUDE).expect("valid");
        let session = template.session_identity.expect("session policy");
        for invalid in [
            String::new(),
            " padded".to_owned(),
            "padded ".to_owned(),
            "line\nbreak".to_owned(),
            "--dangerously-bypass-approvals-and-sandbox".to_owned(),
            "x".repeat(MAX_SESSION_ID_BYTES + 1),
        ] {
            assert!(
                matches!(
                    session.resume_argv(&["claude".to_owned()], &invalid),
                    Err(SessionResumeError::InvalidIdentity(_))
                ),
                "{invalid:?}"
            );
        }
    }

    /// Plugin-root expansion is per element, so shell metacharacters stay
    /// inside one argument; extra args append verbatim.
    #[test]
    fn expansion_substitutes_per_element_and_appends_extra_args() {
        let command = vec![
            "sh".to_owned(),
            "${PHUX_PLUGIN_ROOT}/a b; rm -rf ~".to_owned(),
        ];
        let argv = expand_launch_argv(&command, Path::new("/root"), &["--resume".to_owned()]);
        assert_eq!(argv, ["sh", "/root/a b; rm -rf ~", "--resume"]);
    }
}
