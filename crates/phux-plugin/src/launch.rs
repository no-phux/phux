//! Launch executor resolution (ADR-0042): resolve an agent integration
//! template shipped by an enabled plugin under `integrations/` into a
//! spawnable argv, which the CLI spawns through `SPAWN_RESOURCE`.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use phux_config::integration::{
    self, IntegrationAgentIdentity, IntegrationError, IntegrationLaunch,
    IntegrationSessionIdentity, IntegrationTemplate, LaunchWorkingDirectory, NativeBackend,
    SessionResumeError,
};
use phux_config::loader as config_loader;

/// Where a plugin ships its integration templates, relative to its root.
const INTEGRATIONS_DIR: &str = "integrations";

/// Select terminal execution or an explicitly declared native SDK transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchMode {
    /// Existing interactive terminal integration.
    Terminal,
    /// Native SDK/API host; absence is an error, never a terminal fallback.
    Native,
}

/// The actual SDK transport selected for a native launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeExecution {
    /// Declared SDK transport.
    pub backend: NativeBackend,
    /// Declared structured harness identity.
    pub provider: String,
}

/// A fully resolved launch: the argv to spawn, where, and from which plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedLaunch {
    /// Owning plugin id.
    pub plugin_id: String,
    /// Integration id that was resolved.
    pub integration_id: String,
    /// Integration display name, when declared.
    pub display_name: Option<String>,
    /// The template command, plugin root expanded, extra args appended.
    pub argv: Vec<String>,
    /// Working directory the program runs in.
    pub cwd: PathBuf,
    /// How `cwd` was chosen.
    pub working_directory: LaunchWorkingDirectory,
    /// Owning plugin's root directory.
    pub plugin_root: PathBuf,
    /// Provider-native session policy declared by the integration.
    pub session_identity: Option<IntegrationSessionIdentity>,
    /// The launched agent's self-declared identity, when declared.
    pub agent_identity: Option<IntegrationAgentIdentity>,
    /// Native SDK transport, only when explicitly selected.
    pub native: Option<NativeExecution>,
}

impl ResolvedLaunch {
    /// This launch argv as a provider-native resume invocation.
    ///
    /// # Errors
    ///
    /// When native resume is unsupported or the identity is invalid.
    pub fn resume_argv(&self, native_id: &str) -> Result<Vec<String>, SessionResumeError> {
        self.session_identity
            .as_ref()
            .ok_or(SessionResumeError::Unsupported)?
            .resume_argv(&self.argv, native_id)
    }

    /// This launch argv with a caller-supplied fresh-session identity.
    ///
    /// # Errors
    ///
    /// When fresh identities are unsupported or the identity is invalid.
    pub fn fresh_argv(&self, native_id: &str) -> Result<Vec<String>, SessionResumeError> {
        self.session_identity
            .as_ref()
            .ok_or(SessionResumeError::Unsupported)?
            .fresh_argv(&self.argv, native_id)
    }
}

/// One launchable integration surfaced by [`list_launchable`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchableIntegration {
    /// Owning plugin id.
    pub plugin_id: String,
    /// Integration id (the `phux launch <id>` name).
    pub integration_id: String,
    /// Display name, when declared.
    pub display_name: Option<String>,
    /// Package category (`terminal-agent`), when declared.
    pub kind: Option<String>,
    /// The launched agent's self-declared identity (its detection kind).
    pub agent_identity: Option<IntegrationAgentIdentity>,
    /// Declared native transport, independently of terminal prerequisites.
    pub native: Option<NativeExecution>,
    /// Missing prerequisites for the selected execution mode.
    pub missing_executables: Vec<String>,
}

