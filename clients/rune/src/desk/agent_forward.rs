//! Serving this machine's ssh-agent to the machine this window is on
//! (`forward-agent` on its `[[remote]]` entry; `docs/ssh.md#ssh-agent`).
//!
//! Each connection offers once. A pane's signature waits in the confirm bar
//! with the others: the approval chord or the button signs, and only once the
//! question has been up for [`QUESTION_ARMING`]; Escape, a minute unanswered,
//! or anything that takes the question down refuses.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Instant;

use phux_client_runtime::Client;
use phux_client_runtime::agent_forward::{
    AgentRequest, AgentServing, Consent, QUESTION_ARMING, Question,
};
use phux_client_runtime::control::{ControlPlane, Status};
use phux_protocol::ids::ResourceId;

use super::{Desk, NoticeKind, Pending, inbox, worker};

enum Due {
    /// Ask the person.
    Ask(Question),
    /// Send the answer to request `id` on the connection `epoch` was.
    Reply {
        epoch: u64,
        id: u64,
        message: Vec<u8>,
    },
    /// The server refused the offer, and why.
    Refused(String),
}

/// One client's forwarded agent.
pub struct AgentForward {
    serving: Option<Arc<AgentServing>>,
    /// The connection last offered on.
    offered: Option<u64>,
    /// A refusal was said: a reconnect meets the same one again.
    said: bool,
    /// The question in hand, and since when the confirm bar has shown it.
    asking: Option<(Question, Option<Instant>)>,
    due: (Sender<Due>, Receiver<Due>),
    /// What the config would say, for tests.
    #[cfg(test)]
    test_offer: Option<Offer>,
}

impl Default for AgentForward {
    fn default() -> Self {
        Self {
            serving: None,
            offered: None,
            said: false,
            asking: None,
            due: channel(),
            #[cfg(test)]
            test_offer: None,
        }
    }
}

/// Whose agent to serve, from where, and which of its keys.
#[derive(Clone)]
struct Offer {
    host: String,
    agent: Result<PathBuf, String>,
    keys: Option<Vec<String>>,
}

impl Desk {
    /// What `config.toml` says to serve to this window's machine. Tests
    /// never read the real file.
    #[cfg(test)]
    fn agent_to_offer(&self) -> Option<Offer> {
        self.connection.agent.test_offer.clone()
    }

    /// What `config.toml` says to serve to this window's machine.
    #[cfg(not(test))]
    fn agent_to_offer(&self) -> Option<Offer> {
        // The quick terminal's window may be hidden, with nobody to ask.
        if self.quick.is_some() || self.native.is_some() {
            return None;
        }
        use phux_client_runtime::target;
        let current = target::RemoteTarget::parse(self.machines.current.as_deref()?).ok()?;
        let remotes = phux_config_core::loader::load_client().ok()?.remote;
        let entry = target::find_entry(&remotes, &current).filter(|entry| entry.forward_agent)?;
        Some(Offer {
            host: entry.name.clone(),
            agent: phux_client_runtime::agent_forward::local_agent(
                phux_client_runtime::connection::ssh_agent(),
            ),
            keys: entry.forward_agent_keys.clone(),
        })
    }

    /// Offer on a connection that is new, and do what the last answers and
    /// questions ask. True when the window changed.
    pub(super) fn pump_agent(&mut self, client: &Arc<Client>) -> bool {
        let mut changed = false;
        let epoch = self.connection.epoch;
        let agent = &mut self.connection.agent;
        if self.connection.status == Status::Attached && agent.offered != Some(epoch) {
            agent.offered = Some(epoch);
            // Its server stopped waiting when that connection ended.
            agent.asking = None;
            if let Some(offer) = self.agent_to_offer() {
                changed |= self.offer_agent(client, offer);
            }
        }
        let due: Vec<Due> = self.connection.agent.due.1.try_iter().collect();
        for due in due {
            match due {
                Due::Ask(question) => self.connection.agent.asking = Some((question, None)),
                Due::Reply {
                    epoch: asked,
                    id,
                    message,
                } if asked == epoch => {
                    let _ = client.with_control(|control| control.agent_reply(id, message));
                }
                Due::Reply { .. } => {}
                Due::Refused(why) => changed |= self.agent_notice(&why),
            }
        }
        changed | self.tend_agent_question()
    }

