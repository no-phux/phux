#![allow(clippy::expect_used, clippy::panic, reason = "tests")]

#[path = "../common/mod.rs"]
mod common;

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn start(session: &str) -> common::ServerGuard {
    common::ServerGuard::builder("wa").session(session).start()
}

fn run(args: &[&str]) -> (i32, String, String) {
    output(common::phux_cmd(crate::runner::phux_bin()).args(args))
}

fn run_with_xdg(args: &[&str], xdg: &std::path::Path) -> (i32, String, String) {
    output(
        common::phux_cmd(crate::runner::phux_bin())
            .env("XDG_CONFIG_HOME", xdg)
            .args(args),
    )
}

fn output(cmd: &mut Command) -> (i32, String, String) {
    let out = cmd.stdin(Stdio::null()).output().expect("run phux command");
    (
        out.status.code().expect("phux exited with code"),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Ambient budget for a fake agent's `/bin/sh` to run at all (a loaded box
/// has taken ~8s); `phux wait` returns as soon as the line appears.
const AGENT_PAINT_BUDGET_SECS: &str = "60";

/// Subject budget for a specific line once the agent is painting: it writes
/// every line in one burst, so a timeout here means the argv is wrong.
const AGENT_LINE_BUDGET_SECS: &str = "5";

/// Wait for `target` to paint at all (ambient budget), then for `marker`
/// (subject budget), so load cannot masquerade as a contract failure.
fn wait_painted_then(socket: &str, target: &str, marker: &str) {
    let (code, _, stderr) = run(&[
        "wait",
        "--until",
        "FAKE_AGENT_ARGS=",
        "--timeout",
        AGENT_PAINT_BUDGET_SECS,
        "--socket",
        socket,
        target,
    ]);
    assert_eq!(
        code, 0,
        "{target} never painted within {AGENT_PAINT_BUDGET_SECS}s (environment, not contract): {stderr}"
    );
    let (code, _, stderr) = run(&[
        "wait",
        "--until",
        marker,
        "--timeout",
        AGENT_LINE_BUDGET_SECS,
        "--socket",
        socket,
        target,
    ]);
    assert_eq!(code, 0, "{target} never printed {marker:?}: {stderr}");
}

/// Poll until `selector` is gone from `socket`'s registry (ambient budget:
/// each poll forks a `phux`).
fn wait_for_terminal_absent(socket: &str, selector: &str) {
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
        path.canonicalize().expect("canonical checkout")
    };
    let first = checkout("one", "same");
    let collision = checkout("two", "same");
    let other = checkout("three", "other");
    let config = common::isolated_xdg().config.join("phux");
    std::fs::create_dir_all(&config).expect("config directory");
    std::fs::write(
        config.join("config.toml"),
        format!(
            "[[projects]]\nname = \"Friendly\"\npath = {:?}\n",
            first.display().to_string()
        ),
    )
    .expect("project catalog");
    let first_path = first.to_str().expect("utf-8 path");
    let collision_path = collision.to_str().expect("utf-8 path");
    let other_path = other.to_str().expect("utf-8 path");
    let project_open = |target: &str| {
        let (code, stdout, stderr) =
            run(&["project", "open", target, "--json", "--socket", &socket]);
        (code, stdout, stderr)
    };
    // The server's preexisting "seed" session has no directory attribute.
    // Neither its pane cwd nor a matching basename is sufficient to adopt it.
    let unbound = checkout("four", "seed");
    let (code, stdout, stderr) = project_open(unbound.to_str().expect("utf-8 path"));
    assert_ne!(code, 0, "untagged session must be refused: {stdout}");
    assert!(stderr.contains("no checkout directory"), "{stderr}");

    let (code, stdout, stderr) = project_open("Friendly");
    assert_eq!(code, 0, "first project open: {stderr}");
    let opened: serde_json::Value = serde_json::from_str(&stdout).expect("open JSON");
    assert_eq!(opened["created"], true);
    assert_eq!(opened["project"], "Friendly");
    let attrs = session_attrs(&socket, "same");
    assert_eq!(attr(&attrs, "directory"), Some(first_path));
    assert_eq!(attr(&attrs, "project"), Some("same"));

    let (code, stdout, stderr) = project_open(first_path);
    assert_eq!(code, 0, "reentry: {stderr}");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&stdout).expect("reentry JSON")["created"],
        false
    );
    let (code, stdout, stderr) = project_open(collision_path);
    assert_ne!(code, 0, "colliding repo must be refused: {stdout}");
    assert!(stdout.is_empty());
    assert!(
        stderr.contains(first_path) && stderr.contains(collision_path),
        "{stderr}"
    );
    assert_eq!(
        attr(&session_attrs(&socket, "same"), "directory"),
        Some(first_path)
    );

    let (code, _, stderr) = project_open(other_path);
    assert_eq!(code, 0, "second distinct project: {stderr}");
    assert_eq!(
        attr(&session_attrs(&socket, "other"), "directory"),
        Some(other_path)
    );
    assert_eq!(
        attr(&session_attrs(&socket, "other"), "project"),
        Some("other")
    );
    assert_project_archive_roundtrip(&root, &socket, &dest, first_path, other_path);
}

