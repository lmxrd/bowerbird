/// <reference types="vitest/config" />
import path from "path";
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { tanstackRouter } from "@tanstack/router-plugin/vite";

export default defineConfig({
  plugins: [tanstackRouter({ target: "react", autoCodeSplitting: true }), react(), tailwindcss()],
  // `@/` is src/, as shadcn's Vite setup has it (tsconfig.json has the same);
  // import.meta.dirname rather than its __dirname, which Vite 8 warns about.
  resolve: { alias: { "@": path.resolve(import.meta.dirname, "./src") } },
  // The unit tests (src/lib/*.test.ts) run in node, in UTC, so that dates
  // are the same on every machine.
  test: { environment: "node", env: { TZ: "UTC" } },
  server: { proxy: { "/api": "http://localhost:8000" } },
});
