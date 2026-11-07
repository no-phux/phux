//! What `phux agent app run` launches and how it lets go: the launch plan,
//! the pid it owns, and the app's end. The helper only ever holds a pid it
//! launched: a browser or a Linux command is its own child, and a macOS app
//! is the one new instance `open -n` started, told apart from every instance
//! that ran before the launch.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use rustix::io::Errno;
use rustix::process::{Pid, Signal, kill_process, test_kill_process};

/// What one helper launches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Plan {
    /// A Chromium-family browser, run as the helper's child with a fresh
    /// profile and a debugging port it picks itself.
    Browser {
        program: String,
        args: Vec<String>,
        profile: PathBuf,
    },
    /// A new macOS instance, `open -n -a NAME --args ...`, found by pid.
    MacApp { name: String, open: Vec<String> },
    /// A command run as the helper's child (Linux); it must stay in the
    /// foreground, since its exit is the app's.
    Command { argv: Vec<String> },
}

impl Plan {
    /// The argv the plan runs, for `--dry-run`.
    pub(super) fn argv(&self) -> Vec<String> {
        match self {
            Self::Browser { program, args, .. } => {
                std::iter::once(program.clone()).chain(args.clone()).collect()
            }
            Self::MacApp { open, .. } => std::iter::once("open".to_owned())
                .chain(open.clone())
                .collect(),
            Self::Command { argv } => argv.clone(),
        }
    }
}

/// The plan for `name`: a browser opening `browser`'s URL, else a macOS app
/// or a Linux command (or `.desktop` id), each given `args`.
pub(super) fn plan(
    name: &str,
    browser: Option<&str>,
    headless: bool,
    args: &[String],
    profile: PathBuf,
) -> Result<Plan, String> {
    if let Some(url) = browser {
        let program = browser_candidates(name, cfg!(target_os = "macos"))
            .into_iter()
            .find_map(|candidate| resolve_program(&candidate))
            .ok_or_else(|| format!("no browser {name:?} found; name `chrome`, `chromium`, or a path"))?;
        let mut flags = browser_args(&profile, headless);
        flags.extend(args.iter().cloned());
        flags.push(url.to_owned());
        return Ok(Plan::Browser {
            program,
            args: flags,
            profile,
        });
    }
    if cfg!(target_os = "macos") {
        return Ok(Plan::MacApp {
            name: name.to_owned(),
            open: mac_open_args(name, args),
        });
    }
    let mut command = if Path::new(name)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("desktop"))
    {
        desktop_entry(name)?
    } else {
        vec![name.to_owned()]
    };
    command.extend(args.iter().cloned());
    Ok(Plan::Command { argv: command })
}

/// The browser's flags: no first-run prompts, a debugging port the browser
/// picks and writes to `DevToolsActivePort` in `profile`.
pub(super) fn browser_args(profile: &Path, headless: bool) -> Vec<String> {
    let mut args = vec![
        "--remote-debugging-port=0".to_owned(),
        format!("--user-data-dir={}", profile.display()),
        "--no-first-run".to_owned(),
        "--no-default-browser-check".to_owned(),
    ];
    if headless {
        args.push("--headless=new".to_owned());
    }
    args
}

/// Where a browser named `name` may live: a path as given, else the
/// well-known app bundles (macOS) or commands on `PATH` (Linux).
pub(super) fn browser_candidates(name: &str, macos: bool) -> Vec<String> {
    if name.contains('/') {
        return vec![name.to_owned()];
    }
    let bundle = |app: &str| format!("/Applications/{app}.app/Contents/MacOS/{app}");
    match (name.to_ascii_lowercase().as_str(), macos) {
        ("chrome" | "google chrome", true) => vec![bundle("Google Chrome")],
        ("chromium", true) => vec![bundle("Chromium")],
        (_, true) => vec![bundle(name)],
        ("chrome" | "google-chrome", false) => {
            ["google-chrome", "google-chrome-stable", "chromium", "chromium-browser"]
                .map(String::from)
                .to_vec()
        }
        ("chromium", false) => ["chromium", "chromium-browser"].map(String::from).to_vec(),
        (_, false) => vec![name.to_owned()],
    }
}

/// `candidate` when it is an existing file path, else its match on `PATH`.
fn resolve_program(candidate: &str) -> Option<String> {
    if candidate.contains('/') {
        return Path::new(candidate)
            .is_file()
            .then(|| candidate.to_owned());
    }
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(candidate))
        .find(|path| path.is_file())
        .map(|path| path.display().to_string())
}

