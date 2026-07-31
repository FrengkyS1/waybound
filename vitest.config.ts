import { defineConfig } from "vitest/config";

// Kept separate from vite.config.ts so the app build never pulls in test-only
// settings. `environment: "jsdom"` is the default here because several of the
// things worth testing (zustand stores, anything touching `window`) can't run
// under plain node, and per-file `// @vitest-environment node` is easy to
// forget in the direction that fails confusingly.
export default defineConfig({
  test: {
    environment: "jsdom",
    globals: true,
    setupFiles: ["./src/test/setup.ts"],
    include: ["src/**/*.{test,spec}.{ts,tsx}"],
  },
});
