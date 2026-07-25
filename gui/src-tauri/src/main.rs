#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! The window's backend. Wiring only, exactly like `airlock-cli`.
//!
//! It runs the same `airlock_core::Run` and forwards the same `UiEvent` stream —
//! the loop has no idea a window exists. Swapping the front end for a TUI, or
//! running headless, changes nothing here or in core.

use airlock_core::{EventSink, Message, Run, UiEvent};
use airlock_egress::{DirectLlm, LlmProvider, TapForwarder};
use airlock_journal::Chain;
use airlock_sandbox::{Isolation, SurfaceHint};
use airlock_tools::Capabilities;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Emitter, Manager};

const EVENT: &str = "airlock://event";

/// Forwards core's events into the webview. Emit failures are dropped on purpose:
/// a closed window must never take the run down with it.
struct WindowSink(AppHandle);

impl EventSink for WindowSink {
    fn emit(&self, event: UiEvent) {
        let _ = self.0.emit(EVENT, event);
    }
}

/// Anything the window needs that is not a `UiEvent` — startup facts and
/// terminal states. Same envelope so the front end has one listener.
#[derive(Serialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ShellEvent {
    RunConfig {
        tools: Vec<String>,
        sandbox: Option<String>,
        integrity: String,
        journal: String,
        session: String,
        model: String,
        provider: String,
        context_window: u64,
    },
    RunFailed { message: String },
    ChainVerified { receipts: u64, head: String },
}

fn emit_shell(app: &AppHandle, event: ShellEvent) {
    let _ = app.emit(EVENT, event);
}

fn now() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".into())
}

fn load_dotenv(root: &Path) {
    let Ok(raw) = std::fs::read_to_string(root.join(".env")) else {
        return;
    };
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let (k, v) = (k.trim(), v.trim().trim_matches('"').trim_matches('\''));
            if !k.is_empty() && !v.is_empty() {
                // SAFETY: startup, before any task is spawned.
                unsafe { std::env::set_var(k, v) };
            }
        }
    }
}

fn tap_key() -> Result<String, String> {
    if let Ok(k) = std::env::var("TAP_API_KEY") {
        return Ok(k);
    }
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .map_err(|_| "cannot locate the home directory".to_string())?;
    let path = PathBuf::from(home).join(".tap").join("agent.json");
    let raw = std::fs::read_to_string(&path)
        .map_err(|_| format!("set TAP_API_KEY, or put one in {}", path.display()))?;
    serde_json::from_str::<serde_json::Value>(&raw)
        .ok()
        .and_then(|v| v["api_key"].as_str().map(str::to_string))
        .ok_or_else(|| format!("{} has no \"api_key\"", path.display()))
}

fn provider() -> LlmProvider {
    match std::env::var("AIRLOCK_PROVIDER").as_deref() {
        Ok("anthropic") => LlmProvider::Anthropic,
        _ => LlmProvider::Kimi,
    }
}

fn provider_name(p: LlmProvider) -> &'static str {
    match p {
        LlmProvider::Kimi => "kimi",
        LlmProvider::Anthropic => "anthropic",
    }
}

fn model_name() -> String {
    std::env::var("AIRLOCK_MODEL").unwrap_or_else(|_| "k3".into())
}

/// How much context the model will take.
///
/// Asked of the provider first — Kimi's model list publishes `context_length`,
/// so the number under the gauge is reported rather than invented. The table is
/// only the fallback for a provider that doesn't say, and the env var overrides
/// both. A meter whose denominator you cannot check is decoration.
async fn context_window(llm: &DirectLlm, model: &str) -> u64 {
    if let Ok(n) = std::env::var("AIRLOCK_CONTEXT_WINDOW").unwrap_or_default().parse() {
        return n;
    }
    if let Some(n) = llm.context_length().await {
        return n;
    }
    let m = model.to_ascii_lowercase();
    if m.contains("claude") || m.contains("sonnet") || m.contains("opus") {
        200_000
    } else {
        262_144
    }
}

fn llm_key(p: LlmProvider) -> Result<String, String> {
    let fallback = match p {
        LlmProvider::Kimi => "KIMI_API_KEY",
        LlmProvider::Anthropic => "ANTHROPIC_API_KEY",
    };
    std::env::var("AIRLOCK_LLM_KEY")
        .or_else(|_| std::env::var(fallback))
        .map_err(|_| format!("set AIRLOCK_LLM_KEY in .env, or {fallback}"))
}

