//! Language-neutral public terminal workflow contracts, separate from wire carriers.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A complete intention: create, place and start a new terminal process.
/// A nonempty argv executes directly, never in an existing shell.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TerminalCreate {
    /// Nonzero 32-hex retry identity. Reuse only for the identical intention.
    #[schemars(length(min = 32, max = 32), regex(pattern = "^[0-9a-fA-F]{32}$"))]
    pub idempotency_key: String,
    /// Full durable destination session key (26 base32 digits), not a name.
    #[schemars(length(min = 26, max = 26))]
    pub session: String,
    /// Program and arguments; empty selects the server's default shell.
    #[serde(default)]
    pub argv: Vec<String>,
    /// Working directory on the executing server.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Environment overrides. Reserved server routing variables remain server-owned.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Optional resource label.
    #[serde(default)]
    pub name: Option<String>,
    /// Placement is committed before any process starts. Defaults to a new group.
    #[serde(default)]
    pub placement: TerminalPlacement,
}

/// A read-only lookup of a retained creation receipt. A missing receipt is not a refusal of the original create.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TerminalOperation {
    /// Same nonzero 32-hex identity supplied to terminal.create.
    pub idempotency_key: String,
    /// Original full durable destination session key.
    pub session: String,
}
impl TerminalOperation {
    /// Validate identities and build a read-only command.
    ///
    /// # Errors
    /// Invalid retry or session identity.
    pub fn command(&self) -> Result<crate::wire::frame::Command, String> {
        let operation = parse_hex16(&self.idempotency_key)
            .and_then(crate::ids::OperationId::new)
            .ok_or("invalid idempotency_key")?;
        let session = crate::ids::SessionKey::parse(&self.session).ok_or("invalid session key")?;
        Ok(crate::wire::frame::Command::GetCreation { operation, session })
    }
}

/// Complete input shape for read-only creation receipt lookup.
#[must_use]
pub fn terminal_operation_schema() -> serde_json::Value {
    schemars::schema_for!(TerminalOperation).to_value()
}

/// Where the new resource is presented. Never changes local focus.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum TerminalPlacement {
    /// Append a new unnamed group to the session arrangement.
    #[default]
    NewGroup,
    /// Split one exact entry while it still displays the observed execution.
    Beside {
        /// Stable entry id, 32 hex digits.
        entry: String,
        /// Full durable anchor resource key.
        resource: String,
        /// Anchor's incarnation token, 32 hex digits.
        instance: String,
        /// Anchor's observed activation, starting at 1.
        activation: u64,
        /// Horizontal = side by side; vertical = stacked.
        axis: SplitAxis,
        /// Fraction kept by the anchor, finite and strictly between zero and one.
        ratio: f32,
    },
}

/// Axis along which the terminal is split.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SplitAxis {
    /// Side by side.
    Horizontal,
    /// Stacked.
    Vertical,
}

/// The receipt of committed intent plus a current execution observation.
/// It does not mean the process has exited or its task has succeeded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TerminalCreated {
    /// Public document revision, currently 1.
    pub schema_version: u32,
    /// Retry identity of the committed intention.
    pub idempotency_key: String,
    /// Durable owning authority.
    pub authority: String,
    /// Durable destination session.
    pub session: String,
    /// Durable resource identity, independent of the current execution.
    pub resource: String,
    /// Stable presentation identity created by this operation.
    pub placement: CreatedPlacement,
    /// Current execution, if still recorded. Retained records make failures inspectable.
    pub execution: Option<ExecutionObservation>,
    /// Current local live handle, not a durable selector.
    pub terminal_id: Option<u32>,
    /// True when the retained receipt was replayed without launch or placement effects.
    pub replayed: bool,
}

/// Stable placement identity. It may since have been edited or removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CreatedPlacement {
    /// Durable arrangement key.
    pub arrangement: String,
    /// Stable entry id as 32 hex digits.
    pub entry: String,
}

