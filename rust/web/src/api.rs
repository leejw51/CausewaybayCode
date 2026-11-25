//! The browser's side of cwbcode's API: Cap'n Proto messages over `fetch`. Which HTTP
//! version carries them is the browser's choice (HTTP/3 when it accepts the server's
//! certificate for QUIC, otherwise HTTP/2); the server says which in its status.

use cwbcode_proto::{CALL_PATH, CHAT_PATH, Event, Frames, Request, Response};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

fn js_error(value: JsValue) -> String {
    value.as_string().or_else(|| js_sys::Reflect::get(&value, &"message".into()).ok().and_then(|m| m.as_string())).unwrap_or_else(|| "the request failed".to_string())
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
    let response: web_sys::Response = JsFuture::from(window.fetch_with_str_and_init(path, &init)).await.map_err(|e| format!("cannot reach the server: {}", js_error(e)))?.unchecked_into();
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

/// The bytes of a file the user picked.
pub async fn file_bytes(file: &web_sys::File) -> Result<Vec<u8>, String> {
    let buffer = JsFuture::from(file.array_buffer()).await.map_err(js_error)?;
    Ok(js_sys::Uint8Array::new(&buffer).to_vec())
}
