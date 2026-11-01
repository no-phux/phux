//! `phux-nvim [nvim args]`: one Neovim behind many phux panes.
//!
//! The launcher starts a daemon, detached from its pane, then becomes that
//! pane's helper. `phux-nvim daemon` and `phux-nvim pane` are the internals.

mod daemon;
mod grid;
mod input;
mod pane;
mod render;
mod rpc;

use std::fs::OpenOptions;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

const USAGE: &str = "usage: phux-nvim [nvim arguments]

Runs inside a phux pane. Every Neovim window is a real pane, sharing one
Neovim's buffers, registers, undo and language servers.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("--help" | "-h") => {
            println!("{USAGE}");
            Ok(())
        }
        Some("daemon") if args.len() >= 2 => daemon::run(Path::new(&args[1]), &args[2..]),
        Some("pane") if args.len() == 3 => match args[2].parse() {
            Ok(id) => pane::run(Path::new(&args[1]), id),
            Err(_) => Err(std::io::Error::other("bad pane id")),
        },
        _ => launch(&args),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("phux-nvim: {e}");
            ExitCode::FAILURE
        }
    }
}

/// A plugin pane starts in the plugin directory, which is nobody's project.
fn leave_plugin_dir() {
    if std::env::var_os("PHUX_PLUGIN_PANE_ID").is_some()
        && let Some(home) = std::env::var_os("HOME")
    {
        let _ = std::env::set_current_dir(home);
    }
}

fn launch(args: &[String]) -> std::io::Result<()> {
    if std::env::var_os("PHUX_TERMINAL_ID").is_none() {
        return Err(std::io::Error::other("run this inside a phux pane"));
    }
    // Said here, while there is a terminal to say it on.
    Command::new("nvim")
        .arg("--version")
        .stdout(Stdio::null())
        .status()
        .map_err(|e| std::io::Error::other(format!("can't run nvim: {e}")))?;
    leave_plugin_dir();
    let base = runtime_dir().join(format!("phux-nvim-{}", std::process::id()));
    let socket = base.with_extension("sock");
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(base.with_extension("log"))?;
    // Its own process group, so the pane's hangup doesn't reach it: closing
    // the launcher's pane must leave the other panes running.
    Command::new(std::env::current_exe()?)
        .arg("daemon")
        .arg(&socket)
        .args(args)
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log)
        .spawn()?;
    pane::run(&socket, 0)
}

/// Short and private: a Unix socket's path has a small limit.
fn runtime_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .or_else(|| std::env::var_os("TMPDIR"))
        .map_or_else(|| PathBuf::from("/tmp"), PathBuf::from)
}
