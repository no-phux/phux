//! The smallest GPUI app with a phux terminal in it.
//!
//! ```sh
//! cargo run -p phux-gpui --example embed -- [SOCKET] [SESSION]
//! ```
//!
//! `SOCKET` defaults to `$PHUX_SOCKET` (every phux pane exports it) and
//! `SESSION` to `main`. The example attaches that session and shows its first
//! terminal, which follows the window's size.

use std::path::PathBuf;
use std::sync::Arc;

use gpui::{
    App, AppContext, Context, Entity, IntoElement, Render, Window, WindowOptions, div, prelude::*,
};
use phux_client_runtime::control::{ControlOptions, Event};
use phux_client_runtime::{Client, ClientOptions, Listener, Runtime, Target};
use phux_gpui::terminal::{Settings, Terminal};
use phux_protocol::wire::frame::{AttachTarget, RolePolicy};

/// Wakes the GPUI task below when the runtime has something new.
struct Wake(async_channel::Sender<()>);

impl Listener for Wake {
    fn on_activity(&self) {
        let _ = self.0.try_send(());
    }
}

struct Embed {
    client: Arc<Client>,
    session: String,
    terminal: Option<Entity<Terminal>>,
}

impl Embed {
    /// Drain the runtime's events: open the session's first terminal once it
    /// is attached, and repaint it when its output changes.
    fn drain(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let observation = self.client.take_activity();
        let output = observation.events.iter().any(|event| {
            matches!(
                event,
                Event::TerminalChanged { .. } | Event::InputDelivery { .. }
            )
        });
        if self.terminal.is_none() {
            self.open_first_terminal(window, cx);
        }
        if output && let Some(terminal) = &self.terminal {
            terminal.update(cx, |_, cx| cx.notify());
        }
    }

    fn open_first_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(topology) = self.client.topology() else {
            return;
        };
        let Some(pane) = topology
            .panes
            .iter()
            .find(|pane| pane.session_name == self.session)
        else {
            return;
        };
        let view = match self.client.create_view(&pane.terminal_id) {
            Ok(view) => view,
            Err(error) => {
                eprintln!("terminal {:?} is not ready yet: {error}", pane.terminal_id);
                return;
            }
        };
        eprintln!(
            "showing terminal {:?} of session {:?}",
            pane.terminal_id, self.session
        );
        let settings = Settings {
            focused: true,
            // This view proposes the PTY size from its painted capacity.
            size_owner: true,
            ..Settings::default()
        };
        let (client, id) = (Arc::clone(&self.client), pane.terminal_id.clone());
        self.terminal = Some(cx.new(|cx| Terminal::new(client, view, id, settings, window, cx)));
        cx.notify();
    }
}

impl Render for Embed {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let body = div().size_full();
        match &self.terminal {
            Some(terminal) => body.child(terminal.clone()),
            None => body.child(format!("waiting for session {:?}", self.session)),
        }
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let socket = args
        .next()
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("PHUX_SOCKET").map(PathBuf::from))
        .expect("pass a server socket or run inside a phux pane ($PHUX_SOCKET)");
    let session = args.next().unwrap_or_else(|| "main".to_owned());
    let client = Runtime::connect(
        Target::uds(socket),
        ClientOptions {
            control: ControlOptions {
                client_name: "phux-gpui-embed".to_owned(),
                viewport: (0, 0),
                image_protocols: phux_protocol::caps::ImageProtocolSet::with(&[
                    phux_protocol::caps::ImageProtocol::KittyGraphics,
                ]),
                attach: Some(AttachTarget::ByName(session.clone())),
                attach_role: Some(RolePolicy::PRIMARY),
                ..ControlOptions::default()
            },
            ..ClientOptions::default()
        },
    )
    .expect("connect to the phux server");
    let (tx, rx) = async_channel::bounded(1);
    client.set_listener(Arc::new(Wake(tx)));
    let client = Arc::new(client);

    gpui_platform::application().run(move |cx: &mut App| {
        cx.open_window(WindowOptions::default(), |window, cx| {
            let embed = cx.new(|_| Embed {
                client,
                session,
                terminal: None,
            });
            let handle = embed.downgrade();
            window
                .spawn(cx, async move |cx| {
                    while rx.recv().await.is_ok() {
                        let drained =
                            handle.update_in(cx, |embed, window, cx| embed.drain(window, cx));
                        if drained.is_err() {
                            break;
                        }
                    }
                })
                .detach();
            embed
        })
        .expect("open a window");
        cx.activate(true);
    });
}
