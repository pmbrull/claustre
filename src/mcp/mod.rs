use std::sync::Arc;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::Mutex;

use crate::config;
use crate::session::AUTONOMOUS_SUFFIX;
use crate::store::{ClaudeStatus, Store, TaskStatus};

/// Shared store wrapped for async access from the MCP server.
pub type SharedStore = Arc<Mutex<Store>>;

/// Callback for notifications when a task transitions to `in_review`.
pub type NotifyFn = Arc<dyn Fn(&str) + Send + Sync>;

/// Start the MCP server on a Unix domain socket.
pub async fn start_server(store: SharedStore, notify: Option<NotifyFn>) -> Result<()> {
    let socket_path = config::mcp_socket_path()?;

    // Clean up stale socket
    if socket_path.exists() {
        std::fs::remove_file(&socket_path)?;
    }

    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("failed to bind MCP socket at {}", socket_path.display()))?;

    tracing::info!("MCP server listening on {}", socket_path.display());

    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let store = store.clone();
                let notify = notify.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_connection(stream, store, notify).await {
                        tracing::error!("MCP connection error: {}", e);
                    }
                });
            }
            Err(e) => {
                tracing::error!("MCP accept error: {}", e);
            }
        }
    }
}

/// Generate the .mcp.json content for a worktree to connect to claustre's MCP server.
/// Uses `claustre mcp-bridge` for stdio-to-unix-socket bridging.
pub fn mcp_config_json(session_id: &str) -> Result<Value> {
    Ok(serde_json::json!({
        "mcpServers": {
            "claustre": {
                "type": "stdio",
                "command": "claustre",
                "args": ["mcp-bridge"],
                "env": {
                    "CLAUSTRE_SESSION_ID": session_id
                }
            }
        }
    }))
}

/// Run the MCP stdio-to-unix-socket bridge.
///
/// Connects to the claustre MCP socket and bridges stdin/stdout to it.
/// The bridge reads `CLAUSTRE_SESSION_ID` from its own environment and
/// automatically injects `session_id` into all `tools/call` requests so
/// that Claude never needs to know or provide the session ID.
pub async fn run_bridge() -> Result<()> {
    let session_id =
        std::env::var("CLAUSTRE_SESSION_ID").context("CLAUSTRE_SESSION_ID env var not set")?;

    let socket_path = config::mcp_socket_path()?;
    let stream = tokio::net::UnixStream::connect(&socket_path)
        .await
        .with_context(|| {
            format!(
                "failed to connect to MCP socket at {}",
                socket_path.display()
            )
        })?;

    let (sock_read, mut sock_write) = stream.into_split();
    let stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();

    // stdin → socket: parse messages and inject session_id into tools/call
    let stdin_to_sock = async {
        let mut reader = BufReader::new(stdin);
        loop {
            let msg = read_bridge_message(&mut reader).await?;
            let Some(msg) = msg else { break };
            let modified = inject_session_id(&msg, &session_id);
            let header = format!("Content-Length: {}\r\n\r\n", modified.len());
            sock_write.write_all(header.as_bytes()).await?;
            sock_write.write_all(modified.as_bytes()).await?;
            sock_write.flush().await?;
        }
        anyhow::Ok(())
    };

    // socket → stdout: raw copy (no modification needed)
    let sock_to_stdout = async {
        let mut sock_read = sock_read;
        tokio::io::copy(&mut sock_read, &mut stdout)
            .await
            .context("socket -> stdout copy failed")?;
        anyhow::Ok(())
    };

    tokio::select! {
        r = stdin_to_sock => { r?; }
        r = sock_to_stdout => { r?; }
    };

    Ok(())
}

/// Read a single Content-Length framed message from an async `BufRead` source.
async fn read_bridge_message<R: AsyncBufReadExt + Unpin>(reader: &mut R) -> Result<Option<String>> {
    let mut content_length: Option<usize> = None;

    loop {
        let mut header_line = String::new();
        let bytes_read = reader.read_line(&mut header_line).await?;
        if bytes_read == 0 {
            return Ok(None);
        }

        let trimmed = header_line.trim();
        if trimmed.is_empty() {
            break;
        }

        if let Some(value) = trimmed.strip_prefix("Content-Length:") {
            content_length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .context("invalid Content-Length value")?,
            );
        }
    }

    let Some(length) = content_length else {
        return Ok(None);
    };

    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).await?;

    let message = String::from_utf8(body).context("invalid UTF-8 in bridge message body")?;
    Ok(Some(message))
}

