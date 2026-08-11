//! Black-box acceptance proof for the first-five-minutes journey (phux-4gju.4).
//!
//! This test deliberately starts at an install-shaped prefix rather than at
//! Cargo's target directory: both built release payload binaries are copied
//! into a fresh `bin/`, and every invocation resolves `phux` from there.
//! Publication mechanics are a separate concern. `scripts/test-install.sh`
//! already proves archive layout, checksums, PATH guidance, atomic publish,
//! and rollback, so reproducing a synthetic release archive here would make
//! this slower without strengthening the interactive contract.

#![allow(clippy::expect_used, reason = "acceptance harness")]
#![allow(clippy::panic, reason = "acceptance harness")]
#![allow(clippy::print_stderr, reason = "failure must print retained artifacts")]
#![allow(clippy::unwrap_used, reason = "acceptance harness")]

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

const BUILT_PHUX: &str = env!("CARGO_BIN_EXE_phux");
const PROFILE: &str = "first-five-e2e";
const MARKER: &str = "FIRST_FIVE_COMMAND_RAN";
const DEADLINE: Duration = Duration::from_secs(30);
const GRACEFUL_SHUTDOWN: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(50);

struct Harness {
    root: Option<tempfile::TempDir>,
    phux: PathBuf,
    socket: PathBuf,
    home: PathBuf,
    config: PathBuf,
    cache: PathBuf,
    data: PathBuf,
    state: PathBuf,
    runtime: PathBuf,
    prefix: PathBuf,
    server_pid: Option<u32>,
    server_session: Option<String>,
}

impl Harness {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("create first-five artifact directory");
        let home = root.path().join("home");
        let config = root.path().join("config");
        let cache = root.path().join("cache");
        let data = root.path().join("data");
        let state = root.path().join("state");
        let runtime = root.path().join("runtime");
        let prefix = root.path().join("prefix/bin");
        for dir in [&home, &config, &cache, &data, &state, &runtime, &prefix] {
            std::fs::create_dir_all(dir).expect("create isolated directory");
        }

        let phux = prefix.join(executable_name("phux"));
        std::fs::copy(BUILT_PHUX, &phux).expect("copy built phux into fresh prefix");

        let built_dir = Path::new(BUILT_PHUX)
            .parent()
            .expect("built phux has a parent directory");
        let built_mcp = built_dir.join(executable_name("phux-mcp"));
        let installed_mcp = prefix.join(executable_name("phux-mcp"));
        std::fs::copy(&built_mcp, &installed_mcp).unwrap_or_else(|err| {
            panic!(
                "copy built companion {} into fresh prefix (run `cargo build -p phux-mcp` first): {err}",
                built_mcp.display()
            )
        });

