//! A picked file goes to the pane it was asked for, or nowhere and says why.

use super::*;
use crate::desk::Shown;
use crate::test_support::notice;
use phux_client_runtime::control::ControlOptions;

fn phone() -> Desk {
    let mut desk = Desk::with_preferences(
        &crate::launch::Resolved::offline("not launched"),
        None,
        rune::prefs::Prefs::memory(),
    )
    .with_phone(true);
    desk.native = Some(crate::mobile::Platform::test());
    desk.connection.client = Some(std::sync::Arc::new(phux_client_runtime::Runtime::embedded(
        ControlOptions::default(),
    )));
    show(&mut desk, 1);
    desk
}

fn show(desk: &mut Desk, terminal: u32) {
    desk.connection.shown = Some(Shown {
        resource: ResourceId::local(terminal),
        view: phux_client_runtime::ViewId::from_raw(1),
    });
}

fn ask(desk: &mut Desk) -> u64 {
    desk.screenshot();
    let native = desk.native.as_mut().unwrap();
    assert!(
        std::mem::take(&mut native.attach_requested),
        "asks the host"
    );
    native.attaching.front().unwrap().id
}

fn targets(desk: &Desk) -> Vec<ResourceId> {
    let uploads = desk.connection.uploads.iter();
    uploads.map(|(_, terminal, _)| terminal.clone()).collect()
}

#[test]
fn every_pick_goes_to_the_pane_it_was_asked_for() {
    let mut desk = phone();
    let first = ask(&mut desk);
    show(&mut desk, 2);
    let second = ask(&mut desk);
    desk.attach_from_host(second, "pdf", Some(vec![1]));
    // The first photo was still loading, and there were two of them.
    desk.attach_from_host(first, "jpg", Some(vec![1]));
    desk.attach_from_host(first, "png", Some(vec![1]));
    let panes = [2, 1, 1].map(ResourceId::local);
    assert_eq!(targets(&desk), panes);
    assert_eq!(notice(&desk), "Uploading image…");
}

#[test]
fn what_cannot_be_sent_says_why_and_uploads_nothing() {
    let mut desk = phone();
    let id = ask(&mut desk);
    desk.attach_from_host(id, "jpg", None);
    assert!(notice(&desk).contains("couldn't be read"));
    desk.attach_from_host(id + 1, "jpg", Some(vec![1]));
    assert!(notice(&desk).contains("too much"));
    desk.left_server();
    desk.attach_from_host(id, "jpg", Some(vec![1]));
    assert!(notice(&desk).contains("another computer"));
    assert!(targets(&desk).is_empty());
}

#[test]
fn a_question_with_the_keyboard_holds_attachments_off() {
    let mut desk = phone();
    let id = ask(&mut desk);
    desk.settings_open = true;
    desk.attach_from_host(id, "jpg", Some(vec![1]));
    assert!(targets(&desk).is_empty());
    desk.screenshot();
    assert!(!desk.native.as_ref().unwrap().attach_requested);
}

#[test]
fn without_a_terminal_nothing_is_picked() {
    let mut desk = phone();
    desk.connection.shown = None;
    desk.screenshot();
    assert!(!desk.native.as_ref().unwrap().attach_requested);
    assert_eq!(notice(&desk), "No terminal to attach to.");
}
