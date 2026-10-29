//! OSC 7501 through a real child PTY and the production L3 dispatch loop.
//! Each script stage waits for a file release, then queries OSC 7501 followed
//! by DA in raw mode. Reading DA back proves the preceding bytes were parsed;
//! an idle fence record proves the latest metadata snapshot reached L3.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{
    Command, CommandResult, CommandValue, FrameKind, RESOURCE_AGENT_KEY,
    RESOURCE_PROGRAM_STATUS_KEY, RESOURCE_PROGRAM_STATUS_RECORD_PREFIX, ResourceLifecycle, Scope,
    StateScope,
};
use phux_server::state::RetainPolicy;
use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, ServerHandles, WIRE_RECV_TIMEOUT, attach_by_name, command,
    join_after_shutdown, recv_until, run_local, seed_pty, send_frame, spawn_server_with,
    wait_for_socket,
};
use portable_pty::CommandBuilder;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::time::{Instant, timeout};

const FENCE: &str = "_sync";

fn report(body: &str) -> Vec<u8> {
    format!("\x1b]7501;{body}\x1b\\").into_bytes()
}

fn reports(bodies: &[&str]) -> Vec<u8> {
    bodies.iter().flat_map(|body| report(body)).collect()
}

fn record_key(id: &str) -> String {
    format!("{RESOURCE_PROGRAM_STATUS_RECORD_PREFIX}{id}")
}

fn fenced(mut bytes: Vec<u8>, stage: usize) -> Vec<u8> {
    let title = STANDARD.encode(stage.to_string());
    bytes.extend(report(&format!("id={FENCE}:state=idle:title={title}")));
    bytes
}

/// Emit arbitrary bytes with POSIX printf, without shell interpolation.
fn printf(bytes: &[u8]) -> String {
    let mut octal = String::with_capacity(bytes.len() * 4);
    for byte in bytes {
        write!(&mut octal, "\\{byte:03o}").unwrap();
    }
    format!("printf '{octal}'\n")
}

struct Pane {
    server: ServerHandles,
    tmp: TempDir,
    stream: UnixStream,
    terminal: ResourceId,
    request: u32,
}

impl Pane {
    async fn start(stages: &[Vec<u8>], retained: bool) -> Self {
        Self::start_named(stages, retained, "generic-status").await
    }

