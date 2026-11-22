//! The screen handed to a game and back. Where SDL draws through KMS,
//! only the DRM master may flip frames onto a panel, and the first
//! process to open the card while there is none becomes it. zitch's
//! window stays open behind a game, so on those firmwares it lets go of
//! master for the length of a launch: the game's SDL opens the card and
//! takes it, and zitch takes it back once the game is gone.
//!
//! What the game leaves on the panels varies: LÖVE's SDL puts back the
//! framebuffer it found, RetroArch leaves the CRTC with none, and a flip
//! onto a CRTC with no framebuffer fails (`EBUSY`) forever. So the CRTCs
//! are read before the handover and set again after it where they differ.

use std::sync::Mutex;

/// Held for a launch; the screen is zitch's again at [`Handover::end`],
/// or when it drops.
#[must_use]
pub struct Handover {
    /// What to put back, taken by the first [`Handover::end`].
    saved: Mutex<Option<kms::Saved>>,
}

impl Handover {
    pub(super) fn begin(release: bool) -> Self {
        let saved = release.then(kms::release);
        Handover {
            saved: Mutex::new(saved),
        }
    }

    /// Takes the screen back, before zitch draws again. Once.
    pub fn end(&self) {
        let saved = self.saved.lock().unwrap_or_else(|p| p.into_inner()).take();
        if let Some(saved) = saved {
            kms::reclaim(saved);
        }
    }
}

impl Drop for Handover {
    fn drop(&mut self) {
        self.end();
    }
}

#[cfg(target_os = "linux")]
mod kms {
    use std::os::fd::{AsFd, BorrowedFd, RawFd};
    use std::path::Path;

    use drm::Device as _;
    use drm::control::{Device as _, Mode, connector, crtc, framebuffer};

    /// One of SDL's open card descriptors. SDL keeps them open for the
    /// window's life, which outlasts any launch.
    struct Card(RawFd);

    impl AsFd for Card {
        fn as_fd(&self) -> BorrowedFd<'_> {
            // SAFETY: the descriptor is SDL's and stays open while zitch's
            // window does; we only borrow it for the call.
            unsafe { BorrowedFd::borrow_raw(self.0) }
        }
    }

    impl drm::Device for Card {}
    impl drm::control::Device for Card {}

    /// A CRTC as zitch left it.
    struct Crtc {
        handle: crtc::Handle,
        framebuffer: framebuffer::Handle,
        position: (u32, u32),
        mode: Mode,
        connectors: Vec<connector::Handle>,
    }

    pub(super) struct Saved {
        cards: Vec<RawFd>,
        crtcs: Vec<Crtc>,
    }

    /// Our open DRM card nodes: the one SDL's KMSDRM driver opened, and
    /// any duplicates of it the GL stack made.
    fn card_fds() -> Vec<RawFd> {
        let mut fds: Vec<RawFd> = std::fs::read_dir("/proc/self/fd")
            .into_iter()
            .flatten()
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| {
                let target = std::fs::read_link(entry.path()).ok()?;
                is_card(&target).then(|| entry.file_name().to_str()?.parse().ok())?
            })
            .collect();
        fds.sort_unstable();
        fds
    }

    pub(super) fn is_card(path: &Path) -> bool {
        path.to_str()
            .and_then(|p| p.strip_prefix("/dev/dri/card"))
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    }

    /// The CRTCs showing a framebuffer, with the connectors they drive.
    fn read_crtcs(card: &Card) -> std::io::Result<Vec<Crtc>> {
        let resources = card.resource_handles()?;
        let mut crtcs = Vec::new();
        for &handle in resources.crtcs() {
            let info = card.get_crtc(handle)?;
            let (Some(framebuffer), Some(mode)) = (info.framebuffer(), info.mode()) else {
                continue;
            };
            let connectors = resources
                .connectors()
                .iter()
                .copied()
                .filter(|&connector| {
                    card.get_connector(connector, false)
                        .ok()
                        .and_then(|c| c.current_encoder())
                        .and_then(|encoder| card.get_encoder(encoder).ok())
                        .and_then(|encoder| encoder.crtc())
                        == Some(handle)
                })
                .collect();
            crtcs.push(Crtc {
                handle,
                framebuffer,
                position: info.position(),
                mode,
                connectors,
            });
        }
        Ok(crtcs)
    }

    pub(super) fn release() -> Saved {
        let cards = card_fds();
        let crtcs = match cards.first().map(|&fd| read_crtcs(&Card(fd))) {
            Some(Ok(crtcs)) => crtcs,
            Some(Err(err)) => {
                log::warn!("reading the CRTCs: {err}");
                Vec::new()
            }
            None => Vec::new(),
        };
        for &fd in &cards {
            match Card(fd).release_master_lock() {
                Ok(()) => log::info!("dropped DRM master on fd {fd}"),
                Err(err) => log::debug!("dropping DRM master on fd {fd}: {err}"),
            }
        }
        Saved { cards, crtcs }
    }

    pub(super) fn reclaim(saved: Saved) {
        // Duplicates of one open card share its master, so the first that
        // takes it is the one to work through.
        let Some(card) = saved
            .cards
            .iter()
            .map(|&fd| Card(fd))
            .find(|card| card.acquire_master_lock().is_ok())
        else {
            log::warn!("could not take DRM master back");
            return;
        };
        log::info!("took DRM master on fd {}", card.0);
        for crtc in &saved.crtcs {
            let now = card.get_crtc(crtc.handle).ok();
            let unchanged = now.is_some_and(|now| {
                now.framebuffer() == Some(crtc.framebuffer) && now.mode() == Some(crtc.mode)
            });
            if unchanged {
                continue;
            }
            match card.set_crtc(
                crtc.handle,
                Some(crtc.framebuffer),
                crtc.position,
                &crtc.connectors,
                Some(crtc.mode),
            ) {
                Ok(()) => log::info!("put back CRTC {:?}", crtc.handle),
                Err(err) => log::warn!("putting back CRTC {:?}: {err}", crtc.handle),
            }
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod kms {
    pub(super) struct Saved;

    pub(super) fn release() -> Saved {
        Saved
    }

    pub(super) fn reclaim(_: Saved) {}
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::path::Path;

    use super::kms::is_card;

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
