//! **The only crate in the workspace with an HTTP client.**
//!
//! Enforced, not asserted: `scripts/check-egress-isolation.sh` fails CI if
//! `reqwest` (or any other HTTP stack) becomes reachable from `locked-core`,
//! `locked-tools`, `locked-sandbox`, or `locked-journal`. You can therefore
//! establish that the loop, the tool executor and the sandbox driver cannot open
//! a socket by reading a dependency graph, without reading their logic.
//!
//! Two doors, both declared here:
//!
//! 1. **TAP** — every third-party call. TAP holds the secrets and gates writes.
//! 2. **The model provider** — inference only.
//!
//! Door 2 exists because TAP's `/forward` caps a call at 30 seconds and buffers
//! the whole response (its secret-scanner needs the complete body, so this is a
//! security property, not an oversight). Long generations do not fit. The seam
//! back is `TapLlm` below: when TAP grows a per-credential timeout, wiring it is
//! a config change, not a rewrite.
//!
//! Neither client takes a URL from a caller. Hosts are constants; a method
//! signature never carries a target the agent could influence.

use locked_core::{
    ApprovalState, CoreError, CredentialInfo, EventSink, ForwardCall, ForwardOutcome, Forwarder,
    InferenceRequest, InferenceResponse, Integrity, LlmTransport,
};
use async_trait::async_trait;

const TAP_HOST: &str = "proxy.tap.human.tech";

/// A closed set of model providers.
///
/// Deliberately an enum and not a string: the provider is configurable, the URL
/// is not. There is no code path where an attacker-influenced value becomes a
/// request target.
#[derive(Clone, Copy, Debug)]
pub enum LlmProvider {
    Anthropic,
    Kimi,
}

impl LlmProvider {
    fn messages_url(self) -> &'static str {
        match self {
            // Both speak the Messages API, so one client covers both.
            Self::Anthropic => "https://api.anthropic.com/v1/messages",
            Self::Kimi => "https://api.kimi.com/coding/v1/messages",
        }
    }

    fn models_url(self) -> &'static str {
        match self {
            Self::Anthropic => "https://api.anthropic.com/v1/models",
            Self::Kimi => "https://api.kimi.com/coding/v1/models",
        }
    }

    /// Same wire format, different auth header. Anthropic takes `x-api-key`;
    /// Kimi's Anthropic-compatible endpoint takes a bearer token — which is also
    /// what its TAP credential declares (`auth_mode: authorization_header`).
    fn auth_header(self, key: &str) -> (&'static str, String) {
        match self {
            Self::Anthropic => ("x-api-key", key.to_string()),
            Self::Kimi => ("Authorization", format!("Bearer {key}")),
        }
    }
}

fn transport_err(e: impl std::fmt::Display) -> CoreError {
    CoreError::Transport(e.to_string())
}

// ---------------------------------------------------------------------------
// Door 1 — TAP
// ---------------------------------------------------------------------------

pub struct TapForwarder {
    key: String,
    client: reqwest::Client,
}

impl TapForwarder {
    pub fn new(key: String) -> Result<Self, CoreError> {
        Ok(Self {
            key,
            client: reqwest::Client::builder()
                // TAP itself caps at 30s; going longer here only delays the
                // error we would get anyway.
                .timeout(std::time::Duration::from_secs(35))
                .build()
                .map_err(transport_err)?,
        })
    }
}

