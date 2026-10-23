//! Main-thread GPU fixture support; shared runtime and pixel helpers live in pixels.rs.

use super::*;

/// The actual desktop conversation, painted offscreen by Metal without screen
/// capture permission or bringing a window to the front. Opt-in because it
/// needs a checkout CLI and disposable XDG state, like the native journeys.
#[cfg(feature = "terminal-fixtures")]
pub(super) fn native_conversation_face_is_rendered() {
    use std::path::PathBuf;
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    struct PrivateServer {
        child: Child,
        root: PathBuf,
    }
    impl Drop for PrivateServer {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
    let binary = PathBuf::from(
        std::env::var_os("PHUX_DESKTOP_JOURNEY_TEST_BIN")
            .expect("name a checkout-built CLI, never an installed binary"),
    );
    let state =
        PathBuf::from(std::env::var_os("XDG_STATE_HOME").expect("set disposable XDG_STATE_HOME"));
    let cache = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join(".cache");
    assert!(
        state.starts_with(cache) || state.starts_with(std::env::temp_dir()),
        "native pixel fixture must not write the user's desktop state"
    );
    let root = std::env::temp_dir().join(format!("pdcp-{}", std::process::id()));
    std::fs::create_dir(&root).expect("private fixture directory");
    let socket = root.join("s");
    let child = Command::new(&binary)
        .arg("--socket")
        .arg(&socket)
        .arg("server")
        .env("HOME", &root)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("PHUX_PROFILE", "desktop-conversation-pixels")
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
        .expect("private server");
    let _server = PrivateServer {
        child,
        root: root.clone(),
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    while !socket.exists() {
        assert!(Instant::now() < deadline, "private server startup deadline");
        std::thread::sleep(Duration::from_millis(10));
    }
    let run = |args: &[&str]| {
        let result = Command::new(&binary)
            .arg("--socket")
            .arg(&socket)
            .args(args)
            .env("HOME", &root)
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("XDG_STATE_HOME", root.join("state"))
            .env_remove("PHUX_SOCKET")
            .env_remove("PHUX_WS_TOKENS")
            .env_remove("PHUX_WS_TLS_CERT")
            .env_remove("PHUX_WS_TLS_KEY")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    };
    run(&["new", "--json", "--empty", "-s", "conversation-pixels"]);
    let mut cx = gpu();
    cx.allow_parking();
    let handle = cx.update(|cx| {
        gpui_kit::init(cx);
        phux_desktop::app::open(
            phux_desktop::app::AppOptions {
                socket: socket.clone(),
                session: "conversation-pixels".into(),
                dev_telemetry: false,
                phux: binary.clone(),
                layout_path: root.join("layout.json"),
                fresh: false,
                initial_terminal: None,
                quick: false,
                startup_error: None,
            },
            cx,
        )
        .expect("native desktop")
    });
    let desk = (*handle)
        .update(&mut cx, |root, _, cx| {
            root.downcast::<gpui_kit::component::Root>()
                .unwrap()
                .read(cx)
                .view()
                .clone()
                .downcast::<phux_desktop::app::Desk>()
                .unwrap()
        })
        .unwrap();
    let wait = |cx: &mut HeadlessAppContext,
                condition: &dyn Fn(&phux_desktop::app::Desk) -> bool| {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            cx.run_until_parked();
            if cx.update(|cx| condition(desk.read(cx))) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "native conversation pixel deadline"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    wait(&mut cx, &|desk| {
        desk.focused_placement()
            .and_then(|placement| placement.terminal_id.as_ref())
            .is_some_and(|terminal| {
                desk.link
                    .as_ref()
                    .is_some_and(|link| link.connection.ready(terminal))
            })
    });
    let (placement, terminal) = cx.update(|cx| {
        let placement = desk.read(cx).focused_placement().unwrap();
        (placement.id.clone(), placement.terminal_id.clone().unwrap())
    });
    let selector = match &terminal {
        ResourceId::Local { id } => format!("@{id}"),
        ResourceId::Satellite { .. } => panic!("private fixture must be local"),
    };
    run(&[
        "agent",
        "session",
        "open",
        &selector,
        "--provider",
        "omp",
        "--native-id",
        "native-pixel",
    ]);
    run(&[
        "agent",
        "emit",
        &selector,
        "--type",
        "session_start",
        "--data",
        r#"{"provider":"omp"}"#,
    ]);
    for data in [
        r#"{"provider":"omp","schema":"phux.transcript/v1","entry":{"id":"user","role":"user","text":"Explain the native transcript.\nKeep this second line.","truncated":false,"final":true}}"#,
        r#"{"provider":"omp","schema":"phux.transcript/v1","entry":{"id":"thinking","role":"thinking","text":"Considering the **existing terminal**.","truncated":false,"final":true}}"#,
        r#"{"provider":"omp","schema":"phux.transcript/v1","entry":{"id":"tool","role":"tool","text":"","truncated":false,"final":true,"tool":{"name":"read","call_id":"tool","summary":"Read desktop architecture","status":"ok","output":""}}}"#,
        r###"{"provider":"omp","schema":"phux.transcript/v1","entry":{"id":"reply","role":"assistant","text":"## Native conversation\n\nThe **same terminal** owns the session.\n\n- Streamed prose\n- Stable tool rows\n\n`Escape` interrupts only a working turn.","truncated":false,"final":true}}"###,
    ] {
        run(&[
            "agent",
            "emit",
            &selector,
            "--type",
            "provider_raw",
            "--data",
            data,
        ]);
    }
    run(&["agent", "emit", &selector, "--type", "stop", "--data", "{}"]);
    wait(&mut cx, &|desk| {
        desk.link
            .as_ref()
            .and_then(|link| link.connection.conversation(&terminal))
            .is_some_and(|conversation| {
                conversation.entries.len() == 4
                    && conversation.state.status
                        == phux_client_core::session::agent_stream::AgentSessionStatus::Done
            })
    });
    cx.update_window(*handle, |_, window, cx| {
        desk.update(cx, |desk, cx| {
            desk.toggle_conversation(&placement, window, cx)
        });
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(*handle, |_, window, cx| window.draw(cx).clear(cx))
        .unwrap();
    let image = cx
        .capture_screenshot(*handle)
        .expect("offscreen native conversation");
    assert!(image.width() >= 600 && image.height() >= 400);
    let painted = image
        .pixels()
        .filter(|pixel| {
            let [r, g, b, a] = pixel.0;
            a > 0 && r > 140 && g > 140 && b > 140
        })
        .count();
    assert!(
        painted > 1000,
        "native prose and controls must paint, not an empty sheet ({painted} pixels)"
    );
    if let Some(path) = std::env::var_os("PHUX_DESKTOP_PIXEL_EVIDENCE") {
        image.save(&path).expect("native conversation evidence PNG");
        println!(
            "native conversation evidence: {}",
            PathBuf::from(path).display()
        );
    }
    cx.update(|cx| desk.update(cx, |desk, cx| desk.prepare_close(cx)));
}
