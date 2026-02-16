#!/bin/bash
# Shared helper for claustre hooks — sourced, not executed directly.
# Expects SESSION_ID to be set by the caller.

LOG="$HOME/.claustre/hook-debug.log"

# Read Claude's internal task progress and write to claustre tmp dir
sync_progress() {
    local TASK_DIR="$HOME/.claude/tasks/$SESSION_ID"
    local PROGRESS_DIR="$HOME/.claustre/tmp/$SESSION_ID"

    if [ -d "$TASK_DIR" ]; then
        mkdir -p "$PROGRESS_DIR"
        local PROGRESS="["
        local FIRST=true
        for f in "$TASK_DIR"/[0-9]*.json; do
            [ -f "$f" ] || continue
            local ITEM
            ITEM=$(jq -c '{subject: (.subject // ""), status: (.status // "pending")}' "$f" 2>/dev/null) || continue
            if $FIRST; then FIRST=false; else PROGRESS="$PROGRESS,"; fi
            PROGRESS="$PROGRESS$ITEM"
        done
        PROGRESS="$PROGRESS]"
        printf '%s' "$PROGRESS" > "$PROGRESS_DIR/progress.json"
    fi
}

# Extract cumulative token usage from Claude's JSONL conversation log.
# Sets USAGE_ARGS with --input-tokens / --output-tokens flags.
extract_usage() {
    USAGE_ARGS=""
    local PROJECT_HASH
    PROJECT_HASH=$(printf '%s' "$PWD" | sed 's/[^a-zA-Z0-9]/-/g')
    local PROJECT_DIR="$HOME/.claude/projects/$PROJECT_HASH"

    if [ -d "$PROJECT_DIR" ]; then
        local LATEST
        LATEST=$(ls -t "$PROJECT_DIR"/*.jsonl 2>/dev/null | head -1)
        if [ -n "$LATEST" ]; then
            local INPUT_T OUTPUT_T
            read -r INPUT_T OUTPUT_T < <(
                jq -r 'select(.type == "assistant") | .message.usage | [(.input_tokens // 0) + (.cache_creation_input_tokens // 0) + (.cache_read_input_tokens // 0), (.output_tokens // 0)] | @tsv' "$LATEST" 2>/dev/null \
                | awk 'BEGIN{sum_in=0; sum_out=0} {sum_in+=$1; sum_out+=$2} END{print sum_in, sum_out}'
            )
            if [ "${INPUT_T:-0}" -gt 0 ] || [ "${OUTPUT_T:-0}" -gt 0 ]; then
                USAGE_ARGS="--input-tokens $INPUT_T --output-tokens $OUTPUT_T"
            fi
        fi
    fi
}
