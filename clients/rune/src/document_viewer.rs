//! The viewer seam: source, or an argv filter. Documents are stdin data,
//! never commands, paths or URLs. The native pane owns selection and scrolling.

use std::path::Path;
use std::sync::mpsc;

mod process;
#[cfg(test)]
mod tests;

/// A trusted, locally configured adapter. No shell or document placeholders.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Viewer {
    Source,
    Command(Vec<String>),
}

impl Viewer {
    pub fn configured(argv: Option<&[String]>, executable: &Path) -> Self {
        match argv {
            _ if cfg!(target_os = "ios") => Self::Source,
            Some([]) => Self::Source,
            Some(argv) => Self::Command(argv.to_vec()),
            None => {
                let sibling = executable.parent().unwrap_or(Path::new(".")).join("tmd");
                // Installed apps never select a different binary from PATH.
                // Development runs can use tmd on PATH.
                let binary = if sibling.is_file() || !cfg!(debug_assertions) {
                    sibling.into_os_string()
                } else {
                    "tmd".into()
                };
                Self::Command(vec![
                    binary.to_string_lossy().into_owned(),
                    "--print".into(),
                    "--no-images".into(),
                    "--plain-headings".into(),
                    "--width".into(),
                    "{width}".into(),
                ])
            }
        }
    }
}

pub fn is_markdown(content_type: Option<&str>) -> bool {
    content_type.is_some_and(|value| {
        value
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .eq_ignore_ascii_case("text/markdown")
    })
}

/// At most two worker/process jobs, even if their documents were retired.
#[derive(Default)]
pub struct Pool {
    workers: Vec<std::thread::JoinHandle<()>>,
}

impl Drop for Pool {
    fn drop(&mut self) {
        // Orderly app shutdown must not orphan a renderer. Every process has
        // a deadline; its worker kills/reaps it before releasing this join.
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

impl Pool {
    fn start(&mut self, viewer: Viewer, source: String, width: u16) -> Option<Flight> {
        self.workers.retain(|worker| !worker.is_finished());
        if self.workers.len() >= 2 {
            return None;
        }
        let (send, receive) = mpsc::channel();
        let worker = std::thread::Builder::new()
            .name("document-viewer".into())
            .spawn(move || {
                let result = match viewer {
                    Viewer::Source => Ok(source),
                    Viewer::Command(argv) => process::render(&argv, &source, width),
                };
                let _ = send.send(result);
            });
        match worker {
            Ok(worker) => {
                self.workers.push(worker);
                Some(Flight(receive))
            }
            Err(error) => {
                let (send, receive) = mpsc::channel();
                let _ = send.send(Err(format!("Couldn't start viewer: {error}")));
                Some(Flight(receive))
            }
        }
    }
}

struct Flight(mpsc::Receiver<Result<String, String>>);

/// One publication's presentation. Replacing it drops the receiver, so an
/// obsolete result can never replace a newer publication or another resource.
#[derive(Default)]
pub struct Presentation {
    pub source: bool,
    width: u16,
    viewer: Option<Viewer>,
    flight: Option<Flight>,
    result: Option<Result<String, String>>,
}

impl Presentation {
    pub fn new(source: bool) -> Self {
        Self {
            source,
            ..Self::default()
        }
    }

    pub fn text<'a>(&'a self, source: &'a str) -> &'a str {
        if self.source {
            return source;
        }
        match &self.result {
            Some(Ok(text)) => text,
            _ => source,
        }
    }

    pub fn status(&self) -> &str {
        if self.source {
            return "Source";
        }
        match &self.result {
            Some(Ok(_)) => "Rendered · plain text",
            Some(Err(error)) => error,
            None => "Rendering… · showing source",
        }
    }

    pub fn toggle(&mut self) {
        self.source = !self.source;
        if !self.source {
            self.result = None;
        }
    }

    /// Returns (changed, needs another poll). No work on the UI thread.
    pub fn poll(
        &mut self,
        pool: &mut Pool,
        viewer: &Viewer,
        source: &str,
        width: u16,
    ) -> (bool, bool) {
        if self.source || width == 0 {
            return (false, false);
        }
        if self.width != width || self.viewer.as_ref() != Some(viewer) {
            self.width = width;
            self.viewer = Some(viewer.clone());
            self.flight = None;
            self.result = None;
        }
        if matches!(viewer, Viewer::Source) {
            self.source = true;
            return (true, false);
        }
        if self.result.is_some() {
            return (false, false);
        }
        if self.flight.is_none() {
            self.flight = pool.start(viewer.clone(), source.to_owned(), width);
        }
        let result = self.flight.as_ref().and_then(|f| match f.0.try_recv() {
            Ok(result) => Some(result),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                Some(Err("Viewer stopped · showing source".into()))
            }
        });
        let Some(result) = result else {
            return (false, true);
        };
        self.result = Some(result);
        self.flight = None;
        (true, false)
    }
}
