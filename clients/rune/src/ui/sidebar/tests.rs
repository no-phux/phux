use crate::host_color;
use crate::theme::{DIM_BAR, contrast};
use phux_config_core::host_color::PALETTE;

#[test]
fn host_marks_read_in_every_theme() {
    let mut low = Vec::new();
    for theme in std::iter::once(None).chain(crate::theme::all().map(Some)) {
        for dark in [true, false] {
            let ink = super::super::ink(dark, rune::oklch(0.6, 0.15, 250.0), theme);
            for (i, (id, _)) in PALETTE.iter().enumerate() {
                let host = host_color::swatch(i, ink.sidebar);
                let name = theme.map_or("system", |t| t.id);
                // Eased for the sidebar; the focused card's rim sits on
                // the window ground, a shade away.
                let mark = contrast(host, ink.sidebar);
                if mark < DIM_BAR || contrast(host, ink.bg) < DIM_BAR - 0.1 {
                    low.push(format!("{id} on {name}/{dark}: {mark:.2}"));
                }
            }
        }
    }
    assert!(low.is_empty(), "{low:#?}");
}

#[test]
fn twins_with_an_agent_still_name_their_pane() {
    let id = phux_protocol::ids::ResourceId::local(90);
    let detail = |place: Option<&str>| {
        super::Row {
            id: &id,
            session: "rune-3",
            name: "Shopify inventory sync to Meta".to_owned(),
            place: place.map(str::to_owned),
            key: None,
            agent: Some("claude"),
            mark: crate::activity::Mark::Done,
            selected: false,
            host: ("mini", 0),
            color: None,
        }
        .detail()
        .map(|(words, _)| words)
    };
    assert_eq!(detail(Some("@90")).as_deref(), Some("claude · done · @90"));
    assert_eq!(detail(None).as_deref(), Some("claude · done"));
}
