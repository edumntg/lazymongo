//! Self-update: check GitHub releases for a newer version, download the
//! platform asset, swap the running binary, and hand a restart request back
//! to `main`. Network I/O shells out to `curl` (present on macOS, Linux and
//! Windows 10+) on a plain thread, so the TUI never blocks and no HTTP
//! dependency is added; if `curl` is missing the check silently no-ops.

use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc::Sender;

use serde::{Deserialize, Serialize};

pub const REPO: &str = "edumntg/lazymongo";
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

pub enum UpdateMsg {
    /// A newer release with a binary for this platform exists; `tag` is the
    /// release's actual tag name (usually but not necessarily `v`-prefixed).
    Available {
        tag: String,
    },
    /// Binary swapped on disk; `exe` is the path to the new executable.
    Installed {
        exe: PathBuf,
    },
    InstallFailed(String),
}

/// Human version of a release tag: `v0.3.0` and `0.3.0` both -> `0.3.0`.
pub fn display_version(tag: &str) -> &str {
    tag.strip_prefix('v').unwrap_or(tag)
}

/// What `main` needs to relaunch the app after a successful update.
pub struct Restart {
    pub exe: PathBuf,
    pub resume: ResumeInfo,
}

/// View to restore after the restart, passed via the LAZYMONGO_RESUME env
/// var (never written to disk: the URI may contain credentials).
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ResumeInfo {
    pub uri: Option<String>,
    pub db: Option<String>,
    pub coll: Option<String>,
    #[serde(default)]
    pub filter: String,
    /// "json" | "table"
    #[serde(default)]
    pub view: String,
    #[serde(default)]
    pub read_only: bool,
}

pub const RESUME_ENV: &str = "LAZYMONGO_RESUME";

/// Fire-and-forget release check; sends `Available` only when the latest
/// release is strictly newer than the running version AND actually has a
/// binary for this platform (hand-made releases may ship no assets).
pub fn spawn_check(tx: Sender<UpdateMsg>) {
    std::thread::spawn(move || {
        let Some(tag) = fetch_latest_tag() else {
            return;
        };
        if !is_newer(display_version(&tag), CURRENT) {
            return;
        }
        let Ok(target) = target() else {
            return;
        };
        if curl(&["-I", &asset_url(&tag, target)]).is_err() {
            return;
        }
        let _ = tx.send(UpdateMsg::Available { tag });
    });
}

/// Download the release asset for this platform, swap the current
/// executable, and report the result.
pub fn spawn_install(tag: String, tx: Sender<UpdateMsg>) {
    std::thread::spawn(move || {
        let msg = match install(&tag) {
            Ok(exe) => UpdateMsg::Installed { exe },
            Err(e) => UpdateMsg::InstallFailed(e),
        };
        let _ = tx.send(msg);
    });
}

fn curl(args: &[&str]) -> Result<Vec<u8>, String> {
    let out = Command::new("curl")
        .args(["-fsSL", "--max-time", "120", "-A", "lazymongo"])
        .args(args)
        .output()
        .map_err(|e| format!("curl: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "curl failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(out.stdout)
}

fn fetch_latest_tag() -> Option<String> {
    let body = curl(&[&format!(
        "https://api.github.com/repos/{REPO}/releases/latest"
    )])
    .ok()?;
    let json: serde_json::Value = serde_json::from_slice(&body).ok()?;
    let tag = json.get("tag_name")?.as_str()?;
    // Non-semver tags (e.g. a hand-made "stable" release) are ignored.
    parse_version(display_version(tag))?;
    Some(tag.to_string())
}

/// Cargo target triple of the running binary, mirroring release.yml's matrix.
fn target() -> Result<&'static str, String> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        ("macos", "x86_64") => Ok("x86_64-apple-darwin"),
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-musl"),
        ("linux", "aarch64") => Ok("aarch64-unknown-linux-musl"),
        ("windows", "x86_64") => Ok("x86_64-pc-windows-msvc"),
        (os, arch) => Err(format!("no release build for {os}/{arch}")),
    }
}

fn asset_name(version: &str, target: &str) -> String {
    let ext = if target.contains("windows") {
        "zip"
    } else {
        "tar.gz"
    };
    format!("lazymongo-{version}-{target}.{ext}")
}

/// Download URL for this platform's asset in the release tagged `tag`.
/// release.yml names assets by the tag with any `v` prefix stripped.
fn asset_url(tag: &str, target: &str) -> String {
    let asset = asset_name(display_version(tag), target);
    format!("https://github.com/{REPO}/releases/download/{tag}/{asset}")
}

