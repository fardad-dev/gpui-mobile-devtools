//! Console and Network devtools for GPUI apps on phones.
//!
//! The app streams its logs and HTTP traffic to a viewer in your desktop
//! browser. Setup in the app is two calls:
//!
//! ```ignore
//! // Where you used to call `android_logger::init_once(...)`:
//! gpui_mobile_devtools::init(gpui_mobile_devtools::Config::new("my-app"));
//!
//! // Where you hand GPUI its HTTP client:
//! let client = gpui_mobile_devtools::wrap_http_client(Arc::new(reqwest_client));
//! app.with_http_client(client)
//! ```
//!
//! Then open the viewer the app serves, in any browser on your computer:
//!
//! * **Over adb.** `adb forward tcp:9700 tcp:9700`, then open
//!   <http://127.0.0.1:9700>. The server listens on the phone's loopback only.
//! * **Over Wi-Fi, by the phone's IP.** Turn on [`Config::lan`] (or build with
//!   `GPUI_DEVTOOLS_LAN=1`), then open `http://<phone-ip>:9700` from a computer
//!   on the same network. The phone's IP is in Settings → About phone → Status
//!   (or `adb shell ip -f inet addr show wlan0`); the app also logs its URL.
//!
//! The app keeps a buffer of recent events (16 MiB by default), so a viewer
//! that connects late still sees the app's startup.
//!
//! # Release builds
//!
//! By default devtools are on only when `debug_assertions` are on. When off,
//! [`init`] installs just the platform logger (logcat on Android, stderr
//! elsewhere) and [`wrap_http_client`] returns the client unchanged.
//!
//! # Other HTTP stacks
//!
//! The [`network`] module reports requests from any HTTP client;
//! [`wrap_http_client`] is built on it.

mod event;
mod hub;
mod logger;
pub mod network;
mod server;

#[cfg(any(feature = "gpui-pre", feature = "gpui"))]
mod http_client;

#[cfg(feature = "gpui-pre")]
pub use http_client::gpui_pre::wrap_http_client;
#[cfg(all(feature = "gpui", not(feature = "gpui-pre")))]
pub use http_client::gpui::wrap_http_client;
#[cfg(feature = "gpui")]
pub use http_client::gpui::wrap_http_client as wrap_gpui_http_client;

use log::LevelFilter;
use std::sync::OnceLock;

/// The viewer page, a single self-contained HTML file the app serves at `/`.
pub(crate) const VIEWER_HTML: &str = include_str!("../viewer/index.html");

/// Version of the event stream sent over the WebSocket. See `PROTOCOL.md`.
pub const PROTOCOL_VERSION: u32 = 1;

/// The port the app listens on unless configured otherwise.
pub const DEFAULT_PORT: u16 = 9700;

/// Devtools settings. Every field has a default; usually only the app name
/// is given.
#[derive(Clone, Debug)]
pub struct Config {
    pub(crate) app_name: String,
    pub(crate) log_tag: Option<String>,
    pub(crate) enabled: bool,
    pub(crate) logcat_level: LevelFilter,
    pub(crate) devtools_filter: String,
    pub(crate) port: u16,
    pub(crate) lan: bool,
    pub(crate) lan_token: bool,
    pub(crate) max_body_bytes: usize,
    pub(crate) buffer_bytes: usize,
    pub(crate) platform_logger: bool,
}

impl Config {
    /// Settings for an app called `app_name`. The name is shown in the
    /// viewer and is the logcat tag.
    pub fn new(app_name: impl Into<String>) -> Self {
        let port = option_env!("GPUI_DEVTOOLS_PORT")
            .and_then(|port| port.parse().ok())
            .unwrap_or(DEFAULT_PORT);
        let lan = matches!(option_env!("GPUI_DEVTOOLS_LAN"), Some("1" | "true" | "yes"));
        let app_name: String = app_name.into();
        let devtools_filter = option_env!("GPUI_DEVTOOLS_LOG")
            .map(str::to_owned)
            .unwrap_or_else(|| format!("info,{}=debug", app_name.replace('-', "_")));
        Self {
            app_name,
            log_tag: None,
            enabled: cfg!(debug_assertions),
            logcat_level: LevelFilter::Info,
            devtools_filter,
            port,
            lan,
            lan_token: false,
            max_body_bytes: 512 * 1024,
            buffer_bytes: 16 * 1024 * 1024,
            platform_logger: true,
        }
    }

