//! `COMMAND` dispatch of the L3 session keys and verbs: create, rename, and detach-clients.

use super::*;

/// `SESSION_CREATE_KEY` seeds a session (listed by `GET_STATE`), publishes its
/// seed pane, installs resume provenance, honors a valid wire `cwd`, and falls
/// back (rather than failing) for a missing or unenterable one.
#[test]
fn session_create_via_metadata_seeds_session_and_validates_cwd() {
    use std::os::unix::fs::PermissionsExt;
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown, server) = spawn_server_seed_pty_no_cmd(socket_path.clone(), None);
        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;

        let agent_session =
            br#"{"plugin_id":"com.phux.agents","integration_id":"codex","native_id":"session-42"}"#
                .to_vec();
        let result = create_session(
            &mut stream,
            1,
            serde_json::json!({ "name": "scratch", "command": null, "cwd": null, "agent_session": agent_session }),
        )
        .await;
        assert_eq!(result["name"], "scratch");
        let scratch = created_terminal(&result);
        let provenance = get_metadata(
            &mut stream,
            3,
            Scope::Resource(scratch),
            RESOURCE_AGENT_SESSION_KEY,
        )
        .await;
        assert_eq!(
            provenance,
            Some(agent_session),
            "resume provenance installed with the seed pane"
        );

        let valid = TempDir::new().unwrap();
        let valid_path = valid.path().canonicalize().unwrap();
        let locked = tmp.path().join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let bogus = tmp.path().join("does-not-exist");
        let mut created = Vec::new();
        for (n, (name, cwd)) in [
            ("rooted", &valid_path),
            ("fallback", &bogus),
            ("locked-cwd", &locked),
        ]
        .into_iter()
        .enumerate()
        {
            let request_id = 10 + 2 * u32::try_from(n).unwrap();
            let request = serde_json::json!({ "name": name, "command": null, "cwd": cwd.display().to_string() });
            let result = create_session(&mut stream, request_id, request).await;
            assert_eq!(result["name"], name, "a bad cwd must not fail the create");
            created.push(created_terminal(&result));
        }
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).ok();

        let state = snapshot(&mut stream, 20).await;
        for name in ["scratch", "rooted", "fallback", "locked-cwd"] {
            assert!(session_names(&state).contains(&name), "{name}");
        }
        assert_eq!(
            pane_cwd(&state, &created[0]),
            valid_path,
            "a valid wire cwd is honored"
        );
        for id in &created[1..] {
            let cwd = pane_cwd(&state, id);
            assert!(
                cwd.is_dir() && cwd != bogus && cwd != locked,
                "fell back to a real dir: {}",
                cwd.display()
            );
        }

        drop(stream);
        join_after_shutdown(shutdown, server).await;
    });
}

/// Correlated create results are one-shot and connection-private: not listed,
/// not readable or deletable by another connection, consumed on read.
#[test]
fn correlated_session_create_results_cannot_reuse_another_creators_success() {
    run_local(async {
        let (server, mut winner) = spawn_server_connected(Some("work")).await;
        let mut loser = server.connect().await;
        let winner_token = "11111111-1111-4111-8111-111111111111";
        let loser_token = "22222222-2222-4222-8222-222222222222";
        let result_key = |token: &str| format!("{SESSION_CREATE_RESULT_KEY_PREFIX}{token}");

        for (stream, request_id, token, command) in [
            (&mut winner, 10, winner_token, "winner"),
            (&mut loser, 20, loser_token, "loser"),
        ] {
            let value = serde_json::json!({
                "name": "contended",
                "command": ["sh", "-c", format!("printf {command}")],
                "cwd": null,
                "request_token": token,
            });
            set_global(
                stream,
                request_id,
                SESSION_CREATE_KEY,
                serde_json::to_vec(&value).unwrap(),
            )
            .await;
            // A correlated reply orders the fire-and-forget write before it.
            let _ = snapshot(stream, request_id + 1).await;
        }

        send_frame(
            &mut winner,
            &FrameKind::ListMetadata {
                request_id: 29,
                scope: Scope::Global,
            },
        )
        .await;
        let listed = recv_until(&mut winner, |_, frame| match frame {
            FrameKind::MetadataKeys {
                request_id: 29,
                keys,
            } => Some(keys),
            _ => None,
        })
        .await;
        assert!(
            listed
                .iter()
                .all(|key| !key.starts_with(SESSION_CREATE_RESULT_KEY_PREFIX)),
            "{listed:?}"
        );
        assert!(
            get_metadata(&mut loser, 29, Scope::Global, &result_key(winner_token))
                .await
                .is_none()
        );
        send_frame(
            &mut loser,
            &FrameKind::DeleteMetadata {
                request_id: 30,
                scope: Scope::Global,
                key: result_key(winner_token),
            },
        )
        .await;
        let _ = snapshot(&mut loser, 31).await;

        let won = get_metadata(&mut winner, 30, Scope::Global, &result_key(winner_token))
            .await
            .expect("winner owns its correlated result");
        let won: serde_json::Value = serde_json::from_slice(&won).unwrap();
        assert_eq!(won["request_token"], winner_token);
        assert!(
            get_metadata(&mut loser, 31, Scope::Global, &result_key(loser_token))
                .await
                .is_none()
        );
        assert!(
            get_metadata(&mut winner, 32, Scope::Global, &result_key(winner_token))
                .await
                .is_none(),
            "consumed on read"
        );
    });
}

