//! Regression for phux-ycw0: a consumer that advertises NO image protocols
//! (the phux-web capability profile — its canvas renderer paints text, color,
//! and the cursor only) must not receive image-protocol escapes on the wire.
//!
//! Per SPEC 6.2 and ADR-0034, the server adapts forwarded PTY bytes to each
//! client's advertised capability set (`phux-server::downsample`): kitty
//! graphics APC (`ESC _ G ... ST`), sixel DCS (`ESC P q ... ST`), and iTerm2
//! inline images (`OSC 1337`) are dropped when the matching
//! `ImageProtocol` bit was not advertised in HELLO. This test drives that
//! gate end-to-end over the real wire:
//!
//! * a seed PTY emits all three image escapes bracketed by text markers;
//! * a "web-profile" client (no image protocols) must receive the markers
//!   but none of the image escapes;
//! * a control client advertising every image protocol must receive all
//!   three escapes verbatim — proving the fixture really emitted them and
//!   the gate (not an accident of the pipeline) is what protects the
//!   web-profile client.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

mod common;

use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{ClientCapabilities, ImageProtocolSet};
use phux_protocol::wire::frame::{FrameKind, TYPE_ATTACHED, TYPE_BOOTSTRAP_BEGIN};
use portable_pty::CommandBuilder;
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::time::timeout;

use common::{
    SOCKET_CONNECT_DEADLINE, attach_by_name, recv_typed, run_local, send_frame,
    spawn_server_with_seed_cmd, wait_for_raw_socket,
};

/// Kitty graphics transmit-and-display, base64 payload (`ESC _ G ... ESC \`).
const KITTY_APC_INTRO: &[u8] = b"\x1b_G";
/// Sixel introducer: `ESC P q` (no params) — `is_sixel_dcs` matches it.
const SIXEL_INTRO: &[u8] = b"\x1bPq";
/// iTerm2 inline image: `OSC 1337 ; ... BEL`.
const ITERM2_INTRO: &[u8] = b"\x1b]1337;";

/// The markers the seed script prints around the image burst. Plain text, so
/// they survive every capability profile.
const BEGIN_MARKER: &[u8] = b"IMG_BEGIN";
const END_MARKER: &[u8] = b"IMG_END";

/// The capability profile phux-web advertises in its HELLO. Mirrors
/// `clients/phux-web/src/session.rs::client_caps()` — phux-web is a separate
/// (wasm-only) cargo workspace, so the profile is restated here rather than
/// imported. Everything default except: no image protocols.
const fn web_profile_caps() -> ClientCapabilities {
    ClientCapabilities::new().with_image_protocols(ImageProtocolSet::new())
}

/// A deterministic seed command: sleep long enough for both clients to
/// attach (so the burst arrives as live `TERMINAL_OUTPUT`, not snapshot
/// replay), then print the three image-protocol escapes bracketed by text
/// markers, then idle so the pane stays alive through teardown.
fn image_burst_command() -> CommandBuilder {
    // Attach completes in well under 100 ms; the 0.5 s lead keeps margin
    // for a loaded 2-core CI runner without stalling every run a full second.
    let script = "sleep 0.5; \
         printf 'IMG_BEGIN\\r\\n'; \
         printf '\\033_Ga=T,f=100,s=1,v=1;QUJDRA==\\033\\\\'; \
         printf '\\033Pq#0;2;0;0;0#0~~@@~~$\\033\\\\'; \
         printf '\\033]1337;File=inline=1:QUJDRA==\\007'; \
         printf 'IMG_END\\r\\n'; \
         sleep 30";
    let mut cmd = CommandBuilder::new("/bin/sh");
    cmd.args(["-c", script]);
    cmd
}

