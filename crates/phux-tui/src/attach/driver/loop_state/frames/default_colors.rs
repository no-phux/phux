//! The host's default colours mid-attach: a theme switch (DEC 2031, or the
//! config reload Omarchy's theme hook rings) re-asks OSC 10/11; a changed
//! answer re-solves the chrome and becomes the panes' OSC 10/11 answer.

use super::super::{AttachError, Connection, FrameKind, SidebarReservation};

/// OSC 10 and OSC 11 queries; the answers arrive on stdin.
const DEFAULT_COLORS_QUERY: &[u8] = b"\x1b]10;?\x1b\\\x1b]11;?\x1b\\";

impl super::super::SessionLoop {
    /// Ask the host for its default colours again.
    pub(in crate::attach::driver::loop_state) fn query_default_colors<
        W: crate::attach::RenderSink,
    >(
        out: &mut W,
    ) -> Result<(), AttachError> {
        out.write_all(DEFAULT_COLORS_QUERY)?;
        out.flush()?;
        Ok(())
    }

    /// After a stdin read: re-ask on a scheme report; adopt a changed answer.
    pub(in crate::attach::driver::loop_state) async fn refresh_default_colors<
        W: crate::attach::RenderSink,
    >(
        &mut self,
        conn: &mut Connection,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
    ) -> Result<(), AttachError> {
        if self.parser.take_color_scheme_change() {
            Self::query_default_colors(out)?;
        }
        let Some(colors) = self.parser.take_default_colors() else {
            return Ok(());
        };
        if !crate::render::theme::record_reported(colors) {
            return Ok(());
        }
        if conn
            .server_features_ext()
            .contains(phux_protocol::ServerFeatureExt::ViewportColors)
        {
            conn.send(&FrameKind::ViewportColors { colors }).await?;
        }
        // The theme solves its roles against the reported background.
        self.reload_config(out, sidebar);
        Ok(())
    }
}
