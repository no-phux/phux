//! Native terminal surface. Publications and glyphs never enter the retained JS tree.

mod actions;
#[cfg(feature = "terminal-fixtures")]
pub mod fixtures;
pub mod geometry;
mod input_host;
mod paint;
mod settings;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::presentation::{self, Ticket};
use gpui::prelude::*;
use gpuix_native::native_extensions::{
    CustomElement, CustomElementFactory, CustomElementRegistry, CustomRenderContext, GpuixView,
    custom_surface, gpui,
};
use phux_client_runtime::{ViewId, publication::GridFrame};
use phux_protocol::ResourceId;

pub use paint::{GlyphObservation as PaintedGlyph, Observation as PaintedFrame};
use settings::Settings;

pub fn install(registry: &mut CustomElementRegistry) {
    registry.register(Box::new(TerminalFactory));
}

struct TerminalFactory;

impl CustomElementFactory for TerminalFactory {
    fn element_type(&self) -> &str {
        "phux-terminal"
    }

    fn create(&self, id: u64) -> Box<dyn CustomElement> {
        let observation = Arc::new(Mutex::new(paint::Observation::default()));
        #[cfg(feature = "terminal-fixtures")]
        fixtures::register(id, &observation);
        Box::new(Terminal {
            element_id: gpui::ElementId::Name(format!("__phux_terminal_{id}").into()),
            settings: Settings::default(),
            observation,
            scene: paint::SceneCache::default(),
            surface: presentation::Surface::default(),
            input: None,
            rebind: Arc::new(AtomicBool::new(false)),
            was_focused: false,
            actions: actions::HostActions::default(),
        })
    }
}

struct Terminal {
    element_id: gpui::ElementId,
    settings: Settings,
    // The surface owns the acceptance record. Pending paint closures hold only
    // Weak references, so removal cannot resurrect a detached view's report.
    observation: Arc<Mutex<paint::Observation>>,
    /// The last prepared scene, reused while its inputs are unchanged.
    scene: paint::SceneCache,
    surface: presentation::Surface,
    input: Option<BoundInput>,
    rebind: Arc<AtomicBool>,
    was_focused: bool,
    actions: actions::HostActions,
}

struct BoundInput {
    handle: String,
    view: ViewId,
    terminal: ResourceId,
    entity: gpui::Entity<crate::input::TerminalInput>,
}

impl Terminal {
    fn painted_frame(
        &mut self,
        window: &mut gpui::Window,
        cx: &mut gpui::App,
    ) -> (Result<Arc<GridFrame>, String>, Option<Ticket>) {
        let Some(view) = self.settings.view_id else {
            return (Err("missing viewId".into()), None);
        };
        let _timed = crate::perf::ACQUIRE.timer();
        match self.surface.acquire(
            &self.settings.client_handle,
            &self.settings.terminal_id,
            view,
            window,
            cx,
        ) {
            Ok(ticket) => (Ok(ticket.frame()), Some(ticket)),
            Err(error) => {
                crate::perf::ACQUIRE_REJECTED.incr();
                (Err(error.to_string()), None)
            }
        }
    }
}

impl Terminal {
    /// The bound input entity, and whether it is new this render: a new
    /// entity brings a new focus handle that nothing has focused yet.
    fn ensure_input(
        &mut self,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<GpuixView>,
    ) -> Option<(gpui::Entity<crate::input::TerminalInput>, bool)> {
        let view = self.settings.view_id?;
        let terminal = phux_client_ffi::projection::id::parse(&self.settings.terminal_id)?;
        let handle = self.settings.client_handle.clone();
        if handle.is_empty() {
            return None;
        }
        let same = self.input.as_ref().is_some_and(|bound| {
            bound.handle == handle && bound.view == view && bound.terminal == terminal
        });
        if same && !input_host::note_rebind(&self.rebind) {
            return self
                .input
                .as_ref()
                .map(|bound| (bound.entity.clone(), false));
        }
        let entity = cx.new(|cx| {
            crate::input::TerminalInput::new(
                handle.clone(),
                view,
                terminal.clone(),
                cx.focus_handle(),
                window,
            )
        });
        cx.observe(&entity, |_, _, cx| cx.notify()).detach();
        entity.update(cx, |state, _| {
            state.set_option_as_alt(self.settings.option_as_alt)
        });
        self.input = Some(BoundInput {
            handle,
            view,
            terminal,
            entity: entity.clone(),
        });
        Some((entity, true))
    }
}

