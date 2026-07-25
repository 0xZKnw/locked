//! The agent loop, and the traits the outside world must satisfy to be plugged
//! into it.
//!
//! Note the dependency direction: **core owns the transport traits, and
//! `airlock-egress` implements them.** If core depended on egress instead, it
//! would inherit `reqwest` transitively and the loop — the thing that decides
//! what happens — would sit in a crate that can open sockets. Inverting it keeps
//! every network-capable line in one crate plus forty lines of wiring in the
//! binary.
//!
//! Core also makes no assumption about display. It emits events into an
//! `EventSink`; a TUI, the Tauri window, or a test collector all consume the
//! same stream.

use airlock_journal::{Chain, Evidence, Event};

/// Re-exported so an implementer of [`LlmTransport`] only needs to know about
/// the crate that defines the trait.
pub use airlock_journal::Integrity;
use airlock_sandbox::Sandbox;
use airlock_tools::{Capabilities, ToolCall, ToolResult, tool_specs};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Transport traits — implemented in airlock-egress
// ---------------------------------------------------------------------------

#[derive(Serialize, Clone, Debug)]
pub struct InferenceRequest {
    pub system: String,
    pub messages: Vec<Message>,
    pub tools: Vec<serde_json::Value>,
    pub max_tokens: u32,
}

/// A conversation turn.
///
/// `System` is not the top-level system prompt: it is a mid-conversation
/// operator message appended to `messages`. That is how a resolved approval
/// re-enters the conversation — it carries operator authority a user-role
/// message could not, and unlike editing the top-level prompt it leaves the
/// cached prefix intact.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "role", rename_all = "lowercase")]
pub enum Message {
    User { content: serde_json::Value },
    Assistant { content: serde_json::Value },
    System { content: String },
}

/// An image attached to a user turn.
///
/// Held as base64 rather than a path because that is what the Messages API takes
/// and because the loop must not read files the user has not handed it — a path
/// would put the window in the business of granting filesystem access, which is
/// the sandbox's job and nobody else's.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Image {
    pub media_type: String,
    pub data: String,
}

#[derive(Deserialize, Clone, Debug)]
pub struct InferenceResponse {
    pub model: String,
    pub content: serde_json::Value,
    pub stop_reason: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[async_trait]
pub trait LlmTransport: Send + Sync {
    /// The sink receives text as it is generated. A transport that cannot stream
    /// simply never calls it and returns the finished response — the loop and the
    /// UI behave identically either way, they just see the text later.
    async fn infer(
        &self,
        req: InferenceRequest,
        sink: &dyn EventSink,
    ) -> Result<InferenceResponse, CoreError>;
    /// Recorded in the run's opening receipt, so a journal says which door the
    /// inferences went through.
    fn integrity(&self) -> Integrity;
}

#[derive(Clone, Debug)]
pub struct ForwardCall {
    pub credential: String,
    pub target: String,
    pub method: String,
    pub body: Option<String>,
}

#[derive(Clone, Debug)]
pub enum ForwardOutcome {
    /// Read, or a write whose credential auto-approves. No third party recorded
    /// it, so the receipt will say `HarnessAttested`.
    Immediate {
        status: u16,
        body: String,
        /// An identifier the upstream volunteered (Dune `execution_id`, ETag,
        /// block number). Upgrades the receipt from harness- to source-attested
        /// when present — see `Evidence`.
        source_id: Option<(String, String)>,
    },
    /// Write awaiting a human. The agent is told and moves on.
    Pending { txn_id: String },
}

#[derive(Clone, Debug)]
pub enum ApprovalState {
    Pending,
    Forwarded { status: u16, body: String },
    Denied,
    Failed { detail: String },
}

#[derive(Serialize, Clone, Debug)]
pub struct CredentialInfo {
    pub name: String,
    pub target_shape: String,
    pub writes_auto_approve: bool,
    pub description: String,
}

#[async_trait]
pub trait Forwarder: Send + Sync {
    async fn discover(&self) -> Result<Vec<CredentialInfo>, CoreError>;
    async fn forward(&self, call: ForwardCall) -> Result<ForwardOutcome, CoreError>;
    async fn poll(&self, txn_id: &str) -> Result<ApprovalState, CoreError>;
    /// Block until a decision arrives or the deadline passes.
    ///
    /// Lives on the trait rather than in the loop because waiting means sleeping,
    /// and a sleep needs a runtime — which core deliberately does not depend on.
    async fn await_decision(
        &self,
        txn_id: &str,
        timeout_secs: u64,
    ) -> Result<ApprovalState, CoreError>;
}

// ---------------------------------------------------------------------------
// The system prompt
//
// It lives here rather than in each binary because it is part of the loop's
// contract with the model, not a property of the window or the terminal. Two
// copies drifted apart once already.
// ---------------------------------------------------------------------------

pub mod prompt {
    use super::CredentialInfo;
    use airlock_sandbox::Isolation;

