//! ACP questions: what a native agent asks over the protocol, answered
//! through its SDK. Never typed into the terminal, never approved for you,
//! and never sent once the question it answers has closed or changed.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};

use phux_client_runtime::Client;
use phux_client_runtime::control::{Status, Topology};
use phux_protocol::ids::ResourceId;
use phux_protocol::native_agent::{
    NativeDisposition, NativeIntent, NativeOption, NativeQuestion, NativeView,
};
use rune::TextEdit;
use serde_json::{Value, json};

use super::Desk;

pub mod response;
#[cfg(test)]
mod tests;

pub const SUBMITTING: &str = "Submitting response; waiting for ACP receipt…";
pub const CLOSED: &str = "The provider request closed or changed. Nothing was sent.";
const UNSCHEDULED: &str = "Response not scheduled; your draft is preserved.";

/// One open question's controls, as the person has set them.
pub struct Draft {
    /// The exact question it answers; a changed question starts afresh.
    pub question: NativeQuestion,
    /// Text fields by id: `text` for input and editor, else the item or
    /// property id.
    pub texts: HashMap<String, TextEdit>,
    /// Chosen option ids by item or property id. Nothing starts chosen.
    pub choices: HashMap<String, Vec<String>>,
    /// A form's fixed-choice fields.
    pub options: BTreeMap<String, Vec<NativeOption>>,
}

impl Draft {
    pub fn new(question: &NativeQuestion) -> Self {
        let mut texts = HashMap::new();
        let fields = &question.fields;
        match question.kind.as_str() {
            "input" | "editor" => {
                let initial = fields.get("initial_value").and_then(Value::as_str);
                texts.insert("text".to_owned(), TextEdit::new(initial.unwrap_or("")));
            }
            "ask" if fields.get("allow_custom_input").and_then(Value::as_bool) != Some(false) => {
                for item in &question.questions {
                    texts.insert(item.id.clone(), TextEdit::default());
                }
            }
            _ => {}
        }
        let options = response::form_options(question);
        if question.kind == "form" {
            let properties = fields.get("properties").and_then(Value::as_object);
            for id in properties.into_iter().flat_map(|p| p.keys()) {
                if !options.contains_key(id) {
                    texts.insert(id.clone(), TextEdit::default());
                }
            }
        }
        Self {
            question: question.clone(),
            texts,
            choices: HashMap::new(),
            options,
        }
    }

    pub fn text(&self, id: &str) -> String {
        self.texts
            .get(id)
            .map(|edit| edit.text.clone())
            .unwrap_or_default()
    }
}

/// A captured local question owner. The generation fences old widgets and
/// receipts when the same request changes or closes and later reopens.
/// It is not a protocol revision and is never sent to the provider.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Owner {
    terminal: ResourceId,
    session: ResourceId,
    activation: String,
    question: String,
    generation: u64,
}

