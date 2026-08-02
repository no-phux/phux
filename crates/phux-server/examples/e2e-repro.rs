//! `e2e-repro` — a one-command, real-server repro of a lag/crash edge case.
//!
//! This is the human-facing end of the e2e flywheel: it spins a REAL
//! `phux` server (the public [`ServerRuntime`] API, no mocks, a real PTY
//! pane), attaches a client over a UDS, drives a scripted scenario —
//! heavy colored output, a resize storm, a second client attaching
//! mid-stream — and writes screen snapshots plus a server trace dump to
//! `/tmp/phux-repro-*` for inspection. Run it, read the artifacts, file
//! the bug.
//!
//! Usage:
//!   cargo run -p phux-server --example e2e-repro
//!
//! The example is intentionally self-contained: examples cannot reach the
//! `tests/common` harness (Cargo compiles them as separate targets), so it
//! replicates the small spin-up (server on a `LocalSet`, connect, drain
//! `ATTACHED + TERMINAL_SNAPSHOT`, render VT bytes via a fresh libghostty
//! `Terminal`) here. The shape mirrors `tests/common/builder.rs`; this is
//! the standalone diagnostic twin of that suite-internal harness.

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::struct_field_names,
    reason = "standalone diagnostic harness, not library code"
)]

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use libghostty_vt::render::{CellIterator, RenderState, RowIterator};
use libghostty_vt::screen::CellWide;
use libghostty_vt::{Terminal as GhosttyTerminal, TerminalOptions};
use phux_protocol::input::paste::{PasteEvent, PasteTrust};
use phux_protocol::wire::frame::{
    AttachTarget, FrameKind, TYPE_ATTACHED, TYPE_BOOTSTRAP_BEGIN, TYPE_TERMINAL_OUTPUT,
    ViewportInfo,
};
use phux_server::{ServerConfig, ServerRuntime};
use portable_pty::CommandBuilder;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::UnixStream;
use tokio::time::timeout;

const COLS: u16 = 80;
const ROWS: u16 = 40;
const RECV_TIMEOUT: Duration = Duration::from_secs(10);

fn main() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, run());
}

