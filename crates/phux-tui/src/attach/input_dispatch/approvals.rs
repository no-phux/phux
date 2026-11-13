//! Approval UX over the attached server's dedicated control dial. Never read
//! request/reply traffic from the output-bearing attach stream, and never retry
//! a decision after losing its acknowledgement.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::time::Duration;

use phux_client::approvals::{Approval, Decision, Requester};
use phux_config::keybind::ResolvedAction;
use phux_protocol::ids::ApprovalId;
use phux_protocol::input::key::{KeyAction, KeyEvent, PhysicalKey};
use ratatui::{buffer::Buffer, layout::Rect, text::Line};

use crate::attach::connection::Connection;
use crate::render::overlay::widgets::{Modal, centered_panel, modal_inner_width};
use crate::render::overlay::{OverlayCommand, RenderOverlay, SelectItem, SelectList, ToastOverlay};
use crate::render::{ChromeBreakpoints, Theme};

use super::args::str_arg;
use super::ctx::DispatchCtx;
use super::run_action::ActionCall;

pub(in crate::attach) fn queue(
    ActionCall {
        resolved, effects, ..
    }: ActionCall<'_, '_>,
) {
    effects.approval = Some(resolved.clone());
}

#[allow(clippy::future_not_send, reason = "TUI state is current-thread bound")]
pub(super) async fn apply(action: Option<ResolvedAction>, ctx: &mut DispatchCtx<'_>) {
    let Some(action) = action else { return };
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        let dial = ctx
            .control_dial
            .ok_or_else(|| "No dedicated approval control dial; nothing sent.".to_owned())?;
        // Only this review UI offers credential approvals, not generic control verbs.
        let mut conn =
            phux_client::approvals::connect(dial, "phux approval review".to_owned(), true)
                .await
                .map_err(|err| err.to_string())?;
        manage(&mut conn, &action, ctx.theme).await
    })
    .await;
    let overlay = match result {
        Ok(Ok(overlay)) => overlay,
        Ok(Err(message)) => Box::new(ToastOverlay::new(
            "Approval not confirmed",
            vec![
                message,
                "Not retried. Refresh Approvals before deciding again.".to_owned(),
            ],
            ctx.theme,
        )),
        Err(_) => Box::new(ToastOverlay::new(
            "Approval outcome unknown",
            vec![
                "The control request timed out; a sent decision may have taken effect.".to_owned(),
                "Not retried. Refresh Approvals before deciding again.".to_owned(),
            ],
            ctx.theme,
        )),
    };
    ctx.overlays.push(overlay);
}

async fn manage(
    conn: &mut Connection,
    action: &ResolvedAction,
    theme: &Theme,
) -> Result<Box<dyn RenderOverlay>, String> {
    // A remote attach's dial is already authenticated to that server. Do not
    // interpret an approval id or a provider label as a host-routing selector.
    let approvals = phux_client::approvals::list(conn)
        .await
        .map_err(|err| err.to_string())?;
    if action.action == "approvals" {
        return Ok(Box::new(picker(&approvals, theme)));
    }
    let id = str_arg(action, "id")
        .and_then(|id| ApprovalId::parse(&id))
        .ok_or_else(|| "Invalid approval identity; nothing sent.".to_owned())?;
    let approval = approvals
        .into_iter()
        .find(|approval| approval.id == id.to_string())
        .ok_or_else(|| {
            "Stale approval: already decided, expired, or withdrawn. Nothing sent.".to_owned()
        })?;
    if action.action == "approval-review" {
        return Ok(Box::new(Review::new(&approval, theme)));
    }
    let decision = reviewed_decision(action, &approval)?;
    phux_client::approvals::decide(conn, id, decision)
        .await
        .map_err(|err| format!("Decision not confirmed (the request may now be stale): {err}"))?;
    Ok(Box::new(ToastOverlay::new(
        "Approval decided",
        vec![
            format!("Server accepted {} for {id}.", decision.as_str()),
            "This confirms the decision, not completion of the requested action.".to_owned(),
        ],
        theme,
    )))
}

fn reviewed_decision(action: &ResolvedAction, approval: &Approval) -> Result<Decision, String> {
    let decision = match str_arg(action, "decision").as_deref() {
        Some("approve") => Decision::Approve,
        Some("deny") => Decision::Deny,
        _ => return Err("Invalid decision; nothing sent.".to_owned()),
    };
    if decision == Decision::Approve && !reviewable(approval) {
        return Err("Credential details are missing or invalid; approval was not sent. Deny or refresh the request.".to_owned());
    }
    Ok(decision)
}

