<script>
  import { app, loadCapabilities } from "./store.svelte.js";
  import Screen from "./Screen.svelte";
  import { fly } from "svelte/transition";
  import { ms, ease, BASE, stagger } from "./motion.js";

  const gated = $derived(app.capabilities.filter((c) => !c.writes_auto_approve).length);
</script>

<Screen
  title="Capabilities"
  blurb="Everything this run can reach, and nothing else. The agent names a credential;
         it never holds one. A credential can only be sent to the hosts TAP binds it
         to, so pointing it somewhere else is refused before the secret is injected."
>
  {#snippet aside()}
    <button class="refresh" onclick={loadCapabilities}>Refresh</button>
  {/snippet}

  {#if app.capabilities.length}
    <div class="tally">
      {app.capabilities.length}
      {app.capabilities.length === 1 ? "credential" : "credentials"}
      {#if gated}· {gated} pause for you on write{/if}
    </div>
  {/if}

  <div class="grid">
    {#each app.capabilities as c, i (c.name)}
      <div class="card" class:gated={!c.writes_auto_approve}
           in:fly={{ y: 8, duration: ms(BASE), delay: stagger(i), easing: ease }}>
        <div class="top">
          <code class="name">{c.name}</code>
          <span class="gate">
            <i aria-hidden="true"></i>
            {c.writes_auto_approve ? "straight through" : "writes pause"}
          </span>
        </div>
        {#if c.description}<p class="desc">{c.description}</p>{/if}
        <div class="shape">{c.target_shape.replace(/_/g, " ")} target</div>
      </div>
    {:else}
      <p class="empty">
        No credentials loaded. If this stays empty, check that a TAP key is
        reachable — <code>TAP_API_KEY</code>, or <code>~/.tap/agent.json</code>.
      </p>
    {/each}
  </div>

  {#snippet foot()}
    {#if app.config}
      <span>tools this run offers · <code>{app.config.tools.join("  ")}</code></span>
    {/if}
  {/snippet}
</Screen>

<style>
  .tally { color: var(--ash); font-size: 11.5px; margin: 2px 0 14px; }

  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(248px, 1fr));
    gap: 10px;
    align-content: start;
  }

  .card {
    padding: 13px 15px 14px;
    border-radius: var(--r-lg);
    background: rgba(255, 255, 255, 0.72);
    box-shadow:
      inset 0 0 0 1px rgba(20, 18, 14, 0.06),
      0 1px 2px rgba(20, 18, 14, 0.04);
  }

  .top { display: flex; align-items: baseline; justify-content: space-between; gap: 10px; }

  /* A credential name is a literal you type into a tool call, so it is mono. The
     sentence next to it is not. */
  .name { font-family: var(--mono); font-size: 12.5px; color: var(--ink); }

  .gate {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    font-size: 10.5px;
    color: var(--ash);
    white-space: nowrap;
  }
  .gate i { width: 5px; height: 5px; border-radius: 50%; background: var(--tier-3); }
  .card.gated .gate { color: var(--amber); }
  .card.gated .gate i { background: var(--amber); }

  .desc { margin: 7px 0 0; color: var(--mute); font-size: 12.5px; line-height: 1.5; }
  .shape { margin-top: 8px; color: var(--stone); font-size: 11px; }

  .refresh {
    font-size: 12px;
    color: var(--mute);
    border-radius: var(--r-sm);
    padding: 6px 12px;
    box-shadow: inset 0 0 0 1px rgba(20, 18, 14, 0.08);
    transition: color 0.18s var(--ease), background 0.18s var(--ease);
  }
  .refresh:hover { color: var(--ink); background: rgba(255, 255, 255, 0.7); }

  .empty {
    grid-column: 1 / -1;
    color: var(--ash);
    padding: 44px 0;
    text-align: center;
    line-height: 1.7;
    max-width: 46ch;
    margin: 0 auto;
  }
  .empty code { font-family: var(--mono); color: var(--mute); }

</style>
