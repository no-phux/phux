//! Binary-level end-to-end regression for phux-87rr: "service-managed
//! panes inherit a login-shell PATH; ordinary panes are unchanged."
//!
//! The bug: a server installed with `phux service install` runs under
//! launchd/systemd's minimal environment. Its spawned panes ran the
//! configured shell as a plain (non-login) shell, so platform profile
//! initialization (`~/.profile`, `~/.zprofile`, …) never ran — Homebrew
//! and Nix PATH entries were invisible even though environment markers
//! like `NIX_PROFILES` could still be inherited and fool a naive guard
//! into thinking initialization already happened.
//!
//! The fix threads a `login: bool` through
//! `phux_server::terminal_actor::{default_shell_command, shell_command}`
//! (see `crates/phux-server/src/terminal_actor/spawn.rs`), driven by
//! whether `phux server` finds `PHUX_SERVICE_MANAGED` in its own
//! environment — the marker `phux service install` stamps into the
//! generated launchd plist / systemd unit
//! (`crates/phux/src/commands/service.rs::SERVICE_MANAGED_ENV`), never a
//! heuristic sniffed from environment shape.
//!
//! This file proves the whole pipeline against the REAL compiled `phux`
//! binary — real env-var detection in `run_server`, real
//! `shell_command`/`-l` argv construction, a real PTY, a real `/bin/sh`
//! sourcing a real `~/.profile` fixture, and a real command-resolution
//! outcome written to a file by the spawned shell itself. The only thing
//! NOT real is launchd/systemd itself: the "minimal service environment"
//! is simulated by starting the child process with `env_clear()` plus an
//! explicit launchd-shaped minimal `PATH`, and the marker is set directly
//! rather than by an installed unit — a real launchd/systemd test is not
//! runnable in this CI. Everything downstream of "the child process's
//! environment looks like this" is exercised for real.
//!
//! Two scenarios:
//!   * [`service_managed_pane_resolves_a_profile_provided_command`] —
//!     acceptance criterion 5. The marker is present; the profile-added
//!     command must resolve.
//!   * [`ordinary_pane_does_not_source_the_profile_twice`] — acceptance
//!     criterion 6. The marker is absent (an ordinary hand-started
//!     server); the same profile fixture must NOT be sourced, so the
//!     same command must NOT resolve. This is the regression guard: it
//!     would fail immediately if login-shell treatment ever stopped
//!     being conditional.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

#[path = "../common/mod.rs"]
mod common;

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use tempfile::TempDir;

/// Path to the freshly-built `phux` binary, injected by cargo.
const PHUX: &str = env!("CARGO_BIN_EXE_phux");

/// The marker `phux service install` writes into the generated unit's
/// environment (`crates/phux/src/commands/service.rs::SERVICE_MANAGED_ENV`).
/// Duplicated here as a literal rather than imported: this file spawns the
/// compiled binary as a black-box subprocess (the established pattern in
/// this directory, e.g. `idle_exit_e2e.rs`), so it has no dependency on
/// `phux`'s internal `pub(crate)` items — matching how every other
/// service-unit env var name (`PHUX_WS_ADDR`, `PHUX_SOCKET`, …) is already
/// duplicated as a literal across this codebase rather than shared via a
/// cross-module constant.
const SERVICE_MANAGED_ENV: &str = "PHUX_SERVICE_MANAGED";

/// launchd's own default `PATH` for an agent with no `EnvironmentVariables`
/// override (`man launchd.plist`), used here to build a realistically
/// minimal — not artificially empty — service environment.
const LAUNCHD_DEFAULT_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

/// Wait for the server to bind (cold-start bound, matching `idle_exit_e2e.rs`).
const SOCKET_DEADLINE: Duration = Duration::from_secs(30);

/// Wait for the seed pane to run its command and write the result file.
///
/// The command itself resolves in milliseconds; the margin is generous
/// (matching `idle_exit_e2e.rs`'s `EXIT_HANG_CEILING` philosophy) because
/// under `just e2e`'s full parallel run this competes with every other
/// PTY-spawning, real-subprocess integration test on the box for CPU and
/// fork/exec bandwidth. This is a HANG detector, not a timing gate.
const RESULT_DEADLINE: Duration = Duration::from_secs(20);

/// Poll cadence for every wait loop in this file.
const POLL: Duration = Duration::from_millis(50);

/// Monotonic counter so concurrent tests never collide on a socket path.
static COUNTER: AtomicU32 = AtomicU32::new(0);

