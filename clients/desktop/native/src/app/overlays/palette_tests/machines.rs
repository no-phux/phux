//! Add Machine: the tailnet's machines, Tailscale sign-in, and `phux host
//! add` with its progress, against a stand-in CLI.

use std::path::{Path, PathBuf};

use super::*;
use crate::app::desk::TailnetView;
use crate::connection::DeterministicDial;

const RUNNING: &str = r#"{"schema_version":1,
  "tailscale":{"state":"running","cli":"/x","auth_url":null,"tailnet":"me@example.com","self":"mac.ts.net"},
  "peers":[
    {"name":"mini","os":"linux","online":true,"supported":true,"ssh_host":"mini.tail9a8b.ts.net","registered":null},
    {"name":"box","os":"macOS","online":false,"supported":true,"ssh_host":"box.tail9a8b.ts.net","registered":"box"},
    {"name":"phone","os":"iOS","online":true,"supported":false,"ssh_host":"phone.tail9a8b.ts.net","registered":null}
  ]}"#;

const SIGNED_OUT: &str =
    r#"{"schema_version":1,"tailscale":{"state":"needs_login","auth_url":null},"peers":[]}"#;

const SIGNING_IN: &str = r#"{"schema_version":1,
  "tailscale":{"state":"needs_login","auth_url":"https://login.tailscale.com/a/abc"},"peers":[]}"#;

/// A stand-in `phux`: `host discover` prints `discover.json` (and, with
/// `--login`, `login.json`), `host add` logs its arguments, prints progress
/// and fails for the target `bad`, `host ls` lists nothing.
fn stand_in_cli(dir: &Path) -> PathBuf {
    let script = dir.join("test-phux");
    let quote = |path: PathBuf| format!("'{}'", path.display());
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\n\
             case \"$1 $2\" in\n\
             'host discover')\n\
               case \"$*\" in *--login*) cat {login}; touch {logged_in} ;; *) cat {discover} ;; esac ;;\n\
             'host add')\n\
               printf '%s\\n' \"$@\" > {args}\n\
               echo 'phux: info: mini: trying quic://100.70.0.2:8788' >&2\n\
               if [ \"$3\" = bad ]; then\n\
                 echo 'phux: error: cannot reach bad over ssh: refused' >&2\n\
                 echo '  hint: check that `ssh bad` works' >&2\n\
                 exit 1\n\
               fi\n\
               echo 'phux: ok: mini: paired' >&2\n\
               echo 'Registered mini -> quic://100.70.0.2:8788' ;;\n\
             'host ls') echo '{{\"hosts\":[],\"schema_version\":1}}' ;;\n\
             esac\n",
            login = quote(dir.join("login.json")),
            logged_in = quote(dir.join("logged-in")),
            discover = quote(dir.join("discover.json")),
            args = quote(dir.join("add-args")),
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    script
}

fn scratch(cx: &TestAppContext) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "phux-add-machine-{}-{}",
        std::process::id(),
        cx.test_function_name().unwrap_or("isolated")
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Draw until `done` holds: `host add` runs on a real thread.
fn wait_for(desk: &Entity<Desk>, cx: &mut VisualTestContext, done: impl Fn(&Desk) -> bool) -> bool {
    for _ in 0..500 {
        if desk.read_with(cx, |desk, _| done(desk)) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
        render(cx);
    }
    false
}

fn input(desk: &Entity<Desk>, slot: InputSlot, cx: &VisualTestContext) -> String {
    desk.read_with(cx, |desk, cx| desk.input_value(slot, cx))
}

