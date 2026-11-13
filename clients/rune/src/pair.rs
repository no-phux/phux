//! Pairing a phone: what `phux pair --json --enroll` and `phux pair ls --json`
//! say, in the words the Pair a phone screen shows. The state lives in
//! `desk::settings::pair`, the view in `ui::pair`.

use std::net::IpAddr;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

/// A code a phone can scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Code {
    /// The one-tap link, which is also the QR's payload. A secret.
    pub link: String,
    /// When it stops pairing a new phone, in Unix seconds; `None` when the
    /// CLI names no expiry.
    pub expires_at: Option<i64>,
    /// One phone at most: a single-use link, or a single-use enrollment
    /// ticket in it.
    pub once: bool,
    /// The credential the link carries, which `pair ls` lists.
    pub credential: String,
    pub network: Network,
}

/// How the phone reaches this computer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Network {
    Tailnet,
    Lan,
    Relay,
    /// Any other address, by name.
    Other(String),
}

impl Network {
    pub fn words(&self) -> String {
        match self {
            Self::Tailnet => "Over your tailnet. Turn on Tailscale on the phone, signed in to the \
                              same tailnet. Nothing is opened to your local network or the internet."
                .to_owned(),
            Self::Lan => "Over this local network. The phone must be on the same Wi-Fi. Nothing \
                          is opened to the internet."
                .to_owned(),
            Self::Relay => "Through the relay, from any network. The relay carries encrypted \
                            traffic it can't read."
                .to_owned(),
            Self::Other(host) => {
                format!("At {host}. The phone must be able to reach that address.")
            }
        }
    }
}

/// A credential `phux pair ls` lists as able to connect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// The credential id `phux pair revoke` takes.
    pub id: String,
    /// When it last connected, in Unix seconds.
    pub last_seen: Option<i64>,
}

impl Device {
    /// A short name: the store keeps no device names.
    pub fn name(&self) -> String {
        format!("Device {}", &self.id[..self.id.len().min(8)])
    }
}

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

fn json(bytes: &[u8]) -> Result<Value, String> {
    serde_json::from_slice(bytes).map_err(|error| format!("phux printed no JSON: {error}"))
}

/// A `phux pair --json --enroll` mint. The code expires with the soonest of
/// the link, the credential and its enrollment ticket: past any, a new phone
/// can't pair. A CLI that names none of them shows no countdown.
pub fn parse_code(bytes: &[u8]) -> Result<Code, String> {
    let doc = json(bytes)?;
    let link = doc["connect_link"]
        .as_str()
        .filter(|link| !link.is_empty())
        .ok_or("phux made no link a phone can open: the server has no address to give it")?
        .to_owned();
    let expires_at = [
        &doc["link_expires_at"],
        &doc["expires_at"],
        &doc["enrollment_expires_at"],
    ]
    .into_iter()
    .filter_map(instant)
    .min();
    let network = if doc["relay"].is_object() {
        Network::Relay
    } else {
        network(&link)
    };
    Ok(Code {
        link,
        expires_at,
        once: doc["single_use"] == true || doc["enrollment_ticket_id"].is_string(),
        credential: doc["credential_id"].as_str().unwrap_or_default().to_owned(),
        network,
    })
}

/// Unix seconds, from either an RFC 3339 string or a number.
fn instant(value: &Value) -> Option<i64> {
    value.as_i64().or_else(|| value.as_str().and_then(rfc3339))
}