    pub const BASE: &str = "\
You are the agent inside Locked — a harness whose whole claim is that every \
action you take leaves a receipt someone else can check. Work as though that is \
true, because it is.

Never describe your own capabilities from memory. What this run can reach is a \
fact about this machine, not something you know: the inventory below is the \
truth, and tap_discover re-reads it. A confident list of things you have not \
checked is the exact failure this harness exists to prevent — and you are not a \
general assistant listing what language models can do, you are this agent \
reporting what this run can do.

Your only route to the network is tap_call: name a credential, never a key — the \
proxy holds the secret and injects it after policy. A service not in the \
inventory is a service you cannot reach, and saying so plainly is the correct \
answer, not a failure.

Writes pause for a human. When tap_call returns a pending txn_id, keep working on \
something else — you will be told when it resolves. Use tap_await only when you \
genuinely cannot make progress without the result.

Be exact about what backs a claim. A TAP-gated write was witnessed by a third \
party and a human; a plain read was witnessed by nothing but this journal. When \
you report a result, do not lend the second the confidence of the first, and say \
which one you have when it matters.

Answer in the language the user writes in. Say what you did, what came back, and \
what you concluded — no preamble about being an AI, no catalogue of things you \
were not asked about.";

    /// How the window draws a chart.
    ///
    /// Kept as a declarative spec rather than letting the model emit SVG: every
    /// other piece of model output in the transcript is escaped before it is
    /// rendered, and accepting markup here would undo that in the one place a
    /// reader would least expect it. The model describes the chart; our code
    /// draws it.
    pub const CHARTS: &str = "\n\nYou can draw. A fenced block tagged `chart` \
containing JSON is rendered as a real chart in the transcript — you do not emit \
SVG, HTML or images, only the spec:

```chart
{\"type\":\"column\",\"title\":\"Requests by day\",\"x\":[\"Mon\",\"Tue\",\"Wed\"],\
\"series\":[{\"name\":\"reads\",\"data\":[120,180,90]},{\"name\":\"writes\",\"data\":[8,14,5]}]}
```

`type` is one of line, area, column, bar, scatter, pie, donut. `x` holds the \
category labels; each series has `data` aligned to them. Add `\"stacked\":true` on \
column, bar or area when the parts sum to a meaningful whole. For scatter, give \
each series `points` as [x, y] pairs instead of `data`. For pie or donut, send one \
series and let `x` name the slices.

Pick the form from the question, not from habit: change over time is a line, \
comparison between named things is a column or a bar (bar when the labels are \
long), correlation is a scatter, and parts of one whole is a pie — only if there \
are few parts and they genuinely sum to something. Two measures on different \
scales are two charts, never two axes on one. Above eight series, group the tail \
into \"other\" rather than adding a ninth colour.

Draw when a shape carries the answer better than a sentence, and say in prose \
what the chart shows — a chart is never the whole answer, and a reader who \
cannot see it must still get the point. Do not chart three numbers you could \
simply state.";

    /// The live canvas, and the one thing it cannot do.
    pub const CANVAS: &str = "\n\nA fenced block tagged `html` runs. It is rendered \
in an isolated frame with its own strict policy: inline script and style work, and \
there is no network at all — fetch, XHR and WebSockets fail inside it, and it \
cannot reach this window or its data. So write self-contained pages: no CDN, no \
Google Fonts, no remote images. Data URLs and inline SVG are fine. Anything you \
need from outside, fetch through tap_call first and inline the result.

Use it when something is worth interacting with rather than described — a small \
tool, a visualisation with controls, a layout to look at. For a chart, prefer the \
`chart` block above; for code the reader will copy elsewhere, an ordinary fenced \
block is right. Any other fence tag (`rust`, `python`, `sql`, `xml`…) is shown as \
highlighted source and never executed, so tag a block `html` only when you mean \
for it to run.";

