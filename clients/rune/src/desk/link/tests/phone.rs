//! The phone app's connection: its own words, the way back to pairing, and
//! where its banner and buttons land.

use std::time::{Duration, Instant};

use phux_client_runtime::control::Status;
use phux_protocol::ids::ResourceId;
use rune::accesskit::Action;

use super::super::standing::{UNPAIRED, plainly_on_phone, reach_hint};
use super::{Banner, Desk, QUIC_REFUSED, Standing, embedded, stale};
use crate::desk::{Pending, Shown};

const UNANSWERED: &str = "100.79.155.27:8788 did not answer within 15s; check that it is up and its network is connected";

/// The phone app, paired with a computer called `mini`.
fn phone() -> Desk {
    let configuration = serde_json::json!({"endpoint": "quic://localhost:8788", "name": "mini"});
    crate::mobile::state(serde_json::from_value(configuration).expect("a configuration"))
        .expect("a loopback host needs no pairing")
}

/// The phone app with a terminal on screen whose connection ended in `error`.
fn failed(error: &str) -> Desk {
    let mut desk = phone();
    desk.shown = Some(Shown {
        resource: ResourceId::local(1),
        view: phux_client_runtime::ViewId::from_raw(1),
    });
    desk.client = Some(embedded());
    desk.status = Status::Failed;
    desk.error = Some(error.to_owned());
    desk.link.down_since = Some(Instant::now() - Duration::from_secs(5));
    desk
}

fn drawn(desk: Desk, size: (f32, f32)) -> rune::Runtime<Desk> {
    let mut rt = rune::Runtime::new(desk, crate::ui::view, size, rune::Env::new(true, 2.0));
    rt.frame(0.0);
    rt
}

const PHONE: (f32, f32) = (393.0, 852.0);

#[test]
fn the_phone_reads_failures_without_transport_text_or_commands() {
    let hint = "Check that Tailscale is on and mini is awake.";
    assert_eq!(reach_hint("mini"), hint);
    for unreachable in [
        UNANSWERED,
        "mini did not answer (timed out); check that it is up",
        "connect: no answer within the initial budget",
        "liveness probe timed out",
        "mini: the connection was lost: closed",
    ] {
        assert_eq!(plainly_on_phone(unreachable, "mini"), hint, "{unreachable}");
    }
    for unresolved in [
        "mini: could not resolve mini.tail.ts.net: failed to lookup address information; is its \
         network connected?",
        "failed to lookup address information: nodename nor servname provided",
        "mini: mini.tail.ts.net resolved to no addresses",
    ] {
        assert_eq!(
            plainly_on_phone(unresolved, "mini"),
            "Can't find mini. Check that Tailscale is on.",
            "{unresolved}"
        );
    }
    for refusal in [QUIC_REFUSED, "websocket: 401", "pairing token refused"] {
        assert_eq!(plainly_on_phone(refusal, "mini"), UNPAIRED, "{refusal}");
    }
    // A pin's digits can spell a refusal; it is still the pin that changed.
    for changed in [
        "mini: server certificate fingerprint mismatch (pinned ab401c, got cd403e)",
        "mini: certificate authority changed. Refusing to connect: re-pair: `phux host add mini`",
    ] {
        let words = plainly_on_phone(changed, "mini");
        assert!(words.starts_with("mini isn't the computer"), "{words}");
    }
    assert!(plainly_on_phone("connect: Connection refused (os error 61)", "mini").contains("phux"));
    assert_eq!(
        plainly_on_phone("quic: frame 7 was odd", "mini"),
        "Connection problem."
    );
    assert_eq!(plainly_on_phone("", "mini"), "");
    let version = "server error VersionIncompatible: server speaks 12";
    assert_eq!(plainly_on_phone(version, "mini"), "server speaks 12");
    for said in [
        QUIC_REFUSED,
        UNANSWERED,
        "mini: re-pair it with `phux host add mini`",
    ] {
        let words = plainly_on_phone(said, "mini");
        assert!(!words.contains('`') && !words.contains("8788"), "{words}");
    }
}

#[test]
fn the_phone_says_what_to_check_once_a_dial_runs_long() {
    let mut desk = phone();
    let hint = reach_hint("mini");
    // No client at all is already a failure: say so from the start.
    assert_eq!(desk.standing(), Standing::Unreachable(hint.clone()));
    desk.client = Some(embedded());
    desk.status = Status::Connecting;
    assert_eq!(desk.standing(), Standing::Connecting(String::new()));
    desk.link.down_since = Some(Instant::now() - Duration::from_secs(2));
    assert_eq!(desk.standing(), Standing::Connecting(hint.clone()));
    desk.status = Status::Failed;
    desk.error = Some(UNANSWERED.to_owned());
    assert_eq!(desk.standing(), Standing::Unreachable(hint));
    // A desktop keeps the transport's words, and waits without any.
    let mut desktop = stale(embedded());
    assert_eq!(desktop.standing(), Standing::Connecting(String::new()));
    desktop.status = Status::Failed;
    desktop.error = Some(UNANSWERED.to_owned());
    assert_eq!(
        desktop.standing(),
        Standing::Unreachable(UNANSWERED.to_owned())
    );
}