#[async_trait]
impl Forwarder for TapForwarder {
    async fn discover(&self) -> Result<Vec<CredentialInfo>, CoreError> {
        let raw: serde_json::Value = self
            .client
            .get(format!("https://{TAP_HOST}/agent/services"))
            .header("X-TAP-Key", &self.key)
            .send()
            .await
            .map_err(transport_err)?
            .json()
            .await
            .map_err(transport_err)?;

        Ok(raw["services"]
            .as_object()
            .map(|services| {
                services
                    .iter()
                    .map(|(name, svc)| CredentialInfo {
                        // `/agent/services` namespaces names by owning key
                        // ("agent.kimi"); `/forward` wants the bare name.
                        name: name.rsplit('.').next().unwrap_or(name).to_string(),
                        target_shape: svc["target_shape"].as_str().unwrap_or("full_url").into(),
                        writes_auto_approve: writes_auto_approve(svc),
                        description: svc["description"].as_str().unwrap_or_default().into(),
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn forward(&self, call: ForwardCall) -> Result<ForwardOutcome, CoreError> {
        let mut req = self
            .client
            .post(format!("https://{TAP_HOST}/forward"))
            .header("X-TAP-Key", &self.key)
            .header("X-TAP-Target", &call.target)
            .header("X-TAP-Method", call.method.to_uppercase())
            .header("X-TAP-Credential", &call.credential)
            // Several upstreams sit behind bot protection that rejects a default
            // Rust user-agent with a 403 — Cerebras does.
            .header(
                "User-Agent",
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36",
            );
        if let Some(body) = call.body {
            req = req.header("Content-Type", "application/json").body(body);
        }

        let resp = req.send().await.map_err(transport_err)?;
        let status = resp.status().as_u16();
        let body = resp.text().await.map_err(transport_err)?;

        // 202 is the only status that means "a human is deciding". Everything
        // else — including an upstream 4xx — came back with a real answer.
        if status == 202 {
            let parsed: serde_json::Value = serde_json::from_str(&body)?;
            if let Some(txn_id) = parsed["txn_id"].as_str() {
                return Ok(ForwardOutcome::Pending {
                    txn_id: txn_id.to_string(),
                });
            }
        }
        Ok(ForwardOutcome::Immediate {
            source_id: source_identifier(&body),
            status,
            body,
        })
    }

    /// Where a pending write stands.
    ///
    /// Two things here are not obvious, and both were found the hard way.
    ///
    /// First, this route will not take a comma-separated key list. `/forward` and
    /// `/agent/services` both accept one and resolve the owning key themselves;
    /// `/agent/approvals/{txn}` answers 401 to the same header. So the keys are
    /// tried one at a time, and the one that owns the transaction replies —
    /// the others say 401 or "not your transaction".
    ///
    /// Second, and worse: this used to read `status` straight off the body
    /// without looking at the HTTP code. An error body has no `status`, so a 401
    /// was being read as "the human has not decided yet" — reported to the agent,
    /// and to the window, as *pending*, forever. An auth failure dressed as a
    /// benign state is precisely the failure mode this project exists to refuse,
    /// so a non-success now returns an error and says which code it was.
    async fn poll(&self, txn_id: &str) -> Result<ApprovalState, CoreError> {
        let mut last = "no key was tried".to_string();

        for key in self.key.split(',').map(str::trim).filter(|k| !k.is_empty()) {
            let resp = self
                .client
                .get(format!("https://{TAP_HOST}/agent/approvals/{txn_id}"))
                .header("X-TAP-Key", key)
                .send()
                .await
                .map_err(transport_err)?;

            let code = resp.status();
            let raw: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);

            if !code.is_success() {
                last = format!(
                    "{code} — {}",
                    raw["error"].as_str().unwrap_or("no detail from the proxy")
                );
                continue;
            }

            return Ok(match raw["status"].as_str().unwrap_or("pending") {
                "forwarded" => ApprovalState::Forwarded {
                    status: raw["response"]["status"].as_u64().unwrap_or(0) as u16,
                    body: raw["response"]["body"].as_str().unwrap_or_default().into(),
                },
                "denied" => ApprovalState::Denied,
                "error" => ApprovalState::Failed {
                    detail: raw["error_detail"].as_str().unwrap_or("unknown").into(),
                },
                _ => ApprovalState::Pending,
            });
        }

        Err(CoreError::Transport(format!(
            "could not read approval {txn_id}: {last}"
        )))
    }

    async fn await_decision(
        &self,
        txn_id: &str,
        timeout_secs: u64,
    ) -> Result<ApprovalState, CoreError> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);

        loop {
            // A blip while waiting is not a verdict, so a failed poll retries
            // rather than aborting — but whatever the last poll said is what
            // gets returned. Reporting "still pending" after a wait that never
            // got one clean answer would be a guess dressed as a fact.
            let outcome = self.poll(txn_id).await;
            if matches!(&outcome, Ok(state) if !matches!(state, ApprovalState::Pending)) {
                return outcome;
            }
            if std::time::Instant::now() >= deadline {
                return outcome;
            }
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        }
    }
}

fn writes_auto_approve(svc: &serde_json::Value) -> bool {
    svc["approval"]["rules"]
        .as_array()
        .map(|rules| {
            rules.iter().any(|r| {
                r["url_override"].is_null()
                    && r["decision"] == "proceeds_immediately"
                    && r["methods"]
                        .as_array()
                        .map(|m| m.iter().any(|m| m == "POST"))
                        .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

/// Pull an identifier the upstream volunteered, so a read can be corroborated by
/// its source rather than only by our own journal.
///
/// This is the cheap half of the answer to the reads-have-no-witness problem.
/// It covers the sources that offer one and honestly claims nothing for the rest.
fn source_identifier(body: &str) -> Option<(String, String)> {
    let parsed: serde_json::Value = serde_json::from_str(body).ok()?;
    for key in ["execution_id", "query_id", "request_id"] {
        if let Some(id) = parsed[key].as_str() {
            return Some((key.to_string(), id.to_string()));
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Door 2 — the model provider
// ---------------------------------------------------------------------------

pub struct DirectLlm {
    provider: LlmProvider,
    model: String,
    key: String,
    client: reqwest::Client,
}

impl DirectLlm {
    pub fn new(provider: LlmProvider, model: String, key: String) -> Result<Self, CoreError> {
        Ok(Self {
            provider,
            model,
            key,
            client: reqwest::Client::builder()
                // No 30s ceiling here: a long turn at high effort is the point of
                // taking this door.
                .timeout(std::time::Duration::from_secs(600))
                .build()
                .map_err(transport_err)?,
        })
    }

    /// The context window, as the provider states it.
    ///
    /// A gauge whose denominator this app invented would be decoration. Kimi's
    /// model list carries `context_length`, so the number under the ring is
    /// reported rather than assumed; providers that don't publish one return
    /// `None` and the caller falls back to a declared default.
    pub async fn context_length(&self) -> Option<u64> {
        #[derive(serde::Deserialize)]
        struct Models {
            data: Vec<Entry>,
        }
        #[derive(serde::Deserialize)]
        struct Entry {
            id: String,
            context_length: Option<u64>,
        }

        let (auth_name, auth_value) = self.provider.auth_header(&self.key);
        let resp = self
            .client
            .get(self.provider.models_url())
            .header(auth_name, auth_value)
            .header("anthropic-version", "2023-06-01")
            .timeout(std::time::Duration::from_secs(8))
            .send()
            .await
            .ok()?;
        if !resp.status().is_success() {
            return None;
        }
        resp.json::<Models>()
            .await
            .ok()?
            .data
            .into_iter()
            .find(|e| e.id == self.model)
            .and_then(|e| e.context_length)
    }
}

#[async_trait]
impl LlmTransport for DirectLlm {
    async fn infer(
        &self,
        req: InferenceRequest,
        sink: &dyn EventSink,
    ) -> Result<InferenceResponse, CoreError> {
        let (auth_name, auth_value) = self.provider.auth_header(&self.key);
        let resp = self
            .client
            .post(self.provider.messages_url())
            .header(auth_name, auth_value)
            .header("anthropic-version", "2023-06-01")
            .json(&serde_json::json!({
                "model": self.model,
                "max_tokens": req.max_tokens,
                "system": req.system,
                "messages": req.messages,
                "tools": req.tools,
                // Streaming is available here precisely because this door does not
                // go through TAP: the proxy buffers whole responses so its
                // secret-scanner can read them. See the module docs.
                "stream": true,
            }))
            .send()
            .await
            .map_err(transport_err)?;

        // Check the status before reading the stream. Without this an error body
        // is parsed as an (empty) response, the loop sees `end_turn`, and the run
        // reports success having done nothing — the worst failure mode there is
        // for something whose output is an audit trail.
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(CoreError::Transport(format!(
                "model provider returned {status}: {}",
                body.chars().take(400).collect::<String>()
            )));
        }

        SseAccumulator::new(&self.model).consume(resp, sink).await
    }

    fn integrity(&self) -> Integrity {
        // The run says so itself rather than letting a reader assume.
        Integrity::Degraded {
            reason: "inference goes direct to the model provider, not through TAP \
                     (TAP /forward caps at 30s and buffers)"
                .into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Server-sent events
// ---------------------------------------------------------------------------

/// Rebuilds a Messages-API response from its event stream while forwarding text
/// to the sink as it arrives.
///
/// The reconstruction is not optional bookkeeping: `tool_use` inputs arrive as
/// fragments of JSON and `thinking` blocks arrive with a signature that must be
/// echoed back verbatim on the next turn. Dropping either breaks the following
/// request rather than this one.
struct SseAccumulator {
    model: String,
    blocks: Vec<Block>,
    stop_reason: String,
    input_tokens: u64,
    output_tokens: u64,
}

#[derive(Default)]
struct Block {
    kind: String,
    text: String,
    signature: String,
    id: String,
    name: String,
}

impl SseAccumulator {
    fn new(model: &str) -> Self {
        Self {
            model: model.to_string(),
            blocks: Vec::new(),
            stop_reason: "end_turn".into(),
            input_tokens: 0,
            output_tokens: 0,
        }
    }

    async fn consume(
        mut self,
        resp: reqwest::Response,
        sink: &dyn EventSink,
    ) -> Result<InferenceResponse, CoreError> {
        use futures_util::StreamExt;

        let mut stream = resp.bytes_stream();
        let mut buffer = String::new();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(transport_err)?;
            buffer.push_str(&String::from_utf8_lossy(&chunk));

            // SSE frames are separated by a blank line, but a chunk can split one
            // anywhere — so only complete lines are consumed and the remainder is
            // carried into the next read.
            while let Some(newline) = buffer.find('\n') {
                let line = buffer[..newline].trim_end_matches('\r').to_string();
                buffer.drain(..=newline);
                // The space after "data:" is optional in the SSE spec, and Kimi
                // omits it. Assuming it silently yields a stream that parses to
                // nothing at all.
                if let Some(payload) = line.strip_prefix("data:") {
                    self.event(payload.trim_start(), sink)?;
                }
            }
        }

        if self.blocks.is_empty() {
            return Err(CoreError::Transport(
                "model provider streamed no content".into(),
            ));
        }

        Ok(InferenceResponse {
            model: self.model,
            content: serde_json::Value::Array(
                self.blocks.iter().map(Block::finish).collect(),
            ),
            stop_reason: self.stop_reason,
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
        })
    }

    fn event(&mut self, payload: &str, sink: &dyn EventSink) -> Result<(), CoreError> {
        let Ok(e) = serde_json::from_str::<serde_json::Value>(payload) else {
            return Ok(()); // keep-alives and comments
        };

        match e["type"].as_str().unwrap_or_default() {
            "message_start" => {
                if let Some(m) = e["message"].as_str() {
                    self.model = m.to_string();
                }
                if let Some(m) = e["message"]["model"].as_str() {
                    self.model = m.to_string();
                }
                self.input_tokens = e["message"]["usage"]["input_tokens"].as_u64().unwrap_or(0);
            }

            "content_block_start" => {
                let index = e["index"].as_u64().unwrap_or(0) as usize;
                let cb = &e["content_block"];
                self.slot(index).kind = cb["type"].as_str().unwrap_or("text").to_string();
                self.slot(index).id = cb["id"].as_str().unwrap_or_default().to_string();
                self.slot(index).name = cb["name"].as_str().unwrap_or_default().to_string();
            }

            "content_block_delta" => {
                let index = e["index"].as_u64().unwrap_or(0) as usize;
                let d = &e["delta"];
                match d["type"].as_str().unwrap_or_default() {
                    "text_delta" => {
                        let t = d["text"].as_str().unwrap_or_default();
                        self.slot(index).text.push_str(t);
                        sink.emit(locked_core::UiEvent::AssistantDelta { text: t.into() });
                    }
                    "thinking_delta" => {
                        let t = d["thinking"].as_str().unwrap_or_default();
                        self.slot(index).text.push_str(t);
                        sink.emit(locked_core::UiEvent::ThinkingDelta { text: t.into() });
                    }
                    // Tool inputs stream as JSON fragments; they are only valid
                    // once the block closes, so nothing is emitted here.
                    "input_json_delta" => {
                        let t = d["partial_json"].as_str().unwrap_or_default();
                        self.slot(index).text.push_str(t);
                    }
                    "signature_delta" => {
                        let t = d["signature"].as_str().unwrap_or_default();
                        self.slot(index).signature.push_str(t);
                    }
                    _ => {}
                }
            }

            "message_delta" => {
                if let Some(r) = e["delta"]["stop_reason"].as_str() {
                    self.stop_reason = r.to_string();
                }
                if let Some(o) = e["usage"]["output_tokens"].as_u64() {
                    self.output_tokens = o;
                }
            }

            "error" => {
                return Err(CoreError::Transport(format!(
                    "model provider streamed an error: {}",
                    e["error"]["message"].as_str().unwrap_or("unknown")
                )));
            }

            _ => {}
        }
        Ok(())
    }

    fn slot(&mut self, index: usize) -> &mut Block {
        while self.blocks.len() <= index {
            self.blocks.push(Block::default());
        }
        &mut self.blocks[index]
    }
}

impl Block {
    fn finish(&self) -> serde_json::Value {
        match self.kind.as_str() {
            "tool_use" => serde_json::json!({
                "type": "tool_use",
                "id": self.id,
                "name": self.name,
                // An empty fragment stream means a tool called with no arguments.
                "input": serde_json::from_str::<serde_json::Value>(&self.text)
                    .unwrap_or_else(|_| serde_json::json!({})),
            }),
            "thinking" => serde_json::json!({
                "type": "thinking",
                "thinking": self.text,
                "signature": self.signature,
            }),
            "redacted_thinking" => serde_json::json!({
                "type": "redacted_thinking",
                "data": self.text,
            }),
            _ => serde_json::json!({ "type": "text", "text": self.text }),
        }
    }
}

/// The way back. Not wired today — kept compiling so the return stays a config
/// change. The day TAP grows a per-credential forward timeout, this becomes the
/// default and `integrity()` reports `Full`.
pub struct TapLlm<'a> {
    pub forwarder: &'a dyn Forwarder,
    pub credential: String,
    pub provider: LlmProvider,
    pub model: String,
}

#[async_trait]
impl LlmTransport for TapLlm<'_> {
    /// No deltas: TAP buffers the whole upstream response before returning it, so
    /// there is nothing to forward as it happens. The sink is accepted and unused,
    /// which is the honest shape — the UI simply receives the text at the end.
    async fn infer(
        &self,
        req: InferenceRequest,
        _sink: &dyn EventSink,
    ) -> Result<InferenceResponse, CoreError> {
        let outcome = self
            .forwarder
            .forward(ForwardCall {
                credential: self.credential.clone(),
                target: self.provider.messages_url().to_string(),
                method: "POST".into(),
                body: Some(serde_json::to_string(&serde_json::json!({
                    "model": self.model,
                    "max_tokens": req.max_tokens,
                    "system": req.system,
                    "messages": req.messages,
                    "tools": req.tools,
                }))?),
            })
            .await?;

        match outcome {
            ForwardOutcome::Immediate { body, .. } => {
                let raw: serde_json::Value = serde_json::from_str(&body)?;
                Ok(InferenceResponse {
                    model: raw["model"].as_str().unwrap_or(&self.model).to_string(),
                    content: raw["content"].clone(),
                    stop_reason: raw["stop_reason"].as_str().unwrap_or("end_turn").to_string(),
                    input_tokens: raw["usage"]["input_tokens"].as_u64().unwrap_or(0),
                    output_tokens: raw["usage"]["output_tokens"].as_u64().unwrap_or(0),
                })
            }
            // An inference must never sit in an approval queue. If the credential
            // is not scoped to auto-approve the inference route, that is a
            // misconfiguration, and failing loudly beats hanging.
            ForwardOutcome::Pending { txn_id } => Err(CoreError::Transport(format!(
                "inference credential '{}' is not auto-approved (txn {txn_id}); \
                 scope a proceeds_immediately rule to the inference route",
                self.credential
            ))),
        }
    }

    fn integrity(&self) -> Integrity {
        Integrity::Full
    }
}