async fn run() {
    // Install the process-global tracing subscriber so the hot-path spans
    // (`tick_emit`, `synthesize_against_reference`, `handle_attach`,
    // `handle_command`) actually emit. Honors `PHUX_LOG` / `PHUX_LOG_FORMAT`
    // / `RUST_LOG` exactly like the server binary: run this example with
    // `PHUX_LOG=/tmp/phux-repro.jsonl PHUX_LOG_FORMAT=json RUST_LOG=phux=debug`
    // to capture a jq-able trace with span-close durations. The returned
    // `WorkerGuard` must outlive the run to keep the file writer flushing;
    // bind it for the body. `init` only errs if a subscriber is already
    // installed — benign here, so we ignore that.
    let _log_guard = phux_server::telemetry::init().ok().flatten();

    // Artifacts go under /tmp/phux-repro-<ts>/.
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default();
    let out_dir = std::env::temp_dir().join(format!("phux-repro-{ts}"));
    std::fs::create_dir_all(&out_dir).expect("create artifact dir");
    eprintln!("[e2e-repro] artifacts -> {}", out_dir.display());

    let sock_dir = tempfile::tempdir().expect("tempdir");
    let socket_path = sock_dir.path().join("phux.sock");

    // The scripted scenario, all in one seed pane: a heavy colored burst,
    // then echo the kernel winsize on a loop (so the resize storm is
    // observable), then a marker, then idle.
    //
    // The burst is the WORST-case colored shape — the user's actual lag
    // symptom (zsh completion menu / syntax highlight): every cell of
    // every row carries its own 256-color SGR, so an SGR change roughly
    // every other column, rewritten in place (`\033[H`) each generation.
    // This is the same shape the `perf_colored_output` gate drives and the
    // heaviest churn for the per-consumer diff + client VT apply/render.
    // Keep it in sync with `tests/common/builder::colored_burst_command`.
    let mut script = String::new();
    script.push_str("sleep 0.2; cols=80; rows=40; ");
    script.push_str("for g in $(seq 1 10); do ");
    script.push_str("printf '\\033[H'; ");
    script.push_str("r=1; while [ \"$r\" -le \"$rows\" ]; do ");
    script.push_str("line=''; c=1; while [ \"$c\" -le \"$cols\" ]; do ");
    script.push_str("n=$(( 16 + (c + r + g) % 216 )); ");
    script.push_str("line=\"$line\\033[38;5;${n}mX\"; c=$(( c + 1 )); done; ");
    script.push_str("printf \"%b\\033[0m\\r\\n\" \"$line\"; r=$(( r + 1 )); done; done; ");
    script.push_str("printf '\\033[0mBURST_DONE\\r\\n'; ");
    // Resize observability: loop stty size so we catch post-resize dims.
    script.push_str("for i in $(seq 1 60); do stty size; sleep 0.05; done; ");
    script.push_str("printf 'SCRIPT_DONE\\r\\n'; sleep 30");

    let mut cmd = CommandBuilder::new("/bin/sh");
    cmd.args(["-c", &script]);

    let (shutdown_tx, server_handle) = spawn_server(socket_path.clone(), "default", cmd);

    // ---- client 1 attaches ----
    let mut c1 = Client::attach(&socket_path, "default", COLS, ROWS).await;
    eprintln!("[e2e-repro] client 1 attached (client_id={})", c1.client_id);

    // ---- heavy colored burst: drain to BURST_DONE ----
    c1.drain_until(|s| s.contains("BURST_DONE")).await;
    write_snapshot(&out_dir, "01-after-burst.txt", &c1.render());
    eprintln!("[e2e-repro] colored burst rendered");

    // ---- second client attaches mid-stream ----
    let mut c2 = Client::attach(&socket_path, "default", COLS, ROWS).await;
    eprintln!("[e2e-repro] client 2 attached (client_id={})", c2.client_id);
    c2.drain_brief().await;
    write_snapshot(&out_dir, "02-client2-attach.txt", &c2.render());

    // ---- resize storm from client 1 ----
    let storm: &[(u16, u16)] = &[(100, 30), (120, 45), (90, 50), (140, 38), (110, 42)];
    for &(cols, rows) in storm {
        c1.resize(cols, rows).await;
    }
    let (fc, fr) = (128u16, 44u16);
    c1.resize(fc, fr).await;
    // `stty size` prints `<rows> <cols>`.
    let needle = format!("{fr} {fc}");
    c1.drain_until(|s| s.contains(&needle)).await;
    write_snapshot(&out_dir, "03-after-resize-storm.txt", &c1.render());
    eprintln!("[e2e-repro] resize storm settled at {fc}x{fr}");

    // ---- a scripted line of input via paste ----
    c1.send_text("echo hello-from-repro\r").await;
    c1.drain_brief().await;
    write_snapshot(&out_dir, "04-after-input.txt", &c1.render());

    // ---- a small trace summary artifact ----
    let summary = format!(
        "phux e2e-repro\nsocket: {}\nclients: 2\nfinal viewport: {fc}x{fr}\n\
         scenario: heavy colored burst -> 2nd client attach -> resize storm -> input\n",
        socket_path.display(),
    );
    write_snapshot(&out_dir, "00-summary.txt", &summary);

    eprintln!("[e2e-repro] done. snapshots in {}", out_dir.display());

    // ---- clean teardown ----
    drop(c1);
    drop(c2);
    shutdown_tx.send(()).ok();
    let _ = timeout(Duration::from_secs(5), server_handle).await;
}

/// Spawn a `ServerRuntime` with a PTY-backed seed pane on the current
/// `LocalSet`. Mirrors `tests/common::spawn_server_with_seed_cmd`.
fn spawn_server(
    socket_path: PathBuf,
    session: &str,
    cmd: CommandBuilder,
) -> (
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<Result<(), phux_server::ServerError>>,
) {
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let cfg = ServerConfig {
        socket_path,
        pre_seeded_session: Some(session.to_owned()),
        seed_with_pty: true,
        seed_command: Some(cmd),
        ..ServerConfig::with_default_socket()
    };
    let handle = tokio::task::spawn_local(async move {
        let server = ServerRuntime::new(cfg);
        server
            .run_async(async move {
                let _ = rx.await;
            })
            .await
    });
    (tx, handle)
}

