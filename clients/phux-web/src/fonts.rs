//! Load the embedded face before any hosted or standalone terminal measures cells.
use std::cell::OnceCell;
use wasm_bindgen::JsValue;
use wasm_bindgen_futures::JsFuture;
use web_sys::FontFace;

thread_local! {
    static PAPER_MONO: OnceCell<FontFace> = const { OnceCell::new() };
}

fn face() -> Result<FontFace, JsValue> {
    PAPER_MONO.with(|slot| {
        if let Some(face) = slot.get() {
            return Ok(face.clone());
        }
        let face = FontFace::new_with_u8_array(
            "Paper Mono",
            include_bytes!("../assets/fonts/PaperMono-Regular.woff2"),
        )?;
        let _ = slot.set(face.clone());
        Ok(face)
    })
}

/// Load and register the bundled Paper Mono face, shared by every client path.
///
/// # Errors
/// Fails if the font cannot load or no browser document is available.
pub async fn load() -> Result<(), JsValue> {
    let face = face()?;
    JsFuture::from(face.load()?).await?;
    let document = web_sys::window()
        .and_then(|window| window.document())
        .ok_or_else(|| JsValue::from_str("no document for terminal font"))?;
    document.fonts().add(&face)?;
    Ok(())
}
