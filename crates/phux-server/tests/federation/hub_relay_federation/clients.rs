//! Hub clients that connect the way real clients do.

use super::*;
use phux_server_testkit::wait_for_socket_with;

/// A hub client offering what real clients offer: the linked engine's
/// native profile with the synthesized fallbacks. The hub link offers the
/// hub's own profiles, so this client selects the profile the link carries.
pub(super) async fn hub_client(path: &std::path::Path) -> UnixStream {
    wait_for_socket_with(path, STEP_DEADLINE, |caps| {
        caps.with_bootstrap(
            phux_client_core::engine::ghostty::native_bootstrap_capabilities(
                phux_protocol::caps::BootstrapLimits::default(),
            ),
        )
    })
    .await
}
