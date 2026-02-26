//! Passive scanner for external Claude Code sessions.
//!
//! Discovers Claude sessions by scanning `~/.claude/projects/` JSONL files.
//! Extracts token usage, timestamps, model, and project metadata.
//! Skips claustre-managed sessions and unchanged files for efficiency.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::Result;
use serde_json::Value;

use crate::store::ExternalSession;

/// Scan `~/.claude/projects/` for external (non-claustre) sessions.
///
/// - `claustre_ids`: session IDs managed by claustre (skipped)
/// - `known`: map of session ID → (`jsonl_path`, `last_scanned_at`) from DB
///
/// Returns sessions that are new or have changed since last scan.
pub fn scan_external_sessions(
    claustre_ids: &HashSet<String>,
    known: &HashMap<String, (String, String)>,
) -> Result<Vec<ExternalSession>> {
    let Some(home) = dirs::home_dir() else {
        return Ok(vec![]);
    };
    let projects_dir = home.join(".claude/projects");
    if !projects_dir.is_dir() {
        return Ok(vec![]);
    }

    let mut results = Vec::new();

    let entries = fs::read_dir(&projects_dir)?;
    for entry in entries.flatten() {
        let dir_path = entry.path();
        if !dir_path.is_dir() {
            continue;
        }

        let dir_name = dir_path.file_name().and_then(|n| n.to_str()).unwrap_or("");

        // Skip claustre-managed project directories
        if dir_name.contains("-claustre-worktrees-") || dir_name.contains("--claustre-worktrees-") {
            continue;
        }

        // Try to read sessions-index.json for originalPath fallback
        let original_path = read_original_path(&dir_path);

        let jsonl_entries = fs::read_dir(&dir_path);
        let Ok(jsonl_entries) = jsonl_entries else {
            continue;
        };
        for jsonl_entry in jsonl_entries.flatten() {
            let jsonl_path = jsonl_entry.path();
            if jsonl_path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }

            let session_id = jsonl_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();

            if session_id.is_empty() || claustre_ids.contains(&session_id) {
                continue;
            }

            // Check file mtime against last_scanned_at for incremental scanning
            if let Some((_, last_scanned)) = known.get(&session_id)
                && !file_modified_since(&jsonl_path, last_scanned)
            {
                continue;
            }

            if let Ok(session) = parse_jsonl(&jsonl_path, &session_id, original_path.as_deref()) {
                results.push(session);
            }
        }
    }

    Ok(results)
}

/// Check if a file has been modified since the given ISO timestamp.
fn file_modified_since(path: &Path, since: &str) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    let Ok(modified) = metadata.modified() else {
        return true; // Can't determine — scan to be safe
    };

    let Ok(since_time) = chrono::DateTime::parse_from_rfc3339(since) else {
        return true;
    };
    let since_system: SystemTime = since_time.into();

    modified > since_system
}

/// Read `sessions-index.json` to get the `originalPath` for a project directory.
fn read_original_path(dir: &Path) -> Option<String> {
    let index_path = dir.join("sessions-index.json");
    let content = fs::read_to_string(&index_path).ok()?;
    let value: Value = serde_json::from_str(&content).ok()?;
    // The file has a top-level "originalPath" field in recent Claude Code versions
    value
        .get("originalPath")
        .and_then(Value::as_str)
        .map(String::from)
}

