//! Real desktop window lifecycles against an explicitly supplied checkout CLI.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

static NEXT_SERVER: AtomicU64 = AtomicU64::new(1);

use gpui::{AppContext, Entity, TestAppContext};
use phux_client_runtime::control::Status;
use phux_protocol::ids::SessionKey;

use super::Desktop;
use crate::app::AppOptions;
use crate::app::desk::Desk;

struct Server {
    binary: PathBuf,
    root: PathBuf,
    socket: PathBuf,
    child: Child,
}

impl Server {
    fn new() -> Self {
        Self::with_listener(None)
    }

    fn with_listener(port: Option<u16>) -> Self {
        let binary = PathBuf::from(
            std::env::var_os("PHUX_DESKTOP_JOURNEY_TEST_BIN")
                .expect("name a checkout-built CLI, never an installed binary"),
        );
        // Keep the Unix socket below macOS's sockaddr_un path limit, including
        // when the harness uses its longer approved temporary directory.
        let root = std::env::temp_dir().join(format!(
            "pdj-{:x}-{:x}",
            std::process::id(),
            NEXT_SERVER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        let socket = root.join("s");
        let child = Command::new(&binary)
            .arg("--socket")
            .arg(&socket)
            .arg("server")
            .args(
                port.map(|port| vec!["--listen".to_owned(), format!("127.0.0.1:{port}")])
                    .unwrap_or_default(),
            )
            .env("HOME", &root)
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("XDG_STATE_HOME", root.join("state"))
            .env("PHUX_PROFILE", "desktop-journey")
            .env_remove("PHUX_SOCKET")
            .env_remove("PHUX_WS_TOKENS")
            .env_remove("PHUX_WS_TLS_CERT")
            .env_remove("PHUX_WS_TLS_KEY")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(
                std::fs::File::create(root.join("server.log")).unwrap(),
            ))
            .spawn()
            .unwrap();
        let mut server = Self {
            binary,
            root,
            socket,
            child,
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while !server.socket.exists() {
            if let Some(status) = server.child.try_wait().unwrap() {
                panic!(
                    "private server exited {status}: {}",
                    std::fs::read_to_string(server.root.join("server.log")).unwrap()
                );
            }
            assert!(Instant::now() < deadline, "private server startup deadline");
            std::thread::sleep(Duration::from_millis(10));
        }
        server.run(&["new", "--json", "--empty", "-s", "journey-A"]);
        server.run(&["new", "--json", "--empty", "-s", "journey-B"]);
        server
    }

    fn run(&self, args: &[&str]) {
        let output = Command::new(&self.binary)
            .arg("--socket")
            .arg(&self.socket)
            .args(args)
            .env_remove("PHUX_SOCKET")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn launch(
        &self,
        desktop: &Entity<Desktop>,
        cx: &mut TestAppContext,
    ) -> gpui::WindowHandle<gpui_kit::component::Root> {
        self.launch_named(desktop, "journey-A", false, cx)
    }

    fn launch_named(
        &self,
        desktop: &Entity<Desktop>,
        session: &str,
        default_layout: bool,
        cx: &mut TestAppContext,
    ) -> gpui::WindowHandle<gpui_kit::component::Root> {
        desktop.update(cx, |desktop, cx| {
            desktop
                .launch(
                    AppOptions {
                        socket: self.socket.clone(),
                        session: session.to_owned(),
                        phux: self.binary.clone(),
                        layout_path: if default_layout {
                            PathBuf::new()
                        } else {
                            self.root.join("layout.json")
                        },
                        fresh: false,
                        initial_terminal: None,
                        quick: false,
                        dev_telemetry: false,
                        startup_error: None,
                    },
                    cx,
                )
                .unwrap()
        })
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn desktop(cx: &mut TestAppContext) -> Entity<Desktop> {
    require_isolated_state();
    // These fixtures intentionally use the runtime's real I/O thread, rather
    // than pretending its wakes are deterministic scheduler events.
    cx.executor().allow_parking();
    cx.update(|cx| {
        gpui_kit::init(cx);
        Desktop::install(cx)
    })
}

fn require_isolated_state() {
    let state = PathBuf::from(
        std::env::var_os("XDG_STATE_HOME")
            .expect("set XDG_STATE_HOME to a disposable test-state directory"),
    );
    let cache = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join(".cache");
    assert!(
        state.starts_with(cache) || state.starts_with(std::env::temp_dir()),
        "desktop lifecycle fixtures must not write the user's desktop state"
    );
}

#[track_caller]
fn wait_for(cx: &mut TestAppContext, mut predicate: impl FnMut(&mut TestAppContext) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        cx.run_until_parked();
        if predicate(cx) {
            return;
        }
        assert!(Instant::now() < deadline, "desktop lifecycle deadline");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Press a key the way the macOS platform delivers it: one key-down that its
/// binding consumes, then key-up. GPUI's `simulate_keystrokes` also runs a
/// simulated IME that types `"\n"` after a propagated Enter binding, which
/// marks a just-submitted composer draft as edited; a real Return never does.
fn press(cx: &mut TestAppContext, window: gpui::AnyWindowHandle, key: &str) {
    use gpui::InputEvent as _;
    let keystroke = gpui::Keystroke::parse(key).unwrap();
    cx.update_window(window, |_, window, cx| {
        window.dispatch_event(
            gpui::KeyDownEvent {
                keystroke: keystroke.clone(),
                is_held: false,
                prefer_character_input: false,
            }
            .to_platform_input(),
            cx,
        );
        window.dispatch_event(gpui::KeyUpEvent { keystroke }.to_platform_input(), cx);
    })
    .unwrap();
    cx.run_until_parked();
}

fn home_key(desk: &Entity<Desk>, cx: &TestAppContext) -> Option<SessionKey> {
    desk.read_with(cx, |desk, _| desk.link.as_ref()?.connection.home_key())
}

fn attached(desk: &Entity<Desk>, cx: &TestAppContext) -> bool {
    desk.read_with(cx, |desk, _| {
        desk.link
            .as_ref()
            .is_some_and(|link| link.connection.status == Status::Attached)
    })
}

#[gpui::test]
#[ignore = "requires checkout CLI and disposable XDG_CONFIG_HOME/XDG_STATE_HOME; run this journey alone"]
fn federated_work_round_trip_preserves_layouts_and_never_spawns_when_browsing(
    cx: &mut TestAppContext,
) {
    let config = PathBuf::from(std::env::var_os("XDG_CONFIG_HOME").expect("disposable config"));
    assert!(config.starts_with(std::env::temp_dir()) || config.starts_with("/tmp"));
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let local = Server::new();
    let remote = Server::with_listener(Some(port));
    std::fs::create_dir_all(config.join("phux")).unwrap();
    std::fs::write(
        config.join("phux/config.toml"),
        format!("[[remote]]\nname = \"journey-remote\"\nendpoint = \"ws://127.0.0.1:{port}\"\n"),
    )
    .unwrap();
    let desktop = desktop(cx);
    let handle = local.launch(&desktop, cx);
    let desk = desktop.read_with(cx, |desktop, _| desktop.windows[0].desk.clone());
    wait_for(cx, |cx| {
        attached(&desk, cx) && placement_count(&desk, cx) == 1
    });
    cx.update_window(*handle, |_, window, cx| {
        desk.update(cx, |desk, cx| desk.execute("split-right", window, cx));
    })
    .unwrap();
    wait_for(cx, |cx| placement_count(&desk, cx) == 2);
    let expected = snapshot(&desk, cx);
    wait_for(cx, |cx| {
        desk.read_with(cx, |desk, _| {
            desk.work_catalog().iter().any(|work| {
                work.destination.host.as_deref() == Some("journey-remote")
                    && work.destination.name == "journey-A"
                    && work.online
            })
        })
    });
    let target = desk.read_with(cx, |desk, _| {
        desk.work_catalog()
            .into_iter()
            .find(|work| {
                work.destination.host.as_deref() == Some("journey-remote")
                    && work.destination.name == "journey-A"
            })
            .unwrap()
            .destination
    });
    let remote_key = target.key;
    desk.update(cx, |desk, cx| desk.navigate_work(target, cx));
    wait_for(cx, |cx| {
        attached(&desk, cx) && home_key(&desk, cx) == remote_key
    });
    assert_eq!(
        placement_count(&desk, cx),
        0,
        "opening an empty remote session creates no terminal"
    );
    desk.read_with(cx, |desk, _| assert!(desk.work_catalog().iter().any(|work| work.destination.host.is_none() && work.destination.name == "journey-A")));
    desktop.read_with(cx, |desktop, _| {
        assert_eq!(desktop.windows.len(), 1);
        assert_eq!(desktop.windows[0].remote.as_deref(), Some("journey-remote"));
    });
    let back = desk.read_with(cx, |desk, _| {
        desk.work_catalog()
            .into_iter()
            .find(|work| work.destination.host.is_none() && work.destination.name == "journey-A")
            .unwrap()
            .destination
    });
    let local_key = back.key;
    desk.update(cx, |desk, cx| desk.navigate_work(back, cx));
    wait_for(cx, |cx| {
        attached(&desk, cx) && home_key(&desk, cx) == local_key && placement_count(&desk, cx) == 2
    });
    assert_eq!(
        snapshot(&desk, cx).tabs,
        expected.tabs,
        "A -> B -> A restores the exact split tree and placement identities"
    );
    desktop.read_with(cx, |desktop, _| {
        assert_eq!(desktop.windows.len(), 1);
        assert!(desktop.windows[0].remote.is_none());
    });
    // A view of a sibling session must also recover, but only after the home
    // authority/key has been freshly validated on the new socket.
    let other = desk.read_with(cx, |desk, _| {
        desk.link
            .as_ref()
            .unwrap()
            .connection
            .topology
            .as_ref()
            .unwrap()
            .sessions
            .iter()
            .find(|session| session.name == "journey-B")
            .unwrap()
            .id
    });
    cx.update_window(*handle, |_, window, cx| {
        desk.update(cx, |desk, cx| desk.new_terminal_in(other, window, cx));
    })
    .unwrap();
    wait_for(cx, |cx| placement_count(&desk, cx) == 3);
    let before_reconnect = snapshot(&desk, cx);
    let epoch = desk.read_with(cx, |desk, _| {
        desk.link.as_ref().unwrap().connection.connection_epoch()
    });
    desk.update(cx, |desk, _| {
        desk.link.as_ref().unwrap().connection.reconnect().unwrap()
    });
    wait_for(cx, |cx| {
        desk.read_with(cx, |desk, _| {
            let connection = &desk.link.as_ref().unwrap().connection;
            connection.connection_epoch() > epoch
                && connection.status == Status::Attached
                && connection.session_validated()
                && desk
                    .link
                    .as_ref()
                    .unwrap()
                    .workspace
                    .tabs
                    .iter()
                    .flat_map(|tab| crate::workspace::layout::placements(&tab.root))
                    .all(|placement| {
                        placement
                            .terminal_id
                            .as_ref()
                            .is_none_or(|id| connection.ready(id))
                    })
        })
    });
    assert_eq!(
        snapshot(&desk, cx).tabs,
        before_reconnect.tabs,
        "foreign views survive reconnect with their layout unchanged"
    );
    let _remote = remote;
    desk.update(cx, |desk, cx| desk.prepare_close(cx));
    std::fs::remove_file(config.join("phux/config.toml")).unwrap();
}

#[gpui::test]
#[ignore = "requires PHUX_DESKTOP_JOURNEY_TEST_BIN pointing at a checkout-built CLI"]
fn remote_authority_cannot_rename_or_resolve_the_local_quick_home(cx: &mut TestAppContext) {
    let server = Server::new();
    let desktop = desktop(cx);
    server.launch(&desktop, cx);
    let desk = desktop.read_with(cx, |desktop, _| desktop.windows[0].desk.clone());
    wait_for(cx, |cx| {
        attached(&desk, cx) && home_key(&desk, cx).is_some()
    });
    let key = home_key(&desk, cx).unwrap();
    // Model a remote authority exposing the same persistent key (for example
    // a copied server state), without touching the user's remote registry.
    desk.update(cx, |desk, _| desk.remote = Some("remote-fixture".into()));
    desktop.update(cx, |desktop, _| {
        desktop.windows[0].remote = Some("remote-fixture".into())
    });
    server.run(&["rename", "journey-A", "remote-renamed-A"]);
    desk.update(cx, |desk, _| {
        let _ = desk.link.as_ref().unwrap().connection.refresh_topology();
    });
    wait_for(cx, |cx| {
        desk.read_with(cx, |desk, _| desk.session == "remote-renamed-A")
    });
    desktop.update(cx, |desktop, cx| {
        assert_eq!(desktop.home_key, Some(key));
        assert_eq!(
            desktop.home_session, "journey-A",
            "remote topology cannot rename local home"
        );
        assert!(
            desktop.resolve_quick_home(cx).is_err(),
            "only a local authority can resolve Quick's pinned key"
        );
    });
    desk.update(cx, |desk, cx| desk.prepare_close(cx));
}

#[gpui::test]
#[ignore = "requires PHUX_DESKTOP_JOURNEY_TEST_BIN pointing at a checkout-built CLI"]
fn explicit_reconnect_after_offline_rename_never_creates_the_old_name(cx: &mut TestAppContext) {
    let server = Server::new();
    let desktop = desktop(cx);
    server.launch(&desktop, cx);
    let desk = desktop.read_with(cx, |desktop, _| desktop.windows[0].desk.clone());
    wait_for(cx, |cx| {
        attached(&desk, cx) && home_key(&desk, cx).is_some()
    });
    let key = home_key(&desk, cx).unwrap();
    let sessions_before = desk.read_with(cx, |desk, _| {
        desk.link
            .as_ref()
            .unwrap()
            .connection
            .topology
            .as_ref()
            .unwrap()
            .sessions
            .len()
    });
    desk.update(cx, |desk, _| desk.link.as_mut().unwrap().connection.close());
    wait_for(cx, |cx| !attached(&desk, cx));
    server.run(&["rename", "journey-A", "renamed-A"]);
    desk.update(cx, |desk, cx| desk.reconnect(cx));
    wait_for(cx, |cx| {
        attached(&desk, cx) && desk.read_with(cx, |desk, _| desk.session == "renamed-A")
    });
    assert_eq!(home_key(&desk, cx), Some(key));
    desktop.read_with(cx, |desktop, _| {
        assert_eq!(desktop.windows[0].session, "renamed-A");
    });
    desk.read_with(cx, |desk, _| {
        let topology = desk
            .link
            .as_ref()
            .unwrap()
            .connection
            .topology
            .as_ref()
            .unwrap();
        assert!(topology.session_named("journey-A").is_none());
        assert_eq!(
            topology.sessions.len(),
            sessions_before,
            "reconnect must not create another session"
        );
    });
    desk.update(cx, |desk, cx| desk.prepare_close(cx));
    assert!(
        server.root.join("layout.json").exists(),
        "explicit layout path is retained"
    );
}

#[gpui::test]
#[ignore = "requires PHUX_DESKTOP_JOURNEY_TEST_BIN pointing at a checkout-built CLI"]
fn project_window_reuse_follows_rename_without_adopting_the_reused_old_name(
    cx: &mut TestAppContext,
) {
    let server = Server::new();
    let desktop = desktop(cx);
    server.launch(&desktop, cx);
    let original = desktop.read_with(cx, |desktop, _| desktop.windows[0].desk.clone());
    wait_for(cx, |cx| {
        attached(&original, cx) && home_key(&original, cx).is_some()
    });
    let key = home_key(&original, cx).unwrap();
    server.run(&["rename", "journey-A", "renamed-A"]);
    server.run(&["new", "--json", "--empty", "-s", "journey-A"]);
    original.update(cx, |desk, _| {
        let _ = desk.link.as_ref().unwrap().connection.refresh_topology();
    });
    wait_for(cx, |cx| {
        original.read_with(cx, |desk, _| desk.session == "renamed-A")
    });
    desktop.update(cx, |desktop, cx| {
        desktop
            .open_project("renamed-A".into(), phux_protocol::ResourceId::local(1), cx)
            .unwrap();
        assert_eq!(
            desktop.windows.len(),
            1,
            "the renamed project reuses its window"
        );
        assert_eq!(desktop.windows[0].session, "renamed-A");
        assert_eq!(desktop.windows[0].desk.entity_id(), original.entity_id());
        desktop
            .open_project("journey-A".into(), phux_protocol::ResourceId::local(1), cx)
            .unwrap();
        assert_eq!(
            desktop.windows.len(),
            2,
            "a reused old name needs its own window"
        );
        assert_ne!(desktop.windows[1].desk.entity_id(), original.entity_id());
    });
    let replacement = desktop.read_with(cx, |desktop, _| desktop.windows[1].desk.clone());
    wait_for(cx, |cx| {
        attached(&replacement, cx) && home_key(&replacement, cx).is_some()
    });
    assert_eq!(home_key(&original, cx), Some(key));
    assert_ne!(home_key(&replacement, cx), Some(key));
    original.update(cx, |desk, cx| desk.prepare_close(cx));
    replacement.update(cx, |desk, cx| desk.prepare_close(cx));
}

#[gpui::test]
#[ignore = "requires PHUX_DESKTOP_JOURNEY_TEST_BIN pointing at a checkout-built CLI"]
fn remaining_window_keeps_quick_home_identity_after_original_window_closes(
    cx: &mut TestAppContext,
) {
    let server = Server::new();
    let desktop = desktop(cx);
    let home_window = server.launch(&desktop, cx);
    let a = desktop.read_with(cx, |desktop, _| desktop.windows[0].desk.clone());
    wait_for(cx, |cx| attached(&a, cx) && home_key(&a, cx).is_some());
    let key = home_key(&a, cx).unwrap();
    desktop.update(cx, |desktop, cx| {
        desktop.open_session("journey-B".to_owned(), None, cx)
    });
    let b = desktop.read_with(cx, |desktop, _| desktop.windows[1].desk.clone());
    wait_for(cx, |cx| attached(&b, cx));
    cx.update_window(*home_window, |_, window, _| window.remove_window())
        .unwrap();
    wait_for(cx, |cx| {
        desktop.read_with(cx, |desktop, _| desktop.windows.len() == 1)
    });
    server.run(&["rename", "journey-A", "renamed-A"]);
    server.run(&["new", "--json", "--empty", "-s", "journey-A"]);
    wait_for(cx, |cx| {
        desktop.read_with(cx, |desktop, _| desktop.home_session == "renamed-A")
    });
    desktop.update(cx, |desktop, cx| desktop.toggle_quick(cx));
    let quick = desktop.read_with(cx, |desktop, _| {
        desktop.quick.as_ref().unwrap().desk.clone()
    });
    wait_for(cx, |cx| {
        quick.read_with(cx, |desk, _| {
            let connection = &desk.link.as_ref().unwrap().connection;
            assert!(
                !matches!(connection.status, Status::Failed | Status::Closed),
                "quick attach refused: {:?}",
                connection.error
            );
        });
        attached(&quick, cx) && home_key(&quick, cx).is_some()
    });
    assert_eq!(home_key(&quick, cx), Some(key));
    assert_ne!(home_key(&quick, cx), home_key(&b, cx));
    desktop.read_with(cx, |desktop, _| assert_eq!(desktop.home_key, Some(key)));
    quick.update(cx, |desk, cx| desk.prepare_close(cx));
    b.update(cx, |desk, cx| desk.prepare_close(cx));
}

#[gpui::test]
#[ignore = "requires PHUX_DESKTOP_JOURNEY_TEST_BIN pointing at a checkout-built CLI"]
fn managed_redial_after_offline_rename_does_not_adopt_a_reused_name(cx: &mut TestAppContext) {
    let server = Server::new();
    let desktop = desktop(cx);
    server.launch(&desktop, cx);
    let desk = desktop.read_with(cx, |desktop, _| desktop.windows[0].desk.clone());
    wait_for(cx, |cx| {
        attached(&desk, cx) && home_key(&desk, cx).is_some()
    });
    let key = home_key(&desk, cx).unwrap();
    desk.update(cx, |desk, _| desk.link.as_mut().unwrap().connection.close());
    wait_for(cx, |cx| !attached(&desk, cx));
    server.run(&["rename", "journey-A", "renamed-A"]);
    server.run(&["new", "--json", "--empty", "-s", "journey-A"]);
    let updates = desktop.read_with(cx, |desktop, _| desktop.updates.clone());
    updates.update(cx, |updates, cx| {
        updates.runtime_generation += 1;
        cx.notify();
    });
    wait_for(cx, |cx| {
        attached(&desk, cx) && desk.read_with(cx, |desk, _| desk.session == "renamed-A")
    });
    assert_eq!(home_key(&desk, cx), Some(key));
    desk.update(cx, |desk, cx| desk.prepare_close(cx));
}

#[gpui::test]
#[ignore = "requires PHUX_DESKTOP_JOURNEY_TEST_BIN pointing at a checkout-built CLI"]
fn rename_save_close_and_relaunch_preserves_default_tabs_and_splits(cx: &mut TestAppContext) {
    let server = Server::new();
    let desktop = desktop(cx);
    let handle = server.launch_named(&desktop, "journey-A", true, cx);
    let original = desktop.read_with(cx, |desktop, _| desktop.windows[0].desk.clone());
    wait_for(cx, |cx| {
        attached(&original, cx) && placement_count(&original, cx) == 1
    });
    cx.update_window(*handle, |_, window, cx| {
        original.update(cx, |desk, cx| desk.execute("split-right", window, cx));
    })
    .unwrap();
    wait_for(cx, |cx| placement_count(&original, cx) == 2);
    cx.update_window(*handle, |_, window, cx| {
        original.update(cx, |desk, cx| desk.execute("new", window, cx));
    })
    .unwrap();
    wait_for(cx, |cx| placement_count(&original, cx) == 3);
    server.run(&["rename", "journey-A", "renamed-A"]);
    wait_for(cx, |cx| {
        original.read_with(cx, |desk, _| desk.session == "renamed-A")
    });
    let expected = snapshot(&original, cx);
    assert_eq!(expected.tabs.len(), 2);
    original.update(cx, |desk, cx| desk.prepare_close(cx));
    let saved_path = desktop.read_with(cx, |desktop, _| desktop.layout.clone());
    assert!(saved_path.exists());
    cx.update_window(*handle, |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();

    let reopened = cx.update(Desktop::install);
    server.launch_named(&reopened, "renamed-A", true, cx);
    let restored = reopened.read_with(cx, |desktop, _| desktop.windows[0].desk.clone());
    wait_for(cx, |cx| {
        attached(&restored, cx) && placement_count(&restored, cx) == 3
    });
    assert_eq!(
        reopened.read_with(cx, |desktop, _| desktop.layout.clone()),
        saved_path
    );
    assert_eq!(snapshot(&restored, cx).tabs, expected.tabs);
    restored.update(cx, |desk, cx| desk.prepare_close(cx));
}

fn placement_count(desk: &Entity<Desk>, cx: &TestAppContext) -> usize {
    desk.read_with(cx, |desk, _| {
        desk.link
            .as_ref()
            .map(|link| {
                link.workspace
                    .tabs
                    .iter()
                    .flat_map(|tab| crate::workspace::layout::placements(&tab.root))
                    .filter(|placement| placement.terminal_id.is_some())
                    .count()
            })
            .unwrap_or(0)
    })
}

fn snapshot(desk: &Entity<Desk>, cx: &TestAppContext) -> crate::workspace::SavedLayout {
    desk.read_with(cx, |desk, _| {
        let link = desk.link.as_ref().unwrap();
        crate::workspace::snapshot_layout(&link.workspace, &link.connection, &desk.prefs).unwrap()
    })
}

#[gpui::test]
#[ignore = "requires PHUX_DESKTOP_JOURNEY_TEST_BIN pointing at a checkout-built CLI"]
fn rename_dialog_keeps_the_original_key_when_its_old_name_is_reused(cx: &mut TestAppContext) {
    let server = Server::new();
    let desktop = desktop(cx);
    let window = server.launch(&desktop, cx);
    let desk = desktop.read_with(cx, |desktop, _| desktop.windows[0].desk.clone());
    wait_for(cx, |cx| {
        attached(&desk, cx) && placement_count(&desk, cx) == 1
    });
    let key = home_key(&desk, cx).unwrap();
    cx.update_window(*window, |_, window, cx| {
        desk.update(cx, |desk, cx| desk.execute("rename-session", window, cx));
    })
    .unwrap();
    desk.read_with(cx, |desk, _| {
        assert!(matches!(&desk.modal, crate::app::desk::Modal::Session { target: Some(target), .. } if target.key == Some(key)));
    });
    server.run(&["rename", "journey-A", "moved-A"]);
    server.run(&["new", "--json", "--empty", "-s", "journey-A"]);
    // An empty foreign session has no terminal event to refresh the live
    // handle roster. Read it explicitly without changing the captured modal.
    desk.update(cx, |desk, _| {
        let _ = desk.link.as_ref().unwrap().connection.refresh_topology();
    });
    wait_for(cx, |cx| {
        desk.read_with(cx, |desk, _| {
            desk.link
                .as_ref()
                .and_then(|link| link.connection.topology.as_ref())
                .is_some_and(|topology| {
                    topology
                        .session_keyed(&key)
                        .is_some_and(|session| session.name == "moved-A")
                        && topology.session_named("journey-A").is_some()
                })
        })
    });
    cx.update_window(*window, |_, window, cx| {
        desk.update(cx, |desk, cx| {
            desk.apply_session("final-A".to_owned(), None, window, cx)
        });
    })
    .unwrap();
    wait_for(cx, |cx| {
        desk.read_with(cx, |desk, _| {
            let refusal = desk.feed.items().find(|entry| {
                entry.title == "Could not rename the session" || entry.title == "Session changed"
            });
            assert!(refusal.is_none(), "rename refused: {refusal:?}");
            desk.session == "final-A"
                && desk.feed.items().any(|entry| {
                    entry.kind == crate::app::notifications::Kind::Info
                        && entry.title == "Session renamed"
                })
        })
    });
    desk.read_with(cx, |desk, _| {
        let topology = desk
            .link
            .as_ref()
            .unwrap()
            .connection
            .topology
            .as_ref()
            .unwrap();
        assert_eq!(topology.session_keyed(&key).unwrap().name, "final-A");
        assert_ne!(topology.session_named("journey-A").unwrap().key, Some(key));
        assert!(
            desk.feed
                .items()
                .any(|entry| entry.kind == crate::app::notifications::Kind::Info
                    && entry.title == "Session renamed")
        );
    });
    desk.update(cx, |desk, cx| desk.prepare_close(cx));
}

#[gpui::test]
#[ignore = "requires PHUX_DESKTOP_JOURNEY_TEST_BIN pointing at a checkout-built CLI and disposable XDG_STATE_HOME"]
fn unchanged_legacy_layout_is_materialized_once_before_rename_or_close(cx: &mut TestAppContext) {
    use crate::workspace::persist::{
        FileLayoutStore, layout_paths, layout_value, load_layout, session_layout_paths,
    };
    use std::os::unix::fs::MetadataExt;

    let server = Server::new();
    let initial = desktop(cx);
    let initial_window = server.launch(&initial, cx);
    let first = initial.read_with(cx, |desktop, _| desktop.windows[0].desk.clone());
    wait_for(cx, |cx| {
        attached(&first, cx) && placement_count(&first, cx) == 1
    });
    let saved = snapshot(&first, cx);
    let key = home_key(&first, cx).unwrap();
    let socket = server.socket.display().to_string();
    let named = layout_paths(&socket, "journey-A").layout;
    FileLayoutStore::new(&named, None)
        .write(&layout_value(&saved))
        .unwrap();
    let named_bytes = std::fs::read(&named).unwrap();
    first.update(cx, |desk, cx| desk.prepare_close(cx));
    cx.update_window(*initial_window, |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();

    let migrated = cx.update(Desktop::install);
    let migrated_window = server.launch_named(&migrated, "journey-A", true, cx);
    let desk = migrated.read_with(cx, |desktop, _| desktop.windows[0].desk.clone());
    let canonical = session_layout_paths(&socket, key).layout;
    wait_for(cx, |cx| attached(&desk, cx) && canonical.exists());
    assert_eq!(
        load_layout(&FileLayoutStore::new(&canonical, None).read())
            .initial
            .unwrap()
            .tabs,
        saved.tabs
    );
    let inode = std::fs::metadata(&canonical).unwrap().ino();
    desk.update(cx, |desk, _| {
        for _ in 0..5 {
            desk.persist_if_changed();
        }
    });
    assert_eq!(
        std::fs::metadata(&canonical).unwrap().ino(),
        inode,
        "unchanged presentation does not rewrite the canonical file"
    );
    assert_eq!(
        std::fs::read(&named).unwrap(),
        named_bytes,
        "legacy source is preserved"
    );
    server.run(&["rename", "journey-A", "renamed-A"]);
    wait_for(cx, |cx| {
        desk.read_with(cx, |desk, _| desk.session == "renamed-A")
    });
    // Model a crash's no-save boundary: the on-close hook must not rescue the
    // migration. The canonical file already exists before any close or edit.
    desk.update(cx, |desk, _| desk.persist_writes = false);
    cx.update_window(*migrated_window, |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();
    let relaunched = cx.update(Desktop::install);
    server.launch_named(&relaunched, "renamed-A", true, cx);
    let restored = relaunched.read_with(cx, |desktop, _| desktop.windows[0].desk.clone());
    wait_for(cx, |cx| {
        attached(&restored, cx) && placement_count(&restored, cx) == 1
    });
    assert_eq!(snapshot(&restored, cx).tabs, saved.tabs);
    restored.update(cx, |desk, cx| desk.prepare_close(cx));
}

#[gpui::test]
#[ignore = "requires PHUX_DESKTOP_JOURNEY_TEST_BIN pointing at a checkout-built CLI and disposable XDG_STATE_HOME"]
fn deleted_home_with_reused_name_cannot_open_a_replacement_quick_window(cx: &mut TestAppContext) {
    let server = Server::new();
    let desktop = desktop(cx);
    let a_window = server.launch(&desktop, cx);
    let a = desktop.read_with(cx, |desktop, _| desktop.windows[0].desk.clone());
    wait_for(cx, |cx| attached(&a, cx) && home_key(&a, cx).is_some());
    let original = home_key(&a, cx).unwrap();
    desktop.update(cx, |desktop, cx| {
        desktop.open_session("journey-B".to_owned(), None, cx)
    });
    let b = desktop.read_with(cx, |desktop, _| desktop.windows[1].desk.clone());
    wait_for(cx, |cx| attached(&b, cx));
    cx.update_window(*a_window, |_, window, _| window.remove_window())
        .unwrap();
    wait_for(cx, |cx| {
        desktop.read_with(cx, |desktop, _| desktop.windows.len() == 1)
    });
    server.run(&["kill", "--yes", "journey-A"]);
    server.run(&["new", "--json", "--empty", "-s", "journey-A"]);
    wait_for(cx, |cx| {
        b.read_with(cx, |desk, _| {
            let replica = desk.link.as_ref().unwrap().connection.client.organization();
            replica.session(&original).is_none()
                && replica
                    .sessions()
                    .iter()
                    .any(|session| session.name == "journey-A")
        })
    });
    desktop.update(cx, |desktop, cx| desktop.toggle_quick(cx));
    desktop.read_with(cx, |desktop, _| {
        assert_eq!(desktop.home_key, Some(original));
        assert!(
            desktop.quick.is_none(),
            "a reused name never replaces the pinned home"
        );
    });
    b.read_with(cx, |desk, _| {
        assert!(
            desk.feed
                .items()
                .any(|entry| entry.title == "Could not open quick terminal")
        )
    });
    b.update(cx, |desk, cx| desk.prepare_close(cx));
}

#[gpui::test]
#[ignore = "requires PHUX_DESKTOP_JOURNEY_TEST_BIN pointing at a checkout-built CLI and disposable XDG_STATE_HOME"]
fn agent_stream_enables_native_face_without_restart_and_keeps_drafts(cx: &mut TestAppContext) {
    let server = Server::new();
    let desktop = desktop(cx);
    let handle = server.launch(&desktop, cx);
    let desk = desktop.read_with(cx, |desktop, _| desktop.windows[0].desk.clone());
    wait_for(cx, |cx| {
        attached(&desk, cx)
            && desk.read_with(cx, |desk, _| {
                desk.focused_terminal()
                    .is_some_and(|id| desk.link.as_ref().unwrap().connection.ready(&id))
            })
    });
    let (placement, terminal) = desk.read_with(cx, |desk, _| {
        let placement = desk.focused_placement().unwrap();
        (placement.id.clone(), placement.terminal_id.clone().unwrap())
    });
    let selector = match &terminal {
        phux_protocol::ResourceId::Local { id } => format!("@{id}"),
        phux_protocol::ResourceId::Satellite { .. } => panic!("private fixture must be local"),
    };
    server.run(&[
        "agent",
        "session",
        "open",
        &selector,
        "--provider",
        "omp",
        "--native-id",
        "native-journey",
    ]);
    server.run(&[
        "agent",
        "emit",
        &selector,
        "--type",
        "session_start",
        "--data",
        r#"{"provider":"omp","native_id":"native-journey"}"#,
    ]);
    wait_for(cx, |cx| {
        desk.read_with(cx, |desk, _| desk.conversation_eligible(&placement))
    });
    desk.read_with(cx, |desk, _| {
        assert!(
            !desk.conversation_visible(&placement),
            "unsolicited agent start does not switch faces"
        );
        assert_eq!(desk.focused_terminal(), Some(terminal.clone()));
    });
    server.run(&["agent", "emit", &selector, "--type", "provider_raw", "--data",
        r#"{"provider":"omp","schema":"phux.transcript/v1","entry":{"id":"reply","role":"assistant","text":"Streaming","truncated":false,"final":false}}"#]);
    server.run(&["agent", "emit", &selector, "--type", "provider_raw", "--data",
        r#"{"provider":"omp","schema":"phux.transcript/v1","entry":{"id":"reply","role":"assistant","text":"Streaming complete","truncated":false,"final":true}}"#]);
    wait_for(cx, |cx| {
        desk.read_with(cx, |desk, _| {
            desk.link
                .as_ref()
                .unwrap()
                .connection
                .conversation(&terminal)
                .unwrap()
                .entries
                .iter()
                .any(|entry| entry.id == "reply" && entry.is_final)
        })
    });
    desk.read_with(cx, |desk, _| {
        let conversation = desk
            .link
            .as_ref()
            .unwrap()
            .connection
            .conversation(&terminal)
            .unwrap();
        assert_eq!(
            conversation
                .entries
                .iter()
                .filter(|entry| entry.id == "reply")
                .count(),
            1
        );
    });
    cx.update_window(*handle, |_, window, cx| {
        desk.update(cx, |desk, cx| {
            desk.toggle_conversation(&placement, window, cx);
            desk.conversation_faces[&placement]
                .input
                .clone()
                .update(cx, |input, cx| {
                    input.set_value("two\nlines", window, cx);
                });
            desk.toggle_conversation(&placement, window, cx);
            assert!(!desk.conversation_visible(&placement));
            desk.toggle_conversation(&placement, window, cx);
            assert_eq!(
                desk.conversation_faces[&placement].input.read(cx).value(),
                "two\nlines"
            );
        });
    })
    .unwrap();
    cx.run_until_parked();
    // Exercise the real Kit textarea, not a separate text model.
    press(cx, *handle, "shift-enter");
    desk.read_with(cx, |desk, cx| {
        let draft = desk.conversation_faces[&placement].input.read(cx).value();
        assert_eq!(
            draft.matches('\n').count(),
            2,
            "Shift-Enter inserts a line and does not submit: {draft:?}"
        );
    });
    // Enter uses the production acknowledged paste+Return lane to the same PTY.
    cx.update_window(*handle, |_, window, cx| {
        desk.update(cx, |desk, cx| {
            desk.conversation_faces[&placement]
                .input
                .clone()
                .update(cx, |input, cx| {
                    input.set_value("printf 'OMP_NATIVE_INPUT_JOURNEY\\n'", window, cx);
                });
        });
    })
    .unwrap();
    press(cx, *handle, "enter");
    wait_for(cx, |cx| {
        desk.read_with(cx, |desk, cx| {
            desk.conversation_faces[&placement]
                .input
                .read(cx)
                .value()
                .is_empty()
        })
    });
    wait_for(cx, |cx| {
        desk.read_with(cx, |desk, _| {
            desk.client()
                .and_then(|client| client.acquire_view(desk.focused_view()?))
                .is_some_and(|frame| {
                    frame
                        .text()
                        .lines()
                        .any(|line| line.trim() == "OMP_NATIVE_INPUT_JOURNEY")
                })
        })
    });
    server.run(&[
        "agent",
        "emit",
        &selector,
        "--type",
        "prompt",
        "--data",
        r#"{"length":1}"#,
    ]);
    wait_for(cx, |cx| {
        desk.read_with(cx, |desk, _| desk.conversation_writable(&placement, true))
    });
    desk.update(cx, |desk, cx| {
        desk.stop_conversation(&placement, cx);
        assert!(
            desk.link
                .as_ref()
                .unwrap()
                .connection
                .conversation(&terminal)
                .unwrap()
                .stop_requested
        );
        assert!(
            !desk.conversation_writable(&placement, true),
            "a second Escape cannot open rewind"
        );
        desk.stop_conversation(&placement, cx);
    });
    server.run(&[
        "agent",
        "emit",
        &selector,
        "--type",
        "notification",
        "--data",
        r#"{"kind":"permission"}"#,
    ]);
    wait_for(cx, |cx| {
        desk.read_with(cx, |desk, _| {
            desk.link
                .as_ref()
                .unwrap()
                .connection
                .conversation(&terminal)
                .unwrap()
                .state
                .status
                == phux_client_core::session::agent_stream::AgentSessionStatus::Blocked
        })
    });
    cx.update_window(*handle, |_, window, cx| {
        desk.update(cx, |desk, cx| {
            desk.conversation_faces[&placement]
                .input
                .clone()
                .update(cx, |input, cx| {
                    input.set_value("preserve approval draft", window, cx);
                });
            assert!(!desk.conversation_writable(&placement, false));
            desk.send_conversation(&placement, cx);
            assert_eq!(
                desk.conversation_faces[&placement].input.read(cx).value(),
                "preserve approval draft"
            );
        });
    })
    .unwrap();
    // Closing only the stream keeps readable rows, but refuses shell writes and Stop.
    server.run(&["agent", "session", "close", &selector]);
    wait_for(cx, |cx| {
        desk.read_with(cx, |desk, _| {
            desk.link
                .as_ref()
                .unwrap()
                .connection
                .conversation(&terminal)
                .is_some_and(|conversation| conversation.closed)
        })
    });
    desk.update(cx, |desk, cx| {
        assert!(desk.conversation_visible(&placement));
        assert!(!desk.conversation_writable(&placement, false));
        assert!(!desk.conversation_writable(&placement, true));
        let draft = desk.conversation_faces[&placement].input.read(cx).value();
        desk.send_conversation(&placement, cx);
        assert_eq!(
            desk.conversation_faces[&placement].input.read(cx).value(),
            draft
        );
        assert!(
            desk.link.as_ref().unwrap().connection.ready(&terminal),
            "closing agent leaves terminal alive"
        );
    });
    cx.update_window(*handle, |_, window, cx| {
        desk.update(cx, |desk, cx| {
            desk.toggle_conversation(&placement, window, cx)
        });
    })
    .unwrap();
    assert!(!desk.read_with(cx, |desk, _| desk.conversation_visible(&placement)));
    desk.update(cx, |desk, cx| desk.prepare_close(cx));
}
