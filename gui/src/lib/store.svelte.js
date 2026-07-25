import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";

/**
 * The window is a pure consumer of the event stream `airlock-core` emits.
 * It holds no agent logic and makes no decisions — which is why swapping it for
 * a terminal front end, or running headless, changes nothing in the loop.
 */
export const app = $state({
  view: "transcript",
  timeline: [],
  receipts: [],
  approvals: [],
  capabilities: [],
  config: null,
  running: false,
  error: null,
  verified: null,
  task: "",
  /** Images waiting to go with the next message. */
  attachments: [],
  /** Every chat on disk, newest first. */
  sessions: [],
  /** The one being written to. Its journal is the chain shown under Receipts. */
  session: null,
});

/**
 * Tokens carried by the most recent inference.
 *
 * Read off the receipt chain rather than tracked separately: the number in the
 * gauge is the same one an auditor would recompute from the journal, which is
 * the only kind of number this app should be showing.
 */
export function lastUsage(receipts) {
  for (let i = receipts.length - 1; i >= 0; i--) {
    const r = receipts[i];
    if (r.event === "inference") {
      return { input: r.input_tokens ?? 0, output: r.output_tokens ?? 0 };
    }
  }
  return null;
}

/** Receipts a third party could corroborate, versus our own word. */
export function attestationOf(receipt) {
  switch (receipt.attestation) {
    case "tap_attested":
      return { tier: "tap", label: "TAP + human", detail: receipt.txn_id ?? "" };
    case "source_attested":
      return {
        tier: "source",
        label: "source",
        detail: `${receipt.scheme}:${receipt.id}`,
      };
    default:
      return { tier: "harness", label: "harness only", detail: "" };
  }
}

function push(item) {
  app.timeline.push(item);
}

/* ---------------------------------------------------------------------------
   Streaming, smoothed.

   The model does not arrive evenly: a whole sentence lands at once, then nothing
   for a few hundred milliseconds. Painted as it lands, that reads as text being
   slammed onto the page — and every burst reflows the markdown underneath it.

   So every delta goes into a buffer and is released at a steady character rate.
   The rate adapts to the backlog, so a long burst never takes longer to draw
   than it took to arrive: the display stays honest about how fast the model is
   actually going, it just stops stuttering.

   Blocks are tracked by index rather than by object identity — indices are
   stable under push, proxy identity is not something to rely on.
   --------------------------------------------------------------------------- */

const draining = new Set();
let frame = null;
let lastFrame = 0;

/** Characters per second below which streaming stops feeling live. */
const FLOOR_CPS = 340;

function feed(index, text) {
  const item = app.timeline[index];
  item.pending = (item.pending ?? "") + text;
  draining.add(index);
  if (frame === null) {
    lastFrame = performance.now();
    frame = requestAnimationFrame(pump);
  }
}

function pump(now) {
  // Clamp dt so a backgrounded window doesn't dump the whole buffer on return.
  const dt = Math.min(0.05, Math.max(0, (now - lastFrame) / 1000));
  lastFrame = now;

  for (const index of draining) {
    const item = app.timeline[index];
    const left = item.pending?.length ?? 0;

    if (left > 0) {
      const cps = Math.max(FLOOR_CPS, left * 7);
      const n = Math.max(1, Math.round(cps * dt));
      item.text += item.pending.slice(0, n);
      item.pending = item.pending.slice(n);
    }

    // Checked after the slice, not before it: the frame that lands the last
    // character is also the frame the block closes on. Testing it first left a
    // sealed block claiming to be open for one frame after it had finished.
    if (!item.pending) {
      if (item.sealed) item.open = false;
      draining.delete(index);
    }
  }

  frame = draining.size ? requestAnimationFrame(pump) : null;
}

/** Open a streaming block of `kind`, reusing the current one if it fits. */
function stream(kind, text, extra) {
  const index = app.timeline.length - 1;
  const last = app.timeline[index];
  if (last?.kind === kind && last.open) {
    feed(index, text);
    return;
  }
  // A block of a different kind means the previous one is over. Without this,
  // reasoning followed by an answer stays open forever and the transcript keeps
  // claiming the model is still thinking long after it has finished.
  seal();
  push({ kind, text: "", pending: "", open: true, sealed: false, ...extra });
  feed(app.timeline.length - 1, text);
}

