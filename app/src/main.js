// Claustre Desktop App — Frontend Logic
// Uses Tauri's IPC to communicate with the Rust backend.

const { invoke } = window.__TAURI__.core;

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

const state = {
  projects: [],
  selectedProjectId: null,
  tasks: [],
  selectedTaskId: null,
  refreshInterval: null,
};

// ---------------------------------------------------------------------------
// Initialization
// ---------------------------------------------------------------------------

document.addEventListener("DOMContentLoaded", async () => {
  await loadVersion();
  await refreshAll();

  // Auto-refresh every 2 seconds
  state.refreshInterval = setInterval(refreshAll, 2000);

  // Event listeners
  document.getElementById("btn-add-project").addEventListener("click", showProjectForm);
  document.getElementById("btn-new-task").addEventListener("click", () => showTaskForm());
  document.getElementById("btn-stats").addEventListener("click", showStats);
  document.getElementById("btn-close-detail").addEventListener("click", hideDetailPanel);

  document.getElementById("task-form").addEventListener("submit", handleTaskFormSubmit);
  document.getElementById("project-form").addEventListener("submit", handleProjectFormSubmit);
  document.getElementById("subtask-form").addEventListener("submit", handleSubtaskFormSubmit);

  // Close overlays
  document.querySelectorAll(".overlay-close").forEach((btn) => {
    btn.addEventListener("click", (e) => {
      e.target.closest(".overlay").classList.add("hidden");
    });
  });

  // Close overlays on backdrop click
  document.querySelectorAll(".overlay").forEach((overlay) => {
    overlay.addEventListener("click", (e) => {
      if (e.target === overlay) overlay.classList.add("hidden");
    });
  });

  // Keyboard shortcuts
  document.addEventListener("keydown", handleGlobalKeydown);
});

// ---------------------------------------------------------------------------
// Data Loading
// ---------------------------------------------------------------------------

async function loadVersion() {
  try {
    const version = await invoke("get_version");
    document.getElementById("version").textContent = version;
  } catch (e) {
    console.error("Failed to load version:", e);
  }
}

async function refreshAll() {
  try {
    await loadProjects();
    if (state.selectedProjectId) {
      await loadTasks(state.selectedProjectId);
      await loadRateLimit();
    }
  } catch (e) {
    console.error("Refresh error:", e);
  }
}

async function loadProjects() {
  const projects = await invoke("list_projects");
  state.projects = projects;
  renderProjectList();
}

async function loadTasks(projectId) {
  const tasks = await invoke("list_tasks", { projectId });
  state.tasks = tasks;
  renderTaskList();
}

async function loadRateLimit() {
  try {
    const rl = await invoke("get_rate_limit");
    renderRateLimit(rl);
  } catch (_) {
    // Rate limit data may not be available
  }
}

// ---------------------------------------------------------------------------
// Rendering — Projects
// ---------------------------------------------------------------------------

function renderProjectList() {
  const list = document.getElementById("project-list");
  list.innerHTML = "";

  for (const p of state.projects) {
    const li = document.createElement("li");
    li.className = p.id === state.selectedProjectId ? "active" : "";
    li.dataset.id = p.id;

    const counts = [];
    if (p.working_tasks > 0) counts.push(`<span class="count-badge count-working">${p.working_tasks} working</span>`);
    if (p.pending_tasks > 0) counts.push(`<span class="count-badge count-pending">${p.pending_tasks} pending</span>`);
    if (p.in_review_tasks > 0) counts.push(`<span class="count-badge count-review">${p.in_review_tasks} review</span>`);

    li.innerHTML = `
      <span class="project-name">${escapeHtml(p.name)}</span>
      <span class="project-counts">${counts.join("") || `<span class="count-badge count-done">${p.total_tasks} tasks</span>`}</span>
    `;

    li.addEventListener("click", () => selectProject(p.id));
    li.addEventListener("contextmenu", (e) => showProjectContextMenu(e, p));
    list.appendChild(li);
  }
}