fn assert_project_archive_roundtrip(
    root: &tempfile::TempDir,
    socket: &str,
    dest: &common::ServerGuard,
    first_path: &str,
    other_path: &str,
) {
    let archive = root.path().join("projects.json");
    let (code, _, stderr) = run(&[
        "archive",
        "export",
        "--socket",
        socket,
        "--output",
        archive.to_str().expect("utf-8 archive"),
    ]);
    assert_eq!(code, 0, "archive export: {stderr}");
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&archive).expect("read archive"))
            .expect("archive JSON");
    for (name, path, label) in [("same", first_path, "same"), ("other", other_path, "other")] {
        let row = saved["sessions"]
            .as_array()
            .expect("sessions")
            .iter()
            .find(|row| row["name"] == name)
            .expect("archived session");
        assert_eq!(row["cwd"], path);
        assert_eq!(row["project"], label);
    }
    let dest_socket = dest.socket_text();
    let (code, stdout, stderr) = run(&[
        "archive",
        "import",
        archive.to_str().expect("utf-8 archive"),
        "--socket",
        &dest_socket,
    ]);
    assert_eq!(code, 0, "archive import: {stdout} {stderr}");
    assert_eq!(
        attr(&session_attrs(&dest_socket, "same"), "directory"),
        Some(first_path)
    );
    assert_eq!(
        attr(&session_attrs(&dest_socket, "same"), "project"),
        Some("same")
    );
    assert_eq!(
        attr(&session_attrs(&dest_socket, "other"), "directory"),
        Some(other_path)
    );
    assert_eq!(
        attr(&session_attrs(&dest_socket, "other"), "project"),
        Some("other")
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
    let (code, _, stderr) = run(&["project", "trust", path]);
    assert_eq!(code, 0, "trust recipe: {stderr}");
    let (code, stdout, stderr) = open();
    assert_eq!(code, 0, "trusted recipe open: {stderr}");
    let first: serde_json::Value = serde_json::from_str(&stdout).expect("open JSON");
    assert_eq!(first["created"], true);
    let first_id = first["terminal_id"].as_u64().expect("first terminal");
    std::fs::write(&recipe, "[[windows]]\nname = \"changed\"\n").expect("change recipe");
    let selector = format!("@{first_id}");
    let (code, _, stderr) = run(&["kill", "--yes", "--socket", &socket, &selector]);
    assert_eq!(code, 0, "kill first terminal: {stderr}");
    wait_for_terminal_absent(&socket, &selector);
    let (code, stdout, stderr) = open();
    assert_eq!(
        code, 0,
        "reentry seeds shell without rerunning recipe: {stderr}"
    );
    let reopened: serde_json::Value = serde_json::from_str(&stdout).expect("open JSON");
    assert_eq!(reopened["created"], false);
    assert_ne!(reopened["terminal_id"], first["terminal_id"]);
    let canonical = project.canonicalize().expect("canonical project");
    assert_eq!(
        attr(&session_attrs(&socket, "trusted"), "directory"),
        canonical.to_str()
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
    let (code, _, stderr) = run(&["project", "trust", root_path]);
    assert_eq!(code, 0, "trust recipe: {stderr}");
    let source_socket = source.socket_text();
    let (code, stdout, stderr) = run(&[
        "project",
        "open",
        root_path,
        "--json",
        "--socket",
        &source_socket,
    ]);
    assert_eq!(code, 0, "open recipe: {stdout} {stderr}");
    assert_eq!(
        attr(&session_attrs(&source_socket, "site"), "directory"),
        Some(root_path)
    );

    let archive = root.path().join("site.json");
    let archive_path = archive.to_str().expect("utf-8 archive");
    let (code, _, stderr) = run(&[
        "archive",
        "export",
        "--socket",
        &source_socket,
        "--output",
        archive_path,
    ]);
    assert_eq!(code, 0, "export recipe session: {stderr}");
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&archive).expect("read archive")).expect("archive");
    let site = saved["sessions"]
        .as_array()
        .expect("sessions")
        .iter()
        .find(|session| session["name"] == "site")
        .expect("site archive");
    assert_eq!(site["cwd"], root_path, "session identity is checkout root");
    assert_eq!(
        site["windows"][0]["panes"][0]["cwd"], web_path,
        "pane launch cwd is web"
    );

    let dest_socket = dest.socket_text();
    let (code, stdout, stderr) =
        run(&["archive", "import", archive_path, "--socket", &dest_socket]);
    assert_eq!(code, 0, "import: {stdout} {stderr}");
    assert_eq!(
        attr(&session_attrs(&dest_socket, "site"), "directory"),
        Some(root_path)
    );
    let (code, _, stderr) = run(&["restore", "site", "--socket", &dest_socket]);
    assert_eq!(code, 0, "restore site: {stderr}");
    let resaved = root.path().join("resaved.json");
    let (code, _, stderr) = run(&[
        "archive",
        "export",
        "--socket",
        &dest_socket,
        "--output",
        resaved.to_str().expect("utf-8 archive"),
    ]);
    assert_eq!(code, 0, "export restored site: {stderr}");
    let restored: serde_json::Value =
        serde_json::from_slice(&std::fs::read(resaved).expect("read restored archive"))
            .expect("archive");
    let site = restored["sessions"]
        .as_array()
        .expect("sessions")
        .iter()
        .find(|session| session["name"] == "site")
        .expect("restored site");
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
    let selector = format!("@{first_id}");
    let (code, _, stderr) = run(&["kill", "--yes", "--socket", &socket, &selector]);
    assert_eq!(code, 0, "kill terminal: {stderr}");
    wait_for_terminal_absent(&socket, &selector);
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

