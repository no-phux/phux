//! The browser's published socket contract against the actual CLI/server.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod common;
#[path = "common/runner.rs"]
mod runner;

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::os::unix::fs::MetadataExt as _;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

fn read_file(path: &Path) -> String {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Ok(text) = std::fs::read_to_string(path)
            && !text.is_empty()
        {
            return text;
        }
        assert!(
            Instant::now() < deadline,
            "no contents at {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn rpc(socket: &Path, method: &str, params: Value, cwd: &Path) -> Value {
    let mut stream = UnixStream::connect(socket).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut request = json!({"id":"test-request", "method":method,
        "caller":{"pid":std::process::id(), "cwd":cwd}});
    request["params"] = params;
    writeln!(stream, "{request}").unwrap();
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).unwrap();
    let reply: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(reply["id"], "test-request");
    reply
}

fn result(socket: &Path, method: &str, params: Value, cwd: &Path) -> Value {
    let reply = rpc(socket, method, params, cwd);
    assert!(reply.get("error").is_none(), "{method}: {reply}");
    reply["result"].clone()
}

fn fixture() -> (common::ServerGuard, tempfile::TempDir, PathBuf, String) {
    let dir = tempfile::tempdir().unwrap();
    let endpoint = dir.path().join("endpoint");
    let tty = dir.path().join("tty");
    let command = format!(
        "printf '%s\\n' \"$TERMINAL_BROWSER_TERMINAL_SOCKET\" > {}; tty > {}; exec /bin/cat",
        shell_words::quote(endpoint.to_str().unwrap()),
        shell_words::quote(tty.to_str().unwrap())
    );
    let server = common::ServerGuard::builder("terminal-browser")
        .isolated_home()
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .seed_command(command)
        .start();
    let endpoint = PathBuf::from(read_file(&endpoint).trim());
    let tty = read_file(&tty).trim().to_owned();
    (server, dir, endpoint, tty)
}

