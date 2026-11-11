import { invoke } from "@tauri-apps/api/core";

type Answer = { status: number; content_type: string | null; body: string };

/**
 * The SDK's `fetch` in the desktop app: each request goes to the app's
 * `api` command, which sends it to the server with the device's token.
 * So the web app's components (and their SDK calls) work unchanged, the
 * token never reaches the web view, and the server needs no CORS.
 */
export async function appFetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
  const request = input instanceof Request ? input : new Request(input, init);
  const url = new URL(request.url);
  const body = ["GET", "HEAD"].includes(request.method) ? null : await request.text();
  const answer = await invoke<Answer>("api", {
    method: request.method,
    path: url.pathname + url.search,
    contentType: request.headers.get("content-type"),
    body: body || null,
  });
  // A body is not allowed with these statuses.
  const empty = [101, 204, 205, 304].includes(answer.status);
  return new Response(empty ? null : answer.body, {
    status: answer.status,
    headers: answer.content_type ? { "content-type": answer.content_type } : {},
  });
}
