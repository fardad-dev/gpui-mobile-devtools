//! The server inside the app. One port serves:
//!
//! * `GET /`: the viewer page,
//! * `GET /hello`: the session's `hello` event as JSON (used by the CLI to
//!   tell whether the app is up),
//! * `GET /ws` with a WebSocket upgrade: the event stream. It sends a
//!   `hello`, then the buffered history, then events as they happen.
//!
//! On a LAN listener every request needs the session token as a `token`
//! query parameter.

use crate::event::{hello_event, random_hex};
use crate::{hub, Config};
use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{IpAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::mpsc::RecvTimeoutError;
use std::thread;
use std::time::Duration;
use tungstenite::protocol::Role;
use tungstenite::{Message, WebSocket};

const LOG_TARGET: &str = "gpui_mobile_devtools";

pub(crate) fn start(config: &Config) {
    let host = if config.lan { "0.0.0.0" } else { "127.0.0.1" };
    let listener = match TcpListener::bind((host, config.port)) {
        Ok(listener) => listener,
        Err(error) => {
            log::warn!(target: LOG_TARGET, "devtools: can't listen on {host}:{}: {error}", config.port);
            return;
        }
    };
    let token = config.lan.then(random_hex);
    announce(config.port, token.as_deref());

    let spawned = thread::Builder::new()
        .name("devtools-server".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let token = token.clone();
                let _ = thread::Builder::new()
                    .name("devtools-viewer".into())
                    .spawn(move || {
                        let _ = handle(stream, token.as_deref());
                    });
            }
        });
    if let Err(error) = spawned {
        log::warn!(target: LOG_TARGET, "devtools: can't start the server thread: {error}");
    }
}

/// Logs how to connect.
fn announce(port: u16, token: Option<&str>) {
    match token {
        None => log::info!(
            target: LOG_TARGET,
            "devtools: run `gpui-mobile-devtools` on your computer \
             (or `adb forward tcp:{port} tcp:{port}` and open http://127.0.0.1:{port}/)"
        ),
        Some(token) => {
            let ip = lan_ip().map_or_else(|| "<phone-ip>".to_owned(), |ip| ip.to_string());
            log::info!(
                target: LOG_TARGET,
                "devtools: open http://{ip}:{port}/?token={token} on your computer \
                 (or run `gpui-mobile-devtools http://{ip}:{port}/?token={token}`)"
            );
        }
    }
}

/// The address other machines on the network reach this one at. Connecting
/// a UDP socket sends nothing; it only picks the outgoing interface.
fn lan_ip() -> Option<IpAddr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    Some(socket.local_addr().ok()?.ip())
}

struct Request {
    method: String,
    path: String,
    query: HashMap<String, String>,
    headers: HashMap<String, String>,
}

fn read_request(stream: &mut TcpStream) -> io::Result<Request> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut head = Vec::new();
    let mut chunk = [0u8; 1024];
    while !head.windows(4).any(|window| window == b"\r\n\r\n") {
        if head.len() > 16 * 1024 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "request head too large"));
        }
        // Reading one byte at a time past the head would be safest, but a
        // client sends nothing after the head until it gets our response.
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        head.extend_from_slice(&chunk[..read]);
    }
    let head = String::from_utf8_lossy(&head);
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next().unwrap_or_default().split(' ');
    let method = request_line.next().unwrap_or_default().to_owned();
    let target = request_line.next().unwrap_or("/");
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let query = query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect();
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    Ok(Request {
        method,
        path: path.to_owned(),
        query,
        headers,
    })
}

fn respond(stream: &mut TcpStream, status: &str, content_type: &str, body: &str) -> io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    stream.flush()
}

/// Browsers send `Origin` with WebSocket requests. Only the viewer page,
/// served from loopback or from this server, may read the stream, not
/// whatever site happens to be open in the same browser.
fn origin_allowed(request: &Request) -> bool {
    let Some(origin) = request.headers.get("origin") else {
        return true;
    };
    let without_scheme = origin.split_once("://").map_or(origin.as_str(), |(_, rest)| rest);
    let origin_host = strip_port(without_scheme);
    let own_host = request.headers.get("host").map(|host| strip_port(host));
    matches!(origin_host, "127.0.0.1" | "localhost" | "[::1]" | "null")
        || Some(origin_host) == own_host
}