/// Parse a JSONL file to extract session metadata.
///
/// Streams line-by-line to avoid loading the entire file into memory.
fn parse_jsonl(
    path: &PathBuf,
    session_id: &str,
    original_path_fallback: Option<&str>,
) -> Result<ExternalSession> {
    let file = fs::File::open(path)?;
    let reader = BufReader::new(file);

    let mut project_path: Option<String> = None;
    let mut model: Option<String> = None;
    let mut git_branch: Option<String> = None;
    let mut first_timestamp: Option<String> = None;
    let mut last_timestamp: Option<String> = None;
    let mut total_input_tokens: i64 = 0;
    let mut total_output_tokens: i64 = 0;

    for line in reader.lines() {
        let Ok(line) = line else { continue };
        if line.is_empty() {
            continue;
        }

        let Ok(entry) = serde_json::from_str::<Value>(&line) else {
            continue;
        };

        // Extract common fields from any message type
        if project_path.is_none()
            && let Some(cwd) = entry.get("cwd").and_then(Value::as_str)
        {
            project_path = Some(cwd.to_string());
        }

        if git_branch.is_none()
            && let Some(branch) = entry.get("gitBranch").and_then(Value::as_str)
        {
            git_branch = Some(branch.to_string());
        }

        if let Some(ts) = entry.get("timestamp").and_then(Value::as_str) {
            if first_timestamp.is_none() {
                first_timestamp = Some(ts.to_string());
            }
            last_timestamp = Some(ts.to_string());
        }

        // Extract token usage from assistant messages
        let msg_type = entry.get("type").and_then(Value::as_str).unwrap_or("");
        if msg_type == "assistant" {
            if model.is_none()
                && let Some(m) = entry.get("model").and_then(Value::as_str)
            {
                model = Some(m.to_string());
            }

            if let Some(usage) = entry.get("message").and_then(|m| m.get("usage")) {
                if let Some(inp) = usage.get("input_tokens").and_then(Value::as_i64) {
                    total_input_tokens += inp;
                }
                if let Some(out) = usage.get("output_tokens").and_then(Value::as_i64) {
                    total_output_tokens += out;
                }
            }
        }
    }

    let project_path = project_path
        .or_else(|| original_path_fallback.map(String::from))
        .unwrap_or_default();

    let project_name = Path::new(&project_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();

    let now = chrono::Utc::now().to_rfc3339();

    Ok(ExternalSession {
        id: session_id.to_string(),
        project_path,
        project_name,
        model,
        git_branch,
        input_tokens: total_input_tokens,
        output_tokens: total_output_tokens,
        started_at: first_timestamp,
        ended_at: last_timestamp,
        last_scanned_at: now,
        jsonl_path: path.to_string_lossy().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn make_jsonl_line(
        msg_type: &str,
        cwd: Option<&str>,
        branch: Option<&str>,
        timestamp: Option<&str>,
        model: Option<&str>,
        input_tokens: Option<i64>,
        output_tokens: Option<i64>,
    ) -> String {
        let mut obj = serde_json::Map::new();
        obj.insert("type".into(), Value::String(msg_type.into()));
        if let Some(cwd) = cwd {
            obj.insert("cwd".into(), Value::String(cwd.into()));
        }
        if let Some(branch) = branch {
            obj.insert("gitBranch".into(), Value::String(branch.into()));
        }
        if let Some(ts) = timestamp {
            obj.insert("timestamp".into(), Value::String(ts.into()));
        }
        if let Some(m) = model {
            obj.insert("model".into(), Value::String(m.into()));
        }
        if msg_type == "assistant" {
            let mut usage = serde_json::Map::new();
            if let Some(inp) = input_tokens {
                usage.insert("input_tokens".into(), Value::Number(inp.into()));
            }
            if let Some(out) = output_tokens {
                usage.insert("output_tokens".into(), Value::Number(out.into()));
            }
            let mut message = serde_json::Map::new();
            message.insert("usage".into(), Value::Object(usage));
            obj.insert("message".into(), Value::Object(message));
        }
        serde_json::to_string(&Value::Object(obj)).unwrap()
    }

    #[test]
    fn test_parse_jsonl_extracts_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let jsonl_path = dir.path().join("test-session.jsonl");
        let mut file = fs::File::create(&jsonl_path).unwrap();

        writeln!(
            file,
            "{}",
            make_jsonl_line(
                "human",
                Some("/home/user/project"),
                Some("main"),
                Some("2025-01-01T00:00:00Z"),
                None,
                None,
                None
            )
        )
        .unwrap();
        writeln!(
            file,
            "{}",
            make_jsonl_line(
                "assistant",
                Some("/home/user/project"),
                Some("main"),
                Some("2025-01-01T00:01:00Z"),
                Some("claude-sonnet-4-5-20250514"),
                Some(100),
                Some(50)
            )
        )
        .unwrap();
        writeln!(
            file,
            "{}",
            make_jsonl_line(
                "assistant",
                Some("/home/user/project"),
                Some("main"),
                Some("2025-01-01T00:02:00Z"),
                Some("claude-sonnet-4-5-20250514"),
                Some(200),
                Some(100)
            )
        )
        .unwrap();

        let session = parse_jsonl(&jsonl_path, "test-session", None).unwrap();
        assert_eq!(session.id, "test-session");
        assert_eq!(session.project_path, "/home/user/project");
        assert_eq!(session.project_name, "project");
        assert_eq!(session.model.as_deref(), Some("claude-sonnet-4-5-20250514"));
        assert_eq!(session.git_branch.as_deref(), Some("main"));
        assert_eq!(session.input_tokens, 300);
        assert_eq!(session.output_tokens, 150);
        assert_eq!(session.started_at.as_deref(), Some("2025-01-01T00:00:00Z"));
        assert_eq!(session.ended_at.as_deref(), Some("2025-01-01T00:02:00Z"));
    }

    #[test]
    fn test_parse_jsonl_with_fallback_path() {
        let dir = tempfile::tempdir().unwrap();
        let jsonl_path = dir.path().join("session.jsonl");
        let mut file = fs::File::create(&jsonl_path).unwrap();

        // No cwd field in any message
        writeln!(
            file,
            "{}",
            make_jsonl_line(
                "assistant",
                None,
                None,
                Some("2025-01-01T00:00:00Z"),
                None,
                Some(50),
                Some(25)
            )
        )
        .unwrap();

        let session = parse_jsonl(&jsonl_path, "session", Some("/fallback/project")).unwrap();
        assert_eq!(session.project_path, "/fallback/project");
        assert_eq!(session.project_name, "project");
    }

    #[test]
    fn test_parse_jsonl_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let jsonl_path = dir.path().join("empty.jsonl");
        fs::File::create(&jsonl_path).unwrap();

        let session = parse_jsonl(&jsonl_path, "empty", None).unwrap();
        assert_eq!(session.input_tokens, 0);
        assert_eq!(session.output_tokens, 0);
        assert!(session.started_at.is_none());
        assert_eq!(session.project_name, "unknown");
    }

    #[test]
    fn test_file_modified_since() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.txt");
        fs::write(&file_path, "data").unwrap();

        // File was just created, so it should be modified since a past timestamp
        assert!(file_modified_since(&file_path, "2020-01-01T00:00:00+00:00"));

        // File should NOT be modified since a future timestamp
        assert!(!file_modified_since(
            &file_path,
            "2030-01-01T00:00:00+00:00"
        ));
    }
}
