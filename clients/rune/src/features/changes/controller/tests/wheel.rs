//! Nested wheel routing through the real Changes consumer, not a stand-in UI.

use super::*;

const PANES: [&str; 2] = ["changes-files", "changes-diff"];

fn short_sheet() -> (Arc<Client>, Runtime<Desk>) {
    let (client, mut rt) = fixture(true);
    rt.set_viewport((640.0, 360.0), 2.0);
    open(&mut rt);
    let id = request(&client, CheckoutRequest::Status);
    reply(
        &client,
        id,
        status(
            (0..30)
                .map(|i| entry(&format!("file-{i:02}.txt"), CheckoutArea::Unstaged))
                .collect(),
        ),
    );
    poll(&mut rt);
    let id = diff_request(&client, "file-00.txt", CheckoutArea::Unstaged);
    reply(
        &client,
        id,
        CheckoutAnswer::Diff(CheckoutDiff {
            path: "file-00.txt".into(),
            area: CheckoutArea::Unstaged,
            text: format!(
                "@@ -0,0 +1,80 @@\n{}",
                (0..80)
                    .map(|i| format!("+wheel-proof-{i:02}\n"))
                    .collect::<String>()
            ),
            truncated: false,
            binary: false,
        }),
    );
    poll(&mut rt);
    for _ in 0..60 {
        rt.frame(1.0 / 60.0);
    }
    assert!(requests(&client).is_empty());
    (client, rt)
}

fn rect(rt: &Runtime<Desk>, key: &str) -> [f32; 4] {
    rt.elements()
        .into_iter()
        .find(|e| e.key.as_deref() == Some(key))
        .unwrap_or_else(|| panic!("missing {key}"))
        .rect
}

/// The viewport's window y follows the outer sheet; the child's relative y
/// follows only the inner scroll. This distinguishes handoff from inner motion.
fn positions(rt: &Runtime<Desk>, pane: &str) -> (f32, f32) {
    let viewport = rect(rt, pane);
    let child = if pane == "changes-files" {
        rect(rt, "changes-row-0")
    } else {
        rt.elements()
            .into_iter()
            .find(|e| {
                e.text
                    .as_deref()
                    .is_some_and(|text| text.starts_with("+wheel-proof-00"))
            })
            .expect("the actual diff text")
            .rect
    };
    (viewport[1], child[1] - viewport[1])
}

fn pointer(rt: &Runtime<Desk>, pane: &str) -> (f32, f32) {
    let e = rt
        .elements()
        .into_iter()
        .find(|e| e.key.as_deref() == Some(pane))
        .unwrap();
    let [x, y, w, h] = e.visible;
    assert!(
        w > 40.0 && h > 40.0,
        "{pane} has a real visible wheel target"
    );
    assert!(h < e.rect[3], "the short sheet clips its nested viewport");
    (x + w / 2.0, y + h / 2.0)
}

#[track_caller]
fn near(actual: f32, expected: f32) {
    assert!(
        (actual - expected).abs() < 0.5,
        "expected {expected:.2}, got {actual:.2}"
    );
}

fn wheel(rt: &mut Runtime<Desk>, at: (f32, f32), dy: f32) {
    rt.wheel_xy(at.0, at.1, 0.0, dy);
    rt.frame(0.0);
}

fn read_only(client: &Client, rt: &Runtime<Desk>) {
    assert_eq!(rt.state.connection.changes.picked, 0);
    assert_eq!(rt.state.connection.changes.rows()[0].path, "file-00.txt");
    assert!(rt.state.connection.changes.open);
    // requests inspects every frame and rejects terminal input, scroll commands,
    // and mutations as well as unexpected sheet traffic.
    assert!(requests(client).is_empty());
}

#[test]
fn fresh_run_at_inner_bottom_scrolls_the_short_changes_sheet() {
    for pane in PANES {
        let (client, mut rt) = short_sheet();
        rt.update(|_, tx| tx.scroll_to_instant(pane, 0.0, 100_000.0));
        rt.frame(0.0);
        let before = positions(&rt, pane);
        assert!(before.1 < -100.0, "{pane} really overflowed");
        let at = pointer(&rt, pane);
        wheel(&mut rt, at, -40.0);
        let after = positions(&rt, pane);
        near(after.0, before.0 - 40.0);
        near(after.1, before.1);
        read_only(&client, &rt);
    }
}
