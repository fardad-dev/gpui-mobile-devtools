//! Building and publishing events. The wire format is documented in
//! `PROTOCOL.md` at the repository root.

use serde_json::{json, Value};
use std::hash::{BuildHasher, Hasher};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

/// Milliseconds since the Unix epoch.
pub(crate) fn now_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs_f64() * 1000.0)
        .unwrap_or(0.0)
}

/// 64 random bits as hex, from the standard library's randomly seeded hasher.
pub(crate) fn random_hex() -> String {
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u128(now_ms().to_bits() as u128 ^ std::process::id() as u128);
    format!("{:016x}", hasher.finish())
}

/// Identifies this run of the app, so the viewer can tell a reconnect from
/// a restart.
pub(crate) fn session_id() -> &'static str {
    static SESSION: OnceLock<String> = OnceLock::new();
    SESSION.get_or_init(random_hex)
}

/// Publishes `event` if devtools are running.
pub(crate) fn publish(event: Value) {
    let Some(hub) = crate::hub::get() else {
        return;
    };
    if let Ok(json) = serde_json::to_string(&event) {
        hub.publish(&json);
    }
}

/// The first event of every session.
pub(crate) fn hello_event() -> Value {
    let app = crate::config().map(|config| config.app_name.as_str()).unwrap_or("app");
    json!({
        "type": "hello",
        "protocol": crate::PROTOCOL_VERSION,
        "session": session_id(),
        "app": app,
        "platform": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "pid": std::process::id(),
        "devtools": env!("CARGO_PKG_VERSION"),
        "ts": now_ms(),
    })
}
