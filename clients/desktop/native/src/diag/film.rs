//! Film: `PHUX_DESKTOP_FILM=DIR` writes the first window to `DIR` as PNGs,
//! [`INTERVAL`] apart, for demo videos. Each file is named by the
//! milliseconds since launch, so a frame dropped while the encoders catch up
//! shortens nothing: the names keep the real timing.
//!
//! Frames are offscreen renders, so filming needs no screen-recording
//! permission. The platform stops asking a covered window for frames, so
//! each capture draws the window first; filming works behind other windows
//! and with the screen locked.
//! `scripts/film.ts` turns a directory of frames into a video.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpui::{App, Window};
use image::RgbaImage;
use image::codecs::png::{CompressionType, FilterType, PngEncoder};

/// Ten frames a second.
pub const INTERVAL: Duration = Duration::from_millis(100);

/// PNG encoding a Retina frame takes longer than [`INTERVAL`] on one core.
const ENCODERS: usize = 4;

type Frame = (u128, RgbaImage);

/// Start filming when `PHUX_DESKTOP_FILM` names a directory.
pub fn install(cx: &mut App) {
    let Some(dir) = std::env::var_os("PHUX_DESKTOP_FILM").map(PathBuf::from) else {
        return;
    };
    if let Err(error) = std::fs::create_dir_all(&dir) {
        log::error!("film directory {}: {error}", dir.display());
        return;
    }
    log::info!("filming to {}", dir.display());
    // A frame of slack per encoder; slower encoding drops frames, never piles up.
    let (frames, queue) = sync_channel::<Frame>(ENCODERS);
    let queue = Arc::new(Mutex::new(queue));
    for _ in 0..ENCODERS {
        let (dir, queue) = (dir.clone(), Arc::clone(&queue));
        std::thread::spawn(move || encode(&dir, &queue));
    }
    spawn_camera(frames, cx);
}

fn spawn_camera(frames: SyncSender<Frame>, cx: &mut App) {
    let started = Instant::now();
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(INTERVAL).await;
            let at = started.elapsed().as_millis();
            cx.update(|cx| {
                let Some(window) = cx.windows().first().copied() else {
                    return;
                };
                let _ = window.update(cx, |_, window, cx| match capture(window, cx) {
                    Ok(image) => drop(frames.try_send((at, image))),
                    Err(error) => log::debug!("film: frame skipped: {error:#}"),
                });
            });
        }
    })
    .detach();
}

fn capture(window: &mut Window, cx: &mut App) -> anyhow::Result<RgbaImage> {
    window.refresh();
    window.draw(cx).clear(cx);
    window.render_to_image()
}

fn encode(dir: &Path, queue: &Mutex<Receiver<Frame>>) {
    loop {
        let next = queue.lock().map(|queue| queue.recv());
        let Ok(Ok((at, image))) = next else { return };
        if let Err(error) = write_frame(&dir.join(format!("{at:09}.png")), &image) {
            log::warn!("film: {error:#}");
        }
    }
}

/// Encode beside `path` and rename into place, so a reader never sees half a frame.
fn write_frame(path: &Path, image: &RgbaImage) -> anyhow::Result<()> {
    let partial = path.with_extension("part");
    let file = std::io::BufWriter::new(std::fs::File::create(&partial)?);
    let encoder = PngEncoder::new_with_quality(file, CompressionType::Fast, FilterType::Sub);
    image.write_with_encoder(encoder)?;
    std::fs::rename(&partial, path)?;
    Ok(())
}
