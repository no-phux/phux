//! An honest native extension fixture. This is deliberately not a terminal tag.

use std::sync::atomic::{AtomicU32, Ordering};

use gpui::prelude::*;
use gpuix_native::native_extensions::{
    CustomElement, CustomElementFactory, CustomElementRegistry, CustomRenderContext, GpuixView,
    custom_surface, gpui,
};
use napi_derive::napi;

static CREATED: AtomicU32 = AtomicU32::new(0);
static DESTROYED: AtomicU32 = AtomicU32::new(0);
static DROPPED: AtomicU32 = AtomicU32::new(0);
static PAINTED: AtomicU32 = AtomicU32::new(0);

pub fn install(registry: &mut CustomElementRegistry) {
    registry.register(Box::new(ProbeFactory));
}

struct ProbeFactory;

impl CustomElementFactory for ProbeFactory {
    fn element_type(&self) -> &str {
        "phux-host-probe"
    }

    fn create(&self, id: u64) -> Box<dyn CustomElement> {
        CREATED.fetch_add(1, Ordering::Relaxed);
        Box::new(Probe {
            element_id: gpui::ElementId::Name(format!("__phux_host_probe_{id}").into()),
            label: String::new(),
        })
    }
}

struct Probe {
    element_id: gpui::ElementId,
    label: String,
}

impl CustomElement for Probe {
    fn render(
        &mut self,
        ctx: CustomRenderContext,
        _window: &mut gpui::Window,
        _cx: &mut gpui::Context<GpuixView>,
    ) -> gpui::AnyElement {
        custom_surface(gpui::div().id(self.element_id.clone()), &ctx)
            .child(ctx.text(0, self.label.clone(), None))
            .on_painted(|_, _, _| {
                PAINTED.fetch_add(1, Ordering::Relaxed);
            })
            .into_any_element()
    }

    fn set_prop(&mut self, key: &str, value: serde_json::Value) {
        if key == "label" {
            self.label = value.as_str().unwrap_or_default().to_owned();
        }
    }

    fn supported_props(&self) -> &'static [&'static str] {
        &["label"]
    }

    fn supported_events(&self) -> &'static [&'static str] {
        &["click", "mouseEnter", "mouseLeave"]
    }

    fn destroy(&mut self) {
        DESTROYED.fetch_add(1, Ordering::Relaxed);
    }
}

impl Drop for Probe {
    fn drop(&mut self) {
        DROPPED.fetch_add(1, Ordering::Relaxed);
    }
}

#[napi(object)]
pub struct HostProbeCounts {
    pub created: u32,
    pub destroyed: u32,
    pub dropped: u32,
    pub painted: u32,
}

/// Process-local fixture counts, including actual GPUI paint callbacks.
#[napi]
pub fn desktop_host_probe_counts() -> HostProbeCounts {
    HostProbeCounts {
        created: CREATED.load(Ordering::Relaxed),
        destroyed: DESTROYED.load(Ordering::Relaxed),
        dropped: DROPPED.load(Ordering::Relaxed),
        painted: PAINTED.load(Ordering::Relaxed),
    }
}