fn parallel_project_opens(socket: &str, path: &str, count: usize) -> Vec<serde_json::Value> {
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
                    serde_json::from_str::<serde_json::Value>(&stdout).expect("open JSON")
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("open thread"))
            .collect()
    })
}

fn live_terminals(socket: &str, name: &str) -> Vec<u32> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        let mut conn =
            phux_client::attach::connection::Connection::connect(std::path::Path::new(socket))
                .await
                .expect("connect organization");
        let records =
            phux_client::org::fetch(&mut conn, phux_protocol::wire::frame::OrgFilter::All, 1)
                .await
                .expect("organization");
        let session = records.session_named(name).expect("session");
        let ids = records
            .members(&session.key)
            .filter_map(|resource| resource.live.as_ref()?.local_id())
            .collect();
        drop(conn);
        ids
    })
}

fn session_attrs(socket: &str, name: &str) -> Vec<(String, String)> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        let mut conn =
            phux_client::attach::connection::Connection::connect(std::path::Path::new(socket))
                .await
                .expect("connect organization");
        let records =
            phux_client::org::fetch(&mut conn, phux_protocol::wire::frame::OrgFilter::All, 1)
                .await
                .expect("organization records");
        let attrs = records
            .session_named(name)
            .expect("named organization session")
            .attrs
            .clone();
        drop(conn);
        attrs
    })
}

fn arranged_terminal(socket: &str, name: &str, terminal_id: u32) -> bool {
    use phux_protocol::wire::frame::{ArrangementNode, EntryTarget, OrgFilter};
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        let mut conn =
            phux_client::attach::connection::Connection::connect(std::path::Path::new(socket))
                .await
                .expect("connect organization");
        let records = phux_client::org::fetch(&mut conn, OrgFilter::All, 1)
            .await
            .expect("organization records");
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
        let document = phux_client::org::arrangement(
            &mut conn,
            3,
            session.arrangement.expect("session arrangement"),
        )
        .await
        .expect("read arrangement")
        .document;
        drop(conn);
        document.groups.iter().any(|group| {
            matches!(&group.tree, ArrangementNode::Leaf(entry)
                if entry.target == EntryTarget::Member(resource.key))
        })
    })
}

fn attr<'a>(attrs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.as_str())
}

/// `archive export` archives a session's real L3 split layout (not the flat
/// fallback), and export -> import -> restore -> export keeps the same split.
#[test]
#[ignore = "spawns real phux servers; run explicitly when validating workspace archives."]
#[allow(
    clippy::too_many_lines,
    reason = "one linear save-restore-resave round trip keeps its own assertions together"
)]
fn archive_export_captures_a_real_split_tree_and_restore_replays_it() {
    let source = start("source");
    let dest = start("seed");
    let archive_dir = tempfile::tempdir().expect("archive tempdir");
    let source_archive = archive_dir.path().join("source.json");
    let dest_archive = archive_dir.path().join("dest.json");
    let cwd = archive_dir.path().to_string_lossy().into_owned();
    let source_socket = source.socket_text();
    let dest_socket = dest.socket_text();

    let (code, stdout, stderr) = run(&[
        "new",
        "--socket",
        &source_socket,
        "--json",
        "-s",
        "split-bench",
        "--cwd",
        &cwd,
    ]);
    assert_eq!(code, 0, "create split-bench session failed: {stderr}");
    let created: serde_json::Value = serde_json::from_str(&stdout).expect("new --json");
    let seed_pane = created["terminal_id"]
        .as_u64()
        .expect("new --json names the seed pane");
    let seed_selector = format!("@{seed_pane}");

    // A placed pane writes a real two-leaf split into the layout envelope.
    let (code, _stdout, stderr) = run(&[
        "spawn",
        "--socket",
        &source_socket,
        "--target",
        &seed_selector,
        "--split",
        "vertical",
        "--",
        "sleep",
        "30",
    ]);
    assert_eq!(code, 0, "placed spawn failed: {stderr}");

    let (code, stdout, stderr) = run(&[
        "archive",
        "export",
        "--socket",
        &source_socket,
        "--output",
        &source_archive.to_string_lossy(),
    ]);
    assert_eq!(code, 0, "archive export failed: {stderr}");
    assert!(stdout.is_empty());

    let saved: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&source_archive).expect("read saved archive"),
    )
    .expect("saved archive JSON");
    let split_window = split_bench_window(&saved);
    assert_eq!(
        split_window["panes"].as_array().expect("panes array").len(),
        2,
        "the saved window must carry both panes, not just the seed: {split_window}"
    );
    assert_eq!(
        split_window["layout"]["kind"], "split",
        "the saved layout must be the real two-leaf split, not the bare-pane fallback: {split_window}"
    );

    let (code, stdout, stderr) = run(&[
        "archive",
        "import",
        &source_archive.to_string_lossy(),
        "--socket",
        &dest_socket,
    ]);
    assert_eq!(code, 0, "archive import failed: {stderr}");
    let summary: serde_json::Value = serde_json::from_str(&stdout).expect("restore summary JSON");
    assert_eq!(summary["schema_version"], 2);
    assert!(
        summary["failed"].as_array().is_none_or(Vec::is_empty),
        "restore must not report a failure: {summary}"
    );
    assert!(
        summary["restored"]
            .as_array()
            .expect("restored array")
            .iter()
            .any(|name| name == "split-bench")
    );
    // An import starts nothing (R16); `phux restore` starts the imported
    // dormant panes.
    let (code, _, stderr) = run(&["restore", "split-bench", "--socket", &dest_socket]);
    assert_eq!(code, 0, "restore split-bench failed: {stderr}");

    let (code, stdout, stderr) = run(&[
        "archive",
        "export",
        "--socket",
        &dest_socket,
        "--output",
        &dest_archive.to_string_lossy(),
    ]);
    assert_eq!(code, 0, "archive export (post-restore) failed: {stderr}");
    assert!(stdout.is_empty());
    let resaved: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&dest_archive).expect("read resaved archive"),
    )
    .expect("resaved archive JSON");
    let resaved_window = split_bench_window(&resaved);
    assert_eq!(
        resaved_window["panes"]
            .as_array()
            .expect("panes array")
            .len(),
        2,
        "the round-tripped window must still carry both panes: {resaved_window}"
    );
    assert_eq!(
        resaved_window["layout"]["kind"], "split",
        "the round-tripped layout must still be a real split, not flattened: {resaved_window}"
    );
}

