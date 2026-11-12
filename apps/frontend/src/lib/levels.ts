import type {
  LevelSetting,
  LevelValue,
  ResolvedSetting,
  SettingMode,
  SettingSource,
  UserSetting,
} from "organizer-sdk";
import { WORKS } from "@/lib/ai";

/**
 * Settings levels (docs/M7B_DESIGN.md §4): the superadmin, a team, a user
 * (and a drive) each lock a setting, give a default, hold a number to a
 * range, or leave it to those below.
 */

/** What the level control offers; `unset` is no row at that level. */
export type LevelChoice = SettingMode | "unset";

export const LEVEL_CHOICES: readonly { value: LevelChoice; label: string }[] = [
  { value: "default", label: "Users may change" },
  { value: "locked", label: "Locked" },
  { value: "range", label: "A range" },
  { value: "unset", label: "Not set" },
];

/** The kinds of value a range can hold. */
export function isNumber(kind: string): boolean {
  return kind === "integer" || kind === "fraction";
}

/** The level control's choices for a setting of `kind` (a range for numbers only). */
export function levelChoices(kind: string): readonly { value: LevelChoice; label: string }[] {
  return LEVEL_CHOICES.filter((c) => c.value !== "range" || isNumber(kind));
}

/** A value of the API's JSON as what it is. */
export type Json = boolean | number | string | string[] | null;

/** The API's JSON value (typed as an object by the spec) as a plain value. */
export function jsonOf(value: unknown): Json {
  return (value ?? null) as Json;
}

/** A plain value as the API's JSON value. */
export function toJson(value: Json): NonNullable<LevelValue["value"]> {
  return value as unknown as NonNullable<LevelValue["value"]>;
}

/** How people know a setting: its label, the unit of a number, its choices' names. */
export type SettingInfo = {
  label: string;
  unit?: string;
  choices?: Record<string, string>;
};

const work = (key: string) => WORKS.find((w) => w.key === key)?.label ?? key;

/** Every setting with levels, as the pages show it. */
export const SETTINGS: Record<string, SettingInfo> = {
  "drop_box.pending_mode": {
    label: "When a place is proposed",
    choices: {
      move_pending: "Move it, pending until confirmed",
      stay: "Keep it in the drop box until confirmed",
      timeout: "Move it, and accept it after some hours",
      confidence: "Accept it when the AI is sure enough",
    },
  },
  "drop_box.accept_after_hours": { label: "Accept after", unit: "hours" },
  "drop_box.confidence_threshold": { label: "Accept from a confidence of", unit: "%" },
  "drop_box.read_contents": {
    label: "Read contents",
    choices: {
      always: "For every item",
      per_item: "Chosen per item",
      default_on: "On, can be turned off per item",
    },
  },
  "drop_box.folder_freedom": {
    label: "New folders",
    choices: {
      consistent: "Consistent: prefer existing folders",
      balanced: "Balanced",
      refined: "Refined: more specific folders",
    },
  },
  "retention.trash_days": { label: "Empty the trash after (0: never)", unit: "days" },
  "retention.versions_hours": { label: "Keep every version for", unit: "hours" },
  "retention.versions_days": { label: "Then one a day for", unit: "days" },
  "retention.versions_months": { label: "Then one a month for", unit: "months" },
  "notifications.review": { label: "Drop box items to review" },
  "notifications.shared": { label: "Something shared with them" },
  "notifications.backup": { label: "A failed backup (superadmins)" },
  "ai.model.proposals": { label: work("proposals") },
  "ai.model.images": { label: work("images") },
  "ai.model.scans": { label: work("scans") },
  "ai.model.audio": { label: work("audio") },
  "ai.own_models": { label: "Users may add their own models" },
  "links.allowed": { label: "Public links may be made" },
  "links.password_required": { label: "Every link has a password" },
  "links.max_days": { label: "Longest a link lives (0: no limit)", unit: "days" },
  "encryption.allowed": { label: "Users may make encrypted folders" },
  "storage.own_backends": { label: "Users may add their own storage" },
  "mirrors.shared_drives": { label: "Members may mirror shared drives to their own storage" },
  "mirrors.allowed": { label: "Allow mirrors of this drive" },
  "sign_in.two_factor_required": { label: "Two-factor authentication required" },
  "federation.share_with_any": {
    label: "Share with any address on another server, not only contacts",
  },
  "federation.allowed": { label: "Share with people on other servers" },
  "federation.read_remote": { label: "Search and AI read shares from other servers" },
  "drives.create": {
    label: "Who may make shared drives",
    choices: {
      superadmin: "The superadmin only",
      team_managers: "Also team managers, for their team",
      everyone: "Everyone",
    },
  },
};

/** A setting's info; its key and the server's description when it is not listed. */
export function infoOf(key: string): SettingInfo {
  return SETTINGS[key] ?? { label: key };
}

/** The groups of the team defaults page, in order: a title and the keys' prefix. */
export const GROUPS: readonly { title: string; prefix: string }[] = [
  { title: "Drop box", prefix: "drop_box." },
  { title: "Trash and versions", prefix: "retention." },
  { title: "Notifications", prefix: "notifications." },
  { title: "AI models", prefix: "ai." },
  { title: "Public links", prefix: "links." },
  { title: "Encryption", prefix: "encryption." },
  { title: "Sign-in", prefix: "sign_in." },
  { title: "Shared drives", prefix: "drives." },
  { title: "Storage", prefix: "storage." },
  { title: "Mirrors", prefix: "mirrors." },
  { title: "Other servers", prefix: "federation." },
];

