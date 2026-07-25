<script>
  import { onMount } from "svelte";
  import {
    app,
    boot,
    loadCapabilities,
    lastUsage,
    newChat,
    openSession,
    removeSession,
  } from "./lib/store.svelte.js";
  import Gauge from "./lib/Gauge.svelte";
  import { fly, slide, fade } from "svelte/transition";
  import { ms, ease, BASE, CALM } from "./lib/motion.js";
  import Transcript from "./lib/Transcript.svelte";
  import Receipts from "./lib/Receipts.svelte";
  import Approvals from "./lib/Approvals.svelte";
  import Capabilities from "./lib/Capabilities.svelte";
  import Icon from "./lib/Icon.svelte";

  // The rail's state outlives the window: reopening to a layout you didn't
  // choose is the kind of small betrayal that makes an app feel careless.
  let open = $state(localStorage.getItem("locked.rail") !== "closed");

  function toggleRail() {
    open = !open;
    localStorage.setItem("locked.rail", open ? "open" : "closed");
  }

  onMount(() => {
    boot();
    loadCapabilities();
  });

  /* -------------------------------------------------------------------------
     The aurora as the loading state.

     Speeding this up by swapping animation durations would tear: progress is
     elapsed-time ÷ duration, so changing the duration jumps every blob to a new
     position. Playback rate is the property built for this — it rescales the
     clock and holds the current time, so the field keeps its exact composition
     and simply starts moving through it faster.

     The curve matters more than the endpoints. Spin-up hesitates for a beat,
     dipping just below its resting speed before it catches, then surges past
     the target and settles back — that undershoot-then-overshoot is what makes
     a thing read as having inertia rather than as a value being switched.
     Spin-down has no such kick: it coasts, with a long tail.
     ------------------------------------------------------------------------- */

  let aurora;
  let rate = 1;
  let ramp = null;

  const BUSY_RATE = 4.4;
  const SPIN_UP_MS = 2000;
  const SPIN_DOWN_MS = 2600;

  /** Undershoots at the start, overshoots at the end. The engine curve. */
  function spinUp(x) {
    const c = 1.70158 * 1.525;
    return x < 0.5
      ? ((2 * x) ** 2 * ((c + 1) * 2 * x - c)) / 2
      : ((2 * x - 2) ** 2 * ((c + 1) * (2 * x - 2) + c) + 2) / 2;
  }

  const coastDown = (x) => 1 - (1 - x) ** 4;

  function setRate(r) {
    if (!aurora) return;
    // A rate at or below zero would freeze the field, or run it backwards.
    const safe = Math.max(0.12, r);
    for (const a of aurora.getAnimations({ subtree: true })) {
      // `getAnimations` also returns the CSS transitions that choreograph the
      // busy state. Speeding those up collapses a two-second staggered descent
      // into half a second — which is exactly what made it feel abrupt. Only
      // the field's own keyframe animations get the rate.
      if (!("animationName" in a)) continue;
      a.playbackRate = safe;
    }
  }

  $effect(() => {
    const busy = app.running;
    const target = busy ? BUSY_RATE : 1;
    if (!aurora || rate === target) return;

    if (ramp) cancelAnimationFrame(ramp);
    const from = rate;
    const t0 = performance.now();
    const dur = busy ? SPIN_UP_MS : SPIN_DOWN_MS;
    const curve = busy ? spinUp : coastDown;

    const tick = (now) => {
      const p = Math.min(1, (now - t0) / dur);
      rate = from + (target - from) * curve(p);
      setRate(rate);
      ramp = p < 1 ? requestAnimationFrame(tick) : null;
    };
    ramp = requestAnimationFrame(tick);
  });

  /* The rail's selection is one object that moves, not four that blink. Reading
     the live geometry rather than assuming a row height means it stays correct
     if the rail ever grows a row of a different size. */
  let railEl = $state(null);
  let marker = $state(null);

  $effect(() => {
    app.view;
    open;
    if (!railEl) return;
    const active = railEl.querySelector("button.nav.active");
    marker = active ? { top: active.offsetTop, height: active.offsetHeight } : null;
  });

  const pendingCount = $derived(app.approvals.filter((a) => !a.decision).length);

  const views = $derived([
    { id: "transcript", label: "Transcript", badge: null },
    { id: "receipts", label: "Receipts", badge: app.receipts.length || null },
    { id: "approvals", label: "Approvals", badge: pendingCount || null },
    { id: "capabilities", label: "Capabilities", badge: app.capabilities.length || null },
  ]);

  // Context, as the chain reports it. `used` is what the last inference carried
  // in and out — i.e. roughly what the next one will have to carry.
  const usage = $derived(lastUsage(app.receipts));
  const window_ = $derived(app.config?.context_window ?? 0);
  const used = $derived(usage ? usage.input + usage.output : 0);
  const fraction = $derived(window_ ? Math.min(1, used / window_) : 0);
  const pressure = $derived(fraction > 0.9 ? "hot" : fraction > 0.72 ? "warm" : "");

  // "1049k" is a number you have to decode; "1M" is one you read.
  const compact = (n) => {
    if (n >= 1_000_000) return `${(n / 1e6).toFixed(n >= 10_000_000 ? 0 : 1)}M`.replace(".0", "");
    if (n >= 1000) return `${(n / 1000).toFixed(n >= 10_000 ? 0 : 1)}k`.replace(".0", "");
    return String(n);
  };

  // The window states its own integrity rather than letting the reader assume.
  const integrity = $derived(
    app.config?.integrity === "full"
      ? { label: "invariant held", tone: "ok" }
      : app.config
        ? { label: "degraded", tone: "warn", title: app.config.integrity }
        : null,
  );