#[test]
fn the_phones_banner_names_its_computer_and_keeps_the_servers_words() {
    let mut desk = failed(UNANSWERED);
    desk.status = Status::Connecting;
    desk.error = None;
    let keeps = "Your sessions keep running on mini.";
    assert_eq!(desk.banner(), Some(Banner::Reconnecting(keeps.to_owned())));
    desk.link.ended = Some("the server is shutting down".to_owned());
    let said = Banner::Reconnecting("the server is shutting down".to_owned());
    assert_eq!(desk.banner(), Some(said));
    // A desktop's is unchanged.
    let desktop = stale(embedded());
    let keeps = "Your sessions keep running on the server.";
    assert_eq!(
        desktop.banner(),
        Some(Banner::Reconnecting(keeps.to_owned()))
    );
}

#[test]
fn a_refused_phone_is_offered_pairing_from_the_banner() {
    let desk = failed(QUIC_REFUSED);
    assert!(desk.stuck() && desk.unpaired());
    let stuck = Banner::Stuck(Standing::Unreachable(UNPAIRED.to_owned()));
    assert_eq!(desk.banner(), Some(stuck));
    let mut rt = drawn(desk, PHONE);
    assert!(rt.find("banner-retry").is_some());
    assert!(rt.find("banner-update").is_none());
    assert!(rt.accessibility_action("banner-pair", Action::Click, None));
    rt.frame(0.0);
    let native = rt.state.native.as_ref().expect("the phone app");
    assert!(native.connection_requested, "pairing is the host's sheet");
    // An unanswered dial keeps retrying on its own: nothing to pair.
    let rt = drawn(failed(UNANSWERED), PHONE);
    assert!(rt.find("banner-pair").is_none());
    assert!(rt.find("banner-retry").is_some());
}

#[test]
fn a_desktops_stuck_banner_is_unchanged() {
    let mut desk = stale(embedded());
    desk.status = Status::Failed;
    desk.error = Some(QUIC_REFUSED.to_owned());
    let rt = drawn(desk, (1100.0, 720.0));
    assert!(rt.find("banner-pair").is_none());
    let (banner, _) = rt.presented("reconnect-banner").expect("the banner");
    assert!((banner[1] - 44.0).abs() < 0.5, "{banner:?}");
    let (retry, _) = rt.presented("banner-retry").expect("retry");
    assert!(retry[3] < 44.0, "{retry:?}");
}

#[test]
fn the_phones_banner_clears_its_top_bar_and_takes_a_thumb() {
    let mut rt = drawn(failed(QUIC_REFUSED), PHONE);
    let (bar, _) = rt.presented("phone-top").expect("the top bar");
    let (banner, _) = rt.presented("reconnect-banner").expect("the banner");
    assert!(banner[1] >= bar[1] + bar[3], "{banner:?} under {bar:?}");
    for key in ["banner-pair", "banner-retry"] {
        let (button, _) = rt.presented(key).expect(key);
        assert!(button[3] >= 44.0, "{key}: {button:?}");
    }
    // A held confirmation keeps the top of the stage; the banner goes under.
    rt.update(|desk, _| desk.pending = Some(Pending::Close(ResourceId::local(1))));
    for _ in 0..60 {
        rt.frame(1.0 / 60.0);
    }
    let (confirm, _) = rt.presented("confirm-bar").expect("the confirmation");
    let (banner, _) = rt.presented("reconnect-banner").expect("the banner");
    assert!(
        banner[1] >= confirm[1] + confirm[3],
        "{banner:?} under {confirm:?}"
    );
    for key in ["confirm-cancel", "confirm-ok"] {
        let (button, _) = rt.presented(key).expect(key);
        assert!(button[3] >= 44.0, "{key}: {button:?}");
    }
}

#[test]
fn the_phones_placeholder_offers_what_can_help() {
    let keys = |desk: Desk| {
        let rt = drawn(desk, PHONE);
        [
            "placeholder-pair",
            "placeholder-retry",
            "placeholder-machines",
        ]
        .map(|key| rt.presented(key).map(|(rect, _)| rect[3]))
    };
    let unshown = |error: &str| {
        let mut desk = failed(error);
        desk.shown = None;
        desk
    };
    // Refused: pair again, which is also where its other computers are.
    let [pair, retry, machines] = keys(unshown(QUIC_REFUSED));
    assert!(pair.is_some_and(|h| h >= 44.0) && retry.is_some_and(|h| h >= 44.0));
    assert_eq!(machines, None);
    // Unanswered: try again, or go elsewhere.
    let [pair, retry, machines] = keys(unshown(UNANSWERED));
    assert!(pair.is_none() && retry.is_some() && machines.is_some_and(|h| h >= 44.0));
    // Dialing says nothing at first, then offers a retry beside its hint.
    let mut dialing = unshown(UNANSWERED);
    dialing.status = Status::Connecting;
    dialing.error = None;
    dialing.link.down_since = None;
    let [pair, retry, machines] = keys(dialing);
    assert!(pair.is_none() && retry.is_none() && machines.is_some());
    let mut slow = unshown(UNANSWERED);
    slow.status = Status::Connecting;
    slow.error = None;
    let [_, retry, _] = keys(slow);
    assert!(retry.is_some());
}
