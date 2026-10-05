//! `SPAWN_RESOURCE`: placement, the spawned child's environment and cwd,
//! `RESOURCE_CLOSED` on exit, `RESIZE_TERMINAL`, and `initial_size`.

use phux_protocol::ids::{GroupId, ResourceId};
use phux_protocol::input::key::PhysicalKey;
use phux_protocol::wire::frame::{
    FrameKind, RESOURCE_AGENT_SESSION_KEY, Scope, SpawnError, SpawnResult,
};
use phux_server_testkit::{Spawn, ascii_key, send_frame, spawn_resource};
use portable_pty::CommandBuilder;
use tempfile::TempDir;
use tokio::net::UnixStream;

use crate::common::{
    Seen, Server, attach, attach_create, create, find, get_metadata, next_event, output_containing,
    release, sh, spawned, state, subscribe, wait_frame,
};

/// A server with an attached client in `default`, so spawns auto-subscribe
/// the spawning client to the new pane.
async fn attached() -> (Server, UnixStream) {
    let server = Server::start(None, |_| {});
    let mut stream = server.connect().await;
    attach_create(&mut stream, "default", None, None).await;
    (server, stream)
}

/// The first `BOOTSTRAP_BEGIN` grid for `pane`.
async fn bootstrap_dims(stream: &mut UnixStream, pane: &ResourceId) -> (u16, u16) {
    wait_frame(stream, "BOOTSTRAP_BEGIN", |frame| match frame {
        FrameKind::BootstrapBegin {
            terminal_id,
            cols,
            rows,
            ..
        } if &terminal_id == pane => Some((cols, rows)),
        _ => None,
    })
    .await
}

#[test]
fn spawn_round_trips_input_and_publishes_agent_session_provenance() {
    phux_server_testkit::run_local(async {
        let (server, mut stream) = attached().await;
        let provenance = br#"{"plugin_id":"com.phux.agents","native_id":"session-42"}"#.to_vec();
        let spawn = Spawn {
            agent_session: Some(provenance.clone()),
            ..Spawn::command(&["/bin/cat"])
        };
        let pane = spawned(&mut stream, 42, spawn).await;
        assert!(pane.is_local());
        let scope = Scope::Resource(pane.clone());
        assert_eq!(
            get_metadata(&mut stream, 43, scope, RESOURCE_AGENT_SESSION_KEY).await,
            Some(provenance),
            "provenance is installed with the new pane"
        );

        let key = FrameKind::InputKey {
            terminal_id: pane.clone(),
            event: ascii_key('a', PhysicalKey::A),
        };
        send_frame(&mut stream, &key).await;
        release(&mut stream, &pane).await;
        output_containing(&mut stream, &pane, b"a").await;

        drop(stream);
        server.stop().await;
    });
}

/// Invalid requests fail cleanly and leave nothing behind: bad provenance,
/// an unknown group, and a child that cannot be exec'd (whose atomic
/// provenance must be reaped with it).
#[test]
fn invalid_spawns_are_refused_without_leaving_resources() {
    phux_server_testkit::run_local(async {
        let (server, mut stream) = attached().await;
        for (request_id, provenance) in [(50, Vec::new()), (51, vec![b'x'; 4097])] {
            let spawn = Spawn {
                agent_session: Some(provenance),
                ..Spawn::command(&["/bin/cat"])
            };
            let result = spawn_resource(&mut stream, request_id, spawn).await;
            assert!(
                matches!(&result, SpawnResult::Err(SpawnError::SpawnFailed(r)) if r.contains("1..=4096")),
                "{result:?}"
            );
        }

        let mut frame = Spawn::default().frame(52);
        if let FrameKind::SpawnResource { group, .. } = &mut frame {
            *group = GroupId::new(99_999);
        }
        send_frame(&mut stream, &frame).await;
        let result = wait_frame(&mut stream, "RESOURCE_SPAWNED", |frame| match frame {
            FrameKind::ResourceSpawned {
                request_id: 52,
                result,
            } => Some(result),
            _ => None,
        })
        .await;
        assert_eq!(result, SpawnResult::Err(SpawnError::GroupNotFound));

        let unspawnable = Spawn {
            agent_session: Some(br#"{"native_id":"never-live"}"#.to_vec()),
            ..Spawn::command(&["/definitely/not/a/phux-test-program"])
        };
        assert!(matches!(
            spawn_resource(&mut stream, 60, unspawnable).await,
            SpawnResult::Err(SpawnError::SpawnFailed(_))
        ));
        assert_eq!(
            state(&mut stream, 61).await.resources.len(),
            1,
            "only the seed pane remains"
        );

        drop(stream);
        server.stop().await;
    });
}

/// An attached client's split lands in its own session (never a new
/// `spawn-N` one), and an explicit `owner_terminal` wins over whichever
/// session was most recently active.
#[test]
fn spawn_placement_follows_the_attached_session_or_the_named_owner() {
    phux_server_testkit::run_local(async {
        let server = Server::start(None, |_| {});
        let mut first = server.connect().await;
        let owner = attach_create(&mut first, "first", None, None)
            .await
            .focused_resource;
        let split = spawned(&mut first, 7, Spawn::command(&["/bin/cat"])).await;
        let snapshot = state(&mut first, 8).await;
        let names: Vec<&str> = snapshot.sessions.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["first"], "a split must not create a session");
        assert_eq!(snapshot.resources.len(), 2);
        assert!(find(&snapshot, &split).is_some());

        let mut second = server.connect().await;
        attach_create(&mut second, "second", None, None).await;
        let mut headless = server.connect().await;
        let spawn = Spawn {
            owner_terminal: Some(owner.clone()),
            ..Spawn::command(&["/bin/cat"])
        };
        let placed = spawned(&mut headless, 50, spawn).await;
        let snapshot = state(&mut headless, 51).await;
        assert_eq!(
            find(&snapshot, &placed).unwrap().window_id,
            find(&snapshot, &owner).unwrap().window_id,
            "the owner's window wins over the most recently active session"
        );

        drop((first, second, headless));
        server.stop().await;
    });
}

