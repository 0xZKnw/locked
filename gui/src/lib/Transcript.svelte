<script>
  import { app, startRun, attach, detach } from "./store.svelte.js";
  import Markdown from "./Markdown.svelte";
  import Icon from "./Icon.svelte";
  import { fly, scale, slide, fade } from "svelte/transition";
  import { flip } from "svelte/animate";
  import { ms, ease, springOut, QUICK, BASE, CALM } from "./motion.js";

  // `$state` because `bind:this` reassigns them and two are read inside effects:
  // a plain `let` never re-runs the effect when the element is swapped, so after
  // a chat switch the tail-following effect would hold a node that is gone.
  let scroller = $state(null);
  let picker = $state(null);
  let field = $state(null);

  /**
   * Grow the field to fit what is in it.
   *
   * Height has to be reset before it is measured, or the box can only ever get
   * taller — `scrollHeight` of an already-tall element includes the slack.
   */
  function fit() {
    if (!field) return;
    field.style.height = "auto";
    field.style.height = `${Math.min(field.scrollHeight, 176)}px`;
  }

  $effect(() => {
    app.task;
    fit();
  });

  const src = (img) => `data:${img.media_type};base64,${img.data}`;

  /** Enough of a transaction to recognise it; the whole thing is on hover. */
  const short = (id) => (id ?? "").slice(0, 8);

  /** Paste is how anyone actually attaches a screenshot. */
  function onPaste(e) {
    const files = [...(e.clipboardData?.files ?? [])];
    if (!files.length) return;
    e.preventDefault();
    attach(files);
  }

  function onDrop(e) {
    const files = [...(e.dataTransfer?.files ?? [])];
    if (!files.length) return;
    e.preventDefault();
    dragging = false;
    attach(files);
  }

  let dragging = $state(false);

  /**
   * Follow the tail — but only while the reader is already there.
   *
   * The pump releases text a few characters at a time, so an unconditional
   * scroll-to-bottom fires dozens of times a second and yanks the view back down
   * the instant you try to read anything above. Scrolling up is how you say "let
   * me look at this", so it stops the following until you come back.
   */
  let stick = $state(true);
  const NEAR_BOTTOM = 64;

  function onScroll() {
    if (!scroller) return;
    const gap = scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight;
    stick = gap <= NEAR_BOTTOM;
  }

  function toBottom() {
    stick = true;
    scroller?.scrollTo({ top: scroller.scrollHeight, behavior: "smooth" });
  }

  // A different chat starts at its own bottom.
  $effect(() => {
    app.session?.id;
    stick = true;
  });

  $effect(() => {
    app.timeline.length;
    app.timeline.at(-1)?.text;
    if (scroller && stick) scroller.scrollTop = scroller.scrollHeight;
  });

  function onKey(e) {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      startRun();
    }
  }
</script>

