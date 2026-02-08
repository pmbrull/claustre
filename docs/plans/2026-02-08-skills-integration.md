# Skills.sh Integration Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add a Skills view to the claustre TUI and CLI subcommands for browsing, installing, removing, and updating agent skills via skills.sh (`npx skills`).

**Architecture:** New `src/skills/mod.rs` module wraps `npx skills` CLI commands, parses their ANSI output into Rust structs. The TUI gets a third view (`Skills`) in the Tab cycle. CLI gets a `skills` subcommand group. No database changes — skills are files on disk.

**Tech Stack:** Existing stack (ratatui, crossterm, clap, tokio, anyhow). New: `regex` crate for ANSI stripping.

---

### Task 1: Add regex dependency

**Files:**
- Modify: `Cargo.toml`

**Step 1: Add regex to Cargo.toml**

In `Cargo.toml`, add under `[dependencies]`:

```toml
regex = "1"
```

Place it in the Utilities section after `anyhow = "1"`.

**Step 2: Verify it compiles**

Run: `cargo check`
Expected: compiles with no errors (just downloads regex)

**Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "deps: add regex crate for ANSI stripping"
```

---

### Task 2: Skills module — data types and ANSI stripping

**Files:**
- Create: `src/skills/mod.rs`

**Step 1: Create the skills module with types and strip_ansi**

Create `src/skills/mod.rs`:

```rust
use std::process::Command;

use anyhow::{Context, Result, bail};
use regex::Regex;

