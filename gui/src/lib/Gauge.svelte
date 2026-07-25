<script>
  /**
   * A ring, not a bar: at 13px a bar is three pixels of colour and reads as
   * nothing, while a ring keeps its shape and stays legible next to text.
   *
   * The fraction is drawn as a dash offset around the circumference, so the arc
   * starts at twelve o'clock and grows clockwise the way a fill is expected to.
   */
  let { fraction = 0, size = 13 } = $props();

  const R = 6;
  const C = 2 * Math.PI * R;
  const arc = $derived(Math.max(0, Math.min(1, fraction)) * C);
</script>

<svg viewBox="0 0 16 16" width={size} height={size} aria-hidden="true">
  <circle cx="8" cy="8" r={R} fill="none" stroke="rgba(20,18,14,0.13)" stroke-width="2.4" />
  <circle
    cx="8"
    cy="8"
    r={R}
    fill="none"
    stroke="currentColor"
    stroke-width="2.4"
    stroke-linecap="round"
    stroke-dasharray="{arc} {C}"
    transform="rotate(-90 8 8)"
  />
</svg>

<style>
  svg { display: block; flex: 0 0 auto; }
  circle { transition: stroke-dasharray 0.5s var(--ease), stroke 0.3s var(--ease); }
</style>