    pub const SANDBOX: &str = "\n\nYou have a shell and a filesystem: exec and the \
fs_* tools run inside a container started with no network stack at all — not a \
firewall rule, no interface exists. Code you write and run there cannot reach the \
network even if it tries, so anything needing the network goes through tap_call. \
The workspace persists across the messages of this chat, so a file you write now \
is still there later. Prefer doing real work in it — running the code, checking \
the output — over describing what the code would do.";

    pub const WORKSPACE: &str = "\n\nYou have a workspace on disk: fs_read, fs_write \
and fs_glob work inside it and nowhere else, and it persists across the messages \
of this chat. There is no shell on this run — running code you wrote needs a \
sandbox that can deny it a network, and this machine has none available, so the \
tool is absent rather than restricted. Do not offer to run anything. Anything \
outbound still goes through tap_call.";

    pub const NO_SANDBOX: &str = "\n\nThis run has no sandbox, so you have no shell \
and no filesystem. Work entirely through TAP and your own reasoning.";

    /// The credentials this run actually holds, written into the prompt.
    ///
    /// Asking the model to call `tap_discover` before it answers is advice it can
    /// ignore, and what that looks like is a fluent paragraph about "preconfigured
    /// identifiers", none of it checked. Handing it the real list costs one
    /// request at run start and removes the opportunity to guess.
    pub fn inventory(creds: &[CredentialInfo]) -> String {
        if creds.is_empty() {
            return "\n\nInventory: this run holds no credentials at all. tap_call \
will fail for every service — say so rather than attempting one."
                .to_string();
        }

        let lines: String = creds
            .iter()
            .map(|c| {
                let gate = if c.writes_auto_approve {
                    "reads and writes go straight through"
                } else {
                    "reads go through, writes pause for a human"
                };
                let desc = if c.description.trim().is_empty() {
                    String::new()
                } else {
                    format!(" — {}", c.description.trim())
                };
                format!("\n  {}{desc} ({gate})", c.name)
            })
            .collect();

        format!(
            "\n\nInventory — the credentials this run can reach, as TAP reports \
them right now:{lines}\n\nThat list is complete. Do not add to it from memory."
        )
    }

    /// The whole prompt for one run.
    ///
    /// The isolation tier comes from the sandbox itself, so the model is told
    /// what it actually has rather than what the caller meant to give it.
    pub fn system(isolation: &Isolation, creds: &[CredentialInfo]) -> String {
        let tier = match isolation {
            Isolation::Container { .. } => SANDBOX,
            Isolation::Workspace => WORKSPACE,
            Isolation::None => NO_SANDBOX,
        };
        format!("{BASE}{CHARTS}{CANVAS}{tier}{}", inventory(creds))
    }
}

// ---------------------------------------------------------------------------
// Display — core emits, someone else renders
// ---------------------------------------------------------------------------

#[derive(Serialize, Clone, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UiEvent {
    TurnStarted { turn: u32 },
    /// A fragment of the answer, as the model produces it.
    AssistantDelta { text: String },
    /// A fragment of the model's reasoning. Shown apart from the answer, because
    /// conflating the two would misrepresent what the agent actually concluded.
    ThinkingDelta { text: String },
    AssistantText { text: String },
    ToolStarted { name: String, summary: String },
    ToolFinished { name: String, is_error: bool },
    ApprovalPending { txn_id: String, summary: String },
    ApprovalResolved { txn_id: String, decision: String },
    ReceiptAppended { receipt: airlock_journal::Receipt },
    RunFinished { turns: u32 },
}

pub trait EventSink: Send + Sync {
    fn emit(&self, event: UiEvent);
}

/// Drops everything. Useful in tests and headless runs.
pub struct NullSink;
impl EventSink for NullSink {
    fn emit(&self, _: UiEvent) {}
}

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("transport: {0}")]
    Transport(String),
    #[error("journal: {0}")]
    Journal(#[from] airlock_journal::JournalError),
    #[error("sandbox: {0}")]
    Sandbox(#[from] airlock_sandbox::SandboxError),
    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),
}

// ---------------------------------------------------------------------------
// The loop
// ---------------------------------------------------------------------------

pub struct Run<'a> {
    llm: &'a dyn LlmTransport,
    tap: &'a dyn Forwarder,
    sandbox: &'a dyn Sandbox,
    chain: Chain,
    sink: &'a dyn EventSink,
    messages: Vec<Message>,
    pending: Vec<String>,
    turn: u32,
    caps: Capabilities,
    now: fn() -> String,
}

