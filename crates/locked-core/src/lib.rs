//! The agent loop, and the traits the outside world must satisfy to be plugged
//! into it.
//!
//! Note the dependency direction: **core owns the transport traits, and
//! `locked-egress` implements them.** If core depended on egress instead, it
//! would inherit `reqwest` transitively and the loop — the thing that decides
//! what happens — would sit in a crate that can open sockets. Inverting it keeps
//! every network-capable line in one crate plus forty lines of wiring in the
//! binary.
//!
//! Core also makes no assumption about display. It emits events into an
//! `EventSink`; a TUI, the Tauri window, or a test collector all consume the
//! same stream.

use locked_journal::{Chain, Evidence, Event};

/// Re-exported so an implementer of [`LlmTransport`] only needs to know about
/// the crate that defines the trait.
pub use locked_journal::Integrity;
use locked_sandbox::Sandbox;
use locked_tools::{Capabilities, ToolCall, ToolResult, tool_specs};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Transport traits — implemented in locked-egress
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
    /// Everything the model read this turn, cached or not.
    ///
    /// Not the same as the provider's `input_tokens`, which counts only what was
    /// billed at full rate once a cached prefix is subtracted. The context gauge
    /// asks "how full is the window", and the window does not care what a token
    /// cost — so the cached prefix is added back in here, and what it saved is
    /// reported separately.
    pub input_tokens: u64,
    /// The part of `input_tokens` that came back from the provider's cache.
    #[serde(default)]
    pub cached_tokens: u64,
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
    use locked_sandbox::Isolation;

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
    ReceiptAppended { receipt: locked_journal::Receipt },
    /// The conversation was shortened to keep fitting. Surfaced rather than done
    /// quietly: the user is entitled to know when the agent stopped being able to
    /// see what they said earlier.
    Compacted { dropped: u32 },
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
    Journal(#[from] locked_journal::JournalError),
    #[error("sandbox: {0}")]
    Sandbox(#[from] locked_sandbox::SandboxError),
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
    /// The model's context window, or 0 when nobody published one.
    ///
    /// Zero disables compaction rather than guessing a limit: shortening a
    /// conversation against an invented number would throw away real context to
    /// respect a constraint that may not exist.
    context_window: u64,
    /// What the provider said the last request cost to read. Measured, not
    /// estimated — the loop does not tokenise, and a character-count heuristic
    /// would be wrong by a different factor for prose, code and base64 images.
    last_input_tokens: u64,
    now: fn() -> String,
}

const SUMMARY_PROMPT: &str = "You are compacting a conversation between a user \
and an agent so that it fits in a smaller context. Write a summary that lets the \
agent carry on without the original. Keep: what the user asked for and why, \
decisions taken and rejected, facts established, file paths, names, identifiers \
and numbers, and anything still outstanding. Drop pleasantries and restatement. \
Prefer specifics over characterisation. Do not invent anything, and do not \
address the user - you are writing a note to the agent.";

/// Characters of transcript sent to be summarised.
const MAX_TRANSCRIPT: usize = 120_000;

/// Where the conversation can be cut without breaking it.
///
/// A tool result refers back to a tool call, and a request carrying one without
/// the other is malformed — so the cut moves earlier until it lands somewhere
/// that leaves every pair intact. Moving earlier only ever keeps more, so this
/// terminates and can never orphan a call.
///
/// `None` means there is nothing worth folding.
fn compaction_boundary(messages: &[Message]) -> Option<usize> {
    if messages.len() <= KEEP_RECENT + 1 {
        return None;
    }
    let mut split = messages.len() - KEEP_RECENT;
    while split > 0 && carries_tool_result(&messages[split]) {
        split -= 1;
    }
    (split >= MIN_FOLD).then_some(split)
}

fn carries_tool_result(message: &Message) -> bool {
    let Message::User { content } = message else {
        return false;
    };
    content
        .as_array()
        .is_some_and(|blocks| blocks.iter().any(|b| b["type"] == "tool_result"))
}

/// Flatten messages into something a model can read as a transcript.
///
/// Images become a placeholder rather than their bytes: re-sending a megabyte of
/// base64 to be summarised would cost more than the turns it replaces.
fn transcribe(messages: &[Message]) -> String {
    let mut out = String::new();
    for message in messages {
        let (who, content) = match message {
            Message::User { content } => ("user", content.clone()),
            Message::Assistant { content } => ("agent", content.clone()),
            Message::System { content } => ("operator", serde_json::json!(content)),
        };
        out.push_str(&format!("\n[{who}]\n"));

        if let Some(text) = content.as_str() {
            out.push_str(text);
            out.push('\n');
            continue;
        }
        for block in content.as_array().into_iter().flatten() {
            match block["type"].as_str().unwrap_or_default() {
                "text" => out.push_str(block["text"].as_str().unwrap_or_default()),
                "image" => out.push_str("(image)"),
                "thinking" => continue,
                "tool_use" => out.push_str(&format!(
                    "(called {} with {})",
                    block["name"].as_str().unwrap_or("a tool"),
                    truncate(&block["input"].to_string(), 400)
                )),
                "tool_result" => out.push_str(&format!(
                    "(result: {})",
                    truncate(&block["content"].to_string(), 800)
                )),
                other => out.push_str(&format!("({other})")),
            }
            out.push('\n');
        }
    }
    // The summary request has to fit too. The tail is kept rather than the head,
    // because the recent past is what the next turn is most likely about.
    if out.len() > MAX_TRANSCRIPT {
        let start = out.len() - MAX_TRANSCRIPT;
        let start = (start..out.len())
            .find(|i| out.is_char_boundary(*i))
            .unwrap_or(out.len());
        out = format!("(earlier turns omitted)\n{}", &out[start..]);
    }
    out
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    format!("{}...", text.chars().take(max).collect::<String>())
}

/// The fraction of the window at which the conversation is shortened.
///
/// Below the limit, with room left for the answer itself: compaction that only
/// triggers once a request has already been refused is compaction that arrives
/// one turn too late.
const COMPACT_AT: f64 = 0.75;

/// The smallest fold worth doing.
///
/// Compaction costs an inference and loses detail, so it has to buy more than it
/// spends. Without a floor, a conversation sitting just over the line folds again
/// every couple of turns — each one replacing a summary with a summary of a
/// summary, which is how a chat quietly forgets everything it was about.
const MIN_FOLD: usize = KEEP_RECENT;

/// Turns kept verbatim after a compaction.
///
/// The recent past is what the next turn is usually about, so it survives as
/// itself. Everything older becomes a summary.
const KEEP_RECENT: usize = 6;

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
            context_window: 0,
            last_input_tokens: 0,
            turn: 0,
            caps,
            now,
        }
    }

    /// Tell the run how much window it has.
    ///
    /// Without this the conversation is never shortened, which is the right
    /// default for a caller that does not know the limit: guessing one would mean
    /// throwing away real context to respect a constraint that may not exist.
    pub fn with_context_window(mut self, tokens: u64) -> Self {
        self.context_window = tokens;
        self
    }

    /// Shorten the conversation if it no longer comfortably fits.
    ///
    /// A long chat otherwise ends by failing — the request is refused for length
    /// and the run stops mid-thought. The alternative is to fold the older part
    /// into a summary and carry on, which loses detail but keeps going. This picks
    /// the second, states it in the journal, and says so in the window.
    ///
    /// It runs on the measured size of the last request, not on an estimate. The
    /// loop does not tokenise, and a character count would be wrong by a different
    /// factor for prose, for code and for a base64 image.
    async fn compact_if_needed(&mut self) -> Result<(), CoreError> {
        if self.context_window == 0 {
            return Ok(());
        }
        if (self.last_input_tokens as f64) < self.context_window as f64 * COMPACT_AT {
            return Ok(());
        }
        let Some(split) = compaction_boundary(&self.messages) else {
            // Everything left is either recent or structurally inseparable. There
            // is nothing to fold, so the request goes out at its real size and the
            // provider decides — better than dropping half a tool call to make a
            // number look right.
            return Ok(());
        };

        let before = locked_journal::digest_bytes(&serde_json::to_vec(&self.messages)?);
        let earlier = self.messages[..split].to_vec();
        let summary = self.summarize(&earlier).await?;

        let mut kept = Vec::with_capacity(1 + self.messages.len() - split);
        kept.push(Message::System {
            content: format!(
                "The earlier part of this conversation no longer fits in context and \
                 has been replaced by this summary. Treat it as a record of what was \
                 said, not as something the user typed, and say so if you are asked \
                 about a detail it does not cover.\n\n{summary}"
            ),
        });
        kept.extend_from_slice(&self.messages[split..]);

        let dropped = split as u32;
        let after = locked_journal::digest_bytes(&serde_json::to_vec(&kept)?);
        self.messages = kept;
        // Measured again on the next request. Until then the old figure describes a
        // conversation that no longer exists.
        self.last_input_tokens = 0;

        self.record(
            Event::ConversationCompacted {
                dropped,
                kept: (self.messages.len() - 1) as u32,
                before_digest: before,
                after_digest: after,
            },
            Evidence::HarnessAttested,
        )?;
        self.sink.emit(UiEvent::Compacted { dropped });
        Ok(())
    }

    /// Fold a stretch of conversation into prose, through the same door.
    ///
    /// The transcript goes out as one block of text rather than as the messages
    /// themselves: a slice can end on a tool call whose result is in the part we
    /// are keeping, and a request carrying that dangling call is malformed. Text
    /// has no such structure to break.
    ///
    /// It is an inference, so it gets an inference receipt like any other. A
    /// summarisation that did not appear in the journal would be the one call the
    /// model made that nobody could see.
    async fn summarize(&mut self, earlier: &[Message]) -> Result<String, CoreError> {
        let transcript = transcribe(earlier);
        let response = self
            .llm
            .infer(
                InferenceRequest {
                    system: SUMMARY_PROMPT.to_string(),
                    messages: vec![Message::User {
                        content: serde_json::json!(transcript),
                    }],
                    tools: vec![],
                    max_tokens: 1500,
                },
                // Silent: these deltas are bookkeeping, and streaming them into the
                // transcript would look like the agent answering a question nobody
                // asked.
                &NullSink,
            )
            .await?;

        self.record(
            Event::Inference {
                model: response.model.clone(),
                prompt_digest: locked_journal::digest_bytes(transcript.as_bytes()),
                response_digest: locked_journal::digest_bytes(&serde_json::to_vec(
                    &response.content,
                )?),
                input_tokens: response.input_tokens,
                cached_tokens: response.cached_tokens,
                output_tokens: response.output_tokens,
            },
            Evidence::HarnessAttested,
        )?;

        let text = response
            .content
            .as_array()
            .into_iter()
            .flatten()
            .filter(|b| b["type"] == "text")
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");

        Ok(if text.trim().is_empty() {
            // A summary that came back empty is still a fact worth carrying: it
            // says the earlier turns are gone, which is the part the model most
            // needs to know.
            "(the summary came back empty - the earlier turns are no longer available)".into()
        } else {
            text
        })
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
                    locked_sandbox::Isolation::Container { image } => Some(image.clone()),
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
        self.compact_if_needed().await?;

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
                prompt_digest: locked_journal::digest_bytes(
                    &serde_json::to_vec(&self.messages)?,
                ),
                response_digest: locked_journal::digest_bytes(&serde_json::to_vec(
                    &response.content,
                )?),
                input_tokens: response.input_tokens,
                cached_tokens: response.cached_tokens,
                output_tokens: response.output_tokens,
            },
            // Nobody but us witnessed the inference. Since the LLM left TAP, this
            // is the honest label — and it is the same two-tier distinction the
            // reads already carry, not a special case.
            Evidence::HarnessAttested,
        )?;
        self.last_input_tokens = response.input_tokens;

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

            // A model that names an argument wrong gets told so and tries again.
            // Ending the run here would be the harness deciding that a typo is
            // unrecoverable — and it would leave the tool call unanswered, which
            // makes the *next* request malformed on top of it. Every `tool_use`
            // gets a `tool_result`, including the ones we could not read.
            let call: ToolCall = match serde_json::from_value(block.clone()) {
                Ok(call) => call,
                Err(e) => {
                    let name = block["name"].as_str().unwrap_or("tool").to_string();
                    self.sink.emit(UiEvent::ToolStarted {
                        name: name.clone(),
                        summary: "arguments rejected".into(),
                    });
                    self.sink.emit(UiEvent::ToolFinished {
                        name,
                        is_error: true,
                    });
                    results.push(ToolResult::new(
                        id,
                        format!("the arguments did not match the tool's schema: {e}"),
                        true,
                    ));
                    continue;
                }
            };
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
                                args_digest: locked_journal::digest_bytes(path.as_bytes()),
                                result_digest: locked_journal::digest_bytes(contents.as_bytes()),
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
                            args_digest: locked_journal::digest_bytes(command.as_bytes()),
                            result_digest: locked_journal::digest_bytes(combined.as_bytes()),
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

