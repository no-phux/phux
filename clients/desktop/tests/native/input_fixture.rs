//! Test-only custom surface. Include from the host with a temporary path module
//! and install alongside the probe; never register in the shipping host.
#![cfg_attr(
    test,
    allow(
        dead_code,
        reason = "NAPI exports are exercised by the Node fixture, not Rust test registration"
    )
)]
use crate::input::{InputMetrics, KeyDisposition, TerminalInput};
use gpui::{EntityInputHandler, prelude::*};
use gpuix_native::native_extensions::{
    CustomElement, CustomElementFactory, CustomElementRegistry, CustomRenderContext, GpuixView,
    custom_surface, gpui,
};
use phux_client_runtime::ViewId;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Mutex, OnceLock},
};

fn snapshots() -> &'static Mutex<BTreeMap<u64, Value>> {
    static VALUES: OnceLock<Mutex<BTreeMap<u64, Value>>> = OnceLock::new();
    VALUES.get_or_init(Mutex::default)
}

#[napi_derive::napi]
pub fn input_fixture_snapshot(id: u32) -> String {
    snapshots()
        .lock()
        .expect("fixture snapshot")
        .get(&u64::from(id))
        .unwrap_or(&Value::Null)
        .to_string()
}

#[napi_derive::napi]
pub fn input_fixture_create_view(handle: String, terminal: u32) -> napi::Result<String> {
    let client = phux_client_ffi::napi::initialize()
        .client(&handle)
        .map_err(|e| napi::Error::from_reason(e.to_string()))?;
    client
        .create_view(&phux_protocol::ResourceId::local(terminal))
        .map(|v| v.get().to_string())
        .map_err(|e| napi::Error::from_reason(e.to_string()))
}

#[napi_derive::napi]
pub fn input_fixture_destroy_view(handle: String, view: String) -> napi::Result<()> {
    let client = phux_client_ffi::napi::initialize()
        .client(&handle)
        .map_err(|e| napi::Error::from_reason(e.to_string()))?;
    let view = ViewId::from_raw(view.parse().expect("fixture view")).expect("view");
    client
        .destroy_view(view)
        .map_err(|e| napi::Error::from_reason(e.to_string()))
}

#[napi_derive::napi]
pub fn input_fixture_view_state(handle: String, view: String) -> String {
    let client = phux_client_ffi::napi::initialize()
        .client(&handle)
        .expect("client");
    let view = ViewId::from_raw(view.parse().expect("fixture view")).expect("view");
    let Some(frame) = client.acquire_view(view) else {
        return "null".into();
    };
    let engine = client.engine().expect("engine");
    json!({ "stream": frame.stream_id, "offset": frame.scrollbar.offset, "mouse": format!("{:?}", engine.mouse_mode(&frame.terminal_id).expect("mouse")), "selection": engine.view_selection_text(view).ok().map(|text| String::from_utf8_lossy(&text).into_owned()) }).to_string()
}

#[napi_derive::napi]
pub fn input_fixture_resync(handle: String) {
    phux_client_ffi::napi::initialize()
        .client(&handle)
        .expect("client")
        .resync();
}

pub fn install(registry: &mut CustomElementRegistry) {
    registry.register(Box::new(Factory));
}
struct Factory;
impl CustomElementFactory for Factory {
    fn element_type(&self) -> &str {
        "phux-input-fixture"
    }
    fn create(&self, id: u64) -> Box<dyn CustomElement> {
        Box::new(Fixture {
            id,
            handle: String::new(),
            view: None,
            terminal: 0,
            input: None,
            command: None,
            last_command: Value::Null,
        })
    }
}

struct Fixture {
    id: u64,
    handle: String,
    view: Option<ViewId>,
    terminal: u32,
    input: Option<gpui::Entity<TerminalInput>>,
    command: Option<Value>,
    last_command: Value,
}