    async fn start_named(stages: &[Vec<u8>], retained: bool, name: &str) -> Self {
        use std::os::unix::fs::PermissionsExt;

        let tmp = TempDir::new().unwrap();
        let mut script = String::from("#!/bin/sh\nstty raw -echo\n");
        for (stage, bytes) in stages.iter().enumerate() {
            let go = tmp.path().join(format!("go-{stage}"));
            let capture = tmp.path().join(format!("reply-{stage}"));
            let ack = tmp.path().join(format!("ack-{stage}"));
            writeln!(
                script,
                "while [ ! -f '{}' ]; do sleep 0.01; done",
                go.display()
            )
            .unwrap();
            script.push_str(&printf(bytes));
            script.push_str(&printf(b"\x1b]7501;?\x1b\\\x1b[c"));
            // Do not assume a particular DA payload or length. DA is the
            // only reply with a `[`, and the first `c` after it ends DA (the
            // feature reply's state list contains a `c`).
            write!(
                script,
                "da=\n\
                 while :; do\n\
                 byte=$(dd bs=1 count=1 2>/dev/null)\n\
                 printf '%s' \"$byte\" >> '{}'\n\
                 [ \"$byte\" = '[' ] && da=1\n\
                 [ -n \"$da\" ] && [ \"$byte\" = c ] && break\n\
                 done\n\
                 : > '{}'\n",
                capture.display(),
                ack.display(),
            )
            .unwrap();
        }
        writeln!(
            script,
            "while [ ! -f '{}' ]; do sleep 0.01; done\nexit 0",
            tmp.path().join("exit").display(),
        )
        .unwrap();
        let script_path = tmp.path().join(name);
        std::fs::write(&script_path, script).unwrap();
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let cmd = CommandBuilder::new(script_path);
        let socket = tmp.path().join("phux.sock");
        let server = spawn_server_with(socket.clone(), Some("demo"), |cfg| {
            seed_pty(cfg, cmd);
            cfg.retain = RetainPolicy {
                by_default: retained,
                ..RetainPolicy::default()
            };
        });
        let mut stream = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut stream, &attach_by_name("demo")).await;
        let terminal = recv_until(&mut stream, |_, frame| match frame {
            FrameKind::Attached { snapshot, .. } => Some(snapshot.focused_resource),
            _ => None,
        })
        .await;
        Self {
            server,
            tmp,
            stream,
            terminal,
            request: 100,
        }
    }

    async fn release(&self, stage: usize) -> Vec<u8> {
        std::fs::write(self.tmp.path().join(format!("go-{stage}")), b"go").unwrap();
        wait_file(&self.tmp.path().join(format!("ack-{stage}"))).await;
        std::fs::read(self.tmp.path().join(format!("reply-{stage}"))).unwrap()
    }

    const fn next_request(&mut self) -> u32 {
        self.request += 1;
        self.request
    }

    async fn get(&mut self, key: &str) -> Option<Value> {
        let request_id = self.next_request();
        send_frame(
            &mut self.stream,
            &FrameKind::GetMetadata {
                request_id,
                scope: Scope::Resource(self.terminal.clone()),
                key: key.to_owned(),
            },
        )
        .await;
        recv_until(&mut self.stream, |_, frame| match frame {
            FrameKind::MetadataValue {
                request_id: got,
                value,
            } if got == request_id => {
                Some(value.map(|bytes| serde_json::from_slice(&bytes).unwrap()))
            }
            _ => None,
        })
        .await
    }

    async fn wait_value(
        &mut self,
        key: &str,
        ready: impl Fn(&Option<Value>) -> bool + Send + Sync,
    ) -> Option<Value> {
        timeout(WIRE_RECV_TIMEOUT, async {
            loop {
                let value = self.get(key).await;
                if ready(&value) {
                    return value;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("metadata never converged for {key}"))
    }

    async fn stage(&mut self, stage: usize) {
        self.release(stage).await;
        let stage = stage.to_string();
        self.wait_value(&record_key(FENCE), |value| {
            value.as_ref().is_some_and(|value| value["title"] == stage)
        })
        .await;
    }

    async fn record(&mut self, id: &str) -> Value {
        self.get(&record_key(id)).await.expect("record must exist")
    }

    async fn summary(&mut self) -> Value {
        self.get(RESOURCE_PROGRAM_STATUS_KEY)
            .await
            .expect("summary must exist")
    }

    async fn assert_ids(&mut self, want: &[&str]) {
        let summary = self.summary().await;
        let mut got: Vec<_> = summary["record_ids"]
            .as_array()
            .expect("record_ids is an array")
            .iter()
            .map(|id| id.as_str().expect("ids are strings"))
            .filter(|id| *id != FENCE)
            .collect();
        got.sort_unstable();
        let mut want = want.to_vec();
        want.sort_unstable();
        assert_eq!(got, want, "{summary}");
    }

    async fn wait_badge(&mut self, state: &str, attention: &str) -> Value {
        self.wait_value(RESOURCE_AGENT_KEY, |value| {
            value.as_ref().is_some_and(|value| {
                let default = match state {
                    "idle" => "none",
                    "blocked" => "high",
                    "done" => "low",
                    _ => "normal",
                };
                value["state"] == state
                    && value["attention"].as_str().unwrap_or(default) == attention
            })
        })
        .await
        .unwrap()
    }

    async fn watcher(&self, key: &str) -> UnixStream {
        let socket: PathBuf = self.tmp.path().join("phux.sock");
        let mut stream = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
        send_frame(
            &mut stream,
            &FrameKind::SubscribeMetadata {
                scope: Scope::Resource(self.terminal.clone()),
                key: key.to_owned(),
            },
        )
        .await;
        // Same-connection GET proves subscription installation before release.
        send_frame(
            &mut stream,
            &FrameKind::GetMetadata {
                request_id: 1,
                scope: Scope::Resource(self.terminal.clone()),
                key: key.to_owned(),
            },
        )
        .await;
        recv_until(&mut stream, |_, frame| {
            matches!(frame, FrameKind::MetadataValue { request_id: 1, .. }).then_some(())
        })
        .await;
        stream
    }

    async fn stop(self) {
        drop(self.stream);
        join_after_shutdown(self.server.0, self.server.1).await;
    }
}

async fn wait_file(path: &Path) {
    let end = Instant::now() + Duration::from_secs(60);
    while !path.exists() {
        assert!(
            Instant::now() < end,
            "child never reached {}",
            path.display()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn changed(stream: &mut UnixStream, terminal: &ResourceId, key: &str) -> Option<Value> {
    recv_until(stream, |_, frame| match frame {
        FrameKind::MetadataChanged {
            scope,
            key: got,
            value,
            ..
        } if scope == Scope::Resource(terminal.clone()) && got == key => {
            Some(value.map(|bytes| serde_json::from_slice(&bytes).unwrap()))
        }
        _ => None,
    })
    .await
}

mod lifetime;
mod projection;
mod reports;
