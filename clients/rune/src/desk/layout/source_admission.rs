//! Other panes use the same retained source admission as the focused pane.

use super::*;
use crate::test_support::terminal::Replica;
use phux_client_core::layout::SplitDir;

fn id(n: u32) -> ResourceId {
    ResourceId::local(n)
}
fn tree() -> LayoutNode {
    LayoutNode::Split {
        dir: SplitDir::Horizontal,
        ratio: 0.5,
        left: Box::new(LayoutNode::Leaf(id(1))),
        right: Box::new(LayoutNode::Leaf(id(2))),
    }
}

#[test]
fn a_delayed_other_pane_does_not_retarget_identical_ids_after_server_replacement() {
    for pending in [false, true] {
        let replica = if pending {
            Replica::pending(80, 24, 2)
        } else {
            Replica::new(80, 24, 2)
        };
        let client = &replica.client;
        let mut desk = replica.window();
        desk.show(id(1), "budget");
        desk.connection.layout = Some(tree());
        if pending {
            assert!(!desk.sync_views(client)); // Records the original pre-first-source scope.
        }
        replica.replace_server(2);
        let _ = client.take_outbound();
        assert!(!desk.sync_views(client));
        assert!(!desk.connection.others.contains_key(&id(2)));
        assert!(
            client.take_outbound().is_empty(),
            "no attach or detach of the replacement execution"
        );
    }
}

#[test]
fn an_other_pane_does_not_upgrade_a_captured_bootstrap_generation() {
    let replica = Replica::new(80, 24, 2);
    let client = &replica.client;
    let mut desk = replica.window();
    desk.show(id(1), "budget");
    desk.connection.layout = Some(tree());
    crate::wire_fixture::bootstrap(client, &id(2), [9; 2], [80, 24], b"replacement generation");
    assert!(!desk.sync_views(client));
    assert!(!desk.connection.others.contains_key(&id(2)));
    // The new publication is usable by fresh intent, not by this delayed pane.
    let source = client.view_source(&id(2), Some(&[1; 16]), 1).unwrap();
    let view = client.create_view(&source).unwrap();
    assert!(client.acquire_view(view).is_some());
    client.destroy_view(view).unwrap();
}

#[test]
fn releasing_all_window_views_keeps_sibling_views_and_execution_replicas() {
    let replica = Replica::new(80, 24, 2);
    let client = &replica.client;
    let mut desk = replica.window();
    desk.show(id(1), "budget");
    desk.connection.layout = Some(tree());
    assert!(desk.sync_views(client));
    let own = [
        desk.connection.shown.as_ref().unwrap().view.unwrap(),
        desk.connection.others[&id(2)].unwrap(),
    ];
    let _ = client.take_outbound();
    desk.release_connection_views();
    assert!(
        own.into_iter()
            .all(|view| client.view_generation(view).is_none())
    );
    assert!(
        replica
            .views
            .iter()
            .all(|view| client.acquire_view(*view).is_some())
    );
    assert!(
        client.take_outbound().is_empty(),
        "release never terminates execution or sibling subscriptions"
    );
}