/// A minimal attached client: a wire stream plus an accumulator of all VT
/// bytes (snapshot + every `TERMINAL_OUTPUT`). `render()` feeds the
/// accumulator into a fresh libghostty `Terminal` for a text snapshot.
struct Client {
    stream: UnixStream,
    vt: Vec<u8>,
    client_id: u32,
    terminal_id: phux_protocol::TerminalId,
    viewport: ViewportInfo,
}

impl Client {
    async fn attach(socket_path: &Path, session: &str, cols: u16, rows: u16) -> Self {
        let mut stream = connect(socket_path).await;
        let viewport = ViewportInfo::new(cols, rows);
        send(
            &mut stream,
            &FrameKind::Attach {
                attach_id: 1,
                target: AttachTarget::ByName(session.to_owned()),
                viewport,
                request_scrollback: false,
                scrollback_limit_lines: 0,
            },
        )
        .await;

        let (tb, attached) = recv(&mut stream).await;
        assert_eq!(tb, TYPE_ATTACHED, "first frame must be ATTACHED");
        let (client_id, terminal_id) = match attached {
            FrameKind::Attached {
                attach_id: _,
                initial_client_id,
                snapshot,
            } => {
                let pane = snapshot.panes.first().expect("attach snapshot has a pane");
                (initial_client_id.get(), pane.id.clone())
            }
            other => panic!("expected Attached, got {other:?}"),
        };

        let mut vt = Vec::new();
        let (snap_tb, begin) = recv(&mut stream).await;
        assert_eq!(snap_tb, TYPE_BOOTSTRAP_BEGIN, "second frame must be BEGIN");
        assert!(matches!(begin, FrameKind::BootstrapBegin { .. }));
        let (_, chunk) = recv(&mut stream).await;
        if let FrameKind::BootstrapChunk { payload, .. } = chunk {
            vt.extend_from_slice(&payload);
        } else {
            panic!("expected BootstrapChunk");
        }
        let (_, ready) = recv(&mut stream).await;
        assert!(matches!(ready, FrameKind::BootstrapReady { .. }));

        Self {
            stream,
            vt,
            client_id,
            terminal_id,
            viewport,
        }
    }

    /// Drain `TERMINAL_OUTPUT` into the accumulator until `pred` holds on
    /// the rendered text or the recv timeout elapses.
    async fn drain_until<P: Fn(&str) -> bool>(&mut self, pred: P) {
        if pred(&self.render()) {
            return;
        }
        let deadline = Instant::now() + RECV_TIMEOUT;
        while Instant::now() < deadline {
            let remaining = deadline - Instant::now();
            let Ok((tb, frame)) = timeout(remaining, recv(&mut self.stream)).await else {
                break;
            };
            if tb == TYPE_TERMINAL_OUTPUT
                && let FrameKind::TerminalOutput { bytes, .. } = frame
            {
                self.vt.extend_from_slice(&bytes);
                if pred(&self.render()) {
                    return;
                }
            }
        }
    }

