# Code Review Fixes Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Fix all critical (C1-C3) and moderate (M2-M5) issues identified in the deep code review.

**Architecture:** Direct fixes to existing modules — no new files needed. Add `tracing::warn` for silent parse fallbacks, add transition validation to `TaskStatus`, switch git stats to `--numstat`, add safety guard to external session pruning, add timeouts to skills CLI, and unify duplicated worktree creation.

**Tech Stack:** Rust 2024, tracing, rusqlite, std::process::Command

---

### Task 1: C1 — Add tracing::warn on silent enum parse fallbacks

**Files:**
- Modify: `src/store/queries.rs` (lines 224-250, 483-507, 754-767)

Replace `unwrap_or(default)` with a pattern that logs a warning before falling back.

### Task 2: C2 — Add task status transition validation

**Files:**
- Modify: `src/store/models.rs` (add `can_transition_to` method to `TaskStatus`)
- Modify: `src/store/queries.rs` (validate in `update_task_status`)

### Task 3: C3 — Optimize Stop hook token extraction

**Files:**
- Modify: `src/session/mod.rs` (lines 568-588, `extract_usage()` in the common hook script)

Use `tail` to read only last portion of JSONL instead of scanning the entire file.

### Task 4: M2 — Guard against empty active_ids in external session pruning

**Files:**
- Modify: `src/store/queries.rs` (lines 918-940)

### Task 5: M3 — Switch git stats to `--numstat` for reliable parsing

**Files:**
- Modify: `src/session/mod.rs` (lines 755-811)

### Task 6: M4 — Add timeout to skills CLI operations

**Files:**
- Modify: `src/skills/mod.rs` (lines 160-247)

### Task 7: M5 — Unify worktree creation functions

**Files:**
- Modify: `src/session/mod.rs` (lines 288-427)