/// A level observation, not a completion promise or a lossless event history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ExecutionObservation {
    /// Monotonic execution identity within the resource.
    pub activation: u64,
    /// Incarnation token, 32 hex digits.
    pub instance: String,
    /// Current state.
    pub state: ExecutionState,
    /// Observed end, when known. Failed launch never implies exit status zero.
    pub outcome: Option<ExecutionOutcome>,
}

/// Execution lifecycle; loss is not completion.
///
/// Unknown values deserialize as [`Self::Unknown`]. Adding a variant does
/// not bump `schema_version`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionState {
    /// Intent committed, process not yet reported running.
    Starting,
    /// Process running.
    Running,
    /// Termination requested.
    Stopping,
    /// Process ended.
    Completed,
    /// Launch failed.
    Failed,
    /// Owning engine lost; process may still run.
    Lost,
    /// Historical released uncertain execution.
    Abandoned,
    /// A state this build does not name. Preserved so a newer server stays readable.
    #[serde(other)]
    Unknown,
}

/// An execution's end, distinct from commitment of a creation intention.
///
/// Unknown `type` tags deserialize as [`Self::Unknown`]. Adding a variant
/// does not bump `schema_version`. Wire [`crate::wire::frame::ErrorCode`]
/// already decodes an unrecognized code as `Unknown`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExecutionOutcome {
    /// Process exit status.
    Exited {
        /// Exit status.
        code: i32,
    },
    /// Process ended by a signal.
    Signaled {
        /// Signal number.
        signal: i32,
    },
    /// End not observed, including a launch that never produced an exit status.
    Unobserved,
    /// An outcome this build does not name.
    #[serde(other)]
    Unknown,
}

impl TerminalCreate {
    /// A new-group intention with the default shell and no environment overrides.
    /// Identities are validated before any I/O by the client operation.
    #[must_use]
    pub fn new(session: impl Into<String>, idempotency_key: impl Into<String>) -> Self {
        Self {
            session: session.into(),
            idempotency_key: idempotency_key.into(),
            argv: Vec::new(),
            cwd: None,
            env: BTreeMap::new(),
            name: None,
            placement: TerminalPlacement::NewGroup,
        }
    }

    /// Validate a public intention and build its typed wire command. No I/O.
    ///
    /// # Errors
    /// Invalid identities, placement, launch arguments or unsupported environment overrides.
    pub fn command(&self) -> Result<crate::wire::frame::Command, String> {
        use crate::ids::{IdempotencyKey, ResourceKind, SessionKey};
        use crate::wire::frame::{Command, ExitPolicy, LaunchMode, LaunchSpec};
        let operation = parse_hex16(&self.idempotency_key)
            .and_then(IdempotencyKey::new)
            .ok_or("idempotency_key must be nonzero 32-hex")?;
        let session =
            SessionKey::parse(&self.session).ok_or("session must be a full durable key")?;
        let launch = LaunchSpec::new(self.argv.clone())
            .with_cwd(self.cwd.clone())
            .with_env(
                self.env
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect(),
            );
        validate_terminal_launch(&launch)?;
        if self
            .name
            .as_ref()
            .is_some_and(|name| name.trim().is_empty() || name.len() > 256 || name.contains('\0'))
        {
            return Err("name must be nonempty, NUL-free and at most 256 bytes".into());
        }
        Ok(Command::ResourceCreate {
            operation,
            session,
            kind: ResourceKind::Terminal,
            launch: Some(Box::new(launch)),
            name: self.name.clone(),
            on_exit: ExitPolicy::Retain,
            launch_mode: LaunchMode::Immediate,
            placement: Some(Box::new(self.placement.wire()?)),
        })
    }
}

