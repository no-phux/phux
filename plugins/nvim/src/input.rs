//! Terminal input to Neovim's terms: key notation for `nvim_input`, pastes
//! for `nvim_paste`, and mouse events for `nvim_input_mouse`.

use std::fmt::Write as _;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// In `nvim_input` notation, such as `<C-w>`.
    Key(String),
    /// One piece of a streamed paste, with its `nvim_paste` phase.
    Paste(String, i64),
    Mouse(Mouse),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mouse {
    pub button: &'static str,
    pub action: &'static str,
    /// As in key names: `C-A-S-`.
    pub mods: String,
    pub row: i32,
    pub col: i32,
}

impl Mouse {
    /// A press that picks a place, not a wheel notch.
    pub fn is_press(&self) -> bool {
        self.action == "press"
    }
}

const PASTE_END: &[u8] = b"\x1b[201~";

/// Holds what a read cut short: a sequence or a paste's tail.
#[derive(Debug, Default)]
pub struct Parser {
    buf: Vec<u8>,
    /// Inside a bracketed paste; the next `nvim_paste` phase.
    paste: Option<i64>,
}

impl Parser {
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Event> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        let mut i = 0;
        while i < self.buf.len() {
            let used = if self.paste.is_some() {
                self.paste_chunk(i, &mut out)
            } else {
                self.one(i, &mut out)
            };
            let Some(n) = used else { break };
            i += n;
        }
        self.buf.drain(..i);
        out
    }

    /// Consumes paste text from `i`, keeping back a possible partial end
    /// marker.
    fn paste_chunk(&mut self, i: usize, out: &mut Vec<Event>) -> Option<usize> {
        let rest = &self.buf[i..];
        let phase = self.paste?;
        let end = rest.windows(PASTE_END.len()).position(|w| w == PASTE_END);
        let body = end.unwrap_or_else(|| {
            let keep = (1..PASTE_END.len())
                .rev()
                .find(|&k| rest.ends_with(&PASTE_END[..k]))
                .unwrap_or(0);
            rest.len() - keep
        });
        let text = String::from_utf8_lossy(&rest[..body]).replace('\r', "\n");
        let mut phase = phase;
        if !text.is_empty() {
            out.push(Event::Paste(text, phase));
            phase = 2;
        }
        if end.is_some() {
            if phase == 2 {
                out.push(Event::Paste(String::new(), 3));
            }
            self.paste = None;
            return Some(body + PASTE_END.len());
        }
        self.paste = Some(phase);
        (body > 0).then_some(body)
    }

    /// One key or sequence at `i`. None waits for more bytes.
    fn one(&mut self, i: usize, out: &mut Vec<Event>) -> Option<usize> {
        let rest = &self.buf[i..];
        let b = rest[0];
        if b != 0x1b {
            let n = utf8_len(b).min(rest.len());
            if let Some(key) = plain(&rest[..n]) {
                out.push(Event::Key(key));
            }
            return (rest.len() >= utf8_len(b)).then_some(n);
        }
        match rest.get(1) {
            // Neovim does its own Esc timing; a lone ESC is the key.
            None => {
                out.push(Event::Key("<Esc>".into()));
                Some(1)
            }
            Some(b'[') => self.csi(i, out),
            Some(b'O') => {
                let &f = rest.get(2)?;
                if let Some(name) = letter_key(f) {
                    out.push(Event::Key(format!("<{name}>")));
                }
                Some(3)
            }
            Some(_) => {
                let n = utf8_len(rest[1]);
                let s = rest.get(1..1 + n)?;
                let key = plain(s).map_or_else(|| "<Esc>".to_owned(), |k| alt(&k));
                out.push(Event::Key(key));
                Some(1 + n)
            }
        }
    }

    fn csi(&mut self, i: usize, out: &mut Vec<Event>) -> Option<usize> {
        let rest = &self.buf[i + 2..];
        let end = rest.iter().position(|b| (0x40..=0x7e).contains(b))?;
        let params = &rest[..end];
        let fin = rest[end];
        let used = 2 + end + 1;
        if params.first() == Some(&b'<') {
            if let Some(m) = mouse(&params[1..], fin) {
                out.push(Event::Mouse(m));
            }
            return Some(used);
        }
        let text = std::str::from_utf8(params).unwrap_or("");
        match (fin, text) {
            (b'~', "200") => self.paste = Some(1),
            // Focus reports: which pane is typed in says this better.
            (b'I' | b'O', _) => {}
            (b'u', _) => out.extend(kitty(text).map(Event::Key)),
            (b'~', _) => out.extend(tilde(text).map(Event::Key)),
            (b'Z', _) => out.push(Event::Key("<S-Tab>".into())),
            _ => out.extend(letter(fin, text).map(Event::Key)),
        }
        Some(used)
    }
}

