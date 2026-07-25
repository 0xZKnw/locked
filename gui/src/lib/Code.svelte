<script>
  /**
   * A code block, coloured.
   *
   * `highlight.js` produces escaped HTML from raw source, so the escaping the
   * transcript relies on is preserved rather than bypassed — the raw text goes
   * in, markup-safe spans come out, and nothing the model wrote is ever
   * interpreted as markup.
   *
   * The language is *detected against a known list*, never trusted: an unknown
   * fence tag falls back to plain text rather than being passed to a highlighter
   * that would try to load something.
   */
  import hljs from "highlight.js/lib/common";

  let { lang = "", code = "" } = $props();

  /** Names people actually type in a fence, mapped to what hljs calls them. */
  const ALIAS = {
    js: "javascript",
    jsx: "javascript",
    ts: "typescript",
    tsx: "typescript",
    sh: "bash",
    shell: "bash",
    zsh: "bash",
    console: "bash",
    py: "python",
    rb: "ruby",
    rs: "rust",
    yml: "yaml",
    md: "markdown",
    "c++": "cpp",
    "c#": "csharp",
    cs: "csharp",
    golang: "go",
    psql: "sql",
    postgres: "sql",
    dockerfile: "dockerfile",
    text: "",
    txt: "",
    "": "",
  };

  const resolved = $derived.by(() => {
    const raw = (lang || "").toLowerCase();
    const name = raw in ALIAS ? ALIAS[raw] : raw;
    return name && hljs.getLanguage(name) ? name : "";
  });

  const html = $derived.by(() => {
    const body = code.replace(/\n$/, "");
    if (resolved) {
      try {
        return hljs.highlight(body, { language: resolved, ignoreIllegals: true }).value;
      } catch {
        /* fall through to plain */
      }
    }
    // No language, or one we do not carry: escape and show it plainly. Guessing
    // is worse than not colouring — a wrong grammar mis-colours the code and
    // implies a language the author did not write.
    return body.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
  });

  let copied = $state(false);
  async function copy() {
    try {
      await navigator.clipboard.writeText(code.replace(/\n$/, ""));
      copied = true;
      setTimeout(() => (copied = false), 1400);
    } catch {
      copied = false;
    }
  }
</script>

<div class="block">
  <div class="bar">
    <span class="lang">{resolved || lang || "text"}</span>
    <button onclick={copy}>{copied ? "copied" : "copy"}</button>
  </div>
  <pre><code class="hljs">{@html html}</code></pre>
</div>

<style>
  .block {
    margin: 12px 0 14px;
    border-radius: var(--r-md);
    background: rgba(255, 255, 255, 0.72);
    box-shadow: inset 0 0 0 1px rgba(20, 18, 14, 0.06);
    overflow: hidden;
  }

  .bar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 5px 10px 5px 13px;
    box-shadow: inset 0 -1px 0 rgba(20, 18, 14, 0.05);
  }
  .lang { font-size: 10.5px; color: var(--ash); letter-spacing: 0.3px; }
  .bar button {
    font-size: 10.5px;
    color: var(--ash);
    padding: 3px 7px;
    border-radius: var(--r-xs);
    transition: color 0.16s var(--ease), background 0.16s var(--ease);
  }
  .bar button:hover { color: var(--body); background: rgba(20, 18, 14, 0.05); }

  pre {
    margin: 0;
    padding: 11px 13px 13px;
    overflow-x: auto;
    font-size: 12px;
    line-height: 1.6;
  }
  code { font-family: var(--mono); }

  /* A theme in this app's ink, not a stock one.
     Structure is carried by weight and by a few restrained hues — the aim is
     code that reads as part of the page, not a terminal pasted into it. */
  .block :global(.hljs-comment),
  .block :global(.hljs-quote) { color: var(--ash); font-style: italic; }

  .block :global(.hljs-keyword),
  .block :global(.hljs-selector-tag),
  .block :global(.hljs-literal),
  .block :global(.hljs-doctag) { color: #7a3ea8; }

  .block :global(.hljs-string),
  .block :global(.hljs-regexp),
  .block :global(.hljs-addition) { color: #1f7a4d; }

  .block :global(.hljs-number),
  .block :global(.hljs-symbol),
  .block :global(.hljs-bullet),
  .block :global(.hljs-meta) { color: #b0621a; }

  .block :global(.hljs-title),
  .block :global(.hljs-section),
  .block :global(.hljs-name),
  .block :global(.hljs-selector-id),
  .block :global(.hljs-selector-class) { color: #1f5fa8; }

  .block :global(.hljs-attr),
  .block :global(.hljs-attribute),
  .block :global(.hljs-variable),
  .block :global(.hljs-template-variable),
  .block :global(.hljs-type),
  .block :global(.hljs-params) { color: #2f6f8f; }

  .block :global(.hljs-built_in),
  .block :global(.hljs-class .hljs-title) { color: #8a5a1f; }

  .block :global(.hljs-deletion) { color: var(--error); }
  .block :global(.hljs-emphasis) { font-style: italic; }
  .block :global(.hljs-strong) { font-weight: 600; }
</style>
