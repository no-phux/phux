//! Native terminal surface. Publications and glyphs never enter the retained JS tree.

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
            surface: presentation::Surface::default(),
            input: None,
            rebind: Arc::new(AtomicBool::new(false)),
            was_focused: false,
        })
    }
}

struct Terminal {
    element_id: gpui::ElementId,
    settings: Settings,
    // The surface owns the acceptance record. Pending paint closures hold only
    // Weak references, so removal cannot resurrect a detached view's report.
    observation: Arc<Mutex<paint::Observation>>,
    surface: presentation::Surface,
    input: Option<BoundInput>,
    rebind: Arc<AtomicBool>,
    was_focused: bool,
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
        match self.surface.acquire(
            &self.settings.client_handle,
            &self.settings.terminal_id,
            view,
            window,
            cx,
        ) {
            Ok(ticket) => (Ok(ticket.frame()), Some(ticket)),
            Err(error) => (Err(error.to_string()), None),
        }
    }
}

impl Terminal {
    fn ensure_input(
        &mut self,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<GpuixView>,
    ) -> Option<gpui::Entity<crate::input::TerminalInput>> {
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
            return self.input.as_ref().map(|bound| bound.entity.clone());
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
        Some(entity)
    }
}

impl CustomElement for Terminal {
    fn render(
        &mut self,
        ctx: CustomRenderContext,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<GpuixView>,
    ) -> gpui::AnyElement {
        // Always reacquire, including after removal, skipped generations, or a
        // slot replacement. Dirty rows are not a cache-coherency contract.
        let (frame, recovery) = self.painted_frame(window, cx);
        let scheduled = Rc::new(RefCell::new(recovery));
        let settings = self.settings.clone();
        let observation = Arc::downgrade(&self.observation);
        let input = self.ensure_input(window, cx);
        let rebind = Arc::clone(&self.rebind);
        let mut surface =
            custom_surface(gpui::div().id(self.element_id.clone()), &ctx).overflow_hidden();
        if let Some(input) = input.clone() {
            input.update(cx, |state, _| {
                state.set_option_as_alt(self.settings.option_as_alt);
            });
            let focus = input.read(cx).focus_handle().clone();
            if self.settings.focused && !self.was_focused && !focus.is_focused(window) {
                focus.focus(window, cx);
            }
            // The shell withdrew input (a modal or find field took over). Give
            // the keyboard back to the window so keys cannot reach the PTY
            // behind an overlay that has no text field of its own.
            if !self.settings.focused && self.was_focused && focus.is_focused(window) {
                window.blur();
            }
            self.was_focused = self.settings.focused;
            let down = input.clone();
            let up = input.clone();
            let press = input.clone();
            let movement = input.clone();
            let release = input.clone();
            let wheel = input.clone();
            surface = surface
                .track_focus(&focus)
                .on_key_down(move |event, window, cx| {
                    input_host::on_key_down(&down, event, window, cx)
                })
                .on_key_up(move |event, window, cx| input_host::on_key_up(&up, event, window, cx))
                .on_mouse_down(gpui::MouseButton::Left, move |event, window, cx| {
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
        surface
            .child(
                gpui::canvas(
                    move |bounds, window, cx| paint::prepare(frame, settings, bounds, window, cx),
                    move |_bounds, prepared, window, cx| {
                        let report = prepared.paint(_bounds, window, cx);
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
        self.input = None;
        self.rebind.store(false, Ordering::Release);
        self.observation = Arc::new(Mutex::new(paint::Observation::default()));
    }
}

pub(super) fn parse_view(value: &str) -> Option<ViewId> {
    value.parse::<u64>().ok().and_then(ViewId::from_raw)
}
