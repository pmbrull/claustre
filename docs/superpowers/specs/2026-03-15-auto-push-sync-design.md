# Auto-Push Strategy for Task Lifecycle Sync

## Problem

`claustre sync push` is entirely manual. Users must remember to run it after meaningful state changes, which means cross-machine sync is always stale. The sync repo (`~/.claustre/sync/`) is a lightweight git repo separate from project repos, so pushing there is cheap and non-intrusive.

## Design

### Approach: Debounced background push on state-changing events

Rather than pushing on every individual event (which could create dozens of commits per hour in a busy session), use a **dirty flag + debounced timer** pattern:

1. Certain lifecycle events mark the sync state as "dirty"
2. A background timer checks the dirty flag periodically (default: 60 seconds)
3. When dirty, it runs `sync::push()` in a background thread and clears the flag
4. If the push fails (no remote, network error), it logs a warning and retries on the next cycle

This approach avoids excessive commits while ensuring state propagates within ~60 seconds of any meaningful change.

### Events that mark sync as dirty

These are the meaningful state transitions that another machine would care about:

| Event | Where it happens | Why it matters |
|---|---|---|
| Task created | `store.create_task()` via TUI form or CLI | New work item visible on other machines |
| Task status changed | `store.update_task_status()` | Progress tracking (pending -> working -> in_review -> done) |
| Task edited | TUI edit form submit | Updated description/config |
| Task deleted | TUI confirm delete | Removes stale work |
| Task reordered | `J`/`K` key handler | Priority changes |
| PR detected (in_review) | `session_update::apply()` | Task has a PR to review |
| Task done (PR merged or manual) | PR merge poller or `r` key | Work completed |
| Project added/removed | CLI `add-project`/`remove-project` | Structural change |
| Subtask created/edited/deleted | TUI subtask panel | Task breakdown changed |

Events that do **not** trigger sync (machine-local state):
- Session created/destroyed (sessions are machine-specific)
- Token usage updates (high frequency, machine-specific)
- Claude status changes (idle/working/paused — transient)
- Rate limit state changes (machine-specific)
- Git stats updates (machine-specific)

### Config

Add an `auto_sync` boolean to `config.toml` (default: `false` to avoid surprising users who haven't set up `sync init`):

```toml
auto_sync = false
```

When `true` and the sync repo exists (`~/.claustre/sync/.git`), the TUI runs the debounced sync loop. When `false` or sync repo doesn't exist, the feature is completely inert.

### Implementation plan

#### 1. Add `auto_sync` config field

In `config/mod.rs`, add to `Config`:
```rust
#[serde(default)]
pub auto_sync: bool,
```

#### 2. Add sync dirty flag + timer to `App`

In `tui/app/mod.rs`, add fields:
```rust
sync_dirty: bool,
last_sync_push: Instant,
sync_push_in_progress: Arc<AtomicBool>,
```

#### 3. Add `mark_sync_dirty()` helper

A simple method on `App`:
```rust
fn mark_sync_dirty(&mut self) {
    if self.config.auto_sync {
        self.sync_dirty = true;
    }
}
```

Called at each lifecycle event listed above (in the existing key handlers and poll result handlers).

#### 4. Add `maybe_auto_sync_push()` to the slow-tick path

In `event_loop.rs`, add to the slow-tick block:
```rust
self.maybe_auto_sync_push();
```

The method:
```rust
fn maybe_auto_sync_push(&mut self) {
    const SYNC_INTERVAL: Duration = Duration::from_secs(60);

    if !self.config.auto_sync || !self.sync_dirty {
        return;
    }
    if self.last_sync_push.elapsed() < SYNC_INTERVAL {
        return;
    }
    if self.sync_push_in_progress.load(Ordering::SeqCst) {
        return;
    }

    // Verify sync repo exists
    let Ok(sync_dir) = config::sync_dir() else { return };
    if !sync_dir.join(".git").exists() {
        return;
    }

    self.sync_dirty = false;
    self.last_sync_push = Instant::now();

    let flag = self.sync_push_in_progress.clone();
    flag.store(true, Ordering::SeqCst);

    std::thread::spawn(move || {
        if let Ok(store) = Store::open() {
            if let Err(e) = sync::push(&store) {
                eprintln!("auto-sync push failed: {e}");
            }
        }
        flag.store(false, Ordering::SeqCst);
    });
}
```

#### 5. Mark dirty at each trigger point

Add `self.mark_sync_dirty()` calls after:
- Task creation (in `handle_input_key` after `store.create_task()`)
- Task status changes (in `poll_pr_merge_results`, `handle_normal_key` for `r`/`MarkDone`)
- Task edit submit
- Task delete confirm
- Task reorder (`J`/`K`)
- Subtask create/edit/delete
- Project add/remove

The `session_update` CLI path (hooks) does NOT trigger auto-push because it runs outside the TUI process. This is intentional: the TUI's next `refresh_data()` tick will detect the DB state change and mark dirty if needed. However, the `feed-next` path (autonomous chains) could optionally call `sync::push()` at the end of a chain (after all tasks complete).

#### 6. Suppress `sync::push()` stdout in auto mode

The current `push()` function prints to stdout. Add a `quiet` parameter or redirect output when called from auto-sync to avoid polluting the TUI.

### What about the CLI path?

`claustre add-task`, `claustre remove-project`, etc. run outside the TUI. These could also auto-push by calling `sync::push()` at the end of the command, gated by `auto_sync` config. This is a simple addition:

```rust
// At the end of add-task, add-project, remove-project handlers:
if config::load().map_or(false, |c| c.auto_sync) {
    let _ = sync::push(&store);
}
```

### Commit message strategy

The sync repo commit messages already include hostname and timestamp (`sync: {host} at {now}`). No change needed — git's content deduplication ensures identical state doesn't create extra commits.

### Why not event-driven push (push immediately on each event)?

1. **Batching is better**: Multiple events often happen in quick succession (creating a task with subtasks, reordering multiple tasks). A 60-second window naturally batches these into a single commit.
2. **Resilience**: If push fails (network blip), the dirty flag stays set and retries automatically.
3. **No UI jank**: Push runs in a background thread, invisible to the user.
4. **No excessive commits**: Even with constant activity, at most 1 commit per minute.

### Why not longer intervals?

60 seconds is the sweet spot. Shorter (10s) would create more commits with minimal benefit. Longer (5min+) defeats the purpose — you could easily create 10 tasks and switch machines before they sync.

## Testing

- Unit test: `mark_sync_dirty` sets flag, `maybe_auto_sync_push` clears it and spawns thread
- Integration test: create task -> verify sync repo gets a new commit within the interval
- Edge cases: sync repo not initialized (no-op), `auto_sync = false` (no-op), push failure (flag stays dirty)
