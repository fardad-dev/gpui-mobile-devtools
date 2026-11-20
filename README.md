# gpui-mobile-devtools

Console and Network devtools for [GPUI](https://www.gpui.rs) apps running on a
phone. The app streams its logs and HTTP traffic to a viewer in your desktop
browser, over adb or Wi-Fi.

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

On your computer, once:

```sh
cargo install gpui-mobile-devtools-cli
```

## Use

### Over adb (USB, or adb over Wi-Fi)

```sh
gpui-mobile-devtools                  # the only attached device
gpui-mobile-devtools -s RFCX7175C8X   # pick one; $ANDROID_SERIAL works too
```

It finds `adb` (`$ADB`, the `PATH`, `$ANDROID_HOME` or the default SDK
location), runs `adb forward tcp:9600 tcp:9600`, serves the viewer at
<http://127.0.0.1:9700> and opens it. Leave it running: it restores the
forward when the phone reconnects, and the viewer reconnects when the app
restarts.

Without the CLI: `adb forward tcp:9600 tcp:9600`, then open
<http://127.0.0.1:9600> (the app serves the viewer too).

### Over Wi-Fi, without adb

Build the app with LAN access, either in code (`Config::new("my-app").lan(true)`)
or at build time:

```sh
GPUI_DEVTOOLS_LAN=1 cargo build ...
```

At startup the app logs a connection string:

```text
devtools: run `gpui-mobile-devtools ws://192.168.1.23:9600/ws?token=6f1c…` on your computer
```

Pass it to the CLI:

```sh
gpui-mobile-devtools ws://192.168.1.23:9600/ws?token=6f1c…
```

The token is random per app launch; requests without it are refused. The phone
and the computer must reach each other (same network, no client isolation). The
address is the one the phone routes Internet traffic through, so on a VPN it
may be the VPN's.

## Configuration

Everything is optional:

```rust
Config::new("my-app")
    .enabled(true)                         // default: debug builds only
    .log_tag("MyApp")                      // logcat tag, default: the app name
    .logcat_level(LevelFilter::Info)       // default Info
    .devtools_filter("info,my_app=debug")  // RUST_LOG syntax; default: Info, Debug for
                                           // the app crate (or GPUI_DEVTOOLS_LOG at build time)
    .port(9600)                            // or GPUI_DEVTOOLS_PORT at build time
    .lan(false)                            // or GPUI_DEVTOOLS_LAN=1 at build time
    .max_body_bytes(512 * 1024)            // per body, default 512 KiB
    .buffer_bytes(16 * 1024 * 1024)        // history kept in the app, default 16 MiB
    .platform_logger(true)                 // false: keep your own `log` logger
```

With `platform_logger(false)`, call `gpui_mobile_devtools::log_record(record)`
from your own logger to feed the Console.

CLI options: `gpui-mobile-devtools --help`.

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
cargo run -p gpui-mobile-devtools-cli -- 127.0.0.1:9600
```

## How it works

`init` starts a small server thread in the app, on the phone's loopback port
9600 (all interfaces with `lan`). One port serves the viewer page (`/`), the
session info (`/hello`) and a WebSocket (`/ws`) that sends a `hello`, the
buffered history, then events as they happen. The wire format is in
[PROTOCOL.md](PROTOCOL.md). WebSocket connections from other web pages open in
the same browser are refused (Origin check).

Logs go through the `log` facade. Requests go through a wrapper around GPUI's
`HttpClient`; response bodies are copied while the app reads them (up to
`max_body_bytes`; images, video, audio, fonts and archives are only counted).

## License

MIT or Apache-2.0, at your option.