/// Failure resolving a launch before a spawnable argv exists.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LaunchError {
    /// Config load failed.
    #[error("{0}")]
    Config(#[from] phux_config::ConfigError),
    /// A configured plugin manifest failed to load.
    #[error("could not load {path}: {source}")]
    Manifest {
        /// Manifest path.
        path: PathBuf,
        /// Manifest error.
        source: phux_config::plugin::PluginManifestError,
    },
    /// Multiple enabled manifests claimed one globally unique plugin id.
    #[error(
        "enabled plugin id {id:?} is ambiguous between {} and {}",
        first.display(),
        second.display()
    )]
    DuplicatePluginId {
        /// Duplicated manifest id.
        id: String,
        /// First owning plugin root.
        first: PathBuf,
        /// Conflicting plugin root.
        second: PathBuf,
    },
    /// Multiple enabled plugins/templates claimed one integration id.
    #[error("enabled integration id {id:?} is ambiguous between plugins {first:?} and {second:?}")]
    DuplicateIntegrationId {
        /// Duplicated integration id.
        id: String,
        /// First plugin that declared the id.
        first: String,
        /// Conflicting plugin that declared the id.
        second: String,
    },
    /// The requested integration's template failed to read or validate.
    #[error("could not load integration template {path}: {source}")]
    Template {
        /// Template path.
        path: PathBuf,
        /// Template error.
        source: IntegrationError,
    },
    /// A plugin's `integrations/` directory could not be read.
    #[error("could not read integration directory {path}: {source}")]
    Dir {
        /// Directory path.
        path: PathBuf,
        /// I/O error.
        source: std::io::Error,
    },
    /// No enabled plugin ships an integration with this id.
    #[error("no launchable integration named {name:?} in any enabled plugin")]
    NotFound {
        /// Requested integration id.
        name: String,
        /// Ids of the launchable integrations that are available.
        available: Vec<String>,
    },
    /// The integration exists but declares no `[launch]` command.
    #[error("integration {name:?} declares no `[launch]` command to launch")]
    NoLaunchCommand {
        /// Requested integration id.
        name: String,
    },
    /// No supported native SDK/API launch was declared.
    #[error("integration {name:?} is terminal-only; it declares no `[acp]` SDK launch")]
    NoNativeLaunch {
        /// Requested integration id.
        name: String,
    },
    /// The integration is valid but one or more external programs it needs
    /// are not executable on the launcher's `PATH`.
    #[error(
        "integration {name:?} is unavailable because these executables are not on PATH: {}",
        missing.join(", ")
    )]
    MissingExecutables {
        /// Requested integration id.
        name: String,
        /// Required executable names that were not found, in template order.
        missing: Vec<String>,
    },
}

struct EnabledPlugin {
    plugin_id: String,
    plugin_root: PathBuf,
}
include!("launch/resolve.rs");
include!("launch/templates.rs");

#[cfg(test)]
mod tests {
    use super::{KindClaim, LaunchableIntegration, integration_for_kind, kind_matches};

    #[test]
    #[cfg(unix)]
    fn builtin_opencode_prefers_path_then_the_official_user_install_without_mutating_environment() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let bin = tmp.path().join("bin");
        let official = home.join(".opencode/bin/opencode");
        std::fs::create_dir_all(official.parent().unwrap()).unwrap();
        std::fs::create_dir(&bin).unwrap();
        std::fs::write(&official, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&official, std::fs::Permissions::from_mode(0o700)).unwrap();
        let resolve = || super::opencode_executable(Some(bin.as_os_str()), Some(home.as_os_str()));
        assert_eq!(resolve(), Some(official.canonicalize().unwrap()));
        let on_path = bin.join("opencode");
        std::fs::copy(&official, &on_path).unwrap();
        assert_eq!(resolve(), Some(on_path.canonicalize().unwrap()));
        std::fs::remove_file(&on_path).unwrap();
        std::fs::set_permissions(&official, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(resolve(), None);
    }

    /// A launchable integration with category `terminal-agent` and an
    /// optional `[agent_identity] kind`.
    fn launchable(id: &str, agent_kind: Option<&str>) -> LaunchableIntegration {
        LaunchableIntegration {
            plugin_id: "example.agent-tools".to_owned(),
            integration_id: id.to_owned(),
            display_name: None,
            kind: Some("terminal-agent".to_owned()),
            agent_identity: agent_kind.map(|kind| {
                phux_config::integration::IntegrationAgentIdentity {
                    name: None,
                    kind: Some(kind.to_owned()),
                }
            }),
            native: None,
            missing_executables: Vec::new(),
        }
    }

    #[test]
    fn kind_claims_resolve_uniquely_ambiguously_or_not_at_all() {
        assert!(kind_matches(" CLAUDE ", "claude") && kind_matches("claude", "\tClaude\n"));
        assert!(!kind_matches("claude", "claude-code") && !kind_matches("", "claude"));

        let unique = [
            launchable("claude-code", Some("claude")),
            launchable("codex", Some("codex")),
            launchable("bare", None),
        ];
        let two = [
            launchable("claude-fork", Some("claude")),
            launchable("claude-code", Some("claude")),
        ];
        let duplicated = [
            launchable("claude-code", Some("claude")),
            launchable("claude-code", Some("claude")),
        ];
        let claude = || KindClaim::Unique("claude-code".to_owned());
        assert_eq!(integration_for_kind(" CLAUDE ", &unique), claude());
        // The category `kind` is never a claim.
        assert_eq!(
            integration_for_kind("terminal-agent", &unique),
            KindClaim::Unclaimed
        );
        assert_eq!(integration_for_kind("bare", &unique), KindClaim::Unclaimed);
        assert_eq!(integration_for_kind("claude", &[]), KindClaim::Unclaimed);
        assert_eq!(
            integration_for_kind("claude", &two),
            KindClaim::Ambiguous(vec!["claude-code".to_owned(), "claude-fork".to_owned()])
        );
        assert_eq!(integration_for_kind("claude", &duplicated), claude());
    }
}
