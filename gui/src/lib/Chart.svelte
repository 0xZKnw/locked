<script>
  /**
   * Charts the model asks for.
   *
   * The model emits a ```chart fence containing JSON; this draws it. It does not
   * emit SVG or HTML, and that is the whole security argument: model output is
   * data everywhere else in this window — it is escaped before it reaches the
   * markdown parser — so letting it emit markup here would open the one hole the
   * rest of the transcript is careful to close. A declarative spec that our own
   * code reads keeps that property intact.
   *
   * Everything below follows one rule: the data is the only thing allowed to be
   * loud. Hairline axes, no borders on marks, white doing the separating, and
   * labels that stay in text ink so a pale series colour is never asked to be
   * readable as type.
   */
  let { spec } = $props();

  const W = 640;
  const H = 300;

  const PALETTE = Array.from({ length: 8 }, (_, i) => `var(--viz-${i + 1})`);
  const colour = (i) => PALETTE[i % PALETTE.length];

  const KINDS = ["line", "area", "column", "bar", "scatter", "pie", "donut"];
  const kind = $derived(KINDS.includes(spec?.type) ? spec.type : "column");

  /** Series, normalised to `{name, values[], points[]}` whatever the form. */
  const series = $derived(
    (Array.isArray(spec?.series) ? spec.series : [])
      .slice(0, 8)
      .map((s, i) => ({
        name: s?.name ?? `Series ${i + 1}`,
        values: (Array.isArray(s?.data) ? s.data : []).map((v) => (Number.isFinite(+v) ? +v : 0)),
        points: (Array.isArray(s?.points) ? s.points : [])
          .filter((p) => Array.isArray(p) && p.length >= 2)
          .map((p) => [+p[0], +p[1]]),
      })),
  );

  const labels = $derived((Array.isArray(spec?.x) ? spec.x : []).map(String));
  const stacked = $derived(spec?.stacked === true);
  const isRound = $derived(kind === "pie" || kind === "donut");
  const isBar = $derived(kind === "bar");

  let showTable = $state(false);
  let hover = $state(null);

  // ---------------------------------------------------------------------
  // Scales
  // ---------------------------------------------------------------------

  /** Ticks a person would have chosen: 0, 500, 1,000 — never 0, 437, 874. */
  function niceTicks(lo, hi, count = 5) {
    if (!Number.isFinite(lo) || !Number.isFinite(hi) || lo === hi) {
      return { ticks: [0, 1], min: 0, max: Math.max(1, hi || 1) };
    }
    const raw = (hi - lo) / (count - 1);
    const mag = 10 ** Math.floor(Math.log10(raw));
    const step = [1, 2, 2.5, 5, 10].find((m) => m * mag >= raw) * mag;
    const min = Math.min(0, Math.floor(lo / step) * step);
    const max = Math.ceil(hi / step) * step;
    const ticks = [];
    for (let v = min; v <= max + step / 2; v += step) ticks.push(+v.toFixed(10));
    return { ticks, min, max };
  }

  const extent = $derived.by(() => {
    if (isRound) return { ticks: [], min: 0, max: 1 };
    if (kind === "scatter") {
      const ys = series.flatMap((s) => s.points.map((p) => p[1]));
      return niceTicks(Math.min(0, ...ys), Math.max(0, ...ys));
    }
    if (stacked) {
      const totals = labels.map((_, i) => series.reduce((sum, s) => sum + (s.values[i] ?? 0), 0));
      return niceTicks(Math.min(0, ...totals), Math.max(0, ...totals));
    }
    const all = series.flatMap((s) => s.values);
    return niceTicks(Math.min(0, ...all), Math.max(0, ...all));
  });

  const xExtent = $derived.by(() => {
    if (kind !== "scatter") return { min: 0, max: 1 };
    const xs = series.flatMap((s) => s.points.map((p) => p[0]));
    return niceTicks(Math.min(...xs), Math.max(...xs), 4);
  });

  const fmt = (n) =>
    Math.abs(n) >= 1000 ? n.toLocaleString("en-US") : String(+(+n).toFixed(2));

  const CHAR = 6.2;
  const LABEL_CAP = 190;

  /**
   * The left gutter, measured from whatever actually sits in it.
   *
   * On a column chart that is the widest numeric tick; on a horizontal bar it is
   * the longest category name — which is the whole reason someone picks a bar
   * over a column. Sizing it from the ticks in both cases clipped the names, and
   * a clipped label is worse than a shortened one: the reader cannot tell it
   * happened.
   */
  const pad = $derived({
    left: isRound
      ? 0
      : isBar
        ? Math.min(LABEL_CAP, 16 + Math.max(4, ...labels.map((l) => l.length)) * CHAR)
        : Math.max(34, 9 + Math.max(...extent.ticks.map((t) => fmt(t).length)) * 7),
    right: isRound ? 0 : 18,
    top: spec?.title ? 26 : 12,
    bottom: isRound ? 0 : labels.length || kind === "scatter" ? 30 : 12,
  });

  const plot = $derived({
    x: pad.left,
    y: pad.top,
    w: Math.max(10, W - pad.left - pad.right),
    h: Math.max(10, H - pad.top - pad.bottom),
  });

  /** Value → pixel, on whichever axis carries the measure. */
  const vScale = $derived((v) => {
    const t = (v - extent.min) / (extent.max - extent.min || 1);
    return isBar ? plot.x + t * plot.w : plot.y + plot.h - t * plot.h;
  });

  const zero = $derived(vScale(Math.max(extent.min, 0)));

  /** Category index → the centre of its band. */
  const band = $derived(plot[isBar ? "h" : "w"] / Math.max(1, labels.length));
  const catAt = $derived((i) => (isBar ? plot.y : plot.x) + band * (i + 0.5));

  // ---------------------------------------------------------------------
  // Marks
  // ---------------------------------------------------------------------

  /** Shortened, never clipped — the ellipsis is the reader's cue. */
  const catLabel = $derived((l) => {
    if (!isBar) return l;
    const room = Math.floor((pad.left - 16) / CHAR);
    return l.length > room ? l.slice(0, Math.max(1, room - 1)) + "…" : l;
  });

  const linePath = $derived((s) =>
    s.values
      .map((v, i) => `${i ? "L" : "M"}${catAt(i).toFixed(1)},${vScale(v).toFixed(1)}`)
      .join(" "),
  );

  const areaPath = $derived(
    (s) => `${linePath(s)} L${catAt(s.values.length - 1).toFixed(1)},${zero.toFixed(1)} L${catAt(0).toFixed(1)},${zero.toFixed(1)} Z`,
  );

  /** Stacked bars need each segment's floor, which is the sum below it. */
  const floorAt = $derived((si, i) =>
    stacked ? series.slice(0, si).reduce((sum, s) => sum + (s.values[i] ?? 0), 0) : 0,
  );

  const BAR_MAX = 24;
  const GAP = 2;

  const barSize = $derived(
    Math.min(BAR_MAX, (band * 0.68) / (stacked ? 1 : Math.max(1, series.length))),
  );

  function barRect(si, i) {
    const v = series[si].values[i] ?? 0;
    const base = floorAt(si, i);
    const a = vScale(base);
    const b = vScale(base + v);
    const thick = barSize;
    const groupW = stacked ? thick : thick * series.length;
    const start = catAt(i) - groupW / 2 + (stacked ? 0 : si * thick);

    // A 2px gap in the surface colour, not a stroke: separation without adding
    // ink that isn't data.
    const inset = stacked && si > 0 ? GAP : 0;

    return isBar
      ? { x: Math.min(a, b), y: start, width: Math.abs(b - a) - inset, height: Math.max(1, thick - (stacked ? 0 : GAP)) }
      : { x: start, y: Math.min(a, b) + inset, width: Math.max(1, thick - (stacked ? 0 : GAP)), height: Math.abs(b - a) - inset };
  }

  // Rounded at the data end, square at the baseline — so the mark still reads as
  // growing from a single floor. Rounding all four corners instead turns a
  // stacked column into a stack of pills, each looking like its own bar.
  const barRadius = $derived(Math.min(4, barSize / 2));

  /** Which segment sits at the tip of the stack, and so earns the rounding. */
  const capIndex = $derived((i) => {
    for (let si = series.length - 1; si >= 0; si--) if ((series[si].values[i] ?? 0) > 0) return si;
    return -1;
  });

  /**
   * A rect with only the two corners at the data end rounded.
   *
   * `side` is where the value grows to, so a negative value rounds the other
   * end and the baseline stays square either way.
   */
  function barPath(x, y, w, h, r, side) {
    const rr = Math.max(0, Math.min(r, w / 2, h / 2));
    const [X, Y, W2, H2] = [x, y, Math.max(0, w), Math.max(0, h)];
    if (!rr) return `M${X},${Y}h${W2}v${H2}h${-W2}Z`;
    switch (side) {
      case "top":
        return `M${X},${Y + H2}V${Y + rr}a${rr},${rr} 0 0 1 ${rr},${-rr}h${W2 - 2 * rr}a${rr},${rr} 0 0 1 ${rr},${rr}V${Y + H2}Z`;
      case "bottom":
        return `M${X},${Y}V${Y + H2 - rr}a${rr},${rr} 0 0 0 ${rr},${rr}h${W2 - 2 * rr}a${rr},${rr} 0 0 0 ${rr},${-rr}V${Y}Z`;
      case "right":
        return `M${X},${Y}H${X + W2 - rr}a${rr},${rr} 0 0 1 ${rr},${rr}v${H2 - 2 * rr}a${rr},${rr} 0 0 1 ${-rr},${rr}H${X}Z`;
      default:
        return `M${X + W2},${Y}H${X + rr}a${rr},${rr} 0 0 0 ${-rr},${rr}v${H2 - 2 * rr}a${rr},${rr} 0 0 0 ${rr},${rr}H${X + W2}Z`;
    }
  }

  function barShape(si, i) {
    const r = barRect(si, i);
    const v = series[si].values[i] ?? 0;
    // Interior segments of a stack have no free end, so they stay square.
    const capped = !stacked || si === capIndex(i);
    const side = isBar ? (v < 0 ? "left" : "right") : v < 0 ? "bottom" : "top";
    return barPath(r.x, r.y, r.width, r.height, capped ? barRadius : 0, side);
  }

  // ---------------------------------------------------------------------
  // Pie
  // ---------------------------------------------------------------------

  const slices = $derived.by(() => {
    if (!isRound) return [];
    const vals = (series[0]?.values ?? []).map((v) => Math.max(0, v));
    const total = vals.reduce((a, b) => a + b, 0) || 1;
    const cx = W / 2;
    const cy = H / 2 + (spec?.title ? 8 : 0);
    const r = Math.min(W, H) / 2 - 34;
    const inner = kind === "donut" ? r * 0.58 : 0;

    let angle = -Math.PI / 2;
    return vals.map((v, i) => {
      const sweep = (v / total) * Math.PI * 2;
      const a0 = angle;
      const a1 = angle + sweep;
      angle = a1;
      const p = (rad, ang) => `${(cx + rad * Math.cos(ang)).toFixed(1)},${(cy + rad * Math.sin(ang)).toFixed(1)}`;
      const big = sweep > Math.PI ? 1 : 0;
      const d = inner
        ? `M${p(r, a0)} A${r},${r} 0 ${big} 1 ${p(r, a1)} L${p(inner, a1)} A${inner},${inner} 0 ${big} 0 ${p(inner, a0)} Z`
        : `M${cx},${cy} L${p(r, a0)} A${r},${r} 0 ${big} 1 ${p(r, a1)} Z`;
      const mid = (a0 + a1) / 2;
      return {
        d,
        i,
        value: v,
        share: v / total,
        label: labels[i] ?? `#${i + 1}`,
        lx: cx + (r + 16) * Math.cos(mid),
        ly: cy + (r + 16) * Math.sin(mid),
        anchor: Math.cos(mid) > 0.1 ? "start" : Math.cos(mid) < -0.1 ? "end" : "middle",
      };
    });
  });

  // ---------------------------------------------------------------------
  // Hover — a chart in a window is interactive, so it answers when pointed at.
  // ---------------------------------------------------------------------

  function onMove(e) {
    if (isRound) return;
    const r = e.currentTarget.getBoundingClientRect();
    const px = ((e.clientX - r.left) / r.width) * W;
    const py = ((e.clientY - r.top) / r.height) * H;

    if (kind === "scatter") {
      let best = null;
      for (const [si, s] of series.entries()) {
        for (const [x, y] of s.points) {
          const dx = sx(x) - px;
          const dy = vScale(y) - py;
          const d = dx * dx + dy * dy;
          if (d < 400 && (!best || d < best.d)) best = { d, si, x, y };
        }
      }
      hover = best ? { kind: "point", ...best } : null;
      return;
    }

    if (!labels.length) return;
    const along = isBar ? py - plot.y : px - plot.x;
    const i = Math.max(0, Math.min(labels.length - 1, Math.floor(along / band)));
    hover = { kind: "index", i };
  }

  const sx = $derived((x) => {
    const t = (x - xExtent.min) / (xExtent.max - xExtent.min || 1);
    return plot.x + t * plot.w;
  });

  const tip = $derived.by(() => {
    if (!hover) return null;
    if (hover.kind === "point") {
      return {
        x: sx(hover.x),
        y: vScale(hover.y),
        title: series[hover.si].name,
        rows: [{ name: fmt(hover.x), value: fmt(hover.y), colour: colour(hover.si) }],
      };
    }
    const i = hover.i;
    return {
      x: isBar ? plot.x + plot.w / 2 : catAt(i),
      y: isBar ? catAt(i) : plot.y,
      title: labels[i],
      rows: series.map((s, si) => ({ name: s.name, value: fmt(s.values[i] ?? 0), colour: colour(si) })),
    };
  });
