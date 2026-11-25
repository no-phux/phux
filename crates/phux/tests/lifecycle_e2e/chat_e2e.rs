//! Two independent CLI environments, a real paired hub, and no production state.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test fixture assertions"
)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::Value;
use tempfile::TempDir;

use crate::hermetic::{self, Server};
use crate::listeners;

mod journey;

struct Peers {
    _root: TempDir,
    hub: PathBuf,
    peer: PathBuf,
    endpoint: String,
    server: Option<Server>,
}

impl Peers {
    fn new() -> Self {
        // Keep Unix socket paths short and canonical (macOS /tmp is a symlink).
        let root = tempfile::Builder::new()
            .prefix("phux-chat-")
            .tempdir_in("/private/tmp")
            .unwrap_or_else(|_| tempfile::tempdir().unwrap());
        let base = root.path().canonicalize().unwrap();
        let hub = base.join("h");
        let peer = base.join("p");
        hermetic::prepare_dirs(&hub);
        hermetic::prepare_dirs(&peer);
        std::fs::write(
            hub.join("config/phux/config.toml"),
            "[policy]\nmode = \"paired\"\n",
        )
        .unwrap();
        let init = invoke(&hub, &["workload", "authority", "--init"], b"");
        assert!(init.status.success(), "{}", text(&init.stderr));
        let server = start(&hub, listeners::LOOPBACK_ANY_PORT);
        let endpoint = listeners::bound_listener_addr(
            &hub.join("s.sock"),
            listeners::RemoteListenerTransport::Quic,
        )
        .to_string();
        Self {
            _root: root,
            hub,
            peer,
            endpoint,
            server: Some(server),
        }
    }

    fn connect(&self) -> (Value, Value) {
        let invited = invoke(
            &self.hub,
            &[
                "chat",
                "invite",
                "buddy",
                "--hub",
                &self.endpoint,
                "-m",
                "hello from the actual inviter",
                "--json",
            ],
            b"",
        );
        let invite = success(&invited);
        assert_eq!(invite["state"], "welcome_stored");
        assert_eq!(invite["peer_joined"], false);
        let code = invite["invite"].as_str().unwrap();
        let joined = invoke(
            &self.peer,
            &["chat", "join", "friend", "--json"],
            code.as_bytes(),
        );
        let join = success(&joined);
        assert_eq!(join["welcome"]["body"], "hello from the actual inviter");
        assert_eq!(join["state"], "welcome_received");
        (invite, join)
    }

    fn restart(&mut self) {
        let mut old = self.server.take().unwrap();
        old.sigkill();
        drop(old);
        let server = start(&self.hub, &self.endpoint);
        let bound = listeners::bound_listener_addr(
            &self.hub.join("s.sock"),
            listeners::RemoteListenerTransport::Quic,
        );
        assert_eq!(bound.to_string(), self.endpoint);
        self.server = Some(server);
    }
}

fn start(dir: &Path, endpoint: &str) -> Server {
    hermetic::start_server(
        dir,
        &["--no-seed", "--quic", endpoint],
        &[
            ("PHUX_SOCKET", dir.join("s.sock").to_str().unwrap()),
            ("HOME", dir.to_str().unwrap()),
            ("TMPDIR", dir.to_str().unwrap()),
            ("XDG_DATA_HOME", dir.join("data").to_str().unwrap()),
        ],
    )
}

