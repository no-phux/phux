//! `phux pick`: a chooser for scripts. The list is drawn on the calling
//! terminal, and inside a phux pane the `phux-ask` title sentinel (ADR-0035)
//! offers the same items as an ask, so a client's question card can answer
//! it. Either way the answer is a line typed into this terminal, so there is
//! one reader and no new wire message.

use std::fmt::Write as _;
use std::fs::File;
use std::io::{self, BufRead, Read, Write};
use std::process::ExitCode;

use rustix::termios::{self, LocalModes, OptionalActions};

use crate::exit_codes::{EXIT_FAILURE, EXIT_USAGE};

/// The ask contract's own ceiling on suggestions (`REPORT_ASKED`).
// ponytail: a client card offers only the first 16; the terminal list has all.
const MAX_SUGGESTIONS: usize = 16;

/// What one chunk of terminal input decided.
#[derive(Debug, PartialEq, Eq)]
enum Step {
    More,
    Picked(String),
    Cancel,
}

/// The line typed so far, against the items it may name.
struct Picker<'a> {
    items: &'a [String],
    line: Vec<u8>,
}

impl Picker<'_> {
    /// Feed one `read` of raw terminal bytes; `echo` gets what to draw.
    fn feed(&mut self, chunk: &[u8], echo: &mut Vec<u8>) -> Step {
        for (at, &byte) in chunk.iter().enumerate() {
            match byte {
                // A lone Escape cancels; one that starts a key sequence (an
                // arrow) arrives with its tail in the same read.
                // ponytail: a sequence split across reads reads as Escape.
                0x1b if at + 1 == chunk.len() => return Step::Cancel,
                0x1b => return Step::More,
                0x03 | 0x04 => return Step::Cancel,
                b'\r' | b'\n' => {
                    let typed = String::from_utf8_lossy(&self.line).into_owned();
                    self.line.clear();
                    if let Some(item) = resolve(self.items, &typed) {
                        echo.extend_from_slice(b"\r\n");
                        return Step::Picked(item.to_owned());
                    }
                    if !typed.trim().is_empty() {
                        echo.extend_from_slice(b"\r\n  no such choice");
                    }
                    echo.extend_from_slice(b"\r\n> ");
                }
                0x7f | 0x08 => {
                    // Drop a whole UTF-8 character, not one byte of it.
                    while let Some(last) = self.line.pop() {
                        if last & 0xc0 != 0x80 {
                            echo.extend_from_slice(b"\x08 \x08");
                            break;
                        }
                    }
                }
                byte if byte >= 0x20 => {
                    self.line.push(byte);
                    echo.push(byte);
                }
                _ => {}
            }
        }
        Step::More
    }
}

/// The item `typed` names: its exact text (what a client's card types), else
/// its 1-based number in the list.
fn resolve<'a>(items: &'a [String], typed: &str) -> Option<&'a str> {
    let typed = typed.trim();
    items
        .iter()
        .find(|item| item.trim() == typed)
        .or_else(|| {
            let index: usize = typed.parse().ok()?;
            items.get(index.checked_sub(1)?)
        })
        .map(String::as_str)
}

/// The OSC 2 sentinel offering `items` as answers to `prompt`. Items a title
/// cannot carry (a `|`, a control character) stay terminal-only.
fn ask_title(id: &str, prompt: &str, items: &[String]) -> String {
    let clean = |text: &str| text.chars().filter(|c| !c.is_control()).collect::<String>();
    let suggestions: Vec<String> = items
        .iter()
        .map(|item| clean(item))
        .filter(|item| !item.contains('|') && !item.trim().is_empty())
        .take(MAX_SUGGESTIONS)
        .collect();
    let question = clean(prompt).replace("?s=", "? s=");
    format!(
        "\x1b]2;phux-ask[{id}]:{question}?s={}\x07",
        suggestions.join("|")
    )
}

