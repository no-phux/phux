//! Always-visible session actions, separate from the header's select/drag hit.

use phux_protocol::ids::SessionKey;
use rune::{El, el};

use super::super::{Ink, icon_press, icons};

/// A fixed close slot for local sessions. Satellites have no supported
/// session mutation on this connection, so expose no misleading button.
pub(super) fn session_controls(key: Option<SessionKey>, name: &str, ink: &Ink) -> El {
    let Some(key) = key else { return el() };
    let label = format!("Close session {name}");
    // Its own press handler owns the hit, including pointer moves: Rune
    // does not forward a child button's gesture to its draggable parent.
    icon_press(
        &icons::CLOSE,
        "Close session",
        format!("session-close-{key}"),
        ink,
        move |d| d.close_session_key(key),
    )
    .size(22.0, 22.0)
    .radius(5.0)
    .label(label.clone())
    .help(label)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desk::{Desk, Pending};
    use phux_client_runtime::control::Topology;
    use phux_protocol::ids::{ResourceId, SessionId, WindowId};
    use phux_protocol::wire::info::{SessionInfo, SessionSnapshot};
    use rune::{Cx, Env, Runtime, row};

    fn key() -> SessionKey {
        SessionKey::new([2; 16]).unwrap()
    }

    fn header(_: &Desk, _: &mut Cx<Desk>) -> El {
        let ink = crate::ui::ink(true, rune::oklch(0.6, 0.15, 250.0), None);
        let header = row()
            .size(200.0, 30.0)
            .items_center()
            .on_press(|d: &mut Desk| d.show_session("empty"))
            .on_double_press(|d: &mut Desk| d.rename_session(key(), "empty"))
            .child(session_controls(Some(key()), "cached label", &ink));
        super::super::drag::reorderable(header, Some(key()), false, &ink)
    }

    #[test]
    fn a_close_button_owns_its_press_in_a_selectable_draggable_header() {
        let (client, _) = crate::test_support::input_fixture();
        let mut desk = crate::test_support::window();
        desk.connection.client = Some(client.clone());
        let snapshot =
            SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(1))
                .with_sessions(vec![
                    SessionInfo::new(SessionId::new(1), "budget"),
                    SessionInfo::new(SessionId::new(2), "empty"),
                ]);
        let request_id = client.refresh_topology().unwrap();
        client
            .feed(phux_protocol::wire::frame::FrameKind::CommandResult {
                request_id,
                result: phux_protocol::wire::frame::CommandResult::OkWith(
                    phux_protocol::wire::frame::CommandValue::State(snapshot.clone()),
                ),
            })
            .unwrap();
        desk.connection.topology = Some(Topology::from_snapshot(&snapshot));
        desk.connection.topology.as_mut().unwrap().sessions[1].key = Some(key());
        let mut rt = Runtime::new(desk, header, (240.0, 80.0), Env::new(true, 1.0));
        rt.frame(0.0);
        let button = format!("session-close-{}", key());
        let (x, y) = rt.find(&button).expect("an always-visible close control");
        let rect = rt.presented(&button).unwrap().0;
        assert_eq!((rect[2], rect[3]), (22.0, 22.0));
        let _ = client.take_outbound();

        rt.pointer_down(x, y);
        rt.pointer_move(x + 6.0, y + 6.0);
        rt.frame(0.0);
        assert!(
            rt.state.connection.reorder.held.is_none(),
            "the parent never starts dragging"
        );
        assert_eq!(client.selected_session(), Some(1));
        rt.pointer_up(x + 6.0, y + 6.0);
        rt.frame(0.0);
        rt.update(|d, _| d.cancel_pending());

        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.0);
        assert_eq!(
            rt.state.pending,
            Some(Pending::CloseSession(key(), "empty".into())),
            "the question uses topology, not a stale rendered name",
        );
        assert_eq!(
            client.selected_session(),
            Some(1),
            "the header never selects"
        );
        assert!(
            rt.state.connection.tabs.renaming.is_none(),
            "the header never double-presses"
        );
        rt.update(|d, _| d.cancel_pending());
        assert!(
            client.take_outbound().is_empty(),
            "press and cancel send no commands"
        );

        assert!(rt.accessibility_action(&button, rune::accesskit::Action::Click, None));
        rt.frame(0.0);
        assert_eq!(
            rt.state.pending,
            Some(Pending::CloseSession(key(), "empty".into()))
        );
    }

    #[test]
    fn a_satellite_exposes_no_close_button() {
        fn satellite(_: &Desk, _: &mut Cx<Desk>) -> El {
            let ink = crate::ui::ink(true, rune::oklch(0.6, 0.15, 250.0), None);
            session_controls(None, "satellite", &ink)
        }
        let desk = crate::test_support::window();
        let mut rt = Runtime::new(desk, satellite, (80.0, 30.0), Env::new(true, 1.0));
        rt.frame(0.0);
        assert!(!rt.accessibility_action(
            &format!("session-close-{}", key()),
            rune::accesskit::Action::Click,
            None,
        ));
        rt.pointer_down(10.0, 10.0);
        rt.pointer_up(10.0, 10.0);
        rt.frame(0.0);
        assert!(rt.state.pending.is_none());
    }
}