    fn offer_agent(&mut self, client: &Arc<Client>, offer: Offer) -> bool {
        let host = offer.host.clone();
        if !client.with_control(|control| control.forward_agent_supported()) {
            return self.agent_notice(&format!(
                "{host} does not forward ssh-agents: set [forward] agent = true there."
            ));
        }
        let agent = match offer.agent {
            Ok(agent) => agent,
            Err(why) => {
                return self.agent_notice(&format!("ssh-agent not forwarded to {host}: {why}"));
            }
        };
        let (sender, waker) = (self.connection.agent.due.0.clone(), self.waker.clone());
        let ask = {
            let (sender, waker) = (sender.clone(), waker.clone());
            move |question| {
                // A question with no window left to show it is dropped: a no.
                let _ = sender.send(Due::Ask(question));
                if let Some(waker) = &waker {
                    waker();
                }
            }
        };
        let serving = AgentServing::new(agent, offer.host, offer.keys, Consent::Ask(Box::new(ask)));
        self.connection.agent.serving = Some(Arc::new(serving));
        let Ok(call) = client.with_control(ControlPlane::agent_offer) else {
            return false;
        };
        worker::spawn(sender, waker, move |send| {
            if let Err(why) = pollster::block_on(call) {
                send(Due::Refused(format!(
                    "{host} refused your ssh-agent: {why}"
                )));
            }
        });
        false
    }

    /// Say why nothing is served, once per client.
    fn agent_notice(&mut self, why: &str) -> bool {
        let first = !std::mem::replace(&mut self.connection.agent.said, true);
        if first {
            self.say(NoticeKind::Info, why);
        }
        first
    }

    /// A pane asked: answer off the main thread.
    pub(super) fn agent_request(&mut self, request: AgentRequest) -> bool {
        let Some(serving) = &self.connection.agent.serving else {
            return false;
        };
        let (epoch, id) = (self.connection.epoch, request.id);
        let (sender, waker) = (self.connection.agent.due.0.clone(), self.waker.clone());
        serving.handle(request, move |answer| {
            let message = answer.message;
            let _ = sender.send(Due::Reply { epoch, id, message });
            if let Some(waker) = &waker {
                waker();
            }
        });
        false
    }

    /// What the person calls a pane, when anything names it.
    pub(crate) fn pane_called(&self, id: &ResourceId) -> Option<String> {
        let pane = self.connection.topology.as_ref()?.pane(id)?;
        phux_client_core::pane_label::pane_label(
            pane.name.as_deref(),
            pane.title.as_deref(),
            pane.foreground.as_deref(),
            pane.cwd.as_deref(),
        )
    }

    /// Keep the confirm bar and the question in hand together: whatever took
    /// it down refused it, an expired one goes, and one that waited for
    /// another question goes up.
    fn tend_agent_question(&mut self) -> bool {
        let up = matches!(self.pending, Some(Pending::SshSign(_)));
        let over = match &self.connection.agent.asking {
            Some((question, shown)) => {
                Instant::now() >= question.expires || (shown.is_some() && !up)
            }
            None => up,
        };
        if over {
            self.connection.agent.asking = None;
            if up {
                self.pending = None;
            }
            return up;
        }
        if self.pending.is_some() {
            return false;
        }
        let Some((question, _)) = &self.connection.agent.asking else {
            return false;
        };
        let called = question
            .pane
            .as_ref()
            .and_then(|pane| self.pane_called(pane));
        let text = format!(
            "{} wants to sign with your ssh key {}.",
            question.asker(called.as_deref()),
            question.key
        );
        self.pending = Some(Pending::SshSign(text));
        if let Some((_, shown)) = &mut self.connection.agent.asking {
            *shown = Some(Instant::now());
        }
        true
    }

    /// The confirm bar's keys while a signature is asked: the approval chord
    /// signs, Escape refuses, and typing does neither. `None` when none is.
    pub(super) fn agent_sign_key(
        &mut self,
        event: &rune::KeyEvent,
        tx: &mut rune::Tx,
    ) -> Option<bool> {
        if !matches!(self.pending, Some(Pending::SshSign(_))) {
            return None;
        }
        let macos = cfg!(any(target_os = "macos", target_os = "ios"));
        let chord = inbox::decision_chord(&event.mods, macos) && !tx.key_repeat();
        Some(match event.key {
            rune::Key::Escape => self.answer_agent_question(false),
            rune::Key::Enter if chord => self.answer_agent_question(true),
            _ => false,
        })
    }

    /// Answer the question up, unless it is too new to sign. True when done.
    pub(super) fn answer_agent_question(&mut self, yes: bool) -> bool {
        let Some((question, Some(shown))) = &self.connection.agent.asking else {
            return false;
        };
        if yes && shown.elapsed() < QUESTION_ARMING {
            return false;
        }
        question.answer(yes);
        self.connection.agent.asking = None;
        self.pending = None;
        true
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};
    use std::os::unix::net::UnixListener;
    use std::time::Duration;

