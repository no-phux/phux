//! Serving this machine's ssh-agent to the attached host while attached
//! (`forward-agent` on its `[[remote]]` entry; `docs/ssh.md#ssh-agent`).
//!
//! [`AgentLink`] lives as long as one connection, across session switches: it
//! offers once, answers each request off the loop, and hands the loop what
//! it owes ([`Due`]): a question to ask, or a reply to send.

use std::cell::Cell;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use phux_client::agent_forward::{
    AgentRequest, AgentServing, Consent, QUESTION_ARMING, Question, local_agent, request_in,
};
use phux_protocol::forward::ForwardOp;
use phux_protocol::input::key::{KeyAction, KeyEvent, PhysicalKey};
use phux_protocol::input::mouse::MouseEvent;
use phux_protocol::wire::frame::{Command, CommandResult, FrameKind};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratcn::runtime::{DeclareCtx, Event, EventResult, FocusState, Ratcn};
use ratcn::{Button, Dialog};
use tokio::sync::mpsc;

use crate::render::kit::{self, Surface};
use crate::render::overlay::confirm::dialog_area;
use crate::render::overlay::{OverlayCommand, RenderOverlay};
use crate::render::{ChromeBreakpoints, Theme};

/// `notice`, unless this process has already said it: a reconnect meets the
/// same refusal again.
fn say_once(notice: &str) -> Option<String> {
    static SAID: Mutex<Option<HashSet<String>>> = Mutex::new(None);
    let mut said = SAID.lock().unwrap_or_else(PoisonError::into_inner);
    said.get_or_insert_default()
        .insert(notice.to_owned())
        .then(|| notice.to_owned())
}

/// What the attach loop owes the link.
#[derive(Debug)]
pub(in crate::attach) enum Due {
    /// Ask the person at the terminal.
    Ask(Question),
    /// Send the answer to request `id`; its question, if any, is over.
    Reply { id: u64, message: Vec<u8> },
}

/// Where this connection's `AGENT_OFFER` stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Offer {
    Owed,
    /// Sent as this command; its answer is awaited.
    Sent(u32),
    Settled,
}

/// One connection's forwarded agent.
pub(in crate::attach) struct AgentLink {
    host: String,
    /// What answers; or why there is nothing to serve.
    serving: Result<Arc<AgentServing>, String>,
    offer: Mutex<Offer>,
    /// A question waiting for another one's answer.
    held: Mutex<Option<Question>>,
    due_tx: mpsc::UnboundedSender<Due>,
    due_rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<Due>>,
}

impl AgentLink {
    /// The link for a fresh connection to the machine this process attached
    /// to, when its `[[remote]]` entry says `forward-agent = true`.
    pub(in crate::attach) fn for_attach() -> Option<Arc<Self>> {
        let Some(crate::attach::hosts::AttachOrigin::Remote(name)) =
            crate::attach::hosts::recorded_origin()
        else {
            return None;
        };
        let entry = phux_config::loader::load()
            .ok()?
            .remote
            .into_iter()
            .find(|entry| entry.name == name && entry.forward_agent)?;
        Some(Self::new(name, local_agent(None), entry.forward_agent_keys))
    }

    pub(in crate::attach) fn new(
        host: String,
        agent: Result<PathBuf, String>,
        keys: Option<Vec<String>>,
    ) -> Arc<Self> {
        let (due_tx, due_rx) = mpsc::unbounded_channel();
        let ask_tx = due_tx.clone();
        // A question the loop is not there to show is dropped: a no.
        let consent = Consent::Ask(Box::new(move |question| {
            let _ = ask_tx.send(Due::Ask(question));
        }));
        let serving =
            agent.map(|agent| Arc::new(AgentServing::new(agent, host.clone(), keys, consent)));
        Arc::new(Self {
            host,
            serving,
            offer: Mutex::new(Offer::Owed),
            held: Mutex::new(None),
            due_tx,
            due_rx: tokio::sync::Mutex::new(due_rx),
        })
    }

