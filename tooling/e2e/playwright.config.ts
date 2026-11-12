import { defineConfig, devices } from "@playwright/test";

// The end-to-end smoke tests: a real browser against the built image (see
// scripts/e2e.sh, which starts it). One flow, in order, on one database:
// no parallel workers.
export default defineConfig({
  testDir: "tests",
  fullyParallel: false,
  workers: 1,
  retries: process.env.CI ? 1 : 0,
  reporter: process.env.CI ? [["list"], ["github"]] : "list",
  use: {
    baseURL: process.env.E2E_BASE_URL ?? "http://127.0.0.1:8090",
    trace: "retain-on-failure",
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
});