/// `host:port` or `[v6]:port` to the host part.
fn strip_port(authority: &str) -> &str {
    if authority.starts_with('[') {
        return authority
            .find(']')
            .map_or(authority, |end| &authority[..=end]);
    }
    authority.rsplit_once(':').map_or(authority, |(host, _)| host)
}

fn handle(mut stream: TcpStream, token: Option<&str>) -> io::Result<()> {
    let request = read_request(&mut stream)?;
    if let Some(token) = token {
        if request.query.get("token").map(String::as_str) != Some(token) {
            return respond(&mut stream, "403 Forbidden", "text/plain", "missing or wrong token\n");
        }
    }
    if request.method != "GET" {
        return respond(&mut stream, "405 Method Not Allowed", "text/plain", "GET only\n");
    }
    match request.path.as_str() {
        "/" | "/index.html" => respond(&mut stream, "200 OK", "text/html; charset=utf-8", crate::VIEWER_HTML),
        "/hello" => respond(&mut stream, "200 OK", "application/json", &hello_event().to_string()),
        "/ws" => {
            let upgrade = request
                .headers
                .get("upgrade")
                .is_some_and(|upgrade| upgrade.eq_ignore_ascii_case("websocket"));
            let Some(key) = request.headers.get("sec-websocket-key").filter(|_| upgrade) else {
                return respond(&mut stream, "400 Bad Request", "text/plain", "WebSocket only\n");
            };
            if !origin_allowed(&request) {
                return respond(&mut stream, "403 Forbidden", "text/plain", "origin not allowed\n");
            }
            let accept = tungstenite::handshake::derive_accept_key(key.as_bytes());
            write!(
                stream,
                "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
                 Sec-WebSocket-Accept: {accept}\r\n\r\n"
            )?;
            stream.flush()?;
            stream_events(stream)
        }
        _ => respond(&mut stream, "404 Not Found", "text/plain", "not found\n"),
    }
}

fn stream_events(stream: TcpStream) -> io::Result<()> {
    let Some(hub) = hub::get() else {
        return Ok(());
    };
    // Short read timeout: reads only look for a close or ping between
    // batches of events. A viewer that stops reading is dropped after 10 s.
    stream.set_read_timeout(Some(Duration::from_millis(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    stream.set_nodelay(true)?;
    let mut socket = WebSocket::from_raw_socket(stream, Role::Server, None);

    let (history, events) = hub.subscribe();
    let send = |socket: &mut WebSocket<TcpStream>, text: &str| {
        socket.write(Message::text(text)).map_err(io::Error::other)
    };
    send(&mut socket, &hello_event().to_string())?;
    for event in &history {
        send(&mut socket, event)?;
    }
    drop(history);
    socket.flush().map_err(io::Error::other)?;

    loop {
        match events.recv_timeout(Duration::from_millis(50)) {
            Ok(event) => {
                send(&mut socket, &event)?;
                for event in events.try_iter().take(1024) {
                    send(&mut socket, &event)?;
                }
                socket.flush().map_err(io::Error::other)?;
            }
            Err(RecvTimeoutError::Timeout) => {}
            // The hub dropped this viewer for falling behind.
            Err(RecvTimeoutError::Disconnected) => break,
        }
        match socket.read() {
            Ok(Message::Close(_)) => break,
            Ok(_) => {}
            Err(tungstenite::Error::Io(error))
                if matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {}
            Err(_) => break,
        }
    }
    let _ = socket.close(None);
    let _ = socket.flush();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(origin: Option<&str>, host: &str) -> Request {
        let mut headers = HashMap::from([("host".to_owned(), host.to_owned())]);
        if let Some(origin) = origin {
            headers.insert("origin".to_owned(), origin.to_owned());
        }
        Request {
            method: "GET".into(),
            path: "/ws".into(),
            query: HashMap::new(),
            headers,
        }
    }

    #[test]
    fn allows_only_loopback_or_own_origins() {
        assert!(origin_allowed(&request(None, "127.0.0.1:9700")));
        assert!(origin_allowed(&request(Some("http://127.0.0.1:9700"), "127.0.0.1:9700")));
        assert!(origin_allowed(&request(Some("http://localhost:9700"), "127.0.0.1:9700")));
        assert!(origin_allowed(&request(Some("http://192.168.1.5:9700"), "192.168.1.5:9700")));
        assert!(!origin_allowed(&request(Some("https://evil.example"), "127.0.0.1:9700")));
        assert!(!origin_allowed(&request(Some("http://evil.example:9700"), "127.0.0.1:9700")));
    }
}
