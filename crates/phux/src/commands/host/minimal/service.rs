//! The per-user service that runs `phux host minimal watch`: a launchd agent
//! on macOS, a systemd user unit on Linux. It is kept separate from the
//! server's own unit so installing or removing it never touches the server.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::super::super::service::{Manager, profile_suffix};
use super::boxes::MinCli;

/// The environment the watcher must share with the `phux` that set it up, so
/// it reads the same config, state, and socket.
const CARRIED_ENV: [&str; 8] = [
    "HOME",
    "PHUX_SOCKET",
    "PHUX_PROFILE",
    "XDG_CONFIG_HOME",
    "XDG_STATE_HOME",
    "XDG_DATA_HOME",
    "XDG_RUNTIME_DIR",
    "TMPDIR",
];

/// What the unit runs and where it logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WatchPlan {
    pub(super) label: String,
    pub(super) argv: Vec<String>,
    pub(super) env: Vec<(String, String)>,
    pub(super) log: PathBuf,
}

impl WatchPlan {
    fn current(min: &MinCli, hub: Option<&str>) -> Result<Self, String> {
        let exe = std::env::current_exe()
            .map_err(|err| format!("could not resolve this binary: {err}"))?;
        let mut argv = vec![
            exe.display().to_string(),
            "host".to_owned(),
            "minimal".to_owned(),
            "watch".to_owned(),
            "--min".to_owned(),
            min.program().display().to_string(),
        ];
        if let Some(hub) = hub {
            argv.extend(["--hub".to_owned(), hub.to_owned()]);
        }
        let env = CARRIED_ENV
            .iter()
            .filter_map(|key| Some(((*key).to_owned(), std::env::var(key).ok()?)))
            .filter(|(_, value)| !value.is_empty())
            .collect();
        Ok(Self {
            label: label(),
            argv,
            env,
            log: phux_server::telemetry::state_dir()
                .join("minimal")
                .join("watch.log"),
        })
    }
}

/// `sh.phux.minimal`, or `sh.phux.minimal.PROFILE` off the default profile.
fn label() -> String {
    profile_suffix().map_or_else(
        || "sh.phux.minimal".to_owned(),
        |profile| format!("sh.phux.minimal.{profile}"),
    )
}

fn unit_path(manager: Manager, label: &str) -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or("HOME is not set")?;
    Ok(match manager {
        Manager::Launchd => home
            .join("Library/LaunchAgents")
            .join(format!("{label}.plist")),
        Manager::Systemd => home
            .join(".config/systemd/user")
            .join(format!("{}.service", label.replace('.', "-"))),
    })
}

/// Write and (re)start the watcher unit, returning its path.
pub(super) fn install(min: &MinCli, hub: Option<&str>) -> Result<PathBuf, String> {
    let manager = Manager::host().ok_or("no service manager on this platform")?;
    let plan = WatchPlan::current(min, hub)?;
    let path = unit_path(manager, &plan.label)?;
    create_parent(&path)?;
    create_parent(&plan.log)?;
    let body = match manager {
        Manager::Launchd => render_plist(&plan),
        Manager::Systemd => render_systemd(&plan),
    };
    std::fs::write(&path, body)
        .map_err(|err| format!("could not write {}: {err}", path.display()))?;
    restart(manager, &plan.label, &path)?;
    Ok(path)
}

fn create_parent(path: &Path) -> Result<(), String> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    std::fs::create_dir_all(parent)
        .map_err(|err| format!("could not create {}: {err}", parent.display()))
}

/// Load the unit at `path`, replacing a loaded one so a rerun picks up new
/// arguments.
fn restart(manager: Manager, label: &str, path: &Path) -> Result<(), String> {
    match manager {
        Manager::Launchd => {
            // Fails harmlessly when nothing is loaded.
            let _ = run("launchctl", &["bootout", &launchd_target(label)]);
            run(
                "launchctl",
                &["bootstrap", &launchd_domain(), &path.display().to_string()],
            )
        }
        Manager::Systemd => {
            let unit = unit_name(path);
            run("systemctl", &["--user", "daemon-reload"])?;
            run("systemctl", &["--user", "enable", &unit])?;
            run("systemctl", &["--user", "restart", &unit])
        }
    }
}

