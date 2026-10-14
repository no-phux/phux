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

fn glyphs_land_in_their_columns() {
    let (client, view) = publish(b"\x1b[?25l\x1b[HAB C");
    let frame = frame_of(&client, view);
    assert_eq!(frame.row_text(0), "AB C");
    let shot = paint(client, view, settings(false));
    assert_ink(&cell_pixels(&shot, 0, 0), "column 0 (A)");
    assert_ink(&cell_pixels(&shot, 0, 1), "column 1 (B)");
    assert_blank(&cell_pixels(&shot, 0, 2), "column 2 (the gap)");
    assert_ink(&cell_pixels(&shot, 0, 3), "column 3 (C)");
}

fn wide_character_occupies_two_columns() {
    let (client, view) = publish("\u{1b}[?25l\u{1b}[HX中Y".as_bytes());
    let frame = frame_of(&client, view);
    assert_eq!(frame.row_text(0), "X中Y");
    assert_eq!(frame.cell(0, 1).expect("wide head").wide, 1);
    let spacer = frame.cell(0, 2).expect("spacer").wide;
    assert!(matches!(spacer, 2 | 3), "spacer wide={spacer}");
    assert_eq!(frame.cell_text(0, 3), b"Y");
    let shot = paint(client, view, settings(false));
    assert_ink(&cell_pixels(&shot, 0, 0), "column 0 (X)");
    assert_ink(&cell_pixels(&shot, 0, 1), "wide head");
    assert_ink(
        &cell_pixels(&shot, 0, 3),
        "column 3 (Y follows the wide cell)",
    );
    assert_blank(&cell_pixels(&shot, 0, 4), "column 4 stays empty");
}

fn block_cursor_fills_its_cell() {
    // Steady block, then CUP to row 2 column 6 (1-based).
    let (client, view) = publish(b"\x1b[2 q\x1b[2;6H");
    let frame = frame_of(&client, view);
    assert!(frame.cursor.visible);
    assert_eq!(frame.cursor.style, CursorStyle::Block);
    assert_eq!((frame.cursor.row, frame.cursor.col), (1, 5));
    let shot = paint(client, view, settings(true));
    assert_mostly(&cell_pixels(&shot, 1, 5), CURSOR, "cursor cell");
    assert_mostly(
        &cell_pixels(&shot, 1, 6),
        BACKGROUND,
        "cell after the cursor",
    );
    assert_mostly(
        &cell_pixels(&shot, 0, 0),
        BACKGROUND,
        "origin is not the cursor",
    );
}

fn selection_paints_the_selection_background() {
    let (client, view) = publish(b"\x1b[?25l\x1b[HSELECT");
    let found = client
        .engine()
        .expect("engine")
        .search_view(view, "SELECT".into(), true)
        .expect("search");
    let matched = found.first().expect("SELECT is on screen");
    client
        .engine()
        .expect("engine")
        .set_view_selection(view, matched.start, matched.end, false)
        .expect("select");
    let frame = frame_of(&client, view);
    assert_ne!(frame.cell(0, 0).expect("S").flags & CELL_SELECTED, 0);
    assert_eq!(frame.cell(0, 8).expect("gap").flags & CELL_SELECTED, 0);
    let shot = paint(client, view, settings(false));
    let selected = cell_pixels(&shot, 0, 0);
    // The glyph covers the rest of the cell, so this is lower than a solid swatch.
    assert_share(&selected, SELECTION, 0.6, "selected cell background");
    assert!(
        selected
            .iter()
            .any(|pixel| distance(*pixel, SELECTION) > 80),
        "the selected glyph must still be ink on top of the selection"
    );
    assert_share(
        &cell_pixels(&shot, 0, 5),
        SELECTION,
        0.6,
        "last selected column",
    );
    assert_blank(&cell_pixels(&shot, 0, 8), "unselected gap");
}