<div class="wrap">
  <!-- Keyed on the chat, so opening one — or starting a fresh one — crossfades
       the whole transcript instead of blanking it. Each session getting its own
       scroller is the useful side effect: scroll position no longer leaks from
       the chat you left into the one you opened. -->
  <div class="stage">
    {#key app.session?.id}
      <div
        class="stream"
        bind:this={scroller}
        onscroll={onScroll}
        in:fly={{ y: 10, duration: ms(CALM), easing: ease }}
        out:fade={{ duration: ms(BASE), easing: ease }}
      >
       <div class="col">
    {#if app.timeline.length === 0}
      <div class="empty" out:fly|global={{ y: -10, duration: ms(BASE), easing: ease }}>
        <h1>One door, and a receipt for everything through it.</h1>
        <p>
          This agent reaches the network only through TAP, which holds the
          credentials and pauses writes for you. Every action lands in a chained
          journal you can read under Receipts.
        </p>
      </div>
    {/if}

    {#each app.timeline as item, i (i)}
      {#if item.kind === "task"}
        <div class="task" in:fly|global={{ y: 8, duration: ms(BASE), easing: ease }}>
          {#if item.images?.length}
            <div class="shots">
              {#each item.images as img, n (n)}
                <img src={src(img)} alt={img.name ?? "attached image"} />
              {/each}
            </div>
          {/if}
          {#if item.text}<div>{item.text}</div>{/if}
        </div>
      {:else if item.kind === "thinking"}
        <div class="thinking" in:fly|global={{ y: 6, duration: ms(QUICK), easing: ease }}>
          <button class="think" class:on={item.expanded} onclick={() => (item.expanded = !item.expanded)}>
            <span class="chev" class:down={item.expanded}><Icon name="chev" /></span>
            {item.open ? "Thinking" : "Reasoning"}
          </button>
          {#if item.expanded}
            <div class="tbody">{item.text}</div>
          {/if}
        </div>
      {:else if item.kind === "text"}
        <div class="answer" in:fly|global={{ y: 8, duration: ms(BASE), easing: ease }}>
          <Markdown source={item.text} streaming={item.open} />
        </div>
      {:else if item.kind === "tool"}
        <div class="tool" class:err={item.status === "error"} class:live={item.status === "running"} in:fly|global={{ y: 6, duration: ms(QUICK), easing: ease }}>
          <!-- The house mark: a rounded square, breathing while the call is out,
               solid once it lands. The same shape the header and the receipt
               chain use, so one glance means the same thing everywhere. -->
          <span class="mark" aria-hidden="true"></span>
          <code class="name">{item.name}</code>
          {#if item.summary && item.summary !== item.name}
            <span class="summary">{item.summary}</span>
          {/if}
        </div>
      {:else if item.kind === "approval"}
        <div class="approval" in:fly|global={{ y: 8, duration: ms(BASE), easing: ease }}>
          <!-- The same breathing dot the Approvals screen uses: one state, drawn
               one way, wherever you meet it. -->
          <span class="pulse" aria-hidden="true"></span>
          <div class="body">
            <div class="lede">Waiting on a human <code title={item.txn_id}>{short(item.txn_id)}</code></div>
            <div class="note">The run carries on meanwhile. Approve in Telegram or the dashboard.</div>
          </div>
        </div>
      {:else if item.kind === "approval_resolved"}
        <div class="resolved" in:fly|global={{ y: 6, duration: ms(QUICK), easing: ease }}>
          <span class="verdict-mark" class:refused={item.decision !== "approved"} aria-hidden="true"></span>
          <code title={item.txn_id}>{short(item.txn_id)}</code>
          <span>{item.decision}</span>
        </div>
      {:else if item.kind === "compacted"}
        <!-- A seam in the conversation, drawn as one: everything above it the
             agent now knows only as a summary. -->
        <div class="seam" in:fly|global={{ y: 6, duration: ms(QUICK), easing: ease }}>
          <span class="rule" aria-hidden="true"></span>
          <span class="seam-label">
            {item.dropped} earlier {item.dropped === 1 ? "message" : "messages"} folded into a summary
          </span>
          <span class="rule" aria-hidden="true"></span>
        </div>
      {:else if item.kind === "note"}
        <!-- An operator message replayed from a reopened chat. -->
        <div class="resolved">{item.text}</div>
      {/if}
    {/each}
       </div>
      </div>
    {/key}
  </div>

  <!-- Only while you have stepped away from the tail: otherwise it is a button
       that does nothing, sitting over the conversation. -->
  {#if !stick}
    <button class="catch-up" onclick={toBottom} transition:fly={{ y: 8, duration: ms(QUICK), easing: ease }}>
      <span class="chev-down"><Icon name="chev" /></span>
      {app.running ? "still writing" : "latest"}
    </button>
  {/if}

  <div class="dock">
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div
      class="composer"
      class:busy={app.running}
      class:dragging
      ondrop={onDrop}
      ondragover={(e) => { e.preventDefault(); dragging = true; }}
      ondragleave={() => (dragging = false)}
    >
      {#if app.attachments.length}
        <div class="tray" transition:slide={{ duration: ms(BASE), easing: ease }}>
          {#each app.attachments as img, n (n)}
            <div
              class="chip"
              animate:flip={{ duration: ms(BASE), easing: ease }}
              in:scale|global={{ start: 0.4, duration: ms(CALM), easing: springOut }}
              out:scale|global={{ start: 0.75, duration: ms(QUICK), easing: ease }}
            >
              <img src={src(img)} alt={img.name} />
              <button class="drop" onclick={() => detach(n)} title="Remove" aria-label="Remove">×</button>
            </div>
          {/each}
        </div>
      {/if}

      <div class="row">
        <button
          class="clip"
          onclick={() => picker.click()}
          disabled={app.running}
          title="Attach an image"
          aria-label="Attach an image"
        >
          <Icon name="image" />
        </button>
        <input
          class="hidden"
          type="file"
          accept="image/png,image/jpeg,image/gif,image/webp"
          multiple
          bind:this={picker}
          onchange={(e) => { attach([...e.currentTarget.files]); e.currentTarget.value = ""; }}
        />

        <textarea
          bind:value={app.task}
          bind:this={field}
          onkeydown={onKey}
          onpaste={onPaste}
          placeholder="What should it do?"
          rows="1"
          disabled={app.running}
        ></textarea>

          <button
          class="go"
          class:working={app.running}
          onclick={startRun}
          disabled={app.running || (!app.task.trim() && !app.attachments.length)}
          title={app.running ? "Running" : "Run  ⏎"}
          aria-label="Run"
        >
          {#key app.running}
            <span class="swap" in:fade={{ duration: ms(QUICK) }}>
              {#if app.running}
                <svg class="spinner" viewBox="0 0 24 24" width="15" height="15" aria-hidden="true">
                  <circle cx="12" cy="12" r="9" fill="none" stroke="rgba(255,255,255,0.22)" stroke-width="2.6" />
                  <circle
                    cx="12" cy="12" r="9" fill="none"
                    stroke="#fff" stroke-width="2.6" stroke-linecap="round"
                    stroke-dasharray="15 42"
                  />
                </svg>
              {:else}
                <Icon name="send" />
              {/if}
            </span>
          {/key}
        </button>
      </div>
    </div>
  </div>
</div>

<style>
  /* The one place the transcript admits it lost something. Quiet, but a line
     across the whole column — the conversation genuinely has two halves now. */
  .seam {
    display: flex;
    align-items: center;
    gap: 10px;
    margin: 18px 0 14px;
  }
  .seam .rule {
    flex: 1 1 auto;
    height: 1px;
    background: linear-gradient(
      90deg,
      transparent,
      rgba(20, 18, 14, 0.14) 30%,
      rgba(20, 18, 14, 0.14) 70%,
      transparent
    );
  }
  .seam-label {
    flex: 0 0 auto;
    font-size: 10.5px;
    letter-spacing: 0.2px;
    color: var(--ash);
  }

  .wrap { display: flex; flex-direction: column; height: 100%; min-height: 0; position: relative; }

  /* The stage holds the space so the outgoing and incoming transcripts can share
     it. Without it they stack in flow and the scroller lurches. */
  .stage { position: relative; flex: 1 1 auto; min-height: 0; }
  .stream { position: absolute; inset: 0; overflow-y: auto; padding: 24px 28px 12px; }

  /* One reading column, shared by the transcript and the composer below it. A
     full-bleed input under centred content reads as two unrelated layouts. */
  .col { max-width: var(--col); margin: 0 auto; }

  /* Out of flow, so it can dissolve while the first message flies in above it
     without the two ever fighting over the same vertical space. */
  .empty {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
    justify-content: center;
    /* Centred by the container, not by auto margins on the children: `.empty p`
       sets `margin: 0`, which silently wiped the auto margins and dropped the
       paragraph to the left edge. Alignment that a child's own margin can undo
       is not alignment. */
    align-items: center;
    padding: 0 28px 10vh;
    text-align: center;
    pointer-events: none;
  }
  .empty > * { max-width: 470px; }
  .empty h1 {
    font-size: 26px;
    font-weight: 450;
    letter-spacing: -0.5px;
    line-height: 1.4;
    margin-bottom: 10px;
  }
  .empty p { color: var(--mute); margin: 0; }

  .task {
    background: rgba(255, 255, 255, 0.8);
    border: 1px solid rgba(20, 18, 14, 0.07);
    border-radius: var(--r-lg);
    padding: 10px 14px;
    margin: 14px 0 4px;
    color: var(--ink);
    box-shadow: 0 1px 2px rgba(20, 18, 14, 0.04);
  }

  .thinking { margin: 2px 0 10px; }

  /* Reasoning is secondary by default: a quiet chip that grows a surface under
     the cursor, rather than a line of console output with a plus sign on it. */
  .think {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    height: 26px;
    padding: 0 10px 0 7px;
    margin-left: -7px;
    border-radius: var(--r-pill);
    color: var(--ash);
    font-size: 12px;
    letter-spacing: 0.1px;
    transition: color 0.18s var(--ease), background 0.18s var(--ease);
  }
  .think:hover { color: var(--body); background: rgba(20, 18, 14, 0.05); }
  .think.on { color: var(--mute); }

  .chev {
    display: flex;
    transition: transform 0.22s var(--ease);
  }
  .chev.down { transform: rotate(90deg); }

  .tbody {
    margin: 4px 0 4px 5px;
    padding-left: 14px;
    border-left: 1px solid var(--line);
    white-space: pre-wrap;
    color: var(--ash);
    font-size: 12.5px;
    line-height: 1.65;
    animation: fade 0.2s var(--ease) both;
  }

  .answer { margin: 4px 0 14px; color: var(--body); }

  /* An action the run took, drawn like the rest of the app: a quiet row that
     grows a surface under the cursor, monospace only on the tool's own name. */
  .tool {
    display: flex;
    align-items: baseline;
    gap: 9px;
    padding: 6px 10px;
    margin: 1px 0;
    margin-left: -10px;
    border-radius: var(--r-sm);
    font-size: 12.5px;
    min-width: 0;
    transition: background 0.16s var(--ease);
  }
  .tool:hover { background: rgba(255, 255, 255, 0.55); }

  .tool .mark {
    width: 8px; height: 8px; flex: 0 0 auto;
    align-self: center;
    border-radius: 2px;
    background: var(--tier-3);
  }
  .tool.live .mark { background: var(--tier-2); animation: breathe 1.6s ease-in-out infinite; }
  .tool.err .mark { background: var(--error); }

  /* The tool name is a literal you could type into a call; the sentence after it
     is not, so it stops wearing monospace. */
  .name { font-family: var(--mono); font-size: 12px; color: var(--ink); flex: 0 0 auto; }
  .summary {
    color: var(--mute);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0;
  }
  .tool.err .summary { color: var(--error); }

  /* The same surface as everything else in the transcript: translucent white and
     a hairline, no grey fill and no hard border. */
  .approval {
    display: flex;
    align-items: flex-start;
    gap: 11px;
    padding: 11px 14px;
    margin: 10px 0;
    border-radius: var(--r-lg);
    background: rgba(255, 255, 255, 0.72);
    box-shadow:
      inset 0 0 0 1px rgba(20, 18, 14, 0.06),
      0 1px 2px rgba(20, 18, 14, 0.04);
  }
  .pulse {
    width: 8px; height: 8px; flex: 0 0 auto;
    margin-top: 5px;
    border-radius: 50%;
    background: var(--amber);
    animation: breathe 1.9s ease-in-out infinite;
  }
  @keyframes breathe {
    0%, 100% { opacity: 1; transform: scale(1); }
    50% { opacity: 0.35; transform: scale(0.82); }
  }
  .body { min-width: 0; }
  .lede { font-size: 13px; color: var(--ink); }
  .note { color: var(--ash); font-size: 12px; margin-top: 2px; line-height: 1.5; }

  /* Mono only on the identifier, and only as much of it as you need to
     recognise the transaction — the whole id is one hover away. */
  .approval code, .resolved code {
    font-family: var(--mono);
    font-size: 11.5px;
    color: var(--mute);
    cursor: default;
  }

  .resolved {
    display: flex;
    align-items: center;
    gap: 8px;
    color: var(--mute);
    font-size: 12.5px;
    padding: 6px 2px;
  }
  .verdict-mark {
    width: 7px; height: 7px; flex: 0 0 auto;
    border-radius: 50%;
    background: var(--tier-3);
  }
  .verdict-mark.refused { background: var(--error); }

  /* Composer. A plain surface with a hairline — the whole point of dropping the
     glass was that a control should be obvious, not atmospheric. */
  .catch-up {
    position: absolute;
    left: 50%;
    bottom: 86px;
    transform: translateX(-50%);
    z-index: 3;
    display: flex;
    align-items: center;
    gap: 7px;
    height: 30px;
    padding: 0 13px 0 10px;
    border-radius: var(--r-pill);
    background: rgba(255, 255, 255, 0.9);
    backdrop-filter: blur(10px) saturate(1.3);
    box-shadow:
      inset 0 0 0 1px rgba(20, 18, 14, 0.07),
      0 6px 18px -8px rgba(20, 18, 14, 0.45);
    color: var(--body);
    font-size: 12px;
    white-space: nowrap;
    transition: background 0.18s var(--ease), transform 0.18s var(--ease);
  }
  .catch-up:hover { background: #fff; transform: translateX(-50%) translateY(-1px); }
  .chev-down { display: flex; transform: rotate(90deg); color: var(--mute); }

  .dock { flex: 0 0 auto; padding: 8px 28px 18px; }

  .composer {
    max-width: var(--col);
    margin: 0 auto;
    display: flex;
    flex-direction: column;
    /* Symmetric: the 1px difference showed up as the left disc sitting closer
       to the edge than the right one. */
    padding: 7px;
    background: rgba(255, 255, 255, 0.86);
    backdrop-filter: blur(18px) saturate(1.4);
    border: 1px solid rgba(20, 18, 14, 0.08);
    border-radius: 24px;
    box-shadow:
      0 1px 2px rgba(20, 18, 14, 0.05),
      0 8px 26px -10px rgba(20, 18, 14, 0.18);
    transition: box-shadow 0.22s var(--ease), border-color 0.22s var(--ease);
  }
  .composer:focus-within {
    border-color: rgba(20, 18, 14, 0.14);
    box-shadow:
      0 1px 2px rgba(20, 18, 14, 0.06),
      0 14px 38px -12px rgba(20, 18, 14, 0.26);
  }
  /* Dropping is the fastest way in, so it says so while you hover. */
  .composer.dragging {
    border-color: var(--line-strong);
    box-shadow:
      inset 0 0 0 1px rgba(20, 18, 14, 0.1),
      0 14px 38px -12px rgba(20, 18, 14, 0.3);
  }

  .row { display: flex; align-items: flex-end; gap: 8px; }

  /* Attached images sit above the field rather than inside it: they are part of
     the message, not part of the sentence. */
  .tray {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    padding: 5px 6px 9px 8px;
  }
  .chip {
    position: relative;
    /* The spring scales about the centre, so the thumbnail expands into its own
       footprint rather than growing out of a corner. */
    transform-origin: 50% 50%;
  }
  .chip img {
    display: block;
    width: 54px;
    height: 54px;
    object-fit: cover;
    border-radius: var(--r-md);
    box-shadow:
      inset 0 0 0 1px rgba(20, 18, 14, 0.1),
      0 1px 2px rgba(20, 18, 14, 0.06);
    transition: box-shadow 0.18s var(--ease), transform 0.18s var(--ease);
  }
  .chip:hover img {
    transform: translateY(-1px);
    box-shadow:
      inset 0 0 0 1px rgba(20, 18, 14, 0.14),
      0 4px 10px -3px rgba(20, 18, 14, 0.25);
  }
  .drop {
    position: absolute;
    top: -5px;
    right: -5px;
    width: 18px;
    height: 18px;
    border-radius: 50%;
    background: var(--ink);
    color: var(--canvas);
    font-size: 13px;
    line-height: 1;
    display: flex;
    align-items: center;
    justify-content: center;
    box-shadow: 0 1px 3px rgba(20, 18, 14, 0.35);
  }

  /* The same disc as Run, one weight down. Two controls of the same silhouette
     read as a pair bracketing the field; a pale glyph tucked in front of the
     placeholder read as a label for it, which is why it never looked clickable. */
  .clip {
    display: flex;
    align-items: center;
    justify-content: center;
    flex: 0 0 auto;
    width: 34px;
    height: 34px;
    border-radius: 50%;
    color: var(--mute);
    background: rgba(20, 18, 14, 0.05);
    box-shadow: inset 0 0 0 1px rgba(20, 18, 14, 0.04);
    transition:
      color 0.18s var(--ease),
      background 0.18s var(--ease),
      box-shadow 0.18s var(--ease),
      transform 0.18s var(--ease);
  }
  .clip:hover:not(:disabled) {
    color: var(--ink);
    background: rgba(20, 18, 14, 0.09);
    box-shadow: inset 0 0 0 1px rgba(20, 18, 14, 0.07);
    transform: translateY(-1px);
  }
  .clip:active:not(:disabled) { transform: scale(0.94); background: rgba(20, 18, 14, 0.13); }
  .clip:disabled { color: var(--stone); background: rgba(20, 18, 14, 0.03); box-shadow: none; cursor: default; }

  .hidden { display: none; }

  /* Thumbnails inside a sent turn. */
  .shots { display: flex; flex-wrap: wrap; gap: 8px; margin-bottom: 8px; }
  .shots img {
    display: block;
    max-width: 200px;
    max-height: 160px;
    border-radius: var(--r-md);
    box-shadow: inset 0 0 0 1px rgba(20, 18, 14, 0.1);
  }

  textarea {
    flex: 1 1 auto;
    min-width: 0;
    resize: none;
    background: none;
    border: none;
    color: var(--ink);
    /* The disc already provides the inset, so the text starts right after it
       instead of being pushed a second time. */
    padding: 6.5px 6px 6.5px 9px;
    font-family: var(--sans);
    font-weight: 450;
    font-size: 13.5px;
    line-height: 1.55;
    /* One line of this field is exactly as tall as the discs beside it, so
       `flex-end` and "vertically centred" are the same place at rest — and the
       row still grows downward past them when the text wraps, which is what
       flex-end is there for. 13.5px x 1.55 leading = 20.9, so 6.5 of padding
       either side lands on 34. */
    min-height: 34px;
    max-height: 176px;
    transition: height 0.18s var(--ease);
    outline: none;
  }
  textarea::placeholder { color: var(--ash); }
  textarea { transition: opacity 0.22s var(--ease); }
  textarea:disabled { opacity: 0.72; }

  /* A disc, not a labelled slab. Every worded version of this button read cheap
     at this size, and the word was never carrying anything — the field states
     what it does, the arrow says go. 34px inside 7px of padding sits it exactly
     concentric with the composer's 24px radius.

     The dimension comes from lighting, not from a border: a top inner highlight
     and a bottom inner shade make it read as a raised object, and the drop
     shadow sits it above the composer surface. */
  .go {
    display: flex;
    align-items: center;
    justify-content: center;
    flex: 0 0 auto;
    width: 34px;
    height: 34px;
    border-radius: 50%;
    background: linear-gradient(180deg, #38352d 0%, #1b1913 58%, #131209 100%);
    color: #fdfcfa;
    box-shadow:
      inset 0 1px 0 rgba(255, 255, 255, 0.18),
      inset 0 -1px 0 rgba(0, 0, 0, 0.35),
      0 1px 2px rgba(20, 18, 14, 0.24),
      0 7px 18px -9px rgba(20, 18, 14, 0.55);
    transition:
      transform 0.2s var(--ease),
      box-shadow 0.2s var(--ease),
      background 0.2s var(--ease),
      color 0.2s var(--ease);
  }
  .go:hover:not(:disabled) {
    transform: translateY(-1px) scale(1.05);
    box-shadow:
      inset 0 1px 0 rgba(255, 255, 255, 0.22),
      inset 0 -1px 0 rgba(0, 0, 0, 0.35),
      0 2px 4px rgba(20, 18, 14, 0.2),
      0 12px 26px -10px rgba(20, 18, 14, 0.6);
  }
  .go:active:not(:disabled) {
    transform: translateY(0) scale(0.95);
    background: linear-gradient(180deg, #1b1913 0%, #26241d 100%);
    box-shadow:
      inset 0 2px 4px rgba(0, 0, 0, 0.5),
      0 1px 1px rgba(20, 18, 14, 0.16);
  }
  /* Disabled is the resting state of an empty field, so it has to look composed
     rather than broken: same silhouette, no lighting, no promise. */
  /* Both states occupy the same spot while they trade, so the disc never looks
     momentarily empty. */
  .go { position: relative; }
  .swap { position: absolute; inset: 0; display: flex; align-items: center; justify-content: center; }

  .go:disabled {
    background: rgba(20, 18, 14, 0.045);
    color: var(--stone);
    box-shadow: inset 0 0 0 1px rgba(20, 18, 14, 0.05);
    cursor: default;
  }

  /* Running is not the same state as nothing-to-send, and it was being drawn
     that way: the black disc dropped to a near-transparent grey the instant you
     pressed send, which reads as the control breaking rather than working. A
     busy button keeps its weight — it simply stops accepting a second press. */
  .go.working:disabled {
    background: linear-gradient(180deg, #38352d 0%, #1b1913 58%, #131209 100%);
    color: #fdfcfa;
    box-shadow:
      inset 0 1px 0 rgba(255, 255, 255, 0.18),
      inset 0 -1px 0 rgba(0, 0, 0, 0.35),
      0 1px 2px rgba(20, 18, 14, 0.24),
      0 7px 18px -9px rgba(20, 18, 14, 0.55);
  }
  .spinner {
    display: block;
    transform-origin: 50% 50%;
    animation: spin 0.8s linear infinite;
  }


</style>
