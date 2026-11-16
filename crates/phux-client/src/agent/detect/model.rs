use crate::agent_meta::{AgentAttention, AgentMetaState, AgentRecord};
use phux_config::plugin::{PluginAgentAttention, PluginAgentState};
use serde::Serialize;

/// One pane's projected agent state: an element of `agents[]` in
/// `phux agent list|show|explain --json`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AgentStateReport {
    /// The pane's canonical selector (`@N`).
    pub terminal: String,
    /// The phux session the pane is in, or `unknown`.
    pub session: String,
    /// The legacy window label (`window-0`), kept for output stability.
    pub window: String,
    /// Which agent runs there, as far as the evidence says.
    pub agent: AgentIdentity,
    /// The reported lifecycle state.
    pub state: AgentMetaState,
    /// How much the evidence supports `state`, 0 to 1.
    #[serde(serialize_with = "confidence_json")]
    pub confidence: f32,
    /// How urgently the pane wants a human.
    pub attention: AgentAttention,
    /// The pane's live OSC title.
    pub title: Option<String>,
    /// The pane's working directory, when the server knows it.
    pub cwd: Option<String>,
    /// The evidence behind `state`, strongest first.
    pub sources: Vec<AgentSource>,
    /// Why `state` is what it is, in a sentence.
    pub explanation: String,
    /// The pane's live `AgentSession` child (ADR-0103), or `null` when it
    /// has none. Additive; `schema_version` stays 1. Named `agent_session`
    /// because `session` in this document is already the phux session name.
    pub agent_session: Option<AgentSessionJson>,
    /// `%name` when that selector resolves to this pane, else `null`
    /// (ADR-0075 point 4). Additive; `schema_version` stays 1.
    pub address: Option<String>,
    /// Why this pane's record name is listed but not `%`-addressable: the
    /// `--json` code `%name` would refuse with (`invalid_agent_name`,
    /// `selector_not_single`, `partial_view`). `null` when addressable or
    /// when the pane has no record.
    pub address_refusal: Option<&'static str>,
}

impl AgentStateReport {
    /// Search only inventory fields, never screen contents or detector evidence.
    pub fn matches_query(&self, query: &str) -> bool {
        if query.trim().is_empty() {
            return true;
        }
        let fields = [
            self.terminal.as_str(),
            self.session.as_str(),
            self.agent.id.as_str(),
            self.agent.label.as_str(),
            self.address.as_deref().unwrap_or_default(),
            self.title.as_deref().unwrap_or_default(),
            self.cwd.as_deref().unwrap_or_default(),
        ]
        .map(str::to_lowercase);
        query.split_whitespace().all(|word| {
            let word = word.to_lowercase();
            fields.iter().any(|field| field.contains(&word))
        })
    }

    /// Record what `%name` makes of this pane ([`crate::selector::agent_address`]).
    pub fn set_address(&mut self, address: Option<Result<String, &'static str>>) {
        (self.address, self.address_refusal) = match address {
            None => (None, None),
            Some(Ok(address)) => (Some(address), None),
            Some(Err(code)) => (None, Some(code)),
        };
    }

    /// The human view's address column: `%name`, `-` with no record, or why
    /// the record's name is not `%`-addressable. A name `%` cannot spell, or
    /// one several panes share, prints the command that names the pane.
    #[must_use]
    pub fn address_column(&self) -> String {
        use crate::contract::codes::{INVALID_AGENT_NAME, SELECTOR_NOT_SINGLE};
        match (&self.address, self.address_refusal) {
            (Some(address), _) => address.clone(),
            (None, Some(code)) if code == INVALID_AGENT_NAME || code == SELECTOR_NOT_SINGLE => {
                format!("(name it: phux agent set {} --name NAME)", self.terminal)
            }
            (None, Some(code)) => format!("(not %-addressable: {code})"),
            (None, None) => "-".to_owned(),
        }
    }
}

/// A confidence as the shortest decimal that round-trips the `f32` (`0.2`),
/// not the widened `f64` (`0.20000000298023224`) a JSON value would carry.
#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's serialize_with hands the field by reference"
)]
fn confidence_json<S: serde::Serializer>(
    confidence: &f32,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let shortest = confidence
        .to_string()
        .parse::<f64>()
        .unwrap_or_else(|_| f64::from(*confidence));
    serializer.serialize_f64(shortest)
}

/// The `agent_session` object of `AgentStateJson`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentSessionJson {
    /// The session's canonical selector (`@N`).
    pub resource: String,
    /// The harness slug the session was opened with.
    pub provider: String,
    /// The provider's own session id, when it has one.
    pub native_id: Option<String>,
}

/// Which agent a pane runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentIdentity {
    /// The agent's slug (`codex`, a plugin id, `unknown`).
    pub id: String,
    /// The name to show: the record's name, else the agent's label.
    pub label: String,
    /// The agent family.
    pub kind: AgentKind,
}

/// The agent family a report names, as `agent.kind` spells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    /// `OpenAI` Codex.
    Codex,
    /// Claude Code.
    Claude,
    /// `OpenCode`.
    OpenCode,
    /// Pi.
    Pi,
    /// Oh My Pi.
    Omp,
    /// Grok.
    Grok,
    /// Amp.
    Amp,
    /// Cursor Agent.
    CursorAgent,
    /// Gemini CLI.
    Gemini,
    /// Goose.
    Goose,
    /// Aider.
    Aider,
    /// A `[[plugins]]` agent declaration.
    Plugin,
    /// ADR-0040: identity declared via a `phux.agent/v1` record whose kind
    /// slug is neither a first-party agent nor a configured plugin.
    Declared,
    /// No evidence names an agent.
    Unknown,
}

