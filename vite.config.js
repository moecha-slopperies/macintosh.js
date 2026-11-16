import { defineConfig } from "vite";
import { resolve } from "node:path";

const root = resolve(import.meta.dirname);

export default defineConfig({
  root: resolve(root, "src/renderer"),
  publicDir: false,
  base: "./",
  clearScreen: false,
  build: {
    outDir: resolve(root, "dist-renderer"),
    emptyOutDir: true,
    target: "safari15",
    rollupOptions: {
      input: {
        index: resolve(root, "src/renderer/index.html"),
        help: resolve(root, "src/renderer/help.html"),
        credits: resolve(root, "src/renderer/credits.html"),
      },
    },
  },
  server: {
    fs: {
      allow: [root],
    },
    port: 1420,
    strictPort: true,
    headers: {
      "Cross-Origin-Embedder-Policy": "require-corp",
      "Cross-Origin-Opener-Policy": "same-origin",
    },
  },
  worker: {
    format: "es",
  },
});