/// Download + extract + swap. Returns the path of the (new) executable.
fn install(tag: &str) -> Result<PathBuf, String> {
    let target = target()?;
    let version = display_version(tag);
    let asset = asset_name(version, target);
    let url = asset_url(tag, target);

    let work = std::env::temp_dir().join(format!("lazymongo-update-{version}"));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let archive = work.join(&asset);
    curl(&["-o", &archive.to_string_lossy(), &url]).map_err(|e| {
        format!("download of {url} failed — does release {tag} have a {target} asset (published by the v* tag workflow)? {e}")
    })?;

    // bsdtar (shipped on macOS and Windows 10+) also extracts zip.
    let status = Command::new("tar")
        .args([
            "-xf",
            &archive.to_string_lossy(),
            "-C",
            &work.to_string_lossy(),
        ])
        .status()
        .map_err(|e| format!("tar: {e}"))?;
    if !status.success() {
        return Err("extracting the release archive failed".into());
    }
    let bin_name = if cfg!(windows) {
        "lazymongo.exe"
    } else {
        "lazymongo"
    };
    let new_bin = work
        .join(format!("lazymongo-{version}-{target}"))
        .join(bin_name);
    if !new_bin.exists() {
        return Err(format!(
            "binary not found in archive: {}",
            new_bin.display()
        ));
    }

    let exe = std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .map_err(|e| format!("current_exe: {e}"))?;
    // Stage next to the target so the final rename is same-filesystem.
    // Remove any leftover first: overwriting a binary in place invalidates
    // its code-signature cache on macOS and the kernel SIGKILLs it.
    let staged = exe.with_extension("update");
    let _ = std::fs::remove_file(&staged);
    std::fs::copy(&new_bin, &staged).map_err(|e| format!("staging update: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
    }
    // A running executable can be renamed away on every supported OS.
    let old = exe.with_extension("old");
    let _ = std::fs::remove_file(&old);
    std::fs::rename(&exe, &old).map_err(|e| format!("moving old binary: {e}"))?;
    if let Err(e) = std::fs::rename(&staged, &exe) {
        let _ = std::fs::rename(&old, &exe); // roll back
        return Err(format!("installing new binary: {e}"));
    }
    #[cfg(unix)]
    let _ = std::fs::remove_file(&old); // Windows keeps .old until next run
    let _ = std::fs::remove_dir_all(&work);
    Ok(exe)
}

fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let mut it = s.trim().splitn(3, '.');
    let major = it.next()?.parse().ok()?;
    let minor = it.next()?.parse().ok()?;
    // Tolerate suffixes like "3-rc1" by taking leading digits.
    let patch_raw = it.next().unwrap_or("0");
    let digits: String = patch_raw
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    let patch = if digits.is_empty() {
        0
    } else {
        digits.parse().ok()?
    };
    Some((major, minor, patch))
}

fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_compare() {
        assert!(is_newer("0.2.0", "0.1.0"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(is_newer("0.1.10", "0.1.9"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.2.0"));
        assert!(!is_newer("stable", "0.1.0")); // non-semver tag ignored
    }

    #[test]
    fn asset_url_uses_actual_tag() {
        // v-prefixed tag (workflow releases) and bare tag (hand-made) both
        // point at the real tag path, with the version-only asset name.
        assert_eq!(
            asset_url("v0.3.0", "aarch64-apple-darwin"),
            "https://github.com/edumntg/lazymongo/releases/download/v0.3.0/lazymongo-0.3.0-aarch64-apple-darwin.tar.gz"
        );
        assert_eq!(
            asset_url("0.3.0", "aarch64-apple-darwin"),
            "https://github.com/edumntg/lazymongo/releases/download/0.3.0/lazymongo-0.3.0-aarch64-apple-darwin.tar.gz"
        );
    }

    #[test]
    fn display_version_strips_v() {
        assert_eq!(display_version("v0.3.0"), "0.3.0");
        assert_eq!(display_version("0.3.0"), "0.3.0");
    }

    #[test]
    fn asset_names_match_release_workflow() {
        assert_eq!(
            asset_name("0.2.0", "aarch64-apple-darwin"),
            "lazymongo-0.2.0-aarch64-apple-darwin.tar.gz"
        );
        assert_eq!(
            asset_name("0.2.0", "x86_64-pc-windows-msvc"),
            "lazymongo-0.2.0-x86_64-pc-windows-msvc.zip"
        );
    }

    #[test]
    fn resume_info_round_trips() {
        let r = ResumeInfo {
            uri: Some("mongodb://localhost:27017".into()),
            db: Some("app_db".into()),
            coll: Some("users".into()),
            filter: "{ status: 'active' }".into(),
            view: "table".into(),
            read_only: true,
        };
        let json = serde_json::to_string(&r).unwrap();
        let back: ResumeInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(back.db.as_deref(), Some("app_db"));
        assert_eq!(back.filter, "{ status: 'active' }");
        assert!(back.read_only);
    }
}