/// `open`'s arguments for a new instance of `name`: `-n` always, so the
/// helper never adopts an instance someone else started.
pub(super) fn mac_open_args(name: &str, args: &[String]) -> Vec<String> {
    let mut open = vec!["-n".to_owned(), "-a".to_owned(), name.to_owned()];
    if !args.is_empty() {
        open.push("--args".to_owned());
        open.extend(args.iter().cloned());
    }
    open
}

/// The `Exec` of desktop entry `id`, from the XDG data directories.
fn desktop_entry(id: &str) -> Result<Vec<String>, String> {
    let home = std::env::var("XDG_DATA_HOME")
        .ok()
        .or_else(|| std::env::var("HOME").ok().map(|h| format!("{h}/.local/share")));
    let dirs = std::env::var("XDG_DATA_DIRS")
        .unwrap_or_else(|_| "/usr/local/share:/usr/share".to_owned());
    home.into_iter()
        .chain(dirs.split(':').map(str::to_owned))
        .map(|dir| Path::new(&dir).join("applications").join(id))
        .find_map(|path| std::fs::read_to_string(path).ok())
        .and_then(|entry| desktop_exec(&entry))
        .ok_or_else(|| format!("no desktop entry {id:?} with an Exec line"))
}

/// The `Exec` argv of a desktop entry, field codes (`%u`, `%F`) dropped.
// ponytail: splits on whitespace; quoted Exec arguments need the spec's unquoting.
pub(super) fn desktop_exec(entry: &str) -> Option<Vec<String>> {
    let mut in_entry = false;
    for line in entry.lines().map(str::trim) {
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
        } else if let Some(exec) = line.strip_prefix("Exec=").filter(|_| in_entry) {
            let argv: Vec<String> = exec
                .split_whitespace()
                .filter(|arg| !arg.starts_with('%'))
                .map(str::to_owned)
                .collect();
            return (!argv.is_empty()).then_some(argv);
        }
    }
    None
}

/// The one instance in `after` that was not in `before`: `None` while it
/// has not appeared, an error when several did (the helper cannot tell
/// which is its own). A pid from `before` is never returned.
pub(super) fn new_instance(before: &[u32], after: &[u32]) -> Result<Option<u32>, String> {
    let fresh: Vec<u32> = after
        .iter()
        .copied()
        .filter(|pid| !before.contains(pid))
        .collect();
    match fresh.as_slice() {
        [] => Ok(None),
        [pid] => Ok(Some(*pid)),
        many => Err(format!(
            "several new instances appeared ({many:?}); not adopting any"
        )),
    }
}

/// The first normal-layer window owned by `pid` in a CoreGraphics listing
/// of `{id, pid, layer}` objects.
pub(super) fn window_of_pid(pid: u32, listing: &str) -> Option<u32> {
    let windows: Vec<serde_json::Value> = serde_json::from_str(listing.trim()).ok()?;
    windows
        .iter()
        .filter(|w| w["layer"].as_i64().unwrap_or(0) == 0)
        .find(|w| w["pid"].as_u64() == Some(u64::from(pid)))
        .and_then(|w| u32::try_from(w["id"].as_u64()?).ok())
}

/// The Hyprland class of `pid`'s first mapped window, from `hyprctl -j clients`.
#[cfg(any(target_os = "linux", test))]
pub(super) fn class_of_pid(pid: u32, clients: &str) -> Option<String> {
    let clients: Vec<serde_json::Value> = serde_json::from_str(clients.trim()).ok()?;
    clients
        .iter()
        .filter(|c| c["mapped"].as_bool().unwrap_or(true))
        .find(|c| c["pid"].as_u64() == Some(u64::from(pid)))
        .and_then(|c| c["class"].as_str())
        .filter(|class| !class.is_empty())
        .map(str::to_owned)
}

/// The application the helper launched, and owns.
#[derive(Debug)]
pub(super) struct Launched {
    pub(super) pid: u32,
    /// The process, when it is the helper's child.
    child: Option<Child>,
    /// The browser profile to remove once it is gone.
    profile: Option<PathBuf>,
    /// The macOS app name the pid must still carry before it is quit.
    mac_name: Option<String>,
}

