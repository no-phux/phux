//! Notifications through `notify-send` (libnotify), which every freedesktop
//! notification daemon answers: mako, dunst, Omarchy's shell. One child per
//! alert stays up while it shows: it prints the daemon's id, then the action
//! if the person clicks, and exits when the notification closes.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::{LazyLock, Mutex, MutexGuard, OnceLock};

type OnClick = Box<dyn Fn(String) + Send + Sync>;

static CLICK: OnceLock<OnClick> = OnceLock::new();
/// Our alert id to the daemon's, while it shows.
static SHOWN: LazyLock<Mutex<HashMap<String, u32>>> = LazyLock::new(Mutex::default);

fn shown() -> MutexGuard<'static, HashMap<String, u32>> {
    SHOWN.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn available() -> bool {
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|dir| dir.join("notify-send").is_file())
    })
}

pub fn on_click(f: impl Fn(String) + Send + Sync + 'static) {
    let _ = CLICK.set(Box::new(f));
}

pub fn post(id: &str, title: &str, body: &str) {
    let Ok(mut child) = Command::new("notify-send")
        .args([
            "--app-name=Phux Rune",
            // Names the app to the daemon: its icon, and who to raise.
            "--hint=string:desktop-entry:sh.phux.rune",
            "--print-id",
            "--action=default=Open",
            // Long enough to be seen; Omarchy caps a toast at 30 s.
            "--expire-time=30000",
            "--",
            title,
            &escape(body),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return;
    };
    let id = id.to_owned();
    std::thread::spawn(move || {
        let mut lines = child.stdout.take().map(|out| BufReader::new(out).lines());
        let mut next = || lines.as_mut()?.next()?.ok();
        if let Some(n) = next().and_then(|line| line.trim().parse().ok()) {
            shown().insert(id.clone(), n);
        }
        let clicked = next().is_some_and(|action| action.trim() == "default");
        let _ = child.wait();
        shown().remove(&id);
        if clicked && let Some(f) = CLICK.get() {
            f(id);
        }
    });
}

pub fn remove(ids: &[&str]) {
    let gone: Vec<u32> = {
        let mut shown = shown();
        ids.iter().filter_map(|id| shown.remove(*id)).collect()
    };
    if gone.is_empty() {
        return;
    }
    // Off the UI thread: a session bus call can stall.
    std::thread::spawn(move || {
        for n in gone {
            let _ = Command::new("gdbus")
                .args([
                    "call",
                    "--session",
                    "--dest=org.freedesktop.Notifications",
                    "--object-path=/org/freedesktop/Notifications",
                    "--method=org.freedesktop.Notifications.CloseNotification",
                    &n.to_string(),
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    });
}

/// Wayland lets no app raise itself, and a daemon that runs our action
/// raises nothing (Omarchy's skips its own focus then), so ask Hyprland,
/// in its Lua dialect, then its older one. Elsewhere a click only switches
/// what the window shows.
pub fn bring_to_front() {
    if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_none() {
        return;
    }
    let window = format!("pid:{}", std::process::id());
    std::thread::spawn(move || {
        let dispatch = |args: &[&str]| {
            Command::new("hyprctl")
                .arg("dispatch")
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|status| status.success())
        };
        let lua = format!("hl.dsp.focus({{ window = \"{window}\" }})");
        let _ = dispatch(&[&lua]) || dispatch(&["focuswindow", &window]);
    });
}

/// Daemons may read the body as markup, and an agent's question is not.
fn escape(body: &str) -> String {
    body.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_body_never_reads_as_markup() {
        assert_eq!(
            super::escape("run <cmd> && echo"),
            "run &lt;cmd&gt; &amp;&amp; echo"
        );
    }
}