/// If the message is a `tools/call` JSON-RPC request, inject `session_id`
/// into `params.arguments`. Returns the (possibly modified) message string.
fn inject_session_id(message: &str, session_id: &str) -> String {
    let Ok(mut parsed) = serde_json::from_str::<Value>(message) else {
        return message.to_string();
    };

    let is_tools_call = parsed
        .get("method")
        .and_then(|m| m.as_str())
        .is_some_and(|m| m == "tools/call");

    if is_tools_call && let Some(params) = parsed.get_mut("params") {
        if let Some(arguments) = params.get_mut("arguments") {
            if let Some(obj) = arguments.as_object_mut() {
                obj.insert(
                    "session_id".to_string(),
                    Value::String(session_id.to_string()),
                );
            }
        } else if let Some(p) = params.as_object_mut() {
            p.insert(
                "arguments".to_string(),
                serde_json::json!({"session_id": session_id}),
            );
        }
    }

    // Unwrap is safe: we just parsed it successfully
    serde_json::to_string(&parsed).expect("re-serialization of valid JSON cannot fail")
}

// ── JSON-RPC types ──

#[derive(Debug, Deserialize)]
#[expect(
    dead_code,
    reason = "fields deserialized from JSON-RPC but not all read directly"
)]
struct JsonRpcRequest {
    jsonrpc: String,
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: String,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
struct JsonRpcError {
    code: i64,
    message: String,
}

impl JsonRpcResponse {
    fn success(id: Value, result: Value) -> Self {
        JsonRpcResponse {
            jsonrpc: "2.0".into(),
            id,
            result: Some(result),
            error: None,
        }
    }

    fn error(id: Value, code: i64, message: String) -> Self {
        JsonRpcResponse {
            jsonrpc: "2.0".into(),
            id,
            result: None,
            error: Some(JsonRpcError { code, message }),
        }
    }
}

// ── MCP Protocol Messages ──

#[derive(Debug, Serialize)]
struct McpToolDefinition {
    name: String,
    description: String,
    #[serde(rename = "inputSchema")]
    input_schema: Value,
}

fn tool_definitions() -> Vec<McpToolDefinition> {
    vec![
        McpToolDefinition {
            name: "claustre_status".into(),
            description: "Report the current status of this Claude session to claustre. Call this whenever you start working on something new, encounter an issue, or finish a task.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "state": {
                        "type": "string",
                        "enum": ["working", "waiting_for_input", "done", "error"],
                        "description": "Current session state"
                    },
                    "message": {
                        "type": "string",
                        "description": "Short human-readable description of what you're doing right now"
                    }
                },
                "required": ["state", "message"]
            }),
        },
        McpToolDefinition {
            name: "claustre_task_done".into(),
            description: "Signal that the current task is complete and ready for review. You MUST commit, push, and create a PR before calling this. Claustre will transition the task to in_review status. If there are more autonomous tasks queued, the next one will be started automatically.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "summary": {
                        "type": "string",
                        "description": "Brief summary of what was accomplished in this task"
                    },
                    "pr_url": {
                        "type": "string",
                        "description": "URL of the pull request created for this task"
                    }
                },
                "required": ["summary", "pr_url"]
            }),
        },
        McpToolDefinition {
            name: "claustre_usage".into(),
            description: "Report token usage and cost for the current task. Call this at the end of a task or periodically for long-running tasks.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "input_tokens": {
                        "type": "integer",
                        "description": "Number of input tokens used"
                    },
                    "output_tokens": {
                        "type": "integer",
                        "description": "Number of output tokens used"
                    },
                    "cost": {
                        "type": "number",
                        "description": "Estimated cost in USD"
                    }
                },
                "required": ["input_tokens", "output_tokens", "cost"]
            }),
        },
        McpToolDefinition {
            name: "claustre_log".into(),
            description: "Send a structured log message to claustre for tracking and debugging.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "level": {
                        "type": "string",
                        "enum": ["info", "warn", "error"],
                        "description": "Log level"
                    },
                    "message": {
                        "type": "string",
                        "description": "Log message"
                    }
                },
                "required": ["level", "message"]
            }),
        },
        McpToolDefinition {
            name: "claustre_rate_limited".into(),
            description: "Report that you have hit a rate limit. Claustre will pause all autonomous task feeding until the limit resets.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "limit_type": {
                        "type": "string",
                        "enum": ["5h", "7d"],
                        "description": "Which rate limit window was hit"
                    },
                    "reset_at": {
                        "type": "string",
                        "description": "ISO 8601 timestamp when the limit resets (optional)"
                    },
                    "usage_5h_pct": {
                        "type": "number",
                        "description": "Current 5h window usage percentage (0-100)"
                    },
                    "usage_7d_pct": {
                        "type": "number",
                        "description": "Current 7d window usage percentage (0-100)"
                    }
                },
                "required": ["limit_type"]
            }),
        },
        McpToolDefinition {
            name: "claustre_usage_windows".into(),
            description: "Report your current usage window percentages so the claustre dashboard stays updated. Call this periodically.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "usage_5h_pct": {
                        "type": "number",
                        "description": "Current 5h window usage percentage (0-100)"
                    },
                    "usage_7d_pct": {
                        "type": "number",
                        "description": "Current 7d window usage percentage (0-100)"
                    }
                },
                "required": ["usage_5h_pct", "usage_7d_pct"]
            }),
        },
    ]
}