</script>

<figure class="chart" class:round={isRound}>
  <svg
    viewBox="0 0 {W} {H}"
    role="img"
    aria-label={spec?.title ?? `${kind} chart`}
    onpointermove={onMove}
    onpointerleave={() => (hover = null)}
  >
    {#if spec?.title}
      <text class="title" x="0" y="14">{spec.title}</text>
    {/if}

    {#if !isRound}
      <!-- Gridlines: hairline, solid, one step off the surface. They exist to be
           looked past. -->
      {#each extent.ticks as t (t)}
        {#if isBar}
          <line class="grid" x1={vScale(t)} x2={vScale(t)} y1={plot.y} y2={plot.y + plot.h} />
          <text class="tick" x={vScale(t)} y={plot.y + plot.h + 18} text-anchor="middle">{fmt(t)}</text>
        {:else}
          <line class="grid" x1={plot.x} x2={plot.x + plot.w} y1={vScale(t)} y2={vScale(t)} />
          <text class="tick" x={plot.x - 9} y={vScale(t) + 4} text-anchor="end">{fmt(t)}</text>
        {/if}
      {/each}
      <line class="axis" x1={plot.x} x2={isBar ? plot.x : plot.x + plot.w} y1={isBar ? plot.y : zero} y2={plot.y + plot.h} />
    {/if}

    {#if kind === "area"}
      {#each series as s, si (s.name)}
        <path class="area" d={areaPath(s)} fill={colour(si)} />
      {/each}
    {/if}

    {#if kind === "line" || kind === "area"}
      {#each series as s, si (s.name)}
        <path class="line" d={linePath(s)} stroke={colour(si)} />
        <!-- The endpoint only. A number beside every point is chaos and goes
             unread; the axis, the legend and the tooltip carry the rest. -->
        {#if s.values.length}
          <circle class="ring" cx={catAt(s.values.length - 1)} cy={vScale(s.values.at(-1))} r="5" />
          <circle cx={catAt(s.values.length - 1)} cy={vScale(s.values.at(-1))} r="4" fill={colour(si)} />
        {/if}
      {/each}
    {/if}

    {#if kind === "column" || kind === "bar"}
      {#each series as s, si (s.name)}
        {#each s.values as _, i (i)}
          <path d={barShape(si, i)} fill={colour(si)} />
        {/each}
      {/each}
    {/if}

    {#if kind === "scatter"}
      {#each extent.ticks as t (t)}{/each}
      {#each series as s, si (s.name)}
        {#each s.points as [x, y], i (i)}
          <circle class="ring" cx={sx(x)} cy={vScale(y)} r="5.5" />
          <circle cx={sx(x)} cy={vScale(y)} r="4.5" fill={colour(si)} />
        {/each}
      {/each}
    {/if}

    {#if isRound}
      {#each slices as s (s.i)}
        <!-- The 2px separation is the surface showing through, not a border. -->
        <path d={s.d} fill={colour(s.i)} class="slice" />
        {#if s.share > 0.05}
          <text class="slice-label" x={s.lx} y={s.ly} text-anchor={s.anchor}>
            {s.label} · {Math.round(s.share * 100)}%
          </text>
        {/if}
      {/each}
    {/if}

    {#if !isRound && labels.length}
      {#each labels as l, i (i)}
        {#if labels.length <= 12 || i % Math.ceil(labels.length / 8) === 0}
          <text
            class="tick"
            x={isBar ? plot.x - 9 : catAt(i)}
            y={isBar ? catAt(i) + 4 : plot.y + plot.h + 20}
            text-anchor={isBar ? "end" : "middle"}
          >{catLabel(l)}</text>
        {/if}
      {/each}
    {/if}

    {#if tip}
      <line class="crosshair" x1={tip.x} x2={tip.x} y1={plot.y} y2={plot.y + plot.h} />
    {/if}
  </svg>

  {#if tip}
    <div class="tip" style="left: {(tip.x / W) * 100}%">
      <div class="tip-title">{tip.title}</div>
      {#each tip.rows as r (r.name)}
        <div class="tip-row"><i style="background: {r.colour}"></i>{r.name}<b>{r.value}</b></div>
      {/each}
    </div>
  {/if}

  <!-- A legend for two or more series, always. Identity must never rest on
       colour-matching alone, and three of these hues sit under 3:1 on white. -->
  <figcaption>
  <div class="caption-row">
    {#if series.length > 1 || isRound}
      <div class="legend">
        {#each isRound ? labels.slice(0, 8) : series.map((s) => s.name) as name, i (name)}
          <span class="key"><i style="background: {colour(i)}"></i>{name}</span>
        {/each}
      </div>
    {/if}
    <button class="numbers" onclick={() => (showTable = !showTable)}>
      {showTable ? "hide numbers" : "numbers"}
    </button>
  </div>

  {#if showTable}
    <!-- The relief the palette owes: every value readable as text, so nothing is
         gated behind a hue. -->
    <table>
      <thead>
        <tr>
          <th></th>
          {#each isRound || kind !== "scatter" ? labels : ["x", "y"] as l (l)}<th>{l}</th>{/each}
        </tr>
      </thead>
      <tbody>
        {#each series as s, si (s.name)}
          <tr>
            <th><i style="background: {colour(si)}"></i>{s.name}</th>
            {#if kind === "scatter"}
              <td colspan={2}>{s.points.map(([x, y]) => `${fmt(x)}, ${fmt(y)}`).join(" · ")}</td>
            {:else}
              {#each labels as _, i (i)}<td>{fmt(s.values[i] ?? 0)}</td>{/each}
            {/if}
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}
  </figcaption>
</figure>

<style>
  .chart {
    margin: 14px 0 16px;
    padding: 12px 14px 10px;
    border-radius: var(--r-lg);
    background: rgba(255, 255, 255, 0.72);
    box-shadow:
      inset 0 0 0 1px rgba(20, 18, 14, 0.06),
      0 1px 2px rgba(20, 18, 14, 0.04);
    position: relative;
  }

  svg { display: block; width: 100%; height: auto; overflow: visible; }

  .title { fill: var(--ink); font-size: 13px; font-weight: 550; }

  /* Recessive by design: the reader should look past these to the data. */
  .grid { stroke: var(--line); stroke-width: 1; }
  .axis { stroke: var(--stone); stroke-width: 1; }
  .crosshair { stroke: var(--line-strong); stroke-width: 1; pointer-events: none; }

  /* Text never wears the data colour — a pale series hue is illegible as type,
     and identity comes from the mark beside the label. */
  .tick {
    fill: var(--ash);
    font-size: 10.5px;
    font-variant-numeric: tabular-nums;
  }
  .slice-label { fill: var(--body); font-size: 11px; }

  .line { fill: none; stroke-width: 2; stroke-linejoin: round; stroke-linecap: round; }
  /* A wash, never a saturated block. */
  .area { opacity: 0.1; }
  /* The ring is the surface showing through, so a dot stays legible where it
     crosses its own line. */
  .ring { fill: var(--canvas); }
  .slice { stroke: var(--canvas); stroke-width: 2; }

  .caption-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    margin-top: 8px;
  }
  .legend { display: flex; flex-wrap: wrap; gap: 4px 14px; }
  .key, .tip-row, table th i {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: 11.5px;
    color: var(--mute);
  }
  .key i, .tip-row i, table th i {
    width: 9px;
    height: 9px;
    border-radius: 2px;
    flex: 0 0 auto;
  }

  .numbers {
    flex: 0 0 auto;
    font-size: 11px;
    color: var(--ash);
    padding: 3px 8px;
    border-radius: var(--r-xs);
    transition: color 0.18s var(--ease), background 0.18s var(--ease);
  }
  .numbers:hover { color: var(--body); background: rgba(20, 18, 14, 0.05); }

  .tip {
    position: absolute;
    top: 14px;
    transform: translateX(-50%);
    background: rgba(255, 255, 255, 0.94);
    backdrop-filter: blur(8px);
    box-shadow:
      inset 0 0 0 1px rgba(20, 18, 14, 0.08),
      0 6px 18px -8px rgba(20, 18, 14, 0.4);
    border-radius: var(--r-sm);
    padding: 7px 10px;
    pointer-events: none;
    white-space: nowrap;
    z-index: 2;
  }
  .tip-title { font-size: 11.5px; color: var(--ink); margin-bottom: 3px; }
  .tip-row { display: flex; gap: 7px; align-items: center; }
  .tip-row b { margin-left: auto; padding-left: 12px; color: var(--ink); font-weight: 550; font-variant-numeric: tabular-nums; }

  table {
    width: 100%;
    margin-top: 10px;
    border-collapse: collapse;
    font-size: 11.5px;
    font-variant-numeric: tabular-nums;
  }
  table th, table td { padding: 4px 8px; text-align: right; color: var(--mute); }
  table thead th { color: var(--ash); font-weight: 450; }
  table tbody th { text-align: left; color: var(--body); font-weight: 450; white-space: nowrap; }
  table tbody tr:nth-child(odd) { background: rgba(20, 18, 14, 0.025); }
</style>
