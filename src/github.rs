//! GitHub CLI wrapper for sprint board data.
//!
//! Uses `gh` via `std::process::Command` to fetch issues and milestones.
//! The sprint board maps GitHub milestones to sprints and uses issue labels
//! to assign issues to board columns.

use std::process::Command;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// A GitHub issue label.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubLabel {
    pub name: String,
    pub color: Option<String>,
}

/// A GitHub milestone (used as sprint).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubMilestone {
    pub number: i64,
    pub title: String,
    pub state: String,
    #[serde(rename = "dueOn")]
    pub due_on: Option<String>,
}

/// A GitHub user (assignee).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubUser {
    pub login: String,
}

/// A GitHub issue fetched via `gh`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubIssue {
    pub number: i64,
    pub title: String,
    pub body: Option<String>,
    pub state: String,
    pub url: String,
    pub labels: Vec<GitHubLabel>,
    pub assignees: Vec<GitHubUser>,
    pub milestone: Option<GitHubMilestone>,
    #[serde(rename = "createdAt")]
    pub created_at: Option<String>,
}

/// Fetch open issues (and optionally recently closed) from a git repository.
/// Uses `gh issue list --json ...` from the repo directory.
/// If `milestone` is `Some`, filters to that milestone title.
pub fn fetch_issues(repo_path: &str, milestone: Option<&str>) -> Result<Vec<GitHubIssue>> {
    let fields = "number,title,body,state,url,labels,assignees,milestone,createdAt";
    let mut args = vec![
        "issue", "list", "--json", fields, "--limit", "100", "--state", "all",
    ];

    if let Some(ms) = milestone {
        args.extend(["--milestone", ms]);
    }

    let output = Command::new("gh")
        .args(&args)
        .current_dir(repo_path)
        .output()
        .context("failed to run `gh issue list` — is `gh` installed and authenticated?")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("gh issue list failed: {stderr}");
    }

    let issues: Vec<GitHubIssue> =
        serde_json::from_slice(&output.stdout).context("failed to parse gh issue list output")?;

    Ok(issues)
}

/// Fetch milestones from a git repository.
/// Returns open milestones sorted by due date (closest first).
pub fn fetch_milestones(repo_path: &str) -> Result<Vec<GitHubMilestone>> {
    let output = Command::new("gh")
        .args([
            "api",
            "repos/{owner}/{repo}/milestones",
            "--jq",
            ".",
            "--paginate",
        ])
        .current_dir(repo_path)
        .output()
        .context("failed to run `gh api` for milestones")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("gh api milestones failed: {stderr}");
    }

    let mut milestones: Vec<GitHubMilestone> =
        serde_json::from_slice(&output.stdout).context("failed to parse milestones response")?;

    // Sort: open milestones first, then by due date (nearest first).
    milestones.sort_by(|a, b| {
        let a_open = a.state == "open";
        let b_open = b.state == "open";
        b_open.cmp(&a_open).then_with(|| {
            a.due_on
                .as_deref()
                .unwrap_or("9999")
                .cmp(b.due_on.as_deref().unwrap_or("9999"))
        })
    });

    Ok(milestones)
}

/// Get the "current" milestone -- the first open milestone with the nearest due date.
pub fn current_milestone(milestones: &[GitHubMilestone]) -> Option<&GitHubMilestone> {
    milestones.iter().find(|m| m.state == "open")
}

/// Assign an issue to a board column based on its labels.
///
/// Returns the column index (0-based) matching the first label found,
/// or the default column (0 = first column) if no labels match.
/// Closed issues always go to the last column.
pub fn assign_column(issue: &GitHubIssue, column_labels: &[(String, Vec<String>)]) -> usize {
    // Closed issues go to last column (typically "Done").
    if issue.state == "CLOSED" || issue.state == "closed" {
        return column_labels.len().saturating_sub(1);
    }

    let issue_label_names: Vec<String> =
        issue.labels.iter().map(|l| l.name.to_lowercase()).collect();

    for (col_idx, (_name, labels)) in column_labels.iter().enumerate() {
        for label in labels {
            if issue_label_names.contains(&label.to_lowercase()) {
                return col_idx;
            }
        }
    }

    // Default: first column (backlog).
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_issue(state: &str, labels: Vec<&str>) -> GitHubIssue {
        GitHubIssue {
            number: 1,
            title: "test".to_string(),
            body: None,
            state: state.to_string(),
            url: "https://github.com/test/1".to_string(),
            labels: labels
                .into_iter()
                .map(|name| GitHubLabel {
                    name: name.to_string(),
                    color: None,
                })
                .collect(),
            assignees: vec![],
            milestone: None,
            created_at: None,
        }
    }

    #[test]
    fn assign_column_closed_goes_to_last() {
        let columns = vec![
            ("Backlog".to_string(), vec![]),
            ("In Progress".to_string(), vec!["in progress".to_string()]),
            ("Done".to_string(), vec![]),
        ];
        let issue = make_issue("CLOSED", vec![]);
        assert_eq!(assign_column(&issue, &columns), 2);
    }

    #[test]
    fn assign_column_matches_label() {
        let columns = vec![
            ("Backlog".to_string(), vec![]),
            (
                "In Progress".to_string(),
                vec!["in progress".to_string(), "wip".to_string()],
            ),
            ("In Review".to_string(), vec!["in review".to_string()]),
            ("Done".to_string(), vec![]),
        ];
        let issue = make_issue("OPEN", vec!["In Progress"]);
        assert_eq!(assign_column(&issue, &columns), 1);
    }

    #[test]
    fn assign_column_no_match_goes_to_first() {
        let columns = vec![
            ("Backlog".to_string(), vec![]),
            ("In Progress".to_string(), vec!["in progress".to_string()]),
            ("Done".to_string(), vec![]),
        ];
        let issue = make_issue("OPEN", vec!["bug"]);
        assert_eq!(assign_column(&issue, &columns), 0);
    }

    #[test]
    fn assign_column_case_insensitive() {
        let columns = vec![
            ("Backlog".to_string(), vec![]),
            ("In Progress".to_string(), vec!["IN PROGRESS".to_string()]),
            ("Done".to_string(), vec![]),
        ];
        let issue = make_issue("OPEN", vec!["in progress"]);
        assert_eq!(assign_column(&issue, &columns), 1);
    }

    #[test]
    fn current_milestone_returns_first_open() {
        let milestones = vec![
            GitHubMilestone {
                number: 1,
                title: "Sprint 1".to_string(),
                state: "closed".to_string(),
                due_on: Some("2024-01-01".to_string()),
            },
            GitHubMilestone {
                number: 2,
                title: "Sprint 2".to_string(),
                state: "open".to_string(),
                due_on: Some("2024-02-01".to_string()),
            },
            GitHubMilestone {
                number: 3,
                title: "Sprint 3".to_string(),
                state: "open".to_string(),
                due_on: Some("2024-03-01".to_string()),
            },
        ];
        let current = current_milestone(&milestones).expect("should find an open milestone");
        assert_eq!(current.title, "Sprint 2");
    }

    #[test]
    fn current_milestone_returns_none_when_all_closed() {
        let milestones = vec![GitHubMilestone {
            number: 1,
            title: "Sprint 1".to_string(),
            state: "closed".to_string(),
            due_on: None,
        }];
        assert!(current_milestone(&milestones).is_none());
    }
}