/// `2026-11-13T02:53:20Z`, with optional fraction and offset, as Unix
/// seconds.
fn rfc3339(text: &str) -> Option<i64> {
    let (date, time) = text.split_once(['T', 't', ' '])?;
    let mut ymd = date.splitn(3, '-').map(|p| p.parse::<i64>().ok());
    let (y, m, d) = (ymd.next()??, ymd.next()??, ymd.next()??);
    let (clock, offset) = time.split_at(time.find(['Z', 'z', '+', '-'])?);
    let mut hms = clock.splitn(3, ':');
    let h: i64 = hms.next()?.parse().ok()?;
    let min: i64 = hms.next()?.parse().ok()?;
    let s: i64 = hms.next()?.split('.').next()?.parse().ok()?;
    let offset = match offset.as_bytes().first() {
        Some(b'Z' | b'z') => 0,
        Some(sign) => {
            let (oh, om) = offset[1..].split_once(':')?;
            let minutes = oh.parse::<i64>().ok()? * 60 + om.parse::<i64>().ok()?;
            if *sign == b'-' { -minutes } else { minutes }
        }
        None => return None,
    };
    // Days from the civil date (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + h * 3600 + min * 60 + s - offset * 60)
}

/// Which network the link's server address is on.
fn network(link: &str) -> Network {
    let host = url::Url::parse(link)
        .ok()
        .and_then(|link| {
            link.query_pairs()
                .find(|(key, _)| key == "url")
                .map(|(_, server)| server.into_owned())
        })
        .and_then(|server| url::Url::parse(&server).ok())
        .and_then(|server| server.host_str().map(str::to_owned));
    let Some(host) = host else {
        return Network::Other("the address in the link".to_owned());
    };
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    match bare.parse::<IpAddr>() {
        Ok(ip) if tailnet(ip) => Network::Tailnet,
        Ok(IpAddr::V4(ip)) if ip.is_private() || ip.is_link_local() => Network::Lan,
        Ok(IpAddr::V6(ip)) if (ip.segments()[0] & 0xfe00) == 0xfc00 => Network::Lan,
        Ok(IpAddr::V6(ip)) if (ip.segments()[0] & 0xffc0) == 0xfe80 => Network::Lan,
        Ok(_) => Network::Other(host),
        Err(_) if bare.ends_with(".ts.net") => Network::Tailnet,
        Err(_) if bare.ends_with(".local") => Network::Lan,
        Err(_) => Network::Other(host),
    }
}

/// Tailscale's ranges: 100.64.0.0/10 and fd7a:115c:a1e0::/48.
fn tailnet(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.octets()[0] == 100 && (ip.octets()[1] & 0xc0) == 64,
        IpAddr::V6(ip) => ip.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0],
    }
}

/// The devices `phux pair ls --json` says can connect. A CLI that reports
/// `status` marks a paired phone `active` with no `link_expires_at`; an
/// older one says only whether a credential is revoked.
pub fn parse_devices(bytes: &[u8]) -> Result<Vec<Device>, String> {
    let doc = json(bytes)?;
    let rows = doc["credentials"]
        .as_array()
        .ok_or("phux listed no credentials")?;
    Ok(rows
        .iter()
        .filter(|row| match row["status"].as_str() {
            Some(status) => status == "active" && row["link_expires_at"].is_null(),
            None => row["revoked"] == false,
        })
        .filter_map(|row| {
            Some(Device {
                id: row["id"].as_str()?.to_owned(),
                last_seen: instant(&row["last_seen"]),
            })
        })
        .collect())
}

/// The CLI refused because remote access is off and only an explicit
/// `--enable` turns it on.
pub fn needs_enable(error: &str) -> bool {
    error.contains("[policy] mode is unset")
}

/// The CLI's own words, without the command line and prefixes around them.
pub fn reason(error: &str) -> String {
    let words = error.split_once("failed: ").map_or(error, |(_, rest)| rest);
    let words = words.strip_prefix("phux: error: ").unwrap_or(words);
    words
        .strip_prefix("pair: ")
        .unwrap_or(words)
        .trim()
        .to_owned()
}

/// `9:05` left, `1:02:03` past an hour; `None` once it has run out.
pub fn countdown(left: i64) -> Option<String> {
    (left > 0).then(|| {
        let (h, m, s) = (left / 3600, left / 60 % 60, left % 60);
        if h > 0 {
            format!("{h}:{m:02}:{s:02}")
        } else {
            format!("{m}:{s:02}")
        }
    })
}

