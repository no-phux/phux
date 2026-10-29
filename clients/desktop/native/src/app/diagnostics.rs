//! `Copy Diagnostics`: a redacted identity receipt on the clipboard.
//!
//! The receipt is assembled from typed connection and updater state only
//! (see [`crate::diag::Receipt`]); it never reads a terminal, a title or the
//! clipboard, so terminal content cannot reach a support thread through it.

use gpui::{ClipboardItem, Context};

use super::desk::Desk;
use super::notifications::Kind;
use crate::devtools::SocketState;
use crate::diag::{Receipt, development_version, recent_errors, redact};

impl Desk {
    /// Build this window's receipt.
    pub(crate) fn diagnostic_receipt(&self, cx: &Context<Self>) -> Receipt {
        let updater = self.updates.read(cx);
        let installed = updater.current.as_ref();
        let mut receipt = Receipt {
            app_version: installed
                .map_or_else(development_version, |release| release.version.clone()),
            app_revision: installed.map(|release| release.revision.clone()),
            app_channel: installed.map(|release| release.channel.as_str().to_owned()),
            app_protocol: crate::updates::own_protocol(),
            runtime_version: updater
                .runtime
                .as_ref()
                .map(|runtime| runtime.current.clone()),
            host: self.remote.clone().unwrap_or_else(|| "local".to_owned()),
            recent_errors: recent_errors(),
            ..Receipt::default()
        };
        if let Some(link) = &self.link {
            let connection = &link.connection;
            receipt.status = Some(format!("{:?}", connection.status));
            receipt.server_id = connection.server_id();
            receipt.connection_epoch = Some(connection.connection_epoch());
            receipt.server_protocol = connection.server.as_ref().map(|server| {
                let (major, minor, patch) = server.protocol;
                format!("{major}.{minor}.{patch}")
            });
            receipt.connection_error = connection.error.as_deref().map(redact);
        }
        receipt
    }

    /// Put the receipt on the clipboard and say so.
    pub(crate) fn copy_diagnostics(&mut self, cx: &mut Context<Self>) {
        let text = self.diagnostic_receipt(cx).to_clipboard();
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.post(
            Kind::Info,
            "Diagnostics copied",
            Some(
                "Versions, server id, connection epoch, host and recent errors. No terminal content."
                    .into(),
            ),
        );
    }

    /// Save Developer mode and apply it to every window now. A setting that
    /// could not be saved still applies to this run, and says so.
    pub(crate) fn set_developer_mode(&mut self, on: bool, cx: &mut Context<Self>) {
        if let Err(error) = crate::devtools::set_dev_mode(on, cx) {
            self.post(
                Kind::Error,
                "Developer mode was not saved",
                Some(format!(
                    "It applies until you quit. {}",
                    redact(&format!("{error:#}"))
                )),
            );
        }
        cx.notify();
    }

    /// `Toggle Developer Mode`, with a notice of where things stand.
    pub(crate) fn toggle_developer_mode(&mut self, cx: &mut Context<Self>) {
        let on = !crate::devtools::dev_mode();
        self.set_developer_mode(on, cx);
        let (title, body) = developer_mode_notice(on, crate::devtools::dev_mode());
        self.post(Kind::Info, title, Some(body));
    }

    /// `Toggle Performance HUD`.
    pub(crate) fn toggle_hud(&mut self, cx: &mut Context<Self>) {
        crate::devtools::set_hud(!crate::devtools::hud_visible());
        cx.notify();
    }

    /// `Capture Performance Trace`: five seconds of desktop and server spans
    /// to one Perfetto file, its path on the clipboard.
    pub(crate) fn capture_trace(&mut self, cx: &mut Context<Self>) {
        self.post(
            Kind::Info,
            "Recording a 5-second trace",
            Some("Desktop and server spans, memory and frame rate.".into()),
        );
        cx.spawn(async move |desk, cx| {
            let result = smol::unblock(|| {
                crate::devtools::capture_trace(std::time::Duration::from_secs(5), true)
            })
            .await;
            let _ = desk.update(cx, |desk, cx| {
                match result {
                    Ok(trace) => {
                        let path = trace.path.display().to_string();
                        cx.write_to_clipboard(ClipboardItem::new_string(path));
                        let server = trace.server_error.map_or_else(String::new, |error| {
                            format!(" The server did not take part: {error}")
                        });
                        desk.post(
                            Kind::Info,
                            "Trace saved; path copied",
                            Some(format!(
                                "{} · {} events. Open it in ui.perfetto.dev.{server}",
                                crate::devtools::display_path(&trace.path),
                                trace.events,
                            )),
                        );
                    }
                    Err(error) => desk.post(
                        Kind::Error,
                        "Trace failed",
                        Some(redact(&format!("{error:#}"))),
                    ),
                }
                cx.notify();
            });
        })
        .detach();
    }
}

/// What `Toggle Developer Mode` says: what was asked, and what applies now.
fn developer_mode_notice(asked: bool, now: bool) -> (&'static str, String) {
    match (asked, now) {
        (false, true) => (
            "Developer mode stays on",
            "PHUX_DEV or --dev holds it on until you quit. It is saved off for the next launch."
                .to_owned(),
        ),
        (_, false) => (
            "Developer mode off",
            "The HUD, allocation counting and the dev socket are off.".to_owned(),
        ),
        (true, true) => ("Developer mode on", developer_mode_on_body()),
    }
}

fn developer_mode_on_body() -> String {
    let socket = match crate::devtools::socket_state() {
        SocketState::Listening(path) => {
            format!("the dev socket at {}", crate::devtools::display_path(&path))
        }
        SocketState::Failed(error) => format!("no dev socket ({})", redact(&error)),
        SocketState::Off => "no dev socket yet".to_owned(),
    };
    format!("Performance HUD, allocation counting and {socket}. Settings > Developer has the rest.")
}
