//! Transfer host runtimes between passive inventory and the visible workspace.

use super::*;

impl Desk {
    pub(super) fn fleet_wake(
        &self,
        connection: &Connection,
        source: Option<String>,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        let rx = connection.activity();
        cx.spawn(async move |desk, cx| {
            while rx.recv().await.is_ok() {
                if desk
                    .update(cx, |desk, cx| {
                        if desk.fleet.drain(&source) {
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
    }

    /// Keep one working runtime and its last workspace per host, without GPUI views.
    pub(in crate::app) fn park_link(&mut self, cx: &mut Context<Self>) {
        self._wake = None;
        let link = self.link.take();
        self.clear_projection(true);
        let Some(link) = link else { return };
        let last = if available(&link.connection) {
            Vec::new()
        } else {
            work_on(&link.connection, self.remote.clone())
        };
        let wake = self.fleet_wake(&link.connection, self.remote.clone(), cx);
        let mut observer = Observer {
            connection: Some(link.connection),
            workspace: Some(link.workspace),
            last,
            error: None,
            _wake: Some(wake),
        };
        observer.refresh_inventory(&self.remote);
        self.fleet.observers.insert(self.remote.clone(), observer);
    }

    /// Transfer the sole activity drain from inventory back to the visible desk.
    pub(in crate::app) fn take_warm_link(
        &mut self,
    ) -> Option<(Connection, Option<crate::workspace::Workspace>)> {
        self.fleet.drain(&self.remote);
        let observer = self.fleet.observers.get_mut(&self.remote)?;
        observer._wake = None;
        let connection = observer.connection.take()?;
        observer.error = None;
        Some((connection, observer.workspace.take()))
    }
}