#[cfg(test)]
mod compaction_tests {
    use super::*;

    fn user(text: &str) -> Message {
        Message::User { content: serde_json::json!(text) }
    }

    fn result(id: &str) -> Message {
        Message::User {
            content: serde_json::json!([
                { "type": "tool_result", "tool_use_id": id, "content": "done" }
            ]),
        }
    }

    fn call(id: &str) -> Message {
        Message::Assistant {
            content: serde_json::json!([
                { "type": "tool_use", "id": id, "name": "fs_write", "input": { "path": "a" } }
            ]),
        }
    }

    fn agent(text: &str) -> Message {
        Message::Assistant {
            content: serde_json::json!([{ "type": "text", "text": text }]),
        }
    }

    /// A short conversation is left alone. Folding six messages into a summary of
    /// six messages costs an inference and buys nothing.
    #[test]
    fn a_short_conversation_is_not_worth_folding() {
        let short: Vec<Message> = (0..KEEP_RECENT).map(|i| user(&i.to_string())).collect();
        assert_eq!(compaction_boundary(&short), None);
    }

    /// The recent turns stay; everything before them goes.
    #[test]
    fn the_recent_turns_are_the_ones_kept() {
        let long: Vec<Message> = (0..20)
            .map(|i| {
                if i % 2 == 0 {
                    user(&i.to_string())
                } else {
                    agent(&i.to_string())
                }
            })
            .collect();
        assert_eq!(compaction_boundary(&long), Some(20 - KEEP_RECENT));
    }