/// A headless create forwards its `env` to the seed process and arms
/// last-session self-exit like an attached client.
#[test]
fn headless_session_create_forwards_env_and_arms_last_session_exit() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let marker = tmp.path().join("seed-env");
        let (_shutdown, server) =
            spawn_server_seed_pty_no_cmd(socket_path.clone(), Some("bootstrap"));
        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;

        // Write-then-rename so the poll never sees a half-written file.
        let request = serde_json::json!({
            "name": "managed",
            "command": [
                "/bin/sh", "-c",
                "printf %s \"$GC_PHUX_TEST_VALUE\" > \"$1.partial\"; mv \"$1.partial\" \"$1\"; sleep 60",
                "sh", marker,
            ],
            "cwd": null,
            "env": { "GC_PHUX_TEST_VALUE": "forwarded" },
        });
        let _ = create_session(&mut stream, 10, request).await;
        let value = timeout(Duration::from_secs(2), async {
            loop {
                if let Ok(value) = tokio::fs::read_to_string(&marker).await {
                    break value;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("seed process wrote the env marker");
        assert_eq!(value, "forwarded");

        let state = snapshot(&mut stream, 12).await;
        assert_eq!(state.sessions.len(), 2, "bootstrap + managed");
        let kill = Command::KillResources {
            ids: state.resources.into_iter().map(|pane| pane.id).collect(),
            operation_id: None,
        };
        assert_eq!(command(&mut stream, 13, kill).await, CommandResult::Ok);
        drop(stream);
        timeout(Duration::from_secs(2), server)
            .await
            .expect("the server self-exits after its last session")
            .unwrap()
            .unwrap();
    });
}

/// An applied `SESSION_NAME_KEY` rename updates `GET_STATE` and fans out
/// `METADATA_CHANGED`; a refused (unknown session) or no-op rename sent first
/// does not (ordering, not timing, proves the suppression).
#[test]
fn session_rename_applies_and_broadcasts_only_applied_renames() {
    run_local(async {
        fn rename(current: &str, new_name: &str) -> Vec<u8> {
            [current.as_bytes(), b"\0", new_name.as_bytes()].concat()
        }
        let (server, mut subscriber) = spawn_server_connected(Some("work")).await;
        attach(&mut subscriber, "work").await;
        send_frame(
            &mut subscriber,
            &FrameKind::SubscribeMetadata {
                scope: Scope::Global,
                key: SESSION_NAME_KEY.to_owned(),
            },
        )
        .await;
        let _ = snapshot(&mut subscriber, 1).await; // barrier: subscription installed

        let mut renamer = server.connect().await;
        for (request_id, value) in [
            (10, rename("ghost", "phantom")),
            (11, rename("work", "work")),
            (12, rename("work", "renamed")),
        ] {
            set_global(&mut renamer, request_id, SESSION_NAME_KEY, value).await;
        }
        let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
        let (scope, key, value) =
            recv_until_deadline(&mut subscriber, deadline, |_, frame| match frame {
                FrameKind::MetadataChanged {
                    scope, key, value, ..
                } => Some((scope, key, value)),
                _ => None,
            })
            .await
            .expect("METADATA_CHANGED for the applied rename");
        assert_eq!((scope, key.as_str()), (Scope::Global, SESSION_NAME_KEY));
        assert_eq!(
            value,
            Some(rename("work", "renamed")),
            "the first broadcast is the applied rename"
        );
        let names = session_names(&snapshot(&mut renamer, 13).await)
            .iter()
            .map(|n| (*n).to_owned())
            .collect::<Vec<_>>();
        assert!(
            names.contains(&"renamed".to_owned()) && !names.contains(&"work".to_owned()),
            "{names:?}"
        );
    });
}

/// `DETACH_CLIENTS` from another connection detaches the session's clients
/// and reports how many; an unknown session reports zero.
#[test]
fn detach_clients_force_detaches_and_counts() {
    run_local(async {
        let (server, mut victim) = spawn_server_connected(Some("work")).await;
        attach(&mut victim, "work").await;
        let mut controller = server.connect().await;
        for (request_id, session, expected) in [(5, "work", "1"), (9, "nope", "0")] {
            let detach = Command::DetachClients {
                session: Some(session.to_owned()),
            };
            match command(&mut controller, request_id, detach).await {
                CommandResult::OkWith(CommandValue::Json(count)) => {
                    assert_eq!(count, expected, "{session}");
                }
                other => panic!("expected a count, got {other:?}"),
            }
        }
        assert!(matches!(
            recv_until_detached(&mut victim).await,
            FrameKind::Detached { .. }
        ));
    });
}
