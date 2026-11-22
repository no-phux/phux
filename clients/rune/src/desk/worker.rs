//! Work off the main thread whose answer comes back as news on a channel.

use std::sync::mpsc::Sender;
use std::time::Duration;

use rune::Waker;

use super::Desk;
use crate::launch::run_for;

/// Run `job` on its own thread; each answer it sends wakes the window.
pub(super) fn spawn<D: Send + 'static>(
    sender: Sender<D>,
    waker: Option<Waker>,
    job: impl FnOnce(&dyn Fn(D)) + Send + 'static,
) {
    std::thread::spawn(move || {
        let send = |done| {
            let _ = sender.send(done);
            if let Some(waker) = &waker {
                waker();
            }
        };
        job(&send);
    });
}

impl Desk {
    /// `phux ARGS` on a worker, `done` turning its output into news on
    /// `sender`. `server` prepends `--socket` for this computer's server.
    pub(super) fn ask_phux<D: Send + 'static>(
        &self,
        sender: Sender<D>,
        server: bool,
        args: &[&str],
        timeout: Duration,
        done: impl FnOnce(Result<Vec<u8>, String>) -> D + Send + 'static,
    ) {
        let Some(phux) = self.launch.phux.clone() else {
            let _ = sender.send(done(Err("phux is not installed".to_owned())));
            return;
        };
        let mut argv: Vec<String> = match &self.launch.socket {
            Ok(socket) if server => vec!["--socket".to_owned(), socket.display().to_string()],
            _ => Vec::new(),
        };
        argv.extend(args.iter().map(|a| (*a).to_owned()));
        spawn(sender, self.waker.clone(), move |send| {
            let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
            send(done(run_for(&phux, &argv, timeout)));
        });
    }
}
