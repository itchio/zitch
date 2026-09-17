//! What kind of device this is, for the sign-in to report when the user
//! allows it. Best effort: anything unknown is left out, nothing here
//! can fail the sign-in.

use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Map, Value, json};

use crate::muos;

/// The screen the interface last drew to, width in the high word, height
/// in the low, or 0 before the first frame. Set by the interface thread,
/// read by the backend's.
static RESOLUTION: AtomicU64 = AtomicU64::new(0);

/// Records the screen size the interface is laid out for, in pixels.
pub fn set_resolution(width: f32, height: f32) {
    if width <= 0.0 || height <= 0.0 {
        return;
    }
    let packed = ((width.round() as u64) << 32) | (height.round() as u64 & 0xffff_ffff);
    RESOLUTION.store(packed, Ordering::Relaxed);
}

fn resolution() -> Option<String> {
    let packed = RESOLUTION.load(Ordering::Relaxed);
    (packed != 0).then(|| format!("{}x{}", packed >> 32, packed & 0xffff_ffff))
}

/// A compact JSON object describing this device, for `device_info`.
pub fn gather() -> String {
    let mut info = Map::new();
    let on_muos = muos::available();
    info.insert(
        "platform".into(),
        json!(if on_muos { "muos" } else { "desktop" }),
    );
    info.insert("os".into(), json!(std::env::consts::OS));
    info.insert("arch".into(), json!(std::env::consts::ARCH));
    if on_muos {
        if let Some(board) = muos::board() {
            info.insert("board".into(), json!(board));
        }
        info.insert("release".into(), json!(muos::release().name()));
        let (major, minor) = muos::glibc_version();
        info.insert("glibc".into(), json!(format!("{major}.{minor}")));
    }
    if let Some(resolution) = resolution() {
        info.insert("resolution".into(), json!(resolution));
    }
    Value::Object(info).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    // One test: they share the resolution static.
    #[test]
    fn gathers_what_is_known() {
        RESOLUTION.store(0, Ordering::Relaxed);
        let info: Value = serde_json::from_str(&gather()).unwrap();
        assert!(info.get("resolution").is_none());
        set_resolution(0.0, 480.0);
        assert_eq!(resolution(), None);

        set_resolution(640.0, 480.0);
        let info: Value = serde_json::from_str(&gather()).unwrap();
        assert_eq!(info["os"], std::env::consts::OS);
        assert_eq!(info["arch"], std::env::consts::ARCH);
        assert_eq!(info["resolution"], "640x480");
        assert!(info["platform"] == "desktop" || info["platform"] == "muos");
        // Off-device the muOS fields stay out rather than default.
        if info["platform"] == "desktop" {
            assert!(info.get("board").is_none());
            assert!(info.get("release").is_none());
            assert!(info.get("glibc").is_none());
        }
    }
}
