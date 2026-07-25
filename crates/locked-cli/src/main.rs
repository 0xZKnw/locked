//! Wiring. This binary sees both sides and implements neither.
//!
//! It is the second and last crate allowed to reach an HTTP stack, and only
//! because it constructs the clients that `locked-egress` defines. Keep it
//! boring: anything with a decision in it belongs in `locked-core`.

use locked_core::{EventSink, Run, UiEvent};
use locked_egress::{DirectLlm, LlmProvider, TapForwarder};
use locked_journal::Chain;
use locked_sandbox::SurfaceHint;
use locked_tools::Capabilities;

/// Prints as it goes. The Tauri window will consume the same event stream — core
/// makes no assumption about who is rendering.
struct StdoutSink;

impl EventSink for StdoutSink {
    fn emit(&self, event: UiEvent) {
        match event {
            UiEvent::TurnStarted { turn } => println!("\n\x1b[2m── turn {turn} ──\x1b[0m"),
            // Streamed: written as it arrives, so the terminal shows the answer
            // forming rather than appearing all at once.
            UiEvent::AssistantDelta { text } => {
                print!("{text}");
                let _ = std::io::Write::flush(&mut std::io::stdout());
            }
            UiEvent::ThinkingDelta { .. } => {}
            UiEvent::AssistantText { text } => println!("{text}"),
            UiEvent::ToolStarted { name, summary } => println!("  \x1b[36m→\x1b[0m {name}  \x1b[2m{summary}\x1b[0m"),
            UiEvent::ToolFinished { name, is_error } => println!(
                "  {} {name}",
                if is_error { "\x1b[31m✗\x1b[0m" } else { "\x1b[32m✓\x1b[0m" }
            ),
            UiEvent::ApprovalPending { txn_id, .. } => {
                println!("  \x1b[33m⏳ waiting on a human — {txn_id}\x1b[0m \x1b[2m(the run continues)\x1b[0m")
            }
            UiEvent::ApprovalResolved { txn_id, decision } => {
                println!("  \x1b[33m⏳\x1b[0m {txn_id} → {decision}")
            }
            UiEvent::ReceiptAppended { receipt } => {
                // The two evidence tiers are rendered differently on purpose:
                // a reader must never have to guess which receipts a third party
                // could corroborate.
                let (mark, label) = match &receipt.evidence {
                    locked_journal::Evidence::TapAttested { txn_id } => {
                        ("\x1b[32m■\x1b[0m", format!("tap:{txn_id}"))
                    }
                    locked_journal::Evidence::SourceAttested { scheme, id } => {
                        ("\x1b[36m■\x1b[0m", format!("{scheme}:{id}"))
                    }
                    locked_journal::Evidence::HarnessAttested => {
                        ("\x1b[2m□\x1b[0m", "harness only".into())
                    }
                };
                println!(
                    "  {mark} \x1b[2m#{:<3} {:<14} {}\x1b[0m",
                    receipt.seq,
                    label,
                    &receipt.digest[..23.min(receipt.digest.len())]
                );
            }
            UiEvent::RunFinished { turns } => println!("\n\x1b[2m── done in {turns} turns ──\x1b[0m"),
        }
    }
}

fn now() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".into())
}

/// TAP key: the environment first, then `~/.tap/agent.json` — the same order the
/// official helper uses, so Locked works on a machine already set up for TAP
/// without asking for the key a second time.
///
/// The value may be several comma-separated keys; `/forward` accepts that and
/// resolves the owning key per credential, so it is passed through untouched.
fn tap_key() -> Result<String, String> {
    if let Ok(k) = std::env::var("TAP_API_KEY") {
        return Ok(k);
    }
    let path = home()?.join(".tap").join("agent.json");
    let raw = std::fs::read_to_string(&path)
        .map_err(|_| format!("set TAP_API_KEY, or put one in {}", path.display()))?;
    serde_json::from_str::<serde_json::Value>(&raw)
        .ok()
        .and_then(|v| v["api_key"].as_str().map(str::to_string))
        .ok_or_else(|| format!("{} has no \"api_key\"", path.display()))
}

/// Model-provider key. Falls back to the well-known variable for the chosen
/// provider so an already-configured machine needs no new setup.
fn llm_key(provider: LlmProvider) -> Result<String, String> {
    let fallback = match provider {
        LlmProvider::Kimi => "KIMI_API_KEY",
        LlmProvider::Anthropic => "ANTHROPIC_API_KEY",
    };
    std::env::var("LOCKED_LLM_KEY")
        .or_else(|_| std::env::var(fallback))
        .map_err(|_| format!("set LOCKED_LLM_KEY or {fallback}"))
}

/// Load `.env` from the working directory into the process environment.
///
/// The file **wins over an already-set variable**, which is not the usual dotenv
/// convention — but a stale exported key silently shadowing the file is exactly
/// the failure this project has already hit once, and it costs an hour every
/// time. The source of each key is printed at startup so the override is never
/// a surprise.
///
/// Returns the names it set, for that startup line. Values are never printed.
fn load_dotenv() -> Vec<String> {
    let Ok(raw) = std::fs::read_to_string(".env") else {
        return Vec::new();
    };
    let mut loaded = Vec::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim().trim_matches('"').trim_matches('\'');
        if key.is_empty() || value.is_empty() {
            continue;
        }
        // SAFETY: single-threaded startup, before any task is spawned.
        unsafe { std::env::set_var(key, value) };
        loaded.push(key.to_string());
    }
    loaded
}

