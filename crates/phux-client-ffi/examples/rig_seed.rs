//! Seed the E2E rig through the same shared runtime the app drives.
//!
//! Run: `cargo run -p phux-client-ffi --example rig_seed --features uniffi`

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "this command-line fixture reports seeding progress directly to its operator"
)]

use std::time::{Duration, Instant};

use phux_client_runtime::control::{ControlOptions, Event, SpawnRequest, Status};
use phux_client_runtime::{Client, ClientOptions, ConnectOptions, Runtime, Target, Transport};
use phux_protocol::wire::frame::AttachTarget;

struct SessionSpec {
    name: &'static str,
    seed: Option<&'static [&'static str]>,
    extra_panes: &'static [Option<&'static [&'static str]>],
}

const SESSIONS: &[SessionSpec] = &[
    SessionSpec {
        name: "clock",
        seed: Some(&["sh", "-c", "while true; do date; sleep 1; done"]),
        extra_panes: &[Some(&[
            "sh",
            "-c",
            "i=0; while true; do i=$((i+1)); printf 'tick %s\\n' \"$i\"; sleep 2; done",
        ])],
    },
    SessionSpec {
        name: "build",
        seed: Some(&[
            "sh",
            "-c",
            "i=0; while true; do i=$((i+1)); \
             printf '\\033[32m   Compiling\\033[0m module-%s v0.%s.0\\n' \"$i\" \"$((i % 9))\"; \
             sleep 0.7; done",
        ]),
        extra_panes: &[None],
    },
    SessionSpec {
        name: "tui",
        seed: Some(&["top"]),
        extra_panes: &[None],
    },
];

fn main() {
    let url = std::env::var("RIG_WS_URL").unwrap_or_else(|_| "ws://127.0.0.1:8787".to_owned());
    for spec in SESSIONS {
        match seed_session(&url, spec) {
            Ok(spawned) => println!(
                "rig-seed: session '{}' ready ({} pane(s) spawned)",
                spec.name, spawned
            ),
            Err(error) => {
                eprintln!("rig-seed: session '{}' FAILED: {error}", spec.name);
                std::process::exit(1);
            }
        }
    }
    println!("rig-seed: done — sessions: clock, build, tui");
}

fn seed_session(url: &str, spec: &SessionSpec) -> Result<usize, String> {
    let client = Runtime::connect(
        Target {
            transport: Transport::Ws(url.to_owned()),
            name: url.to_owned(),
            cert_fingerprint: None,
            token_file: None,
            token: None,
        },
        ClientOptions {
            control: ControlOptions {
                client_name: "phux-mobile-rig-seed".to_owned(),
                viewport: (100, 30),
                scrollback_lines: 0,
                attach: Some(AttachTarget::CreateIfMissing {
                    name: spec.name.to_owned(),
                    command: command(spec.seed),
                    cwd: None,
                }),
                ..ControlOptions::default()
            },
            connect: ConnectOptions::default(),
        },
    )
    .map_err(|error| format!("connect {url}: {error}"))?;

    let topology = wait_for_topology(&client, spec.name)?;
    let session = topology
        .session_named(spec.name)
        .ok_or_else(|| "attached topology is missing the requested session".to_owned())?;
    let existing = topology
        .panes
        .iter()
        .filter(|pane| pane.session_id == session.id)
        .count();
    let target = 1 + spec.extra_panes.len();
    let mut spawned = 0;
    for (index, pane_command) in spec.extra_panes.iter().enumerate() {
        if existing >= index + 2 {
            continue;
        }
        let request_id = client.spawn_terminal(SpawnRequest {
            command: command(*pane_command),
            session_id: Some(session.id),
            ..SpawnRequest::default()
        });
        wait_for_spawn(&client, request_id)?;
        spawned += 1;
    }
    client.close();
    if existing > target {
        eprintln!(
            "rig-seed: note — session '{}' has {existing} panes (target {target}); leaving as-is",
            spec.name
        );
    }
    Ok(spawned)
}

#[allow(
    clippy::single_option_map,
    reason = "the helper centralizes conversion of declarative fixture argv"
)]
fn command(argv: Option<&[&str]>) -> Option<Vec<String>> {
    argv.map(|items| items.iter().map(ToString::to_string).collect())
}

fn wait_for_topology(
    client: &Client,
    session_name: &str,
) -> Result<phux_client_runtime::control::Topology, String> {
    wait_until(client, |client, event| {
        if matches!(event, Some(Event::TopologyChanged)) {
            return client
                .topology()
                .filter(|topology| topology.session_named(session_name).is_some());
        }
        None
    })
}

fn wait_for_spawn(client: &Client, expected: u32) -> Result<(), String> {
    wait_until(client, |_, event| match event {
        Some(Event::TerminalSpawned {
            request_id,
            terminal_id: Some(_),
            error: None,
        }) if request_id == expected => Some(()),
        _ => None,
    })
}

fn wait_until<T>(
    client: &Client,
    mut inspect: impl FnMut(&Client, Option<Event>) -> Option<T>,
) -> Result<T, String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        let events = client.take_events();
        if events.is_empty()
            && let Some(value) = inspect(client, None)
        {
            return Ok(value);
        }
        for event in events {
            if let Event::ServerError { message, .. } = &event {
                return Err(format!("server error: {message}"));
            }
            if let Some(value) = inspect(client, Some(event)) {
                return Ok(value);
            }
        }
        match client.status() {
            Status::Failed => {
                return Err(client
                    .last_error()
                    .unwrap_or_else(|| "connection failed".to_owned()));
            }
            Status::Closed => return Err("connection closed".to_owned()),
            _ => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    Err("timed out waiting for the runtime".to_owned())
}
