//! End-to-end tests of the agent loop.
//!
//! Everything below drives a real `Run` against a real journal on disk and a real
//! workspace sandbox — only the model and TAP are scripted, because those are the
//! two things a test cannot make deterministic. The assertions are deliberately
//! about *observable* behaviour: what the window saw, what landed in the chain,
//! what reached the filesystem. Nothing pokes at internals.
//!
//! The properties worth defending here are the ones the project claims out loud:
//! a run's receipts describe exactly what it could do, the query string of a call
//! never reaches durable storage, a pending write does not block the agent, and a
//! tool the run cannot honour is never offered.

mod harness;

use locked_core::{ApprovalState, ForwardOutcome, Message, Run, UiEvent};
use locked_journal::{Chain, Evidence, Event};
use locked_sandbox::{Isolation, LocalWorkspace, NoSandbox, Sandbox};
use locked_tools::Capabilities;
use harness::*;

const SYSTEM: &str = "test system prompt";

/// The receipt every run opens with, as a parsed event.
fn run_started(chain: &Chain) -> Event {
    chain
        .receipts()
        .iter()
        .find(|r| matches!(r.event, Event::RunStarted { .. }))
        .expect("a run always opens with RunStarted")
        .event
        .clone()
}

// ---------------------------------------------------------------------------

