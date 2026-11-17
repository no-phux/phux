//! A replaced connection must not replace window-local Settings or phone state.

use super::*;
use crate::launch::Resolved;

#[test]
fn reconnect_keeps_settings_navigation_search_return_context_and_phone_form() {
    let mut desk = Desk::new(&Resolved::offline("settings reconnect")).with_phone(true);
    desk.settings_open = true;
    desk.settings_category(crate::desk::SettingsCategory::Developer);
    desk.settings_ui.query = rune::TextEdit::new("logs");
    desk.settings_ui.query.focused = true;
    desk.settings_ui.return_from_themes = true;
    desk.renew();
    assert!(desk.phone.is_some());
    assert!(desk.settings_open);
    assert_eq!(
        desk.settings_ui.category,
        Some(crate::desk::SettingsCategory::Developer)
    );
    assert_eq!(desk.settings_ui.query.text, "logs");
    assert!(desk.settings_search_focused());
    assert!(desk.settings_ui.return_from_themes);
}

#[test]
fn replacing_a_machine_keeps_phone_preferences_not_old_panes_or_latches() {
    // Embedded launch never locates/spawns an installed CLI on the redial.
    let mut desk = Desk::new(&Resolved::embedded(None)).with_phone(true);
    desk.machines.current = Some("previous".into());
    let phone = desk.phone.as_mut().unwrap();
    phone.keyboard = false;
    phone.symbols = true;
    phone.ctrl = true;
    phone.picker = true;
    phone.tree = Some(phux_client_core::layout::LayoutNode::Leaf(
        phux_protocol::ids::ResourceId::local(1),
    ));
    desk.switch_machine(None);
    assert!(desk.machines.current.is_none());
    let phone = desk
        .phone
        .as_ref()
        .expect("machine replacement stays a phone");
    assert!(!phone.keyboard);
    assert!(phone.symbols);
    assert!(!phone.ctrl);
    assert!(!phone.picker);
    assert!(phone.tree.is_none());
    assert!(!phone.navigating());
}

#[test]
fn replacing_a_runtime_does_not_reload_window_preferences_or_geometry() {
    for policy in [Replacement::Retry, Replacement::Machine] {
        let mut desk = crate::test_support::window().with_phone(true);
        desk.font_size = 19.0;
        desk.font_family = "test font".into();
        desk.scale.set(1.5);
        desk.window_w = 777.0;
        desk.sidebar_open = false;
        desk.sidebar_peek = true;
        desk.settings.glass = 73;
        let waker: rune::Waker = std::sync::Arc::new(|| {});
        desk.waker = Some(waker.clone());
        desk.settings_open = true;
        desk.settings_ui.query = rune::TextEdit::new("retained search");
        desk.palette_query = rune::TextEdit::new("retained palette");
        desk.pending = Some(crate::desk::Pending::Close(
            phux_protocol::ids::ResourceId::local(1),
        ));
        desk.connection.pending_spawns.push(7);
        desk.connection
            .uploads
            .push((8, phux_protocol::ids::ResourceId::local(1), None));
        desk.connection.pastes.insert(9);
        desk.connection.preedit = Some("composition".into());
        desk.replace_connection(policy);
        assert_eq!(desk.font_size, 19.0);
        assert_eq!(desk.font_family, "test font");
        assert_eq!(desk.scale.get(), 1.5);
        assert_eq!(desk.window_w, 777.0);
        assert!(!desk.sidebar_open);
        assert!(desk.sidebar_peek);
        assert_eq!(desk.settings.glass, 73);
        assert!(std::sync::Arc::ptr_eq(desk.waker.as_ref().unwrap(), &waker));
        assert!(desk.connection.pending_spawns.is_empty());
        assert!(desk.connection.uploads.is_empty());
        assert!(desk.connection.pastes.is_empty());
        assert!(desk.connection.preedit.is_none());
        assert_eq!(desk.settings_open, policy == Replacement::Retry);
        assert_eq!(desk.pending.is_some(), policy == Replacement::Retry);
        if policy == Replacement::Retry {
            assert_eq!(desk.settings_ui.query.text, "retained search");
            assert_eq!(desk.palette_query.text, "retained palette");
        } else {
            assert!(desk.settings_ui.query.text.is_empty());
            assert!(desk.palette_query.text.is_empty());
        }
    }
}

#[test]
fn explicit_replacement_releases_old_view_leases_even_if_the_client_survives() {
    let replica = crate::test_support::terminal::Replica::new(80, 24, 2);
    let client = replica.client.clone();
    let mut desk = crate::test_support::window();
    desk.connection.client = Some(client.clone());
    desk.connection.shown = Some(crate::desk::Shown {
        resource: replica.terminal.clone(),
        view: Some(replica.views[0]),
    });
    desk.connection.others.insert(
        phux_protocol::ids::ResourceId::local(2),
        Some(replica.views[1]),
    );
    let fence = desk.connection.callback_fence().unwrap();
    desk.replace_connection(Replacement::Retry);
    for view in replica.views {
        assert!(
            client.view_generation(view).is_none(),
            "old view lease retired"
        );
    }
    assert_eq!(
        desk.connection.wanted.as_ref().map(|w| &w.0),
        Some(&replica.terminal)
    );
    // Deliberately reuse the very same embedded client: the namespace still
    // changed and queued callbacks must not regain authority.
    desk.connection.client = Some(client);
    assert!(!fence.is_current(&desk.connection));
    desk.replace_connection(Replacement::Retry);
}
