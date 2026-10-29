/// The pre-seeded session name the test drives against.
const SESSION: &str = "work";

/// Poll cadence while sampling `phux agent show`. Denser than the detector's
/// identified tick (~300 ms) so a transient record value cannot slip between
/// two samples of [`ServerGuard::states_over`].
const RECORD_POLL: Duration = Duration::from_millis(100);

/// The detector startup grace the shim test runs its server under (ADR-0046
/// point 6; production default 3 s). The fake Claude paints its dialog
/// immediately, so shortening it changes nothing about what is proven.
const TEST_STARTUP_GRACE_MS: &str = "200";

/// Ceiling for a detector verdict to reach the record and come back out of
/// `phux agent show`. A failure bound, not a timing gate.
const DETECT_DEADLINE: Duration = Duration::from_secs(20);

/// Bounded window for the "and it STAYS there" halves. Covers many detector
/// ticks at the identified cadence plus slack for a loaded pool.
const HOLD_WINDOW: Duration = Duration::from_secs(3);

/// A server with extra environment on its child (the detector's tuning
/// seams are read once, inside the server).
fn start_with_env(envs: &[(&str, &str)]) -> ServerGuard {
    ServerGuard::builder("agent")
        .envs(envs.iter().copied())
        .start()
}

impl ServerGuard {
    /// Run `phux <args...> --socket <sock>` with `envs` capturing stdout.
    /// The verbs used here all take `--socket` as a per-verb flag (no
    /// trailing positional swallows it), so appending is safe.
    fn run_with_env(&self, args: &[&str], envs: &[(&str, &std::path::Path)]) -> String {
        let mut cmd = common::phux_cmd(crate::runner::phux_bin());
        cmd.args(args).arg("--socket").arg(&self.socket);
        for (key, value) in envs {
            cmd.env(key, value);
        }
        let out = cmd.stdin(Stdio::null()).output().expect("run phux verb");
        assert!(
            out.status.success(),
            "phux {args:?} exited {:?}; stderr={}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// Run `phux agent <args...> --socket <sock>` capturing stdout.
    /// `agent`'s subcommands take `--socket` as a per-verb flag (no
    /// trailing positional swallows it), so appending is safe here.
    fn agent(&self, args: &[&str]) -> String {
        let out = common::phux_cmd(crate::runner::phux_bin())
            .arg("agent")
            .args(args)
            .arg("--socket")
            .arg(&self.socket)
            .stdin(Stdio::null())
            .output()
            .expect("run phux agent verb");
        assert!(
            out.status.success(),
            "phux agent {args:?} exited {:?}; stderr={}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// Create a pane running `command` and return its local Terminal id.
    ///
    /// `--socket` goes BEFORE the subcommand: `spawn`'s trailing positional
    /// would otherwise swallow it as part of the command line.
    fn spawn_pane(&self, command: &std::path::Path) -> u32 {
        let out = common::phux_cmd(crate::runner::phux_bin())
            .arg("--socket")
            .arg(&self.socket)
            .args(["spawn", "--json", "--"])
            .arg(command)
            .stdin(Stdio::null())
            .output()
            .expect("run phux spawn");
        assert!(
            out.status.success(),
            "phux spawn exited {:?}; stderr={}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
        let json: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("phux spawn --json");
        json["terminal_id"]
            .as_u64()
            .expect("spawn reports a terminal id")
            .try_into()
            .expect("terminal id fits u32")
    }

    /// `phux agent show TARGET --json`, decoded.
    fn agent_show(&self, target: &str) -> serde_json::Value {
        let shown = self.agent(&["show", target, "--json"]);
        serde_json::from_str(&shown)
            .unwrap_or_else(|err| panic!("agent show JSON ({err}): {shown}"))
    }

    /// Poll `agent show` until the record's `state` reads `want`, and return
    /// the whole document. Panics with the last reading on timeout.
    fn await_agent_state(&self, target: &str, want: &str, deadline: Duration) -> serde_json::Value {
        let end = Instant::now() + deadline;
        loop {
            let json = self.agent_show(target);
            if json["agents"][0]["state"] == want {
                return json;
            }
            assert!(
                Instant::now() < end,
                "{target} never reached state {want} within {deadline:?}; last: {json}"
            );
            std::thread::sleep(RECORD_POLL);
        }
    }

    /// Every distinct `state` seen while sampling `agent show` densely across
    /// `window`, in first-seen order: a bounded negative assertion.
    fn states_over(&self, target: &str, window: Duration) -> Vec<String> {
        let end = Instant::now() + window;
        let mut seen: Vec<String> = Vec::new();
        loop {
            let json = self.agent_show(target);
            let state = json["agents"][0]["state"]
                .as_str()
                .unwrap_or("<missing>")
                .to_owned();
            if !seen.contains(&state) {
                seen.push(state);
            }
            if Instant::now() >= end {
                return seen;
            }
            std::thread::sleep(RECORD_POLL);
        }
    }
}