/// Importing an archive whose session is on host `edge`, in `/src/api`, and
/// tagged `phux` keeps that organization. The directory does not have to
/// exist: the child falls back, and the registry still records the request.
#[test]
#[ignore = "spawns real phux servers; run explicitly when validating workspace archives."]
fn import_keeps_edge_host_src_api_directory_and_phux_project() {
    let dest = start("edge");
    let archive_dir = tempfile::tempdir().expect("archive tempdir");
    let archive = archive_dir.path().join("edge.json");
    let resaved = archive_dir.path().join("resaved.json");
    std::fs::write(
        &archive,
        r#"{
            "schema_version": 2,
            "sessions": [
                {
                    "name": "api",
                    "host": "edge",
                    "cwd": "/src/api",
                    "project": "phux",
                    "windows": []
                }
            ]
        }"#,
    )
    .expect("write archive");
    let socket = dest.socket_text();

    let (code, stdout, stderr) = run(&["archive", "import", "--plan", &archive.to_string_lossy()]);
    assert_eq!(code, 0, "restore --plan failed: {stderr}");
    assert!(
        stdout.contains("api host=edge directory=/src/api project=phux"),
        "plan must print the archived organization: {stdout}"
    );

    let (code, stdout, stderr) = run(&[
        "archive",
        "import",
        &archive.to_string_lossy(),
        "--socket",
        &socket,
    ]);
    assert_eq!(code, 0, "archive import failed: {stderr} {stdout}");
    let summary: serde_json::Value = serde_json::from_str(&stdout).expect("restore summary JSON");
    assert!(
        summary["failed"].as_array().is_none_or(Vec::is_empty),
        "restore must not report a failure: {summary} {stderr}"
    );
    assert!(
        summary["restored"]
            .as_array()
            .expect("restored array")
            .iter()
            .any(|name| name == "api"),
        "api must be restored: {summary}"
    );

    let attrs = session_attrs(&socket, "api");
    assert_eq!(attr(&attrs, "host"), Some("edge"));
    assert_eq!(attr(&attrs, "directory"), Some("/src/api"));
    assert_eq!(attr(&attrs, "project"), Some("phux"));

    let (code, _stdout, stderr) = run(&[
        "archive",
        "export",
        "--socket",
        &socket,
        "--output",
        &resaved.to_string_lossy(),
    ]);
    assert_eq!(code, 0, "archive export after restore failed: {stderr}");
    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&resaved).expect("read resaved archive"))
            .expect("resaved archive JSON");
    let session = saved["sessions"]
        .as_array()
        .expect("sessions")
        .iter()
        .find(|session| session["name"] == "api")
        .expect("api session was saved");
    assert_eq!(
        session["host"], "edge",
        "host tag was not applied: {session}"
    );
    assert_eq!(
        session["cwd"], "/src/api",
        "archived directory was not recorded: {session}"
    );
    assert_eq!(
        session["project"], "phux",
        "project tag was not applied: {session}"
    );
}

/// The one window of the archive's `split-bench` session.
fn split_bench_window(archive: &serde_json::Value) -> serde_json::Value {
    let sessions = archive["sessions"].as_array().expect("sessions array");
    let session = sessions
        .iter()
        .find(|session| session["name"] == "split-bench")
        .unwrap_or_else(|| panic!("archive names no split-bench session: {archive}"));
    session["windows"]
        .as_array()
        .and_then(|windows| windows.first())
        .unwrap_or_else(|| panic!("split-bench session has no window: {session}"))
        .clone()
}

/// Named session's windows array from an archive, or panics naming the
/// archive it looked in.
fn session_windows<'a>(archive: &'a serde_json::Value, name: &str) -> &'a Vec<serde_json::Value> {
    archive["sessions"]
        .as_array()
        .expect("sessions array")
        .iter()
        .find(|session| session["name"] == name)
        .unwrap_or_else(|| panic!("archive names no {name:?} session: {archive}"))
        .get("windows")
        .and_then(serde_json::Value::as_array)
        .unwrap_or_else(|| panic!("{name:?} session has no windows array: {archive}"))
}