/// A running `phux server` child plus its private socket.
///
/// Sockets live at the root of `/tmp` (matching `idle_exit_e2e.rs` /
/// `rec_e2e.rs`): macOS caps `sun_path` at 104 bytes and this crate runs
/// from deep worktree paths that can already exceed it before adding a
/// filename.
struct ServerGuard {
    _process: common::ServerProcess,
    socket: PathBuf,
}

impl ServerGuard {
    /// Start `phux server` with a from-scratch environment: `env_clear()`
    /// then exactly `HOME` (the caller's fixture directory) and `PATH`
    /// (launchd's own default) — no inherited `$SHELL`, no inherited
    /// Homebrew/Nix PATH entries from whatever shell is running this test
    /// suite (relevant since `just ci` and this repo's dev shell both run
    /// under `nix develop`, which is exactly the transient-PATH shape
    /// phux-87rr's acceptance criterion 4 is about — see
    /// `crates/phux/src/commands/service.rs`'s
    /// `install_never_captures_the_process_path` test for that half).
    ///
    /// `service_managed` stamps [`SERVICE_MANAGED_ENV`] when `true`,
    /// mirroring exactly what `phux service install` writes into the
    /// generated unit; absent, this is indistinguishable from a server a
    /// human started directly from their own terminal.
    fn start(home: &Path, seed_command: &str, service_managed: bool) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let socket = PathBuf::from(format!(
            "/tmp/phux-login-e2e-{}-{n}.sock",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&socket);

        let mut cmd = Command::new(PHUX);
        cmd.env_clear();
        cmd.env("HOME", home);
        cmd.env("PATH", LAUNCHD_DEFAULT_PATH);
        if service_managed {
            cmd.env(SERVICE_MANAGED_ENV, "1");
        }
        cmd.args(["server", "--session", "svc", "--socket"])
            .arg(&socket)
            .arg("--seed-command")
            .arg(seed_command)
            .args(["--exit-after-idle", common::SERVER_IDLE_LIMIT_SECS]);
        let child = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn phux server");

        let guard = Self {
            _process: common::ServerProcess::from_child(child, socket.clone()),
            socket,
        };
        let deadline = Instant::now() + SOCKET_DEADLINE;
        while Instant::now() < deadline {
            if guard.socket.exists() {
                return guard;
            }
            std::thread::sleep(POLL);
        }
        panic!(
            "phux server did not bind {} within {SOCKET_DEADLINE:?}",
            guard.socket.display()
        );
    }
}

/// Build a fixture `$HOME` whose `~/.profile` prepends `$HOME/bin` to
/// `PATH` and drops an executable marker script there — the minimal
/// stand-in for what a Homebrew or Nix installer's profile snippet does.
/// Read only by a shell invoked in *login* mode; a plain `sh -c` never
/// sources it, which is exactly the behavior under test.
fn write_profile_fixture() -> TempDir {
    let home = TempDir::new().expect("tempdir");
    let bin = home.path().join("bin");
    std::fs::create_dir_all(&bin).expect("mkdir bin");

    let marker = bin.join("phux-profile-marker");
    let mut f = std::fs::File::create(&marker).expect("create marker");
    writeln!(f, "#!/bin/sh\necho PHUX_PROFILE_MARKER_FOUND").expect("write marker");
    drop(f);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&marker, std::fs::Permissions::from_mode(0o755))
            .expect("chmod marker");
    }

    let profile = home.path().join(".profile");
    std::fs::write(&profile, "PATH=\"$HOME/bin:$PATH\"\nexport PATH\n").expect("write .profile");

    home
}

/// Last line the seed shell writes, and the only safe signal that the record
/// is complete. See [`read_result_file`].
const RESULT_TERMINATOR: &str = "PHUX_RESULT_END";

/// The seed command every case in this file runs: report `PATH`, try the
/// `~/.profile`-provided command, then mark the record finished.
///
/// The terminator is a separate `printf` rather than part of the marker's own
/// output because the marker is exactly the thing that may not resolve — when
/// it does not, the shell's "not found" goes to the same file via `2>&1` and
/// execution continues, so the terminator still lands. That is what makes it a
/// completeness signal rather than a second thing to race on.
fn seed_command(result_path: &Path) -> String {
    format!(
        "{{ printf '%s\\n' \"$PATH\"; phux-profile-marker; printf '%s\\n' \
         '{RESULT_TERMINATOR}'; }} >'{}' 2>&1",
        result_path.display()
    )
}