#[gpui::test]
fn a_tailnet_machine_is_chosen_and_set_up_with_phux_installed(cx: &mut TestAppContext) {
    // A finished setup re-reads the registry and redials; keep that in-process.
    let _dial = DeterministicDial::arm();
    let dir = scratch(cx);
    std::fs::write(dir.join("discover.json"), RUNNING).unwrap();
    let cli = stand_in_cli(&dir);
    let (handle, desk) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    desk.update(cx, |desk, _| desk.phux = cli);
    open(&desk, &Modal::AddHost, cx);

    // Only machines phux runs on are offered; a registered one says so.
    let peers = desk.read_with(cx, |desk, _| match &desk.add_machine.tailnet {
        TailnetView::Ready(tailnet) => tailnet.peers.clone(),
        other => panic!("tailnet not read: {other:?}"),
    });
    assert_eq!(
        peers
            .iter()
            .map(|peer| peer.name.as_str())
            .collect::<Vec<_>>(),
        ["mini", "box"]
    );
    assert!(cx.debug_bounds("tailnet-peer-mini").is_some());
    assert!(cx.debug_bounds("tailnet-peer-box").is_some());

    cx.update(|window, cx| {
        desk.update(cx, |desk, cx| desk.choose_peer(&peers[0], window, cx));
    });
    render(cx);
    assert_eq!(
        input(&desk, InputSlot::HostTarget, cx),
        "mini.tail9a8b.ts.net"
    );
    assert_eq!(input(&desk, InputSlot::HostName, cx), "mini");

    keys(cx, "enter");
    assert!(
        wait_for(&desk, cx, |desk| desk.add_machine.adding.is_none()
            && desk.modal == Modal::None),
        "a successful setup closes the dialog"
    );
    let args = std::fs::read_to_string(dir.join("add-args")).unwrap();
    assert_eq!(
        args.lines().collect::<Vec<_>>(),
        [
            "host",
            "add",
            "mini.tail9a8b.ts.net",
            "--name",
            "mini",
            "--install",
            "--accept-new-host-key"
        ],
        "a tailnet peer installs phux and trusts its first host key"
    );
    desk.read_with(cx, |desk, _| assert!(desk.add_machine.error.is_none()));
    std::fs::remove_dir_all(dir).unwrap();
}

#[gpui::test]
fn a_failed_setup_stays_open_with_the_reason(cx: &mut TestAppContext) {
    // A finished setup re-reads the registry and redials; keep that in-process.
    let _dial = DeterministicDial::arm();
    let dir = scratch(cx);
    std::fs::write(dir.join("discover.json"), RUNNING).unwrap();
    let cli = stand_in_cli(&dir);
    let (handle, desk) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    desk.update(cx, |desk, _| desk.phux = cli);
    open(&desk, &Modal::AddHost, cx);

    // Not on the tailnet, and installing turned off: strict host keys, no
    // install.
    desk.update(cx, |desk, cx| desk.set_install(false, cx));
    type_text(cx, "bad");
    keys(cx, "enter");
    assert!(
        wait_for(&desk, cx, |desk| desk.add_machine.error.is_some()),
        "the failure is reported"
    );
    desk.read_with(cx, |desk, _| {
        assert_eq!(desk.modal, Modal::AddHost, "the dialog stays to retry");
        assert!(desk.add_machine.adding.is_none());
        assert_eq!(
            desk.add_machine.error.as_deref(),
            Some("cannot reach bad over ssh: refused\ncheck that `ssh bad` works")
        );
    });
    render(cx);
    assert!(cx.debug_bounds("host-add-error").is_some());
    let args = std::fs::read_to_string(dir.join("add-args")).unwrap();
    assert_eq!(args.lines().collect::<Vec<_>>(), ["host", "add", "bad"]);
    std::fs::remove_dir_all(dir).unwrap();
}

#[gpui::test]
fn signing_in_to_tailscale_opens_the_link_once_and_lists_machines_after(cx: &mut TestAppContext) {
    // A finished setup re-reads the registry and redials; keep that in-process.
    let _dial = DeterministicDial::arm();
    let dir = scratch(cx);
    std::fs::write(dir.join("discover.json"), SIGNED_OUT).unwrap();
    std::fs::write(dir.join("login.json"), SIGNING_IN).unwrap();
    let cli = stand_in_cli(&dir);
    let (handle, desk) = fixture(cx);
    let vcx = &mut VisualTestContext::from_window(handle, cx);
    desk.update(vcx, |desk, _| desk.phux = cli);
    open(&desk, &Modal::AddHost, vcx);
    assert_eq!(
        super::super::dialogs::tailnet_step("needs_login", false).1,
        Some("Sign In to Tailscale")
    );
    assert!(
        vcx.debug_bounds("tailnet-action").is_some(),
        "one button moves it on"
    );

    desk.update(vcx, |desk, cx| desk.refresh_tailnet(true, cx));
    render(vcx);
    assert!(dir.join("logged-in").exists(), "--login ran");
    assert_eq!(
        cx.opened_url().as_deref(),
        Some("https://login.tailscale.com/a/abc"),
        "the sign-in link opens in the browser"
    );

    // The browser finishes; the next poll lists the machines.
    std::fs::write(dir.join("discover.json"), RUNNING).unwrap();
    cx.executor()
        .advance_clock(std::time::Duration::from_secs(3));
    let vcx = &mut VisualTestContext::from_window(handle, cx);
    render(vcx);
    desk.read_with(vcx, |desk, _| {
        let tailnet = desk.add_machine.tailnet().expect("read");
        assert!(tailnet.running(), "{tailnet:?}");
        assert_eq!(tailnet.peers.len(), 2);
    });
    std::fs::remove_dir_all(dir).unwrap();
}