fn palette_recolors_ansi_and_keeps_true_color() {
    // ANSI red background (palette index 1), then a true-color glyph.
    let (client, view) = publish(b"\x1b[?25l\x1b[H\x1b[41m \x1b[0m\x1b[38;2;0;220;30mT");
    let frame = frame_of(&client, view);
    let ansi = frame.cell(0, 0).expect("ansi cell");
    let ansi_meta = frame.buffer.metadata[0];
    assert!(!ansi_meta.background_color_is_default);
    let published = [ansi.background_r, ansi.background_g, ansi.background_b];
    assert_eq!(
        published,
        [
            frame.colors.palette[1].r,
            frame.colors.palette[1].g,
            frame.colors.palette[1].b
        ]
    );
    assert_ne!(
        published, AMBER,
        "the theme color must differ from the terminal palette or the remap is invisible"
    );
    let colored = frame.cell(0, 1).expect("true-color cell");
    assert_eq!(frame.buffer.metadata[1].foreground_kind, COLOR_KIND_RGB);
    assert_eq!(
        [
            colored.foreground_r,
            colored.foreground_g,
            colored.foreground_b
        ],
        TRUE_GREEN
    );
    let shot = paint(client, view, settings(false));
    assert_mostly(
        &cell_pixels(&shot, 0, 0),
        AMBER,
        "ANSI red takes the theme palette",
    );
    let glyph = cell_pixels(&shot, 0, 1);
    let green = glyph
        .iter()
        .filter(|pixel| near(**pixel, TRUE_GREEN))
        .count();
    assert!(
        green >= 4,
        "true color stayed green ({green} pixels), center {:?}",
        glyph[glyph.len() / 2]
    );
    // The glyph covers the rest. A true-color background fill would miss this.
    assert_share(
        &glyph,
        BACKGROUND,
        0.7,
        "true color does not repaint the background",
    );
}

fn nerd_symbols_are_not_missing_glyph_boxes_in_any_face() {
    let mut payload = String::from("\u{1b}[?25l");
    for (row, face) in ["0", "1", "3", "1;3"].into_iter().enumerate() {
        // U+0378 is unassigned; compare the actual rendered missing-glyph
        // box, not merely whether an icon cell contains some ink.
        payload.push_str(&format!(
            "\u{1b}[{};1H\u{1b}[0;{face}m\u{f013}\u{f0206}\u{0378}",
            row + 1
        ));
    }
    let (client, view) = publish(payload.as_bytes());
    let shot = paint(Arc::clone(&client), view, settings(false));
    let mut explicit = settings(false);
    explicit.font = gpui::font(phux_desktop::fonts::SYMBOLS);
    let expected = paint(client, view, explicit);
    for row in 0..4 {
        let missing = cell_pixels(&shot, row, 2);
        for col in 0..2 {
            let icon = cell_pixels(&shot, row, col);
            assert_ink(&icon, "Nerd Font symbol");
            assert_ne!(
                icon, missing,
                "Nerd icon column {col}, SGR face row {row} resolved to the missing-glyph box"
            );
            assert_eq!(
                ink_mask(&shot, row, col),
                ink_mask(&expected, row, col),
                "Nerd icon column {col}, SGR face row {row} must render the bundled symbol outline"
            );
        }
    }
}

/// Compare the actual symbol outline, independent of primary-font cell width
/// and baseline. A LastResort box can encode the missing codepoint, so merely
/// differing from another missing glyph is not evidence of a rendered icon.
fn ink_mask(shot: &Shot, row: u16, col: u16) -> (u32, u32, Vec<bool>) {
    let geometry = shot.painted.geometry;
    let bounds = geometry.cell_bounds(row, col, 1);
    let (mut left, mut top) = (u32::MAX, u32::MAX);
    let (mut right, mut bottom) = (0, 0);
    for y in device(bounds.top(), geometry.scale)..device(bounds.bottom(), geometry.scale) {
        for x in device(bounds.left(), geometry.scale)..device(bounds.right(), geometry.scale) {
            if distance(shot.image.get_pixel(x, y).0, BACKGROUND) > 80 {
                left = left.min(x);
                top = top.min(y);
                right = right.max(x + 1);
                bottom = bottom.max(y + 1);
            }
        }
    }
    assert!(
        right > left && bottom > top,
        "symbol has no visible outline"
    );
    let mask = (top..bottom)
        .flat_map(|y| {
            (left..right).map(move |x| distance(shot.image.get_pixel(x, y).0, BACKGROUND) > 80)
        })
        .collect();
    (right - left, bottom - top, mask)
}

