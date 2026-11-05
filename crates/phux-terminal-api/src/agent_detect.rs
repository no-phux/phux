//! Reports cross the engine boundary; detection algorithms do not.

pub use phux_agent_rules::DetectedState;
pub type LiveSessionProbe = std::rc::Rc<dyn Fn() -> bool>;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PaneOccupant {
    pub foreground: String,
    pub is_pane_shell: bool,
    pub background: bool,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentReport {
    pub kind: String,
    pub name: String,
    pub state: DetectedState,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentDetectEvent {
    Occupant(PaneOccupant),
    State(AgentReport),
    Reidentified { kind: String, name: String },
    AskSentinel(Option<phux_agent_session::asked::AskedPayload>),
    Retract,
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ProgramStatusRecord {
    pub id: String,
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub msg: Option<String>,
}
