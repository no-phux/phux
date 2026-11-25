#![allow(clippy::expect_used, clippy::panic, reason = "tests")]

use crate::common;

use phux_client::attach::connection::Connection;
use phux_client::org::OrgInventory;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

mod restore;

fn start(session: &str) -> common::ServerGuard {
    common::ServerGuard::builder("wa").session(session).start()
}

fn run(args: &[&str]) -> (i32, String, String) {
    run_in(args, None)
}

/// Run `phux` with the process's isolated XDG dirs, or with `xdg` as
/// `XDG_CONFIG_HOME` when given.
fn run_in(args: &[&str], xdg: Option<&Path>) -> (i32, String, String) {
    let mut cmd = common::phux_cmd(crate::runner::phux_bin());
    if let Some(xdg) = xdg {
        cmd.env("XDG_CONFIG_HOME", xdg);
    }
    output(cmd.args(args))
}

fn output(cmd: &mut Command) -> (i32, String, String) {
    common::Out::of(cmd).into_parts()
}

/// `run_in` that must exit 0; returns stdout.
fn ok(args: &[&str], xdg: Option<&Path>) -> String {
    let (code, stdout, stderr) = run_in(args, xdg);
    assert_eq!(code, 0, "phux {args:?}: {stdout} {stderr}");
    stdout
}

/// `archive export` of `socket` to `path`, parsed. Export prints nothing.
fn export(socket: &str, path: &Path, xdg: Option<&Path>) -> Value {
    let path_arg = path.to_str().expect("utf-8 archive");
    let stdout = ok(
        &[
            "archive", "export", "--socket", socket, "--output", path_arg,
        ],
        xdg,
    );
    assert!(stdout.is_empty(), "{stdout}");
    serde_json::from_slice(&std::fs::read(path).expect("read archive")).expect("archive JSON")
}

/// `archive import` of `path` into `socket`, which must restore `name`.
fn import(path: &Path, socket: &str, name: &str, xdg: Option<&Path>) -> Value {
    let path_arg = path.to_str().expect("utf-8 archive");
    let summary = ok(&["archive", "import", path_arg, "--socket", socket], xdg);
    let summary: Value = serde_json::from_str(&summary).expect("restore summary JSON");
    assert!(
        summary["failed"].as_array().is_none_or(Vec::is_empty),
        "restore must not report a failure: {summary}"
    );
    assert!(
        summary["restored"]
            .as_array()
            .expect("restored array")
            .iter()
            .any(|restored| restored == name),
        "{name} must be restored: {summary}"
    );
    summary
}

/// [`import`], then `restore` of `name`: an import starts nothing (R16);
/// `phux restore` starts the dormant panes.
fn import_and_restore(path: &Path, socket: &str, name: &str, xdg: Option<&Path>) -> Value {
    let summary = import(path, socket, name, xdg);
    ok(&["restore", name, "--socket", socket], xdg);
    summary
}

/// The archive's session called `name`.
fn archived<'a>(archive: &'a Value, name: &str) -> &'a Value {
    archive["sessions"]
        .as_array()
        .expect("sessions array")
        .iter()
        .find(|session| session["name"] == name)
        .unwrap_or_else(|| panic!("archive names no {name:?} session: {archive}"))
}

fn archived_mut<'a>(archive: &'a mut Value, name: &str) -> &'a mut Value {
    archive["sessions"]
        .as_array_mut()
        .expect("sessions array")
        .iter_mut()
        .find(|session| session["name"] == name)
        .expect("archived session")
}

/// Named session's windows array from an archive.
fn session_windows<'a>(archive: &'a Value, name: &str) -> &'a Vec<Value> {
    archived(archive, name)["windows"]
        .as_array()
        .unwrap_or_else(|| panic!("{name:?} session has no windows array: {archive}"))
}

/// Total pane count across every window of `windows`.
fn total_panes(windows: &[Value]) -> usize {
    windows
        .iter()
        .map(|window| window["panes"].as_array().map_or(0, Vec::len))
        .sum()
}

/// Ambient budget for a fake agent's `/bin/sh` to run at all (a loaded box
/// has taken ~8s); `phux wait` returns as soon as the line appears.
const AGENT_PAINT_BUDGET_SECS: &str = "60";

/// Subject budget for a specific line once the agent is painting: it writes
/// every line in one burst, so a timeout here means the argv is wrong.
const AGENT_LINE_BUDGET_SECS: &str = "5";

