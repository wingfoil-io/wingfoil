import { defineConfig } from "vitest/config";

// Dedicated config so vitest doesn't pick up `vite.config.ts`, which is
// rooted in `examples/solid-dashboard` for the dev playground.
export default defineConfig({
  test: {
    environment: "node",
    include: ["tests/**/*.test.ts"],
  },
});
