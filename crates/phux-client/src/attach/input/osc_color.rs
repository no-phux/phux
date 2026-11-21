//! The host terminal's default colours: OSC 10/11 answers (`10;rgb:...`),
//! parsed for the attach probe and kept by the input parser when the host
//! answers a re-query mid-attach, plus its DEC 2031 colour-scheme reports.

use phux_protocol::caps::{TerminalColor, TerminalDefaultColors};

/// The longest OSC 10/11 answer kept (`11;rgb:ffff/ffff/ffff` is 21 bytes).
const MAX_BODY: usize = 64;

/// Record an OSC 10 (foreground) or 11 (background) answer's colour.
pub fn note_osc_color(
    payload: &[u8],
    foreground: &mut Option<TerminalColor>,
    background: &mut Option<TerminalColor>,
) {
    let Some(at) = payload.iter().position(|&b| b == b';') else {
        return;
    };
    let Some(color) = parse_color(&payload[at + 1..]) else {
        return;
    };
    match &payload[..at] {
        b"10" => *foreground = Some(color),
        b"11" => *background = Some(color),
        _ => {}
    }
}

fn parse_color(value: &[u8]) -> Option<TerminalColor> {
    let value = std::str::from_utf8(value).ok()?;
    if let Some(rgb) = value.strip_prefix("rgb:") {
        let mut components = rgb.split('/');
        let r = normalize_component(components.next()?)?;
        let g = normalize_component(components.next()?)?;
        let b = normalize_component(components.next()?)?;
        return components
            .next()
            .is_none()
            .then_some(TerminalColor { r, g, b });
    }
    let hex = value.strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    Some(TerminalColor {
        r: u8::from_str_radix(&hex[0..2], 16).ok()?,
        g: u8::from_str_radix(&hex[2..4], 16).ok()?,
        b: u8::from_str_radix(&hex[4..6], 16).ok()?,
    })
}

fn normalize_component(component: &str) -> Option<u8> {
    if component.is_empty() || component.len() > 4 {
        return None;
    }
    let value = u16::from_str_radix(component, 16).ok()?;
    let max = (1_u32 << (component.len() * 4)) - 1;
    u8::try_from((u32::from(value) * 255 + max / 2) / max).ok()
}

/// Keeps OSC 10/11 answers until both colours are in, and whether the host
/// reported a colour-scheme change (`CSI ? 997 ; Ps n`).
#[derive(Debug, Default)]
pub(super) struct ColorCapture {
    active: bool,
    body: Vec<u8>,
    foreground: Option<TerminalColor>,
    background: Option<TerminalColor>,
    scheme_changed: bool,
}

impl ColorCapture {
    pub(super) const fn new() -> Self {
        Self {
            active: false,
            body: Vec::new(),
            foreground: None,
            background: None,
            scheme_changed: false,
        }
    }

    /// A control string starts; only an OSC may be an answer.
    pub(super) fn begin(&mut self, osc: bool) {
        self.active = osc;
        self.body.clear();
    }

    pub(super) fn push(&mut self, b: u8) {
        if !self.active {
            return;
        }
        let ok = match self.body.len() {
            0 => b == b'1',
            1 => b == b'0' || b == b'1',
            2 => b == b';',
            n => n < MAX_BODY,
        };
        if ok {
            self.body.push(b);
        } else {
            self.active = false;
        }
    }

    pub(super) fn finish(&mut self) {
        if std::mem::take(&mut self.active) {
            note_osc_color(&self.body, &mut self.foreground, &mut self.background);
        }
        self.body.clear();
    }

    /// A `CSI ? 997 ; Ps n` report: the host's light/dark scheme changed.
    pub(super) const fn note_scheme_change(&mut self) {
        self.scheme_changed = true;
    }

    pub(super) const fn take_scheme_change(&mut self) -> bool {
        std::mem::replace(&mut self.scheme_changed, false)
    }

    /// Both colours, once both answers are in.
    pub(super) const fn take_colors(&mut self) -> Option<TerminalDefaultColors> {
        let (Some(foreground), Some(background)) = (self.foreground, self.background) else {
            return None;
        };
        self.foreground = None;
        self.background = None;
        Some(TerminalDefaultColors {
            foreground,
            background,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::StdinParser;
    use super::*;

    /// Both answers, split across reads and with either terminator, come
    /// back once as one palette; keys around them still arrive.
    #[test]
    fn a_split_pair_of_answers_is_one_palette() {
        let mut parser = StdinParser::new();
        let mut events = parser.feed(b"a\x1b]10;rgb:ffff/8080/0000\x1b\\\x1b]1");
        assert_eq!(parser.take_default_colors(), None);
        events.extend(parser.feed(b"1;#102030\x07b"));
        assert_eq!(events.len(), 2);
        let colors = parser.take_default_colors().expect("palette");
        assert_eq!(
            colors.foreground,
            TerminalColor {
                r: 255,
                g: 128,
                b: 0
            }
        );
        assert_eq!(
            colors.background,
            TerminalColor {
                r: 16,
                g: 32,
                b: 48
            }
        );
        assert_eq!(parser.take_default_colors(), None);
    }

    /// A DEC 2031 report is noted, not typed; other OSC strings are not
    /// colours.
    #[test]
    fn a_scheme_report_is_noted_and_produces_no_key() {
        let mut parser = StdinParser::new();
        assert!(parser.feed(b"\x1b[?997;1n\x1b]12;rgb:0/0/0\x07").is_empty());
        assert!(parser.take_color_scheme_change());
        assert!(!parser.take_color_scheme_change());
        assert_eq!(parser.take_default_colors(), None);
    }
}
