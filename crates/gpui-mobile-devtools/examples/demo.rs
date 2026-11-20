//! Runs devtools on this computer with fake traffic, to try the viewer
//! without a phone:
//!
//! ```text
//! cargo run --example demo
//! # then open http://127.0.0.1:9600, or run `gpui-mobile-devtools 127.0.0.1:9600`
//! ```

use gpui_pre_http::{http, AsyncBody, HttpClient, Response, Url};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

/// Answers every request after a short delay: JSON for `/api/…`, a fake PNG
/// for `.png`, a 404 for `/missing`, an error for `/offline`.
struct FakeClient;

impl HttpClient for FakeClient {
    fn user_agent(&self) -> Option<&http::HeaderValue> {
        None
    }

    fn proxy(&self) -> Option<&Url> {
        None
    }

    fn send(
        &self,
        request: http::Request<AsyncBody>,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<Response<AsyncBody>>> + Send + 'static>> {
        let path = request.uri().path().to_owned();
        Box::pin(async move {
            std::thread::sleep(Duration::from_millis(80));
            if path == "/offline" {
                anyhow::bail!("error sending request: dns error: failed to lookup address");
            }
            let (status, content_type, body): (u16, &str, Vec<u8>) = if path.ends_with(".png") {
                (200, "image/png", vec![0x89; 48_000])
            } else if path == "/missing" {
                (404, "application/json", br#"{"status":404,"message":"not found"}"#.to_vec())
            } else {
                let body = serde_json::json!({
                    "status": 200,
                    "data": { "path": path, "title": "پیشنهاد شگفت‌انگیز", "items": [1, 2, 3], "ok": true, "price": null },
                });
                (200, "application/json; charset=utf-8", body.to_string().into_bytes())
            };
            Ok(Response::builder()
                .status(status)
                .header("content-type", content_type)
                .body(AsyncBody::from_reader(futures_io_cursor(body)))?)
        })
    }
}

/// A streaming body, like reqwest's.
fn futures_io_cursor(bytes: Vec<u8>) -> impl futures_io::AsyncRead + Send + Sync {
    struct Reader(std::io::Cursor<Vec<u8>>);
    impl futures_io::AsyncRead for Reader {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            buf: &mut [u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            std::task::Poll::Ready(std::io::Read::read(&mut self.0, buf))
        }
    }
    Reader(std::io::Cursor::new(bytes))
}

/// Polls a future to completion on this thread (the fake client never
/// returns `Pending`).
fn block_on<F: Future>(future: F) -> F::Output {
    use std::task::{Context, Poll, Waker};
    let mut future = std::pin::pin!(future);
    let mut cx = Context::from_waker(Waker::noop());
    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut cx) {
            return output;
        }
    }
}

fn read_all(body: &mut AsyncBody) -> usize {
    use futures_io::AsyncRead;
    let mut total = 0;
    let mut buf = [0u8; 4096];
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    while let std::task::Poll::Ready(Ok(read)) = Pin::new(&mut *body).poll_read(&mut cx, &mut buf) {
        if read == 0 {
            break;
        }
        total += read;
    }
    total
}

fn main() {
    gpui_mobile_devtools::init(gpui_mobile_devtools::Config::new("demo").enabled(true));
    let client = gpui_mobile_devtools::wrap_http_client(Arc::new(FakeClient));

    let urls = [
        ("GET", "https://api.example.com/api/home?page=1"),
        ("POST", "https://api.example.com/api/cart"),
        ("GET", "https://cdn.example.com/images/banner.png"),
        ("GET", "https://api.example.com/missing"),
        ("GET", "https://api.example.com/offline"),
    ];
    for round in 0.. {
        log::info!("round {round}: loading the home page");
        log::debug!(target: "demo::widgets", "laid out 12 widgets in 3.2 ms");
        if round % 3 == 0 {
            log::warn!("slow frame: 31 ms");
        }
        if round % 5 == 4 {
            log::error!("home page: request failed: timed out");
        }
        let (method, url) = urls[round % urls.len()];
        let body = (method == "POST").then_some(r#"{"productId":123,"quantity":1}"#);
        let request = http::Request::builder()
            .method(method)
            .uri(url)
            .header("accept", "application/json")
            .header("x-app-version", "1.0.0")
            .body(body.map_or_else(AsyncBody::empty, AsyncBody::from))
            .unwrap();
        match block_on(client.send(request)) {
            Ok(mut response) => {
                let size = read_all(response.body_mut());
                log::debug!("{method} {url}: {} ({size} bytes)", response.status());
            }
            Err(error) => log::warn!("{method} {url}: {error}"),
        }
        std::thread::sleep(Duration::from_millis(1500));
    }
}
