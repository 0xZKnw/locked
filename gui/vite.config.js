import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";

// Everything is bundled and inlined: the shipped window loads no remote origin,
// which is what makes the strict CSP in tauri.conf.json survivable.
export default defineConfig({
  plugins: [svelte()],
  clearScreen: false,
  server: { port: 5173, strictPort: true },
  build: { target: "esnext", assetsInlineLimit: 1024 * 1024 },

  test: {
    // The store runs against real DOM timing — `requestAnimationFrame` is what
    // drives the streaming pump, so testing it in a bare node environment would
    // test something else.
    environment: "jsdom",
    include: ["tests/**/*.test.js"],
    // `.svelte.js` carries runes, so it has to go through the Svelte compiler
    // like any component would.
    alias: { "@app": "/src" },
  },
});