    fn offer(&self) -> std::sync::MutexGuard<'_, Offer> {
        self.offer.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// This connection's one offer, to send as `request_id`: `None` once it
    /// was made. `supported` when the server advertised `FORWARD_AGENT`.
    ///
    /// # Errors
    ///
    /// Why it cannot be made, the first time this process finds out.
    pub(in crate::attach) fn take_offer(
        &self,
        supported: bool,
        request_id: impl FnOnce() -> u32,
    ) -> Result<Option<FrameKind>, Option<String>> {
        let mut offer = self.offer();
        if *offer != Offer::Owed {
            return Ok(None);
        }
        *offer = Offer::Settled;
        if !supported {
            return Err(say_once(&format!(
                "{} does not forward ssh-agents: set `[forward] agent = true` there",
                self.host
            )));
        }
        if let Err(why) = &self.serving {
            return Err(say_once(&format!(
                "ssh-agent not forwarded to {}: {why}",
                self.host
            )));
        }
        let request_id = request_id();
        *offer = Offer::Sent(request_id);
        drop(offer);
        Ok(Some(FrameKind::Command {
            request_id,
            command: Command::Forward {
                op: ForwardOp::AgentOffer,
            },
        }))
    }

    /// Whether `frame` is the link's: a request, or the offer's answer. `Some`
    /// takes it, with what to say if the offer was refused.
    pub(in crate::attach) fn take(&self, frame: &FrameKind) -> Option<Vec<String>> {
        let sent = *self.offer();
        if let FrameKind::CommandResult { request_id, result } = frame
            && sent == Offer::Sent(*request_id)
        {
            *self.offer() = Offer::Settled;
            let refusal = match result {
                CommandResult::Error { message, .. } => {
                    say_once(&format!("{} refused your ssh-agent: {message}", self.host))
                }
                _ => None,
            };
            return Some(refusal.into_iter().collect());
        }
        self.handle(request_in(frame)?);
        Some(Vec::new())
    }

    fn handle(&self, request: AgentRequest) {
        let Ok(serving) = &self.serving else {
            return;
        };
        let (id, due) = (request.id, self.due_tx.clone());
        serving.handle(request, move |answer| {
            if let Some(why) = &answer.refused {
                tracing::info!(%why, "ssh-agent request refused");
            }
            let message = answer.message;
            let _ = due.send(Due::Reply { id, message });
        });
    }

    /// The session under the link is being switched: its request ids start
    /// over, so an offer still unanswered is no longer matched by one.
    pub(in crate::attach) fn session_switched(&self) {
        let mut offer = self.offer();
        if *offer != Offer::Owed {
            *offer = Offer::Settled;
        }
    }

    /// The question to put up, which [`Self::hold`] kept.
    pub(in crate::attach) fn take_held(&self) -> Option<Question> {
        let held = self
            .held
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        held.filter(|question| Instant::now() < question.expires)
    }

    /// Keep `question` until it can go up.
    pub(in crate::attach) fn hold(&self, question: Question) {
        *self.held.lock().unwrap_or_else(PoisonError::into_inner) = Some(question);
    }

    /// What is owed next; pending while nothing is. Cancel-safe.
    pub(in crate::attach) async fn due(&self) -> Due {
        // The link holds a sender, so the queue never closes.
        match self.due_rx.lock().await.recv().await {
            Some(due) => due,
            None => std::future::pending().await,
        }
    }
}

/// [`AgentLink::due`] of the link there is; pending without one.
pub(in crate::attach) async fn next_due(link: Option<&AgentLink>) -> Due {
    match link {
        Some(link) => link.due().await,
        None => std::future::pending().await,
    }
}

const DIALOG: &str = "agent-sign";
const SIGN: &str = "sign";
const REFUSE: &str = "refuse";

#[derive(Debug)]
struct Model {
    focus: FocusState,
}

#[derive(Debug, Clone)]
enum Msg {
    Focus(FocusState),
    Answer(bool),
}

/// The question of one signature, a ratcn [`Dialog`]. Focus starts on
/// `Refuse`, the right-hand button. Only Left then Enter, or a click, signs,
/// and only while the keyboard has been quiet for [`QUESTION_ARMING`]: a
/// line being typed, a completion or suggestion it accepts, and its Enter
/// all leave the question standing. Dropping the overlay refuses.
pub(in crate::attach) struct AgentSignOverlay {
    question: Question,
    message: String,
    /// The last key that was not an answer; at first, when it was shown.
    last_key: Instant,
    theme: Theme,
    breakpoints: ChromeBreakpoints,
    model: Model,
    ui: Surface<Model, Msg>,
    /// The viewport of the last paint, which input is laid out against.
    outer: Cell<Rect>,
}