/// Poll `path` until the seed shell's record is **complete**, or panic at
/// `deadline`.
///
/// Completeness is the terminator line, not a non-empty file. The seed writes
/// its record with several sequential commands sharing one redirection
/// (`{ printf; marker; printf; } >file`); the redirection truncates once, but
/// the writes land at different times, so a read taken between them returns a
/// prefix. The earlier version of this returned on the first non-empty read
/// and therefore raced: it usually caught the whole record, and occasionally
/// caught only the `PATH` line, which then failed the marker assertion and
/// looked exactly like a real login-shell regression. It was a flake on every
/// lane that ran this suite before anyone read the seed closely enough to
/// notice that "one shot" described the truncation, not the writes.
fn read_result_file(path: &Path, deadline: Duration) -> String {
    let end = Instant::now() + deadline;
    let mut last = String::new();
    while Instant::now() < end {
        if let Ok(contents) = std::fs::read_to_string(path) {
            if contents.contains(RESULT_TERMINATOR) {
                return contents;
            }
            last = contents;
        }
        std::thread::sleep(POLL);
    }
    panic!(
        "seed pane never wrote a complete result to {} within {deadline:?} \
         (waiting for the {RESULT_TERMINATOR} line); got so far: {last:?}",
        path.display()
    );
}

/// phux-87rr acceptance criterion 5: an end-to-end regression that starts
/// the service under a minimal environment and proves the seed pane got
/// login-shell treatment.
///
/// The assertion is "the pane's `PATH` is the one a login `/bin/sh` produces
/// on this host, and not the one a plain `/bin/sh` produces" — measured
/// against this host rather than assumed. The older form asserted a fixed
/// outcome instead (a `~/.profile`-provided command must resolve), which
/// silently encoded an assumption about the host: that nothing between
/// `/etc/profile` and `$HOME/.profile` interferes with the fixture.
///
/// That assumption broke on a CI image whose `/etc/profile.d` exports
/// `HOME=/home/runner`. `/etc/profile` runs BEFORE `$HOME/.profile`, so dash
/// then expanded `$HOME/.profile` to the runner's profile and never read the
/// fixture's — producing a failure that looked exactly like phux forgetting
/// the login flag, and cost a five-run CI bisect to tell apart. No behaviour
/// of phux's can survive that, and no assertion phrased as a fixed outcome
/// can distinguish it from a real regression.
///
/// Comparing against the host's own login shell keeps the full end-to-end
/// path (real binary, real env detection, real `-l` argv, real PTY, real
/// shell) while testing only the part phux owns. Where the host does let the
/// fixture through, the stronger original assertion still runs — see
/// [`fixture_profile_is_reachable`].
#[test]
fn service_managed_pane_resolves_a_profile_provided_command() {
    let home = write_profile_fixture();
    let result_path = home.path().join("result");
    let seed = seed_command(&result_path);

    let _server = ServerGuard::start(home.path(), &seed, true);

    let contents = read_result_file(&result_path, RESULT_DEADLINE);
    let pane_path = contents.lines().next().unwrap_or_default().to_owned();

    let login = probe_shell_path(home.path(), true);
    let plain = probe_shell_path(home.path(), false);
    let diagnostics = diagnostics(home.path(), &login, &plain);

    assert_ne!(
        pane_path, plain,
        "a service-managed server's seed pane must NOT get a plain shell's \
         PATH — login-shell treatment was not applied.\n{diagnostics}"
    );
    assert_eq!(
        pane_path, login,
        "a service-managed server's seed pane must get the same PATH a login \
         `/bin/sh` produces on this host.\n{diagnostics}"
    );

    // Where the host lets the fixture's `~/.profile` through, hold the
    // stronger line too: the profile-provided command must actually resolve.
    if fixture_profile_is_reachable(home.path(), &login) {
        assert!(
            contents.contains("PHUX_PROFILE_MARKER_FOUND"),
            "the host's login shell does source the fixture `~/.profile` (its \
             bin directory is on the login PATH), so the profile-provided \
             command must resolve in the pane; got: {contents:?}\n{diagnostics}"
        );
    }
}

/// Whether this host's login shell actually reached the fixture's
/// `~/.profile` — true when the login `PATH` contains the fixture's `bin`.
///
/// False on a host whose `/etc/profile` interferes with `$HOME` before dash
/// expands `$HOME/.profile`; there the fixture cannot be observed at all and
/// the marker assertion would be testing the image, not phux.
fn fixture_profile_is_reachable(home: &Path, login_probe: &str) -> bool {
    login_probe.contains(&home.join("bin").display().to_string())
}

