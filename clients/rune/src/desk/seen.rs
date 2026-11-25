//! Telling the server which panes are in front of the person
//! (`phux.in-view/v1`), so a look here clears their unseen marks everywhere.

use phux_client_runtime::Client;
use phux_client_runtime::control::Status;
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{FrameKind, RESOURCE_IN_VIEW_KEY, Scope};

use super::Desk;

impl Desk {
    /// Report what came into and left view since the last report. A pane's
    /// own server keeps its marks, so a satellite's is not reported.
    pub(super) fn report_in_view(&mut self, client: &Client) {
        if self.connection.status != Status::Attached {
            return;
        }
        let Some(topology) = &self.connection.topology else {
            return;
        };
        let listed = |id: &ResourceId| id.is_local() && topology.pane(id).is_some();
        // A sheet or a scene over the panes hides them, and a report reaches
        // every device, so it claims no more than is in front of the person.
        let shown: Vec<ResourceId> = if self.focused && self.foreground().is_none() {
            self.on_screen().into_iter().filter(listed).collect()
        } else {
            Vec::new()
        };
        let told = &self.connection.in_view;
        let came = shown.iter().filter(|id| !told.contains(*id));
        // A closed pane left every view with it.
        let left = (told.iter()).filter(|id| !shown.contains(id) && listed(id));
        let reports = (came.map(|id| (id, "true"))).chain(left.map(|id| (id, "false")));
        for (id, value) in reports {
            client.queue_frame(&FrameKind::SetMetadata {
                request_id: client.next_request_id(),
                scope: Scope::Resource(id.clone()),
                key: RESOURCE_IN_VIEW_KEY.to_owned(),
                value: value.as_bytes().to_vec(),
            });
        }
        self.connection.in_view = shown.into_iter().collect();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use phux_client_core::layout::LayoutNode;
    use phux_client_runtime::Runtime;
    use phux_client_runtime::control::{ControlOptions, ControlPlane};

    use super::*;
    use crate::launch::Resolved;

    fn reports(desk: &mut Desk, client: &Client) -> Vec<(ResourceId, bool)> {
        desk.report_in_view(client);
        (crate::wire_fixture::outbound(client).into_iter())
            .filter_map(|frame| match frame {
                FrameKind::SetMetadata {
                    scope: Scope::Resource(pane),
                    key,
                    value,
                    ..
                } if key == RESOURCE_IN_VIEW_KEY => Some((pane, value == b"true")),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn each_pane_is_reported_as_it_comes_into_and_leaves_view() {
        let client = Arc::new(Runtime::embedded(ControlOptions::default()));
        client.with_control(ControlPlane::connection_opened);
        client.feed(crate::wire_fixture::hello(1, false)).unwrap();
        let _ = client.take_outbound();
        let mut desk = Desk::with_preferences(
            &Resolved::offline("seen"),
            None,
            rune::prefs::Prefs::memory(),
        );
        let work = crate::test_support::server::session(1, "work", 1);
        let pane = |n| crate::test_support::server::pane(n, &work, "shell", None);
        desk.connection.topology = Some(crate::desk::topology(Vec::new(), vec![pane(1), pane(2)]));
        desk.connection.layout = Some(LayoutNode::Leaf(ResourceId::local(1)));
        desk.focused = true;
        assert!(reports(&mut desk, &client).is_empty(), "not attached");

        desk.connection.status = Status::Attached;
        let one = ResourceId::local(1);
        let two = ResourceId::local(2);
        assert_eq!(reports(&mut desk, &client), [(one.clone(), true)]);
        assert!(reports(&mut desk, &client).is_empty(), "told once");
        desk.connection.layout = Some(LayoutNode::Leaf(two.clone()));
        assert_eq!(
            reports(&mut desk, &client),
            [(two.clone(), true), (one, false)]
        );
        desk.settings_open = true;
        assert_eq!(reports(&mut desk, &client), [(two.clone(), false)]);
        desk.settings_open = false;
        assert_eq!(reports(&mut desk, &client), [(two.clone(), true)]);
        // A window in the background shows nothing to anyone.
        desk.focused = false;
        assert_eq!(reports(&mut desk, &client), [(two, false)]);
    }
}