impl<'a> Run<'a> {
    pub fn new(
        llm: &'a dyn LlmTransport,
        tap: &'a dyn Forwarder,
        sandbox: &'a dyn Sandbox,
        chain: Chain,
        sink: &'a dyn EventSink,
        caps: Capabilities,
        now: fn() -> String,
    ) -> Self {
        Self {
            llm,
            tap,
            sandbox,
            chain,
            sink,
            messages: Vec::new(),
            pending: Vec::new(),
            turn: 0,
            caps,
            now,
        }
    }

    fn record(&mut self, event: Event, evidence: Evidence) -> Result<(), CoreError> {
        let receipt = self.chain.append(event, evidence, (self.now)())?.clone();
        self.sink.emit(UiEvent::ReceiptAppended { receipt });
        Ok(())
    }

    /// Open the run.
    ///
    /// Both isolation fields are read off the sandbox rather than passed in, so a
    /// caller cannot record a stronger boundary than the run actually had.
    pub async fn start(&mut self, task: &str) -> Result<(), CoreError> {
        self.start_with(task, &[]).await
    }

    /// Open the run with images attached to the first message.
    ///
    /// The images ride inside the prompt, so they are covered by the inference
    /// receipt's `prompt_digest` like everything else — the chain proves what the
    /// model was shown without the journal ever holding a picture.
    pub async fn start_with(&mut self, task: &str, images: &[Image]) -> Result<(), CoreError> {
        let isolation = self.sandbox.isolation();
        self.record(
            Event::RunStarted {
                integrity: self.llm.integrity(),
                tools: tool_specs(self.caps).iter().map(|t| t.name.to_string()).collect(),
                sandbox_image: match &isolation {
                    airlock_sandbox::Isolation::Container { image } => Some(image.clone()),
                    _ => None,
                },
                isolation: isolation.label(),
            },
            Evidence::HarnessAttested,
        )?;
        // A plain string when there is nothing but text, so the common case keeps
        // the exact shape it has always had and nothing downstream has to learn a
        // second form.
        self.messages.push(Message::User {
            content: if images.is_empty() {
                serde_json::json!(task)
            } else {
                let mut blocks: Vec<serde_json::Value> = images
                    .iter()
                    .map(|img| {
                        serde_json::json!({
                            "type": "image",
                            "source": {
                                "type": "base64",
                                "media_type": img.media_type,
                                "data": img.data,
                            }
                        })
                    })
                    .collect();
                // Text last: the images are what the question is about, so the
                // model reads them before it reads the ask.
                blocks.push(serde_json::json!({ "type": "text", "text": task }));
                serde_json::Value::Array(blocks)
            },
        });
        Ok(())
    }

    /// One turn: drain resolved approvals, infer, dispatch tools.
    /// Returns `false` when the model stops asking for tools.
    pub async fn step(&mut self, system: &str, max_tokens: u32) -> Result<bool, CoreError> {
        self.turn += 1;
        self.sink.emit(UiEvent::TurnStarted { turn: self.turn });
        self.drain_approvals().await?;

        let response = self
            .llm
            .infer(
                InferenceRequest {
                    system: system.to_string(),
                    messages: self.messages.clone(),
                    tools: tool_specs(self.caps)
                        .iter()
                        .map(|t| serde_json::to_value(t).expect("tool spec serializes"))
                        .collect(),
                    max_tokens,
                },
                self.sink,
            )
            .await?;

        self.record(
            Event::Inference {
                model: response.model.clone(),
                prompt_digest: airlock_journal::digest_bytes(
                    &serde_json::to_vec(&self.messages)?,
                ),
                response_digest: airlock_journal::digest_bytes(&serde_json::to_vec(
                    &response.content,
                )?),
                input_tokens: response.input_tokens,
                output_tokens: response.output_tokens,
            },
            // Nobody but us witnessed the inference. Since the LLM left TAP, this
            // is the honest label — and it is the same two-tier distinction the
            // reads already carry, not a special case.
            Evidence::HarnessAttested,
        )?;

        // Text reached the sink as deltas while it was being generated; emitting
        // the finished blocks here as well would print everything twice.
        self.messages.push(Message::Assistant {
            content: response.content.clone(),
        });

        if response.stop_reason != "tool_use" {
            self.record(Event::RunFinished { turns: self.turn }, Evidence::HarnessAttested)?;
            self.sink.emit(UiEvent::RunFinished { turns: self.turn });
            return Ok(false);
        }

        let mut results = Vec::new();
        for block in response.content.as_array().unwrap_or(&Vec::new()) {
            if block["type"] != "tool_use" {
                continue;
            }
            let id = block["id"].as_str().unwrap_or_default().to_string();
            let call: ToolCall = serde_json::from_value(block.clone())?;
            results.push(self.dispatch(id, call).await?);
        }
        self.messages.push(Message::User {
            content: serde_json::to_value(&results)?,
        });
        Ok(true)
    }

