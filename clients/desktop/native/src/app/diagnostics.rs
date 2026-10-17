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
}
