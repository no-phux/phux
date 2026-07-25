use phux_client::agent_meta::{AgentAttention, AgentMetaState, AgentRecord};
use phux_config::plugin::{PluginAgentAttention, PluginAgentState};
use phux_protocol::ids::TerminalId;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(super) struct AgentStateReport {
    pub(super) terminal: String,
    pub(super) session: String,
    pub(super) window: String,
    pub(super) agent: AgentIdentity,
    pub(super) state: AgentState,
    pub(super) confidence: f32,
    pub(super) attention: Attention,
    pub(super) title: Option<String>,
    pub(super) cwd: Option<String>,
    pub(super) sources: Vec<AgentSource>,
    pub(super) explanation: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct AgentIdentity {
    pub(super) id: String,
    pub(super) label: String,
    pub(super) kind: AgentKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum AgentKind {
    Codex,
    Claude,
    OpenCode,
    Pi,
    Omp,
    Plugin,
    /// ADR-0040: identity declared via a `phux.agent/v1` record whose kind
    /// slug is neither a first-party agent nor a configured plugin.
    Declared,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum AgentState {
    Unknown,
    Idle,
    Working,
    Blocked,
    Done,
}

impl std::fmt::Display for AgentState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown => f.write_str("unknown"),
            Self::Idle => f.write_str("idle"),
            Self::Working => f.write_str("working"),
            Self::Blocked => f.write_str("blocked"),
            Self::Done => f.write_str("done"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Attention {
    None,
    Low,
    Normal,
    High,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(super) struct AgentSource {
    pub(super) kind: &'static str,
    pub(super) signal: String,
    pub(super) confidence: f32,
    pub(super) observed: String,
}

#[derive(Debug, Clone)]
pub(super) struct PaneEvidence {
    pub(super) terminal: String,
    pub(super) session: String,
    pub(super) window: String,
    pub(super) title: Option<String>,
    pub(super) cwd: Option<String>,
    /// ADR-0040: the pane's decoded `phux.agent/v1` record, when declared.
    /// Outranks every heuristic source below.
    pub(super) record: Option<AgentRecord>,
    pub(super) lines: Vec<String>,
    pub(super) semantic_input: bool,
}

impl PaneEvidence {
    #[cfg(test)]
    pub(super) fn for_test(terminal: &str, title: Option<&str>, lines: &[&str]) -> Self {
        Self {
            terminal: terminal.to_owned(),
            session: "test".to_owned(),
            window: "window-0".to_owned(),
            title: title.map(str::to_owned),
            cwd: None,
            record: None,
            lines: lines.iter().map(|line| (*line).to_owned()).collect(),
            semantic_input: false,
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct PluginAgent {
    pub(super) id: String,
    pub(super) label: String,
    pub(super) state: PluginAgentState,
    pub(super) attention: PluginAgentAttention,
}

#[derive(Debug, Clone)]
pub(super) struct StateSignal {
    pub(super) state: AgentState,
    pub(super) confidence: f32,
    pub(super) explanation: String,
}

impl StateSignal {
    pub(super) fn new(state: AgentState, confidence: f32, explanation: &str) -> Self {
        Self {
            state,
            confidence,
            explanation: explanation.to_owned(),
        }
    }

    pub(super) fn from_plugin(state: PluginAgentState) -> Self {
        match state {
            PluginAgentState::Unknown => {
                Self::new(AgentState::Unknown, 0.55, "plugin reports unknown")
            }
            PluginAgentState::Idle => Self::new(AgentState::Idle, 0.55, "plugin reports idle"),
            PluginAgentState::Working => {
                Self::new(AgentState::Working, 0.55, "plugin reports working")
            }
            PluginAgentState::Blocked => {
                Self::new(AgentState::Blocked, 0.55, "plugin reports blocked")
            }
        }
    }
}

pub(super) const fn attention_for(state: AgentState) -> Attention {
    match state {
        AgentState::Blocked => Attention::High,
        AgentState::Working => Attention::Normal,
        AgentState::Done | AgentState::Unknown => Attention::Low,
        AgentState::Idle => Attention::None,
    }
}

pub(super) const fn plugin_attention(attention: PluginAgentAttention) -> Attention {
    match attention {
        PluginAgentAttention::None => Attention::None,
        PluginAgentAttention::Low => Attention::Low,
        PluginAgentAttention::Normal => Attention::Normal,
        PluginAgentAttention::High => Attention::High,
    }
}

/// ADR-0040: map a declared `phux.agent/v1` state onto the report vocabulary.
pub(super) const fn record_state(state: AgentMetaState) -> AgentState {
    match state {
        AgentMetaState::Unknown => AgentState::Unknown,
        AgentMetaState::Idle => AgentState::Idle,
        AgentMetaState::Working => AgentState::Working,
        AgentMetaState::Blocked => AgentState::Blocked,
        AgentMetaState::Done => AgentState::Done,
    }
}

/// ADR-0040: map a declared `phux.agent/v1` attention level onto the
/// report vocabulary.
pub(super) const fn record_attention(attention: AgentAttention) -> Attention {
    match attention {
        AgentAttention::None => Attention::None,
        AgentAttention::Low => Attention::Low,
        AgentAttention::Normal => Attention::Normal,
        AgentAttention::High => Attention::High,
    }
}

pub(crate) fn format_terminal(id: &TerminalId) -> String {
    crate::selector::format_terminal_id(id)
}

pub(super) fn identity(id: &str, label: &str, kind: AgentKind) -> AgentIdentity {
    AgentIdentity {
        id: id.to_owned(),
        label: label.to_owned(),
        kind,
    }
}