    use phux_client_runtime::Runtime as ClientRuntime;
    use phux_client_runtime::control::ControlOptions;
    use phux_protocol::caps::ServerFeatureExt;
    use phux_protocol::forward::ForwardOp;
    use phux_protocol::wire::frame::{AgentEvent, Command, FrameKind};
    use rune::{Key, Tx};

    use super::*;
    use crate::launch::Resolved;
    use crate::wire_fixture::{hello_with, outbound};

    fn string(value: &[u8]) -> Vec<u8> {
        let mut out = u32::try_from(value.len()).unwrap().to_be_bytes().to_vec();
        out.extend(value);
        out
    }

    /// A local agent holding one key, `k`, under `me@here`.
    fn agent(dir: &tempfile::TempDir) -> PathBuf {
        let path = dir.path().join("agent.sock");
        let listener = UnixListener::bind(&path).unwrap();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut len = [0; 4];
                if stream.read_exact(&mut len).is_err() {
                    continue;
                }
                let mut message = vec![0; u32::from_be_bytes(len) as usize];
                stream.read_exact(&mut message).unwrap();
                let answer = if message[0] == 11 {
                    let mut list = vec![12, 0, 0, 0, 1];
                    list.extend(string(b"k"));
                    list.extend(string(b"me@here"));
                    list
                } else {
                    vec![14]
                };
                stream.write_all(&string(&answer)).unwrap();
            }
        });
        path
    }

    fn connected() -> (Arc<Client>, Desk) {
        let client = Arc::new(ClientRuntime::embedded(ControlOptions::default()));
        let mut desk = Desk::with_preferences(
            &Resolved::offline("agent"),
            None,
            rune::prefs::Prefs::memory(),
        );
        desk.connection.client = Some(client.clone());
        desk.connection.status = Status::Attached;
        connect(&client);
        (client, desk)
    }

    fn connect(client: &Client) {
        client.with_control(ControlPlane::connection_opened);
        client
            .feed(hello_with(1, &[ServerFeatureExt::ForwardAgent]))
            .unwrap();
        outbound(client);
    }

    fn ops(client: &Client) -> Vec<ForwardOp> {
        let sent = outbound(client).into_iter();
        sent.filter_map(|frame| match frame {
            FrameKind::Command {
                command: Command::Forward { op },
                ..
            } => Some(op),
            _ => None,
        })
        .collect()
    }

    fn offer(dir: &tempfile::TempDir) -> Offer {
        Offer {
            host: "mini".to_owned(),
            agent: Ok(agent(dir)),
            keys: None,
        }
    }

    fn sign(desk: &mut Desk, client: &Arc<Client>, id: u64) {
        let mut message = vec![13];
        message.extend(string(b"k"));
        message.extend(string(b"data"));
        message.extend([0; 4]);
        client
            .feed(FrameKind::Event {
                terminal: Some(ResourceId::local(4)),
                event: AgentEvent::SshAgentRequest { id, message },
                stamp: None,
            })
            .unwrap();
        for event in client.take_events() {
            desk.apply(client, event, &mut Tx::default());
        }
    }

    /// Pump until `done`.
    fn until(desk: &mut Desk, client: &Arc<Client>, done: impl Fn(&Desk) -> bool) {
        for _ in 0..500 {
            desk.pump_agent(client);
            if done(desk) {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("the agent never got there: {:?}", desk.pending);
    }

    /// The replies sent back, once there is one.
    fn replied(desk: &mut Desk, client: &Arc<Client>) -> Vec<ForwardOp> {
        for _ in 0..500 {
            desk.pump_agent(client);
            let ops = ops(client);
            if !ops.is_empty() {
                return ops;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("no reply");
    }

    fn reply(id: u64, message: u8) -> [ForwardOp; 1] {
        let message = vec![message];
        [ForwardOp::AgentReply { id, message }]
    }

    const fn asked(desk: &Desk) -> bool {
        matches!(desk.pending, Some(Pending::SshSign(_)))
    }

    #[test]
    fn a_signature_waits_in_the_confirm_bar_and_only_the_chord_signs() {
        let dir = tempfile::tempdir().unwrap();
        let (client, mut desk) = connected();
        desk.connection.agent.offered = Some(desk.connection.epoch);
        desk.offer_agent(&client, offer(&dir));
        assert_eq!(ops(&client), [ForwardOp::AgentOffer]);

        sign(&mut desk, &client, 7);
        until(&mut desk, &client, asked);
        let Some(Pending::SshSign(text)) = &desk.pending else {
            unreachable!()
        };
        for needle in ["pane @4 on mini", "SHA256:", "(me@here)"] {
            assert!(text.contains(needle), "{needle}: {text}");
        }
        let mut tx = Tx::default();
        let mut key = |desk: &mut Desk, key: Key, chord: bool| {
            let mut event = crate::keys::plain(key);
            if chord {
                event.mods.cmd = true;
                event.mods.shift = !cfg!(target_os = "macos");
            }
            desk.agent_sign_key(&event, &mut tx)
        };
        // Too soon for the chord, and typing never answers.
        assert_eq!(key(&mut desk, Key::Enter, true), Some(false));
        let shown = &mut desk.connection.agent.asking.as_mut().unwrap().1;
        *shown = shown.map(|at| at - QUESTION_ARMING);
        for typed in [Key::Enter, Key::Text("y".into()), Key::Text("a".into())] {
            assert_eq!(key(&mut desk, typed, false), Some(false));
        }
        assert!(asked(&desk));
        assert_eq!(key(&mut desk, Key::Enter, true), Some(true));
        assert_eq!(replied(&mut desk, &client), reply(7, 14));

        // Escape refuses; so does anything else that takes the bar down.
        sign(&mut desk, &client, 8);
        until(&mut desk, &client, asked);
        assert_eq!(key(&mut desk, Key::Escape, false), Some(true));
        assert_eq!(replied(&mut desk, &client), reply(8, 5));
        sign(&mut desk, &client, 9);
        until(&mut desk, &client, asked);
        desk.cancel_pending();
        assert_eq!(replied(&mut desk, &client), reply(9, 5));
    }

    /// A question waits for one already up, and is gone with its connection.
    #[test]
    fn a_question_waits_its_turn_and_ends_with_its_connection() {
        let dir = tempfile::tempdir().unwrap();
        let (client, mut desk) = connected();
        desk.connection.agent.offered = Some(desk.connection.epoch);
        desk.offer_agent(&client, offer(&dir));
        ops(&client);
        desk.pending = Some(Pending::Close(ResourceId::local(1)));
        sign(&mut desk, &client, 7);
        until(&mut desk, &client, |d| d.connection.agent.asking.is_some());
        assert_eq!(desk.pending, Some(Pending::Close(ResourceId::local(1))));
        desk.pending = None;
        until(&mut desk, &client, asked);

        desk.connection.epoch += 1;
        desk.pump_agent(&client);
        assert!(!asked(&desk) && desk.connection.agent.asking.is_none());
        // The answer to the old connection's request is not sent on this one.
        std::thread::sleep(Duration::from_millis(50));
        desk.pump_agent(&client);
        assert!(ops(&client).is_empty());
    }

    /// A link prompt and a signature take turns: the signature waits for the
    /// link's answer, and a link asked for meanwhile is declined unasked.
    #[test]
    fn a_link_prompt_and_a_signature_take_turns() {
        let dir = tempfile::tempdir().unwrap();
        let (client, mut desk) = connected();
        desk.connection.agent.offered = Some(desk.connection.epoch);
        desk.offer_agent(&client, offer(&dir));
        let pane = ResourceId::local(1);
        let link = "https://id.example/";
        assert!(desk.open_url_asked(pane.clone(), 3, link));
        sign(&mut desk, &client, 7);
        until(&mut desk, &client, |d| d.connection.agent.asking.is_some());
        assert!(matches!(desk.pending, Some(Pending::OpenUrl(_))));
        desk.cancel_pending();
        until(&mut desk, &client, asked);
        desk.connection.resources.link_declined = None;
        assert!(!desk.open_url_asked(pane, 4, link));
        assert!(asked(&desk), "still the signature");
    }

    /// Each connection offers once: the first, and the one after a reconnect.
    #[test]
    fn a_reconnect_offers_again() {
        let dir = tempfile::tempdir().unwrap();
        let (client, mut desk) = connected();
        desk.connection.agent.test_offer = Some(offer(&dir));
        for _ in 0..2 {
            desk.connection.epoch = client.connection_epoch();
            desk.pump_agent(&client);
            assert_eq!(ops(&client), [ForwardOp::AgentOffer]);
            desk.pump_agent(&client);
            assert!(ops(&client).is_empty());
            client.with_control(|control| control.connection_lost(None));
            connect(&client);
        }
    }

    /// Nothing to serve is said once and sends nothing.
    #[test]
    fn a_missing_agent_is_said_once() {
        let (client, mut desk) = connected();
        for first in [true, false] {
            let offer = Offer {
                host: "mini".to_owned(),
                agent: Err("no agent".to_owned()),
                keys: None,
            };
            assert_eq!(desk.offer_agent(&client, offer), first);
        }
        assert!(ops(&client).is_empty());
    }
}
