//! One [`Terminal`] entity per placement. Identity is kept until the view is replaced.

use std::collections::HashMap;
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{Context, Entity, Subscription, Window};
use phux_client_runtime::{Client, ViewId};
use phux_protocol::ResourceId;

use crate::terminal::{HostAction, Settings, Terminal, TerminalEvent};

use super::desk::Desk;

/// Retained terminal views for one window.
#[derive(Default)]
pub struct Panes {
    by_placement: HashMap<String, Slot>,
}

struct Slot {
    view: ViewId,
    terminal: ResourceId,
    entity: Entity<Terminal>,
    /// Click-to-focus and file drops for this entity. Dropped with the slot, so
    /// a rebound view or reconnect subscribes its new entity.
    _events: Subscription,
}

impl Panes {
    /// The entity for this placement. The same view id is kept; a rebound view replaces it.
    #[allow(
        clippy::too_many_arguments,
        reason = "identity, settings and the GPUI contexts it binds"
    )]
    pub fn entity_for(
        &mut self,
        placement_id: &str,
        terminal: &ResourceId,
        view: ViewId,
        settings: Settings,
        client: &Arc<Client>,
        window: &mut Window,
        cx: &mut Context<Desk>,
    ) -> Entity<Terminal> {
        if let Some(slot) = self.by_placement.get(placement_id)
            && slot.view == view
            && &slot.terminal == terminal
        {
            let entity = slot.entity.clone();
            entity.update(cx, |terminal, cx| {
                terminal.configure(settings, window, cx);
            });
            return entity;
        }
        let entity = cx
            .new(|cx| Terminal::new(client.clone(), view, terminal.clone(), settings, window, cx));
        let placement = placement_id.to_owned();
        let events = cx.subscribe_in(
            &entity,
            window,
            move |desk: &mut Desk, _, event: &TerminalEvent, window, cx| {
                desk.on_terminal_event(&placement, event, window, cx);
            },
        );
        self.by_placement.insert(
            placement_id.to_owned(),
            Slot {
                view,
                terminal: terminal.clone(),
                entity: entity.clone(),
                _events: events,
            },
        );
        entity
    }

    pub fn request(
        &self,
        placement_id: &str,
        action: HostAction,
        cx: &mut Context<Desk>,
    ) -> Option<crate::input::InputNotice> {
        self.by_placement.get(placement_id).and_then(|slot| {
            slot.entity
                .update(cx, |terminal, cx| terminal.request(action, cx))
        })
    }

    pub fn focus(&self, placement_id: &str, window: &mut Window, cx: &mut Context<Desk>) {
        if let Some(slot) = self.by_placement.get(placement_id) {
            slot.entity.update(cx, |terminal, cx| {
                window.focus(terminal.focus_handle(), cx);
            });
        }
    }

    pub fn get(&self, placement_id: &str) -> Option<Entity<Terminal>> {
        self.by_placement
            .get(placement_id)
            .map(|slot| slot.entity.clone())
    }

    /// Ask every placed terminal to paint. The desk calls this once when an
    /// attach lands, so the retained frame is replaced without waiting for
    /// another output byte.
    pub fn notify_all(&self, cx: &mut Context<Desk>) {
        for slot in self.by_placement.values() {
            slot.entity.update(cx, |_, cx| cx.notify());
        }
    }

    /// Ask each placed view of these terminals to paint. The desk chrome stays
    /// as it is: the views are notified by id, so the desk, which reads each
    /// terminal entity as it configures it, is not drawn again.
    pub fn notify_terminals(&self, terminals: &[ResourceId], cx: &mut Context<Desk>) {
        for slot in self.by_placement.values() {
            if terminals.iter().any(|id| id == &slot.terminal) {
                let app: &mut gpui::App = cx;
                app.notify(slot.entity.entity_id());
            }
        }
    }

    pub fn retain(&mut self, live: &[String]) {
        self.by_placement
            .retain(|id, _| live.iter().any(|keep| keep == id));
    }

    pub fn clear(&mut self) {
        self.by_placement.clear();
    }
}