    /// Turns the server and the HTTP capture on or off. Defaults to on in
    /// debug builds (`debug_assertions`) and off in release builds.
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// The logcat tag (stderr prefix elsewhere). Defaults to the app name.
    pub fn log_tag(mut self, tag: impl Into<String>) -> Self {
        self.log_tag = Some(tag.into());
        self
    }

    /// The most verbose level written to logcat (stderr elsewhere).
    /// Defaults to `Info`.
    pub fn logcat_level(mut self, level: LevelFilter) -> Self {
        self.logcat_level = level;
        self
    }

    /// Which log records go to the viewer, as a `RUST_LOG`-style filter:
    /// `"info,my_app=debug,my_app::net=trace"`. Defaults to Info for
    /// everything and Debug for the app's own crate (the app name with `-`
    /// as `_`), or `GPUI_DEVTOOLS_LOG` at build time.
    pub fn devtools_filter(mut self, filter: impl Into<String>) -> Self {
        self.devtools_filter = filter.into();
        self
    }

    /// Sends every record at `level` and above to the viewer, from every
    /// crate. Shorthand for [`Config::devtools_filter`] with just a level.
    pub fn devtools_level(mut self, level: LevelFilter) -> Self {
        self.devtools_filter = level.to_string();
        self
    }

    /// The port the app listens on. Defaults to 9700, or `GPUI_DEVTOOLS_PORT`
    /// at build time.
    pub fn port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    /// Listen on every network interface instead of loopback only, so a
    /// browser on the same network can open `http://<phone-ip>:<port>`
    /// without adb. Anyone on that network can then read the app's logs and
    /// traffic; see [`Config::lan_token`]. Defaults to off, or
    /// `GPUI_DEVTOOLS_LAN=1` at build time.
    pub fn lan(mut self, lan: bool) -> Self {
        self.lan = lan;
        self
    }

    /// With [`Config::lan`], require a random per-launch token in the URL
    /// (`http://<phone-ip>:<port>/?token=…`). The app logs the full URL at
    /// startup. Defaults to off.
    pub fn lan_token(mut self, required: bool) -> Self {
        self.lan_token = required;
        self
    }

    /// The largest request or response body kept per request. Longer bodies
    /// are cut. Defaults to 512 KiB.
    pub fn max_body_bytes(mut self, bytes: usize) -> Self {
        self.max_body_bytes = bytes;
        self
    }

    /// How much recent history the app keeps for viewers that connect
    /// later. Defaults to 16 MiB.
    pub fn buffer_bytes(mut self, bytes: usize) -> Self {
        self.buffer_bytes = bytes;
        self
    }

    /// Whether [`init`] also installs the platform logger (logcat on
    /// Android, stderr elsewhere). Turn it off when the app installs its own
    /// `log` logger; then call [`log_record`] from it to feed the viewer.
    pub fn platform_logger(mut self, install: bool) -> Self {
        self.platform_logger = install;
        self
    }
}

pub(crate) struct State {
    pub(crate) config: Config,
}

static STATE: OnceLock<State> = OnceLock::new();

/// Whether devtools are running, i.e. [`init`] was called with devtools
/// enabled.
pub fn is_active() -> bool {
    STATE.get().is_some()
}

pub(crate) fn config() -> Option<&'static Config> {
    STATE.get().map(|state| &state.config)
}

/// Installs the logger and, when devtools are enabled, starts the server.
/// Call it once, as early as possible: on Android at the top of
/// `android_main`. Later calls do nothing.
pub fn init(config: Config) {
    static ONCE: OnceLock<()> = OnceLock::new();
    if ONCE.set(()).is_err() {
        return;
    }

    logger::install(&config);

    if !config.enabled {
        return;
    }
    hub::init(config.buffer_bytes);
    let _ = STATE.set(State {
        config: config.clone(),
    });
    server::start(&config);
}

/// Sends one `log` record to the viewer. Only needed with
/// [`Config::platform_logger`] turned off, from the app's own logger.
pub fn log_record(record: &log::Record) {
    logger::forward(record);
}
