//! A native menu's answer belongs to the row and source that offered it,
//! never whichever menu happens to be open when UIKit calls back.
use std::sync::atomic::{AtomicU64, Ordering};

use phux_client_runtime::control::Status;
use rune::Runtime;
use serde_json::{Value, json};

use super::{Desk, Held, Page, Verb};
use crate::desk::input::Modal;

static NEXT_REQUEST: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone)]
pub(crate) struct Request {
    pub(crate) id: u64,
    held: Held,
    source: String,
    verbs: Vec<Verb>,
}

/// Include the live transport epoch too: a callback can precede Desk's poll.
pub(super) fn source(d: &Desk, held: &Held) -> Option<String> {
    if !d.native.as_ref()?.active
        || d.modal() != Some(Modal::PhoneNavigation)
        || d.phone_page() != Some(Page::Sessions)
    {
        return None;
    }
    let owner = owner(d, held)?;
    let client = d.connection.client.as_ref()?;
    client.with_control(|control| {
        if control.status() != Status::Attached {
            return None;
        }
        let key = match held {
            Held::Pane(id, _) => control.organization().resource_by_live(id).map(|r| r.key),
            Held::Session(_) => None,
        };
        Some(format!(
            "{}:{}:{}:{:?}:{}:{:?}:{held:?}:{owner}:{key:?}",
            d.link.server,
            d.connection.namespace(),
            d.connection.epoch,
            std::sync::Arc::as_ptr(client),
            control.connection_epoch(),
            control.server().map(|server| &server.id),
        ))
    })
}

fn owner(d: &Desk, held: &Held) -> Option<String> {
    let topology = d.connection.topology.as_ref()?;
    match held {
        Held::Session(id) => {
            let session = topology.sessions.iter().find(|s| s.id == *id)?;
            Some(format!("{:?}:{}", session.key, session.name))
        }
        Held::Pane(id, name) => {
            let resource = d.addressable_of(id)?;
            if &resource.session != name {
                return None;
            }
            // A hub's satellite has no local session id. Its qualified host,
            // resource id/key and kind still belong to this source.
            let key = topology
                .sessions
                .iter()
                .find(|s| s.id == resource.session_id)
                .and_then(|s| s.key);
            Some(format!(
                "{}:{}:{:?}:{key:?}",
                resource.session_id, resource.session, resource.kind
            ))
        }
    }
}

impl Request {
    fn valid(&self, d: &Desk) -> bool {
        source(d, &self.held).as_ref() == Some(&self.source)
    }
}

/// Retire a source that changed even if no callback has arrived yet.
pub(crate) fn current(d: &mut Desk) -> Option<u64> {
    let request = d.native.as_ref()?.held_menu.as_ref()?;
    if !request.valid(d) {
        d.close_phone_menu();
        return None;
    }
    Some(request.id)
}

pub(crate) fn export(rt: &mut Runtime<Desk>) -> Value {
    current(&mut rt.state);
    let Some(menu) = rt.state.phone.as_ref().and_then(|p| p.menu.as_ref()) else {
        return Value::Null;
    };
    if menu.asked || menu.source.is_none() || menu.source != source(&rt.state, &menu.held) {
        return Value::Null;
    }
    let Some((title, subtitle, verbs)) = crate::ui::phone_menu(&rt.state) else {
        return Value::Null;
    };
    let Ok(id) =
        NEXT_REQUEST.try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
    else {
        return Value::Null;
    };
    let [x, y] = menu.at;
    let rect = rt
        .presented(&menu.row)
        .map_or([x, y, 1.0, 1.0], |(rect, _)| rect);
    let request = Request {
        id,
        held: menu.held.clone(),
        source: menu.source.clone().expect("checked source"),
        verbs: verbs.iter().map(|(verb, _, _)| *verb).collect(),
    };
    let items: Vec<Value> = verbs
        .into_iter()
        .map(|(verb, title, symbol)| {
            json!({"id": verb.id(), "title": title, "symbol": symbol,
                "destructive": verb == Verb::Close})
        })
        .collect();
    rt.state.native.as_mut().expect("native source").held_menu = Some(request);
    rt.state
        .phone
        .as_mut()
        .unwrap()
        .menu
        .as_mut()
        .unwrap()
        .asked = true;
    json!({"request": id, "rect": rect, "title": title, "subtitle": subtitle, "items": items})
}

/// Consume only an offered action from this exact request and original source.
/// A stale answer must not consume a newer request, even on the same row.
pub fn menu_pick(rt: &mut Runtime<Desk>, request: u64, id: &str) -> bool {
    let Some(verb) = Verb::from_id(id) else {
        return false;
    };
    let Some(asked) = rt.state.native.as_ref().and_then(|n| n.held_menu.as_ref()) else {
        return false;
    };
    if asked.id != request || !asked.verbs.contains(&verb) || !asked.valid(&rt.state) {
        return false;
    }
    rt.update(|d, tx| {
        let asked = d.native.as_mut().unwrap().held_menu.take().unwrap();
        d.close_phone_menu();
        d.phone_held_pick(asked.held, verb);
        tx.instant();
    });
    true
}

/// Visual dismissal is not proof of cancellation: UIKit does not guarantee
/// action/disappearance callback ordering. Keep at most this one correlated
/// answer until selection or explicit navigation/source/lifecycle invalidation.
pub fn menu_dismiss(rt: &mut Runtime<Desk>, request: u64) {
    if current(&mut rt.state) != Some(request) {
        return;
    }
    rt.update(|d, tx| {
        d.phone.as_mut().unwrap().menu = None;
        tx.instant();
    });
}
