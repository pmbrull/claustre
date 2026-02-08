use std::process::Command;

use anyhow::{Context, Result, bail};
use regex::Regex;

/// Scope for skill installation
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillScope {
    Global,
    Project(String),
}

/// An installed skill parsed from `npx skills list` output
#[derive(Debug, Clone)]
pub struct InstalledSkill {
    pub name: String,
    pub path: String,
    pub agents: Vec<String>,
    pub scope: SkillScope,
}

/// A search result parsed from `npx skills find` output
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub package: String,
    pub owner_repo: String,
    pub skill_name: String,
    pub url: String,
}

/// Strip ANSI escape codes from a string
pub fn strip_ansi(input: &str) -> String {
    let re = Regex::new(r"\x1b\[[0-9;]*[a-zA-Z]|\x1b\][^\x07]*\x07|\x1b\[\?[0-9]*[hl]|\[999D|\[J")
        .unwrap();
    re.replace_all(input, "").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_ansi_colors() {
        let input = "\x1b[36magent-browser\x1b[0m \x1b[38;5;102m~/.agents/skills/agent-browser\x1b[0m";
        let result = strip_ansi(input);
        assert_eq!(result, "agent-browser ~/.agents/skills/agent-browser");
    }

    #[test]
    fn test_strip_ansi_bold() {
        let input = "\x1b[1mGlobal Skills\x1b[0m";
        let result = strip_ansi(input);
        assert_eq!(result, "Global Skills");
    }

    #[test]
    fn test_strip_ansi_plain_text() {
        let input = "no escape codes here";
        let result = strip_ansi(input);
        assert_eq!(result, "no escape codes here");
    }

    #[test]
    fn test_strip_ansi_not_linked() {
        let input = "  \x1b[38;5;102mAgents:\x1b[0m \x1b[33mnot linked\x1b[0m";
        let result = strip_ansi(input);
        assert_eq!(result, "  Agents: not linked");
    }
}
