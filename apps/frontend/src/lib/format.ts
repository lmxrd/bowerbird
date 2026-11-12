/** A size in bytes, for people: "840 KB", "1.2 GB". */
export function formatSize(bytes: number): string {
  if (bytes < 1000) return `${bytes} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let value = bytes;
  let unit = "B";
  for (const next of units) {
    if (value < 1000) break;
    value /= 1000;
    unit = next;
  }
  return `${value < 10 ? value.toFixed(1) : Math.round(value)} ${unit}`;
}

/** A stored time (UTC, ISO 8601), in the browser's language and zone. */
export function formatDate(iso: string, now: Date = new Date()): string {
  const date = new Date(iso);
  const sameDay = date.toDateString() === now.toDateString();
  if (sameDay) {
    return date.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
  }
  return formatDay(iso, now);
}

/** A stored time's day, in the browser's language: "2 Nov", with the year when not this one. */
export function formatDay(iso: string, now: Date = new Date()): string {
  const date = new Date(iso);
  const sameYear = date.getFullYear() === now.getFullYear();
  return date.toLocaleDateString(undefined, {
    day: "numeric",
    month: "short",
    ...(sameYear ? {} : { year: "numeric" }),
  });
}

/** The path of a folder in the app's URLs: `/files/Documents/Taxes`. */
export function folderUrl(path: string): string {
  const parts = path.split("/").filter(Boolean);
  return parts.length === 0 ? "/" : `/files/${parts.map(encodeURIComponent).join("/")}`;
}

/**
 * When something kept for `days` days after `iso` goes for good: "in 7
 * days", "within a day"; empty when it is kept until removed (0 days).
 */
export function formatGoes(iso: string, days: number, now: Date = new Date()): string {
  if (days <= 0) return "";
  const goes = new Date(iso).getTime() + days * 86_400_000;
  const left = Math.ceil((goes - now.getTime()) / 86_400_000);
  return left <= 1 ? "within a day" : `in ${left} days`;
}

/** A stored time with the date and the time of day, for lists of events. */
export function formatDateTime(iso: string): string {
  return new Date(iso).toLocaleString(undefined, {
    day: "numeric",
    month: "short",
    year: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}