        Self {
            root: Some(root),
            phux,
            socket: runtime.join("first-five.sock"),
            home,
            config,
            cache,
            data,
            state,
            runtime,
            prefix,
            server_pid: None,
            server_session: None,
        }
    }

    fn apply_command_env(&self, command: &mut Command) {
        command
            .env_clear()
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", &self.config)
            .env("XDG_CONFIG_DIRS", &self.config)
            .env("XDG_CACHE_HOME", &self.cache)
            .env("XDG_DATA_HOME", &self.data)
            .env("XDG_DATA_DIRS", &self.data)
            .env("XDG_STATE_HOME", &self.state)
            .env("XDG_RUNTIME_DIR", &self.runtime)
            .env("PHUX_PROFILE", PROFILE)
            .env("PHUX_SOCKET", &self.socket)
            .env("SHELL", "/bin/sh")
            .env("TERM", "xterm-256color")
            .env("RUST_LOG", "off")
            .env("PATH", self.path_value());
    }

    fn apply_pty_env(&self, command: &mut CommandBuilder) {
        command.env_clear();
        for (key, value) in [
            ("HOME", self.home.as_os_str()),
            ("XDG_CONFIG_HOME", self.config.as_os_str()),
            ("XDG_CONFIG_DIRS", self.config.as_os_str()),
            ("XDG_CACHE_HOME", self.cache.as_os_str()),
            ("XDG_DATA_HOME", self.data.as_os_str()),
            ("XDG_DATA_DIRS", self.data.as_os_str()),
            ("XDG_STATE_HOME", self.state.as_os_str()),
            ("XDG_RUNTIME_DIR", self.runtime.as_os_str()),
            ("PHUX_SOCKET", self.socket.as_os_str()),
        ] {
            command.env(key, value);
        }
        command.env("PHUX_PROFILE", PROFILE);
        command.env("SHELL", "/bin/sh");
        command.env("TERM", "xterm-256color");
        command.env("RUST_LOG", "off");
        command.env("PATH", self.path_value());
    }

    fn path_value(&self) -> OsString {
        let mut paths = vec![self.prefix.clone()];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        std::env::join_paths(paths).expect("construct isolated PATH")
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(&self.phux);
        command.args(args);
        self.apply_command_env(&mut command);
        command
    }

    fn capture(&self, label: &str, args: &[&str]) -> Output {
        let output = self
            .command(args)
            .stdin(Stdio::null())
            .output()
            .unwrap_or_else(|err| panic!("run {label}: {err}"));
        self.write_output(label, &output);
        output
    }

    fn write_output(&self, label: &str, output: &Output) {
        let root = self.root.as_ref().expect("artifact root").path();
        std::fs::write(root.join(format!("{label}.stdout")), &output.stdout)
            .expect("write captured stdout");
        std::fs::write(root.join(format!("{label}.stderr")), &output.stderr)
            .expect("write captured stderr");
    }

    fn remember_server_pid(&mut self) {
        let output = self.capture("status", &["status", "--json"]);
        assert!(output.status.success(), "status failed: {output:?}");
        let doc: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("status emits JSON");
        self.server_pid = Some(
            u32::try_from(doc["pid"].as_u64().expect("status JSON names server pid"))
                .expect("server pid fits u32"),
        );
        self.server_session = Some(
            doc["sessions"][0]["name"]
                .as_str()
                .expect("status JSON names the first session")
                .to_owned(),
        );
    }

    fn cleanup(&mut self) -> Result<(), String> {
        let deadline = Instant::now() + DEADLINE;
        let socket_was_observed = self.socket.exists();
        while self.server_pid.is_none() && self.socket.exists() && Instant::now() < deadline {
            let output = self.capture("cleanup-status", &["status", "--json"]);
            if output.status.success()
                && let Ok(doc) = serde_json::from_slice::<serde_json::Value>(&output.stdout)
            {
                self.server_pid = doc["pid"].as_u64().and_then(|pid| u32::try_from(pid).ok());
                self.server_session = doc["sessions"][0]["name"].as_str().map(ToOwned::to_owned);
            }
            if self.server_pid.is_none() {
                std::thread::sleep(POLL);
            }
        }

        let Some(pid) = self.server_pid else {
            if socket_was_observed {
                return Err(format!(
                    "cleanup could not discover the server PID; socket_exists={}, path={}",
                    self.socket.exists(),
                    self.socket.display(),
                ));
            }
            return Ok(());
        };

        let deadline = Instant::now() + DEADLINE;
        let graceful = self.server_session.as_deref().is_some_and(|session| {
            self.capture("cleanup-kill", &["kill", session])
                .status
                .success()
        });
        let graceful_deadline = Instant::now() + GRACEFUL_SHUTDOWN;
        let mut term_sent = if graceful {
            false
        } else {
            let _ = Command::new("kill")
                .args(["-TERM", &pid.to_string()])
                .output();
            true
        };

        loop {
            let process_gone = !process_exists(pid);
            if !self.socket.exists() && process_gone {
                return Ok(());
            }
            if process_gone && self.socket.exists() {
                std::fs::remove_file(&self.socket)
                    .map_err(|err| format!("remove stale cleanup socket: {err}"))?;
                continue;
            }
            if !process_gone && !term_sent && Instant::now() >= graceful_deadline {
                let _ = Command::new("kill")
                    .args(["-TERM", &pid.to_string()])
                    .output();
                term_sent = true;
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "cleanup timed out: socket_exists={}, server_pid={pid}, process_exists={}",
                    self.socket.exists(),
                    process_exists(pid),
                ));
            }
            std::thread::sleep(POLL);
        }
    }

    fn retain(mut self) -> PathBuf {
        self.root.take().expect("artifact root").keep()
    }
}

fn executable_name(stem: &str) -> String {
    format!("{stem}{}", std::env::consts::EXE_SUFFIX)
}

