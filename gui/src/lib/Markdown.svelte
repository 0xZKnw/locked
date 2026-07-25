<script>
  import { marked } from "marked";
  import Chart from "./Chart.svelte";
  import Code from "./Code.svelte";
  import Canvas from "./Canvas.svelte";
  import { splitBlocks } from "./blocks.js";

  let { source = "", streaming = false } = $props();

  marked.setOptions({ gfm: true, breaks: true });

  /**
   * Models write markdown, so the transcript has to read it — but this window is
   * an audit surface, and rendering model output as live markup would be a
   * strange thing for it to do. So every `<` and `&` is escaped *before* parsing:
   * markdown still works, and any HTML the model emits is shown as the literal
   * text it wrote rather than executed.
   *
   * This is the same instinct as the rest of the project — the model's output is
   * data, never instructions to the machinery around it.
   */
  function escapeHtml(s) {
    return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
  }

  /**
   * Hide an unterminated code fence while text is still arriving, otherwise the
   * transcript flickers between "paragraph" and "code block" on every token.
   */
  function stabilise(s) {
    if (!streaming) return s;
    const fences = (s.match(/^```/gm) || []).length;
    return fences % 2 === 1 ? s + "\n```" : s;
  }

  const md = (s) => marked.parse(stabilise(escapeHtml(s)));

  const parts = $derived(
    splitBlocks(source).map((p) => (p.kind === "md" ? { kind: "md", html: md(p.text) } : p)),
  );
</script>

{#each parts as p, i (i)}
  {#if p.kind === "chart"}
    <Chart spec={p.spec} />
  {:else if p.kind === "canvas"}
    <Canvas code={p.code} />
  {:else if p.kind === "code"}
    <Code lang={p.lang} code={p.code} />
  {:else}
    <div class="md">{@html p.html}</div>
  {/if}
{/each}

<style>
  .md { line-height: 1.68; }
</style>
