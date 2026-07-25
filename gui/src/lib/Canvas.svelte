<script>
  /**
   * A page the model wrote, running.
   *
   * It runs inside a frame that is sandboxed without `allow-same-origin`, so it
   * has an opaque origin: no access to this window's DOM, its storage, or the
   * Tauri bridge — which is the part that actually matters, because that bridge
   * can start runs and delete chats.
   *
   * The page is served by the app's own `canvas:` scheme rather than as
   * `srcdoc`, and that is not a detail. A `srcdoc` frame inherits its parent's
   * CSP, so the model's inline script would be blocked by the app's
   * `script-src 'self'` — and loosening that to make the canvas work would
   * weaken every other part of the window. A response carrying its own CSP does
   * not inherit, so the canvas gets exactly what it needs and the app keeps its
   * own guarantees.
   *
   * That CSP is `default-src 'none'` with inline script and style: the canvas can
   * compute and draw, and it has no `connect-src`, so fetch, XHR and WebSockets
   * all fail inside it. The egress invariant survives contact with a live page.
   */
  import { invoke } from "@tauri-apps/api/core";
  import Code from "./Code.svelte";
  import Icon from "./Icon.svelte";
  import { fade } from "svelte/transition";
  import { ms, QUICK } from "./motion.js";

  let { code = "" } = $props();

  let url = $state(null);
  let error = $state(null);
  let view = $state("preview");
  let height = $state(320);
  let frame = $state(null);
  let big = $state(false);

  /**
   * Whether the frame's own script ever ran.
   *
   * This cannot be tested from here — WebView2 is not scriptable from the test
   * harness, and a CSP that silently blocked the inline script would look
   * exactly like a page that draws nothing. So the page reports in, and a frame
   * that never does says so instead of sitting there looking empty and correct.
   */
  let alive = $state(null);

  /**
   * Escape closes it.
   *
   * A pane that takes the whole window has to be dismissable by the key everyone
   * already tries, not only by finding the button again.
   */
  function onKey(e) {
    if (e.key === "Escape" && big) {
      e.preventDefault();
      big = false;
    }
  }

  /** A bare fragment still needs a document around it to be a page. */
  const page = $derived(
    /<html[\s>]/i.test(code)
      ? code
      : `<!doctype html><html><head><meta charset="utf-8">
<style>
  html,body{margin:0;background:#fff;color:#16150f;
    font:14px/1.6 system-ui,-apple-system,"Segoe UI",sans-serif}
  body{padding:14px}
</style></head><body>
${code}
<script>
  // Report the content height so the frame can be the size of what is in it.
  // This is the only thing that crosses the boundary, and the parent clamps it.
  //
  // Measured on the body, never on documentElement: inside a fixed-height frame
  // the latter is at least the viewport, so the page would report back its own
  // height and could only ever grow. (No backticks in here — this whole document
  // is a template literal, and one would end it.)
  const tell = () => parent.postMessage(
    { type: "locked:canvas-height", value: document.body.scrollHeight }, "*");
  new ResizeObserver(tell).observe(document.body);
  addEventListener("load", tell);
  tell();
<\/script></body></html>`,
  );

  $effect(() => {
    let current = true;
    alive = null;
    invoke("stage_canvas", { html: page })
      .then((u) => current && ((url = u), (error = null)))
      .catch((e) => current && (error = String(e)));

    // Generous: a slow first paint is not a failure. Silence past this is.
    const giveUp = setTimeout(() => current && alive === null && (alive = false), 2500);
    return () => {
      current = false;
      clearTimeout(giveUp);
    };
  });

  /**
   * The frame may ask to be taller. It is one number, from a known frame, and it
   * is clamped — a page that asks for ten thousand pixels gets nine hundred.
   */
  function onMessage(e) {
    if (!frame || e.source !== frame.contentWindow) return;
    if (e.data?.type !== "locked:canvas-height") return;
    alive = true;
    const n = Number(e.data.value);
    if (Number.isFinite(n)) height = Math.max(120, Math.min(900, Math.round(n) + 2));
  }
</script>

<svelte:window onmessage={onMessage} onkeydown={onKey} />

