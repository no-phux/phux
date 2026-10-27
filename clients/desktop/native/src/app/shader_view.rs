//! A view whose box a shader program draws.
//!
//! The view owns a [`Renderer`] that draws on a background thread and reads
//! the pixels back; each finished frame becomes the one GPUI image the view
//! paints, and the frame before it leaves the atlas. A program that reads the
//! clock renders at the view's frame rate while its window is key and at a
//! slower one while it is not; a still program renders only when its inputs
//! or size change. Without motion the clock stops at [`STILL_TIME`].
//!
//! The fleet's counts arrive through [`ShaderView::set_fleet`]; the view eases
//! them into `phux.energy` and fires `phux.pulse` when more agents need you.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{Bounds, Context, Corners, Pixels, RenderImage, Task, Window, canvas, px};

use crate::shader::gpu::{Frame, Renderer};
use crate::shader::{Program, Uniforms};

/// The time a program shows while motion is off: far enough in that every
/// built-in has left its opening pose.
pub const STILL_TIME: f32 = 7.5;

/// Agent counts across every connected machine.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Fleet {
    pub working: u32,
    pub blocked: u32,
    pub done: u32,
    pub idle: u32,
    pub agents: u32,
    /// Blocked, finished or urgent: what fires the pulse when it grows.
    pub needs_you: u32,
}

/// The fleet's eased signals.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Signals {
    pub energy: f32,
    pub pulse: f32,
    needs_you: u32,
}

/// How long energy takes to cover most of a change, and the pulse to fade.
const ENERGY_EASE: f32 = 1.2;
const PULSE_FADE: f32 = 0.55;

impl Signals {
    /// The energy `working` agents settle toward: one agent is a third of
    /// full, five are nearly all of it.
    #[must_use]
    pub fn target(working: u32) -> f32 {
        1. - (-(working as f32) / 2.5).exp()
    }

    /// Move `dt` seconds toward `fleet`. Without motion the energy lands at
    /// once and nothing flashes.
    pub fn advance(&mut self, fleet: Fleet, dt: f32, motion: bool) {
        let target = Self::target(fleet.working);
        let grew = fleet.needs_you > self.needs_you;
        self.needs_you = fleet.needs_you;
        if !motion {
            self.energy = target;
            self.pulse = 0.;
            return;
        }
        if grew {
            self.pulse = 1.;
        }
        let dt = dt.clamp(0., 1.);
        self.energy += (target - self.energy) * (1. - (-dt / ENERGY_EASE).exp());
        if (self.energy - target).abs() < 0.002 {
            self.energy = target;
        }
        self.pulse *= (-dt / PULSE_FADE).exp();
        if self.pulse < 0.004 {
            self.pulse = 0.;
        }
    }

    /// Still moving toward `fleet`.
    #[must_use]
    pub fn settling(&self, fleet: Fleet) -> bool {
        self.energy != Self::target(fleet.working) || self.pulse > 0.
    }
}

/// The pixel size to render a `points` box at `density` pixels per point,
/// shrunk to fit `max` on its longer side.
#[must_use]
pub fn pixels(points: (f32, f32), density: f32, max: u32) -> (u32, u32) {
    let (width, height) = (points.0.max(1.) * density, points.1.max(1.) * density);
    let fit = (max as f32 / width.max(height)).min(1.);
    (
        (width * fit).round().max(1.) as u32,
        (height * fit).round().max(1.) as u32,
    )
}

/// Frame rates and resolution for one kind of view.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pace {
    /// Frames a second while the window is key, and while it is not.
    pub fps: f32,
    pub background_fps: f32,
    /// Pixels per point, before the renderer's size cap.
    pub density: f32,
    pub radius: f32,
}

pub struct ShaderView {
    program: Option<Arc<Program>>,
    base: Uniforms,
    fleet: Fleet,
    signals: Signals,
    motion: bool,
    pace: Pace,
    started: Instant,
    last_tick: Option<Instant>,
    frames: u32,
    /// Bounds the last paint measured, shared with the canvas.
    measured: Rc<Cell<Option<Bounds<Pixels>>>>,
    renderer: Arc<Mutex<Option<Renderer>>>,
    image: Option<Arc<RenderImage>>,
    in_flight: bool,
    /// An input changed since the last frame started.
    stale: bool,
    /// The last frame failed; wait for a new input before trying again.
    failed: Option<String>,
    next: Option<Task<()>>,
    /// Wakes the clock when the window is shown again.
    visibility: Option<gpui::Subscription>,
}

