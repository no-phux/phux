//! Real desktop window lifecycles against an explicitly supplied checkout CLI.

mod agent_stream;
mod capture;
mod federation;
mod identity;
mod layouts;
mod window_transfer;

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
        if port.is_some() {
            // A remote listener runs only under `paired` (ADR-0179).
            std::fs::create_dir_all(root.join("config/phux")).unwrap();
            std::fs::write(
                root.join("config/phux/config.toml"),
                "[policy]\nmode = \"paired\"\n",
            )
            .unwrap();
        }
        let socket = root.join("s");
        let child = Command::new(&binary)
            .current_dir(&root)
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
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    root.join("bin").display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
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
        self.output(args, b"");
    }

    /// `phux --socket S ARGS` in this server's private dirs, fed `stdin`;
    /// its stdout. Panics on failure.
    fn output(&self, args: &[&str], stdin: &[u8]) -> Vec<u8> {
        use std::io::Write as _;
        let mut command = Command::new(&self.binary);
        // Workload authority commands operate on isolated disk state and
        // explicitly reject socket selectors; every dialing verb uses ours.
        if args.first() != Some(&"workload") {
            command.arg("--socket").arg(&self.socket);
        }
        let mut child = command
            .args(args)
            .env("HOME", &self.root)
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_STATE_HOME", self.root.join("state"))
            .env("PHUX_PROFILE", "desktop-journey")
            .env_remove("PHUX_SOCKET")
            .env_remove("PHUX_WS_TOKENS")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(stdin).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    }

    /// The `[[remote]]` entry for this paired server's wss listener: a
    /// pairing token, the leaf pin, and a client certificate enrolled into
    /// its workload registry from a key `openssl` generated here.
    fn paired_remote_entry(&self, name: &str) -> String {
        let pair: serde_json::Value =
            serde_json::from_slice(&self.output(&["pair", "--json"], b"")).unwrap();
        let token = self.root.join("client.token");
        std::fs::write(&token, format!("{}\n", pair["token"].as_str().unwrap())).unwrap();
        let sec1 = self.root.join("client.sec1");
        let key = self.root.join("client.key");
        let csr = self.root.join("client.csr");
        let cert = self.root.join("client.pem");
        for args in [
            vec![
                "ecparam",
                "-name",
                "prime256v1",
                "-genkey",
                "-noout",
                "-out",
            ],
            vec!["pkcs8", "-topk8", "-nocrypt", "-in"],
        ] {
            let mut command = Command::new("openssl");
            command.args(&args);
            if args[0] == "ecparam" {
                command.arg(&sec1);
            } else {
                command.arg(&sec1).arg("-out").arg(&key);
            }
            assert!(command.status().unwrap().success(), "openssl {}", args[0]);
        }
        let made = Command::new("openssl")
            .args(["req", "-new", "-subj", "/CN=journey", "-key"])
            .arg(&key)
            .arg("-out")
            .arg(&csr)
            .status()
            .unwrap();
        assert!(made.success(), "openssl req");
        for path in [&token, &key] {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        self.run(&["workload", "authority", "--init"]);
        self.output(
            &[
                "workload",
                "add-key",
                "--cert-out",
                cert.to_str().unwrap(),
                "--scope",
                "*@global",
            ],
            &std::fs::read(&csr).unwrap(),
        );
        format!(
            "[[remote]]\nname = \"{name}\"\nendpoint = \"wss://{}\"\n\
             cert-fingerprint = \"{}\"\ntoken-file = \"{}\"\n\
             client-cert = \"{}\"\nclient-key = \"{}\"\n",
            pair["ws_addr"].as_str().unwrap(),
            pair["cert_fingerprint"].as_str().unwrap(),
            token.display(),
            cert.display(),
            key.display(),
        )
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

/// Press a key the way the macOS platform delivers it: one key-down dispatched
/// to its bindings (consumed or propagated) with no IME text insertion, then
/// key-up. GPUI's `simulate_keystrokes` also runs a
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
