# Redesign Task Storage Hierarchy for Scalability

## Problem

The sync directory stores all tasks for a project in a single JSON file (`projects/<name>.json`). This causes three scaling issues:

1. **Large diffs**: Changing one task rewrites the entire file. Git diffs show the full file, not just the changed task.
2. **Merge conflicts**: Two machines editing different tasks in the same project conflict on the same file. With auto-push enabled, this becomes a frequent problem.
3. **File size**: Projects with hundreds of tasks produce large JSON files that are slow to parse and diff.

## Design

### New directory structure

```
sync/
  projects/
    <project-name>/
      project.json                # Project metadata only (name, default_branch)
      tasks/
        <task-uuid>.json          # Individual task with embedded subtasks
  config.toml                     # Shared config (unchanged)
```

**Before** (flat):
```
sync/projects/MyProject.json     # 1 file with all tasks
```

**After** (hierarchical):
```
sync/projects/MyProject/
  project.json                   # {"name": "MyProject", "default_branch": "main"}
  tasks/
    abc-123.json                 # Task with subtasks embedded
    def-456.json
```

### Serialization format

**`project.json`** — new struct `SyncProjectMeta`:
```json
{
  "name": "MyProject",
  "default_branch": "main"
}
```

**`tasks/<uuid>.json`** — existing `SyncTask` struct (unchanged):
```json
{
  "id": "abc-123",
  "title": "Implement feature X",
  "description": "...",
  "status": "pending",
  "subtasks": [...]
}
```

### Export changes

1. For each project, create `projects/<sanitized-name>/` directory
2. Write `project.json` with just name + default_branch
3. For each task, write `tasks/<task-id>.json` with that task's data (including subtasks)
4. Before writing, remove stale task files (tasks deleted from DB) by clearing the `tasks/` dir

### Import changes

1. Scan `projects/` for directories (new format) and `.json` files (old format)
2. For directories: read `project.json` for metadata, iterate `tasks/*.json` for task data
3. For `.json` files: use existing single-file parsing (backward compatibility)
4. Backward compat can be removed in a future version

### Task deletion sync

When a task is deleted locally and exported, its JSON file simply won't be written. The `tasks/` directory is cleared before writing, so stale files are removed. Git tracks the deletion naturally.

On import, tasks present in the DB but absent from the sync files are **not** deleted — import is additive (upsert-only). This matches existing behavior and avoids accidental data loss.

### Why not one file per subtask?

Subtasks are lightweight (title + description + status) and always displayed/modified as part of their parent task. Splitting them further adds filesystem overhead without meaningful git diff benefit. A task with 10 subtasks is still a small JSON file.

## Migration

No DB migration needed — this only changes the sync directory layout. Backward compatibility is built into the import path: detect whether `projects/<name>` is a file (old format) or directory (new format) and handle both.

## Impact on auto-push

This change makes auto-push significantly more effective:
- Single-task changes produce small, focused commits
- Concurrent edits on different tasks won't conflict
- `git log --name-only` shows exactly which tasks changed
