<script>
  /**
   * The frame every non-transcript view sits in.
   *
   * Three screens had drifted into three layouts — different headers, different
   * paddings, different widths — which is most of why they read as bolted on.
   * They share this now, and they share the transcript's reading column, so the
   * app is one surface with four things on it rather than four pages.
   */
  let { title, blurb, aside, foot, children } = $props();
</script>

<div class="screen">
  <header>
    <div class="col">
      <div class="top">
        <h2>{title}</h2>
        {#if aside}<div class="aside">{@render aside()}</div>{/if}
      </div>
      {#if blurb}<p>{blurb}</p>{/if}
    </div>
  </header>

  <div class="body">
    <div class="col">{@render children()}</div>
  </div>

  {#if foot}
    <footer><div class="col">{@render foot()}</div></footer>
  {/if}
</div>

<style>
  .screen { display: flex; flex-direction: column; height: 100%; min-height: 0; }

  /* The same column the transcript and the composer use. */
  .col { max-width: var(--col); margin: 0 auto; width: 100%; }

  header { flex: 0 0 auto; padding: 22px 28px 14px; }

  .top { display: flex; align-items: baseline; justify-content: space-between; gap: 20px; }

  h2 {
    margin: 0;
    font-size: 17px;
    font-weight: 500;
    letter-spacing: -0.2px;
  }

  .aside { flex: 0 0 auto; }

  /* Prose is prose: sans, and narrow enough to actually read. Monospace is kept
     for strings you might copy — a digest, a host — and nothing else. */
  header p {
    margin: 8px 0 0;
    color: var(--mute);
    font-size: 13px;
    line-height: 1.6;
    max-width: 62ch;
  }

  .body { flex: 1 1 auto; overflow-y: auto; padding: 6px 28px 24px; }

  footer {
    flex: 0 0 auto;
    padding: 10px 28px 14px;
    color: var(--ash);
    font-size: 11.5px;
  }
  /* The snippet's markup belongs to the child's scope, so reaching it from here
     needs :global — a plain `footer code` matches nothing and Svelte says so. */
  footer :global(code) { font-family: var(--mono); word-break: break-all; }
</style>