fn solid_blocks_cover_shared_row_edges_with_extra_line_height() {
    let (client, view) = publish(
        concat!(
            "\u{1b}[?25l\u{1b}[H██\u{1b}[2;1H██\u{1b}[3;1H▀▄",
            "\u{1b}[4;1H\u{1b}[2m██",
            "\u{1b}[1;5H\u{1b}[0;4;58;2;0;220;30m█",
        )
        .as_bytes(),
    );
    let mut config = settings(false);
    config.line_height = 1.8;
    let shot = paint(client, view, config);
    let geometry = shot.painted.geometry;
    let first = geometry.cell_bounds(0, 0, 2);
    let second = geometry.cell_bounds(1, 0, 2);
    let left = device(first.left(), geometry.scale);
    let right = device(first.right(), geometry.scale);
    let top = device(first.top(), geometry.scale);
    let bottom = device(second.bottom(), geometry.scale);
    for y in top..bottom {
        for x in left..right {
            assert!(
                near(shot.image.get_pixel(x, y).0, FOREGROUND),
                "solid block seam at device pixel ({x}, {y})"
            );
        }
    }
    for (col, upper) in [(0, true), (1, false)] {
        let bounds = geometry.cell_bounds(2, col, 1);
        let x = device(bounds.center().x, geometry.scale);
        for (fraction, filled) in [(0.25, upper), (0.75, !upper)] {
            let y = device(bounds.top() + bounds.size.height * fraction, geometry.scale);
            let expected = if filled { FOREGROUND } else { BACKGROUND };
            assert!(near(shot.image.get_pixel(x, y).0, expected));
        }
    }
    let faint = geometry.cell_bounds(3, 0, 2);
    let center = shot
        .image
        .get_pixel(
            device(faint.center().x, geometry.scale),
            device(faint.center().y, geometry.scale),
        )
        .0;
    assert!(
        !near(center, FOREGROUND) && !near(center, BACKGROUND),
        "faint block was not dimmed"
    );
    let expected = [center[0], center[1], center[2]];
    for y in device(faint.top(), geometry.scale)..device(faint.bottom(), geometry.scale) {
        for x in device(faint.left(), geometry.scale)..device(faint.right(), geometry.scale) {
            assert!(
                near(shot.image.get_pixel(x, y).0, expected),
                "translucent block seam"
            );
        }
    }
    let decorated = geometry.cell_bounds(0, 4, 1);
    let underline_y = device(decorated.bottom(), geometry.scale) - 2;
    assert!(
        near(
            shot.image
                .get_pixel(device(decorated.center().x, geometry.scale), underline_y,)
                .0,
            TRUE_GREEN,
        ),
        "colored underline must remain above solid block ink"
    );
}

