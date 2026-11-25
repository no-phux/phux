//! The complete two-peer journey and its terminal-admission boundary.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use phux_client::conversation::ConversationRoute;

use super::{Peers, has_body, hermetic, invoke, success, text};

#[test]
#[ignore = "isolated real paired QUIC hub; e2e lane"]
#[allow(
    clippy::too_many_lines,
    clippy::print_stderr,
    reason = "one linear two-peer scenario and its measured timing evidence"
)]
fn chat_two_peers_exchange_confirm_and_reopen_without_changing_global_coordination() {
    let mut peers = Peers::new();
    let config = std::fs::read(peers.hub.join("config/phux/config.toml")).unwrap();
    let ordinary = invoke(
        &peers.hub,
        &[
            "mail",
            "send",
            "local-reader",
            "--as",
            "local-writer",
            "-t",
            "existing",
            "-m",
            "leave local mail alone",
            "--json",
        ],
        b"",
    );
    success(&ordinary);
    let started = Instant::now();
    let (invite, joined) = peers.connect();
    let confirmed = success(&invoke(
        &peers.hub,
        &["chat", "status", "buddy", "--json"],
        b"",
    ));
    let first_message = started.elapsed();
    assert_eq!(confirmed["peer_confirmed_welcome"], true);
    eprintln!(
        "native chat invite -> received welcome -> peer confirmation: {first_message:?}; installed compatible binaries, paired reachable loopback hub; no model/provider time"
    );
    assert!(first_message < Duration::from_secs(10), "{first_message:?}");
    assert_eq!(
        std::fs::read(peers.hub.join("config/phux/config.toml")).unwrap(),
        config
    );
    assert!(!peers.peer.join("config/phux/config.toml").exists());

    success(&invoke(
        &peers.hub,
        &["chat", "send", "buddy", "-m", "A to B", "--json"],
        b"",
    ));
    let peer_read = success(&invoke(
        &peers.peer,
        &["chat", "read", "friend", "--json"],
        b"",
    ));
    assert!(has_body(&peer_read, "A to B"));
    success(&invoke(
        &peers.peer,
        &["chat", "send", "friend", "-m", "B to A", "--json"],
        b"",
    ));
    assert!(has_body(
        &success(&invoke(
            &peers.hub,
            &["chat", "read", "buddy", "--json"],
            b""
        )),
        "B to A"
    ));

    let operation = "1234567890abcdef1234567890abcdef";
    let original = [
        "chat",
        "send",
        "friend",
        "-m",
        "one durable message",
        "--operation-id",
        operation,
        "--json",
    ];
    assert_eq!(
        success(&invoke(&peers.peer, &original, b""))["replayed"],
        false
    );
    assert_eq!(
        success(&invoke(&peers.peer, &original, b""))["replayed"],
        true
    );
    let conflicting = invoke(
        &peers.peer,
        &[
            "chat",
            "send",
            "friend",
            "-m",
            "different text",
            "--operation-id",
            operation,
            "--json",
        ],
        b"",
    );
    assert!(!conflicting.status.success());
    let history = success(&invoke(
        &peers.hub,
        &["chat", "read", "buddy", "--json"],
        b"",
    ));
    assert_eq!(
        history["mail"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|mail| mail["body"] == "one durable message")
            .count(),
        1
    );

    let typed = invoke(
        &peers.peer,
        &["chat", "open", "friend"],
        b"from the native conversation view\n/quit\n",
    );
    assert!(typed.status.success(), "{}", text(&typed.stderr));
    assert!(text(&typed.stdout).contains("Type a message"));
    assert!(has_body(
        &success(&invoke(
            &peers.hub,
            &["chat", "read", "buddy", "--json"],
            b""
        )),
        "from the native conversation view"
    ));

    // A deliberately wrong home/profile still follows the exact private route.
    let record = joined["record"].as_str().unwrap();
    let wrong = peers.peer.join("wrong-home");
    hermetic::prepare_dirs(&wrong);
    let explicit = hermetic::phux_with(
        &wrong,
        &[
            "chat",
            "send",
            "--record",
            record,
            "-m",
            "reply under a different home",
            "--json",
        ],
        b"",
        &[
            ("HOME", wrong.clone()),
            ("TMPDIR", wrong.clone()),
            ("XDG_DATA_HOME", wrong.join("data")),
            ("PHUX_PROFILE", PathBuf::from("another-profile")),
        ],
    );
    success(&explicit);
    assert!(has_body(
        &success(&invoke(
            &peers.hub,
            &["chat", "read", "buddy", "--json"],
            b""
        )),
        "reply under a different home"
    ));

    // Viewing history does not steal the launched agent's incoming mail.
    let taken = success(&invoke(
        &peers.peer,
        &[
            "chat",
            "run",
            "friend",
            "--",
            "/bin/sh",
            "-c",
            "exec \"$PHUX_BIN\" mail wait --timeout 0 --json",
        ],
        b"",
    ));
    assert!(has_body(&taken, "A to B"));
    assert!(taken["text"].as_str().unwrap().contains("--record"));
    assert!(taken["text"].as_str().unwrap().contains(record));

    peers.restart();
    assert!(has_body(
        &success(&invoke(
            &peers.peer,
            &["chat", "read", "friend", "--json"],
            b""
        )),
        "A to B"
    ));
    assert_eq!(
        success(&invoke(
            &peers.hub,
            &["chat", "status", "buddy", "--json"],
            b""
        ))["peer_confirmed_welcome"],
        true
    );
    let local = success(&invoke(
        &peers.hub,
        &[
            "mail",
            "read",
            "--as",
            "local-reader",
            "-t",
            "existing",
            "--all",
            "--peek",
            "--json",
        ],
        b"",
    ));
    assert!(has_body(&local, "leave local mail alone"));
    assert_no_terminal_access(invite["record"].as_str().unwrap());
    assert_no_terminal_access(record);
}

fn assert_no_terminal_access(record: &str) {
    let conversation = phux_client::conversation::load_path(Path::new(record)).unwrap();
    let ConversationRoute::Hub { entry, .. } = conversation.route;
    let dial = phux_dial::QuicDial {
        addr: entry.hub.parse().unwrap(),
        server_name: entry
            .server_name
            .clone()
            .unwrap_or_else(|| "127.0.0.1".to_owned()),
        token: None,
        trust: phux_dial::CertTrust::Authority {
            ca: entry.ca_fingerprint,
        },
        identity: Some(phux_dial::TlsClientIdentity::PemFiles {
            certificate: entry.client_cert,
            private_key: entry.client_key,
        }),
        inner: None,
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let connected = phux_dial::quic::dial(&dial).await;
        let Ok(mut connection) = connected else {
            return;
        };
        let hello = phux_protocol::wire::frame::FrameKind::Hello {
            client_name: "chat-has-no-terminal-authority".to_owned(),
            protocol_major: phux_protocol::PROTOCOL_VERSION.major,
            protocol_minor: phux_protocol::PROTOCOL_VERSION.minor,
            protocol_patch: phux_protocol::PROTOCOL_VERSION.patch,
            client_caps: phux_protocol::caps::ClientCapabilities::default(),
        };
        let mut bytes = bytes::BytesMut::new();
        hello.encode(&mut bytes);
        let _ = connection.2.write_all(&bytes).await;
        let mut header = [0; 4];
        let reply =
            tokio::time::timeout(Duration::from_secs(3), connection.3.read_exact(&mut header))
                .await;
        assert!(
            matches!(reply, Ok(Err(_))),
            "a chat credential must be refused, not admitted or left hanging"
        );
    });
}
