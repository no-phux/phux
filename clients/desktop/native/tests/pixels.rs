//! Offscreen pixel fixtures for the native terminal painter.
//!
//! `harness = false` (see `Cargo.toml`). Cargo's built-in harness runs each
//! `#[test]` on a worker thread. GPUI's macOS platform constructs
//! `MacPlatform` with `MainThreadMarker` and panics with "Mac platform not
//! created on main thread" from any other thread. This binary's `main` is
//! that thread, so the fixtures below run there on every OS.
//!
//! Each fixture publishes VT through the real runtime, paints that
//! publication with [`phux_desktop::terminal::Terminal`], and reads the frame
//! back from GPUI's headless renderer. Regions are checked for a color, not
//! against a stored image. On Linux the renderer is wgpu and needs a Vulkan
//! adapter (a hardware device or lavapipe). On macOS it is Metal.

use std::panic::catch_unwind;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

use gpui::{AppContext, HeadlessAppContext, Pixels, px, size};
use image::RgbaImage;
use phux_client_core::grid::{CELL_SELECTED, COLOR_KIND_RGB};
use phux_client_runtime::control::{ControlOptions, ControlPlane};
use phux_client_runtime::publication::{CursorStyle, GridDamage, GridFrame};
use phux_client_runtime::{Client, Runtime, ViewId};
use phux_desktop::terminal::{PaintedFrame, Settings, Terminal};
use phux_protocol::{
    PROTOCOL_VERSION, ResourceId,
    caps::{
        BootstrapLimits, BootstrapProfile, BootstrapStreamProfile, ServerCapabilities,
        ServerFeature, ServerFeatureSet,
    },
    ids::{BootstrapId, ClientId, SessionId, StreamId, WindowId},
    wire::{
        frame::{AttachTarget, FrameKind},
        info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo},
    },
};

#[cfg(feature = "terminal-fixtures")]
#[path = "support/conversation_pixels.rs"]
mod conversation_pixels;
#[path = "support/glyph_pixels.rs"]
mod glyph_pixels;
#[path = "support/headless_perf.rs"]
mod headless_perf;
#[path = "support/hub.rs"]
mod hub;
#[path = "support/kitty_pixels.rs"]
mod kitty_pixels;
#[path = "support/stream_pixels.rs"]
mod stream_pixels;
#[path = "support/window_perf.rs"]
mod window_perf;

#[cfg(feature = "terminal-fixtures")]
use conversation_pixels::*;
use glyph_pixels::*;
use headless_perf::*;
use stream_pixels::*;

const BACKGROUND: [u8; 3] = [0x10, 0x14, 0x18];
const FOREGROUND: [u8; 3] = [0xf4, 0xf4, 0xf4];
const CURSOR: [u8; 3] = [0xff, 0x00, 0xaa];
const SELECTION: [u8; 3] = [0x22, 0x66, 0xff];
const AMBER: [u8; 3] = [0xff, 0xcc, 0x00];
const TRUE_GREEN: [u8; 3] = [0x00, 0xdc, 0x1e];

struct Shot {
    image: RgbaImage,
    painted: PaintedFrame,
}

fn color(channels: [u8; 3]) -> gpui::Hsla {
    gpui::rgb(u32::from(channels[0]) << 16 | u32::from(channels[1]) << 8 | u32::from(channels[2]))
        .into()
}

fn settings(cursor_visible: bool) -> Settings {
    let mut palette = [color([0x20, 0x20, 0x20]); 16];
    palette[1] = color(AMBER);
    Settings {
        font_size: 16.,
        background: Some(color(BACKGROUND)),
        foreground: Some(color(FOREGROUND)),
        cursor: Some(color(CURSOR)),
        selection_background: color(SELECTION),
        selection_foreground: color(FOREGROUND),
        palette: Some(palette),
        cursor_visible,
        focused: true,
        ..Settings::default()
    }
}

