import type { Status } from "organizer-sdk/types";

/** The statuses of items that wait for the user: the sidebar's badge. */
export const WAITING: readonly Status[] = [
  "proposed",
  "needs_manual",
  "rejected",
  "failed",
  "waiting",
  "undone",
];
/** The statuses of items the AI is still on. */
export const ANALYSING: readonly Status[] = ["queued", "analysing"];
/** The statuses of placed and accepted items. */
export const DONE: readonly Status[] = ["confirmed", "changed", "auto_accepted"];
/** The statuses of items to place by hand, or to try again. */
export const UNPLACED: readonly Status[] = [
  "needs_manual",
  "rejected",
  "failed",
  "waiting",
  "undone",
];

/** How many of these items wait for the user. */
export function waitingCount(items: readonly { status: Status }[]): number {
  return items.filter((item) => WAITING.includes(item.status)).length;
}

/** The status as a label, in sentence case. */
export function statusLabel(status: Status): string {
  const labels: Record<Status, string> = {
    queued: "Queued",
    analysing: "Analysing",
    waiting: "Waiting for the AI",
    proposed: "Proposed",
    confirmed: "Confirmed",
    changed: "Changed",
    rejected: "Rejected",
    auto_accepted: "Accepted",
    failed: "Failed",
    needs_manual: "Place by hand",
    undone: "Undone",
  };
  return labels[status];
}

/** A confidence from 0 to 1 as a whole percent. */
export function percent(confidence: number): string {
  return `${Math.round(Math.min(1, Math.max(0, confidence)) * 100)}%`;
}
