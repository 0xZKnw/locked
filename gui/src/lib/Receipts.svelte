<script>
  import { app, attestationOf } from "./store.svelte.js";
  import Screen from "./Screen.svelte";
  import { slide, fly } from "svelte/transition";
  import { ms, ease, BASE, stagger } from "./motion.js";

  let selected = $state(null);

  const counts = $derived({
    tap: app.receipts.filter((r) => r.attestation === "tap_attested").length,
    source: app.receipts.filter((r) => r.attestation === "source_attested").length,
    harness: app.receipts.filter((r) => r.attestation === "harness_attested").length,
  });

  /** Sentence case, not snake_case: this is a label, not an identifier. */
  const title = (r) =>
    ({
      run_started: "Run opened",
      inference: "Inference",
      tap_call: "Call through TAP",
      sandbox_call: "Sandbox",
      approval_resolved: "Human decision",
      run_finished: "Run closed",
    })[r.event] ?? r.event.replace(/_/g, " ");

  /**
   * The line under the title. Returned as parts so the template can set the one
   * piece that is a literal machine string in mono and leave the rest in prose.
   */
  function detail(r) {
    switch (r.event) {
      case "tap_call":
        return { lead: r.method, mono: r.target_host, trail: `via ${r.credential}` };
      case "inference":
        return { mono: r.model, trail: `${r.input_tokens} in · ${r.output_tokens} out` };
      case "sandbox_call":
        return { mono: r.tool, trail: r.exit_code == null ? "" : `exit ${r.exit_code}` };
      case "run_started":
        return { trail: `${r.tools.length} tools · ${r.isolation || r.sandbox_image || "no sandbox"}` };
      case "run_finished":
        return { trail: `${r.turns} ${r.turns === 1 ? "turn" : "turns"}` };
      case "approval_resolved":
        return { mono: r.txn_id?.slice(0, 8), trail: r.decision };
      default:
        return {};
    }
  }

  const clock = (ts) => (ts ?? "").slice(11, 19);
</script>

<Screen
  title="Receipt chain"
  blurb="Every entry carries the hash of the one before it, so an edit anywhere breaks
         the links after it. What stops the agent rewriting its own history is not the
         hashing though — it is that this file lives outside the workspace it can reach."
