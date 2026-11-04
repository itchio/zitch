//! spruceOS: detected by the environment its launch script sources,
//! so a spruce card mounted on a desktop is not one. Games are our LÖVE
//! and Linux builds.

use std::path::Path;

use super::*;

/// What spruce's `appEnv.sh` sets for third-party apps, which our
/// launch script sources.
const APP_ENV: &str = "/mnt/SDCARD/spruce/scripts/appEnv.sh";

pub(super) struct Spruce;

static SPRUCE: Spruce = Spruce;

pub(super) fn detect() -> Option<&'static dyn Firmware> {
    let sourced = std::env::var_os("CFW").is_some_and(|cfw| cfw == "SPRUCE");
    (sourced && Path::new(APP_ENV).is_file()).then_some(&SPRUCE as &dyn Firmware)
}

impl Firmware for Spruce {
    fn name(&self) -> &'static str {
        "spruce"
    }

    /// `appEnv.sh` names the model, e.g. `BrickPro`.
    fn details(&self) -> Vec<(&'static str, String)> {
        std::env::var("SPRUCE_PLATFORM")
            .ok()
            .map(|board| ("board", board))
            .into_iter()
            .collect()
    }
}
