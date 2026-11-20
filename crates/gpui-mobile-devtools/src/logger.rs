//! The `log` logger: writes to the platform log (logcat on Android, stderr
//! elsewhere) and forwards records to the viewer.

use crate::Config;
use log::{LevelFilter, Log, Metadata, Record};
use serde_json::json;
use std::sync::OnceLock;

/// Targets never forwarded to the viewer: the WebSocket library and this
/// crate's server log while sending events, which would feed back into
/// themselves.
const QUIET_TARGETS: &[&str] = &["tungstenite", "gpui_mobile_devtools::server"];

/// Which records go to the viewer, from a `RUST_LOG`-style string such as
/// `info,my_app=debug,my_app::net=trace`: a default level, then levels for
/// targets (a module path and everything under it). The most specific
/// target wins.
#[derive(Debug, PartialEq)]
pub(crate) struct Filter {
    default: LevelFilter,
    /// Longest prefix first.
    targets: Vec<(String, LevelFilter)>,
}

impl Filter {
    pub(crate) fn parse(spec: &str) -> Self {
        let mut filter = Filter {
            default: LevelFilter::Off,
            targets: Vec::new(),
        };
        for directive in spec.split(',').map(str::trim).filter(|d| !d.is_empty()) {
            match directive.split_once('=') {
                Some((target, level)) => {
                    if let Ok(level) = level.trim().parse() {
                        filter.targets.push((target.trim().to_owned(), level));
                    }
                }
                None => match directive.parse() {
                    Ok(level) => filter.default = level,
                    // A bare target means everything from it.
                    Err(_) => filter.targets.push((directive.to_owned(), LevelFilter::Trace)),
                },
            }
        }
        filter.targets.sort_by_key(|(target, _)| std::cmp::Reverse(target.len()));
        filter
    }

    pub(crate) fn level_for(&self, target: &str) -> LevelFilter {
        self.targets
            .iter()
            .find(|(prefix, _)| {
                target == prefix
                    || target
                        .strip_prefix(prefix.as_str())
                        .is_some_and(|rest| rest.starts_with("::"))
            })
            .map_or(self.default, |(_, level)| *level)
    }

    fn max(&self) -> LevelFilter {
        self.targets
            .iter()
            .map(|(_, level)| *level)
            .fold(self.default, Ord::max)
    }
}

static FILTER: OnceLock<Filter> = OnceLock::new();

struct Logger {
    platform: Option<Box<dyn Log>>,
    platform_level: LevelFilter,
}

impl Log for Logger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= self.platform_level
            || FILTER
                .get()
                .is_some_and(|filter| metadata.level() <= filter.level_for(metadata.target()))
    }

    fn log(&self, record: &Record) {
        if record.level() <= self.platform_level {
            if let Some(platform) = &self.platform {
                platform.log(record);
            }
        }
        forward(record);
    }

    fn flush(&self) {
        if let Some(platform) = &self.platform {
            platform.flush();
        }
    }
}

pub(crate) fn install(config: &Config) {
    let tag = config.log_tag.clone().unwrap_or_else(|| config.app_name.clone());
    let filter = Filter::parse(if config.enabled { &config.devtools_filter } else { "off" });
    let max = config.logcat_level.max(filter.max());
    let _ = FILTER.set(filter);
    if config.platform_logger {
        let logger = Logger {
            platform: Some(platform_logger(&tag, config.logcat_level)),
            platform_level: config.logcat_level,
        };
        if log::set_boxed_logger(Box::new(logger)).is_ok() {
            log::set_max_level(max);
        }
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
    let wanted = FILTER
        .get()
        .is_some_and(|filter| record.level() <= filter.level_for(target));
    if !wanted || QUIET_TARGETS.iter().any(|quiet| target.starts_with(quiet)) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn most_specific_target_wins() {
        let filter = Filter::parse("info, my_app=debug, my_app::net=trace, noisy=off");
        assert_eq!(filter.level_for("wgpu_core::device"), LevelFilter::Info);
        assert_eq!(filter.level_for("my_app"), LevelFilter::Debug);
        assert_eq!(filter.level_for("my_app::home"), LevelFilter::Debug);
        assert_eq!(filter.level_for("my_app::net::http"), LevelFilter::Trace);
        assert_eq!(filter.level_for("my_app_extra"), LevelFilter::Info);
        assert_eq!(filter.level_for("noisy::x"), LevelFilter::Off);
        assert_eq!(filter.max(), LevelFilter::Trace);
    }

    #[test]
    fn bare_level_and_bare_target() {
        assert_eq!(Filter::parse("debug").level_for("anything"), LevelFilter::Debug);
        let filter = Filter::parse("warn,my_app");
        assert_eq!(filter.level_for("my_app::x"), LevelFilter::Trace);
        assert_eq!(filter.level_for("other"), LevelFilter::Warn);
    }
}
