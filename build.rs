use std::process::Command;

/// The version shown by --version and the drawer: the build environment's
/// ZITCH_VERSION, else `git describe`, else the crate version.
fn main() {
    println!("cargo::rerun-if-env-changed=ZITCH_VERSION");
    for path in [".git/HEAD", ".git/packed-refs", ".git/refs/tags"] {
        if std::path::Path::new(path).exists() {
            println!("cargo::rerun-if-changed={path}");
        }
    }
    let version = std::env::var("ZITCH_VERSION")
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(git_describe)
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
    println!("cargo::rustc-env=ZITCH_VERSION={version}");
}

fn git_describe() -> Option<String> {
    let output = Command::new("git")
        .args(["describe", "--tags", "--always", "--dirty"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let text = text.trim();
    let text = text.strip_prefix('v').unwrap_or(text);
    (!text.is_empty()).then(|| text.to_string())
}
