//! A curated demo shell: turns keystrokes into VT output bytes. No OS, no
//! processes — a small line editor + command interpreter over an in-memory FS,
//! so it runs anywhere (a Durable Object, the browser, a test). The browser's
//! libghostty-vt engine renders whatever bytes this emits.

use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use serde::{Deserialize, Serialize};

const MAX_INPUT_LINE_BYTES: usize = 4096;

// SGR / control sequences.
const RESET: &str = "\x1b[0m";
const VIOLET: &str = "\x1b[38;5;141m"; // flux violet
const CYAN: &str = "\x1b[38;5;80m"; // signal cyan
const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";

/// A curated, OS-less shell. Owns the current input line and a tiny FS.
pub struct Shell {
    line: String,
    portfolio: Option<PortfolioShell>,
}

impl Shell {
    pub fn new(mode: &str, snapshot_json: &str) -> Self {
        Self {
            line: String::new(),
            portfolio: (mode == "portfolio" || mode == "native-fallback")
                .then(|| PortfolioShell::new(snapshot_json, mode == "native-fallback")),
        }
    }

    /// The prompt string (on its own, no leading newline).
    fn prompt(&self) -> String {
        format!("{VIOLET}phux{RESET} {DIM}demo{RESET} $ ")
    }

    /// Bytes to paint on attach: a welcome banner, then the first prompt.
    pub fn greeting(&self) -> Vec<u8> {
        if let Some(portfolio) = &self.portfolio {
            return portfolio.render().into_bytes();
        }
        let mut o = String::new();
        o.push_str("\x1b[2J\x1b[H"); // clear + home
        o.push_str(&logo());
        o.push_str("\r\n");
        o.push_str(&format!(
            "  {DIM}a real phux session, served from the edge — type into it.{RESET}\r\n\r\n"
        ));
        o.push_str(&format!("  try:  {BOLD}help{RESET}   {BOLD}demo{RESET}   {BOLD}ls{RESET}   {BOLD}demo links{RESET}\r\n\r\n"));
        o.push_str(&self.prompt());
        o.into_bytes()
    }

    /// Process one key event; return the VT bytes to emit (may be empty).
    pub fn input(&mut self, ev: &KeyEvent) -> Vec<u8> {
        if ev.action == KeyAction::Release {
            return Vec::new();
        }

        if let Some(portfolio) = &mut self.portfolio {
            return portfolio.input(ev).into_bytes();
        }

        // Ctrl-C: abandon the line.
        if ev.mods.contains(ModSet::CTRL) && ev.key == PhysicalKey::C {
            self.line.clear();
            return format!("^C\r\n{}", self.prompt()).into_bytes();
        }
        // Ctrl-L: clear screen, keep the line.
        if ev.mods.contains(ModSet::CTRL) && ev.key == PhysicalKey::L {
            return format!("\x1b[2J\x1b[H{}{}", self.prompt(), self.line).into_bytes();
        }

        match ev.key {
            PhysicalKey::Enter | PhysicalKey::NumpadEnter => {
                let line = std::mem::take(&mut self.line);
                let mut out = String::from("\r\n");
                out.push_str(&run(line.trim()));
                out.push_str(&self.prompt());
                out.into_bytes()
            }
            PhysicalKey::Backspace | PhysicalKey::NumpadBackspace => {
                if self.line.pop().is_some() {
                    b"\x08 \x08".to_vec() // erase one cell
                } else {
                    Vec::new()
                }
            }
            _ => {
                // Printable input: the client fills `text` for single, non-Ctrl
                // characters. Echo it and buffer it.
                if let Some(t) = ev.text.as_deref()
                    && !t.is_empty()
                    && !ev.mods.contains(ModSet::CTRL)
                    && !ev.mods.contains(ModSet::SUPER)
                    && self.line.len().saturating_add(t.len()) <= MAX_INPUT_LINE_BYTES
                {
                    self.line.push_str(t);
                    return t.as_bytes().to_vec();
                }
                Vec::new()
            }
        }
    }

    pub fn checkpoint(&self) -> ShellCheckpoint {
        match &self.portfolio {
            Some(portfolio) => ShellCheckpoint::Portfolio {
                selected: portfolio.selected,
                detail: portfolio.detail,
            },
            None => ShellCheckpoint::Demo {
                line: self.line.clone(),
            },
        }
    }

