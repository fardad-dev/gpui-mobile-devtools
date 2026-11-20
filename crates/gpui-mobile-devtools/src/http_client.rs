//! A GPUI `HttpClient` that reports every request to the Network panel.
//!
//! GPUI loads remote images through the app's `HttpClient` too, so wrapping
//! the client covers `img("https://…")` as well as the app's own calls.
//!
//! `gpui-pre` and Zed's `gpui` 0.2 publish the same trait under different
//! crates (0.2 adds `type_name`), so the wrapper is written once as a macro.

macro_rules! devtools_http_client {
    ($module:ident, $krate:ident, { $($extra:item)* }) => {
        pub(crate) mod $module {
            use crate::network;
            use std::future::Future;
            use std::io;
            use std::pin::Pin;
            use std::sync::Arc;
            use std::task::{Context, Poll};
            use $krate::{http, AsyncBody, HttpClient, Inner, Response, Url};

            type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

            /// Wraps `client` so its requests show up in the Network panel.
            ///
            /// Requests go straight to `client` while devtools aren't running,
            /// and in release builds `client` is returned as is.
            pub fn wrap_http_client(client: Arc<dyn HttpClient>) -> Arc<dyn HttpClient> {
                if !cfg!(debug_assertions) && !crate::is_active() {
                    return client;
                }
                Arc::new(DevtoolsHttpClient { inner: client })
            }

            struct DevtoolsHttpClient {
                inner: Arc<dyn HttpClient>,
            }

            fn header_list(headers: &http::HeaderMap) -> Vec<network::Header> {
                headers
                    .iter()
                    .map(|(name, value)| {
                        let value = value.to_str().unwrap_or("<non-UTF-8 value>");
                        (name.as_str().to_owned(), value.to_owned())
                    })
                    .collect()
            }

            impl HttpClient for DevtoolsHttpClient {
                fn user_agent(&self) -> Option<&http::HeaderValue> {
                    self.inner.user_agent()
                }

                fn proxy(&self) -> Option<&Url> {
                    self.inner.proxy()
                }

                fn send(
                    &self,
                    request: http::Request<AsyncBody>,
                ) -> BoxFuture<anyhow::Result<Response<AsyncBody>>> {
                    if !crate::is_active() {
                        return self.inner.send(request);
                    }
                    let request_body = match &request.body().0 {
                        Inner::Bytes(cursor) => Some(cursor.get_ref().clone()),
                        _ => None,
                    };
                    let mut report = network::request(
                        request.method().as_str(),
                        &request.uri().to_string(),
                        &header_list(request.headers()),
                        request_body.as_deref(),
                    );
                    let response = self.inner.send(request);
                    Box::pin(async move {
                        match response.await {
                            Ok(response) => {
                                report.response(
                                    response.status().as_u16(),
                                    &header_list(response.headers()),
                                );
                                let (parts, body) = response.into_parts();
                                Ok(Response::from_parts(parts, tee(body, report)))
                            }
                            Err(error) => {
                                report.failed(&format!("{error:#}"));
                                Err(error)
                            }
                        }
                    })
                }

                $($extra)*
            }

            /// Hands `body` on, copying what the Network panel keeps.
            fn tee(body: AsyncBody, report: network::Request) -> AsyncBody {
                let keep = report.wants_body();
                let limit = report.max_body_bytes();
                if let Inner::Bytes(cursor) = &body.0 {
                    let bytes = cursor.get_ref();
                    let kept = if keep { &bytes[..bytes.len().min(limit)] } else { &[][..] };
                    report.finished(kept, bytes.len(), kept.len() < bytes.len());
                    return body;
                }
                AsyncBody::from_reader(Tee {
                    body,
                    report: Some(report),
                    kept: Vec::new(),
                    keep,
                    limit,
                    size: 0,
                })
            }

            struct Tee {
                body: AsyncBody,
                report: Option<network::Request>,
                kept: Vec<u8>,
                keep: bool,
                limit: usize,
                size: usize,
            }

            impl Tee {
                fn finish(&mut self) {
                    if let Some(report) = self.report.take() {
                        let truncated = self.kept.len() < self.size;
                        report.finished(&self.kept, self.size, truncated && self.keep);
                    }
                }
            }

            impl futures_io::AsyncRead for Tee {
                fn poll_read(
                    self: Pin<&mut Self>,
                    cx: &mut Context<'_>,
                    buf: &mut [u8],
                ) -> Poll<io::Result<usize>> {
                    let this = self.get_mut();
                    let poll = Pin::new(&mut this.body).poll_read(cx, buf);
                    match &poll {
                        Poll::Ready(Ok(0)) if !buf.is_empty() => this.finish(),
                        Poll::Ready(Ok(read)) => {
                            this.size += read;
                            if this.keep && this.kept.len() < this.limit {
                                let room = this.limit - this.kept.len();
                                this.kept.extend_from_slice(&buf[..(*read).min(room)]);
                            }
                        }
                        Poll::Ready(Err(error)) => {
                            if let Some(report) = this.report.take() {
                                report.failed(&error.to_string());
                            }
                        }
                        Poll::Pending => {}
                    }
                    poll
                }
            }

            impl Drop for Tee {
                /// Readers that stop at the expected length never see the end
                /// of the stream; report what they read.
                fn drop(&mut self) {
                    self.finish();
                }
            }
        }
    };
}

#[cfg(feature = "gpui-pre")]
devtools_http_client!(gpui_pre, gpui_pre_http, {});

#[cfg(feature = "gpui")]
devtools_http_client!(gpui, gpui_http, {
    fn type_name(&self) -> &'static str {
        "gpui_mobile_devtools::DevtoolsHttpClient"
    }
});
