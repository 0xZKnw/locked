<script>
  import { app, refreshApprovals } from "./store.svelte.js";
  import Screen from "./Screen.svelte";
  import { fly } from "svelte/transition";
  import { flip } from "svelte/animate";
  import { ms, ease, BASE, CALM } from "./motion.js";

  const pending = $derived(app.approvals.filter((a) => !a.decision));
  const settled = $derived(app.approvals.filter((a) => a.decision));

  let checking = $state(false);

  async function check() {
    if (checking) return;
    checking = true;
    await refreshApprovals();
    checking = false;
  }

  /**
   * While anything is waiting, ask TAP about it.
   *
   * The run reconciles approvals at the top of its next turn, which is fine
   * mid-run and useless once the run has ended — a write approved a minute later
   * would sit here saying "waiting" forever. Polling stops the moment nothing is
   * pending, and never runs during a run: the loop is writing to the same chain
   * then, and two writers would fork the hash links.
   */
  $effect(() => {
    if (!pending.length || app.running) return;
    check();
    const id = setInterval(check, 4000);
    return () => clearInterval(id);
  });
</script>

<Screen
  title="Approvals"
  blurb="Writes pause for a human. You decide where TAP already reaches you — Telegram
         or the dashboard — not here; this screen is the run's side of that
         conversation. The agent is told to carry on meanwhile, so a pending write
         never freezes a run."
>
  {#snippet aside()}
    {#if pending.length}
      <button class="recheck" onclick={check} disabled={checking || app.running}>
        {checking ? "checking…" : "check now"}
      </button>
    {/if}
  {/snippet}

  {#if pending.length}
    <h3>Waiting on you</h3>
    {#each pending as a (a.txn_id)}
      <!-- Keyed and flipped, so approving something slides the card down into
           the decided list instead of deleting it here and drawing a new one
           there. The movement is the receipt of your decision. -->
      <div class="card waiting" animate:flip={{ duration: ms(CALM) }}
           in:fly|global={{ y: 8, duration: ms(BASE), easing: ease }}>
        <span class="pulse" aria-hidden="true"></span>
        <div class="meta">
          <div class="what">{a.summary}</div>
          <code>{a.txn_id}</code>
        </div>
        <span class="where">approve in Telegram</span>
      </div>
    {/each}
  {/if}

  {#if settled.length}
    <h3>Decided</h3>
    {#each settled as a (a.txn_id)}
      <div class="card" class:refused={a.decision !== "approved"}
           animate:flip={{ duration: ms(CALM) }}
           in:fly|global={{ y: 8, duration: ms(BASE), easing: ease }}>
        <span class="mark" aria-hidden="true"></span>
        <div class="meta">
          <div class="what">{a.summary}</div>
          <code>{a.txn_id}</code>
        </div>
        <span class="verdict">{a.decision}</span>
      </div>
    {/each}
  {/if}

  {#if !app.approvals.length}
    <p class="empty">
      No write has needed a decision yet. Reads run straight through; only calls
      that change something on the other side pause here.
    </p>
  {/if}
</Screen>

<style>
  h3 {
    font-size: 11.5px;
    font-weight: 500;
    letter-spacing: 0.2px;
    color: var(--ash);
    margin: 18px 0 8px;
  }
  h3:first-child { margin-top: 6px; }

  .card {
    display: flex;
    align-items: center;
    gap: 13px;
    padding: 13px 15px;
    margin-bottom: 8px;
    border-radius: var(--r-lg);
    background: rgba(255, 255, 255, 0.72);
    box-shadow:
      inset 0 0 0 1px rgba(20, 18, 14, 0.06),
      0 1px 2px rgba(20, 18, 14, 0.04);
  }

  /* A slow breath rather than a spinner: nothing is being computed, someone is
     being waited on. */
  .pulse {
    width: 9px; height: 9px; flex: 0 0 auto;
    border-radius: 50%;
    background: var(--amber);
    animation: breathe 1.9s ease-in-out infinite;
  }
  @keyframes breathe {
    0%, 100% { opacity: 1; transform: scale(1); }
    50% { opacity: 0.35; transform: scale(0.82); }
  }

  .mark {
    width: 9px; height: 9px; flex: 0 0 auto;
    border-radius: 50%;
    background: var(--tier-3);
  }
  .card.refused .mark { background: var(--error); }

  .meta { flex: 1 1 auto; min-width: 0; }
  .what { font-size: 13px; color: var(--ink); }
  .meta code {
    font-family: var(--mono);
    font-size: 11px;
    color: var(--ash);
    word-break: break-all;
  }

  .where { font-size: 11.5px; color: var(--amber); white-space: nowrap; }
  .verdict { font-size: 11.5px; color: var(--mute); white-space: nowrap; }
  .card.refused .verdict { color: var(--error); }

  .recheck {
    font-size: 12px;
    color: var(--mute);
    border-radius: var(--r-sm);
    padding: 6px 12px;
    box-shadow: inset 0 0 0 1px rgba(20, 18, 14, 0.08);
    white-space: nowrap;
    transition: color 0.18s var(--ease), background 0.18s var(--ease);
  }
  .recheck:hover:not(:disabled) { color: var(--ink); background: rgba(255, 255, 255, 0.7); }
  .recheck:disabled { color: var(--stone); cursor: default; }

  .empty { color: var(--ash); padding: 44px 0; text-align: center; line-height: 1.6; max-width: 46ch; margin: 0 auto; }
</style>
