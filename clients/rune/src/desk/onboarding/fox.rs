//! The onboarding fox and badge: the brand's sprite strips and app icon
//! (`brand.config.json` pins them), cut into frames and stepped on the app
//! clock. Welcome idles; All set celebrates once, then idles. Reduced motion
//! holds the still pose.

use std::cell::Cell;
use std::time::Duration;

use rune::Tx;

use super::super::Desk;
use crate::onboarding::Step;

const IDLE: &[u8] = include_bytes!("../../../assets/brand/fox/idle.png");
const CELEBRATE: &[u8] = include_bytes!("../../../assets/brand/fox/celebrate.png");
const STILL: &[u8] = include_bytes!("../../../assets/brand/fox/still.png");
const BADGE: &[u8] = include_bytes!("../../../packaging/AppIcon.png");

/// Frames per strip and each strip's length, from the brand's `atlas.json`.
const FRAMES: usize = 8;
const IDLE_S: f64 = 1.4;
const CELEBRATE_S: f64 = 0.72;
/// Half the quickest frame, so no frame is skipped.
const WAKE: Duration = Duration::from_millis(45);

#[derive(Default)]
pub struct Fox {
    art: Option<Art>,
    /// When the step showing the fox opened, on the app clock.
    pub since: f64,
    /// Set by the view: reduced motion holds the still pose.
    pub still: Cell<bool>,
    shown: Option<Pose>,
}

struct Art {
    idle: Vec<rune::Image>,
    celebrate: Vec<rune::Image>,
    still: rune::Image,
    badge: rune::Image,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Pose {
    Still,
    Idle(usize),
    Celebrate(usize),
}

/// The frame `elapsed` seconds into a step: a celebration plays once, then
/// the fox idles on a loop.
pub(super) fn pose(celebrating: bool, elapsed: f64) -> Pose {
    let elapsed = elapsed.max(0.0);
    let frame = |t: f64, len: f64| ((t / len * FRAMES as f64) as usize).min(FRAMES - 1);
    if celebrating && elapsed < CELEBRATE_S {
        return Pose::Celebrate(frame(elapsed, CELEBRATE_S));
    }
    let t = if celebrating {
        elapsed - CELEBRATE_S
    } else {
        elapsed
    };
    Pose::Idle(frame(t % IDLE_S, IDLE_S))
}

/// A horizontal strip's frames as RGBA8, each `side` pixels square.
fn frames(png_bytes: &[u8]) -> Option<(u32, Vec<Vec<u8>>)> {
    let mut decoder = png::Decoder::new(png_bytes);
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().ok()?;
    let mut rgba = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut rgba).ok()?;
    if info.color_type != png::ColorType::Rgba || !(info.width as usize).is_multiple_of(FRAMES) {
        return None;
    }
    let (side, height) = (info.width as usize / FRAMES, info.height as usize);
    let row = side * 4;
    let cut = (0..FRAMES)
        .map(|f| {
            (0..height)
                .flat_map(|y| {
                    let start = y * info.line_size + f * row;
                    rgba[start..start + row].iter().copied()
                })
                .collect()
        })
        .collect();
    Some((side as u32, cut))
}

fn strip(cache: &rune::ImageCache, png_bytes: &[u8]) -> Option<Vec<rune::Image>> {
    let (side, cut) = frames(png_bytes)?;
    Some(cut.into_iter().map(|f| cache.rgba(side, side, f)).collect())
}

impl Desk {
    fn fox_pose(&self) -> Pose {
        let o = &self.onboarding;
        if o.fox.still.get() {
            return Pose::Still;
        }
        pose(o.step == Step::Done, self.clock - o.fox.since)
    }

    /// The fox to draw now, once its art is decoded.
    pub fn fox_image(&self) -> Option<&rune::Image> {
        let art = self.onboarding.fox.art.as_ref()?;
        Some(match self.fox_pose() {
            Pose::Still => &art.still,
            Pose::Idle(i) => &art.idle[i],
            Pose::Celebrate(i) => &art.celebrate[i],
        })
    }

    /// The app badge, as the server's app looks in a privacy list.
    pub fn badge_image(&self) -> Option<&rune::Image> {
        self.onboarding.fox.art.as_ref().map(|art| &art.badge)
    }

    /// Decode the art the first time onboarding shows, and step the fox while
    /// a step that shows it is up.
    pub(super) fn tick_fox(&mut self, tx: &mut Tx) -> bool {
        if !self.onboarding.open {
            return false;
        }
        let mut changed = false;
        if self.onboarding.fox.art.is_none() {
            let Some(cache) = self.resources.images.as_ref() else {
                return false;
            };
            self.onboarding.fox.art = Some(Art {
                idle: strip(cache, IDLE).unwrap_or_default(),
                celebrate: strip(cache, CELEBRATE).unwrap_or_default(),
                still: cache.load(STILL),
                badge: cache.load(BADGE),
            });
            changed = true;
        }
        let art = self.onboarding.fox.art.as_ref().expect("art loaded above");
        let shows = matches!(self.onboarding.step, Step::Welcome | Step::Done);
        if !shows || art.idle.len() != FRAMES || art.celebrate.len() != FRAMES {
            return changed;
        }
        let now = self.fox_pose();
        if now != Pose::Still {
            tx.wake_after(WAKE);
        }
        changed | (self.onboarding.fox.shown.replace(now) != Some(now))
    }
}

#[cfg(test)]
mod tests {
    use super::{FRAMES, IDLE, Pose, frames, pose};

    #[test]
    fn celebrating_plays_once_then_idles() {
        assert_eq!(pose(true, 0.0), Pose::Celebrate(0));
        assert_eq!(pose(true, 0.71), Pose::Celebrate(FRAMES - 1));
        assert_eq!(pose(true, 0.72), Pose::Idle(0));
        assert_eq!(pose(false, 1.4), Pose::Idle(0));
        assert_eq!(pose(false, 0.7), Pose::Idle(4));
        assert_eq!(pose(false, -1.0), Pose::Idle(0));
    }

    #[test]
    fn the_idle_strip_cuts_into_square_frames() {
        let (side, cut) = frames(IDLE).expect("brand idle strip decodes");
        assert_eq!(cut.len(), FRAMES);
        assert!(cut.iter().all(|f| f.len() == (side * side * 4) as usize));
    }
}