fn terminal() -> ResourceId {
    ResourceId::local(7)
}

/// An attached embedded runtime with a four-row fixture viewport.
fn publish(payload: &[u8]) -> (Arc<Client>, ViewId) {
    publish_at(payload, 20, 4)
}

/// Publish the payload at the requested terminal geometry.
fn publish_at(payload: &[u8], cols: u16, rows: u16) -> (Arc<Client>, ViewId) {
    let client = Runtime::embedded(ControlOptions {
        attach: Some(AttachTarget::ByName("pixels".into())),
        viewport: (cols, rows),
        image_protocols: phux_protocol::caps::ImageProtocolSet::with(&[
            phux_protocol::caps::ImageProtocol::KittyGraphics,
        ]),
        ..ControlOptions::default()
    });
    client.with_control(ControlPlane::connection_opened);
    let _ = client.take_outbound();
    client
        .feed(FrameKind::HelloOk {
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            server_caps: ServerCapabilities::new()
                .with_features(ServerFeatureSet::with(&[ServerFeature::AcknowledgedInput])),
            server_id: vec![1; 16],
            selected_profile: BootstrapProfile::SynthesizedVtRaw,
            bootstrap_limits: BootstrapLimits::default(),
        })
        .expect("hello");
    let attach_id = client
        .take_outbound()
        .iter()
        .find_map(|bytes| match FrameKind::decode(bytes).expect("decode").0 {
            FrameKind::Attach { attach_id, .. } => Some(attach_id),
            _ => None,
        })
        .expect("attach request");
    let stream_id = StreamId::new(1).unwrap();
    let bootstrap_id = BootstrapId::new(1).unwrap();
    let snapshot = SessionSnapshot::new(SessionId::new(1), WindowId::new(1), terminal())
        .with_sessions(vec![SessionInfo::new(SessionId::new(1), "pixels")])
        .with_windows(vec![WindowInfo::new(
            WindowId::new(1),
            SessionId::new(1),
            "shell",
        )])
        .with_resources(vec![ResourceInfo::new(
            terminal(),
            WindowId::new(1),
            cols,
            rows,
        )]);
    for frame in [
        FrameKind::Attached {
            attach_id,
            snapshot,
            initial_client_id: ClientId::new(1),
        },
        FrameKind::BootstrapBegin {
            terminal_id: terminal(),
            stream_id,
            bootstrap_id,
            profile: BootstrapStreamProfile::SynthesizedVtRaw,
            cols,
            rows,
            base_seq: 0,
        },
        FrameKind::BootstrapChunk {
            terminal_id: terminal(),
            stream_id,
            bootstrap_id,
            chunk_seq: 0,
            payload: payload.to_vec().into(),
        },
        FrameKind::BootstrapReady {
            terminal_id: terminal(),
            stream_id,
            bootstrap_id,
            history_cursor: None,
        },
        FrameKind::AttachReady { attach_id },
    ] {
        client.feed(frame).expect("bootstrap");
    }
    let _ = client.take_outbound();
    let _ = client.take_events();
    let view = client.create_view(&terminal()).expect("view");
    let frame = client.acquire_view(view).expect("publication");
    assert_eq!(
        (frame.cols, frame.rows),
        (cols, rows),
        "viewport {}",
        frame.text()
    );
    (Arc::new(client), view)
}

fn gpu() -> HeadlessAppContext {
    // `current_platform` builds `MacPlatform` on macOS. Call it only from
    // `main` (see the crate-level docs); a worker thread panics there.
    let platform = gpui_platform::current_platform(true);
    HeadlessAppContext::with_platform(platform.text_system(), Arc::new(()), || {
        gpui_platform::current_headless_renderer()
    })
}