impl ShaderView {
    pub fn new(pace: Pace, cx: &mut Context<Self>) -> Self {
        cx.on_release(|view: &mut Self, cx| {
            if let Some(image) = view.image.take() {
                cx.drop_image(image, None);
            }
        })
        .detach();
        Self {
            program: None,
            base: Uniforms::default(),
            fleet: Fleet::default(),
            signals: Signals::default(),
            motion: true,
            pace,
            started: Instant::now(),
            last_tick: None,
            frames: 0,
            measured: Rc::default(),
            renderer: Arc::default(),
            image: None,
            in_flight: false,
            stale: true,
            failed: None,
            next: None,
            visibility: None,
        }
    }

    /// Show `program`, or nothing.
    pub fn set_program(&mut self, program: Option<Arc<Program>>, cx: &mut Context<Self>) {
        let same = match (&self.program, &program) {
            (Some(old), Some(new)) => old.hash == new.hash,
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        self.program = program;
        if self.program.is_none()
            && let Some(image) = self.image.take()
        {
            cx.drop_image(image, None);
        }
        self.invalidate(cx);
    }

    /// The theme, intensity and everything else the owner sets; the view
    /// fills in size, pointer, clock and fleet signals.
    pub fn set_uniforms(&mut self, base: Uniforms, cx: &mut Context<Self>) {
        if self.base != base {
            self.base = base;
            self.invalidate(cx);
        }
    }

    pub fn set_fleet(&mut self, fleet: Fleet, cx: &mut Context<Self>) {
        if self.fleet != fleet {
            self.fleet = fleet;
            self.invalidate(cx);
        }
    }

    /// Whether the clock runs. Off shows a still frame at [`STILL_TIME`].
    pub fn set_motion(&mut self, motion: bool, cx: &mut Context<Self>) {
        if self.motion != motion {
            self.motion = motion;
            self.invalidate(cx);
        }
    }

    /// Frames rendered so far.
    #[cfg(feature = "ui-fixtures")]
    pub fn frames(&self) -> u32 {
        self.frames
    }

    /// Why the last frame could not be drawn.
    pub fn failure(&self) -> Option<&str> {
        self.failed.as_deref()
    }

    fn invalidate(&mut self, cx: &mut Context<Self>) {
        self.stale = true;
        self.failed = None;
        cx.notify();
    }

    fn wants_clock(&self) -> bool {
        let animated = self
            .program
            .as_ref()
            .is_some_and(|program| program.animated);
        self.motion && (animated || self.signals.settling(self.fleet))
    }

    /// Arrange the next tick, if anything wants one. A changed input always
    /// renders, so a window shown again never starts stale; the clock runs
    /// only while the platform presents the window (not covered, minimized
    /// or on a sleeping display).
    fn schedule(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.next.is_some() || self.in_flight || self.failed.is_some() || self.program.is_none()
        {
            return;
        }
        let delay = if self.stale {
            Duration::ZERO
        } else if self.wants_clock() && window.is_visible() {
            let fps = if window.is_window_active() {
                self.pace.fps
            } else {
                self.pace.background_fps
            };
            let interval = Duration::from_secs_f32(1. / fps.max(0.5));
            interval.saturating_sub(self.last_tick.map_or(interval, |at| at.elapsed()))
        } else {
            return;
        };
        let timer = cx.background_executor().timer(delay);
        self.next = Some(cx.spawn_in(window, async move |view, cx| {
            timer.await;
            let _ = view.update_in(cx, |view, window, cx| {
                view.next = None;
                view.tick(window, cx);
            });
        }));
    }

    fn tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(program), Some(bounds)) = (self.program.clone(), self.measured.get()) else {
            return;
        };
        if self.in_flight {
            return;
        }
        let now = Instant::now();
        let dt = self
            .last_tick
            .map_or(0., |at| now.duration_since(at).as_secs_f32());
        self.last_tick = Some(now);
        self.signals.advance(self.fleet, dt, self.motion);
        let size = (bounds.size.width.as_f32(), bounds.size.height.as_f32());
        let (width, height) = pixels(size, self.pace.density, crate::shader::gpu::MAX_SIDE);
        let mouse = window.mouse_position();
        let pointer = if bounds.contains(&mouse) {
            [
                (mouse.x - bounds.origin.x).as_f32() / size.0.max(1.),
                (mouse.y - bounds.origin.y).as_f32() / size.1.max(1.),
            ]
        } else {
            [-1., -1.]
        };
        let fleet = self.fleet;
        let uniforms = Uniforms {
            size: [size.0, size.1],
            pointer,
            time: if self.motion {
                self.started.elapsed().as_secs_f32()
            } else {
                STILL_TIME
            },
            scale: width as f32 / size.0.max(1.),
            energy: self.signals.energy,
            pulse: self.signals.pulse,
            working: fleet.working as f32,
            blocked: fleet.blocked as f32,
            done: fleet.done as f32,
            idle: fleet.idle as f32,
            agents: fleet.agents as f32,
            frame: self.frames as f32,
            ..self.base
        };
        self.frames = self.frames.wrapping_add(1);
        self.stale = false;
        self.in_flight = true;
        let renderer = self.renderer.clone();
        let frame = cx
            .background_executor()
            .spawn(async move { render(&renderer, &program, &uniforms, width, height) });
        cx.spawn_in(window, async move |view, cx| {
            let frame = frame.await;
            let _ = view.update_in(cx, |view, window, cx| view.finish(frame, window, cx));
        })
        .detach();
    }

    fn finish(
        &mut self,
        frame: Result<Frame, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.in_flight = false;
        match frame.and_then(image) {
            Ok(image) => {
                if self.program.is_none() {
                    cx.drop_image(image, Some(window));
                } else if let Some(old) = self.image.replace(image) {
                    cx.drop_image(old, Some(window));
                }
            }
            Err(error) => {
                if self.failed.as_deref() != Some(&error) {
                    log::warn!("shader frame failed: {error}");
                }
                self.failed = Some(error);
            }
        }
        // The clock does not wait for the window to draw this frame.
        self.schedule(window, cx);
        cx.notify();
    }
}