/// Total pane count across every window of `windows`.
fn total_panes(windows: &[serde_json::Value]) -> usize {
    windows
        .iter()
        .map(|window| window["panes"].as_array().map_or(0, std::vec::Vec::len))
        .sum()
}

/// A stored layout can miss live panes (a headless spawn never touches it)
/// and keep leaves of closed panes (nothing prunes them). `archive export`
/// must drop the dead leaf and put the headless pane in an `"unplaced"`
/// window with a warning, so export -> import -> restore -> export keeps
/// exactly the two live panes.
#[test]
#[ignore = "spawns real phux servers; run explicitly when validating workspace archives."]
#[allow(
    clippy::too_many_lines,
    reason = "one linear repro-save-restore-resave round trip keeps its own assertions together"
)]
fn archive_export_reconciles_a_dead_layout_leaf_and_a_headless_spawn() {
    // One session only, so the headless spawn can join nothing else; its
    // seed pane is always `@1`.
    let source = start("reconcile-repro");
    let dest = start("seed");
    let archive_dir = tempfile::tempdir().expect("archive tempdir");
    let source_archive = archive_dir.path().join("reconcile-source.json");
    let dest_archive = archive_dir.path().join("reconcile-dest.json");
    let source_socket = source.socket_text();
    let dest_socket = dest.socket_text();
    let seed_selector = "@1";

    // A placed pane, then killed: a dead layout leaf.
    let (code, stdout, stderr) = run(&[
        "spawn",
        "--socket",
        &source_socket,
        "--target",
        seed_selector,
        "--split",
        "vertical",
        "--json",
        "--",
        "sleep",
        "30",
    ]);
    assert_eq!(code, 0, "placed spawn failed: {stderr}");
    let placed: serde_json::Value = serde_json::from_str(&stdout).expect("spawn --json");
    let placed_id = placed["terminal_id"]
        .as_u64()
        .expect("spawn --json names the placed pane");
    let placed_selector = format!("@{placed_id}");
    let (code, _, stderr) = run(&[
        "kill",
        "--yes",
        "--socket",
        &source_socket,
        &placed_selector,
    ]);
    assert_eq!(code, 0, "kill the placed pane before save: {stderr}");
    wait_for_terminal_absent(&source_socket, &placed_selector);

    // A headless spawn joins the session but never touches its layout.
    let (code, _stdout, stderr) = run(&["spawn", "--socket", &source_socket, "--", "sleep", "30"]);
    assert_eq!(code, 0, "headless spawn failed: {stderr}");

    let (code, stdout, stderr) = run(&[
        "archive",
        "export",
        "--socket",
        &source_socket,
        "--output",
        &source_archive.to_string_lossy(),
    ]);
    assert_eq!(code, 0, "archive export failed: {stderr}");
    assert!(stdout.is_empty());
    assert!(
        stderr.contains("reconcile-repro") && stderr.contains("absent from its stored layout"),
        "save must warn about the unplaced headless pane naming the session: {stderr}"
    );

    let saved: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&source_archive).expect("read saved archive"),
    )
    .expect("saved archive JSON");
    let windows = session_windows(&saved, "reconcile-repro");
    assert_eq!(
        total_panes(windows),
        2,
        "archive must hold exactly the seed pane and the headless spawn — the dead leaf \
         dropped, the headless pane not lost: {windows:?}"
    );
    let unplaced = windows
        .iter()
        .find(|window| window["name"] == "unplaced")
        .unwrap_or_else(|| {
            panic!("the headless pane must land in a synthesized unplaced window: {windows:?}")
        });
    assert_eq!(
        unplaced["panes"].as_array().expect("panes array").len(),
        1,
        "unplaced window must hold exactly the one pane the layout never named: {unplaced}"
    );
    let seed_window = windows
        .iter()
        .find(|window| window["name"] != "unplaced")
        .expect("the seed's own window is still present");
    assert_eq!(
        seed_window["panes"].as_array().expect("panes array").len(),
        1,
        "the dead leaf must be pruned from the seed's own window, collapsing its split: \
         {seed_window}"
    );

    let (code, stdout, stderr) = run(&[
        "archive",
        "import",
        &source_archive.to_string_lossy(),
        "--socket",
        &dest_socket,
    ]);
    assert_eq!(code, 0, "archive import failed: {stderr}");
    let summary: serde_json::Value = serde_json::from_str(&stdout).expect("restore summary JSON");
    assert!(
        summary["failed"].as_array().is_none_or(Vec::is_empty),
        "restore must not report a failure: {summary}"
    );
    // An import starts nothing (R16); `phux restore` starts the imported
    // dormant panes.
    let (code, _, stderr) = run(&["restore", "reconcile-repro", "--socket", &dest_socket]);
    assert_eq!(code, 0, "restore reconcile-repro failed: {stderr}");

    let (code, stdout, stderr) = run(&[
        "archive",
        "export",
        "--socket",
        &dest_socket,
        "--output",
        &dest_archive.to_string_lossy(),
    ]);
    assert_eq!(code, 0, "archive export (post-restore) failed: {stderr}");
    assert!(stdout.is_empty());
    let resaved: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&dest_archive).expect("read resaved archive"),
    )
    .expect("resaved archive JSON");
    let resaved_windows = session_windows(&resaved, "reconcile-repro");
    assert_eq!(
        total_panes(resaved_windows),
        2,
        "the restored session must carry exactly two live panes, matching the source: \
         {resaved_windows:?}"
    );
}

