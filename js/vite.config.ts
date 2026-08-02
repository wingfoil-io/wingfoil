import { defineConfig } from "vite";
import solid from "vite-plugin-solid";
import { fileURLToPath, URL } from "node:url";

// Dev server for the `examples/solid-dashboard` playground.
// Run the Rust example first: `cargo run --example web --features web`.
export default defineConfig({
  root: "examples/solid-dashboard",
  plugins: [solid()],
  resolve: {
    alias: {
      "@wingfoil/client": fileURLToPath(new URL("./src/index.ts", import.meta.url)),
      "@wingfoil/client/solid": fileURLToPath(new URL("./src/solid.ts", import.meta.url)),
    },
  },
  server: {
    port: 5173,
  },
});
