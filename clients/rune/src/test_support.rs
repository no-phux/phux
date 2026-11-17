//! Application fixtures: isolated preferences and real embedded clients.
//! Feature tests do not depend on renderer benchmarks or installed services.

use std::sync::Arc;

use phux_client_runtime::Client;
use rune::{Env, Runtime};

use crate::desk::{Desk, Shown};
use crate::launch::Resolved;

pub mod server;
pub mod terminal;
pub mod visual;

pub const SCALE: f32 = 2.0;
pub const SIZE: (f32, f32) = (1100.0, 720.0);

pub fn window() -> Desk {
    Desk::with_preferences(
        &Resolved::offline("test window"),
        None,
        rune::prefs::Prefs::memory(),
    )
}

pub fn settings_window() -> Desk {
    let mut desk = window();
    desk.settings_open = true;
    desk
}

pub fn input_fixture() -> (Arc<Client>, Runtime<Desk>) {
    input_fixture_showing("")
}

pub fn input_fixture_showing(output: &str) -> (Arc<Client>, Runtime<Desk>) {
    let mut replica = terminal::Replica::new(120, 40, 1);
    if !output.is_empty() {
        replica.feed(output.to_owned());
    }
    let mut desk = window();
    desk.connection.client = Some(Arc::clone(&replica.client));
    desk.connection.layout = Some(phux_client_core::layout::LayoutNode::Leaf(
        replica.terminal.clone(),
    ));
    desk.connection.shown = Some(Shown {
        resource: replica.terminal,
        view: Some(replica.views[0]),
    });
    let mut rt = Runtime::new(desk, crate::ui::view, SIZE, Env::new(true, SCALE));
    rt.on_key = Some(Desk::key);
    (replica.client, rt)
}

/// The text of the notice `desk` shows; panics when it shows none.
pub fn notice(desk: &Desk) -> &str {
    &desk.notice.as_ref().expect("a notice is showing").text
}