impl CustomElement for Fixture {
    fn render(
        &mut self,
        ctx: CustomRenderContext,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<GpuixView>,
    ) -> gpui::AnyElement {
        let view = self.view.expect("view prop precedes mount");
        let input = self
            .input
            .get_or_insert_with(|| {
                cx.new(|cx| {
                    let mut input = TerminalInput::new(
                        self.handle.clone(),
                        view,
                        phux_protocol::ResourceId::local(self.terminal),
                        cx.focus_handle(),
                        window,
                    );
                    input.simulate_window_activation(true);
                    input
                })
            })
            .clone();
        if let Some(command) = self.command.take() {
            input.update(cx, |state, cx| run_command(state, &command, window, cx));
        }
        let focus = input.read(cx).focus_handle().clone();
        let down = input.clone();
        let up = input.clone();
        let mouse_down = input.clone();
        let mouse_move = input.clone();
        let mouse_up = input.clone();
        let scroll = input.clone();
        let id = self.id;
        let handle = self.handle.clone();
        custom_surface(gpui::div().id(gpui::ElementId::Name(format!("input-fixture-{id}").into())), &ctx)
            .track_focus(&focus)
            .on_key_down(move |event, window, cx| {
                let result = down.update(cx, |state, cx| state.key_down(event, window, cx));
                if result != Ok(KeyDisposition::Platform) { cx.stop_propagation(); }
                snapshots().lock().expect("snapshot").entry(id).or_default()["keyDown"] = json!(format!("{result:?}"));
            })
            .on_key_up(move |event, window, cx| {
                let result = up.update(cx, |state, cx| state.key_up(event, window, cx));
                if result != Ok(KeyDisposition::Platform) { cx.stop_propagation(); }
            })
            .on_mouse_down(gpui::MouseButton::Left, move |event, window, cx| {
                mouse_down.read(cx).focus_handle().clone().focus(window, cx);
                mouse_down.update(cx, |state, cx| { if let Err(error) = state.mouse_down(event, window, cx) { eprintln!("fixture mouse down refused: {error:?}"); } });
                cx.stop_propagation();
            })
            .on_mouse_move(move |event, window, cx| {
                mouse_move.update(cx, |state, cx| { let _result = state.mouse_move(event, window, cx); });
            })
            .on_mouse_up(gpui::MouseButton::Left, move |event, window, cx| {
                mouse_up.update(cx, |state, cx| { let _result = state.mouse_up(event, window, cx); });
            })
            .on_scroll_wheel(move |event, window, cx| {
                scroll.update(cx, |state, cx| { assert!(state.scroll(event, window, cx).is_ok()); });
                cx.stop_propagation();
            })
            .on_painted(move |bounds, window, cx| {
                let client = phux_client_ffi::napi::initialize().client(&handle).ok();
                let frame = client.as_ref().and_then(|client| client.acquire_view(view));
                if let Some(frame) = &frame {
                    let metrics = InputMetrics { bounds, cell_width: gpui::px(10.0), line_height: gpui::px(20.0), cursor_bounds: gpui::Bounds::new(bounds.origin, gpui::size(gpui::px(10.0), gpui::px(20.0))) };
                    input.update(cx, |state, _| { let _presentation = state.presented(frame, metrics); });
                }
                let focus = input.read(cx).focus_handle().clone();
                window.handle_input(&focus, gpui::ElementInputHandler::new(bounds, input.clone()), cx);
                let (preedit, selected) = input.read(cx).preedit();
                let value = json!({ "preedit": preedit, "selected": [selected.start, selected.end], "error": format!("{:?}", input.read(cx).last_error()), "text": frame.as_ref().map(|f| f.text()), "offset": frame.as_ref().map(|f| f.scrollbar.offset), "focused": focus.is_focused(window), "active": window.is_window_active() });
                snapshots().lock().expect("snapshot").insert(id, value);
            })
            .into_any_element()
    }

    fn set_prop(&mut self, key: &str, value: Value) {
        match key {
            "handle" => self.handle = value.as_str().expect("handle").to_owned(),
            "view" => {
                self.view =
                    ViewId::from_raw(value.as_str().expect("view").parse().expect("view id"))
            }
            "terminal" => self.terminal = value.as_u64().expect("terminal") as u32,
            "command" if value.is_object() && value != self.last_command => {
                self.last_command = value.clone();
                self.command = Some(value);
            }
            _ => {}
        }
    }
    fn supported_props(&self) -> &'static [&'static str] {
        &["handle", "view", "terminal", "command"]
    }
    fn supported_events(&self) -> &'static [&'static str] {
        &[]
    }
    fn destroy(&mut self) {
        self.input = None;
    }
}

fn run_command(
    state: &mut TerminalInput,
    command: &Value,
    window: &mut gpui::Window,
    cx: &mut gpui::Context<TerminalInput>,
) {
    let text = command["text"].as_str().unwrap_or_default();
    match command["kind"].as_str().expect("command") {
        "mark" => state.replace_and_mark_text_in_range(None, text, Some(1..2), window, cx),
        "commit" => state.replace_text_in_range(None, text, window, cx),
        "unmark" => state.unmark_text(window, cx),
        "paste" => {
            state
                .paste_text(text, window, cx)
                .expect("paste correlation");
        }
        "copy" => state.copy_selection(window, cx).expect("copy"),
        "cancel" => state.cancel(),
        "option" => state.set_option_as_alt(command["enabled"].as_bool().expect("enabled")),
        "active" => {
            state.simulate_window_activation(command["enabled"].as_bool().expect("enabled"))
        }
        _ => panic!("unknown fixture command"),
    }
}