/// `TERM` precedence: wire `env` > the `term` field > `defaults.term`
/// (`xterm-256color`).
#[test]
fn spawned_term_follows_env_then_field_then_default() {
    phux_server_testkit::run_local(async {
        let (server, mut stream) = attached().await;
        let env = |value: &str| Some(vec![("TERM".to_owned(), value.to_owned())]);
        let cases = [
            (env("phux-spawn-override"), None, "phux-spawn-override"),
            (None, None, "xterm-256color"),
            (None, Some("phux-term-field"), "phux-term-field"),
            (
                env("phux-env-wins"),
                Some("phux-term-field"),
                "phux-env-wins",
            ),
        ];
        for (request_id, (env, term, expected)) in (10..).zip(cases) {
            let spawn = Spawn {
                env,
                term: term.map(str::to_owned),
                ..sh("read _; printf 'TERMIS=%s.\\n' \"$TERM\"; read _")
            };
            let pane = spawned(&mut stream, request_id, spawn).await;
            release(&mut stream, &pane).await;
            output_containing(&mut stream, &pane, format!("TERMIS={expected}.").as_bytes()).await;
        }

        drop(stream);
        server.stop().await;
    });
}

#[test]
fn pty_exit_emits_resource_closed_with_the_status() {
    phux_server_testkit::run_local(async {
        let (server, mut stream) = attached().await;
        let pane = spawned(&mut stream, 1, sh("read _; exit 42")).await;
        release(&mut stream, &pane).await;
        let status = wait_frame(&mut stream, "RESOURCE_CLOSED", |frame| match frame {
            FrameKind::ResourceClosed {
                terminal_id,
                exit_status,
                ..
            } if terminal_id == pane => Some(exit_status),
            _ => None,
        })
        .await;
        assert_eq!(status, Some(42));

        drop(stream);
        server.stop().await;
    });
}

/// `RESIZE_TERMINAL` updates the registry dims a later attach reports; a
/// correlated `GET_STATE` on the same connection orders after it.
#[test]
fn resize_terminal_updates_reported_dims() {
    phux_server_testkit::run_local(async {
        let (server, mut stream) = attached().await;
        let pane = spawned(&mut stream, 1, Spawn::command(&["/bin/cat"])).await;
        let resize = FrameKind::ResizeTerminal {
            terminal_id: pane.clone(),
            cols: 120,
            rows: 40,
            cell_px: None,
        };
        send_frame(&mut stream, &resize).await;
        state(&mut stream, 2).await;

        let mut other = server.connect().await;
        let snapshot = attach(&mut other, "default").await;
        let info = find(&snapshot, &pane).expect("the pane in the re-attach snapshot");
        assert_eq!((info.cols, info.rows), (120, 40));

        drop((stream, other));
        server.stop().await;
    });
}