impl CustomElement for Terminal {
    fn render(
        &mut self,
        ctx: CustomRenderContext,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<GpuixView>,
    ) -> gpui::AnyElement {
        crate::fonts::install(cx);
        crate::perf::RENDERS.incr();
        // Always reacquire, including after removal, skipped generations, or a
        // slot replacement. Dirty rows are not a cache-coherency contract; the
        // scene cache compares the acquired frame itself.
        let (frame, recovery) = self.painted_frame(window, cx);
        let epoch = recovery.as_ref().map(Ticket::connection_epoch);
        let scheduled = Rc::new(RefCell::new(recovery));
        let settings = self.settings.clone();
        let scene = Rc::clone(&self.scene);
        let observation = Arc::downgrade(&self.observation);
        let bound = self.ensure_input(window, cx);
        let rebound = bound.as_ref().is_some_and(|(_, fresh)| *fresh);
        let input = bound.map(|(input, _)| input);
        // Taken either way: a request with no bound input is dropped, not
        // replayed whenever a view next binds.
        let action = self.actions.take();
        let rebind = Arc::clone(&self.rebind);
        let mut surface =
            custom_surface(gpui::div().id(self.element_id.clone()), &ctx).overflow_hidden();
        if let Some(input) = input.clone() {
            input.update(cx, |state, _| {
                state.set_option_as_alt(self.settings.option_as_alt);
            });
            let focus = input.read(cx).focus_handle().clone();
            match focus_step(
                self.settings.focused,
                self.was_focused,
                rebound,
                focus.is_focused(window),
            ) {
                FocusStep::Focus => focus.focus(window, cx),
                // The shell withdrew input (a modal or find field took over).
                // Give the keyboard back to the window so keys cannot reach
                // the PTY behind an overlay that has no text field of its own.
                FocusStep::Blur => window.blur(),
                FocusStep::Keep => (),
            }
            self.was_focused = self.settings.focused;
            if let Some(action) = action {
                actions::run(action, &input, cx);
            }
            let down = input.clone();
            let app_chords = self.settings.app_chords.clone();
            let up = input.clone();
            let press = input.clone();
            let movement = input.clone();
            let release = input.clone();
            let wheel = input.clone();
            surface = surface
                .track_focus(&focus)
                .on_key_down(move |event, window, cx| {
                    // A chord the shell owns bubbles to the window untouched.
                    if !event.keystroke.modifiers.platform
                        && app_chords.contains(&input_host::chord(&event.keystroke))
                    {
                        return;
                    }
                    input_host::on_key_down(&down, event, window, cx)
                })
                .on_key_up(move |event, window, cx| input_host::on_key_up(&up, event, window, cx))
                .on_mouse_down(gpui::MouseButton::Left, move |event, window, cx| {
                    if event.modifiers.platform
                        && let Some(url) = press.read(cx).link_at(event.position)
                    {
                        cx.open_url(&url);
                        cx.stop_propagation();
                        return;
                    }
                    press.read(cx).focus_handle().clone().focus(window, cx);
                    press.update(cx, |state, cx| {
                        let _ = state.mouse_down(event, window, cx);
                    });
                    cx.stop_propagation();
                })
                .on_mouse_move(move |event, window, cx| {
                    movement.update(cx, |state, cx| {
                        let _ = state.mouse_move(event, window, cx);
                    });
                })
                .on_mouse_up(gpui::MouseButton::Left, move |event, window, cx| {
                    release.update(cx, |state, cx| {
                        let _ = state.mouse_up(event, window, cx);
                    });
                })
                .on_scroll_wheel(move |event, window, cx| {
                    let handled = wheel.update(cx, |state, cx| {
                        state.scroll(event, window, cx).unwrap_or(false)
                    });
                    if handled {
                        cx.stop_propagation();
                    }
                });
        }
        let painted = input;
        let size_owner = self.settings.size_owner;
        self.actions.arm();
        surface
            .child(
                gpui::canvas(
                    move |bounds, window, cx| {
                        paint::prepare(frame, settings, bounds, &scene, window, cx)
                    },
                    move |bounds, prepared, window, cx| {
                        let mut report = prepared.paint(bounds, window, cx);
                        report.epoch = epoch;
                        if let Some(input) = &painted {
                            input_host::present(input, &report, size_owner, &rebind, window, cx);
                            input_host::paint_preedit(
                                input,
                                report.geometry.bounds.origin,
                                window,
                                cx,
                            );
                        }
                        let painted_frame = report.frame.clone();
                        let bounds = report.geometry.bounds;
                        let failed = report.error.is_some();
                        if let Some(observation) = observation.upgrade() {
                            *observation
                                .lock()
                                .unwrap_or_else(|error| error.into_inner()) = report;
                        }
                        if !failed
                            && let Some(frame) = painted_frame.as_ref()
                            && let Some(ticket) = scheduled.borrow_mut().take()
                        {
                            presentation::schedule_recovery(ticket, frame, bounds, window, cx);
                        }
                    },
                )
                .size_full(),
            )
            .into_any_element()
    }

