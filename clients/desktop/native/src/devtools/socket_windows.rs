//! The Unix peer-credential dev socket is intentionally not exposed over TCP
//! on Windows. The in-app HUD, monitor and trace capture remain available.
use std::path::{Path, PathBuf};

pub fn path_for(_server_socket: Option<&Path>) -> anyhow::Result<PathBuf> {
    anyhow::bail!(
        "The external developer socket is Unix-only. Use the in-app Performance Monitor on Windows."
    )
}

pub fn serve(_path: &Path) -> anyhow::Result<PathBuf> {
    anyhow::bail!("The external developer socket is not supported on Windows.")
}

pub fn stop(_path: &Path) {}
