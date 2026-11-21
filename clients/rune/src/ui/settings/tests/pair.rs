//! Pair a phone in Settings: the code, the explicit choice, no tailnet,
//! and a refusal said in a sentence.

use super::*;

#[test]
fn pair_a_phone_shows_a_code_devices_and_the_explicit_choice() {
    let gpu = rune::render::Gpu::headless();
    for (size, name) in [((1100, 720), "pair"), ((640, 360), "pair-min")] {
        let mut rt = runtime((size.0 as f32, size.1 as f32), false);
        let mut camera = Camera::new(&gpu, size, name);
        rt.update(|d, tx| {
            tx.instant();
            d.connection.resources.images = Some(rune::ImageCache::new(std::sync::Arc::new(|| {})));
            d.settings_category(Category::RemoteHosts);
            d.pairing_fixture();
        });
        rt.frame(0.0);
        for key in [
            "pair-qr",
            "pair-expiry",
            "pair-network",
            "pair-copy",
            "pair-new",
        ] {
            assert!(rt.find(key).is_some(), "{key}");
        }
        assert!(rt.find("pair-revoke-1").is_some());
        let expiry = rt.accessibility_json().to_string();
        assert!(expiry.contains("Expires in 9:0"), "counts down");
        assert!(expiry.contains("Over your tailnet"));
        camera.shoot(&mut rt, "");
        // Revealing the device list scrolls past the code.
        rt.update(|_, tx| tx.scroll_into_view("pair-revoke-1"));
        camera.shoot(&mut rt, "-devices");

        // Remote access off: a code only after the explicit choice.
        rt.update(|d, tx| {
            tx.instant();
            d.settings_ui.pairing.code = Some(Err(
                "the server at /s has no remote listener bound because [policy] mode is unset"
                    .to_owned(),
            ));
        });
        rt.frame(0.0);
        assert!(rt.find("pair-allow").is_some());
        assert!(rt.find("pair-qr").is_none());
        rt.update(|_, tx| tx.scroll_into_view("pair-allow"));
        camera.shoot(&mut rt, "-enable");

        // No tailnet: plain words and a way to get one, never the CLI's
        // words, and no Allow button that can't work.
        rt.update(|d, tx| {
            tx.instant();
            d.settings_ui.pairing.code = Some(Err(
                "`phux pair --json --enroll --enable` failed: phux: error: pair: the server at \
                 /run/user/1000/phux/phux.sock has no remote listener bound (wss at ? waiting \
                 for a private network address), so none was minted; pass `--listen HOST:PORT`"
                    .to_owned(),
            ));
        });
        rt.frame(0.0);
        for key in ["pair-no-address", "pair-tailscale", "pair-recheck"] {
            assert!(rt.find(key).is_some(), "{key}");
        }
        assert!(rt.find("pair-allow").is_none() && rt.find("pair-retry").is_none());
        let tree = rt.accessibility_json().to_string();
        assert!(tree.contains("over Tailscale"), "{tree}");
        assert!(
            !tree.contains("--listen") && !tree.contains("phux.sock"),
            "{tree}"
        );
        rt.update(|_, tx| tx.scroll_into_view("pair-recheck"));
        camera.shoot(&mut rt, "-no-tailnet");
        // Coming back to the window asks again, without --enable.
        rt.update(|d, _| d.focus_changed(true));
        assert!(rt.state.settings_ui.pairing.minting, "re-checked on focus");

        // Anything else: a sentence, and the CLI's words only on the clipboard.
        rt.update(|d, tx| {
            tx.instant();
            d.settings_ui.pairing.minting = false;
            d.settings_ui.pairing.code = Some(Err(
                "phux: error: pair: could not read /etc/phux/tokens: permission denied".to_owned(),
            ));
        });
        rt.frame(0.0);
        assert!(rt.find("pair-retry").is_some() && rt.find("pair-details").is_some());
        let tree = rt.accessibility_json().to_string();
        assert!(tree.contains("Couldn't make a code."), "{tree}");
        assert!(!tree.contains("permission denied"), "{tree}");
        rt.update(|_, tx| tx.scroll_into_view("pair-details"));
        camera.shoot(&mut rt, "-error");
    }
}

#[test]
fn search_finds_pairing_and_a_phone_never_offers_it() {
    let mut rt = runtime((1100.0, 720.0), false);
    rt.update(|d, _| d.settings_ui.query = rune::TextEdit::new("pair"));
    rt.frame(0.0);
    assert!(rt.find("pair-phone").is_some());
    let mut rt = runtime((393.0, 852.0), true);
    rt.update(|d, _| d.settings_category(Category::RemoteHosts));
    rt.frame(0.0);
    assert!(rt.find("pair-phone").is_none());
}