/// The project root — where `.env` and `.airlock/` live.
///
/// In dev the binary sits under `target/debug`, so walk up until a `.env` or a
/// `.airlock` shows up before falling back to the working directory.
fn project_root() -> PathBuf {
    let mut dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let start = dir.clone();
    for _ in 0..6 {
        if dir.join(".env").exists() || dir.join(".airlock").exists() {
            return dir;
        }
        if !dir.pop() {
            break;
        }
    }
    start
}

// ---------------------------------------------------------------------------
// Sessions
//
// A chat and its receipt chain are the same object, so a session owns its own
// journal: `.airlock/sessions/<id>/journal.jsonl`. Opening an old chat opens the
// chain that belongs to it, and "verify" means something specific rather than
// "verify everything this machine has ever done".
//
// `messages.json` sits beside it and holds the actual conversation. That is a
// deliberate separation: the chain stores digests of every prompt and response
// and nothing readable, so the transcript is a convenience file next to the
// audit trail — never part of the attested record.
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone)]
struct SessionMeta {
    id: String,
    title: String,
    created: String,
    updated: String,
    turns: u32,
    receipts: u64,
}

#[derive(Serialize)]
struct SessionView {
    meta: SessionMeta,
    receipts: Vec<airlock_journal::Receipt>,
    messages: Vec<Message>,
}

fn sessions_dir() -> PathBuf {
    project_root().join(".airlock").join("sessions")
}

/// Ids arrive from the webview, and a front end is not a trust boundary: without
/// this, `../../` in an id would let the window read and delete arbitrary paths.
fn session_dir(id: &str) -> Result<PathBuf, String> {
    let ok = !id.is_empty()
        && id.len() <= 64
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    if !ok {
        return Err(format!("not a session id: {id}"));
    }
    Ok(sessions_dir().join(id))
}

fn read_meta(dir: &Path) -> Option<SessionMeta> {
    serde_json::from_str(&std::fs::read_to_string(dir.join("meta.json")).ok()?).ok()
}

fn write_meta(dir: &Path, meta: &SessionMeta) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(meta).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("meta.json"), bytes).map_err(|e| e.to_string())
}

/// The first thing asked, trimmed to something that fits a rail.
fn title_from(task: &str) -> String {
    let one_line: String = task.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= 52 {
        return one_line;
    }
    one_line.chars().take(51).collect::<String>().trim_end().to_string() + "…"
}

#[tauri::command]
fn list_sessions() -> Result<Vec<SessionMeta>, String> {
    let dir = sessions_dir();
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out: Vec<SessionMeta> = std::fs::read_dir(&dir)
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir())
        .filter_map(|e| read_meta(&e.path()))
        .collect();
    out.sort_by(|a, b| b.updated.cmp(&a.updated));
    Ok(out)
}

#[tauri::command]
fn create_session() -> Result<SessionMeta, String> {
    // Nanoseconds, so two clicks in the same second cannot collide.
    let id = format!("s{}", time::OffsetDateTime::now_utc().unix_timestamp_nanos());
    let dir = session_dir(&id)?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let meta = SessionMeta {
        id,
        title: String::new(),
        created: now(),
        updated: now(),
        turns: 0,
        receipts: 0,
    };
    write_meta(&dir, &meta)?;
    Ok(meta)
}