#[test]
fn browser_discovers_splits_and_routes_input_without_an_outer_terminal() {
    let (server, dir, endpoint, tty) = fixture();
    let cwd = dir.path().join("cwd with spaces");
    std::fs::create_dir(&cwd).unwrap();
    let cwd = cwd.canonicalize().unwrap();
    let hello = result(&endpoint, "hello", json!({"protocol":1}), &cwd);
    assert_eq!(hello["name"], "phux");
    assert!(
        !hello["methods"]
            .as_array()
            .unwrap()
            .contains(&json!("focusPane"))
    );
    let pane = result(&endpoint, "getCurrentPane", json!({"tty":tty}), &cwd)["pane"].clone();
    assert!(pane["id"].as_str().unwrap().starts_with('@'));
    let initial = result(&endpoint, "listPanes", json!({"tty":tty}), &cwd);
    assert_eq!(initial["panes"].as_array().unwrap().len(), 1);
    assert_eq!(initial["panes"][0]["tab"], pane["tab"]);
    assert_eq!(initial["panes"][0]["tty"], tty);
    assert_cardinal_splits(&endpoint, &pane, &tty, &cwd);
    let listed = result(&endpoint, "listPanes", json!({}), &cwd);
    assert_eq!(listed["panes"].as_array().unwrap().len(), 5);
    let rejected = rpc(
        &endpoint,
        "split",
        json!({
            "from":{"id":pane["id"],"tab":"stale"}, "direction":"right", "command":["/bin/cat"]
        }),
        &cwd,
    );
    assert!(
        rejected["error"]["message"]
            .as_str()
            .unwrap()
            .contains("tab")
    );
    assert_eq!(
        result(&endpoint, "listPanes", json!({}), &cwd)["panes"],
        listed["panes"]
    );
    assert!(
        rpc(&endpoint, "focusPane", json!({"pane":pane["id"]}), &cwd)
            .get("error")
            .is_some()
    );

    assert_hidden_slot_splits(&server, &endpoint, &pane, &cwd);
    let capture = dir.path().join("input-bytes");
    result(
        &endpoint,
        "split",
        json!({
            "from":pane,"direction":"right","command":["/bin/sh","-c",
                "stty raw -echo; printf '\\033[?2004hTB_INPUT_READY'; dd bs=1 count=18 2>/dev/null | od -An -tx1 > \"$1\"; exec /bin/sleep 60",
                "browser-input",capture]
        }),
        &cwd,
    );
    let input = result(
        &endpoint,
        "neighbor",
        json!({"from":pane,"direction":"right"}),
        &cwd,
    )["pane"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    server.success(&[
        "wait",
        &input,
        "--until",
        "TB_INPUT_READY",
        "--timeout",
        "20",
    ]);
    result(
        &endpoint,
        "sendText",
        json!({"pane":input,"text":"Enter"}),
        &cwd,
    );
    result(&endpoint, "pasteKey", json!({"pane":input}), &cwd);
    let hex = read_file(&capture);
    let bytes: Vec<u8> = hex
        .split_whitespace()
        .map(|b| u8::from_str_radix(b, 16).unwrap())
        .collect();
    assert_eq!(
        bytes, b"\x1b[200~Enter\x1b[201~\x16",
        "literal paste, then real Ctrl+V"
    );
    assert_shutdown(&server, &endpoint);
}

fn assert_shutdown(server: &common::ServerGuard, endpoint: &Path) {
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(i32::try_from(server.pid()).unwrap()),
        nix::sys::signal::Signal::SIGTERM,
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while endpoint.exists() {
        assert!(
            Instant::now() < deadline,
            "adapter socket survived graceful shutdown"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn assert_cardinal_splits(endpoint: &Path, pane: &Value, tty: &str, cwd: &Path) {
    for (direction, opposite) in [
        ("right", "left"),
        ("left", "right"),
        ("down", "up"),
        ("up", "down"),
    ] {
        let output = cwd.join(direction);
        let text = "literal ' quote; $(not-a-command)";
        result(
            endpoint,
            "split",
            json!({
                "from":pane, "direction":direction, "size":0.4, "tty":tty,
                "command":["/bin/sh","-c","printf '%s\\n%s\\n' \"$PWD\" \"$1\" > \"$2\"; exec /bin/cat",
                    "browser-test",text,output]
            }),
            cwd,
        );
        assert_eq!(read_file(&output), format!("{}\n{text}\n", cwd.display()));
        let neighbor = result(
            endpoint,
            "neighbor",
            json!({"from":pane,"direction":direction}),
            cwd,
        )["pane"]
            .clone();
        assert!(neighbor.is_object(), "{direction}");
        assert_eq!(neighbor["tab"], pane["tab"]);
        assert_eq!(
            result(
                endpoint,
                "neighbor",
                json!({"from":neighbor,"direction":opposite}),
                cwd
            )["pane"],
            *pane
        );
    }
}

fn assert_hidden_slot_splits(
    server: &common::ServerGuard,
    endpoint: &Path,
    anchor: &Value,
    cwd: &Path,
) {
    server.success(&["slot", "add", "@2", "--to", anchor["id"].as_str().unwrap()]);
    let hidden = json!({"id":"@2", "tab":anchor["tab"]});
    for (direction, opposite) in [
        ("right", "left"),
        ("left", "right"),
        ("down", "up"),
        ("up", "down"),
    ] {
        result(
            endpoint,
            "split",
            json!({"from":hidden,"direction":direction,"command":["/bin/cat"]}),
            cwd,
        );
        let neighbor = result(
            endpoint,
            "neighbor",
            json!({"from":hidden,"direction":direction}),
            cwd,
        )["pane"]
            .clone();
        assert!(neighbor.is_object(), "{direction}");
        assert_eq!(
            result(
                endpoint,
                "neighbor",
                json!({"from":neighbor,"direction":opposite}),
                cwd
            )["pane"],
            *anchor
        );
    }
}

#[test]
fn kitty_and_geometry_queries_are_answered_by_the_canonical_terminal() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("query-results");
    let helper = std::env::current_exe().unwrap();
    let seed = shell_words::join([
        helper.to_str().unwrap(),
        "--exact",
        "terminal_browser_query_helper",
        "--nocapture",
    ]);
    let _server = common::ServerGuard::builder("browser-queries")
        .isolated_home()
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("TB_QUERY_RESULT", &path)
        .seed_command(seed)
        .start();
    let results: Value = serde_json::from_str(&read_file(&path)).unwrap();
    assert!(
        results["inline"].as_str().unwrap().contains("i=301;OK"),
        "{results}"
    );
    for media in ["shared", "file"] {
        let reply = results[media].as_str().unwrap();
        assert!(
            reply.contains("ERROR")
                || reply.contains("ENOTSUP")
                || reply.contains("EINVAL")
                || reply.contains("ENOENT")
                || reply.contains("EBADF"),
            "{media}: {reply:?}"
        );
        assert!(
            !reply.contains(";OK"),
            "{media} must not grant local-file access"
        );
    }
    for (name, prefix) in [
        ("cell", "\x1b[6;"),
        ("pixels", "\x1b[4;"),
        ("cells", "\x1b[8;"),
    ] {
        let reply = results[name].as_str().unwrap();
        assert!(
            reply.starts_with(prefix) && reply.ends_with('t'),
            "{name}: {reply:?}"
        );
        assert!(!reply.contains(";0"), "{name}: nonzero geometry: {reply:?}");
    }
}

/// Executed only as a real program inside this test's private PTY.
#[test]
fn terminal_browser_query_helper() {
    let Some(path) = std::env::var_os("TB_QUERY_RESULT") else {
        return;
    };
    let mut input = std::io::stdin();
    let mut termios = rustix::termios::tcgetattr(&input).unwrap();
    termios.make_raw();
    termios.special_codes[rustix::termios::SpecialCodeIndex::VMIN] = 0;
    termios.special_codes[rustix::termios::SpecialCodeIndex::VTIME] = 30;
    rustix::termios::tcsetattr(&input, rustix::termios::OptionalActions::Now, &termios).unwrap();
    let mut output = std::io::stdout();
    let mut replies = serde_json::Map::new();
    for (name, query, final_byte) in [
        (
            "shared",
            "\x1b_Ga=q,i=299,t=s,f=32,s=1,v=1;L3Rlc3Q=\x1b\\",
            b'\\',
        ),
        (
            "file",
            "\x1b_Ga=q,i=300,t=f,f=32,s=1,v=1;L3Rlc3Q=\x1b\\",
            b'\\',
        ),
        (
            "inline",
            "\x1b_Ga=q,i=301,t=d,f=32,s=1,v=1;AAAAAA==\x1b\\",
            b'\\',
        ),
        ("cell", "\x1b[16t", b't'),
        ("pixels", "\x1b[14t", b't'),
        ("cells", "\x1b[18t", b't'),
    ] {
        output.write_all(query.as_bytes()).unwrap();
        output.flush().unwrap();
        let mut reply = Vec::new();
        let mut byte = [0];
        while input.read(&mut byte).unwrap() == 1 {
            reply.push(byte[0]);
            if byte[0] == final_byte {
                break;
            }
        }
        replies.insert(name.into(), json!(String::from_utf8_lossy(&reply)));
    }
    std::fs::write(path, Value::Object(replies).to_string()).unwrap();
}

#[test]
fn startup_environment_duplicate_start_and_reexec_keep_the_right_endpoint() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("env");
    let builder = common::ServerGuard::builder("browser-lifecycle")
        .isolated_home().env_clear().env("PATH", "/usr/bin:/bin")
        .env("BROWSER", "inherited-browser")
        .env("TERMINAL_BROWSER_TERMINAL_SOCKET", "/outer/terminal.sock")
        .seed_command(format!(
            "printf '%s\\n%s\\n' \"$BROWSER\" \"$TERMINAL_BROWSER_TERMINAL_SOCKET\" > {}; exec /bin/cat",
            shell_words::quote(output.to_str().unwrap())));
    let config = builder.home().root().join("config/phux");
    std::fs::create_dir(&config).unwrap();
    std::fs::write(
        config.join("config.toml"),
        "[defaults]\nbrowser-shim = true\n",
    )
    .unwrap();
    let log = dir.path().join("server.log");
    let server = builder.start_with(|cmd| {
        cmd.stderr(std::fs::File::create(&log).unwrap());
    });
    let environment = read_file(&output);
    let mut lines = environment.lines();
    assert!(
        lines.next().unwrap().ends_with("phux-browser"),
        "{environment}\n{}",
        std::fs::read_to_string(&log).unwrap()
    );
    let endpoint = PathBuf::from(lines.next().unwrap());
    assert_ne!(endpoint, Path::new("/outer/terminal.sock"));
    // Unlinking the old socket lets filesystems immediately reuse its inode.
    // Keep that inode allocated so a successful rebind can't look unchanged.
    std::fs::hard_link(&endpoint, endpoint.with_extension("before-upgrade")).unwrap();
    let before = std::fs::metadata(&endpoint).unwrap().ino();
    assert_ne!(server.run(&["server", "--no-seed"]).0, 0);
    assert_eq!(std::fs::metadata(&endpoint).unwrap().ino(), before);
    assert_eq!(
        result(&endpoint, "hello", json!({"protocol":1}), dir.path())["name"],
        "phux"
    );
    server.success(&["upgrade"]);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let replaced = std::fs::metadata(&endpoint).is_ok_and(|meta| meta.ino() != before);
        if replaced && UnixStream::connect(&endpoint).is_ok() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "adapter did not restart after reexec at {} (old inode {before}, current metadata {:?})\n{}",
            endpoint.display(),
            std::fs::metadata(&endpoint),
            std::fs::read_to_string(&log).unwrap()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        result(&endpoint, "hello", json!({"protocol":1}), dir.path())["name"],
        "phux"
    );
    assert_eq!(
        result(&endpoint, "listPanes", json!({}), dir.path())["panes"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}