fn wait_until(socket: &str, target: &str, marker: &str, budget: &str) -> (i32, String) {
    let (code, _, stderr) = run(&[
        "wait",
        "--until",
        marker,
        "--timeout",
        budget,
        "--socket",
        socket,
        target,
    ]);
    (code, stderr)
}

/// Wait for `target` to paint at all (ambient budget), then for `marker`
/// (subject budget), so load cannot masquerade as a contract failure.
fn wait_painted_then(socket: &str, target: &str, marker: &str) {
    let (code, stderr) = wait_until(socket, target, "FAKE_AGENT_ARGS=", AGENT_PAINT_BUDGET_SECS);
    assert_eq!(
        code, 0,
        "{target} never painted within {AGENT_PAINT_BUDGET_SECS}s (environment, not contract): {stderr}"
    );
    let (code, stderr) = wait_until(socket, target, marker, AGENT_LINE_BUDGET_SECS);
    assert_eq!(code, 0, "{target} never printed {marker:?}: {stderr}");
}

/// Kill `selector`, then poll until it is gone from `socket`'s registry
/// (ambient budget: each poll forks a `phux`).
fn kill_and_wait_absent(socket: &str, selector: &str) {
    ok(&["kill", "--yes", "--socket", socket, selector], None);
    let budget = Duration::from_secs(60);
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        let (code, _, _) = run(&["snapshot", "--json", "--socket", socket, selector]);
        if code != 0 {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("{selector} was still present {budget:?} after kill");
}

/// Run `read` against `socket`'s organization records.
fn with_org<T>(socket: &str, read: impl AsyncFnOnce(&mut Connection, OrgInventory) -> T) -> T {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        let mut conn = Connection::connect(Path::new(socket))
            .await
            .expect("connect organization");
        let records =
            phux_client::org::fetch(&mut conn, phux_protocol::wire::frame::OrgFilter::All, 1)
                .await
                .expect("organization records");
        read(&mut conn, records).await
    })
}

fn live_terminals(socket: &str, name: &str) -> Vec<u32> {
    with_org(socket, async |_, records| {
        let session = records.session_named(name).expect("session");
        records
            .members(&session.key)
            .filter_map(|resource| resource.live.as_ref()?.local_id())
            .collect()
    })
}

fn session_attrs(socket: &str, name: &str) -> Vec<(String, String)> {
    with_org(socket, async |_, records| {
        records
            .session_named(name)
            .expect("named organization session")
            .attrs
            .clone()
    })
}

fn arranged_terminal(socket: &str, name: &str, terminal_id: u32) -> bool {
    use phux_protocol::wire::frame::{ArrangementNode, EntryTarget};
    with_org(socket, async |conn, records| {
        let session = records.session_named(name).expect("named session");
        let resource = records
            .members(&session.key)
            .find(|resource| {
                resource
                    .live
                    .as_ref()
                    .and_then(phux_protocol::ids::ResourceId::local_id)
                    == Some(terminal_id)
            })
            .expect("reopened resource");
        let arrangement = session.arrangement.expect("session arrangement");
        let document = phux_client::org::arrangement(conn, 3, arrangement)
            .await
            .expect("read arrangement")
            .document;
        document.groups.iter().any(|group| {
            matches!(&group.tree, ArrangementNode::Leaf(entry)
                if entry.target == EntryTarget::Member(resource.key))
        })
    })
}

/// Assert the session `name` on `socket` carries each `(key, value)` attribute.
fn assert_attrs(socket: &str, name: &str, expected: &[(&str, &str)]) {
    let attrs = session_attrs(socket, name);
    for (key, value) in expected {
        assert_eq!(attr(&attrs, key), Some(*value), "{name} {key}: {attrs:?}");
    }
}

fn attr<'a>(attrs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.as_str())
}

