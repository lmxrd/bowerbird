/** A backup schedule as the form edits it. */
export type Schedule = { kind: "daily"; time: string } | { kind: "every"; hours: string };

/** `daily 03:00` or `every 6h`, as the server keeps it; daily at 03:00 otherwise. */
export function parseSchedule(text: string): Schedule {
  const daily = /^daily\s+(\d{1,2}:\d{2})$/i.exec(text.trim());
  if (daily) return { kind: "daily", time: daily[1].padStart(5, "0") };
  const every = /^every\s+(\d+)\s*h$/i.exec(text.trim());
  if (every) return { kind: "every", hours: every[1] };
  return { kind: "daily", time: "03:00" };
}

export function formatSchedule(schedule: Schedule): string {
  return schedule.kind === "daily" ? `daily ${schedule.time}` : `every ${schedule.hours}h`;
}

/** A long random password from the browser's own generator, for a repository. */
export function generatePassword(length = 32): string {
  const alphabet = "ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789";
  const bytes = crypto.getRandomValues(new Uint8Array(length));
  // 256 is not a multiple of the alphabet: the few top values are skewed,
  // which a 32-character password does not notice.
  return Array.from(bytes, (b) => alphabet[b % alphabet.length]).join("");
}
