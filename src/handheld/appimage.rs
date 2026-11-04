//! AppImages. The runtime at the front of one mounts the rest with
//! FUSE 2, which the firmware does not have, so the image is unpacked
//! once next to itself (its own `--appimage-extract` does that without
//! FUSE) and the `AppRun` inside is launched in its place.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::SystemTime;

use anyhow::{Context, Result, bail};

const UNPACKED_SUFFIX: &str = ".d";

pub fn is_appimage(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("AppImage"))
}

/// Where `appimage` unpacks to: a sibling with `.d` added to its name.
pub fn unpacked_dir(appimage: &Path) -> PathBuf {
    let mut name = OsString::from(appimage.as_os_str());
    name.push(UNPACKED_SUFFIX);
    PathBuf::from(name)
}

/// The AppImage an unpacked tree came from, given a path inside it.
/// The `AppRun` at the tree's root is what runs in the image's place.
pub fn unpacked_from(path: &Path) -> Option<(PathBuf, bool)> {
    for dir in path.ancestors().skip(1) {
        let name = dir.file_name()?.to_str()?;
        if let Some(stem) = name.strip_suffix(UNPACKED_SUFFIX)
            && is_appimage(Path::new(stem))
        {
            let is_run =
                path.parent() == Some(dir) && path.file_name().is_some_and(|f| f == "AppRun");
            return Some((dir.with_file_name(stem), is_run));
        }
    }
    None
}

/// Unpacks `appimage` beside itself unless a tree at least as new as it
/// is already there. The extraction goes to a work directory first so a
/// half-done one is never taken for finished.
pub fn unpack(appimage: &Path) -> Result<PathBuf> {
    let dir = unpacked_dir(appimage);
    let modified = |p: &Path| fs::metadata(p).and_then(|m| m.modified()).ok();
    if let (Some(have), Some(want)) = (modified(&dir), modified(appimage))
        && have >= want
    {
        return Ok(dir);
    }
    let parent = appimage
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .with_context(|| format!("{} has no parent", appimage.display()))?;
    let name = appimage
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default();
    let work = parent.join(format!(".{name}.unpacking"));
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).with_context(|| format!("creating {}", work.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(appimage)?.permissions().mode();
        if mode & 0o100 == 0 {
            fs::set_permissions(appimage, fs::Permissions::from_mode(mode | 0o755))?;
        }
    }
    let output = Command::new(appimage)
        .arg("--appimage-extract")
        .current_dir(&work)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("running {name}"))?;
    if !output.status.success() {
        let _ = fs::remove_dir_all(&work);
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "{name} --appimage-extract: {}: {}",
            output.status,
            stderr.lines().last().unwrap_or("").trim()
        );
    }
    let _ = fs::remove_dir_all(&dir);
    fs::rename(work.join("squashfs-root"), &dir)
        .with_context(|| format!("moving the unpacked tree to {}", dir.display()))?;
    let _ = fs::remove_dir_all(&work);
    // The tree keeps the image's own timestamps; the directory's marks
    // when it was unpacked, which is what the check above compares.
    if let Ok(dir_file) = fs::File::open(&dir) {
        let _ = dir_file.set_modified(SystemTime::now());
    }
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpacked_paths_name_their_image() {
        let image = Path::new("/games/x/Game-1.0-aarch64.AppImage");
        assert!(is_appimage(image));
        assert!(!is_appimage(Path::new("/games/x/Game")));
        let dir = unpacked_dir(image);
        assert_eq!(dir, Path::new("/games/x/Game-1.0-aarch64.AppImage.d"));
        assert_eq!(
            unpacked_from(&dir.join("AppRun")),
            Some((image.to_path_buf(), true))
        );
        assert_eq!(
            unpacked_from(&dir.join("usr/bin/Game")),
            Some((image.to_path_buf(), false))
        );
        assert_eq!(
            unpacked_from(&dir.join("x/AppRun")),
            Some((image.to_path_buf(), false))
        );
        assert_eq!(unpacked_from(image), None);
        assert_eq!(unpacked_from(Path::new("/games/x/other.d/AppRun")), None);
    }
}