    /// The one thing compaction must never do. A tool result names a tool call;
    /// send the result without the call and the request is malformed, so the cut
    /// walks backwards until both are on the same side of it.
    #[test]
    fn a_cut_never_separates_a_tool_call_from_its_result() {
        // The boundary lands on a run of tool results, so it has to walk back past
        // the call that produced them. Long enough that the fold is still worth
        // doing after the walk-back, so this tests the boundary and not the floor.
        let mut messages: Vec<Message> = (0..10).map(|i| user(&i.to_string())).collect();
        messages.push(call("t1"));
        for _ in 0..KEEP_RECENT {
            messages.push(result("t1"));
        }

        let split = compaction_boundary(&messages).expect("there is something to fold");
        assert!(
            !carries_tool_result(&messages[split]),
            "the kept side must not begin with an orphaned result"
        );
        // It walked back to the call itself, which is the first safe place.
        assert!(matches!(&messages[split], Message::Assistant { .. }));
    }

    /// A conversation that is nothing but one tool call and its results has no
    /// safe cut at all — and the right answer is to leave it whole and let the
    /// provider judge, not to break it to hit a number.
    #[test]
    fn a_conversation_with_no_safe_cut_is_left_alone() {
        let mut messages = vec![call("t1")];
        for _ in 0..20 {
            messages.push(result("t1"));
        }
        assert_eq!(compaction_boundary(&messages), None);
    }

