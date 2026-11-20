//! Reports HTTP requests to the Network panel from any HTTP client.
//!
//! [`crate::wrap_http_client`] uses this for GPUI's client. For another
//! client, call [`request`] when a request starts, then
//! [`Request::response`] when the head arrives, and [`Request::finished`] or
//! [`Request::failed`] at the end:
//!
//! ```
//! use gpui_mobile_devtools::network;
//!
//! let mut request = network::request("GET", "https://example.com/api", &[], None);
//! // ... send it ...
//! request.response(200, &[("content-type".into(), "application/json".into())]);
//! request.finished(br#"{"ok":true}"#, 11, false);
//! ```
//!
//! Every function is a cheap no-op while devtools aren't running.

use crate::event::{now_ms, publish};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// A header name and value.
pub type Header = (String, String);

/// One reported request. Dropping it before [`Request::finished`] or
/// [`Request::failed`] reports the request as cancelled.
pub struct Request {
    id: u64,
    started: Instant,
    done: bool,
    /// Whether the response body should be kept, decided from its
    /// content type.
    keep_body: bool,
}

/// Starts reporting a request and returns its handle. `body` is the request
/// body if it is in memory.
pub fn request(method: &str, url: &str, headers: &[Header], body: Option<&[u8]>) -> Request {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    if crate::is_active() {
        let content_type = header(headers, "content-type");
        publish(json!({
            "type": "http.request",
            "id": id,
            "ts": now_ms(),
            "method": method,
            "url": url,
            "headers": headers,
            "body": body.map(|body| body_json(body, content_type, body.len(), false)),
        }));
    }
    Request {
        id,
        started: Instant::now(),
        done: false,
        keep_body: true,
    }
}

impl Request {
    /// Whether devtools want the response body. When false, callers can skip
    /// copying it and pass an empty slice to [`Request::finished`].
    pub fn wants_body(&self) -> bool {
        self.keep_body && crate::is_active()
    }

    /// The largest body worth copying, from [`crate::Config::max_body_bytes`].
    pub fn max_body_bytes(&self) -> usize {
        crate::config().map(|config| config.max_body_bytes).unwrap_or(0)
    }

    /// Reports the response head.
    pub fn response(&mut self, status: u16, headers: &[Header]) {
        let content_type = header(headers, "content-type").unwrap_or("");
        self.keep_body = keeps_body(content_type);
        if !crate::is_active() {
            return;
        }
        publish(json!({
            "type": "http.response",
            "id": self.id,
            "ts": now_ms(),
            "elapsedMs": self.elapsed_ms(),
            "status": status,
            "headers": headers,
        }));
    }

    /// Reports that the response body was read. `body` is the start of it
    /// (empty when not kept), `size` its full length, `truncated` whether
    /// `body` is shorter than the full body.
    pub fn finished(mut self, body: &[u8], size: usize, truncated: bool) {
        self.done = true;
        if !crate::is_active() {
            return;
        }
        let body = (!body.is_empty()).then(|| body_json(body, None, size, truncated));
        publish(json!({
            "type": "http.finished",
            "id": self.id,
            "ts": now_ms(),
            "elapsedMs": self.elapsed_ms(),
            "size": size,
            "body": body,
        }));
    }

    /// Reports that the request failed.
    pub fn failed(mut self, error: &str) {
        self.done = true;
        self.publish_failed(error);
    }

    fn publish_failed(&self, error: &str) {
        if !crate::is_active() {
            return;
        }
        publish(json!({
            "type": "http.failed",
            "id": self.id,
            "ts": now_ms(),
            "elapsedMs": self.elapsed_ms(),
            "error": error,
        }));
    }

    fn elapsed_ms(&self) -> f64 {
        self.started.elapsed().as_secs_f64() * 1000.0
    }
}

impl Drop for Request {
    fn drop(&mut self) {
        if !self.done {
            self.publish_failed("cancelled");
        }
    }
}

fn header<'a>(headers: &'a [Header], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

/// Images, video, audio, fonts and archives are only counted, not kept.
fn keeps_body(content_type: &str) -> bool {
    let content_type = content_type.to_ascii_lowercase();
    !["image/", "video/", "audio/", "font/", "application/octet-stream", "application/zip"]
        .iter()
        .any(|prefix| content_type.starts_with(prefix))
}

/// A body for the viewer: text when it is UTF-8 (cut at the configured
/// limit), otherwise only its size.
fn body_json(body: &[u8], content_type: Option<&str>, size: usize, truncated: bool) -> Value {
    let limit = crate::config().map(|config| config.max_body_bytes).unwrap_or(0);
    let binary_type = content_type.is_some_and(|content_type| !keeps_body(content_type));
    let (body, truncated) = if body.len() > limit {
        (&body[..limit], true)
    } else {
        (body, truncated)
    };
    // A cut can split a UTF-8 character; drop the partial one.
    let text = match std::str::from_utf8(body) {
        Ok(text) => Some(text),
        Err(error) if error.error_len().is_none() => {
            std::str::from_utf8(&body[..error.valid_up_to()]).ok()
        }
        Err(_) => None,
    };
    match text {
        Some(text) if !binary_type => json!({ "text": text, "size": size, "truncated": truncated }),
        _ => json!({ "binary": true, "size": size }),
    }
}