#[test]
#[ignore = "spawns real phux servers; run explicitly when validating workspace archives."]
fn import_then_restore_starts_archived_command_process() {
    let dest = start("seed");
    let archive_dir = tempfile::tempdir().expect("archive tempdir");
    let archive_path = archive_dir.path().join("workspace-command.json");
    let cwd = archive_dir.path().to_string_lossy().into_owned();
    let marker = format!(
        "PHUX_RESTORED_PROCESS_{}_{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed),
    );
    let command = vec![
        "sh".to_owned(),
        "-lc".to_owned(),
        format!("printf '%s\\nPWD=%s\\n' {marker} \"$PWD\"; sleep 30"),
    ];
    let archive = serde_json::json!({
        "schema_version": 1,
        "sessions": [
            {
                "name": "restored-proc",
                "active": true,
                "cwd": cwd,
                "command": command,
                "windows": [
                    {
                        "name": "main",
                        "active": true,
                        "layout": { "kind": "pane", "pane": 0 },
                        "panes": [
                            {
                                "active": true,
                                "cwd": cwd,
                                "command": command,
                                "cols": 80,
                                "rows": 24
                            }
                        ]
                    }
                ]
            }
        ]
    });
    std::fs::write(
        &archive_path,
        serde_json::to_string_pretty(&archive).expect("render archive"),
    )
    .expect("write archive");
    let archive_arg = archive_path.to_string_lossy().into_owned();
    let socket_arg = dest.socket_text();

    let (code, stdout, stderr) = run(&["archive", "import", &archive_arg, "--socket", &socket_arg]);
    assert_eq!(code, 0, "archive import failed: {stderr}");
    let summary: serde_json::Value = serde_json::from_str(&stdout).expect("restore summary JSON");
    assert!(
        summary["restored"]
            .as_array()
            .expect("restored array")
            .iter()
            .any(|name| name == "restored-proc")
    );
    // An import starts nothing (R16); `phux restore` starts the imported
    // dormant panes.
    let (code, _, stderr) = run(&["restore", "restored-proc", "--socket", &socket_arg]);
    assert_eq!(code, 0, "restore restored-proc failed: {stderr}");

    // Ambient budget: the marker is unique, so only load can expire it.
    let (code, _stdout, stderr) = run(&[
        "wait",
        "--until",
        &marker,
        "--timeout",
        AGENT_PAINT_BUDGET_SECS,
        "--socket",
        &socket_arg,
        "restored-proc",
    ]);
    assert_eq!(code, 0, "restored command marker did not appear: {stderr}");

    let (code, stdout, stderr) = run(&[
        "snapshot",
        "--json",
        "--socket",
        &socket_arg,
        "restored-proc",
    ]);
    assert_eq!(code, 0, "snapshot after restore failed: {stderr}");
    assert!(
        stdout.contains(&marker),
        "snapshot should show restored command output"
    );
    assert!(
        stdout.contains(&cwd),
        "snapshot should show restored command cwd"
    );
}

fn write_fake_agent_plugin(root: &std::path::Path) -> PathBuf {
    let plugin = root.join("plugin");
    let integrations = plugin.join("integrations");
    let scripts = plugin.join("scripts");
    let xdg = root.join("xdg");
    std::fs::create_dir_all(&integrations).expect("create integrations");
    std::fs::create_dir_all(&scripts).expect("create scripts");
    std::fs::create_dir_all(xdg.join("phux")).expect("create config");
    std::fs::write(
        plugin.join("phux-plugin.toml"),
        r#"id = "com.phux.test.restore"
name = "Restore test agent"
version = "1.0.0"
min_phux_version = "0.0.2"
platforms = ["linux", "macos"]
"#,
    )
    .expect("write manifest");
    std::fs::write(
        integrations.join("restore-agent.toml"),
        r#"schema_version = 1
id = "restore-agent"
display_name = "Restore Agent"
kind = "terminal-agent"
first_party = true

[session_identity]
mode = "native-or-phux"
native_env = "PHUX_FAKE_SESSION_ID"
restore = "external-cli"
resume_args = ["--resume", "${PHUX_AGENT_SESSION_ID}"]
fresh_args = ["--new", "${PHUX_AGENT_SESSION_ID}"]

[launch]
command = ["${PHUX_PLUGIN_ROOT}/scripts/fake-agent.sh"]
working_directory = "plugin-root"
"#,
    )
    .expect("write integration");
    // Claims the `fakeprov` provider that a shell-started agent's
    // `AgentSession` names, so save can bridge it (ADR-0151).
    std::fs::write(
        integrations.join("shell-resume-agent.toml"),
        r#"schema_version = 1
id = "shell-resume-agent"
display_name = "Shell-started resume agent"
kind = "terminal-agent"
first_party = true

[session_identity]
mode = "native-or-phux"
native_env = "PHUX_FAKE_SESSION_ID"
restore = "external-cli"
resume_args = ["--resume", "${PHUX_AGENT_SESSION_ID}"]

[agent_identity]
name = "fakeprov"
kind = "fakeprov"

[launch]
command = ["${PHUX_PLUGIN_ROOT}/scripts/fake-agent.sh"]
working_directory = "workspace"
"#,
    )
    .expect("write shell-started integration");
    std::fs::write(
        scripts.join("fake-agent.sh"),
        r#"#!/bin/sh
printf 'FAKE_AGENT_ARGS=%s\n' "$*"
printf 'FAKE_AGENT_ENV=%s\n' "$PHUX_FAKE_SESSION_ID"
printf 'FAKE_AGENT_CWD=%s\n' "$PWD"
exec sleep 60
"#,
    )
    .expect("write fake agent");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let script = scripts.join("fake-agent.sh");
        let mut permissions = std::fs::metadata(&script)
            .expect("stat fake agent")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(script, permissions).expect("make fake agent executable");
    }
    std::fs::write(
        xdg.join("phux/config.toml"),
        format!(
            "[[plugins]]\nmanifest = {:?}\nenabled = true\n",
            plugin.join("phux-plugin.toml").to_string_lossy()
        ),
    )
    .expect("write config");
    xdg
}

