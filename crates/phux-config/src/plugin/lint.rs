//! Manifest entries that load fine and never do anything.
//!
//! The loader stays lenient so one stale entry does not take a whole plugin
//! down; `phux plugin validate` rejects these and the server warns about
//! them when it reads plugin hooks.

use std::fmt;

use super::{PluginManifest, PluginManifestEvent};
use crate::vocab;

/// A `[[events]]` hook whose `on` names no hook event
/// ([`vocab::HOOK_EVENTS`]), so it never fires.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeadPluginEvent {
    /// The event hook's plugin-local id.
    pub id: String,
    /// The `on` value that matches no hook event.
    pub on: String,
    /// The closest hook event, when one is close.
    pub suggestion: Option<&'static str>,
}

impl fmt::Display for DeadPluginEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "event `{}` listens for `{}`, which is not a hook event",
            self.id, self.on
        )?;
        if let Some(hit) = self.suggestion {
            write!(f, " (did you mean `{hit}`?)")?;
        }
        write!(f, "; it will never fire")
    }
}

/// The hook for one event entry, or `None` when its `on` is a hook event.
#[must_use]
pub fn dead_event(event: &PluginManifestEvent) -> Option<DeadPluginEvent> {
    if vocab::HOOK_EVENTS.contains(&event.on.as_str()) {
        return None;
    }
    Some(DeadPluginEvent {
        id: event.id.clone(),
        on: event.on.clone(),
        suggestion: vocab::did_you_mean(&event.on, vocab::HOOK_EVENTS),
    })
}

/// Every `[[events]]` entry in `manifest` that can never fire.
#[must_use]
pub fn dead_events(manifest: &PluginManifest) -> Vec<DeadPluginEvent> {
    manifest.events.iter().filter_map(dead_event).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(on: &str) -> PluginManifestEvent {
        PluginManifestEvent {
            id: "e".to_owned(),
            title: "E".to_owned(),
            description: None,
            on: on.to_owned(),
            platforms: None,
            command: vec!["true".to_owned()],
        }
    }

    #[test]
    fn hook_events_are_live_and_others_are_dead() {
        for on in vocab::HOOK_EVENTS {
            assert_eq!(dead_event(&event(on)), None, "{on}");
        }
        let dead = dead_event(&event("pane-exited")).expect("not a hook event");
        assert_eq!(dead.suggestion, Some("pane-exit"));
        assert_eq!(
            dead.to_string(),
            "event `e` listens for `pane-exited`, which is not a hook event \
             (did you mean `pane-exit`?); it will never fire"
        );
        let far = dead_event(&event("session.changed")).expect("not a hook event");
        assert_eq!(far.suggestion, None);
        assert!(!far.to_string().contains("did you mean"));
    }
}
