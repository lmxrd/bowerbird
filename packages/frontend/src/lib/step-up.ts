/**
 * Step-up (docs/M7A_DESIGN.md §4): the superadmin's endpoints answer a
 * session whose passkey or two-factor code was confirmed within 15
 * minutes. Otherwise they refuse with a 403 whose `code` says what the web
 * asks for: `step_up_required` (confirm it, then the request is sent
 * again) or `second_factor_required` (the user has neither: they add one
 * first). The dialog is `components/step-up.tsx`; this is its logic.
 */
import type { StepUpState } from "organizer-sdk/types";

export type Refusal = "step_up_required" | "second_factor_required";

/** The step-up refusal an error body is, if it is one. */
export function refusalOf(body: unknown): Refusal | null {
  if (typeof body !== "object" || body === null || !("code" in body)) return null;
  return body.code === "step_up_required" || body.code === "second_factor_required"
    ? body.code
    : null;
}

/** Whether the session's confirmation still holds at `now`. */
export function steppedUp(stepUp: StepUpState | null | undefined, now = Date.now()): boolean {
  return !!stepUp?.until && Date.parse(stepUp.until) > now;
}

/**
 * What to ask before a superadmin page, from the session: nothing (still
 * confirmed), to confirm, or to add a second factor first.
 */
export function stepUpNeeded(
  stepUp: StepUpState | null | undefined,
  now = Date.now(),
): Refusal | null {
  if (steppedUp(stepUp, now)) return null;
  return stepUp?.totp || stepUp?.passkey ? "step_up_required" : "second_factor_required";
}

/** A code as the authenticator app shows it: six digits, spaces allowed. */
export function totpCode(input: string): string | null {
  const code = input.replace(/\s/g, "");
  return /^\d{6}$/.test(code) ? code : null;
}

/** The superadmin's endpoints (and their old path, answered for one release). */
export function isSuperadminUrl(url: string): boolean {
  const path = url.startsWith("/") ? url : new URL(url).pathname;
  return path.startsWith("/api/superadmin/") || path.startsWith("/api/admin/");
}

/**
 * One question at a time: while one is open, whoever asks gets its answer
 * (several requests refused at once open one dialog).
 */
export function oneAtATime<A, T>(ask: (arg: A) => Promise<T>): (arg: A) => Promise<T> {
  let open: Promise<T> | null = null;
  return (arg) => {
    open ??= ask(arg).finally(() => {
      open = null;
    });
    return open;
  };
}