    /// Poll every outstanding approval once, and re-enter the resolved ones as
    /// operator messages. No background task, no channel — the agent simply
    /// learns about them at the top of its next turn.
    async fn drain_approvals(&mut self) -> Result<(), CoreError> {
        let mut still_pending = Vec::new();
        for txn in std::mem::take(&mut self.pending) {
            let state = match self.tap.poll(&txn).await {
                Ok(state) => state,
                // A proxy that cannot be reached has not decided anything. Keep
                // the transaction open and ask again next turn — ending the run
                // over a failed status check would throw away work for a reason
                // that has nothing to do with the work.
                Err(_) => {
                    still_pending.push(txn);
                    continue;
                }
            };
            match state {
                ApprovalState::Pending => still_pending.push(txn),
                state => {
                    let (decision, detail) = match &state {
                        ApprovalState::Forwarded { status, .. } => {
                            ("approved".to_string(), format!("upstream {status}"))
                        }
                        ApprovalState::Denied => {
                            ("denied".to_string(), "the human refused".to_string())
                        }
                        ApprovalState::Failed { detail } => ("error".to_string(), detail.clone()),
                        ApprovalState::Pending => unreachable!(),
                    };
                    self.record(
                        Event::ApprovalResolved {
                            txn_id: txn.clone(),
                            decision: decision.clone(),
                        },
                        // A human approved through TAP: this is the one kind of
                        // receipt a third party can corroborate.
                        Evidence::TapAttested {
                            txn_id: txn.clone(),
                        },
                    )?;
                    self.sink.emit(UiEvent::ApprovalResolved {
                        txn_id: txn.clone(),
                        decision: decision.clone(),
                    });
                    self.messages.push(Message::System {
                        content: format!("Transaction {txn} resolved: {decision} ({detail})."),
                    });
                }
            }
        }
        self.pending = still_pending;
        Ok(())
    }