fn render(
    renderer: &Mutex<Option<Renderer>>,
    program: &Program,
    uniforms: &Uniforms,
    width: u32,
    height: u32,
) -> Result<Frame, String> {
    let mut slot = renderer.lock().unwrap_or_else(PoisonError::into_inner);
    if slot.as_ref().is_none_or(Renderer::lost) {
        *slot = Some(Renderer::new()?);
    }
    let Some(renderer) = slot.as_mut() else {
        return Err("no renderer".into());
    };
    renderer.render(program, uniforms, width, height)
}

fn image(frame: Frame) -> Result<Arc<RenderImage>, String> {
    let buffer = image::RgbaImage::from_raw(frame.width, frame.height, frame.bgra)
        .ok_or("shader frame has the wrong size")?;
    Ok(Arc::new(RenderImage::new([image::Frame::new(buffer)])))
}

impl Render for ShaderView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.visibility.is_none() {
            let view = cx.weak_entity();
            self.visibility = Some(window.observe_window_visibility(move |_, _, cx| {
                let _ = view.update(cx, |_, cx| cx.notify());
            }));
        }
        self.schedule(window, cx);
        let image = self.image.clone();
        let measured = self.measured.clone();
        let radius = px(self.pace.radius);
        let view = cx.weak_entity();
        canvas(
            move |bounds, window, _| {
                if measured.replace(Some(bounds)).map(|old| old.size) != Some(bounds.size) {
                    window.on_next_frame(move |_, cx| {
                        let _ = view.update(cx, |view, cx| view.invalidate(cx));
                    });
                }
            },
            move |bounds, (), window, _| {
                if let Some(image) = image {
                    let _ =
                        window.paint_image(bounds, bounds, Corners::all(radius), image, 0, false);
                }
            },
        )
        .size_full()
    }
}

#[cfg(test)]
mod tests;
