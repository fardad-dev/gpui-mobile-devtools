# gpui-mobile-devtools

Console and Network devtools for [GPUI](https://www.gpui.rs) apps running on a
phone. The app serves a viewer for its logs and HTTP traffic; open it in your
desktop browser over adb, or by the phone's IP over Wi-Fi.

- **Console**: `log` records (Debug and up from the app's own crate, Info and
  up from dependencies, configurable), with level, target, file:line, level
  filters and search. Logcat keeps getting Info and up.
- **Network**: every request made through GPUI's `HttpClient`, including the
  images GPUI loads for `img("https://…")`. Status, timing, request and
  response headers, request body, response body (JSON pretty-printed),
  "copy as cURL".
- Keeps recent history in the app, so a viewer opened late still sees startup.
  The viewer survives app restarts and marks each one.
- Debug builds only by default; release builds get just the logger.

## Install

In the app's `Cargo.toml`:

```toml
gpui-mobile-devtools = "0.1"
```

Two lines in the app. Where you used to set up `android_logger`:

```rust
gpui_mobile_devtools::init(gpui_mobile_devtools::Config::new("my-app"));
```

Where you give GPUI its HTTP client:

```rust
let client = gpui_mobile_devtools::wrap_http_client(Arc::new(reqwest_client));
let app = app.with_http_client(client);
```

`init` installs the `log` logger itself: logcat on Android (tag `my-app`),
stderr elsewhere. Remove your own `android_logger::init_once` call.

## Open the viewer

The app serves the viewer itself, on port 9700. Nothing to install on your
computer; any browser works.

### Over adb (USB, or adb over Wi-Fi)

```sh
adb forward tcp:9700 tcp:9700
```

Then open <http://127.0.0.1:9700>.

- Several phones attached: `adb -s <serial> forward tcp:9700 tcp:9700`
  (`adb devices` lists the serials).
- The forward lasts until the phone disconnects or adb restarts; run the
  command again after that. App restarts don't need it: the open page
  reconnects by itself and marks each restart.
- Remove it with `adb forward --remove tcp:9700`.

### By the phone's IP and port (Wi-Fi, no adb)

Build the app with LAN access, in code or at build time:

```rust
Config::new("my-app").lan(true)
```

```sh
GPUI_DEVTOOLS_LAN=1 cargo build ...
```

Then, from a computer on the same network, open:

```text
http://<phone-ip>:9700
```

The phone's IP is in Settings → About phone → Status → IP address, or
`adb shell ip -f inet addr show wlan0`. The app also logs its URL at startup
(`devtools: open http://192.168.1.23:9700/ …`).

The phone and the computer must reach each other: same network, and no
client isolation (common on guest and office Wi-Fi). With LAN access anyone on
that network can read the app's logs and traffic, so turn it on only on
trusted networks, or add `.lan_token(true)`: the URL then needs a random
per-launch token, which the app logs (`http://<phone-ip>:9700/?token=…`).

## Configuration

Everything is optional:

```rust
Config::new("my-app")
    .enabled(true)                         // default: debug builds only
    .log_tag("MyApp")                      // logcat tag, default: the app name
    .logcat_level(LevelFilter::Info)       // default Info
    .devtools_filter("info,my_app=debug")  // RUST_LOG syntax; default: Info, Debug for
                                           // the app crate (or GPUI_DEVTOOLS_LOG at build time)
    .port(9700)                            // or GPUI_DEVTOOLS_PORT at build time
    .lan(false)                            // or GPUI_DEVTOOLS_LAN=1 at build time
    .lan_token(false)                      // require ?token=… on LAN
    .max_body_bytes(512 * 1024)            // per body, default 512 KiB
    .buffer_bytes(16 * 1024 * 1024)        // history kept in the app, default 16 MiB
    .platform_logger(true)                 // false: keep your own `log` logger
```

With `platform_logger(false)`, call `gpui_mobile_devtools::log_record(record)`
from your own logger to feed the Console.

## GPUI versions

| Feature | HTTP client trait | Used by |
|---|---|---|
| `gpui-pre` (default) | `gpui-pre-http-client` 0.3 | gpui-kit, gpui-mobile |
| `gpui` | `gpui_http_client` 0.2 | Zed's `gpui` 0.2 on crates.io |

```toml
gpui-mobile-devtools = { version = "0.1", default-features = false, features = ["gpui"] }
```

With both features on, `wrap_http_client` wraps the `gpui-pre` client and
`wrap_gpui_http_client` the `gpui` one.

Any other HTTP client can report requests through
`gpui_mobile_devtools::network` (see its docs).

## Try it without a phone

```sh
cargo run --example demo          # fake app with logs and requests
```

Then open <http://127.0.0.1:9700>.

## How it works

```text
 phone: app process (debug build)                          computer
 ┌──────────────────────────────────────────┐
 │ log::*!() ─► Logger ─► logcat            │
 │                 │                        │
 │ GPUI HttpClient wrapper (API + img())    │
 │                 │                        │
 │                 ▼                        │   adb forward tcp:9700
 │ Hub: numbered events, 16 MiB history,    │   or http://<phone-ip>:9700
 │      one queue per viewer                │ ◄───────────────────── browser
 │                 │                        │     GET /    viewer page
 │ Server :9700 ◄──┘                        │     GET /ws  event stream
 └──────────────────────────────────────────┘
```

The app is the whole devtools backend: `init` starts a small server thread
in it, on the phone's loopback port 9700 (all interfaces with `lan`). That one
port serves the viewer page (`/`), the session info (`/hello`) and a
WebSocket (`/ws`) that sends a `hello`, the buffered history, then events as
they happen. The computer only needs to reach that port: an adb forward, or
the phone's IP on the same network. The wire format is in
[PROTOCOL.md](PROTOCOL.md). WebSocket connections from other web pages open in
the same browser are refused (Origin check).

Logs go through the `log` facade. Requests go through a wrapper around GPUI's
`HttpClient`; response bodies are copied while the app reads them (up to
`max_body_bytes`; images, video, audio, fonts and archives are only counted).

## License

MIT or Apache-2.0, at your option.