fn sparse_output_repaints_a_row_without_disturbing_its_neighbor() {
    let (client, view) = publish(b"hello\r\nworld");
    let mut cx = gpu();
    let in_view = Arc::clone(&client);
    let window = cx
        .open_window(size(px(480.), px(200.)), move |window, cx| {
            cx.new(|cx| Terminal::new(in_view, view, terminal(), settings(false), window, cx))
        })
        .expect("offscreen window");
    cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear(cx))
        .expect("first draw");
    let before = Shot {
        painted: window
            .update(&mut cx, |terminal, _, _| terminal.painted())
            .unwrap(),
        image: cx.capture_screenshot(window.into()).expect("first pixels"),
    };
    let patched = phux_desktop::perf::PREPARE_PATCHED.get();
    client
        .feed(FrameKind::ResourceOutput {
            terminal_id: terminal(),
            stream_id: StreamId::new(1).unwrap(),
            bootstrap_id: BootstrapId::new(1).unwrap(),
            seq: 1,
            bytes: b"\x1b[2;1HWORLD".to_vec().into(),
        })
        .expect("live output");
    window.update(&mut cx, |_, _, cx| cx.notify()).unwrap();
    cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear(cx))
        .expect("updated draw");
    let after = Shot {
        painted: window
            .update(&mut cx, |terminal, _, _| terminal.painted())
            .unwrap(),
        image: cx
            .capture_screenshot(window.into())
            .expect("updated pixels"),
    };
    assert_eq!(phux_desktop::perf::PREPARE_PATCHED.get(), patched + 1);
    let frame = after.painted.frame.as_ref().unwrap();
    assert_eq!(frame.damage, GridDamage::Rows);
    assert_eq!(frame.dirty_rows().collect::<Vec<_>>(), vec![1]);
    assert_eq!(frame.row_text(1), "WORLD");
    assert_eq!(cell_pixels(&before, 0, 0), cell_pixels(&after, 0, 0));
    assert_ne!(cell_pixels(&before, 1, 0), cell_pixels(&after, 1, 0));
    drop(cx);
    drop(client);
}

/// An opt-in, in-process render benchmark. It reuses the pixel fixture's
/// embedded runtime and main-thread GPUI renderer; screenshots are outside
/// the timed path but prove that draws reach a real offscreen surface.
fn headless_perf() {
    const FRAMES: usize = 128;
    const FILL: &str = "terminal workload terminal workload terminal workload terminal workload ";
    let payload = (0..24)
        .map(|row| format!("{row:02} {FILL}"))
        .collect::<Vec<_>>()
        .join("\r\n");
    let (client, view) = publish_at(payload.as_bytes(), 80, 24);
    let mut cx = gpu();
    let in_view = Arc::clone(&client);
    let window = cx
        .open_window(size(px(960.), px(600.)), move |window, cx| {
            cx.new(|cx| Terminal::new(in_view, view, terminal(), settings(false), window, cx))
        })
        .expect("offscreen benchmark window");
    cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear(cx))
        .expect("first draw");
    let before = cx
        .capture_screenshot(window.into())
        .expect("initial pixels");

    let mut draw = || {
        cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear(cx))
            .expect("draw");
    };
    bench_phase("cached", FRAMES, |_| draw());
    let mut seq = 0_u64;
    bench_phase("sparse", FRAMES, |index| {
        seq += 1;
        let letter = if index % 2 == 0 { b'X' } else { b'Y' };
        client
            .feed(FrameKind::ResourceOutput {
                terminal_id: terminal(),
                stream_id: StreamId::new(1).unwrap(),
                bootstrap_id: BootstrapId::new(1).unwrap(),
                seq,
                bytes: [b"\x1b[13;1H".as_slice(), &[letter]].concat().into(),
            })
            .expect("sparse output");
        draw();
    });
    bench_phase("scroll", FRAMES, |index| {
        seq += 1;
        client
            .feed(FrameKind::ResourceOutput {
                terminal_id: terminal(),
                stream_id: StreamId::new(1).unwrap(),
                bootstrap_id: BootstrapId::new(1).unwrap(),
                seq,
                bytes: format!("\x1b[24;1H{index:04} {FILL}\r\n")
                    .into_bytes()
                    .into(),
            })
            .expect("scroll output");
        draw();
    });
    drop(draw);
    let after = cx.capture_screenshot(window.into()).expect("final pixels");
    assert_ne!(
        before.as_raw(),
        after.as_raw(),
        "output must change visible pixels"
    );
    drop(cx);
    drop(client);
}