function selectProject(projectId) {
  state.selectedProjectId = projectId;
  state.selectedTaskId = null;

  const project = state.projects.find((p) => p.id === projectId);
  if (project) {
    document.getElementById("project-title").textContent = project.name;
    document.getElementById("btn-stats").style.display = "";
    document.getElementById("btn-new-task").style.display = "";
    document.getElementById("empty-state").classList.add("hidden");
    document.getElementById("task-list-container").style.display = "";
  }

  renderProjectList();
  loadTasks(projectId);
  hideDetailPanel();
}

// ---------------------------------------------------------------------------
// Rendering — Tasks
// ---------------------------------------------------------------------------

const STATUS_CONFIG = {
  draft:       { symbol: "\u270e", label: "Draft" },
  pending:     { symbol: "\u2610", label: "Pending" },
  working:     { symbol: "\u25cf", label: "Working" },
  interrupted: { symbol: "\u25cc", label: "Interrupted" },
  in_review:   { symbol: "\u25d0", label: "In Review" },
  conflict:    { symbol: "\u26a0", label: "Conflict" },
  ci_failed:   { symbol: "\u2298", label: "CI Failed" },
  done:        { symbol: "\u2713", label: "Done" },
  error:       { symbol: "\u2717", label: "Error" },
};

function renderTaskList() {
  const container = document.getElementById("task-list");
  container.innerHTML = "";

  if (state.tasks.length === 0) {
    container.innerHTML = '<div class="empty-state"><p>No tasks yet. Create one to get started.</p></div>';
    return;
  }

  // Sort: active tasks first (by sort_priority), then done at bottom
  const sorted = [...state.tasks].sort((a, b) => {
    const priorityA = statusSortPriority(a.status);
    const priorityB = statusSortPriority(b.status);
    if (priorityA !== priorityB) return priorityA - priorityB;
    return a.sort_order - b.sort_order;
  });

  for (const task of sorted) {
    const el = createTaskElement(task);
    container.appendChild(el);
  }
}

function statusSortPriority(status) {
  const order = {
    draft: 0, in_review: 1, ci_failed: 2, conflict: 3,
    interrupted: 4, error: 5, pending: 6, working: 7, done: 8,
  };
  return order[status] ?? 99;
}

function createTaskElement(task) {
  const div = document.createElement("div");
  div.className = `task-item${task.status === "done" ? " task-done" : ""}${task.id === state.selectedTaskId ? " selected" : ""}`;
  div.dataset.id = task.id;

  const cfg = STATUS_CONFIG[task.status] || { symbol: "?", label: task.status };
  const tokens = task.input_tokens + task.output_tokens;
  const tokenStr = tokens > 0 ? formatTokens(tokens) : "";

  div.innerHTML = `
    <span class="task-status-icon">${cfg.symbol}</span>
    <div class="task-info">
      <span class="task-title-text">${escapeHtml(task.title)}</span>
      <div class="task-meta">
        ${tokenStr ? `<span class="token-display">${tokenStr}</span>` : ""}
        ${task.pr_url ? '<span>PR</span>' : ""}
      </div>
    </div>
    <div class="task-badges">
      <span class="badge badge-${task.status}">${cfg.label}</span>
      <span class="badge badge-${task.mode}">${task.mode}</span>
    </div>
    <div class="task-actions">
      ${task.status === "pending" || task.status === "draft" ? `<button class="task-action-btn" data-action="launch" title="Launch">Launch</button>` : ""}
      ${task.status === "pending" || task.status === "draft" ? `<button class="task-action-btn" data-action="edit" title="Edit">Edit</button>` : ""}
      ${task.status === "working" || task.status === "in_review" ? `<button class="task-action-btn" data-action="done" title="Mark done">Done</button>` : ""}
      ${task.pr_url ? `<button class="task-action-btn" data-action="open-pr" title="Open PR">PR</button>` : ""}
      <button class="task-action-btn" data-action="subtasks" title="Subtasks">Sub</button>
      ${task.status !== "working" ? `<button class="task-action-btn danger" data-action="delete" title="Delete">Del</button>` : ""}
    </div>
  `;

  // Click to select + show detail
  div.addEventListener("click", (e) => {
    if (e.target.closest(".task-actions")) return;
    selectTask(task.id);
  });

  // Action buttons
  div.querySelectorAll("[data-action]").forEach((btn) => {
    btn.addEventListener("click", (e) => {
      e.stopPropagation();
      handleTaskAction(btn.dataset.action, task);
    });
  });

  return div;
}