/**
 * Close any streaming block so the next fragment starts a fresh one. A block
 * still holding buffered text is only marked: it closes when it runs dry, so
 * sealing never truncates or dumps what is left.
 */
function seal() {
  const last = app.timeline.at(-1);
  if (!last?.open) return;
  if (last.pending) last.sealed = true;
  else last.open = false;
}

function handle(e) {
  switch (e.type) {
    case "run_config":
      app.config = e;
      break;

    // A turn boundary closes whatever was streaming, but it is not shown: the
    // model's turn count is an implementation detail of the loop, and printing a
    // rule across the transcript every time was cutting the answer in half.
    case "turn_started":
      seal();
      break;

    // Streamed text. Fragments are appended to the open block rather than
    // creating one item per token, so the transcript stays a paragraph the user
    // can read instead of a wall of spans.
    case "assistant_delta":
      stream("text", e.text);
      break;

    case "thinking_delta":
      stream("thinking", e.text, { expanded: false });
      break;

    case "assistant_text":
      push({ kind: "text", text: e.text });
      break;

    case "tool_started":
      seal();
      push({ kind: "tool", name: e.name, summary: e.summary, status: "running" });
      break;

    case "tool_finished": {
      // Close the most recent open call with this name.
      for (let i = app.timeline.length - 1; i >= 0; i--) {
        const item = app.timeline[i];
        if (item.kind === "tool" && item.name === e.name && item.status === "running") {
          item.status = e.is_error ? "error" : "ok";
          break;
        }
      }
      break;
    }

    case "approval_pending":
      app.approvals.push({
        txn_id: e.txn_id,
        summary: e.summary,
        decision: null,
        at: new Date().toISOString(),
      });
      push({ kind: "approval", txn_id: e.txn_id });
      break;

    case "approval_resolved": {
      const pending = app.approvals.find((a) => a.txn_id === e.txn_id);
      if (pending) pending.decision = e.decision;
      push({ kind: "approval_resolved", txn_id: e.txn_id, decision: e.decision });
      break;
    }

    case "receipt_appended":
      app.receipts.push(e.receipt);
      break;

    // No end marker either. The composer coming back and the field coasting down
    // already say the run is over; a line of grey text repeating it is noise.
    case "run_finished":
      seal();
      app.running = false;
      // The session is titled from its first task, so the rail only learns the
      // name once the run that set it has finished writing.
      refreshSessions();
      // A write can be approved a minute after the run that asked for it ends.
      // Nothing would notice, because reconciliation is turn-driven and there is
      // no next turn — so the window takes over from here.
      refreshApprovals();
      break;

    case "chain_verified":
      app.verified = { receipts: e.receipts, head: e.head };
      break;

    case "run_failed":
      seal();
      app.running = false;
      app.error = e.message;
      refreshSessions();
      break;
  }
}

/**
 * Subscribing twice means every event is handled twice: two turn markers, and
 * two copies of each delta interleaved character by character. A hot reload or a
 * webview refresh re-runs `boot`, so the old subscription has to go first —
 * an event stream with no de-duplication cannot be joined more than once.
 */
let unlisten = null;
let bootSeq = 0;

