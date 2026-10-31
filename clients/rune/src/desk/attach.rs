//! Things that aren't text arriving at a terminal: a clipboard image goes to
//! the server as a file and its path is pasted, for agents that read images;
//! dropped or copied files paste as quoted paths.

use std::path::{Path, PathBuf};

use phux_client_runtime::Client;
use phux_client_runtime::control::FileUploadOutcome;
use phux_protocol::ids::ResourceId;
use rune::ClipboardContent;

use super::Desk;

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
        self.paste(content.text());
    }

    fn upload_image(&mut self, png: Vec<u8>) {
        let (Some(client), Some(shown)) = (&self.client, &self.shown) else {
            return;
        };
        let terminal = shown.terminal.clone();
        let transfer = client.put_file(terminal.clone(), "png".to_owned(), png);
        self.uploads.push((transfer, terminal));
        self.notice = Some("Uploading image…".to_owned());
    }

    /// Files dropped on a pane land in that pane.
    pub fn drop_files(&mut self, terminal: &ResourceId, paths: &[PathBuf]) {
        self.focus_pane(terminal);
        self.paste(&paths_line(paths));
    }

    /// Uploads finishing: paste each path where its image was pasted.
    pub(super) fn finish_uploads(&mut self, client: &Client) {
        for receipt in client.take_file_upload_receipts() {
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
                    self.notice = None;
                    let line = format!("{} ", quoted(Path::new(&path)));
                    client.with_control(|control| control.apply_user_paste(&terminal, &line));
                    client.wake();
                }
                _ => self.notice = Some(format!("Image not sent: {}", receipt.message)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
