import { formatSize } from "@/lib/format";

/** A user's storage for people: "1.2 GB", or "1.2 GB of 10 GB" with a limit. */
export function formatUsage(used: number, limit: number | null | undefined): string {
  return limit == null ? formatSize(used) : `${formatSize(used)} of ${formatSize(limit)}`;
}

/** How much of the limit is used, 0 to 100; none without a limit. */
export function usagePercent(used: number, limit: number | null | undefined): number | undefined {
  if (limit == null) return undefined;
  if (limit <= 0) return 100;
  return Math.min(100, Math.round((used / limit) * 100));
}

const GB = 1_000_000_000;

/**
 * A storage limit typed in GB ("10", "2.5") as bytes; null for an empty
 * field (no limit); undefined when it is not a number of 0 or more.
 */
export function parseLimitGb(text: string): number | null | undefined {
  const trimmed = text.trim().replace(",", ".");
  if (trimmed === "") return null;
  if (!/^\d+(\.\d+)?$/.test(trimmed)) return undefined;
  return Math.round(Number(trimmed) * GB);
}

/** A limit in bytes as the GB field shows it: "10", "2.5"; empty for none. */
export function limitToGb(limit: number | null | undefined): string {
  if (limit == null) return "";
  return String(Math.round((limit / GB) * 100) / 100);
}

/** When an invite stops working: "expires in 7 days", "expires within a day". */
export function formatExpires(iso: string, now: Date = new Date()): string {
  const left = Math.ceil((new Date(iso).getTime() - now.getTime()) / 86_400_000);
  if (left <= 1) return "expires within a day";
  return `expires in ${left} days`;
}

/** An invite's link, on this server. */
export function inviteLink(token: string, origin: string = window.location.origin): string {
  return `${origin}/invite/${token}`;
}