    pub fn restore(&mut self, checkpoint: ShellCheckpoint) -> Result<(), String> {
        match (checkpoint, &mut self.portfolio) {
            (ShellCheckpoint::Demo { line }, None) => {
                if line.len() > MAX_INPUT_LINE_BYTES {
                    return Err("input line is too long".to_owned());
                }
                self.line = line;
                Ok(())
            }
            (ShellCheckpoint::Portfolio { selected, detail }, Some(portfolio)) => {
                let count = portfolio.snapshot.repos.len();
                if (count == 0 && selected != 0) || (count > 0 && selected >= count) {
                    return Err("portfolio selection is out of range".to_owned());
                }
                if count == 0 && detail {
                    return Err("portfolio detail requires a repository".to_owned());
                }
                portfolio.selected = selected;
                portfolio.detail = detail;
                Ok(())
            }
            _ => Err("checkpoint shell kind does not match mode".to_owned()),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ShellCheckpoint {
    Demo { line: String },
    Portfolio { selected: usize, detail: bool },
}

#[derive(Deserialize, Default)]
struct PortfolioSnapshot {
    #[serde(default)]
    fetched_at: String,
    #[serde(default)]
    repos: Vec<PortfolioRepo>,
    error: Option<String>,
}

#[derive(Deserialize)]
struct PortfolioRepo {
    name: String,
    description: Option<String>,
    url: String,
    homepage: Option<String>,
    language: Option<String>,
    stars: u64,
    forks: u64,
    open_issues: u64,
    pushed_at: String,
    latest_run: Option<PortfolioRun>,
}

#[derive(Deserialize)]
struct PortfolioRun {
    workflow: String,
    branch: String,
    status: String,
    conclusion: Option<String>,
    url: String,
    updated_at: String,
}

struct PortfolioShell {
    snapshot: PortfolioSnapshot,
    selected: usize,
    detail: bool,
    native_fallback: bool,
}

impl PortfolioShell {
    fn new(snapshot_json: &str, native_fallback: bool) -> Self {
        let snapshot = serde_json::from_str(snapshot_json).unwrap_or_else(|_| PortfolioSnapshot {
            error: Some("live portfolio data unavailable".to_owned()),
            ..PortfolioSnapshot::default()
        });
        Self {
            snapshot,
            selected: 0,
            detail: false,
            native_fallback,
        }
    }

    fn input(&mut self, ev: &KeyEvent) -> String {
        let count = self.snapshot.repos.len();
        match ev.key {
            PhysicalKey::ArrowDown | PhysicalKey::J if count > 0 => {
                self.selected = (self.selected + 1) % count;
            }
            PhysicalKey::ArrowUp | PhysicalKey::K if count > 0 => {
                self.selected = (self.selected + count - 1) % count;
            }
            PhysicalKey::Home if count > 0 => self.selected = 0,
            PhysicalKey::End if count > 0 => self.selected = count - 1,
            PhysicalKey::Enter | PhysicalKey::NumpadEnter | PhysicalKey::ArrowRight
                if count > 0 =>
            {
                self.detail = true
            }
            PhysicalKey::Escape | PhysicalKey::Backspace | PhysicalKey::ArrowLeft
                if self.detail =>
            {
                self.detail = false
            }
            _ => return String::new(),
        }
        self.render()
    }

    fn render(&self) -> String {
        let mut out = String::from("\x1b[2J\x1b[H");
        out.push_str(&format!(
            "  {VIOLET}{BOLD}phall.io{RESET}  {DIM}live systems console{RESET}\r\n"
        ));
        if self.native_fallback {
            out.push_str(&format!(
                "  {BOLD}Native shells are busy or unavailable.{RESET} This is the instant edge tour.\r\n"
            ));
        }
        out.push_str(&format!(
            "  {DIM}VISITOR · READ ONLY · powered by phux · refreshed {}{RESET}\r\n",
            short_date(&self.snapshot.fetched_at)
        ));
        out.push_str("\r\n");

        if let Some(error) = &self.snapshot.error {
            out.push_str(&format!("  {DIM}{error}{RESET}\r\n"));
            return out;
        }
        if self.snapshot.repos.is_empty() {
            out.push_str(&format!(
                "  {DIM}no public showcase repositories found{RESET}\r\n"
            ));
            return out;
        }

        if self.detail {
            out.push_str(&self.render_detail());
        } else {
            out.push_str(&self.render_list());
        }
        out
    }

    fn render_list(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("  {DIM}PROJECT              STACK          LATEST AUTOMATION                 UPDATED{RESET}\r\n"));
        out.push_str(&format!("  {DIM}──────────────────────────────────────────────────────────────────────────────────────────{RESET}\r\n"));
        for (index, repo) in self.snapshot.repos.iter().enumerate() {
            let selected = index == self.selected;
            let marker = if selected { "▸" } else { " " };
            let run = repo
                .latest_run
                .as_ref()
                .map_or_else(|| "· no workflow run".to_owned(), run_label);
            let row = format!(
                "{marker} {:<20} {:<14} {:<33} {:<10}",
                truncate(&repo.name, 20),
                truncate(repo.language.as_deref().unwrap_or("—"), 14),
                truncate(&run, 33),
                short_date(&repo.pushed_at),
            );
            if selected {
                out.push_str(&format!("\x1b[7m  {row:<94}\x1b[27m\r\n"));
            } else {
                out.push_str(&format!("  {row}\r\n"));
            }
        }
        out.push_str("\r\n");
        out.push_str(&format!("  {DIM}↑↓ / j k navigate   enter inspect   home/end jump   sessions expire automatically{RESET}\r\n"));
        out
    }

    fn render_detail(&self) -> String {
        let repo = &self.snapshot.repos[self.selected];
        let mut out = String::new();
        out.push_str(&format!(
            "  {BOLD}{}{RESET}\r\n",
            osc8(&repo.url, &repo.name)
        ));
        out.push_str(&format!(
            "  {}\r\n\r\n",
            repo.description.as_deref().unwrap_or("No description.")
        ));
        out.push_str(&format!(
            "  language     {}\r\n",
            repo.language.as_deref().unwrap_or("—")
        ));
        out.push_str(&format!("  stars        {}\r\n", repo.stars));
        out.push_str(&format!("  forks        {}\r\n", repo.forks));
        out.push_str(&format!("  open issues  {}\r\n", repo.open_issues));
        out.push_str(&format!(
            "  pushed       {}\r\n",
            short_date(&repo.pushed_at)
        ));
        if let Some(homepage) = &repo.homepage {
            out.push_str(&format!("  homepage     {}\r\n", osc8(homepage, homepage)));
        }
        if let Some(run) = &repo.latest_run {
            out.push_str("\r\n");
            out.push_str(&format!("  {DIM}LATEST AUTOMATION{RESET}\r\n"));
            out.push_str(&format!(
                "  {}  {}  {}  {}\r\n",
                run_glyph(run),
                run.workflow,
                run.branch,
                run_state(run)
            ));
            if !run.url.is_empty() {
                out.push_str(&format!("  {}\r\n", osc8(&run.url, "open workflow run ↗")));
            }
            out.push_str(&format!("  updated {}\r\n", short_date(&run.updated_at)));
        }
        out.push_str("\r\n");
        out.push_str(&format!("  {DIM}esc / ← back   links are clickable   GitHub access is structurally read-only{RESET}\r\n"));
        out
    }
}

fn run_glyph(run: &PortfolioRun) -> &'static str {
    if run.status != "completed" {
        "●"
    } else if run.conclusion.as_deref() == Some("success") {
        "✓"
    } else {
        "✗"
    }
}

fn run_state(run: &PortfolioRun) -> String {
    if run.status == "completed" {
        run.conclusion
            .clone()
            .unwrap_or_else(|| "completed".to_owned())
    } else {
        run.status.replace('_', " ")
    }
}

fn run_label(run: &PortfolioRun) -> String {
    format!("{} {} · {}", run_glyph(run), run.workflow, run_state(run))
}

fn truncate(value: &str, width: usize) -> String {
    if value.chars().count() <= width {
        return value.to_owned();
    }
    value
        .chars()
        .take(width.saturating_sub(1))
        .collect::<String>()
        + "…"
}

fn short_date(value: &str) -> &str {
    value.get(..10).unwrap_or(value)
}

fn osc8(url: &str, text: &str) -> String {
    format!("\x1b]8;;{url}\x1b\\\x1b[4m{text}\x1b[24m\x1b]8;;\x1b\\")
}

/// Run a command line; return its VT output (commands end their own lines).
fn run(line: &str) -> String {
    if line.is_empty() {
        return String::new();
    }
    let mut parts = line.split_whitespace();
    let cmd = parts.next().unwrap_or("");
    let args: Vec<&str> = parts.collect();

    match cmd {
        "help" => help(),
        "ls" => fs_list(),
        "cat" => cat(&args),
        "pwd" => "/home/guest/phux\r\n".to_owned(),
        "echo" => format!("{}\r\n", args.join(" ")),
        "whoami" => "guest\r\n".to_owned(),
        "uname" => "phux-edge (wasm) · Cloudflare Durable Object\r\n".to_owned(),
        "clear" => "\x1b[2J\x1b[H".to_owned(),
        "date" => "a fine day on the wire\r\n".to_owned(),
        "demo" => demo(&args),
        "" => String::new(),
        other => format!("phux: {other}: command not found\r\n"),
    }
}

fn help() -> String {
    format!(
        "{BOLD}phux demo shell{RESET} — a curated, OS-less terminal on the edge.\r\n\
         \r\n\
         {BOLD}demo{RESET}        the showcase: logo, truecolor, hyperlinks\r\n\
         {BOLD}demo links{RESET}  OSC 8 hyperlinks + 24-bit truecolor\r\n\
         {BOLD}ls / cat{RESET}    poke around a tiny in-memory filesystem\r\n\
         {BOLD}echo / pwd / whoami / uname / clear{RESET}\r\n\
         \r\n\
         {DIM}every byte here crosses the real phux wire — the browser renders\r\n\
         it with the same libghostty engine native phux uses.{RESET}\r\n"
    )
}

// ── a tiny in-memory filesystem ──────────────────────────────────────────────
const FILES: &[(&str, &str)] = &[
    (
        "README",
        "phux — a terminal is an object you spawn, observe, and drive over a wire.\r\n\
         This demo runs that wire end-to-end, with the server living in a\r\n\
         Cloudflare Durable Object (WASM) instead of a container.\r\n",
    ),
    (
        "the-wire.txt",
        "The same libghostty-vt engine runs on both ends, so Kitty graphics,\r\n\
         OSC 8 hyperlinks, sixel, and 24-bit color pass through untouched.\r\n",
    ),
];

fn fs_list() -> String {
    let mut names: Vec<&str> = FILES.iter().map(|(n, _)| *n).collect();
    names.sort_unstable();
    format!("{}\r\n", names.join("   "))
}

fn cat(args: &[&str]) -> String {
    let Some(name) = args.first() else {
        return "cat: missing file operand\r\n".to_owned();
    };
    FILES.iter().find(|(n, _)| n == name).map_or_else(
        || format!("cat: {name}: No such file\r\n"),
        |(_, c)| (*c).to_owned(),
    )
}

// ── the showcase ─────────────────────────────────────────────────────────────
fn logo() -> String {
    format!("  {VIOLET}{BOLD}▟█▙  phux{RESET}  {DIM}terminal control plane{RESET}\r\n")
}

fn demo(args: &[&str]) -> String {
    match args.first().copied() {
        Some("links") => links(),
        Some("colors") => format!("{}\r\n", truecolor_bar()),
        Some("logo") => format!("{}\r\n", logo()),
        _ => {
            // `demo` / `demo all`
            let mut o = String::new();
            o.push_str(&logo());
            o.push_str("\r\n");
            o.push_str(&links());
            o
        }
    }
}

fn truecolor_bar() -> String {
    let mut s = String::from("  truecolor: ");
    for i in 0..40u32 {
        let r = 40 + i * 5;
        let g = 20 + i * 3;
        let b = (90 + i * 4).min(255);
        s.push_str(&format!("\x1b[48;2;{r};{g};{b}m \x1b[0m"));
    }
    s
}

fn links() -> String {
    let osc8 =
        |url: &str, text: &str| format!("\x1b]8;;{url}\x1b\\\x1b[4m{text}\x1b[24m\x1b]8;;\x1b\\");
    format!(
        "  {BOLD}OSC 8 hyperlinks + truecolor — passed through, not re-implemented:{RESET}\r\n\
         \r\n\
         {CYAN}  {}{RESET}\r\n\
         {CYAN}  {}{RESET}\r\n\
         {}\r\n",
        osc8("https://phux.sh", "phux.sh"),
        osc8("https://github.com/phall1/phux", "github.com/phall1/phux"),
        truecolor_bar(),
    )
}

#[cfg(test)]
mod tests {
    use super::PortfolioShell;

    #[test]
    fn invalid_portfolio_data_fails_closed() {
        let shell = PortfolioShell::new("not json", false);
        assert!(shell.render().contains("live portfolio data unavailable"));
    }

    #[test]
    fn portfolio_renders_only_supplied_repositories() {
        let shell = PortfolioShell::new(
            r#"{"schema":1,"owner":"phall1","fetched_at":"2026-07-11T00:00:00Z","repos":[{"name":"phui","description":"GitHub TUI","url":"https://github.com/phall1/phui","homepage":"https://phall.io","language":"TypeScript","stars":1,"forks":2,"open_issues":3,"pushed_at":"2026-07-11T00:00:00Z","latest_run":null}]}"#,
            false,
        );
        let rendered = shell.render();
        assert!(rendered.contains("phui"));
        assert!(!rendered.contains("private-repo"));
    }

    #[test]
    fn native_fallback_is_honest() {
        let shell = PortfolioShell::new(
            r#"{"schema":1,"owner":"phall1","fetched_at":"2026-07-11T00:00:00Z","repos":[]}"#,
            true,
        );
        let rendered = shell.render();
        assert!(rendered.contains("Native shells are busy or unavailable"));
        assert!(rendered.contains("instant edge tour"));
    }
}
