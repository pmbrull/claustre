//! Onboarding wizard for configuring Claude Code and related tools.
//!
//! `claustre configure` walks the user through checking prerequisites (claude CLI,
//! gh CLI) and aligning `~/.claude/settings.json` permissions with recommended
//! defaults for autonomous workflow.

use std::collections::BTreeSet;
use std::fs;
use std::io::{self, BufRead, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};

use crate::config::RecommendedPermissions;

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Path to `~/.claude/settings.json`.
fn claude_settings_path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("could not determine home directory")?;
    Ok(home.join(".claude").join("settings.json"))
}

/// Read and parse `~/.claude/settings.json`, returning `None` if it doesn't
/// exist.
fn read_settings(path: &Path) -> Result<Option<serde_json::Value>> {
    if !path.exists() {
        return Ok(None);
    }
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let val: serde_json::Value = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(Some(val))
}

/// Extract a string array from a JSON value at `obj.permissions.<key>`.
fn get_permission_set(settings: &serde_json::Value, key: &str) -> BTreeSet<String> {
    settings
        .get("permissions")
        .and_then(|p| p.get(key))
        .and_then(serde_json::Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(serde_json::Value::as_str)
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

/// Build a `BTreeSet` from a slice of `&str`.
#[cfg(test)]
fn set_from_slice(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

/// Compute what entries from `recommended` are missing in `current`.
pub fn missing_entries(
    current: &BTreeSet<String>,
    recommended: &BTreeSet<String>,
) -> BTreeSet<String> {
    recommended.difference(current).cloned().collect()
}

/// Compute what entries in `current` are not in `recommended` (extras the user
/// added themselves).
#[cfg(test)]
fn extra_entries(current: &BTreeSet<String>, recommended: &BTreeSet<String>) -> BTreeSet<String> {
    current.difference(recommended).cloned().collect()
}

// ── Diff display ─────────────────────────────────────────────────────────────

/// A summary of differences between the user's current permissions and the
/// recommended set, for a single permission category (allow / deny / ask).
#[derive(Debug)]
pub struct PermissionDiff {
    pub category: String,
    pub missing: BTreeSet<String>,
}

impl PermissionDiff {
    pub fn is_aligned(&self) -> bool {
        self.missing.is_empty()
    }
}

/// Build diffs for all three permission categories against the recommended set.
pub fn compute_diffs(
    settings: &serde_json::Value,
    recommended: &RecommendedPermissions,
) -> Vec<PermissionDiff> {
    let categories: Vec<(&str, &[String])> = vec![
        ("allow", &recommended.allow),
        ("deny", &recommended.deny),
        ("ask", &recommended.ask),
    ];

    categories
        .iter()
        .map(|(cat, rec)| {
            let current = get_permission_set(settings, cat);
            let rec_set: BTreeSet<String> = rec.iter().cloned().collect();
            PermissionDiff {
                category: (*cat).to_string(),
                missing: missing_entries(&current, &rec_set),
            }
        })
        .collect()
}

/// Apply accepted permission changes to the settings JSON value.
/// `to_add` maps category name → set of permission strings to add.
pub fn apply_permission_changes(
    settings: &mut serde_json::Value,
    to_add: &[(&str, &BTreeSet<String>)],
) -> Result<()> {
    let permissions = settings
        .as_object_mut()
        .context("settings is not a JSON object")?
        .entry("permissions")
        .or_insert_with(|| serde_json::json!({}));

    let perm_obj = permissions
        .as_object_mut()
        .context("permissions is not a JSON object")?;

    for (category, additions) in to_add {
        if additions.is_empty() {
            continue;
        }

        let arr = perm_obj
            .entry(*category)
            .or_insert_with(|| serde_json::json!([]));

        let existing: BTreeSet<String> = arr
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default();

        let mut merged: Vec<String> = existing.into_iter().collect();
        for item in *additions {
            if !merged.contains(item) {
                merged.push(item.clone());
            }
        }

        *arr =
            serde_json::Value::Array(merged.into_iter().map(serde_json::Value::String).collect());
    }

    Ok(())
}

// ── Prerequisite checks ──────────────────────────────────────────────────────

#[derive(Debug)]
struct CheckResult {
    name: String,
    installed: bool,
    detail: String,
}

fn check_command_exists(name: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {name}")])
        .output()
        .is_ok_and(|o| o.status.success())
}

fn check_claude() -> CheckResult {
    let installed = check_command_exists("claude");
    let detail = if installed {
        let version = Command::new("claude")
            .arg("--version")
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        if version.is_empty() {
            "installed".to_string()
        } else {
            format!("installed ({version})")
        }
    } else {
        "not found — install from https://docs.anthropic.com/en/docs/claude-code".to_string()
    };

    CheckResult {
        name: "claude".to_string(),
        installed,
        detail,
    }
}

fn check_gh() -> CheckResult {
    let installed = check_command_exists("gh");
    if !installed {
        return CheckResult {
            name: "gh".to_string(),
            installed: false,
            detail: "not found — install from https://cli.github.com".to_string(),
        };
    }

    // Check authentication
    let auth_ok = Command::new("gh")
        .args(["auth", "status"])
        .output()
        .is_ok_and(|o| o.status.success());

    let detail = if auth_ok {
        "installed and authenticated".to_string()
    } else {
        "installed but NOT authenticated — run `gh auth login`".to_string()
    };

    CheckResult {
        name: "gh".to_string(),
        installed,
        detail,
    }
}

fn check_git() -> CheckResult {
    let installed = check_command_exists("git");
    let detail = if installed {
        "installed".to_string()
    } else {
        "not found — install git first".to_string()
    };

    CheckResult {
        name: "git".to_string(),
        installed,
        detail,
    }
}

// ── Interactive prompts ──────────────────────────────────────────────────────

/// Prompt for yes/no, returning `true` for yes.  `default` is used when the
/// user presses Enter without typing anything.
fn prompt_yn(question: &str, default: bool) -> bool {
    let hint = if default { "[Y/n]" } else { "[y/N]" };
    print!("{question} {hint} ");
    io::stdout().flush().ok();

    let mut line = String::new();
    if io::stdin().lock().read_line(&mut line).is_err() {
        return default;
    }

    match line.trim().to_lowercase().as_str() {
        "y" | "yes" => true,
        "n" | "no" => false,
        _ => default,
    }
}

// ── ANSI helpers ─────────────────────────────────────────────────────────────

fn bold(s: &str) -> String {
    format!("\x1b[1m{s}\x1b[0m")
}

fn green(s: &str) -> String {
    format!("\x1b[32m{s}\x1b[0m")
}

fn yellow(s: &str) -> String {
    format!("\x1b[33m{s}\x1b[0m")
}

fn red(s: &str) -> String {
    format!("\x1b[31m{s}\x1b[0m")
}

fn dim(s: &str) -> String {
    format!("\x1b[2m{s}\x1b[0m")
}

// ── Main wizard ──────────────────────────────────────────────────────────────

pub fn run() -> Result<()> {
    let cfg = crate::config::load()?;
    let perms = &cfg.permissions;

    println!();
    println!("{}", bold("claustre configure"));
    println!("{}", dim("Onboarding wizard for Claude Code + claustre"));
    println!();

    // ── Step 1: Prerequisites ────────────────────────────────────────────
    println!("{}", bold("Step 1: Checking prerequisites"));
    println!();

    let checks = [check_git(), check_claude(), check_gh()];

    let mut all_ok = true;
    for check in &checks {
        let icon = if check.installed {
            green("✓")
        } else {
            all_ok = false;
            red("✗")
        };
        println!("  {icon} {}: {}", bold(&check.name), check.detail);
    }
    println!();

    if all_ok {
        println!("{}", green("All prerequisites are installed."));
        println!();
    } else {
        println!(
            "{}",
            yellow(
                "Some prerequisites are missing. claustre needs claude and gh to function properly."
            )
        );
        println!(
            "{}",
            dim("You can still configure permissions now and install the missing tools later.")
        );
        println!();
    }

    // ── Step 2: Claude permissions ───────────────────────────────────────
    println!("{}", bold("Step 2: Claude Code permissions"));
    println!();

    let settings_path = claude_settings_path()?;
    let settings_opt = read_settings(&settings_path)?;

    let mut settings = if let Some(s) = settings_opt {
        println!("  Found {}", dim(&settings_path.display().to_string()));
        s
    } else {
        println!(
            "  {} does not exist yet — will create it with recommended permissions.",
            dim(&settings_path.display().to_string())
        );
        serde_json::json!({})
    };
    println!();

    let diffs = compute_diffs(&settings, perms);

    let all_aligned = diffs.iter().all(PermissionDiff::is_aligned);

    if all_aligned {
        println!(
            "{}",
            green("  Your permissions already match all recommendations!")
        );
        println!();
        print_current_permissions(&settings);
        println!();
        println!("{}", green("Configuration complete — you're all set!"));
        return Ok(());
    }

    // Show recommended permissions overview
    println!("  claustre recommends these Claude Code permissions for autonomous workflows:");
    println!("  (customise in ~/.claustre/config.toml under [permissions])");
    println!();
    println!("  {}:", bold("allow"));
    for p in &perms.allow {
        println!("    {}", green(&format!("+ {p}")));
    }
    println!("  {}:", bold("deny"));
    for p in &perms.deny {
        println!("    {}", red(&format!("- {p}")));
    }
    println!("  {}:", bold("ask"));
    for p in &perms.ask {
        println!("    {}", yellow(&format!("? {p}")));
    }
    println!();

    // Show what's different
    for diff in &diffs {
        if diff.missing.is_empty() {
            continue;
        }
        println!(
            "  {} — missing {} recommended {}:",
            bold(&diff.category),
            diff.missing.len(),
            if diff.missing.len() == 1 {
                "entry"
            } else {
                "entries"
            }
        );
        for m in &diff.missing {
            println!("    {}", green(&format!("+ {m}")));
        }
    }

    println!();

    // Ask user what to do
    let mut additions: Vec<(&str, BTreeSet<String>)> = Vec::new();

    // First offer bulk accept
    if prompt_yn("  Apply all recommended permissions at once?", true) {
        for diff in &diffs {
            if !diff.missing.is_empty() {
                additions.push((&diff.category, diff.missing.clone()));
            }
        }
    } else {
        // Per-category selection
        for diff in &diffs {
            if diff.missing.is_empty() {
                continue;
            }
            println!();
            println!(
                "  {} — {} missing:",
                bold(&diff.category),
                diff.missing.len()
            );
            for m in &diff.missing {
                println!("    {}", green(&format!("+ {m}")));
            }

            if diff.missing.len() == 1 {
                let item = diff.missing.iter().next().expect("checked non-empty");
                if prompt_yn(
                    &format!("  Add {item} to {category}?", category = diff.category),
                    true,
                ) {
                    additions.push((&diff.category, diff.missing.clone()));
                }
            } else if prompt_yn(
                &format!(
                    "  Add all {} entries to {}?",
                    diff.missing.len(),
                    diff.category
                ),
                true,
            ) {
                additions.push((&diff.category, diff.missing.clone()));
            } else {
                // Individual selection
                let mut selected = BTreeSet::new();
                for item in &diff.missing {
                    if prompt_yn(&format!("    Add {item}?"), true) {
                        selected.insert(item.clone());
                    }
                }
                if !selected.is_empty() {
                    additions.push((&diff.category, selected));
                }
            }
        }
    }

    if additions.is_empty() {
        println!();
        println!("{}", dim("  No changes applied."));
        println!();
        println!("{}", green("Configuration complete."));
        return Ok(());
    }

    // Apply changes
    let add_refs: Vec<(&str, &BTreeSet<String>)> =
        additions.iter().map(|(cat, set)| (*cat, set)).collect();
    apply_permission_changes(&mut settings, &add_refs)?;

    // Ensure parent directory exists
    if let Some(parent) = settings_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    // Write back with pretty formatting
    let json = serde_json::to_string_pretty(&settings).context("failed to serialize settings")?;
    fs::write(&settings_path, format!("{json}\n"))
        .with_context(|| format!("failed to write {}", settings_path.display()))?;

    println!();
    println!("  {} Updated {}", green("✓"), settings_path.display());

    // Show summary
    println!();
    let total: usize = additions.iter().map(|(_, set)| set.len()).sum();
    println!(
        "  Added {} permission {}.",
        total,
        if total == 1 { "entry" } else { "entries" }
    );

    // ── Step 3: claustre init ────────────────────────────────────────────
    println!();
    println!("{}", bold("Step 3: claustre directories"));
    println!();

    crate::config::ensure_dirs()?;
    println!("  {} ~/.claustre/ initialized", green("✓"));

    // ── Done ─────────────────────────────────────────────────────────────
    println!();
    println!("{}", green("Configuration complete — you're all set!"));
    println!();
    println!("  Next steps:");
    println!(
        "    {} Add a project: claustre add-project <name> <path>",
        dim("1.")
    );
    println!(
        "    {} Add tasks:     claustre add-task <project> <title>",
        dim("2.")
    );
    println!("    {} Launch TUI:    claustre", dim("3."));
    println!();

    Ok(())
}

/// Print the current permission categories from settings.
fn print_current_permissions(settings: &serde_json::Value) {
    for cat in ["allow", "deny", "ask"] {
        let set = get_permission_set(settings, cat);
        if !set.is_empty() {
            println!("  {}:", bold(cat));
            for item in &set {
                let color_fn = match cat {
                    "allow" => green,
                    "deny" => red,
                    "ask" => yellow,
                    _ => dim,
                };
                println!("    {}", color_fn(item));
            }
        }
    }
}

// ── Public API for TUI integration ───────────────────────────────────────────

/// Lightweight check that returns a human-readable warning if Claude Code
/// permissions are not aligned with recommendations, or `None` if everything
/// is fine.  Designed to be called once on TUI startup.
pub fn check_config_status() -> Option<String> {
    let cfg = crate::config::load().ok()?;
    let path = claude_settings_path().ok()?;
    let settings = read_settings(&path).ok()?.unwrap_or(serde_json::json!({}));
    let diffs = compute_diffs(&settings, &cfg.permissions);

    let total_missing: usize = diffs.iter().map(|d| d.missing.len()).sum();
    if total_missing == 0 {
        return None;
    }

    Some(format!(
        "{total_missing} recommended Claude permission{} missing — press c to configure",
        if total_missing == 1 { "" } else { "s" }
    ))
}

/// Summary of the current permission state for the TUI configure overlay.
pub struct ConfigStatus {
    pub diffs: Vec<PermissionDiff>,
    pub settings_path: PathBuf,
    pub settings: serde_json::Value,
    pub recommended: RecommendedPermissions,
}

/// Load settings and compute diffs for TUI display.
pub fn load_config_status() -> Result<ConfigStatus> {
    let cfg = crate::config::load()?;
    let settings_path = claude_settings_path()?;
    let settings = read_settings(&settings_path)?.unwrap_or(serde_json::json!({}));
    let diffs = compute_diffs(&settings, &cfg.permissions);
    Ok(ConfigStatus {
        diffs,
        settings_path,
        settings,
        recommended: cfg.permissions,
    })
}

/// Apply all missing recommended permissions and write the settings file.
/// Returns the number of permissions added.
pub fn apply_all_recommendations(status: &mut ConfigStatus) -> Result<usize> {
    let mut additions: Vec<(&str, BTreeSet<String>)> = Vec::new();
    let mut total = 0;

    for diff in &status.diffs {
        if !diff.missing.is_empty() {
            total += diff.missing.len();
            additions.push((&diff.category, diff.missing.clone()));
        }
    }

    if total == 0 {
        return Ok(0);
    }

    let add_refs: Vec<(&str, &BTreeSet<String>)> =
        additions.iter().map(|(cat, set)| (*cat, set)).collect();
    apply_permission_changes(&mut status.settings, &add_refs)?;

    // Ensure parent directory exists
    if let Some(parent) = status.settings_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let json =
        serde_json::to_string_pretty(&status.settings).context("failed to serialize settings")?;
    fs::write(&status.settings_path, format!("{json}\n"))
        .with_context(|| format!("failed to write {}", status.settings_path.display()))?;

    // Recompute diffs after applying
    status.diffs = compute_diffs(&status.settings, &status.recommended);

    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shorthand for the default recommended permissions in tests.
    fn defaults() -> RecommendedPermissions {
        RecommendedPermissions::default()
    }

    #[test]
    fn missing_entries_finds_gaps() {
        let current: BTreeSet<String> = ["Bash", "Read(*)"]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        let recommended: BTreeSet<String> = defaults().allow.into_iter().collect();
        let missing = missing_entries(&current, &recommended);

        assert!(missing.contains("Glob(*)"));
        assert!(missing.contains("Edit(*)"));
        assert!(!missing.contains("Bash"));
        assert!(!missing.contains("Read(*)"));
    }

    #[test]
    fn missing_entries_empty_when_superset() {
        let set: BTreeSet<String> = defaults().allow.into_iter().collect();
        let missing = missing_entries(&set, &set);
        assert!(missing.is_empty());
    }

    #[test]
    fn extra_entries_finds_user_additions() {
        let mut current: BTreeSet<String> = defaults().allow.into_iter().collect();
        current.insert("WebSearch(*)".to_string());
        let recommended: BTreeSet<String> = defaults().allow.into_iter().collect();
        let extra = extra_entries(&current, &recommended);

        assert_eq!(extra.len(), 1);
        assert!(extra.contains("WebSearch(*)"));
    }

    #[test]
    fn compute_diffs_empty_settings() {
        let perms = defaults();
        let settings = serde_json::json!({});
        let diffs = compute_diffs(&settings, &perms);

        assert_eq!(diffs.len(), 3);

        let allow = &diffs[0];
        assert_eq!(allow.category, "allow");
        assert_eq!(allow.missing.len(), perms.allow.len());

        let deny = &diffs[1];
        assert_eq!(deny.category, "deny");
        assert_eq!(deny.missing.len(), perms.deny.len());

        let ask = &diffs[2];
        assert_eq!(ask.category, "ask");
        assert_eq!(ask.missing.len(), perms.ask.len());
    }

    #[test]
    fn compute_diffs_fully_aligned() {
        let perms = defaults();
        let settings = serde_json::json!({
            "permissions": {
                "allow": perms.allow,
                "deny": perms.deny,
                "ask": perms.ask,
            }
        });
        let diffs = compute_diffs(&settings, &defaults());

        for diff in &diffs {
            assert!(
                diff.is_aligned(),
                "category '{}' should be aligned but has missing: {:?}",
                diff.category,
                diff.missing
            );
        }
    }

    #[test]
    fn compute_diffs_partial_allow() {
        let perms = defaults();
        let settings = serde_json::json!({
            "permissions": {
                "allow": ["Bash", "Read(*)"],
                "deny": perms.deny,
                "ask": perms.ask,
            }
        });
        let diffs = compute_diffs(&settings, &defaults());

        let allow = &diffs[0];
        assert!(!allow.is_aligned());
        assert!(allow.missing.contains("Glob(*)"));
        assert!(!allow.missing.contains("Bash"));

        // deny and ask should be aligned
        assert!(diffs[1].is_aligned());
        assert!(diffs[2].is_aligned());
    }

    #[test]
    fn apply_permission_changes_adds_to_existing() {
        let mut settings = serde_json::json!({
            "permissions": {
                "allow": ["Bash"]
            }
        });

        let additions: BTreeSet<String> = ["Glob(*)", "Read(*)"]
            .iter()
            .map(|s| (*s).to_string())
            .collect();

        apply_permission_changes(&mut settings, &[("allow", &additions)]).unwrap();

        let result = get_permission_set(&settings, "allow");
        assert!(result.contains("Bash"));
        assert!(result.contains("Glob(*)"));
        assert!(result.contains("Read(*)"));
    }

    #[test]
    fn apply_permission_changes_creates_category() {
        let mut settings = serde_json::json!({
            "permissions": {}
        });

        let additions: BTreeSet<String> = ["Bash(rm:*)"].iter().map(|s| (*s).to_string()).collect();

        apply_permission_changes(&mut settings, &[("ask", &additions)]).unwrap();

        let result = get_permission_set(&settings, "ask");
        assert!(result.contains("Bash(rm:*)"));
    }

    #[test]
    fn apply_permission_changes_no_duplicates() {
        let mut settings = serde_json::json!({
            "permissions": {
                "allow": ["Bash", "Read(*)"]
            }
        });

        let additions: BTreeSet<String> = ["Bash", "Glob(*)"]
            .iter()
            .map(|s| (*s).to_string())
            .collect();

        apply_permission_changes(&mut settings, &[("allow", &additions)]).unwrap();

        let arr = settings["permissions"]["allow"]
            .as_array()
            .expect("should be an array");
        let bash_count = arr.iter().filter(|v| v.as_str() == Some("Bash")).count();
        assert_eq!(bash_count, 1, "Bash should appear exactly once");
    }

    #[test]
    fn apply_permission_changes_creates_permissions_object() {
        let mut settings = serde_json::json!({});

        let additions: BTreeSet<String> = ["Bash"].iter().map(|s| (*s).to_string()).collect();

        apply_permission_changes(&mut settings, &[("allow", &additions)]).unwrap();

        let result = get_permission_set(&settings, "allow");
        assert!(result.contains("Bash"));
    }

    #[test]
    fn apply_permission_changes_preserves_other_fields() {
        let mut settings = serde_json::json!({
            "env": {"FOO": "bar"},
            "permissions": {
                "allow": ["Bash"]
            },
            "hooks": {}
        });

        let additions: BTreeSet<String> = ["Read(*)"].iter().map(|s| (*s).to_string()).collect();

        apply_permission_changes(&mut settings, &[("allow", &additions)]).unwrap();

        // Other fields should be untouched
        assert_eq!(settings["env"]["FOO"], "bar");
        assert!(settings.get("hooks").is_some());
    }

    #[test]
    fn set_from_slice_produces_sorted_set() {
        let set = set_from_slice(&["Zebra", "Apple", "Mango"]);
        let items: Vec<_> = set.iter().collect();
        assert_eq!(items, vec!["Apple", "Mango", "Zebra"]);
    }

    #[test]
    fn get_permission_set_handles_missing_permissions() {
        let settings = serde_json::json!({});
        let result = get_permission_set(&settings, "allow");
        assert!(result.is_empty());
    }

    #[test]
    fn get_permission_set_handles_missing_category() {
        let settings = serde_json::json!({
            "permissions": {}
        });
        let result = get_permission_set(&settings, "allow");
        assert!(result.is_empty());
    }

    #[test]
    fn permission_diff_is_aligned() {
        let diff = PermissionDiff {
            category: "allow".to_string(),
            missing: BTreeSet::new(),
        };
        assert!(diff.is_aligned());

        let diff_with_missing = PermissionDiff {
            category: "allow".to_string(),
            missing: set_from_slice(&["Bash"]),
        };
        assert!(!diff_with_missing.is_aligned());
    }

    #[test]
    fn apply_multiple_categories() {
        let mut settings = serde_json::json!({});

        let allow_add = set_from_slice(&["Bash", "Read(*)"]);
        let deny_add = set_from_slice(&["Bash(git push --force*)"]);
        let ask_add = set_from_slice(&["Bash(rm:*)"]);

        apply_permission_changes(
            &mut settings,
            &[
                ("allow", &allow_add),
                ("deny", &deny_add),
                ("ask", &ask_add),
            ],
        )
        .unwrap();

        assert!(get_permission_set(&settings, "allow").contains("Bash"));
        assert!(get_permission_set(&settings, "allow").contains("Read(*)"));
        assert!(get_permission_set(&settings, "deny").contains("Bash(git push --force*)"));
        assert!(get_permission_set(&settings, "ask").contains("Bash(rm:*)"));
    }
}
