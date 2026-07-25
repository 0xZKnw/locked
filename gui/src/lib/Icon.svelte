<script>
  /**
   * One stroke weight, one grid, no fills. The rail collapses to these alone, so
   * each has to be legible at 16px with no label under it.
   */
  let { name } = $props();
</script>

<svg
  viewBox="0 0 16 16"
  width="16"
  height="16"
  fill="none"
  stroke="currentColor"
  stroke-width="1.4"
  stroke-linecap="round"
  stroke-linejoin="round"
  aria-hidden="true"
>
  {#if name === "transcript"}
    <!-- Lines of a conversation, uneven like real speech. -->
    <path d="M3 4.2h10M3 8h10M3 11.8h6.2" />
  {:else if name === "receipts"}
    <!-- A slip with a torn edge: the thing the journal actually produces. -->
    <path d="M4 2.6h8v10.1l-2 1.2-2-1.2-2 1.2V2.6Z" />
    <path d="M6.3 5.7h3.4M6.3 8.3h3.4" />
  {:else if name === "approvals"}
    <!-- A decision, not a status: the tick is what a human puts there. -->
    <circle cx="8" cy="8" r="5.7" />
    <path d="m5.6 8.2 1.7 1.7 3.2-3.7" />
  {:else if name === "capabilities"}
    <!-- A key. Capabilities are credentials TAP holds, nothing more. -->
    <circle cx="5.3" cy="8" r="2.7" />
    <path d="M8 8h5.6M12 8v2.3M10 8v1.7" />
  {:else if name === "panel"}
    <!-- The rail itself, shown as the thing being folded away. -->
    <rect x="2.4" y="3.2" width="11.2" height="9.6" rx="2" />
    <path d="M6.6 3.2v9.6" />
  {:else if name === "image"}
    <rect x="2.4" y="3.4" width="11.2" height="9.2" rx="2" />
    <circle cx="6" cy="6.6" r="1.1" />
    <path d="m3.2 11.4 3-3 2.4 2.3 2-1.8 2.2 2.1" />
  {:else if name === "expand"}
    <path d="M9.6 2.6h3.8v3.8M6.4 13.4H2.6V9.6M13.4 2.6 9.2 6.8M2.6 13.4l4.2-4.2" />
  {:else if name === "shrink"}
    <path d="M13.2 6.4H9.4V2.6M2.8 9.6h3.8v3.8M9.4 6.6l4-4M6.6 9.4l-4 4" />
  {:else if name === "plus"}
    <path d="M8 3.4v9.2M3.4 8h9.2" />
  {:else if name === "trash"}
    <path d="M3.4 4.6h9.2M6.5 4.6V3.4h3v1.2M4.6 4.6l.6 8.2h5.6l.6-8.2" />
  {:else if name === "chev"}
    <path d="m6.2 4.4 4 3.6-4 3.6" />
  {:else if name === "send"}
    <path d="M8 12.8V3.9M8 3.9 4.5 7.4M8 3.9l3.5 3.5" />
  {/if}
</svg>

<style>
  svg { display: block; flex: 0 0 auto; }
</style>
