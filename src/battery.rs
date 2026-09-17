//! The device's battery, read from sysfs for the header's indicator.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const SUPPLIES: &str = "/sys/class/power_supply";
const REFRESH: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Reading {
    pub percent: u8,
    /// On external power, whether still charging or full.
    pub charging: bool,
}

pub struct Battery {
    dir: Option<PathBuf>,
    reading: Option<Reading>,
    read_at: Instant,
}

impl Battery {
    pub fn new() -> Self {
        let dir = find(Path::new(SUPPLIES));
        let reading = dir.as_deref().and_then(read);
        Self {
            dir,
            reading,
            read_at: Instant::now(),
        }
    }

    /// `None` on a device without a battery.
    pub fn reading(&mut self) -> Option<Reading> {
        if self.read_at.elapsed() >= REFRESH {
            self.reading = self.dir.as_deref().and_then(read);
            self.read_at = Instant::now();
        }
        self.reading
    }
}

fn find(supplies: &Path) -> Option<PathBuf> {
    let mut dirs: Vec<_> = std::fs::read_dir(supplies)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .collect();
    dirs.sort();
    dirs.into_iter().find(|dir| {
        // Scope "Device" is a mouse's or controller's battery.
        field(dir, "type").as_deref() == Some("Battery")
            && field(dir, "scope").as_deref() != Some("Device")
            && read(dir).is_some()
    })
}

fn read(dir: &Path) -> Option<Reading> {
    let percent: u8 = field(dir, "capacity")?.parse().ok()?;
    let status = field(dir, "status").unwrap_or_default();
    Some(Reading {
        percent: percent.min(100),
        charging: matches!(status.as_str(), "Charging" | "Full"),
    })
}

fn field(dir: &Path, name: &str) -> Option<String> {
    let text = std::fs::read_to_string(dir.join(name)).ok()?;
    Some(text.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn supply(root: &Path, name: &str, fields: &[(&str, &str)]) {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        for (field, value) in fields {
            std::fs::write(dir.join(field), format!("{value}\n")).unwrap();
        }
    }

    #[test]
    fn finds_the_system_battery() {
        let root = std::env::temp_dir().join(format!("zitch-battery-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        supply(&root, "axp2202-usb", &[("type", "USB"), ("online", "1")]);
        supply(
            &root,
            "hidpp_battery_0",
            &[("type", "Battery"), ("scope", "Device"), ("capacity", "50")],
        );
        supply(
            &root,
            "axp2202-battery",
            &[
                ("type", "Battery"),
                ("capacity", "67"),
                ("status", "Discharging"),
            ],
        );
        let dir = find(&root).unwrap();
        assert!(dir.ends_with("axp2202-battery"));
        assert_eq!(
            read(&dir),
            Some(Reading {
                percent: 67,
                charging: false
            })
        );
        std::fs::write(dir.join("status"), "Full\n").unwrap();
        assert!(read(&dir).unwrap().charging);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn no_supplies_is_no_battery() {
        assert_eq!(find(Path::new("/nonexistent/power_supply")), None);
    }
}