>
  {#snippet aside()}
    <div class="legend">
      <span class="tier tap"><i></i>{counts.tap}</span>
      <span class="tier source"><i></i>{counts.source}</span>
      <span class="tier harness"><i></i>{counts.harness}</span>
    </div>
  {/snippet}

  {#if counts.harness > 0 && counts.tap === 0}
    <p class="caveat" transition:slide={{ duration: ms(BASE), easing: ease }}>
      Nothing here has an outside witness yet. Reads and inferences are attested by
      this journal alone — TAP issues no transaction id for a read it approves
      automatically. Writes are the entries someone else could corroborate.
    </p>
  {/if}

  <ol class="chain">
    {#each app.receipts as r, i (r.digest)}
      {@const a = attestationOf(r)}
      {@const d = detail(r)}
      <li class={a.tier} class:open={selected === r.digest}
          in:fly={{ y: 6, duration: ms(BASE), delay: stagger(i), easing: ease }}>
        <!-- The link, drawn. The chain is the product; stating it in prose and
             then rendering a flat list would be describing rather than showing. -->
        <span class="node" aria-hidden="true"></span>

        <button onclick={() => (selected = selected === r.digest ? null : r.digest)}>
          <span class="line1">
            <span class="what">{title(r)}</span>
            <span class="when">{clock(r.ts)}</span>
          </span>
          <span class="line2">
            {#if d.lead}<span class="lead">{d.lead}</span>{/if}
            {#if d.mono}<code>{d.mono}</code>{/if}
            {#if d.trail}<span class="trail">{d.trail}</span>{/if}
            <span class="attest">{a.label}</span>
          </span>
        </button>

        {#if selected === r.digest}
          <div class="expand" transition:slide={{ duration: ms(BASE), easing: ease }}>
            <div class="kv"><span>this</span><code>{r.digest}</code></div>
            <div class="kv"><span>follows</span><code>{r.prev}</code></div>
            <div class="kv"><span>at</span><code>{r.ts}</code></div>
            {#if a.detail}<div class="kv"><span>witness</span><code>{a.detail}</code></div>{/if}
          </div>
        {/if}
      </li>
    {:else}
      <li class="empty">Nothing yet. The journal is written as the agent acts.</li>
    {/each}
  </ol>

  {#snippet foot()}
    {#if app.config}<span>journal · <code>{app.config.journal}</code></span>{/if}
  {/snippet}
</Screen>

<style>
  .legend { display: flex; gap: 12px; align-items: center; }
  .tier {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: 11.5px;
    color: var(--ash);
    font-variant-numeric: tabular-nums;
  }
  /* The value ladder, made literal: corroborated by someone else = solid and
     dark; our word alone = a hollow outline. Brightness is the argument. */
  .tier i { width: 8px; height: 8px; border-radius: 2px; box-shadow: inset 0 0 0 1px var(--tier-1); }
  .tier.tap i { background: var(--tier-3); box-shadow: none; }
  .tier.source i { background: var(--tier-2); box-shadow: none; }

  .caveat {
    margin: 4px 0 16px;
    padding: 11px 14px;
    border-radius: var(--r-md);
    background: rgba(255, 255, 255, 0.66);
    box-shadow: inset 0 0 0 1px rgba(20, 18, 14, 0.06);
    color: var(--mute);
    font-size: 12.5px;
    line-height: 1.6;
  }

  .chain { list-style: none; margin: 0; padding: 0 0 0 20px; }

  li { position: relative; padding-bottom: 2px; }

  /* The connector: a hairline from each node down to the next. */
  li::before {
    content: "";
    position: absolute;
    left: -12px;
    top: 20px;
    bottom: -2px;
    width: 1px;
    background: var(--line);
  }
  li:last-child::before { display: none; }

  .node {
    position: absolute;
    left: -16px;
    top: 15px;
    width: 9px;
    height: 9px;
    border-radius: 2px;
    background: var(--canvas);
    box-shadow: inset 0 0 0 1px var(--tier-1);
  }
  li.tap .node { background: var(--tier-3); box-shadow: none; }
  li.source .node { background: var(--tier-2); box-shadow: none; }

  li > button {
    display: block;
    width: 100%;
    text-align: left;
    padding: 9px 12px;
    border-radius: var(--r-md);
    transition: background 0.16s var(--ease), box-shadow 0.16s var(--ease);
  }
  li > button:hover { background: rgba(255, 255, 255, 0.6); }
  li.open > button {
    background: rgba(255, 255, 255, 0.82);
    box-shadow: 0 1px 2px rgba(20, 18, 14, 0.06);
  }

  .line1 { display: flex; align-items: baseline; justify-content: space-between; gap: 12px; }
  .what { color: var(--ink); font-size: 13px; }
  .when {
    color: var(--stone);
    font-size: 11px;
    font-family: var(--mono);
    font-variant-numeric: tabular-nums;
  }

  .line2 {
    display: flex;
    align-items: baseline;
    gap: 8px;
    margin-top: 2px;
    font-size: 12px;
    color: var(--mute);
    min-width: 0;
  }
  .lead { color: var(--body); }
  /* Mono earns its place only on strings you might copy. */
  .line2 code { font-family: var(--mono); font-size: 11.5px; color: var(--body); }
  .trail { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .attest { margin-left: auto; color: var(--ash); font-size: 11.5px; white-space: nowrap; }
  li.tap .attest { color: var(--ink); }
  li.source .attest { color: var(--mute); }

  .expand {
    margin: 2px 0 8px 12px;
    padding: 10px 14px;
    border-radius: var(--r-md);
    background: rgba(255, 255, 255, 0.55);
    box-shadow: inset 0 0 0 1px rgba(20, 18, 14, 0.05);
  }
  .kv { display: flex; gap: 14px; padding: 3px 0; font-size: 11.5px; align-items: baseline; }
  .kv span { color: var(--ash); width: 56px; flex: 0 0 auto; }
  .kv code { font-family: var(--mono); color: var(--mute); word-break: break-all; }

  .empty { color: var(--ash); padding: 40px 0; text-align: center; }
  .empty::before { display: none; }

</style>
