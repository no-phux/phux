//! `phux watch --json` reports a different `client_name` for the interactive
//! client, a headless verb, and the MCP adapter (`docs/building.md#who-is-typing`).

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests"
)]

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::common::{self, PtyAttach, ServerGuard};
use crate::runner;

const SESSION: &str = "work";
const DEADLINE: Duration = Duration::from_secs(20);

/// `phux watch --json` lines, killed on drop.
struct Watch {
    child: Child,
    lines: Receiver<String>,
}

impl Drop for Watch {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Watch {
    fn start(server: &ServerGuard) -> Self {
        let mut child = server
            .cmd(&["watch", "--json", SESSION])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn phux watch --json");
        let stdout = child.stdout.take().expect("watch stdout");
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Self { child, lines }
    }
}

/// One `phux-mcp` process speaking newline JSON-RPC.
struct Mcp {
    child: Child,
    stdin: std::process::ChildStdin,
    lines: Receiver<String>,
    id: u32,
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Mcp {
    fn start(socket: &Path) -> Self {
        let mut command = Command::new(mcp_bin());
        for key in common::ambient_phux_keys() {
            command.env_remove(key);
        }
        command
            .env("PHUX_SOCKET", socket)
            .env("PHUX_PROFILE", "controller-client-e2e")
            .env(common::NO_UPDATE_CHECK_ENV, common::NO_UPDATE_CHECK_VALUE)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = command.spawn().expect("spawn phux-mcp");
        let stdin = child.stdin.take().expect("mcp stdin");
        let stdout = child.stdout.take().expect("mcp stdout");
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let mut mcp = Self {
            child,
            stdin,
            lines,
            id: 0,
        };
        let initialized = mcp.request("initialize", &json!({"protocolVersion": "2025-06-18"}));
        assert_eq!(
            initialized["protocolVersion"], "2025-06-18",
            "{initialized}"
        );
        mcp
    }

    fn send_key(&mut self, socket: &Path) {
        let result = self.request(
            "tools/call",
            &json!({
                "name": "phux_send_keys",
                "arguments": {"target": SESSION, "keys": ["m"], "socket": socket}
            }),
        );
        assert_eq!(result["isError"], false, "{result}");
    }

    fn request(&mut self, method: &str, params: &Value) -> Value {
        self.id += 1;
        let id = self.id;
        let message = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(self.stdin, "{message}").expect("write mcp");
        self.stdin.flush().expect("flush mcp");
        let deadline = Instant::now() + DEADLINE;
        while Instant::now() < deadline {
            match self.lines.recv_timeout(Duration::from_millis(200)) {
                Ok(line) => {
                    let Ok(response) = serde_json::from_str::<Value>(&line) else {
                        continue;
                    };
                    if response["id"] != id {
                        continue;
                    }
                    assert!(response.get("error").is_none(), "{response}");
                    return response["result"].clone();
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(err) => panic!("mcp closed while waiting for {method}: {err}"),
            }
        }
        panic!("mcp timed out waiting for {method}");
    }
}

fn mcp_bin() -> std::path::PathBuf {
    if let Some(path) = std::env::var_os("PHUX_TEST_MCP_BIN") {
        return path.into();
    }
    let built = Path::new(runner::phux_bin()).with_file_name("phux-mcp");
    assert!(
        built.exists(),
        "{} is missing: build phux-mcp or set PHUX_TEST_MCP_BIN",
        built.display()
    );
    built
}

fn drain_controllers(lines: &Receiver<String>, names: &mut Vec<String>) {
    while let Ok(line) = lines.try_recv() {
        let Ok(event) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if event["event"] != "controller_changed" {
            continue;
        }
        if let Some(name) = event["controller"]["client_name"].as_str() {
            names.push(name.to_owned());
        }
    }
}

/// Index of a `product/<version>` name at or after `from`.
fn index_of(names: &[String], product: &str, from: usize) -> Option<usize> {
    names
        .iter()
        .enumerate()
        .skip(from)
        .find_map(|(index, name)| {
            let (token, version) = name.rsplit_once('/')?;
            (token == product && !version.is_empty()).then_some(index)
        })
}

fn send_keys(server: &ServerGuard) {
    let (code, stdout, stderr) = server.run(&["send-keys", SESSION, "c"]);
    assert_eq!(
        code, 0,
        "send-keys exited {code}; stdout={stdout}; stderr={stderr}"
    );
}

fn wait_until_attached(server: &ServerGuard) {
    let deadline = Instant::now() + DEADLINE;
    while Instant::now() < deadline {
        let (code, stdout, _) = server.run(&["ls", "--json"]);
        if code == 0
            && serde_json::from_str::<Value>(&stdout).is_ok_and(|doc| {
                doc["sessions"].as_array().is_some_and(|sessions| {
                    sessions
                        .iter()
                        .any(|session| session["name"] == SESSION && session["attached"] == true)
                })
            })
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("session {SESSION} did not attach");
}

fn wait_for_product(
    watch: &Watch,
    names: &mut Vec<String>,
    product: &str,
    from: usize,
    mut nudge: impl FnMut(),
) -> usize {
    let deadline = Instant::now() + DEADLINE;
    let mut last_nudge = None;
    while Instant::now() < deadline {
        drain_controllers(&watch.lines, names);
        if let Some(index) = index_of(names, product, from) {
            return index;
        }
        let due = last_nudge.is_none_or(|at: Instant| at.elapsed() >= Duration::from_millis(400));
        if due {
            nudge();
            last_nudge = Some(Instant::now());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("watch did not report {product} after index {from}; saw {names:?}");
}

#[test]
#[ignore = "spawns a real server, TUI, and MCP adapter; run via `just e2e`."]
fn watch_json_distinguishes_tui_cli_and_mcp() {
    let server = ServerGuard::start("controller-client");
    let watch = Watch::start(&server);
    let mut names = Vec::new();
    // A key from a fresh connection rewrites the record, so repeating
    // `send-keys` until one line arrives proves the watcher is subscribed.
    wait_for_product(&watch, &mut names, "phux-cli", 0, || send_keys(&server));

    let mut tui = PtyAttach::start(&server.socket, &[SESSION], (80, 24), &[]);
    wait_until_attached(&server);
    let tui_at = wait_for_product(&watch, &mut names, "phux-tui", 0, || tui.send(b"x"));

    let cli_at = wait_for_product(&watch, &mut names, "phux-cli", tui_at + 1, || {
        send_keys(&server);
    });

    let mut mcp = Mcp::start(&server.socket);
    wait_for_product(&watch, &mut names, "phux-mcp", cli_at + 1, || {
        mcp.send_key(&server.socket);
    });

    let version = env!("CARGO_PKG_VERSION");
    for name in &names {
        let Some((product, ver)) = name.rsplit_once('/') else {
            panic!("client_name is not name/version: {name}");
        };
        assert!(!product.is_empty(), "{name}");
        assert_eq!(ver, version, "{name}");
    }
    let products: Vec<&str> = names
        .iter()
        .filter_map(|name| name.rsplit_once('/').map(|(product, _)| product))
        .collect();
    assert!(products.contains(&"phux-tui"), "{names:?}");
    assert!(products.contains(&"phux-cli"), "{names:?}");
    assert!(products.contains(&"phux-mcp"), "{names:?}");
}
