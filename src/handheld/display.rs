//! The screen handed to a game and back. Where SDL draws through KMS,
//! only the DRM master may flip frames onto a panel, and the first
//! process to open the card while there is none becomes it. zitch's
//! window stays open behind a game, so on those firmwares it lets go of
//! master for the length of a launch: the game's SDL opens the card and
//! takes it, and zitch takes it back once the game is gone.

use std::os::fd::RawFd;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

/// `DRM_IOCTL_SET_MASTER` and `DRM_IOCTL_DROP_MASTER`: `_IO('d', 0x1e)`
/// and `_IO('d', 0x1f)`.
const SET_MASTER: libc::c_ulong = 0x641e;
const DROP_MASTER: libc::c_ulong = 0x641f;

/// Our open DRM card nodes: the one SDL's KMSDRM driver opened.
fn card_fds() -> Vec<RawFd> {
    std::fs::read_dir("/proc/self/fd")
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let target = std::fs::read_link(entry.path()).ok()?;
            is_card(&target).then(|| entry.file_name().to_str()?.parse().ok())?
        })
        .collect()
}

fn is_card(path: &Path) -> bool {
    path.to_str()
        .and_then(|p| p.strip_prefix("/dev/dri/card"))
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

fn master(request: libc::c_ulong, what: &str) {
    for fd in card_fds() {
        // SAFETY: a request without an argument on a descriptor we hold;
        // a descriptor that isn't (or is already) master just fails.
        let result = unsafe { libc::ioctl(fd, request as _, 0) };
        if result == 0 {
            log::info!("{what} DRM master on fd {fd}");
        } else {
            log::debug!(
                "{what} DRM master on fd {fd}: {}",
                std::io::Error::last_os_error()
            );
        }
    }
}

/// Held for a launch; the screen is zitch's again at [`Handover::end`],
/// or when it drops.
#[must_use]
pub struct Handover {
    released: AtomicBool,
}

impl Handover {
    pub(super) fn begin(release: bool) -> Self {
        if release {
            master(DROP_MASTER, "dropped");
        }
        Handover {
            released: AtomicBool::new(release),
        }
    }

    /// Takes the screen back, before zitch draws again. Once.
    pub fn end(&self) {
        if self.released.swap(false, Ordering::SeqCst) {
            master(SET_MASTER, "took");
        }
    }
}

impl Drop for Handover {
    fn drop(&mut self) {
        self.end();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn card_nodes_only() {
        assert!(is_card(Path::new("/dev/dri/card0")));
        assert!(is_card(Path::new("/dev/dri/card12")));
        assert!(!is_card(Path::new("/dev/dri/renderD128")));
        assert!(!is_card(Path::new("/dev/dri/card")));
        assert!(!is_card(Path::new("/dev/dri/card0 (deleted)")));
        assert!(!is_card(Path::new("/dev/null")));
    }
}
