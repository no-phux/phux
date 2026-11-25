//! The server's unseen marks (`phux.unseen/v1`) over this window's own: what
//! any device has looked at is seen here too, and what happened while Rune was
//! closed still shows. A pane whose server keeps none keeps the local rules.

use phux_client_core::agent_record::AgentState;
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::PaneUnseen;

use super::{Activities, Activity};

impl Activity {
    /// Someone looked.
    pub(super) fn see(&mut self) {
        self.bell = false;
        self.finished = None;
        self.done_unseen = false;
        self.unseen = self.unseen.map(|_| PaneUnseen::Seen);
    }

    /// An agent's finish is held for you.
    pub(super) fn held_done(&self) -> bool {
        match self.unseen {
            None => self.done_unseen,
            Some(mark) => {
                mark == PaneUnseen::Finished
                    && (self.agent.as_ref()).is_some_and(|a| a.state == AgentState::Done)
            }
        }
    }

    /// A bell is held for you.
    pub(super) fn held_bell(&self) -> bool {
        self.unseen
            .map_or(self.bell, |mark| mark == PaneUnseen::Bell)
    }

    /// A long command's result held for you. The server marks only its
    /// output, so the result itself is this window's.
    ///
    /// ponytail: one that ended in view of another device shows here once the
    /// pane prints again; a server-side command mark would be exact.
    pub(super) fn held_finish(&self) -> Option<Option<i32>> {
        self.finished
            .filter(|_| self.unseen.is_none_or(|mark| mark > PaneUnseen::Seen))
    }
}

impl Activities {
    /// The server's mark for `id`. True when its row changed.
    pub fn marked(&mut self, id: &ResourceId, mark: Option<PaneUnseen>) -> bool {
        let held = self.terminals.get(id);
        if held.and_then(|entry| entry.unseen) == mark {
            return false;
        }
        let before = held.map(Activity::face);
        let entry = self.terminals.entry(id.clone()).or_default();
        entry.unseen = mark;
        if mark == Some(PaneUnseen::Seen) {
            entry.see();
        }
        self.settled(id, before)
    }
}

#[cfg(test)]
mod tests {
    use phux_client_runtime::control::Event;

    use super::super::Mark;
    use super::*;

    #[test]
    fn the_servers_mark_overrules_this_windows_own() {
        let mut a = Activities::default();
        let id = ResourceId::local(1);
        let bell = Event::Bell {
            terminal_id: id.clone(),
        };
        a.apply(&bell, &[]);
        assert_eq!(a.mark(&id), Mark::Bell, "no server marks: the local rule");
        assert!(!a.marked(&id, Some(PaneUnseen::Bell)), "the same row");
        assert!(a.marked(&id, Some(PaneUnseen::Seen)), "seen elsewhere");
        assert_eq!(a.mark(&id), Mark::None);
        a.apply(&bell, &[]);
        assert_eq!(a.mark(&id), Mark::None, "in view on another device");
        assert!(a.marked(&id, Some(PaneUnseen::Bell)));
        assert_eq!(a.mark(&id), Mark::Bell);
        a.seen(&id);
        assert_eq!(a.mark(&id), Mark::None, "seen here before the answer");

        let done = |state: &str| Event::AgentMetadata {
            terminal_id: id.clone(),
            value: Some(format!(r#"{{"name":"claude","state":"{state}"}}"#).into_bytes()),
        };
        a.apply(&done("done"), &[]);
        assert_eq!(a.mark(&id), Mark::Idle, "finished and already seen");
        a.marked(&id, Some(PaneUnseen::Finished));
        assert_eq!(a.mark(&id), Mark::Done);
        a.apply(&done("working"), &[]);
        assert_eq!(a.mark(&id), Mark::Working, "its next turn");
    }
}
