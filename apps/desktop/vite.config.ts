/// <reference types="vitest/config" />
import path from "path";
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

// The window of the desktop app (Tauri serves `dist/`; `tauri dev` this
// dev server). `@/` is the web app's src/: its components, the shadcn/ui
// ones and its lib are imported from there, never copied. React and
// TanStack Query are one copy for both, or their contexts would not meet.
export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: { "@": path.resolve(import.meta.dirname, "../frontend/src") },
    dedupe: ["react", "react-dom", "@tanstack/react-query"],
  },
  clearScreen: false,
  server: { port: 5174, strictPort: true },
  // One bundle is fine: the app loads it from disk, not the network.
  build: { target: "es2022", chunkSizeWarningLimit: 2000 },
  // The unit tests (src/lib/*.test.ts) run in node, in UTC.
  test: { environment: "node", env: { TZ: "UTC" } },
});
