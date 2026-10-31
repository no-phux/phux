//! Things that aren't text arriving at a terminal: a clipboard image goes to
//! the server as a file and its path is pasted, for agents that read images;
//! dropped or copied files paste as quoted paths.

use std::path::{Path, PathBuf};

use phux_client_runtime::Client;
use phux_client_runtime::control::FileUploadOutcome;
use phux_protocol::ids::ResourceId;
use rune::ClipboardContent;

use super::{Desk, NoticeKind};

/// `path` as one shell word.
pub fn quoted(path: &Path) -> String {
    let path = path.to_string_lossy();
    let safe = |c: char| c.is_ascii_alphanumeric() || "/._-+=:@%,".contains(c);
    if !path.is_empty() && path.chars().all(safe) {
        return path.into_owned();
    }
    format!("'{}'", path.replace('\'', r"'\''"))
}

fn paths_line(paths: &[PathBuf]) -> String {
    let words: Vec<String> = paths.iter().map(|p| quoted(p)).collect();
    words.join(" ")
}

impl Desk {
    /// A paste of any kind: files, then an image, then text.
    pub fn paste_content(&mut self, content: &ClipboardContent) {
        if !content.files().is_empty() {
            return self.paste(&paths_line(content.files()));
        }
        if let Some(image) = content.image().filter(|_| content.text().is_empty()) {
            return self.upload_image(image.png().to_vec());
        }
        if content.text().is_empty() {
            return self.say(NoticeKind::Error, "Nothing here can be pasted.");
        }
        self.paste(content.text());
    }

    fn upload_image(&mut self, png: Vec<u8>) {
        let (Some(client), Some(shown)) = (&self.client, &self.shown) else {
            return;
        };
        let terminal = shown.terminal.clone();
        let transfer = client.put_file(terminal.clone(), "png".to_owned(), png);
        self.uploads.push((transfer, terminal));
        self.say(NoticeKind::Info, "Uploading image…");
    }

    /// Files dropped on a pane land in that pane.
    pub fn drop_files(&mut self, terminal: &ResourceId, paths: &[PathBuf]) {
        self.focus_pane(terminal);
        self.paste(&paths_line(paths));
    }

    /// Uploads finishing: paste each path where its image was pasted. True
    /// when any finished.
    pub(super) fn finish_uploads(&mut self, client: &Client) -> bool {
        let receipts = client.take_file_upload_receipts();
        let finished = !receipts.is_empty();
        for receipt in receipts {
            let Some(at) = self
                .uploads
                .iter()
                .position(|(transfer, _)| *transfer == receipt.transfer_id)
            else {
                continue;
            };
            let (_, terminal) = self.uploads.remove(at);
            match (receipt.outcome, receipt.path) {
                (FileUploadOutcome::Completed, Some(path)) => {
                    self.end_uploading_notice();
                    let line = format!("{} ", quoted(Path::new(&path)));
                    if !self.deliver_paste(&terminal, &line) {
                        self.say(
                            NoticeKind::Error,
                            format!("Image saved at {path}; not pasted."),
                        );
                    }
                }
                _ => self.say(
                    NoticeKind::Error,
                    format!("Image not sent: {}", receipt.message),
                ),
            }
        }
        finished
    }

    /// "Uploading" is over with the last upload. A failure's notice stays.
    fn end_uploading_notice(&mut self) {
        if self.uploads.is_empty() {
            self.notice
                .take_if(|notice| notice.kind == NoticeKind::Info);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;
    use crate::launch::Resolved;

    fn desk() -> Desk {
        Desk::new(&Resolved::offline("not launched"))
    }

    #[test]
    fn uploading_stays_until_the_last_upload_ends() {
        let mut desk = desk();
        desk.uploads.push((2, ResourceId::local(1)));
        desk.say(NoticeKind::Info, "Uploading image…");
        desk.end_uploading_notice();
        assert!(desk.notice.is_some(), "one is still on its way");
        desk.uploads.clear();
        desk.end_uploading_notice();
        assert!(desk.notice.is_none());
    }

    #[test]
    fn a_success_leaves_a_failure_showing() {
        let mut desk = desk();
        desk.say(NoticeKind::Error, "Image not sent: full");
        desk.end_uploading_notice();
        assert!(desk.notice.is_some());
    }

    #[test]
    fn a_notice_asks_for_the_poll_that_times_it() {
        let wakes = Arc::new(AtomicU32::new(0));
        let counted = wakes.clone();
        let mut desk = desk();
        desk.waker = Some(Arc::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
        }));
        desk.say(NoticeKind::Error, "Rename failed. Try again.");
        assert_eq!(wakes.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn plain_paths_stay_bare() {
        assert_eq!(quoted(Path::new("/tmp/a-b_c.png")), "/tmp/a-b_c.png");
    }

    #[test]
    fn anything_a_shell_reads_is_quoted() {
        assert_eq!(quoted(Path::new("/tmp/my file")), "'/tmp/my file'");
        assert_eq!(quoted(Path::new("/tmp/$(x)")), "'/tmp/$(x)'");
        assert_eq!(quoted(Path::new("it's")), r"'it'\''s'");
    }

    #[test]
    fn several_paths_make_one_line() {
        let paths = [PathBuf::from("/a"), PathBuf::from("/b c")];
        assert_eq!(paths_line(&paths), "/a '/b c'");
    }
}