fn paint(client: Arc<Client>, view: ViewId, settings: Settings) -> Shot {
    // Keep one `Arc` until after the GPUI context drops. The terminal's own
    // clone must not be the last: dropping a `Client` on the UI thread joins
    // the runtime driver.
    let retained = Arc::clone(&client);
    let mut cx = gpu();
    let terminal_id = terminal();
    let window = cx
        .open_window(size(px(480.), px(200.)), move |window, cx| {
            cx.new(|cx| Terminal::new(client, view, terminal_id, settings, window, cx))
        })
        .expect("offscreen window");
    // Draw outside the root-view lease. `Window::draw` paints the terminal,
    // and painting it while `WindowHandle::update` already holds the entity
    // panics.
    cx.update_window(window.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
    })
    .expect("draw");
    let painted = window
        .update(&mut cx, |terminal, _, _| terminal.painted())
        .expect("painted");
    assert!(
        painted.error.is_none(),
        "paint failed: {}",
        painted.error.clone().unwrap_or_default()
    );
    let image = cx
        .capture_screenshot(window.into())
        .expect("headless pixels; on Linux this needs a Vulkan adapter such as lavapipe");
    assert!(image.width() > 0 && image.height() > 0);
    drop(cx);
    drop(retained);
    Shot { image, painted }
}

fn frame_of(client: &Client, view: ViewId) -> Arc<GridFrame> {
    client.acquire_view(view).expect("publication")
}

/// Device-pixel bounds of one cell, inset so a neighbor's antialiased edge
/// is not this cell's ink.
fn cell_pixels(shot: &Shot, row: u16, col: u16) -> Vec<[u8; 4]> {
    let geometry = shot.painted.geometry;
    let scale = geometry.scale;
    assert!(scale > 0.);
    let left = device(
        geometry.bounds.origin.x + geometry.cell_width * f32::from(col),
        scale,
    );
    let top = device(
        geometry.bounds.origin.y + geometry.cell_height * f32::from(row),
        scale,
    );
    let right = device(
        geometry.bounds.origin.x + geometry.cell_width * f32::from(col + 1),
        scale,
    );
    let bottom = device(
        geometry.bounds.origin.y + geometry.cell_height * f32::from(row + 1),
        scale,
    );
    let (left, top, right, bottom) = inset(left, top, right, bottom);
    assert!(right <= shot.image.width() && bottom <= shot.image.height());
    let mut pixels = Vec::new();
    for y in top..bottom {
        for x in left..right {
            pixels.push(shot.image.get_pixel(x, y).0);
        }
    }
    assert!(
        !pixels.is_empty(),
        "cell {row},{col} has no interior pixels"
    );
    pixels
}

fn device(logical: Pixels, scale: f32) -> u32 {
    (f32::from(logical) * scale).round() as u32
}

fn inset(left: u32, top: u32, right: u32, bottom: u32) -> (u32, u32, u32, u32) {
    assert!(
        right > left + 2 && bottom > top + 2,
        "cell is too small to sample"
    );
    (left + 1, top + 1, right - 1, bottom - 1)
}

fn distance(pixel: [u8; 4], color: [u8; 3]) -> u32 {
    pixel
        .iter()
        .zip(color)
        .map(|(channel, expected)| u32::from(channel.abs_diff(expected)))
        .sum()
}

fn near(pixel: [u8; 4], color: [u8; 3]) -> bool {
    pixel
        .iter()
        .zip(color)
        .all(|(channel, expected)| channel.abs_diff(expected) <= 8)
}

fn assert_share(pixels: &[[u8; 4]], color: [u8; 3], minimum: f32, what: &str) {
    let hits = pixels.iter().filter(|pixel| near(**pixel, color)).count();
    let fraction = hits as f32 / pixels.len() as f32;
    assert!(
        fraction > minimum,
        "{what}: {fraction:.2} of {} pixels near {color:?}, center {:?}",
        pixels.len(),
        pixels[pixels.len() / 2]
    );
}

