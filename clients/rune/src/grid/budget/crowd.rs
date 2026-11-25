//! One terminal streams in the real window while the server holds many
//! more, dealt round other sessions: the sidebar lists them all, and every
//! chunk runs the window's poll and a frame. The pane work must not move
//! with the crowd; the poll and frame CPU times it prints are the evidence
//! for what does (thread CPU time, so a loaded host reads the same).

use std::sync::Arc;

use phux_protocol::caps::ServerCapabilities;
use rune::{Env, Runtime};

use super::{SCALE, SIZE, phase, scroll_line};
use crate::desk::{Desk, Shown};
use crate::test_support::terminal::Replica;

/// This thread's CPU time, in microseconds.
#[cfg(unix)]
#[allow(unsafe_code)]
fn cpu_us() -> f64 {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `now` is a valid timespec for the call to fill.
    unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &raw mut now) };
    now.tv_sec as f64 * 1e6 + now.tv_nsec as f64 / 1e3
}

/// Wall time where there is no thread clock to read.
#[cfg(not(unix))]
fn cpu_us() -> f64 {
    static START: std::sync::LazyLock<std::time::Instant> =
        std::sync::LazyLock::new(std::time::Instant::now);
    START.elapsed().as_secs_f64() * 1e6
}

fn stream(name: &str, terminals: u32, sessions: u32) {
    const FRAMES: u64 = 128;
    let (cols, rows) = (120, 40);
    let capabilities = ServerCapabilities::new();
    let mut replica = Replica::spread(cols, rows, terminals, sessions, capabilities);
    let mut desk = crate::test_support::window();
    desk.connection.client = Some(Arc::clone(&replica.client));
    desk.connection.topology = replica.client.topology();
    desk.connection.layout = Some(phux_client_core::layout::LayoutNode::Leaf(
        replica.terminal.clone(),
    ));
    desk.connection.shown = Some(Shown {
        resource: replica.terminal.clone(),
        view: Some(replica.views[0]),
    });
    let mut runtime =
        crate::test_support::runtime(desk, crate::ui::view, SIZE, Env::new(true, SCALE));
    let mut clock = 0.0;
    // The poll's and the frame's CPU microseconds.
    let mut tick = |runtime: &mut Runtime<Desk>| {
        clock += 1.0 / 60.0;
        let started = cpu_us();
        runtime.update(|desk, tx| {
            desk.poll(clock, tx);
        });
        let polled = cpu_us();
        runtime.frame(1.0 / 60.0);
        (polled - started, cpu_us() - polled)
    };
    tick(&mut runtime);
    replica.feed(scroll_line(rows, 9999));
    tick(&mut runtime);
    assert_eq!(
        runtime.state.addressable().len(),
        terminals as usize,
        "{name} lists every terminal"
    );
    let (mut poll, mut frame) = (0.0, 0.0);
    let work = phase(name, FRAMES, |index| {
        replica.feed(scroll_line(rows, index));
        let (p, f) = tick(&mut runtime);
        poll += p;
        frame += f;
    });
    assert_eq!((work.patched, work.rows), (FRAMES, FRAMES), "{name}");
    assert!(
        !runtime.animating,
        "{name}: output keeps the window drawing"
    );
    let line = serde_json::json!({
        "crowd": name,
        "terminals": terminals,
        "poll_cpu_us": (poll / FRAMES as f64).round(),
        "frame_cpu_us": (frame / FRAMES as f64).round(),
    });
    println!("{line}");
}

pub(super) fn phases() {
    stream("crowd_1", 1, 1);
    stream("crowd_256", 256, 16);
    stream("crowd_1024", 1024, 64);
}
