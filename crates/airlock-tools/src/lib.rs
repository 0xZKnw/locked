//! The agent's entire vocabulary.
//!
//! This crate is deliberately small and deliberately dependency-free. It defines
//! *what the model may ask for* — nothing here executes anything, and nothing
//! here can open a socket. Dispatch lives in `airlock-core`.
//!
//! The list below is the whole surface. There is no `web_fetch`, no `web_search`,
//! no general-purpose HTTP tool. If the agent wants a web page it goes through a
//! TAP credential that is allowed to fetch it, or it does not get the page.
//! Adding a tool here is a security decision, which is why they are enumerated in
//! one place rather than registered dynamically.

use serde::{Deserialize, Serialize};

/// What the model is shown for one tool.
#[derive(Serialize, Clone, Debug)]
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: serde_json::Value,
}

/// A parsed tool call. The loop matches on this; an unknown name never reaches
/// dispatch because parsing fails first.
#[derive(Deserialize, Clone, Debug)]
#[serde(tag = "name", content = "input", rename_all = "snake_case")]
pub enum ToolCall {
    /// List the credentials this run may use. Cheap, read-only.
    TapDiscover {},

    /// The only outbound channel in the process.
    ///
    /// Note what is *not* here: no header map, no arbitrary auth. The credential
    /// is named, never supplied; TAP injects the secret after policy. The agent
    /// cannot present a key it invented.
    TapCall {
        credential: String,
        target: String,
        method: String,
        #[serde(default)]
        body: Option<String>,
    },

    /// Check a write that is waiting on a human, without blocking.
    TapCheck { txn_id: String },

    /// Block until a pending write resolves. The agent chooses to wait; waiting
    /// is never imposed on it.
    TapAwait { txn_id: String },

    FsRead { path: String },
    FsWrite { path: String, contents: String },
    FsGlob { pattern: String },

    /// Shell inside the sandbox. The sandbox has no network stack, so a `curl`
    /// here fails at the syscall, not at a policy check.
    Exec { command: String },
}

#[derive(Serialize, Clone, Debug)]
pub struct ToolResult {
    /// The Messages API requires this discriminator on every content block.
    /// Omitting it does not fail the turn that produced the result — it fails
    /// the *next* request, with a bare "Invalid request Error", which is a
    /// thoroughly unpleasant thing to debug. Hence the constructor below: there
    /// is no way to build a `ToolResult` without it.
    #[serde(rename = "type")]
    block_type: &'static str,
    pub tool_use_id: String,
    pub content: String,
    pub is_error: bool,
}

impl ToolResult {
    pub fn new(tool_use_id: String, content: String, is_error: bool) -> Self {
        Self {
            block_type: "tool_result",
            tool_use_id,
            content,
            is_error,
        }
    }
}

fn schema(properties: serde_json::Value, required: &[&str]) -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

/// What this run is able to offer.
///
/// A run without a sandbox does not get a disabled `exec` that returns an error
/// — it gets **no `exec` at all**. Removing a capability removes the tool, so the
/// model is never told about something it cannot have, and a run's real surface
/// is always exactly what its receipts say it was.
///
/// Files and shell are separate flags because they need different guarantees.
/// Confining writes to one directory is something this process can do by itself;
/// running code the model wrote and denying it a network is not. So a machine
/// without a container runtime still gets the file tools, and only `exec` is
/// withheld — the surface shrinks to exactly what can be enforced.
#[derive(Clone, Copy, Debug)]
pub struct Capabilities {
    pub files: bool,
    pub shell: bool,
}

impl Capabilities {
    /// TAP only. A legitimate way to run: an agent whose entire power is calling
    /// credentials it does not hold.
    pub const TAP_ONLY: Self = Self { files: false, shell: false };
    /// A confined workspace, no code execution. No container needed, and no
    /// network question to answer — nothing runs.
    pub const FILES: Self = Self { files: true, shell: false };
    /// Everything, which requires a sandbox that can deny a network at the
    /// kernel.
    pub const FULL: Self = Self { files: true, shell: true };
}

