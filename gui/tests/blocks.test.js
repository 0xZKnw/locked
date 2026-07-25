import { describe, it, expect } from "vitest";
import { splitBlocks } from "../src/lib/blocks.js";

const fence = (lang, body) => "```" + lang + "\n" + body + "\n```";
const chart = (o) => fence("chart", JSON.stringify(o));
const SPEC = { type: "line", x: ["a", "b"], series: [{ name: "s", data: [1, 2] }] };

describe("splitting an answer into blocks", () => {
  it("keeps prose and blocks in the order they were written", () => {
    const parts = splitBlocks(`Before.\n\n${chart(SPEC)}\n\nAfter.`);
    expect(parts.map((p) => p.kind)).toEqual(["md", "chart", "md"]);
    expect(parts[0].text.trim()).toBe("Before.");
    expect(parts[1].spec.type).toBe("line");
    expect(parts[2].text.trim()).toBe("After.");
  });

  it("handles several blocks of different kinds in one answer", () => {
    const src = [chart(SPEC), "text", fence("python", "print(1)"), fence("html", "<b>hi</b>")].join("\n");
    expect(splitBlocks(src).map((p) => p.kind)).toEqual(["chart", "md", "code", "canvas"]);
  });

  /**
   * The streaming case. A block arrives a few characters at a time, so a fence
   * with no closing line is not a block yet — treating it as complete would
   * flash a broken chart, or run half a page, on every token.
   */
  it("leaves a half-arrived block as prose", () => {
    for (const half of ['```chart\n{"type":"line"', "```html\n<script>alert(1)"]) {
      const parts = splitBlocks(half);
      expect(parts.map((p) => p.kind)).toEqual(["md"]);
      expect(parts[0].text).toBe(half);
    }
  });

  it("shows a chart spec it cannot parse instead of dropping it", () => {
    const parts = splitBlocks(fence("chart", "{ oops not json"));
    expect(parts.map((p) => p.kind)).toEqual(["code"]);
    expect(parts[0].lang).toBe("json");
    expect(parts[0].code).toContain("oops not json");
  });

  it("refuses a chart spec that is not an object", () => {
    expect(splitBlocks(fence("chart", "[1,2,3]"))[0].kind).toBe("code");
    expect(splitBlocks(fence("chart", "42"))[0].kind).toBe("code");
  });

  it("keeps an ordinary fence as code, with its language", () => {
    const parts = splitBlocks(fence("rust", "fn main() {}"));
    expect(parts.map((p) => p.kind)).toEqual(["code"]);
    expect(parts[0].lang).toBe("rust");
    expect(parts[0].code).toBe("fn main() {}");
  });

  it("treats an untagged fence as code with no language", () => {
    expect(splitBlocks("```\nplain\n```")[0]).toMatchObject({ kind: "code", lang: "" });
  });

  /**
   * Only an explicit runnable tag executes. A language that merely *contains*
   * markup — xml, svelte, a template — is read, not run: the difference between
   * "here is some HTML" and "run this" has to be the model's decision, stated,
   * not something inferred from the content.
   */
  it("runs only the fences that ask to run", () => {
    for (const tag of ["html", "canvas", "preview"]) {
      const parts = splitBlocks(fence(tag, "<b>hi</b>"));
      expect(parts.map((p) => p.kind)).toEqual(["canvas"]);
      expect(parts[0].code).toBe("<b>hi</b>");
    }
    for (const tag of ["xml", "svelte", "vue", "jsx", "markdown"]) {
      expect(splitBlocks(fence(tag, "<b>hi</b>"))[0].kind).toBe("code");
    }
  });

  it("is not confused by a second call", () => {
    const src = chart(SPEC);
    expect(splitBlocks(src).filter((p) => p.kind === "chart")).toHaveLength(1);
    expect(splitBlocks(src).filter((p) => p.kind === "chart")).toHaveLength(1);
  });
});
