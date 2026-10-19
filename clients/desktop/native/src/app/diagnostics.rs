//! `Copy Diagnostics`: a redacted identity receipt on the clipboard.
//!
//! The receipt is assembled from typed connection and updater state only
//! (see [`crate::diag::Receipt`]); it never reads a terminal, a title or the
//! clipboard, so terminal content cannot reach a support thread through it.

use gpui::{ClipboardItem, Context};

use super::desk::Desk;
use super::notifications::Kind;
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
