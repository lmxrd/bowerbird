import { defineConfig } from "@hey-api/openapi-ts";

// The client is generated from the backend's OpenAPI spec (`mise run
// gen:spec` writes it); it is not committed. OPENAPI_SPEC points elsewhere,
// e.g. in the Docker build.
export default defineConfig({
  input: process.env.OPENAPI_SPEC ?? "../../apps/server/dist/openapi.json",
  output: "src",
  plugins: [
    "@hey-api/typescript",
    "@hey-api/client-fetch",
    // Requests and responses are validated with the generated zod schemas,
    // so a backend that drifts from its spec fails loudly in the browser.
    { name: "@hey-api/sdk", validator: true },
    "zod",
    "@tanstack/react-query",
  ],
});