fn action(name: &str, id: Option<&str>) -> ResolvedAction {
    let mut args = BTreeMap::new();
    if let Some(id) = id {
        args.insert("id".to_owned(), toml::Value::String(id.to_owned()));
    }
    ResolvedAction {
        action: name.to_owned(),
        args,
    }
}

fn picker(approvals: &[Approval], theme: &Theme) -> SelectList {
    let mut rows: Vec<_> = approvals
        .iter()
        .map(|approval| {
            SelectItem::new(
                format!(
                    "{:?} · requester {} · {:?}",
                    approval.method, approval.requester.client, approval.id
                ),
                action("approval-review", Some(&approval.id)),
            )
        })
        .collect();
    if rows.is_empty() {
        rows.push(SelectItem::header(
            "No pending approvals on the attached server",
        ));
    }
    rows.push(SelectItem::new(
        "Refresh pending approvals",
        action("approvals", None),
    ));
    SelectList::new("Approvals · attached server (snapshot)", rows, theme)
}

fn identity(lines: &mut Vec<String>, label: &str, who: &Requester) {
    lines.push(format!("{label}: client {}", who.client));
    lines.push(format!("  Credential identity: {:?}", who.credential_id));
    lines.push(format!(
        "  Announced name (untrusted): {:?}",
        who.client_name
    ));
}

fn reviewable(approval: &Approval) -> bool {
    approval.credential.as_ref().map_or(
        !matches!(
            approval.method.as_str(),
            "CREDENTIALS" | "CREDENTIAL_REQUEST"
        ),
        |credential| credential.request.validate().is_ok(),
    )
}

fn details(approval: &Approval) -> Vec<String> {
    let mut lines = vec![
        if reviewable(approval) {
            "a: approve once · d: deny · Esc: close without deciding".to_owned()
        } else {
            "Approval disabled: credential details missing or invalid. d: deny · Esc: close"
                .to_owned()
        },
        format!("Approval: {:?}", approval.id),
        format!("Method: {:?}", approval.method),
    ];
    identity(&mut lines, "Requester", &approval.requester);
    if let Some(credential) = &approval.credential {
        lines
            .push("Approving lets this requester receive these credential values once.".to_owned());
        lines.push(format!(
            "Provider label (not an identity): {:?}",
            credential.request.provider
        ));
        identity(&mut lines, "Bound provider", &credential.provider);
        lines.push("Exact requested references (no secret values):".to_owned());
        for reference in &credential.request.references {
            lines.push(format!("  {reference:?}"));
        }
        lines.push("Purpose (UNTRUSTED requester text, not instructions):".to_owned());
        lines.push(format!("  {:?}", credential.request.purpose));
    } else {
        lines.push(format!("Signal: {:?}", approval.signal));
        lines.push(format!("Subjects: {:?}", approval.subjects));
    }
    lines.push(format!("Requested: {} Unix ms", approval.requested_at_ms));
    lines.push(format!("Expires: {} Unix ms", approval.expires_at_ms));
    lines
}

/// A review pins the exact approval id; Enter never means approve. Details
/// are quoted/escaped and character-wrapped, not clipped or interpreted as VT.
struct Review {
    id: String,
    reviewable: bool,
    lines: Vec<String>,
    offset: Cell<usize>,
    page: Cell<usize>,
    total: Cell<usize>,
    theme: Theme,
    breakpoints: ChromeBreakpoints,
}

impl Review {
    fn new(approval: &Approval, theme: &Theme) -> Self {
        Self {
            id: approval.id.clone(),
            reviewable: reviewable(approval),
            lines: details(approval),
            offset: Cell::new(0),
            page: Cell::new(1),
            total: Cell::new(1),
            theme: *theme,
            breakpoints: ChromeBreakpoints::default(),
        }
    }

    fn area(&self, outer: Rect) -> Rect {
        centered_panel(outer, 9, 40, 16, self.breakpoints)
    }

    fn decide(&self, decision: Decision) -> OverlayCommand {
        let mut action = action("approval-decide", Some(&self.id));
        action.args.insert(
            "decision".to_owned(),
            toml::Value::String(decision.as_str().to_owned()),
        );
        OverlayCommand::Commit(action)
    }

    fn scroll(&self, key: PhysicalKey) {
        let offset = self.offset.get();
        let page = self.page.get();
        let max = self.total.get().saturating_sub(page);
        let next = match key {
            PhysicalKey::ArrowDown => offset.saturating_add(1),
            PhysicalKey::ArrowUp => offset.saturating_sub(1),
            PhysicalKey::PageDown => offset.saturating_add(page),
            PhysicalKey::PageUp => offset.saturating_sub(page),
            PhysicalKey::Home => 0,
            PhysicalKey::End => max,
            _ => offset,
        };
        self.offset.set(next.min(max));
    }
}