function selectTask(taskId) {
  state.selectedTaskId = taskId;
  renderTaskList();
  showTaskDetail(taskId);
}

// ---------------------------------------------------------------------------
// Task Detail Panel
// ---------------------------------------------------------------------------

function showTaskDetail(taskId) {
  const task = state.tasks.find((t) => t.id === taskId);
  if (!task) return;

  const panel = document.getElementById("detail-panel");
  panel.classList.remove("hidden");

  document.getElementById("detail-title").textContent = task.title;

  const cfg = STATUS_CONFIG[task.status] || { symbol: "?", label: task.status };
  const tokens = task.input_tokens + task.output_tokens;

  let html = "";

  // Status
  html += `<div class="detail-section">
    <h4>Status</h4>
    <div class="detail-field"><span class="label">State</span><span class="value"><span class="badge badge-${task.status}">${cfg.symbol} ${cfg.label}</span></span></div>
    <div class="detail-field"><span class="label">Mode</span><span class="value"><span class="badge badge-${task.mode}">${task.mode}</span></span></div>
    <div class="detail-field"><span class="label">Push</span><span class="value">${task.push_mode}</span></div>
    ${task.review_loop ? '<div class="detail-field"><span class="label">Review Loop</span><span class="value">enabled</span></div>' : ""}
    ${task.branch ? `<div class="detail-field"><span class="label">Branch</span><span class="value">${escapeHtml(task.branch)}</span></div>` : ""}
    ${task.pr_url ? `<div class="detail-field"><span class="label">PR</span><span class="value"><a href="#" onclick="openUrl('${escapeHtml(task.pr_url)}')" style="color:var(--accent)">View PR</a></span></div>` : ""}
  </div>`;

  // Description
  if (task.description) {
    html += `<div class="detail-section">
      <h4>Description</h4>
      <pre>${escapeHtml(task.description)}</pre>
    </div>`;
  }

  // Tokens
  if (tokens > 0) {
    html += `<div class="detail-section">
      <h4>Usage</h4>
      <div class="detail-field"><span class="label">Input</span><span class="value">${formatTokens(task.input_tokens)}</span></div>
      <div class="detail-field"><span class="label">Output</span><span class="value">${formatTokens(task.output_tokens)}</span></div>
      <div class="detail-field"><span class="label">Total</span><span class="value">${formatTokens(tokens)}</span></div>
    </div>`;
  }

  // Timing
  html += `<div class="detail-section">
    <h4>Timing</h4>
    <div class="detail-field"><span class="label">Created</span><span class="value">${formatDate(task.created_at)}</span></div>
    ${task.started_at ? `<div class="detail-field"><span class="label">Started</span><span class="value">${formatDate(task.started_at)}</span></div>` : ""}
    ${task.completed_at ? `<div class="detail-field"><span class="label">Completed</span><span class="value">${formatDate(task.completed_at)}</span></div>` : ""}
  </div>`;

  document.getElementById("detail-content").innerHTML = html;
}

function hideDetailPanel() {
  document.getElementById("detail-panel").classList.add("hidden");
  state.selectedTaskId = null;
  renderTaskList();
}

// ---------------------------------------------------------------------------
// Task Actions
// ---------------------------------------------------------------------------