/// Two Git repositories with the same checkout basename must never share a
/// session. Organization attributes survive an archive export/import even
/// though project open does not write the legacy singleton metadata.
#[test]
#[ignore = "spawns real phux servers; run explicitly when validating project sessions."]
fn project_open_keeps_canonical_checkout_identity_and_archive_attributes() {
    let server = start("seed");
    let dest = start("destination");
    let socket = server.socket_text();
    let root = tempfile::tempdir().expect("project tempdir");
    let checkout = |parent: &str, basename: &str| {
        let path = root.path().join(parent).join(basename);
        std::fs::create_dir_all(&path).expect("checkout directory");
        let status = Command::new("git")
            .args(["init", "-q"])
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .current_dir(&path)
            .status()
            .expect("git init");
        assert!(status.success());
        path.canonicalize()
            .expect("canonical checkout")
            .to_str()
            .expect("utf-8 path")
            .to_owned()
    };
    let first = checkout("one", "same");
    let collision = checkout("two", "same");
    let other = checkout("three", "other");
    let config = common::isolated_xdg().config.join("phux");
    std::fs::create_dir_all(&config).expect("config directory");
    std::fs::write(
        config.join("config.toml"),
        format!("[[projects]]\nname = \"Friendly\"\npath = {first:?}\n"),
    )
    .expect("project catalog");
    let project_open =
        |target: &str| run(&["project", "open", target, "--json", "--socket", &socket]);
    // The server's preexisting "seed" session has no directory attribute.
    // Neither its pane cwd nor a matching basename is sufficient to adopt it.
    let (code, stdout, stderr) = project_open(&checkout("four", "seed"));
    assert_ne!(code, 0, "untagged session must be refused: {stdout}");
    assert!(stderr.contains("no checkout directory"), "{stderr}");

    let (code, stdout, stderr) = project_open("Friendly");
    assert_eq!(code, 0, "first project open: {stderr}");
    let opened: Value = serde_json::from_str(&stdout).expect("open JSON");
    assert_eq!(opened["created"], true);
    assert_eq!(opened["project"], "Friendly");
    assert_attrs(
        &socket,
        "same",
        &[("directory", &first), ("project", "same")],
    );

    let (code, stdout, stderr) = project_open(&first);
    assert_eq!(code, 0, "reentry: {stderr}");
    let reentry: Value = serde_json::from_str(&stdout).expect("reentry JSON");
    assert_eq!(reentry["created"], false);
    let (code, stdout, stderr) = project_open(&collision);
    assert_ne!(code, 0, "colliding repo must be refused: {stdout}");
    assert!(stdout.is_empty());
    assert!(
        stderr.contains(&first) && stderr.contains(&collision),
        "{stderr}"
    );
    assert_attrs(&socket, "same", &[("directory", &first)]);

    let (code, _, stderr) = project_open(&other);
    assert_eq!(code, 0, "second distinct project: {stderr}");
    assert_attrs(
        &socket,
        "other",
        &[("directory", &other), ("project", "other")],
    );

    let saved = export(&socket, &root.path().join("projects.json"), None);
    for (name, path) in [("same", &first), ("other", &other)] {
        let row = archived(&saved, name);
        assert_eq!(row["cwd"], path.as_str());
        assert_eq!(row["project"], name);
    }
    let dest_socket = dest.socket_text();
    let archive = root.path().join("projects.json");
    let archive = archive.to_str().expect("utf-8 archive");
    ok(
        &["archive", "import", archive, "--socket", &dest_socket],
        None,
    );
    assert_attrs(
        &dest_socket,
        "same",
        &[("directory", &first), ("project", "same")],
    );
    assert_attrs(
        &dest_socket,
        "other",
        &[("directory", &other), ("project", "other")],
    );
}

/// A repository recipe cannot run in JSON mode until its exact bytes have
/// been approved, but changing it after the session exists never reruns it.
#[test]
#[ignore = "spawns a real phux server; run explicitly when validating project recipes."]
fn project_open_requires_trust_only_on_first_creation() {
    let server = start("seed");
    let root = tempfile::tempdir().expect("project tempdir");
    let project = root.path().join("trusted");
    std::fs::create_dir_all(project.join(".phux")).expect("recipe directory");
    let recipe = project.join(".phux/project.toml");
    std::fs::write(&recipe, "[[windows]]\nname = \"shell\"\n").expect("recipe");
    let path = project.to_str().expect("utf-8 project");
    let socket = server.socket_text();
    let open = || run(&["project", "open", path, "--json", "--socket", &socket]);
    let (code, stdout, stderr) = open();
    assert_ne!(code, 0, "untrusted recipe must refuse: {stdout}");
    assert!(stderr.contains("not trusted"), "{stderr}");
    ok(&["project", "trust", path], None);
    let (code, stdout, stderr) = open();
    assert_eq!(code, 0, "trusted recipe open: {stderr}");
    let first: Value = serde_json::from_str(&stdout).expect("open JSON");
    assert_eq!(first["created"], true);
    let first_id = first["terminal_id"].as_u64().expect("first terminal");
    std::fs::write(&recipe, "[[windows]]\nname = \"changed\"\n").expect("change recipe");
    kill_and_wait_absent(&socket, &format!("@{first_id}"));
    let (code, stdout, stderr) = open();
    assert_eq!(
        code, 0,
        "reentry seeds shell without rerunning recipe: {stderr}"
    );
    let reopened: Value = serde_json::from_str(&stdout).expect("open JSON");
    assert_eq!(reopened["created"], false);
    assert_ne!(reopened["terminal_id"], first["terminal_id"]);
    let canonical = project.canonicalize().expect("canonical project");
    assert_attrs(
        &socket,
        "trusted",
        &[("directory", canonical.to_str().expect("utf-8 path"))],
    );
    let reopened_id = u32::try_from(reopened["terminal_id"].as_u64().expect("terminal"))
        .expect("local terminal id");
    assert!(
        arranged_terminal(&socket, "trusted", reopened_id),
        "new shell must appear in the session arrangement"
    );
}

