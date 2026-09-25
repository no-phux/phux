//! Native terminal surface. Publications and glyphs never enter the retained JS tree.

#[cfg(feature = "terminal-fixtures")]
pub mod fixtures;
pub mod geometry;
mod input_host;
mod paint;
mod settings;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

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
            input: None,
            rebind: Arc::new(AtomicBool::new(false)),
        })
    }
}

struct Terminal {
    element_id: gpui::ElementId,
    settings: Settings,
    // The surface owns the acceptance record. Pending paint closures hold only
    // Weak references, so removal cannot resurrect a detached view's report.
    observation: Arc<Mutex<paint::Observation>>,
    input: Option<BoundInput>,
    rebind: Arc<AtomicBool>,
}

struct BoundInput {
    handle: String,
    view: ViewId,
    terminal: ResourceId,
    entity: gpui::Entity<crate::input::TerminalInput>,
}

impl Terminal {
    fn acquire(&self) -> Result<Arc<GridFrame>, String> {
        let view = self.settings.view_id.ok_or("missing viewId")?;
        let client = phux_client_ffi::napi::initialize()
            .client(&self.settings.client_handle)
            .map_err(|error| error.to_string())?;
        let frame = client.acquire_view(view).ok_or("view has no publication")?;
        if phux_client_ffi::projection::id::encode(&frame.terminal_id) != self.settings.terminal_id
        {
            return Err("terminalId does not match viewId".into());
        }
        Ok(frame)
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
        let frame = self.acquire();
        let settings = self.settings.clone();
        let observation = Arc::downgrade(&self.observation);
        let input = self.ensure_input(window, cx);
        let rebind = Arc::clone(&self.rebind);
        let mut surface =
            custom_surface(gpui::div().id(self.element_id.clone()), &ctx).overflow_hidden();
        if let Some(input) = input.clone() {
            let focus = input.read(cx).focus_handle().clone();
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
        surface
            .child(
                gpui::canvas(
                    move |bounds, window, cx| paint::prepare(frame, settings, bounds, window, cx),
                    move |_bounds, prepared, window, cx| {
                        let report = prepared.paint(_bounds, window, cx);
                        if let Some(input) = &painted {
                            input_host::present(input, &report, &rebind, window, cx);
                        }
                        if let Some(observation) = observation.upgrade() {
                            *observation
                                .lock()
                                .unwrap_or_else(|error| error.into_inner()) = report;
                        }
                    },
                )
                .size_full(),
            )
            .into_any_element()
    }

    fn set_prop(&mut self, key: &str, value: serde_json::Value) {
        self.settings.set(key, &value);
    }

    fn supported_props(&self) -> &'static [&'static str] {
        &[
            "clientHandle",
            "terminalId",
            "viewId",
            "font",
            "theme",
            "focused",
            "cursorVisible",
            "blinkVisible",
            "paintRevision",
        ]
    }

    fn supported_events(&self) -> &'static [&'static str] {
        &[]
    }

    fn destroy(&mut self) {
        self.input = None;
        self.rebind.store(false, Ordering::Release);
        self.observation = Arc::new(Mutex::new(paint::Observation::default()));
    }
}

pub(super) fn parse_view(value: &str) -> Option<ViewId> {
    value.parse::<u64>().ok().and_then(ViewId::from_raw)
}