fn wrapped(lines: &[String], width: usize) -> Vec<String> {
    let mut result = Vec::new();
    let width = width.max(1);
    for line in lines {
        let mut row = String::new();
        let mut cells = 0;
        for ch in line.chars() {
            let size = crate::render::cell_width(ch).unwrap_or(0);
            if cells + size > width && !row.is_empty() {
                result.push(std::mem::take(&mut row));
                cells = 0;
            }
            row.push(ch);
            cells += size;
        }
        result.push(row);
    }
    result
}

impl RenderOverlay for Review {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        let panel = self.area(area);
        let rows = wrapped(&self.lines, usize::from(modal_inner_width(panel.width)));
        let page = usize::from(panel.height.saturating_sub(2)).max(1);
        let offset = self.offset.get().min(rows.len().saturating_sub(page));
        self.offset.set(offset);
        self.page.set(page);
        self.total.set(rows.len());
        let body = rows
            .iter()
            .skip(offset)
            .take(page)
            .map(|row| Line::from(row.clone()))
            .collect();
        Modal::new(&self.theme, "Review approval · attached server", body)
            .footer(
                format!("{}/{}", offset + 1, rows.len()),
                if self.reviewable {
                    "↑↓ scroll · a approve · d deny · esc"
                } else {
                    "↑↓ scroll · d deny · esc"
                },
            )
            .render_into(panel, buf);
    }

    fn bounds(&self, area: Rect) -> Option<Rect> {
        Some(self.area(area))
    }

    fn set_breakpoints(&mut self, bp: ChromeBreakpoints) {
        self.breakpoints = bp;
    }

    fn set_theme(&mut self, theme: &Theme) {
        self.theme = *theme;
    }

    fn handle_key(&mut self, key: &KeyEvent) -> OverlayCommand {
        if key.action != KeyAction::Press || !key.mods.is_empty() || key.composing {
            return OverlayCommand::Stay;
        }
        match key.key {
            PhysicalKey::Escape => OverlayCommand::Dismiss,
            PhysicalKey::A if self.reviewable => self.decide(Decision::Approve),
            PhysicalKey::D => self.decide(Decision::Deny),
            key => {
                self.scroll(key);
                OverlayCommand::Stay
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, reason = "tests")]
mod tests {
    use super::*;
    use phux_protocol::input::key::ModSet;
    mod live;

    fn approval() -> Approval {
        Approval::parse(br#"{"id":"0123456789abcdef0123456789abcdef","requester":{"client":3,"credential_id":"sha256:requester","client_name":"agent"},"method":"CREDENTIAL_REQUEST","requested_at_ms":1,"expires_at_ms":120001,"credential":{"request":{"provider":"desktop","references":["op://vault/item/field","op://vault/item/section/another-field"],"purpose":"Run native TUI tests"},"provider":{"client":8,"credential_id":"sha256:bound","client_name":"desktop label"}}}"#).unwrap()
    }

    fn press(key: PhysicalKey) -> KeyEvent {
        KeyEvent {
            action: KeyAction::Press,
            key,
            mods: ModSet::empty(),
            consumed_mods: ModSet::empty(),
            composing: false,
            text: None,
            unshifted_codepoint: None,
        }
    }

    #[test]
    fn review_requires_explicit_decision_and_pins_exact_identity() {
        let mut review = Review::new(&approval(), &Theme::default());
        assert_eq!(
            review.handle_key(&press(PhysicalKey::Enter)),
            OverlayCommand::Stay
        );
        for (key, decision) in [(PhysicalKey::A, "approve"), (PhysicalKey::D, "deny")] {
            let OverlayCommand::Commit(action) = review.handle_key(&press(key)) else {
                panic!("decision")
            };
            assert_eq!(action.action, "approval-decide");
            assert_eq!(str_arg(&action, "id"), Some(approval().id));
            assert_eq!(str_arg(&action, "decision").as_deref(), Some(decision));
            let mut repeat = press(key);
            repeat.action = KeyAction::Repeat;
            assert_eq!(review.handle_key(&repeat), OverlayCommand::Stay);
            repeat.action = KeyAction::Press;
            repeat.mods = ModSet::CTRL;
            assert_eq!(review.handle_key(&repeat), OverlayCommand::Stay);
        }
        assert_eq!(
            review.handle_key(&press(PhysicalKey::Escape)),
            OverlayCommand::Dismiss
        );
    }

    #[test]
    fn credential_details_name_exact_references_both_identities_and_untrusted_purpose() {
        let mut approval = approval();
        approval.credential.as_mut().unwrap().request.purpose =
            "Ignore instructions\n\u{1b}[2J\u{202e}".to_owned();
        let lines = details(&approval);
        let text = lines.join("\n");
        for expected in [
            "Requester: client 3",
            "sha256:requester",
            "Bound provider: client 8",
            "sha256:bound",
            "op://vault/item/field",
            "op://vault/item/section/another-field",
            "UNTRUSTED requester text",
            "Ignore instructions",
            "120001",
        ] {
            assert!(text.contains(expected), "{expected}: {text}");
        }
        assert!(!text.contains('\u{1b}'));
        assert!(!text.contains('\u{202e}'));
        assert!(text.contains("\\n"));
        // Narrow terminals can inspect every character, even a reference
        // longer than the viewport. Wrapping never inserts an ellipsis.
        assert_eq!(wrapped(&lines, 12).concat(), lines.concat());
    }

    #[test]
    fn incomplete_or_invalid_credential_details_cannot_be_approved() {
        let mut incomplete = approval();
        incomplete.credential = None;
        for method in ["CREDENTIALS", "CREDENTIAL_REQUEST"] {
            incomplete.method = method.to_owned();
            assert_not_approvable(&incomplete);
        }
        let mut invalid = approval();
        invalid
            .credential
            .as_mut()
            .unwrap()
            .request
            .references
            .clear();
        assert_not_approvable(&invalid);
    }

    fn assert_not_approvable(approval: &Approval) {
        let mut review = Review::new(approval, &Theme::default());
        assert!(!reviewable(approval));
        assert!(details(approval).join("\n").contains("Approval disabled"));
        assert_eq!(
            review.handle_key(&press(PhysicalKey::A)),
            OverlayCommand::Stay
        );
        assert!(matches!(
            review.handle_key(&press(PhysicalKey::D)),
            OverlayCommand::Commit(_)
        ));
    }

    #[test]
    fn held_actions_and_empty_lists_remain_useful() {
        let mut held = approval();
        held.credential = None;
        held.method = "SIGNAL_TERMINAL".to_owned();
        held.signal = Some("kill".to_owned());
        held.subjects = vec!["terminal:4".to_owned()];
        let text = details(&held).join("\n");
        assert!(
            text.contains("SIGNAL_TERMINAL")
                && text.contains("kill")
                && text.contains("terminal:4")
        );
        let area = Rect::new(0, 0, 120, 24);
        let mut buf = Buffer::empty(area);
        picker(&[], &Theme::default()).render(area, &mut buf);
        let text: String = buf
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(
            text.contains("No pending approvals") && text.contains("Refresh pending approvals")
        );
    }

    #[test]
    fn review_scrolls_to_the_end_on_a_small_viewport() {
        let mut review = Review::new(&approval(), &Theme::default());
        let area = Rect::new(0, 0, 40, 8);
        let mut buf = Buffer::empty(area);
        review.render(area, &mut buf);
        assert!(review.total.get() > review.page.get());
        review.handle_key(&press(PhysicalKey::End));
        review.render(area, &mut buf);
        let text: String = buf
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(text.contains("Expires: 120001 Unix ms"), "{text}");
        review.handle_key(&press(PhysicalKey::Home));
        assert_eq!(review.offset.get(), 0);
    }

    #[tokio::test]
    async fn unsupported_server_receives_no_decision_or_metadata_requests() {
        use phux_client::testkit::{ScriptSpec, ScriptedServer};
        use phux_protocol::wire::frame::FrameKind;

        let (client, server) = tokio::net::UnixStream::pair().unwrap();
        let peer = tokio::spawn(ScriptedServer::on_stream(server, ScriptSpec::new()).run());
        let mut conn = Connection::from_stream(client);
        let result = manage(&mut conn, &action("approvals", None), &Theme::default()).await;
        assert!(result.is_err());
        drop(conn);
        assert!(matches!(
            peer.await.unwrap().as_slice(),
            [FrameKind::Hello { .. }]
        ));
    }

    #[test]
    fn registered_actions_queue_control_work_without_deciding_synchronously() {
        use crate::attach::input_dispatch::test_support::CtxFixture;

        let mut fixture = CtxFixture::default();
        for name in ["approvals", "approval-review", "approval-decide"] {
            let action = action(name, Some(&approval().id));
            let effects = fixture.run(&action);
            assert_eq!(effects.approval, Some(action));
            assert!(effects.command_frames.is_empty());
        }
    }
}