/// One piece of evidence behind a reported state, as `sources[]` in
/// `agent list --json`. `rule` / `region` are present only on sources that are
/// a detection-manifest rule.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AgentSource {
    /// The source family (`agent_record`, `detector_rule`, `identity`, ...).
    pub kind: &'static str,
    /// What the source says, in a sentence.
    pub signal: String,
    /// How much this source alone supports the state, 0 to 1.
    #[serde(serialize_with = "confidence_json")]
    pub confidence: f32,
    /// What the source saw.
    pub observed: String,
    /// The ADR-0046 manifest rule id, when this source is one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rule: Option<String>,
    /// The manifest region that rule read, in the spelling a manifest uses.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
}

impl AgentSource {
    /// A source that is not a manifest rule.
    #[must_use]
    pub fn new(
        kind: &'static str,
        signal: impl Into<String>,
        confidence: f32,
        observed: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            signal: signal.into(),
            confidence,
            observed: observed.into(),
            rule: None,
            region: None,
        }
    }

    /// Attach the detection-manifest rule this source came from.
    #[must_use]
    pub fn with_rule(mut self, rule: impl Into<String>, region: impl Into<String>) -> Self {
        self.rule = Some(rule.into());
        self.region = Some(region.into());
        self
    }
}

/// What the projection knows about one pane before it decides anything.
#[derive(Debug, Clone)]
pub struct PaneEvidence {
    /// The pane's canonical selector (`@N`).
    pub terminal: String,
    /// The phux session the pane is in, or `unknown`.
    pub session: String,
    /// The legacy window label.
    pub window: String,
    /// The pane's live OSC title.
    pub title: Option<String>,
    /// The pane's working directory.
    pub cwd: Option<String>,
    /// ADR-0040: the pane's decoded `phux.agent/v1` record, when declared.
    /// Outranks every heuristic source below.
    pub record: Option<AgentRecord>,
    /// ADR-0103: the pane's live `AgentSession` child and the state its
    /// stream derived, when the server serves one.
    pub agent_session: Option<SessionEvidence>,
    /// The visible screen, one string per row.
    pub lines: Vec<String>,
    /// Whether any visible cell is OSC-133 input.
    pub semantic_input: bool,
    /// The server-observed foreground process, when the detector has
    /// published one. A pane whose foreground is its own shell runs no
    /// agent, whatever its screen says.
    pub occupant: Option<crate::agent_meta::PaneOccupantRecord>,
}

/// The `AgentSession` facet a snapshot carries for a pane's child.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionEvidence {
    /// The session's canonical selector (`@N`).
    pub resource: String,
    /// The harness slug the session was opened with.
    pub provider: String,
    /// The provider's own session id, when it has one.
    pub native_id: Option<String>,
    /// The stream-derived state word (open vocabulary on the wire).
    pub state: AgentMetaState,
}

impl SessionEvidence {
    /// The report's `agent_session` object.
    #[must_use]
    pub fn to_json(&self) -> AgentSessionJson {
        AgentSessionJson {
            resource: self.resource.clone(),
            provider: self.provider.clone(),
            native_id: self.native_id.clone(),
        }
    }
}

impl PaneEvidence {
    /// A pane with only these fields set, for tests of the projection.
    #[cfg(any(test, feature = "testkit"))]
    #[must_use]
    pub fn for_test(terminal: &str, title: Option<&str>, lines: &[&str]) -> Self {
        Self {
            terminal: terminal.to_owned(),
            session: "test".to_owned(),
            window: "window-0".to_owned(),
            title: title.map(str::to_owned),
            cwd: None,
            record: None,
            agent_session: None,
            lines: lines.iter().map(|line| (*line).to_owned()).collect(),
            semantic_input: false,
            occupant: None,
        }
    }
}

/// An agent a `[[plugins]]` manifest declares.
#[derive(Debug, Clone)]
pub struct PluginAgent {
    /// The agent's slug.
    pub id: String,
    /// The name to show.
    pub label: String,
    /// The state the plugin reports for it.
    pub state: PluginAgentState,
    /// The attention the plugin reports for it.
    pub attention: PluginAgentAttention,
}

#[derive(Debug, Clone)]
pub(super) struct StateSignal {
    pub(super) state: AgentMetaState,
    pub(super) confidence: f32,
    pub(super) explanation: String,
}

impl StateSignal {
    pub(super) fn new(state: AgentMetaState, confidence: f32, explanation: &str) -> Self {
        Self {
            state,
            confidence,
            explanation: explanation.to_owned(),
        }
    }

    pub(super) fn from_plugin(state: PluginAgentState) -> Self {
        match state {
            PluginAgentState::Unknown => {
                Self::new(AgentMetaState::Unknown, 0.55, "plugin reports unknown")
            }
            PluginAgentState::Idle => Self::new(AgentMetaState::Idle, 0.55, "plugin reports idle"),
            PluginAgentState::Working => {
                Self::new(AgentMetaState::Working, 0.55, "plugin reports working")
            }
            PluginAgentState::Blocked => {
                Self::new(AgentMetaState::Blocked, 0.55, "plugin reports blocked")
            }
        }
    }
}

pub(super) const fn plugin_attention(attention: PluginAgentAttention) -> AgentAttention {
    match attention {
        PluginAgentAttention::None => AgentAttention::None,
        PluginAgentAttention::Low => AgentAttention::Low,
        PluginAgentAttention::Normal => AgentAttention::Normal,
        PluginAgentAttention::High => AgentAttention::High,
    }
}

pub(super) fn identity(id: &str, label: &str, kind: AgentKind) -> AgentIdentity {
    AgentIdentity {
        id: id.to_owned(),
        label: label.to_owned(),
        kind,
    }
}