/// A solid cell (cursor, empty gap, palette swatch) is this color almost everywhere.
fn assert_mostly(pixels: &[[u8; 4]], color: [u8; 3], what: &str) {
    assert_share(pixels, color, 0.85, what);
}

fn assert_ink(pixels: &[[u8; 4]], what: &str) {
    let ink = pixels
        .iter()
        .filter(|pixel| distance(**pixel, BACKGROUND) > 80)
        .count();
    assert!(
        ink >= 8,
        "{what}: only {ink} ink pixels, center {:?}",
        pixels[pixels.len() / 2]
    );
}

fn assert_blank(pixels: &[[u8; 4]], what: &str) {
    let ink = pixels
        .iter()
        .filter(|pixel| distance(**pixel, BACKGROUND) > 80)
        .count();
    assert_eq!(
        ink,
        0,
        "{what}: unexpected ink, center {:?}",
        pixels[pixels.len() / 2]
    );
    assert_mostly(pixels, BACKGROUND, what);
}

/// Qualified the way the old `#[cfg(test)]` module reported them, so
/// `cargo test terminal::pixels` and `--skip terminal::pixels` still select
/// this binary's fixtures.
const FIXTURES: &[(&str, fn(), bool)] = &[
    (
        "terminal::pixels::kitty_graphics_paints_rgb_png_and_unicode_placeholders",
        kitty_pixels::kitty_graphics_paints_rgb_png_and_unicode_placeholders,
        false,
    ),
    (
        "terminal::pixels::kitty_graphics_preserves_crop_alpha_and_z_order",
        kitty_pixels::kitty_graphics_preserves_crop_alpha_and_z_order,
        false,
    ),
    (
        "terminal::pixels::kitty_graphics_replaces_and_deletes_cached_pixels",
        kitty_pixels::kitty_graphics_replaces_and_deletes_cached_pixels,
        false,
    ),
    (
        "terminal::pixels::kitty_placeholder_sampling_matches_classic_in_fractional_previews",
        kitty_pixels::kitty_placeholder_sampling_matches_classic_in_fractional_previews,
        false,
    ),
    (
        "app::hub::navigation_and_settings_render",
        hub::navigation_and_settings_render,
        false,
    ),
    (
        "terminal::pixels::cached_ascii_matches_platform_pixels",
        cached_ascii_matches_platform_pixels,
        false,
    ),
    (
        "terminal::pixels::glyphs_land_in_their_columns",
        glyphs_land_in_their_columns,
        false,
    ),
    (
        "terminal::pixels::faint_text_and_box_drawing_keep_their_columns",
        faint_text_and_box_drawing_keep_their_columns,
        false,
    ),
    (
        "terminal::pixels::wide_character_occupies_two_columns",
        wide_character_occupies_two_columns,
        false,
    ),
    (
        "terminal::pixels::block_cursor_fills_its_cell",
        block_cursor_fills_its_cell,
        false,
    ),
    (
        "terminal::pixels::selection_paints_the_selection_background",
        selection_paints_the_selection_background,
        false,
    ),
    (
        "terminal::pixels::palette_recolors_ansi_and_keeps_true_color",
        palette_recolors_ansi_and_keeps_true_color,
        false,
    ),
    (
        "terminal::pixels::truecolor_grays_are_not_quantized",
        truecolor_grays_are_not_quantized,
        false,
    ),
    #[cfg(feature = "terminal-fixtures")]
    (
        "app::conversation::native_face_is_rendered",
        native_conversation_face_is_rendered,
        true,
    ),
    (
        "terminal::pixels::nerd_symbols_are_not_missing_glyph_boxes_in_any_face",
        nerd_symbols_are_not_missing_glyph_boxes_in_any_face,
        false,
    ),
    (
        "terminal::pixels::solid_blocks_cover_shared_row_edges_with_extra_line_height",
        solid_blocks_cover_shared_row_edges_with_extra_line_height,
        false,
    ),
    (
        "terminal::pixels::sparse_output_repaints_a_row_without_disturbing_its_neighbor",
        sparse_output_repaints_a_row_without_disturbing_its_neighbor,
        false,
    ),
    (
        "terminal::pixels::streaming_output_recycles_frame_buffers",
        streaming_output_recycles_frame_buffers,
        false,
    ),
    (
        "terminal::pixels::smooth_scroll_paints_the_row_below_the_viewport",
        smooth_scroll_paints_the_row_below_the_viewport,
        false,
    ),
    (
        "terminal::pixels::stepped_history_scroll_paints_like_one_long_scroll",
        stepped_history_scroll_paints_like_one_long_scroll,
        false,
    ),
];