impl AgentSignOverlay {
    /// Ask `question`; `label` is what this client calls its pane.
    pub(in crate::attach) fn new(question: Question, label: Option<&str>, theme: &Theme) -> Self {
        let message = format!(
            "{} wants to sign with your key\n{}\n← then Enter signs; Enter or Esc refuses",
            question.asker(label),
            question.key
        );
        Self {
            question,
            message,
            last_key: Instant::now(),
            theme: *theme,
            breakpoints: ChromeBreakpoints::default(),
            model: Model {
                focus: FocusState::intent([DIALOG, REFUSE]),
            },
            ui: Surface::new(Ratcn::new().focus(|m: &Model| &m.focus, Msg::Focus)),
            outer: Cell::new(Rect::new(0, 0, 80, 24)),
        }
    }

    fn armed(&self) -> bool {
        self.last_key.elapsed() >= QUESTION_ARMING
    }

    fn modal_area(&self, outer: Rect) -> Rect {
        dialog_area(outer, &self.message, self.breakpoints)
    }

    /// Declare the dialog filling `area` (the modal rect).
    fn declare(&self, area: Rect) -> impl FnMut(&mut DeclareCtx<'_, Model, Msg>) + use<> {
        let message = self.message.clone();
        move |ctx| {
            let dialog = Dialog::new()
                .title("Use your ssh key?")
                .description(message.clone())
                .outer_width(area.width)
                .outer_height(area.height)
                .on_dismiss(|| Msg::Answer(false))
                .action(
                    SIGN,
                    Button::new("Sign")
                        .secondary()
                        .on_press(|| Msg::Answer(true)),
                )
                .action(
                    REFUSE,
                    Button::new("Refuse").on_press(|| Msg::Answer(false)),
                );
            ctx.component(DIALOG, dialog, area);
        }
    }

    /// Route one ratcn event and fold its message into a command.
    fn route(&mut self, event: Event) -> OverlayCommand {
        let area = self.modal_area(self.outer.get());
        let declare = self.declare(area);
        let theme = kit::theme(&self.theme);
        match self.ui.route(event, &self.model, &theme, declare) {
            EventResult::Emit(Msg::Focus(focus)) => {
                self.model.focus = focus;
                OverlayCommand::Stay
            }
            EventResult::Emit(Msg::Answer(yes)) => self.answer(yes),
            EventResult::Consumed | EventResult::Ignored => OverlayCommand::Stay,
        }
    }

    /// Record the answer and close, unless it signs before the prompt armed.
    fn answer(&self, yes: bool) -> OverlayCommand {
        if yes && !self.armed() {
            return OverlayCommand::Stay;
        }
        self.question.answer(yes);
        OverlayCommand::Dismiss
    }
}

impl RenderOverlay for AgentSignOverlay {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        self.outer.set(area);
        let modal = self.modal_area(area);
        let theme = kit::theme(&self.theme);
        self.ui
            .paint(buf, modal, &self.model, &theme, self.declare(modal));
    }

    fn bounds(&self, area: Rect) -> Option<Rect> {
        Some(self.modal_area(area))
    }

    fn set_breakpoints(&mut self, bp: ChromeBreakpoints) {
        self.breakpoints = bp;
        self.ui.invalidate();
    }

    fn set_theme(&mut self, theme: &Theme) {
        self.theme = *theme;
    }

    fn on_viewport_resize(&mut self, _pane_cols: u16, _pane_rows: u16) {
        self.ui.invalidate();
    }

    fn expired(&self) -> bool {
        Instant::now() >= self.question.expires
    }

    fn asks(&self) -> bool {
        true
    }

    fn handle_key(&mut self, key: &KeyEvent) -> OverlayCommand {
        if key.action != KeyAction::Press {
            return OverlayCommand::Stay;
        }
        if key.key == PhysicalKey::Escape {
            return self.answer(false);
        }
        if key.mods.is_empty() && self.armed() {
            match key.key {
                PhysicalKey::ArrowLeft => {
                    self.model.focus = FocusState::intent([DIALOG, SIGN]);
                    return OverlayCommand::Stay;
                }
                PhysicalKey::Enter => {
                    return kit::key(key).map_or(OverlayCommand::Stay, |event| self.route(event));
                }
                _ => {}
            }
        }
        // Typing: back to the default, and quiet again before any answer.
        self.last_key = Instant::now();
        self.model.focus = FocusState::intent([DIALOG, REFUSE]);
        OverlayCommand::Stay
    }

    fn handle_mouse(&mut self, mouse: &MouseEvent) -> OverlayCommand {
        kit::mouse(mouse).map_or(OverlayCommand::Stay, |event| self.route(event))
    }
}

#[cfg(test)]
pub(in crate::attach) mod tests;
