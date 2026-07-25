import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { mount, unmount } from "svelte";
import Chart from "../src/lib/Chart.svelte";
import Code from "../src/lib/Code.svelte";
import Markdown from "../src/lib/Markdown.svelte";

/**
 * Rendering tests, on the components that draw the model's output.
 *
 * These are the pieces where a mistake is invisible in the source and obvious on
 * screen — a chart axis that clips, a highlighter that guesses the wrong
 * grammar, an escape that stops escaping. They mount the real component into a
 * document and read what it actually produced.
 */

vi.mock("@tauri-apps/api/core", () => ({ invoke: () => Promise.resolve("canvas://localhost/c1") }));

let host;
let instance;

beforeEach(() => {
  host = document.createElement("div");
  document.body.appendChild(host);
});

afterEach(() => {
  if (instance) unmount(instance);
  instance = null;
  host.remove();
});

const render = (Component, props) => {
  instance = mount(Component, { target: host, props });
  return host;
};

// ---------------------------------------------------------------------------
// Charts
// ---------------------------------------------------------------------------

const SERIES = [
  { name: "reads", data: [120, 180, 90] },
  { name: "writes", data: [8, 14, 5] },
];

describe("charts", () => {
  it("draws every form without throwing", () => {
    for (const type of ["line", "area", "column", "bar", "pie", "donut"]) {
      const el = render(Chart, {
        spec: { type, title: "t", x: ["a", "b", "c"], series: SERIES },
      });
      expect(el.querySelector("svg"), type).toBeTruthy();
      unmount(instance);
      instance = null;
    }

    const scatter = render(Chart, {
      spec: { type: "scatter", series: [{ name: "s", points: [[1, 2], [3, 4]] }] },
    });
    expect(scatter.querySelectorAll("circle").length).toBeGreaterThan(0);
  });

  /**
   * The relief the palette owes. Three of the eight hues sit under 3:1 on white,
   * which is only allowed because every value is also readable as text.
   */
  it("can show every value as text", async () => {
    const el = render(Chart, { spec: { type: "column", x: ["a", "b", "c"], series: SERIES } });
    expect(el.querySelector("table")).toBeNull();

    el.querySelector("button.numbers").click();
    await Promise.resolve();

    const cells = [...el.querySelectorAll("tbody td")].map((td) => td.textContent);
    expect(cells).toEqual(["120", "180", "90", "8", "14", "5"]);
  });

  /** A legend for two or more series; a single series is named by the title. */
  it("shows a legend only when identity is ambiguous", () => {
    const two = render(Chart, { spec: { type: "line", x: ["a"], series: SERIES } });
    expect(two.querySelectorAll(".key")).toHaveLength(2);
    unmount(instance);
    instance = null;

    const one = render(Chart, { spec: { type: "line", x: ["a"], series: [SERIES[0]] } });
    expect(one.querySelectorAll(".key")).toHaveLength(0);
  });

  it("survives a spec with nothing in it", () => {
    for (const spec of [{}, { type: "line" }, { type: "bar", series: [] }, { type: "nonsense" }]) {
      const el = render(Chart, { spec });
      expect(el.querySelector("svg")).toBeTruthy();
      unmount(instance);
      instance = null;
    }
    instance = mount(Chart, { target: host, props: { spec: {} } });
  });

  /**
   * Regression: the left gutter was measured from the numeric ticks, so on a
   * horizontal bar — the form you pick *because* the labels are long — the names
   * were clipped. Shortened with an ellipsis is honest; clipped is not.
   */
  it("shortens a long bar label instead of clipping it", () => {
    const el = render(Chart, {
      spec: {
        type: "bar",
        x: ["api.dune.com", "an.extremely.long.hostname.that.will.not.fit.at.all.example.com"],
        series: [{ name: "calls", data: [10, 20] }],
      },
    });
    const labels = [...el.querySelectorAll("text")].map((t) => t.textContent);
    expect(labels).toContain("api.dune.com");
    expect(labels.some((l) => l.endsWith("…"))).toBe(true);
    expect(labels).not.toContain(
      "an.extremely.long.hostname.that.will.not.fit.at.all.example.com",
    );
  });
});

// ---------------------------------------------------------------------------
// Code
// ---------------------------------------------------------------------------

describe("code blocks", () => {
  it("colours a language it carries", () => {
    const el = render(Code, { lang: "rust", code: "fn main() { let x = 1; }" });
    expect(el.querySelectorAll(".hljs-keyword").length).toBeGreaterThan(0);
    expect(el.querySelector(".lang").textContent.trim()).toBe("rust");
  });

  it("resolves the names people actually type", () => {
    for (const [typed, shown] of [["js", "javascript"], ["py", "python"], ["sh", "bash"], ["rs", "rust"]]) {
      const el = render(Code, { lang: typed, code: "x" });
      expect(el.querySelector(".lang").textContent.trim(), typed).toBe(shown);
      unmount(instance);
      instance = null;
    }
    instance = mount(Code, { target: host, props: { lang: "", code: "x" } });
  });

  /**
   * Guessing is worse than not colouring: a wrong grammar mis-colours the code
   * and implies a language the author did not write.
   */
  it("leaves an unknown language plain rather than guessing", () => {
    const el = render(Code, { lang: "brainfuck-9000", code: "fn main() {}" });
    expect(el.querySelectorAll("[class^=hljs-]")).toHaveLength(0);
  });

  it("never lets code become markup", () => {
    const el = render(Code, { lang: "", code: '<img src=x onerror="boom()">' });
    expect(el.querySelector("img")).toBeNull();
    expect(el.querySelector("code").textContent).toContain("<img");
  });
});

// ---------------------------------------------------------------------------
// The transcript's renderer
// ---------------------------------------------------------------------------

describe("rendering an answer", () => {
  it("escapes markup the model wrote", () => {
    const el = render(Markdown, {
      source: 'Look: <img src=x onerror="boom()"> and <b>bold</b>.',
    });
    expect(el.querySelector("img")).toBeNull();
    expect(el.querySelector("b")).toBeNull();
    expect(el.textContent).toContain("<img");
  });

  it("still renders markdown", () => {
    const el = render(Markdown, { source: "# Title\n\n- one\n- two\n\n**strong**" });
    expect(el.querySelector("h1").textContent).toBe("Title");
    expect(el.querySelectorAll("li")).toHaveLength(2);
    expect(el.querySelector("strong").textContent).toBe("strong");
  });

  it("turns a chart fence into a chart and a code fence into code", () => {
    const el = render(Markdown, {
      source:
        'Before.\n\n```chart\n{"type":"line","x":["a"],"series":[{"name":"s","data":[1]}]}\n```\n\n```python\nprint(1)\n```',
    });
    expect(el.querySelector("figure svg")).toBeTruthy();
    expect(el.querySelector(".block .lang").textContent.trim()).toBe("python");
  });

  /** A fence still arriving must not render as a half-built chart. */
  it("leaves a half-arrived chart as text", () => {
    const el = render(Markdown, {
      source: '```chart\n{"type":"line"',
      streaming: true,
    });
    expect(el.querySelector("figure svg")).toBeNull();
  });
});
