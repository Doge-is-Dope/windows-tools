//! The model-facing MCP tools: wire names, descriptions and argument schemas. The lease calls
//! are not tools; a connector makes them on its own route.
use rmcp::model::Tool;
use serde_json::{Value, json};
use std::sync::LazyLock;

pub const DESCRIBE: &str = "windows_describe";
pub const EXEC_RUN: &str = "windows_exec_run";

/// In this order, which keeps `tools/list` deterministic.
pub static TOOLS: LazyLock<Vec<Tool>> = LazyLock::new(|| {
    vec![
        tool(
            DESCRIBE,
            true,
            "Read the Windows session's identity, who holds it and what the host can do. This grants nothing.",
            json!({}),
            &[],
        ),
        tool(
            EXEC_RUN,
            false,
            "Run a PowerShell command in the Windows session and wait for it: returns how it ended, its exit code and the end of what it printed. It starts in the user's profile directory and is stopped at the deadline. A program it starts with Start-Process keeps running once the command ends on its own, but not when it is stopped; that program's output is not returned. Nothing is replayed: after a lost reply, check what it did before running it again.",
            json!({
                "command": {"type": "string", "minLength": 1, "maxLength": crate::jobs::MAX_COMMAND},
                "timeoutMs": {
                    "type": "integer",
                    "minimum": 1000,
                    "maximum": 600000,
                    "default": crate::jobs::DEFAULT_TIMEOUT_MS
                }
            }),
            &["command"],
        ),
    ]
});

fn tool(
    name: &str,
    read_only: bool,
    description: &str,
    properties: Value,
    required: &[&str],
) -> Tool {
    let definition = json!({
        "name": name,
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false
        },
        "outputSchema": envelope(),
        "annotations": {
            "readOnlyHint": read_only,
            "destructiveHint": !read_only,
            "idempotentHint": false,
            "openWorldHint": true
        }
    });
    serde_json::from_value(definition).expect("Static MCP schema")
}

/// A relayed tool as listed, returning the same envelope as the host's own.
pub fn relayed(listed: &Value) -> Option<Tool> {
    let mut definition = listed.clone();
    definition["outputSchema"] = envelope();
    serde_json::from_value(definition).ok()
}

/// Every tool returns the native response envelope; failures also set `isError`.
fn envelope() -> Value {
    json!({
        "type": "object",
        "properties": {
            "id": {"type": "string"},
            "result": {"description": "Method result; null when error is set."},
            "error": {
                "anyOf": [
                    {"type": "null"},
                    {
                        "type": "object",
                        "properties": {
                            "code": {"type": "string"},
                            "message": {"type": "string"},
                            "delivery": {"type": "string", "enum": ["not_sent", "unknown"]}
                        },
                        "required": ["code", "message", "delivery"]
                    }
                ]
            }
        },
        "required": ["id", "result", "error"]
    })
}
