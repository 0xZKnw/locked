//! Test doubles for the two things the loop talks to.
//!
//! Both are scripted rather than clever: a test says exactly what the model will
//! answer and exactly what TAP will do, so a failure names one behaviour of the
//! loop instead of a flaky conversation. Every double also *records* what it was
//! asked, because half the properties worth asserting are about the request, not
//! the reply — what tools the model was shown, what target reached TAP.

use airlock_core::{
    ApprovalState, CoreError, CredentialInfo, EventSink, ForwardCall, ForwardOutcome, Forwarder,
    InferenceRequest, InferenceResponse, Integrity, LlmTransport, UiEvent,
};
use async_trait::async_trait;
use std::sync::Mutex;

// ---------------------------------------------------------------------------
// The model
// ---------------------------------------------------------------------------

/// One scripted assistant turn: the content blocks it returns.
pub fn text(s: &str) -> serde_json::Value {
    serde_json::json!({ "type": "text", "text": s })
}

pub fn tool(id: &str, name: &str, input: serde_json::Value) -> serde_json::Value {
    serde_json::json!({ "type": "tool_use", "id": id, "name": name, "input": input })
}

pub struct ScriptedLlm {
    turns: Mutex<std::collections::VecDeque<Vec<serde_json::Value>>>,
    /// Every request the loop made, in order. Lets a test assert what the model
    /// was actually shown.
    pub seen: Mutex<Vec<InferenceRequest>>,
    integrity: Integrity,
}

impl ScriptedLlm {
    pub fn new(turns: Vec<Vec<serde_json::Value>>) -> Self {
        Self {
            turns: Mutex::new(turns.into()),
            seen: Mutex::new(Vec::new()),
            integrity: Integrity::Degraded {
                reason: "scripted".into(),
            },
        }
    }

    pub fn last_request(&self) -> InferenceRequest {
        self.seen.lock().unwrap().last().cloned().expect("a request")
    }

}

#[async_trait]
impl LlmTransport for ScriptedLlm {
    async fn infer(
        &self,
        req: InferenceRequest,
        sink: &dyn EventSink,
    ) -> Result<InferenceResponse, CoreError> {
        self.seen.lock().unwrap().push(req);

        let blocks = self
            .turns
            .lock()
            .unwrap()
            .pop_front()
            .expect("the script ran out of turns — the loop asked for one more than expected");

        // Text is streamed, exactly as the real transport does, so the tests
        // exercise the delta path rather than a shortcut.
        for b in &blocks {
            if b["type"] == "text" {
                sink.emit(UiEvent::AssistantDelta {
                    text: b["text"].as_str().unwrap_or_default().to_string(),
                });
            }
        }

        let wants_tools = blocks.iter().any(|b| b["type"] == "tool_use");
        Ok(InferenceResponse {
            model: "scripted-1".into(),
            content: serde_json::Value::Array(blocks),
            stop_reason: if wants_tools { "tool_use" } else { "end_turn" }.into(),
            input_tokens: 100,
            output_tokens: 10,
        })
    }

    fn integrity(&self) -> Integrity {
        self.integrity.clone()
    }
}

// ---------------------------------------------------------------------------
// TAP
// ---------------------------------------------------------------------------

pub struct ScriptedTap {
    forwards: Mutex<std::collections::VecDeque<ForwardOutcome>>,
    polls: Mutex<std::collections::VecDeque<ApprovalState>>,
    /// Every call that reached the proxy. Used to assert what the loop sent —
    /// notably that a full URL with a query string goes out intact even though
    /// the receipt records only the host.
    pub calls: Mutex<Vec<ForwardCall>>,
    creds: Vec<CredentialInfo>,
}

impl ScriptedTap {
    pub fn new(forwards: Vec<ForwardOutcome>, polls: Vec<ApprovalState>) -> Self {
        Self {
            forwards: Mutex::new(forwards.into()),
            polls: Mutex::new(polls.into()),
            calls: Mutex::new(Vec::new()),
            creds: vec![CredentialInfo {
                name: "dune".into(),
                target_shape: "full_url".into(),
                writes_auto_approve: false,
                description: "Dune Analytics".into(),
            }],
        }
    }

    pub fn empty() -> Self {
        Self::new(Vec::new(), Vec::new())
    }
}

#[async_trait]
impl Forwarder for ScriptedTap {
    async fn discover(&self) -> Result<Vec<CredentialInfo>, CoreError> {
        Ok(self.creds.clone())
    }

    async fn forward(&self, call: ForwardCall) -> Result<ForwardOutcome, CoreError> {
        self.calls.lock().unwrap().push(call);
        self.forwards
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| CoreError::Transport("no scripted outcome".into()))
    }

    async fn poll(&self, _txn_id: &str) -> Result<ApprovalState, CoreError> {
        Ok(self
            .polls
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(ApprovalState::Pending))
    }

    async fn await_decision(
        &self,
        txn_id: &str,
        _timeout_secs: u64,
    ) -> Result<ApprovalState, CoreError> {
        self.poll(txn_id).await
    }
}

// ---------------------------------------------------------------------------
// The event stream
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct Collector(Mutex<Vec<UiEvent>>);

impl EventSink for Collector {
    fn emit(&self, event: UiEvent) {
        self.0.lock().unwrap().push(event);
    }
}

impl Collector {
    pub fn events(&self) -> Vec<UiEvent> {
        self.0.lock().unwrap().clone()
    }

    /// The event stream as `type` tags, which is what most assertions are really
    /// about: did the window see the right *sequence*.
    pub fn kinds(&self) -> Vec<String> {
        self.events()
            .iter()
            .map(|e| {
                serde_json::to_value(e).unwrap()["type"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect()
    }

    /// Concatenated assistant text, as the transcript would show it.
    pub fn answer(&self) -> String {
        self.events()
            .iter()
            .filter_map(|e| match e {
                UiEvent::AssistantDelta { text } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Scratch space
// ---------------------------------------------------------------------------

/// A directory of its own per test, removed first so a previous failure cannot
/// make the next run pass.
pub fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("locked-e2e-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

pub fn stamp() -> String {
    "2026-07-25T00:00:00Z".into()
}
