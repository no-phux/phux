//! Data passed to an action process, never a credential or a dial plan.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Which adapter invoked an action. Set by the adapter, never by its input.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActionSource {
    /// A local command-line invocation.
    Cli,
    /// An invocation through the local MCP adapter.
    Mcp,
    /// A TUI palette commit.
    TuiPalette,
    /// A TUI keybinding commit.
    TuiKey,
    /// Other TUI chrome (for example a status button).
    Tui,
    /// A Rune desktop palette commit.
    RunePalette,
}

/// Host and selection facts for an action invocation.
///
/// The adapter captures these facts, or a headless caller supplies them.
/// Actions must not fill in missing facts from the process cwd, a default
/// server, or the ambient pane.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct ActionTarget {
    /// Serving host name, when observed. Not a socket path or URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// The adapter's configured route label, distinct from the serving host.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route: Option<String>,
    /// Transport family, never its credentials or dial parameters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport: Option<String>,
    /// Selected satellite route, if the resource is not on the serving host.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub satellite: Option<String>,
    /// Durable organization authority, when observed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authority: Option<String>,
    /// Durable workspace key, if the adapter knows it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// Attached/selected session's durable key, when observed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// Connection-local session identifier, when observed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_handle: Option<u32>,
    /// Observed session label; not an identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,
    /// Selected resource's durable key, when observed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource: Option<String>,
    /// Selected resource's connection-relative handle, including route.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_handle: Option<String>,
    /// Selected resource execution's activation, when observed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activation: Option<u64>,
    /// Selected resource execution's instance, when observed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
    /// Arrangement containing the selected view, when observed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arrangement: Option<String>,
    /// The exact arrangement group containing the visible entry.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// Selected entry in the arrangement, when observed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entry: Option<String>,
}

/// Immutable context captured before dispatching the child job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ActionContext {
    schema_version: u8,
    source: ActionSource,
    /// Connection-local invoking client, never a guessed ID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client: Option<u32>,
    /// Whether the target was observed by the adapter. Headless target input
    /// is declarative data and does not attest identity or grant access.
    observed: bool,
    /// Local execution is independent of the selected resource's machine.
    executor: &'static str,
    /// Exact target facts known at commit time.
    pub target: ActionTarget,
    /// Application-specific JSON input.
    pub input: Value,
}

impl ActionContext {
    /// Create a headless invocation with only caller-supplied target facts.
    #[must_use]
    pub const fn supplied(source: ActionSource, target: ActionTarget, input: Value) -> Self {
        Self::new(source, None, false, target, input)
    }

    /// Create a UI invocation from facts already observed on its route.
    #[must_use]
    pub const fn observed(source: ActionSource, client: Option<u32>, target: ActionTarget) -> Self {
        Self::new(source, client, true, target, Value::Null)
    }

    /// Carry the invocation's arguments, leaving the captured target unchanged.
    #[must_use]
    pub fn with_input(mut self, input: Value) -> Self {
        self.input = input;
        self
    }

    const fn new(
        source: ActionSource,
        client: Option<u32>,
        observed: bool,
        target: ActionTarget,
        input: Value,
    ) -> Self {
        Self {
            schema_version: 1,
            source,
            client,
            observed,
            executor: "invoking_machine",
            target,
            input,
        }
    }
}
