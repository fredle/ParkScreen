//! Installing and updating the ParkScreen display driver from the tray (Windows).
//!
//! The driver is a separate, optional download because loading it needs administrator rights
//! and a signature Windows trusts. CI publishes a rolling GitHub release (`driver`) with:
//!   - `driver-manifest.json`: `{ "version", "url", "sha256", "testSigned", "minHost" }`
//!   - `ParkScreenIdd.zip`: the signed package and `install.ps1`
//!
//! This module reads the manifest, downloads and verifies the zip, unpacks it and runs
//! `install.ps1` elevated (one UAC prompt). The app itself stays per-user and never touches the
//! driver without the user choosing to.

use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicBool, Ordering},
};

const MANIFEST_URL: &str = "https://github.com/fredle/ParkScreen/releases/download/driver/driver-manifest.json";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

static UPDATE_AVAILABLE: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Manifest {
    pub version: String,
    pub url: String,
    pub sha256: String,
    #[serde(default, rename = "testSigned")]
    pub test_signed: bool,
}

/// What the install script reports through `-ResultFile`.
#[derive(Debug, Deserialize)]
struct InstallResult {
    ok: bool,
    #[serde(default)]
    code: i32,
    #[serde(default)]
    message: String,
}

/// A newer driver than the installed one is available (set by the background check).
pub fn update_available() -> bool {
    UPDATE_AVAILABLE.load(Ordering::Relaxed)
}

fn data_dir() -> PathBuf {
    PathBuf::from(std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into())).join("ParkScreen")
}

fn version_file() -> PathBuf {
    data_dir().join("driver-version.txt")
}

/// The driver version this app last installed, if any.
pub fn installed_version() -> Option<String> {
    std::fs::read_to_string(version_file()).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// `0.1.12` -> `[0, 1, 12]`; anything unparsable counts as zero so it never beats a real version.
fn parse_version(v: &str) -> Vec<u64> {
    v.trim().trim_start_matches('v').split('.').map(|p| p.parse().unwrap_or(0)).collect()
}

/// True when `candidate` is strictly newer than `installed`.
pub fn is_newer(candidate: &str, installed: &str) -> bool {
    let (mut a, mut b) = (parse_version(candidate), parse_version(installed));
    let n = a.len().max(b.len());
    a.resize(n, 0);
    b.resize(n, 0);
    a > b
}

fn http_get(url: &str, limit: u64) -> Result<Vec<u8>, String> {
    let mut resp = ureq::get(url).call().map_err(|e| format!("could not download {url}: {e}"))?;
    resp.body_mut().with_config().limit(limit).read_to_vec().map_err(|e| format!("could not read {url}: {e}"))
}

pub fn fetch_manifest() -> Result<Manifest, String> {
    let bytes = http_get(MANIFEST_URL, 64 * 1024)?;
    serde_json::from_slice(&bytes).map_err(|e| format!("the driver manifest is not valid: {e}"))
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

fn ps_quote(p: &Path) -> String {
    format!("'{}'", p.display().to_string().replace('\'', "''"))
}

/// Download, verify, unpack and install `m`. Returns a message for the user.
pub fn install(m: &Manifest) -> Result<String, String> {
    let zip = http_get(&m.url, 64 * 1024 * 1024)?;
    if sha256_hex(&zip) != m.sha256.to_ascii_lowercase() {
        return Err("The downloaded driver did not match its checksum, so it was not installed.".into());
    }
    let dir = data_dir().join("driver").join(&m.version);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    let zip_path = dir.join("ParkScreenIdd.zip");
    std::fs::write(&zip_path, &zip).map_err(|e| format!("could not save the driver: {e}"))?;
    // Windows 10+ ships bsdtar, which unpacks zip files. Use it by full path: a GNU tar earlier on
    // PATH (Git for Windows, MSYS) mistakes "C:" for a host name.
    let tar = PathBuf::from(std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into())).join(r"System32\tar.exe");
    let ok = Command::new(tar)
        .arg("-xf")
        .arg(&zip_path)
        .arg("-C")
        .arg(&dir)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !ok || !dir.join("install.ps1").exists() {
        return Err("Could not unpack the downloaded driver.".into());
    }

    let result = dir.join("result.json");
    let script = dir.join("install.ps1");
    // The inner PowerShell runs elevated (one UAC prompt); this one waits for it.
    let inner = format!(
        "-NoProfile -ExecutionPolicy Bypass -File {} -Package {} -ResultFile {}",
        ps_quote(&script),
        ps_quote(&dir),
        ps_quote(&result)
    );
    let outer = format!("Start-Process powershell -Verb RunAs -Wait -WindowStyle Hidden -ArgumentList \"{}\"", inner.replace('"', "\\\""));
    let launched = Command::new("powershell")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", &outer])
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    match launched {
        Ok(o) if !o.status.success() => {
            return Err("Windows did not grant administrator permission, so the driver was not installed.".into())
        }
        Err(e) => return Err(format!("could not start the installer: {e}")),
        _ => {}
    }
    let text = std::fs::read_to_string(&result).map_err(|_| "The installer did not report a result. Nothing was changed.".to_string())?;
    let r: InstallResult = serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|e| format!("unreadable installer result: {e}"))?;
    if r.ok {
        let _ = std::fs::write(version_file(), &m.version);
        crate::settings::set_driver_present(crate::idd::driver_installed());
        UPDATE_AVAILABLE.store(false, Ordering::Relaxed);
        Ok(format!("Display driver {} is installed. Choose \"Extend\" in the ParkScreen tray menu.", m.version))
    } else if r.code == 3 {
        Err(format!(
            "Windows is not in test-signing mode, so it will not load this test-signed driver.\n\n{}\n\nTo allow it, run \"bcdedit /set testsigning on\" as administrator (Secure Boot must be off), restart, then choose Install display driver again.",
            r.message
        ))
    } else {
        Err(format!("The driver could not be installed (code {}): {}", r.code, r.message))
    }
}

