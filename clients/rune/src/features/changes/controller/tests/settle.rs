//! A host settle must not swallow the real Changes sheet's input-driven exit.

use super::*;

fn opened_sheet() -> (Arc<Client>, Runtime<Desk>) {
    let (client, mut rt) = fixture(true);
    open(&mut rt);
    let _ = request(&client, CheckoutRequest::Status);
    rt.settle_all();
    rt.frame(0.0);
    assert!(rt.state.connection.changes.open);
    assert!(rt.find("changes-close").is_some());
    assert!(requests(&client).is_empty());
    (client, rt)
}

fn assert_exit_finishes(client: &Client, rt: &mut Runtime<Desk>) {
    assert!(rt.frame(1.0 / 60.0), "the sheet's exit still animates");
    assert!(!rt.state.connection.changes.open);
    // find() excludes exiting nodes; inspect the actual mounted sheet instead.
    let sheet = rt
        .elements()
        .into_iter()
        .find(|e| e.key.as_deref() == Some("changes"))
        .expect("Changes stays mounted while its exit plays");
    assert!(sheet.exiting, "the actual Changes sheet is exiting");
    assert!(
        sheet.opacity > 0.0 && sheet.opacity < 1.0,
        "the sheet is mid-fade, not cut: {}",
        sheet.opacity
    );
    assert!(requests(client).is_empty());
    for _ in 0..120 {
        rt.frame(1.0 / 60.0);
    }
    assert!(
        rt.elements()
            .into_iter()
            .all(|e| e.key.as_deref() != Some("changes")),
        "Changes unmounts once its exit settles"
    );
    assert!(requests(client).is_empty());
}

#[test]
fn a_settle_and_escape_in_one_frame_preserve_the_changes_exit() {
    let (client, mut rt) = opened_sheet();
    rt.settle_all();
    rt.key(KeyEvent {
        key: Key::Escape,
        mods: Mods::default(),
    });
    assert_exit_finishes(&client, &mut rt);
}

#[test]
fn a_settle_and_close_press_in_one_frame_preserve_the_changes_exit() {
    let (client, mut rt) = opened_sheet();
    let (x, y) = rt.find("changes-close").expect("the real Close button");
    rt.settle_all();
    rt.pointer_down(x, y);
    rt.pointer_up(x, y);
    assert_exit_finishes(&client, &mut rt);
}