const fn utf8_len(b: u8) -> usize {
    match b {
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => 1,
    }
}

fn alt(key: &str) -> String {
    key.strip_prefix('<')
        .map_or_else(|| format!("<A-{key}>"), |inner| format!("<A-{inner}"))
}

/// A key from legacy bytes: text, or a C0 control.
fn plain(bytes: &[u8]) -> Option<String> {
    let [b] = bytes else {
        return std::str::from_utf8(bytes).ok().map(str::to_owned);
    };
    Some(match *b {
        0 => "<C-Space>".into(),
        8 => "<C-h>".into(),
        b'\t' => "<Tab>".into(),
        b'\r' | b'\n' => "<CR>".into(),
        0x1c => "<C-Bslash>".into(),
        0x1d => "<C-]>".into(),
        0x1e => "<C-^>".into(),
        0x1f => "<C-_>".into(),
        0x7f => "<BS>".into(),
        b'<' => "<lt>".into(),
        c @ 1..=0x1a => format!("<C-{}>", (c + b'a' - 1) as char),
        c if c < 0x80 => (c as char).to_string(),
        _ => return None,
    })
}

/// The `S-A-C-D-` prefix for a CSI modifier parameter.
fn mod_prefix(param: u32) -> String {
    let bits = param.saturating_sub(1);
    let mut s = String::new();
    for (bit, name) in [(4, "C-"), (2, "A-"), (1, "S-"), (8, "D-")] {
        if bits & bit != 0 {
            s.push_str(name);
        }
    }
    s
}

fn named(name: &str, mods: u32) -> String {
    format!("<{}{name}>", mod_prefix(mods))
}

/// `mods[:event]`. None for a key release.
fn mods_of(field: Option<&str>) -> Option<u32> {
    let Some(field) = field else { return Some(1) };
    let mut p = field.split(':');
    let mods = p.next().and_then(|m| m.parse().ok()).unwrap_or(1);
    (p.next() != Some("3")).then_some(mods)
}

/// `CSI code[:alts] ; mods[:event] u`.
fn kitty(params: &str) -> Option<String> {
    let mut f = params.split(';');
    let code: u32 = f.next()?.split(':').next()?.parse().ok()?;
    let mods = mods_of(f.next())?;
    let name = match code {
        9 => "Tab".to_owned(),
        13 => "CR".to_owned(),
        27 => "Esc".to_owned(),
        32 => "Space".to_owned(),
        92 => "Bslash".to_owned(),
        124 => "Bar".to_owned(),
        60 => "lt".to_owned(),
        127 => "BS".to_owned(),
        // the private-use keys: keypad and bare modifiers
        57344..=63743 => return None,
        c => char::from_u32(c)?.to_string(),
    };
    Some(named(&name, mods))
}

fn tilde(params: &str) -> Option<String> {
    let mut f = params.split(';');
    let code: u32 = f.next()?.parse().ok()?;
    let mods = mods_of(f.next())?;
    let name = match code {
        1 | 7 => "Home",
        2 => "Insert",
        3 => "Del",
        4 | 8 => "End",
        5 => "PageUp",
        6 => "PageDown",
        11 => "F1",
        12 => "F2",
        13 => "F3",
        14 => "F4",
        15 => "F5",
        17 => "F6",
        18 => "F7",
        19 => "F8",
        20 => "F9",
        21 => "F10",
        23 => "F11",
        24 => "F12",
        _ => return None,
    };
    Some(named(name, mods))
}

const fn letter_key(f: u8) -> Option<&'static str> {
    Some(match f {
        b'A' => "Up",
        b'B' => "Down",
        b'C' => "Right",
        b'D' => "Left",
        b'H' => "Home",
        b'F' => "End",
        b'P' => "F1",
        b'Q' => "F2",
        b'R' => "F3",
        b'S' => "F4",
        _ => return None,
    })
}

/// `CSI 1 ; mods X`, or a bare `CSI X`.
fn letter(fin: u8, params: &str) -> Option<String> {
    let name = letter_key(fin)?;
    let mods = mods_of(params.split(';').nth(1))?;
    Some(named(name, mods))
}