    /// The transcript keeps what a summary would need and drops what it would only
    /// pay for — a base64 image re-sent to be summarised costs more than the turns
    /// it replaces.
    #[test]
    fn the_transcript_carries_substance_and_not_bytes() {
        let messages = vec![
            Message::User {
                content: serde_json::json!([
                    { "type": "text", "text": "look at this" },
                    { "type": "image", "source": { "data": "AAAABBBBCCCC" } },
                ]),
            },
            Message::Assistant {
                content: serde_json::json!([
                    { "type": "thinking", "thinking": "private reasoning" },
                    { "type": "tool_use", "id": "t1", "name": "fs_write",
                      "input": { "path": "notes.md" } },
                ]),
            },
            Message::System { content: "operator note".into() },
        ];
        let text = transcribe(&messages);

        assert!(text.contains("look at this"));
        assert!(text.contains("fs_write"));
        assert!(text.contains("notes.md"));
        assert!(text.contains("operator note"));
        assert!(text.contains("(image)"));
        assert!(!text.contains("AAAABBBBCCCC"), "image bytes must not be re-sent");
        assert!(!text.contains("private reasoning"), "thinking is not transcript");
    }

    /// Long transcripts are cut on a character boundary. Slicing a UTF-8 string at
    /// an arbitrary byte index panics, and a summariser that crashes on an accented
    /// word is worse than one that summarises badly.
    #[test]
    fn a_long_transcript_is_cut_without_splitting_a_character() {
        let big = "éàü ".repeat(MAX_TRANSCRIPT);
        let text = transcribe(&[user(&big)]);
        assert!(text.len() <= MAX_TRANSCRIPT + 64);
        assert!(text.starts_with("(earlier turns omitted)"));
    }
}
