//! The `log` logger: writes to the platform log (logcat on Android, stderr
//! elsewhere) and forwards records to the viewer.

use crate::Config;
use log::{LevelFilter, Log, Metadata, Record};
use serde_json::json;

/// Targets never forwarded to the viewer: the WebSocket library and this
/// crate's server log while sending events, which would feed back into
/// themselves.
const QUIET_TARGETS: &[&str] = &["tungstenite", "gpui_mobile_devtools::server"];

struct Logger {
    platform: Option<Box<dyn Log>>,
    platform_level: LevelFilter,
    devtools_level: LevelFilter,
}

impl Log for Logger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= self.platform_level || metadata.level() <= self.devtools_level
    }

    fn log(&self, record: &Record) {
        if record.level() <= self.platform_level {
            if let Some(platform) = &self.platform {
                platform.log(record);
            }
        }
        if record.level() <= self.devtools_level {
            forward(record);
        }
    }

    fn flush(&self) {
        if let Some(platform) = &self.platform {
            platform.flush();
        }
    }
}

pub(crate) fn install(config: &Config) {
    let tag = config.log_tag.clone().unwrap_or_else(|| config.app_name.clone());
    let devtools_level = if config.enabled {
        config.devtools_level
    } else {
        LevelFilter::Off
    };
    let logger = Logger {
        platform: Some(platform_logger(&tag, config.logcat_level)),
        platform_level: config.logcat_level,
        devtools_level,
    };
    if log::set_boxed_logger(Box::new(logger)).is_ok() {
        log::set_max_level(config.logcat_level.max(devtools_level));
    }
}

#[cfg(target_os = "android")]
fn platform_logger(tag: &str, level: LevelFilter) -> Box<dyn Log> {
    Box::new(android_logger::AndroidLogger::new(
        android_logger::Config::default()
            .with_max_level(level)
            .with_tag(tag),
    ))
}

#[cfg(not(target_os = "android"))]
fn platform_logger(tag: &str, _level: LevelFilter) -> Box<dyn Log> {
    Box::new(Stderr { tag: tag.to_owned() })
}

#[cfg(not(target_os = "android"))]
struct Stderr {
    tag: String,
}

#[cfg(not(target_os = "android"))]
impl Log for Stderr {
    fn enabled(&self, _: &Metadata) -> bool {
        true
    }

    fn log(&self, record: &Record) {
        eprintln!(
            "[{} {:5} {}] {}",
            self.tag,
            record.level(),
            record.target(),
            record.args()
        );
    }

    fn flush(&self) {}
}

/// Sends `record` to the viewer.
pub(crate) fn forward(record: &Record) {
    if crate::hub::get().is_none() {
        return;
    }
    let target = record.target();
    if QUIET_TARGETS.iter().any(|quiet| target.starts_with(quiet)) {
        return;
    }
    crate::event::publish(json!({
        "type": "log",
        "ts": crate::event::now_ms(),
        "level": record.level().as_str(),
        "target": target,
        "message": record.args().to_string(),
        "file": record.file(),
        "line": record.line(),
        "thread": std::thread::current().name(),
    }));
}
