//! Developer mode applies live and is saved, in its own process: the mode,
//! heap accounting and the HUD are process-wide, and the setting is read
//! from `$XDG_CONFIG_HOME`. This crate denies `unsafe` (so `set_var`), so
//! the test re-runs itself with a disposable config directory and dev
//! socket path, and without an inherited `PHUX_DEV`.

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use phux_desktop::devtools::{self, SocketState};
use phux_perf::alloc;

#[global_allocator]
static ALLOC: alloc::Counting = alloc::Counting::system();

const CHILD: &str = "PHUX_DESKTOP_DEVELOPER_MODE_TEST_ROOT";
const TEST: &str = "developer_mode_applies_live_and_is_saved";

#[gpui::test]
fn developer_mode_applies_live_and_is_saved(cx: &mut gpui::TestAppContext) {
    match std::env::var_os(CHILD) {
        Some(root) => toggle_in_this_process(&PathBuf::from(root), cx),
        None => run_child(),
    }
}

fn run_child() {
    let root = std::env::temp_dir().join(format!("pdm-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let socket_dir = root.join("s");
    std::fs::create_dir_all(&socket_dir).unwrap();
    std::fs::set_permissions(&socket_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .env(CHILD, &root)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env(devtools::socket::PATH_ENV, socket_dir.join("d.sock"))
        .env_remove(phux_perf::dev::ENV)
        .args([TEST, "--exact", "--nocapture"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    eprintln!("{stdout}{}", String::from_utf8_lossy(&output.stderr));
    let _ = std::fs::remove_dir_all(&root);
    assert!(output.status.success(), "the child failed");
    assert!(stdout.contains("1 passed"), "the child did not run");
}

fn toggle_in_this_process(root: &Path, cx: &mut gpui::TestAppContext) {
    let saved = root.join("config/phux-desktop/developer.json");
    assert_eq!(devtools::prefs::path(), saved);
    devtools::init(false, devtools::prefs::load(), None);
    assert!(!devtools::dev_mode(), "off until saved on");
    assert_eq!(devtools::socket_state(), SocketState::Off);

    cx.update(|cx| devtools::set_dev_mode(true, cx)).unwrap();
    assert!(devtools::dev_mode());
    assert!(alloc::enabled());
    assert!(devtools::hud_visible());
    let socket = devtools::socket_path().expect("the dev socket starts with Developer mode");
    assert!(socket.exists());
    assert!(devtools::prefs::load(), "saved for the next launch");

    cx.update(|cx| devtools::set_dev_mode(false, cx)).unwrap();
    assert!(!devtools::dev_mode());
    assert!(!alloc::enabled());
    assert!(!devtools::hud_visible());
    assert_eq!(devtools::socket_state(), SocketState::Off);
    assert!(!socket.exists(), "turning it off closes the dev socket");
    assert!(!devtools::prefs::load());

    cx.update(|cx| devtools::set_dev_mode(true, cx)).unwrap();
    assert_eq!(devtools::socket_path(), Some(socket), "it starts again");
    cx.update(|cx| devtools::set_dev_mode(false, cx)).unwrap();
}
