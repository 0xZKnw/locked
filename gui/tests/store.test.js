import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

/**
 * The window's whole job is to render an event stream faithfully. These tests
 * drive the real store with the same events the Rust side emits, and assert what
 * a reader would see.
 *
 * Tauri is mocked at the module boundary rather than stubbed on `window`,
 * because the store imports the API directly — and the mock records what it was
 * called with, which is how the subscription tests below work at all.
 */

let handlers = [];
let invoked = [];
let invokeImpl = () => Promise.resolve(null);
let unlistenCalls = 0;

vi.mock("@tauri-apps/api/event", () => ({
  listen: async (_name, handler) => {
    handlers.push(handler);
    const mine = handler;
    return () => {
      unlistenCalls += 1;
      handlers = handlers.filter((h) => h !== mine);
    };
  },
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd, args) => {
    invoked.push({ cmd, args });
    return invokeImpl(cmd, args);
  },
}));

/** Emit an event exactly as the Rust side would, to every live subscriber. */
function fire(payload) {
  for (const h of [...handlers]) h({ event: "locked://event", payload });
}

/**
 * Run the requestAnimationFrame pump until the buffer is actually empty.
 *
 * Waiting a fixed number of frames both wastes seconds when the buffer drains
 * early and lies when it does not, so this watches the buffer itself and keeps a
 * cap only as a deadlock guard.
 */
async function settle(maxFrames = 600) {
  for (let i = 0; i < maxFrames; i++) {
    await new Promise((r) => requestAnimationFrame(r));
    if (i > 1 && !store.app.timeline.some((item) => item.pending)) return;
  }
  throw new Error("the streaming buffer never drained");
}

const META = { id: "s1", title: "", created: "a", updated: "a", turns: 0, receipts: 0 };

let store;

beforeEach(async () => {
  handlers = [];
  invoked = [];
  unlistenCalls = 0;
  invokeImpl = (cmd) => {
    if (cmd === "list_sessions") return Promise.resolve([META]);
    if (cmd === "create_session") return Promise.resolve({ ...META, id: "sNew" });
    if (cmd === "load_session")
      return Promise.resolve({ meta: META, receipts: [], messages: [] });
    if (cmd === "describe_run") return Promise.resolve({ type: "run_config", model: "k3" });
    return Promise.resolve(null);
  };
  vi.resetModules();
  store = await import("../src/lib/store.svelte.js");
});

afterEach(() => {
  vi.restoreAllMocks();
});

// ---------------------------------------------------------------------------
// Streaming
// ---------------------------------------------------------------------------

