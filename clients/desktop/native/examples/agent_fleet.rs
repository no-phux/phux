//! Capture the actual fleet UI with representative inventory and disposable state.
//! No live agent, daemon, credentials or production socket is used.

fn main() {
    let directory = std::path::PathBuf::from(
        std::env::args_os()
            .nth(1)
            .expect("usage: agent_fleet <output-directory> [film]"),
    );
    let film = std::env::args().nth(2).as_deref() == Some("film");
    let home = std::path::PathBuf::from(std::env::var_os("HOME").expect("disposable HOME"))
        .canonicalize()
        .expect("HOME exists");
    let temp = std::env::temp_dir()
        .canonicalize()
        .expect("temporary directory");
    let config =
        std::path::PathBuf::from(std::env::var_os("XDG_CONFIG_HOME").expect("disposable config"))
            .canonicalize()
            .expect("config exists");
    assert!(
        home.starts_with(temp) && config.starts_with(&home),
        "use a temporary HOME and config"
    );
    assert!(
        std::env::var_os("PHUX_SOCKET").is_none() && std::env::var_os("PHUX_PROFILE").is_none(),
        "preview never targets a named server"
    );
    gpui_platform::application()
        .with_assets(phux_desktop::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            let capture = if film {
                phux_desktop::app::capture_fleet_film
            } else {
                phux_desktop::app::capture_fleet_fixture
            };
            capture(
                phux_desktop::app::AppOptions {
                    socket: home.join("absent.sock"),
                    session: "fleet-preview".into(),
                    phux: home.join("no-phux"),
                    layout_path: home.join("layout.json"),
                    fresh: true,
                    initial_terminal: None,
                    quick: false,
                    dev_telemetry: false,
                    startup_error: Some("Isolated design fixture".into()),
                    cli_install: None,
                },
                directory.clone(),
                cx,
            )
            .expect("fleet fixture");
        });
}
