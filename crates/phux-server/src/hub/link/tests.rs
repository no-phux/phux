use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use super::*;
use phux_protocol::Version;

mod newer_satellite;
mod planning;
mod supervisor;
mod transports;
mod write_bounds;

use planning::entry;
use supervisor::{host, relay_pair, wait_for_status};
use write_bounds::{ScriptConn, ScriptTransport};

/// A satellite on any minor or patch of the hub's major is linked; only
/// a different major is refused.
#[test]
fn link_admits_any_minor_and_patch_of_the_hubs_major() {
    let offered = hub_link_capabilities();
    let admits = |major: u16, minor: u16, patch: u16| {
        validate_link_hello_ok(
            &offered,
            major,
            minor,
            patch,
            BootstrapProfile::SynthesizedVtRaw,
            offered.bootstrap.limits,
        )
        .is_ok()
    };
    let Version {
        major,
        minor,
        patch,
    } = PROTOCOL_VERSION;
    assert!(admits(major, minor, patch));
    assert!(admits(major, minor + 7, patch + 3), "a newer minor links");
    assert!(admits(major, minor, patch + 1), "a newer patch links");
    assert!(!admits(major + 1, minor, patch), "a newer major is refused");
    assert!(
        !admits(major - 1, minor, patch),
        "an older major is refused"
    );
}

/// Revoking the link's token on the satellite ends the link live and
/// refuses the redial (`docs/remote.md#managing-credentials`).
#[tokio::test(flavor = "current_thread")]
async fn revoking_the_links_token_on_the_satellite_drops_the_link() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let tokens = dir.path().join("satellite-tokens");
            let secret = [0x5a; crate::auth::TOKEN_LEN];
            crate::auth::write_test_credential(&tokens, &secret);
            let store = std::sync::Arc::new(
                crate::auth::ReloadingTokenStore::load(tokens.clone()).expect("store"),
            );
            let listener = crate::transport::WsListener::loopback_with_tokens(store)
                .await
                .expect("bind satellite");
            let port = listener.local_addr().expect("addr").port();
            let satellite = crate::state::SharedState::new();
            // The bearer's revocation is under test, not the grant: an
            // embedder engine admits the link (ADR-0072).
            satellite.with_mut(|s| {
                s.set_policy_engine(std::sync::Arc::new(crate::policy::OwnerForAll));
            });
            let satellite_root = CancellationToken::new();
            let accept_state = satellite.clone();
            let accept_root = satellite_root.clone();
            tokio::task::spawn_local(async move {
                crate::runtime::connections::accept_loop(&listener, accept_state, accept_root, None)
                    .await
            });
            crate::runtime::revocation::spawn_revocation_watcher(&satellite, &satellite_root);

            let token_file = dir.path().join("link.token");
            std::fs::write(&token_file, hex::encode(secret)).expect("write link token");
            let statuses = HubLinkStatuses::default();
            let cancel = CancellationToken::new();
            let host = host();
            let (_relay, relay_rx) = relay_pair(&host);
            tokio::task::spawn_local(run_link(
                host.clone(),
                entry(
                    &format!("ws://127.0.0.1:{port}"),
                    Some(token_file.to_str().expect("utf8 path")),
                    None,
                ),
                NetLinkTransport::new("ssh".into(), crate::hub::adopt::SatelliteAdopter::default()),
                statuses.clone(),
                relay_rx,
                cancel.child_token(),
            ));
            wait_for_status(&statuses, &host, |s| *s == LinkStatus::Connected).await;

            crate::auth::revoke_credential(&tokens, "test-credential").expect("revoke");
            wait_for_status(&statuses, &host, |s| {
                matches!(s, LinkStatus::Backoff { .. })
            })
            .await;
            // The redial presents the revoked token and is refused.
            tokio::time::sleep(Duration::from_millis(1500)).await;
            assert_ne!(
                statuses.get(&host),
                Some(LinkStatus::Connected),
                "a revoked link token must not reconnect"
            );

            cancel.cancel();
            satellite_root.cancel();
        })
        .await;
}

// --- dial-target selection ----------------------------------------

// --- fail-closed matrix ---------------------------------------------

// --- ssh-stdio planning and argv (phux-v45.9) ------------------------

// --- token file -----------------------------------------------------

// --- stderr tail ------------------------------------------------------

// --- backoff ----------------------------------------------------------

// --- supervisor (scripted transport, no network) ---------------------

// --- loopback integration: the real transport over a real socket -----

// --- ssh-stdio through the real transport against a stub `$PHUX_SSH`,
// which records its argv; its exit is the drop signal. The bridge half
// is covered by `crates/phux/tests/fleet/stdio_bridge_e2e.rs`.