// ── MCP Content-Length framed transport ──

/// Read a single MCP message using Content-Length header framing.
/// Format: "Content-Length: N\r\n\r\n{json body of N bytes}"
async fn read_mcp_message(
    reader: &mut BufReader<tokio::net::unix::OwnedReadHalf>,
) -> Result<Option<String>> {
    // Read headers until we find an empty line
    let mut content_length: Option<usize> = None;

    loop {
        let mut header_line = String::new();
        let bytes_read = reader.read_line(&mut header_line).await?;

        if bytes_read == 0 {
            return Ok(None); // EOF
        }

        let trimmed = header_line.trim();

        if trimmed.is_empty() {
            // End of headers
            break;
        }

        if let Some(value) = trimmed.strip_prefix("Content-Length:") {
            content_length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .context("invalid Content-Length value")?,
            );
        }
        // Ignore other headers
    }

    let Some(length) = content_length else {
        return Ok(None); // No Content-Length found
    };

    // Read exactly `length` bytes for the body
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).await?;

    let message = String::from_utf8(body).context("invalid UTF-8 in MCP message body")?;
    Ok(Some(message))
}

/// Write an MCP message with Content-Length framing.
async fn write_mcp_message(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    body: &str,
) -> Result<()> {
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    writer.write_all(header.as_bytes()).await?;
    writer.write_all(body.as_bytes()).await?;
    writer.flush().await?;
    Ok(())
}

// ── Connection handler ──

async fn handle_connection(
    stream: tokio::net::UnixStream,
    store: SharedStore,
    notify: Option<NotifyFn>,
) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    while let Some(message) = read_mcp_message(&mut reader).await? {
        if message.trim().is_empty() {
            continue;
        }

        let request: JsonRpcRequest = match serde_json::from_str(&message) {
            Ok(req) => req,
            Err(e) => {
                tracing::warn!(
                    "Invalid JSON-RPC request: {} — raw: {}",
                    e,
                    &message[..message.len().min(200)]
                );
                continue;
            }
        };

        let response = handle_request(&request, &store, notify.as_ref()).await;

        if let Some(response) = response {
            let json = serde_json::to_string(&response)?;
            write_mcp_message(&mut writer, &json).await?;
        }
    }

    Ok(())
}

