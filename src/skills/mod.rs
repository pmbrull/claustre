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

pub fn parse_list_output(raw: &str, scope: SkillScope) -> Vec<InstalledSkill> {
    let cleaned = strip_ansi(raw);
    let mut skills = Vec::new();
    let mut lines = cleaned.lines().peekable();

    while let Some(line) = lines.next() {
        let trimmed = line.trim();

        if trimmed.is_empty()
            || trimmed == "Global Skills"
            || trimmed == "Project Skills"
            || trimmed.starts_with("No ")
            || trimmed.starts_with("Try ")
        {
            continue;
        }

        if !trimmed.starts_with("Agents:") {
            let parts: Vec<&str> = trimmed.splitn(2, ' ').collect();
            if parts.len() == 2 && !parts[0].is_empty() {
                let name = parts[0].to_string();
                let path = parts[1].to_string();

                let agents = if let Some(next) = lines.peek() {
                    let next_clean = next.trim();
                    if next_clean.starts_with("Agents:") {
                        let agent_str = next_clean.strip_prefix("Agents:").unwrap().trim();
                        lines.next();
                        agent_str
                            .split(',')
                            .map(|a| a.trim().to_string())
                            .filter(|a| !a.is_empty())
                            .collect()
                    } else {
                        vec![]
                    }
                } else {
                    vec![]
                };

                skills.push(InstalledSkill {
                    name,
                    path,
                    agents,
                    scope: scope.clone(),
                });
            }
        }
    }

    skills
}

pub fn parse_find_output(raw: &str) -> Vec<SearchResult> {
    let cleaned = strip_ansi(raw);
    let mut results = Vec::new();
    let mut lines = cleaned.lines().peekable();

    while let Some(line) = lines.next() {
        let trimmed = line.trim();

        if trimmed.contains('/') && trimmed.contains('@') && !trimmed.contains(' ') {
            let package = trimmed.to_string();

            if let Some((owner_repo, skill_name)) = package.split_once('@') {
                let owner_repo = owner_repo.to_string();
                let skill_name = skill_name.to_string();

                let url = if let Some(next) = lines.peek() {
                    let next_clean = next.trim();
                    if next_clean.starts_with("└ ") {
                        let url = next_clean.strip_prefix("└ ").unwrap().to_string();
                        lines.next();
                        url
                    } else {
                        String::new()
                    }
                } else {
                    String::new()
                };

                results.push(SearchResult {
                    package,
                    owner_repo,
                    skill_name,
                    url,
                });
            }
        }
    }

    results
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

    #[test]
    fn test_parse_list_output_global() {
        let raw = "\x1b[1mGlobal Skills\x1b[0m\n\
            \n\
            \x1b[36mfrontend-design\x1b[0m \x1b[38;5;102m~/.agents/skills/frontend-design\x1b[0m\n\
            \x20\x20\x1b[38;5;102mAgents:\x1b[0m Claude Code\n\
            \x1b[36mvalidate-i18n\x1b[0m \x1b[38;5;102m~/.claude/skills/validate-i18n\x1b[0m\n\
            \x20\x20\x1b[38;5;102mAgents:\x1b[0m Claude Code\n";

        let skills = parse_list_output(raw, SkillScope::Global);
        assert_eq!(skills.len(), 2);
        assert_eq!(skills[0].name, "frontend-design");
        assert_eq!(skills[0].path, "~/.agents/skills/frontend-design");
        assert_eq!(skills[0].agents, vec!["Claude Code"]);
        assert_eq!(skills[1].name, "validate-i18n");
    }

    #[test]
    fn test_parse_list_output_not_linked() {
        let raw = "\x1b[1mGlobal Skills\x1b[0m\n\
            \n\
            \x1b[36mfind-skills\x1b[0m \x1b[38;5;102m~/.agents/skills/find-skills\x1b[0m\n\
            \x20\x20\x1b[38;5;102mAgents:\x1b[0m \x1b[33mnot linked\x1b[0m\n";

        let skills = parse_list_output(raw, SkillScope::Global);
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "find-skills");
        assert_eq!(skills[0].agents, vec!["not linked"]);
    }

    #[test]
    fn test_parse_list_output_empty() {
        let raw = "\x1b[38;5;102mNo project skills found.\x1b[0m\n\
            \x1b[38;5;102mTry listing global skills with -g\x1b[0m\n";

        let skills = parse_list_output(raw, SkillScope::Global);
        assert_eq!(skills.len(), 0);
    }

    #[test]
    fn test_parse_find_output() {
        let raw = "\x1b[38;5;250m███████╗██╗  ██╗██╗██╗     ██╗     ███████╗\x1b[0m\n\
            \x1b[38;5;248m██╔════╝██║ ██╔╝██║██║     ██║     ██╔════╝\x1b[0m\n\
            \x1b[38;5;245m███████╗█████╔╝ ██║██║     ██║     ███████╗\x1b[0m\n\
            \x1b[38;5;243m╚════██║██╔═██╗ ██║██║     ██║     ╚════██║\x1b[0m\n\
            \x1b[38;5;240m███████║██║  ██╗██║███████╗███████╗███████║\x1b[0m\n\
            \x1b[38;5;238m╚══════╝╚═╝  ╚═╝╚═╝╚══════╝╚══════╝╚══════╝\x1b[0m\n\
            \n\
            \x1b[38;5;102mInstall with\x1b[0m npx skills add <owner/repo@skill>\n\
            \n\
            \x1b[38;5;145manthropics/skills@frontend-design\x1b[0m\n\
            \x1b[38;5;102m└ https://skills.sh/anthropics/skills/frontend-design\x1b[0m\n\
            \n\
            \x1b[38;5;145mlanggenius/dify@frontend-code-review\x1b[0m\n\
            \x1b[38;5;102m└ https://skills.sh/langgenius/dify/frontend-code-review\x1b[0m\n";

        let results = parse_find_output(raw);
        assert_eq!(results.len(), 2);

        assert_eq!(results[0].package, "anthropics/skills@frontend-design");
        assert_eq!(results[0].owner_repo, "anthropics/skills");
        assert_eq!(results[0].skill_name, "frontend-design");
        assert_eq!(results[0].url, "https://skills.sh/anthropics/skills/frontend-design");

        assert_eq!(results[1].package, "langgenius/dify@frontend-code-review");
        assert_eq!(results[1].skill_name, "frontend-code-review");
    }

    #[test]
    fn test_parse_find_output_empty() {
        let raw = "\x1b[38;5;102mNo skills found for \"xyznonexistent\"\x1b[0m\n";
        let results = parse_find_output(raw);
        assert_eq!(results.len(), 0);
    }
}
