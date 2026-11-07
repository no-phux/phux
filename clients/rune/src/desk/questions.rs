//! ACP questions: what a native agent asks over the protocol, answered
//! through its SDK. Never typed into the terminal, never approved for you,
//! and never sent once the question it answers has closed or changed.

use std::collections::{BTreeMap, HashMap};

use phux_client_runtime::Client;
use phux_client_runtime::control::{Status, Topology};
use phux_protocol::ids::ResourceId;
use phux_protocol::native_agent::{
    NativeDisposition, NativeIntent, NativeOption, NativeQuestion, NativeView,
};
use rune::TextEdit;
use serde_json::Value;

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

#[derive(Default)]
pub struct Questions {
    /// Each ACP pane's agent session and the view it publishes, by terminal.
    pub views: HashMap<ResourceId, (ResourceId, NativeView)>,
    /// Open questions' drafts, by question id.
    pub drafts: HashMap<String, Draft>,
    /// Answers awaiting their receipt: operation to question id.
    pub pending: HashMap<u64, String>,
    /// How a submitted answer stands. While set, its controls are held.
    pub status: HashMap<String, String>,
    /// Why the last try sent nothing, or was refused. The draft stays.
    pub error: HashMap<String, String>,
    /// What a field edits once its question has gone.
    spare: TextEdit,
}

impl Questions {
    /// The view and open question an ACP ask on `terminal` refers to.
    pub fn open(&self, terminal: &ResourceId, id: &str) -> Option<(&NativeView, &Draft)> {
        let (_, view) = self.views.get(terminal)?;
        view.questions.iter().find(|q| q.id == id)?;
        Some((view, self.drafts.get(id)?))
    }

    /// Whether `id`'s controls take no input now.
    pub fn held(&self, view: &NativeView, id: &str) -> bool {
        self.status.contains_key(id)
            || !view.connected
            || !view.capabilities.questions.is_supported()
    }

    pub fn text_mut(&mut self, question: &str, field: &str) -> &mut TextEdit {
        let Self { drafts, spare, .. } = self;
        match drafts.get_mut(question) {
            Some(draft) => draft.texts.entry(field.to_owned()).or_default(),
            None => spare,
        }
    }

    /// Pick or unpick one offered option; a single choice replaces the last.
    pub fn choose(&mut self, question: &str, item: &str, option: &str, multi: bool) {
        if self.status.contains_key(question) {
            return;
        }
        let Some(draft) = self.drafts.get_mut(question) else {
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
        let open: Vec<&NativeQuestion> = views.values().flat_map(|(_, v)| &v.questions).collect();
        self.drafts
            .retain(|id, draft| open.iter().any(|q| &q.id == id && **q == draft.question));
        let gone = |id: &String| !open.iter().any(|q| &q.id == id);
        self.status.retain(|id, _| !gone(id));
        self.error.retain(|id, _| !gone(id));
        for question in open {
            if !self.drafts.contains_key(&question.id) {
                self.status.remove(&question.id);
                self.error.remove(&question.id);
                self.drafts
                    .insert(question.id.clone(), Draft::new(question));
            }
        }
        self.views = views;
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
        if self.drafts.contains_key(&question) {
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

    /// The connection left before some receipts came back.
    pub fn disconnected(&mut self) -> bool {
        let unknown = response::receipt(
            NativeDisposition::Unknown,
            Some("Connection changed before receipt."),
        );
        for question in self.pending.values() {
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
    /// Answer `question` on `terminal`'s agent: with `response` from its
    /// button, or with what its draft holds.
    pub fn answer_question(
        &mut self,
        terminal: &ResourceId,
        question: &str,
        response: Option<Value>,
    ) {
        let result = self.schedule_answer(terminal, question, response);
        let questions = &mut self.inbox.questions;
        match result {
            Ok(op) => {
                questions.pending.insert(op, question.to_owned());
                questions
                    .status
                    .insert(question.to_owned(), SUBMITTING.to_owned());
                questions.error.remove(question);
            }
            Err(error) => {
                questions.error.insert(question.to_owned(), error);
            }
        }
    }

    fn schedule_answer(
        &self,
        terminal: &ResourceId,
        id: &str,
        response: Option<Value>,
    ) -> Result<u64, String> {
        let questions = &self.inbox.questions;
        if questions.status.contains_key(id) {
            return Err("That response is already on its way.".into());
        }
        let draft = questions.drafts.get(id).ok_or(CLOSED)?;
        let response = match response {
            Some(response) => response,
            None => response::build(draft)?,
        };
        let client = self
            .client
            .as_ref()
            .ok_or("Not connected. Nothing was sent.")?;
        let (session, _) = questions.views.get(terminal).ok_or(CLOSED)?;
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
        let intent = NativeIntent::Answer {
            question_id: id.to_owned(),
            response,
        };
        client
            .native_agent_command(session, activation, intent)
            .ok_or_else(|| UNSCHEDULED.to_owned())
    }
}
