//! Updating zitch itself on muOS: find a newer GitHub release and put its
//! .muxapp where the Archive Manager looks. Installing is left to the user.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};

use sha2::{Digest, Sha256};

const LATEST_URL: &str = "https://api.github.com/repos/itchio/zitch/releases/latest";
const USER_AGENT: &str = concat!("zitch/", env!("ZITCH_VERSION"));
const ARCHIVE_DIR: &str = "/mnt/mmc/ARCHIVE";
const ASSET_SUFFIX: &str = "-muos.muxapp";
const SUMS_NAME: &str = "SHA256SUMS";
/// Downloads go here instead, which also turns updating on off-device.
const DIR_OVERRIDE: &str = "ZITCH_SELF_UPDATE_DIR";

#[derive(Debug, Clone, PartialEq)]
pub struct Release {
    pub version: String,
    pub file_name: String,
    url: String,
    pub size: u64,
    sums_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum State {
    Idle,
    Checking,
    UpToDate,
    Failed(String),
    Available(Release),
    Downloading { release: Release, done: u64 },
    Ready(Release),
}

enum Msg {
    Checked(Result<Option<Release>, String>),
    Progress(u64),
    Downloaded(Result<(), String>),
}

pub struct SelfUpdate {
    state: State,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    ctx: egui::Context,
}

impl SelfUpdate {
    pub fn new(ctx: &egui::Context) -> Self {
        let (tx, rx) = channel();
        Self {
            state: State::Idle,
            tx,
            rx,
            ctx: ctx.clone(),
        }
    }

    /// Only muOS has an archive folder to download into.
    pub fn supported() -> bool {
        crate::muos::available() || std::env::var_os(DIR_OVERRIDE).is_some()
    }

    pub fn state(&self) -> &State {
        &self.state
    }

    pub fn dismiss(&mut self) {
        if matches!(self.state, State::UpToDate | State::Failed(_)) {
            self.state = State::Idle;
        }
    }

    pub fn check(&mut self) {
        if !matches!(self.state, State::Idle | State::UpToDate | State::Failed(_)) {
            return;
        }
        self.state = State::Checking;
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(Msg::Checked(check(&current_version())));
            ctx.request_repaint();
        });
    }

    pub fn download(&mut self) {
        let State::Available(release) = &self.state else {
            return;
        };
        let release = release.clone();
        self.state = State::Downloading {
            release: release.clone(),
            done: 0,
        };
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let dir = std::env::var_os(DIR_OVERRIDE).map_or(ARCHIVE_DIR.into(), PathBuf::from);
            let result = download(&release, &dir, |done| {
                let _ = tx.send(Msg::Progress(done));
                ctx.request_repaint();
            });
            let _ = tx.send(Msg::Downloaded(result));
            ctx.request_repaint();
        });
    }

    /// True when a download just finished.
    pub fn poll(&mut self) -> bool {
        let mut finished = false;
        while let Ok(msg) = self.rx.try_recv() {
            match (msg, std::mem::replace(&mut self.state, State::Idle)) {
                (Msg::Checked(Ok(Some(release))), _) => self.state = State::Available(release),
                (Msg::Checked(Ok(None)), _) => self.state = State::UpToDate,
                (Msg::Checked(Err(error)), _) => {
                    log::warn!("zitch update check failed: {error}");
                    self.state = State::Failed(format!("Couldn't check for an update: {error}"));
                }
                (Msg::Progress(done), State::Downloading { release, .. }) => {
                    self.state = State::Downloading { release, done };
                }
                (Msg::Downloaded(Ok(())), State::Downloading { release, .. }) => {
                    finished = true;
                    self.state = State::Ready(release);
                }
                (Msg::Downloaded(Err(error)), State::Downloading { .. }) => {
                    log::warn!("zitch update download failed: {error}");
                    self.state = State::Failed(format!("Couldn't download the update: {error}"));
                }
                (_, state) => self.state = state,
            }
        }
        finished
    }
}

/// ZITCH_SELF_UPDATE_VERSION stands in for the running version, to test
/// against a real release.
fn current_version() -> String {
    std::env::var("ZITCH_SELF_UPDATE_VERSION").unwrap_or_else(|_| env!("ZITCH_VERSION").to_string())
}

fn get(url: &str) -> Result<ureq::http::Response<ureq::Body>, String> {
    ureq::get(url)
        .header("User-Agent", USER_AGENT)
        .call()
        .map_err(|e| e.to_string())
}

fn check(current: &str) -> Result<Option<Release>, String> {
    let body = get(LATEST_URL)?
        .body_mut()
        .read_to_string()
        .map_err(|e| e.to_string())?;
    newer_release(&body, current)
}

fn newer_release(json: &str, current: &str) -> Result<Option<Release>, String> {
    #[derive(serde::Deserialize)]
    struct Asset {
        name: String,
        size: u64,
        browser_download_url: String,
    }
    #[derive(serde::Deserialize)]
    struct Latest {
        tag_name: String,
        assets: Vec<Asset>,
    }
    let latest: Latest = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let version = latest.tag_name.trim_start_matches('v');
    let Some(found) = parse_version(version) else {
        return Err(format!("unexpected release tag {}", latest.tag_name));
    };
    // A development build has no place in the order, so it is offered
    // the latest release.
    if parse_version(current).is_some_and(|running| running >= found) {
        return Ok(None);
    }
    // The release exists before CI has uploaded its files.
    let Some(asset) = latest
        .assets
        .iter()
        .find(|a| a.name.ends_with(ASSET_SUFFIX))
    else {
        return Ok(None);
    };
    // The name becomes a path in the archive folder.
    if asset.name.contains(['/', '\\']) {
        return Err(format!("unexpected file name {}", asset.name));
    }
    Ok(Some(Release {
        version: version.to_string(),
        file_name: asset.name.clone(),
        url: asset.browser_download_url.clone(),
        size: asset.size,
        sums_url: latest
            .assets
            .iter()
            .find(|a| a.name == SUMS_NAME)
            .map(|a| a.browser_download_url.clone()),
    }))
}

