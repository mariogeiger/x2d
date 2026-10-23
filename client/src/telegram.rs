//! Signing in from the bot's Mini App: Telegram opens the page with the init
//! data it signed in the address's fragment, which the page hands to the
//! server once, then takes out of the address.

use wasm_bindgen::JsValue;
use wasm_bindgen_futures::JsFuture;

/// Hands the server the Mini App's init data, if Telegram opened the page.
pub async fn sign_in() {
    let Some(window) = web_sys::window() else { return };
    let location = window.location();
    let Some(init_data) = location
        .hash()
        .ok()
        .and_then(|hash| web_sys::UrlSearchParams::new_with_str(hash.trim_start_matches('#')).ok())
        .and_then(|fragment| fragment.get("tgWebAppData"))
    else {
        return;
    };
    let request = web_sys::RequestInit::new();
    request.set_method("POST");
    request.set_body(&JsValue::from_str(&init_data));
    _ = JsFuture::from(window.fetch_with_str_and_init("/api/telegram", &request)).await;
    // The fragment holds what Telegram signed: it leaves the address and the
    // history.
    if let (Ok(history), Ok(path)) = (window.history(), location.pathname()) {
        _ = history.replace_state_with_url(&JsValue::NULL, "", Some(&path));
    }
}
