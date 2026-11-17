/// A socket path that does not exist, so no verb finds (or spawns) a server.
fn dead_socket() -> String {
    format!("/tmp/phux-no-such-server-{}.sock", std::process::id())
}

/// A socket bound then dropped under `dir`: a clean `ECONNREFUSED`.
fn abandoned_socket(dir: &std::path::Path) -> String {
    let sock_file = dir.join("dead.sock");
    drop(std::os::unix::net::UnixListener::bind(&sock_file).expect("bind then abandon socket"));
    assert!(sock_file.exists(), "the abandoned socket file must persist");
    sock_file.to_str().expect("utf-8 temp path").to_owned()
}

/// An `XDG_CONFIG_HOME` under `root` named `name` whose `phux/config.toml`
/// holds `body`.
fn xdg_with_config(root: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
    let xdg = root.join(name);
    std::fs::create_dir_all(xdg.join("phux")).expect("create config dir");
    std::fs::write(xdg.join("phux").join("config.toml"), body).expect("write config");
    xdg
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