/// A recipe pane's working directory is independent of the session's typed
/// checkout directory, including after an archive export/import.
#[test]
#[ignore = "spawns real phux servers; run explicitly when validating project recipes."]
fn project_recipe_first_pane_cwd_survives_archive_roundtrip() {
    let source = start("seed");
    let dest = start("destination");
    let root = tempfile::tempdir().expect("project tempdir");
    let checkout = root.path().join("site");
    std::fs::create_dir_all(checkout.join(".phux")).expect("recipe directory");
    std::fs::create_dir(checkout.join("web")).expect("web directory");
    std::fs::write(
        checkout.join(".phux/project.toml"),
        "focus = \"site.web\"\n[[windows]]\nid = \"site\"\n\
         panes = [{ id = \"web\", cwd = \"web\" }]\n",
    )
    .expect("recipe");
    let canonical = checkout.canonicalize().expect("canonical checkout");
    let root_path = canonical.to_str().expect("utf-8 root");
    let web_path = canonical.join("web");
    let web_path = web_path.to_str().expect("utf-8 web");
    ok(&["project", "trust", root_path], None);
    let source_socket = source.socket_text();
    ok(
        &[
            "project",
            "open",
            root_path,
            "--json",
            "--socket",
            &source_socket,
        ],
        None,
    );
    assert_attrs(&source_socket, "site", &[("directory", root_path)]);

    let archive = root.path().join("site.json");
    let saved = export(&source_socket, &archive, None);
    let site = archived(&saved, "site");
    assert_eq!(site["cwd"], root_path, "session identity is checkout root");
    assert_eq!(
        site["windows"][0]["panes"][0]["cwd"], web_path,
        "pane launch cwd is web"
    );

    let dest_socket = dest.socket_text();
    import_and_restore(&archive, &dest_socket, "site", None);
    assert_attrs(&dest_socket, "site", &[("directory", root_path)]);
    let restored = export(&dest_socket, &root.path().join("resaved.json"), None);
    let site = archived(&restored, "site");
    assert_eq!(site["cwd"], root_path);
    assert_eq!(
        site["windows"][0]["panes"][0]["cwd"], web_path,
        "restored pane must launch in web"
    );
}

/// Multiple openers share exactly one newly created pane, and exactly one
/// reseeded pane after that session's first shell exits.
#[test]
#[ignore = "spawns a real phux server; run explicitly when validating project concurrency."]
fn concurrent_project_opens_share_the_same_shell() {
    let server = start("seed");
    let dir = tempfile::tempdir().expect("project tempdir");
    let project = dir.path().join("concurrent");
    std::fs::create_dir(&project).expect("checkout directory");
    let canonical = project.canonicalize().expect("canonical checkout");
    let path = canonical.to_str().expect("utf-8 path");
    let socket = server.socket_text();
    let first = parallel_project_opens(&socket, path, 6);
    assert_eq!(
        first
            .iter()
            .filter(|value| value["created"] == true)
            .count(),
        1
    );
    let first_id = first[0]["terminal_id"].as_u64().expect("terminal id");
    assert!(first.iter().all(|value| value["terminal_id"] == first_id));
    assert_eq!(
        live_terminals(&socket, "concurrent"),
        vec![u32::try_from(first_id).expect("local id")]
    );
    kill_and_wait_absent(&socket, &format!("@{first_id}"));
    let second = parallel_project_opens(&socket, path, 6);
    let second_id = second[0]["terminal_id"].as_u64().expect("terminal id");
    assert_ne!(second_id, first_id);
    assert!(
        second
            .iter()
            .all(|value| value["created"] == false && value["terminal_id"] == second_id)
    );
    assert_eq!(
        live_terminals(&socket, "concurrent"),
        vec![u32::try_from(second_id).expect("local id")]
    );
}

