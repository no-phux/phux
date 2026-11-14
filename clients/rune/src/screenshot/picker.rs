//! Own exactly the picker child we spawned. A window's drop cancels and
//! reaps it; the worker can never retain a screen picker after its owner left.

use std::io;
use std::process::{Child, Command, ExitStatus};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone, Default)]
pub(crate) struct Picker(Arc<Mutex<Process>>);

#[derive(Default)]
struct Process {
    child: Option<Child>,
    cancelled: bool,
}

impl Picker {
    pub(crate) fn cancel(&self) {
        let Ok(mut process) = self.0.lock() else {
            return;
        };
        process.cancelled = true;
        if let Some(mut child) = process.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    #[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]
    pub(super) fn run(&self, command: &mut Command) -> io::Result<ExitStatus> {
        {
            let mut process = self.0.lock().map_err(|_| io::Error::other("picker lock"))?;
            if process.cancelled {
                return Err(io::ErrorKind::Interrupted.into());
            }
            process.child = Some(command.spawn()?);
        }
        loop {
            {
                let mut process = self.0.lock().map_err(|_| io::Error::other("picker lock"))?;
                let child = process.child.as_mut().ok_or(io::ErrorKind::Interrupted)?;
                if let Some(status) = child.try_wait()? {
                    process.child = None;
                    return Ok(status);
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests {
    use super::*;
    use std::process::Stdio;

    #[test]
    fn cancellation_reaps_only_the_owned_child() {
        let picker = Picker::default();
        // No GUI, shell grandchild or screen permission: our PID becomes sleep.
        let child = Command::new("/bin/sleep")
            .arg("60")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        picker.0.lock().unwrap().child = Some(child);
        picker.cancel();
        let process = picker.0.lock().unwrap();
        assert!(process.cancelled);
        assert!(process.child.is_none(), "owned PID {pid} was reaped");
        drop(process);
        // Cancel-before-spawn must not start even an inert command.
        assert_eq!(
            picker
                .run(&mut Command::new("/usr/bin/true"))
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
    }
}
