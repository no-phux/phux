/// A socket path that does not exist, so no verb finds (or spawns) a server.
fn dead_socket() -> String {
    format!("/tmp/phux-no-such-server-{}.sock", std::process::id())
}

/// A nonexistent `$PHUX_TAILSCALE` CLI: turns overlay detection (and the
/// CGNAT heuristic) off, and a failed `execve` cannot be slow.
const NO_OVERLAY_CLI: &str = "/nonexistent/phux-output-hygiene-no-overlay";

/// The binary under test, with overlay detection off.
fn phux() -> Command {
    let mut cmd = crate::common::phux_cmd(crate::runner::phux_bin());
    cmd.env("PHUX_TAILSCALE", NO_OVERLAY_CLI);
    cmd
}

/// Run `phux <args...>` and return `(exit_code, stdout, stderr)`.
fn run(args: &[&str]) -> (i32, String, String) {
    Out::of(phux().args(args)).into_parts()
}

fn run_with_xdg(args: &[&str], xdg_config_home: &std::path::Path) -> (i32, String, String) {
    Out::of(phux().env("XDG_CONFIG_HOME", xdg_config_home).args(args)).into_parts()
}

/// The build banner; no one-shot verb may print it.
const BANNER_FRAGMENT: &str = concat!("phux ", env!("CARGO_PKG_VERSION"));
