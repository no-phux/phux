//! Handoff: the pane in front, offered to this person's phone as an
//! `NSUserActivity`. The phone app opens the same pane on the host it has
//! already paired with; nothing here can pair one. The payload names a host
//! by what a paired phone already holds (its certificate pin, else its name)
//! and a pane by the id every client binding uses. It carries no credential.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, PoisonError};

use phux_protocol::ids::ResourceId;
use serde_json::Value;

/// The activity type both apps declare under `NSUserActivityTypes`
/// (`packaging/Info.plist` here, `ios/Project.swift` in phux-mobile).
pub const ACTIVITY: &str = "sh.phux.pane";

/// What the activity's `userInfo` carries, every value a string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Payload {
    /// `host`: the machine's name here, a registry name or this computer's
    /// short host name.
    pub host: String,
    /// `fp`: its server certificate's SHA-256 pin, 64 lowercase hex digits.
    pub fp: Option<String>,
    /// `pane`: the resource, spelled as the client bindings spell it.
    pub pane: String,
}

impl Payload {
    #[cfg(any(target_os = "macos", test))]
    pub fn entries(&self) -> impl Iterator<Item = (&'static str, &str)> {
        let fp = self.fp.as_deref().map(|fp| ("fp", fp));
        [("host", self.host.as_str()), ("pane", self.pane.as_str())]
            .into_iter()
            .chain(fp)
    }
}

/// `phux-client-ffi`'s `projection::id::encode`, which the phone's pane ids
/// come from; that crate is not in this graph, and the test pins the shape.
fn pane(id: &ResourceId) -> String {
    match id {
        ResourceId::Local { id } => format!("local:{id}"),
        ResourceId::Satellite { host, id } => format!("satellite:{}:{id}", host.as_str()),
    }
}

/// A pin in one spelling: `phux pair` prints `AB:CD`, the registry keeps
/// bare hex. None unless it is a whole SHA-256.
fn pin(value: &Value) -> Option<String> {
    let hex: String = value
        .as_str()?
        .chars()
        .filter(char::is_ascii_hexdigit)
        .map(|c| c.to_ascii_lowercase())
        .collect();
    (hex.len() == 64).then_some(hex)
}

/// What `phux host ls --json` last said: this machine's name and pin, and
/// each registered machine's pin.
#[derive(Default)]
struct Known {
    local: Option<String>,
    local_pin: Option<String>,
    pins: HashMap<String, String>,
}

// ponytail: process-wide like `host_color`'s table, and fed by the same
// listing; per-window state if windows ever read different registries.
static KNOWN: LazyLock<Mutex<Known>> = LazyLock::new(Mutex::default);

/// Take in a `phux host ls --json` document. An older phux reports no pin
/// for this machine, and the phone falls back to its name.
pub fn learn(doc: &Value) {
    let hosts = doc["hosts"].as_array().into_iter().flatten();
    let pins = hosts
        .filter_map(|host| {
            Some((
                host["name"].as_str()?.to_owned(),
                pin(&host["cert_fingerprint"])?,
            ))
        })
        .collect();
    *KNOWN.lock().unwrap_or_else(PoisonError::into_inner) = Known {
        local: doc["local"]["name"].as_str().map(str::to_owned),
        local_pin: pin(&doc["local"]["cert_fingerprint"]),
        pins,
    };
}

/// The payload for `resource` on `machine`; `None` is this computer.
pub fn payload(machine: Option<&str>, resource: &ResourceId) -> Payload {
    let known = KNOWN.lock().unwrap_or_else(PoisonError::into_inner);
    let (host, fp) = match machine {
        Some(name) => (name.to_owned(), known.pins.get(name).cloned()),
        None => (
            known
                .local
                .clone()
                .unwrap_or_else(phux_config_core::instance::local_host_name),
            known.local_pin.clone(),
        ),
    };
    Payload {
        host,
        fp,
        pane: pane(resource),
    }
}

/// Offer `wanted` to the person's other devices, or nothing. Call it freely:
/// only a change reaches the system.
pub fn offer(wanted: Option<Payload>) {
    static OFFERED: Mutex<Option<Payload>> = Mutex::new(None);
    let mut offered = OFFERED.lock().unwrap_or_else(PoisonError::into_inner);
    if *offered == wanted {
        return;
    }
    if cfg!(debug_assertions) && !cfg!(test) {
        eprintln!("phux-rune: handoff {ACTIVITY} {wanted:?}");
    }
    #[cfg(all(target_os = "macos", not(test)))]
    set_current(wanted.as_ref());
    *offered = wanted;
}

/// Replace the current activity. The system advertises it only from a signed
/// bundle whose `Info.plist` declares [`ACTIVITY`]; elsewhere this is inert.
#[cfg(all(target_os = "macos", not(test)))]
#[allow(unsafe_code)]
fn set_current(payload: Option<&Payload>) {
    use std::cell::RefCell;

    use objc2::rc::{Allocated, Retained};
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    use objc2_foundation::NSString;

    thread_local! {
        static CURRENT: RefCell<Option<Retained<AnyObject>>> = const { RefCell::new(None) };
    }
    // Safety: plain Foundation calls with objects of the classes each
    // selector takes, on the main thread (Rune's loop, from `poll` and the
    // focus callback), which is also the only thread that holds `CURRENT`.
    let next = payload.map(|payload| unsafe {
        let activity: Allocated<AnyObject> = msg_send![class!(NSUserActivity), alloc];
        let activity: Retained<AnyObject> =
            msg_send![activity, initWithActivityType: &*NSString::from_str(ACTIVITY)];
        let info: Retained<AnyObject> = msg_send![class!(NSMutableDictionary), new];
        for (key, value) in payload.entries() {
            let _: () = msg_send![
                &info,
                setObject: &*NSString::from_str(value),
                forKey: &*NSString::from_str(key)
            ];
        }
        let _: () = msg_send![&activity, setUserInfo: &*info];
        let _: () = msg_send![&activity, setTitle: &*NSString::from_str(&payload.host)];
        let _: () = msg_send![&activity, becomeCurrent];
        activity
    });
    if let Some(old) = CURRENT.replace(next) {
        // Safety: as above; an invalidated activity is never used again.
        unsafe {
            let _: () = msg_send![&old, invalidate];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The same payload phux-mobile's `HandoffRouteTests` reads: change one
    /// and the other must follow.
    #[test]
    fn payload_matches_the_phone_fixture() {
        learn(&serde_json::json!({
            "hosts": [
                {"name": "powerhouse-1", "cert_fingerprint": "cd".repeat(32)},
                {"name": "ssh-only", "cert_fingerprint": null},
            ],
            "local": {"name": "mini", "cert_fingerprint": "AB:".repeat(32).trim_end_matches(':')},
        }));
        let local = payload(None, &ResourceId::local(7));
        assert_eq!(
            local.entries().collect::<Vec<_>>(),
            [
                ("host", "mini"),
                ("pane", "local:7"),
                ("fp", "ab".repeat(32).as_str())
            ]
        );
        let remote = payload(Some("powerhouse-1"), &ResourceId::satellite("box", 3));
        assert_eq!(remote.host, "powerhouse-1");
        assert_eq!(remote.fp, Some("cd".repeat(32)));
        assert_eq!(remote.pane, "satellite:box:3");
        // No pin known: the name alone, never a guess.
        assert_eq!(payload(Some("ssh-only"), &ResourceId::local(1)).fp, None);
        assert_eq!(pin(&serde_json::json!("abcd")), None);
    }
}
