//! Reproduce the component allocation measurements motivating ACK scratch reuse.
//!
//! Run `cargo run --locked -p phux-server --example ack_allocations --features ack-allocations-example`.
//! Render allocations are routed through Rust's global allocator so DHAT observes
//! the engine allocations too. This measures allocation traffic, not live heap,
//! network latency, whole-server CPU, or all allocations during an ACK.

use std::{collections::BTreeMap, hint::black_box};

use libghostty_vt::{Terminal, alloc::Allocator, render::RenderState};

#[global_allocator]
static ALLOCATOR: dhat::Alloc = dhat::Alloc;

#[derive(Debug, Default)]
struct AllocationTraffic {
    blocks: u64,
    bytes: u64,
}

impl AllocationTraffic {
    fn observe(&mut self, before: &dhat::HeapStats) {
        let after = dhat::HeapStats::get();
        self.blocks += after.total_blocks - before.total_blocks;
        self.bytes += after.total_bytes - before.total_bytes;
    }
}

fn sample_cursor(
    render: &mut RenderState<'static>,
    terminal: &Terminal<'static, '_>,
) -> Result<(), libghostty_vt::Error> {
    let snapshot = render.update(terminal)?;
    black_box(snapshot.cursor_viewport()?);
    black_box(snapshot.cursor_visible()?);
    black_box(snapshot.cursor_visual_style()?);
    black_box(snapshot.cursor_blinking()?);
    Ok(())
}

fn render_traffic(
    cols: u16,
    rows: u16,
    reuse: bool,
) -> Result<AllocationTraffic, libghostty_vt::Error> {
    let mut terminal = Terminal::new(cols, rows)?;
    terminal.vt_write(b"hello world");
    let mut render = None;
    let mut traffic = AllocationTraffic::default();
    for _ in 0..1_000 {
        terminal.vt_write(b"x");
        let before = dhat::HeapStats::get();
        if !reuse || render.is_none() {
            render = Some(RenderState::new_with_alloc(&Allocator::GLOBAL)?);
        }
        if let Some(render) = render.as_mut() {
            sample_cursor(render, &terminal)?;
        }
        traffic.observe(&before);
    }
    Ok(traffic)
}

fn pruning_traffic(window: u64, split: bool) -> AllocationTraffic {
    let mut map: BTreeMap<u64, u64> = (1..=window).map(|key| (key, key)).collect();
    let mut traffic = AllocationTraffic::default();
    for ack in 1..=10_000 {
        let before = dhat::HeapStats::get();
        if split {
            map = map.split_off(&(ack + 1));
        } else {
            while map.first_key_value().is_some_and(|(&key, _)| key <= ack) {
                map.pop_first();
            }
        }
        traffic.observe(&before);
        assert_eq!(map.len() as u64, window - 1);
        map.insert(ack + window, ack + window);
    }
    traffic
}

#[allow(
    clippy::print_stdout,
    reason = "standalone measurement emits its results"
)]
fn main() -> Result<(), libghostty_vt::Error> {
    let _profiler = dhat::Profiler::builder().testing().build();
    for (cols, rows) in [(80, 24), (200, 60)] {
        println!(
            "{cols}x{rows} / 1000 engine cursor captures: fresh={:?}, reused={:?}",
            render_traffic(cols, rows, false)?,
            render_traffic(cols, rows, true)?,
        );
    }
    for window in [1, 8, 32, 256] {
        println!(
            "window={window} / 10000 ACK map prunes: split={:?}, in_place={:?}",
            pruning_traffic(window, true),
            pruning_traffic(window, false),
        );
    }
    Ok(())
}