#[tauri::command]
fn load_session(id: String) -> Result<SessionView, String> {
    let dir = session_dir(&id)?;
    let meta = read_meta(&dir).ok_or_else(|| format!("no session {id}"))?;

    let journal = dir.join("journal.jsonl");
    let receipts = if journal.exists() {
        Chain::open(&journal).map(|c| c.receipts().to_vec()).map_err(|e| e.to_string())?
    } else {
        Vec::new()
    };

    let messages = std::fs::read_to_string(dir.join("messages.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();

    Ok(SessionView { meta, receipts, messages })
}

/// Ask TAP where a pending write stands, and record the answer.
///
/// The loop reconciles approvals at the top of its next turn, which is the right
/// place while a run is going. But a write approved *after* the run ended has no
/// next turn to be noticed in, so without this the window shows it waiting
/// forever — and, worse, the decision a human actually made never reaches the
/// chain. It is the one receipt tier a third party can corroborate; losing it
/// because of when the human happened to tap approve is not acceptable.
///
/// The caller must not do this while the same session is running: the loop holds
/// its own `Chain` and computes `prev` from it, so two writers would fork the
/// links. The window enforces that; this is not safe to call concurrently.
#[tauri::command]
async fn check_approval(session: String, txn_id: String) -> Result<String, String> {
    use airlock_core::{ApprovalState, Forwarder};

    let dir = session_dir(&session)?;
    let tap = TapForwarder::new(tap_key()?).map_err(|e| e.to_string())?;

    let decision = match tap.poll(&txn_id).await.map_err(|e| e.to_string())? {
        ApprovalState::Pending => return Ok("pending".into()),
        ApprovalState::Forwarded { .. } => "approved",
        ApprovalState::Denied => "denied",
        ApprovalState::Failed { .. } => "error",
    };

    let journal = dir.join("journal.jsonl");
    let mut chain = Chain::open(&journal).map_err(|e| e.to_string())?;

    let already = chain.receipts().iter().any(|r| {
        matches!(&r.event, airlock_journal::Event::ApprovalResolved { txn_id: t, .. } if *t == txn_id)
    });
    if !already {
        chain
            .append(
                airlock_journal::Event::ApprovalResolved {
                    txn_id: txn_id.clone(),
                    decision: decision.to_string(),
                },
                airlock_journal::Evidence::TapAttested { txn_id },
                now(),
            )
            .map_err(|e| e.to_string())?;
    }

    Ok(decision.into())
}

#[tauri::command]
fn delete_session(id: String) -> Result<(), String> {
    let dir = session_dir(&id)?;
    if !dir.exists() {
        return Ok(());
    }
    std::fs::remove_dir_all(&dir).map_err(|e| e.to_string())
}

#[tauri::command]
async fn list_capabilities() -> Result<Vec<airlock_core::CredentialInfo>, String> {
    use airlock_core::Forwarder;
    let tap = TapForwarder::new(tap_key()?).map_err(|e| e.to_string())?;
    tap.discover().await.map_err(|e| e.to_string())
}

/// The tool surface a sandbox tier can honour. Kept next to the tier so the two
/// can never drift apart.
fn caps_for(surface: SurfaceHint) -> Capabilities {
    match surface {
        SurfaceHint::TapOnly => Capabilities::TAP_ONLY,
        SurfaceHint::Files => Capabilities::FILES,
        SurfaceHint::Full => Capabilities::FULL,
    }
}

/// What the header shows. `None` renders as "no sandbox".
fn isolation_label(isolation: &Isolation) -> Option<String> {
    match isolation {
        Isolation::Container { image } => Some(image.clone()),
        Isolation::Workspace => Some("workspace".into()),
        Isolation::None => None,
    }
}

fn surface_label(surface: SurfaceHint) -> Option<String> {
    match surface {
        SurfaceHint::Full => Some(
            std::env::var("AIRLOCK_IMAGE")
                .unwrap_or_else(|_| airlock_sandbox::DEFAULT_IMAGE.into()),
        ),
        SurfaceHint::Files => Some("workspace".into()),
        SurfaceHint::TapOnly => None,
    }
}

/// What tier a run would get, without starting one.
async fn probe_surface() -> SurfaceHint {
    let image = std::env::var("AIRLOCK_IMAGE").unwrap_or_default();
    match std::env::var("AIRLOCK_SANDBOX")
        .unwrap_or_else(|_| if image == "NONE" { "none".into() } else { "auto".into() })
        .as_str()
    {
        "none" => SurfaceHint::TapOnly,
        "workspace" => SurfaceHint::Files,
        "container" => SurfaceHint::Full,
        _ if airlock_sandbox::container_runtime_available().await => SurfaceHint::Full,
        _ => SurfaceHint::Files,
    }
}

/// What this window would run, without running it.
///
/// The header states the model and the context window from the moment the app
/// opens rather than after the first answer — a harness that cannot say what it
/// is about to use has no business claiming to be auditable. It fails loudly if
/// the run is not configured, for the same reason.
#[tauri::command]
async fn describe_run(session: String) -> Result<ShellEvent, String> {
    // What a run started right now would get. Asking the runtime is the only
    // honest answer — an image name in the environment says nothing about
    // whether the daemon is up.
    let surface = probe_surface().await;
    let caps = caps_for(surface);

    let p = provider();
    let model = model_name();
    let llm = DirectLlm::new(p, model.clone(), llm_key(p)?).map_err(|e| e.to_string())?;

    Ok(ShellEvent::RunConfig {
        tools: airlock_tools::tool_specs(caps).iter().map(|t| t.name.to_string()).collect(),
        sandbox: surface_label(surface),
        integrity: match airlock_core::LlmTransport::integrity(&llm) {
            airlock_journal::Integrity::Full => "full".into(),
            airlock_journal::Integrity::Degraded { reason } => reason,
        },
        journal: session_dir(&session)?.join("journal.jsonl").display().to_string(),
        session,
        context_window: context_window(&llm, &model).await,
        provider: provider_name(p).to_string(),
        model,
    })
}

// ---------------------------------------------------------------------------
// The canvas
//
// The model can write a page and have it run. Three things make that safe rather
// than reckless, and all three are structural:
//
//  1. It is served from its own scheme, not from `srcdoc`. A `srcdoc` frame
//     *inherits the parent's CSP*, so the model's inline script would be blocked
//     by the app's `script-src 'self'` — and relaxing that to make the canvas
//     work would weaken the whole window. A response with its own CSP header
//     does not inherit, so the canvas gets exactly the permissions it needs and
//     the app keeps its own.
//
//  2. That CSP is `default-src 'none'` plus inline script and style. There is no
//     `connect-src`, so fetch, XHR and WebSockets all fail inside the frame. The
//     canvas can compute and draw; it cannot call out. Same trade as the
//     sandbox tier — keep the guarantee, drop the capability.
//
//  3. The frame is sandboxed without `allow-same-origin`, which gives it an
//     opaque origin. It cannot reach the app's DOM, its storage, or the Tauri
//     bridge — which is the part that matters, since that bridge can delete
//     chats.
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Canvases(std::sync::Mutex<std::collections::HashMap<String, String>>);

const CANVAS_CSP: &str = "default-src 'none'; \
script-src 'unsafe-inline'; style-src 'unsafe-inline'; \
img-src data: blob:; font-src data:; media-src data: blob:; form-action 'none'; base-uri 'none'";

/// Build the response for a staged page.
///
/// Split out from the protocol handler so the guarantee can be asserted rather
/// than eyeballed: the header this returns is the entire reason the canvas is
/// safe to run, and a silent edit to it would be invisible.
fn canvas_response(body: Option<String>) -> tauri::http::Response<Vec<u8>> {
    match body {
        Some(html) => tauri::http::Response::builder()
            .header("Content-Type", "text/html; charset=utf-8")
            // Its own policy, so the frame does not inherit the app's.
            .header("Content-Security-Policy", CANVAS_CSP)
            .header("Cache-Control", "no-store")
            .body(html.into_bytes())
            .expect("static response"),
        None => tauri::http::Response::builder()
            .status(404)
            .body(Vec::new())
            .expect("static response"),
    }
}

/// Stage a page and hand back the URL that serves it.
///
/// The URL is built here rather than guessed in the window: Windows serves a
/// custom scheme as `http://<scheme>.localhost`, everything else as
/// `<scheme>://localhost`, and getting that wrong is a blank frame with no error.
#[tauri::command]
fn stage_canvas(html: String, canvases: tauri::State<'_, Canvases>) -> Result<String, String> {
    if html.len() > 2 * 1024 * 1024 {
        return Err("that page is too large to render".into());
    }
    let id = format!("c{}", time::OffsetDateTime::now_utc().unix_timestamp_nanos());
    canvases.0.lock().map_err(|_| "canvas store poisoned")?.insert(id.clone(), html);

    Ok(if cfg!(windows) {
        format!("http://canvas.localhost/{id}")
    } else {
        format!("canvas://localhost/{id}")
    })
}

/// What the window may attach to a turn.
///
/// The webview is not a trust boundary, so these are checked here rather than
/// taken on faith: an unbounded paste would be sent verbatim to the provider and
/// billed, and an arbitrary `media_type` is a string this process would otherwise
/// forward into someone else's parser.
const IMAGE_TYPES: [&str; 4] = ["image/png", "image/jpeg", "image/gif", "image/webp"];
const MAX_IMAGE_BYTES: usize = 5 * 1024 * 1024;
const MAX_IMAGES: usize = 8;

fn check_images(images: &[airlock_core::Image]) -> Result<(), String> {
    if images.len() > MAX_IMAGES {
        return Err(format!("{} images is more than one turn can carry (max {MAX_IMAGES})", images.len()));
    }
    for img in images {
        if !IMAGE_TYPES.contains(&img.media_type.as_str()) {
            return Err(format!(
                "{} is not an image type this run will send ({})",
                img.media_type,
                IMAGE_TYPES.join(", ")
            ));
        }
        // base64 is 4 characters per 3 bytes; close enough to reject the absurd.
        let bytes = img.data.len() / 4 * 3;
        if bytes > MAX_IMAGE_BYTES {
            return Err(format!(
                "an image of about {} MB is over the {} MB limit",
                bytes / 1_048_576,
                MAX_IMAGE_BYTES / 1_048_576
            ));
        }
        if img.data.is_empty() {
            return Err("an attached image has no data".into());
        }
    }
    Ok(())
}

#[tauri::command]
async fn start_run(
    app: AppHandle,
    task: String,
    session: String,
    images: Option<Vec<airlock_core::Image>>,
) {
    let images = images.unwrap_or_default();
    if let Err(message) = check_images(&images) {
        emit_shell(&app, ShellEvent::RunFailed { message });
        return;
    }
    tokio::spawn(async move {
        if let Err(message) = run(&app, task, session, images).await {
            emit_shell(&app, ShellEvent::RunFailed { message });
        }
    });
}

async fn run(
    app: &AppHandle,
    task: String,
    session: String,
    images: Vec<airlock_core::Image>,
) -> Result<(), String> {
    let airlock_dir = project_root().join(".airlock");
    std::fs::create_dir_all(&airlock_dir).map_err(|e| e.to_string())?;

    let dir = session_dir(&session)?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    let journal_path = dir.join("journal.jsonl");
    let chain = Chain::open(&journal_path).map_err(|e| e.to_string())?;

    // Everything said so far in this chat. Absent on the first prompt.
    let prior: Vec<Message> = std::fs::read_to_string(dir.join("messages.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();

    // The workspace belongs to the chat, not to the prompt. A fresh empty
    // directory per run would mean a file written in one message is gone by the
    // next — the same reasoning that gives a session its own journal.
    let (sandbox, surface) = airlock_sandbox::open_best(dir.join("workspace"), &session)
        .await
        .map_err(|e| format!("{e} — set AIRLOCK_SANDBOX=workspace in .env to run without a container"))?;
    let caps = caps_for(surface);
    let isolation = sandbox.isolation();
    let tap = TapForwarder::new(tap_key()?).map_err(|e| e.to_string())?;

    // One discovery per run. If TAP is unreachable the run should still start
    // and fail honestly at the first call, rather than refuse to begin.
    let creds = {
        use airlock_core::Forwarder;
        tap.discover().await.unwrap_or_default()
    };

    let system = airlock_core::prompt::system(&isolation, &creds);

    let p = provider();
    let model = model_name();
    let llm = DirectLlm::new(p, model.clone(), llm_key(p)?).map_err(|e| e.to_string())?;

    emit_shell(
        app,
        ShellEvent::RunConfig {
            tools: airlock_tools::tool_specs(caps)
                .iter()
                .map(|t| t.name.to_string())
                .collect(),
            sandbox: isolation_label(&isolation),
            integrity: match airlock_core::LlmTransport::integrity(&llm) {
                airlock_journal::Integrity::Full => "full".into(),
                airlock_journal::Integrity::Degraded { reason } => reason,
            },
            journal: journal_path.display().to_string(),
            session: session.clone(),
            context_window: context_window(&llm, &model).await,
            provider: provider_name(p).to_string(),
            model,
        },
    );

    let sink = WindowSink(app.clone());
    let mut run = Run::new(&llm, &tap, sandbox.as_ref(), chain, &sink, caps, now);
    run.resume(prior);
    run.start_with(&task, &images).await.map_err(|e| e.to_string())?;

    // Titled from the first thing ever asked in this chat, so renaming a session
    // is never something you have to remember to do.
    let mut meta = read_meta(&dir).unwrap_or(SessionMeta {
        id: session.clone(),
        title: String::new(),
        created: now(),
        updated: now(),
        turns: 0,
        receipts: 0,
    });
    if meta.title.is_empty() {
        meta.title = title_from(&task);
    }

    let mut turns = 0;
    let outcome = loop {
        match run.step(&system, 8_000).await {
            Ok(true) => {
                turns += 1;
                // Persisted every turn rather than at the end: a run that fails
                // on turn nine should not cost you the eight that worked.
                save_transcript(&dir, run.messages())?;
                if turns > 40 {
                    break Err("turn cap reached".to_string());
                }
            }
            Ok(false) => break Ok(()),
            Err(e) => break Err(e.to_string()),
        }
    };

    save_transcript(&dir, run.messages())?;
    meta.updated = now();
    meta.turns += turns.max(1);
    meta.receipts = run.chain().receipts().len() as u64;
    write_meta(&dir, &meta)?;

    // Before the `?` below, so a failed run tears its container down too. The
    // workspace survives on the host; only the box goes.
    sandbox.shutdown().await;

    outcome?;

    let receipts = run.chain().verify().map_err(|e| e.to_string())?;
    emit_shell(
        app,
        ShellEvent::ChainVerified {
            receipts,
            head: run.chain().head().to_string(),
        },
    );
    Ok(())
}

fn save_transcript(dir: &Path, messages: &[Message]) -> Result<(), String> {
    let bytes = serde_json::to_vec(messages).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("messages.json"), bytes).map_err(|e| e.to_string())
}

fn main() {
    tauri::Builder::default()
        .manage(Canvases::default())
        .register_uri_scheme_protocol("canvas", |ctx, request| {
            let id = request.uri().path().trim_start_matches('/').to_string();
            let body = ctx
                .app_handle()
                .state::<Canvases>()
                .0
                .lock()
                .ok()
                .and_then(|m| m.get(&id).cloned());

            canvas_response(body)
        })
        .setup(|_app| {
            load_dotenv(&project_root());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            start_run,
            list_capabilities,
            describe_run,
            stage_canvas,
            list_sessions,
            create_session,
            load_session,
            check_approval,
            delete_session
        ])
        .run(tauri::generate_context!())
        .expect("error while running locked");
}

#[cfg(test)]
mod session_tests {
    use super::*;

    /// The webview supplies these ids. It is not a trust boundary, and a path
    /// built from an unvalidated id is a read and delete primitive over the whole
    /// disk — `load_session` and `delete_session` both take one.
    #[test]
    fn a_session_id_can_never_become_a_path() {
        assert!(session_dir("s1753441").is_ok());
        assert!(session_dir("s-123-abc").is_ok());

        for hostile in [
            "",
            "..",
            "../../etc",
            "..\\..\\Windows",
            "a/b",
            "a\\b",
            "C:",
            "s1\0",
            &"x".repeat(65),
        ] {
            assert!(
                session_dir(hostile).is_err(),
                "{hostile:?} was accepted as a session id"
            );
        }
    }

    /// A validated id must still land under the sessions directory and nowhere
    /// else — the check above is only worth something if the join agrees with it.
    #[test]
    fn a_valid_id_stays_under_the_sessions_directory() {
        let dir = session_dir("s42").unwrap();
        assert!(dir.starts_with(sessions_dir()));
        assert_eq!(dir.file_name().unwrap(), "s42");
    }

    #[test]
    fn a_title_is_one_line_and_fits_a_rail() {
        assert_eq!(title_from("short one"), "short one");
        assert_eq!(
            title_from("  ragged\n  whitespace \t everywhere  "),
            "ragged whitespace everywhere"
        );

        let long = title_from(&"word ".repeat(40));
        assert!(long.chars().count() <= 52, "got {} chars", long.chars().count());
        assert!(long.ends_with('…'));
        assert!(!long.contains('\n'));

        // Exactly at the limit is not truncated.
        let edge = "a".repeat(52);
        assert_eq!(title_from(&edge), edge);
    }

    /// Metadata round-trips through disk, because the rail reads it back on every
    /// launch and a silently unreadable file would show an empty history.
    #[test]
    fn session_metadata_round_trips() {
        let dir = std::env::temp_dir().join(format!("locked-meta-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let meta = SessionMeta {
            id: "s1".into(),
            title: "Check the analytics credentials".into(),
            created: "2026-07-25T00:00:00Z".into(),
            updated: "2026-07-25T00:01:00Z".into(),
            turns: 3,
            receipts: 12,
        };
        write_meta(&dir, &meta).unwrap();

        let back = read_meta(&dir).expect("meta must read back");
        assert_eq!(back.id, meta.id);
        assert_eq!(back.title, meta.title);
        assert_eq!(back.turns, 3);
        assert_eq!(back.receipts, 12);

        // A directory with no meta is skipped rather than fatal — that is what
        // keeps one corrupt session from emptying the whole rail.
        let empty = dir.join("nothing");
        std::fs::create_dir_all(&empty).unwrap();
        assert!(read_meta(&empty).is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The tier and the tool surface are chosen together on purpose. If they ever
    /// disagree, a run offers something no one is enforcing.
    #[test]
    fn a_tier_never_offers_more_than_it_enforces() {
        use airlock_tools::tool_specs;

        let full: Vec<_> = tool_specs(caps_for(SurfaceHint::Full)).iter().map(|t| t.name).collect();
        let files: Vec<_> = tool_specs(caps_for(SurfaceHint::Files)).iter().map(|t| t.name).collect();
        let tap: Vec<_> = tool_specs(caps_for(SurfaceHint::TapOnly)).iter().map(|t| t.name).collect();

        assert!(full.contains(&"exec"));
        assert!(!files.contains(&"exec"), "no container, no shell");
        assert!(files.contains(&"fs_write"));
        assert!(!tap.contains(&"fs_read"));

        // Each tier is a strict subset of the one above it.
        assert!(files.iter().all(|t| full.contains(t)));
        assert!(tap.iter().all(|t| files.contains(t)));
    }

    #[test]
    fn the_header_label_matches_the_tier() {
        assert_eq!(
            isolation_label(&Isolation::Container { image: "python:3.12-slim".into() }),
            Some("python:3.12-slim".into())
        );
        assert_eq!(isolation_label(&Isolation::Workspace), Some("workspace".into()));
        assert_eq!(isolation_label(&Isolation::None), None);
    }
}

#[cfg(test)]
mod canvas_tests {
    use super::*;

    /// The canvas's whole safety argument is this header. If it ever loosens, a
    /// page the model wrote gets a network — which is the one thing this project
    /// spends all its effort denying.
    #[test]
    fn a_canvas_is_served_with_no_way_out() {
        let res = canvas_response(Some("<b>hi</b>".into()));
        assert_eq!(res.status(), 200);

        let csp = res.headers()["Content-Security-Policy"].to_str().unwrap();
        assert!(csp.contains("default-src 'none'"), "everything is denied by default");
        assert!(
            !csp.contains("connect-src"),
            "no connect-src means fetch, XHR and WebSockets all fail — do not add one"
        );
        // Scripts and styles are what make it a canvas rather than a picture.
        assert!(csp.contains("script-src 'unsafe-inline'"));
        assert!(csp.contains("style-src 'unsafe-inline'"));
        // Nothing may be loaded from anywhere, only inlined.
        for remote in ["http:", "https:", "*"] {
            assert!(!csp.contains(remote), "{remote} would open a door");
        }
        assert!(csp.contains("form-action 'none'"));
        assert!(csp.contains("base-uri 'none'"));
    }

    #[test]
    fn an_unknown_canvas_is_a_404_not_a_blank_page() {
        assert_eq!(canvas_response(None).status(), 404);
    }

    /// The URL is built in Rust because the scheme is spelled differently per
    /// platform, and a wrong guess is a blank frame with no error to read.
    #[test]
    fn a_staged_canvas_is_addressable() {
        let store = Canvases::default();
        let url = {
            let map = store.0.lock().unwrap();
            drop(map);
            // Mirrors `stage_canvas` without needing Tauri's State wrapper.
            if cfg!(windows) { "http://canvas.localhost/c1" } else { "canvas://localhost/c1" }
        };
        assert!(url.contains("canvas"));
        assert!(url.ends_with("/c1"));
    }
}
