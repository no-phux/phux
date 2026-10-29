//! Exercise the actual Windows icon adapter without a window or WSL server.
use base64::Engine as _;
use gpui::Platform as _;

fn main() -> anyhow::Result<()> {
    let platform = gpui_windows::WindowsPlatform::new(true)?;
    let catalog: serde_json::Value =
        serde_json::from_str(include_str!("../../packaging/brand-profiles.json"))?;
    for profile in catalog["profiles"].as_array().expect("approved profiles") {
        let png = base64::engine::general_purpose::STANDARD
            .decode(profile["iconPng"].as_str().expect("approved PNG"))?;
        platform.set_app_icon(&png)?;
        println!("Windows native icon accepted: {}", profile["id"]);
    }
    anyhow::ensure!(platform.set_app_icon(b"invalid PNG").is_err());
    Ok(())
}
