//! The audit log of viewers' document input (`SEND_DOCUMENT_INPUT`): who
//! sent what kind of intent to which document, and what became of it. Text
//! is never logged, only its length.
//!
//! Typing would write a line per keystroke, so lines group into bursts: a
//! burst is one sender's intents to one resource with one outcome, until
//! [`QUIET`] passes without another. Its first intent is logged at once; a
//! summary of the rest follows when it ends.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{CommandResult, DocumentInput, OnBehalfOf};

use crate::state::{ClientId, ServerState};

/// The pause that ends a burst.
const QUIET: Duration = Duration::from_secs(2);

static BURSTS: Mutex<Bursts> = Mutex::new(Bursts(None));

/// Log `intent` from `client` to `resource`, which ended in `result`.
/// `relayed` is the hub's claim of who it sent it for, if any.
pub(crate) fn record(
    state: &ServerState,
    client: ClientId,
    relayed: Option<&OnBehalfOf>,
    resource: &ResourceId,
    intent: &DocumentInput,
    result: &CommandResult,
) {
    let outcome = match result {
        CommandResult::Error { code, .. } => format!("refused: {code:?}"),
        _ => "relayed".to_owned(),
    };
    let key = Key {
        who: who(state, client, relayed),
        resource: resource.to_string(),
        outcome,
    };
    let (kind, chars) = kind(intent);
    let opens = BURSTS.lock().unwrap_or_else(PoisonError::into_inner).note(
        &key,
        kind,
        chars,
        Instant::now(),
    );
    if !opens {
        return;
    }
    let Key {
        who,
        resource,
        outcome,
    } = &key;
    tracing::info!(who, resource, kind, chars, outcome, "document input");
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        // No clock to end the burst on: every intent gets its own line.
        BURSTS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .settle(&key, None);
        return;
    };
    runtime.spawn(async move {
        loop {
            tokio::time::sleep(QUIET).await;
            let settled = BURSTS
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .settle(&key, Some(Instant::now()));
            let Some(burst) = settled else { continue };
            if burst.more > 0 {
                let Key {
                    who,
                    resource,
                    outcome,
                } = &key;
                let (more, chars, kinds) = (burst.more, burst.chars, format!("{:?}", burst.kinds));
                tracing::info!(
                    who,
                    resource,
                    more,
                    chars,
                    kinds,
                    outcome,
                    "document input burst ended"
                );
            }
            return;
        }
    });
}

/// `relayed` when `client`'s link may vouch for who it relays for
/// (`docs/authority.md#relayed-identity`); otherwise the claim is ignored.
pub(super) fn believed<'a>(
    state: &ServerState,
    client: ClientId,
    relayed: Option<&'a OnBehalfOf>,
) -> Option<&'a OnBehalfOf> {
    relayed.filter(|_| crate::policy::filter::vouches_for_relay(state, client))
}

/// The name `input_activity` gives the sender: a believed relay claim's
/// client name (its credential when it has none), else the connection's.
pub(super) fn origin(
    state: &ServerState,
    client: ClientId,
    relayed: Option<&OnBehalfOf>,
) -> String {
    believed(state, client, relayed).map_or_else(
        || state.input_origin(client),
        |relayed| {
            if relayed.name.is_empty() {
                relayed.credential.clone()
            } else {
                relayed.name.clone()
            }
        },
    )
}

/// The sender as the log names it: its credential id, `owner` on the
/// owner's socket, else `anonymous`; through a believed relay,
/// `via hub <that>, on behalf of <credential> (<name>)`.
fn who(state: &ServerState, client: ClientId, relayed: Option<&OnBehalfOf>) -> String {
    let link = state.input_sender(client);
    match believed(state, client, relayed) {
        Some(relayed) => format!(
            "via hub {link}, on behalf of {} ({:?})",
            relayed.credential, relayed.name
        ),
        None => link,
    }
}