fn bench_phase(name: &str, frames: usize, mut tick: impl FnMut(usize)) {
    let before = phux_perf::snapshot(
        "desktop",
        phux_desktop::perf::TABLE,
        phux_desktop::perf::uptime(),
    );
    let mut elapsed = Vec::with_capacity(frames);
    for index in 0..frames {
        let started = Instant::now();
        tick(index);
        elapsed.push(started.elapsed().as_secs_f64() * 1_000_000.);
    }
    elapsed.sort_by(f64::total_cmp);
    let delta = phux_perf::snapshot(
        "desktop",
        phux_desktop::perf::TABLE,
        phux_desktop::perf::uptime(),
    )
    .delta(&before);
    let renders = delta.metric("desktop.render").expect("render counter");
    assert_eq!(
        renders.value,
        phux_perf::MetricValue::Counter(frames as u64),
        "{name}: every measured tick must paint"
    );
    let histogram = |name| match &delta.metric(name).expect("stage metric").value {
        phux_perf::MetricValue::Histogram(value) => serde_json::json!({
            "count": value.count,
            "mean_us": value.mean(),
            "p50_us": value.percentile(50),
            "p99_us": value.percentile(99),
        }),
        _ => panic!("{name} is not a histogram"),
    };
    let counter = |name| match &delta.metric(name).expect("counter metric").value {
        phux_perf::MetricValue::Counter(value) => *value,
        _ => panic!("{name} is not a counter"),
    };
    println!(
        "{}",
        serde_json::json!({
            "phase": name,
            "frames": frames,
            "tick_us": {
                "mean": elapsed.iter().sum::<f64>() / frames as f64,
                "p50": elapsed[frames / 2],
                "p99": elapsed[frames * 99 / 100],
            },
            "stages": {
                "acquire": histogram("desktop.acquire"),
                "prepare": histogram("desktop.prepare"),
                "paint": histogram("desktop.paint"),
                "present": histogram("desktop.present"),
            },
            "scene": {
                "reused": counter("desktop.prepare_reused"),
                "patched": counter("desktop.prepare_patched"),
                "shaped_lines": counter("desktop.shaped"),
            },
            "process": delta.process,
        })
    );
}

/// Qualified the way the old `#[cfg(test)]` module reported them, so
/// `cargo test terminal::pixels` and `--skip terminal::pixels` still select
/// this binary's fixtures.
const FIXTURES: &[(&str, fn())] = &[
    (
        "terminal::pixels::glyphs_land_in_their_columns",
        glyphs_land_in_their_columns,
    ),
    (
        "terminal::pixels::wide_character_occupies_two_columns",
        wide_character_occupies_two_columns,
    ),
    (
        "terminal::pixels::block_cursor_fills_its_cell",
        block_cursor_fills_its_cell,
    ),
    (
        "terminal::pixels::selection_paints_the_selection_background",
        selection_paints_the_selection_background,
    ),
    (
        "terminal::pixels::palette_recolors_ansi_and_keeps_true_color",
        palette_recolors_ansi_and_keeps_true_color,
    ),
    (
        "terminal::pixels::nerd_symbols_are_not_missing_glyph_boxes_in_any_face",
        nerd_symbols_are_not_missing_glyph_boxes_in_any_face,
    ),
    (
        "terminal::pixels::solid_blocks_cover_shared_row_edges_with_extra_line_height",
        solid_blocks_cover_shared_row_edges_with_extra_line_height,
    ),
    (
        "terminal::pixels::sparse_output_repaints_a_row_without_disturbing_its_neighbor",
        sparse_output_repaints_a_row_without_disturbing_its_neighbor,
    ),
];

struct Args {
    list: bool,
    exact: bool,
    /// `--ignored` selects only ignored tests. This binary has none.
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

    fn selected(&self, name: &str) -> bool {
        if self.ignored_only || self.skips.iter().any(|skip| name.contains(skip)) {
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
        .filter(|(name, _)| args.selected(name))
        .collect();

    if args.list {
        for (name, _) in &selected {
            println!("{name}: test");
        }
        return ExitCode::SUCCESS;
    }

    println!("running {} tests", selected.len());
    let mut failed = 0usize;
    for (name, fixture) in selected {
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