/// SGR mouse: `CSI < b ; x ; y M` or `m`.
fn mouse(params: &[u8], fin: u8) -> Option<Mouse> {
    let text = std::str::from_utf8(params).ok()?;
    let mut f = text.split(';').map(|n| n.parse::<i32>().ok());
    let (b, x, y) = (f.next()??, f.next()??, f.next()??);
    let mut mods = String::new();
    for (bit, name) in [(16, "C-"), (8, "A-"), (4, "S-")] {
        if b & bit != 0 {
            let _ = write!(mods, "{name}");
        }
    }
    let code = b & !(4 | 8 | 16 | 32);
    let moved = b & 32 != 0;
    let (button, action) = match code {
        64 => ("wheel", "up"),
        65 => ("wheel", "down"),
        66 => ("wheel", "left"),
        67 => ("wheel", "right"),
        0..=2 => (
            ["left", "middle", "right"][code as usize],
            match (moved, fin) {
                (true, _) => "drag",
                (_, b'm') => "release",
                _ => "press",
            },
        ),
        _ => return None,
    };
    Some(Mouse {
        button,
        action,
        mods,
        row: y - 1,
        col: x - 1,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(bytes: &[u8]) -> Vec<String> {
        Parser::default()
            .feed(bytes)
            .into_iter()
            .filter_map(|e| match e {
                Event::Key(k) => Some(k),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn legacy_keys() {
        assert_eq!(
            keys(b"a<\r\x7f\x17"),
            ["a", "<lt>", "<CR>", "<BS>", "<C-w>"]
        );
        assert_eq!(keys("界".as_bytes()), ["界"]);
        assert_eq!(keys(b"\x1b"), ["<Esc>"]);
        assert_eq!(keys(b"\x1bx"), ["<A-x>"]);
        assert_eq!(keys(b"\x1b\x17"), ["<A-C-w>"]);
    }

    #[test]
    fn csi_keys() {
        assert_eq!(keys(b"\x1b[A\x1bOB"), ["<Up>", "<Down>"]);
        assert_eq!(keys(b"\x1b[1;5C"), ["<C-Right>"]);
        assert_eq!(keys(b"\x1b[3~\x1b[5;2~"), ["<Del>", "<S-PageUp>"]);
        assert_eq!(keys(b"\x1b[Z"), ["<S-Tab>"]);
    }

    #[test]
    fn kitty_keys() {
        assert_eq!(keys(b"\x1b[119;5u"), ["<C-w>"]);
        assert_eq!(keys(b"\x1b[13;2u"), ["<S-CR>"]);
        assert_eq!(keys(b"\x1b[27u"), ["<Esc>"]);
        assert_eq!(keys(b"\x1b[60;5u"), ["<C-lt>"]);
        // a release, and a bare modifier
        assert!(keys(b"\x1b[119;5:3u\x1b[57441;2u").is_empty());
    }

    #[test]
    fn sequences_cut_by_a_read_finish_on_the_next() {
        let mut p = Parser::default();
        assert!(p.feed(b"\x1b[1;").is_empty());
        assert_eq!(p.feed(b"5C"), [Event::Key("<C-Right>".into())]);
        assert!(p.feed(&"界".as_bytes()[..1]).is_empty());
        assert_eq!(p.feed(&"界".as_bytes()[1..]), [Event::Key("界".into())]);
    }

    #[test]
    fn pastes_stream_in_phases() {
        let mut p = Parser::default();
        assert_eq!(
            p.feed(b"\x1b[200~one\rtwo\x1b[20"),
            [Event::Paste("one\ntwo".into(), 1)]
        );
        assert_eq!(
            p.feed(b"1~x"),
            [Event::Paste(String::new(), 3), Event::Key("x".into())]
        );
    }

    #[test]
    fn a_paste_marker_lookalike_is_text() {
        let mut p = Parser::default();
        p.feed(b"\x1b[200~a\x1b[20");
        assert_eq!(p.feed(b"Xb\x1b[201~").len(), 2);
    }

    #[test]
    fn mouse_events() {
        let parse = |b: &[u8]| Parser::default().feed(b);
        let press = Mouse {
            button: "left",
            action: "press",
            mods: "C-".into(),
            row: 4,
            col: 2,
        };
        assert_eq!(parse(b"\x1b[<16;3;5M"), [Event::Mouse(press)]);
        let [Event::Mouse(rel)] = &parse(b"\x1b[<0;1;1m")[..] else {
            panic!()
        };
        assert_eq!(rel.action, "release");
        let [Event::Mouse(drag)] = &parse(b"\x1b[<32;1;1M")[..] else {
            panic!()
        };
        assert_eq!(drag.action, "drag");
        let [Event::Mouse(wheel)] = &parse(b"\x1b[<65;1;1M")[..] else {
            panic!()
        };
        assert_eq!((wheel.button, wheel.action), ("wheel", "down"));
        // bare motion has nothing to say
        assert!(parse(b"\x1b[<35;1;1M").is_empty());
    }

    #[test]
    fn focus_reports_are_not_keys() {
        assert!(keys(b"\x1b[I\x1b[O").is_empty());
    }
}
