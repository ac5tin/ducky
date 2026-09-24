//! Built-in native tools: filesystem (sandboxed to the working directory),
//! web search (keyless DuckDuckGo) and web fetch. They are presented to the
//! model exactly like MCP tools and ride the same approval pipeline; the
//! tool-rule key prefix is `builtin/`.

pub mod fs;
pub mod html;
pub mod session_context;
pub mod web;

use std::path::Path;
use std::sync::LazyLock;

use serde_json::Value;

use crate::providers::ToolDef;

pub const SERVER_ID: &str = "builtin";
pub const SERVER_TITLE: &str = "Ducky";

/// The subagent tool. Registered here so the model sees it like any other
/// tool, but executed by the agent itself (`agent.rs`) because it needs the
/// chat engine, not just a cwd.
pub const SUBAGENT: &str = "ducky__subagent";

/// Present a plan for approval. Offered only in plan mode, and executed by the
/// agent loop like `ducky__subagent` — it needs the bridge, not a cwd.
pub const PRESENT_PLAN: &str = "ducky__present_plan";

/// Let the model switch the conversation's mode. Offered only in auto mode.
pub const SET_MODE: &str = "ducky__set_mode";

/// The definition an omitted `agent` argument resolves to (when it exists in
/// the user's config; otherwise the spawn stays base-generic).
pub const DEFAULT_SUBAGENT_NAME: &str = "General-Purpose";

pub struct BuiltinTool {
    pub name: &'static str,
    pub description: &'static str,
    pub schema: Value,
    pub read_only: bool,
}

pub fn tools() -> &'static [BuiltinTool] {
    &REGISTRY
}

pub fn is_builtin(name: &str) -> bool {
    REGISTRY.iter().any(|t| t.name == name)
}

pub fn lookup(name: &str) -> Option<&'static BuiltinTool> {
    REGISTRY.iter().find(|t| t.name == name)
}

/// The control tools: chat flow rather than tool calls. They are main-scope
/// only and bypass the approval gate, so the engine intercepts them before
/// `execute`.
pub fn is_control(name: &str) -> bool {
    name == PRESENT_PLAN || name == SET_MODE
}

pub fn tool_defs() -> Vec<ToolDef> {
    REGISTRY
        .iter()
        .map(|t| ToolDef {
            name: t.name.to_string(),
            description: t.description.to_string(),
            parameters: t.schema.clone(),
        })
        .collect()
}

/// The `ducky__subagent` tool definition built from the configured subagent
/// definitions: the static entry's schema plus an `agent` enum whose
/// description carries each type's name and when-to-use hint. Used instead of
/// the registry entry whenever at least one subagent is configured.
pub fn subagent_tool_def(defs: &[crate::config::SubagentConfig]) -> ToolDef {
    let registry_entry = lookup(SUBAGENT).expect("subagent tool is registered");
    let mut schema = registry_entry.schema.clone();
    let list = defs
        .iter()
        .map(|d| format!("- {}: {}", d.name, d.description))
        .collect::<Vec<_>>()
        .join("\n");
    let omit_hint = if defs
        .iter()
        .any(|d| d.name.trim().eq_ignore_ascii_case(DEFAULT_SUBAGENT_NAME))
    {
        format!("Omitting it uses {DEFAULT_SUBAGENT_NAME}.")
    } else {
        "Omit it for a generic subagent with every tool.".to_string()
    };
    schema["properties"]["agent"] = serde_json::json!({
        "type": "string",
        "enum": defs.iter().map(|d| d.name.clone()).collect::<Vec<_>>(),
        "description": format!(
            "Kind of subagent to run, picked by matching the task against each \
             type's description. {omit_hint} Available types:\n{list}"
        )
    });
    ToolDef {
        name: SUBAGENT.to_string(),
        description: "Spawn a subagent: an autonomous helper with a fresh context \
                      that works on one self-contained task and returns its final \
                      answer. Set `agent` to the type whose description fits the \
                      task, or omit it for a generic subagent that has the same \
                      tools you have. Subagents can spawn their own subagents \
                      (up to 3 levels deep), though some types are restricted to \
                      specific tools. The subagent cannot see this conversation, \
                      so `task` must contain everything it needs. Give each spawn \
                      a short `name` and a one-line `description` so the user can \
                      see at a glance what is running. To run subagents in \
                      parallel, make several ducky__subagent calls in the same \
                      message."
            .to_string(),
        parameters: schema,
    }
}

