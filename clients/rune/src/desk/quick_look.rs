//! ⌘Y previews the file a path in the output names: the selection's first
//! line, else the word under the pointer, resolved against the pane's cwd.
//!
//! The preview is `qlmanage -p`, not `QLPreviewPanel`: the panel finds its
//! controller through the key window's responder chain, and here that chain
//! is winit's views, which Rune doesn't let the app extend.

use std::path::Path;
use std::process::{Command, Stdio};

use phux_protocol::ids::ResourceId;

use super::input::selection::Selected;
use super::{Desk, NoticeKind};

impl Desk {
    pub fn quick_look(&mut self) {
        // A second ⌘Y closes the preview, as space does in Finder.
        if let Some(mut open) = self.pointer.quick_look.take()
            && matches!(open.try_wait(), Ok(None))
        {
            let _ = open.kill();
            let _ = open.wait();
            return;
        }
        let Some((terminal, word)) = self.quick_look_target() else {
            return self.say(
                NoticeKind::Info,
                "Select a file path or point at one, then press ⌘Y.",
            );
        };
        if self.remote(&terminal) {
            return self.say(
                NoticeKind::Error,
                "That file is on the other machine, so it can't be previewed here.",
            );
        }
        let cwd = self
            .topology
            .as_ref()
            .and_then(|t| t.pane(&terminal)?.cwd.clone());
        let Some(path) = crate::quick_look::resolve(&word, cwd.as_deref().map(Path::new)) else {
            return self.say(
                NoticeKind::Info,
                format!("There's no file called {word} here."),
            );
        };
        let preview = Command::new("qlmanage")
            .arg("-p")
            .arg(&path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        match preview {
            Ok(child) => self.pointer.quick_look = Some(child),
            Err(error) => self.say(
                NoticeKind::Error,
                format!("Couldn't preview {}: {error}", path.display()),
            ),
        }
    }

    /// The pane and the text naming the file.
    fn quick_look_target(&self) -> Option<(ResourceId, String)> {
        if let Selected::Text(text) = self.selected() {
            let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
            return Some((self.keyboard()?.0.clone(), line.to_owned()));
        }
        let (terminal, frame, col, row) = self.pointed()?;
        let word = crate::quick_look::word_at(&frame, col, row);
        (!word.is_empty()).then_some((terminal, word))
    }
}
