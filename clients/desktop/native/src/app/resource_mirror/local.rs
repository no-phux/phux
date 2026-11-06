//! Window and application mirrors run as this app's own children.
//!
//! macOS grants Screen Recording and Accessibility to the app that owns a
//! process. A producer the server spawns runs under the server's launchd job,
//! which no grant reaches, so its captures are refused. Run by the desktop,
//! the producer and the `screencapture` it calls are attributed to the app the
//! person already granted.
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use gpui::{Context, WeakEntity};

use super::super::desk::Desk;
use super::super::notifications::Kind;

/// A running producer. Dropping it stops the process.
pub(crate) struct LocalMirror {
    id: u64,
    child: Child,
    log: PathBuf,
}

impl Drop for LocalMirror {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The producer argv with the parent pane: the desktop is not in a pane, so
/// the mirror cannot infer one.
pub(super) fn local_argv(mut argv: Vec<String>, pane: &str) -> Vec<String> {
    argv.extend(["--pane".into(), pane.into()]);
    argv
}

impl Desk {
    /// Start `argv` as a child of this app and watch it until it exits.
    pub(super) fn start_local_mirror(
        &mut self,
        argv: &[String],
        label: &str,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let (program, args) = argv.split_first().ok_or("empty producer argv")?;
        let id = self
            .mirrors
            .iter()
            .map(|mirror| mirror.id)
            .max()
            .unwrap_or(0)
            + 1;
        let log = std::env::temp_dir().join(format!("phux-mirror-{}-{id}.log", std::process::id()));
        let stderr = std::fs::File::create(&log).map_err(|error| error.to_string())?;
        let child = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(stderr)
            .spawn()
            .map_err(|error| format!("Could not run {program}: {error}"))?;
        log::info!("local mirror {id} started: {label}");
        self.mirrors.push(LocalMirror { id, child, log });
        let label = label.to_owned();
        cx.spawn(async move |this: WeakEntity<Desk>, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let Ok(done) = this.update(cx, |desk, cx| desk.reap_mirror(id, &label, cx)) else {
                    return;
                };
                if done {
                    return;
                }
            }
        })
        .detach();
        Ok(())
    }

    /// Whether mirror `id` is gone; one that failed says why.
    fn reap_mirror(&mut self, id: u64, label: &str, cx: &mut Context<Self>) -> bool {
        let Some(index) = self.mirrors.iter().position(|mirror| mirror.id == id) else {
            return true;
        };
        let Ok(Some(status)) = self.mirrors[index].child.try_wait() else {
            return false;
        };
        let mirror = self.mirrors.remove(index);
        let reason = std::fs::read_to_string(&mirror.log).unwrap_or_default();
        let _ = std::fs::remove_file(&mirror.log);
        if !status.success() {
            let last = reason.lines().rev().find(|line| !line.trim().is_empty());
            log::warn!("local mirror {id} exited {status}: {reason}");
            self.post(
                Kind::Error,
                &format!("Mirror of {label} stopped"),
                Some(last.unwrap_or("The producer exited.").to_owned()),
            );
            cx.notify();
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_local_mirror_names_its_parent_pane() {
        let argv = local_argv(vec!["/bin/phux".into(), "agent".into()], "@7");
        assert_eq!(argv, ["/bin/phux", "agent", "--pane", "@7"]);
    }
}