/// Session viewport votes must reach subscribed panes even when registry
/// focus points elsewhere. Both the replacement generation and the child's
/// real `stty` readback defend this wire path.
#[test]
fn viewport_resize_reaches_nonactive_panes_and_preserves_noop_generations() {
    phux_server_testkit::run_local(async {
        let (server, mut stream) = attached().await;
        let first = spawned(
            &mut stream,
            31,
            sh("while :; do stty size; sleep 0.02; done"),
        )
        .await;
        let second = spawned(
            &mut stream,
            32,
            sh("while :; do stty size; sleep 0.02; done"),
        )
        .await;
        // Two subscribed panes necessarily include a non-active pane; do
        // not change registry focus to make the resize accidentally work.
        state(&mut stream, 33).await;
        let panes = [first, second];
        let viewport = FrameKind::ViewportResize {
            viewport: phux_protocol::wire::frame::ViewportInfo::new(137, 53),
        };
        send_frame(&mut stream, &viewport).await;
        let mut generations = [None, None];
        let mut ready = [false, false];
        let mut output = [Vec::new(), Vec::new()];
        let deadline = tokio::time::Instant::now() + phux_server_testkit::WIRE_RECV_TIMEOUT;
        let reached =
            phux_server_testkit::recv_until_deadline(&mut stream, deadline, |_, frame| {
                match frame {
                    FrameKind::BootstrapBegin {
                        terminal_id,
                        stream_id,
                        bootstrap_id,
                        cols: 137,
                        rows: 53,
                        ..
                    } => {
                        if let Some(index) = panes.iter().position(|pane| pane == &terminal_id) {
                            generations[index] = Some((stream_id, bootstrap_id));
                            ready[index] = false;
                        }
                    }
                    FrameKind::BootstrapReady {
                        stream_id,
                        bootstrap_id,
                        ..
                    } => {
                        if let Some(index) = generations
                            .iter()
                            .position(|generation| *generation == Some((stream_id, bootstrap_id)))
                        {
                            ready[index] = true;
                        }
                    }
                    FrameKind::ResourceOutput {
                        terminal_id, bytes, ..
                    } => {
                        if let Some(index) = panes.iter().position(|pane| pane == &terminal_id) {
                            output[index].extend_from_slice(&bytes);
                        }
                    }
                    _ => {}
                }
                (ready.iter().all(|ready| *ready)
                    && output
                        .iter()
                        .all(|bytes| bytes.windows(6).any(|part| part == b"53 137")))
                .then_some(())
            })
            .await;
        assert!(
            reached.is_some(),
            "both panes need a replacement grid and real PTY readback"
        );

        send_frame(&mut stream, &viewport).await;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(200);
        phux_server_testkit::recv_until_deadline(&mut stream, deadline, |_, frame| {
            assert!(
                !matches!(frame, FrameKind::BootstrapBegin { ref terminal_id, .. }
                if panes.contains(terminal_id)),
                "same geometry must not replace a generation"
            );
            None::<()>
        })
        .await;
        drop(stream);
        server.stop().await;
    });
}

/// `initial_size` builds the first bootstrap generation at that grid (no
/// capture-then-resize); absent or with a zero axis the 80x24 default holds.
#[test]
fn initial_size_sets_the_first_bootstrap_grid() {
    phux_server_testkit::run_local(async {
        let (server, mut stream) = attached().await;
        for (request_id, size, expected) in [
            (11, Some((132, 43)), (132, 43)),
            (21, None, (80, 24)),
            (23, Some((0, 43)), (80, 24)),
        ] {
            let spawn = Spawn {
                initial_size: size,
                ..Spawn::command(&["/bin/cat"])
            };
            let pane = spawned(&mut stream, request_id, spawn).await;
            assert_eq!(
                bootstrap_dims(&mut stream, &pane).await,
                expected,
                "{size:?}"
            );
            let info_dims = {
                let snapshot = state(&mut stream, request_id + 100).await;
                let info = find(&snapshot, &pane).unwrap();
                (info.cols, info.rows)
            };
            assert_eq!(info_dims, expected, "registry dims match the actor's grid");
        }

        drop(stream);
        server.stop().await;
    });
}

