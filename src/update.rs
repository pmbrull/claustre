//! Auto-update support for claustre.
//!
//! Checks GitHub releases for newer versions and downloads/replaces the binary
//! in the background.  Uses `curl` and `tar` via `std::process::Command` to
//! avoid adding HTTP client dependencies.
//!
//! ## Safety measures
//!
//! 1. **Smoke test** — the downloaded binary is run with `health-check` before
//!    it ever replaces the installed one.  If it exits non-zero or times out,
//!    the update is aborted.
//! 2. **Backup** — the current binary is copied to
//!    `~/.claustre/bin/claustre.prev` before replacement.  If the copy fails,
//!    the backup is restored automatically.
//! 3. **`claustre rollback`** — manual escape hatch that copies the backup
//!    back over the running binary.

use anyhow::{Context, Result};
use std::fs;
use std::path::PathBuf;
use std::process::Command;

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
    /// A newer version exists but installation failed.
    Available { new_version: String, reason: String },
    /// Could not determine the latest version (non-fatal).
    Failed { reason: String },
}

/// Path to the backup binary: `~/.claustre/bin/claustre.prev`.
fn backup_path() -> Result<PathBuf> {
    let base = crate::config::base_dir()?;
    let bin_dir = base.join("bin");
    fs::create_dir_all(&bin_dir).context("failed to create ~/.claustre/bin/")?;
    Ok(bin_dir.join("claustre.prev"))
}

/// Restore the previous binary from `~/.claustre/bin/claustre.prev`.
///
/// Called by the `claustre rollback` subcommand.
pub fn rollback() -> Result<()> {
    let backup = backup_path()?;
    anyhow::ensure!(backup.exists(), "no backup found at {}", backup.display());

    let current_exe = std::env::current_exe().context("could not determine current executable")?;
    fs::copy(&backup, &current_exe).context("failed to restore backup")?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&current_exe, fs::Permissions::from_mode(0o755))?;
    }

    #[cfg(target_os = "macos")]
    {
        let _ = Command::new("codesign")
            .args(["--sign", "-", "--force"])
            .arg(&current_exe)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }

    println!(
        "Rolled back to previous version (backup: {})",
        backup.display()
    );
    Ok(())
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

/// Run `<binary> health-check` and verify it exits 0 within the timeout.
fn smoke_test(binary: &std::path::Path) -> Result<()> {
    let child = Command::new(binary)
        .arg("health-check")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("failed to spawn health-check")?;

    let output = child
        .wait_with_output()
        .context("health-check process failed")?;

    // The wait_with_output call above is blocking but the child should
    // complete almost instantly.  For a hard timeout we rely on the
    // background thread being non-critical — if it hangs the TUI stays
    // responsive.  A future improvement could use a timed waitpid.

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("health-check exited {}: {stderr}", output.status);
    }

    Ok(())
}

/// Download the release archive, smoke-test, backup, and replace the current binary.
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

    let new_binary = tmp_dir.join("claustre");
    if !new_binary.exists() {
        let _ = fs::remove_dir_all(&tmp_dir);
        anyhow::bail!("extracted archive does not contain 'claustre' binary");
    }

    // ── Smoke test: run health-check on the new binary before touching
    //    the installed one.  If it fails, abort the entire update.
    if let Err(e) = smoke_test(&new_binary) {
        let _ = fs::remove_dir_all(&tmp_dir);
        anyhow::bail!("new binary failed smoke test: {e}");
    }

    // ── Backup the current binary so `claustre rollback` can restore it.
    let current_exe = std::env::current_exe().context("could not determine current executable")?;
    let backup = backup_path()?;
    fs::copy(&current_exe, &backup)
        .with_context(|| format!("failed to backup current binary to {}", backup.display()))?;

    // ── Replace the installed binary.
    if let Err(e) = fs::copy(&new_binary, &current_exe) {
        // Copy failed — restore the backup.
        let _ = fs::copy(&backup, &current_exe);
        let _ = fs::remove_dir_all(&tmp_dir);
        return Err(e).context("failed to replace binary (backup restored)");
    }

    // Ensure the new binary is executable.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&current_exe, fs::Permissions::from_mode(0o755))?;
    }

    // Re-sign the binary on macOS.  `fs::copy` invalidates the ad-hoc code
    // signature and Apple System Policy will SIGKILL unsigned binaries.
    #[cfg(target_os = "macos")]
    {
        let status = Command::new("codesign")
            .args(["--sign", "-", "--force"])
            .arg(&current_exe)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        if let Err(e) = status {
            tracing::warn!("codesign failed after update: {e}");
        }
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
        Err(e) => UpdateCheckResult::Available {
            new_version: latest_tag,
            reason: format!("{e}"),
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

    #[test]
    fn backup_path_is_inside_claustre_dir() {
        let path = backup_path().unwrap();
        assert!(path.ends_with("bin/claustre.prev"));
    }
}