</script>

<!-- Two layers on purpose: `field` owns the hue drift and the blending, `aurora`
     owns the busy state. Stacking one filter above the other is what lets the
     run's saturation swell without touching the animation underneath it. -->
<div class="aurora" class:busy={app.running} bind:this={aurora} aria-hidden="true">
  <div class="field">
    <!-- Each lobe is wrapped so the busy state can move it on its own clock.
         The span keeps its drift animation; the wrapper carries the descent,
         and the two compose. -->
    <div class="lobe l1"><span class="a1"></span></div>
    <div class="lobe l2"><span class="a2"></span></div>
    <div class="lobe l3"><span class="a3"></span></div>
    <div class="lobe l4"><span class="a4"></span></div>
    <div class="lobe l5"><span class="a5"></span></div>
  </div>
</div>

<div class="shell">
  <header>
    <div class="brand">
      <!-- The fold control belongs to the window, not to the rail: leaving it
           inside meant it moved every time you used it. -->
      <button class="rail-toggle" onclick={toggleRail} title={open ? "Collapse" : "Expand"}>
        <Icon name="panel" />
      </button>
      <span class="mark" class:live={app.running}></span>
      <span class="wordmark">Locked</span>
    </div>

    <!-- Two capsules, not six chips: what is running, then what it is allowed
         to do. Semantic colour lives in a 5px dot; the chrome stays grey. -->
    {#if app.config}
      <div class="hud" in:fly={{ y: -6, duration: ms(BASE), easing: ease }}>
        <div class="status">
          <span class="seg model" title="{app.config.provider} · {app.config.model}">
            {app.config.model}
          </span>
          <span
            class="seg ctx {pressure}"
            title="{used.toLocaleString()} of {window_.toLocaleString()} tokens carried by the last inference. The window is a declared assumption — override it with LOCKED_CONTEXT_WINDOW."
          >
            <Gauge {fraction} />
            <!-- One flex item, or the row gap opens between the count and the
                 slash and it reads as two separate numbers. -->
            <span class="nums">{compact(used)}<span class="of">/{compact(window_)}</span></span>
          </span>
        </div>

        <div class="status">
          {#if integrity}
            <span class="seg" title={integrity.title ?? ""}>
              <i class="dot" class:warn={integrity.tone === "warn"}></i>{integrity.label}
            </span>
          {/if}
          <span class="seg">{app.config.sandbox ?? "no sandbox"}</span>
          <span class="seg">{app.config.tools.length} tools</span>
        </div>
      </div>
    {/if}
  </header>

  <div class="body">
    <nav class:closed={!open} bind:this={railEl}>
      {#if marker}
        <span class="marker" style="transform: translateY({marker.top}px); height: {marker.height}px"></span>
      {/if}
      <button class="fresh" onclick={newChat} disabled={app.running} title="New chat">
        <Icon name="plus" />
        <span class="label">New chat</span>
      </button>

      {#each views as v (v.id)}
        <button
          class="nav"
          class:active={app.view === v.id}
          onclick={() => (app.view = v.id)}
          title={open ? "" : v.label}
        >
          <Icon name={v.id} />
          <span class="label">{v.label}</span>
          {#if v.badge}
            <span class="count">{v.badge}</span>
            <!-- Collapsed, a count has nowhere to go — a mark says "something's
                 here" without pretending to be readable at that width. -->
            <span class="pip"></span>
          {/if}
        </button>
      {/each}

      <!-- History. Hidden when folded: a chat title is unreadable at 56px and a
           row of identical stubs would only be noise. -->
      {#if open && app.sessions.length}
        <div class="recent" transition:slide={{ duration: ms(BASE), easing: ease }}>
          <div class="rlabel">Chats</div>
          <div class="rlist">
            {#each app.sessions as s (s.id)}
              <div class="chat" class:active={app.session?.id === s.id} transition:slide={{ duration: ms(BASE), easing: ease }}>
                <button class="ctitle" onclick={() => openSession(s.id)} disabled={app.running}>
                  {s.title || "New chat"}
                </button>
                <button class="del" onclick={() => removeSession(s.id)} title="Delete chat">
                  <Icon name="trash" />
                </button>
              </div>
            {/each}
          </div>
        </div>
      {/if}

      <div class="spacer"></div>

      {#if app.verified}
        <div class="verified" in:fly={{ y: 6, duration: ms(BASE), easing: ease }} title="{app.verified.receipts} receipts · {app.verified.head}">
          <div class="v-label">chain verified</div>
          <div class="v-value">{app.verified.receipts} receipts</div>
          <div class="v-head">{app.verified.head.slice(7, 27)}</div>
          <div class="v-mark"></div>
        </div>
      {/if}
    </nav>

    <main>
      {#if app.error}
        <div class="error" transition:fly={{ y: -8, duration: ms(BASE), easing: ease }}>{app.error}</div>
      {/if}

      <!-- The two views overlap while they trade places. With only an intro the
           outgoing one is torn out on the same frame the incoming one starts,
           which is the blank flash between pages — a crossfade needs both to be
           on screen at once, so the stage stacks them. -->
      <div class="stage">
        {#key app.view}
          <div
            class="view"
            in:fly={{ y: 10, duration: ms(CALM), easing: ease }}
            out:fade={{ duration: ms(BASE), easing: ease }}
          >
            {#if app.view === "transcript"}
              <Transcript />
            {:else if app.view === "receipts"}
              <Receipts />
            {:else if app.view === "approvals"}
              <Approvals />
            {:else}
              <Capabilities />
            {/if}
          </div>
        {/key}
      </div>
    </main>
  </div>
</div>

<style>
  .shell { display: flex; flex-direction: column; height: 100%; }

  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    padding: 0 16px;
    height: 46px;
    flex: 0 0 auto;
  }

  .brand { display: flex; align-items: center; gap: 9px; margin-left: -6px; }

  .rail-toggle {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 28px;
    height: 28px;
    margin-right: 3px;
    border-radius: var(--r-xs);
    color: var(--ash);
    transition: color 0.18s var(--ease), background 0.18s var(--ease);
  }
  .rail-toggle:hover { color: var(--ink); background: rgba(20, 18, 14, 0.06); }

  .wordmark {
    color: var(--ink);
    font-weight: 550;
    font-size: 13px;
    letter-spacing: 0.2px;
  }

  /* A square, not a glowing orb. It reports whether a run is in flight. */
  .mark {
    width: 7px; height: 7px;
    border-radius: 2px;
    background: var(--stone);
    transition: background 0.3s var(--ease);
  }
  .mark.live { background: var(--tier-2); animation: blink 1.6s steps(1, end) infinite; }

  /* One capsule with hairline dividers rather than three bordered chips: the
     run has one posture, so it should be one object. Semantic colour is a 5px
     dot — the chrome itself never takes a tint. */
  .status {
    display: flex;
    align-items: center;
    height: 26px;
    border-radius: var(--r-pill);
    background: rgba(255, 255, 255, 0.62);
    backdrop-filter: blur(14px) saturate(1.35);
    box-shadow:
      inset 0 0 0 1px rgba(20, 18, 14, 0.055),
      0 1px 2px rgba(20, 18, 14, 0.045);
    font-size: 11px;
    letter-spacing: 0.15px;
    color: var(--mute);
  }
  .seg {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 0 11px;
    white-space: nowrap;
  }
  .seg + .seg { box-shadow: inset 1px 0 0 rgba(20, 18, 14, 0.07); }
  .dot { width: 5px; height: 5px; border-radius: 50%; background: var(--tier-3); }
  .dot.warn { background: var(--amber); }

  .hud { display: flex; align-items: center; gap: 8px; }

  .model { font-family: var(--mono); font-size: 10.5px; color: var(--body); }

  /* The denominator is shown, not just a percentage: the window is an assumption
     this app declares, so hiding it would make the gauge unfalsifiable. */
  .ctx { color: var(--tier-3); gap: 7px; font-variant-numeric: tabular-nums; }
  .ctx .of { color: var(--ash); }
  .ctx.warm { color: var(--amber); }
  .ctx.hot { color: var(--error); }

  .body { display: flex; flex: 1 1 auto; min-height: 0; }

  /* The rail folds to its icons. Width is the only thing that animates — labels
     fade out ahead of the fold so nothing is ever caught mid-clip. */
  /* The travelling selection. It sits under the labels, so the text never has to
     fade with it — only the surface moves. */
  .marker {
    position: absolute;
    left: 8px;
    right: 8px;
    top: 0;
    border-radius: var(--r-sm);
    background: rgba(255, 255, 255, 0.72);
    box-shadow: 0 1px 2px rgba(20, 18, 14, 0.07);
    transition:
      transform 0.34s var(--ease),
      height 0.34s var(--ease);
    pointer-events: none;
    z-index: 0;
  }

  nav {
    position: relative;
    width: 196px;
    flex: 0 0 auto;
    padding: 4px 8px 10px;
    display: flex;
    flex-direction: column;
    gap: 1px;
    overflow: hidden;
    transition: width 0.28s var(--ease);
  }
  nav.closed { width: 56px; }

  /* The only filled control in the rail. Starting a chat is the one thing here
     you do rather than look at. */
  .fresh {
    display: flex;
    align-items: center;
    gap: 11px;
    height: 34px;
    padding: 0 11px;
    margin-bottom: 7px;
    border-radius: var(--r-sm);
    background: rgba(255, 255, 255, 0.66);
    box-shadow:
      inset 0 0 0 1px rgba(20, 18, 14, 0.07),
      0 1px 2px rgba(20, 18, 14, 0.05);
    color: var(--ink);
    font-size: 13px;
    white-space: nowrap;
    transition: background 0.18s var(--ease), box-shadow 0.18s var(--ease);
  }
  .fresh:hover:not(:disabled) {
    background: rgba(255, 255, 255, 0.92);
    box-shadow:
      inset 0 0 0 1px rgba(20, 18, 14, 0.1),
      0 2px 6px rgba(20, 18, 14, 0.07);
  }
  .fresh:disabled { color: var(--ash); box-shadow: inset 0 0 0 1px rgba(20, 18, 14, 0.05); cursor: default; }

  .nav {
    position: relative;
    display: flex;
    align-items: center;
    gap: 11px;
    height: 34px;
    padding: 0 11px;
    border-radius: var(--r-sm);
    color: var(--mute);
    font-size: 13px;
    text-align: left;
    white-space: nowrap;
    transition: color 0.18s var(--ease), background 0.18s var(--ease);
  }
  .nav:hover { color: var(--body); background: rgba(20, 18, 14, 0.05); }
  /* The active surface belongs to `.marker` now; the button only changes ink, so
     the two never fight over the same pixels mid-slide. */
  .nav.active { color: var(--ink); background: none; }
  .nav { position: relative; z-index: 1; }

  .label {
    flex: 1 1 auto;
    min-width: 0;
    opacity: 1;
    transition: opacity 0.16s var(--ease);
  }

  .count {
    font-size: 11px;
    font-family: var(--mono);
    color: var(--ash);
    font-variant-numeric: tabular-nums;
    transition: opacity 0.16s var(--ease);
  }
  .nav.active .count { color: var(--mute); }

  /* Only one of the two ever shows. */
  .pip {
    position: absolute;
    top: 8px;
    left: 27px;
    width: 5px;
    height: 5px;
    border-radius: 50%;
    background: var(--tier-2);
    opacity: 0;
    transition: opacity 0.16s var(--ease);
  }

  nav.closed .label,
  nav.closed .count { opacity: 0; transition-duration: 0.1s; }
  nav.closed .pip { opacity: 1; transition-delay: 0.14s; }

  .recent { display: flex; flex-direction: column; min-height: 0; margin-top: 14px; }

  .rlabel {
    padding: 0 11px 6px;
    font-size: 11px;
    letter-spacing: 0.3px;
    color: var(--stone);
    flex: 0 0 auto;
  }

  /* Scrolls on its own so a long history never pushes the chain summary out of
     the window — that footer is the one thing the rail must always show. */
  .rlist { overflow-y: auto; min-height: 0; max-height: 42vh; padding-bottom: 2px; }

  .chat {
    display: flex;
    align-items: center;
    border-radius: var(--r-sm);
    transition: background 0.16s var(--ease);
  }
  .chat:hover { background: rgba(20, 18, 14, 0.05); }
  .chat.active { background: rgba(255, 255, 255, 0.72); box-shadow: 0 1px 2px rgba(20, 18, 14, 0.07); }

  .ctitle {
    flex: 1 1 auto;
    min-width: 0;
    height: 30px;
    padding: 0 4px 0 11px;
    text-align: left;
    font-size: 12.5px;
    color: var(--mute);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    transition: color 0.16s var(--ease);
  }
  .chat:hover .ctitle { color: var(--body); }
  .chat.active .ctitle { color: var(--ink); }
  .ctitle:disabled { cursor: default; }

  /* Deleting is permanent, so it stays out of the way until you go looking. */
  .del {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 26px;
    height: 30px;
    flex: 0 0 auto;
    color: var(--stone);
    opacity: 0;
    transition: opacity 0.16s var(--ease), color 0.16s var(--ease);
  }
  .chat:hover .del { opacity: 1; }
  .del:hover { color: var(--error); }

  .spacer { flex: 1 1 auto; }

  .verified {
    border-top: 1px solid var(--line);
    margin-top: 8px;
    padding: 11px 11px 3px;
    font-family: var(--mono);
    font-size: 11px;
    white-space: nowrap;
    transition: opacity 0.16s var(--ease);
  }
  .v-label { color: var(--tier-3); letter-spacing: 0.2px; }
  .v-value { color: var(--mute); margin-top: 4px; }
  .v-head { color: var(--stone); margin-top: 2px; overflow: hidden; text-overflow: ellipsis; }

  /* Folded, the chain still has to report itself — a solid mark, hoverable for
     the full head. Silence here would be the one thing the app can't afford. */
  .v-mark {
    display: none;
    width: 7px;
    height: 7px;
    border-radius: 2px;
    background: var(--tier-3);
    margin: 1px auto 4px;
  }
  nav.closed .verified { padding: 11px 0 3px; }
  nav.closed .v-label,
  nav.closed .v-value,
  nav.closed .v-head { display: none; }
  nav.closed .v-mark { display: block; }

  main { flex: 1 1 auto; min-width: 0; display: flex; flex-direction: column; overflow: hidden; }

  /* The stage owns the space so the outgoing and incoming views can both sit in
     it without pushing each other around. */
  .stage { position: relative; flex: 1 1 auto; min-height: 0; }
  .view { position: absolute; inset: 0; display: flex; flex-direction: column; }

  .error {
    margin: 12px 20px 0;
    padding: 10px 13px;
    border: 1px solid var(--line);
    border-left: 2px solid var(--error);
    background: var(--s1);
    color: var(--body);
    border-radius: var(--r-sm);
    font-family: var(--mono);
    font-size: 12px;
    white-space: pre-wrap;
  }
</style>
