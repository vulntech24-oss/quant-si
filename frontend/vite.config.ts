import { defineConfig } from "vitest/config";

// The frontend talks only to the QuantDesk backend (INV-15). In development
// Vite proxies /api to the local server; in production qd-server serves the
// built files itself, so there is no cross-origin traffic at all.
export default defineConfig({
  server: {
    proxy: { "/api": "http://127.0.0.1:8080" },
  },
  build: { outDir: "dist", sourcemap: false },
  test: { environment: "node" },
});