/// Stop and delete the watcher unit; `None` when none was installed.
pub(super) fn uninstall() -> Result<Option<PathBuf>, String> {
    let Some(manager) = Manager::host() else {
        return Ok(None);
    };
    let label = label();
    let path = unit_path(manager, &label)?;
    if !path.exists() {
        return Ok(None);
    }
    match manager {
        Manager::Launchd => {
            let _ = run("launchctl", &["bootout", &launchd_target(&label)]);
        }
        Manager::Systemd => {
            let _ = run(
                "systemctl",
                &["--user", "disable", "--now", &unit_name(&path)],
            );
        }
    }
    std::fs::remove_file(&path)
        .map_err(|err| format!("could not remove {}: {err}", path.display()))?;
    if manager == Manager::Systemd {
        let _ = run("systemctl", &["--user", "daemon-reload"]);
    }
    Ok(Some(path))
}

fn unit_name(path: &Path) -> String {
    path.file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned())
}

fn launchd_domain() -> String {
    format!("gui/{}", rustix::process::getuid().as_raw())
}

fn launchd_target(label: &str) -> String {
    format!("{}/{label}", launchd_domain())
}

fn run(program: &str, args: &[&str]) -> Result<(), String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|err| format!("could not run {program}: {err}"))?;
    if output.status.success() {
        return Ok(());
    }
    Err(format!(
        "`{program} {}` failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr).trim()
    ))
}

pub(super) fn render_plist(plan: &WatchPlan) -> String {
    use std::fmt::Write as _;
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n<dict>\n\
         \x20 <!-- Generated by `phux host minimal setup`; `phux host minimal remove` deletes it. -->\n",
    );
    let _ = writeln!(
        out,
        "  <key>Label</key>\n  <string>{}</string>",
        xml(&plan.label)
    );
    out.push_str("  <key>ProgramArguments</key>\n  <array>\n");
    for arg in &plan.argv {
        let _ = writeln!(out, "    <string>{}</string>", xml(arg));
    }
    out.push_str("  </array>\n  <key>RunAtLoad</key>\n  <true/>\n");
    out.push_str("  <key>KeepAlive</key>\n  <true/>\n  <key>ThrottleInterval</key>\n  <integer>10</integer>\n");
    if !plan.env.is_empty() {
        out.push_str("  <key>EnvironmentVariables</key>\n  <dict>\n");
        for (key, value) in &plan.env {
            let _ = writeln!(
                out,
                "    <key>{}</key>\n    <string>{}</string>",
                xml(key),
                xml(value)
            );
        }
        out.push_str("  </dict>\n");
    }
    let log = xml(&plan.log.display().to_string());
    let _ = writeln!(
        out,
        "  <key>StandardOutPath</key>\n  <string>{log}</string>"
    );
    let _ = writeln!(
        out,
        "  <key>StandardErrorPath</key>\n  <string>{log}</string>"
    );
    out.push_str("</dict>\n</plist>\n");
    out
}

pub(super) fn render_systemd(plan: &WatchPlan) -> String {
    use std::fmt::Write as _;
    let mut out = String::from(
        "# Generated by `phux host minimal setup`; `phux host minimal remove` deletes it.\n\
         [Unit]\nDescription=phux: join Minimal boxes to this hub\n\n[Service]\n",
    );
    let exec: Vec<String> = plan.argv.iter().map(|arg| systemd_quote(arg)).collect();
    let _ = writeln!(out, "ExecStart={}", exec.join(" "));
    for (key, value) in &plan.env {
        let _ = writeln!(
            out,
            "Environment={}",
            systemd_quote(&format!("{key}={value}"))
        );
    }
    let _ = writeln!(out, "Restart=always\nRestartSec=10");
    let log = plan.log.display();
    let _ = writeln!(
        out,
        "StandardOutput=append:{log}\nStandardError=append:{log}"
    );
    out.push_str("\n[Install]\nWantedBy=default.target\n");
    out
}

fn xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Double-quote a systemd word, escaping what systemd unescapes inside one.
fn systemd_quote(word: &str) -> String {
    let escaped = word
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%")
        .replace('$', "$$");
    format!("\"{escaped}\"")
}