    /// Drain whatever is immediately available (a short settle window).
    async fn drain_brief(&mut self) {
        let deadline = Instant::now() + Duration::from_millis(400);
        while Instant::now() < deadline {
            match timeout(Duration::from_millis(100), recv(&mut self.stream)).await {
                Ok((tb, FrameKind::TerminalOutput { bytes, .. })) if tb == TYPE_TERMINAL_OUTPUT => {
                    self.vt.extend_from_slice(&bytes);
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
    }

    async fn resize(&mut self, cols: u16, rows: u16) {
        self.viewport = ViewportInfo::new(cols, rows);
        send(
            &mut self.stream,
            &FrameKind::ViewportResize {
                viewport: self.viewport,
            },
        )
        .await;
    }

    /// Send `text` as an `INPUT_PASTE` targeted at the focused pane (whose
    /// terminal id was captured from the attach snapshot).
    async fn send_text(&mut self, text: &str) {
        send(
            &mut self.stream,
            &FrameKind::InputPaste {
                terminal_id: self.terminal_id.clone(),
                event: PasteEvent {
                    trust: PasteTrust::Trusted,
                    data: text.as_bytes().to_vec(),
                },
            },
        )
        .await;
    }

    /// Render the accumulated VT bytes into a row-major text snapshot via a
    /// fresh libghostty `Terminal` (the same parse a real terminal does).
    fn render(&self) -> String {
        render_vt(&self.vt, self.viewport.cols, self.viewport.rows)
    }
}

// ---------------------------------------------------------------------------
// Wire + render helpers (replicated from tests/common; examples can't reach it)
// ---------------------------------------------------------------------------

/// Poll-connect to the UDS until success or a deadline. The socket file is
/// racy with the bind sequence; only an actual connect is race-free.
async fn connect(path: &Path) -> UnixStream {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut last_err = None;
    while Instant::now() < deadline {
        match UnixStream::connect(path).await {
            Ok(s) => return s,
            Err(e) => last_err = Some(e),
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("socket {} never connectable: {last_err:?}", path.display());
}

/// Encode + write a length-prefixed frame.
async fn send(stream: &mut UnixStream, frame: &FrameKind) {
    let mut buf = bytes::BytesMut::new();
    frame.encode(&mut buf);
    stream.write_all(&buf).await.unwrap();
    stream.flush().await.unwrap();
}

/// Read one length-prefixed frame; returns the type byte + decoded frame.
async fn recv(stream: &mut UnixStream) -> (u8, FrameKind) {
    let mut header = [0u8; 4];
    stream.read_exact(&mut header).await.unwrap();
    let body_len = u32::from_be_bytes(header) as usize;
    let mut body = vec![0u8; body_len];
    stream.read_exact(&mut body).await.unwrap();
    let mut framed = Vec::with_capacity(4 + body_len);
    framed.extend_from_slice(&header);
    framed.extend_from_slice(&body);
    let type_byte = framed[4];
    let (frame, rest) = FrameKind::decode(&framed).expect("decode frame");
    assert!(rest.is_empty(), "decoder left trailing bytes");
    (type_byte, frame)
}

/// Write a snapshot artifact to `dir/name`.
fn write_snapshot(dir: &Path, name: &str, body: &str) {
    let path = dir.join(name);
    let mut f = std::fs::File::create(&path).expect("create snapshot file");
    f.write_all(body.as_bytes()).expect("write snapshot");
}

/// Render VT bytes into a row-major plain-text grid via a fresh libghostty
/// `Terminal`. Wide-cell tails are skipped (mirrors the server's grid
/// walk). This is the standalone twin of `tests/common/screen.rs`.
fn render_vt(bytes: &[u8], cols: u16, rows: u16) -> String {
    let mut term = GhosttyTerminal::new(TerminalOptions {
        cols,
        rows,
        max_scrollback: 200,
    })
    .expect("terminal");
    term.vt_write(bytes);

    let mut state = RenderState::new().expect("render state");
    let mut row_iter = RowIterator::new().expect("rows");
    let mut cell_iter = CellIterator::new().expect("cells");
    let Ok(snap) = state.update(&term) else {
        return String::new();
    };
    let total = snap.rows().unwrap_or(rows);
    let Ok(mut ri) = row_iter.update(&snap) else {
        return String::new();
    };

    let mut out: Vec<String> = Vec::with_capacity(usize::from(total));
    let mut idx: u16 = 0;
    while let Some(row) = ri.next() {
        if idx >= total {
            break;
        }
        let mut line = String::with_capacity(usize::from(cols));
        if let Ok(mut ci) = cell_iter.update(row) {
            while let Some(cell) = ci.next() {
                let wide = cell
                    .raw_cell()
                    .and_then(libghostty_vt::screen::Cell::wide)
                    .unwrap_or(CellWide::Narrow);
                if matches!(wide, CellWide::SpacerTail) {
                    continue;
                }
                let graphemes = cell.graphemes().unwrap_or_default();
                if graphemes.is_empty() {
                    line.push(' ');
                } else {
                    for ch in graphemes {
                        line.push(ch);
                    }
                }
            }
        }
        out.push(line.trim_end().to_owned());
        idx += 1;
    }
    out.join("\n")
}