/// The host facts that decide who owns a failure here, rendered once.
fn diagnostics(home: &Path, login: &str, plain: &str) -> String {
    format!(
        "\nHost diagnostics:\n\
         \x20 fixture HOME      : {home_dir}\n\
         \x20 ~/.profile exists : {profile_exists}\n\
         \x20 marker executable : {marker_exec}\n\
         \x20 /bin/sh resolves  : {sh_target}\n\
         \x20 sh -l -c $HOME    : {login_home}\n\
         \x20 sh -l -c $PATH    : {login}\n\
         \x20 sh -c    $PATH    : {plain}\n\
         \x20 /etc/profile.d    : {profile_d}\n\
         \n\
         If the two probes above are identical, this host's `/bin/sh` gives a \
         login shell nothing extra and the comparison cannot detect the flag \
         at all. If `sh -l -c` reports a HOME other than the fixture, the \
         host's /etc/profile is resetting it and the fixture is unreachable \
         by construction.",
        home_dir = home.display(),
        profile_exists = home.join(".profile").exists(),
        marker_exec = home.join("bin/phux-profile-marker").exists(),
        sh_target = std::fs::read_link("/bin/sh").map_or_else(
            |_| "(not a symlink)".to_owned(),
            |p| p.display().to_string()
        ),
        login_home = probe_shell(home, true, "$HOME"),
        profile_d = probe_system_profile(),
    )
}

/// Run `/bin/sh [-l] -c` against the fixture `$HOME` under exactly the
/// environment [`ServerGuard::start`] gives the server, and return what the
/// shell expanded `expr` to.
///
/// This is the reference the pane is compared against: it isolates "what does
/// a login shell do on THIS host" from "what did phux ask for", two questions
/// whose answers used to be conflated in one assertion. Telling them apart
/// once cost a five-run CI bisect.
fn probe_shell(home: &Path, login: bool, expr: &str) -> String {
    let mut cmd = Command::new("/bin/sh");
    cmd.env_clear();
    cmd.env("HOME", home);
    cmd.env("PATH", LAUNCHD_DEFAULT_PATH);
    if login {
        cmd.arg("-l");
    }
    cmd.arg("-c").arg(format!("printf %s \"{expr}\""));
    cmd.output().map_or_else(
        |e| format!("(probe failed: {e})"),
        |out| String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

/// The `PATH` a `[-l]` `/bin/sh` produces against the fixture home.
fn probe_shell_path(home: &Path, login: bool) -> String {
    probe_shell(home, login, "$PATH")
}

/// What the host's system profile does to a login shell, listed so a failure
/// names the file responsible instead of leaving it to be guessed.
fn probe_system_profile() -> String {
    let mut entries: Vec<String> = std::fs::read_dir("/etc/profile.d").map_or_else(
        |_| Vec::new(),
        |dir| {
            dir.filter_map(Result::ok)
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        },
    );
    entries.sort();
    if entries.is_empty() {
        "(none)".to_owned()
    } else {
        entries.join(" ")
    }
}

/// phux-87rr acceptance criterion 6: existing direct/server-in-terminal
/// pane startup remains regression-free. Same profile fixture, same
/// command, only the service marker is missing — the command must NOT
/// resolve, proving login-shell treatment stayed conditional rather than
/// becoming the new default for every server.
#[test]
fn ordinary_pane_does_not_source_the_profile_twice() {
    let home = write_profile_fixture();
    let result_path = home.path().join("result");
    let seed = seed_command(&result_path);

    let _server = ServerGuard::start(home.path(), &seed, false);

    let contents = read_result_file(&result_path, RESULT_DEADLINE);
    let pane_path = contents.lines().next().unwrap_or_default().to_owned();

    let login = probe_shell_path(home.path(), true);
    let plain = probe_shell_path(home.path(), false);
    let diagnostics = diagnostics(home.path(), &login, &plain);

    assert_eq!(
        pane_path, plain,
        "an ordinary (non-service) server's seed pane must get a plain \
         shell's PATH — login-shell treatment must stay conditional on the \
         service marker.\n{diagnostics}"
    );
    assert!(
        !contents.contains("PHUX_PROFILE_MARKER_FOUND"),
        "an ordinary (non-service) server's seed pane must NOT get \
         login-shell treatment — `~/.profile` must stay unsourced; \
         got: {contents:?}\n{diagnostics}"
    );
}