/// The whole loop, once: the model writes a file, reads it back, then answers.
///
/// This is the shape almost every real run takes, so it is worth one test that
/// checks the entire chain of consequences rather than several that each check a
/// link.
#[tokio::test]
async fn a_run_writes_reads_and_answers() {
    let dir = scratch("full-run");
    let ws = LocalWorkspace::open(dir.join("workspace")).unwrap();
    let chain = Chain::open(dir.join("journal.jsonl")).unwrap();

    let llm = ScriptedLlm::new(vec![
        vec![tool("t1", "fs_write", serde_json::json!({
            "path": "notes.md", "contents": "three lines"
        }))],
        vec![tool("t2", "fs_read", serde_json::json!({ "path": "notes.md" }))],
        vec![text("Written and read back.")],
    ]);
    let tap = ScriptedTap::empty();
    let sink = Collector::default();

    let mut run = Run::new(&llm, &tap, &ws, chain, &sink, Capabilities::FILES, stamp);
    run.start("write some notes").await.unwrap();
    while run.step(SYSTEM, 1000).await.unwrap() {}

    // What the window saw.
    assert_eq!(
        sink.kinds(),
        vec![
            "receipt_appended", // run_started
            "turn_started",
            "receipt_appended", // inference
            "tool_started",
            "receipt_appended", // sandbox_call
            "tool_finished",
            "turn_started",
            "receipt_appended", // inference
            "tool_started",
            "tool_finished", // fs_read records nothing: it changed nothing
            "turn_started",
            "assistant_delta",
            "receipt_appended", // inference
            "receipt_appended", // run_finished
            "run_finished",
        ]
    );
    assert_eq!(sink.answer(), "Written and read back.");

    // What reached the filesystem.
    assert_eq!(
        std::fs::read_to_string(dir.join("workspace").join("notes.md")).unwrap(),
        "three lines"
    );

    // What the chain says, and that it says it consistently.
    let chain = Chain::open(dir.join("journal.jsonl")).unwrap();
    assert_eq!(chain.verify().unwrap(), chain.receipts().len() as u64);
    assert!(chain.receipts().len() >= 6);

    // Reads are not recorded; writes are. A journal that logged every read would
    // drown the one thing it exists to show.
    let sandbox_calls: Vec<_> = chain
        .receipts()
        .iter()
        .filter_map(|r| match &r.event {
            Event::SandboxCall { tool, .. } => Some(tool.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(sandbox_calls, vec!["fs_write"]);
}

/// A run's opening receipt must describe the run's real surface — both the tools
/// it was offered and the strength of the boundary behind them.
#[tokio::test]
async fn the_opening_receipt_states_the_runs_real_surface() {
    let dir = scratch("surface");
    let ws = LocalWorkspace::open(dir.join("workspace")).unwrap();
    let chain = Chain::open(dir.join("journal.jsonl")).unwrap();

    let llm = ScriptedLlm::new(vec![vec![text("done")]]);
    let tap = ScriptedTap::empty();
    let sink = Collector::default();

    let mut run = Run::new(&llm, &tap, &ws, chain, &sink, Capabilities::FILES, stamp);
    run.start("nothing").await.unwrap();
    while run.step(SYSTEM, 1000).await.unwrap() {}

    let Event::RunStarted {
        tools,
        isolation,
        sandbox_image,
        ..
    } = run_started(run.chain())
    else {
        unreachable!()
    };

    assert_eq!(isolation, "workspace");
    assert_eq!(sandbox_image, None, "there is no image at this tier");
    assert!(tools.contains(&"fs_write".to_string()));
    assert!(
        !tools.contains(&"exec".to_string()),
        "the workspace tier cannot honour a shell, so it must not claim one"
    );

    // And the model was shown exactly that list — no more.
    let offered: Vec<String> = llm.last_request().tools.iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(offered, tools);
}

/// A run with no sandbox is not a run with disabled tools: the model is never
/// told the tools exist.
#[tokio::test]
async fn a_tap_only_run_never_mentions_a_filesystem() {
    let dir = scratch("tap-only");
    let chain = Chain::open(dir.join("journal.jsonl")).unwrap();

    let llm = ScriptedLlm::new(vec![vec![text("ok")]]);
    let tap = ScriptedTap::empty();
    let sink = Collector::default();
    let none = NoSandbox;

    let mut run = Run::new(&llm, &tap, &none, chain, &sink, Capabilities::TAP_ONLY, stamp);
    run.start("hello").await.unwrap();
    while run.step(SYSTEM, 1000).await.unwrap() {}

    let offered: Vec<String> = llm.last_request().tools.iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(offered, vec!["tap_discover", "tap_call", "tap_check", "tap_await"]);
    assert_eq!(none.isolation(), Isolation::None);

    let Event::RunStarted { isolation, .. } = run_started(run.chain()) else {
        unreachable!()
    };
    assert_eq!(isolation, "none");
}

/// The receipt records the host and nothing else. A query string can carry an
/// API key, and a journal is durable.
#[tokio::test]
async fn a_query_string_never_reaches_the_journal() {
    let dir = scratch("query");
    let chain = Chain::open(dir.join("journal.jsonl")).unwrap();

    let llm = ScriptedLlm::new(vec![
        vec![tool("t1", "tap_call", serde_json::json!({
            "credential": "dune",
            "target": "https://api.dune.com/v1/query/42/results?api_key=SUPERSECRET",
            "method": "GET"
        }))],
        vec![text("done")],
    ]);
    let tap = ScriptedTap::new(
        vec![ForwardOutcome::Immediate {
            status: 200,
            body: "{}".into(),
            source_id: Some(("execution_id".into(), "01JX".into())),
        }],
        vec![],
    );
    let sink = Collector::default();
    let none = NoSandbox;

    let mut run = Run::new(&llm, &tap, &none, chain, &sink, Capabilities::TAP_ONLY, stamp);
    run.start("fetch it").await.unwrap();
    while run.step(SYSTEM, 1000).await.unwrap() {}

    // The proxy got the whole URL — the redaction is about storage, not delivery.
    assert!(tap.calls.lock().unwrap()[0].target.contains("SUPERSECRET"));

    let raw = std::fs::read_to_string(dir.join("journal.jsonl")).unwrap();
    assert!(
        !raw.contains("SUPERSECRET"),
        "the journal is durable; a key in a query string must never land in it"
    );
    assert!(raw.contains("api.dune.com"));

    // An identifier the upstream volunteered upgrades the receipt above our word.
    let ev = run.chain().receipts().iter().find_map(|r| match &r.event {
        Event::TapCall { .. } => Some(r.evidence.clone()),
        _ => None,
    });
    assert_eq!(
        ev,
        Some(Evidence::SourceAttested {
            scheme: "execution_id".into(),
            id: "01JX".into()
        })
    );
}

/// A write that pauses for a human must not stop the agent. It is told, it keeps
/// working, and the decision re-enters the conversation on a later turn.
#[tokio::test]
async fn a_pending_write_does_not_block_the_agent() {
    let dir = scratch("approval");
    let chain = Chain::open(dir.join("journal.jsonl")).unwrap();

    let llm = ScriptedLlm::new(vec![
        vec![tool("t1", "tap_call", serde_json::json!({
            "credential": "dune", "target": "https://api.dune.com/v1/execute", "method": "POST"
        }))],
        // The agent carries on rather than waiting.
        vec![text("Kicked it off, carrying on. ")],
    ]);
    let tap = ScriptedTap::new(
        vec![ForwardOutcome::Pending { txn_id: "txn_7".into() }],
        vec![ApprovalState::Forwarded { status: 200, body: "{}".into() }],
    );
    let sink = Collector::default();
    let none = NoSandbox;

    let mut run = Run::new(&llm, &tap, &none, chain, &sink, Capabilities::TAP_ONLY, stamp);
    run.start("run the query").await.unwrap();
    while run.step(SYSTEM, 1000).await.unwrap() {}

    let kinds = sink.kinds();
    assert!(kinds.contains(&"approval_pending".to_string()));
    assert!(kinds.contains(&"approval_resolved".to_string()));

    // The pending write is the one receipt a third party could corroborate.
    let pending = run.chain().receipts().iter().find(|r| {
        matches!(&r.event, Event::TapCall { upstream_status: None, .. })
    });
    assert_eq!(
        pending.map(|r| r.evidence.clone()),
        Some(Evidence::TapAttested { txn_id: "txn_7".into() })
    );

    // And the decision reached the model as an operator message, not as a user
    // one — that distinction is what carries the authority.
    let resumed = llm.last_request();
    let operator: Vec<&Message> = resumed
        .messages
        .iter()
        .filter(|m| matches!(m, Message::System { .. }))
        .collect();
    assert_eq!(operator.len(), 1);
    let Message::System { content } = operator[0] else { unreachable!() };
    assert!(content.contains("txn_7") && content.contains("approved"));
}

/// A refusal is a decision, not an error, and the model is told plainly.
#[tokio::test]
async fn a_refused_write_is_reported_as_a_decision() {
    let dir = scratch("denied");
    let chain = Chain::open(dir.join("journal.jsonl")).unwrap();

    let llm = ScriptedLlm::new(vec![
        vec![tool("t1", "tap_call", serde_json::json!({
            "credential": "dune", "target": "https://api.dune.com/v1/execute", "method": "DELETE"
        }))],
        vec![text("Understood, it was refused.")],
    ]);
    let tap = ScriptedTap::new(
        vec![ForwardOutcome::Pending { txn_id: "txn_9".into() }],
        vec![ApprovalState::Denied],
    );
    let sink = Collector::default();
    let none = NoSandbox;

    let mut run = Run::new(&llm, &tap, &none, chain, &sink, Capabilities::TAP_ONLY, stamp);
    run.start("delete it").await.unwrap();
    while run.step(SYSTEM, 1000).await.unwrap() {}

    assert!(sink.events().iter().any(|e| matches!(
        e,
        UiEvent::ApprovalResolved { decision, .. } if decision == "denied"
    )));

    let Message::System { content } = llm
        .last_request()
        .messages
        .into_iter()
        .find(|m| matches!(m, Message::System { .. }))
        .unwrap()
    else {
        unreachable!()
    };
    assert!(content.contains("denied"), "got: {content}");
}

/// A path that leaves the workspace is refused, the model is told, and nothing is
/// recorded — a receipt for a write that did not happen would be a lie.
#[tokio::test]
async fn escaping_the_workspace_fails_loudly_and_silently() {
    let dir = scratch("escape");
    let ws = LocalWorkspace::open(dir.join("workspace")).unwrap();
    let chain = Chain::open(dir.join("journal.jsonl")).unwrap();

    let llm = ScriptedLlm::new(vec![
        vec![tool("t1", "fs_write", serde_json::json!({
            "path": "../../escaped.txt", "contents": "pwned"
        }))],
        vec![text("Refused, as expected.")],
    ]);
    let tap = ScriptedTap::empty();
    let sink = Collector::default();

    let mut run = Run::new(&llm, &tap, &ws, chain, &sink, Capabilities::FILES, stamp);
    run.start("escape").await.unwrap();
    while run.step(SYSTEM, 1000).await.unwrap() {}

    assert!(sink.events().iter().any(|e| matches!(
        e,
        UiEvent::ToolFinished { is_error: true, .. }
    )));
    assert!(!dir.join("escaped.txt").exists());
    assert!(!dir.parent().unwrap().join("escaped.txt").exists());
    assert!(
        !run.chain().receipts().iter().any(|r| matches!(r.event, Event::SandboxCall { .. })),
        "nothing was written, so nothing may be recorded"
    );
}

/// TAP refusing a call is information, not a crash. The agent must get the error
/// as a tool result and be able to carry on — a run that dies because one
/// credential was not allowed cannot recover, apologise, or try another route.
#[tokio::test]
async fn a_refused_credential_is_survivable() {
    let dir = scratch("tap-error");
    let chain = Chain::open(dir.join("journal.jsonl")).unwrap();

    let llm = ScriptedLlm::new(vec![
        vec![tool("t1", "tap_call", serde_json::json!({
            "credential": "not-mine", "target": "https://api.dune.com/v1/x", "method": "GET"
        }))],
        vec![text("That credential is not in my inventory.")],
    ]);
    // No scripted outcome: the proxy errors, exactly as a 403 would surface.
    let tap = ScriptedTap::empty();
    let sink = Collector::default();
    let none = NoSandbox;

    let mut run = Run::new(&llm, &tap, &none, chain, &sink, Capabilities::TAP_ONLY, stamp);
    run.start("call it").await.unwrap();
    while run.step(SYSTEM, 1000).await.unwrap() {}

    assert!(sink.events().iter().any(|e| matches!(
        e,
        UiEvent::ToolFinished { is_error: true, .. }
    )));
    assert_eq!(sink.answer(), "That credential is not in my inventory.");
    assert!(
        !run.chain().receipts().iter().any(|r| matches!(r.event, Event::TapCall { .. })),
        "the call never happened, so it must not be recorded"
    );
    run.chain().verify().unwrap();
}

/// A tool the model invents must not reach dispatch. Parsing refuses it first.
#[tokio::test]
async fn an_invented_tool_never_reaches_dispatch() {
    let dir = scratch("invented");
    let chain = Chain::open(dir.join("journal.jsonl")).unwrap();

    let llm = ScriptedLlm::new(vec![vec![tool(
        "t1",
        "web_fetch",
        serde_json::json!({ "url": "http://evil.example" }),
    )]]);
    let tap = ScriptedTap::empty();
    let sink = Collector::default();
    let none = NoSandbox;

    let mut run = Run::new(&llm, &tap, &none, chain, &sink, Capabilities::TAP_ONLY, stamp);
    run.start("fetch").await.unwrap();

    assert!(run.step(SYSTEM, 1000).await.is_err());
    assert!(tap.calls.lock().unwrap().is_empty());
}

/// A chat is several runs sharing one conversation. The second must see the
/// first, and both must land in the same chain.
#[tokio::test]
async fn a_resumed_chat_keeps_its_conversation_and_its_chain() {
    let dir = scratch("resume");
    let journal = dir.join("journal.jsonl");

    let first = {
        let ws = LocalWorkspace::open(dir.join("workspace")).unwrap();
        let llm = ScriptedLlm::new(vec![vec![text("Noted.")]]);
        let tap = ScriptedTap::empty();
        let sink = Collector::default();
        let mut run = Run::new(
            &llm,
            &tap,
            &ws,
            Chain::open(&journal).unwrap(),
            &sink,
            Capabilities::FILES,
            stamp,
        );
        run.start("remember the number 41").await.unwrap();
        while run.step(SYSTEM, 1000).await.unwrap() {}
        run.messages().to_vec()
    };
    assert_eq!(first.len(), 2, "one user turn and one assistant turn");

    let ws = LocalWorkspace::open(dir.join("workspace")).unwrap();
    let llm = ScriptedLlm::new(vec![vec![text("41.")]]);
    let tap = ScriptedTap::empty();
    let sink = Collector::default();
    let mut run = Run::new(
        &llm,
        &tap,
        &ws,
        Chain::open(&journal).unwrap(),
        &sink,
        Capabilities::FILES,
        stamp,
    );
    run.resume(first);
    run.start("what number?").await.unwrap();
    while run.step(SYSTEM, 1000).await.unwrap() {}

    // The model saw the earlier exchange.
    let seen = llm.last_request().messages;
    assert!(seen.len() >= 3);
    let as_json = serde_json::to_string(&seen).unwrap();
    assert!(as_json.contains("remember the number 41"));

    // One chain across both runs, still consistent, with two openings in it.
    let chain = Chain::open(&journal).unwrap();
    chain.verify().unwrap();
    let openings = chain
        .receipts()
        .iter()
        .filter(|r| matches!(r.event, Event::RunStarted { .. }))
        .count();
    assert_eq!(openings, 2);
}

/// The chain is append-only and self-checking. Editing a receipt in place — the
/// lazy tamper — must be caught on the next open.
#[tokio::test]
async fn editing_the_journal_breaks_it() {
    let dir = scratch("tamper");
    let journal = dir.join("journal.jsonl");
    let ws = LocalWorkspace::open(dir.join("workspace")).unwrap();

    let llm = ScriptedLlm::new(vec![
        vec![tool("t1", "fs_write", serde_json::json!({ "path": "a.txt", "contents": "x" }))],
        vec![text("ok")],
    ]);
    let tap = ScriptedTap::empty();
    let sink = Collector::default();
    let mut run = Run::new(
        &llm,
        &tap,
        &ws,
        Chain::open(&journal).unwrap(),
        &sink,
        Capabilities::FILES,
        stamp,
    );
    run.start("write").await.unwrap();
    while run.step(SYSTEM, 1000).await.unwrap() {}

    Chain::open(&journal).unwrap();

    // Rewrite a recorded fact without recomputing the digest.
    let raw = std::fs::read_to_string(&journal).unwrap();
    std::fs::write(&journal, raw.replace("fs_write", "fs_read")).unwrap();

    assert!(
        Chain::open(&journal).is_err(),
        "a chain that opens after an edit is not a chain"
    );
}

/// Text arrives as it is generated, and the loop does not emit it a second time
/// when the turn closes.
#[tokio::test]
async fn streamed_text_is_not_repeated_at_the_end_of_a_turn() {
    let dir = scratch("stream");
    let chain = Chain::open(dir.join("journal.jsonl")).unwrap();

    let llm = ScriptedLlm::new(vec![vec![text("Hello "), text("world.")]]);
    let tap = ScriptedTap::empty();
    let sink = Collector::default();
    let none = NoSandbox;

    let mut run = Run::new(&llm, &tap, &none, chain, &sink, Capabilities::TAP_ONLY, stamp);
    run.start("greet").await.unwrap();
    while run.step(SYSTEM, 1000).await.unwrap() {}

    assert_eq!(sink.answer(), "Hello world.");
    assert!(
        !sink.kinds().contains(&"assistant_text".to_string()),
        "the finished block would print the answer twice"
    );
}

/// The system prompt states the run's real tier, and the inventory is the
/// credentials TAP reports rather than anything the model may believe.
#[tokio::test]
async fn the_prompt_states_the_tier_and_the_real_inventory() {
    let tap = ScriptedTap::empty();
    let creds = {
        use locked_core::Forwarder;
        tap.discover().await.unwrap()
    };

    let container = locked_core::prompt::system(
        &Isolation::Container { image: "python:3.12-slim".into() },
        &creds,
    );
    assert!(container.contains("no network stack at all"));
    assert!(container.contains("dune"));
    assert!(container.contains("writes pause for a human"));

    let workspace = locked_core::prompt::system(&Isolation::Workspace, &creds);
    assert!(workspace.contains("There is no shell on this run"));
    assert!(!workspace.contains("no network stack at all"));

    let bare = locked_core::prompt::system(&Isolation::None, &[]);
    assert!(bare.contains("no shell"));
    assert!(
        bare.contains("no credentials at all"),
        "an empty inventory must be stated, not omitted"
    );
}
