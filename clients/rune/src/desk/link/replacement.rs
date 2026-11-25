//! Replace a runtime namespace, not the window that happens to display it.
use super::Desk;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::desk) enum Replacement {
    Retry,
    Machine,
}

#[cfg(test)]
mod tests;

impl Desk {
    /// An ended client is replaced; a transport reconnect on the same client
    /// keeps its replay-safe receipt namespace and never comes through here.
    pub(super) fn renew(&mut self) {
        self.replace_connection(Replacement::Retry);
    }

    pub(in crate::desk) fn replace_connection(&mut self, policy: Replacement) {
        let returning = self.connection.resources.returning.or_else(|| {
            self.connection
                .shown
                .as_ref()
                .and_then(|shown| self.connection.resources.entries.get(&shown.resource))
                .map(|document| document.record.key)
        });
        // Releases must target the old views/client, never their replacements.
        self.phone_interrupted();
        self.release_capture();
        self.end_drag();
        self.cancel_quick_move("the main window's connection was replaced");
        self.release_connection_views();
        let next = self.connection.successor();
        let mut old = std::mem::replace(&mut self.connection, next);
        old.resources.carry_images(&mut self.connection.resources);
        if policy == Replacement::Retry {
            self.connection.resources.returning = returning;
            self.connection.resources.dismissed = old.resources.dismissed;
            self.retire_answers();
        } else {
            self.leave_machine_ui();
        }
        self.reset_connection_interactions();
        self.generation += 1;
    }

    fn reset_connection_interactions(&mut self) {
        if let Some(panel) = self.quick.as_mut() {
            panel.forget_move();
        }
        if let Some(phone) = &mut self.phone {
            phone.reconcile(None, None);
            phone.picker = false;
            phone.page = None;
            phone.release();
        }
        if let Some(native) = &mut self.native {
            native.paste_requested = false;
        }
        // The opener's asynchronous error channel belongs to the window.
        let failed = self.pointer.failed.clone();
        self.pointer = super::super::links::Pointer::default();
        self.pointer.failed = failed;
        if matches!(
            self.palette_mode,
            crate::palette::Mode::Paths | crate::palette::Mode::Screens
        ) {
            self.palette_mode = crate::palette::Mode::All;
        }
        // Selected scenes can contain handles from the retired namespace.
        self.playground = None;
        self.galaxy = Default::default();
        self.reef = Default::default();
        self.pitch = Default::default();
        self.kitchen = Default::default();
        self.pitlane = Default::default();
        self.hive = Default::default();
        self.fleet = Default::default();
        // Migration panes and outstanding CLI results cannot be inherited by
        // a new runtime namespace. Saved onboarding progress is unaffected.
        self.onboarding = Default::default();
    }

    fn leave_machine_ui(&mut self) {
        self.left_server();
        self.inbox = Default::default();
        self.activities = Default::default();
        self.close_palette();
        self.palette_query.clear();
        self.palette_picked = 0;
        self.palette_mode = crate::palette::Mode::All;
        self.close_settings();
        self.settings_ui = Default::default();
        self.clear_notices();
    }
}
