//! The desktop's light or dark preference on Linux, read from the XDG
//! desktop portal (`org.freedesktop.appearance` `color-scheme`). Rune reads
//! it too, but takes 0, "no preference", as dark. GNOME's default style
//! answers 0 and GNOME's own apps draw light there, so Phux does too: dark
//! only on 1. A worker reads it once, follows `SettingChanged`, and wakes the
//! window on a change.

use std::sync::atomic::{AtomicU8, Ordering};

use rune::Waker;

/// No answer from the portal yet, or none at all (not Linux, no portal).
const UNKNOWN: u8 = u8::MAX;

/// The portal's last `color-scheme`: 0 no preference, 1 dark, 2 light.
static SCHEME: AtomicU8 = AtomicU8::new(UNKNOWN);

/// The system appearance: the portal's answer once it has one, else
/// `fallback`, Rune's own (`cx.system_dark()`).
pub fn system_dark(fallback: bool) -> bool {
    scheme_dark(SCHEME.load(Ordering::Relaxed)).unwrap_or(fallback)
}

/// Dark only on 1. The portal spec reads any other value as no preference.
fn scheme_dark(scheme: u8) -> Option<bool> {
    match scheme {
        UNKNOWN => None,
        1 => Some(true),
        _ => Some(false),
    }
}

/// Follow the portal for as long as Rune runs. Nothing runs off Linux.
#[cfg(target_os = "linux")]
pub fn watch(waker: Waker) {
    let _ = std::thread::Builder::new()
        .name("phux-appearance".into())
        .spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            runtime.block_on(follow(&waker));
        });
}

#[cfg(not(target_os = "linux"))]
pub fn watch(_waker: Waker) {}

#[cfg(target_os = "linux")]
async fn follow(waker: &Waker) -> Option<()> {
    use futures_util::StreamExt;
    use zbus::{Connection, OwnedVariant, Proxy};
    const NAMESPACE: &str = "org.freedesktop.appearance";
    const KEY: &str = "color-scheme";

    let conn = Connection::session().await.ok()?;
    let portal = Proxy::new(
        &conn,
        "org.freedesktop.portal.Desktop",
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.Settings",
    )
    .await
    .ok()?;
    // Subscribe first, so a change between the read and the subscription isn't lost.
    let mut changes = portal.receive_signal("SettingChanged").await.ok()?;
    let first = match portal
        .call::<_, _, OwnedVariant>("ReadOne", &(NAMESPACE, KEY))
        .await
    {
        Ok(value) => Some(value),
        Err(_) => portal.call("Read", &(NAMESPACE, KEY)).await.ok(),
    };
    if let Some(value) = first {
        store(&value, waker);
    }
    while let Some(message) = changes.next().await {
        if let Ok((namespace, key, value)) = message
            .body()
            .deserialize::<(String, String, OwnedVariant)>()
            && namespace == NAMESPACE
            && key == KEY
        {
            store(&value, waker);
        }
    }
    Some(())
}

/// `color-scheme` as a number. Portals answer `v`, and `Read` (before
/// `ReadOne`) wraps it twice.
#[cfg(target_os = "linux")]
fn store(value: &zbus::Variant<'_>, waker: &Waker) {
    fn scheme(value: &zbus::Variant<'_>) -> Option<u32> {
        match value {
            zbus::Variant::U32(x) => Some(*x),
            zbus::Variant::Variant(inner) => scheme(inner),
            _ => None,
        }
    }
    let Some(scheme) = scheme(value) else { return };
    let scheme = u8::try_from(scheme).unwrap_or(0).min(2);
    if SCHEME.swap(scheme, Ordering::Relaxed) != scheme {
        waker();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// GNOME's default style answers 0 and draws light; dark only on 1.
    #[test]
    fn no_preference_is_light_and_only_one_is_dark() {
        assert_eq!(scheme_dark(0), Some(false), "GNOME's default style");
        assert_eq!(scheme_dark(1), Some(true));
        assert_eq!(scheme_dark(2), Some(false));
        assert_eq!(
            scheme_dark(7),
            Some(false),
            "unknown reads as no preference"
        );
        assert_eq!(scheme_dark(UNKNOWN), None, "no answer keeps Rune's");
    }
}
