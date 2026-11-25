use std::io::{Read as _, Write as _};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use phux_protocol::ResourceId;
use phux_protocol::input::key::{ModSet, PhysicalKey};
use phux_protocol::input::mouse::{MouseAction, MouseButton, MouseEvent};
use phux_protocol::wire::frame::{AgentEvent, FrameKind};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::{AgentLink, AgentSignOverlay, Due, QUESTION_ARMING};
use crate::render::Theme;
use crate::render::overlay::{OverlayCommand, RenderOverlay as _};
use crate::test_util::{key_event, press, press_text, rendered};

const SIGNATURE: &[u8] = &[14, 0, 0, 0, 1, b's'];

fn string(value: &[u8]) -> Vec<u8> {
    let mut out = u32::try_from(value.len()).unwrap().to_be_bytes().to_vec();
    out.extend(value);
    out
}

/// A local agent holding one key, `k`, under `me@here`.
pub(in crate::attach) fn local_agent(dir: &tempfile::TempDir) -> PathBuf {
    let path = dir.path().join("agent.sock");
    let listener = UnixListener::bind(&path).unwrap();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut len = [0; 4];
            if stream.read_exact(&mut len).is_err() {
                continue;
            }
            let mut message = vec![0; usize::try_from(u32::from_be_bytes(len)).unwrap()];
            stream.read_exact(&mut message).unwrap();
            let answer = if message[0] == 11 {
                let mut list = vec![12, 0, 0, 0, 1];
                list.extend(string(b"k"));
                list.extend(string(b"me@here"));
                list
            } else {
                SIGNATURE.to_vec()
            };
            stream.write_all(&string(&answer)).unwrap();
        }
    });
    path
}

/// A pane's sign request, as the server sends it.
pub(in crate::attach) fn sign_request(id: u64) -> FrameKind {
    let mut message = vec![13];
    message.extend(string(b"k"));
    message.extend(string(b"data"));
    message.extend([0; 4]);
    FrameKind::Event {
        terminal: Some(ResourceId::local(3)),
        event: AgentEvent::SshAgentRequest { id, message },
        stamp: None,
    }
}

pub(in crate::attach) async fn due(link: &AgentLink) -> Due {
    tokio::time::timeout(Duration::from_secs(5), link.due())
        .await
        .unwrap()
}

/// The overlay of one sign request, and the link its answer comes back on.
async fn asked(dir: &tempfile::TempDir, armed: bool) -> (AgentSignOverlay, Arc<AgentLink>) {
    let link = AgentLink::new("mini".to_owned(), Ok(local_agent(dir)), None);
    link.take(&sign_request(7)).unwrap();
    let Due::Ask(question) = due(&link).await else {
        panic!("a signature is asked first");
    };
    let mut overlay = AgentSignOverlay::new(question, Some("zsh"), &Theme::default());
    if armed {
        arm(&mut overlay);
    }
    (overlay, link)
}

fn arm(overlay: &mut AgentSignOverlay) {
    overlay.last_key -= QUESTION_ARMING;
}

/// What the link sends back for the request.
async fn replied(link: &AgentLink) -> Vec<u8> {
    match due(link).await {
        Due::Reply { id: 7, message } => message,
        other => panic!("expected the reply, got {other:?}"),
    }
}

/// The offer is made once, and its refusal said once, however many
/// connections meet it.
#[test]
fn a_refused_offer_is_said_once() {
    use phux_protocol::wire::frame::{CommandResult, ErrorCode};
    for said in [1, 0] {
        let link = AgentLink::new("refuser".to_owned(), Ok("/nonexistent".into()), None);
        assert!(matches!(link.take_offer(true, || 4), Ok(Some(_))));
        assert!(matches!(link.take_offer(true, || 5), Ok(None)));
        let result = |request_id| FrameKind::CommandResult {
            request_id,
            result: CommandResult::error(ErrorCode::PermissionDenied, "no"),
        };
        assert_eq!(link.take(&result(5)), None, "another command's answer");
        assert_eq!(
            link.take(&result(4)).map(|notices| notices.len()),
            Some(said)
        );
        assert_eq!(link.take(&result(4)), None, "answered once");
    }
}

