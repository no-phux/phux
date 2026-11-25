//! `[restore] scrollback` against a real server: `kill -9`, a restart, and
//! the old lines above a divider, never on the new screen.

#![allow(clippy::expect_used, clippy::panic, reason = "tests")]

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::common;
use crate::hermetic;

/// The server saves two seconds after output settles.
const BUDGET: Duration = Duration::from_secs(30);

fn start(dir: &Path) -> hermetic::Server {
    let socket = dir.join("s.sock");
    let server = hermetic::start_server(
        dir,
        &["--no-seed"],
        &[("HOME", dir.to_str().expect("UTF-8"))],
    );
    assert!(common::wait_until_accepting(&socket));
    server
}

fn phux(dir: &Path, args: &[&str]) -> serde_json::Value {
    let env = [("PHUX_SOCKET", dir.join("s.sock"))];
    let out = hermetic::await_success(dir, args, &env);
    serde_json::from_slice(&out.stdout).unwrap_or_default()
}

/// The kept files, wherever under the state directory.
fn kept(dir: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, found: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, found);
            } else if path
                .parent()
                .and_then(Path::extension)
                .is_some_and(|ext| ext == "scrollback")
            {
                found.push(path);
            }
        }
    }
    let mut found = Vec::new();
    walk(&dir.join("state"), &mut found);
    found
}

fn lines(screen: &serde_json::Value, field: &str) -> Vec<String> {
    screen[field]
        .as_array()
        .expect("lines")
        .iter()
        .filter_map(|line| line.as_str().filter(|line| !line.is_empty()))
        .map(str::to_owned)
        .collect()
}

#[test]
#[ignore = "restarts a real server; e2e lane"]
fn a_sigkilled_servers_lines_come_back_above_a_divider() {
    let root = tempfile::tempdir().expect("tempdir");
    let dir = root.path().canonicalize().expect("canonical");
    hermetic::prepare_dirs(&dir);
    let config = dir.join("config/phux/config.toml");

    // Off: the same pane writes nothing.
    let mut server = start(&dir);
    let run = [
        "new",
        "--json",
        "-s",
        "work",
        "--",
        "/bin/sh",
        "-c",
        "echo run-$$; [ -e again ] || { touch again; seq 40 80; }; exec sleep 600",
    ];
    phux(&dir, &run);
    std::thread::sleep(Duration::from_secs(4));
    assert_eq!(kept(&dir), Vec::<PathBuf>::new());
    phux(&dir, &["kill", "work", "--yes", "--json"]);
    server.sigkill();
    drop(server);
    std::fs::remove_file(dir.join("again")).expect("the pane ran");

    std::fs::write(&config, "[restore]\nscrollback = 100\n").expect("config");
    let mut server = start(&dir);
    phux(&dir, &run);
    common::until(BUDGET, || {
        let files = kept(&dir);
        let text = std::fs::read_to_string(files.first()?).ok()?;
        text.contains("80").then_some(())
    })
    .expect("saved once quiet");
    server.sigkill();
    drop(server);

    let mut server = start(&dir);
    phux(&dir, &["restore", "--json"]);
    let screen = common::until(BUDGET, || {
        let screen = phux(&dir, &["snapshot", "work", "--scrollback", "--json"]);
        lines(&screen, "lines")
            .iter()
            .any(|line| line.starts_with("run-"))
            .then_some(screen)
    })
    .expect("relaunched");
    let history = lines(&screen, "scrollback");
    let (divider, old) = history.split_last().expect("history");
    assert!(
        divider.starts_with("-- before the restart, "),
        "{history:?}"
    );
    assert!(old[0].starts_with("run-"), "{history:?}");
    assert_eq!(
        old[1..],
        (40..=80).map(|n| n.to_string()).collect::<Vec<_>>()
    );
    assert_eq!(
        lines(&screen, "lines").len(),
        1,
        "new output only: {screen}"
    );

    assert_eq!(kept(&dir).len(), 1);
    phux(&dir, &["kill", "work", "--yes", "--json"]);
    common::until(BUDGET, || kept(&dir).is_empty().then_some(())).expect("deleted with its pane");
    server.sigkill();
}
