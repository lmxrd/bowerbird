import { QueryClient } from "@tanstack/react-query";
import { listDropBoxItemsOptions, listSharedWithMeOptions } from "organizer-sdk/react-query";
import { refusalSentence } from "@/lib/federation";

/**
 * The one query cache of the app. The API is generated from the backend's
 * OpenAPI spec (`organizer-sdk`, `mise run gen:sdk`): routes use its query
 * options and mutations through TanStack Query, and this module holds what
 * is shared around them.
 */
export const queryClient = new QueryClient({
  defaultOptions: { queries: { retry: false, refetchOnWindowFocus: false } },
});

/**
 * The message of a failed request. The API answers errors as
 * `{"error": "..."}`, which the generated client throws as it is; the
 * desktop app's commands fail with the message itself.
 */
export function errorMessage(error: unknown, fallback = "Something went wrong"): string {
  if (typeof error === "string" && error) return error;
  if (error instanceof Error) return error.message;
  if (typeof error === "object" && error !== null && "error" in error) {
    if (typeof error.error !== "string") return fallback;
    // A drive's storage fails or is locked (503): the reason, and that
    // nothing was lost.
    if ("code" in error && error.code === "storage_unavailable") {
      return `${error.error.replace(/\.$/, "")}. Nothing was changed: try again once it's back.`;
    }
    // A write to a drive being moved to other storage (409): read-only
    // until that is done.
    if ("code" in error && error.code === "read_only") {
      return "This drive is being moved to other storage. Nothing was changed: try again once it's done.";
    }
    // Federation's refusals (docs/M7E_DESIGN.md §10.12), in words by their code.
    const refusal = refusalSentence(errorCode(error));
    if (refusal) return refusal;
    return error.error;
  }
  return fallback;
}

/** An error's `code` (`ErrorBody.code`), when it has one: what a client acts on, never the words. */
export function errorCode(error: unknown): string | undefined {
  if (typeof error !== "object" || error === null || !("code" in error)) return undefined;
  return typeof error.code === "string" ? error.code : undefined;
}

/**
 * The drop box's items, newest first: one query for the page, its tabs'
 * counts and the sidebar's badge.
 */
export function dropBoxItemsOptions() {
  return listDropBoxItemsOptions({ query: { limit: 500 } });
}

/** What is shared with the user: one query for the page and the sidebar's badge. */
export function sharedWithMeOptions() {
  return listSharedWithMeOptions();
}
