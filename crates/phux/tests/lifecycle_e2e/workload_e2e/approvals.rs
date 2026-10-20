/// A paired server, the owner's socket, and one enrolled workload whose
/// grant holds `?signal` (ADR-0128).
struct HeldWorld {
    dir: TempDir,
    _server: Server,
    cert: PathBuf,
    key: PathBuf,
    credential: String,
}

impl HeldWorld {
    fn start() -> Self {
        let dir = TempDir::new().expect("tempdir");
        prepare_dirs(dir.path());
        paired(dir.path());
        init_authority(dir.path());
        let client = client_key();
        let cert = dir.path().join("client.pem");
        let key = dir.path().join("client.key");
        std::fs::write(&key, &client.key_pem).expect("write client key");
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).expect("chmod");
        let credential = enroll(
            dir.path(),
            &client,
            &cert,
            &["inventory,observe,?signal@global"],
        );
        let server = start_server(dir.path(), &["--quic", listeners::LOOPBACK_ANY_PORT], &[]);
        register_loopback_remote(dir.path());
        let world = Self {
            dir,
            _server: server,
            cert,
            key,
            credential,
        };
        let whoami = ["whoami", "--remote", REMOTE, "--json"];
        await_success(world.dir.path(), &whoami, &world.identity());
        world
    }

    fn identity(&self) -> [(&'static str, &Path); 2] {
        [
            ("PHUX_WORKLOAD_CERT", self.cert.as_path()),
            ("PHUX_WORKLOAD_KEY", self.key.as_path()),
        ]
    }

    fn socket(&self) -> String {
        self.dir.path().join("s.sock").display().to_string()
    }

    /// `phux ARGS --socket S` on the owner's socket, stdin not a terminal.
    fn owner(&self, args: &[&str]) -> Output {
        let socket = self.socket();
        let mut all = args.to_vec();
        all.extend(["--socket", &socket]);
        phux(self.dir.path(), &all)
    }

    /// `phux ARGS` in the background, as the owner or as the workload.
    fn spawn(&self, args: &[&str], as_workload: bool) -> Child {
        let mut command = common::phux_cmd(crate::runner::phux_bin());
        command
            .envs(hermetic_env(self.dir.path()))
            .current_dir(self.dir.path())
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if as_workload {
            command.envs(self.identity());
        }
        command.spawn().expect("spawn phux")
    }

    /// The first pending approval, once one exists.
    fn first_approval(&self) -> serde_json::Value {
        let start = Instant::now();
        loop {
            let listed = json_doc(&self.owner(&["approvals", "--json"]));
            if let Some(first) = listed["approvals"].as_array().and_then(|all| all.first()) {
                return first.clone();
            }
            assert!(start.elapsed() < READY_DEADLINE, "nothing was ever held");
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// The NDJSON event named `name` in `watched`'s stdout.
fn watched_event(watched: &Output, name: &str) -> serde_json::Value {
    let found = text(&watched.stdout)
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|event| event["event"] == name);
    assert!(
        found.is_some(),
        "`phux watch` rendered no {name}: {}",
        text(&watched.stdout)
    );
    found.expect("checked above")
}

/// ADR-0128 end to end: a `?signal` grant's kill is held; `phux approvals`
/// lists it, `approve` runs it once as the workload, and a resumed `phux watch`
/// renders both approval events. A dangerous verb without `--yes` on a
/// non-terminal exits 2 and sends nothing.
#[test]
#[ignore = "spawns a real server with a workload-mTLS QUIC listener; runs in the e2e lane"]
fn approvals_list_approve_and_watch_render_both_events() {
    let world = HeldWorld::start();
    let created = world.owner(&["new", "-s", "work", "--json"]);
    assert!(created.status.success(), "{}", text(&created.stderr));
    let terminal = json_doc(&created)["terminal_id"]
        .as_u64()
        .expect("terminal_id");
    let pane = format!("@{terminal}");
    let waited = world.owner(&["resource", "wait", &pane, "--timeout", "1", "--json"]);
    let cursor = json_doc(&waited)["cursor"]
        .as_str()
        .expect("a cursor")
        .to_owned();

    // No --yes and no terminal to ask: exit 2, nothing sent.
    let unconfirmed = world.owner(&["kill", &pane]);
    assert_eq!(
        unconfirmed.status.code(),
        Some(2),
        "{}",
        text(&unconfirmed.stderr)
    );
    assert!(text(&unconfirmed.stderr).contains("--yes"));
    assert!(
        world
            .owner(&["resource", "show", &pane, "--json"])
            .status
            .success()
    );

    let socket = world.socket();
    let watch = world.spawn(
        &[
            "watch",
            &pane,
            "--json",
            "--after",
            &cursor,
            "--until",
            "approval_decided",
            "--timeout",
            "60",
            "--socket",
            &socket,
        ],
        false,
    );
    let held_kill = world.spawn(&["kill", "--yes", "--remote", REMOTE, &pane], true);

    let approval = world.first_approval();
    assert_eq!(approval["method"], "KILL_RESOURCE", "{approval}");
    let subject = format!("terminal:{terminal}");
    assert_eq!(approval["subjects"], serde_json::json!([subject]));
    assert_eq!(
        approval["requester"]["credential_id"],
        world.credential.as_str()
    );
    let id = approval["id"].as_str().expect("id").to_owned();
    let still_there = world.owner(&["resource", "show", &pane, "--json"]);
    assert!(still_there.status.success(), "a held kill has not run");

    let approved = world.owner(&["approve", "--yes", &id]);
    assert!(approved.status.success(), "{}", text(&approved.stderr));
    let killed = wait_exit(held_kill, READY_DEADLINE);
    assert!(
        killed.status.success(),
        "the approved kill completes: {}",
        text(&killed.stderr)
    );
    let listed = json_doc(&world.owner(&["approvals", "--json"]));
    assert_eq!(listed["approvals"], serde_json::json!([]));

    let watched = wait_exit(watch, READY_DEADLINE);
    assert!(watched.status.success(), "{}", text(&watched.stderr));
    assert_eq!(
        watched_event(&watched, "approval_requested")["id"],
        id.as_str()
    );
    let decided = watched_event(&watched, "approval_decided");
    assert_eq!(decided["id"], id.as_str());
    assert_eq!(decided["outcome"], "approved");
}