/// Scope for skill installation
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillScope {
    Global,
    Project(String), // project repo_path
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
    pub package: String,    // e.g. "anthropics/skills@frontend-design"
    pub owner_repo: String, // e.g. "anthropics/skills"
    pub skill_name: String, // e.g. "frontend-design"
    pub url: String,        // e.g. "https://skills.sh/..."
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
```

**Step 2: Register the module in main.rs**

In `src/main.rs`, add after `mod mcp;`:

```rust
mod skills;
```

**Step 3: Run tests**

Run: `cargo test skills::tests`
Expected: all 4 tests pass

**Step 4: Commit**

```bash
git add src/skills/mod.rs src/main.rs
git commit -m "feat(skills): add module with types and ANSI stripping"
```

---

### Task 3: Skills module — parse list output

**Files:**
- Modify: `src/skills/mod.rs`

**Step 1: Write tests for parse_list_output**

Add to the `tests` module in `src/skills/mod.rs`:

```rust
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
```

**Step 2: Run tests to verify they fail**

Run: `cargo test skills::tests::test_parse_list`
Expected: FAIL — `parse_list_output` not defined

**Step 3: Implement parse_list_output**

Add this function to `src/skills/mod.rs` (before the `tests` module):

```rust
/// Parse output from `npx skills list` into InstalledSkill structs.
///
/// The output format (after ANSI stripping) is:
/// ```text
/// Global Skills
///
/// frontend-design ~/.agents/skills/frontend-design
///   Agents: Claude Code
/// validate-i18n ~/.claude/skills/validate-i18n
///   Agents: Claude Code
/// ```
pub fn parse_list_output(raw: &str, scope: SkillScope) -> Vec<InstalledSkill> {
    let cleaned = strip_ansi(raw);
    let mut skills = Vec::new();
    let mut lines = cleaned.lines().peekable();

    while let Some(line) = lines.next() {
        let trimmed = line.trim();

        // Skip headers and empty lines
        if trimmed.is_empty()
            || trimmed == "Global Skills"
            || trimmed == "Project Skills"
            || trimmed.starts_with("No ")
            || trimmed.starts_with("Try ")
        {
            continue;
        }

        // A skill line has a name and a path separated by space
        // e.g. "frontend-design ~/.agents/skills/frontend-design"
        if !trimmed.starts_with("Agents:") {
            let parts: Vec<&str> = trimmed.splitn(2, ' ').collect();
            if parts.len() == 2 && !parts[0].is_empty() {
                let name = parts[0].to_string();
                let path = parts[1].to_string();

                // Next line should be the agents line
                let agents = if let Some(next) = lines.peek() {
                    let next_clean = next.trim();
                    if next_clean.starts_with("Agents:") {
                        let agent_str = next_clean.strip_prefix("Agents:").unwrap().trim();
                        lines.next(); // consume it
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
```

**Step 4: Run tests to verify they pass**

Run: `cargo test skills::tests::test_parse_list`
Expected: all 3 tests pass

**Step 5: Commit**

```bash
git add src/skills/mod.rs
git commit -m "feat(skills): parse npx skills list output"
```

---

### Task 4: Skills module — parse find output

**Files:**
- Modify: `src/skills/mod.rs`

**Step 1: Write tests for parse_find_output**

Add to the `tests` module:

```rust
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
```

**Step 2: Run tests to verify they fail**

Run: `cargo test skills::tests::test_parse_find`
Expected: FAIL — `parse_find_output` not defined

**Step 3: Implement parse_find_output**

Add to `src/skills/mod.rs`:

```rust
/// Parse output from `npx skills find <query>` into SearchResult structs.
///
/// The output format (after ANSI stripping) is:
/// ```text
/// [ASCII art banner...]
///
/// Install with npx skills add <owner/repo@skill>
///
/// anthropics/skills@frontend-design
/// └ https://skills.sh/anthropics/skills/frontend-design
///
/// langgenius/dify@frontend-code-review
/// └ https://skills.sh/langgenius/dify/frontend-code-review
/// ```
pub fn parse_find_output(raw: &str) -> Vec<SearchResult> {
    let cleaned = strip_ansi(raw);
    let mut results = Vec::new();
    let mut lines = cleaned.lines().peekable();

    while let Some(line) = lines.next() {
        let trimmed = line.trim();

        // Look for lines matching "owner/repo@skill-name"
        if trimmed.contains('/') && trimmed.contains('@') && !trimmed.contains(' ') {
            let package = trimmed.to_string();

            // Split into owner_repo and skill_name
            if let Some((owner_repo, skill_name)) = package.split_once('@') {
                // Next line should be the URL
                let url = if let Some(next) = lines.peek() {
                    let next_clean = next.trim();
                    if next_clean.starts_with("└ ") {
                        let url = next_clean.strip_prefix("└ ").unwrap().to_string();
                        lines.next(); // consume
                        url
                    } else {
                        String::new()
                    }
                } else {
                    String::new()
                };

                results.push(SearchResult {
                    package,
                    owner_repo: owner_repo.to_string(),
                    skill_name: skill_name.to_string(),
                    url,
                });
            }
        }
    }

    results
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test skills::tests::test_parse_find`
Expected: both tests pass

**Step 5: Commit**

```bash
git add src/skills/mod.rs
git commit -m "feat(skills): parse npx skills find output"
```

---

### Task 5: Skills module — command runners

**Files:**
- Modify: `src/skills/mod.rs`

**Step 1: Implement the command runner functions**

Add to `src/skills/mod.rs` (these shell out to `npx skills` so they're not unit-testable without mocking, but are straightforward wrappers):

```rust
/// Run `npx skills list` and return installed skills.
/// If `global` is true, uses `-g` flag. Otherwise runs in the given project directory.
pub fn list_skills(global: bool, project_path: Option<&str>) -> Result<Vec<InstalledSkill>> {
    let mut cmd = Command::new("npx");
    cmd.args(["skills", "list", "-a", "claude-code"]);

    if global {
        cmd.arg("-g");
    }

    if let Some(path) = project_path {
        cmd.current_dir(path);
    }

    let scope = if global {
        SkillScope::Global
    } else {
        SkillScope::Project(project_path.unwrap_or(".").to_string())
    };

    let output = cmd.output().context("failed to run npx skills list")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_list_output(&stdout, scope))
}

/// Run `npx skills find <query>` and return search results.
pub fn find_skills(query: &str) -> Result<Vec<SearchResult>> {
    let output = Command::new("npx")
        .args(["skills", "find", query])
        .output()
        .context("failed to run npx skills find")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_find_output(&stdout))
}

/// Install a skill. `package` is e.g. "anthropics/skills@frontend-design".
pub fn add_skill(package: &str, global: bool, project_path: Option<&str>) -> Result<String> {
    let mut cmd = Command::new("npx");
    cmd.args(["skills", "add", package, "-a", "claude-code", "-y"]);

    if global {
        cmd.arg("-g");
    }

    if let Some(path) = project_path {
        cmd.current_dir(path);
    }

    let output = cmd.output().context("failed to run npx skills add")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if !output.status.success() {
        bail!("npx skills add failed: {}{}", stdout, stderr);
    }

    Ok(strip_ansi(&stdout))
}

/// Remove an installed skill by name.
pub fn remove_skill(name: &str, global: bool, project_path: Option<&str>) -> Result<String> {
    let mut cmd = Command::new("npx");
    cmd.args(["skills", "remove", name, "-a", "claude-code", "-y"]);

    if global {
        cmd.arg("-g");
    }

    if let Some(path) = project_path {
        cmd.current_dir(path);
    }

    let output = cmd.output().context("failed to run npx skills remove")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if !output.status.success() {
        bail!("npx skills remove failed: {}{}", stdout, stderr);
    }

    Ok(strip_ansi(&stdout))
}

/// Update all installed skills.
pub fn update_skills() -> Result<String> {
    let output = Command::new("npx")
        .args(["skills", "update"])
        .output()
        .context("failed to run npx skills update")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(strip_ansi(&stdout))
}

/// Read the SKILL.md content for a given skill path.
pub fn read_skill_md(skill_path: &str) -> Result<String> {
    // Expand ~ to home dir
    let expanded = if skill_path.starts_with("~/") {
        if let Some(home) = dirs::home_dir() {
            skill_path.replacen("~", home.to_str().unwrap_or(""), 1)
        } else {
            skill_path.to_string()
        }
    } else {
        skill_path.to_string()
    };

    let md_path = std::path::Path::new(&expanded).join("SKILL.md");
    std::fs::read_to_string(&md_path)
        .with_context(|| format!("failed to read {}", md_path.display()))
}
```

**Step 2: Verify it compiles**

Run: `cargo check`
Expected: compiles with no errors

**Step 3: Commit**

```bash
git add src/skills/mod.rs
git commit -m "feat(skills): add command runners for npx skills CLI"
```

---

### Task 6: TUI app state — add Skills view

**Files:**
- Modify: `src/tui/app.rs`

**Step 1: Add View::Skills and new input modes**

In `src/tui/app.rs`, modify the `View` enum:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Active,
    History,
    Skills,
}
```

Modify the `InputMode` enum:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    Normal,
    NewTask,
    NewSession,
    CommandPalette,
    SkillSearch,
    SkillAdd,
}
```

Add `PaletteAction` variants:

```rust
#[derive(Debug, Clone)]
pub enum PaletteAction {
    NewTask,
    NewSession,
    ToggleView,
    FocusProjects,
    FocusSessions,
    FocusTasks,
    FindSkills,
    UpdateSkills,
    Quit,
}
```

**Step 2: Add skill fields to App struct**

Add these fields to the `App` struct (after `palette_index`):

```rust
    // Skills state
    pub installed_skills: Vec<crate::skills::InstalledSkill>,
    pub search_results: Vec<crate::skills::SearchResult>,
    pub skill_index: usize,
    pub skill_scope_global: bool,
    pub skill_detail_content: String,
    pub skill_status_message: String,
```

**Step 3: Initialize skills state in App::new()**

In the `App::new()` constructor, add the skill fields to the return value and the palette items. Add "Find Skills" and "Update Skills" to `palette_items`. Initialize the skills fields:

```rust
    installed_skills: vec![],
    search_results: vec![],
    skill_index: 0,
    skill_scope_global: true,
    skill_detail_content: String::new(),
    skill_status_message: String::new(),
```

Add palette items:

```rust
PaletteItem { label: "Find Skills".into(), action: PaletteAction::FindSkills },
PaletteItem { label: "Update Skills".into(), action: PaletteAction::UpdateSkills },
```

**Step 4: Update Tab cycling in handle_normal_key**

Change the Tab/h handler to cycle through 3 views:

```rust
// View toggle
(KeyCode::Char('h'), _) | (KeyCode::Tab, _) => {
    self.view = match self.view {
        View::Active => View::History,
        View::History => View::Skills,
        View::Skills => View::Active,
    };
    if self.view == View::Skills {
        self.refresh_skills();
    }
}
```

**Step 5: Add refresh_skills method**

```rust
pub fn refresh_skills(&mut self) {
    // Load global skills
    let mut all_skills = crate::skills::list_skills(true, None).unwrap_or_default();

    // Load project-level skills for the selected project
    if let Some(project) = self.selected_project() {
        let project_skills = crate::skills::list_skills(false, Some(&project.repo_path))
            .unwrap_or_default();
        all_skills.extend(project_skills);
    }

    self.installed_skills = all_skills;

    // Clamp index
    if self.skill_index >= self.installed_skills.len() && !self.installed_skills.is_empty() {
        self.skill_index = self.installed_skills.len() - 1;
    }

    // Load detail for selected skill
    self.refresh_skill_detail();
}

fn refresh_skill_detail(&mut self) {
    if let Some(skill) = self.installed_skills.get(self.skill_index) {
        self.skill_detail_content = crate::skills::read_skill_md(&skill.path)
            .unwrap_or_else(|_| "Could not read SKILL.md".to_string());
    } else {
        self.skill_detail_content.clear();
    }
}
```

**Step 6: Add palette action handlers for FindSkills and UpdateSkills**

In `execute_palette_action()`:

```rust
PaletteAction::FindSkills => {
    self.view = View::Skills;
    self.input_mode = InputMode::SkillSearch;
    self.input_buffer.clear();
    self.search_results.clear();
}
PaletteAction::UpdateSkills => {
    self.skill_status_message = "Updating skills...".to_string();
    match crate::skills::update_skills() {
        Ok(msg) => {
            self.skill_status_message = msg;
            self.refresh_skills();
        }
        Err(e) => {
            self.skill_status_message = format!("Update failed: {}", e);
        }
    }
}
```

**Step 7: Verify it compiles**

Run: `cargo check`
Expected: compiles (UI drawing not yet wired, but that's OK)

**Step 8: Commit**

```bash
git add src/tui/app.rs
git commit -m "feat(tui): add Skills view state and Tab cycling"
```

---

### Task 7: TUI app — skills keybindings

**Files:**
- Modify: `src/tui/app.rs`

**Step 1: Add handle_skills_key method**

Add this method to the `App` impl block:

```rust
fn handle_skills_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> Result<()> {
    match (code, modifiers) {
        (KeyCode::Char('q'), _) => self.should_quit = true,
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => self.should_quit = true,

        (KeyCode::Char('p'), KeyModifiers::CONTROL) => {
            self.input_mode = InputMode::CommandPalette;
            self.input_buffer.clear();
            self.palette_index = 0;
            self.filter_palette();
        }

        // View toggle
        (KeyCode::Tab, _) => {
            self.view = View::Active;
        }

        // Navigate skill list
        (KeyCode::Char('j'), _) | (KeyCode::Down, _) => {
            if self.input_mode == InputMode::Normal {
                if !self.installed_skills.is_empty() {
                    self.skill_index =
                        (self.skill_index + 1).min(self.installed_skills.len() - 1);
                    self.refresh_skill_detail();
                }
            }
        }
        (KeyCode::Char('k'), _) | (KeyCode::Up, _) => {
            if self.input_mode == InputMode::Normal {
                self.skill_index = self.skill_index.saturating_sub(1);
                self.refresh_skill_detail();
            }
        }

        // Find skills (search mode)
        (KeyCode::Char('f'), _) if self.input_mode == InputMode::Normal => {
            self.input_mode = InputMode::SkillSearch;
            self.input_buffer.clear();
            self.search_results.clear();
            self.skill_index = 0;
        }

        // Add skill by typing owner/repo@skill
        (KeyCode::Char('a'), _) if self.input_mode == InputMode::Normal => {
            self.input_mode = InputMode::SkillAdd;
            self.input_buffer.clear();
        }

        // Remove selected skill
        (KeyCode::Char('x'), _) if self.input_mode == InputMode::Normal => {
            if let Some(skill) = self.installed_skills.get(self.skill_index) {
                let name = skill.name.clone();
                let global = skill.scope == crate::skills::SkillScope::Global;
                let project_path = if let crate::skills::SkillScope::Project(ref p) = skill.scope {
                    Some(p.clone())
                } else {
                    None
                };

                match crate::skills::remove_skill(
                    &name,
                    global,
                    project_path.as_deref(),
                ) {
                    Ok(msg) => {
                        self.skill_status_message = format!("Removed {}", name);
                        self.refresh_skills();
                    }
                    Err(e) => {
                        self.skill_status_message = format!("Remove failed: {}", e);
                    }
                }
            }
        }

        // Update all skills
        (KeyCode::Char('u'), _) if self.input_mode == InputMode::Normal => {
            self.skill_status_message = "Updating skills...".to_string();
            match crate::skills::update_skills() {
                Ok(_) => {
                    self.skill_status_message = "Skills updated".to_string();
                    self.refresh_skills();
                }
                Err(e) => {
                    self.skill_status_message = format!("Update failed: {}", e);
                }
            }
        }

        // Toggle scope
        (KeyCode::Char('g'), _) if self.input_mode == InputMode::Normal => {
            self.skill_scope_global = !self.skill_scope_global;
        }

        _ => {}
    }
    Ok(())
}
```

**Step 2: Add handle_skill_search_key and handle_skill_add_key methods**

```rust
fn handle_skill_search_key(&mut self, code: KeyCode) -> Result<()> {
    match code {
        KeyCode::Enter => {
            if !self.input_buffer.is_empty() {
                // If we have search results and an item is selected, install it
                if !self.search_results.is_empty() {
                    if let Some(result) = self.search_results.get(self.skill_index) {
                        let package = result.package.clone();
                        let global = self.skill_scope_global;
                        let project_path = if !global {
                            self.selected_project().map(|p| p.repo_path.clone())
                        } else {
                            None
                        };

                        self.skill_status_message = format!("Installing {}...", package);
                        match crate::skills::add_skill(
                            &package,
                            global,
                            project_path.as_deref(),
                        ) {
                            Ok(_) => {
                                self.skill_status_message = format!("Installed {}", package);
                                self.input_mode = InputMode::Normal;
                                self.input_buffer.clear();
                                self.search_results.clear();
                                self.refresh_skills();
                            }
                            Err(e) => {
                                self.skill_status_message = format!("Install failed: {}", e);
                            }
                        }
                    }
                } else {
                    // Run the search
                    let query = self.input_buffer.clone();
                    self.skill_status_message = format!("Searching for '{}'...", query);
                    match crate::skills::find_skills(&query) {
                        Ok(results) => {
                            self.skill_status_message = format!("Found {} results", results.len());
                            self.search_results = results;
                            self.skill_index = 0;
                        }
                        Err(e) => {
                            self.skill_status_message = format!("Search failed: {}", e);
                        }
                    }
                }
            }
        }
        KeyCode::Esc => {
            self.input_buffer.clear();
            self.search_results.clear();
            self.input_mode = InputMode::Normal;
            self.skill_status_message.clear();
        }
        KeyCode::Char(c) => {
            self.input_buffer.push(c);
            // Clear results when query changes so Enter triggers search again
            self.search_results.clear();
        }
        KeyCode::Backspace => {
            self.input_buffer.pop();
            self.search_results.clear();
        }
        KeyCode::Down => {
            if !self.search_results.is_empty() {
                self.skill_index =
                    (self.skill_index + 1).min(self.search_results.len() - 1);
            }
        }
        KeyCode::Up => {
            self.skill_index = self.skill_index.saturating_sub(1);
        }
        _ => {}
    }
    Ok(())
}

fn handle_skill_add_key(&mut self, code: KeyCode) -> Result<()> {
    match code {
        KeyCode::Enter => {
            if !self.input_buffer.is_empty() {
                let package = self.input_buffer.clone();
                let global = self.skill_scope_global;
                let project_path = if !global {
                    self.selected_project().map(|p| p.repo_path.clone())
                } else {
                    None
                };

                self.skill_status_message = format!("Installing {}...", package);
                match crate::skills::add_skill(&package, global, project_path.as_deref()) {
                    Ok(_) => {
                        self.skill_status_message = format!("Installed {}", package);
                        self.input_mode = InputMode::Normal;
                        self.input_buffer.clear();
                        self.refresh_skills();
                    }
                    Err(e) => {
                        self.skill_status_message = format!("Install failed: {}", e);
                    }
                }
            }
        }
        KeyCode::Esc => {
            self.input_buffer.clear();
            self.input_mode = InputMode::Normal;
        }
        KeyCode::Char(c) => {
            self.input_buffer.push(c);
        }
        KeyCode::Backspace => {
            self.input_buffer.pop();
        }
        _ => {}
    }
    Ok(())
}
```

**Step 3: Wire the new handlers into the event loop**

In the `run()` method, update the match on `input_mode`:

```rust
match self.input_mode {
    InputMode::Normal => {
        if self.view == View::Skills {
            self.handle_skills_key(key.code, key.modifiers)?;
        } else {
            self.handle_normal_key(key.code, key.modifiers)?;
        }
    }
    InputMode::NewTask => self.handle_input_key(key.code)?,
    InputMode::NewSession => self.handle_session_input_key(key.code)?,
    InputMode::CommandPalette => self.handle_palette_key(key.code)?,
    InputMode::SkillSearch => self.handle_skill_search_key(key.code)?,
    InputMode::SkillAdd => self.handle_skill_add_key(key.code)?,
}
```

**Step 4: Verify it compiles**

Run: `cargo check`
Expected: compiles

**Step 5: Commit**

```bash
git add src/tui/app.rs
git commit -m "feat(tui): add skills keybindings and input handlers"
```

---

### Task 8: TUI UI — draw_skills view

**Files:**
- Modify: `src/tui/ui.rs`

**Step 1: Add draw dispatch for Skills view**

In `draw()` function, update the match:

```rust
pub fn draw(frame: &mut Frame, app: &App) {
    match app.view {
        View::Active => draw_active(frame, app),
        View::History => draw_history(frame, app),
        View::Skills => draw_skills(frame, app),
    }

    // Overlay command palette if active
    if app.input_mode == InputMode::CommandPalette {
        draw_command_palette(frame, app);
    }
}
```

**Step 2: Implement draw_skills**

Add the following functions to `src/tui/ui.rs`:

```rust
fn draw_skills(frame: &mut Frame, app: &App) {
    let size = frame.area();

    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0), Constraint::Length(1)])
        .split(size);

    // Title bar
    let scope_label = if app.skill_scope_global {
        "global"
    } else {
        "project"
    };
    let title = Line::from(vec![
        Span::styled(
            " claustre — skills ",
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ),
        Span::raw("                              "),
        Span::styled(
            format!("Tab:active  g:scope [{}]  q:quit", scope_label),
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    frame.render_widget(Paragraph::new(title), outer[0]);

    // Main area: left panel | right panel
    let main = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
        .split(outer[1]);

    // Left: skill list or search results
    if app.input_mode == InputMode::SkillSearch {
        draw_skill_search(frame, app, main[0]);
    } else {
        draw_installed_skills(frame, app, main[0]);
    }

    // Right: skill detail
    draw_skill_detail(frame, app, main[1]);

    // Status bar
    let status = match app.input_mode {
        InputMode::SkillSearch => {
            if app.search_results.is_empty() {
                Line::from(vec![
                    Span::styled(" Search: ", Style::default().fg(Color::Yellow)),
                    Span::raw(&app.input_buffer),
                    Span::styled("█", Style::default().fg(Color::Yellow)),
                    Span::styled(
                        "  (Enter to search, Esc to cancel)",
                        Style::default().fg(Color::DarkGray),
                    ),
                ])
            } else {
                Line::from(vec![
                    Span::styled(
                        format!(" {} results ", app.search_results.len()),
                        Style::default().fg(Color::Green),
                    ),
                    Span::styled(
                        " j/k:navigate  Enter:install  Esc:back",
                        Style::default().fg(Color::DarkGray),
                    ),
                ])
            }
        }
        InputMode::SkillAdd => {
            Line::from(vec![
                Span::styled(" Package: ", Style::default().fg(Color::Green)),
                Span::raw(&app.input_buffer),
                Span::styled("█", Style::default().fg(Color::Green)),
                Span::styled(
                    "  (owner/repo@skill, Enter to install, Esc to cancel)",
                    Style::default().fg(Color::DarkGray),
                ),
            ])
        }
        _ => {
            if !app.skill_status_message.is_empty() {
                Line::from(Span::styled(
                    format!(" {} ", app.skill_status_message),
                    Style::default().fg(Color::Yellow),
                ))
            } else {
                Line::from(Span::styled(
                    " f:find  a:add  x:remove  u:update  g:scope  j/k:navigate",
                    Style::default().fg(Color::DarkGray),
                ))
            }
        }
    };
    frame.render_widget(Paragraph::new(status), outer[2]);
}

fn draw_installed_skills(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .title(" Installed Skills ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    if app.installed_skills.is_empty() {
        let msg = Paragraph::new("  No skills installed.\n  Press 'f' to find or 'a' to add.")
            .style(Style::default().fg(Color::DarkGray))
            .block(block);
        frame.render_widget(msg, area);
        return;
    }

    let mut items: Vec<ListItem> = Vec::new();
    let mut current_scope: Option<&crate::skills::SkillScope> = None;

    for (i, skill) in app.installed_skills.iter().enumerate() {
        // Group header when scope changes
        let scope_changed = current_scope.map_or(true, |s| s != &skill.scope);
        if scope_changed {
            let header = match &skill.scope {
                crate::skills::SkillScope::Global => "── Global ──".to_string(),
                crate::skills::SkillScope::Project(p) => {
                    // Show just the last path component
                    let name = std::path::Path::new(p)
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| p.clone());
                    format!("── {} ──", name)
                }
            };
            items.push(ListItem::new(Line::from(
                Span::styled(format!("  {}", header), Style::default().fg(Color::DarkGray)),
            )));
            current_scope = Some(&skill.scope);
        }

        let is_selected = i == app.skill_index;
        let style = if is_selected {
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };

        let prefix = if is_selected { "▸ " } else { "  " };
        let prefix_style = if is_selected {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default()
        };

        items.push(ListItem::new(Line::from(vec![
            Span::styled(prefix, prefix_style),
            Span::styled(&skill.name, style),
        ])));
    }

    let list = List::new(items).block(block);
    frame.render_widget(list, area);
}

fn draw_skill_search(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .title(" Search Skills ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height < 2 {
        return;
    }

    // Search input line
    let input_area = Rect::new(inner.x, inner.y, inner.width, 1);
    let input_line = Line::from(vec![
        Span::styled("> ", Style::default().fg(Color::Yellow)),
        Span::raw(&app.input_buffer),
        Span::styled("█", Style::default().fg(Color::Yellow)),
    ]);
    frame.render_widget(Paragraph::new(input_line), input_area);

    // Search results
    if !app.search_results.is_empty() {
        let results_area = Rect::new(
            inner.x,
            inner.y + 1,
            inner.width,
            inner.height.saturating_sub(1),
        );

        let items: Vec<ListItem> = app
            .search_results
            .iter()
            .enumerate()
            .map(|(i, result)| {
                let is_selected = i == app.skill_index;
                let style = if is_selected {
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                };
                let prefix = if is_selected { "▸ " } else { "  " };

                ListItem::new(Line::from(vec![
                    Span::styled(prefix, style),
                    Span::styled(&result.package, style),
                ]))
            })
            .collect();

        frame.render_widget(List::new(items), results_area);
    } else if !app.input_buffer.is_empty() {
        let msg_area = Rect::new(
            inner.x,
            inner.y + 1,
            inner.width,
            inner.height.saturating_sub(1),
        );
        let msg = Paragraph::new("  Press Enter to search")
            .style(Style::default().fg(Color::DarkGray));
        frame.render_widget(msg, msg_area);
    }
}

fn draw_skill_detail(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .title(" Skill Detail ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    // In search mode, show search result detail
    if app.input_mode == InputMode::SkillSearch && !app.search_results.is_empty() {
        if let Some(result) = app.search_results.get(app.skill_index) {
            let lines = vec![
                Line::from(vec![
                    Span::styled("  Repo: ", Style::default().fg(Color::DarkGray)),
                    Span::styled(&result.owner_repo, Style::default().fg(Color::White)),
                ]),
                Line::from(vec![
                    Span::styled("  Skill: ", Style::default().fg(Color::DarkGray)),
                    Span::styled(&result.skill_name, Style::default().fg(Color::Cyan)),
                ]),
                Line::from(""),
                Line::from(vec![
                    Span::styled("  URL: ", Style::default().fg(Color::DarkGray)),
                    Span::styled(&result.url, Style::default().fg(Color::Blue)),
                ]),
                Line::from(""),
                Line::from(vec![
                    Span::styled("  Install: ", Style::default().fg(Color::DarkGray)),
                    Span::styled(
                        format!("npx skills add {}", result.package),
                        Style::default().fg(Color::Green),
                    ),
                ]),
                Line::from(""),
                Line::from(Span::styled(
                    "  Press Enter to install",
                    Style::default().fg(Color::Yellow),
                )),
            ];

            let detail = Paragraph::new(lines).block(block).wrap(Wrap { trim: false });
            frame.render_widget(detail, area);
            return;
        }
    }

    // In installed mode, show SKILL.md content
    if let Some(skill) = app.installed_skills.get(app.skill_index) {
        let mut lines = vec![
            Line::from(vec![
                Span::styled("  Name: ", Style::default().fg(Color::DarkGray)),
                Span::styled(&skill.name, Style::default().fg(Color::Cyan)),
            ]),
            Line::from(vec![
                Span::styled("  Path: ", Style::default().fg(Color::DarkGray)),
                Span::styled(&skill.path, Style::default().fg(Color::White)),
            ]),
            Line::from(vec![
                Span::styled("  Agents: ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    skill.agents.join(", "),
                    Style::default().fg(Color::White),
                ),
            ]),
            Line::from(""),
        ];

        // Truncate SKILL.md content preview
        for md_line in app.skill_detail_content.lines().take(20) {
            lines.push(Line::from(Span::styled(
                format!("  {}", md_line),
                Style::default().fg(Color::White),
            )));
        }

        if app.skill_detail_content.lines().count() > 20 {
            lines.push(Line::from(Span::styled(
                "  ...",
                Style::default().fg(Color::DarkGray),
            )));
        }

        let detail = Paragraph::new(lines).block(block).wrap(Wrap { trim: false });
        frame.render_widget(detail, area);
    } else {
        let msg = Paragraph::new("  No skill selected")
            .style(Style::default().fg(Color::DarkGray))
            .block(block);
        frame.render_widget(msg, area);
    }
}
```

**Step 3: Update the import for InputMode at the top of ui.rs**

The existing import line should already cover it since it imports `InputMode` from `app`. Just make sure the `View::Skills` variant is handled. No new imports needed — but `InputMode::SkillSearch` and `InputMode::SkillAdd` need to be covered in the pattern match for the status bar, which is already done in `draw_skills`.

**Step 4: Verify it compiles**

Run: `cargo check`
Expected: compiles

**Step 5: Commit**

```bash
git add src/tui/ui.rs
git commit -m "feat(tui): add skills view rendering with search and detail panels"
```

---

### Task 9: CLI subcommands

**Files:**
- Modify: `src/main.rs`

**Step 1: Add the Skills subcommand group**

Add a new variant to the `Commands` enum in `src/main.rs`:

```rust
/// Manage agent skills (skills.sh integration)
Skills {
    #[command(subcommand)]
    action: Option<SkillsAction>,
},
```

Add the `SkillsAction` enum after `Commands`:

```rust
#[derive(Subcommand)]
enum SkillsAction {
    /// Search for skills on skills.sh
    Find {
        /// Search query
        query: String,
    },
    /// Add a skill package
    Add {
        /// Package (e.g. owner/repo or owner/repo@skill)
        package: String,
        /// Install globally (default) or to a project
        #[arg(short, long)]
        project: Option<String>,
    },
    /// Remove an installed skill
    Remove {
        /// Skill name to remove
        name: String,
        /// Remove from project instead of global
        #[arg(short, long)]
        project: Option<String>,
    },
    /// Update all installed skills
    Update,
}
```

**Step 2: Add the handler in the main match**

Add the following arm to the main match:

```rust
Commands::Skills { action } => {
    match action {
        None => {
            // Default: list installed skills
            println!("Global skills:");
            let global = skills::list_skills(true, None)?;
            if global.is_empty() {
                println!("  (none)");
            } else {
                for s in &global {
                    println!("  {} — {}", s.name, s.path);
                    if !s.agents.is_empty() {
                        println!("    Agents: {}", s.agents.join(", "));
                    }
                }
            }
            Ok(())
        }
        Some(SkillsAction::Find { query }) => {
            let results = skills::find_skills(&query)?;
            if results.is_empty() {
                println!("No skills found for '{}'", query);
            } else {
                for r in &results {
                    println!("  {} — {}", r.package, r.url);
                }
            }
            Ok(())
        }
        Some(SkillsAction::Add { package, project }) => {
            let (global, project_path) = if let Some(ref proj_name) = project {
                let store = store::Store::open()?;
                store.migrate()?;
                let proj = find_project_by_name(&store, proj_name)?;
                (false, Some(proj.repo_path))
            } else {
                (true, None)
            };

            let msg = skills::add_skill(&package, global, project_path.as_deref())?;
            println!("{}", msg);
            Ok(())
        }
        Some(SkillsAction::Remove { name, project }) => {
            let (global, project_path) = if let Some(ref proj_name) = project {
                let store = store::Store::open()?;
                store.migrate()?;
                let proj = find_project_by_name(&store, proj_name)?;
                (false, Some(proj.repo_path))
            } else {
                (true, None)
            };

            let msg = skills::remove_skill(&name, global, project_path.as_deref())?;
            println!("{}", msg);
            Ok(())
        }
        Some(SkillsAction::Update) => {
            let msg = skills::update_skills()?;
            println!("{}", msg);
            Ok(())
        }
    }
}
```

**Step 3: Verify it compiles**

Run: `cargo check`
Expected: compiles

**Step 4: Commit**

```bash
git add src/main.rs
git commit -m "feat(cli): add claustre skills subcommands"
```

---

### Task 10: Integration test — verify build and manual smoke test

**Step 1: Full build**

Run: `cargo build`
Expected: builds successfully

**Step 2: Test CLI list**

Run: `cargo run -- skills`
Expected: lists global skills (or "(none)" if empty)

**Step 3: Test CLI find**

Run: `cargo run -- skills find frontend`
Expected: shows search results from skills.sh

**Step 4: Test unit tests**

Run: `cargo test`
Expected: all tests pass

**Step 5: Commit**

No code changes — just verification.
