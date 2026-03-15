// Claustre Desktop App — Frontend Logic
// Uses Tauri's IPC to communicate with the Rust backend.

let invoke;
let listen;

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

const state = {
  projects: [],
  selectedProjectId: null,
  tasks: [],
  selectedTaskId: null,
  refreshInterval: null,
  // Tab system: "dashboard" is always index 0, sessions are 1+
  activeTab: "dashboard",
  sessions: new Map(), // sessionId -> { id, label, panes: Map<paneId, {terminal, fitAddon, containerEl, unlistenOutput, unlistenExit}>, focusedPane, resizeHandler, containerEl }
  // Task filter
  taskFilter: "",
  // Done section expanded
  doneExpanded: false,
  // Skills
  skillsGlobal: true,
  installedSkills: [],
  selectedSkillIndex: -1,
  // Command palette
  paletteIndex: 0,
  // Permissions
  permissionsAligned: true,
};

// ---------------------------------------------------------------------------
// Initialization
// ---------------------------------------------------------------------------

document.addEventListener("DOMContentLoaded", async () => {
  try {
    invoke = window.__TAURI__.core.invoke;
    listen = window.__TAURI__.event.listen;
  } catch (e) {
    document.getElementById("empty-state").innerHTML =
      `<p style="color:#f7768e">Tauri API not available: ${e.message}</p>`;
    console.error("Tauri init failed:", e);
    return;
  }

  await loadVersion();
  await refreshAll();
  checkPermissions();

  // Auto-refresh every 2 seconds (only when on dashboard)
  state.refreshInterval = setInterval(() => {
    if (state.activeTab === "dashboard") refreshAll();
  }, 2000);

  // Event listeners
  document.getElementById("btn-add-project").addEventListener("click", showProjectForm);
  document.getElementById("btn-new-task").addEventListener("click", () => showTaskForm());
  document.getElementById("btn-stats").addEventListener("click", showStats);
  document.getElementById("btn-close-detail").addEventListener("click", hideDetailPanel);

  document.getElementById("task-form").addEventListener("submit", handleTaskFormSubmit);
  document.getElementById("project-form").addEventListener("submit", handleProjectFormSubmit);
  document.getElementById("subtask-form").addEventListener("submit", handleSubtaskFormSubmit);

  // Task filter
  const filterInput = document.getElementById("task-filter-input");
  filterInput.addEventListener("input", () => {
    state.taskFilter = filterInput.value;
    renderTaskList();
  });
  document.getElementById("task-filter-clear").addEventListener("click", clearTaskFilter);

  // Command palette
  document.getElementById("palette-input").addEventListener("input", renderPaletteResults);
  document.getElementById("palette-input").addEventListener("keydown", handlePaletteKeydown);

  // Skills
  document.getElementById("skills-scope-toggle").addEventListener("click", toggleSkillScope);
  document.getElementById("skills-find-btn").addEventListener("click", showSkillSearch);
  document.getElementById("skills-add-btn").addEventListener("click", showSkillAdd);
  document.getElementById("skills-update-btn").addEventListener("click", updateAllSkills);
  document.getElementById("skills-search-input").addEventListener("keydown", (e) => {
    if (e.key === "Enter") searchSkills();
  });
  document.getElementById("skills-search-install").addEventListener("click", installSelectedSkills);
  document.getElementById("skills-add-submit").addEventListener("click", addSkillByPackage);

  // Configure
  document.getElementById("configure-apply").addEventListener("click", applyPermissions);

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
// Tab System
// ---------------------------------------------------------------------------

function renderTabBar() {
  const tabBar = document.getElementById("tab-bar");

  if (state.sessions.size === 0) {
    tabBar.classList.add("hidden");
    return;
  }

  tabBar.classList.remove("hidden");
  tabBar.innerHTML = "";

  // Dashboard tab
  const dashTab = document.createElement("div");
  dashTab.className = `tab-item${state.activeTab === "dashboard" ? " active" : ""}`;
  dashTab.textContent = "Dashboard";
  dashTab.addEventListener("click", () => switchTab("dashboard"));
  tabBar.appendChild(dashTab);

  // Session tabs
  for (const [sessionId, session] of state.sessions) {
    const tab = document.createElement("div");
    tab.className = `tab-item${state.activeTab === sessionId ? " active" : ""}`;

    const label = document.createElement("span");
    label.textContent = session.label;
    tab.appendChild(label);

    const close = document.createElement("span");
    close.className = "tab-close";
    close.textContent = "\u00d7";
    close.addEventListener("click", (e) => {
      e.stopPropagation();
      closeSessionTab(sessionId);
    });
    tab.appendChild(close);

    tab.addEventListener("click", () => switchTab(sessionId));
    tabBar.appendChild(tab);
  }
}

function switchTab(tabId) {
  state.activeTab = tabId;

  const dashboardView = document.getElementById("dashboard-view");
  if (tabId === "dashboard") {
    dashboardView.classList.remove("hidden");
    dashboardView.style.display = "";
  } else {
    dashboardView.classList.add("hidden");
    dashboardView.style.display = "none";
  }

  // Show/hide session panels
  for (const [sessionId, session] of state.sessions) {
    if (sessionId === tabId) {
      session.containerEl.classList.add("active");
      // Refit and focus all panes when switching to session
      requestAnimationFrame(() => {
        for (const pane of session.panes.values()) {
          pane.fitAddon.fit();
        }
        // Focus the active pane
        const focusedPaneData = session.panes.get(session.focusedPane);
        if (focusedPaneData) focusedPaneData.terminal.focus();
      });
    } else {
      session.containerEl.classList.remove("active");
    }
  }

  renderTabBar();
}

const XTERM_THEME = {
  background: "#1a1b26",
  foreground: "#c0caf5",
  cursor: "#c0caf5",
  cursorAccent: "#1a1b26",
  selectionBackground: "#33467c",
  black: "#15161e",
  red: "#f7768e",
  green: "#9ece6a",
  yellow: "#e0af68",
  blue: "#7aa2f7",
  magenta: "#bb9af7",
  cyan: "#7dcfff",
  white: "#a9b1d6",
  brightBlack: "#414868",
  brightRed: "#f7768e",
  brightGreen: "#9ece6a",
  brightYellow: "#e0af68",
  brightBlue: "#7aa2f7",
  brightMagenta: "#bb9af7",
  brightCyan: "#7dcfff",
  brightWhite: "#c0caf5",
};

function createTerminalPane(paneId) {
  const wrapper = document.createElement("div");
  wrapper.className = "pane-wrapper";
  wrapper.dataset.paneId = paneId;

  const header = document.createElement("div");
  header.className = "pane-header";
  wrapper.appendChild(header);

  const termContainer = document.createElement("div");
  termContainer.className = "session-terminal";
  wrapper.appendChild(termContainer);

  const terminal = new Terminal({
    cursorBlink: true,
    fontSize: 13,
    fontFamily: '"SF Mono", "Fira Code", "Cascadia Code", monospace',
    theme: XTERM_THEME,
  });

  const fitAddon = new FitAddon.FitAddon();
  terminal.loadAddon(fitAddon);
  terminal.open(termContainer);

  return { wrapper, terminal, fitAddon, termContainer, header };
}

async function setupPaneIO(paneId, terminal, fitAddon) {
  // Send initial size
  await invoke("pty_resize", {
    sessionId: paneId,
    rows: terminal.rows,
    cols: terminal.cols,
  }).catch(() => {});

  // Forward keystrokes
  terminal.onData((data) => {
    invoke("pty_write", { sessionId: paneId, data }).catch((e) => {
      console.error("pty_write error:", e);
    });
  });

  // Resize PTY when terminal resizes
  terminal.onResize(({ rows, cols }) => {
    invoke("pty_resize", { sessionId: paneId, rows, cols }).catch(() => {});
  });

  // Listen for PTY output
  const unlistenOutput = await listen(`pty-output-${paneId}`, (event) => {
    terminal.write(event.payload);
  });

  // Listen for PTY exit
  const unlistenExit = await listen(`pty-exit-${paneId}`, () => {
    terminal.write("\r\n\x1b[90m[Session ended]\x1b[0m\r\n");
  });

  return { unlistenOutput, unlistenExit };
}

async function addSessionTab(sessionId, label, worktreePath) {
  // Create the session panel DOM
  const panel = document.createElement("div");
  panel.className = "session-panel";
  panel.id = `session-${sessionId}`;

  // Split pane layout: shell (left) + claude (right)
  const splitContainer = document.createElement("div");
  splitContainer.className = "pane-split horizontal";
  panel.appendChild(splitContainer);

  // Session hint bar
  const hintBar = document.createElement("div");
  hintBar.className = "session-hint-bar";
  hintBar.innerHTML = "Ctrl+H/L: switch pane &middot; Ctrl+R: split right &middot; Ctrl+B: split down &middot; Ctrl+W: close pane &middot; Ctrl+D: dashboard";
  panel.appendChild(hintBar);

  document.getElementById("session-views").appendChild(panel);

  const panes = new Map();

  // 1. Spawn shell PTY in the worktree
  let shellPaneId;
  try {
    shellPaneId = await invoke("pty_spawn_shell", { sessionId, worktreePath });
  } catch (e) {
    console.error("Failed to spawn shell:", e);
    // Fall back to claude-only layout
    shellPaneId = null;
  }

  // The claude PTY uses the sessionId as its pane ID
  const claudePaneId = sessionId;

  if (shellPaneId) {
    // Shell pane (left)
    const shell = createTerminalPane(shellPaneId);
    shell.header.textContent = "Shell";
    shell.wrapper.classList.add("pane-shell");
    splitContainer.appendChild(shell.wrapper);

    const shellIO = await setupPaneIO(shellPaneId, shell.terminal, shell.fitAddon);
    panes.set(shellPaneId, {
      terminal: shell.terminal,
      fitAddon: shell.fitAddon,
      containerEl: shell.wrapper,
      label: "Shell",
      isClaudePane: false,
      ...shellIO,
    });
  }

  // Claude pane (right, or full-width if no shell)
  const claude = createTerminalPane(claudePaneId);
  claude.header.textContent = "Claude";
  claude.wrapper.classList.add("pane-claude");
  splitContainer.appendChild(claude.wrapper);

  const claudeIO = await setupPaneIO(claudePaneId, claude.terminal, claude.fitAddon);
  panes.set(claudePaneId, {
    terminal: claude.terminal,
    fitAddon: claude.fitAddon,
    containerEl: claude.wrapper,
    label: "Claude",
    isClaudePane: true,
    ...claudeIO,
  });

  // Refit all panes on window resize
  const resizeHandler = () => {
    if (state.activeTab === sessionId) {
      for (const pane of panes.values()) {
        pane.fitAddon.fit();
      }
    }
  };
  window.addEventListener("resize", resizeHandler);

  // Focus the Claude pane by default
  const focusedPane = claudePaneId;

  // Store session state
  state.sessions.set(sessionId, {
    id: sessionId,
    label,
    panes,
    focusedPane,
    claudePaneId,
    worktreePath: worktreePath || "",
    splitContainer,
    resizeHandler,
    containerEl: panel,
  });

  updatePaneFocus(sessionId);

  // Switch to the new tab
  switchTab(sessionId);
}

function updatePaneFocus(sessionId) {
  const session = state.sessions.get(sessionId);
  if (!session) return;

  for (const [paneId, pane] of session.panes) {
    const isFocused = paneId === session.focusedPane;
    pane.containerEl.classList.toggle("pane-focused", isFocused);
    pane.containerEl.querySelector(".pane-header").classList.toggle("focused", isFocused);
    if (isFocused) {
      pane.terminal.focus();
    }
  }
}

function cyclePaneFocus(sessionId, direction) {
  const session = state.sessions.get(sessionId);
  if (!session || session.panes.size <= 1) return;

  const paneIds = [...session.panes.keys()];
  const currentIdx = paneIds.indexOf(session.focusedPane);
  const nextIdx = direction > 0
    ? (currentIdx + 1) % paneIds.length
    : (currentIdx - 1 + paneIds.length) % paneIds.length;

  session.focusedPane = paneIds[nextIdx];
  updatePaneFocus(sessionId);
}

async function splitPane(sessionId, direction) {
  const session = state.sessions.get(sessionId);
  if (!session) return;

  try {
    const paneId = await invoke("pty_spawn_shell", {
      sessionId,
      worktreePath: session.worktreePath,
    });

    const shellNum = [...session.panes.values()].filter((p) => !p.isClaudePane).length + 1;
    const pane = createTerminalPane(paneId);
    pane.header.textContent = `Shell ${shellNum}`;

    const io = await setupPaneIO(paneId, pane.terminal, pane.fitAddon);
    session.panes.set(paneId, {
      terminal: pane.terminal,
      fitAddon: pane.fitAddon,
      containerEl: pane.wrapper,
      label: `Shell ${shellNum}`,
      isClaudePane: false,
      ...io,
    });

    // Add to the split container
    session.splitContainer.appendChild(pane.wrapper);

    // If splitting vertically, switch to vertical layout class
    if (direction === "vertical") {
      session.splitContainer.classList.remove("horizontal");
      session.splitContainer.classList.add("vertical");
    }

    // Refit all panes after adding
    requestAnimationFrame(() => {
      for (const p of session.panes.values()) {
        p.fitAddon.fit();
      }
    });

    session.focusedPane = paneId;
    updatePaneFocus(sessionId);
  } catch (e) {
    showToast(`Error splitting pane: ${e}`, "error");
  }
}

function closePane(sessionId) {
  const session = state.sessions.get(sessionId);
  if (!session) return;

  const paneId = session.focusedPane;
  const pane = session.panes.get(paneId);
  if (!pane) return;

  // Can't close the Claude pane or the last pane
  if (pane.isClaudePane || session.panes.size <= 1) {
    showToast("Cannot close this pane", "error");
    return;
  }

  // Cleanup
  if (pane.unlistenOutput) pane.unlistenOutput();
  if (pane.unlistenExit) pane.unlistenExit();
  pane.terminal.dispose();
  pane.containerEl.remove();
  session.panes.delete(paneId);

  // Focus the Claude pane
  session.focusedPane = session.claudePaneId;
  updatePaneFocus(sessionId);

  // Refit remaining panes
  requestAnimationFrame(() => {
    for (const p of session.panes.values()) {
      p.fitAddon.fit();
    }
  });
}

async function closeSessionTab(sessionId) {
  const session = state.sessions.get(sessionId);
  if (!session) return;

  // Cleanup all panes
  for (const pane of session.panes.values()) {
    if (pane.unlistenOutput) pane.unlistenOutput();
    if (pane.unlistenExit) pane.unlistenExit();
    pane.terminal.dispose();
  }
  window.removeEventListener("resize", session.resizeHandler);
  session.containerEl.remove();
  state.sessions.delete(sessionId);

  // Kill the backend PTYs and session (worktree cleanup, DB close)
  invoke("kill_session", { sessionId }).catch((e) => {
    console.error("Failed to clean up session:", e);
  });

  // Switch to dashboard if the closed tab was active
  if (state.activeTab === sessionId) {
    switchTab("dashboard");
  } else {
    renderTabBar();
  }
}

// Find session for a task (by checking DB sessions)
function findSessionForTask(task) {
  if (!task.session_id) return null;
  return state.sessions.get(task.session_id) ? task.session_id : null;
}

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

  let activeTasks = state.tasks.filter((t) => t.status !== "done");
  let doneTasks = state.tasks.filter((t) => t.status === "done");

  // Apply filter
  if (state.taskFilter) {
    const filter = state.taskFilter.toLowerCase();
    activeTasks = activeTasks.filter((t) => t.title.toLowerCase().includes(filter));
    doneTasks = doneTasks.filter((t) => t.title.toLowerCase().includes(filter));
  }

  if (state.tasks.length === 0) {
    container.innerHTML = '<div class="empty-state"><p>No tasks yet. Create one to get started.</p></div>';
    updateAttentionCount();
    return;
  }

  if (activeTasks.length === 0 && doneTasks.length > 0) {
    container.innerHTML = `<div class="empty-state"><p>All ${doneTasks.length} tasks done.</p></div>`;
  }

  if (activeTasks.length === 0 && doneTasks.length === 0 && state.taskFilter) {
    container.innerHTML = '<div class="empty-state"><p>No tasks match filter.</p></div>';
  }

  // Sort active tasks by priority then sort_order
  const sorted = [...activeTasks].sort((a, b) => {
    const priorityA = statusSortPriority(a.status);
    const priorityB = statusSortPriority(b.status);
    if (priorityA !== priorityB) return priorityA - priorityB;
    return a.sort_order - b.sort_order;
  });

  for (const task of sorted) {
    const el = createTaskElement(task);
    container.appendChild(el);
  }

  // Collapsible done section (preserve expanded state across re-renders)
  if (doneTasks.length > 0) {
    const toggle = document.createElement("button");
    toggle.className = `done-toggle${state.doneExpanded ? " expanded" : ""}`;
    toggle.textContent = `${doneTasks.length} done`;
    const doneContainer = document.createElement("div");
    doneContainer.className = `done-section${state.doneExpanded ? "" : " hidden"}`;
    for (const task of doneTasks) {
      doneContainer.appendChild(createTaskElement(task));
    }
    toggle.addEventListener("click", () => {
      state.doneExpanded = !state.doneExpanded;
      doneContainer.classList.toggle("hidden");
      toggle.classList.toggle("expanded");
    });
    container.appendChild(toggle);
    container.appendChild(doneContainer);
  }

  updateAttentionCount();
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
  const hasSession = findSessionForTask(task);
  div.className = `task-item${task.status === "done" ? " task-done" : ""}${task.id === state.selectedTaskId ? " selected" : ""}${hasSession ? " has-session" : ""}`;
  div.dataset.id = task.id;

  const cfg = STATUS_CONFIG[task.status] || { symbol: "?", label: task.status };
  const tokens = task.input_tokens + task.output_tokens;
  const tokenStr = tokens > 0 ? formatTokens(tokens) : "";

  const ciHtml = task.ci_status
    ? `<span class="ci-badge ci-${task.ci_status}">${task.ci_status === "running" ? "\u27f3" : task.ci_status === "passed" ? "\u2714" : "\u2718"} CI</span>`
    : "";

  div.innerHTML = `
    <span class="task-status-icon">${cfg.symbol}</span>
    <div class="task-info">
      <span class="task-title-text">${escapeHtml(task.title)}</span>
      <div class="task-meta">
        ${tokenStr ? `<span class="token-display">${tokenStr}</span>` : ""}
        ${task.pr_url ? '<span>PR</span>' : ""}
        ${ciHtml}
      </div>
    </div>
    <div class="task-badges">
      <span class="badge badge-${task.status}">${cfg.label}</span>
      <span class="badge badge-${task.mode}">${task.mode}</span>
    </div>
    <div class="task-actions">
      ${task.status === "pending" || task.status === "draft" ? `<button class="task-action-btn" data-action="launch" title="Launch">Launch</button>` : ""}
      ${task.status === "pending" || task.status === "draft" ? `<button class="task-action-btn" data-action="edit" title="Edit">Edit</button>` : ""}
      ${hasSession ? `<button class="task-action-btn" data-action="goto" title="Go to session">Terminal</button>` : ""}
      ${task.status === "working" || task.status === "in_review" ? `<button class="task-action-btn" data-action="done" title="Mark done">Done</button>` : ""}
      ${task.session_id ? `<button class="task-action-btn danger" data-action="kill" title="Kill session">Kill</button>` : ""}
      ${task.pr_url ? `<button class="task-action-btn" data-action="open-pr" title="Open PR">PR</button>` : ""}
      <button class="task-action-btn" data-action="subtasks" title="Subtasks">Sub</button>
      <button class="task-action-btn danger" data-action="delete" title="Delete">Del</button>
    </div>
  `;

  // Click to select; double-click to go to session
  div.addEventListener("click", (e) => {
    if (e.target.closest(".task-actions")) return;
    selectTask(task.id);
  });
  div.addEventListener("dblclick", (e) => {
    if (e.target.closest(".task-actions")) return;
    if (hasSession) switchTab(hasSession);
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
      case "launch": {
        const session = await invoke("launch_task", { taskId: task.id });
        showToast("Task launched", "success");
        await refreshAll();
        await addSessionTab(session.id, session.tab_label, session.worktree_path);
        break;
      }

      case "goto": {
        const sid = findSessionForTask(task);
        if (sid) switchTab(sid);
        break;
      }

      case "edit":
        showTaskForm(task);
        break;

      case "done":
        await invoke("mark_task_done", { taskId: task.id });
        showToast("Task marked done", "success");
        // Close the session tab if it exists
        if (task.session_id && state.sessions.has(task.session_id)) {
          await closeSessionTab(task.session_id);
        }
        hideDetailPanel();
        await refreshAll();
        break;

      case "delete":
        showConfirm(`Delete task "${task.title}"?`, async () => {
          // Kill session first if task has one
          if (task.session_id) {
            if (state.sessions.has(task.session_id)) {
              await closeSessionTab(task.session_id);
            }
            await invoke("kill_session", { sessionId: task.session_id });
          }
          await invoke("delete_task", { id: task.id });
          showToast("Task deleted", "success");
          hideDetailPanel();
          await refreshAll();
        });
        break;

      case "kill":
        if (task.session_id) {
          showConfirm(`Kill session for "${task.title}"?`, async () => {
            await invoke("kill_session", { sessionId: task.session_id });
            if (state.sessions.has(task.session_id)) {
              await closeSessionTab(task.session_id);
            }
            showToast("Session killed", "success");
            await refreshAll();
          });
        }
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
    const [stats, extSessions] = await Promise.all([
      invoke("get_project_stats", { projectId: state.selectedProjectId }),
      invoke("list_external_sessions").catch(() => []),
    ]);

    const body = document.getElementById("stats-body");
    let html = `
      <div class="stat-card"><div class="stat-label">Total Tasks</div><div class="stat-value">${stats.total_tasks}</div></div>
      <div class="stat-card"><div class="stat-label">Completed</div><div class="stat-value">${stats.completed_tasks}</div></div>
      <div class="stat-card"><div class="stat-label">Sessions</div><div class="stat-value">${stats.total_sessions}</div></div>
      <div class="stat-card"><div class="stat-label">Total Time</div><div class="stat-value">${stats.total_time}</div></div>
      <div class="stat-card"><div class="stat-label">Total Tokens</div><div class="stat-value">${formatTokens(stats.total_tokens)}</div></div>
      <div class="stat-card"><div class="stat-label">Avg Task Time</div><div class="stat-value">${stats.avg_task_time}</div></div>
    `;

    if (extSessions.length > 0) {
      html += `<div style="grid-column:1/-1;margin-top:8px;border-top:1px solid var(--border);padding-top:12px">
        <div style="font-size:11px;text-transform:uppercase;letter-spacing:0.5px;color:var(--text-muted);margin-bottom:8px">External Sessions (${extSessions.length})</div>
        ${extSessions
          .map(
            (s) => `<div style="font-size:12px;color:var(--text-secondary);padding:4px 0;font-family:var(--font-mono)">
            ${escapeHtml(s.project_name)}${s.git_branch ? ` \u2022 ${escapeHtml(s.git_branch)}` : ""}${s.model ? ` \u2022 ${escapeHtml(s.model)}` : ""} \u2022 ${formatTokens(s.input_tokens + s.output_tokens)} tokens
          </div>`
          )
          .join("")}
      </div>`;
    }

    body.innerHTML = html;
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

let _confirmHandler = null;
function showConfirm(message, onConfirm) {
  document.getElementById("confirm-message").textContent = message;
  document.getElementById("confirm-overlay").classList.remove("hidden");

  const btn = document.getElementById("confirm-action");
  // Remove any stale handler from a previous confirm that was dismissed
  if (_confirmHandler) btn.removeEventListener("click", _confirmHandler);
  _confirmHandler = async () => {
    btn.removeEventListener("click", _confirmHandler);
    _confirmHandler = null;
    document.getElementById("confirm-overlay").classList.add("hidden");
    await onConfirm();
  };
  btn.addEventListener("click", _confirmHandler);
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
  // Escape closes overlays, then filter
  if (e.key === "Escape") {
    const openOverlays = document.querySelectorAll(".overlay:not(.hidden)");
    if (openOverlays.length > 0) {
      openOverlays.forEach((o) => o.classList.add("hidden"));
      return;
    }
    if (state.taskFilter) {
      clearTaskFilter();
      return;
    }
    return;
  }

  // Command palette: Ctrl+P (works from dashboard)
  if (e.ctrlKey && e.key === "p" && state.activeTab === "dashboard") {
    e.preventDefault();
    showCommandPalette();
    return;
  }

  // Tab navigation: Ctrl+J / Ctrl+K (works from any view)
  if (e.ctrlKey && (e.key === "j" || e.key === "k")) {
    if (state.sessions.size > 0) {
      const tabIds = ["dashboard", ...state.sessions.keys()];
      const currentIdx = tabIds.indexOf(state.activeTab);
      const next = e.key === "j"
        ? (currentIdx + 1) % tabIds.length
        : (currentIdx - 1 + tabIds.length) % tabIds.length;
      switchTab(tabIds[next]);
      e.preventDefault();
      return;
    }
  }

  // Ctrl+D returns to dashboard from session
  if (e.ctrlKey && e.key === "d" && state.activeTab !== "dashboard") {
    switchTab("dashboard");
    e.preventDefault();
    return;
  }

  // Session pane management (when in a session tab)
  if (state.activeTab !== "dashboard" && e.ctrlKey) {
    const sessionId = state.activeTab;
    switch (e.key) {
      case "h": // Focus previous pane
        e.preventDefault();
        cyclePaneFocus(sessionId, -1);
        return;
      case "l": // Focus next pane
        e.preventDefault();
        cyclePaneFocus(sessionId, 1);
        return;
      case "r": // Split right (horizontal)
        e.preventDefault();
        splitPane(sessionId, "horizontal");
        return;
      case "b": // Split down (vertical)
        e.preventDefault();
        splitPane(sessionId, "vertical");
        return;
      case "w": // Close focused pane
        e.preventDefault();
        closePane(sessionId);
        return;
    }
  }

  // Don't handle shortcuts when typing in forms or in a terminal session
  if (e.target.tagName === "INPUT" || e.target.tagName === "TEXTAREA" || e.target.tagName === "SELECT") {
    return;
  }
  if (state.activeTab !== "dashboard") return;

  // Don't handle shortcuts when overlays are open
  if (document.querySelector(".overlay:not(.hidden)")) return;

  const selectedTask = state.tasks.find((t) => t.id === state.selectedTaskId);

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
    case "?":
      showHelp();
      break;
    case "i":
      showSkills();
      break;
    case "c":
      showConfigure();
      break;
    case "/":
      e.preventDefault();
      showTaskFilter();
      break;
    case "e":
      if (selectedTask && (selectedTask.status === "pending" || selectedTask.status === "draft")) {
        showTaskForm(selectedTask);
      }
      break;
    case "l":
      if (selectedTask && (selectedTask.status === "pending" || selectedTask.status === "draft")) {
        handleTaskAction("launch", selectedTask);
      }
      break;
    case "k":
      if (selectedTask && selectedTask.session_id) {
        handleTaskAction("kill", selectedTask);
      }
      break;
    case "r":
      if (selectedTask && (selectedTask.status === "working" || selectedTask.status === "in_review")) {
        handleTaskAction("done", selectedTask);
      }
      break;
    case "o":
      if (selectedTask && selectedTask.pr_url) {
        openUrl(selectedTask.pr_url);
      }
      break;
    case "d":
      if (selectedTask) {
        handleTaskAction("delete", selectedTask);
      }
      break;
    case "v":
      if (selectedTask) selectTask(selectedTask.id);
      break;
    case "j":
    case "ArrowDown":
      navigateTasks(1);
      break;
    case "k":
      // k is already used for kill above — only navigate if no selected task or task has no session
      break;
    case "ArrowUp":
      navigateTasks(-1);
      break;
    case "Enter":
      if (selectedTask) {
        const sid = findSessionForTask(selectedTask);
        if (sid) switchTab(sid);
      }
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

// ---------------------------------------------------------------------------
// Task Navigation (j/k keys)
// ---------------------------------------------------------------------------

function navigateTasks(direction) {
  const visible = getVisibleTasks();
  if (visible.length === 0) return;

  const currentIdx = visible.findIndex((t) => t.id === state.selectedTaskId);
  let nextIdx;
  if (currentIdx === -1) {
    nextIdx = direction > 0 ? 0 : visible.length - 1;
  } else {
    nextIdx = currentIdx + direction;
    if (nextIdx < 0) nextIdx = 0;
    if (nextIdx >= visible.length) nextIdx = visible.length - 1;
  }

  state.selectedTaskId = visible[nextIdx].id;
  renderTaskList();
  showTaskDetail(visible[nextIdx].id);
}

function getVisibleTasks() {
  let tasks = state.tasks.filter((t) => t.status !== "done");
  if (state.taskFilter) {
    const filter = state.taskFilter.toLowerCase();
    tasks = tasks.filter((t) => t.title.toLowerCase().includes(filter));
  }
  return tasks.sort((a, b) => {
    const pa = statusSortPriority(a.status);
    const pb = statusSortPriority(b.status);
    if (pa !== pb) return pa - pb;
    return a.sort_order - b.sort_order;
  });
}

// ---------------------------------------------------------------------------
// Task Filter
// ---------------------------------------------------------------------------

function showTaskFilter() {
  const bar = document.getElementById("task-filter-bar");
  bar.classList.remove("hidden");
  const input = document.getElementById("task-filter-input");
  input.value = state.taskFilter;
  input.focus();
}

function clearTaskFilter() {
  state.taskFilter = "";
  document.getElementById("task-filter-input").value = "";
  document.getElementById("task-filter-bar").classList.add("hidden");
  renderTaskList();
}

// ---------------------------------------------------------------------------
// Attention Counter
// ---------------------------------------------------------------------------

function updateAttentionCount() {
  const attention = state.tasks.filter((t) =>
    ["in_review", "conflict", "ci_failed"].includes(t.status)
  ).length;

  const el = document.getElementById("attention-count");
  el.textContent = attention > 0
    ? `${attention} task${attention !== 1 ? "s" : ""} need${attention === 1 ? "s" : ""} attention`
    : "";
}

// ---------------------------------------------------------------------------
// Help Overlay
// ---------------------------------------------------------------------------

function showHelp() {
  const body = document.getElementById("help-body");
  const sections = [
    {
      title: "Navigation",
      bindings: [
        ["j / \u2193", "Select next task"],
        ["\u2191", "Select previous task"],
        ["Enter", "Go to session terminal"],
        ["Ctrl+J/K", "Switch tabs"],
        ["Ctrl+D", "Return to dashboard"],
        ["Ctrl+P", "Command palette"],
        ["/", "Filter tasks"],
        ["?", "Show this help"],
        ["Esc", "Close overlay / clear filter"],
      ],
    },
    {
      title: "Projects",
      bindings: [
        ["a", "Add project"],
        ["s", "Show stats"],
      ],
    },
    {
      title: "Tasks",
      bindings: [
        ["n", "New task"],
        ["e", "Edit task (pending/draft)"],
        ["l", "Launch task"],
        ["r", "Mark done"],
        ["d", "Delete task"],
        ["v", "View task details"],
        ["o", "Open PR in browser"],
      ],
    },
    {
      title: "Other",
      bindings: [
        ["i", "Skills panel"],
        ["c", "Configure permissions"],
      ],
    },
    {
      title: "Session Tab",
      bindings: [
        ["Ctrl+H", "Focus previous pane"],
        ["Ctrl+L", "Focus next pane"],
        ["Ctrl+R", "Split right (new shell)"],
        ["Ctrl+B", "Split down (new shell)"],
        ["Ctrl+W", "Close focused pane"],
        ["Ctrl+D", "Return to dashboard"],
        ["Ctrl+J/K", "Switch tabs"],
      ],
    },
  ];

  body.innerHTML = sections
    .map(
      (s) => `
    <div class="help-section">
      <h3>${s.title}</h3>
      ${s.bindings.map(([key, desc]) => `<div class="help-row"><span class="help-key">${escapeHtml(key)}</span><span class="help-desc">${escapeHtml(desc)}</span></div>`).join("")}
    </div>
  `
    )
    .join("");

  document.getElementById("help-overlay").classList.remove("hidden");
}

// ---------------------------------------------------------------------------
// Command Palette
// ---------------------------------------------------------------------------

const PALETTE_COMMANDS = [
  { label: "New Task", shortcut: "n", action: () => state.selectedProjectId && showTaskForm() },
  { label: "Add Project", shortcut: "a", action: showProjectForm },
  { label: "Show Stats", shortcut: "s", action: () => state.selectedProjectId && showStats() },
  { label: "Skills", shortcut: "i", action: showSkills },
  { label: "Configure Permissions", shortcut: "c", action: showConfigure },
  { label: "Filter Tasks", shortcut: "/", action: showTaskFilter },
  { label: "Help", shortcut: "?", action: showHelp },
];

function showCommandPalette() {
  const overlay = document.getElementById("palette-overlay");
  const input = document.getElementById("palette-input");
  overlay.classList.remove("hidden");
  input.value = "";
  state.paletteIndex = 0;
  renderPaletteResults();
  input.focus();
}

function renderPaletteResults() {
  const query = document.getElementById("palette-input").value.toLowerCase();
  const filtered = PALETTE_COMMANDS.filter((c) => c.label.toLowerCase().includes(query));
  const container = document.getElementById("palette-results");

  if (state.paletteIndex >= filtered.length) state.paletteIndex = 0;

  container.innerHTML = filtered
    .map(
      (cmd, i) => `
    <div class="palette-item${i === state.paletteIndex ? " selected" : ""}" data-index="${i}">
      <span>${escapeHtml(cmd.label)}</span>
      <span class="palette-shortcut">${escapeHtml(cmd.shortcut)}</span>
    </div>
  `
    )
    .join("");

  container.querySelectorAll(".palette-item").forEach((el) => {
    el.addEventListener("click", () => {
      const idx = parseInt(el.dataset.index, 10);
      executePaletteCommand(filtered[idx]);
    });
  });
}

function handlePaletteKeydown(e) {
  const query = document.getElementById("palette-input").value.toLowerCase();
  const filtered = PALETTE_COMMANDS.filter((c) => c.label.toLowerCase().includes(query));

  if (e.key === "ArrowDown" || (e.ctrlKey && e.key === "n")) {
    e.preventDefault();
    state.paletteIndex = Math.min(state.paletteIndex + 1, filtered.length - 1);
    renderPaletteResults();
  } else if (e.key === "ArrowUp" || (e.ctrlKey && e.key === "p")) {
    e.preventDefault();
    state.paletteIndex = Math.max(state.paletteIndex - 1, 0);
    renderPaletteResults();
  } else if (e.key === "Enter") {
    e.preventDefault();
    if (filtered[state.paletteIndex]) {
      executePaletteCommand(filtered[state.paletteIndex]);
    }
  } else if (e.key === "Escape") {
    document.getElementById("palette-overlay").classList.add("hidden");
  }
}

function executePaletteCommand(cmd) {
  document.getElementById("palette-overlay").classList.add("hidden");
  cmd.action();
}

// ---------------------------------------------------------------------------
// Skills Panel
// ---------------------------------------------------------------------------

async function showSkills() {
  document.getElementById("skills-overlay").classList.remove("hidden");
  await loadInstalledSkills();
}

async function loadInstalledSkills() {
  try {
    const projectPath = getSelectedProjectPath();
    const skills = await invoke("list_installed_skills", {
      global: state.skillsGlobal,
      projectPath: state.skillsGlobal ? null : projectPath,
    });
    state.installedSkills = skills;
    state.selectedSkillIndex = skills.length > 0 ? 0 : -1;
    renderSkillsList();
    renderSkillDetail();
  } catch (e) {
    showToast(`Error loading skills: ${e}`, "error");
  }
}

function getSelectedProjectPath() {
  const p = state.projects.find((p) => p.id === state.selectedProjectId);
  return p ? p.repo_path : null;
}

function renderSkillsList() {
  const container = document.getElementById("skills-list");
  if (state.installedSkills.length === 0) {
    container.innerHTML = '<div class="empty-state" style="padding:20px"><p>No skills installed</p></div>';
    return;
  }

  container.innerHTML = state.installedSkills
    .map(
      (s, i) => `
    <div class="skill-item${i === state.selectedSkillIndex ? " selected" : ""}" data-index="${i}">
      <span class="skill-name">${escapeHtml(s.name)}</span>
      <span class="skill-remove" data-name="${escapeHtml(s.name)}" title="Remove">&times;</span>
    </div>
  `
    )
    .join("");

  container.querySelectorAll(".skill-item").forEach((el) => {
    el.addEventListener("click", (e) => {
      if (e.target.classList.contains("skill-remove")) return;
      state.selectedSkillIndex = parseInt(el.dataset.index, 10);
      renderSkillsList();
      renderSkillDetail();
    });
  });

  container.querySelectorAll(".skill-remove").forEach((el) => {
    el.addEventListener("click", async (e) => {
      e.stopPropagation();
      const name = el.dataset.name;
      try {
        await invoke("remove_skill", {
          name,
          global: state.skillsGlobal,
          projectPath: state.skillsGlobal ? null : getSelectedProjectPath(),
        });
        showToast(`Removed ${name}`, "success");
        await loadInstalledSkills();
      } catch (err) {
        showToast(`Error: ${err}`, "error");
      }
    });
  });
}

async function renderSkillDetail() {
  const detail = document.getElementById("skills-detail");
  if (state.selectedSkillIndex < 0 || state.selectedSkillIndex >= state.installedSkills.length) {
    detail.innerHTML = '<p class="empty-state" style="padding:20px">Select a skill to view details</p>';
    return;
  }

  const skill = state.installedSkills[state.selectedSkillIndex];
  let mdContent = "";
  try {
    mdContent = await invoke("read_skill_content", { path: skill.path });
  } catch {
    mdContent = "(No SKILL.md found)";
  }

  detail.innerHTML = `
    <h3>${escapeHtml(skill.name)}</h3>
    <div class="skill-agents">Agents: ${skill.agents.length > 0 ? escapeHtml(skill.agents.join(", ")) : "not linked"}</div>
    <div class="skill-md">${escapeHtml(mdContent)}</div>
  `;
}

function toggleSkillScope() {
  state.skillsGlobal = !state.skillsGlobal;
  document.getElementById("skills-scope-toggle").textContent = state.skillsGlobal ? "Global" : "Project";
  loadInstalledSkills();
}

async function updateAllSkills() {
  try {
    showToast("Updating skills...");
    const result = await invoke("update_all_skills");
    showToast("Skills updated", "success");
    await loadInstalledSkills();
  } catch (e) {
    showToast(`Error: ${e}`, "error");
  }
}

// ---------------------------------------------------------------------------
// Skills Search
// ---------------------------------------------------------------------------

function showSkillSearch() {
  document.getElementById("skills-search-overlay").classList.remove("hidden");
  document.getElementById("skills-search-input").value = "";
  document.getElementById("skills-search-results").innerHTML = "";
  document.getElementById("skills-search-status").textContent = "";
  document.getElementById("skills-search-install").style.display = "none";
  document.getElementById("skills-search-input").focus();
}

async function searchSkills() {
  const query = document.getElementById("skills-search-input").value.trim();
  if (!query) return;

  const status = document.getElementById("skills-search-status");
  status.textContent = "Searching...";

  try {
    const results = await invoke("find_skills_search", { query });
    status.textContent = results.length > 0 ? `${results.length} results` : "No results found";

    const container = document.getElementById("skills-search-results");
    container.innerHTML = results
      .map(
        (r, i) => `
      <div class="search-result-item" data-index="${i}">
        <input type="checkbox" data-package="${escapeHtml(r.package)}" />
        <span class="result-package">${escapeHtml(r.package)}</span>
        <span class="result-installs">${escapeHtml(r.installs)} installs</span>
      </div>
    `
      )
      .join("");

    // Show install button when there are results
    document.getElementById("skills-search-install").style.display = results.length > 0 ? "" : "none";

    // Toggle checkbox on row click
    container.querySelectorAll(".search-result-item").forEach((el) => {
      el.addEventListener("click", (e) => {
        if (e.target.tagName === "INPUT") return;
        const cb = el.querySelector('input[type="checkbox"]');
        cb.checked = !cb.checked;
      });
    });
  } catch (e) {
    status.textContent = `Error: ${e}`;
  }
}

async function installSelectedSkills() {
  const checkboxes = document.querySelectorAll('#skills-search-results input[type="checkbox"]:checked');
  if (checkboxes.length === 0) {
    showToast("Select skills to install", "error");
    return;
  }

  for (const cb of checkboxes) {
    const pkg = cb.dataset.package;
    try {
      await invoke("add_skill_package", {
        package: pkg,
        global: state.skillsGlobal,
        projectPath: state.skillsGlobal ? null : getSelectedProjectPath(),
      });
      showToast(`Installed ${pkg}`, "success");
    } catch (e) {
      showToast(`Failed to install ${pkg}: ${e}`, "error");
    }
  }

  document.getElementById("skills-search-overlay").classList.add("hidden");
  await loadInstalledSkills();
}

// ---------------------------------------------------------------------------
// Skills Add by Package
// ---------------------------------------------------------------------------

function showSkillAdd() {
  document.getElementById("skills-add-overlay").classList.remove("hidden");
  document.getElementById("skills-add-input").value = "";
  document.getElementById("skills-add-input").focus();
}

async function addSkillByPackage() {
  const pkg = document.getElementById("skills-add-input").value.trim();
  if (!pkg) return;

  try {
    await invoke("add_skill_package", {
      package: pkg,
      global: state.skillsGlobal,
      projectPath: state.skillsGlobal ? null : getSelectedProjectPath(),
    });
    showToast(`Installed ${pkg}`, "success");
    document.getElementById("skills-add-overlay").classList.add("hidden");
    await loadInstalledSkills();
  } catch (e) {
    showToast(`Error: ${e}`, "error");
  }
}

// ---------------------------------------------------------------------------
// Configure Wizard
// ---------------------------------------------------------------------------

async function checkPermissions() {
  try {
    const status = await invoke("check_permissions");
    state.permissionsAligned = status.is_aligned;
    if (!status.is_aligned) {
      const versionEl = document.getElementById("version");
      versionEl.innerHTML += ' <span style="color:var(--warning)">⚠ Permissions</span>';
    }
  } catch {
    // Ignore — permissions check is best-effort
  }
}

async function showConfigure() {
  const body = document.getElementById("configure-body");
  const actions = document.getElementById("configure-actions");
  body.innerHTML = "Loading...";

  document.getElementById("configure-overlay").classList.remove("hidden");

  try {
    const status = await invoke("check_permissions");
    let html = "";

    for (const diff of status.diffs) {
      const icon = diff.is_aligned
        ? '<span class="check-ok">\u2713</span>'
        : '<span class="check-missing">\u2717</span>';

      html += `<div class="configure-category">
        <h4>${icon} ${escapeHtml(diff.category)}${diff.is_aligned ? "" : ` (${diff.missing.length} missing)`}</h4>
        ${diff.missing.length > 0 ? `<ul class="configure-missing">${diff.missing.map((m) => `<li>${escapeHtml(m)}</li>`).join("")}</ul>` : ""}
      </div>`;
    }

    if (status.is_aligned) {
      html += '<p style="color:var(--success);margin-top:12px">All permissions aligned with recommendations.</p>';
      actions.style.display = "none";
    } else {
      actions.style.display = "";
    }

    body.innerHTML = html;
  } catch (e) {
    body.innerHTML = `<p style="color:var(--error)">Error: ${escapeHtml(String(e))}</p>`;
    actions.style.display = "none";
  }
}

async function applyPermissions() {
  try {
    const result = await invoke("apply_permissions");
    showToast(result, "success");
    state.permissionsAligned = true;
    await showConfigure(); // Refresh the view
  } catch (e) {
    showToast(`Error: ${e}`, "error");
  }
}
