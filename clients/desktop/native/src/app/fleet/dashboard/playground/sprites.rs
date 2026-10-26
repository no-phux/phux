//! The ghost strips released through phux-brand (`playground/ghost`, pinned
//! by brand.lock.json), decoded once and cut into frames. The atlas names
//! each loop's length; the product owns when a loop plays.

use std::sync::{Arc, OnceLock};

use gpui::RenderImage;
use serde::Deserialize;

use super::plan::Strip;

const ATLAS: &str = include_str!("../../../../../assets/playground/ghost/atlas.json");
const STRIPS: [&[u8]; 4] = [
    include_bytes!("../../../../../assets/playground/ghost/idle.png"),
    include_bytes!("../../../../../assets/playground/ghost/celebrate.png"),
    include_bytes!("../../../../../assets/playground/ghost/dash.png"),
    include_bytes!("../../../../../assets/playground/ghost/sleep.png"),
];

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Atlas {
    frame_count: usize,
    duration_ms: std::collections::HashMap<String, f32>,
}

pub(crate) struct Sheet {
    strips: [Arc<RenderImage>; 4],
    seconds: [f32; 4],
    frames: usize,
}

impl Sheet {
    /// The frame `strip` shows `time` seconds into its loop.
    pub fn frame(&self, strip: Strip, time: f32) -> (Arc<RenderImage>, usize) {
        let loop_s = self.seconds[strip.index()].max(0.1);
        let phase = (time.rem_euclid(loop_s) / loop_s).clamp(0., 0.999);
        (
            Arc::clone(&self.strips[strip.index()]),
            (phase * self.frames as f32) as usize,
        )
    }
}

/// The sheet, decoded on first use; None when the art does not decode.
pub(crate) fn sheet() -> Option<&'static Sheet> {
    static SHEET: OnceLock<Option<Sheet>> = OnceLock::new();
    SHEET.get_or_init(load).as_ref()
}

fn load() -> Option<Sheet> {
    let atlas: Atlas = serde_json::from_str(ATLAS).ok()?;
    let frames = atlas.frame_count.max(1);
    let mut strips = Vec::with_capacity(4);
    let mut seconds = [1f32; 4];
    for strip in Strip::ALL {
        let rgba = image::load_from_memory(STRIPS[strip.index()])
            .ok()?
            .to_rgba8();
        let width = rgba.width() / frames as u32;
        let height = rgba.height();
        let cut = (0..frames)
            .map(|index| {
                let mut buffer =
                    image::imageops::crop_imm(&rgba, index as u32 * width, 0, width, height)
                        .to_image();
                // GPUI's atlas takes straight-alpha BGRA.
                for pixel in buffer.as_chunks_mut::<4>().0 {
                    pixel.swap(0, 2);
                }
                image::Frame::new(buffer)
            })
            .collect::<Vec<_>>();
        strips.push(Arc::new(RenderImage::new(cut)));
        seconds[strip.index()] = atlas
            .duration_ms
            .get(strip.name())
            .copied()
            .unwrap_or(1000.)
            / 1000.;
    }
    let strips: [Arc<RenderImage>; 4] = strips.try_into().ok()?;
    Some(Sheet {
        strips,
        seconds,
        frames,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_released_art_decodes_into_eight_frame_loops() {
        let sheet = sheet().expect("the brand exports decode");
        assert_eq!(sheet.frames, 8);
        let (_, first) = sheet.frame(Strip::Dash, 0.);
        let (_, last) = sheet.frame(Strip::Dash, 0.53);
        assert_eq!(first, 0);
        assert_eq!(last, 7);
        assert!(sheet.seconds[Strip::Sleep.index()] > sheet.seconds[Strip::Dash.index()]);
    }
}
