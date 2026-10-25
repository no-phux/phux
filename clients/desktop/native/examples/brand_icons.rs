//! Verify AppKit icon changes without opening or activating a window.
#[cfg(target_os = "macos")]
fn main() {
    gpui_platform::application().run(|cx| {
        cx.set_activation_policy(gpui::ActivationPolicy::Accessory);
        match phux_desktop::app::verify_runtime_branding(cx) {
            Ok(()) => std::process::exit(0),
            Err(error) => {
                eprintln!("native branding: {error:#}");
                std::process::exit(1);
            }
        }
    });
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("The native application-icon fixture requires macOS");
    std::process::exit(2);
}