async function handleTaskAction(action, task) {
  try {
    switch (action) {
      case "launch":
        await invoke("launch_task", { taskId: task.id });
        showToast("Task launched", "success");
        await refreshAll();
        break;

      case "edit":
        showTaskForm(task);
        break;

      case "done":
        await invoke("mark_task_done", { taskId: task.id });
        showToast("Task marked done", "success");
        hideDetailPanel();
        await refreshAll();
        break;

      case "delete":
        showConfirm(`Delete task "${task.title}"?`, async () => {
          await invoke("delete_task", { id: task.id });
          showToast("Task deleted", "success");
          hideDetailPanel();
          await refreshAll();
        });
        break;

      case "open-pr":
        if (task.pr_url) {
          openUrl(task.pr_url);
        }
        break;

      case "subtasks":
        showSubtaskOverlay(task);
        break;
    }
  } catch (e) {
    showToast(`Error: ${e}`, "error");
  }
}

// ---------------------------------------------------------------------------
// Task Form
// ---------------------------------------------------------------------------

function showTaskForm(existingTask) {
  const overlay = document.getElementById("task-form-overlay");
  const title = document.getElementById("task-form-title");
  const submitBtn = document.getElementById("task-form-submit");

  if (existingTask) {
    title.textContent = "Edit Task";
    submitBtn.textContent = "Save";
    document.getElementById("task-form-id").value = existingTask.id;
    document.getElementById("task-form-project-id").value = existingTask.project_id;
    document.getElementById("task-title").value = existingTask.title;
    document.getElementById("task-description").value = existingTask.description;
    document.getElementById("task-mode").value = existingTask.mode;
    document.getElementById("task-push-mode").value = existingTask.push_mode;
    document.getElementById("task-branch").value = existingTask.branch || "";
    document.getElementById("task-base").value = existingTask.base || "";
    document.getElementById("task-review-loop").checked = existingTask.review_loop;
  } else {
    title.textContent = "New Task";
    submitBtn.textContent = "Create";
    document.getElementById("task-form").reset();
    document.getElementById("task-form-id").value = "";
    document.getElementById("task-form-project-id").value = state.selectedProjectId;
  }

  overlay.classList.remove("hidden");
  document.getElementById("task-title").focus();
}

async function handleTaskFormSubmit(e) {
  e.preventDefault();

  const id = document.getElementById("task-form-id").value;
  const projectId = document.getElementById("task-form-project-id").value;
  const title = document.getElementById("task-title").value.trim();
  const description = document.getElementById("task-description").value;
  const mode = document.getElementById("task-mode").value;
  const pushMode = document.getElementById("task-push-mode").value;
  const branch = document.getElementById("task-branch").value.trim() || null;
  const base = document.getElementById("task-base").value.trim() || null;
  const reviewLoop = document.getElementById("task-review-loop").checked;

  if (!title) {
    showToast("Title is required", "error");
    return;
  }

  try {
    if (id) {
      await invoke("update_task", {
        req: { id, title, description, mode, push_mode: pushMode, review_loop: reviewLoop, branch, base },
      });
      showToast("Task updated", "success");
    } else {
      await invoke("create_task", {
        req: { project_id: projectId, title, description, mode, push_mode: pushMode, review_loop: reviewLoop, branch, base },
      });
      showToast("Task created", "success");
    }

    document.getElementById("task-form-overlay").classList.add("hidden");
    await refreshAll();
  } catch (e) {
    showToast(`Error: ${e}`, "error");
  }
}

// ---------------------------------------------------------------------------
// Project Form
// ---------------------------------------------------------------------------

function showProjectForm() {
  document.getElementById("project-form").reset();
  document.getElementById("project-form-overlay").classList.remove("hidden");
  document.getElementById("project-name").focus();
}

async function handleProjectFormSubmit(e) {
  e.preventDefault();

  const name = document.getElementById("project-name").value.trim();
  const path = document.getElementById("project-path").value.trim();

  if (!name || !path) {
    showToast("Name and path are required", "error");
    return;
  }

  try {
    await invoke("create_project", { name, path });
    showToast("Project added", "success");
    document.getElementById("project-form-overlay").classList.add("hidden");
    await refreshAll();
  } catch (e) {
    showToast(`Error: ${e}`, "error");
  }
}

