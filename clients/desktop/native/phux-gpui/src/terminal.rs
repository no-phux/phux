//! Native terminal surface. One GPUI view paints one runtime publication.
//!
//! Identity is the `Client`, `ViewId` and `ResourceId` given to [`Terminal::new`].
//! Settings are ordinary fields, not a JSON prop bag, and [`Terminal::request`]
//! runs once on this bound entity instead of being replayed from a retained prop.

mod actions;
mod focus;
pub mod geometry;
mod input_host;
mod laser;
mod paint;
mod preview;
pub mod settings;

use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::presentation::{self, Ticket};
use gpui::{
    Context, EventEmitter, FocusHandle, MouseButton, Render, SharedString, Subscription, Window,
    canvas, div, prelude::*,
};
use phux_client_runtime::{Client, ViewId, publication::GridFrame};
use phux_protocol::ResourceId;

pub use actions::HostAction;
pub use paint::{GlyphObservation as PaintedGlyph, Observation as PaintedFrame};
pub use settings::Settings;
pub use preview::Preview;

/// Shell-facing signals that are not input. Pointer delivery stays native.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalEvent {
    /// A primary click chose this pane. File drops are the pane's, not the surface's.
    Activated,
    /// A paste or file link this surface refused.
    Notice {
        /// Notification title.
        title: &'static str,
        /// Notification body.
        body: String,
    },
    /// Local files or image bytes to upload into this terminal.
    /// The shell pastes the server path only after the receipt.
    Upload(crate::attach::UploadBatch),
    /// A paste the program would read as typed keys, held because it breaks
    /// a line. The shell asks first and sends it with [`HostAction::PasteConfirmed`].
    ConfirmPaste(crate::input::HeldPaste),
}

/// A terminal view bound to one runtime publication.
pub struct Terminal {
    client: Arc<Client>,
    view: ViewId,
    terminal: ResourceId,
    settings: Settings,
    focus: FocusHandle,
    element_id: SharedString,
    // Pending paint closures hold only weak references, so removal cannot
    // resurrect a detached view's report.
    observation: Arc<Mutex<paint::Observation>>,
    /// The last prepared scene, reused while its inputs are unchanged.
    scene: paint::SceneCache,
    surface: presentation::Surface,
    laser: laser::Laser,
    input: gpui::Entity<crate::input::TerminalInput>,
    /// Forwards clipboard refusals and upload requests from [`crate::input::TerminalInput`].
    /// Kept so the subscription lives with the view.
    _input_notices: Subscription,
    /// Registry host this window attaches to. Absent is the local server.
    window_remote: Option<String>,
    rebind: Arc<AtomicBool>,
    was_focused: bool,
    /// Epoch, input readiness and native focus last reported to the runtime.
    reported_focus: Option<(u64, bool, bool)>,
    _focus_subscriptions: Vec<Subscription>,
}

impl EventEmitter<TerminalEvent> for Terminal {}

impl Terminal {
    /// Bind `view` of `terminal` on `client`. The entity is the only identity;
    /// a replaced view or client is a new entity, not a prop update.
    ///
    /// This entity does not own the runtime view. When closing it on a shared
    /// client, the host must call `client.destroy_view(view)`; dropping the
    /// entity alone does not release the `ViewId`.
    pub fn new(
        client: Arc<Client>,
        view: ViewId,
        terminal: ResourceId,
        mut settings: Settings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        crate::perf::mark_started();
        crate::fonts::install(cx);
        settings.sanitize();
        crate::fonts::resolve(&mut settings.font, cx);
        let focus = cx.focus_handle();
        let (input, _input_notices) = input_host::bind_input(
            Arc::downgrade(&client),
            view,
            terminal.clone(),
            focus.clone(),
            settings.option_as_alt,
            window,
            cx,
        );
        let focus_subscriptions = focus::subscribe(&focus, window, cx);
        Self {
            element_id: SharedString::from(format!("phux-terminal-{}", view.get())),
            client,
            view,
            terminal,
            settings,
            focus,
            observation: Arc::new(Mutex::new(paint::Observation::default())),
            scene: paint::SceneCache::default(),
            surface: presentation::Surface::default(),
            laser: laser::Laser::default(),
            input,
            _input_notices,
            window_remote: None,
            rebind: Arc::new(AtomicBool::new(false)),
            was_focused: false,
            reported_focus: None,
            _focus_subscriptions: focus_subscriptions,
        }
    }