struct Args {
    list: bool,
    exact: bool,
    /// `--ignored` selects the private-server conversation fixture.
    ignored_only: bool,
    filters: Vec<String>,
    skips: Vec<String>,
}

impl Args {
    fn from_env() -> Self {
        let mut args = Self {
            list: false,
            exact: false,
            ignored_only: false,
            filters: Vec::new(),
            skips: Vec::new(),
        };
        let mut raw = std::env::args().skip(1).peekable();
        while let Some(arg) = raw.next() {
            let (flag, inline) = split_flag(&arg);
            match flag {
                "--list" => args.list = true,
                "--exact" => args.exact = true,
                "--ignored" => args.ignored_only = true,
                "--skip" => {
                    if let Some(name) = inline.or_else(|| take_value(&mut raw)) {
                        args.skips.push(name);
                    }
                }
                "--test-threads" | "--format" | "--logfile" | "--color" | "--shuffle-seed" => {
                    if inline.is_none() {
                        let _ = take_value(&mut raw);
                    }
                }
                other if other.starts_with('-') => {}
                other => args.filters.push(other.to_owned()),
            }
        }
        args
    }

    fn selected(&self, name: &str, ignored: bool) -> bool {
        if self.ignored_only != ignored || self.skips.iter().any(|skip| name.contains(skip)) {
            return false;
        }
        if self.filters.is_empty() {
            return true;
        }
        if self.exact {
            self.filters.iter().any(|filter| filter == name)
        } else {
            self.filters.iter().any(|filter| name.contains(filter))
        }
    }
}

fn split_flag(arg: &str) -> (&str, Option<String>) {
    match arg.split_once('=') {
        Some((flag, value)) => (flag, Some(value.to_owned())),
        None => (arg, None),
    }
}

/// The next argv token when it is a flag value rather than another flag.
fn take_value<I>(args: &mut std::iter::Peekable<I>) -> Option<String>
where
    I: Iterator<Item = String>,
{
    if args.peek().is_some_and(|next| !next.starts_with('-')) {
        args.next()
    } else {
        None
    }
}

fn main() -> ExitCode {
    if std::env::args().any(|arg| arg == "--headless-perf") {
        headless_perf();
        return ExitCode::SUCCESS;
    }
    let args = Args::from_env();
    let selected: Vec<_> = FIXTURES
        .iter()
        .copied()
        .filter(|(name, _, ignored)| args.selected(name, *ignored))
        .collect();

    if args.list {
        for (name, _, _) in &selected {
            println!("{name}: test");
        }
        return ExitCode::SUCCESS;
    }

    println!("running {} tests", selected.len());
    let mut failed = 0usize;
    for (name, fixture, _) in selected {
        // Direct call. Spawning here would put `current_platform` off the
        // main thread again, which is the macOS panic this binary exists
        // to avoid.
        match catch_unwind(fixture) {
            Ok(()) => println!("test {name} ... ok"),
            Err(_) => {
                failed += 1;
                eprintln!("test {name} ... FAILED");
            }
        }
    }
    if failed == 0 {
        println!("test result: ok");
        ExitCode::SUCCESS
    } else {
        eprintln!("test result: FAILED. {failed} failed");
        ExitCode::from(1)
    }
}