impl TerminalPlacement {
    fn wire(&self) -> Result<crate::wire::frame::CreatePlacement, String> {
        use crate::ids::{ActivationId, ResourceKey, ServerInstance};
        use crate::wire::frame::{CreatePlacement, SplitDirection};
        match self {
            Self::NewGroup => Ok(CreatePlacement::NewGroup),
            Self::Beside {
                entry,
                resource,
                instance,
                activation,
                axis,
                ratio,
            } => {
                if *activation == 0 || !ratio.is_finite() || *ratio <= 0.0 || *ratio >= 1.0 {
                    return Err("placement needs a nonzero activation and a finite ratio strictly between zero and one".into());
                }
                Ok(CreatePlacement::Beside {
                    entry: parse_hex16(entry).ok_or("entry must be 32-hex")?,
                    resource: ResourceKey::parse(resource)
                        .ok_or("resource must be a full durable key")?,
                    instance: ServerInstance::new(
                        parse_hex16(instance).ok_or("instance must be 32-hex")?,
                    ),
                    activation: ActivationId::new(*activation),
                    dir: match axis {
                        SplitAxis::Horizontal => SplitDirection::Horizontal,
                        SplitAxis::Vertical => SplitDirection::Vertical,
                    },
                    ratio: *ratio,
                })
            }
        }
    }
}

/// Shared launch validation, enforced again by the executing server for placed creation.
///
/// # Errors
/// Oversized lists, embedded NULs or overrides of private routing/bootstrap variables.
pub fn validate_terminal_launch(launch: &crate::wire::frame::LaunchSpec) -> Result<(), String> {
    if launch.command.first().is_some_and(String::is_empty) {
        return Err("argv[0] must not be empty".into());
    }
    if launch
        .cwd
        .as_ref()
        .is_some_and(|cwd| !std::path::Path::new(cwd).is_absolute())
    {
        return Err("cwd must be an absolute directory on the executing server".into());
    }
    if launch.command.len() > 256 || launch.env.len() > 256 {
        return Err("argv and env are limited to 256 items".into());
    }
    if launch
        .command
        .iter()
        .chain(launch.cwd.iter())
        .any(|value| value.contains('\0'))
    {
        return Err("argv and cwd must not contain NUL".into());
    }
    launch.env.iter().try_for_each(validate_environment_pair)
}

fn validate_environment_pair((key, value): &(String, String)) -> Result<(), String> {
    if key.is_empty() || key.contains(['=', '\0']) || value.contains('\0') {
        return Err("invalid environment key or value".into());
    }
    if key.starts_with("PHUX_") || key == "TERM" {
        return Err("PHUX_ and TERM environment variables are server-owned".into());
    }
    Ok(())
}

/// Parse an exact 16-byte hexadecimal identity, accepting either case.
#[must_use]
pub fn parse_hex16(text: &str) -> Option<[u8; 16]> {
    if text.len() != 32 || !text.is_ascii() {
        return None;
    }
    let mut bytes = [0; 16];
    for (slot, pair) in bytes.iter_mut().zip(text.as_bytes().as_chunks::<2>().0) {
        *slot = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(bytes)
}

/// Canonical lowercase hexadecimal representation of a 16-byte identity.
#[must_use]
pub fn hex16(bytes: &[u8; 16]) -> String {
    use std::fmt::Write as _;
    let mut text = String::with_capacity(32);
    for byte in bytes {
        let _ = write!(text, "{byte:02x}");
    }
    text
}

/// Input schema derived from every nested request type.
#[must_use]
pub fn terminal_create_schema() -> serde_json::Value {
    schemars::schema_for!(TerminalCreate).to_value()
}

/// Output schema derived from every nested receipt type.
#[must_use]
pub fn terminal_created_schema() -> serde_json::Value {
    schemars::schema_for!(TerminalCreated).to_value()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "decode assertions")]
mod tests {
    use super::{ExecutionOutcome, ExecutionState};

    #[test]
    fn unknown_execution_values_decode_without_a_schema_bump() {
        assert_eq!(
            serde_json::from_str::<ExecutionState>("\"future_state\"").unwrap(),
            ExecutionState::Unknown
        );
        assert_eq!(
            serde_json::from_str::<ExecutionOutcome>(r#"{"type":"melted"}"#).unwrap(),
            ExecutionOutcome::Unknown
        );
        assert_eq!(
            serde_json::from_str::<ExecutionState>("\"failed\"").unwrap(),
            ExecutionState::Failed
        );
    }
}
