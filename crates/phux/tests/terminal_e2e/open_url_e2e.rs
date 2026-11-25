//! `[defaults] browser-shim` end to end: a program in a pane opens its
//! browser, the attached TUI asks, and only a deliberate answer opens the
//! link there.

#![allow(clippy::unwrap_used, reason = "tests")]

use std::path::Path;
use std::time::{Duration, Instant};

use crate::common::{PtyAttach, ServerGuard, write_executable};

const WAIT: Duration = Duration::from_secs(20);
/// Past the prompt's quiet second.
const QUIET: Duration = Duration::from_millis(1300);
const LINK: &str = "https://id.example/auth?client=a&state=b";

fn opened(log: &Path) -> String {
    std::fs::read_to_string(log).unwrap_or_default()
}

#[test]
#[ignore = "spawns a real server and attached PTY client; run in the e2e lane"]
fn a_panes_browser_asks_the_attached_client_and_falls_back_when_declined() {
    let builder = ServerGuard::builder("open-url")
        .isolated_home()
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .seed_command("exec /bin/sh");
    let config = builder.home().root().join("config/phux");
    std::fs::create_dir(&config).unwrap();
    std::fs::write(
        config.join("config.toml"),
        "[defaults]\nbrowser-shim = true\n",
    )
    .unwrap();
    let server = builder.start();

    // Outside a pane there is nobody to ask.
    let (code, _, stderr) = server.run(&["open-url", LINK]);
    assert_eq!(code, 2, "{stderr}");

    let bin = tempfile::tempdir().unwrap();
    let log = bin.path().join("opened");
    for opener in ["open", "xdg-open"] {
        write_executable(
            bin.path().join(opener),
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$#\" \"$1\" >> '{}'\n",
                log.display()
            ),
        );
    }
    let mut client = PtyAttach::start_with(&server.socket, &[], (100, 30), |_, command| {
        let path = format!("{}:/usr/bin:/bin", bin.path().display());
        command.env("PATH", path);
        command.env("DISPLAY", ":0");
        command.env_remove("SSH_CONNECTION");
        command.env_remove("SSH_TTY");
    });
    client.await_painted(&["+ session", "$ "], WAIT);
    client.send(b"echo up-$((1+1)) ${PATH##*/}\r");
    client.await_painted(&["up-2 browser-shim"], WAIT);

    // Asked, and what was being typed ahead opens nothing.
    client.send(format!("\"$BROWSER\" '{LINK}'; echo rc=$?\r").as_bytes());
    client.await_painted(&["Open a link?", "id.example", "o open"], WAIT);
    client.send(b"ooo\r");
    std::thread::sleep(QUIET);
    assert_eq!(opened(&log), "", "typing opened a link");
    client.send(b"o");
    let deadline = Instant::now() + WAIT;
    while opened(&log).is_empty() {
        assert!(Instant::now() < deadline, "{}", client.painted());
        std::thread::sleep(Duration::from_millis(20));
    }
    client.await_painted(&["rc=0"], WAIT);
    assert_eq!(opened(&log), format!("1\n{LINK}\n"), "one argument, once");
    assert!(!client.painted().contains("Open this on your own machine"));

    // Declined, the stand-in prints it, once.
    client.send(format!("\"$BROWSER\" '{LINK}&again'\r").as_bytes());
    client.await_painted(&["state=b&again"], WAIT);
    std::thread::sleep(QUIET);
    client.send(b"\x1b");
    client.await_painted(&["Open this on your own machine"], WAIT);
    assert_eq!(opened(&log), format!("1\n{LINK}\n"));
}
