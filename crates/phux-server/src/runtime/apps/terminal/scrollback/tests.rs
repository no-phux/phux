use std::os::unix::fs::PermissionsExt as _;

use phux_core::{Actor, ExitPolicy, TerminalLaunch};

use super::*;

fn state(restore: phux_config::RestoreCfg) -> SharedState {
    let state = SharedState::new();
    state.with_mut(|s| s.config_mut().restore = restore);
    state
}

fn keeping(lines: u32) -> phux_config::RestoreCfg {
    phux_config::RestoreCfg {
        scrollback: lines,
        ..phux_config::RestoreCfg::default()
    }
}

fn pane(state: &SharedState, session: &str, argv: &[&str]) -> (ResourceId, ResourceKey) {
    state.with_mut(|s| {
        let (_, id) = s
            .create_terminal(
                crate::state::TerminalTarget::NewSession {
                    name: session,
                    lifetime: phux_core::SessionLifetime::Persistent,
                },
                TerminalLaunch {
                    argv: argv.iter().map(|arg| (*arg).to_owned()).collect(),
                    ..TerminalLaunch::default()
                },
                ExitPolicy::Retain,
                &Actor::owner(),
            )
            .expect("create");
        (id, s.registry().resource_key(id).expect("key"))
    })
}

/// Run `body` with the keeper armed beside a store in a fresh directory,
/// then write what it owes.
fn armed<T>(
    state: &SharedState,
    store: &Path,
    body: impl AsyncFnOnce(Option<Arc<Keeper>>) -> T,
) -> T {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    tokio::task::LocalSet::new().block_on(&runtime, async {
        start(state, Some(store), &CancellationToken::new());
        let out = body(keeper(state)).await;
        finish(state).await;
        out
    })
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path)
        .expect("metadata")
        .permissions()
        .mode()
        & 0o777
}

#[test]
fn a_save_is_owner_only_capped_and_read_back_and_a_gone_panes_file_is_removed() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = tmp.path().join("v1.sqlite3");
    let dir = tmp.path().join("v1.scrollback");
    let state = state(keeping(2));
    let (id, key) = pane(&state, "work", &["zsh"]);
    armed(&state, &store, async |keeper| {
        let ask = keeper.expect("armed").ask(&state, id);
        (ask.save)(b"1\r\n2\r\n\x1b]0;t\x073".to_vec());
    });
    let file = dir.join(key.to_string());
    assert_eq!(std::fs::read_to_string(&file).expect("file"), "2\n3");
    assert_eq!((mode(&dir), mode(&file)), (0o700, 0o600));

    // Anything that is not a live pane's file goes at the next start.
    let orphan = dir.join(ResourceKey::from_bytes([9; 16]).to_string());
    for stray in [&orphan, &dir.join("x.autosave.tmp")] {
        std::fs::write(stray, "old").expect("stray");
    }
    armed(&state, &store, async |keeper| {
        let shown = String::from_utf8(seed(&state, id, 24)).expect("UTF-8");
        assert!(shown.starts_with("2\r\n3\x1b[0m\r\n\x1b[2m-- before the restart, 20"));
        // Let the first sweep run.
        tokio::time::sleep(Duration::from_millis(50)).await;
        // An edited file is cleaned again, or dropped.
        std::fs::write(&file, "ok\x1b[6n\x1b]7501;state=working\x07").expect("edit");
        let shown = String::from_utf8(seed(&state, id, 24)).expect("UTF-8");
        assert!(shown.starts_with("ok\x1b[0m\r\n"), "{shown:?}");
        for bad in [vec![0xff], vec![b'a'; KEPT_SCROLLBACK_MAX_BYTES + 1]] {
            std::fs::write(&file, bad).expect("edit");
            assert!(seed(&state, id, 24).is_empty());
        }
        std::fs::remove_file(&file).expect("remove");
        std::os::unix::fs::symlink("/etc/hosts", &file).expect("link");
        assert!(seed(&state, id, 24).is_empty(), "a link is not followed");

        state.with_mut(|s| s.reap_resource(id));
        (keeper.expect("armed").ask(&state, id).save)(b"late".to_vec());
        tokio::time::sleep(SWEEP * 2).await;
    });
    assert_eq!(std::fs::read_dir(&dir).expect("dir").count(), 0);
}

#[test]
fn an_excluded_pane_is_not_saved_and_excluding_one_removes_its_file() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = tmp.path().join("v1.sqlite3");
    let restore = phux_config::RestoreCfg {
        scrollback_exclude: ["secrets", "psql"].map(str::to_owned).to_vec(),
        ..keeping(10)
    };
    let state = state(restore);
    let panes = [
        pane(&state, "work", &["zsh"]),
        pane(&state, "secrets", &["zsh"]),
        pane(&state, "db", &["/usr/bin/psql", "prod"]),
    ];
    let saved = || {
        panes
            .iter()
            .map(|(_, key)| {
                tmp.path()
                    .join("v1.scrollback")
                    .join(key.to_string())
                    .exists()
            })
            .collect::<Vec<_>>()
    };
    armed(&state, &store, async |keeper| {
        let keeper = keeper.expect("armed");
        for (id, _) in panes {
            (keeper.ask(&state, id).save)(b"text".to_vec());
        }
    });
    assert_eq!(saved(), [true, false, false]);
    state.with_mut(|s| {
        s.commit_system(phux_core::Op::ResourceRename {
            resource: panes[0].1,
            name: Some("secrets".to_owned()),
        })
        .expect("rename");
    });
    armed(&state, &store, async |_| {
        tokio::time::sleep(Duration::from_millis(50)).await;
    });
    assert_eq!(saved(), [false, false, false]);
}

#[test]
fn off_writes_nothing_and_deletes_what_was_kept() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = tmp.path().join("v1.sqlite3");
    let dir = tmp.path().join("v1.scrollback");
    let state = state(keeping(0));
    armed(&state, &store, async |keeper| assert!(keeper.is_none()));
    assert_eq!(std::fs::read_dir(tmp.path()).expect("dir").count(), 0);
    std::fs::create_dir(&dir).expect("dir");
    std::fs::write(dir.join("kept"), "secret").expect("file");
    // Dropping the runtime waits for the deletion.
    armed(&state, &store, async |_| ());
    assert!(!dir.exists());
}

#[test]
fn a_linked_directory_is_refused() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let elsewhere = tempfile::tempdir().expect("tempdir");
    std::os::unix::fs::symlink(elsewhere.path(), tmp.path().join("v1.scrollback")).expect("link");
    let state = state(keeping(10));
    armed(&state, &tmp.path().join("v1.sqlite3"), async |keeper| {
        assert!(keeper.is_none());
    });
}