    async fn dispatch(&mut self, id: String, call: ToolCall) -> Result<ToolResult, CoreError> {
        let name = tool_name(&call);
        self.sink.emit(UiEvent::ToolStarted {
            name: name.clone(),
            summary: summarize(&call),
        });

        // A tool that fails must come back to the model as a failed tool result,
        // never as an error out of `dispatch` — `?` here would end the whole run
        // on a bad path or a refused credential, when the agent's job is to read
        // the error and try something else.
        //
        // The one exception is `record`: if the journal cannot be written, the
        // run must stop. Continuing would mean taking actions nothing recorded,
        // which is the only failure this design genuinely cannot tolerate. So
        // `?` appears below on `self.record(..)` and nowhere else.
        let result: Result<String, CoreError> = match call {
            ToolCall::TapDiscover {} => match self.tap.discover().await {
                Ok(creds) => Ok(serde_json::to_string_pretty(&creds)?),
                Err(e) => Err(e),
            },

            ToolCall::TapCall {
                credential,
                target,
                method,
                body,
            } => {
                let host = host_of(&target);
                let outcome = self
                    .tap
                    .forward(ForwardCall {
                        credential: credential.clone(),
                        target,
                        method: method.clone(),
                        body,
                    })
                    .await;
                match outcome {
                    Err(e) => Err(e),
                    Ok(ForwardOutcome::Immediate {
                        status,
                        body,
                        source_id,
                    }) => {
                        let evidence = match source_id {
                            Some((scheme, id)) => Evidence::SourceAttested { scheme, id },
                            None => Evidence::HarnessAttested,
                        };
                        self.record(
                            Event::TapCall {
                                credential,
                                target_host: host,
                                method,
                                upstream_status: Some(status),
                            },
                            evidence,
                        )?;
                        Ok(body)
                    }
                    Ok(ForwardOutcome::Pending { txn_id }) => {
                        self.record(
                            Event::TapCall {
                                credential,
                                target_host: host,
                                method,
                                upstream_status: None,
                            },
                            Evidence::TapAttested {
                                txn_id: txn_id.clone(),
                            },
                        )?;
                        self.pending.push(txn_id.clone());
                        self.sink.emit(UiEvent::ApprovalPending {
                            txn_id: txn_id.clone(),
                            summary: name.clone(),
                        });
                        Ok(format!(
                            "{{\"status\":\"pending\",\"txn_id\":\"{txn_id}\"}} — a human is \
                             deciding. Continue with other work; you will be told when it \
                             resolves. Use tap_await only if you cannot proceed without it."
                        ))
                    }
                }
            }

            ToolCall::TapCheck { txn_id } => {
                self.tap.poll(&txn_id).await.map(|s| format!("{s:?}"))
            }

            ToolCall::TapAwait { txn_id } => {
                // Bounded. An agent that blocks forever on a human who has gone
                // to lunch is worse than one that reports the wait and moves on.
                self.tap
                    .await_decision(&txn_id, 600)
                    .await
                    .map(|s| format!("{s:?}"))
            }

            ToolCall::FsRead { path } => self.sandbox.read(&path).await.map_err(Into::into),

            ToolCall::FsWrite { path, contents } => {
                match self.sandbox.write(&path, &contents).await {
                    Ok(()) => {
                        self.record(
                            Event::SandboxCall {
                                tool: "fs_write".into(),
                                args_digest: airlock_journal::digest_bytes(path.as_bytes()),
                                result_digest: airlock_journal::digest_bytes(contents.as_bytes()),
                                exit_code: None,
                            },
                            Evidence::HarnessAttested,
                        )?;
                        Ok(format!("wrote {path}"))
                    }
                    // Nothing was written, so nothing is recorded. A receipt for
                    // a write that did not happen would be worse than no receipt.
                    Err(e) => Err(e.into()),
                }
            }

            ToolCall::FsGlob { pattern } => self
                .sandbox
                .glob(&pattern)
                .await
                .map(|hits| hits.join("\n"))
                .map_err(Into::into),

            ToolCall::Exec { command } => match self.sandbox.exec(&command).await {
                Ok(out) => {
                    let combined = format!("{}{}", out.stdout, out.stderr);
                    self.record(
                        Event::SandboxCall {
                            tool: "exec".into(),
                            args_digest: airlock_journal::digest_bytes(command.as_bytes()),
                            result_digest: airlock_journal::digest_bytes(combined.as_bytes()),
                            exit_code: out.exit_code,
                        },
                        Evidence::HarnessAttested,
                    )?;
                    Ok(combined)
                }
                Err(e) => Err(e.into()),
            },
        };

        let (content, is_error) = match result {
            Ok(content) => (content, false),
            Err(e) => (format!("error: {e}"), true),
        };
        self.sink.emit(UiEvent::ToolFinished {
            name,
            is_error,
        });
        Ok(ToolResult::new(id, content, is_error))
    }

    pub fn chain(&self) -> &Chain {
        &self.chain
    }

    /// Continue an existing conversation.
    ///
    /// The loop keeps no state between processes, so a resumed chat is simply a
    /// run handed the messages that came before it. Call before [`start`].
    pub fn resume(&mut self, messages: Vec<Message>) {
        self.messages = messages;
    }

    /// The conversation so far, for a caller that wants to persist it.
    ///
    /// Note this is *not* what the chain records: receipts carry digests of the
    /// prompt and response, never the text. Anyone storing this is keeping a
    /// transcript beside the audit trail, not part of it.
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }
}

fn tool_name(call: &ToolCall) -> String {
    match call {
        ToolCall::TapDiscover { .. } => "tap_discover",
        ToolCall::TapCall { .. } => "tap_call",
        ToolCall::TapCheck { .. } => "tap_check",
        ToolCall::TapAwait { .. } => "tap_await",
        ToolCall::FsRead { .. } => "fs_read",
        ToolCall::FsWrite { .. } => "fs_write",
        ToolCall::FsGlob { .. } => "fs_glob",
        ToolCall::Exec { .. } => "exec",
    }
    .to_string()
}

fn summarize(call: &ToolCall) -> String {
    match call {
        ToolCall::TapCall {
            credential, method, target, ..
        } => format!("{method} {} via {credential}", host_of(target)),
        ToolCall::Exec { command } => command.chars().take(60).collect(),
        other => tool_name(other),
    }
}

/// Host without the scheme or path. Query strings can carry things we have no
/// business writing into a durable receipt.
fn host_of(target: &str) -> String {
    target
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(target)
        .split('/')
        .next()
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipts_never_carry_the_query_string() {
        assert_eq!(
            host_of("https://api.dune.com/v1/query?api_key=leak"),
            "api.dune.com"
        );
        assert_eq!(host_of("/relative/path"), "");
    }
}