#[test]
#[ignore = "spawns real phux servers; run explicitly when validating workspace archives."]
#[allow(
    clippy::too_many_lines,
    reason = "one linear process-lifecycle scenario keeps restart and stale-owner assertions together"
)]
fn native_agent_session_is_replayed_after_pane_restart_and_rejects_stale_ownership() {
    let root = tempfile::tempdir().expect("restore test tempdir");
    let xdg = write_fake_agent_plugin(root.path());
    let source_archive = root.path().join("source.json");
    let replay_archive = root.path().join("replay.json");
    let stale_archive = root.path().join("stale.json");
    let source_archive_arg = source_archive.to_string_lossy().into_owned();
    let replay_archive_arg = replay_archive.to_string_lossy().into_owned();
    let stale_archive_arg = stale_archive.to_string_lossy().into_owned();

    let source = start("agent-restart");
    let source_socket = source.socket_text();
    let (code, stdout, stderr) = run_with_xdg(
        &[
            "launch",
            "restore-agent",
            "--socket",
            &source_socket,
            "--json",
        ],
        &xdg,
    );
    assert_eq!(code, 0, "fresh agent launch failed: {stderr}");
    let launch: serde_json::Value = serde_json::from_str(&stdout).expect("launch JSON");
    assert_eq!(launch["integration"], "restore-agent");
    let launched_id = launch["terminal_id"].as_u64().expect("terminal id");
    let launched_selector = format!("@{launched_id}");
    wait_painted_then(&source_socket, &launched_selector, "FAKE_AGENT_ARGS=--new");
    let (code, _, stderr) = run(&["kill", "--yes", "--socket", &source_socket, "@1"]);
    assert_eq!(
        code, 0,
        "remove the pre-agent seed pane before save: {stderr}"
    );
    wait_for_terminal_absent(&source_socket, "@1");
    let (code, _, stderr) = run_with_xdg(
        &[
            "archive",
            "export",
            "--socket",
            &source_socket,
            "--output",
            &source_archive_arg,
        ],
        &xdg,
    );
    assert_eq!(code, 0, "agent archive save failed: {stderr}");

    let mut archived: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&source_archive).expect("read source archive"))
            .expect("parse source archive");
    assert_eq!(archived["schema_version"], 2);
    let agent = archived["sessions"]
        .as_array()
        .expect("sessions")
        .iter()
        .find(|session| session["name"] == "agent-restart")
        .and_then(|session| session["windows"][0]["panes"][0]["agent_session"].as_object())
        .expect("saved agent session");
    let native_id = agent["native_id"].as_str().expect("native id").to_owned();
    assert!(
        uuid::Uuid::parse_str(&native_id).is_ok(),
        "fresh identity is a UUID: {native_id}"
    );
    archived["sessions"]
        .as_array_mut()
        .expect("sessions")
        .iter_mut()
        .find(|session| session["name"] == "agent-restart")
        .expect("agent session")["windows"][0]["panes"][0]["cwd"] =
        serde_json::Value::String("/archived/cwd/must-not-win".to_owned());
    std::fs::write(
        &source_archive,
        serde_json::to_vec_pretty(&archived).expect("render cwd-edited archive"),
    )
    .expect("write cwd-edited archive");
    drop(source);

    let dest = start("replay-seed");
    let dest_socket = dest.socket_text();
    let (code, stdout, stderr) = run_with_xdg(
        &[
            "archive",
            "import",
            &source_archive_arg,
            "--socket",
            &dest_socket,
        ],
        &xdg,
    );
    assert_eq!(code, 0, "native restore failed: {stderr}");
    let summary: serde_json::Value = serde_json::from_str(&stdout).expect("restore summary");
    assert!(
        summary["restored"]
            .as_array()
            .expect("restored")
            .iter()
            .any(|name| name == "agent-restart")
    );
    // An import starts nothing (R16); `phux restore` starts the imported
    // dormant panes.
    let (code, _, stderr) = run_with_xdg(
        &["restore", "agent-restart", "--socket", &dest_socket],
        &xdg,
    );
    assert_eq!(code, 0, "restore agent-restart failed: {stderr}");
    let resume_marker = format!("FAKE_AGENT_ARGS=--resume {native_id}");
    wait_painted_then(&dest_socket, "agent-restart", &resume_marker);
    let plugin_cwd = root
        .path()
        .join("plugin")
        .canonicalize()
        .expect("canonical plugin root");
    let (code, _, stderr) = run(&[
        "wait",
        "--until",
        &format!("FAKE_AGENT_CWD={}", plugin_cwd.display()),
        "--timeout",
        AGENT_LINE_BUDGET_SECS,
        "--socket",
        &dest_socket,
        "agent-restart",
    ]);
    assert_eq!(
        code, 0,
        "restore must use current integration working directory: {stderr}"
    );

    let (code, _, stderr) = run_with_xdg(
        &[
            "archive",
            "export",
            "--socket",
            &dest_socket,
            "--output",
            &replay_archive_arg,
        ],
        &xdg,
    );
    assert_eq!(code, 0, "replayed archive save failed: {stderr}");
    let replayed: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&replay_archive).expect("read replay archive"))
            .expect("parse replay archive");
    let replayed_id = replayed["sessions"]
        .as_array()
        .expect("sessions")
        .iter()
        .find(|session| session["name"] == "agent-restart")
        .and_then(|session| {
            session["windows"][0]["panes"][0]["agent_session"]["native_id"].as_str()
        });
    assert_eq!(replayed_id, Some(native_id.as_str()));

    let mut stale = archived;
    let stale_agent = stale["sessions"]
        .as_array_mut()
        .expect("sessions")
        .iter_mut()
        .find(|session| session["name"] == "agent-restart")
        .and_then(|session| session["windows"][0]["panes"][0]["agent_session"].as_object_mut())
        .expect("saved agent session");
    stale_agent.insert(
        "plugin_id".to_owned(),
        serde_json::Value::String("com.phux.wrong-owner".to_owned()),
    );
    std::fs::write(
        &stale_archive,
        serde_json::to_vec_pretty(&stale).expect("render stale archive"),
    )
    .expect("write stale archive");
    let stale_dest = start("stale-seed");
    let stale_socket = stale_dest.socket_text();
    let (code, _, stderr) = run_with_xdg(
        &[
            "archive",
            "import",
            &stale_archive_arg,
            "--socket",
            &stale_socket,
        ],
        &xdg,
    );
    assert_eq!(code, 1, "stale owner must fail closed");
    assert!(
        stderr.contains("not owning plugin"),
        "ownership refusal must be explicit: {stderr}"
    );
    let (code, stdout, stderr) = run(&["ls", "--json", "--socket", &stale_socket]);
    assert_eq!(code, 0, "list stale destination: {stderr}");
    let listing: serde_json::Value = serde_json::from_str(&stdout).expect("listing JSON");
    assert!(
        !listing["sessions"]
            .as_array()
            .expect("sessions")
            .iter()
            .any(|session| session["name"] == "agent-restart"),
        "ownership mismatch must not create the archived session"
    );
}