async fn handle_request(
    request: &JsonRpcRequest,
    store: &SharedStore,
    notify: Option<&NotifyFn>,
) -> Option<JsonRpcResponse> {
    let id = request.id.clone().unwrap_or(Value::Null);

    match request.method.as_str() {
        // MCP initialize handshake
        "initialize" => Some(JsonRpcResponse::success(
            id,
            serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {
                    "tools": {}
                },
                "serverInfo": {
                    "name": "claustre",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }),
        )),

        // Keepalive
        "ping" => Some(JsonRpcResponse::success(id, serde_json::json!({}))),

        // List available tools
        "tools/list" => {
            let tools = tool_definitions();
            Some(JsonRpcResponse::success(
                id,
                serde_json::json!({ "tools": tools }),
            ))
        }

        // Execute a tool
        "tools/call" => {
            let tool_name = request
                .params
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let args = request.params.get("arguments").cloned().unwrap_or_default();

            let result = handle_tool_call(tool_name, &args, store, notify).await;

            match result {
                Ok(content) => Some(JsonRpcResponse::success(
                    id,
                    serde_json::json!({
                        "content": [{
                            "type": "text",
                            "text": content
                        }]
                    }),
                )),
                Err(e) => Some(JsonRpcResponse::error(id, -32000, e.to_string())),
            }
        }

        // Notifications (no response needed per JSON-RPC spec — no id)
        "notifications/initialized" | "notifications/cancelled" | "notifications/progress" => None,

        _ => {
            // If no id, it's a notification — don't respond
            if request.id.is_none() {
                None
            } else {
                Some(JsonRpcResponse::error(
                    id,
                    -32601,
                    format!("Unknown method: {}", request.method),
                ))
            }
        }
    }
}

