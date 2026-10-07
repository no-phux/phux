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
            // Do not assume a particular DA payload or length. The final `c`
            // terminates DA, and OSC's fixed query response contains no `c`.
            write!(
                script,
                "while :; do\n\
                 byte=$(dd bs=1 count=1 2>/dev/null)\n\
                 printf '%s' \"$byte\" >> '{}'\n\
                 [ \"$byte\" = c ] && break\n\
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

#[test]
fn feature_reply_precedes_device_attributes_and_does_not_create_a_record() {
    run_local(async {
        let mut pane = Pane::start(&[Vec::new()], false).await;
        let bytes = pane.release(0).await;
        let tail = bytes
            .strip_prefix(b"\x1b]7501;?")
            .expect("OSC feature detection reply must arrive before DA");
        let da = tail
            .strip_prefix(b"\x1b\\")
            .or_else(|| tail.strip_prefix(b"\x07"))
            .expect("feature reply has an OSC terminator");
        assert!(
            da.starts_with(b"\x1b["),
            "DA follows feature detection: {bytes:?}"
        );
        assert!(da.ends_with(b"c"));
        assert_eq!(pane.get(RESOURCE_PROGRAM_STATUS_KEY).await, None);
        assert_eq!(pane.get(&record_key("")).await, None);
        pane.stop().await;
    });
}

#[test]
fn hierarchy_replacement_inheritance_and_subtree_clear_are_visible_in_l3() {
    run_local(async {
        let stages = [
            fenced(
                reports(&[
                    "state=idle:app=cargo:title=Um9vdA==",
                    "id=build:state=working:app=builder:title=QnVpbGQ=",
                    "id=build/test:state=blocked:kind=permission:progress=45:msg=QXBwcm92ZT8=",
                    "id=build.other:state=done:app=independent",
                    "id=orphan/leaf:state=working",
                ]),
                0,
            ),
            fenced(report("id=build:state=idle"), 1),
            fenced(report("state=idle:app=terraform"), 2),
            fenced(
                reports(&["id=build:state=clear", "id=orphan:state=clear"]),
                3,
            ),
            report("state=clear"),
        ];
        let mut pane = Pane::start(&stages, false).await;
        let mut watcher = pane.watcher(&record_key("build/test")).await;
        pane.stage(0).await;
        pane.assert_ids(&["", "build", "build/test", "build.other", "orphan/leaf"])
            .await;
        let child = pane.record("build/test").await;
        assert_eq!(
            child,
            json!({
                "id": "build/test", "state": "blocked", "kind": "permission",
                "progress": 45, "app": "builder", "msg": "Approve?"
            })
        );
        assert_eq!(pane.summary().await["active"], child);
        assert_eq!(
            changed(&mut watcher, &pane.terminal, &record_key("build/test")).await,
            Some(child)
        );
        assert_eq!(pane.record("").await["id"], "");

        pane.stage(1).await;
        assert_eq!(
            pane.record("build").await,
            json!({"id": "build", "state": "idle", "app": "cargo"})
        );
        assert_eq!(pane.record("build/test").await["app"], "cargo");
        pane.stage(2).await;
        assert_eq!(pane.record("build/test").await["app"], "terraform");
        assert_eq!(
            pane.record("orphan/leaf").await["app"],
            "terraform",
            "parents need not exist"
        );
        assert_eq!(
            pane.record("").await.get("title"),
            None,
            "replacement removes omitted keys"
        );
        pane.stage(3).await;
        pane.assert_ids(&["", "build.other"]).await;
        assert_eq!(pane.get(&record_key("build")).await, None);
        assert_eq!(pane.get(&record_key("build/test")).await, None);
        assert_eq!(pane.get(&record_key("orphan/leaf")).await, None);
        // Ancestor app changes may have queued earlier updates; consume through
        // the deletion instead of asserting an incidental number of broadcasts.
        loop {
            if changed(&mut watcher, &pane.terminal, &record_key("build/test"))
                .await
                .is_none()
            {
                break;
            }
        }
        assert_eq!(pane.summary().await["active"]["id"], "build.other");
        pane.release(4).await;
        pane.wait_value(RESOURCE_PROGRAM_STATUS_KEY, Option::is_none)
            .await;
        assert_eq!(pane.get(&record_key("")).await, None);
        assert_eq!(pane.get(&record_key("build.other")).await, None);
        drop(watcher);
        pane.stop().await;
    });
}

#[test]
fn invalid_reports_are_atomic_but_malformed_pairs_and_unknown_keys_are_skipped() {
    run_local(async {
        let initial =
            "id=task:state=blocked:kind=auth:progress=12:app=deploy:title=TG9naW4=:msg=VG9rZW4/";
        let mut invalid = reports(&[
            "id=task:state=working:msg=A===",
            "id=task:state=working:msg=AA==",
            "id=task:state=working:msg=/w==",
            "id=task:state=working:msg=woA=",
            "id=task:state=working:title=wqEAcQ==",
            "id=task:state=future:app=changed",
            "id=task:app=changed",
            "id=/task:state=clear",
            "id=task/:state=clear",
            "id=task//child:state=working",
            "id=a/b/c/d/e/f/g/h/i:state=working",
            "id=task:state=clear:msg=AA==",
        ]);
        invalid.extend(report(&format!(
            "id=task:state=working:title={}",
            STANDARD.encode("x".repeat(193))
        )));
        invalid.extend(report(&format!(
            "id=task:state=clear:msg={}",
            STANDARD.encode("x".repeat(2049))
        )));
        invalid.extend(report(&format!("id={}:state=working", "x".repeat(33))));
        invalid.extend(report(&format!(
            "id={}:state=working",
            vec!["x".repeat(32); 4].join("/")
        )));
        invalid.extend(report(&format!(
            "id=task:state=working:{}=value",
            "k".repeat(17)
        )));
        invalid.extend(report(&format!(
            "id=task:state=working:unknown={}",
            "x".repeat(4096)
        )));
        let valid = "id=task:state=working:broken:key=not@legal:state=blocked:kind=future:progress=101:app=bad/name:unknown=ignored:msg=SGVsbG8";
        let mut pane = Pane::start(
            &[
                fenced(reports(&["state=done:app=root", initial]), 0),
                fenced(invalid, 1),
                fenced(report(valid), 2),
                fenced(report("id=task:state=done:kind=permission:progress=50"), 3),
            ],
            false,
        )
        .await;
        pane.stage(0).await;
        let before = pane.record("task").await;
        let root = pane.record("").await;
        pane.stage(1).await;
        pane.assert_ids(&["", "task"]).await;
        assert_eq!(pane.record("task").await, before);
        assert_eq!(
            pane.record("").await,
            root,
            "invalid id must never fall back to root"
        );
        pane.stage(2).await;
        assert_eq!(
            pane.record("task").await,
            json!({
                "id": "task", "state": "blocked", "app": "root", "msg": "Hello"
            })
        );
        pane.stage(3).await;
        assert_eq!(
            pane.record("task").await,
            json!({
                "id": "task", "state": "done", "app": "root"
            })
        );
        pane.stop().await;
    });
}

#[test]
fn a_shell_prompt_expires_live_work_but_preserves_results() {
    run_local(async {
        let mut prompt = b"\x1b]133;A\x07".to_vec();
        prompt.extend(b"$ ");
        let mut pane = Pane::start(
            &[
                fenced(
                    reports(&[
                        "id=running:state=working:progress=20",
                        "id=waiting:state=blocked:kind=question",
                        "id=success:state=done:msg=UmVhZHk=",
                        "id=failure:state=error:msg=RmFpbGVk",
                    ]),
                    0,
                ),
                fenced(prompt, 1),
            ],
            false,
        )
        .await;
        pane.stage(0).await;
        let success = pane.record("success").await;
        let failure = pane.record("failure").await;
        pane.stage(1).await;
        pane.assert_ids(&["success", "failure"]).await;
        assert_eq!(pane.get(&record_key("running")).await, None);
        assert_eq!(pane.get(&record_key("waiting")).await, None);
        assert_eq!(pane.record("success").await, success);
        assert_eq!(pane.record("failure").await, failure);
        assert_eq!(pane.summary().await["active"], failure);
        pane.stop().await;
    });
}

#[test]
fn soft_reset_and_alternate_screens_preserve_records_but_ris_clears_them() {
    run_local(async {
        let mut pane = Pane::start(
            &[
                fenced(
                    reports(&["state=working:app=generic", "id=child:state=done"]),
                    0,
                ),
                fenced(b"\x1b[?1049h\x1b[!p".to_vec(), 1),
                fenced(b"\x1b[?1049l".to_vec(), 2),
                b"\x1bc".to_vec(),
            ],
            false,
        )
        .await;
        pane.stage(0).await;
        let root = pane.record("").await;
        let child = pane.record("child").await;
        for stage in 1..=2 {
            pane.stage(stage).await;
            pane.assert_ids(&["", "child"]).await;
            assert_eq!(pane.record("").await, root);
            assert_eq!(pane.record("child").await, child);
        }
        let mut watcher = pane.watcher(RESOURCE_PROGRAM_STATUS_KEY).await;
        pane.release(3).await;
        assert_eq!(
            changed(&mut watcher, &pane.terminal, RESOURCE_PROGRAM_STATUS_KEY).await,
            None
        );
        assert_eq!(pane.get(RESOURCE_PROGRAM_STATUS_KEY).await, None);
        assert_eq!(pane.get(&record_key("")).await, None);
        assert_eq!(pane.get(&record_key("child")).await, None);
        drop(watcher);
        pane.stop().await;
    });
}

#[test]
fn clients_cannot_mutate_the_summary_or_record_namespace() {
    run_local(async {
        let mut pane =
            Pane::start(&[fenced(report("state=blocked:app=terraform"), 0)], false).await;
        pane.stage(0).await;
        for key in [
            RESOURCE_PROGRAM_STATUS_KEY.to_owned(),
            record_key(""),
            record_key("forged"),
        ] {
            let before = pane.get(&key).await;
            let request_id = pane.next_request();
            send_frame(
                &mut pane.stream,
                &FrameKind::SetMetadata {
                    request_id,
                    scope: Scope::Resource(pane.terminal.clone()),
                    key: key.clone(),
                    value: br#"{"id":"forged","state":"done"}"#.to_vec(),
                },
            )
            .await;
            assert_eq!(pane.get(&key).await, before, "SET must not land: {key}");
            let request_id = pane.next_request();
            send_frame(
                &mut pane.stream,
                &FrameKind::DeleteMetadata {
                    request_id,
                    scope: Scope::Resource(pane.terminal.clone()),
                    key: key.clone(),
                },
            )
            .await;
            assert_eq!(pane.get(&key).await, before, "DELETE must not land: {key}");
        }
        pane.assert_ids(&[""]).await;
        pane.stop().await;
    });
}

#[test]
fn generic_programs_project_priority_and_attention_into_existing_badges() {
    run_local(async {
        let mut pane = Pane::start(
            &[
                fenced(report("state=working:app=terraform:progress=10"), 0),
                fenced(report("id=result:state=done"), 1),
                fenced(report("id=failure:state=error"), 2),
                fenced(report("id=approval:state=blocked:kind=permission"), 3),
                fenced(report("id=question:state=blocked:kind=question"), 4),
                fenced(report("id=approval:state=blocked:kind=auth"), 5),
                fenced(
                    reports(&[
                        "id=approval:state=clear",
                        "id=question:state=clear",
                        "id=failure:state=clear",
                        "id=result:state=clear",
                        "state=idle:app=terraform",
                    ]),
                    6,
                ),
            ],
            false,
        )
        .await;
        for (stage, id, state, attention) in [
            (0, "", "working", "normal"),
            (1, "result", "done", "low"),
            (2, "failure", "done", "high"),
            (3, "approval", "blocked", "high"),
            (4, "question", "blocked", "high"),
            (5, "approval", "blocked", "high"),
        ] {
            pane.stage(stage).await;
            assert_eq!(pane.summary().await["active"]["id"], id);
            let badge = pane.wait_badge(state, attention).await;
            assert_eq!(
                badge["name"], "terraform",
                "generic apps need no agent manifest"
            );
        }
        pane.stage(6).await;
        pane.wait_badge("idle", "none").await;
        pane.stop().await;
    });
}

#[test]
fn a_retained_terminal_expires_work_at_exit_and_keeps_done_and_error_records() {
    run_local(async {
        let mut pane = Pane::start(
            &[fenced(
                reports(&[
                    "id=running:state=working:app=generic",
                    "id=waiting:state=blocked:kind=question",
                    "id=success:state=done:msg=UmVhZHk=",
                    "id=failure:state=error:msg=RmFpbGVk",
                ]),
                0,
            )],
            true,
        )
        .await;
        // The sole session terminal is deliberately respawned as a shell.
        // Keep another terminal alive so this one exercises retained exit.
        let request_id = pane.next_request();
        let keeper = phux_server_testkit::Spawn {
            owner_terminal: Some(pane.terminal.clone()),
            ..phux_server_testkit::Spawn::command(&["/bin/sh", "-c", "exec sleep 600"])
        };
        assert!(matches!(
            phux_server_testkit::spawn_resource(&mut pane.stream, request_id, keeper).await,
            phux_protocol::wire::frame::SpawnResult::Ok(_)
        ));
        pane.stage(0).await;
        let success = pane.record("success").await;
        let failure = pane.record("failure").await;
        std::fs::write(pane.tmp.path().join("exit"), b"exit").unwrap();
        timeout(WIRE_RECV_TIMEOUT, async {
            loop {
                let request_id = pane.next_request();
                let result = command(
                    &mut pane.stream,
                    request_id,
                    Command::GetState {
                        scope: StateScope::Server,
                    },
                )
                .await;
                let CommandResult::OkWith(CommandValue::State(snapshot)) = result else {
                    panic!("GET_STATE failed: {result:?}");
                };
                if snapshot.resources.iter().any(|resource| {
                    resource.id == pane.terminal && resource.lifecycle == ResourceLifecycle::Exited
                }) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("pane must remain in inventory after exit");
        pane.wait_value(&record_key("running"), Option::is_none)
            .await;
        assert_eq!(pane.get(&record_key("waiting")).await, None);
        assert_eq!(pane.record("success").await, success);
        assert_eq!(pane.record("failure").await, failure);
        assert_eq!(pane.summary().await["active"], failure);
        pane.stop().await;
    });
}

#[test]
fn program_status_beats_a_live_screen_heuristic_but_not_an_explicit_state() {
    run_local(async {
        // The same permission dialog used by agent_detect.rs, kept on screen
        // while OSC reports change. First prove the real detector recognizes it.
        let rule = "─".repeat(20);
        let dialog = format!(
            "\x1b[2J\x1b[Htranscript\n\n{rule}\n Bash command\n\n\
             touch /tmp/probe.txt\n\n Do you want to proceed?\n\
             ❯ 1. Yes\n 2. Yes, and always allow access\n 3. No\n\n Esc to cancel\n"
        );
        let mut pane = Pane::start_named(
            &[
                dialog.into_bytes(),
                fenced(report("state=done:app=claude:msg=UmVhZHk="), 1),
                fenced(report("state=working:app=claude:progress=75"), 2),
            ],
            false,
            "claude",
        )
        .await;
        pane.release(0).await;
        pane.wait_badge("blocked", "high").await;
        pane.stage(1).await;
        pane.wait_badge("done", "low").await;
        assert_eq!(pane.record("").await["state"], "done");

        let request_id = pane.next_request();
        send_frame(&mut pane.stream, &FrameKind::SetMetadata {
            request_id,
            scope: Scope::Resource(pane.terminal.clone()),
            key: RESOURCE_AGENT_KEY.to_owned(),
            value: br#"{"name":"pinned","kind":"claude","state":"blocked","attention":"high","session":"manual"}"#.to_vec(),
        }).await;
        pane.wait_value(RESOURCE_AGENT_KEY, |value| {
            value
                .as_ref()
                .is_some_and(|value| value["name"] == "pinned" && value["state"] == "blocked")
        })
        .await;
        pane.stage(2).await;
        assert_eq!(
            pane.record("").await["state"],
            "working",
            "raw reports remain independent"
        );
        let declared = pane.get(RESOURCE_AGENT_KEY).await.unwrap();
        assert_eq!(declared["name"], "pinned");
        assert_eq!(declared["kind"], "claude");
        assert_eq!(declared["state"], "blocked");
        assert_eq!(declared["attention"], "high");
        assert_eq!(declared["session"], "manual");
        pane.stop().await;
    });
}

#[test]
fn bidi_formatting_stays_in_raw_metadata_but_is_disarmed_in_badge_names() {
    run_local(async {
        let title = "deploy \u{202e}prod\u{202c}";
        let body = format!(
            "state=blocked:app=terraform:title={}",
            STANDARD.encode(title)
        );
        let blank_title = " \u{202e}\u{202c} ";
        let blank_body = format!(
            "state=blocked:app=terraform:title={}",
            STANDARD.encode(blank_title)
        );
        let mut pane = Pane::start(
            &[fenced(report(&body), 0), fenced(report(&blank_body), 1)],
            false,
        )
        .await;
        pane.stage(0).await;
        assert_eq!(pane.record("").await["title"], title);
        assert_eq!(pane.summary().await["active"]["title"], title);
        let badge = pane.wait_badge("blocked", "high").await;
        let name = badge["name"].as_str().expect("badge has a display name");
        assert_eq!(
            name, "deploy prod",
            "disarming controls must preserve label content"
        );
        assert!(
            !name.contains('\u{202e}'),
            "untrusted direction override must not reach chrome"
        );
        assert!(
            !name.contains('\u{202c}'),
            "untrusted direction control must not reach chrome"
        );
        pane.stage(1).await;
        assert_eq!(pane.record("").await["title"], blank_title);
        let badge = pane.wait_badge("blocked", "high").await;
        assert_eq!(
            badge["name"], "terraform",
            "a blank label must not erase the status badge"
        );
        pane.stop().await;
    });
}

#[test]
fn live_agent_stream_keeps_precedence_and_program_status_resumes_without_a_report() {
    run_local(async {
        for finish in [true, false] {
            let mut pane = Pane::start_named(
                &[fenced(report("state=error:app=cargo:msg=RmFpbGVk"), 0)],
                false,
                "claude",
            )
            .await;
            pane.stage(0).await;
            pane.wait_badge("done", "high").await;
            let raw = pane.summary().await;
            let request_id = pane.next_request();
            let spawn = phux_server_testkit::Spawn {
                resource: Some(Box::new(
                    phux_protocol::wire::frame::SpawnResource::agent_session(
                        pane.terminal.clone(),
                        "claude",
                    ),
                )),
                ..phux_server_testkit::Spawn::default()
            };
            let phux_protocol::wire::frame::SpawnResult::Ok(session) =
                phux_server_testkit::spawn_resource(&mut pane.stream, request_id, spawn).await
            else {
                panic!("agent session spawn failed");
            };
            let request_id = pane.next_request();
            let result = command(
                &mut pane.stream,
                request_id,
                Command::AppendResourceOutput {
                    terminal_id: session.clone(),
                    bytes: b"{\"type\":\"prompt\"}\n".to_vec(),
                },
            )
            .await;
            assert!(matches!(result, CommandResult::OkWith(_)), "{result:?}");
            pane.wait_badge("working", "normal").await;
            assert_eq!(
                pane.summary().await,
                raw,
                "the stream does not replace OSC records"
            );
            let request_id = pane.next_request();
            let ending = if finish {
                Command::AppendResourceOutput {
                    terminal_id: session,
                    bytes: b"{\"type\":\"session_end\"}\n".to_vec(),
                }
            } else {
                Command::KillResource {
                    terminal_id: session,
                    operation_id: None,
                }
            };
            let result = command(&mut pane.stream, request_id, ending).await;
            assert!(
                matches!(result, CommandResult::Ok | CommandResult::OkWith(_)),
                "{result:?}"
            );
            pane.wait_badge("done", "high").await;
            assert_eq!(pane.summary().await, raw);
            pane.stop().await;
        }
    });
}