{#if big}
  <!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
  <div class="scrim" onclick={() => (big = false)} transition:fade={{ duration: ms(QUICK) }}></div>
{/if}

<figure class="canvas" class:big>
  <div class="bar">
    <span class="what">
      canvas <span class="sep">·</span> no network, no access to this window
      {#if alive === false}
        <span class="stalled" title="The page loaded but its script never reported in — most likely the frame's policy blocked it.">· script did not run</span>
      {/if}
    </span>
    <div class="tabs">
      <button class:on={view === "preview"} onclick={() => (view = "preview")}>preview</button>
      <button class:on={view === "code"} onclick={() => (view = "code")}>code</button>
      <button
        class="grow"
        onclick={() => (big = !big)}
        title={big ? "Close  Esc" : "Open full width"}
        aria-label={big ? "Close" : "Open full width"}
      >
        <Icon name={big ? "shrink" : "expand"} />
      </button>
    </div>
  </div>

  {#if view === "preview"}
    {#if error}
      <p class="oops">{error}</p>
    {:else if url}
      <iframe
        bind:this={frame}
        src={url}
        title="Canvas"
        sandbox="allow-scripts"
        referrerpolicy="no-referrer"
        style="height: {height}px"
      ></iframe>
    {:else}
      <div class="waiting" style="height: {height}px"></div>
    {/if}
  {:else}
    <div class="source"><Code lang="html" {code} /></div>
  {/if}
</figure>

<style>
  .canvas {
    margin: 12px 0 16px;
    border-radius: var(--r-lg);
    background: rgba(255, 255, 255, 0.72);
    box-shadow:
      inset 0 0 0 1px rgba(20, 18, 14, 0.06),
      0 1px 2px rgba(20, 18, 14, 0.04);
    overflow: hidden;
  }

  .bar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 14px;
    padding: 6px 8px 6px 13px;
    box-shadow: inset 0 -1px 0 rgba(20, 18, 14, 0.05);
  }
  /* The frame's limits are stated where you meet it, not buried in a doc. */
  .what { font-size: 10.5px; color: var(--ash); letter-spacing: 0.2px; }
  /* A frame whose script never ran looks identical to one that drew nothing on
     purpose. Saying so is the difference between a bug and a mystery. */
  .stalled { color: var(--amber); }
  .sep { color: var(--stone); }

  .tabs { display: flex; gap: 2px; flex: 0 0 auto; }
  .tabs button {
    font-size: 10.5px;
    color: var(--ash);
    padding: 3px 9px;
    border-radius: var(--r-xs);
    transition: color 0.16s var(--ease), background 0.16s var(--ease);
  }
  .tabs button:hover { color: var(--body); background: rgba(20, 18, 14, 0.05); }
  .tabs button.on { color: var(--ink); background: rgba(20, 18, 14, 0.07); }

  iframe {
    display: block;
    width: 100%;
    border: 0;
    background: #fff;
    transition: height 0.24s var(--ease);
  }
  .waiting { background: rgba(20, 18, 14, 0.02); }

  .grow { display: flex; align-items: center; padding: 3px 6px; }

  /* Expanded, it takes the window rather than opening a second copy somewhere
     else — same element, same frame, so nothing reloads and whatever the page
     was doing keeps doing it. */
  .scrim {
    position: fixed;
    inset: 0;
    z-index: 40;
    background: rgba(20, 18, 14, 0.28);
    backdrop-filter: blur(3px);
  }

  .canvas.big {
    position: fixed;
    inset: 3vh 3vw;
    z-index: 41;
    margin: 0;
    display: flex;
    flex-direction: column;
    background: rgba(255, 255, 255, 0.96);
    backdrop-filter: blur(14px) saturate(1.3);
    box-shadow:
      inset 0 0 0 1px rgba(20, 18, 14, 0.08),
      0 24px 70px -20px rgba(20, 18, 14, 0.5);
  }
  /* The reported height governs the inline frame; expanded, the pane governs. */
  .canvas.big iframe,
  .canvas.big .waiting { flex: 1 1 auto; height: auto !important; }
  .canvas.big .source { flex: 1 1 auto; overflow: auto; }

  .oops { margin: 0; padding: 16px; color: var(--error); font-size: 12.5px; }
  /* The code view reuses the ordinary block, minus its own outer frame. */
  .source :global(.block) { margin: 0; border-radius: 0; box-shadow: none; background: none; }
  .source :global(.bar) { display: none; }
</style>