/// When a device last connected, from `now`.
pub fn last_seen(at: Option<i64>, now: i64) -> String {
    match at.map(|at| (now - at).max(0)) {
        None => "Not connected yet".to_owned(),
        Some(ago) if ago < 3600 => "Connected in the last hour".to_owned(),
        Some(ago) if ago < 86_400 => "Last connected today".to_owned(),
        Some(ago) if ago < 2 * 86_400 => "Last connected yesterday".to_owned(),
        Some(ago) => format!("Last connected {} days ago", ago / 86_400),
    }
}

/// The QR, dark on white whatever the theme, as straight RGBA: side length
/// and pixels. At least `min_side` pixels square, with the standard
/// four-module quiet zone.
pub fn qr_rgba(payload: &str, min_side: u32) -> Option<(u32, Vec<u8>)> {
    let code = qrcode::QrCode::new(payload.as_bytes()).ok()?;
    let width = u32::try_from(code.width()).ok()?;
    let modules = width + 8;
    let scale = min_side.div_ceil(modules).max(1);
    let side = modules * scale;
    let colors = code.to_colors();
    let mut rgba = vec![255; (side * side * 4) as usize];
    for y in 0..side {
        for x in 0..side {
            let (mx, my) = ((x / scale).wrapping_sub(4), (y / scale).wrapping_sub(4));
            if mx < width && my < width && colors[(my * width + mx) as usize] == qrcode::Color::Dark
            {
                let at = ((y * side + x) * 4) as usize;
                rgba[at..at + 3].fill(0);
            }
        }
    }
    Some((side, rgba))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINT: &str = r#"{
      "schema_version": 1, "token": "t",
      "overlay_addresses": ["100.113.38.51"],
      "ws_addr": "100.113.38.51:8787", "quic_addr": "0.0.0.0:8788",
      "connect_link": "https://phux.sh/connect?url=wss://mini.tail01774c.ts.net:8787&quic=quic://mini.tail01774c.ts.net:8788&name=mini#enroll=e&token=t",
      "credential_id": "00f5", "generation": 1,
      "expires_at": "2026-11-13T02:53:20Z",
      "enrollment_ticket_id": "t-42", "enrollment_expires_at": 1794538100
    }"#;

    #[test]
    fn a_mint_gives_its_link_soonest_expiry_and_network() {
        let code = parse_code(MINT.as_bytes()).unwrap();
        assert!(
            code.link
                .starts_with("https://phux.sh/connect?url=wss://mini")
        );
        // The ticket (02:48:20Z) runs out before the credential (02:53:20Z).
        assert_eq!(code.expires_at, Some(1_794_538_100));
        assert_eq!(code.network, Network::Tailnet);
        assert!(code.once, "the enrollment ticket is single use");
        assert_eq!(code.credential, "00f5");
        let next = MINT.replace(
            r#""credential_id""#,
            r#""single_use": true, "link_expires_at": "2026-11-13T02:40:00Z", "credential_id""#,
        );
        let code = parse_code(next.as_bytes()).unwrap();
        assert_eq!(
            code.expires_at,
            Some(1_794_537_600),
            "the link runs out first"
        );
    }

    #[test]
    fn todays_mint_without_expiry_shows_no_countdown() {
        let doc = r#"{"connect_link":"https://phux.sh/connect?url=wss://192.168.1.4:8787&token=t","expires_at":null}"#;
        let code = parse_code(doc.as_bytes()).unwrap();
        assert_eq!(code.expires_at, None);
        assert!(!code.once);
        assert_eq!(code.network, Network::Lan);
    }

    #[test]
    fn a_mint_without_a_link_says_why() {
        let error = parse_code(br#"{"connect_link":null}"#).unwrap_err();
        assert!(error.contains("no link"));
    }

    #[test]
    fn rfc3339_reads_offsets_and_fractions() {
        assert_eq!(rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(rfc3339("2026-11-13T02:53:20Z"), Some(1_794_538_400));
        assert_eq!(
            rfc3339("2026-11-13T03:53:20.123+01:00"),
            Some(1_794_538_400)
        );
        assert_eq!(rfc3339("2000-02-29T00:00:00-00:30"), Some(951_784_200));
        assert_eq!(rfc3339("not a date"), None);
    }

    #[test]
    fn the_network_follows_the_link_address() {
        let at = |host: &str| network(&format!("https://phux.sh/connect?url=wss://{host}:8787"));
        assert_eq!(at("100.101.1.2"), Network::Tailnet);
        assert_eq!(at("[fd7a:115c:a1e0::1]"), Network::Tailnet);
        assert_eq!(at("box.tail1.ts.net"), Network::Tailnet);
        assert_eq!(at("10.0.0.2"), Network::Lan);
        assert_eq!(at("studio.local"), Network::Lan);
        assert_eq!(at("[fe80::1]"), Network::Lan);
        assert_eq!(at("100.200.0.1"), Network::Other("100.200.0.1".into()));
        assert_eq!(at("example.com"), Network::Other("example.com".into()));
        let relay = r#"{"connect_link":"phux://connect?url=wss://r:1","relay":{"route":"x"}}"#;
        assert_eq!(
            parse_code(relay.as_bytes()).unwrap().network,
            Network::Relay
        );
    }

    #[test]
    fn devices_are_paired_phones_or_unrevoked_credentials_on_an_older_cli() {
        let next = r#"{"credentials":[
          {"id":"84b11329e12c","status":"active","link_expires_at":null,"last_seen":"2026-11-12T00:00:00Z"},
          {"id":"aaaa","status":"unused","link_expires_at":"2026-11-13T03:00:00Z","last_seen":null},
          {"id":"bbbb","status":"expired","link_expires_at":null},
          {"id":"cccc","status":"revoked","link_expires_at":null}
        ]}"#;
        let devices = parse_devices(next.as_bytes()).unwrap();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].name(), "Device 84b11329");
        assert_eq!(devices[0].last_seen, Some(1_794_441_600));
        let today = r#"{"credentials":[
          {"id":"1f3b","revoked":false,"last_seen":null},
          {"id":"a410","revoked":true,"last_seen":"2026-10-07T21:09:31.062219+00:00"}
        ]}"#;
        let devices = parse_devices(today.as_bytes()).unwrap();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].last_seen, None);
    }

    #[test]
    fn errors_read_as_the_cli_wrote_them() {
        let error = "`phux --socket /s pair --json --enroll` failed: phux: error: pair: the server \
                     at /s has no remote listener bound because [policy] mode is unset, so none was minted";
        assert!(needs_enable(error));
        assert!(reason(error).starts_with("the server at /s"));
        assert_eq!(reason("phux is not installed"), "phux is not installed");
    }

    #[test]
    fn countdown_and_access_words() {
        assert_eq!(countdown(545).as_deref(), Some("9:05"));
        assert_eq!(countdown(3723).as_deref(), Some("1:02:03"));
        assert_eq!(countdown(0), None);
        assert_eq!(last_seen(None, 0), "Not connected yet");
        assert_eq!(last_seen(Some(0), 600), "Connected in the last hour");
        assert_eq!(
            last_seen(Some(0), 86_400 * 3 + 5),
            "Last connected 3 days ago"
        );
    }

    #[test]
    fn the_qr_has_a_quiet_zone_and_dark_finders() {
        let (side, rgba) = qr_rgba("https://phux.sh/connect?url=wss://h:1", 200).unwrap();
        assert!(side >= 200);
        let pixel = |x: u32, y: u32| rgba[((y * side + x) * 4) as usize];
        assert_eq!(pixel(0, 0), 255, "quiet zone is white");
        let scale = side
            / (qrcode::QrCode::new(b"https://phux.sh/connect?url=wss://h:1")
                .unwrap()
                .width() as u32
                + 8);
        assert_eq!(pixel(4 * scale, 4 * scale), 0, "finder corner is dark");
    }
}