#[tokio::test]
async fn the_dialog_names_the_pane_the_host_and_the_key() {
    let dir = tempfile::tempdir().unwrap();
    let (overlay, _link) = asked(&dir, true).await;
    let text = rendered(&overlay, 100, 30);
    for needle in [
        "Use your ssh key?",
        "pane @3 (zsh) on mini",
        "SHA256:",
        "(me@here)",
        "Sign",
        "Refuse",
    ] {
        assert!(text.contains(needle), "{needle} missing:\n{text}");
    }
}

/// Enter refuses; Left then Enter signs; Esc refuses.
#[tokio::test]
async fn the_default_refuses_and_signing_takes_a_deliberate_move() {
    let dir = tempfile::tempdir().unwrap();
    for (keys, answer) in [
        (&[PhysicalKey::Enter][..], &[5][..]),
        (&[PhysicalKey::ArrowLeft, PhysicalKey::Enter], SIGNATURE),
        (&[PhysicalKey::Escape], &[5]),
    ] {
        let dir_one = tempfile::tempdir_in(dir.path()).unwrap();
        let (mut overlay, link) = asked(&dir_one, true).await;
        let last = keys
            .iter()
            .map(|key| overlay.handle_key(&press(*key)))
            .last();
        assert_eq!(last, Some(OverlayCommand::Dismiss), "{keys:?}");
        assert_eq!(replied(&link).await, answer, "{keys:?}");
    }
}

/// Nothing typed signs: not the line in progress, the `y` in it, a Tab or
/// Right that completes it, its Enter, or a Left and Enter hard on its heels.
#[tokio::test]
async fn type_ahead_never_signs() {
    let dir = tempfile::tempdir().unwrap();
    let (mut overlay, link) = asked(&dir, false).await;
    let typed = |overlay: &mut AgentSignOverlay, key, text| {
        assert_eq!(
            overlay.handle_key(&press_text(key, text)),
            OverlayCommand::Stay,
            "{key:?}"
        );
    };
    for _ in 0..2 {
        typed(&mut overlay, PhysicalKey::ArrowLeft, None);
        typed(&mut overlay, PhysicalKey::Enter, None);
        typed(&mut overlay, PhysicalKey::Y, Some("y"));
        typed(&mut overlay, PhysicalKey::Enter, None);
    }
    arm(&mut overlay);
    typed(&mut overlay, PhysicalKey::ArrowLeft, None);
    // Typing after the move starts over, as do Tab, Right and a chord.
    for key in [PhysicalKey::Y, PhysicalKey::Tab, PhysicalKey::ArrowRight] {
        typed(&mut overlay, key, None);
        typed(&mut overlay, PhysicalKey::Enter, None);
        arm(&mut overlay);
    }
    let chord = key_event(PhysicalKey::Enter, ModSet::CTRL, None);
    assert_eq!(overlay.handle_key(&chord), OverlayCommand::Stay);
    overlay.handle_paste("y\n");
    arm(&mut overlay);
    assert_eq!(
        overlay.handle_key(&press(PhysicalKey::Enter)),
        OverlayCommand::Dismiss
    );
    assert_eq!(replied(&link).await, [5]);

    // Dropped unanswered, whatever dropped it.
    let dir = tempfile::tempdir().unwrap();
    let (overlay, link) = asked(&dir, true).await;
    drop(overlay);
    assert_eq!(replied(&link).await, [5]);
}

#[tokio::test]
async fn a_click_signs_only_once_armed() {
    let dir = tempfile::tempdir().unwrap();
    let (mut overlay, link) = asked(&dir, false).await;
    let area = Rect::new(0, 0, 100, 30);
    let click = |overlay: &mut AgentSignOverlay| {
        let mut buf = Buffer::empty(area);
        overlay.render(area, &mut buf);
        let (x, y) = (0..area.height)
            .rev()
            .find_map(|y| {
                let row: String = (0..area.width).map(|x| buf[(x, y)].symbol()).collect();
                row.find("Sign").map(|x| (x, y))
            })
            .expect("the button is painted");
        let at = |action| MouseEvent {
            action,
            button: MouseButton::Left,
            mods: ModSet::empty(),
            x: f64::from(u16::try_from(x).expect("fits") + 1),
            y: f64::from(y),
        };
        overlay.handle_mouse(&at(MouseAction::Press));
        overlay.handle_mouse(&at(MouseAction::Release))
    };
    assert_eq!(click(&mut overlay), OverlayCommand::Stay);
    arm(&mut overlay);
    assert_eq!(click(&mut overlay), OverlayCommand::Dismiss);
    assert_eq!(replied(&link).await, SIGNATURE);
}