fn invoke(dir: &Path, args: &[&str], input: &[u8]) -> std::process::Output {
    hermetic::phux_with(
        dir,
        args,
        input,
        &[
            ("PHUX_SOCKET", dir.join("s.sock")),
            ("HOME", dir.to_path_buf()),
            ("TMPDIR", dir.to_path_buf()),
            ("XDG_DATA_HOME", dir.join("data")),
        ],
    )
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn success(output: &std::process::Output) -> Value {
    assert!(output.status.success(), "{}", text(&output.stderr));
    hermetic::json_doc(output)
}

fn has_body(document: &Value, body: &str) -> bool {
    document["mail"]
        .as_array()
        .unwrap()
        .iter()
        .any(|mail| mail["body"] == body)
}

#[test]
#[ignore = "isolated real paired QUIC hub; e2e lane"]
fn chat_reused_invites_fail_without_leaking_secrets_or_poisoning_the_alias() {
    let peers = Peers::new();
    let (invite, _) = peers.connect();
    let code = invite["invite"].as_str().unwrap();
    let replay = invoke(
        &peers.peer,
        &["chat", "join", "reused", "--json"],
        code.as_bytes(),
    );
    assert!(!replay.status.success());
    assert!(replay.stdout.is_empty());
    assert!(!text(&replay.stderr).contains(code));
    assert!(
        !peers.peer.join("state/phux/chats/reused").exists(),
        "an unspent local identity must not be fabricated after refusal"
    );
    let resumed = success(&invoke(
        &peers.peer,
        &["chat", "join", "friend", "--json"],
        b"",
    ));
    assert_eq!(resumed["state"], "welcome_received");

    let old_client = invoke(
        &peers.peer,
        &["host", "join", "--mail-only", "-", "--json"],
        code.as_bytes(),
    );
    assert!(!old_client.status.success());
    assert!(!text(&old_client.stderr).contains(code));
}

#[test]
#[ignore = "isolated real paired QUIC hub; e2e lane"]
fn chat_refuses_unknown_versions_and_does_not_fall_back_when_the_hub_is_down() {
    let mut peers = Peers::new();
    let (_, joined) = peers.connect();
    let bad = invoke(
        &peers.peer,
        &["chat", "join", "future", "--json"],
        b"phux://chat/v2#not-a-real-secret",
    );
    assert!(!bad.status.success());
    assert!(text(&bad.stderr).contains("compatible"));
    assert!(!peers.peer.join("state/phux/chats/future").exists());

    let mut server = peers.server.take().unwrap();
    server.sigkill();
    drop(server);
    let started = Instant::now();
    let unavailable = invoke(&peers.peer, &["chat", "status", "friend", "--json"], b"");
    assert!(!unavailable.status.success());
    assert!(unavailable.stdout.is_empty());
    assert!(started.elapsed() < Duration::from_secs(9));
    assert!(!peers.peer.join("state/phux/work/work-v1.sqlite").exists());
    let record = joined["record"].as_str().unwrap();
    assert!(
        phux_client::conversation::load_path(Path::new(record)).is_ok(),
        "network failure never deletes the enrolled identity"
    );
}

#[test]
#[ignore = "isolated real paired QUIC hub; e2e lane"]
fn chat_run_keeps_claims_local_and_pins_the_actual_executable_and_record() {
    let peers = Peers::new();
    let (_, joined) = peers.connect();
    let path = peers.peer.join("private-source");
    std::fs::create_dir_all(&path).unwrap();
    let acquired = success(&invoke(
        &peers.peer,
        &["claim", "take", path.to_str().unwrap(), "--json"],
        b"",
    ));
    let listed = success(&invoke(
        &peers.peer,
        &[
            "chat",
            "run",
            "friend",
            "--",
            "/bin/sh",
            "-c",
            "test \"$PHUX_BIN\" = \"$PHUX_AGENT_PHUX_BIN\" && test -n \"$PHUX_CHAT_RECORD\" && exec \"$PHUX_BIN\" claim list --json",
        ],
        b"",
    ));
    assert_eq!(listed["claims"][0]["holder"], acquired["holder"]);
    assert_eq!(listed["claims"][0]["path"], path.to_str().unwrap());
    assert!(
        joined["record"]
            .as_str()
            .unwrap()
            .ends_with("/friend/conversation.json")
    );
    let fence = acquired["claim"]["claims"][0]["fence"].as_u64().unwrap();
    let renewed = success(&invoke(
        &peers.peer,
        &[
            "chat",
            "run",
            "friend",
            "--",
            "/bin/sh",
            "-c",
            &format!("exec \"$PHUX_BIN\" claim renew {fence} --json"),
        ],
        b"",
    ));
    assert_eq!(renewed["holder"], acquired["holder"]);
    let released = success(&invoke(
        &peers.peer,
        &[
            "chat",
            "run",
            "friend",
            "--",
            "/bin/sh",
            "-c",
            &format!("exec \"$PHUX_BIN\" claim release {fence} --json"),
        ],
        b"",
    ));
    assert_eq!(released["released"], 1);
}

#[test]
#[ignore = "isolated real paired QUIC hub; e2e lane"]
fn chat_expired_and_revoked_invites_fail_with_bounded_secret_free_errors() {
    let peers = Peers::new();
    for (name, expire) in [("expired", true), ("revoked", false)] {
        let invite = success(&invoke(
            &peers.hub,
            &["chat", "invite", name, "--hub", &peers.endpoint, "--json"],
            b"",
        ));
        let ticket = invite["ticket_id"].as_str().unwrap();
        if expire {
            // Fault injection into this test's own verifier-only ticket store.
            let file = peers.hub.join("state/phux/enrollment-tickets");
            let mut state: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
            let row = state["tickets"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .find(|row| row["id"] == ticket)
                .unwrap();
            row["expires_at"] = 1.into();
            std::fs::write(file, serde_json::to_vec(&state).unwrap()).unwrap();
        } else {
            let revoked = invoke(&peers.hub, &["workload", "revoke", ticket], b"");
            assert!(revoked.status.success(), "{}", text(&revoked.stderr));
        }
        let code = invite["invite"].as_str().unwrap();
        let started = Instant::now();
        let refused = invoke(
            &peers.peer,
            &["chat", "join", name, "--json"],
            code.as_bytes(),
        );
        assert!(!refused.status.success());
        assert!(refused.stdout.is_empty());
        assert!(!text(&refused.stderr).contains(code));
        assert!(
            text(&refused.stderr).contains("refused"),
            "{}",
            text(&refused.stderr)
        );
        assert!(started.elapsed() < Duration::from_secs(10));
    }
}

#[test]
#[ignore = "isolated real paired QUIC hub and companion MCP binary; e2e lane"]
fn chat_explicit_mcp_route_works_under_an_unrelated_home_and_defaults_its_identity() {
    use std::io::{BufRead as _, BufReader, Write as _};
    use std::process::Stdio;

    let peers = Peers::new();
    let (_, joined) = peers.connect();
    let record_path = joined["record"].as_str().unwrap();
    let record = phux_client::conversation::load_path(Path::new(record_path)).unwrap();
    let wrong = peers.peer.join("mcp-home");
    hermetic::prepare_dirs(&wrong);
    // Never fall through `phux mcp`'s PATH lookup to the installed adapter.
    let binary = Path::new(crate::runner::phux_bin())
        .parent()
        .unwrap()
        .join("phux-mcp");
    assert!(
        binary.is_file(),
        "build the companion first: cargo build -p phux-mcp"
    );
    let mut command = std::process::Command::new(binary);
    command
        .envs(hermetic::hermetic_env(&wrong))
        .env("HOME", &wrong)
        .env("TMPDIR", &wrong)
        .env("XDG_DATA_HOME", wrong.join("data"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let mut exchange = |request: Value| {
        writeln!(input, "{request}").unwrap();
        input.flush().unwrap();
        let mut line = String::new();
        output.read_line(&mut line).unwrap();
        serde_json::from_str::<Value>(&line).unwrap()
    };
    exchange(serde_json::json!({
        "jsonrpc":"2.0", "id":1, "method":"initialize",
        "params":{"protocolVersion":"2025-06-18"}
    }));
    let reply = exchange(serde_json::json!({
        "jsonrpc":"2.0", "id":2, "method":"tools/call",
        "params":{"name":"phux_mail_send", "arguments":{
            "conversation_record":record_path,
            "to":[record.peer_address], "thread":record.thread, "message":"explicit MCP route"
        }}
    }));
    assert_ne!(reply["result"]["isError"], true, "{reply}");
    assert_eq!(
        reply["result"]["structuredContent"]["mail"]["from"],
        "inbox"
    );
    drop(input);
    assert!(child.wait().unwrap().success());
    assert!(!wrong.join("state/phux/work/work-v1.sqlite").exists());
    assert!(has_body(
        &success(&invoke(
            &peers.hub,
            &["chat", "read", "buddy", "--json"],
            b""
        )),
        "explicit MCP route"
    ));
}

#[test]
#[ignore = "isolated real paired QUIC hub; e2e lane"]
fn chat_invite_metadata_is_checked_before_spending_the_ticket() {
    use base64::Engine as _;
    let peers = Peers::new();
    let invite = success(&invoke(
        &peers.hub,
        &[
            "chat",
            "invite",
            "buddy",
            "--hub",
            &peers.endpoint,
            "--json",
        ],
        b"",
    ));
    let original = invite["invite"].as_str().unwrap();
    let codec = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let data = codec
        .decode(original.strip_prefix("phux://chat/v1#").unwrap())
        .unwrap();
    let source: Value = serde_json::from_slice(&data).unwrap();
    for (field, invalid) in [
        (
            "welcome_id",
            "ABCDEF1234567890ABCDEF1234567890AB".to_owned(),
        ),
        ("id", "00000000000000000000000000000000".to_owned()),
        (
            "peer_address",
            format!(" {} ", source["peer_address"].as_str().unwrap()),
        ),
    ] {
        let mut changed = source.clone();
        changed[field] = invalid.into();
        let code = format!(
            "phux://chat/v1#{}",
            codec.encode(serde_json::to_vec(&changed).unwrap())
        );
        let refused = invoke(
            &peers.peer,
            &["chat", "join", "friend", "--json"],
            code.as_bytes(),
        );
        assert!(!refused.status.success());
        assert!(!peers.peer.join("state/phux/chats/friend").exists());
    }
    success(&invoke(
        &peers.peer,
        &["chat", "join", "friend", "--json"],
        original.as_bytes(),
    ));
}

#[test]
#[ignore = "isolated real paired QUIC hub; e2e lane"]
fn chat_inviter_resumes_its_saved_identity_after_a_failed_welcome_exchange() {
    let peers = Peers::new();
    let welcome = "resumable real welcome";
    let invite = success(&invoke(
        &peers.hub,
        &[
            "chat",
            "invite",
            "buddy",
            "--hub",
            &peers.endpoint,
            "-m",
            welcome,
            "--json",
        ],
        b"",
    ));
    let record_path = Path::new(invite["record"].as_str().unwrap());
    let directory = record_path.parent().unwrap();
    let pending_path = directory.join("setup/conversation.json");
    let staged = std::fs::read(&pending_path).unwrap();
    // Simulate the boundary before welcome publication and peer-ticket mint.
    assert!(
        invoke(
            &peers.hub,
            &["workload", "revoke", invite["ticket_id"].as_str().unwrap(),],
            b""
        )
        .status
        .success()
    );
    std::fs::remove_file(record_path).unwrap();
    std::fs::remove_file(directory.join("invitation-issued")).unwrap();
    let mut broken: Value = serde_json::from_slice(&staged).unwrap();
    let unused = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let unreachable = unused.local_addr().unwrap().to_string();
    broken["route"]["entry"]["hub"] = unreachable.clone().into();
    std::fs::write(&pending_path, serde_json::to_vec(&broken).unwrap()).unwrap();
    let failed = invoke(
        &peers.hub,
        &[
            "chat",
            "invite",
            "buddy",
            "--hub",
            &unreachable,
            "-m",
            welcome,
            "--json",
        ],
        b"",
    );
    assert!(!failed.status.success());
    assert!(pending_path.exists());
    std::fs::write(&pending_path, staged).unwrap();
    let resumed = success(&invoke(
        &peers.hub,
        &[
            "chat",
            "invite",
            "buddy",
            "--hub",
            &peers.endpoint,
            "-m",
            welcome,
            "--json",
        ],
        b"",
    ));
    assert_eq!(
        resumed["welcome_id"], invite["welcome_id"],
        "the welcome send replays rather than duplicating"
    );
    let joined = success(&invoke(
        &peers.peer,
        &["chat", "join", "friend", "--json"],
        resumed["invite"].as_str().unwrap().as_bytes(),
    ));
    assert_eq!(joined["welcome"]["body"], welcome);
    assert!(
        !invoke(
            &peers.hub,
            &[
                "chat",
                "invite",
                "buddy",
                "--hub",
                &peers.endpoint,
                "--json",
            ],
            b""
        )
        .status
        .success(),
        "a completed alias is never reused"
    );
}

#[test]
#[ignore = "real paired QUIC hub and Bun-backed OpenCode adapter; e2e lane"]
fn chat_agent_adapter_receives_native_mail_and_replies_through_the_exact_record() {
    let peers = Peers::new();
    peers.connect();
    success(&invoke(
        &peers.hub,
        &[
            "chat",
            "send",
            "buddy",
            "-m",
            "please reply through the agent API",
            "--json",
        ],
        b"",
    ));
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let adapter = manifest
        .join("../../integrations/opencode-v2/src/mail.ts")
        .canonicalize()
        .unwrap();
    let source = format!(
        r#"
        const {{openCodeMail}} = await import({});
        const deadline = setTimeout(() => process.exit(3), 8000);
        let inbox;
        inbox = openCodeMail({{
            env: process.env,
            visibleSession: () => "receiving-session",
            sessions: {{synthetic: async (input) => {{
                if (!input.text.includes("please reply through the agent API")) throw Error("missing real message");
                if (input.sessionID !== "receiving-session" || input.resume !== true) throw Error("wrong receiver");
                const reply = Bun.spawnSync([process.env.PHUX_BIN, "chat", "send", "--record",
                    process.env.PHUX_CHAT_RECORD, "-m", "reply from the agent adapter", "--json"],
                    {{env: process.env}});
                if (reply.exitCode !== 0) throw Error(new TextDecoder().decode(reply.stderr));
                console.log(JSON.stringify({{delivered: true, via: "synthetic", resumesIdle: input.resume}}));
                clearTimeout(deadline);
                inbox.stop();
            }}}},
            onError: error => {{ console.error(error); process.exit(2); }},
        }});
        inbox.start();
    "#,
        serde_json::to_string(adapter.to_str().unwrap()).unwrap()
    );
    let delivered = success(&invoke(
        &peers.peer,
        &["chat", "run", "friend", "--", "bun", "-e", &source],
        b"",
    ));
    assert_eq!(delivered["delivered"], true);
    assert_eq!(delivered["resumesIdle"], true);
    assert!(has_body(
        &success(&invoke(
            &peers.hub,
            &["chat", "read", "buddy", "--json",],
            b""
        )),
        "reply from the agent adapter"
    ));
}
