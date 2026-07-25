/**
 * Pull the special blocks out of an answer.
 *
 * Fenced blocks are lifted out *before* the prose is escaped, which is the whole
 * reason this is a separate pass. The transcript escapes model output so it can
 * never become markup; code and charts need the opposite — the highlighter wants
 * the raw source, and escaping first then unescaping to highlight would be a
 * round trip through the exact representation the escaping exists to prevent.
 *
 * Kept apart from the component because the interesting cases are all about
 * *incomplete* input — a fence still arriving token by token, a spec the model
 * got wrong — and those are much easier to pin down as plain functions than by
 * rendering.
 */

const FENCE = /^```([^\n`]*)\n([\s\S]*?)^```[ \t]*$/gm;

/** Languages that get a live canvas rather than a code block. */
const RUNNABLE = new Set(["html", "canvas", "preview"]);

export function splitBlocks(source = "") {
  const out = [];
  // Whitespace between two fences is not prose. Emitting it produces empty
  // paragraphs that render as gaps nobody asked for.
  const prose = (text) => {
    if (text.trim()) out.push({ kind: "md", text });
  };
  let last = 0;
  let m;

  FENCE.lastIndex = 0;
  while ((m = FENCE.exec(source)) !== null) {
    const lang = (m[1] || "").trim().toLowerCase();
    // The capture runs up to the closing fence, so it carries the newline that
    // preceded it. Consumers want the source, not the source plus a blank line.
    const body = m[2].replace(/\n$/, "");

    if (m.index > last) prose(source.slice(last, m.index));

    if (lang === "chart") {
      let spec = null;
      try {
        const parsed = JSON.parse(body);
        if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) spec = parsed;
      } catch {
        spec = null;
      }
      // A spec that will not parse becomes an ordinary JSON block rather than
      // disappearing: the model wrote something, and swallowing it would leave
      // the reader wondering where the chart went.
      out.push(spec ? { kind: "chart", spec } : { kind: "code", lang: "json", code: body });
    } else if (RUNNABLE.has(lang)) {
      out.push({ kind: "canvas", code: body });
    } else {
      out.push({ kind: "code", lang, code: body });
    }

    last = FENCE.lastIndex;
  }

  // A fence with no closing line is left as prose. While an answer streams the
  // block arrives a few characters at a time; treating it as complete would
  // flash a broken chart, or run half a page, on every token.
  if (last < source.length) prose(source.slice(last));
  return out;
}
