//! The overlay layer: a modal composited over the view.

use super::super::{
    ChromeCtx, RepaintLevel, SidebarReservation, StatusBarPaint, paint_active_overlay,
};

impl super::super::SessionLoop {
    /// Paint the active overlay layer over the current pane composition.
    pub(in crate::attach::driver::loop_state) fn paint_overlay<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
    ) {
        let painted = self.paint_overlay_layer(out, sidebar);
        self.finish_paint(painted);
    }

    /// The paint half of [`Self::paint_overlay`]; committing the onboarding
    /// claim is the caller's.
    pub(in crate::attach::driver::loop_state) fn paint_overlay_layer<
        W: crate::attach::RenderSink,
    >(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
    ) -> StatusBarPaint {
        // The overlay composites over a full frame, which is the hold's paint;
        // what it draws on top is no longer a view an in-place repaint fixes.
        self.paint_hold = None;
        self.painted_view = None;
        // With nothing arranged, the empty state is the frame under a modal.
        let empty = self
            .paint_empty_state(out, sidebar, RepaintLevel::Full)
            .unwrap_or(StatusBarPaint::NotPublished);
        let base = self
            .mirror
            .workspace
            .render_group(self.mirror.zoomed.as_ref());
        let mut chrome = ChromeCtx::new(
            &mut self.settings,
            &mut self.sidebar_painter,
            &self.mirror.session_name,
            self.viewport_dims,
            sidebar,
            &self.mirror.leaves,
        );
        let focus = self.mirror.paint_focus();
        let painted = paint_active_overlay(
            out,
            &self.overlays,
            base.as_deref(),
            &mut self.mirror.panes,
            &self.mirror.engine_kernel,
            focus.as_ref(),
            &mut chrome,
        );
        if base.is_none() { empty } else { painted }
    }

    /// Repaint the overlay stack when the live overlay tagged `key` took the
    /// fresh `items`; a no-op when it is not open.
    pub(in crate::attach::driver::loop_state) fn refresh_live_overlay<
        W: crate::attach::RenderSink,
    >(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
        key: &str,
        items: &[crate::render::overlay::SelectItem],
    ) {
        if self.overlays.refresh_items(key, items) {
            self.paint_overlay(out, sidebar);
        }
    }

    /// Open the directory picker on the listing the latest `go-to-directory`
    /// asked for; a reply to an older request is dropped.
    pub(in crate::attach::driver::loop_state) fn open_directory_picker<
        W: crate::attach::RenderSink,
    >(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
        reply: Option<(u32, phux_protocol::wire::frame::DirectoryListingResult)>,
    ) {
        let Some((request_id, result)) = reply else {
            return;
        };
        let Some(pending) = self
            .pending_directory
            .take_if(|pending| pending.request_id == request_id)
        else {
            tracing::debug!(request_id, "dropping stale DIRECTORY_LISTING");
            return;
        };
        let home = std::env::var("HOME").ok();
        let picker = crate::render::overlay::SelectList::new(
            crate::attach::directory_picker::picker_title(&pending.host),
            crate::attach::directory_picker::picker_items(&result, &pending.host),
            &self.settings.theme,
        )
        .with_footer_status(crate::attach::directory_picker::picker_status(
            &result,
            &pending.host,
            home.as_deref(),
        ));
        // Only over its own placeholder: if the user cancelled it or opened
        // something else on top, the reply is dropped.
        if !self.overlays.replace_pending(request_id, Box::new(picker)) {
            tracing::debug!(request_id, "dropping DIRECTORY_LISTING: placeholder gone");
            return;
        }
        self.paint_overlay(out, sidebar);
    }
}
