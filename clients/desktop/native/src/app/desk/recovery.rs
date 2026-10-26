//! Local first-launch recovery and identity-safe reconnects.

use super::*;

impl Desk {
    /// Resolve the existing home before redialing; never recreate a stale name.
    pub(super) fn prepare_reconnect_home(&mut self, cx: &mut Context<Self>) -> bool {
        if self.session_is_attached() {
            return true;
        }
        let key = self.link_home_key().or(self.home_key);
        let needs_target = self.socket.as_os_str().is_empty();
        let result = if let Some(remote) = self.remote.as_deref() {
            crate::connection::home::existing_remote_home(remote, &self.session, key)
        } else {
            host::start_server(&self.phux, &self.socket).and_then(|socket| {
                let name = if self.session.is_empty() || needs_target {
                    let requested = (!self.session.is_empty()).then_some(self.session.as_str());
                    crate::connection::home::open_home(&self.phux, &socket, requested)
                } else {
                    crate::connection::home::existing_home(&socket, &self.session, key)
                }?;
                // Adopt the target only once its home is known; a failed probe stays retryable.
                self.socket = socket;
                Ok(name)
            })
        };
        match result {
            // A launch that could not name a session reopens on the one
            // the server now offers, with that session's saved layout.
            Ok(name) if self.session.is_empty() || needs_target => {
                let old = cx.entity_id();
                let phux = self.phux.clone();
                let socket = self.socket.clone();
                self.defer_desktop(cx, move |desktop, cx| {
                    desktop.rehome(old, name, key, phux, socket, cx)
                });
                false
            }
            Ok(name) => {
                self.session = name.clone();
                self.workspace_call(|link| link.workspace.set_home_session(name));
                true
            }
            Err(error) => {
                self.connect_error = Some(error.clone());
                self.post(Kind::Error, "Could not reconnect", Some(error));
                cx.notify();
                false
            }
        }
    }

    pub(crate) fn reconnect(&mut self, cx: &mut Context<Self>) {
        if !self.prepare_reconnect_home(cx) {
            return;
        }
        self.redial(cx);
    }

    pub(super) fn runtime_reconnect(&mut self, socket: PathBuf, cx: &mut Context<Self>) {
        if self.remote.is_none() && self.socket.as_os_str().is_empty() {
            let key = self.home_key;
            let requested = (!self.session.is_empty()).then_some(self.session.as_str());
            match crate::connection::home::open_home(&self.phux, &socket, requested) {
                Ok(name) => {
                    self.socket = socket;
                    let old = cx.entity_id();
                    let phux = self.phux.clone();
                    let socket = self.socket.clone();
                    self.defer_desktop(cx, move |desktop, cx| {
                        desktop.rehome(old, name, key, phux, socket, cx)
                    });
                }
                Err(error) => {
                    self.connect_error = Some(error.clone());
                    self.post(Kind::Error, "Could not reconnect", Some(error));
                    cx.notify();
                }
            }
            return;
        }
        self.redial(cx);
    }
}