impl Launched {
    /// Start `plan`. A macOS app is waited for (up to `timeout`) until its
    /// new instance appears.
    pub(super) fn start(plan: &Plan, timeout: Duration) -> Result<Self, String> {
        match plan {
            Plan::Browser {
                program,
                args,
                profile,
            } => {
                std::fs::create_dir_all(profile)
                    .map_err(|err| format!("could not make {}: {err}", profile.display()))?;
                let child = Command::new(program)
                    .args(args)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .map_err(|err| format!("could not start {program}: {err}"))?;
                Ok(Self::child(child, Some(profile.clone())))
            }
            Plan::Command { argv } => {
                let (program, rest) = argv.split_first().ok_or("an empty command")?;
                let child = Command::new(program)
                    .args(rest)
                    .stdin(Stdio::null())
                    .spawn()
                    .map_err(|err| format!("could not start {program}: {err}"))?;
                Ok(Self::child(child, None))
            }
            Plan::MacApp { name, open } => {
                let before = mac::pids(name)?;
                let status = Command::new("/usr/bin/open")
                    .args(open)
                    .status()
                    .map_err(|err| format!("could not run open: {err}"))?;
                if !status.success() {
                    return Err(format!("`open -n -a {name}` exited with {status}"));
                }
                let deadline = std::time::Instant::now() + timeout;
                loop {
                    if let Some(pid) = new_instance(&before, &mac::pids(name)?)? {
                        return Ok(Self {
                            pid,
                            child: None,
                            profile: None,
                            mac_name: Some(name.clone()),
                        });
                    }
                    if std::time::Instant::now() >= deadline {
                        return Err(format!("no new {name} instance appeared"));
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
            }
        }
    }

    fn child(child: Child, profile: Option<PathBuf>) -> Self {
        Self {
            pid: child.id(),
            child: Some(child),
            profile,
            mac_name: None,
        }
    }

    /// Whether the application still runs.
    // ponytail: a macOS pid is probed with kill(0), so a reused pid reads as alive; `quit` re-checks the name.
    pub(super) fn alive(&mut self) -> bool {
        match &mut self.child {
            Some(child) => matches!(child.try_wait(), Ok(None)),
            None => pid(self.pid).is_some_and(|pid| test_kill_process(pid) != Err(Errno::SRCH)),
        }
    }

    /// End the application: a macOS app is asked to quit, a child gets
    /// `SIGTERM`, and a browser (whose profile is throwaway) `SIGKILL` if it
    /// lingers. Its profile is removed.
    pub(super) fn quit(mut self) {
        if let Some(name) = &self.mac_name {
            mac::quit(self.pid, name);
            return;
        }
        if let Some(pid) = pid(self.pid) {
            let _ = kill_process(pid, Signal::TERM);
        }
        if let Some(child) = &mut self.child {
            for _ in 0..20 {
                if !matches!(child.try_wait(), Ok(None)) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            if self.profile.is_some() && matches!(child.try_wait(), Ok(None)) {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
        self.forget();
    }

    /// Remove what the application left that the helper made.
    pub(super) fn forget(&self) {
        if let Some(profile) = &self.profile {
            let _ = std::fs::remove_dir_all(profile);
        }
    }
}

fn pid(raw: u32) -> Option<Pid> {
    i32::try_from(raw).ok().and_then(Pid::from_raw)
}

/// macOS: running instances and polite quits through the system's
/// JavaScript automation bridge to `AppKit`; no bindings, no `unsafe`.
mod mac {
    use std::process::Command;

    const PIDS: &str = r#"function run(argv) {
  ObjC.import("AppKit");
  const apps = $.NSWorkspace.sharedWorkspace.runningApplications;
  const want = argv[0].toLowerCase();
  const pids = [];
  for (let i = 0; i < apps.count; i++) {
    const app = apps.objectAtIndex(i);
    if ((ObjC.unwrap(app.localizedName) || "").toLowerCase() === want) pids.push(app.processIdentifier);
  }
  return JSON.stringify(pids);
}"#;

    const QUIT: &str = r#"function run(argv) {
  ObjC.import("AppKit");
  const app = $.NSRunningApplication.runningApplicationWithProcessIdentifier(Number(argv[0]));
  if (app.isNil() || (ObjC.unwrap(app.localizedName) || "").toLowerCase() !== argv[1].toLowerCase()) return "not ours";
  return app.terminate ? "quit" : "refused";
}"#;

    /// The windows on screen as `{id, pid, layer}`.
    pub(in crate::commands::agent::tree) const WINDOWS: &str = r#"ObjC.import("CoreGraphics");
const list = $.CGWindowListCopyWindowInfo($.kCGWindowListOptionOnScreenOnly | $.kCGWindowListExcludeDesktopElements, 0);
const windows = ObjC.deepUnwrap(ObjC.castRefToObject(list)) || [];
JSON.stringify(windows.map(w => ({ id: w.kCGWindowNumber, pid: w.kCGWindowOwnerPID, layer: w.kCGWindowLayer })));"#;

    fn jxa(script: &str, args: &[&str]) -> Result<String, String> {
        let out = Command::new("/usr/bin/osascript")
            .args(["-l", "JavaScript", "-e", script])
            .args(args)
            .output()
            .map_err(|err| format!("could not run osascript: {err}"))?;
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// The pids of every running app named `name`.
    pub(super) fn pids(name: &str) -> Result<Vec<u32>, String> {
        serde_json::from_str(jxa(PIDS, &[name])?.trim())
            .map_err(|err| format!("could not list running apps: {err}"))
    }

    /// Ask `pid` to quit as the user would, if it is still `name`.
    pub(super) fn quit(pid: u32, name: &str) {
        let _ = jxa(QUIT, &[&pid.to_string(), name]);
    }

    /// The on-screen window listing, for [`super::window_of_pid`].
    pub(in crate::commands::agent::tree) fn windows() -> String {
        jxa(WINDOWS, &[]).unwrap_or_default()
    }
}

pub(super) use mac::windows as mac_windows;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_instance_that_appeared_after_the_launch_is_adopted() {
        assert_eq!(new_instance(&[10, 20], &[10, 20]), Ok(None));
        assert_eq!(new_instance(&[10, 20], &[10, 20, 30]), Ok(Some(30)));
        assert_eq!(new_instance(&[10, 20], &[20, 30]), Ok(Some(30)));
        assert!(new_instance(&[10], &[10, 30, 40]).is_err());
        for after in [vec![10], vec![10, 20], vec![20]] {
            assert_ne!(new_instance(&[10, 20], &after), Ok(Some(10)));
            assert_ne!(new_instance(&[10, 20], &after), Ok(Some(20)));
        }
    }

    #[test]
    fn a_mac_app_always_opens_a_new_instance() {
        assert_eq!(mac_open_args("TextEdit", &[]), ["-n", "-a", "TextEdit"]);
        assert_eq!(
            mac_open_args("TextEdit", &["/tmp/a.txt".into()]),
            ["-n", "-a", "TextEdit", "--args", "/tmp/a.txt"]
        );
        let plan = Plan::MacApp {
            name: "TextEdit".into(),
            open: mac_open_args("TextEdit", &[]),
        };
        assert_eq!(plan.argv(), ["open", "-n", "-a", "TextEdit"]);
    }

    #[test]
    fn a_browser_gets_a_private_profile_and_picks_its_own_port() {
        let args = browser_args(Path::new("/tmp/p"), true);
        assert_eq!(
            args,
            [
                "--remote-debugging-port=0",
                "--user-data-dir=/tmp/p",
                "--no-first-run",
                "--no-default-browser-check",
                "--headless=new"
            ]
        );
        assert!(!browser_args(Path::new("/tmp/p"), false).contains(&"--headless=new".to_owned()));
    }

    #[test]
    fn browser_names_resolve_to_bundles_or_commands() {
        assert_eq!(
            browser_candidates("chrome", true),
            ["/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"]
        );
        assert_eq!(
            browser_candidates("Brave Browser", true),
            ["/Applications/Brave Browser.app/Contents/MacOS/Brave Browser"]
        );
        assert_eq!(browser_candidates("chrome", false)[0], "google-chrome");
        assert_eq!(browser_candidates("/opt/x/chrome", false), ["/opt/x/chrome"]);
    }

    #[test]
    fn a_desktop_entry_runs_its_exec_without_field_codes() {
        let entry = "[Desktop Entry]\nName=Gedit\nExec=gedit --new-window %U\n[Desktop Action new]\nExec=other\n";
        assert_eq!(desktop_exec(entry).unwrap(), ["gedit", "--new-window"]);
        assert_eq!(desktop_exec("[Other]\nExec=x\n"), None);
    }

    #[test]
    fn windows_are_found_by_owner_pid() {
        let listing = r#"[{"id":5,"pid":9,"layer":25},{"id":7,"pid":9,"layer":0},{"id":8,"pid":3,"layer":0}]"#;
        assert_eq!(window_of_pid(9, listing), Some(7));
        assert_eq!(window_of_pid(4, listing), None);
        let clients = r#"[{"pid":9,"class":"","mapped":true},{"pid":3,"class":"gedit","mapped":true}]"#;
        assert_eq!(class_of_pid(3, clients).as_deref(), Some("gedit"));
        assert_eq!(class_of_pid(9, clients), None);
    }

    #[test]
    fn a_child_is_ended_and_its_profile_removed() {
        let profile = std::env::temp_dir().join(format!("phux-app-test-{}", std::process::id()));
        let plan = Plan::Browser {
            program: "/bin/sleep".into(),
            args: vec!["30".into()],
            profile: profile.clone(),
        };
        let mut launched = Launched::start(&plan, Duration::from_secs(1)).expect("start");
        assert!(launched.alive());
        assert!(profile.is_dir());
        let pid = launched.pid;
        launched.quit();
        assert_eq!(
            test_kill_process(super::pid(pid).expect("pid")),
            Err(Errno::SRCH)
        );
        assert!(!profile.exists());
    }
}