fn home() -> Result<std::path::PathBuf, String> {
    std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .map(std::path::PathBuf::from)
        .map_err(|_| "cannot locate the home directory".to_string())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let task = match std::env::args().nth(1) {
        Some(t) => t,
        None => {
            eprintln!("usage: locked \"<task>\"\n");
            eprintln!("  TAP_API_KEY       or ~/.tap/agent.json");
            eprintln!("  LOCKED_LLM_KEY   or KIMI_API_KEY / ANTHROPIC_API_KEY");
            eprintln!("  LOCKED_PROVIDER  kimi | anthropic          (default: kimi)");
            eprintln!("  LOCKED_MODEL     (default: kimi-for-coding)");
            eprintln!("  LOCKED_IMAGE     sandbox image; omit or set NONE to run without one");
            eprintln!("  LOCKED_LLM_VIA_TAP  set to route inference through TAP too");
            std::process::exit(2);
        }
    };

    let from_dotenv = load_dotenv();
    if !from_dotenv.is_empty() {
        println!("\x1b[2m.env → {}\x1b[0m", from_dotenv.join(", "));
    }

    let run_id = std::process::id().to_string();
    let root = std::env::current_dir()?.join(".locked");
    std::fs::create_dir_all(&root)?;

    // The journal lives OUTSIDE the workspace the sandbox mounts. That placement
    // is the actual defence against an agent rewriting its own history — the hash
    // chain only makes tampering visible.
    let chain = Chain::open(root.join("journal.jsonl"))?;

    // A run without a sandbox is not a degraded run: it is a run whose agent was
    // never offered a shell. The tool list shrinks to match, so the receipts state
    // exactly what this run could do.
    // The strongest sandbox this machine can actually provide, and the tool
    // surface that matches it. Chosen together: a tier and a capability set that
    // disagree is how a run ends up offering something nobody is enforcing.
    let (sandbox, surface) = locked_sandbox::open_best(root.join("workspaces").join(&run_id), &run_id)
        .await
        .map_err(|e| format!("{e} — set LOCKED_SANDBOX=workspace to run without a container"))?;
    let caps = match surface {
        SurfaceHint::TapOnly => Capabilities::TAP_ONLY,
        SurfaceHint::Files => Capabilities::FILES,
        SurfaceHint::Full => Capabilities::FULL,
    };
    let isolation = sandbox.isolation();

    let tap = TapForwarder::new(tap_key()?)?;

    // One discovery, then the prompt states what this run holds instead of
    // leaving the model to guess. Same prompt the window uses — it lives in core.
    let creds = {
        use locked_core::Forwarder;
        tap.discover().await.unwrap_or_default()
    };
    let system = locked_core::prompt::system(&isolation, &creds);

    let provider = match std::env::var("LOCKED_PROVIDER").as_deref() {
        Ok("anthropic") => LlmProvider::Anthropic,
        _ => LlmProvider::Kimi,
    };
    let model = std::env::var("LOCKED_MODEL").unwrap_or_else(|_| "k3".into());

    // Two doors, and the choice is explicit rather than inferred.
    //
    // `LOCKED_LLM_VIA_TAP` routes inference through TAP as well, which restores
    // the full invariant — one door, no key held locally, inferences inside the
    // receipt chain. It requires the credential to auto-approve the inference
    // route, and it inherits TAP's 30-second ceiling, so it suits short turns.
    let via_tap = std::env::var("LOCKED_LLM_VIA_TAP").is_ok();
    let direct;
    let through_tap;
    let llm: &dyn locked_core::LlmTransport = if via_tap {
        through_tap = locked_egress::TapLlm {
            forwarder: &tap,
            credential: std::env::var("LOCKED_LLM_CREDENTIAL")
                .unwrap_or_else(|_| "kimi".into()),
            provider,
            model: model.clone(),
        };
        &through_tap
    } else {
        direct = DirectLlm::new(provider, model.clone(), llm_key(provider)?)?;
        &direct
    };

    println!(
        "\x1b[2mlocked — {} tools, isolation: {}\x1b[0m",
        locked_tools::tool_specs(caps).len(),
        isolation.label()
    );

    let sink = StdoutSink;
    let mut run = Run::new(llm, &tap, sandbox.as_ref(), chain, &sink, caps, now);
    run.start(&task).await?;

    let mut turns = 0;
    while run.step(&system, 8_000).await? {
        turns += 1;
        if turns > 40 {
            eprintln!("turn cap reached");
            break;
        }
    }

    // Otherwise every invocation leaves a `sleep infinity` container behind.
    sandbox.shutdown().await;

    let verified = run.chain().verify()?;
    println!(
        "\nchain verified: {verified} receipts, head {}",
        run.chain().head()
    );
    println!("journal: {}", root.join("journal.jsonl").display());
    Ok(())
}