/** A number for people, in its unit: "7 days", "80 %" (a fraction is shown as a percent). */
export function formatAmount(value: number, kind: string, unit?: string): string {
  if (kind === "fraction") return `${Math.round(value * 100)} %`;
  if (!unit) return String(value);
  // "1 days" reads wrong: the unit's singular for one.
  return `${value} ${value === 1 && unit.endsWith("s") ? unit.slice(0, -1) : unit}`;
}

/** A range for people: "7 days or more", "at most 30 days", "7 to 30 days"; none without bounds. */
export function rangeText(
  min: number | null | undefined,
  max: number | null | undefined,
  kind: string,
  unit?: string,
): string | undefined {
  const has = (n: number | null | undefined): n is number => n != null;
  if (has(min) && has(max)) {
    if (kind === "fraction") return `${Math.round(min * 100)} to ${Math.round(max * 100)} %`;
    return `${min} to ${formatAmount(max, kind, unit)}`;
  }
  if (has(min)) return `${formatAmount(min, kind, unit)} or more`;
  if (has(max)) return `at most ${formatAmount(max, kind, unit)}`;
  return undefined;
}

/** Who a source is for people: "your administrator", a team's name, the server's configuration. */
export function sourceName(source: SettingSource): string {
  switch (source.level) {
    case "env":
      return "the server's configuration";
    case "superadmin":
      return "your administrator";
    case "team":
      return source.team_name ?? "your team";
    case "drive":
      return "the drive";
    default:
      return "you";
  }
}

/** Who locked a setting, as "Set by …" says it; none when it is not locked. */
export function lockedByName(setting: Pick<ResolvedSetting, "locked_by">): string | undefined {
  return setting.locked_by ? sourceName(setting.locked_by) : undefined;
}

/**
 * What a user is told under a setting they may change: the range the
 * levels above leave ("7 days or more"), and where the value comes from
 * when it is not their own ("Staff's default").
 */
export function userNote(
  setting: Pick<UserSetting, "source" | "min" | "max" | "kind" | "locked_by">,
  unit?: string,
): string | undefined {
  if (setting.locked_by) return undefined;
  const parts: string[] = [];
  const range = rangeText(setting.min, setting.max, setting.kind, unit);
  if (range) parts.push(range.charAt(0).toUpperCase() + range.slice(1));
  if (setting.source.level === "team") {
    parts.push(`${setting.source.team_name ?? "Your team"}'s default`);
  } else if (setting.source.level === "superadmin") {
    parts.push("Your administrator's default");
  }
  return parts.length ? `${parts.join(". ")}.` : undefined;
}

/** The level control's choice of a setting at its level. */
export function choiceOf(setting: Pick<LevelSetting, "mode">): LevelChoice {
  return setting.mode ?? "unset";
}

/**
 * What a level writes when its control changes to `choice`: a lock or a
 * default keeps the value shown (the level's, else what is above), a range
 * the bounds shown, or starts at least at the value shown (a range needs a
 * bound); null to unset.
 */
export function levelWrite(setting: LevelSetting, choice: LevelChoice): LevelValue | null {
  if (choice === "unset") return null;
  if (choice === "range") {
    const min = setting.min ?? setting.above.min;
    const max = setting.max ?? setting.above.max;
    const shown = shownValue(setting);
    if (min == null && max == null && typeof shown === "number") {
      return { mode: "range", min: shown, max: null };
    }
    return { mode: "range", min, max };
  }
  const value = setting.mode && setting.mode !== "range" ? setting.value : setting.above.value;
  return { mode: choice, value };
}

/** The value a level's field shows: its own when it locks or gives one, else what is above. */
export function shownValue(setting: LevelSetting): Json {
  return jsonOf(setting.mode && setting.mode !== "range" ? setting.value : setting.above.value);
}

/** A number typed in a field as the setting's value (a percent for a fraction); undefined if not one. */
export function parseAmount(text: string, kind: string): number | null | undefined {
  const trimmed = text.trim().replace(",", ".");
  if (trimmed === "") return null;
  const n = Number(trimmed);
  if (!Number.isFinite(n)) return undefined;
  if (kind === "fraction") return Math.round(n) / 100;
  return Number.isInteger(n) ? n : undefined;
}

/** A setting's number as its field shows it ("80" for 0.8 of a fraction). */
export function amountText(value: number | null | undefined, kind: string): string {
  if (value == null) return "";
  return String(kind === "fraction" ? Math.round(value * 100) : value);
}

/** The settings of a group, by their keys' prefix, in the order given. */
export function inGroup<T extends { key: string }>(settings: T[], prefix: string): T[] {
  return settings.filter((s) => s.key.startsWith(prefix));
}

/** A user's settings by key. */
export function byKey<T extends { key: string }>(settings: T[] | undefined): Map<string, T> {
  return new Map((settings ?? []).map((s) => [s.key, s]));
}