/// ADR-0151: an agent started inside an existing shell has no launch record,
/// only a live `AgentSession` with a native id. Save bridges it into the
/// archived resume record through the integration claiming its provider, and
/// restore resumes it through that integration's current argv.
#[test]
#[ignore = "spawns real phux servers; run explicitly when validating workspace archives."]
fn shell_started_agent_session_is_archived_and_resumed() {
    let root = tempfile::tempdir().expect("restore test tempdir");
    let xdg = write_fake_agent_plugin(root.path());
    let archive = root.path().join("shell-agent.json");
    let archive_arg = archive.to_string_lossy().into_owned();

    let source = start("shell-agent");
    let source_socket = source.socket_text();
    let (code, _, stderr) = run(&[
        "agent",
        "session",
        "open",
        "@1",
        "--provider",
        "fakeprov",
        "--native-id",
        "conv-7",
        "--socket",
        &source_socket,
    ]);
    assert_eq!(code, 0, "open the shell-started agent session: {stderr}");
    let (code, _, stderr) = run_with_xdg(
        &[
            "archive",
            "export",
            "--socket",
            &source_socket,
            "--output",
            &archive_arg,
        ],
        &xdg,
    );
    assert_eq!(code, 0, "archive save failed: {stderr}");
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&archive).expect("read archive"))
            .expect("parse archive");
    let agent = saved["sessions"]
        .as_array()
        .expect("sessions")
        .iter()
        .find(|session| session["name"] == "shell-agent")
        .map(|session| session["windows"][0]["panes"][0]["agent_session"].clone())
        .expect("archived session");
    assert_eq!(
        agent,
        serde_json::json!({
            "plugin_id": "com.phux.test.restore",
            "integration_id": "shell-resume-agent",
            "native_id": "conv-7",
        }),
        "the live session is archived as the inert resume record"
    );
    assert!(
        !saved.to_string().contains("fake-agent.sh"),
        "the archive never carries executable argv: {saved}"
    );
    drop(source);

    let dest = start("shell-agent-seed");
    let dest_socket = dest.socket_text();
    let (code, _, stderr) = run_with_xdg(
        &["archive", "import", &archive_arg, "--socket", &dest_socket],
        &xdg,
    );
    assert_eq!(code, 0, "native restore failed: {stderr}");
    // An import starts nothing (R16); `phux restore` starts the imported
    // dormant panes.
    let (code, _, stderr) =
        run_with_xdg(&["restore", "shell-agent", "--socket", &dest_socket], &xdg);
    assert_eq!(code, 0, "restore shell-agent failed: {stderr}");
    wait_painted_then(
        &dest_socket,
        "shell-agent",
        "FAKE_AGENT_ARGS=--resume conv-7",
    );
}