/// Fetch the manifest and report whether a driver install or update is on offer.
/// Returns `Ok(Some(manifest))` when the user could install or update, `Ok(None)` when current.
pub fn check() -> Result<Option<Manifest>, String> {
    let m = fetch_manifest()?;
    let present = crate::idd::driver_installed();
    let newer = match installed_version() {
        Some(v) => is_newer(&m.version, &v),
        None => true,
    };
    // A device that is present but was installed by hand has no version file: offer the update.
    Ok(if !present || newer { Some(m) } else { None })
}

/// Look for a driver update in the background (after start, then twice a day) and flag it for the
/// tray. Does nothing until a driver is installed: first installs are the user's choice.
pub fn spawn_update_checker() {
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(120));
        loop {
            if crate::idd::driver_installed() {
                if let Ok(Some(_)) = check() {
                    UPDATE_AVAILABLE.store(true, Ordering::Relaxed);
                }
            }
            std::thread::sleep(std::time::Duration::from_secs(12 * 3600));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions_numerically() {
        assert!(is_newer("0.1.10", "0.1.9"));
        assert!(is_newer("0.2.0", "0.1.99"));
        assert!(!is_newer("0.1.9", "0.1.9"));
        assert!(!is_newer("0.1.8", "0.1.9"));
        assert!(is_newer("0.1.9.1", "0.1.9"));
        assert!(!is_newer("garbage", "0.1.0"));
    }

    #[test]
    fn parses_the_manifest_the_ci_publishes() {
        let m: Manifest = serde_json::from_str(
            r#"{"version":"0.1.7","url":"https://example/ParkScreenIdd.zip","sha256":"ab12","testSigned":true,"minHost":"0.1.9"}"#,
        )
        .unwrap();
        assert_eq!(m.version, "0.1.7");
        assert!(m.test_signed);
    }

    #[test]
    fn hashes_like_sha256sum() {
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn refuses_a_download_that_does_not_match_its_checksum() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut c, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = c.read(&mut buf);
                let body = b"not the driver you asked for";
                let _ = c.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes());
                let _ = c.write_all(body);
            }
        });
        let m = Manifest {
            version: "0.0.1".into(),
            url: format!("http://127.0.0.1:{port}/ParkScreenIdd.zip"),
            sha256: "00".repeat(32),
            test_signed: true,
        };
        let err = install(&m).unwrap_err();
        assert!(err.contains("checksum"), "{err}");
    }

    #[test]
    fn quotes_paths_for_powershell() {
        assert_eq!(ps_quote(Path::new(r"C:\a b\o'neil")), r"'C:\a b\o''neil'");
    }
}