    /// The registry host this window attaches to. Absent is the local server.
    pub fn set_window_remote(&mut self, remote: Option<String>, cx: &mut Context<Self>) {
        if self.window_remote == remote {
            return;
        }
        self.window_remote = remote;
        let remote = self.window_remote.clone();
        self.input
            .update(cx, |state, _| state.set_window_remote(remote));
    }

    fn host(&self) -> crate::attach::TerminalHost {
        crate::attach::terminal_host(self.window_remote.as_deref(), &self.terminal)
    }

    /// Replace font, theme, visibility, input policy, size ownership and app
    /// chords. Out-of-range metrics are clamped. Identity does not change.
    pub fn configure(
        &mut self,
        mut settings: Settings,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        crate::fonts::install(cx);
        settings.sanitize();
        crate::fonts::resolve(&mut settings.font, cx);
        if self.settings.option_as_alt != settings.option_as_alt {
            self.input.update(cx, |state, _| {
                state.set_option_as_alt(settings.option_as_alt);
            });
        }
        // A desk refresh passes the same settings on every output frame.
        // Notifying here would acquire and prepare the terminal again.
        if self.settings == settings {
            return;
        }
        self.settings = settings;
        cx.notify();
    }

    /// Run `action` once, now, on this bound entity. It is not stored, so a
    /// later render cannot repeat it, and a request is never replayed onto a
    /// surface that was not yet bound.
    pub fn request(
        &mut self,
        action: HostAction,
        cx: &mut Context<Self>,
    ) -> Option<crate::input::InputNotice> {
        actions::run(action, &self.input, cx)
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    /// The last paint. Glyph positions are filled only with `terminal-fixtures`.
    pub fn painted(&self) -> paint::Observation {
        self.observation
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    fn painted_frame(
        &mut self,
        window: &Window,
    ) -> (Result<Arc<GridFrame>, String>, Option<Ticket>) {
        let _timed = crate::perf::ACQUIRE.timer();
        match self
            .surface
            .acquire(&self.client, &self.terminal, self.view, window)
        {
            Ok(ticket) => (Ok(ticket.frame()), Some(ticket)),
            Err(error) => {
                crate::perf::ACQUIRE_REJECTED.incr();
                log::debug!(
                    "terminal {:?} view {:?}: no paintable frame: {error}",
                    self.terminal,
                    self.view
                );
                (Err(error.to_string()), None)
            }
        }
    }

    /// A rejected presentation gets a new input entity so an old IME callback
    /// cannot re-arm. The focus handle stays on this view.
    fn rebind_input(&mut self, window: &Window, cx: &mut Context<Self>) {
        let (input, notices) = input_host::bind_input(
            Arc::downgrade(&self.client),
            self.view,
            self.terminal.clone(),
            self.focus.clone(),
            self.settings.option_as_alt,
            window,
            cx,
        );
        let remote = self.window_remote.clone();
        input.update(cx, |state, _| state.set_window_remote(remote));
        (self.input, self._input_notices) = (input, notices);
    }
}

impl Render for Terminal {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::fonts::install(cx);
        crate::fonts::resolve(&mut self.settings.font, cx);
        crate::perf::RENDERS.incr();
        let rebound = self.rebind.swap(false, Ordering::AcqRel);
        if rebound {
            self.rebind_input(window, cx);
            // A replaced presentation (re-attach, re-bootstrap) may have
            // cleared the runtime's focus; the next report asserts it again.
            self.reported_focus = None;
        }
        let has_focus = self.focus.is_focused(window);
        match focus_step(self.settings.focused, self.was_focused, rebound, has_focus) {
            FocusStep::Focus => self.focus.focus(window, cx),
            // The shell withdrew input (a modal or find field took over).
            // Give the keyboard back to the window so keys cannot reach the
            // PTY behind an overlay that has no text field of its own.
            FocusStep::Blur => window.blur(cx),
            FocusStep::Keep => {}
        }
        self.was_focused = self.settings.focused;
        self.report_focus(window);

        let (frame, recovery) = self.painted_frame(window);
        let fraction = frame
            .as_ref()
            .map_or(0., |frame| self.input.read(cx).fraction_of(frame));
        let now = Instant::now();
        let target = frame
            .as_ref()
            .ok()
            .filter(|_| !cx.reduce_motion())
            .and_then(|frame| paint::laser_target(frame, &self.settings));
        self.laser.observe(target.map(|(spot, _)| spot), now);
        let beam = target
            .filter(|_| self.laser.active(now))
            .map(|(_, color)| (self.laser, color));
        let epoch = recovery.as_ref().map(Ticket::connection_epoch);
        let settings = self.settings.clone();
        let scene = Rc::clone(&self.scene);
        let observation = Arc::downgrade(&self.observation);
        let input = self.input.clone();
        let size_owner = self.settings.size_owner;
        let rebind = Arc::clone(&self.rebind);
        let view = self.view;
        let this = cx.entity();
        let app_chords = self.settings.app_chords.clone();
        let (down, up, press, movement) =
            (input.clone(), input.clone(), input.clone(), input.clone());
        let (release, release_out, wheel) = (input.clone(), input.clone(), input.clone());
        let captured = input.clone();

        div()
            .id(self.element_id.clone())
            .size_full()
            .overflow_hidden()
            .track_focus(&self.focus)
            .key_context("PhuxTerminal")
            .on_key_down(move |event, window, cx| {
                // A chord the shell owns bubbles to the window untouched.
                if !event.keystroke.modifiers.platform
                    && app_chords.contains(&input_host::chord(&event.keystroke))
                {
                    return;
                }
                input_host::on_key_down(&down, event, window, cx);
            })
            .on_key_up(move |event, window, cx| input_host::on_key_up(&up, event, window, cx))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                    // Capture the painted link. The URL opens on release, if
                    // that release is still on the same link.
                    if event.modifiers.platform
                        && press.update(cx, |state, _| state.press_link(event.position))
                    {
                        cx.stop_propagation();
                        return;
                    }
                    this.focus.focus(window, cx);
                    press.update(cx, |state, cx| {
                        let _ = state.mouse_down(event, window, cx);
                    });
                    cx.stop_propagation();
                }),
            )
            .on_mouse_move(move |event, window, cx| {
                // A drag this view captured is delivered by the window-wide
                // listener below, inside the pane or not.
                if movement.read(cx).capturing() {
                    return;
                }
                movement.update(cx, |state, cx| {
                    let _ = state.mouse_move(event, window, cx);
                });
            })
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, event: &gpui::MouseUpEvent, window, cx| {
                    if let Some(url) = release.update(cx, |state, _| {
                        state.release_link(event.position, event.modifiers.platform)
                    }) {
                        match crate::input::admit_url(&url, &this.host()) {
                            crate::input::LinkAdmit::Open => cx.open_url(&url),
                            crate::input::LinkAdmit::Refuse { title, body } => {
                                cx.emit(TerminalEvent::Notice { title, body });
                            }
                        }
                    } else {
                        release.update(cx, |state, cx| {
                            let _ = state.mouse_up(event, window, cx);
                        });
                    }
                    cx.emit(TerminalEvent::Activated);
                }),
            )
            // A drag-select, reported press, or link gesture released outside
            // the pane still ends. The link does not open.
            .on_mouse_up_out(MouseButton::Left, move |event, window, cx| {
                release_out.update(cx, |state, cx| {
                    state.cancel_link();
                    let _ = state.mouse_up(event, window, cx);
                });
            })
            // Right and middle presses reach mouse-reporting programs (vim mouse=a, tmux).
            .on_mouse_down(MouseButton::Right, input_host::other_press(&input))
            .on_mouse_down(MouseButton::Middle, input_host::other_press(&input))
            .on_mouse_up(MouseButton::Right, input_host::other_release(&input))
            .on_mouse_up(MouseButton::Middle, input_host::other_release(&input))
            .on_mouse_up_out(MouseButton::Right, input_host::other_release(&input))
            .on_mouse_up_out(MouseButton::Middle, input_host::other_release(&input))
            .on_scroll_wheel(move |event, window, cx| {
                let handled = wheel.update(cx, |state, cx| state.scroll(event, window, cx).unwrap_or(false));
                if handled {
                    cx.stop_propagation();
                }
            })
            .child(
                canvas(
                    move |bounds, window, cx| paint::prepare(frame, settings, fraction, bounds, &scene, window, cx),
                    move |bounds, prepared, window, cx| {
                        // Hover-gated listeners stop at the pane's edge. A
                        // selection drag or captured button keeps following
                        // the pointer past it, so a drag can overshoot and
                        // autoscroll.
                        window.on_mouse_event(move |event: &gpui::MouseMoveEvent, phase, window, cx| {
                            if phase == gpui::DispatchPhase::Bubble && captured.read(cx).capturing() {
                                captured.update(cx, |state, cx| {
                                    let _ = state.mouse_move(event, window, cx);
                                });
                            }
                        });
                        let mut report = prepared.paint(bounds, window, cx);
                        if let Some((laser, color)) = beam
                            && report.frame.is_some()
                        {
                            let geometry = report.geometry;
                            window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
                                if laser.paint(geometry, color, Instant::now(), window) {
                                    window.request_animation_frame();
                                }
                            });
                        }
                        report.view_id = Some(view);
                        report.epoch = epoch;
                        let needs_rebind =
                            input_host::present(&input, &report, size_owner, &rebind, window, cx);
                        input_host::paint_preedit(
                            &input,
                            report.geometry.bounds.origin,
                            window,
                            cx,
                        );
                        let painted_frame = report.frame.clone();
                        let bounds = report.geometry.bounds;
                        let failed = report.error.is_some();
                        if let Some(observation) = observation.upgrade() {
                            *observation.lock().unwrap_or_else(|error| error.into_inner()) = report;
                        }
                        if needs_rebind {
                            let this = this.clone();
                            window.on_next_frame(move |_, cx| {
                                this.update(cx, |_, cx| cx.notify());
                            });
                        }
                        if !failed
                            && let Some(frame) = painted_frame.as_ref()
                            && let Some(ticket) = recovery
                        {
                            presentation::schedule_recovery(ticket, frame, bounds, window, cx);
                        }
                    },
                )
                .size_full(),
            )
    }
}

/// What a render does with its terminal's focus handle.
#[derive(Debug, PartialEq, Eq)]
enum FocusStep {
    Focus,
    Blur,
    Keep,
}

/// `wanted` is the shell's `focused` flag and `was` its value last render.
/// Focus follows the flag's edges, so a click elsewhere in the window is not
/// undone every frame. A `rebound` input (a new presentation identity after a
/// reconnect or re-bootstrap) has a new handler nothing has focused yet: a
/// terminal the shell still wants focused takes it, or keys would reach no
/// terminal until the user clicked it.
fn focus_step(wanted: bool, was: bool, rebound: bool, has_focus: bool) -> FocusStep {
    if wanted && (!was || rebound) && !has_focus {
        return FocusStep::Focus;
    }
    if !wanted && was && has_focus {
        return FocusStep::Blur;
    }
    FocusStep::Keep
}

#[cfg(test)]
mod tests;