/// Substring search over accumulated wire bytes.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Full opening sequence with an explicit HELLO capability set:
/// `HELLO` -> `HELLO_OK` -> `ATTACH` -> `ATTACHED` -> `TERMINAL_SNAPSHOT`.
async fn attach_with_caps(
    socket_path: &std::path::Path,
    session: &str,
    client_name: &str,
    caps: ClientCapabilities,
) -> UnixStream {
    let mut stream = wait_for_raw_socket(socket_path, SOCKET_CONNECT_DEADLINE).await;
    send_frame(
        &mut stream,
        &FrameKind::Hello {
            client_name: client_name.to_owned(),
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            client_caps: caps,
        },
    )
    .await;
    let (_, hello_ok) = recv_typed(&mut stream).await;
    assert!(
        matches!(hello_ok, FrameKind::HelloOk { .. }),
        "expected HELLO_OK, got {hello_ok:?}"
    );
    send_frame(&mut stream, &attach_by_name(session)).await;
    let (tb, _) = recv_typed(&mut stream).await;
    assert_eq!(tb, TYPE_ATTACHED, "expected ATTACHED after ATTACH");
    let (tb, _) = recv_typed(&mut stream).await;
    assert_eq!(tb, TYPE_BOOTSTRAP_BEGIN, "expected opening snapshot");
    stream
}

/// Accumulate `TERMINAL_OUTPUT` bytes until `marker` appears. Each recv is
/// bounded by the harness `WIRE_RECV_TIMEOUT`, so a server that never emits
/// the marker fails loudly instead of hanging.
async fn drain_output_until(stream: &mut UnixStream, marker: &[u8]) -> Vec<u8> {
    let mut acc = Vec::new();
    while !contains(&acc, marker) {
        let (_, frame) = recv_typed(stream).await;
        if let FrameKind::TerminalOutput { bytes, .. } = frame {
            acc.extend_from_slice(&bytes);
        }
    }
    acc
}

#[test]
fn no_image_escapes_forwarded_to_client_advertising_none() {
    run_local(async {
        let tmp = TempDir::new().expect("tempdir");
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown_tx, server_handle) =
            spawn_server_with_seed_cmd(socket_path.clone(), "default", image_burst_command());

        // Both clients attach during the seed script's initial sleep, so
        // the escape burst reaches them as live TERMINAL_OUTPUT.
        let mut web = attach_with_caps(
            &socket_path,
            "default",
            "web-profile (no image protocols)",
            web_profile_caps(),
        )
        .await;
        let mut control = attach_with_caps(
            &socket_path,
            "default",
            "control (all image protocols)",
            ClientCapabilities::new().with_image_protocols(ImageProtocolSet::all()),
        )
        .await;

        let web_bytes = drain_output_until(&mut web, END_MARKER).await;
        let control_bytes = drain_output_until(&mut control, END_MARKER).await;

        // Control first: the fixture really emitted every escape, and a
        // client that advertised the protocols receives them verbatim. This
        // is what makes the web-profile assertion below non-vacuous.
        for (name, intro) in [
            ("kitty graphics APC", KITTY_APC_INTRO),
            ("sixel DCS", SIXEL_INTRO),
            ("iTerm2 OSC 1337", ITERM2_INTRO),
        ] {
            assert!(
                contains(&control_bytes, intro),
                "control client (all image protocols) should receive the \
                 {name} escape verbatim; fixture may not have emitted it",
            );
        }

        // The web-profile client saw the surrounding text...
        assert!(
            contains(&web_bytes, BEGIN_MARKER),
            "web-profile client should still receive plain text output",
        );
        // ...but none of the image escapes.
        for (name, intro) in [
            ("kitty graphics APC", KITTY_APC_INTRO),
            ("sixel DCS", SIXEL_INTRO),
            ("iTerm2 OSC 1337", ITERM2_INTRO),
        ] {
            assert!(
                !contains(&web_bytes, intro),
                "server forwarded a {name} escape to a client that \
                 advertised no image protocols (SPEC 6.2 / phux-ycw0)",
            );
        }

        drop(web);
        drop(control);
        shutdown_tx.send(()).ok();
        timeout(std::time::Duration::from_secs(5), server_handle)
            .await
            .expect("server did not shut down within 5s")
            .expect("server task join")
            .expect("server run_async ok");
        assert!(
            !socket_path.exists(),
            "socket file leaked after shutdown: {} still on disk",
            socket_path.display(),
        );
    });
}