/// Both the attach-create seed path and the `SPAWN_RESOURCE` path inject
/// `PHUX_TERMINAL_ID` (the pane's own wire id) and `PHUX_SOCKET` (this
/// server), and a `CreateIfMissing` seeds its pane in the wire `cwd`.
#[test]
fn panes_see_their_own_id_the_server_socket_and_the_wire_cwd() {
    phux_server_testkit::run_local(async {
        let server = Server::pty(None);
        let cwd = TempDir::new().unwrap();
        let cwd = cwd.path().canonicalize().unwrap();
        // Compare in-pane so a long socket path cannot soft-wrap the needle.
        let probe = format!(
            "printf 'PTID=%s.\\n' \"$PHUX_TERMINAL_ID\"; \
             [ \"$PHUX_SOCKET\" = '{}' ] && printf 'SOCKOK.\\n'; pwd; read _",
            server.socket.display()
        );
        let mut stream = server.connect().await;
        let snapshot = attach_create(
            &mut stream,
            "seeded",
            Some(vec!["/bin/sh".to_owned(), "-c".to_owned(), probe.clone()]),
            Some(cwd.to_string_lossy().into_owned()),
        )
        .await;
        let seed = snapshot.focused_resource;
        // `pwd` prints last, so the earlier lines are already in the text.
        let text = output_containing(&mut stream, &seed, cwd.to_str().unwrap().as_bytes()).await;
        let local = seed.local_id().unwrap();
        assert!(
            text.contains(&format!("PTID={local}.")) && text.contains("SOCKOK."),
            "{text:?}"
        );

        let pane = spawned(&mut stream, 55, sh(&format!("read _; {probe}"))).await;
        release(&mut stream, &pane).await;
        let text = output_containing(&mut stream, &pane, b"SOCKOK.").await;
        let local = pane.local_id().unwrap();
        assert!(text.contains(&format!("PTID={local}.")), "{text:?}");

        drop(stream);
        server.stop().await;
    });
}

/// With `cwd` unset, a spawn opens in the directory each
/// `defaults.cwd-inheritance` mode names; the seed pane sits in `dir`, so
/// every mode resolves there, and never to `$HOME` or the server's cwd.
#[test]
fn cwd_inheritance_modes_open_spawns_in_the_seed_panes_directory() {
    use phux_config::CwdInheritance;
    phux_server_testkit::run_local(async {
        for mode in [
            CwdInheritance::InheritFocused,
            CwdInheritance::SessionRoot,
            CwdInheritance::LastCwdPerWindow,
        ] {
            let dir = TempDir::new().unwrap();
            let dir = dir.path().canonicalize().unwrap();
            let mut seed = CommandBuilder::new("/bin/sh");
            seed.args(["-c", &format!("cd '{}' && read _", dir.display())]);
            let server = Server::start(Some("main"), |cfg| {
                phux_server_testkit::seed_pty(cfg, seed);
                cfg.cwd_inheritance = mode;
            });
            let mut stream = server.connect().await;
            attach(&mut stream, "main").await;
            // Let the seed shell run its `cd` before the server queries it.
            tokio::time::sleep(std::time::Duration::from_millis(75)).await;
            let pane = spawned(&mut stream, 1, sh("read _; pwd; read _")).await;
            release(&mut stream, &pane).await;
            output_containing(&mut stream, &pane, dir.to_str().unwrap().as_bytes()).await;
            drop(stream);
            server.stop().await;
        }
    });
}

/// A new session's seed pane announces itself with `pane_spawned` to a
/// server-wide follower, on both creation paths: the headless
/// `phux.session.create/v1` write (`phux new`) and an attach `CreateIfMissing`.
#[test]
fn a_new_sessions_seed_pane_is_announced_to_server_wide_watchers() {
    phux_server_testkit::run_local(async {
        let server = Server::pty(None);
        let mut watcher = server.connect().await;
        subscribe(&mut watcher, 1, None, None).await;
        let spawned_event = |e: &Seen| {
            matches!(
                e.event,
                phux_protocol::wire::frame::AgentEvent::ResourceSpawned { .. }
            )
        };

        let mut creator = server.connect().await;
        let body = serde_json::json!({ "name": "scratch", "command": ["/bin/sh", "-c", "read _"] });
        let result = create(&mut creator, 1, body).await;
        let seed =
            ResourceId::local(u32::try_from(result["terminal_id"].as_u64().unwrap()).unwrap());
        assert_eq!(
            next_event(&mut watcher, spawned_event).await.terminal,
            Some(seed)
        );

        let mut joiner = server.connect().await;
        let snapshot = attach_create(
            &mut joiner,
            "made-on-attach",
            Some(vec!["/bin/sh".into(), "-c".into(), "read _".into()]),
            None,
        )
        .await;
        let seed = snapshot.focused_resource;
        assert_eq!(
            next_event(&mut watcher, spawned_event).await.terminal,
            Some(seed)
        );

        drop((watcher, creator, joiner));
        server.stop().await;
    });
}
