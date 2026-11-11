import type { LoginStep } from "organizer-sdk";

/**
 * Where a new account goes once made: the app, or (with `TOTP_REQUIRED`)
 * the login page's step that sets up two-factor authentication first. The
 * server answers 201 with nothing, or 200 with the step.
 */
export function afterNewAccount(answer: unknown): "app" | "totp" | "totp-setup" {
  const step = answer as Partial<LoginStep> | null | undefined;
  if (step?.totp_setup_required) return "totp-setup";
  if (step?.totp_required) return "totp";
  return "app";
}

/** A stand-in origin: `next` is read against it, never fetched. */
const HERE = "http://organizer.invalid";

/**
 * Where a sign-in may go on to: a page of this app, as a path with its
 * query (`/settings/contacts?token=…`, an app's device approval page),
 * percent-encoded; none for anything that leaves this origin (`//host`,
 * `/\host`, a scheme), and none for `/`, where it goes anyway. The
 * browser's own URL parser decides, so what it would take for another
 * host is refused. The backend checks it again (`allowed_next`).
 */
export function allowedNext(next: unknown): string | undefined {
  if (typeof next !== "string" || !next.startsWith("/")) return undefined;
  let url: URL;
  try {
    url = new URL(next, HERE);
  } catch {
    return undefined;
  }
  if (url.origin !== HERE) return undefined;
  const path = `${url.pathname}${url.search}${url.hash}`;
  return path === "/" ? undefined : path;
}