/// Widget roles and provider IDs are separate components, never reserved names.
#[derive(Clone, Copy, Debug)]
pub enum Widget<'a> {
    Action(&'a str),
    Option(&'a str),
    Choices(&'a str),
    Choice(&'a str, &'a str),
    Field(&'a str),
}

impl Owner {
    pub fn terminal(&self) -> &ResourceId {
        &self.terminal
    }

    pub fn widget_prefix(&self) -> String {
        format!("acp-{}-", self.generation)
    }

    /// Widget identity uses the local generation, not a provider's local id.
    pub fn widget(&self, widget: Widget<'_>) -> String {
        let parts = match widget {
            Widget::Action(id) => json!(["action", id]),
            Widget::Option(id) => json!(["option", id]),
            Widget::Choices(item) => json!(["choices", item]),
            Widget::Choice(item, option) => json!(["choice", item, option]),
            Widget::Field(field) => json!(["field", field]),
        };
        format!("{}{}", self.widget_prefix(), key_segment(parts))
    }

    pub fn field_key(&self, field: &str) -> String {
        self.widget(Widget::Field(field))
    }

    pub fn form_key(&self, field: &str) -> String {
        format!("acp-form-{}-{}", self.generation, key_segment(json!(field)))
    }
}

/// Rune appends keys verbatim to full paths. Encode even JSON's literal `/`.
fn key_segment(value: Value) -> String {
    url::form_urlencoded::byte_serialize(value.to_string().as_bytes()).collect()
}

#[derive(Default)]
pub struct Questions {
    /// Each ACP pane's agent session and the view it publishes, by terminal.
    pub views: HashMap<ResourceId, (ResourceId, NativeView)>,
    /// Open questions' drafts, by captured owner.
    pub drafts: HashMap<Owner, Draft>,
    /// Answers awaiting their receipt: operation to captured owner.
    pub pending: HashMap<u64, Owner>,
    /// How a submitted answer stands. While set, its controls are held.
    pub status: HashMap<Owner, String>,
    /// Why the last try sent nothing, or was refused. The draft stays.
    pub error: HashMap<Owner, String>,
    /// What a field edits once its question has gone.
    spare: TextEdit,
}

impl Questions {
    /// Resolve once while constructing a view; callbacks retain this handle.
    pub fn owner(&self, terminal: &ResourceId, id: &str) -> Option<&Owner> {
        self.drafts.keys().find(|owner| {
            &owner.terminal == terminal && owner.question == id && self.draft(owner).is_some()
        })
    }

    fn current_view(&self, owner: &Owner) -> Option<&NativeView> {
        let (session, view) = self.views.get(&owner.terminal)?;
        (session == &owner.session && view.activation == owner.activation).then_some(view)
    }

    pub fn draft(&self, owner: &Owner) -> Option<&Draft> {
        let draft = self.drafts.get(owner)?;
        self.current_view(owner)?
            .questions
            .iter()
            .any(|q| q == &draft.question)
            .then_some(draft)
    }

    /// The view and open question an ACP ask on `terminal` refers to.
    pub fn open(&self, terminal: &ResourceId, id: &str) -> Option<(&NativeView, &Draft)> {
        let owner = self.owner(terminal, id)?;
        Some((self.current_view(owner)?, self.draft(owner)?))
    }

    /// Whether this captured owner's controls take no input now.
    pub fn held(&self, owner: &Owner) -> bool {
        self.status.contains_key(owner)
            || self.draft(owner).is_none()
            || self
                .current_view(owner)
                .is_none_or(|view| !view.connected || !view.capabilities.questions.is_supported())
    }

    /// An enabled field in this exact live question owns text input.
    pub fn field_focused(&self, terminal: &ResourceId, id: &str) -> bool {
        self.owner(terminal, id).is_some_and(|owner| {
            !self.held(owner) && self.drafts[owner].texts.values().any(|edit| edit.focused)
        })
    }

    pub fn field(&self, owner: &Owner, field: &str) -> Option<&TextEdit> {
        if self.held(owner) {
            return None;
        }
        self.draft(owner)?.texts.get(field)
    }

    pub fn field_mut(&mut self, owner: &Owner, field: &str) -> Option<&mut TextEdit> {
        if self.held(owner) {
            return None;
        }
        self.drafts.get_mut(owner)?.texts.get_mut(field)
    }

    /// Lifecycle cleanup may retire only this exact owner's transient preedit,
    /// even while held. It must not grant ordinary access to disabled content.
    pub(in crate::desk) fn cancel_field_composition(&mut self, owner: &Owner, field: &str) {
        if self.draft(owner).is_none() {
            return;
        }
        if let Some(edit) = self
            .drafts
            .get_mut(owner)
            .and_then(|draft| draft.texts.get_mut(field))
        {
            edit.cancel_composition();
        }
    }

    pub fn text_mut(&mut self, owner: &Owner, field: &str) -> &mut TextEdit {
        if self.held(owner) {
            return &mut self.spare;
        }
        self.drafts
            .get_mut(owner)
            .and_then(|draft| draft.texts.get_mut(field))
            .unwrap_or(&mut self.spare)
    }

    /// Pick or unpick one offered option; a single choice replaces the last.
    pub fn choose(&mut self, owner: &Owner, item: &str, option: &str, multi: bool) {
        if self.held(owner) {
            return;
        }
        let Some(draft) = self.drafts.get_mut(owner) else {
            return;
        };
        let values = draft.choices.entry(item.to_owned()).or_default();
        if values.iter().any(|value| value == option) {
            values.retain(|value| value != option);
        } else {
            if !multi {
                values.clear();
            }
            values.push(option.to_owned());
        }
    }

    /// Follow the views the topology's agent sessions publish: a new or
    /// changed question gets a fresh draft, and a closed one takes its own.
    pub fn refresh(&mut self, client: &Client, topology: Option<&Topology>) {
        let engine = client.engine();
        let mut views: HashMap<ResourceId, (ResourceId, NativeView)> = HashMap::new();
        let sessions = topology.into_iter().flat_map(|t| &t.agent_sessions);
        for session in sessions {
            let (Some(parent), Some(engine)) = (&session.parent, &engine) else {
                continue;
            };
            let Some(view) = engine.native_agent_view(&session.id) else {
                continue;
            };
            // A terminal's asking session wins over a quiet one.
            if views
                .get(parent)
                .is_none_or(|(_, v)| v.questions.is_empty())
            {
                views.insert(parent.clone(), (session.id.clone(), view));
            }
        }
        self.views = views;
        self.reconcile();
    }

    /// Reconcile exact questions with their session/activation owner.
    pub(crate) fn reconcile(&mut self) {
        let current: Vec<_> = self
            .drafts
            .keys()
            .filter(|owner| self.draft(owner).is_some())
            .cloned()
            .collect();
        self.drafts.retain(|owner, _| current.contains(owner));
        self.status.retain(|owner, _| current.contains(owner));
        self.error.retain(|owner, _| current.contains(owner));
        for (terminal, (session, view)) in &self.views {
            for question in &view.questions {
                let exists = current.iter().any(|owner| {
                    owner.terminal == *terminal
                        && owner.session == *session
                        && owner.activation == view.activation
                        && owner.question == question.id
                });
                if exists {
                    continue;
                }
                // Survives a connection replacement's fresh Questions value.
                static GENERATION: AtomicU64 = AtomicU64::new(1);
                let generation = GENERATION
                    .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
                    .expect("question generation exhausted");
                let owner = Owner {
                    terminal: terminal.clone(),
                    session: session.clone(),
                    activation: view.activation.clone(),
                    question: question.id.clone(),
                    generation,
                };
                self.drafts.insert(owner, Draft::new(question));
            }
        }
    }

    /// A receipt for one of ours. Refused frees the draft to send again;
    /// Queued and Unknown keep waiting, and nothing is resent.
    pub fn receipt(
        &mut self,
        op: u64,
        disposition: NativeDisposition,
        detail: Option<&str>,
    ) -> bool {
        let Some(question) = self.pending.get(&op).cloned() else {
            return false;
        };
        let message = response::receipt(disposition, detail);
        if self.draft(&question).is_some() {
            if disposition == NativeDisposition::Refused {
                self.status.remove(&question);
                self.error.insert(question, message);
            } else {
                self.status.insert(question, message);
            }
        }
        if !matches!(
            disposition,
            NativeDisposition::Queued | NativeDisposition::Unknown
        ) {
            self.pending.remove(&op);
        }
        true
    }

    /// The open question `terminal`'s agent session publishes, as an ask.
    /// The live ask event carries no text for a question raised before this
    /// window connected; the session's view still has the whole question.
    pub fn ask(&self, terminal: &ResourceId) -> Option<crate::activity::Ask> {
        let question = self.views.get(terminal)?.1.questions.first()?;
        let text = if question.title.is_empty() {
            &question.message
        } else {
            &question.title
        };
        Some(crate::activity::Ask {
            id: question.id.clone(),
            text: text.clone(),
            suggestions: Vec::new(),
            since: None,
        })
    }

    /// The connection left before some receipts came back.
    pub fn disconnected(&mut self) -> bool {
        let unknown = response::receipt(
            NativeDisposition::Unknown,
            Some("Connection changed before receipt."),
        );
        for question in self.pending.values() {
            if self.draft(question).is_none() {
                continue;
            }
            if let Some(status) = self.status.get_mut(question) {
                status.clone_from(&unknown);
            }
        }
        !self.pending.is_empty()
    }
}

/// Whether an answer to `question` may go to `session`'s activation now:
/// the activation, or why nothing is sent.
pub fn admit<'a>(
    attached: bool,
    parented: bool,
    view: Option<&'a NativeView>,
    question: &NativeQuestion,
) -> Result<&'a str, &'static str> {
    if !attached {
        return Err("Not connected. Nothing was sent.");
    }
    if !parented {
        return Err("The agent session changed. Nothing was sent.");
    }
    let view = view.ok_or(CLOSED)?;
    if !view.connected {
        return Err("ACP controller disconnected. Nothing was sent; no terminal fallback.");
    }
    if !view.capabilities.questions.is_supported() {
        return Err("This agent takes no answers over ACP. Nothing was sent.");
    }
    if !view.questions.iter().any(|open| open == question) {
        return Err(CLOSED);
    }
    Ok(&view.activation)
}

impl Desk {
    /// Answer the captured owner's question: with `response` from its
    /// button, or with what its draft holds.
    pub fn answer_question(&mut self, owner: &Owner, response: Option<Value>) {
        // Never resolve a delayed callback's terminal/id to a replacement owner.
        let result = self.schedule_answer(owner, response);
        let questions = &mut self.inbox.questions;
        match result {
            Ok(op) => {
                questions.pending.insert(op, owner.clone());
                questions
                    .status
                    .insert(owner.clone(), SUBMITTING.to_owned());
                questions.error.remove(owner);
            }
            Err(error) => {
                questions.error.insert(owner.clone(), error);
            }
        }
    }

    fn schedule_answer(&self, owner: &Owner, response: Option<Value>) -> Result<u64, String> {
        let questions = &self.inbox.questions;
        if questions.status.contains_key(owner) {
            return Err("That response is already on its way.".into());
        }
        let draft = questions.draft(owner).ok_or(CLOSED)?;
        let response = match response {
            Some(response) => response,
            None => response::build(draft)?,
        };
        let client = self
            .connection
            .client
            .as_ref()
            .ok_or("Not connected. Nothing was sent.")?;
        let (session, terminal) = (&owner.session, &owner.terminal);
        // The cached view may be behind: re-read everything the write rides on.
        let parented = client.topology().is_some_and(|topology| {
            let mut sessions = topology.agent_sessions.iter();
            sessions.any(|s| &s.id == session && s.parent.as_ref() == Some(terminal))
        });
        let view = client
            .engine()
            .and_then(|engine| engine.native_agent_view(session));
        let attached = client.status() == Status::Attached;
        let activation = admit(attached, parented, view.as_ref(), &draft.question)?;
        if activation != owner.activation {
            return Err(CLOSED.into());
        }
        let intent = NativeIntent::Answer {
            question_id: owner.question.clone(),
            response,
        };
        client
            .native_agent_command(session, activation, intent)
            .ok_or_else(|| UNSCHEDULED.to_owned())
    }
}