// ---------------------------------------------------------------------------
// Stats
// ---------------------------------------------------------------------------

async function showStats() {
  if (!state.selectedProjectId) return;

  try {
    const stats = await invoke("get_project_stats", { projectId: state.selectedProjectId });
    const body = document.getElementById("stats-body");
    body.innerHTML = `
      <div class="stat-card"><div class="stat-label">Total Tasks</div><div class="stat-value">${stats.total_tasks}</div></div>
      <div class="stat-card"><div class="stat-label">Completed</div><div class="stat-value">${stats.completed_tasks}</div></div>
      <div class="stat-card"><div class="stat-label">Sessions</div><div class="stat-value">${stats.total_sessions}</div></div>
      <div class="stat-card"><div class="stat-label">Total Time</div><div class="stat-value">${stats.total_time}</div></div>
      <div class="stat-card"><div class="stat-label">Total Tokens</div><div class="stat-value">${formatTokens(stats.total_tokens)}</div></div>
      <div class="stat-card"><div class="stat-label">Avg Task Time</div><div class="stat-value">${stats.avg_task_time}</div></div>
    `;
    document.getElementById("stats-overlay").classList.remove("hidden");
  } catch (e) {
    showToast(`Error: ${e}`, "error");
  }
}

// ---------------------------------------------------------------------------
// Subtasks
// ---------------------------------------------------------------------------

async function showSubtaskOverlay(task) {
  document.getElementById("subtask-overlay-title").textContent = `Subtasks: ${task.title}`;
  document.getElementById("subtask-task-id").value = task.id;
  document.getElementById("subtask-overlay").classList.remove("hidden");
  await loadSubtasks(task.id);
}

async function loadSubtasks(taskId) {
  try {
    const subtasks = await invoke("list_subtasks", { taskId });
    const container = document.getElementById("subtask-list");
    container.innerHTML = "";

    if (subtasks.length === 0) {
      container.innerHTML = '<div class="empty-state" style="padding:16px"><p>No subtasks</p></div>';
      return;
    }

    for (const s of subtasks) {
      const div = document.createElement("div");
      div.className = "subtask-item";
      div.innerHTML = `
        <span class="subtask-status">${STATUS_CONFIG[s.status]?.symbol || "?"}</span>
        <span class="subtask-title">${escapeHtml(s.title)}</span>
        <button class="btn-icon" data-delete="${s.id}" title="Delete">&times;</button>
      `;
      div.querySelector("[data-delete]").addEventListener("click", async () => {
        await invoke("delete_subtask", { id: s.id });
        await loadSubtasks(taskId);
      });
      container.appendChild(div);
    }
  } catch (e) {
    showToast(`Error loading subtasks: ${e}`, "error");
  }
}

async function handleSubtaskFormSubmit(e) {
  e.preventDefault();

  const taskId = document.getElementById("subtask-task-id").value;
  const title = document.getElementById("subtask-title").value.trim();
  const description = document.getElementById("subtask-description").value.trim();

  if (!title) return;

  try {
    await invoke("create_subtask", { taskId, title, description });
    document.getElementById("subtask-title").value = "";
    document.getElementById("subtask-description").value = "";
    await loadSubtasks(taskId);
  } catch (e) {
    showToast(`Error: ${e}`, "error");
  }
}

// ---------------------------------------------------------------------------
// Rate Limit
// ---------------------------------------------------------------------------

function renderRateLimit(rl) {
  const el = document.getElementById("rate-limit");
  if (!rl.usage_5h_pct && !rl.usage_7d_pct) {
    el.textContent = "";
    return;
  }

  const pct5h = rl.usage_5h_pct || 0;
  const pct7d = rl.usage_7d_pct || 0;
  const maxPct = Math.max(pct5h, pct7d);

  const cls = maxPct >= 80 ? "critical" : maxPct >= 50 ? "high" : "";
  el.innerHTML = `<span class="${cls}">5h: ${pct5h.toFixed(0)}% | 7d: ${pct7d.toFixed(0)}%</span>`;
}

