//! Capture every Settings page from a real desk into a directory, then exit.
//! Run it with disposable state; it refuses the user's config:
//!
//! `HOME=$tmp XDG_CONFIG_HOME=$tmp/config XDG_STATE_HOME=$tmp/state
//!  cargo run --features ui-fixtures --example settings_preview -- <out>`

fn main() {
    let directory = std::path::PathBuf::from(
        std::env::args_os()
            .nth(1)
            .expect("usage: settings_preview <output-directory>"),
    );
    let real = |key: &str| {
        std::env::var_os(key)
            .map(std::path::PathBuf::from)
            .and_then(|path| path.canonicalize().ok())
    };
    let (home, config, temp) = (
        real("HOME").expect("HOME"),
        real("XDG_CONFIG_HOME").expect("XDG_CONFIG_HOME"),
        std::env::temp_dir().canonicalize().expect("temp dir"),
    );
    assert!(
        config.starts_with(&home) && home.starts_with(temp),
        "set HOME to a temporary directory and XDG_CONFIG_HOME inside it"
    );
    assert!(
        std::env::var_os("PHUX_SOCKET").is_none() && std::env::var_os("PHUX_PROFILE").is_none(),
        "the preview never aims at a named server"
    );
    gpui_platform::application()
        .with_assets(phux_desktop::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            phux_desktop::app::capture_settings_fixture(
                phux_desktop::app::AppOptions {
                    socket: home.join("absent.sock"),
                    session: "settings-preview".into(),
                    phux: home.join("no-phux"),
                    layout_path: home.join("layout.json"),
                    fresh: false,
                    initial_terminal: None,
                    quick: false,
                    dev_telemetry: false,
                    startup_error: Some("Settings preview: no server".into()),
                    cli_install: None,
                },
                directory.clone(),
                cx,
            )
            .expect("settings preview");
        });
}