describe("streaming", () => {
  it("shows every character the model sent, in order", async () => {
    const { app } = store;
    await store.boot();

    fire({ type: "assistant_delta", text: "Bonjour, " });
    fire({ type: "assistant_delta", text: "je peux " });
    fire({ type: "assistant_delta", text: "t'aider." });
    await settle();

    const text = app.timeline.filter((i) => i.kind === "text").map((i) => i.text).join("");
    expect(text).toBe("Bonjour, je peux t'aider.");
  });

  it("releases text gradually rather than in the bursts it arrives in", async () => {
    const { app } = store;
    await store.boot();

    fire({ type: "assistant_delta", text: "x".repeat(400) });

    // One frame in, the whole burst must not already be on screen — that is the
    // difference between smoothing and not.
    await new Promise((r) => requestAnimationFrame(r));
    const early = app.timeline.at(-1).text.length;
    expect(early).toBeGreaterThan(0);
    expect(early).toBeLessThan(400);

    await settle();
    expect(app.timeline.at(-1).text.length).toBe(400);
  });

  it("closes a reasoning block when the answer starts", async () => {
    const { app } = store;
    await store.boot();

    fire({ type: "thinking_delta", text: "weighing it up" });
    await settle();
    expect(app.timeline.at(-1).open).toBe(true);

    fire({ type: "assistant_delta", text: "Here it is." });
    await settle();

    const thinking = app.timeline.find((i) => i.kind === "thinking");
    expect(thinking.open).toBe(false);
    expect(thinking.text).toBe("weighing it up");
  });

  it("never truncates a block that is sealed while still draining", async () => {
    const { app } = store;
    await store.boot();

    fire({ type: "assistant_delta", text: "y".repeat(600) });
    fire({ type: "run_finished", turns: 1 });
    await settle();

    const text = app.timeline.find((i) => i.kind === "text");
    expect(text.text.length).toBe(600);
    expect(text.open).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// The subscription
// ---------------------------------------------------------------------------

describe("the event subscription", () => {
  /**
   * Regression. Booting twice used to leave both subscriptions live, so every
   * delta was handled twice and the two copies interleaved character by
   * character — "SalSalututut". An event stream with no de-duplication cannot be
   * joined more than once.
   */
  it("survives being booted again", async () => {
    const { app } = store;
    await store.boot();
    await store.boot();

    expect(handlers.length).toBe(1);
    expect(unlistenCalls).toBe(1);

    fire({ type: "assistant_delta", text: "Salut" });
    await settle();

    const text = app.timeline.filter((i) => i.kind === "text").map((i) => i.text).join("");
    expect(text).toBe("Salut");
  });
});

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

describe("sessions", () => {
  it("opens the most recent chat on launch instead of nothing", async () => {
    const { app } = store;
    await store.boot();
    expect(app.session?.id).toBe("s1");
    expect(invoked.map((i) => i.cmd)).toContain("load_session");
  });

  it("starts a chat when there are none", async () => {
    invokeImpl = (cmd) => {
      if (cmd === "list_sessions") return Promise.resolve([]);
      if (cmd === "create_session") return Promise.resolve({ ...META, id: "sFirst" });
      return Promise.resolve(null);
    };
    vi.resetModules();
    store = await import("../src/lib/store.svelte.js");

    await store.boot();
    expect(store.app.session?.id).toBe("sFirst");
  });

  it("clears the previous chat's state when another is opened", async () => {
    const { app } = store;
    await store.boot();

    fire({ type: "assistant_delta", text: "old answer" });
    fire({ type: "receipt_appended", receipt: { seq: 0, event: "inference" } });
    fire({ type: "chain_verified", receipts: 1, head: "sha256:abc" });
    await settle();
    expect(app.timeline.length).toBeGreaterThan(0);

    await store.openSession("s1");
    expect(app.timeline).toEqual([]);
    expect(app.receipts).toEqual([]);
    expect(app.verified).toBe(null);
  });

  it("sends the active session with the task, so a run lands in the right chain", async () => {
    const { app } = store;
    await store.boot();
    app.task = "  do the thing  ";
    await store.startRun();

    const run = invoked.find((i) => i.cmd === "start_run");
    expect(run.args).toEqual({ task: "do the thing", session: "s1", images: [] });
    expect(app.task).toBe("");
    expect(app.running).toBe(true);
  });

  it("refuses to switch chats mid-run", async () => {
    const { app } = store;
    await store.boot();
    app.task = "go";
    await store.startRun();

    await store.newChat();
    expect(app.session.id).toBe("s1");
  });
});

// ---------------------------------------------------------------------------
// Reading a chat back
// ---------------------------------------------------------------------------

describe("replaying a stored conversation", () => {
  it("rebuilds what was said, in order", async () => {
    const messages = [
      { role: "user", content: "check the credentials" },
      {
        role: "assistant",
        content: [
          { type: "thinking", thinking: "list them first" },
          { type: "tool_use", id: "t1", name: "tap_discover", input: {} },
        ],
      },
      { role: "user", content: [{ type: "tool_result", tool_use_id: "t1", content: "..." }] },
      { role: "assistant", content: [{ type: "text", text: "Three are available." }] },
      { role: "system", content: "Transaction txn_7 resolved: approved." },
    ];
    invokeImpl = (cmd) => {
      if (cmd === "list_sessions") return Promise.resolve([META]);
      if (cmd === "load_session") return Promise.resolve({ meta: META, receipts: [], messages });
      return Promise.resolve(null);
    };
    vi.resetModules();
    store = await import("../src/lib/store.svelte.js");
    await store.boot();

    expect(store.app.timeline.map((i) => i.kind)).toEqual([
      "task",
      "thinking",
      "tool",
      "text",
      "note",
    ]);
    // Tool results are already represented by the tool row; replaying them as
    // user turns would show the model's own plumbing as conversation.
    expect(store.app.timeline.filter((i) => i.kind === "task").length).toBe(1);
    expect(store.app.timeline.find((i) => i.kind === "text").text).toBe("Three are available.");
  });

  it("summarises a call without leaking its query string", async () => {
    const messages = [
      {
        role: "assistant",
        content: [
          {
            type: "tool_use",
            id: "t1",
            name: "tap_call",
            input: {
              credential: "dune",
              method: "GET",
              target: "https://api.dune.com/v1/q?api_key=SUPERSECRET",
            },
          },
        ],
      },
    ];
    invokeImpl = (cmd) => {
      if (cmd === "list_sessions") return Promise.resolve([META]);
      if (cmd === "load_session") return Promise.resolve({ meta: META, receipts: [], messages });
      return Promise.resolve(null);
    };
    vi.resetModules();
    store = await import("../src/lib/store.svelte.js");
    await store.boot();

    const row = store.app.timeline.find((i) => i.kind === "tool");
    expect(row.summary).toBe("GET api.dune.com via dune");
    expect(row.summary).not.toContain("SUPERSECRET");
  });
});

// ---------------------------------------------------------------------------
// Images
// ---------------------------------------------------------------------------

describe("attaching images", () => {
  const png = (bytes = 64) =>
    new File([new Uint8Array(bytes)], "shot.png", { type: "image/png" });

  it("sends the base64 the API wants, and clears the tray", async () => {
    const { app } = store;
    await store.boot();

    await store.attach([png()]);
    expect(app.attachments).toHaveLength(1);
    expect(app.attachments[0].media_type).toBe("image/png");
    expect(app.attachments[0].data.length).toBeGreaterThan(0);

    app.task = "what is this";
    await store.startRun();

    const run = invoked.find((i) => i.cmd === "start_run");
    expect(run.args.images).toHaveLength(1);
    expect(run.args.images[0]).toEqual({
      media_type: "image/png",
      data: app.timeline.at(-1).images[0].data,
    });
    // The name is for the window; it has no business on the wire.
    expect(run.args.images[0].name).toBeUndefined();
    expect(app.attachments).toEqual([]);
    expect(app.timeline.at(-1).images).toHaveLength(1);
  });

  it("lets an image be the whole question", async () => {
    const { app } = store;
    await store.boot();
    await store.attach([png()]);
    app.task = "";
    await store.startRun();

    expect(invoked.some((i) => i.cmd === "start_run")).toBe(true);
    expect(app.running).toBe(true);
  });

  it("refuses what it will not send, and says why", async () => {
    const { app } = store;
    await store.boot();

    await store.attach([new File(["x"], "notes.pdf", { type: "application/pdf" })]);
    expect(app.attachments).toHaveLength(0);
    expect(app.error).toContain("not an image");

    await store.attach([png(6 * 1024 * 1024)]);
    expect(app.attachments).toHaveLength(0);
    expect(app.error).toContain("over 5 MB");
  });

  it("puts an image back on screen when the chat is reopened", async () => {
    const messages = [
      {
        role: "user",
        content: [
          { type: "image", source: { type: "base64", media_type: "image/png", data: "AAAA" } },
          { type: "text", text: "what is this" },
        ],
      },
    ];
    invokeImpl = (cmd) => {
      if (cmd === "list_sessions") return Promise.resolve([META]);
      if (cmd === "load_session") return Promise.resolve({ meta: META, receipts: [], messages });
      return Promise.resolve(null);
    };
    vi.resetModules();
    store = await import("../src/lib/store.svelte.js");
    await store.boot();

    const turn = store.app.timeline.find((i) => i.kind === "task");
    expect(turn.text).toBe("what is this");
    expect(turn.images).toEqual([{ media_type: "image/png", data: "AAAA", name: "image" }]);
  });
});

// ---------------------------------------------------------------------------
// The context gauge
// ---------------------------------------------------------------------------

describe("the context gauge", () => {
  it("reads the most recent inference off the chain", () => {
    const receipts = [
      { seq: 0, event: "run_started" },
      { seq: 1, event: "inference", input_tokens: 100, output_tokens: 10 },
      { seq: 2, event: "tap_call" },
      { seq: 3, event: "inference", input_tokens: 47210, output_tokens: 812 },
      { seq: 4, event: "run_finished" },
    ];
    expect(store.lastUsage(receipts)).toEqual({ input: 47210, output: 812 });
  });

  it("reports nothing rather than zero when nothing has run", () => {
    expect(store.lastUsage([])).toBe(null);
    expect(store.lastUsage([{ seq: 0, event: "run_started" }])).toBe(null);
  });
});

// ---------------------------------------------------------------------------
// Approvals
// ---------------------------------------------------------------------------

describe("approvals", () => {
  /**
   * Regression, from the wild. A write approved in Telegram *after* the run had
   * ended stayed on screen as "waiting" forever: reconciliation only happens at
   * the top of the loop's next turn, and a finished run has no next turn. Worse
   * than the stale label, the decision a human actually made never reached the
   * chain — and that is the one receipt tier a third party can corroborate.
   */
  it("notices a write approved after the run already ended", async () => {
    const pendingCall = {
      seq: 1,
      ts: "2026-07-25T12:07:00Z",
      event: "tap_call",
      credential: "discord",
      target_host: "discord.com",
      method: "POST",
      upstream_status: null,
      attestation: "tap_attested",
      txn_id: "918bd940",
    };

    let decision = "pending";
    let receipts = [pendingCall];
    invokeImpl = (cmd) => {
      if (cmd === "list_sessions") return Promise.resolve([META]);
      if (cmd === "load_session")
        return Promise.resolve({ meta: META, receipts, messages: [] });
      if (cmd === "check_approval") return Promise.resolve(decision);
      return Promise.resolve(null);
    };
    vi.resetModules();
    store = await import("../src/lib/store.svelte.js");
    await store.boot();

    // Reopening the chat shows the pending write, read off the chain rather than
    // remembered from a live event that happened in another process.
    expect(store.app.approvals).toHaveLength(1);
    expect(store.app.approvals[0].decision).toBe(null);
    expect(store.app.approvals[0].summary).toBe("POST discord.com via discord");

    // Still waiting: nothing changes, and nothing is invented.
    await store.refreshApprovals();
    expect(store.app.approvals[0].decision).toBe(null);

    // The human taps approve in Telegram.
    decision = "approved";
    receipts = [
      pendingCall,
      {
        seq: 2,
        ts: "2026-07-25T12:08:00Z",
        event: "approval_resolved",
        txn_id: "918bd940",
        decision: "approved",
        attestation: "tap_attested",
      },
    ];
    await store.refreshApprovals();

    expect(store.app.approvals[0].decision).toBe("approved");
    expect(store.app.timeline.some((i) => i.kind === "approval_resolved")).toBe(true);
    // And the decision is now in the chain the window is showing.
    expect(store.app.receipts).toHaveLength(2);
  });

  it("does not touch the chain while a run is writing to it", async () => {
    const { app } = store;
    await store.boot();
    app.approvals.push({ txn_id: "txn_1", summary: "tap_call", decision: null, at: "x" });
    app.running = true;

    await store.refreshApprovals();
    expect(invoked.some((i) => i.cmd === "check_approval")).toBe(false);
  });

  it("tracks a pending write and then its decision", async () => {
    const { app } = store;
    await store.boot();

    fire({ type: "approval_pending", txn_id: "txn_7", summary: "tap_call" });
    expect(app.approvals.length).toBe(1);
    expect(app.approvals[0].decision).toBe(null);

    fire({ type: "approval_resolved", txn_id: "txn_7", decision: "approved" });
    expect(app.approvals[0].decision).toBe("approved");
    expect(app.timeline.some((i) => i.kind === "approval_resolved")).toBe(true);
  });
});

// ---------------------------------------------------------------------------
// Failure
// ---------------------------------------------------------------------------

describe("a failed run", () => {
  it("stops, says why, and leaves what was already streamed intact", async () => {
    const { app } = store;
    await store.boot();

    fire({ type: "assistant_delta", text: "partial answer" });
    await settle();
    fire({ type: "run_failed", message: "turn cap reached" });
    await settle();

    expect(app.running).toBe(false);
    expect(app.error).toBe("turn cap reached");
    expect(app.timeline.find((i) => i.kind === "text").text).toBe("partial answer");
    expect(app.timeline.find((i) => i.kind === "text").open).toBe(false);
  });
});