/// The tool surface for a run, in the order the model sees it.
///
/// Order is stable because the tool list is part of the cached prompt prefix —
/// reordering it would invalidate the cache on every run.
pub fn tool_specs(caps: Capabilities) -> Vec<ToolSpec> {
    let mut specs = vec![
        ToolSpec {
            name: "tap_discover",
            description: "List the credentials this run is allowed to use, with the \
                          shape of each target and whether writes pause for a human.",
            input_schema: schema(serde_json::json!({}), &[]),
        },
        ToolSpec {
            name: "tap_call",
            description: "Call an external service through TAP. This is the only way to \
                          reach the network. Name the credential; never supply a key — \
                          TAP injects the secret after policy. Reads return immediately; \
                          writes return a txn_id and continue in the background.",
            input_schema: schema(
                serde_json::json!({
                    "credential": {"type": "string", "description": "Name from tap_discover."},
                    "target": {"type": "string", "description": "Full upstream URL, or a path for relative-target credentials."},
                    "method": {"type": "string", "enum": ["GET", "POST", "PUT", "PATCH", "DELETE"]},
                    "body": {"type": "string", "description": "Request body for writes."},
                }),
                &["credential", "target", "method"],
            ),
        },
        ToolSpec {
            name: "tap_check",
            description: "Check whether a pending write has been approved, without waiting.",
            input_schema: schema(
                serde_json::json!({"txn_id": {"type": "string"}}),
                &["txn_id"],
            ),
        },
        ToolSpec {
            name: "tap_await",
            description: "Wait for a pending write to resolve. Use only when you genuinely \
                          cannot make progress on anything else.",
            input_schema: schema(
                serde_json::json!({"txn_id": {"type": "string"}}),
                &["txn_id"],
            ),
        },
    ];

    if caps.files {
        specs.extend([
            ToolSpec {
                name: "fs_read",
                description: "Read a file from the run workspace.",
                input_schema: schema(
                    serde_json::json!({"path": {"type": "string"}}),
                    &["path"],
                ),
            },
            ToolSpec {
                name: "fs_write",
                description: "Write a file in the run workspace.",
                input_schema: schema(
                    serde_json::json!({
                        "path": {"type": "string"},
                        "contents": {"type": "string"},
                    }),
                    &["path", "contents"],
                ),
            },
            ToolSpec {
                name: "fs_glob",
                description: "List files in the run workspace matching a glob pattern.",
                input_schema: schema(
                    serde_json::json!({"pattern": {"type": "string"}}),
                    &["pattern"],
                ),
            },
        ]);
    }

    if caps.shell {
        specs.push(ToolSpec {
            name: "exec",
            description: "Run a shell command in the sandbox. The sandbox has no network \
                          access — anything that needs the network must go through tap_call.",
            input_schema: schema(
                serde_json::json!({"command": {"type": "string"}}),
                &["command"],
            ),
        });
    }

    specs
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Guards the property the whole design rests on: the only tool that can
    /// reach the network is `tap_call`. If someone adds `web_fetch`, this fails
    /// and they have to argue for it in review.
    #[test]
    fn tool_surface_is_the_documented_one() {
        let names: Vec<_> = tool_specs(Capabilities::FULL)
            .iter()
            .map(|t| t.name)
            .collect();
        assert_eq!(
            names,
            vec![
                "tap_discover",
                "tap_call",
                "tap_check",
                "tap_await",
                "fs_read",
                "fs_write",
                "fs_glob",
                "exec",
            ]
        );
    }

    /// Without a sandbox the tools are absent, not disabled — the model is never
    /// shown a capability this run cannot honour.
    #[test]
    fn tap_only_run_has_no_sandbox_tools() {
        let names: Vec<_> = tool_specs(Capabilities::TAP_ONLY)
            .iter()
            .map(|t| t.name)
            .collect();
        assert_eq!(
            names,
            vec!["tap_discover", "tap_call", "tap_check", "tap_await"]
        );
        assert!(!names.contains(&"exec"));
    }

    /// The middle tier is the one that lets the app run without a container
    /// runtime: files yes, code execution no. If `exec` ever leaks into it, the
    /// run is offering something no one is enforcing.
    #[test]
    fn files_tier_has_no_shell() {
        let names: Vec<_> = tool_specs(Capabilities::FILES).iter().map(|t| t.name).collect();
        assert!(names.contains(&"fs_write"));
        assert!(!names.contains(&"exec"));
    }

    #[test]
    fn tool_calls_parse_from_model_output() {
        let raw = serde_json::json!({
            "name": "tap_call",
            "input": {"credential": "dune", "target": "https://api.dune.com/x", "method": "GET"}
        });
        assert!(matches!(
            serde_json::from_value::<ToolCall>(raw).unwrap(),
            ToolCall::TapCall { .. }
        ));
    }

    #[test]
    fn unknown_tool_never_reaches_dispatch() {
        let raw = serde_json::json!({"name": "web_fetch", "input": {"url": "http://evil"}});
        assert!(serde_json::from_value::<ToolCall>(raw).is_err());
    }
}

#[cfg(test)]
mod result_shape {
    use super::*;

    /// The discriminator the Messages API needs. Its absence only surfaces on
    /// the following request, so it is worth pinning here.
    #[test]
    fn tool_result_carries_its_type() {
        let v = serde_json::to_value(ToolResult::new("toolu_1".into(), "ok".into(), false)).unwrap();
        assert_eq!(v["type"], "tool_result");
        assert_eq!(v["tool_use_id"], "toolu_1");
    }
}
