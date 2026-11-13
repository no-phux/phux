//! Window-local state across reconnects and machine replacements.
use super::Desk;
use std::time::Instant;

#[cfg(test)]
mod tests;

impl Desk {
    /// Take down what the old connection knew; what the window itself holds
    /// stays, so a background retry doesn't close the sheet you're in.
    pub(super) fn renew(&mut self) {
        let mut next = Desk::open(&self.launch, self.machines.current.clone());
        self.carry_into(&mut next);
        std::mem::swap(&mut next.inbox, &mut self.inbox);
        std::mem::swap(&mut next.activities, &mut self.activities);
        std::mem::swap(&mut next.palette_query, &mut self.palette_query);
        next.pending = self.pending.take();
        next.notice = self.notice.take();
        next.palette_open = self.palette_open;
        next.palette_picked = self.palette_picked;
        // A path search belongs to the old connection.
        next.palette_mode = match self.palette_mode {
            crate::palette::Mode::Paths | crate::palette::Mode::Screens => {
                crate::palette::Mode::All
            }
            mode => mode,
        };
        next.theme_before = self.theme_before;
        next.settings.theme = self.settings.theme;
        next.settings_open = self.settings_open;
        std::mem::swap(&mut next.settings_ui, &mut self.settings_ui);
        next.sidebar_open = self.sidebar_open;
        next.wanted = self
            .shown
            .as_ref()
            .filter(|s| s.view.is_some())
            .map(|shown| (shown.resource.clone(), Instant::now()));
        self.carry_document_focus(&mut next);
        *self = next;
    }

    /// What survives any replacement of this window's connection.
    pub(in crate::desk) fn carry_into(&mut self, next: &mut Desk) {
        self.cancel_quick_move("the main window's connection was replaced");
        std::mem::swap(&mut next.quick, &mut self.quick);
        std::mem::swap(&mut next.quick_chord, &mut self.quick_chord);
        next.native = self.native.take();
        next.phone = self.phone.take();
        if let Some(phone) = &mut next.phone {
            phone.reconcile(None, None);
            phone.picker = false;
            phone.release();
        }
        std::mem::swap(&mut next.updater, &mut self.updater);
        std::mem::swap(&mut next.machines, &mut self.machines);
        std::mem::swap(&mut next.link, &mut self.link);
        std::mem::swap(&mut next.pointer.failed, &mut self.pointer.failed);
        next.generation = self.generation + 1;
        next.clock = self.clock;
        next.focused = self.focused;
        next.waker = self.waker.clone();
        self.resources.carry_images(&mut next.resources);
        next.resources.images = next.resources.images.take().or_else(|| {
            next.waker
                .as_ref()
                .map(|waker| rune::ImageCache::new(waker.clone()))
        });
    }
}
