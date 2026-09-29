//! Window chrome GPUIX does not expose to JavaScript. With a transparent
//! titlebar the app draws its own tab bar, so the empty parts of that bar must
//! still move the window and zoom it on double-click, as AppKit's titlebar does.

use gpui::prelude::*;
use gpuix_native::native_extensions::{
    CustomElement, CustomElementFactory, CustomElementRegistry, CustomRenderContext, GpuixView,
    custom_surface, gpui,
};

pub fn install(registry: &mut CustomElementRegistry) {
    registry.register(Box::new(DragRegionFactory));
}

struct DragRegionFactory;

impl CustomElementFactory for DragRegionFactory {
    fn element_type(&self) -> &str {
        "phux-drag-region"
    }

    fn create(&self, id: u64) -> Box<dyn CustomElement> {
        Box::new(DragRegion {
            element_id: gpui::ElementId::Name(format!("__phux_drag_region_{id}").into()),
        })
    }
}

struct DragRegion {
    element_id: gpui::ElementId,
}

impl CustomElement for DragRegion {
    fn render(
        &mut self,
        ctx: CustomRenderContext,
        _window: &mut gpui::Window,
        _cx: &mut gpui::Context<GpuixView>,
    ) -> gpui::AnyElement {
        custom_surface(gpui::div().id(self.element_id.clone()), &ctx)
            .on_mouse_down(gpui::MouseButton::Left, |event, window, cx| {
                if event.click_count >= 2 {
                    window.titlebar_double_click();
                } else {
                    window.start_window_move();
                }
                cx.stop_propagation();
            })
            .into_any_element()
    }

    fn set_prop(&mut self, _key: &str, _value: serde_json::Value) {}

    fn supported_props(&self) -> &'static [&'static str] {
        &[]
    }

    fn supported_events(&self) -> &'static [&'static str] {
        &[]
    }

    fn destroy(&mut self) {}
}
