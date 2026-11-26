//! The browser's side of cwbcode's API: Cap'n Proto messages over `fetch`. Which HTTP
//! version carries them is the browser's choice (HTTP/3 when it accepts the server's
//! certificate for QUIC, otherwise HTTP/2); the server says which in its status.
//!
//! A page the backend serves asks it at its own address. In the Mac app the page is the app's
//! own (a fixed origin, tauri://localhost: what it keeps in the browser stays from one start
//! to the next) and the backend is on a port chosen when the app starts: the app opens the
//! page with `#api=http://127.0.0.1:<port>&token=...`, and the page asks there (kept for this
//! window in sessionStorage, for a reload).

use cwbcode_proto::{CALL_PATH, CHAT_PATH, EVENTS_PATH, Event, Frames, Progress, Request, Response};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

fn js_error(value: JsValue) -> String {
    value.as_string().or_else(|| js_sys::Reflect::get(&value, &"message".into()).ok().and_then(|m| m.as_string())).unwrap_or_else(|| "the request failed".to_string())
}

const BASE_KEY: &str = "cwbcode-api";

/// Only the app's backend, on this machine, is taken as an address to ask.
fn local_base(value: &str) -> Option<String> {
    let rest = value.strip_prefix("http://127.0.0.1:").or_else(|| value.strip_prefix("http://localhost:"))?;
    (!rest.is_empty() && rest.len() <= 5 && rest.chars().all(|c| c.is_ascii_digit())).then(|| value.to_string())
}

/// Take the backend's address from the page's address (`#api=...`), before anything is asked.
pub fn init_base() {
    let Some(window) = web_sys::window() else { return };
    let hash = window.location().hash().unwrap_or_default();
    for pair in hash.trim_start_matches('#').split('&') {
        if let Some(value) = pair.strip_prefix("api=") {
            let value = js_sys::decode_uri_component(value).ok().and_then(|v| v.as_string()).unwrap_or_default();
            if let (Some(base), Ok(Some(storage))) = (local_base(&value), window.session_storage()) {
                let _ = storage.set_item(BASE_KEY, &base);
            }
        }
    }
}

/// Where the backend is: "" (this page's own server), or the app's backend.
pub fn base() -> String {
    web_sys::window().and_then(|w| w.session_storage().ok().flatten()).and_then(|s| s.get_item(BASE_KEY).ok().flatten()).and_then(|v| local_base(&v)).unwrap_or_default()
}

async fn post(token: &str, path: &str, body: Vec<u8>) -> Result<web_sys::Response, String> {
    let init = web_sys::RequestInit::new();
    init.set_method("POST");
    init.set_body(&js_sys::Uint8Array::from(&body[..]));
    let headers = web_sys::Headers::new().map_err(js_error)?;
    // In local mode there is no login, and nothing to present.
    if !token.is_empty() {
        headers.set("authorization", &format!("Bearer {token}")).map_err(js_error)?;
    }
    headers.set("content-type", "application/x-capnp").map_err(js_error)?;
    init.set_headers(&headers);
    let window = web_sys::window().ok_or("no window")?;
    let response: web_sys::Response = JsFuture::from(window.fetch_with_str_and_init(&format!("{}{path}", base()), &init)).await.map_err(|e| format!("cannot reach the server: {}", js_error(e)))?.unchecked_into();
    if response.status() == 401 {
        return Err("unauthorized".to_string());
    }
    if !response.ok() {
        let said = match response.text() {
            Ok(promise) => JsFuture::from(promise).await.ok().and_then(|t| t.as_string()).unwrap_or_default(),
            Err(_) => String::new(),
        };
        return Err(format!("the server answered {}: {said}", response.status()));
    }
    Ok(response)
}

/// One call and its answer; an error the server reports is an error here.
pub async fn call(token: &str, request: &Request) -> Result<Response, String> {
    let response = post(token, CALL_PATH, request.encode()).await?;
    let buffer = JsFuture::from(response.array_buffer().map_err(js_error)?).await.map_err(js_error)?;
    let bytes = js_sys::Uint8Array::new(&buffer).to_vec();
    Response::decode(&bytes).map_err(|e| format!("the server's answer could not be read: {e}"))?.ok()
}

/// Send a chat message; `on_event` gets each event as the model works.
pub async fn chat(token: &str, request: &Request, mut on_event: impl FnMut(Event)) -> Result<(), String> {
    let response = post(token, CHAT_PATH, request.encode()).await?;
    let reader: web_sys::ReadableStreamDefaultReader = response.body().ok_or("the server sent no stream")?.get_reader().unchecked_into();
    let mut frames = Frames::new();
    loop {
        let chunk = JsFuture::from(reader.read()).await.map_err(|e| format!("the connection broke: {}", js_error(e)))?;
        if js_sys::Reflect::get(&chunk, &"done".into()).map_err(js_error)?.as_bool().unwrap_or(true) {
            break;
        }
        let value = js_sys::Reflect::get(&chunk, &"value".into()).map_err(js_error)?;
        frames.push(&js_sys::Uint8Array::new(&value).to_vec());
        while let Some(event) = frames.next_event().map_err(|e| format!("the stream could not be read: {e}"))? {
            on_event(event);
        }
    }
    if frames.pending() > 0 {
        return Err("the stream ended in the middle of a message".to_string());
    }
    Ok(())
}

/// Follow the model's state and its download (the backend's events stream, the same for every
/// client): `on_progress` gets each message, until the model is ready.
pub async fn events(token: &str, mut on_progress: impl FnMut(Progress)) -> Result<(), String> {
    let response = post(token, EVENTS_PATH, Vec::new()).await?;
    let reader: web_sys::ReadableStreamDefaultReader = response.body().ok_or("the server sent no stream")?.get_reader().unchecked_into();
    let mut frames = Frames::new();
    loop {
        let chunk = JsFuture::from(reader.read()).await.map_err(|e| format!("the connection broke: {}", js_error(e)))?;
        if js_sys::Reflect::get(&chunk, &"done".into()).map_err(js_error)?.as_bool().unwrap_or(true) {
            break;
        }
        let value = js_sys::Reflect::get(&chunk, &"value".into()).map_err(js_error)?;
        frames.push(&js_sys::Uint8Array::new(&value).to_vec());
        while let Some(progress) = frames.next_progress().map_err(|e| format!("the stream could not be read: {e}"))? {
            on_progress(progress);
        }
    }
    Ok(())
}

/// The bytes of a file the user picked.
pub async fn file_bytes(file: &web_sys::File) -> Result<Vec<u8>, String> {
    let buffer = JsFuture::from(file.array_buffer()).await.map_err(js_error)?;
    Ok(js_sys::Uint8Array::new(&buffer).to_vec())
}

#[cfg(test)]
mod tests {
    use super::local_base;

    #[test]
    fn only_the_apps_backend_on_this_machine_is_taken_as_the_address_to_ask() {
        for ok in ["http://127.0.0.1:7481", "http://127.0.0.1:64609", "http://localhost:1"] {
            assert_eq!(local_base(ok).as_deref(), Some(ok));
        }
        for no in ["", "http://127.0.0.1:", "http://127.0.0.1:7481/", "http://127.0.0.1:123456", "https://127.0.0.1:7443", "http://evil.example:80", "http://127.0.0.1.evil.example:80", "http://127.0.0.1:80@evil.example", "javascript:alert(1)"] {
            assert_eq!(local_base(no), None, "{no}");
        }
    }
}