export async function boot() {
  const seq = ++bootSeq;

  if (unlisten) {
    unlisten();
    unlisten = null;
  }
  draining.clear();
  if (frame !== null) {
    cancelAnimationFrame(frame);
    frame = null;
  }

  const off = await listen("airlock://event", (msg) => handle(msg.payload));

  // `listen` is async, so two boots can overlap: whoever started last wins and
  // the older subscription is dropped rather than left running alongside.
  if (seq !== bootSeq) {
    off();
    return;
  }
  unlisten = off;

  try {
    app.sessions = await invoke("list_sessions");
    // Reopen where you left off; a first launch starts a chat rather than
    // showing a window with nowhere to type.
    if (app.sessions.length) await openSession(app.sessions[0].id);
    else await newChat();
  } catch (e) {
    app.error = String(e);
  }
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

function clearRun() {
  draining.clear();
  if (frame !== null) {
    cancelAnimationFrame(frame);
    frame = null;
  }
  app.timeline = [];
  app.receipts = [];
  app.approvals = [];
  app.verified = null;
  app.error = null;
}

/**
 * Ask what the next run would use. The header can then state the model and the
 * context window before anything has been asked, instead of after.
 */
async function describeRun() {
  if (!app.session) return;
  try {
    app.config = await invoke("describe_run", { session: app.session.id });
  } catch (e) {
    app.error = String(e);
  }
}

export async function refreshSessions() {
  try {
    app.sessions = await invoke("list_sessions");
  } catch (e) {
    app.error = String(e);
  }
}

export async function newChat() {
  if (app.running) return;
  try {
    const meta = await invoke("create_session");
    clearRun();
    app.session = meta;
    app.view = "transcript";
    await Promise.all([refreshSessions(), describeRun()]);
  } catch (e) {
    app.error = String(e);
  }
}

export async function openSession(id) {
  if (app.running) return;
  try {
    const { meta, receipts, messages } = await invoke("load_session", { id });
    clearRun();
    app.session = meta;
    app.receipts = receipts;
    app.approvals = approvalsFrom(receipts);
    app.timeline = replay(messages);
    app.view = "transcript";
    await describeRun();
  } catch (e) {
    app.error = String(e);
  }
}

export async function removeSession(id) {
  try {
    await invoke("delete_session", { id });
    await refreshSessions();
    if (app.session?.id === id) {
      if (app.sessions.length) await openSession(app.sessions[0].id);
      else await newChat();
    }
  } catch (e) {
    app.error = String(e);
  }
}

/**
 * The approvals of a chat, read off its chain.
 *
 * The live event stream is not a durable record — reopening a chat, or closing
 * the window while a write was still waiting, would otherwise lose it. The chain
 * already holds both halves: a `tap_call` with no upstream status is a write that
 * paused, and an `approval_resolved` with the same txn is the decision. Deriving
 * from that means the screen agrees with the journal by construction.
 */
function approvalsFrom(receipts) {
  const out = new Map();
  for (const r of receipts ?? []) {
    if (r.event === "tap_call" && r.upstream_status == null && r.txn_id) {
      out.set(r.txn_id, {
        txn_id: r.txn_id,
        summary: `${r.method} ${r.target_host} via ${r.credential}`,
        decision: null,
        at: r.ts,
      });
    } else if (r.event === "approval_resolved" && r.txn_id) {
      const known = out.get(r.txn_id) ?? { txn_id: r.txn_id, summary: "tap_call", at: r.ts };
      out.set(r.txn_id, { ...known, decision: r.decision });
    }
  }
  return [...out.values()];
}

/**
 * Ask TAP about everything still waiting, and record whatever it says.
 *
 * Only while the session is idle: during a run the loop reconciles approvals
 * itself and holds its own handle on the chain, so a second writer here would
 * fork the hash links.
 */
export async function refreshApprovals() {
  if (app.running || !app.session) return;
  const waiting = app.approvals.filter((a) => !a.decision);
  if (!waiting.length) return;

  for (const a of waiting) {
    try {
      const decision = await invoke("check_approval", {
        session: app.session.id,
        txnId: a.txn_id,
      });
      if (decision === "pending") continue;

      a.decision = decision;
      push({ kind: "approval_resolved", txn_id: a.txn_id, decision });
      // The receipt the command just appended belongs on screen too.
      const { receipts } = await invoke("load_session", { id: app.session.id });
      app.receipts = receipts;
    } catch (e) {
      app.error = String(e);
      return;
    }
  }
}

/**
 * Rebuild a transcript from a stored conversation.
 *
 * Reopening a chat replays what was said, not what was journalled — the chain
 * holds digests, so it could never render this. Tool rows come back without
 * their outcome for the same reason: the message log records that a call was
 * made, and the receipt beside it records how it went.
 */
function replay(messages) {
  const out = [];
  for (const m of messages ?? []) {
    if (m.role === "user") {
      if (typeof m.content === "string") {
        out.push({ kind: "task", text: m.content });
      } else if (Array.isArray(m.content) && m.content.some((b) => b.type === "image")) {
        // A turn that carried pictures. Tool-result arrays are skipped: those
        // are already represented by their tool rows.
        out.push({
          kind: "task",
          text: m.content.find((b) => b.type === "text")?.text ?? "",
          images: m.content
            .filter((b) => b.type === "image")
            .map((b) => ({ media_type: b.source?.media_type, data: b.source?.data, name: "image" })),
        });
      }
    } else if (m.role === "assistant") {
      for (const b of Array.isArray(m.content) ? m.content : []) {
        if (b.type === "text") out.push({ kind: "text", text: b.text ?? "" });
        else if (b.type === "thinking")
          out.push({ kind: "thinking", text: b.thinking ?? b.text ?? "", expanded: false });
        else if (b.type === "tool_use")
          out.push({ kind: "tool", name: b.name, summary: summarise(b.input), status: "ok" });
      }
    } else if (m.role === "system") {
      out.push({ kind: "note", text: m.content });
    }
  }
  return out;
}

function summarise(input) {
  if (!input || typeof input !== "object") return "";
  if (input.target) {
    const host = String(input.target).split("://").pop().split("/")[0];
    return `${input.method ?? "GET"} ${host}${input.credential ? ` via ${input.credential}` : ""}`;
  }
  return input.command ?? input.path ?? input.pattern ?? input.txn_id ?? "";
}

export async function loadCapabilities() {
  try {
    app.capabilities = await invoke("list_capabilities");
  } catch (e) {
    app.error = String(e);
  }
}

/** What the window will accept. The backend checks again; this is only so the
 *  refusal happens next to the drop rather than after a round trip. */
const IMAGE_TYPES = ["image/png", "image/jpeg", "image/gif", "image/webp"];
const MAX_IMAGE_BYTES = 5 * 1024 * 1024;
const MAX_IMAGES = 8;

/**
 * Attach images to the next message.
 *
 * Takes anything file-shaped — a paste, a drop, a picker — and keeps the base64
 * the API wants alongside a data URL for the thumbnail, so nothing has to be
 * re-encoded to show it.
 */
export async function attach(files) {
  for (const file of files) {
    if (app.attachments.length >= MAX_IMAGES) {
      app.error = `A turn carries at most ${MAX_IMAGES} images.`;
      return;
    }
    if (!IMAGE_TYPES.includes(file.type)) {
      app.error = `${file.type || file.name} is not an image this run will send.`;
      continue;
    }
    if (file.size > MAX_IMAGE_BYTES) {
      app.error = `${file.name || "That image"} is over ${MAX_IMAGE_BYTES / 1048576} MB.`;
      continue;
    }

    const data = await new Promise((resolve, reject) => {
      const r = new FileReader();
      r.onerror = () => reject(r.error);
      // `data:<type>;base64,<payload>` — the payload is what the API takes.
      r.onload = () => resolve(String(r.result).split(",")[1] ?? "");
      r.readAsDataURL(file);
    }).catch(() => null);

    if (!data) {
      app.error = "That image could not be read.";
      continue;
    }
    app.error = null;
    app.attachments.push({ media_type: file.type, data, name: file.name || "image" });
  }
}

export function detach(index) {
  app.attachments.splice(index, 1);
}

export async function startRun() {
  const task = app.task.trim();
  const images = app.attachments.map(({ media_type, data }) => ({ media_type, data }));
  // An image on its own is a real question — "what is this?" — so a run may
  // start with attachments and no words.
  if ((!task && !images.length) || app.running) return;
  if (!app.session) await newChat();
  if (!app.session) return;

  app.running = true;
  app.error = null;
  app.verified = null;
  push({ kind: "task", text: task, images: app.attachments.map((a) => ({ ...a })) });
  app.task = "";
  app.attachments = [];
  await invoke("start_run", { task, session: app.session.id, images });
}