/// Run a builtin tool. `cwd` is the effective working directory (fs tools are
/// confined to it); the result text goes to the model and the tool card.
pub async fn execute(name: &str, args: &Value, cwd: &Path) -> Result<String, String> {
    if name.starts_with("ducky__fs_") {
        fs::execute(name, args, cwd).await
    } else if name.starts_with("ducky__web_") {
        web::execute(name, args).await
    } else if is_control(name) {
        // reached only if the engine's control branch is ever bypassed
        Err(format!("{name} is handled by the chat engine"))
    } else {
        Err(format!("Unknown builtin tool: {name}"))
    }
}

fn obj(required: &[&str], properties: Value) -> Value {
    let mut schema = serde_json::json!({
        "type": "object",
        "properties": properties,
    });
    if !required.is_empty() {
        schema["required"] = serde_json::json!(required);
    }
    schema
}

static REGISTRY: LazyLock<Vec<BuiltinTool>> = LazyLock::new(|| {
    vec![
        BuiltinTool {
            name: fs::LIST,
            description: "List the files and directories in a folder inside the working \
                      directory. Omit `path` to list the working directory itself.",
            schema: obj(
                &[],
                serde_json::json!({
                    "path": {
                        "type": "string",
                        "description": "Folder to list, relative to the working directory \
                                        (absolute paths must stay inside it). Defaults to \".\"."
                    }
                }),
            ),
            read_only: true,
        },
        BuiltinTool {
            name: fs::READ,
            description: "Read a text file inside the working directory and return its \
                      contents (truncated at 200 KB; binary files are rejected).",
            schema: obj(
                &["path"],
                serde_json::json!({
                    "path": {
                        "type": "string",
                        "description": "File to read, relative to the working directory \
                                        (absolute paths must stay inside it)."
                    }
                }),
            ),
            read_only: true,
        },
        BuiltinTool {
            name: fs::SEARCH,
            description: "Search recursively for file or directory names containing \
                      `pattern` (case-insensitive) under a folder in the working \
                      directory. Skips dependency and VCS folders like node_modules \
                      and .git; caps at 100 matches.",
            schema: obj(
                &["pattern"],
                serde_json::json!({
                    "pattern": { "type": "string", "description": "Case-insensitive substring of the name to find." },
                    "path": {
                        "type": "string",
                        "description": "Folder to search under. Defaults to the working directory."
                    }
                }),
            ),
            read_only: true,
        },
        BuiltinTool {
            name: fs::WRITE,
            description: "Write text to a file inside the working directory, creating \
                      parent folders as needed and overwriting existing content.",
            schema: obj(
                &["path", "content"],
                serde_json::json!({
                    "path": { "type": "string", "description": "File to write, relative to the working directory." },
                    "content": { "type": "string", "description": "Full file contents to write." }
                }),
            ),
            read_only: false,
        },
        BuiltinTool {
            name: fs::MKDIR,
            description: "Create a directory (and any missing parents) inside the \
                      working directory.",
            schema: obj(
                &["path"],
                serde_json::json!({
                    "path": { "type": "string", "description": "Directory to create, relative to the working directory." }
                }),
            ),
            read_only: false,
        },
        BuiltinTool {
            name: web::FETCH,
            description: "Fetch an http(s) URL and return the page body as readable \
                      plain text (HTML is stripped; truncated at 40 000 characters \
                      by default). Use this to read a specific page.",
            schema: obj(
                &["url"],
                serde_json::json!({
                    "url": { "type": "string", "description": "Absolute http:// or https:// URL." },
                    "max_chars": {
                        "type": "integer",
                        "description": "Maximum characters to return (1000–200000). Defaults to 40000."
                    }
                }),
            ),
            read_only: true,
        },
        BuiltinTool {
            name: SUBAGENT,
            description: "Spawn a subagent: an autonomous helper with a fresh context \
                      that works on one self-contained task and returns its final answer. \
                      The subagent has the same tools you have and can spawn its own \
                      subagents (up to 3 levels deep). Use it for work that deserves an \
                      isolated context — parallel research across files or pages, or \
                      independent subtasks you combine afterwards. The subagent cannot \
                      see this conversation, so `task` must contain everything it needs. \
                      Give each spawn a short `name` and a one-line `description` so the \
                      user can see at a glance what is running. To run subagents in \
                      parallel, make several ducky__subagent calls in the same message.",
            schema: obj(
                &["task"],
                serde_json::json!({
                    "task": {
                        "type": "string",
                        "description": "Complete, self-contained instructions for the subagent, \
                                        including any context it needs from this conversation."
                    },
                    "name": {
                        "type": "string",
                        "description": "Short label for this subagent shown to the user while it \
                                        runs, e.g. \"repo-explorer\" or \"doc-writer\"."
                    },
                    "description": {
                        "type": "string",
                        "description": "One-line summary of the delegated task shown to the user, \
                                        e.g. \"find all callers of run_turn\"."
                    }
                }),
            ),
            read_only: false,
        },
        BuiltinTool {
            name: PRESENT_PLAN,
            description: "Present your finished implementation plan to the user for \
                      approval. Use this in plan mode when the plan is ready: the user \
                      reads it and either approves it or asks for changes. Approval ends \
                      plan mode and you continue in default mode with this plan still in \
                      context, so start implementing it then. Never ask for approval in \
                      plain text — this tool is the request. If the user asks for changes, \
                      revise the plan and call this tool again.",
            schema: obj(
                &["plan"],
                serde_json::json!({
                    "plan": {
                        "type": "string",
                        "description": "The complete implementation plan in markdown: what \
                                        changes, in which files, in what order, and how to \
                                        verify it. Include the reasoning the user needs to \
                                        judge it."
                    }
                }),
            ),
            read_only: true,
        },
        BuiltinTool {
            name: SET_MODE,
            description: "Switch how much this conversation lets you change. Auto mode \
                      only. Use `plan` before starting a task with design decisions, \
                      several files to touch, or unclear requirements: you then work \
                      read-only, research, and present a plan for approval. Use \
                      `readonly` when you will only investigate and report. Use `default` \
                      for unrestricted work — that is refused while a plan is waiting for \
                      approval, because plan mode only ends when the user approves a plan \
                      or switches the mode themselves.",
            schema: obj(
                &["mode", "reason"],
                serde_json::json!({
                    "mode": {
                        "type": "string",
                        "enum": ["readonly", "plan", "default"],
                        "description": "The mode to switch to."
                    },
                    "reason": {
                        "type": "string",
                        "description": "One short line for the user explaining why this mode \
                                        fits the task."
                    }
                }),
            ),
            read_only: true,
        },
        BuiltinTool {
            name: web::SEARCH,
            description: "Search the web (DuckDuckGo) and return the top results with \
                      titles, URLs and snippets. Use this to find pages before \
                      fetching them.",
            schema: obj(
                &["query"],
                serde_json::json!({
                    "query": { "type": "string", "description": "What to search for." },
                    "max_results": {
                        "type": "integer",
                        "description": "Number of results to return (1–20). Defaults to 8."
                    }
                }),
            ),
            read_only: true,
        },
    ]
});

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_well_formed() {
        assert!(REGISTRY.len() >= 8);
        for tool in tools() {
            assert!(
                tool.name.starts_with("ducky__"),
                "{}: bad prefix",
                tool.name
            );
            assert!(
                !tool.description.is_empty(),
                "{}: missing description",
                tool.name
            );
            assert_eq!(
                tool.schema["type"], "object",
                "{}: schema must be an object",
                tool.name
            );
            for required in tool.schema["required"].as_array().unwrap_or(&vec![]) {
                let key = required.as_str().unwrap();
                assert!(
                    tool.schema["properties"].get(key).is_some(),
                    "{}: required key {key} missing from properties",
                    tool.name
                );
            }
        }
    }

    #[test]
    fn control_tools_are_registered_and_identified() {
        assert!(is_control(PRESENT_PLAN));
        assert!(is_control(SET_MODE));
        assert!(!is_control(SUBAGENT));
        assert!(!is_control("ducky__fs_write"));

        let plan = lookup(PRESENT_PLAN).expect("plan tool is registered");
        assert!(plan.read_only, "control tools must survive read-only modes");
        assert_eq!(plan.schema["required"], serde_json::json!(["plan"]));

        let set = lookup(SET_MODE).expect("mode tool is registered");
        assert!(set.read_only);
        assert_eq!(set.schema["required"], serde_json::json!(["mode", "reason"]));
        assert_eq!(
            set.schema["properties"]["mode"]["enum"],
            serde_json::json!(["readonly", "plan", "default"])
        );

        // control tools are executed by the engine, never by builtin::execute
        assert!(is_builtin(PRESENT_PLAN) && is_builtin(SET_MODE));
    }

    #[test]
    fn lookup_and_dispatch_guard() {
        assert!(is_builtin("ducky__fs_read"));
        assert!(!is_builtin("filesystem__read_file"));
        assert!(lookup("ducky__web_search").is_some());
        assert!(lookup("ducky__nope").is_none());
    }

    #[tokio::test]
    async fn unknown_tool_errors_cleanly() {
        let err = execute("ducky__fs_nope", &serde_json::json!({}), Path::new("/tmp"))
            .await
            .unwrap_err();
        assert!(err.contains("Unknown builtin"));

        // control tools are engine-handled, never dispatched here
        let err = execute(PRESENT_PLAN, &serde_json::json!({}), Path::new("/tmp"))
            .await
            .unwrap_err();
        assert!(err.contains("handled by the chat engine"));
    }
}