fn process_exists(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

struct PtyClient {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    writer: Box<dyn Write + Send>,
    transcript: Arc<Mutex<Vec<u8>>>,
    reader: Option<JoinHandle<()>>,
    reader_done: mpsc::Receiver<()>,
    child_exited: bool,
}

impl Drop for PtyClient {
    fn drop(&mut self) {
        if !self.child_exited {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        match self.reader_done.recv_timeout(DEADLINE) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                if let Some(reader) = self.reader.take() {
                    let _ = reader.join();
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}

impl PtyClient {
    fn naked(harness: &Harness, label: &str) -> Self {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 30,
                cols: 110,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("open first-five PTY");
        let mut command = CommandBuilder::new(&harness.phux);
        harness.apply_pty_env(&mut command);
        let child = pair
            .slave
            .spawn_command(command)
            .expect("spawn naked phux from fresh prefix");
        drop(pair.slave);

        let transcript = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&transcript);
        let transcript_path = harness
            .root
            .as_ref()
            .expect("artifact root")
            .path()
            .join(format!("{label}.pty"));
        let mut artifact = std::fs::File::create(transcript_path).expect("create PTY artifact");
        let mut reader = pair.master.try_clone_reader().expect("clone PTY reader");
        let (reader_done_tx, reader_done) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut bytes = [0u8; 8192];
            while let Ok(count) = reader.read(&mut bytes) {
                if count == 0 {
                    break;
                }
                sink.lock()
                    .expect("transcript lock")
                    .extend_from_slice(&bytes[..count]);
                let _ = artifact.write_all(&bytes[..count]);
                let _ = artifact.flush();
            }
            let _ = reader_done_tx.send(());
        });

        Self {
            child,
            writer: pair.master.take_writer().expect("take PTY writer"),
            transcript,
            reader: Some(reader),
            reader_done,
            child_exited: false,
        }
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.transcript.lock().expect("transcript lock")).into_owned()
    }

    fn visible_text(&self) -> String {
        strip_terminal_controls(&self.transcript.lock().expect("transcript lock"))
    }

    fn wait_for(&mut self, description: &str, predicate: impl Fn(&str) -> bool) {
        let deadline = Instant::now() + DEADLINE;
        loop {
            let visible = self.visible_text();
            if predicate(&visible) {
                return;
            }
            if let Some(status) = self.child.try_wait().expect("poll attached client") {
                panic!(
                    "client exited before {description} (status {:?}); visible transcript:\n{visible}\nraw transcript:\n{}",
                    status.exit_code(),
                    self.text(),
                );
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {description}; visible transcript:\n{visible}\nraw transcript:\n{}",
                self.text()
            );
            std::thread::sleep(POLL);
        }
    }

    fn send(&mut self, bytes: &[u8]) {
        self.writer.write_all(bytes).expect("write PTY input");
        self.writer.flush().expect("flush PTY input");
    }

    fn wait_for_exit(&mut self) {
        let deadline = Instant::now() + DEADLINE;
        loop {
            if let Some(status) = self.child.try_wait().expect("poll attached client") {
                self.child_exited = true;
                self.drain_reader(Instant::now() + DEADLINE);
                assert!(
                    status.success(),
                    "attached client exited {:?}; transcript:\n{}",
                    status.exit_code(),
                    self.text()
                );
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for detach; transcript:\n{}",
                self.text()
            );
            std::thread::sleep(POLL);
        }
    }

    fn drain_reader(&mut self, deadline: Instant) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        self.reader_done
            .recv_timeout(remaining)
            .expect("PTY reader did not reach EOF after client exit");
        self.reader
            .take()
            .expect("PTY reader thread")
            .join()
            .expect("PTY reader thread panicked");
    }
}