/// The items: the arguments, else stdin's nonblank lines.
fn items_from(args: Vec<String>, stdin: impl BufRead) -> io::Result<Vec<String>> {
    if !args.is_empty() {
        return Ok(args);
    }
    let mut items = Vec::new();
    for line in stdin.lines() {
        let line = line?;
        if !line.trim().is_empty() {
            items.push(line);
        }
    }
    Ok(items)
}

/// Draw the list, take raw keys until a choice or a cancel, then restore the
/// terminal mode and retract the ask.
fn pick_on(tty: &mut File, prompt: &str, items: &[String], ask: Option<&str>) -> io::Result<Step> {
    let saved = termios::tcgetattr(&*tty)?;
    let mut raw = saved.clone();
    // ISIG off: Ctrl-C is a cancel this loop sees, so the title is retracted.
    raw.local_modes
        .remove(LocalModes::ICANON | LocalModes::ECHO | LocalModes::ISIG);
    raw.special_codes[termios::SpecialCodeIndex::VMIN] = 1;
    raw.special_codes[termios::SpecialCodeIndex::VTIME] = 0;
    termios::tcsetattr(&*tty, OptionalActions::Now, &raw)?;

    // Bracketed paste off, so a client's typed answer arrives as plain text;
    // the title is pushed, so popping it after restores what was there.
    let mut screen = String::from("\x1b[?2004l");
    if let Some(title) = ask {
        screen.push_str("\x1b[22;0t");
        screen.push_str(title);
    }
    let _ = write!(screen, "{prompt}\r\n");
    for (n, item) in items.iter().enumerate() {
        let _ = write!(screen, "  {}) {item}\r\n", n + 1);
    }
    screen.push_str("> ");
    let result = tty.write_all(screen.as_bytes()).and_then(|()| {
        let mut picker = Picker {
            items,
            line: Vec::new(),
        };
        let mut buf = [0u8; 1024];
        loop {
            let n = tty.read(&mut buf)?;
            if n == 0 {
                break Ok(Step::Cancel);
            }
            let mut echo = Vec::new();
            let step = picker.feed(&buf[..n], &mut echo);
            tty.write_all(&echo)?;
            if step != Step::More {
                break Ok(step);
            }
        }
    });
    if ask.is_some() {
        // An empty title retracts the ask even where the pop is unsupported.
        let _ = tty.write_all(b"\x1b]2;\x07\x1b[23;0t");
    }
    let _ = termios::tcsetattr(&*tty, OptionalActions::Now, &saved);
    result
}

/// Run `phux pick` as parsed.
pub(crate) fn run(args: crate::commands::PickArgs) -> ExitCode {
    let crate::commands::PickArgs { prompt, items } = args;
    let items = match items_from(items, io::stdin().lock()) {
        Ok(items) if !items.is_empty() => items,
        Ok(_) => {
            error!("nothing to pick: pass items as arguments or one per line on stdin");
            return ExitCode::from(EXIT_USAGE);
        }
        Err(err) => {
            error!("cannot read items from stdin: {err}");
            return ExitCode::from(EXIT_FAILURE);
        }
    };
    let mut tty = match File::options().read(true).write(true).open("/dev/tty") {
        Ok(tty) => tty,
        Err(err) => {
            error!("phux pick needs a terminal to show the list on: /dev/tty: {err}");
            return ExitCode::from(EXIT_USAGE);
        }
    };
    let in_pane =
        std::env::var_os(crate::environment::TERMINAL_ID_ENV).is_some_and(|v| !v.is_empty());
    let ask = in_pane.then(|| ask_title(&format!("pick-{}", std::process::id()), &prompt, &items));
    match pick_on(&mut tty, &prompt, &items, ask.as_deref()) {
        Ok(Step::Picked(item)) => {
            outln!("{item}");
            ExitCode::SUCCESS
        }
        Ok(_) => ExitCode::from(EXIT_FAILURE),
        Err(err) => {
            error!("phux pick: {err}");
            ExitCode::from(EXIT_FAILURE)
        }
    }
}

#[cfg(test)]
mod tests;
