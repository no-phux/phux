//! One [`Terminal`] entity per placement. Identity is kept until the view is replaced.

use std::collections::HashMap;
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{Context, Entity, Window};
use phux_client_runtime::{Client, ViewId};
use phux_protocol::ResourceId;

use crate::terminal::{HostAction, Settings, Terminal};

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
}

impl Panes {
    /// The entity for this placement. The same view id is kept; a rebound view replaces it.
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
        if let Some(slot) = self.by_placement.get(placement_id) {
            if slot.view == view && &slot.terminal == terminal {
                let entity = slot.entity.clone();
                entity.update(cx, |terminal, cx| {
                    terminal.configure(settings, window, cx);
                });
                return entity;
            }
        }
        let entity = cx.new(|cx| {
            Terminal::new(client.clone(), view, terminal.clone(), settings, window, cx)
        });
        self.by_placement.insert(
            placement_id.to_owned(),
            Slot { view, terminal: terminal.clone(), entity: entity.clone() },
        );
        entity
    }

    pub fn request(&self, placement_id: &str, action: HostAction, cx: &mut Context<Desk>) {
        if let Some(slot) = self.by_placement.get(placement_id) {
            slot.entity.update(cx, |terminal, cx| terminal.request(action, cx));
        }
    }

    pub fn focus(&self, placement_id: &str, window: &mut Window, cx: &mut Context<Desk>) {
        if let Some(slot) = self.by_placement.get(placement_id) {
            slot.entity.update(cx, |terminal, cx| {
                window.focus(terminal.focus_handle(), cx);
            });
        }
    }

    pub fn get(&self, placement_id: &str) -> Option<Entity<Terminal>> {
        self.by_placement.get(placement_id).map(|slot| slot.entity.clone())
    }

    pub fn retain(&mut self, live: &[String]) {
        self.by_placement.retain(|id, _| live.iter().any(|keep| keep == id));
    }

    pub fn clear(&mut self) {
        self.by_placement.clear();
    }
}