#[expect(
    clippy::similar_names,
    reason = "5h and 7d are distinct domain-specific window labels"
)]
async fn handle_tool_call(
    tool_name: &str,
    args: &Value,
    store: &SharedStore,
    notify: Option<&NotifyFn>,
) -> Result<String> {
    match tool_name {
        "claustre_status" => {
            let session_id = args
                .get("session_id")
                .and_then(|v| v.as_str())
                .context("missing session_id")?;
            let state = args
                .get("state")
                .and_then(|v| v.as_str())
                .context("missing state")?;
            let message = args
                .get("message")
                .and_then(|v| v.as_str())
                .context("missing message")?;

            let claude_status: ClaudeStatus = state.parse().unwrap_or(ClaudeStatus::Idle);
            let store = store.lock().await;
            store.update_session_status(session_id, claude_status, message)?;

            // Defensive fallback: if Claude calls claustre_status with "done"
            // instead of claustre_task_done, also transition the task to in_review.
            if claude_status == ClaudeStatus::Done {
                let tasks = {
                    let session = store.get_session(session_id)?;
                    store.list_tasks_for_project(&session.project_id)?
                };
                for task in &tasks {
                    if task.session_id.as_deref() == Some(session_id)
                        && task.status == TaskStatus::InProgress
                    {
                        tracing::warn!(
                            "claustre_status got 'done' — transitioning task '{}' to in_review \
                             (should use claustre_task_done instead)",
                            task.title,
                        );

                        // Handle subtask flow: mark current subtask done, feed next
                        let subtasks = store.list_subtasks_for_task(&task.id)?;
                        if !subtasks.is_empty() {
                            for st in &subtasks {
                                if st.status == TaskStatus::InProgress {
                                    store.update_subtask_status(&st.id, TaskStatus::Done)?;
                                    break;
                                }
                            }

                            if let Some(next_st) = store.next_pending_subtask(&task.id)? {
                                store.update_subtask_status(&next_st.id, TaskStatus::InProgress)?;
                                store.update_session_status(
                                    session_id,
                                    ClaudeStatus::Working,
                                    &format!("Starting: {}", next_st.title),
                                )?;
                                let prompt = format!("{}{AUTONOMOUS_SUFFIX}", next_st.description);
                                crate::session::feed_prompt_to_session(&store, session_id, &prompt)
                                    .unwrap_or_else(|e| {
                                        tracing::error!("Failed to feed next subtask: {e}");
                                    });
                                break;
                            }
                            // All subtasks done — fall through to mark task in_review
                        }

                        store.update_task_status(&task.id, TaskStatus::InReview)?;
                        if let Some(notify) = notify {
                            notify(&task.title);
                        }
                        break;
                    }
                }
            }

            Ok(format!("Status updated to '{state}': {message}"))
        }

        "claustre_task_done" => {
            let session_id = args
                .get("session_id")
                .and_then(|v| v.as_str())
                .context("missing session_id")?;
            let summary = args
                .get("summary")
                .and_then(|v| v.as_str())
                .context("missing summary")?;
            let pr_url = args.get("pr_url").and_then(|v| v.as_str());

            let store = store.lock().await;

            // Find the in-progress task for this session
            let mut task_title = String::new();
            let tasks = {
                let session = store.get_session(session_id)?;
                store.list_tasks_for_project(&session.project_id)?
            };

            let current_task = tasks.iter().find(|t| {
                t.session_id.as_deref() == Some(session_id) && t.status == TaskStatus::InProgress
            });

            let Some(current_task) = current_task else {
                store.update_session_status(session_id, ClaudeStatus::Done, summary)?;
                return Ok(format!(
                    "No in-progress task found for session. Summary: {summary}"
                ));
            };

            task_title.clone_from(&current_task.title);
            let task_id = current_task.id.clone();

            // Check if task has subtasks
            let subtasks = store.list_subtasks_for_task(&task_id)?;
            if subtasks.is_empty() {
                // No subtasks — existing behavior
                store.update_task_status(&task_id, TaskStatus::InReview)?;
                if let Some(url) = pr_url {
                    store.update_task_pr_url(&task_id, url)?;
                }
                store.update_session_status(session_id, ClaudeStatus::Done, summary)?;

                if let Some(notify) = notify {
                    notify(&task_title);
                }

                let auto_fed = crate::session::feed_next_task(&store, session_id).unwrap_or(false);
                if auto_fed {
                    Ok(format!(
                        "Task marked as in_review. Next autonomous task queued. Summary: {summary}"
                    ))
                } else {
                    Ok(format!(
                        "Task marked as in_review. No more queued tasks. Summary: {summary}"
                    ))
                }
            } else {
                // Mark current in-progress subtask as done
                for st in &subtasks {
                    if st.status == TaskStatus::InProgress {
                        store.update_subtask_status(&st.id, TaskStatus::Done)?;
                        break;
                    }
                }

                // Check for next pending subtask
                if let Some(next_st) = store.next_pending_subtask(&task_id)? {
                    // Feed next subtask — keep task in_progress
                    store.update_subtask_status(&next_st.id, TaskStatus::InProgress)?;
                    store.update_session_status(
                        session_id,
                        ClaudeStatus::Working,
                        &format!("Starting: {}", next_st.title),
                    )?;
                    let prompt = format!("{}{AUTONOMOUS_SUFFIX}", next_st.description);
                    crate::session::feed_prompt_to_session(&store, session_id, &prompt)
                        .unwrap_or_else(|e| {
                            tracing::error!("Failed to feed next subtask: {e}");
                        });
                    Ok(format!(
                        "Next subtask queued: '{}'. Summary: {summary}",
                        next_st.title
                    ))
                } else {
                    // All subtasks done — mark task in_review (existing flow)
                    store.update_task_status(&task_id, TaskStatus::InReview)?;
                    if let Some(url) = pr_url {
                        store.update_task_pr_url(&task_id, url)?;
                    }
                    store.update_session_status(session_id, ClaudeStatus::Done, summary)?;

                    if let Some(notify) = notify {
                        notify(&task_title);
                    }

                    let auto_fed =
                        crate::session::feed_next_task(&store, session_id).unwrap_or(false);
                    if auto_fed {
                        Ok(format!(
                            "All subtasks done. Task marked as in_review. Next autonomous task queued. Summary: {summary}"
                        ))
                    } else {
                        Ok(format!(
                            "All subtasks done. Task marked as in_review. No more queued tasks. Summary: {summary}"
                        ))
                    }
                }
            }
        }

        "claustre_usage" => {
            let session_id = args
                .get("session_id")
                .and_then(|v| v.as_str())
                .context("missing session_id")?;
            let input_tokens = args
                .get("input_tokens")
                .and_then(serde_json::Value::as_i64)
                .context("missing input_tokens")?;
            let output_tokens = args
                .get("output_tokens")
                .and_then(serde_json::Value::as_i64)
                .context("missing output_tokens")?;
            let cost = args
                .get("cost")
                .and_then(serde_json::Value::as_f64)
                .context("missing cost")?;

            let store = store.lock().await;

            // Find the current task for this session
            let tasks = {
                let session = store.get_session(session_id)?;
                store.list_tasks_for_project(&session.project_id)?
            };

            for task in &tasks {
                if task.session_id.as_deref() == Some(session_id)
                    && (task.status == TaskStatus::InProgress
                        || task.status == TaskStatus::InReview)
                {
                    store.update_task_usage(&task.id, input_tokens, output_tokens, cost)?;
                    break;
                }
            }

            Ok(format!(
                "Usage recorded: {input_tokens} in / {output_tokens} out / ${cost:.4}"
            ))
        }

        "claustre_log" => {
            let session_id = args
                .get("session_id")
                .and_then(|v| v.as_str())
                .context("missing session_id")?;
            let level = args.get("level").and_then(|v| v.as_str()).unwrap_or("info");
            let message = args
                .get("message")
                .and_then(|v| v.as_str())
                .context("missing message")?;

            match level {
                "warn" => tracing::warn!("[{}] {}", session_id, message),
                "error" => tracing::error!("[{}] {}", session_id, message),
                _ => tracing::info!("[{}] {}", session_id, message),
            }

            Ok(format!("Logged [{level}]: {message}"))
        }

        "claustre_rate_limited" => {
            let session_id = args
                .get("session_id")
                .and_then(|v| v.as_str())
                .context("missing session_id")?;
            let limit_type = args
                .get("limit_type")
                .and_then(|v| v.as_str())
                .context("missing limit_type")?;

            // Default reset_at to 30 minutes from now if not provided
            let reset_at = args.get("reset_at").and_then(|v| v.as_str()).map_or_else(
                || (chrono::Utc::now() + chrono::Duration::minutes(30)).to_rfc3339(),
                String::from,
            );

            let usage_5h_pct = args
                .get("usage_5h_pct")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(0.0);
            let usage_7d_pct = args
                .get("usage_7d_pct")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(0.0);

            let store = store.lock().await;
            store.set_rate_limited(limit_type, &reset_at, usage_5h_pct, usage_7d_pct)?;

            tracing::warn!(
                "Rate limited! type={limit_type}, reset_at={reset_at}, session={session_id}"
            );

            Ok(format!(
                "Rate limit recorded ({limit_type} window). Autonomous tasks paused until {reset_at}."
            ))
        }

        "claustre_usage_windows" => {
            let _session_id = args
                .get("session_id")
                .and_then(|v| v.as_str())
                .context("missing session_id")?;
            let usage_5h_pct = args
                .get("usage_5h_pct")
                .and_then(serde_json::Value::as_f64)
                .context("missing usage_5h_pct")?;
            let usage_7d_pct = args
                .get("usage_7d_pct")
                .and_then(serde_json::Value::as_f64)
                .context("missing usage_7d_pct")?;

            let store = store.lock().await;
            store.update_usage_windows(usage_5h_pct, usage_7d_pct)?;

            Ok(format!(
                "Usage windows updated: 5h={usage_5h_pct:.1}%, 7d={usage_7d_pct:.1}%"
            ))
        }

        _ => anyhow::bail!("Unknown tool: {tool_name}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Store, TaskMode, TaskStatus};

    fn make_tool_call(id: u64, tool_name: &str, args: &serde_json::Value) -> JsonRpcRequest {
        JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(Value::Number(id.into())),
            method: "tools/call".into(),
            params: serde_json::json!({
                "name": tool_name,
                "arguments": args
            }),
        }
    }

    #[tokio::test]
    async fn claustre_task_done_with_subtasks_feeds_next() {
        let store = Store::open_in_memory().unwrap();
        let project = store.create_project("proj", "/tmp/proj").unwrap();
        let session = store
            .create_session(&project.id, "b", "/tmp/wt", "tab")
            .unwrap();
        let task = store
            .create_task(&project.id, "parent", "", TaskMode::Autonomous)
            .unwrap();
        store.assign_task_to_session(&task.id, &session.id).unwrap();
        store
            .update_task_status(&task.id, TaskStatus::InProgress)
            .unwrap();

        let s1 = store.create_subtask(&task.id, "step 1", "first").unwrap();
        let s2 = store.create_subtask(&task.id, "step 2", "second").unwrap();
        store
            .update_subtask_status(&s1.id, TaskStatus::InProgress)
            .unwrap();

        let shared: SharedStore = Arc::new(Mutex::new(store));

        let req = make_tool_call(
            1,
            "claustre_task_done",
            &serde_json::json!({
                "session_id": session.id,
                "summary": "Step 1 done"
            }),
        );

        let resp = handle_request(&req, &shared, None).await.unwrap();
        assert!(resp.result.is_some());
        let text = resp.result.unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(text.contains("Next subtask"));

        let store = shared.lock().await;
        let st1 = store.get_subtask(&s1.id).unwrap();
        assert_eq!(st1.status, TaskStatus::Done);

        let st2 = store.get_subtask(&s2.id).unwrap();
        assert_eq!(st2.status, TaskStatus::InProgress);

        // Parent task should still be in_progress
        let t = store.get_task(&task.id).unwrap();
        assert_eq!(t.status, TaskStatus::InProgress);
    }

    #[tokio::test]
    async fn claustre_task_done_last_subtask_marks_task_in_review() {
        let store = Store::open_in_memory().unwrap();
        let project = store.create_project("proj", "/tmp/proj").unwrap();
        let session = store
            .create_session(&project.id, "b", "/tmp/wt", "tab")
            .unwrap();
        let task = store
            .create_task(&project.id, "parent", "", TaskMode::Autonomous)
            .unwrap();
        store.assign_task_to_session(&task.id, &session.id).unwrap();
        store
            .update_task_status(&task.id, TaskStatus::InProgress)
            .unwrap();

        let s1 = store
            .create_subtask(&task.id, "only step", "do it")
            .unwrap();
        store
            .update_subtask_status(&s1.id, TaskStatus::InProgress)
            .unwrap();

        let shared: SharedStore = Arc::new(Mutex::new(store));

        let req = make_tool_call(
            1,
            "claustre_task_done",
            &serde_json::json!({
                "session_id": session.id,
                "summary": "All done",
                "pr_url": "https://github.com/org/repo/pull/1"
            }),
        );

        let resp = handle_request(&req, &shared, None).await.unwrap();
        assert!(resp.result.is_some());

        let store = shared.lock().await;
        let st1 = store.get_subtask(&s1.id).unwrap();
        assert_eq!(st1.status, TaskStatus::Done);

        let t = store.get_task(&task.id).unwrap();
        assert_eq!(t.status, TaskStatus::InReview);
    }

    #[tokio::test]
    async fn claustre_task_done_no_subtasks_existing_behavior() {
        let store = Store::open_in_memory().unwrap();
        let project = store.create_project("proj", "/tmp/proj").unwrap();
        let session = store
            .create_session(&project.id, "b", "/tmp/wt", "tab")
            .unwrap();
        let task = store
            .create_task(&project.id, "simple", "just do it", TaskMode::Autonomous)
            .unwrap();
        store.assign_task_to_session(&task.id, &session.id).unwrap();
        store
            .update_task_status(&task.id, TaskStatus::InProgress)
            .unwrap();

        let shared: SharedStore = Arc::new(Mutex::new(store));

        let req = make_tool_call(
            1,
            "claustre_task_done",
            &serde_json::json!({
                "session_id": session.id,
                "summary": "Done"
            }),
        );

        let resp = handle_request(&req, &shared, None).await.unwrap();
        assert!(resp.result.is_some());

        let store = shared.lock().await;
        let t = store.get_task(&task.id).unwrap();
        assert_eq!(t.status, TaskStatus::InReview);
    }
}
