//! Discover the port a spawned server's remote listener actually bound.
//!
//! Tests start servers on port 0 and read the bound address back from the
//! server's `GET_STATE` listener report (the one `phux doctor` and `phux pair`
//! read), instead of reserving a port, releasing it, and hoping nothing else
//! takes it before the server binds.

#![allow(
    dead_code,
    reason = "shared integration-test helpers are used per test binary"
)]
#![allow(unreachable_pub, reason = "shared by sibling integration-test crates")]
#![allow(clippy::expect_used, clippy::panic, reason = "test harness")]

use std::net::SocketAddr;
use std::path::Path;
use std::time::{Duration, Instant};

pub use phux_protocol::wire::RemoteListenerTransport;

/// How long a freshly spawned server has to answer `GET_STATE`.
const DEADLINE: Duration = Duration::from_secs(30);

/// The loopback bind spec that lets the kernel pick the port.
pub const LOOPBACK_ANY_PORT: &str = "127.0.0.1:0";

/// The address `transport`'s listener bound on the server at `socket`,
/// retrying until the server answers. Panics when it never answers, or when
/// that listener was not configured or did not bind.
pub fn bound_listener_addr(socket: &Path, transport: RemoteListenerTransport) -> SocketAddr {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let start = Instant::now();
    let view = loop {
        match runtime.block_on(phux_client::state::get_state(socket)) {
            Ok(view) => break view,
            Err(err) => assert!(
                start.elapsed() < DEADLINE,
                "the server at {} never answered GET_STATE: {err}",
                socket.display()
            ),
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let report = view
        .snapshot()
        .listeners()
        .unwrap_or_else(|| panic!("the server reported no remote listeners"));
    let slot = report
        .listeners
        .iter()
        .find(|slot| slot.transport == transport)
        .unwrap_or_else(|| panic!("no {transport} listener in {report:?}"));
    assert!(
        slot.bound,
        "the {transport} listener did not bind: {slot:?}"
    );
    slot.addr
        .as_deref()
        .and_then(|addr| addr.parse().ok())
        .unwrap_or_else(|| panic!("the {transport} listener reported no address: {slot:?}"))
}
