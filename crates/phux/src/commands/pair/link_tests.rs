//! Link rendering: secrets stay out of the query, and the QR fits.

use super::{ServerPins, build_connect_link, build_connect_link_pinned, parse_connect_link};

/// The secrets ride the fragment, which a browser never sends (#2396).
#[test]
fn link_secrets_stay_out_of_the_query() {
    let link = build_connect_link_pinned(
        "wss://h:1",
        Some("quic://h:2"),
        None,
        ServerPins {
            leaf: Some("AB"),
            authority: None,
            enrollment: Some("ticket"),
        },
        "deadbeef",
    );
    assert_eq!(
        link,
        "https://phux.sh/connect?url=wss://h:1&quic=quic://h:2&fp=AB#enroll=ticket&token=deadbeef"
    );
    let (query, _) = link.split_once('#').expect("a fragment");
    assert!(!query.contains("ticket") && !query.contains("deadbeef"));
    let parsed = parse_connect_link(&link).expect("parse");
    assert_eq!(parsed.enrollment_ticket.as_deref(), Some("ticket"));
    assert_eq!(parsed.token, "deadbeef");
}

#[test]
fn renders_a_nonempty_qr_for_a_realistic_payload() {
    // A real 32-byte hex token + SHA-256 fingerprint is well within QR
    // capacity; the renderer must produce non-empty half-block art.
    let link = build_connect_link(
        "wss://100.64.0.2:8787",
        Some("quic://100.64.0.2:8788"),
        Some("mini"),
        Some("CD:".repeat(31).trim_end_matches(':')),
        &"ab".repeat(32),
    );
    let art = super::render_qr_scaled(&link, 1).expect("QR should encode");
    assert!(!art.is_empty(), "QR render must be non-empty");
    // Dense1x2 uses half-block glyphs; at least one must appear.
    assert!(
        art.chars().any(|c| matches!(c, '█' | '▀' | '▄' | ' ')),
        "QR render must contain half-block glyphs",
    );
}

#[test]
fn a_larger_qr_is_wider_and_the_scale_stays_inside_the_terminal() {
    let link = build_connect_link(
        "wss://100.64.0.2:8787",
        None,
        Some("studio"),
        Some("AB:CD"),
        &"ab".repeat(32),
    );
    let small = super::render_qr_scaled(&link, 1).expect("scale 1");
    let large = super::render_qr_scaled(&link, 2).expect("scale 2");
    let width = |art: &str| {
        art.lines()
            .map(|line| line.chars().count())
            .max()
            .unwrap_or(0)
    };
    assert_eq!(width(&large), width(&small) * 2);
    let columns = u16::try_from(width(&small)).unwrap();
    assert_eq!(super::qr_scale_that_fits(&link, columns), 1);
    assert_eq!(
        super::qr_scale_that_fits(&link, columns.saturating_mul(2)),
        2
    );
}

/// A single-use link says when it lapses and that pairing keeps it (#2398);
/// `--reusable` still says it never expires.
#[test]
fn the_expiry_line_states_a_single_use_deadline_plainly() {
    let deadline = chrono::DateTime::from_timestamp(1_800_000_000, 0).unwrap();
    let mut secret = super::HumanSecret {
        credential_id: "cred",
        token: "ab",
        fingerprint: None,
        link: Some("https://phux.sh/connect?url=wss://h:1#token=ab"),
        expires_at: None,
        expires_label: None,
        first_use_by: Some(deadline),
        qr: false,
        default_qr: false,
        secret_file: None,
        hide_bearer: false,
    };
    let line = super::expiry_line(&secret);
    assert!(
        line.starts_with("SINGLE USE — EXPIRES 2027-01-15T08:00:00Z (10 minutes)"),
        "{line}"
    );
    assert!(
        line.contains("keeps working until `phux pair revoke cred`"),
        "{line}"
    );
    let body = super::secret_file_body(&secret);
    assert!(
        body.contains("link_expires_at=2027-01-15T08:00:00Z\n"),
        "{body}"
    );
    secret.first_use_by = None;
    assert!(super::expiry_line(&secret).starts_with("EXPIRES NEVER"));
}