/// Remove ECMA-48 control sequences while retaining printable transcript
/// bytes. This is intentionally a transcript view, not a screen emulator:
/// predicates must prove that a phrase was visibly painted at some point,
/// including renderers that put an SGR sequence between every character.
fn strip_terminal_controls(bytes: &[u8]) -> String {
    let mut printable = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != 0x1b {
            if bytes[index] >= 0x20 || matches!(bytes[index], b'\n' | b'\r' | b'\t') {
                printable.push(bytes[index]);
            }
            index += 1;
            continue;
        }

        index += 1;
        let Some(&kind) = bytes.get(index) else { break };
        index += 1;
        match kind {
            b'[' => {
                while let Some(&byte) = bytes.get(index) {
                    index += 1;
                    if (0x40..=0x7e).contains(&byte) {
                        break;
                    }
                }
            }
            b']' | b'P' | b'^' | b'_' => {
                while let Some(&byte) = bytes.get(index) {
                    index += 1;
                    if byte == 0x07 {
                        break;
                    }
                    if byte == 0x1b && bytes.get(index) == Some(&b'\\') {
                        index += 1;
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    String::from_utf8_lossy(&printable).into_owned()
}

fn assert_success(label: &str, output: &Output) -> String {
    assert!(
        output.status.success(),
        "{label} failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8(output.stdout.clone()).expect("command stdout is UTF-8")
}

fn run_journey(harness: &mut Harness) {
    let short = harness.capture("short-help", &["-h"]);
    let short = assert_success("phux -h", &short);
    assert!(short.contains("Start here:"), "short help:\n{short}");
    assert!(
        short.contains("Run `phux --help` for every command"),
        "short help must point to complete help:\n{short}"
    );
    assert!(
        !short.contains("ATTACH / SERVE"),
        "short help grew full:\n{short}"
    );

    let full = harness.capture("full-help", &["--help"]);
    let full = assert_success("phux --help", &full);
    assert!(full.contains("ATTACH / SERVE"), "full help:\n{full}");
    assert!(full.contains("EXIT STATUS"), "full help:\n{full}");
    assert!(
        full.lines().count() > short.lines().count() * 3,
        "--help must remain materially more complete than -h"
    );

    let mut first = PtyClient::naked(harness, "first-attach");
    first.wait_for("the first-use title", |text| {
        text.contains("Your session is live")
    });
    first.wait_for("the first-use persistence promise", |text| {
        text.contains("phux keeps this session alive")
    });
    first.wait_for("the active default detach binding", |text| {
        text.lines()
            .any(|line| line.contains("C-a d") && line.contains("leave this view"))
    });

    // The marker is not present in the input bytes as one contiguous string;
    // seeing it proves the shell executed this ordinary dismissing command.
    first.send(b"printf '%s%s\\n' FIRST_FIVE_ COMMAND_RAN\r");
    first.wait_for("output from the dismissing shell command", |text| {
        text.contains(MARKER)
    });
    harness.remember_server_pid();

    first.send(b"\x01d");
    first.wait_for_exit();
    first.wait_for("the cooked-terminal persistence message", |text| {
        text.contains("phux: session still running; run `phux` when you want to come back")
    });
    let mut second = PtyClient::naked(harness, "second-attach");
    second.wait_for("the preserved shell marker", |text| text.contains(MARKER));
    second.wait_for("the return confirmation", |text| {
        text.contains("Welcome back - this is the session you left running")
    });
    let second_text = second.visible_text();
    assert!(
        !second_text.contains("Your session is live"),
        "the introduction repeated on return:\n{second_text}"
    );
    assert!(
        !second_text.contains("phux keeps this session alive"),
        "the introductory promise repeated on return:\n{second_text}"
    );
    second.send(b"\x01d");
    second.wait_for_exit();

    let mistake = harness.capture("recoverable-mistake", &["--json", "ls"]);
    assert_eq!(mistake.status.code(), Some(2));
    assert!(
        mistake.stdout.is_empty(),
        "usage failure wrote stdout bytes"
    );
    let mistake_err = String::from_utf8(mistake.stderr).expect("usage stderr is UTF-8");
    assert_eq!(
        mistake_err,
        "error: unexpected argument '--json' found\n\n  tip: 'ls --json' exists\n\nUsage: phux [OPTIONS] [COMMAND]\n\nFor more information, try '--help'.\nhint: `--json` is set per verb, not on `phux` itself; place it after the verb: `phux <verb> --json ...`\n"
    );

    let redirected_socket = harness.runtime.join("redirected.sock");
    let mut redirected = harness.command(&[]);
    redirected.env("PHUX_SOCKET", &redirected_socket);
    let redirected = redirected
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run redirected naked phux");
    harness.write_output("redirected-naked", &redirected);
    assert_eq!(redirected.status.code(), Some(1));
    assert!(
        redirected.stdout.is_empty(),
        "redirected naked invocation emitted stdout/control bytes: {:?}",
        redirected.stdout
    );
    assert_eq!(
        String::from_utf8(redirected.stderr).expect("redirected stderr is UTF-8"),
        "phux: interactive use requires both stdin and stdout to be terminals\n"
    );
    assert!(
        !redirected_socket.exists(),
        "redirected invocation spawned a server at {}",
        redirected_socket.display()
    );
}

#[test]
#[ignore = "spawns a real auto-daemon and PTY clients; run serially via `just e2e`"]
fn installed_first_five_minutes_are_safe_and_coherent() {
    let mut harness = Harness::new();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_journey(&mut harness);
    }));
    let cleanup = harness.cleanup();

    match (outcome, cleanup) {
        (Ok(()), Ok(())) => {}
        (Ok(()), Err(err)) => {
            let artifacts = harness.retain();
            panic!(
                "first-five cleanup failed: {err}\nartifacts retained at {}",
                artifacts.display()
            );
        }
        (Err(payload), cleanup) => {
            let artifacts = harness.retain();
            eprintln!("first-five artifacts retained at {}", artifacts.display());
            if let Err(err) = cleanup {
                eprintln!("first-five cleanup also failed: {err}");
            }
            std::panic::resume_unwind(payload);
        }
    }
}