fn parallel_project_opens(socket: &str, path: &str, count: usize) -> Vec<Value> {
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(count));
    std::thread::scope(|scope| {
        #[allow(
            clippy::needless_collect,
            reason = "all threads must start before any join reaches the barrier"
        )]
        let handles: Vec<_> = (0..count)
            .map(|_| {
                let barrier = std::sync::Arc::clone(&barrier);
                scope.spawn(move || {
                    let state = tempfile::tempdir().expect("independent state directory");
                    barrier.wait();
                    let (code, stdout, stderr) = output(
                        common::phux_cmd(crate::runner::phux_bin())
                            .env("XDG_STATE_HOME", state.path())
                            .args(["project", "open", path, "--json", "--socket", socket]),
                    );
                    assert_eq!(code, 0, "concurrent open: {stdout} {stderr}");
                    serde_json::from_str::<Value>(&stdout).expect("open JSON")
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("open thread"))
            .collect()
    })
}

/// `archive export` archives a session's real L3 split layout (not the flat
/// fallback), and export -> import -> restore -> export keeps the same split.
#[test]
#[ignore = "spawns real phux servers; run explicitly when validating workspace archives."]
fn archive_export_captures_a_real_split_tree_and_restore_replays_it() {
    let source = start("source");
    let dest = start("seed");
    let archive_dir = tempfile::tempdir().expect("archive tempdir");
    let source_archive = archive_dir.path().join("source.json");
    let cwd = archive_dir.path().to_string_lossy().into_owned();
    let source_socket = source.socket_text();
    let dest_socket = dest.socket_text();

    let created = ok(
        &[
            "new",
            "--socket",
            &source_socket,
            "--json",
            "-s",
            "split-bench",
            "--cwd",
            &cwd,
        ],
        None,
    );
    let created: Value = serde_json::from_str(&created).expect("new --json");
    let seed_pane = created["terminal_id"]
        .as_u64()
        .expect("new --json names the seed pane");
    // A placed pane writes a real two-leaf split into the layout envelope.
    ok(
        &[
            "spawn",
            "--socket",
            &source_socket,
            "--target",
            &format!("@{seed_pane}"),
            "--split",
            "vertical",
            "--",
            "sleep",
            "30",
        ],
        None,
    );

    let assert_split = |archive: &Value, when: &str| {
        let window = &session_windows(archive, "split-bench")[0];
        assert_eq!(
            window["panes"].as_array().expect("panes array").len(),
            2,
            "the {when} window must carry both panes, not just the seed: {window}"
        );
        assert_eq!(
            window["layout"]["kind"], "split",
            "the {when} layout must be the real two-leaf split, not flattened: {window}"
        );
    };
    assert_split(&export(&source_socket, &source_archive, None), "saved");
    let summary = import_and_restore(&source_archive, &dest_socket, "split-bench", None);
    assert_eq!(summary["schema_version"], 2);
    let resaved = export(&dest_socket, &archive_dir.path().join("dest.json"), None);
    assert_split(&resaved, "round-tripped");
}

