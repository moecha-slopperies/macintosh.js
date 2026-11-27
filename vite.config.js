import { defineConfig } from "vite";
import { resolve } from "node:path";

const root = resolve(import.meta.dirname);

export default defineConfig(({ mode }) => {
  const core = mode === "ppc" ? "sheepshaver" : "basilisk";

  return {
    root: resolve(root, "src/renderer"),
    publicDir: false,
    base: "./",
    clearScreen: false,
    resolve: {
      alias: {
        // Worker entries cannot be selected with a runtime branch: every call
        // to `new Worker(new URL(...))` becomes its own bundled entry, so both
        // cores would end up in every build. Resolving the entry here keeps a
        // build to a single core.
        "@emulator-worker-entry": resolve(
          root,
          core === "sheepshaver"
            ? "src/sheepshaver/SheepShaver-worker-boot.js"
            : "src/basilisk/BasiliskII-worker-boot.js",
        ),
      },
    },
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
  };
});
