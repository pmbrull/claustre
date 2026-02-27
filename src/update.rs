//! Auto-update support for claustre.
//!
//! Checks GitHub releases for newer versions and downloads/replaces the binary
//! in the background. Uses `curl` and `tar` via `std::process::Command` to
//! avoid adding HTTP client dependencies.

use std::fs;
use std::process::Command;

use anyhow::{Context, Result};

const REPO: &str = "pmbrull/claustre";

/// Version string baked in at compile time by `build.rs`.
/// Format: `version-<7-char-commit-hash>` (e.g., `version-abc1234`).
/// Local dev builds without CI will show `version-<local-hash>` or `dev`.
pub const VERSION: &str = env!("CLAUSTRE_VERSION");

/// Result of a background update check.
pub enum UpdateCheckResult {
    /// A newer version is available and was successfully installed.
    Updated { new_version: String },
    /// Already running the latest version.
    UpToDate,
    /// Check or update failed (non-fatal).
    Failed { reason: String },
}

/// Query the GitHub API for the latest release tag.
///
/// Uses `curl` to fetch the releases/latest endpoint and parses the `tag_name`
/// field from the JSON response without a full JSON parser.
fn fetch_latest_tag() -> Result<String> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let output = Command::new("curl")
        .args([
            "-fsSL",
            "-H",
            "Accept: application/vnd.github.v3+json",
            &url,
        ])
        .output()
        .context("failed to run curl")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("GitHub API request failed: {stderr}");
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    // Extract tag_name from JSON without a full parser.
    // The field appears as: "tag_name": "version-abc1234",
    for line in stdout.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("\"tag_name\":") {
            let tag = rest
                .trim()
                .trim_start_matches('"')
                .trim_end_matches(',')
                .trim_end_matches('"');
            return Ok(tag.to_string());
        }
    }

    anyhow::bail!("tag_name not found in GitHub API response")
}

/// Detect the platform archive suffix for the current OS/architecture.
fn platform_archive() -> Result<&'static str> {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;

    match (os, arch) {
        ("macos", "aarch64") => Ok("claustre-macos-aarch64.tar.gz"),
        ("macos", "x86_64") => Ok("claustre-macos-x86_64.tar.gz"),
        ("linux", "aarch64") => Ok("claustre-linux-aarch64.tar.gz"),
        ("linux", "x86_64") => Ok("claustre-linux-x86_64.tar.gz"),
        _ => anyhow::bail!("unsupported platform: {os}/{arch}"),
    }
}

/// Download the release archive and replace the current binary.
fn download_and_install(tag: &str) -> Result<()> {
    let archive = platform_archive()?;
    let url = format!("https://github.com/{REPO}/releases/download/{tag}/{archive}");

    // Create a temp directory for the download.
    let tmp_dir = std::env::temp_dir().join(format!("claustre-update-{tag}"));
    fs::create_dir_all(&tmp_dir).context("failed to create temp directory")?;

    let archive_path = tmp_dir.join(archive);
    let archive_str = archive_path
        .to_str()
        .context("temp path contains invalid UTF-8")?;

    // Download the archive.
    let status = Command::new("curl")
        .args(["-fsSL", &url, "-o", archive_str])
        .status()
        .context("failed to download update")?;

    if !status.success() {
        let _ = fs::remove_dir_all(&tmp_dir);
        anyhow::bail!("download failed (status {})", status.code().unwrap_or(-1));
    }

    // Extract the binary.
    let tmp_str = tmp_dir
        .to_str()
        .context("temp path contains invalid UTF-8")?;
    let status = Command::new("tar")
        .args(["-xzf", archive_str, "-C", tmp_str])
        .status()
        .context("failed to extract archive")?;

    if !status.success() {
        let _ = fs::remove_dir_all(&tmp_dir);
        anyhow::bail!("extraction failed");
    }

    // Replace the current binary.
    let current_exe = std::env::current_exe().context("could not determine current executable")?;
    let new_binary = tmp_dir.join("claustre");

    if !new_binary.exists() {
        let _ = fs::remove_dir_all(&tmp_dir);
        anyhow::bail!("extracted archive does not contain 'claustre' binary");
    }

    fs::copy(&new_binary, &current_exe).context("failed to replace binary")?;

    // Ensure the new binary is executable.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&current_exe, fs::Permissions::from_mode(0o755))?;
    }

    // Clean up.
    let _ = fs::remove_dir_all(&tmp_dir);

    Ok(())
}

/// Check for a newer version and auto-update if one is found.
///
/// This is the entry point called from a background thread.
/// Returns an `UpdateCheckResult` that the TUI uses to show a toast.
pub fn check_and_update() -> UpdateCheckResult {
    let latest_tag = match fetch_latest_tag() {
        Ok(tag) => tag,
        Err(e) => {
            return UpdateCheckResult::Failed {
                reason: format!("version check failed: {e}"),
            };
        }
    };

    // Skip if already up to date (or running a dev build).
    if latest_tag == VERSION || VERSION == "dev" {
        return UpdateCheckResult::UpToDate;
    }

    // Download and install the new version.
    match download_and_install(&latest_tag) {
        Ok(()) => UpdateCheckResult::Updated {
            new_version: latest_tag,
        },
        Err(e) => UpdateCheckResult::Failed {
            reason: format!("update failed: {e}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_set() {
        // build.rs should always set CLAUSTRE_VERSION
        assert!(!VERSION.is_empty());
    }

    #[test]
    fn platform_archive_returns_valid_name() {
        // Should succeed on any supported dev machine
        let result = platform_archive();
        assert!(result.is_ok());
        let name = result.unwrap();
        assert!(name.starts_with("claustre-"));
        assert!(name.ends_with(".tar.gz"));
    }
}
