//! Features the connected server is too old for. Each says so instead of
//! failing, read from the methods `HELLO_OK` advertised, and offers the hot
//! server upgrade when this window manages the local phux: panes survive and
//! the window reconnects on its own.

use phux_client_runtime::handshake::ServerInfo;
use phux_protocol::kinds::method_named;

use super::{Desk, NoticeKind};
use crate::keys::Shortcut;

/// The actions that need a server method, and the feature in words.
const GATED: [(Shortcut, &str, &str); 4] = [
    (Shortcut::LockTab, "ARRANGEMENT_LOCK", "tab locks"),
    (Shortcut::Changes, "CHECKOUT", "changes"),
    (Shortcut::InsertPath, "PATH_QUERY", "path search"),
    (Shortcut::WhatWorksHere, "HOST_CHECKLIST", "What works here"),
];

/// The feature `action` needs when `server` didn't advertise it.
pub fn too_old(server: &ServerInfo, action: Shortcut) -> Option<&'static str> {
    let (_, method, words) = GATED.iter().find(|(gated, ..)| *gated == action)?;
    let advertised = method_named(method)
        .is_some_and(|method| method.advertised(server.features, server.features_ext, server.features_ext2));
    (!advertised).then_some(*words)
}

impl Desk {
    /// The feature `action` needs that the connected server lacks. Nothing
    /// is greyed out before a server has answered.
    pub(super) fn too_old_for(&self, action: Shortcut) -> Option<&'static str> {
        too_old(&self.connection.client.as_ref()?.server()?, action)
    }

    /// Refuse `action` when the server is too old for it, saying why and
    /// how to fix it. True when refused.
    pub(super) fn refuse_too_old(&mut self, action: Shortcut) -> bool {
        let Some(feature) = self.too_old_for(action) else {
            return false;
        };
        let text = format!("The server is too old for {feature}.");
        match &self.machines.current {
            None if self.updater.can_manage_phux() => self.offer(
                format!("{text} Upgrading it keeps every pane."),
                "Upgrade server",
                Self::upgrade_server,
            ),
            // ponytail: no remote upgrade path yet (no `phux update --remote`).
            Some(machine) => {
                let words = format!("{text} Run `phux update` on {machine}.");
                self.say(NoticeKind::Error, words);
            }
            None => self.say(NoticeKind::Error, format!("{text} Run `phux upgrade`.")),
        }
        true
    }

    fn upgrade_server(&mut self) {
        self.updater.upgrade_server();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use phux_client_runtime::Runtime;
    use phux_client_runtime::control::{ControlOptions, ControlPlane};
    use phux_protocol::PROTOCOL_VERSION;
    use phux_protocol::caps::{
        BootstrapLimits, BootstrapProfile, ServerCapabilities, ServerFeatureExt,
        ServerFeatureExtSet,
    };
    use phux_protocol::wire::frame::FrameKind;
    use rune::Tx;

    use super::*;
    use crate::launch::Resolved;

    /// A desk on a server that advertised only `ext`. No process or socket.
    fn desk(ext: &[ServerFeatureExt]) -> (Arc<phux_client_runtime::Client>, Desk) {
        let client = Arc::new(Runtime::embedded(ControlOptions::default()));
        client.with_control(ControlPlane::connection_opened);
        client
            .feed(FrameKind::HelloOk {
                protocol_major: PROTOCOL_VERSION.major,
                protocol_minor: PROTOCOL_VERSION.minor,
                protocol_patch: PROTOCOL_VERSION.patch,
                server_caps: ServerCapabilities::new()
                    .with_features_ext(ServerFeatureExtSet::with(ext)),
                server_id: vec![1; 16],
                selected_profile: Some(BootstrapProfile::SynthesizedVtRaw),
                bootstrap_limits: BootstrapLimits::default(),
            })
            .unwrap();
        let _ = client.take_outbound();
        let mut desk = Desk::with_preferences(
            &Resolved::offline("compat"),
            None,
            rune::prefs::Prefs::memory(),
        );
        desk.connection.client = Some(client.clone());
        (client, desk)
    }

    #[test]
    fn every_gated_method_is_in_the_catalog() {
        for (_, method, _) in GATED {
            assert!(method_named(method).is_some(), "{method}");
        }
    }

    #[test]
    fn a_current_server_greys_nothing_and_an_old_one_names_the_feature() {
        let (_, current) = desk(&[
            ServerFeatureExt::Organization,
            ServerFeatureExt::TabLock,
            ServerFeatureExt::Checkout,
            ServerFeatureExt::PathQuery,
            ServerFeatureExt::HostChecklist,
        ]);
        let (_, old) = desk(&[ServerFeatureExt::Organization]);
        for (action, _, words) in GATED {
            assert_eq!(current.too_old_for(action), None);
            assert_eq!(old.too_old_for(action), Some(words));
        }
        assert_eq!(old.too_old_for(Shortcut::NewTerminal), None);
    }

    #[test]
    fn an_old_local_server_offers_the_upgrade_and_sends_nothing() {
        let (client, mut desk) = desk(&[ServerFeatureExt::Organization]);
        desk.updater.target = Some(("/phux".into(), "/isolated/socket".into()));
        desk.shortcut(Shortcut::LockTab, &mut Tx::default());
        assert!(client.take_outbound().is_empty());
        let notice = desk.notice.as_ref().unwrap();
        assert!(
            notice.text.contains("too old for tab locks"),
            "{}",
            notice.text
        );
        assert_eq!(
            notice.action.map(|(label, _)| label),
            Some("Upgrade server")
        );
    }

    #[test]
    fn an_old_remote_server_says_where_to_update() {
        let (_, mut desk) = desk(&[]);
        desk.updater.target = Some(("/phux".into(), "/isolated/socket".into()));
        desk.machines.current = Some("mini".to_owned());
        desk.shortcut(Shortcut::Changes, &mut Tx::default());
        let notice = desk.notice.as_ref().unwrap();
        assert!(
            notice.text.ends_with("Run `phux update` on mini."),
            "{}",
            notice.text
        );
        assert!(notice.action.is_none());
    }
}