// ---------------------------------------------------------------------------
// Confirm Dialog
// ---------------------------------------------------------------------------

function showConfirm(message, onConfirm) {
  document.getElementById("confirm-message").textContent = message;
  document.getElementById("confirm-overlay").classList.remove("hidden");

  const btn = document.getElementById("confirm-action");
  const handler = async () => {
    btn.removeEventListener("click", handler);
    document.getElementById("confirm-overlay").classList.add("hidden");
    await onConfirm();
  };
  btn.addEventListener("click", handler);
}

// ---------------------------------------------------------------------------
// Project Context Menu
// ---------------------------------------------------------------------------

function showProjectContextMenu(e, project) {
  e.preventDefault();
  removeContextMenu();

  const menu = document.createElement("div");
  menu.className = "context-menu";
  menu.style.left = `${e.clientX}px`;
  menu.style.top = `${e.clientY}px`;

  menu.innerHTML = `
    <div class="context-menu-item" data-action="delete">Delete Project</div>
  `;

  menu.querySelector("[data-action='delete']").addEventListener("click", () => {
    removeContextMenu();
    showConfirm(`Delete project "${project.name}" and all its tasks?`, async () => {
      await invoke("delete_project", { id: project.id });
      if (state.selectedProjectId === project.id) {
        state.selectedProjectId = null;
        state.tasks = [];
        document.getElementById("project-title").textContent = "Select a project";
        document.getElementById("btn-stats").style.display = "none";
        document.getElementById("btn-new-task").style.display = "none";
        document.getElementById("empty-state").classList.remove("hidden");
        document.getElementById("task-list-container").style.display = "none";
      }
      showToast("Project deleted", "success");
      await refreshAll();
    });
  });

  document.body.appendChild(menu);
  document.addEventListener("click", removeContextMenu, { once: true });
}

function removeContextMenu() {
  document.querySelectorAll(".context-menu").forEach((m) => m.remove());
}

// ---------------------------------------------------------------------------
// Keyboard Shortcuts
// ---------------------------------------------------------------------------

function handleGlobalKeydown(e) {
  // Escape closes overlays
  if (e.key === "Escape") {
    document.querySelectorAll(".overlay:not(.hidden)").forEach((o) => o.classList.add("hidden"));
    return;
  }

  // Don't handle shortcuts when typing in forms
  if (e.target.tagName === "INPUT" || e.target.tagName === "TEXTAREA" || e.target.tagName === "SELECT") {
    return;
  }

  switch (e.key) {
    case "n":
      if (state.selectedProjectId) showTaskForm();
      break;
    case "a":
      showProjectForm();
      break;
    case "s":
      if (state.selectedProjectId) showStats();
      break;
  }
}

// ---------------------------------------------------------------------------
// Toast
// ---------------------------------------------------------------------------

function showToast(message, type = "") {
  const toast = document.getElementById("toast");
  toast.textContent = message;
  toast.className = `toast ${type}`;
  setTimeout(() => toast.classList.add("hidden"), 3000);
}

// ---------------------------------------------------------------------------
// Utilities
// ---------------------------------------------------------------------------

function escapeHtml(str) {
  const div = document.createElement("div");
  div.textContent = str;
  return div.innerHTML;
}

function formatTokens(n) {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}k`;
  return String(n);
}

function formatDate(dateStr) {
  if (!dateStr) return "";
  try {
    const d = new Date(dateStr + "Z");
    return d.toLocaleString(undefined, {
      month: "short",
      day: "numeric",
      hour: "2-digit",
      minute: "2-digit",
    });
  } catch {
    return dateStr;
  }
}

function openUrl(url) {
  try {
    window.__TAURI__.shell.open(url);
  } catch {
    window.open(url, "_blank");
  }
}