/// The intent's kind and the length of any text it carries.
fn kind(intent: &DocumentInput) -> (&'static str, usize) {
    match intent {
        DocumentInput::Navigate { url } => ("navigate", url.chars().count()),
        DocumentInput::Click { .. } => ("click", 0),
        DocumentInput::Type { text } => ("type", text.chars().count()),
        DocumentInput::Key { .. } => ("key", 0),
        DocumentInput::Scroll { .. } => ("scroll", 0),
        DocumentInput::Paste { text, .. } => ("paste", text.chars().count()),
        DocumentInput::CopyClipboard { .. } => ("copy_clipboard", 0),
        DocumentInput::ClipboardText { text, .. } => (
            "clipboard_text",
            text.as_ref().map_or(0, |text| text.chars().count()),
        ),
        DocumentInput::Activate { .. } => ("activate", 0),
        DocumentInput::Drag { .. } => ("drag", 0),
        DocumentInput::SelectAll => ("select_all", 0),
        DocumentInput::CopySelection { cut: false, .. } => ("copy_selection", 0),
        DocumentInput::CopySelection { cut: true, .. } => ("cut_selection", 0),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Key {
    who: String,
    resource: String,
    outcome: String,
}

/// The intents after a burst's first.
#[derive(Debug, PartialEq, Eq)]
struct Burst {
    last: Instant,
    more: u32,
    chars: usize,
    kinds: BTreeMap<&'static str, u32>,
}

/// The open bursts (`None` until the first).
struct Bursts(Option<HashMap<Key, Burst>>);

impl Bursts {
    /// Note one intent; `true` when it opens a burst.
    fn note(&mut self, key: &Key, kind: &'static str, chars: usize, now: Instant) -> bool {
        let bursts = self.0.get_or_insert_default();
        if let Some(burst) = bursts.get_mut(key) {
            burst.last = now;
            burst.more += 1;
            burst.chars += chars;
            *burst.kinds.entry(kind).or_default() += 1;
            return false;
        }
        let burst = Burst {
            last: now,
            more: 0,
            chars: 0,
            kinds: BTreeMap::new(),
        };
        bursts.insert(key.clone(), burst);
        true
    }

    /// End burst `key` if it has been quiet for [`QUIET`] at `now` (or
    /// unconditionally without a clock), handing back what followed its
    /// first intent.
    fn settle(&mut self, key: &Key, now: Option<Instant>) -> Option<Burst> {
        let bursts = self.0.as_mut()?;
        let quiet = bursts
            .get(key)
            .is_some_and(|b| now.is_none_or(|now| now.duration_since(b.last) >= QUIET));
        quiet.then(|| bursts.remove(key)).flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(outcome: &str) -> Key {
        Key {
            who: "owner".into(),
            resource: "@3".into(),
            outcome: outcome.into(),
        }
    }

    #[test]
    fn typing_is_one_line_then_one_summary() {
        let mut bursts = Bursts(None);
        let t0 = Instant::now();
        let at = |ms| t0 + Duration::from_millis(ms);
        assert!(bursts.note(&key("relayed"), "type", 1, at(0)));
        assert!(!bursts.note(&key("relayed"), "type", 5, at(300)));
        assert!(!bursts.note(&key("relayed"), "key", 0, at(600)));
        // Another outcome is its own burst.
        assert!(bursts.note(&key("refused"), "click", 0, at(700)));
        // Not quiet yet.
        assert_eq!(bursts.settle(&key("relayed"), Some(at(2000))), None);
        let ended = bursts.settle(&key("relayed"), Some(at(2600))).unwrap();
        assert_eq!((ended.more, ended.chars), (2, 5));
        assert_eq!(ended.kinds, BTreeMap::from([("key", 1), ("type", 1)]));
        // The next intent opens a fresh burst.
        assert!(bursts.note(&key("relayed"), "type", 3, at(2700)));
        assert!(bursts.settle(&key("refused"), None).is_some());
    }

    /// A hub's claim of who it relays for names the sender only from a
    /// link that may vouch for it; anything else keeps the link's own name.
    #[test]
    fn a_relay_claim_is_believed_only_from_a_full_control_link() {
        use crate::policy::ConnectionGrant;
        use phux_protocol::policy::{PeerIdentity, TransportType};
        use phux_protocol::scope::TerminalScopeSet;
        let scoped = |scopes: &[&str]| {
            let set = TerminalScopeSet::parse_all(scopes).unwrap();
            ConnectionGrant::scoped(set, Some("sha256:link".to_owned())).unwrap()
        };
        let mut state = ServerState::new();
        let (hub, bot, owner) = (ClientId(1), ClientId(2), ClientId(3));
        state.set_connection_grant(hub, scoped(&[crate::workload::PAIRED_DEVICE_SCOPE]));
        state.set_connection_grant(bot, scoped(&["input@display"]));
        state.set_connection_grant(owner, ConnectionGrant::owner());
        state.set_peer_identity(
            owner,
            PeerIdentity {
                uid: 0,
                pid: None,
                exe_path: None,
                mcp_host_key: None,
                transport: TransportType::UnixSocket,
                source_addr: None,
            },
        );
        state.set_client_name(owner, "Rune".to_owned());
        let claim = OnBehalfOf {
            credential: "sha256:phone".to_owned(),
            name: "iPhone".to_owned(),
        };
        let claim = Some(&claim);

        assert_eq!(origin(&state, hub, claim), "iPhone");
        assert_eq!(
            who(&state, hub, claim),
            r#"via hub anonymous, on behalf of sha256:phone ("iPhone")"#
        );
        assert_eq!(
            origin(&state, owner, claim),
            "iPhone",
            "an ssh or uplink hub"
        );
        // An automation key's claim is ignored.
        assert_eq!(origin(&state, bot, claim), "client:2");
        assert_eq!(who(&state, bot, claim), "anonymous");
        // No claim: the connection itself.
        assert_eq!(who(&state, owner, None), "owner");

        // What a hub stamps: its caller's credential and name, cut short.
        let stamp = state.relayed_for(owner);
        assert_eq!(
            (stamp.credential.as_str(), stamp.name.as_str()),
            ("owner", "Rune")
        );
        state.set_client_name(owner, "é".repeat(200));
        assert_eq!(state.relayed_for(owner).name.len(), 254);
    }

    #[test]
    fn text_is_measured_never_kept() {
        let typed = DocumentInput::Type {
            text: "pässwörd".into(),
        };
        assert_eq!(kind(&typed), ("type", 8));
        assert_eq!(kind(&DocumentInput::Click { x: 1, y: 2 }), ("click", 0));
    }
}
