//! Server-free captures of every built-in backdrop shader. Uses only
//! disposable HOME/config.

fn main() {
    let directory = std::path::PathBuf::from(
        std::env::args_os()
            .nth(1)
            .expect("usage: backdrop_preview <output-directory>"),
    );
    let home = std::path::PathBuf::from(std::env::var_os("HOME").expect("HOME"))
        .canonicalize()
        .expect("temporary HOME");
    let config =
        std::path::PathBuf::from(std::env::var_os("XDG_CONFIG_HOME").expect("XDG_CONFIG_HOME"));
    let state =
        std::path::PathBuf::from(std::env::var_os("XDG_STATE_HOME").expect("XDG_STATE_HOME"));
    assert!(
        home.starts_with(std::env::temp_dir().canonicalize().unwrap())
            && config.starts_with(&home)
            && state.starts_with(&home),
        "use a temporary HOME, config and state"
    );
    assert!(
        std::env::var_os("PHUX_SOCKET").is_none() && std::env::var_os("PHUX_PROFILE").is_none(),
        "no named server"
    );
    gpui_platform::application()
        .with_assets(phux_desktop::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            phux_desktop::app::capture_backdrop_fixture(
                phux_desktop::app::AppOptions {
                    socket: home.join("absent.sock"),
                    session: "phux".into(),
                    phux: home.join("no-phux"),
                    layout_path: home.join("layout.json"),
                    fresh: false,
                    initial_terminal: None,
                    quick: false,
                    dev_telemetry: false,
                    startup_error: Some("Backdrop preview: embedded runtime".into()),
                    cli_install: None,
                },
                directory.clone(),
                cx,
            )
            .expect("backdrop preview");
        });
}