    fn set_prop(&mut self, key: &str, value: serde_json::Value) {
        if key == "hostAction" {
            self.actions.offer(&value);
            return;
        }
        if matches!(key, "clientHandle" | "terminalId" | "viewId") {
            self.surface.invalidate();
        }
        self.settings.set(key, &value);
    }

    fn supported_props(&self) -> &'static [&'static str] {
        &[
            "clientHandle",
            "terminalId",
            "viewId",
            "font",
            "theme",
            "optionAsAlt",
            "focused",
            "cursorVisible",
            "blinkVisible",
            "paintRevision",
            "sizeOwner",
            "appChords",
            "hostAction",
        ]
    }

    fn supported_events(&self) -> &'static [&'static str] {
        // Wired on primary mouse-up and OS drops by `custom_surface`. Pointer
        // input itself stays native; these only tell the shell which pane the
        // user chose and which paths arrived.
        &["click", "fileDrop"]
    }

    fn destroy(&mut self) {
        self.surface.invalidate();
        self.scene.borrow_mut().take();
        self.input = None;
        self.rebind.store(false, Ordering::Release);
        self.observation = Arc::new(Mutex::new(paint::Observation::default()));
    }
}

/// What a render does with its terminal's focus handle.
#[derive(Debug, PartialEq, Eq)]
enum FocusStep {
    Focus,
    Blur,
    Keep,
}

/// `wanted` is the shell's `focused` prop and `was` its value last render.
/// Focus follows the prop's edges, so a click elsewhere in the window is not
/// undone every frame. A `rebound` input (a new presentation identity after a
/// reconnect or re-bootstrap) has a new focus handle nothing has focused yet:
/// a terminal the shell still wants focused takes it, or keys would reach no
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

pub(super) fn parse_view(value: &str) -> Option<ViewId> {
    value.parse::<u64>().ok().and_then(ViewId::from_raw)
}

#[cfg(test)]
mod tests {
    use super::{FocusStep, focus_step};

    #[test]
    fn focus_follows_the_shell_prop_edges() {
        assert_eq!(focus_step(true, false, false, false), FocusStep::Focus);
        assert_eq!(focus_step(true, true, false, false), FocusStep::Keep);
        assert_eq!(focus_step(false, true, false, true), FocusStep::Blur);
        assert_eq!(focus_step(false, false, false, true), FocusStep::Keep);
    }

    #[test]
    fn a_rebound_input_takes_focus_the_shell_still_wants() {
        // A new presentation identity rebuilt the input and its focus handle.
        assert_eq!(focus_step(true, true, true, false), FocusStep::Focus);
        assert_eq!(focus_step(true, true, true, true), FocusStep::Keep);
        assert_eq!(focus_step(false, false, true, false), FocusStep::Keep);
    }
}