/// A plain `x.y.z`; anything else is a development build.
fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let mut parts = text.split('.').map(|part| part.parse().ok());
    let version = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(version)
}

fn listed_sum<'a>(sums: &'a str, file_name: &str) -> Option<&'a str> {
    sums.lines().find_map(|line| {
        let (sum, name) = line.split_once(char::is_whitespace)?;
        (name.trim_start().trim_start_matches('*') == file_name).then_some(sum)
    })
}

fn download(release: &Release, dir: &Path, progress: impl Fn(u64)) -> Result<(), String> {
    let expected = match &release.sums_url {
        Some(url) => {
            let sums = get(url)?
                .body_mut()
                .read_to_string()
                .map_err(|e| e.to_string())?;
            Some(
                listed_sum(&sums, &release.file_name)
                    .ok_or("the release has no checksum for it")?
                    .to_ascii_lowercase(),
            )
        }
        None => None,
    };

    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let target = dir.join(&release.file_name);
    // The Archive Manager lists .muxapp files, so a partial one is kept
    // under another name.
    let partial: PathBuf = dir.join(format!("{}.part", release.file_name));
    let result = fetch(release, &partial, expected.as_deref(), progress)
        .and_then(|()| std::fs::rename(&partial, &target).map_err(|e| e.to_string()));
    if result.is_err() {
        let _ = std::fs::remove_file(&partial);
    }
    result
}

fn fetch(
    release: &Release,
    partial: &Path,
    expected: Option<&str>,
    progress: impl Fn(u64),
) -> Result<(), String> {
    let mut reader = get(&release.url)?.into_body().into_reader();
    let mut file = std::fs::File::create(partial).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    let (mut done, mut last) = (0u64, 0u8);
    loop {
        let count = reader.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        file.write_all(&buffer[..count])
            .map_err(|e| e.to_string())?;
        hasher.update(&buffer[..count]);
        done += count as u64;
        let percent = (done * 100 / release.size.max(1)).min(100) as u8;
        if percent != last {
            last = percent;
            progress(done);
        }
    }
    file.sync_all().map_err(|e| e.to_string())?;
    if done != release.size {
        return Err(format!("got {done} of {} bytes", release.size));
    }
    let sum: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if expected.is_some_and(|expected| expected != sum) {
        return Err("the download doesn't match its checksum".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn latest(tag: &str, assets: &[&str]) -> String {
        let assets: Vec<_> = assets
            .iter()
            .map(|name| {
                serde_json::json!({
                    "name": name,
                    "size": 10,
                    "browser_download_url": format!("https://example.com/{name}"),
                })
            })
            .collect();
        serde_json::json!({ "tag_name": tag, "assets": assets }).to_string()
    }

    #[test]
    fn versions() {
        assert_eq!(parse_version("0.1.0"), Some((0, 1, 0)));
        assert!(parse_version("0.10.0") > parse_version("0.9.0"));
        assert_eq!(parse_version("0.1.0-3-gabc1234"), None);
        assert_eq!(parse_version("abc1234"), None);
        assert_eq!(parse_version("0.1"), None);
        assert_eq!(parse_version("0.1.0.1"), None);
    }

    #[test]
    fn offers_newer_only() {
        let json = latest("v0.2.0", &["zitch-0.2.0-muos.muxapp", "SHA256SUMS"]);
        let release = newer_release(&json, "0.1.0").unwrap().unwrap();
        assert_eq!(release.version, "0.2.0");
        assert_eq!(release.file_name, "zitch-0.2.0-muos.muxapp");
        assert!(release.sums_url.is_some());
        assert_eq!(newer_release(&json, "0.2.0").unwrap(), None);
        assert_eq!(newer_release(&json, "0.3.0").unwrap(), None);
    }

    #[test]
    fn development_build_is_offered_latest() {
        let json = latest("v0.2.0", &["zitch-0.2.0-muos.muxapp"]);
        assert!(newer_release(&json, "abc1234").unwrap().is_some());
    }

    #[test]
    fn release_without_muxapp_is_not_offered() {
        let json = latest("v0.2.0", &["zitch-0.2.0-linux-x64.tar.gz"]);
        assert_eq!(newer_release(&json, "0.1.0").unwrap(), None);
    }

    #[test]
    fn rejects_path_in_file_name() {
        let json = latest("v0.2.0", &["../zitch-0.2.0-muos.muxapp"]);
        assert!(newer_release(&json, "0.1.0").is_err());
    }

    #[test]
    fn finds_listed_sum() {
        let sums = "aaa  zitch-0.2.0-linux-x64.tar.gz\nbbb  zitch-0.2.0-muos.muxapp\nccc *other\n";
        assert_eq!(listed_sum(sums, "zitch-0.2.0-muos.muxapp"), Some("bbb"));
        assert_eq!(listed_sum(sums, "other"), Some("ccc"));
        assert_eq!(listed_sum(sums, "missing"), None);
    }
}