/// Importing an archive whose session is on host `edge`, in `/src/api`, and
/// tagged `phux` keeps that organization. The directory does not have to
/// exist: the child falls back, and the registry still records the request.
#[test]
#[ignore = "spawns real phux servers; run explicitly when validating workspace archives."]
fn import_keeps_edge_host_src_api_directory_phux_project_and_link() {
    let dest = start("edge");
    let archive_dir = tempfile::tempdir().expect("archive tempdir");
    let archive = archive_dir.path().join("edge.json");
    let session = serde_json::json!({
        "name": "api",
        "host": "edge",
        "cwd": "/src/api",
        "project": "phux",
        "project_link": "platform",
        "windows": []
    });
    let document = serde_json::json!({ "schema_version": 2, "sessions": [session] });
    std::fs::write(&archive, document.to_string()).expect("write archive");
    let socket = dest.socket_text();
    let archive_arg = archive.to_str().expect("utf-8 archive");

    let plan = ok(&["archive", "import", "--plan", archive_arg], None);
    assert!(
        plan.contains("api host=edge directory=/src/api project=phux"),
        "plan must print the archived organization: {plan}"
    );
    import(&archive, &socket, "api", None);

    assert_attrs(
        &socket,
        "api",
        &[
            ("host", "edge"),
            ("directory", "/src/api"),
            ("project", "phux"),
            ("project.link", "platform"),
        ],
    );
    let saved = export(&socket, &archive_dir.path().join("resaved.json"), None);
    let resaved = archived(&saved, "api");
    for key in ["host", "cwd", "project", "project_link"] {
        assert_eq!(resaved[key], session[key], "{key} was not kept: {resaved}");
    }
}

/// A stored layout can miss live panes (a headless spawn never touches it)
/// and keep leaves of closed panes (nothing prunes them). `archive export`
/// must drop the dead leaf and put the headless pane in an `"unplaced"`
/// window with a warning, so export -> import -> restore -> export keeps
/// exactly the two live panes.
#[test]
#[ignore = "spawns real phux servers; run explicitly when validating workspace archives."]
fn archive_export_reconciles_a_dead_layout_leaf_and_a_headless_spawn() {
    // One session only, so the headless spawn can join nothing else; its
    // seed pane is always `@1`.
    let source = start("reconcile-repro");
    let dest = start("seed");
    let archive_dir = tempfile::tempdir().expect("archive tempdir");
    let source_archive = archive_dir.path().join("reconcile-source.json");
    let source_socket = source.socket_text();
    let dest_socket = dest.socket_text();

    // A placed pane, then killed: a dead layout leaf.
    let placed = ok(
        &[
            "spawn",
            "--socket",
            &source_socket,
            "--target",
            "@1",
            "--split",
            "vertical",
            "--json",
            "--",
            "sleep",
            "30",
        ],
        None,
    );
    let placed: Value = serde_json::from_str(&placed).expect("spawn --json");
    let placed_id = placed["terminal_id"]
        .as_u64()
        .expect("spawn --json names the placed pane");
    kill_and_wait_absent(&source_socket, &format!("@{placed_id}"));

    // A headless spawn joins the session but never touches its layout.
    ok(
        &["spawn", "--socket", &source_socket, "--", "sleep", "30"],
        None,
    );

    let path_arg = source_archive.to_str().expect("utf-8 archive");
    let (code, stdout, stderr) = run(&[
        "archive",
        "export",
        "--socket",
        &source_socket,
        "--output",
        path_arg,
    ]);
    assert_eq!(code, 0, "archive export failed: {stderr}");
    assert!(stdout.is_empty());
    assert!(
        stderr.contains("reconcile-repro") && stderr.contains("absent from its stored layout"),
        "save must warn about the unplaced headless pane naming the session: {stderr}"
    );

    let saved: Value =
        serde_json::from_slice(&std::fs::read(&source_archive).expect("read saved archive"))
            .expect("saved archive JSON");
    let windows = session_windows(&saved, "reconcile-repro");
    assert_eq!(
        total_panes(windows),
        2,
        "archive must hold exactly the seed pane and the headless spawn — the dead leaf \
         dropped, the headless pane not lost: {windows:?}"
    );
    let (unplaced, seeded): (Vec<_>, Vec<_>) = windows
        .iter()
        .partition(|window| window["name"] == "unplaced");
    assert!(
        unplaced.len() == 1 && seeded.len() == 1,
        "the headless pane must land in a synthesized unplaced window: {windows:?}"
    );
    for window in [unplaced[0], seeded[0]] {
        assert_eq!(
            window["panes"].as_array().expect("panes array").len(),
            1,
            "unplaced holds the one pane the layout never named; the seed's split \
             collapses after the dead leaf is pruned: {window}"
        );
    }

    import_and_restore(&source_archive, &dest_socket, "reconcile-repro", None);
    let resaved = export(
        &dest_socket,
        &archive_dir.path().join("reconcile-dest.json"),
        None,
    );
    let resaved_windows = session_windows(&resaved, "reconcile-repro");
    assert_eq!(
        total_panes(resaved_windows),
        2,
        "the restored session must carry exactly two live panes, matching the source: \
         {resaved_windows:?}"
    );
}
